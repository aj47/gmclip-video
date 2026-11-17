//! Decision 0052: an agent sees the markers dropped while recording where its cut plays
//! them, in `cut_get` and through `marker_list`.

mod common;

use common::World;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cut_get_shows_the_recording_markers_on_the_cut_clock() {
    let w = World::start().await;
    // parts play 100–102 s and 103–105 s of the source
    let fx = w.fixture("m.gmclip", 2, "m");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let ov = c.ok("project_overview", json!({})).await;
    let source = ov["sources"][0]["id"].as_str().unwrap().to_owned();
    let before = c.ok("cut_get", json!({"cut": fx.cut})).await;
    assert!(before.get("markers").is_none(), "{before}");

    let as_of = ov["as_of"].as_u64().unwrap();
    c.ok(
        "command_run",
        json!({"command": "source_markers_set", "base": {"as_of": as_of},
               "args": {"source": source, "markers_ms": [50_000, 101_500, 104_250]}}),
    )
    .await;
    let got = c.ok("cut_get", json!({"cut": fx.cut})).await;
    assert_eq!(
        got["markers"],
        json!([{"n": 2, "cut_s": 1.5, "part": fx.parts[0]}, {"n": 3, "cut_s": 3.25, "part": fx.parts[1]}]),
        "{got}"
    );
    assert_eq!(got["markers_total"], 2);
    let l = c
        .ok(
            "command_run",
            json!({"command": "marker_list", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(l["output"]["markers"][1]["src_ms"], 104_250, "{l}");
    assert_eq!(l["output"]["markers"][1]["cut_ms"], 3250, "{l}");
}
