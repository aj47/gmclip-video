//! A2.07: `hold_screen_here` (the screen held from the playhead's frame to the end of the
//! sentence, one transaction, whole parts) and `run_clock_make` (a run clock tied to a
//! measurement: floored m:ss, and lint catches a number typed over it by hand).

mod common;

use common::{SEC, fixture, open};
use gmclip_core::lint::lint_cut;
use gmclip_model::anchor::Anchor;
use gmclip_model::entities::{ContentHold, Cut, HoldAt, Part};
use gmclip_model::{Doc, Id, Ulid};
use serde_json::{Value, json};

fn words(p: &common::Project, owner: &str, ws: &[(&str, f64, f64)]) {
    let rev = "asr-test@1";
    let words: Vec<Value> = ws
        .iter()
        .map(|(t, s, e)| json!({"text": t, "start": (s * SEC as f64) as i64, "end": (e * SEC as f64) as i64}))
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": words});
    p.h.put_analysis(owner, "words", rev, body.to_string().into_bytes())
        .unwrap();
}

fn cut_id(s: &str) -> Id<Cut> {
    Id::new(Ulid::parse(s).unwrap())
}

fn parts(doc: &Doc, cut: &str) -> Vec<(i64, i64)> {
    doc.children::<Part>(cut_id(cut).entity())
        .filter_map(|p| doc.get(p))
        .map(|p| (p.src_in.0, p.src_out.0))
        .collect()
}

fn holds(doc: &Doc, cut: &str) -> Vec<ContentHold> {
    doc.children::<ContentHold>(cut_id(cut).entity())
        .filter_map(|h| doc.get(h).cloned())
        .collect()
}

const FPF: i64 = SEC / 30;

#[test]
fn holds_to_the_end_of_the_sentence() {
    let p = open("hold-here");
    let fx = fixture(&p, 1); // one part, source 100 s → 102 s, 30 fps framing
    words(
        &p,
        &fx.source,
        &[
            ("So", 100.2, 100.4),
            ("look.", 100.5, 101.0),
            ("Then", 101.5, 101.7),
            ("more", 101.75, 101.9),
        ],
    );
    // the playhead 0.3 s in (frame 9): in "So look."
    let out = p
        .human("hold_screen_here", json!({"cut": fx.cut, "at_frame": 9}))
        .output;
    assert_eq!(out["until"], "sentence");
    assert_eq!(out["word"], "look.");
    assert_eq!(out["from_s"], 100.3);
    assert_eq!(out["to_s"], 101.0);
    let doc = p.h.snapshot().doc.clone();
    let ps = parts(&doc, &fx.cut);
    assert_eq!(
        ps,
        vec![
            (100 * SEC, 100 * SEC + 9 * FPF),
            (100 * SEC + 9 * FPF, 101 * SEC),
            (101 * SEC, 102 * SEC)
        ]
    );
    let hs = holds(&doc, &fx.cut);
    assert_eq!(hs.len(), 1);
    let h = &hs[0];
    assert!(h.roles.is_empty(), "every layer but the camera");
    assert!(matches!(h.from, Anchor::PartStart(_)) && matches!(h.to, Anchor::PartEnd(_)));
    assert_eq!(
        h.from.part().map(|x| x.0.to_string()),
        out["part"].as_str().map(str::to_owned)
    );
    match h.at {
        // the source has no probed rate: 30 fps; 100.3 s → frame 3009
        HoldAt::Frame(f) => assert_eq!(f.frame_index, 3009),
        HoldAt::AtSrc(_) => panic!("a frame hold"),
    }
    // the hold covers exactly the held part (the planner's whole-part rule)
    let held = doc.get(h.from.part().unwrap()).unwrap();
    assert_eq!((held.src_in.0, held.src_out.0), ps[1]);
    // one undo step takes it all back
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(parts(&doc, &fx.cut), vec![(100 * SEC, 102 * SEC)]);
    assert!(holds(&doc, &fx.cut).is_empty());
    p.h.close().unwrap();
}

#[test]
fn without_words_it_holds_to_the_end_of_the_part() {
    let p = open("hold-here-part");
    let fx = fixture(&p, 2);
    let out = p
        .human("hold_screen_here", json!({"cut": fx.cut, "at_frame": 15}))
        .output;
    assert_eq!(out["until"], "part");
    assert_eq!(out["to_s"], 102.0);
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(parts(&doc, &fx.cut).len(), 3);
    // the frame after the cut, and a frame too close to the part's end, are refused
    assert!(
        p.try_human("hold_screen_here", json!({"cut": fx.cut, "at_frame": 9000}))
            .is_err()
    );
    p.h.close().unwrap();
}

fn text(s: &str) -> Value {
    json!({"runs": [{"text": s}], "style": {"font": "neometric-heavy", "size": 50.0, "fill": "#ffffff", "outline": null, "shadow": null,
        "align": "left", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}})
}

#[test]
fn a_run_clock_shows_its_measurement_and_lint_catches_a_hand_edit() {
    let p = open("run-clock");
    let fx = fixture(&p, 2);
    let m = common::id(&p.human(
        "measurement_add",
        json!({"kind": "interval", "from": {"anchor": {"part_start": {"part": fx.parts[0]}}},
               "to": {"anchor": {"part_end": {"part": fx.parts[1]}}}, "value": 67.4, "unit": "s",
               "method": "queue 0→1 to first result frame", "author": "human"}),
    ));
    let start = json!({"cut_start": {"cut": fx.cut}});
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "role": "section_pill", "start": start, "end": {"dur_ms": 4000},
               "place": {"x": 0.05, "y": 0.14, "w": 0.6}, "text": text("GENERATING")}),
    ));
    let out = p
        .human("run_clock_make", json!({"overlay": ov, "measurement": m}))
        .output;
    assert_eq!(out["total"], "1:07");
    assert_eq!(out["shows"], "DONE IN 1:07");
    let doc = p.h.snapshot().doc.clone();
    let o = doc
        .get::<gmclip_model::entities::Overlay>(Id::new(Ulid::parse(&ov).unwrap()))
        .unwrap();
    assert_eq!(o.role.as_deref(), Some("run_clock"));
    let c = o.counter.as_ref().unwrap();
    assert_eq!(
        c.running_label, "GENERATING",
        "the pill's words become the label"
    );
    assert_eq!(o.number_provenance.len(), 1);
    assert_eq!(o.number_provenance[0].text, "1:07");
    let rule = "number_disagrees_with_provenance";
    let has = |d: &Doc| lint_cut(d, cut_id(&fx.cut)).iter().any(|f| f.rule == rule);
    assert!(!has(&doc));
    // a human types a different total over it
    p.human(
        "overlay_set",
        json!({"overlay": ov, "text": text("DONE IN 1:30")}),
    );
    assert!(
        has(&p.h.snapshot().doc),
        "a hand-typed 1:30 disagrees with 67.4 s"
    );
    // a measurement that is not an interval is refused
    let edge = common::id(&p.human(
        "measurement_add",
        json!({"kind": "edge", "from": {"anchor": {"part_start": {"part": fx.parts[0]}}},
               "value": 1.0, "unit": "s", "method": "m", "author": "human"}),
    ));
    assert!(
        p.try_human(
            "run_clock_make",
            json!({"overlay": ov, "measurement": edge})
        )
        .is_err()
    );
    p.h.close().unwrap();
}
