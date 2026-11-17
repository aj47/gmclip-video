//! Media Foundation video decode (M5) against the Mac's: every frame's packed NV12 md5
//! equals the reference (`ref/*.nv12.framemd5`, ffmpeg's software decode, which equals
//! VideoToolbox bit for bit per `vt_h264.rs` / `vt_vp9.rs`), sequentially through
//! `FrameReader` and by frame-exact random access: H.264 in software and on the GPU
//! (D3D11), VP9 on the GPU (Microsoft's VP9 decoder is hardware only). Lab media (never
//! committed) in `$GMCLIP_LAB_MEDIA`.
#![cfg(windows)]

mod support;

use std::path::PathBuf;

use gmclip_media::backend::frames::FrameReader;
use gmclip_media::backend::mf::{Accel, MfVideoDecoder};
use gmclip_media::decode::{DecodedFrame, FrameImage};
use gmclip_media::demux::{self, Codec, TrackKind};

fn lab(name: &str) -> Option<PathBuf> {
    // the lab media folder, else where the labpack is generated
    let dir = std::env::var_os("GMCLIP_LAB_MEDIA")
        .filter(|d| !d.is_empty())
        .map_or_else(support::lab_dir, PathBuf::from);
    let p = dir.join(name);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: no {} (set GMCLIP_LAB_MEDIA)", p.display());
        None
    }
}

/// The reference md5s, presentation order.
fn framemd5(p: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.rsplit(',').next().unwrap().trim().to_owned())
        .collect()
}

fn md5_of(f: &DecodedFrame) -> String {
    let FrameImage::Cpu { y, uv, .. } = &f.image;
    let mut all = Vec::with_capacity(y.len() + uv.len());
    all.extend_from_slice(y);
    all.extend_from_slice(uv);
    support::md5(&all)
}

type Reader = FrameReader<support::FileSource, MfVideoDecoder>;

fn reader(path: &std::path::Path, accel: Accel) -> (Reader, Vec<i64>) {
    let mut f = demux::io::FileSource::open(path).unwrap();
    let d = demux::index(&mut f).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .expect("a video track")
        .clone();
    let codec = t.codec.clone();
    let (w, h) = (t.width, t.height);
    let mut pts: Vec<i64> = (0..t.samples.dts.len())
        .map(|i| t.samples.dts[i] + i64::from(t.samples.cts_off[i]))
        .collect();
    pts.sort_unstable();
    let src = support::FileSource::open(path.to_str().unwrap(), t.samples.clone());
    (
        FrameReader::new(src, move || match &codec {
            Codec::H264 { avcc } => MfVideoDecoder::h264(avcc, w, h, accel),
            Codec::Vp9 { .. } => MfVideoDecoder::vp9(w, h),
            other => panic!("not H.264 or VP9: {other:?}"),
        }),
        pts,
    )
}

fn check(name: &str, refname: &str, accel: Accel) {
    let (Some(mp4), Some(refs)) = (lab(name), lab(refname)) else {
        return;
    };
    let want = framemd5(&refs);
    let (mut r, pts) = reader(&mp4, accel);
    assert_eq!(pts.len(), want.len(), "frame count");
    // sequential, presentation order
    let t = std::time::Instant::now();
    let mut got = Vec::new();
    let n = r
        .decode_range(0, pts.len(), |f| {
            got.push((f.pts, md5_of(&f)));
            Ok(())
        })
        .unwrap();
    let el = t.elapsed();
    assert_eq!(n as usize, want.len());
    let bad: Vec<usize> = got
        .iter()
        .enumerate()
        .filter(|(i, (p, m))| *p != pts[*i] || *m != want[*i])
        .map(|(i, _)| i)
        .collect();
    eprintln!(
        "{name} {accel:?}: sequential {} frames in {el:?} ({:.0} fps), {} differ from the Mac's",
        n,
        n as f64 / el.as_secs_f64(),
        bad.len()
    );
    assert!(bad.is_empty(), "{name}: frames {bad:?} differ");
    // frame-exact random access: seeded PTS, forward runs continue the chain
    let mut rng = support::Rng(11);
    let mut checked = 0;
    let t = std::time::Instant::now();
    for _ in 0..40 {
        let k = (rng.next() % pts.len() as u64) as usize;
        let f = r.frame_at_pts(pts[k]).unwrap();
        assert_eq!(md5_of(&f), want[k], "{name}: frame {k} by random access");
        checked += 1;
    }
    eprintln!(
        "{name}: {checked} random frames match in {:?} ({} sessions, {} samples submitted)",
        t.elapsed(),
        r.sessions_created,
        r.samples_submitted
    );
    // stepping forward: every request continues the chain the last one drained (the
    // replay path), across GOP boundaries; presentation order with B-frames means some
    // steps go back in decode order and restart
    let (mut r, _) = reader(&mp4, accel);
    let t = std::time::Instant::now();
    let mut steps = 0;
    for k in (0..pts.len()).step_by(3) {
        let f = r.frame_at_pts(pts[k]).unwrap();
        assert_eq!(md5_of(&f), want[k], "{name}: frame {k} stepping forward");
        steps += 1;
    }
    eprintln!(
        "{name}: {steps} forward steps match in {:?} ({} sessions, {} samples submitted)",
        t.elapsed(),
        r.sessions_created,
        r.samples_submitted
    );
}

#[test]
fn h264_high_with_b_frames_matches_the_mac() {
    check(
        "h264_high_aac.mp4",
        "ref/h264_high_aac.nv12.framemd5",
        Accel::Software,
    );
}

#[test]
fn h264_main_60fps_matches_the_mac() {
    check(
        "h264_main_60fps.mp4",
        "ref/h264_main_60fps.nv12.framemd5",
        Accel::Software,
    );
}

#[test]
fn h264_on_the_gpu_matches_the_mac() {
    check(
        "h264_high_aac.mp4",
        "ref/h264_high_aac.nv12.framemd5",
        Accel::Hardware,
    );
    check(
        "h264_main_60fps.mp4",
        "ref/h264_main_60fps.nv12.framemd5",
        Accel::Hardware,
    );
}

#[test]
fn vp9_on_the_gpu_matches_the_mac() {
    check("vp9.mp4", "ref/vp9.nv12.framemd5", Accel::Hardware);
}

/// Pictures come out as decoding goes, without a drain: a sequential reader never has
/// to drain (which on Media Foundation costs a replay from the keyframe).
#[test]
fn pictures_come_out_without_a_drain() {
    use gmclip_media::decode::{EncodedSample, SubmitFlags, VideoDecoder};
    use gmclip_media::index::SampleIdx;
    let Some(mp4) = lab("h264_high_aac.mp4") else {
        return;
    };
    let mut f = demux::io::FileSource::open(&mp4).unwrap();
    let d = demux::index(&mut f).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap();
    let Codec::H264 { avcc } = &t.codec else {
        panic!()
    };
    let bytes = std::fs::read(&mp4).unwrap();
    for accel in [Accel::Software, Accel::Hardware] {
        let mut dec = MfVideoDecoder::h264(avcc, t.width, t.height, accel).unwrap();
        let mut out = Vec::new();
        for i in 0..20 {
            let (o, n) = (t.samples.offset[i] as usize, t.samples.size[i] as usize);
            dec.submit(
                &EncodedSample {
                    sample: SampleIdx(i as u32),
                    data: &bytes[o..o + n],
                    pts: t.samples.dts[i] + i64::from(t.samples.cts_off[i]),
                    dts: t.samples.dts[i],
                    sync: t.samples.sync[i],
                },
                SubmitFlags::default(),
            )
            .unwrap();
            while let Some(p) = dec.poll().unwrap() {
                out.push((i, p.sample.0));
            }
        }
        eprintln!(
            "{accel:?}: {} pictures out of 20 submitted, before any drain: {out:?}",
            out.len()
        );
        assert!(
            out.len() >= 14,
            "{accel:?}: only {} pictures without a drain",
            out.len()
        );
    }
}

/// A sequential `FrameStream` read never drains: every frame's cost stays flat through a
/// GOP (no replay from the keyframe).
#[test]
fn a_sequential_stream_reads_without_replays() {
    use gmclip_media::backend::FrameStream;
    let Some(mp4) = lab("h264_high_aac.mp4") else {
        return;
    };
    let mut f = demux::io::FileSource::open(&mp4).unwrap();
    let d = demux::index(&mut f).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap()
        .clone();
    let Codec::H264 { avcc } = t.codec.clone() else {
        panic!()
    };
    let (w, h) = (t.width, t.height);
    let src = support::FileSource::open(mp4.to_str().unwrap(), t.samples.clone());
    let mut st = FrameStream::new(src, move || {
        MfVideoDecoder::h264(&avcc, w, h, Accel::Hardware)
    });
    st.set_lookahead(4);
    let mut worst = (0.0f64, 0u64);
    for n in 0..120u64 {
        let t0 = std::time::Instant::now();
        st.frame(n).unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if n > 0 && ms > worst.0 {
            worst = (ms, n);
        }
    }
    eprintln!(
        "sequential stream: worst {:.1} ms at frame {}, {} samples submitted for 120 frames, {} seeks",
        worst.0, worst.1, st.submitted, st.seeks
    );
    assert!(st.submitted < 140, "{} samples submitted", st.submitted);
    // a drain + replay from the keyframe costs tens of ms late in a GOP
    assert!(
        worst.0 < 15.0,
        "frame {} took {:.1} ms (a drain?)",
        worst.1,
        worst.0
    );
}

/// 10-bit VP9 (profile 2) comes out as P010, bit for bit ffmpeg's `p010le` frames.
#[test]
fn vp9_10bit_on_the_gpu_matches_as_p010() {
    check(
        "vp9_10bit.mp4",
        "ref/vp9_10bit.p010le.framemd5",
        Accel::Hardware,
    );
}
