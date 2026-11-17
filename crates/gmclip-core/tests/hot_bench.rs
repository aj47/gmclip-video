//! R-CMD-8 [M1 recorded]: the 15 hot commands on a 300-part cut, p99 of handler +
//! R-STORE-7 commit, measured end to end through the actor (so it also includes the
//! channel round trip). Ignored by default; run in release:
//! `cargo test --release -p gmclip-core --test hot_bench -- --ignored --nocapture`.
//! The M2 gate is p99 ≤ 2 ms on mac-ref (the Air); this records this machine.

mod common;

use std::time::{Duration, Instant};

use common::{SEC, fixture, open};
use serde_json::{Value, json};

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

#[test]
#[ignore = "benchmark; run in release with --ignored"]
fn hot_commands_p99_on_a_300_part_cut() {
    let n: usize = std::env::var("GMCLIP_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let p = open("bench");
    let fx = fixture(&p, 300);
    let lay = common::id(&p.human("layout_create", json!({"cut": fx.cut, "name": "panel"})));
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 4500}, "place": {"x": 0.5, "y": 0.16, "w": 0.9}}),
    ));
    let mid = fx.parts[150].clone();
    let other = fx.parts[10].clone();
    let t_mid = 100 * SEC + 150 * 3 * SEC;
    let word = |o: u64| json!({"word_id": {"asr_revision": "r1", "ordinal": o}, "t_src_start": t_mid + SEC / 2, "t_src_end": t_mid + SEC, "normalised_text": "w"});
    let wid = json!({"asr_revision": "r1", "ordinal": 3});
    let mut report = Vec::new();
    let mut worst = Duration::ZERO;
    // (name, a function from iteration → (command, args)); alternating so every call
    // is valid and changes something
    type Gen<'a> = Box<dyn Fn(usize) -> (&'static str, Value) + 'a>;
    let mut split_b: Option<String> = None;
    let cases: Vec<(&str, Gen)> = vec![
        (
            "strike/restore",
            Box::new(|i| {
                if i % 2 == 0 {
                    ("words_strike", json!({"cut": fx.cut, "words": [word(1)]}))
                } else {
                    ("words_restore", json!({"cut": fx.cut, "words": [word(1)]}))
                }
            }),
        ),
        (
            "trim",
            Box::new(|i| {
                (
                    "part_trim",
                    json!({"part": mid, "src_out": t_mid + 2 * SEC - (i % 2) as i64 * SEC / 10}),
                )
            }),
        ),
        (
            "reorder",
            Box::new(|i| {
                (
                    "part_reorder",
                    json!({"part": mid, "after": if i % 2 == 0 { fx.parts[20].clone() } else { fx.parts[149].clone() }}),
                )
            }),
        ),
        (
            "set gain",
            Box::new(|i| {
                (
                    "part_set_gain",
                    json!({"part": mid, "gain_db": (i % 7) as f64}),
                )
            }),
        ),
        (
            "caption edit",
            Box::new(|i| {
                (
                    "caption_edit",
                    json!({"cut": fx.cut, "word": wid, "edit": if i % 2 == 0 { json!({"text": "exe.dev"}) } else { Value::Null }}),
                )
            }),
        ),
        (
            "layout set",
            Box::new(|i| {
                (
                    "layout_set",
                    json!({"part": mid, "framing": fx.framing, "layout": if i % 2 == 0 { json!(lay) } else { Value::Null }}),
                )
            }),
        ),
        (
            "overlay move",
            Box::new(|i| {
                (
                    "overlay_move",
                    json!({"overlay": ov, "place": {"x": 0.1 + (i % 5) as f64 / 10.0, "y": 0.1, "w": 0.5}}),
                )
            }),
        ),
        (
            "crop set",
            Box::new(|i| {
                (
                    "crop_set",
                    json!({"layer": fx.screen, "part": other, "framing": fx.framing, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.1 + (i % 5) as f64 / 10.0}}),
                )
            }),
        ),
        (
            "place set",
            Box::new(|i| {
                (
                    "place_set",
                    json!({"layer": fx.cam, "part": other, "framing": fx.framing, "place": {"x": 0.1, "y": 0.6, "w": 0.1 + (i % 5) as f64 / 10.0}}),
                )
            }),
        ),
        (
            "word-edge nudge",
            Box::new(|i| {
                (
                    "word_edge_nudge",
                    json!({"cut": fx.cut, "word": word(4), "edge": "end", "delta_ms": if i % 2 == 0 { -10 } else { 10 }}),
                )
            }),
        ),
        (
            "keyword toggle",
            Box::new(|_| {
                (
                    "caption_keyword_toggle",
                    json!({"cut": fx.cut, "word": wid}),
                )
            }),
        ),
        (
            "visual-split add",
            Box::new(|i| {
                (
                    "visual_split_add",
                    json!({"part": other, "framing": fx.framing, "at": 100 * SEC + 30 * SEC + (i as i64 % 100) * SEC / 100, "overrides": {fx.cam.clone(): {"opacity": 0.5}}}),
                )
            }),
        ),
    ];
    for (name, gen_) in &cases {
        let mut t = Vec::with_capacity(n);
        for i in 0..n {
            let (cmd, args) = gen_(i);
            p.clock.advance(1);
            let t0 = Instant::now();
            p.h.human(cmd, args)
                .unwrap_or_else(|e| panic!("{name} #{i}: {e:?}"));
            t.push(t0.elapsed());
        }
        let p99 = pct(&mut t, 0.99);
        worst = worst.max(p99);
        report.push(format!(
            "{name:<17} p50 {:>9.3?}  p99 {:>9.3?}",
            pct(&mut t, 0.5),
            p99
        ));
    }
    // split and merge, alternating on the same part
    let (mut ts, mut tm) = (Vec::new(), Vec::new());
    for _ in 0..n / 2 {
        p.clock.advance(1);
        let t0 = Instant::now();
        let b = common::id(
            &p.h.human("part_split", json!({"part": mid, "at": t_mid + SEC}))
                .unwrap(),
        );
        ts.push(t0.elapsed());
        split_b = Some(b.clone());
        p.clock.advance(1);
        let t0 = Instant::now();
        p.h.human("part_merge", json!({"a": mid, "b": b})).unwrap();
        tm.push(t0.elapsed());
    }
    let _ = split_b;
    for (name, mut t) in [("split", ts), ("merge", tm)] {
        let p99 = pct(&mut t, 0.99);
        worst = worst.max(p99);
        report.push(format!(
            "{name:<17} p50 {:>9.3?}  p99 {:>9.3?}",
            pct(&mut t, 0.5),
            p99
        ));
    }
    eprintln!("R-CMD-8 on a 300-part cut, {n} calls each (handler + commit + actor round trip):");
    for l in &report {
        eprintln!("  {l}");
    }
    eprintln!("  worst p99 {worst:?}");
    p.h.close().unwrap();
}
