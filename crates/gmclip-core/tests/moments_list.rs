//! A2.03: `moments_list`, the moments panel's projection: filters (state, origin, media,
//! score), sorts (time, score, state), paging, unfiltered counts, and 1 000 moments
//! listed within the budget (≤ 50 ms in release, 7.8 ms measured in a debug build; debug gets 200 ms of slack).

mod common;

use std::time::Instant;

use common::{fixture, open};
use serde_json::{Value, json};

fn add(p: &common::Project, start: f64, score: u8, kind: &str) -> String {
    p.human(
        "moment_add",
        json!({"start_s": start, "end_s": start + 4.0, "quote": format!("words at {start}"),
               "score": score, "type": kind, "why": "test"}),
    )
    .output["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn ids(v: &Value) -> Vec<String> {
    v["moments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn filters_sorts_pages_and_counts() {
    let p = open("moments-list");
    let _fx = fixture(&p, 1);
    let a = add(&p, 30.0, 3, "result");
    let b = add(&p, 10.0, 5, "reaction");
    let c = add(&p, 20.0, 4, "story");
    p.human("moment_set_state", json!({"moment": b, "state": "staged"}));
    p.human(
        "moment_set_state",
        json!({"moment": c, "state": "archived"}),
    );

    let all = p.human("moments_list", json!({})).output;
    assert_eq!(all["total"], 3);
    assert_eq!(ids(&all), vec![b.clone(), c.clone(), a.clone()], "by time");
    assert_eq!(all["counts"], json!({"new": 1, "staged": 1, "archived": 1}));
    let row = &all["moments"][0];
    assert_eq!(row["start_s"], 10.0);
    assert_eq!(row["end_s"], 14.0);
    assert_eq!(row["type"], "reaction");
    assert_eq!(row["origin"], "manual");
    assert_eq!(row["state"], "staged");

    let by_score = p.human("moments_list", json!({"sort": "score"})).output;
    assert_eq!(ids(&by_score), vec![b.clone(), c.clone(), a.clone()]);
    let by_state = p.human("moments_list", json!({"sort": "state"})).output;
    assert_eq!(ids(&by_state), vec![a.clone(), b.clone(), c.clone()]);

    let open_ones = p
        .human("moments_list", json!({"states": ["new", "staged"]}))
        .output;
    assert_eq!(ids(&open_ones), vec![b.clone(), a.clone()]);
    assert_eq!(open_ones["total"], 2);
    // counts stay unfiltered: the chips show every state
    assert_eq!(open_ones["counts"]["archived"], 1);
    let good = p.human("moments_list", json!({"min_score": 4})).output;
    assert_eq!(ids(&good), vec![b.clone(), c.clone()]);
    let page = p
        .human("moments_list", json!({"offset": 1, "limit": 1}))
        .output;
    assert_eq!(ids(&page), vec![c.clone()]);
    assert_eq!(page["total"], 3);
    let none = p
        .human("moments_list", json!({"origins": ["agent"]}))
        .output;
    assert_eq!(none["total"], 0);
    let other = p
        .human(
            "moments_list",
            json!({"media": "01J00000000000000000000009"}),
        )
        .output;
    assert_eq!(other["total"], 0);

    // read-only: nothing committed, and it is not on the undo stack
    let ok = p.human("moments_list", json!({}));
    assert!(ok.committed.is_none());
    p.h.close().unwrap();
}

#[test]
fn a_thousand_moments_list_within_budget() {
    let p = open("moments-list-1000");
    let _fx = fixture(&p, 1);
    for i in 0..1000 {
        add(&p, 5.0 + i as f64 * 7.0, 2 + (i % 4) as u8, "result");
    }
    // warm, then the best of 5 (the bound is the projection, not a cold page cache)
    let _ = p.human("moments_list", json!({}));
    let mut best = std::time::Duration::MAX;
    for _ in 0..5 {
        let t0 = Instant::now();
        let v = p.human("moments_list", json!({"sort": "score"})).output;
        best = best.min(t0.elapsed());
        assert_eq!(v["total"], 1000);
        assert_eq!(v["moments"].as_array().unwrap().len(), 1000);
    }
    let bound = if cfg!(debug_assertions) { 200 } else { 50 };
    eprintln!("moments_list, 1 000 moments: {best:?} (bound {bound} ms)");
    assert!(best.as_millis() <= bound, "{best:?}");
    p.h.close().unwrap();
}
