//! L.04: in and out tapped on a cut's output frames become a moment by the human whose
//! edges are the nearest sentence start and end — a tap a beat late or early lands on the
//! sentence it meant — across two parts of the cut (the times map through the parts).

use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;

#[test]
fn taps_snap_to_sentences_through_the_cut() {
    let root = std::env::temp_dir().join(format!("gmclip-tap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c: Arc<AppCore> = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let p = c.create_project("Taps").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let src = c
        .call(&pid, "source_add", json!({"path": "/nowhere/s.mp4", "fingerprint": "sha256:00",
                                         "probe": {"duration": 600 * SEC, "tracks": [], "audio_tracks": []}}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // a sentence every 4 s: "Line n goes here." (words at +0, +0.5, +1, +1.5 s)
    let words: Vec<Value> = (0..150)
        .flat_map(|n| {
            ["Line", &format!("{n}"), "goes", "here."]
                .into_iter()
                .enumerate()
                .map(move |(k, w)| {
                    let s = ((n as f64 * 4.0 + k as f64 * 0.5) * SEC as f64) as i64;
                    json!({"text": w, "start": s, "end": s + SEC * 4 / 10})
                })
                .collect::<Vec<_>>()
        })
        .collect();
    c.project(&pid)
        .unwrap()
        .handle
        .put_analysis(
            &src,
            "words",
            "r1",
            serde_json::to_vec(&json!({"revision": "r1", "words": words})).unwrap(),
        )
        .unwrap();
    // a cut of two parts: 100–120 s then 200–220 s (output 0–20 s, 20–40 s)
    let cut = c
        .call(&pid, "cut_create", json!({"title": "two parts"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let p1 = c
        .call(&pid, "part_add", json!({"cut": cut, "media": {"source": src}, "src_in": 100 * SEC, "src_out": 120 * SEC}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(&pid, "part_add", json!({"cut": cut, "after": p1, "media": {"source": src}, "src_in": 200 * SEC, "src_out": 220 * SEC}))
        .unwrap();
    // in at output 4.9 s (source 104.9: a beat after "Line 26" began at 104 s), out at
    // output 9.1 s (source 109.1: just past "Line 27 goes here." ending at 109.9 → its end)
    let r = gmclip_app::tapmark::mark(
        &c,
        &pid,
        &json!({"cut": cut, "in_frame": 147, "out_frame": 273}),
    )
    .unwrap();
    let m = &r["moment"];
    assert_eq!(m["start_s"], 104.0, "{r}");
    assert!((m["end_s"].as_f64().unwrap() - 109.9).abs() < 1e-6, "{r}");
    assert_eq!(m["quote"], "Line 26 goes here. Line 27 goes here.");
    // in and out in the second part map through it (output 21 s / 25 s = source 201 / 205 s)
    let r = gmclip_app::tapmark::mark(
        &c,
        &pid,
        &json!({"cut": cut, "in_frame": 630, "out_frame": 750}),
    )
    .unwrap();
    assert_eq!(r["moment"]["start_s"], 200.0, "{r}");
    assert!(
        (r["moment"]["end_s"].as_f64().unwrap() - 205.9).abs() < 1e-6,
        "{r}"
    );
    // out before in: refused
    assert!(
        gmclip_app::tapmark::mark(
            &c,
            &pid,
            &json!({"cut": cut, "in_frame": 300, "out_frame": 200})
        )
        .is_err()
    );
    let rows = gmclip_app::review::moments_list(&c.project(&pid).unwrap(), &c.transcripts);
    assert_eq!(rows["moments"].as_array().unwrap().len(), 2);
    assert!(
        rows["moments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["origin"] == "manual" && m["type"] == "marked")
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
