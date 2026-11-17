//! H.05 in the app: opening a video queues its scrub proxy (built in the background into
//! the cache, keyed by content), progress shows in `proxy_status`, and the timeline's
//! filmstrip (`gm://data/<project>/filmstrip`) is read from it. Uses a finished reel from
//! the lab media (read only); skips without it.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn proxy_builds_and_feeds_the_filmstrip() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-proxy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let t0 = Instant::now();
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let job = loop {
        let st = c.proxies.status(Some(&pid));
        let j = st["jobs"][0].clone();
        if j["phase"] == "done" || j["phase"] == "failed" {
            break j;
        }
        assert!(t0.elapsed() < Duration::from_secs(180), "proxy: {st}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(job["phase"], "done", "{job}");
    let frames = job["frames"].as_u64().unwrap();
    assert_eq!(job["covered"], frames);
    eprintln!(
        "proxy of h79-v4: {frames} frames in {:.1} s",
        job["seconds"]
    );
    // in the cache, under the project, keyed by content
    let dir = c.proxies.dir_of(job["path"].as_str().unwrap()).unwrap();
    assert!(dir.starts_with(root.join("app/cache").join(&pid).join("proxies")));
    let m = gmclip_render::export::proxy::Manifest::read(&dir).unwrap();
    assert!(m.complete);
    assert_eq!((m.w, m.gop), (640, 4));
    assert_eq!(m.source_frames, frames);

    // The filmstrip: one thumbnail per 30 output frames, all present, 96 px wide.
    let pl = c.planned(&pid, &cut).unwrap();
    let t = Instant::now();
    let body = gmclip_app::ipc::gm_data(
        &c,
        &format!("gm://data/{pid}/filmstrip?cut={cut}&every=30&w=96"),
    )
    .expect("a filmstrip");
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(u16::from_le_bytes([body[2], body[3]]), 3);
    let count = u32::from_le_bytes(body[4..8].try_into().unwrap()) as u64;
    assert_eq!(count, pl.plan.output.frames.div_ceil(30));
    let mut at = 16;
    let mut sizes = std::collections::BTreeSet::new();
    for k in 0..count {
        let f = u32::from_le_bytes(body[at..at + 4].try_into().unwrap()) as u64;
        let w = u16::from_le_bytes([body[at + 4], body[at + 5]]) as usize;
        let h = u16::from_le_bytes([body[at + 6], body[at + 7]]) as usize;
        assert_eq!(f, k * 30);
        assert!(w > 0 && h > 0, "thumbnail {k} missing");
        sizes.insert((w, h));
        at += 8 + w * h * 3;
    }
    assert_eq!(at, body.len());
    assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![(30, 53)]);
    eprintln!(
        "filmstrip: {count} thumbnails in {ms:.0} ms ({} KB)",
        body.len() / 1024
    );
    // a second ask is served from the thumbnail cache
    let t = Instant::now();
    let again = gmclip_app::ipc::gm_data(
        &c,
        &format!("gm://data/{pid}/filmstrip?cut={cut}&every=30&w=96"),
    )
    .unwrap();
    assert_eq!(again, body);
    eprintln!("filmstrip again: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    // only bounded integers, only on the filmstrip
    for bad in [
        format!("gm://data/{pid}/filmstrip?cut={cut}&every=0"),
        format!("gm://data/{pid}/filmstrip?cut={cut}&w=4096"),
        format!("gm://data/{pid}/parts?cut={cut}&every=30"),
    ] {
        assert!(gmclip_app::ipc::gm_data(&c, &bad).is_none(), "{bad}");
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
