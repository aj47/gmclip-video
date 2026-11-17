//! Edit polish (Ideas 2026-10-16, lane K2; decision 0025) on the core: stressed words
//! starred in one step (and the style's automatic emphasis), punch-ins as tagged eased
//! split pairs (added, removed, replaced, each one undo step; the follow-cursor leaves
//! them alone), and the loop option with its lint and both fixes.

mod common;

use common::{SEC, fixture, open};
use gmclip_core::lint::{Level, lint_cut};
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Doc, Id, Ulid};
use serde_json::{Value, json};

fn ulid<T>(s: &str) -> Id<T> {
    Id::new(Ulid::parse(s).unwrap())
}

fn keywords(p: &common::Project, cut: &str) -> usize {
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    doc.children::<CaptionDoc>(ulid::<Cut>(cut).entity())
        .filter_map(|c| doc.get(c))
        .map(|c| c.keywords.len())
        .sum()
}

fn word(o: u32) -> Value {
    json!({"asr_revision": "r1", "ordinal": o})
}

#[test]
fn stressed_words_are_starred_in_one_step_and_the_style_can_ask_for_it() {
    let p = open("polish-keywords");
    let fx = fixture(&p, 1);
    let r = p.human(
        "caption_keywords_mark",
        json!({"cut": fx.cut, "words": [word(3), word(17), word(3), word(40)], "on": true}),
    );
    assert_eq!(r.output["changed"], 3);
    assert_eq!(keywords(&p, &fx.cut), 3);
    // again: nothing new
    let r = p.human(
        "caption_keywords_mark",
        json!({"cut": fx.cut, "words": [word(17)], "on": true}),
    );
    assert_eq!(r.output["changed"], 0);
    // one ⌘Z takes the three back
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    assert_eq!(keywords(&p, &fx.cut), 0);
    // with the style switched to automatic emphasis, in the same step
    p.human(
        "caption_keywords_mark",
        json!({"cut": fx.cut, "words": [word(5)], "on": true, "auto": {"framing": fx.framing, "on": true}}),
    );
    let look = |p: &common::Project| {
        p.h.snapshot()
            .doc
            .get(ulid::<Framing>(&fx.framing))
            .unwrap()
            .caption_look
            .clone()
    };
    assert!(look(&p).is_some_and(|l| l.auto_emphasis));
    p.human(
        "caption_keywords_mark",
        json!({"cut": fx.cut, "words": [word(5)], "on": false, "auto": {"framing": fx.framing, "on": false}}),
    );
    assert_eq!(keywords(&p, &fx.cut), 0);
    assert_eq!(look(&p), None, "back to the plain look");
    p.human("undo", json!({}));
    assert_eq!(keywords(&p, &fx.cut), 1);
    assert!(look(&p).is_some_and(|l| l.auto_emphasis));
    // refused: too many words, a framing of another cut
    let many: Vec<Value> = (0..501).map(word).collect();
    assert!(
        p.try_human(
            "caption_keywords_mark",
            json!({"cut": fx.cut, "words": many, "on": true})
        )
        .is_err()
    );
    let other = fixture(&p, 1);
    assert!(
        p.try_human(
            "caption_keywords_mark",
            json!({"cut": fx.cut, "words": [], "on": true, "auto": {"framing": other.framing, "on": true}}),
        )
        .is_err()
    );
}

fn splits(doc: &Doc, part: &str) -> Vec<VisualSplit> {
    let mut v: Vec<VisualSplit> = doc
        .children::<VisualSplit>(ulid::<Part>(part).entity())
        .filter_map(|s| doc.get(s).cloned())
        .collect();
    v.sort_by_key(|s| s.at_src);
    v
}

fn crop_at(doc: &Doc, part: &str, framing: &str, layer: &str, t: i64) -> NormRect {
    gmclip_model::effective::effective(doc, ulid(part), ulid(framing), gmclip_model::Flicks(t))
        .unwrap()
        .layers
        .iter()
        .find(|l| l.id == ulid::<Layer>(layer))
        .unwrap()
        .values
        .crop
}

#[test]
fn a_punch_in_is_a_tagged_eased_pair_held_for_the_phrase() {
    let p = open("polish-punch");
    let fx = fixture(&p, 2);
    let (a, b) = (fx.parts[0].clone(), fx.parts[1].clone());
    let fpf = SEC / 30;
    let t0 = 100 * SEC; // part a is [100 s, 102 s), b [103 s, 105 s)
    let r = p.human(
        "punch_ins_set",
        json!({"framing": fx.framing, "punches": [
            {"part": a, "layer": fx.screen, "at": t0 + SEC / 2, "until": t0 + 3 * SEC / 2}
        ]}),
    );
    assert_eq!(r.output["added"].as_array().unwrap().len(), 1);
    let doc = p.h.snapshot().doc.clone();
    let s = splits(&doc, &a);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].tag, Some(SplitTag::PunchIn));
    assert_eq!(s[1].tag, Some(SplitTag::PunchOut));
    assert!(matches!(
        s[0].transition,
        Transition::Ease { dur_ms: 220, .. }
    ));
    assert!(s[1].overrides.is_empty(), "back to the part's own look");
    // settled: 8 % in about the centre; after the phrase: the part's own crop again
    let full = NormRect::FULL;
    let mid = crop_at(&doc, &a, &fx.framing, &fx.screen, t0 + SEC);
    assert!((mid.w - full.w / 1.08).abs() < 1e-9, "{mid:?}");
    assert!(((mid.x + mid.w / 2.0) - 0.5).abs() < 1e-9);
    let after = crop_at(&doc, &a, &fx.framing, &fx.screen, t0 + 2 * SEC - fpf);
    assert_eq!(after, full);
    // one ⌘Z removes it
    p.human("undo", json!({}));
    assert!(splits(&p.h.snapshot().doc, &a).is_empty());
    p.human("redo", json!({}));
    // another one over it is refused; so is one outside the part or too big a zoom
    for bad in [
        json!({"part": a, "layer": fx.screen, "at": t0 + SEC, "until": t0 + 3 * SEC / 2}),
        json!({"part": a, "layer": fx.screen, "at": t0 - SEC, "until": t0 + SEC}),
        json!({"part": b, "layer": fx.screen, "at": 103 * SEC + SEC / 2, "until": 104 * SEC, "scale": 0.3}),
        json!({"part": b, "layer": fx.screen, "at": 103 * SEC + SEC / 2, "until": 103 * SEC + SEC / 2 + fpf}),
    ] {
        assert!(
            p.try_human(
                "punch_ins_set",
                json!({"framing": fx.framing, "punches": [bad]})
            )
            .is_err()
        );
    }
    // the follow-cursor keeps its hands off a punch-in
    p.human(
        "cursor_follow_apply",
        json!({"framing": fx.framing, "part": a, "layer": fx.screen, "ease_ms": 600, "keys": []}),
    );
    assert_eq!(splits(&p.h.snapshot().doc, &a).len(), 2);
    // a second one in part b; removing a's by its zoom-in split takes the pair
    let in_a = splits(&p.h.snapshot().doc, &a)[0].id.0.to_string();
    p.human(
        "punch_ins_set",
        json!({"framing": fx.framing, "punches": [
            {"part": b, "layer": fx.screen, "at": 103 * SEC + SEC / 2, "until": 104 * SEC, "scale": 0.06}
        ], "remove": [in_a]}),
    );
    let doc = p.h.snapshot().doc.clone();
    assert!(splits(&doc, &a).is_empty());
    assert_eq!(splits(&doc, &b).len(), 2);
    // the per-cut switch off: replace with none
    let r = p.human(
        "punch_ins_set",
        json!({"framing": fx.framing, "punches": [], "replace": true}),
    );
    assert_eq!(r.output["removed"], 1);
    assert!(splits(&p.h.snapshot().doc, &b).is_empty());
}

#[test]
fn a_punch_in_keeps_the_split_in_force() {
    let p = open("polish-punch-split");
    let fx = fixture(&p, 1);
    let a = fx.parts[0].clone();
    let t0 = 100 * SEC;
    let x = json!({"x": 0.2, "y": 0.1, "w": 0.5, "h": 0.5});
    p.human(
        "crop_follow_apply",
        json!({"framing": fx.framing, "crops": [{"part": a, "layer": fx.screen, "at": t0 + SEC / 5, "crop": x}]}),
    );
    p.human(
        "punch_ins_set",
        json!({"framing": fx.framing, "punches": [
            {"part": a, "layer": fx.screen, "at": t0 + SEC / 2, "until": t0 + 3 * SEC / 2}
        ]}),
    );
    let doc = p.h.snapshot().doc.clone();
    let s = splits(&doc, &a);
    assert_eq!(s.len(), 3);
    let held = NormRect {
        x: 0.2,
        y: 0.1,
        w: 0.5,
        h: 0.5,
    };
    assert_eq!(s[2].overrides[&ulid::<Layer>(&fx.screen)].crop, Some(held));
    let mid = crop_at(&doc, &a, &fx.framing, &fx.screen, t0 + SEC);
    assert!((mid.w - 0.5 / 1.08).abs() < 1e-9, "{mid:?}");
    assert!(((mid.x + mid.w / 2.0) - 0.45).abs() < 1e-9);
    assert_eq!(
        crop_at(&doc, &a, &fx.framing, &fx.screen, t0 + 19 * SEC / 10),
        held
    );
}

fn loop_findings(doc: &Doc, cut: &str) -> Vec<gmclip_core::lint::Finding> {
    lint_cut(doc, ulid(cut))
        .into_iter()
        .filter(|f| f.rule == "loop_last_frame")
        .collect()
}

#[test]
fn a_looping_cut_ends_on_its_opening_layout() {
    let p = open("polish-loop");
    let fx = fixture(&p, 3);
    let last = fx.parts[2].clone();
    // the last part shows the screen cropped in
    p.human(
        "crop_set",
        json!({"layer": fx.screen, "part": last, "framing": fx.framing, "crop": {"x": 0.3, "y": 0.3, "w": 0.4, "h": 0.4}}),
    );
    // not set to loop: no finding
    assert!(loop_findings(&p.h.snapshot().doc, &fx.cut).is_empty());
    p.human("cut_loop_set", json!({"cut": fx.cut, "on": true}));
    let f = loop_findings(&p.h.snapshot().doc, &fx.cut);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].level, Level::Warning);
    assert!(f[0].message.contains("screen crop"), "{}", f[0].message);
    let fix = f[0].fix.clone().unwrap();
    assert_eq!(fix.command, "loop_fix");
    assert_eq!(fix.args["how"], "layout");
    p.human(&fix.command, fix.args.clone());
    assert!(loop_findings(&p.h.snapshot().doc, &fx.cut).is_empty());
    p.human("undo", json!({}));
    assert_eq!(loop_findings(&p.h.snapshot().doc, &fx.cut).len(), 1);

    // the last part zooms with a split of its own: the fix is a tail, which keeps it
    p.human(
        "crop_set",
        json!({"layer": fx.screen, "part": last, "framing": fx.framing, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}}),
    );
    p.human(
        "crop_follow_apply",
        json!({"framing": fx.framing, "crops": [{"part": last, "layer": fx.screen, "at": 106 * SEC + SEC / 2, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}}]}),
    );
    let f = loop_findings(&p.h.snapshot().doc, &fx.cut);
    assert_eq!(f.len(), 1, "{f:?}");
    let fix = f[0].fix.clone().unwrap();
    assert_eq!(fix.args["how"], "tail");
    let r = p.human(&fix.command, fix.args.clone());
    let doc = p.h.snapshot().doc.clone();
    let parts = gmclip_model::clock::cut_layout(&doc, ulid(&fx.cut));
    assert_eq!(parts.len(), 4, "a tail split off the last part");
    assert_eq!(
        parts[3].part.0.to_string(),
        r.output["part"].as_str().unwrap()
    );
    assert_eq!(parts[3].dur.0, 18 * (SEC / 30), "0.6 s on the frame grid");
    assert_eq!(
        splits(&doc, &last).len(),
        1,
        "the zoom before the tail stays"
    );
    assert!(loop_findings(&doc, &fx.cut).is_empty());
    p.human("undo", json!({}));
    assert_eq!(
        gmclip_model::clock::cut_layout(&p.h.snapshot().doc, ulid(&fx.cut)).len(),
        3
    );
    // an inset only on the opening travels to the end too
    p.human(
        "layer_add",
        json!({"owner": {"part_override": {"part": fx.parts[0], "framing": fx.framing}}, "role": "detail",
               "crop": {"x": 0.1, "y": 0.1, "w": 0.2, "h": 0.1}, "place": {"x": 0.1, "y": 0.4, "w": 0.8}, "z": 2}),
    );
    let f = loop_findings(&p.h.snapshot().doc, &fx.cut);
    assert!(
        f[0].message.contains("detail only at the opening"),
        "{}",
        f[0].message
    );
    let fix = f[0].fix.clone().unwrap();
    p.human(&fix.command, fix.args);
    assert!(loop_findings(&p.h.snapshot().doc, &fx.cut).is_empty());
    // off: no finding whatever the end shows
    p.human("undo", json!({}));
    p.human("cut_loop_set", json!({"cut": fx.cut, "on": false}));
    assert!(loop_findings(&p.h.snapshot().doc, &fx.cut).is_empty());
    // refused: the layout fix on a one-part cut
    let one = fixture(&p, 1);
    assert!(
        p.try_human(
            "loop_fix",
            json!({"cut": one.cut, "framing": one.framing, "how": "layout"})
        )
        .is_err()
    );
}
