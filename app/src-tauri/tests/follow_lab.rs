//! QA lab (milestones/m2/qa-e2e.md, W6): "Follow the action" on a part 43 minutes into
//! AJ's 51-minute stream preview, and on the whole stream as one part (it read 6,000
//! frames, minutes with no word in the app; now at most 240). Read only; skips without it.
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::time::Instant;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// `preview/loc_4fadb756.mp4` in the lab media folder.
fn stream() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_4fadb756.mp4"));
    P.as_str()
}
const SEC: i64 = 705_600_000;

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: AJ's stream preview"]
fn follow_the_action_late_in_a_long_stream() {
    if !Path::new(stream()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-follow-lab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exp"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(stream())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    let stream = v["cut"].as_str().unwrap().to_owned();
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let src = doc
        .iter::<gmclip_model::entities::Source>()
        .next()
        .unwrap()
        .id;
    let media = json!({"source": src.0.to_string()});
    let _ = stream;
    let cut = c
        .call(&pid, "cut_create", json!({"title": "late"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": media, "src_in": 2610 * SEC, "src_out": 2640 * SEC}),
    )
    .unwrap();
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let f = c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "16:9", "w": 1280, "h": 720, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.set_active_framing(&pid, &cut, &f).unwrap();
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();
    let t = Instant::now();
    let r = gmclip_app::roto::follow_action(&c, &pid, &cut, None, true).unwrap();
    let s = t.elapsed().as_secs_f64();
    eprintln!("follow: {s:.1} s: {r}");
    assert!(s < 20.0, "follow the action on a 30 s part took {s:.1} s");
    // the whole 51-minute stream as one part: sampled sparsely, still an answer in seconds
    let whole = c
        .call(&pid, "cut_create", json!({"title": "whole"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": whole, "media": media, "src_in": 0, "src_out": 3090 * SEC}),
    )
    .unwrap();
    c.call(&pid, "caption_set", json!({"cut": whole, "enabled": false}))
        .unwrap();
    let f = c.call(&pid, "framing_create", json!({"cut": whole, "name": "16:9", "w": 1280, "h": 720, "fps": {"num": 30, "den": 1}})).unwrap()["output"]["id"].as_str().unwrap().to_owned();
    c.set_active_framing(&pid, &whole, &f).unwrap();
    gmclip_app::framing::set_output(&c, &pid, &whole, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &whole, &[], "screen_cam").unwrap();
    let t = Instant::now();
    let r = gmclip_app::roto::follow_action(&c, &pid, &whole, None, true).unwrap();
    let s = t.elapsed().as_secs_f64();
    eprintln!(
        "follow, whole stream: {s:.1} s: {}",
        r.to_string().chars().take(300).collect::<String>()
    );
    assert!(
        s < 60.0,
        "follow the action on a 51-minute part took {s:.1} s"
    );
    c.shutdown();
}
