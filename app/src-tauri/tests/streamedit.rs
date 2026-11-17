//! Decision 0011 through the app core on an opened video: the video is the project's
//! stream edit (adopted on open), the project map with export states, a long-form from a
//! topic with its diff, a second 9:16 framing picked as the active one. Skips without
//! the test video.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}
const SEC: i64 = 705_600_000;

fn word(n: i64, t: i64) -> Value {
    json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": t, "t_src_end": t + SEC / 4, "normalised_text": "w"})
}

#[test]
fn stream_edit_map_long_form_and_framings() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-streamedit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    c.analysis.cancel_project(v["project"].as_str().unwrap());
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    // the opened video is the stream edit
    assert_eq!(c.cut_view(&pid, &cut).unwrap()["kind"], "stream");
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    assert_eq!(m["stream"]["cut"], cut.as_str());
    assert_eq!(m["stream"]["export"]["state"], "never");

    // a topic (word fingerprints: no transcript in this test) and a long-form from it
    let t = c
        .call(
            &pid,
            "topic_set",
            json!({"start": word(10, 5 * SEC), "end": word(60, 25 * SEC), "title": "Why a VM"}),
        )
        .unwrap()["output"]["topic"]
        .as_str()
        .unwrap()
        .to_owned();
    let lf = c
        .call(&pid, "longform_from_topics", json!({"topics": [t]}))
        .unwrap()["output"]
        .clone();
    let long = lf["cut"].as_str().unwrap().to_owned();
    assert_eq!(c.cut_view(&pid, &long).unwrap()["kind"], "long");
    assert!(c.planned(&pid, &long).is_ok(), "the long-form plans");
    // the stream edit strikes inside the topic: one edit waits in the map
    c.call(
        &pid,
        "words_strike",
        json!({"cut": cut, "words": [word(20, 10 * SEC), word(24, 12 * SEC)]}),
    )
    .unwrap();
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    assert_eq!(m["longs"][0]["parent_edits"], 1);
    assert_eq!(m["longs"][0]["export"]["state"], "never");

    // a second 9:16 framing on the long-form, picked as the one shown
    gmclip_app::framing::set_output(&c, &pid, &long, "9:16").unwrap();
    let f = c
        .call(
            &pid,
            "framing_add",
            json!({"cut": long, "name": "face big", "aspect": "9:16"}),
        )
        .unwrap()["output"]["framing"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::streamedit::framing_pick(&c, &pid, &long, &f).unwrap();
    let cv = c.cut_view(&pid, &long).unwrap();
    assert_eq!(cv["framing"]["active"], f.as_str());
    assert_eq!(cv["canvas"], json!({"w": 1080, "h": 1920}));
    // the overlay of a cut with no transcript is empty, not an error
    let o = gmclip_app::streamedit::stream_overlay(&c, &pid, &cut).unwrap();
    assert_eq!(o["stream"], true);
    let _ = std::fs::remove_dir_all(&root);
}
