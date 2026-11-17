//! H.10 lab: moment clips on a stream's project fixture (loc_4fadb756,
//! picture (b) — the 1440p VP9 MKV — with its 720p proxy's sound). A 30 s moment:
//! rendered from the master before the scrub proxy reaches it, from the proxy once it
//! does (≤ 2 s, SPEC §5.14.1 / backlog H.10), and reused from the cache after that. `cargo test --test clips -- --ignored --nocapture`.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::Moment;
use serde_json::{Value, json};

fn probe_frames(p: &Path) -> (u64, u32, u32) {
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let v = inputs.video_file(p, "clip").unwrap();
    use gmclip_media::backend::SampleSource;
    (v.source.index().len() as u64, v.width, v.height)
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + the lab media and source (b)"]
fn moment_clips_render_from_the_proxy_and_cache() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-clips-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    eprintln!(
        "fixture: {} ({} moments)",
        p["fixture"],
        doc.iter::<Moment>().count()
    );
    let media: Value = serde_json::to_value(doc.iter::<Moment>().next().unwrap().media).unwrap();
    // a 30 s moment early in the stream (the proxy reaches it in its second segment)
    let add = |start: f64, end: f64| -> String {
        let r = c
            .call(
                &pid,
                "moment_add",
                json!({"start_s": start, "end_s": end, "quote": "no such words in this span",
                       "score": 3, "type": "result", "why": "H.10 lab", "media": media}),
            )
            .unwrap();
        r["output"]["id"].as_str().unwrap().to_owned()
    };
    let m30 = add(40.0, 69.0); // span + 1 s = 30 s
    // 1. Before any proxy: from the master, scaled into 1280 × 720.
    let t = Instant::now();
    let (rec, mp4, rendered) = gmclip_app::clips::ensure(&c, &pid, &m30).unwrap();
    let master_ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(rendered);
    assert_eq!(rec.picture, "master");
    assert!(rec.w <= 1280 && rec.h <= 720, "{rec:?}");
    assert_eq!(rec.frames, 900, "{rec:?}");
    let (n, w, h) = probe_frames(&mp4);
    assert_eq!((n, w, h), (900, rec.w, rec.h));
    eprintln!(
        "H.10 30 s clip from the master (1440p VP9 → {}x{}): {master_ms:.0} ms, {} KB",
        rec.w,
        rec.h,
        rec.size / 1024
    );
    // cached: no render
    let t = Instant::now();
    let (again, _, rendered) = gmclip_app::clips::ensure(&c, &pid, &m30).unwrap();
    assert!(!rendered);
    assert_eq!(again, rec);
    eprintln!(
        "H.10 cached clip reused: {:.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    // 2. The scrub proxy of the picture source, until it covers the moment.
    c.proxies.ensure(&c, &pid);
    let t0 = Instant::now();
    let pic_path = loop {
        let st = c.proxies.status(Some(&pid));
        let ready = st["jobs"].as_array().unwrap().iter().find(|j| {
            j["path"].as_str().is_some_and(|p| p.ends_with(".mkv"))
                && j["covered"].as_u64().unwrap_or(0) >= 3 * 1800
        });
        if let Some(j) = ready {
            break j["path"].as_str().unwrap().to_owned();
        }
        assert!(t0.elapsed() < Duration::from_secs(600), "proxy: {st}");
        std::thread::sleep(Duration::from_millis(250));
    };
    eprintln!(
        "proxy of {pic_path} covers 3 segments after {:.0} s",
        t0.elapsed().as_secs_f64()
    );
    // a new span → a new clip, now from the proxy
    let m30p = add(45.0, 74.0);
    let mut times = Vec::new();
    for _ in 0..3 {
        // re-render each time: drop the cached clip
        let dir = gmclip_app::clips::clip_dir(&c, &pid);
        let _ = std::fs::remove_file(dir.join(format!("{m30p}.clip.json")));
        let t = Instant::now();
        let (rec, mp4, rendered) = gmclip_app::clips::ensure(&c, &pid, &m30p).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1e3);
        assert!(rendered);
        assert_eq!(rec.picture, "proxy", "{rec:?}");
        // the clip starts on the proxy keyframe at or before the first frame: ≤ 3 more
        assert_eq!((rec.w, rec.h), (640, 360));
        assert!((900..=903).contains(&rec.frames), "{rec:?}");
        assert_eq!(probe_frames(&mp4).0, rec.frames);
    }
    times.sort_by(f64::total_cmp);
    eprintln!(
        "H.10 30 s clip from the proxy (640x360): median {:.0} ms of 3 ({:?})",
        times[1], times
    );
    assert!(gmclip_app::clips::cached(&c, &pid, &m30).is_some());
    c.shutdown();
    if std::env::var_os("GMCLIP_LAB_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    } else {
        eprintln!("kept {}", root.display());
    }
    assert!(times[1] <= 2000.0, "30 s clip took {:.0} ms", times[1]);
}
