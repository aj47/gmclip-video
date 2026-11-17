//! H-08 acceptance (lab): VideoToolbox H.264 encode at the Reels preset.
//! 60 s synthetic 1080×1920 → High@4.0, has_b_frames = 2, a keyframe every 30 frames,
//! 1-s peak ≤ 25 Mbps, first PTS 0, DTS ≤ PTS. The MP4 (`Mp4Writer`, B-04) is not
//! merged yet: the elementary stream is checked with ffprobe and the timestamps the
//! muxer will get are checked here.
#![cfg(target_os = "macos")]

mod support;

use std::process::Command;

use gmclip_gpu::apple::{CpuPlanes, PixelBufferPool, PlaneFormat, VtEncoderConfig};
use gmclip_media::backend::vt::VtH264Encoder;
use support::*;

/// A moving test card: diagonal gradient, three moving boxes, and a band of per-frame
/// noise (worst case for the rate cap).
fn frame(w: u32, h: u32, n: u32, rng: &mut Rng) -> CpuPlanes {
    let mut p = CpuPlanes::new(w, h, PlaneFormat::Nv12Video);
    let (wu, hu) = (w as usize, h as usize);
    for y in 0..hu {
        for x in 0..wu {
            let v = ((x + y + n as usize * 4) % 220) as u8 + 16;
            p.y[y * wu + x] = v;
        }
    }
    for b in 0..3u32 {
        let bx = ((n * (5 + 3 * b) + b * 300) % (w - 200)) as usize;
        let by = ((n * (7 + 2 * b) + b * 500) % (h - 200)) as usize;
        for y in by..by + 200 {
            for x in bx..bx + 200 {
                p.y[y * wu + x] = 40 + 80 * b as u8;
            }
        }
    }
    let band = (hu * 3 / 4)..(hu * 3 / 4 + 120);
    for y in band {
        for x in 0..wu {
            p.y[y * wu + x] = 16 + (rng.next() % 220) as u8;
        }
    }
    let (cw, ch) = (wu.div_ceil(2), hu.div_ceil(2));
    for y in 0..ch {
        for x in 0..cw {
            p.uv[(y * cw + x) * 2] = 128u8.wrapping_add(((x + n as usize) % 64) as u8);
            p.uv[(y * cw + x) * 2 + 1] = 128u8.wrapping_sub(((y + n as usize) % 64) as u8);
        }
    }
    p
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffprobe (run with --features lab)"
)]
fn reels_preset_60s_synthetic() {
    let (w, h) = (1080u32, 1920u32);
    let cfg = VtEncoderConfig::reels(w, h);
    let frame_ticks = i64::from(cfg.timescale) / i64::from(cfg.fps);
    let mut enc = VtH264Encoder::new(&cfg).unwrap();
    let pool = PixelBufferPool::new(w, h, PlaneFormat::Nv12Video, 8).unwrap();
    let mut sink = AnnexBSink::new();
    let mut rng = Rng(7);
    let n = 1800u32;
    let t = std::time::Instant::now();
    for i in 0..n {
        let mut pb = pool.get().unwrap();
        pb.write_planes(&frame(w, h, i, &mut rng)).unwrap();
        enc.encode(&pb, i64::from(i) * frame_ticks, frame_ticks, &mut sink)
            .unwrap();
    }
    enc.finish(&mut sink).unwrap();
    let dt = t.elapsed();
    let es = lab_dir().join("h08-reels-60s.h264");
    std::fs::write(&es, &sink.out).unwrap();

    // Timestamps handed to the muxer.
    let s = &sink.samples;
    assert_eq!(s.len(), n as usize, "one sample per frame");
    let min_pts = s.iter().map(|x| x.0).min().unwrap();
    assert_eq!(min_pts, 0, "first video PTS 0");
    assert!(s.iter().all(|x| x.1 <= x.0), "DTS <= PTS");
    assert!(
        s.windows(2).all(|w| w[0].1 < w[1].1),
        "DTS strictly increasing"
    );
    let mut pts: Vec<i64> = s.iter().map(|x| x.0).collect();
    pts.sort();
    assert_eq!(
        pts,
        (0..i64::from(n))
            .map(|i| i * frame_ticks)
            .collect::<Vec<_>>()
    );
    // Keyframes: every 30th frame in presentation order, nothing else.
    let mut keys: Vec<i64> = s
        .iter()
        .filter(|x| x.2)
        .map(|x| x.0 / frame_ticks)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        (0..i64::from(n)).step_by(30).collect::<Vec<_>>(),
        "GOP 30, closed"
    );
    check_structure(&sink, frame_ticks);
    // 1-s peak: 1-s windows in decode order (as btrt counts), from the encoder's meter.
    let peak = enc.peak_bits_per_second() as usize;
    let total_bits: usize = s.iter().map(|x| x.3).sum::<usize>() * 8;
    let avg = total_bits as f64 / (f64::from(n) / 30.0);

    // The elementary stream, as ffprobe reads it.
    let probe = String::from_utf8(run(Command::new(FFPROBE).args([
        "-v", "error", "-show_entries", "stream=profile,level,has_b_frames,width,height,color_space,color_transfer,color_primaries,color_range",
        "-of", "default=nw=1",
    ]).arg(&es)))
    .unwrap();
    eprintln!(
        "H-08: {n} frames in {:.2?} ({:.0} fps incl. CPU fill); dts delay {:?} ticks; avg {:.2} Mbps, 1-s peak {:.2} Mbps\n{probe}",
        dt,
        f64::from(n) / dt.as_secs_f64(),
        enc.dts_delay(),
        avg / 1e6,
        peak as f64 / 1e6
    );
    assert!(probe.contains("profile=High"), "{probe}");
    assert!(probe.contains("level=40"), "{probe}");
    assert!(probe.contains("has_b_frames=2"), "{probe}");
    assert!(
        probe.contains("color_space=bt709") && probe.contains("color_range=tv"),
        "{probe}"
    );
    assert!(peak <= 25_000_000, "1-s peak {peak} bps");
    // Decodes back cleanly: ffmpeg counts every frame with no error output.
    let out = Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(&es)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&es); // intermediate (disk is shared by the lanes)
}

/// Encoder throughput alone (frames pre-filled), for the export budget (R-EXP-1).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox (run with --features lab)"
)]
fn reels_preset_encode_fps() {
    let (w, h) = (1080u32, 1920u32);
    let cfg = VtEncoderConfig::reels(w, h);
    let ft = i64::from(cfg.timescale) / i64::from(cfg.fps);
    let pool = PixelBufferPool::new(w, h, PlaneFormat::Nv12Video, 16).unwrap();
    let mut rng = Rng(9);
    let frames: Vec<_> = (0..16)
        .map(|i| {
            let mut pb = pool.get().unwrap();
            pb.write_planes(&frame(w, h, i * 7, &mut rng)).unwrap();
            pb
        })
        .collect();
    let mut enc = VtH264Encoder::new(&cfg).unwrap();
    let mut sink = AnnexBSink::new();
    let n = 900;
    let t = std::time::Instant::now();
    for i in 0..n {
        enc.encode(&frames[i % 16], i as i64 * ft, ft, &mut sink)
            .unwrap();
    }
    enc.finish(&mut sink).unwrap();
    let fps = n as f64 / t.elapsed().as_secs_f64();
    eprintln!("H-08: VT H.264 1080x1920 Reels-preset encode {fps:.0} fps (frames pre-filled)");
    assert_eq!(sink.samples.len(), n);
}

/// Structure checks that do not trust the encoder's own sync flag: sync samples are
/// exactly the samples carrying an IDR slice (NAL type 5); no sample after a sync
/// sample (decode order) shows before it (closed GOPs); durations are the DTS gaps.
fn check_structure(sink: &AnnexBSink, frame_ticks: i64) {
    let s = &sink.samples;
    for (k, (x, t)) in s.iter().zip(&sink.nal_types).enumerate() {
        assert_eq!(
            x.2,
            t.contains(&5),
            "sample {k}: sync flag vs IDR NAL ({t:?})"
        );
    }
    for (k, x) in s.iter().enumerate() {
        if x.2 {
            assert!(
                s[k + 1..].iter().all(|y| y.0 > x.0),
                "open GOP at sample {k}"
            );
        }
    }
    for k in 0..s.len() - 1 {
        assert_eq!(
            i64::from(sink.durations[k]),
            s[k + 1].1 - s[k].1,
            "duration = DTS gap"
        );
    }
    assert_eq!(i64::from(*sink.durations.last().unwrap()), frame_ticks);
}

/// Content the rate control finds hard: 4 s flat, 6 s of a moving card with ±8 code
/// values of noise everywhere, 4 s flat (`amp` sets the noise; 220 is pure noise).
fn hard_clip(enc: &mut VtH264Encoder, sink: &mut AnnexBSink, amp: u64, ft: i64) -> usize {
    let (w, h) = (1080u32, 1920u32);
    let pool = PixelBufferPool::new(w, h, PlaneFormat::Nv12Video, 8).unwrap();
    let mut rng = Rng(5);
    let n = 14 * 30;
    let flat = {
        let mut p = CpuPlanes::new(w, h, PlaneFormat::Nv12Video);
        p.y.fill(120);
        p.uv.fill(128);
        p
    };
    for i in 0..n {
        let mut pb = pool.get().unwrap();
        if (120..300).contains(&i) {
            let mut p = frame(w, h, i as u32, &mut rng);
            for v in p.y.iter_mut().chain(p.uv.iter_mut()) {
                *v = (i64::from(*v) + (rng.next() % (amp + 1)) as i64 - (amp / 2) as i64)
                    .clamp(16, 235) as u8;
            }
            pb.write_planes(&p).unwrap();
        } else {
            pb.write_planes(&flat).unwrap();
        }
        enc.encode(&pb, i as i64 * ft, ft, sink).unwrap();
    }
    enc.finish(sink).unwrap();
    n
}

/// The rate cap on a cut from flat to hard content (H-08 review): the first encode
/// overshoots for about a second (VideoToolbox ignores `DataRateLimits` there), the
/// meter sees it, and the retry ladder (`MinAllowedFrameQP`) brings the 1-s peak under
/// 25 Mbps. The GOP grid holds on flat content too.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox (run with --features lab)"
)]
fn reels_preset_rate_cap_ladder_and_gop_on_hard_content() {
    use gmclip_media::backend::{RATE_CAP_LADDER, SOCIAL_PEAK_BPS};
    let mut passed = None;
    for (rung, min_qp) in RATE_CAP_LADDER.iter().enumerate() {
        let mut cfg = VtEncoderConfig::reels(1080, 1920);
        cfg.min_qp = *min_qp;
        let ft = i64::from(cfg.timescale) / i64::from(cfg.fps);
        let mut enc = VtH264Encoder::new(&cfg).unwrap();
        let mut sink = AnnexBSink::new();
        let n = hard_clip(&mut enc, &mut sink, 16, ft);
        assert_eq!(sink.samples.len(), n);
        check_structure(&sink, ft);
        let mut keys: Vec<i64> = sink
            .samples
            .iter()
            .filter(|x| x.2)
            .map(|x| x.0 / ft)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            (0..n as i64).step_by(30).collect::<Vec<_>>(),
            "GOP grid"
        );
        let peak = enc.peak_bits_per_second();
        let per_s: Vec<String> = sink
            .samples
            .chunks(30)
            .map(|c| format!("{:.1}", c.iter().map(|x| x.3).sum::<usize>() as f64 * 8e-6))
            .collect();
        eprintln!(
            "H-08 hard content, rung {rung} (min QP {min_qp:?}): 1-s peak {:.2} Mbps; per second {}",
            peak as f64 / 1e6,
            per_s.join(" ")
        );
        if peak <= SOCIAL_PEAK_BPS {
            passed = Some(rung);
            break;
        }
    }
    assert!(
        passed.is_some(),
        "no rung of the ladder met the 25 Mbps cap"
    );
}

/// Pure full-frame noise cannot be held to 25 Mbps by the hardware encoder at any
/// setting tried (measured 68 Mbps sustained, 127–153 Mbps peak); the meter must report it so the
/// export fails with a named step instead of shipping an over-cap file.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox (run with --features lab)"
)]
fn pure_noise_breaks_the_cap_and_is_detected() {
    let mut cfg = VtEncoderConfig::reels(1080, 1920);
    cfg.min_qp = gmclip_media::backend::RATE_CAP_LADDER[3];
    let ft = i64::from(cfg.timescale) / i64::from(cfg.fps);
    let mut enc = VtH264Encoder::new(&cfg).unwrap();
    let mut sink = AnnexBSink::new();
    hard_clip(&mut enc, &mut sink, 220, ft);
    let peak = enc.peak_bits_per_second();
    let direct = sink
        .samples
        .iter()
        .enumerate()
        .map(|(k, x)| {
            sink.samples[k..]
                .iter()
                .take_while(|y| y.1 < x.1 + i64::from(cfg.timescale))
                .map(|y| y.3 as u64 * 8)
                .sum::<u64>()
        })
        .max()
        .unwrap();
    eprintln!(
        "H-08 pure noise: meter {:.1} Mbps, direct {:.1} Mbps",
        peak as f64 / 1e6,
        direct as f64 / 1e6
    );
    assert_eq!(
        peak, direct,
        "meter equals a direct decode-order window count"
    );
    assert!(peak > gmclip_media::backend::SOCIAL_PEAK_BPS);
}
