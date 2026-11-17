//! H-03 acceptance (lab): VP9 on VideoToolbox — supplemental decoder registration,
//! synthesised `vpcC`, superframe split, `show_existing_frame` — Y/UV md5 equal to
//! ffmpeg's software decode on 200 seeded PTS for (b) (AJ's 1440p VP9 MKV) and (b′);
//! 1440p VP9 and H.264 decode fps recorded.
#![cfg(target_os = "macos")]

mod support;

use std::collections::HashMap;
use std::process::Command;
use std::time::Instant;

use gmclip_gpu::apple::PlaneFormat;
use gmclip_media::backend::SampleSource;
use gmclip_media::backend::vt::vp9::{parse_header, split_superframe, vpcc};
use gmclip_media::backend::vt::{FrameReader, VtVideoDecoder, VtVp9Decoder};
use gmclip_media::decode::FrameImage;
use gmclip_media::index::SampleIdx;
use support::*;

const SEED: u64 = 0x676d_636c_6970;

fn vp9_reader(path: &str) -> (VideoProbe, FrameReader<FileSource, VtVp9Decoder>) {
    let mut p = probe(path, "v:0");
    assert_eq!(p.codec, "vp9");
    mkv_offsets_to_data(path, &mut p.index);
    let mut src = FileSource::open(path, p.index.clone());
    let mut buf = Vec::new();
    src.read(SampleIdx(0), &mut buf).unwrap();
    let key = parse_header(split_superframe(&buf)[0]).unwrap();
    assert!(key.key_frame);
    let c = vpcc(&key, Some(1), Some(1), Some(1));
    let fmt = VtVp9Decoder::output_format(&key).unwrap();
    let (w, h, ts) = (p.width, p.height, p.time_base.1 as i32);
    let reader = FrameReader::new(src, move || VtVp9Decoder::new(c.clone(), w, h, fmt, ts));
    (p, reader)
}

/// Superframe / hidden-frame / show_existing census over the first `n` packets.
fn census(path: &str, p: &VideoProbe, n: usize) -> (usize, usize, usize) {
    let mut src = FileSource::open(path, p.index.clone());
    let (mut supers, mut hidden, mut existing) = (0, 0, 0);
    let mut buf = Vec::new();
    for i in 0..n.min(p.index.len()) {
        src.read(SampleIdx(i as u32), &mut buf).unwrap();
        let subs = split_superframe(&buf);
        supers += usize::from(subs.len() > 1);
        for s in subs {
            let h = parse_header(s).unwrap();
            existing += usize::from(h.show_existing.is_some());
            hidden += usize::from(h.show_existing.is_none() && !h.show_frame);
        }
    }
    (supers, hidden, existing)
}

fn seeded(path: &str, label: &str) {
    let (p, mut r) = vp9_reader(path);
    let (supers, hidden, existing) = census(path, &r.source().index().clone().into_probe(&p), 3000);
    let targets = seeded_pts(&p.index, 200, SEED);
    let golden: HashMap<i64, (String, String)> = golden_md5s(path, label, &p, &targets)
        .into_iter()
        .map(|(pts, y, uv)| (pts, (y, uv)))
        .collect();
    let mut bad = Vec::new();
    let mut times = Vec::new();
    for &pts in &targets {
        let t = Instant::now();
        let f = r
            .frame_at_pts(pts)
            .unwrap_or_else(|e| panic!("{label} pts {pts}: {e:?}"));
        times.push(t.elapsed().as_secs_f64() * 1e3);
        let FrameImage::Apple(pb) = &f.image else {
            panic!()
        };
        let pl = pb.read_planes().unwrap();
        assert_eq!((pl.width, pl.height), (p.width, p.height));
        if (md5(&pl.y), md5(&pl.uv_planar())) != golden[&pts] {
            bad.push(pts);
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "H-03 {label}: {}x{}, {} samples (first 3000: {supers} superframes, {hidden} hidden frames, {existing} show_existing); 200 seeded PTS: {} md5 mismatches; seek p50 {:.1} ms p95 {:.1} ms",
        p.width,
        p.height,
        p.index.len(),
        bad.len(),
        times[100],
        times[190]
    );
    assert!(bad.is_empty(), "{label}: mismatches at {bad:?}");
}

trait IntoProbe {
    fn into_probe(self, p: &VideoProbe) -> VideoProbe;
}
impl IntoProbe for gmclip_media::SampleIndex {
    fn into_probe(self, p: &VideoProbe) -> VideoProbe {
        VideoProbe {
            index: self,
            ..p.clone()
        }
    }
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg (run with --features lab)"
)]
fn vp9_b_prime_synthetic_altref() {
    let path = synthetic_vp9();
    seeded(&path, "vp9-b-prime-synthetic");
    // Sequential: every frame against ffmpeg.
    let (p, mut r) = vp9_reader(&path);
    let golden = golden_all_md5s(&path, &p);
    let mut got = Vec::new();
    r.decode_range(0, p.index.len(), |f| {
        let FrameImage::Apple(pb) = &f.image else {
            panic!()
        };
        let pl = pb.read_planes().unwrap();
        got.push((md5(&pl.y), md5(&pl.uv_planar())));
        Ok(())
    })
    .unwrap();
    let bad = got.iter().zip(&golden).filter(|(a, b)| a != b).count();
    eprintln!(
        "H-03 vp9-b-prime-synthetic sequential: {} frames, {bad} mismatches",
        got.len()
    );
    assert_eq!(got.len(), golden.len());
    assert_eq!(bad, 0);
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg + AJ's media"
)]
fn vp9_b_1440p_source() {
    seeded(vp9_1440p_mkv(), "vp9-b-1440p");
}

/// Decode throughput, no readback: 1440p VP9 (b) and 1440p H.264 (the MMO source),
/// 1800 frames from the 1000th sync sample, median of 3 runs each. VP9 decodes
/// synchronously (one VideoToolbox call per sub-frame, to capture hidden frames);
/// H.264 asynchronously. Load average printed: the numbers move 2–3× with load.
#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs VideoToolbox + AJ's media")]
fn decode_fps_1440p() {
    if gmclip_model::lab::skip_without([vp9_1440p_mkv()]) {
        return;
    }
    let n = 1800usize;
    let load = String::from_utf8(run(&mut Command::new("/usr/bin/uptime"))).unwrap();
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (p, mut r) = vp9_reader(vp9_1440p_mkv());
    let from = p
        .index
        .sync
        .iter()
        .enumerate()
        .filter(|x| *x.1)
        .nth(1000)
        .unwrap()
        .0;
    let mut vp9 = Vec::new();
    let mut vp9_hw = false;
    for _ in 0..3 {
        let t = Instant::now();
        let k = r.decode_range(from, from + n, |_| Ok(())).unwrap();
        vp9.push(k as f64 / t.elapsed().as_secs_f64());
        vp9_hw = r.decoder_caps().is_some_and(|c| c.hardware);
    }
    // The H.264 row reads the full MMO restream (a); without it the VP9 row is measured
    // alone and the line says so.
    let h264_row = if let Some(gone) = gmclip_model::lab::first_missing([mmo_1440p()]) {
        eprintln!("H.264 row: {}", gmclip_model::lab::skip_line(&gone));
        None
    } else {
        let ph = probe(mmo_1440p(), "v:0");
        let cfg = gmclip_gpu::apple::VtDecoderConfig {
            codec: gmclip_gpu::apple::CodecConfig::H264 {
                avcc: ph.extradata.clone(),
            },
            width: ph.width,
            height: ph.height,
            output: PlaneFormat::Nv12Video,
            require_hardware: false,
            timescale: ph.time_base.1 as i32,
        };
        let from = ph
            .index
            .sync
            .iter()
            .enumerate()
            .filter(|x| *x.1)
            .nth(1000)
            .unwrap()
            .0;
        let mut rh = FrameReader::new(FileSource::open(mmo_1440p(), ph.index.clone()), move || {
            VtVideoDecoder::new(&cfg, true)
        });
        let mut h264 = Vec::new();
        let mut h264_hw = false;
        for _ in 0..3 {
            let t = Instant::now();
            let k = rh.decode_range(from, from + n, |_| Ok(())).unwrap();
            h264.push(k as f64 / t.elapsed().as_secs_f64());
            h264_hw = rh.decoder_caps().is_some_and(|c| c.hardware);
        }
        Some((h264, h264_hw))
    };
    let h264_line = match &h264_row {
        Some((h264, h264_hw)) => format!(
            "H.264 {:.0} fps (session hw={h264_hw}, async; runs {h264:.0?})",
            median(h264.clone())
        ),
        None => "H.264 not measured (source (a) not on this machine)".to_owned(),
    };
    eprintln!(
        "H-03 decode fps (2560x1440, {n} frames, no readback, median of 3): VP9 {:.0} fps (session hw={vp9_hw}, synchronous; runs {vp9:.0?}), {h264_line}; {}",
        median(vp9.clone()),
        load.trim()
    );
    assert!(vp9_hw, "VP9 hardware session");
    if let Some((_, h264_hw)) = h264_row {
        assert!(h264_hw, "H.264 hardware session");
    }
}

/// `show_existing_frame` on real pictures: (b′) with a 1-byte `show_existing(slot 2)`
/// packet inserted a few frames after each hidden alt-ref (which refreshes slot 2,
/// `refresh = 4`), written as IVF; ffmpeg's software decode of that IVF is the golden
/// for every frame, inserted ones included. (Legal: `show_existing_frame` changes no
/// decoder state.)
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg (run with --features lab)"
)]
fn vp9_show_existing_frame_against_ffmpeg() {
    use gmclip_media::VideoDecoder;
    use gmclip_media::decode::{EncodedSample, SubmitFlags};
    let path = synthetic_vp9();
    let mut p = probe(&path, "v:0");
    mkv_offsets_to_data(&path, &mut p.index);
    let mut src = FileSource::open(&path, p.index.clone());
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut inserted = 0;
    let mut since_arf: Option<usize> = None;
    let mut buf = Vec::new();
    for i in 0..p.index.len() {
        src.read(SampleIdx(i as u32), &mut buf).unwrap();
        packets.push(buf.clone());
        for sub in split_superframe(&buf) {
            let h = parse_header(sub).unwrap();
            if h.show_existing.is_none() && !h.show_frame && h.refresh == 4 {
                since_arf = Some(0);
            }
        }
        if let Some(k) = since_arf.as_mut() {
            *k += 1;
            if *k == 4 {
                packets.push(vec![0x8a]); // show_existing_frame, slot 2
                inserted += 1;
                since_arf = None;
            }
        }
    }
    assert!(inserted >= 10, "{inserted} insertions");
    // IVF: 32-byte file header, 12-byte frame headers.
    let mut ivf = Vec::new();
    ivf.extend_from_slice(b"DKIF");
    ivf.extend_from_slice(&0u16.to_le_bytes());
    ivf.extend_from_slice(&32u16.to_le_bytes());
    ivf.extend_from_slice(b"VP90");
    ivf.extend_from_slice(&(p.width as u16).to_le_bytes());
    ivf.extend_from_slice(&(p.height as u16).to_le_bytes());
    ivf.extend_from_slice(&30u32.to_le_bytes());
    ivf.extend_from_slice(&1u32.to_le_bytes());
    ivf.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    ivf.extend_from_slice(&0u32.to_le_bytes());
    for (k, pk) in packets.iter().enumerate() {
        ivf.extend_from_slice(&(pk.len() as u32).to_le_bytes());
        ivf.extend_from_slice(&(k as u64).to_le_bytes());
        ivf.extend_from_slice(pk);
    }
    let ivf_path = lab_dir().join("h03-show-existing.ivf");
    std::fs::write(&ivf_path, &ivf).unwrap();
    let raw = run(Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&ivf_path)
        .args([
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ]));
    let (w, h) = (p.width as usize, p.height as usize);
    let fb = w * h * 3 / 2;
    assert_eq!(
        raw.len(),
        fb * packets.len(),
        "ffmpeg: one picture per packet"
    );
    let key = parse_header(split_superframe(&packets[0])[0]).unwrap();
    let mut dec = VtVp9Decoder::new(
        vpcc(&key, Some(1), Some(1), Some(1)),
        p.width,
        p.height,
        VtVp9Decoder::output_format(&key).unwrap(),
        30,
    )
    .unwrap();
    let mut bad = 0;
    let mut bad_inserted = 0;
    for (k, pk) in packets.iter().enumerate() {
        let s = EncodedSample {
            sample: SampleIdx(k as u32),
            dts: k as i64,
            pts: k as i64,
            sync: k == 0,
            data: pk,
        };
        dec.submit(&s, SubmitFlags::default()).unwrap();
        let f = dec.poll().unwrap().expect("one picture per packet");
        let FrameImage::Apple(pb) = &f.image else {
            panic!()
        };
        let pl = pb.read_planes().unwrap();
        let g = &raw[k * fb..(k + 1) * fb];
        if (md5(&pl.y), md5(&pl.uv_planar())) != (md5(&g[..w * h]), md5(&g[w * h..])) {
            bad += 1;
            bad_inserted += usize::from(pk.len() == 1);
        }
    }
    eprintln!(
        "H-03 show_existing: {} packets, {inserted} show_existing(2) inserted after hidden alt-refs; {bad} md5 mismatches ({bad_inserted} on inserted)",
        packets.len()
    );
    assert_eq!(bad, 0);
    let _ = std::fs::remove_file(&ivf_path);
}

/// `show_existing_frame` repeats a reference slot's picture without decoding. None of
/// the available streams uses it (0 in the first 3000 packets of (b) and (b′)), so the
/// path is driven with a hand-made 1-byte header after a key frame (which refreshes
/// every slot): the repeated picture must equal the key frame bit for bit.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox (run with --features lab)"
)]
fn vp9_show_existing_frame_repeats_the_slot() {
    use gmclip_media::VideoDecoder;
    use gmclip_media::decode::{EncodedSample, SubmitFlags};
    let path = synthetic_vp9();
    let mut p = probe(&path, "v:0");
    mkv_offsets_to_data(&path, &mut p.index);
    let mut src = FileSource::open(&path, p.index.clone());
    let mut key = Vec::new();
    src.read(SampleIdx(0), &mut key).unwrap();
    let h = parse_header(split_superframe(&key)[0]).unwrap();
    let c = vpcc(&h, Some(1), Some(1), Some(1));
    let mut dec = VtVp9Decoder::new(
        c,
        p.width,
        p.height,
        PlaneFormat::Nv12Video,
        p.time_base.1 as i32,
    )
    .unwrap();
    let mut md5s = Vec::new();
    for (i, data) in [key.clone(), vec![0x8b]].iter().enumerate() {
        let s = EncodedSample {
            sample: SampleIdx(i as u32),
            dts: i as i64,
            pts: i as i64,
            sync: i == 0,
            data,
        };
        dec.submit(&s, SubmitFlags::default()).unwrap();
        let f = dec.poll().unwrap().expect("one picture per packet");
        let FrameImage::Apple(pb) = &f.image else {
            panic!()
        };
        let pl = pb.read_planes().unwrap();
        md5s.push((md5(&pl.y), md5(&pl.uv)));
    }
    assert_eq!(
        md5s[0], md5s[1],
        "show_existing_frame of slot 3 repeats the key frame"
    );
    // An empty slot is an error, not a stale or blank picture.
    let mut fresh = VtVp9Decoder::new(
        vpcc(&h, Some(1), Some(1), Some(1)),
        p.width,
        p.height,
        PlaneFormat::Nv12Video,
        1000,
    )
    .unwrap();
    let s = EncodedSample {
        sample: SampleIdx(0),
        dts: 0,
        pts: 0,
        sync: false,
        data: &[0x8b],
    };
    assert_eq!(
        fresh.submit(&s, SubmitFlags::default()).unwrap_err().step,
        "vp9_show_existing_empty_slot"
    );
}
