//! R.01: accurate mask tracks build in the background on their own. Turning a cut-out on
//! (one `matte_set`, no build button) starts the build once the project has been still
//! for a second; `matte_progress` reports per matted part how many of its frames have a
//! track (the timeline's ticks), growing while it builds. A cut without a cut-out never
//! starts one. Lab: Apple Vision and a real video.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::{Layer, LayerRole, RegionRole};
use serde_json::{Value, json};

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

fn progress(c: &AppCore, pid: &str, cut: &str) -> Value {
    gmclip_app::roto::progress(c, pid, cut).unwrap()
}

#[test]
fn cut_outs_build_their_tracks_on_their_own() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-rotoauto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();

    // edits without a cut-out start nothing
    std::thread::sleep(Duration::from_millis(1600));
    let p0 = progress(&c, &pid, &cut);
    assert!(p0["build"].is_null(), "{p0}");
    assert_eq!(p0["parts"], json!([]));

    // the cam cut out: one command, no build button
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let cam = doc
        .iter::<Layer>()
        .find(|l| l.role == LayerRole::Region(RegionRole::Cam))
        .unwrap()
        .id;
    let t0 = Instant::now();
    c.call(
        &pid,
        "matte_set",
        json!({"layer": cam.0.to_string(), "matte": {"roto": {}}}),
    )
    .unwrap();
    // nothing yet while it settles…
    let p = progress(&c, &pid, &cut);
    assert!(p["build"].is_null(), "{p}");
    let parts = p["parts"].as_array().unwrap().clone();
    assert!(!parts.is_empty());
    for x in &parts {
        assert_eq!(x["done"], 0);
        assert!(x["total"].as_u64().unwrap() > 0);
    }
    // … then it starts on its own and its ticks grow
    let mut started = None;
    let mut seen = Vec::new();
    while t0.elapsed() < Duration::from_secs(60) {
        let p = progress(&c, &pid, &cut);
        if !p["build"].is_null() && started.is_none() {
            started = Some(t0.elapsed());
            assert_eq!(p["build"]["auto"], true);
        }
        let done: u64 = p["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["done"].as_u64().unwrap())
            .sum();
        if seen.last() != Some(&done) {
            seen.push(done);
        }
        if seen.len() >= 3 || p["build"]["state"] == "done" {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let started = started.expect("a background build");
    eprintln!("started {started:?} after the matte_set; part ticks {seen:?}");
    assert!(
        started >= Duration::from_millis(900),
        "{started:?}: after it settles"
    );
    assert!(started < Duration::from_secs(5), "{started:?}");
    assert!(
        seen.len() >= 2 && seen.windows(2).all(|w| w[0] < w[1]),
        "{seen:?}"
    );
    // quitting stops it (the build is cancelled between frames)
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
