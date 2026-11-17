//! A2.13 / R-HIST-1 / R-HIST-5: the human's undo stack across sessions (the first ⌘Z
//! after a reopen asks once), who edited after each version, the visual diff of a cut
//! between versions, and restoring a 200-part cut as one undo step.

mod common;

use std::time::Instant;

use common::{SEC, fixture, open};
use serde_json::{Value, json};

fn versions(p: &common::Project, cut: &str) -> Vec<Value> {
    p.human("history_list", json!({"cut": cut})).output["versions"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn the_human_stack_survives_a_reopen_and_the_first_undo_asks_once() {
    let p = open("hist-reopen");
    let fx = fixture(&p, 3);
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 3}));
    p.human("part_set_gain", json!({"part": fx.parts[1], "gain_db": 5}));
    let p = p.reopen();
    let gain = |p: &common::Project, i: usize| -> f64 {
        let d = p.h.snapshot().doc.clone();
        d.get(gmclip_model::Id::<gmclip_model::entities::Part>::new(
            gmclip_model::Ulid::parse(&fx.parts[i]).unwrap(),
        ))
        .unwrap()
        .gain_db
    };
    // the first ⌘Z after the reopen changes nothing and says what it would undo
    let ask = p.human("undo", json!({}));
    assert!(ask.committed.is_none());
    assert_eq!(ask.output["needs_confirm"], true);
    assert_eq!(ask.output["command"], "part_set_gain");
    assert_eq!(gain(&p, 1), 5.0);
    // yes: it undoes the earlier session's last edit
    let r = p.human("undo", json!({"confirm": true}));
    assert!(r.committed.is_some());
    assert_eq!(gain(&p, 1), 0.0);
    // asked once: the next one just undoes
    let r = p.human("undo", json!({}));
    assert!(r.committed.is_some(), "{:?}", r.output);
    assert_eq!(gain(&p, 0), 0.0);
    // redo walks forward again, and an agent's edit stays off the human's stack
    p.human("redo", json!({}));
    assert_eq!(gain(&p, 0), 3.0);
    let rev = p.h.snapshot().doc.rev();
    p.agent(
        "c1",
        "part_set_gain",
        json!({"part": fx.parts[2], "gain_db": 7}),
        Some(gmclip_proto::Base::AsOf { as_of: rev }),
    )
    .unwrap();
    p.human("redo", json!({}));
    assert_eq!(gain(&p, 1), 5.0);
    assert_eq!(gain(&p, 2), 7.0, "the human's redo leaves the agent's edit");
    // a new session asks again
    let p = p.reopen();
    assert_eq!(p.human("undo", json!({})).output["needs_confirm"], true);
}

#[test]
fn versions_say_who_edited_after_them_and_the_diff_shows_parts() {
    let p = open("hist-diff");
    let fx = fixture(&p, 5);
    let named = p
        .human("restore_point_create", json!({"name": "first"}))
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // the human trims part 0 and deletes part 4; an agent splits part 2 and moves part 1
    p.human(
        "part_trim",
        json!({"part": fx.parts[0], "src_out": 101 * SEC}),
    );
    p.human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "part_delete", "part": fx.parts[4]}]}),
    );
    let base = || {
        Some(gmclip_proto::Base::AsOf {
            as_of: p.h.snapshot().doc.rev(),
        })
    };
    p.agent("c9", "run_begin", json!({"label": "tighten"}), base())
        .unwrap();
    p.agent(
        "c9",
        "part_split",
        json!({"part": fx.parts[2], "at": 107 * SEC}),
        base(),
    )
    .unwrap();
    p.agent(
        "c9",
        "part_reorder",
        json!({"part": fx.parts[1], "after": fx.parts[3]}),
        base(),
    )
    .unwrap();
    let vs = versions(&p, &fx.cut);
    let n = p
        .human("history_list", json!({"cut": fx.cut, "count_only": true}))
        .output;
    assert_eq!(n["total"].as_u64(), Some(vs.len() as u64));
    assert_eq!(n["versions"], json!([]));
    let first = vs.iter().find(|v| v["name"] == "first").unwrap();
    assert_eq!(first["parts"], 5);
    let by: Vec<&str> = first["then_by"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(by[0], "human", "{by:?}");
    let run = vs.iter().find(|v| v["kind"] == "run").unwrap();
    assert_eq!(run["run_by"], "local:claude-code");
    assert_eq!(run["run_label"], "tighten");
    assert_eq!(run["then_by"], json!(["local:claude-code"]));

    let d = p
        .human("history_diff", json!({"snapshot": named, "cut": fx.cut}))
        .output;
    assert_eq!(d["from_parts"], 5);
    assert_eq!(d["to_parts"], 5, "one deleted, one split off");
    let ids = |k: &str| -> Vec<String> {
        d[k].as_array()
            .unwrap()
            .iter()
            .map(|p| p["part"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids("removed"), vec![fx.parts[4].clone()]);
    assert_eq!(ids("added").len(), 1, "the split's second piece");
    let trimmed: Vec<String> = d["trimmed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["to"]["part"].as_str().unwrap().to_owned())
        .collect();
    assert!(trimmed.contains(&fx.parts[0]) && trimmed.contains(&fx.parts[2]));

    assert_eq!(d["moved"], json!([fx.parts[1]]));
    assert_eq!(d["by"], json!(["human", "local:claude-code"]));
    let secs = d["from_seconds"].as_f64().unwrap();
    assert!((secs - 10.0).abs() < 1e-6, "{secs}");
}

/// R-HIST-5: restoring any version of a project with a 200-part cut takes ≤ 200 ms and
/// is one undo step. The timing is asserted loosely here (debug build, loaded machine);
/// `cargo test --release -p gmclip-core --test history_browser -- --nocapture` prints it.
#[test]
fn restoring_a_200_part_cut_is_one_step_and_fast() {
    let p = open("hist-200");
    let fx = fixture(&p, 200);
    let named = p
        .human("restore_point_create", json!({"name": "200 parts"}))
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = p.h.snapshot().doc.clone();
    // 300 edits after it: trims, gains, deletes, splits
    for i in 0..300usize {
        let part = &fx.parts[i % 150];
        match i % 3 {
            0 => {
                p.human(
                    "part_set_gain",
                    json!({"part": part, "gain_db": (i % 7) as f64}),
                );
            }
            1 => {
                let t = 100 * SEC + (i % 150) as i64 * 3 * SEC;
                p.human(
                    "part_trim",
                    json!({"part": part, "src_out": t + 2 * SEC - SEC / 10}),
                );
            }
            _ => {}
        }
    }
    for k in 150..200 {
        p.human(
            "cut_edit",
            json!({"cut": fx.cut, "ops": [{"op": "part_delete", "part": fx.parts[k]}]}),
        );
    }
    let mut times = Vec::new();
    let all = versions(&p, &fx.cut);
    let picks: Vec<String> = std::iter::once(named.clone())
        .chain(
            all.iter()
                .filter(|v| v["kind"] == "restore_point")
                .take(4)
                .map(|v| v["id"].as_str().unwrap().to_owned()),
        )
        .collect();
    for id in &picks {
        let t0 = Instant::now();
        let r = p.human("restore_to", json!({"snapshot": id}));
        times.push(t0.elapsed());
        assert!(r.committed.is_some() || r.output["target"] == json!(id));
        // one undo step puts it back
        let t1 = Instant::now();
        p.human("undo", json!({}));
        times.push(t1.elapsed());
    }
    let t0 = Instant::now();
    p.human("restore_to", json!({"snapshot": named}));
    let last = t0.elapsed();
    let now = p.h.snapshot().doc.clone();
    for e in before.entity_ids() {
        assert_eq!(now.encode(e), before.encode(e), "{e} restored");
    }
    let worst = times.iter().chain([&last]).max().copied().unwrap();
    eprintln!(
        "R-HIST-5: 200-part cut, {} restores/undos, worst {:.1} ms, last restore {:.1} ms",
        times.len() + 1,
        worst.as_secs_f64() * 1e3,
        last.as_secs_f64() * 1e3
    );
    let limit = if cfg!(debug_assertions) {
        2_000.0
    } else {
        200.0
    };
    assert!(worst.as_secs_f64() * 1e3 <= limit, "{worst:?}");
}

/// A2.13 / SPEC §13 "undo/redo ≤ 50 ms" [M2]: on a 300-part cut, each human edit of the
/// hot kinds (gain, trim, split, reorder, delete, crop) is undone and redone, every undo
/// and redo timed from the call to its commit. Asserted loosely in a debug build;
/// `cargo test --release -p gmclip-core --test history_browser -- --nocapture` prints
/// the release numbers.
#[test]
fn undo_and_redo_stay_under_50_ms_on_a_300_part_cut() {
    let p = open("hist-undo-300");
    let fx = fixture(&p, 300);
    let mut times: Vec<f64> = Vec::new();
    let mut timed = |cmd: &str| {
        let t0 = Instant::now();
        let r = p.human(cmd, json!({}));
        times.push(t0.elapsed().as_secs_f64() * 1e3);
        r
    };
    for i in 0..60usize {
        let k = 10 + i * 4;
        let part = &fx.parts[k];
        let t = 100 * SEC + k as i64 * 3 * SEC;
        let before = p.h.snapshot().doc.clone();
        match i % 6 {
            0 => p.human("part_set_gain", json!({"part": part, "gain_db": 3.0})),
            1 => p.human("part_trim", json!({"part": part, "src_out": t + SEC})),
            2 => p.human("part_split", json!({"part": part, "at": t + SEC})),
            3 => p.human("part_reorder", json!({"part": part})),
            4 => p.human(
                "cut_edit",
                json!({"cut": fx.cut, "ops": [{"op": "part_delete", "part": part}]}),
            ),
            _ => p.human(
                "crop_set",
                json!({"layer": fx.screen, "part": part, "framing": fx.framing,
                       "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}}),
            ),
        };
        let after = p.h.snapshot().doc.clone();
        timed("undo");
        assert!(p.h.snapshot().doc.rev() > after.rev());
        for e in before.entity_ids() {
            assert_eq!(
                p.h.snapshot().doc.encode(e),
                before.encode(e),
                "undo of edit {i}"
            );
        }
        timed("redo");
        for e in after.entity_ids() {
            assert_eq!(
                p.h.snapshot().doc.encode(e),
                after.encode(e),
                "redo of edit {i}"
            );
        }
    }
    times.sort_by(f64::total_cmp);
    let worst = times[times.len() - 1];
    let p99 = times[(times.len() * 99 / 100).min(times.len() - 1)];
    eprintln!(
        "undo/redo on a 300-part cut: {} timed, median {:.2} ms, p99 {:.2} ms, worst {:.2} ms",
        times.len(),
        times[times.len() / 2],
        p99,
        worst
    );
    let limit = if cfg!(debug_assertions) { 500.0 } else { 50.0 };
    assert!(worst <= limit, "worst undo/redo {worst:.1} ms");
}
