//! Lane VT (lab): the export, the scrub proxy build and playback on VideoToolbox H.264,
//! timed for the decoder's look-ahead (`FrameStream`, `VideoDecoder::needs_input_to_output`).
//!
//! The source is (a′), the 3-minute 1440p30 H.264 trim with B-frames
//! (`a-prime-mmo-1440p-180s.mp4` in `$GMCLIP_LAB_MEDIA`, default `target/lab-media`; the
//! media crate's `vt_h264` lab cuts it). The plan is `schemas/examples/native-mode`,
//! stretched to 60 s: a live part, a part with the screen held, a jump to a later live
//! part. Three runs per row; the median is the row's number (`milestones/m2/perf.md`).
//!
//! `cargo test -p gmclip-render --release --features lab --test vt_perf_lab -- --include-ignored --nocapture --test-threads=1`
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gmclip_render::export::preview::PreviewSession;
use gmclip_render::export::proxy::{self, BuildOptions};
use gmclip_render::export::{DemuxInputs, ExportOptions, export};

mod a_prime_lab;

use a_prime_lab::{a_prime, plan};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn pct(v: &[f64], q: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gmclip-vt-perf-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Export throughput (decode, compose, encode, mux) of the 60 s plan.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + (a′) (run with --features lab)"
)]
fn export_fps() {
    let Some(src) = a_prime() else { return };
    let plan = plan(&src);
    let dir = scratch("export");
    let mut fps = Vec::new();
    for run in 0..3 {
        let rep = export(
            &plan,
            &mut DemuxInputs::new(None),
            &dir.join(format!("run{run}.mp4")),
            &ExportOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.frames, 1800);
        fps.push(rep.frames as f64 / rep.seconds);
        eprintln!(
            "VT export run {run}: {} frames in {:.2} s ({:.0} fps), {} video seeks",
            rep.frames,
            rep.seconds,
            rep.frames as f64 / rep.seconds,
            rep.video_seeks
        );
    }
    eprintln!(
        "VT export (a′) 60 s native-mode: median {:.0} fps (runs {:.0?})",
        median(fps.clone()),
        fps
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Scrub proxy build (decode 1440p, VideoToolbox-scaled encode at 640 × 360) of the whole
/// of (a′).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + (a′) (run with --features lab)"
)]
fn proxy_build_fps() {
    let Some(src) = a_prime() else { return };
    let key = proxy::content_key(&src).unwrap();
    let mut fps = Vec::new();
    for run in 0..3 {
        let dir = scratch("proxy");
        let r = proxy::build(&src, &dir, &key, &BuildOptions::default()).unwrap();
        assert_eq!(r.encoded, r.frames);
        fps.push(r.encoded as f64 / r.seconds);
        eprintln!(
            "VT proxy run {run}: {} frames in {:.2} s ({:.0} fps)",
            r.encoded,
            r.seconds,
            r.encoded as f64 / r.seconds
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    eprintln!(
        "VT proxy build (a′) 1440p → 640×360: median {:.0} fps (runs {:.0?})",
        median(fps.clone()),
        fps
    );
}

/// Playback: the preview session at a viewer's size (half the canvas), every frame of the
/// plan in order, ms per frame (30 fps needs < 33 ms).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox + Metal + (a′) (run with --features lab)"
)]
fn playback_frame_time() {
    let Some(src) = a_prime() else { return };
    let plan = Arc::new(plan(&src));
    let (mut p50, mut p95, mut max) = (Vec::new(), Vec::new(), Vec::new());
    for run in 0..3 {
        let mut s =
            PreviewSession::open(plan.clone(), &mut DemuxInputs::new(None), (540, 960)).unwrap();
        s.render(0, &[], &[]).unwrap();
        let mut times = Vec::new();
        let t0 = Instant::now();
        for f in 1..plan.output.frames {
            let t = Instant::now();
            s.render(f, &[], &[]).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let wall = t0.elapsed().as_secs_f64();
        eprintln!(
            "VT playback run {run}: p50 {:.2} ms, p95 {:.2} ms, max {:.1} ms per frame ({:.0} fps)",
            pct(&times, 0.5),
            pct(&times, 0.95),
            pct(&times, 1.0),
            times.len() as f64 / wall
        );
        p50.push(pct(&times, 0.5));
        p95.push(pct(&times, 0.95));
        max.push(pct(&times, 1.0));
    }
    eprintln!(
        "VT playback (a′) at 540×960: median p50 {:.2} ms, p95 {:.2} ms, max {:.1} ms",
        median(p50),
        median(p95),
        median(max)
    );
}
