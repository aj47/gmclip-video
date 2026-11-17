//! Decision 0024, hook text with measured numbers: `hook_text_suggest` builds 5–8 word
//! lines on numbers the clip measurably shows (a measured run, the count of runs, a
//! number said), each with its source and the provenance to record; never a number the
//! clip does not show, and a spoken duration the measured run contradicts comes last,
//! flagged.

mod common;

use common::{Project, SEC, fixture, open};
use gmclip_core::lint::lint_cut;
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};

fn words(p: &Project, owner: &str, ws: &[(&str, f64, f64)]) {
    let rev = "asr-test@1";
    let words: Vec<Value> = ws
        .iter()
        .map(|(t, s, e)| json!({"text": t, "start": (s * SEC as f64) as i64, "end": (e * SEC as f64) as i64}))
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": words});
    p.h.put_analysis(owner, "words", rev, body.to_string().into_bytes())
        .unwrap();
}

fn run(p: &Project, src: &str, from_frame: i64, to_frame: i64, secs: f64) -> String {
    p.human(
        "measurement_add",
        json!({"kind": "interval", "from": {"frame": {"source": src, "frame_index": from_frame}},
               "to": {"frame": {"source": src, "frame_index": to_frame}}, "value": secs, "unit": "s",
               "evidence_frames": [{"source": src, "frame_index": from_frame}, {"source": src, "frame_index": to_frame}],
               "method": "start and end frames marked on the preview", "author": "local:human"}),
    )
    .output["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn texts(v: &Value) -> Vec<String> {
    v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["text"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn lines_on_measured_runs_counts_and_spoken_numbers() {
    let p = open("hooktext");
    let fx = fixture(&p, 2); // 100–102 s and 103–105 s
    // nothing to build on yet
    let none = p.human("hook_text_suggest", json!({"cut": fx.cut})).output;
    assert!(none["suggestions"].as_array().unwrap().is_empty());
    assert!(none["note"].as_str().unwrap().contains("no transcript"));

    words(
        &p,
        &fx.source,
        &[
            ("So", 100.1, 100.2),
            ("we", 100.25, 100.3),
            ("generated", 100.35, 100.6),
            ("that", 100.65, 100.7),
            ("video", 100.75, 100.9),
            ("in", 100.95, 101.0),
            ("less", 101.05, 101.2),
            ("than", 101.25, 101.3),
            ("two", 101.35, 101.5),
            ("minutes.", 101.55, 101.9),
            // a part the cut does not play: never quoted
            ("ninety", 102.2, 102.4),
            ("seconds", 102.5, 102.8),
            ("It", 103.1, 103.2),
            ("rendered", 103.3, 103.6),
            ("three", 103.7, 103.9),
            ("clips", 104.0, 104.3),
            ("for", 104.35, 104.4),
            ("me.", 104.45, 104.6),
        ],
    );
    let only_spoken = p.human("hook_text_suggest", json!({"cut": fx.cut})).output;
    assert_eq!(
        texts(&only_spoken),
        vec![
            "Generated that video in less than *2 minutes*.",
            "It rendered *3 clips* for me.",
        ],
        "{only_spoken}"
    );
    let s0 = &only_spoken["suggestions"][0];
    assert_eq!(s0["kind"], "spoken");
    assert_eq!(s0["number"], "2");
    assert!(
        s0["source"].as_str().unwrap().starts_with("said at 0:01.4"),
        "{s0}"
    );
    assert_eq!(s0["provenance"][0]["text"], "2");
    assert!(s0["provenance"][0]["measurement"].is_null());
    assert!(s0["warning"].is_null());

    // a measured run (67.4 s) and a second one (41.2 s): measured lines first, the count
    // of runs, and "two minutes" flagged (the screen says 67 s) and ranked last
    let m1 = run(&p, &fx.source, 3000, 5022, 67.4);
    let m2 = run(&p, &fx.source, 6000, 7236, 41.2);
    let all = p
        .human("hook_text_suggest", json!({"cut": fx.cut, "limit": 20}))
        .output;
    let t = texts(&all);
    assert_eq!(
        t,
        vec![
            "Watch it finish in *67 seconds*",
            "Watch it finish in *41 seconds*",
            "2 runs, the fastest in *41 seconds*",
            "It rendered *3 clips* for me.",
            "Generated that video in less than *2 minutes*.",
            "Start to finish: *67 seconds*, measured",
            "Start to finish: *41 seconds*, measured",
        ],
        "{all}"
    );
    assert_eq!(all["measured_runs"], 2);
    let first = &all["suggestions"][0];
    assert_eq!(first["kind"], "measured");
    assert_eq!(first["measurement"], m1.as_str());
    assert_eq!(first["provenance"][0]["measurement"], m1.as_str());
    let count = &all["suggestions"][2];
    assert_eq!(count["kind"], "count");
    assert_eq!(count["provenance"][1]["measurement"], m2.as_str());
    let flagged = &all["suggestions"][4];
    assert!(
        flagged["warning"].as_str().unwrap().contains("67.4 s"),
        "{flagged}"
    );
    for s in all["suggestions"].as_array().unwrap() {
        let n = s["text"].as_str().unwrap().split_whitespace().count();
        assert!((5..=8).contains(&n), "{s}");
    }

    // an agent that puts the measured line on screen with its provenance passes the
    // number lint (R-AI-4); the same line without it does not
    let style = json!({"font": "neometric-heavy", "size": 88.0, "fill": "#ffffff", "outline": null, "shadow": null,
        "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null});
    let add = |prov: Value| {
        p.agent(
            "a",
            "overlay_add",
            json!({"cut": fx.cut, "kind": "text", "role": "text_hook", "band": "hook",
                   "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 4000},
                   "place": {"x": 0.5, "y": 0.16, "w": 0.9},
                   "text": {"runs": [{"text": "Watch it finish in 67 seconds"}], "style": style},
                   "number_provenance": prov}),
            p.as_of(),
        )
        .unwrap()
    };
    add(first["provenance"].clone());
    let cut: Id<gmclip_model::entities::Cut> = Id::new(Ulid::parse(&fx.cut).unwrap());
    let rules: Vec<String> = lint_cut(&p.h.snapshot().doc, cut)
        .into_iter()
        .map(|f| f.rule)
        .collect();
    assert!(!rules.iter().any(|r| r.starts_with("number_")), "{rules:?}");
    add(json!([]));
    let rules: Vec<String> = lint_cut(&p.h.snapshot().doc, cut)
        .into_iter()
        .map(|f| f.rule)
        .collect();
    assert!(rules.iter().any(|r| r == "number_without_provenance"));
}
