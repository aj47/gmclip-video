//! H.05 lab (SPEC §6.3, §13): the scrub proxy and the exact-seek gates, measured through
//! the preview session the viewer uses, on AJ's sources (a) and (b) from
//! `test-media/manifest.toml`.
//!
//! - `proxy_build_a`: the whole proxy of (a) (2 h 1440p30 H.264): build time (≤ 10 min),
//!   frames, size; every segment readable at its first and last frame.
//! - `scrub_and_exact_seek_{a,b}`: `gmclip-app perf --scrub` (`gmclip_app::perf::scrub`,
//!   what AJ.02 runs on the Air): a proxy over the first three minutes, a scrub drag across
//!   them (distinct frames per second, ≥ 20), keyframe-only scrubbing past the proxy, and
//!   100 seeded exact seeks across the whole source (p50/p95 of the targets in the GOP
//!   class: ≤ 2 s → ≤ 60/150 ms, ≤ 5 s → ≤ 150/400 ms).
//!
//! `cargo test --test scrub_lab -- --ignored --nocapture --test-threads=1`
//! (dev profile: the gmclip crates are optimised). Dev-machine numbers; the gate run is
//! AJ's on the Air (AJ.02).
#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::time::Instant;

use gmclip_render::export::proxy::{self, BuildOptions, ProxyReader};

/// `streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4` in the lab media folder.
fn a() -> &'static str {
    static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        gmclip_model::lab::media_str(
            "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
        )
    });
    P.as_str()
}
/// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
fn b() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv"));
    P.as_str()
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gmclip-scrub-lab-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[ignore = "lab: needs VideoToolbox + AJ's source (a); builds the whole proxy (~4 min)"]
fn proxy_build_a() {
    if gmclip_model::lab::skip_without([a()]) {
        return;
    }
    let dir = scratch("a");
    let t = Instant::now();
    let key = proxy::content_key(Path::new(a())).unwrap();
    let key_ms = t.elapsed().as_secs_f64() * 1e3;
    let r = proxy::build(Path::new(a()), &dir, &key, &BuildOptions::default()).unwrap();
    let m = proxy::Manifest::read(&dir).unwrap();
    assert!(m.complete);
    assert_eq!(m.covered(), r.frames);
    // every segment opens and decodes its first and last frame
    let mut rd = ProxyReader::open(&dir).unwrap();
    for seg in 0..m.segments() {
        let f0 = seg * m.seg_frames;
        let f1 = ((seg + 1) * m.seg_frames).min(m.source_frames) - 1;
        for f in [f0, f1] {
            rd.frame(f)
                .unwrap_or_else(|e| panic!("proxy frame {f}: {e}"));
        }
    }
    let secs = r.frames as f64 / f64::from(m.fps);
    eprintln!(
        "H.05 proxy (a): {} frames ({:.0} min of source) → {}x{} GOP {} in {:.1} s ({:.0} fps, {:.1}× real time); {} segments, {:.0} MB ({:.0} kbps); content key {:.0} ms",
        r.frames,
        secs / 60.0,
        r.size.0,
        r.size.1,
        m.gop,
        r.seconds,
        r.frames as f64 / r.seconds,
        secs / r.seconds,
        m.segments(),
        r.bytes as f64 / 1e6,
        r.bytes as f64 * 8.0 / secs / 1e3,
        key_ms
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(r.seconds <= 600.0, "proxy for (a) took {:.0} s", r.seconds);
}

fn scrub_and_seek(path: &str, label: &str) {
    if gmclip_model::lab::skip_without([path]) {
        return;
    }
    let r = gmclip_app::perf::scrub(Path::new(path), &Default::default()).unwrap();
    eprintln!(
        "H.05 {label}: {}",
        serde_json::to_string_pretty(&r).unwrap()
    );
    let f = |p: &str| r.pointer(p).and_then(serde_json::Value::as_f64).unwrap();
    assert_eq!(
        f("/scrub/not_proxy"),
        0.0,
        "{label}: scrub frames not from the proxy"
    );
    assert_eq!(f("/keyframe_only/not_keyframe"), 0.0, "{label}");
    assert!(f("/scrub/frames_per_s") >= 20.0, "{label}: scrub {r}");
    let (p50, p95) = (f("/exact_seek/p50_ms"), f("/exact_seek/p95_ms"));
    let (b50, b95) = (f("/exact_seek/budget_ms/0"), f("/exact_seek/budget_ms/1"));
    assert!(
        p50 <= b50 && p95 <= b95,
        "{label}: exact seek p50 {p50:.1} / p95 {p95:.1} ms over {b50}/{b95}"
    );
}

#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's sources (a) and (b)"]
fn scrub_and_exact_seek_a() {
    scrub_and_seek(a(), "a");
}

#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's sources (a) and (b)"]
fn scrub_and_exact_seek_b() {
    scrub_and_seek(b(), "b");
}
