//! H-02 acceptance (lab): VideoToolbox H.264 decode + ReorderStage + frame-exact seek.
//! Y/UV md5 must equal ffmpeg's software decode on 200 seeded PTS per source.
//!
//! `cargo test -p gmclip-media --features lab --release --test vt_h264 -- --nocapture --test-threads=1`
#![cfg(target_os = "macos")]

mod support;

use std::time::Instant;

use gmclip_gpu::apple::{CodecConfig, PlaneFormat, VtDecoderConfig};
use gmclip_media::backend::vt::{FrameReader, VtVideoDecoder};
use gmclip_media::decode::FrameImage;
use support::*;

const SEED: u64 = 0x676d_636c_6970; // "gmclip"

fn planes(f: &gmclip_media::DecodedFrame) -> gmclip_gpu::apple::CpuPlanes {
    match &f.image {
        FrameImage::Apple(pb) => pb.read_planes().unwrap(),
        other => panic!("expected a CVPixelBuffer, got {other:?}"),
    }
}

fn reader(
    path: &str,
    p: &VideoProbe,
    asynchronous: bool,
) -> FrameReader<FileSource, VtVideoDecoder> {
    assert_eq!(p.codec, "h264");
    let cfg = VtDecoderConfig {
        codec: CodecConfig::H264 {
            avcc: p.extradata.clone(),
        },
        width: p.width,
        height: p.height,
        output: PlaneFormat::Nv12Video,
        require_hardware: false,
        timescale: p.time_base.1 as i32,
    };
    FrameReader::new(FileSource::open(path, p.index.clone()), move || {
        VtVideoDecoder::new(&cfg, asynchronous)
    })
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// 200 seeded PTS visited in random order, each decoded frame-exactly and hashed.
fn seeded_seek_check(path: &str, label: &str) {
    seeded_seek_check_mode(path, label, false);
}

fn seeded_seek_check_mode(path: &str, label: &str, asynchronous: bool) {
    let p = probe(path, "v:0");
    let targets = seeded_pts(&p.index, 200, SEED);
    let golden = golden_md5s(path, label, &p, &targets);
    let golden: std::collections::HashMap<i64, (String, String)> = golden
        .into_iter()
        .map(|(pts, y, uv)| (pts, (y, uv)))
        .collect();
    let mut r = reader(path, &p, asynchronous);
    let mut times = Vec::new();
    let mut mismatches = Vec::new();
    for &pts in &targets {
        let t = Instant::now();
        let f = r
            .frame_at_pts(pts)
            .unwrap_or_else(|e| panic!("{label} pts {pts}: {e:?}"));
        times.push(t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(f.pts, pts);
        let pl = planes(&f);
        assert_eq!((pl.width, pl.height), (p.width, p.height));
        let got = (md5(&pl.y), md5(&pl.uv_planar()));
        if got != golden[&pts] {
            mismatches.push(pts);
        }
    }
    let hw = gmclip_media::VideoDecoder::caps(
        &VtVideoDecoder::new(
            &VtDecoderConfig {
                codec: CodecConfig::H264 {
                    avcc: p.extradata.clone(),
                },
                width: p.width,
                height: p.height,
                output: PlaneFormat::Nv12Video,
                require_hardware: false,
                timescale: p.time_base.1 as i32,
            },
            false,
        )
        .unwrap(),
    )
    .hardware;
    eprintln!(
        "H-02 {label} (async={asynchronous}): {}x{} hw={hw}, {} samples; 200 seeded PTS: {} md5 mismatches; seek p50 {:.1} ms, p95 {:.1} ms; {} sessions, {} samples submitted",
        p.width,
        p.height,
        p.index.len(),
        mismatches.len(),
        pct(&mut times.clone(), 0.5),
        pct(&mut times, 0.95),
        r.sessions_created,
        r.samples_submitted,
    );
    assert!(
        mismatches.is_empty(),
        "{label}: md5 mismatch at pts {mismatches:?}"
    );
}

/// Sequential decode of a whole short clip through the reorder stage, every frame
/// compared with ffmpeg.
fn sequential_check(path: &str, label: &str) {
    let p = probe(path, "v:0");
    let golden = golden_all_md5s(path, &p);
    assert_eq!(golden.len(), p.index.len(), "ffmpeg frame count");
    for asynchronous in [false, true] {
        let mut r = reader(path, &p, asynchronous);
        let mut got = Vec::new();
        let mut last_pts = i64::MIN;
        let t = Instant::now();
        r.decode_range(0, p.index.len(), |f| {
            assert!(f.pts > last_pts, "presentation order");
            last_pts = f.pts;
            let pl = planes(&f);
            got.push((md5(&pl.y), md5(&pl.uv_planar())));
            Ok(())
        })
        .unwrap();
        let dt = t.elapsed();
        let bad = got.iter().zip(&golden).filter(|(a, b)| a != b).count();
        eprintln!(
            "H-02 {label} sequential (async={asynchronous}): {} frames, {bad} mismatches, {:.0} fps incl. CPU readback+md5",
            got.len(),
            got.len() as f64 / dt.as_secs_f64()
        );
        assert_eq!(got.len(), golden.len());
        assert_eq!(bad, 0, "{label}: sequential md5 mismatches");
    }
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg (run with --features lab)"
)]
fn synthetic_bframes_1080p() {
    let path = synthetic_h264(
        "synthetic-h264-1080p-bf3-pyramid.mp4",
        1920,
        1080,
        20,
        false,
    );
    sequential_check(&path, "synthetic-1080p");
    seeded_seek_check(&path, "synthetic-1080p");
    seeded_seek_check_mode(&path, "synthetic-1080p", true);
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg (run with --features lab)"
)]
fn synthetic_bframes_open_gop() {
    let path = synthetic_h264("synthetic-h264-720p-bf3-opengop.mp4", 1280, 720, 20, true);
    sequential_check(&path, "synthetic-720p-opengop");
    seeded_seek_check(&path, "synthetic-720p-opengop");
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg + AJ's media"
)]
fn a_prime_1440p_trim() {
    let Some(path) = a_prime() else {
        eprintln!("{}", gmclip_model::lab::skip_line(mmo_1440p()));
        return;
    };
    seeded_seek_check(&path, "a-prime-1440p");
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg + AJ's media"
)]
fn mmo_1440p_full_source() {
    if gmclip_model::lab::skip_without([mmo_1440p()]) {
        return;
    }
    seeded_seek_check(mmo_1440p(), "mmo-1440p");
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg + AJ's media"
)]
fn proxies_720p() {
    seeded_seek_check(proxy_4fadb756(), "proxy-loc_4fadb756");
    seeded_seek_check(proxy_bd89f38b(), "proxy-loc_bd89f38b");
}

/// H-10's `FrameStream` (presentation-order access with forward decode and seeks):
/// runs, jumps forward and back, repeats, on B-pyramid and open-GOP clips; every
/// frame's md5 equals ffmpeg's.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + ffmpeg (run with --features lab)"
)]
fn frame_stream_matches_ffmpeg() {
    use gmclip_media::backend::FrameStream;
    for (name, w, h, open) in [
        ("synthetic-h264-1080p-bf3-pyramid.mp4", 1920, 1080, false),
        ("synthetic-h264-720p-bf3-opengop.mp4", 1280, 720, true),
    ] {
        let path = synthetic_h264(name, w, h, 20, open);
        let p = probe(&path, "v:0");
        let golden = golden_all_md5s(&path, &p);
        let cfg = VtDecoderConfig {
            codec: CodecConfig::H264 {
                avcc: p.extradata.clone(),
            },
            width: p.width,
            height: p.height,
            output: PlaneFormat::Nv12Video,
            require_hardware: false,
            timescale: p.time_base.1 as i32,
        };
        let mut s = FrameStream::new(FileSource::open(&path, p.index.clone()), move || {
            VtVideoDecoder::new(&cfg, true)
        });
        let mut plan: Vec<u64> = (0..100).collect();
        plan.extend(400..450);
        plan.extend(50..60);
        plan.extend([59, 59, 61]);
        plan.extend(119..125); // across a GOP boundary
        plan.extend(590..600);
        plan.extend([0, 599, 300]);
        let mut bad = 0;
        for &n in &plan {
            let f = s
                .frame(n)
                .unwrap_or_else(|e| panic!("{name} frame {n}: {e:?}"));
            let pl = planes(&f);
            if (md5(&pl.y), md5(&pl.uv_planar())) != golden[n as usize] {
                bad += 1;
            }
        }
        eprintln!(
            "H-10 FrameStream {name}: {} requests, {bad} md5 mismatches, {} seeks, {} samples submitted",
            plan.len(),
            s.seeks,
            s.submitted
        );
        assert_eq!(bad, 0);
    }
}

/// `FrameStream` timing on VideoToolbox (lane VT): per-request cost of a sequential read
/// with nothing between requests (export, proxy build), of one paced like playback
/// (8 ms of caller work between requests: decode can overlap it), and of random access;
/// with the flushes each one did. Three runs per row, median reported.
///
/// `cargo test -p gmclip-media --features lab --release --test vt_h264 frame_stream_timing -- --include-ignored --nocapture`
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + AJ's media (run with --features lab)"
)]
fn frame_stream_timing() {
    use gmclip_media::backend::FrameStream;
    let mut sources: Vec<(String, &str, u64)> = Vec::new();
    match a_prime() {
        Some(p) => sources.push((p, "a-prime 1440p (B-frames)", 600)),
        None => eprintln!("(a′) missing: skipped"),
    }
    if std::path::Path::new(proxy_4fadb756()).exists() {
        sources.push((
            proxy_4fadb756().to_owned(),
            "proxy 720p (no B-frames)",
            30_000,
        ));
    }
    assert!(!sources.is_empty(), "no lab source");
    let med = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    for (path, label, first) in sources {
        let p = probe(&path, "v:0");
        let open = || {
            let cfg = VtDecoderConfig {
                codec: CodecConfig::H264 {
                    avcc: p.extradata.clone(),
                },
                width: p.width,
                height: p.height,
                output: PlaneFormat::Nv12Video,
                require_hardware: false,
                timescale: p.time_base.1 as i32,
            };
            FrameStream::new(FileSource::open(&path, p.index.clone()), move || {
                VtVideoDecoder::new(&cfg, true)
            })
        };
        let n = p.index.len() as u64;
        // (row, per-request ms, flushes, wall s) per run
        type Row = (&'static str, Vec<Vec<f64>>, Vec<f64>, Vec<f64>);
        let mut rows: Vec<Row> = vec![
            ("sequential", vec![], vec![], vec![]),
            ("paced 8 ms", vec![], vec![], vec![]),
            ("random", vec![], vec![], vec![]),
        ];
        for run in 0..3u64 {
            for (k, row) in rows.iter_mut().enumerate() {
                let mut s = open();
                s.frame(first).unwrap(); // session up, chain started
                let f0 = s.flushes;
                let frames: Vec<u64> = match k {
                    0 => (first + 1..first + 601).collect(),
                    1 => (first + 1..first + 241).collect(),
                    _ => {
                        let mut z = 0x9e37_79b9_7f4a_7c15u64 ^ run;
                        (0..60)
                            .map(|_| {
                                z = z
                                    .wrapping_mul(6_364_136_223_846_793_005)
                                    .wrapping_add(1_442_695_040_888_963_407);
                                (z >> 33) % n
                            })
                            .collect()
                    }
                };
                let mut times = Vec::new();
                let t0 = Instant::now();
                for &f in &frames {
                    let t = Instant::now();
                    s.frame(f)
                        .unwrap_or_else(|e| panic!("{label} frame {f}: {e:?}"));
                    times.push(t.elapsed().as_secs_f64() * 1e3);
                    if k == 1 {
                        std::thread::sleep(std::time::Duration::from_millis(8));
                    }
                }
                row.3.push(t0.elapsed().as_secs_f64());
                row.2.push((s.flushes - f0) as f64);
                row.1.push(times);
            }
        }
        for (name, runs, flushes, wall) in rows {
            let reqs = runs[0].len();
            let p50 = med(runs.iter().map(|t| pct(&mut t.clone(), 0.5)).collect());
            let p95 = med(runs.iter().map(|t| pct(&mut t.clone(), 0.95)).collect());
            let max = med(runs.iter().map(|t| pct(&mut t.clone(), 1.0)).collect());
            let fps = med(runs
                .iter()
                .map(|t| t.len() as f64 / (t.iter().sum::<f64>() / 1e3))
                .collect());
            eprintln!(
                "VT FrameStream {label} {name}: {reqs} requests; per request p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.1} ms; {fps:.0} requests/s of decode wait; {:.0} flushes (median of 3; runs {:?}); wall {:.2} s",
                med(flushes.clone()),
                flushes,
                med(wall),
            );
        }
    }
}
