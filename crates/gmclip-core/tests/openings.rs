//! Decision 0024, "three openings, one short": alternate openings are sibling versions
//! of one cut (no copies): each swaps the cut's opening and shares everything after it;
//! a version is computed on demand; the human makes one the main version in one undo
//! step, and the old opening stays an alternate.

mod common;

use common::{Project, SEC, fixture, open};
use gmclip_core::commands::opening::{opening_parts, spans_sig, version_doc};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Id, Ulid};
use serde_json::{Value, json};

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

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

fn word(n: u32, s: f64, e: f64, t: &str) -> Value {
    json!({"word_id": {"asr_revision": "asr-test@1", "ordinal": n}, "t_src_start": (s * SEC as f64) as i64, "t_src_end": (e * SEC as f64) as i64, "normalised_text": t})
}

fn text(s: &str) -> Value {
    json!({"runs": [{"text": s}], "style": {"font": "neometric-heavy", "size": 86.0, "fill": "#ffffff", "outline": null, "shadow": null,
        "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}})
}

/// The cut's parts as (id, src_in s, src_out s).
fn parts(doc: &Doc, cut: &str) -> Vec<(String, i64, i64)> {
    doc.children::<Part>(uid::<Cut>(cut).entity())
        .filter_map(|id| doc.get(id))
        .map(|x| (x.id.0.to_string(), x.src_in.0 / SEC, x.src_out.0 / SEC))
        .collect()
}

fn hook(doc: &Doc, cut: &str) -> Overlay {
    doc.children::<Overlay>(uid::<Cut>(cut).entity())
        .filter_map(|id| doc.get(id))
        .find(|o| o.role.as_deref() == Some("text_hook"))
        .unwrap()
        .clone()
}

/// A short of three 2 s parts (100, 103, 106 s; the first is the hook) with a text hook
/// until the end of its first part's second word, a transcript, and a moment at 200 s.
fn short(tag: &str) -> (Project, common::Fx, String) {
    let p = open(tag);
    let fx = fixture(&p, 3);
    p.human(
        "part_set_role",
        json!({"part": fx.parts[0], "role": "hook"}),
    );
    words(
        &p,
        &fx.source,
        &[
            ("So", 100.2, 100.5),
            ("look.", 100.6, 101.0),
            ("Body", 103.2, 103.6),
            ("It", 200.1, 200.3),
            ("finished", 200.4, 200.9),
            ("in", 201.0, 201.1),
            ("67", 201.2, 201.6),
            ("seconds.", 201.7, 202.4),
        ],
    );
    p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "role": "text_hook", "band": "hook",
               "start": {"cut_start": {"cut": fx.cut}}, "end": {"at": {"word_end": word(1, 100.6, 101.0, "look")}},
               "place": {"x": 0.5, "y": 0.16, "w": 0.9}, "text": text("Look at this")}),
    );
    let m = p.human(
        "moment_add",
        json!({"media": {"source": fx.source}, "start_s": 200.0, "end_s": 203.0, "quote": "It finished in 67 seconds.",
               "score": 4, "type": "result", "why": "the result first", "text_hook": "It finished in *67 seconds*"}),
    );
    let moment = m.output["id"].as_str().unwrap().to_owned();
    (p, fx, moment)
}

#[test]
fn an_alternate_is_a_version_that_shares_the_rest_of_the_cut() {
    let (p, fx, moment) = short("open-version");
    let o = p.human(
        "opening_add",
        json!({"cut": fx.cut, "from": {"moment": moment}}),
    );
    let alt = o.output["id"].as_str().unwrap().to_owned();
    let list = p.human("openings_list", json!({"cut": fx.cut})).output;
    let v = list["versions"].as_array().unwrap();
    assert_eq!(v.len(), 2, "{list}");
    assert_eq!(v[0]["main"], true);
    assert_eq!(v[0]["label"], "Main");
    assert_eq!(v[0]["opening_ms"], 2000);
    assert_eq!(list["body_ms"], 4000, "the two parts after the hook");
    assert_eq!(v[1]["opening"], alt.as_str());
    assert_eq!(v[1]["from"], "moment");
    assert_eq!(v[1]["opening_ms"], 3000);
    assert_eq!(v[1]["total_ms"], 7000);
    assert_eq!(v[1]["hook_text"], "It finished in *67 seconds*");
    assert_eq!(v[1]["quote"], "It finished in 67 seconds.");
    assert_eq!(v[0]["quote"], "So look.");
    assert_eq!(v[0]["hook_text"], "Look at this");

    // the cut itself is untouched; the version is computed on demand
    let doc = p.h.snapshot().doc.clone();
    let before = parts(&doc, &fx.cut);
    let ver = version_doc(&doc, uid(&alt)).unwrap();
    let after = parts(&ver, &fx.cut);
    assert_eq!(after.len(), 3);
    assert_eq!(after[0].0, before[0].0, "the opening part keeps its id");
    assert_eq!((after[0].1, after[0].2), (200, 203));
    assert_eq!(
        after[1..],
        before[1..],
        "everything after the opening is shared"
    );
    assert_eq!(parts(&p.h.snapshot().doc, &fx.cut), before);
    // the version's hook shows its own words and ends with its opening (its old end
    // named a word the version no longer plays)
    let h = hook(&ver, &fx.cut);
    let runs: Vec<(&str, bool)> = h
        .text
        .as_ref()
        .unwrap()
        .runs
        .iter()
        .map(|r| (r.text.as_str(), r.colour.is_some()))
        .collect();
    assert_eq!(
        runs,
        vec![("It finished in\n", false), ("67 seconds", true)]
    );
    assert!(
        matches!(&h.end, OverlayEnd::At(gmclip_model::anchor::Anchor::PartEnd(e)) if e.part.0.to_string() == before[0].0),
        "{:?}",
        h.end
    );
    // an edit after the opening is in every version at once
    p.human(
        "part_trim",
        json!({"part": fx.parts[2], "src_in": 106 * SEC, "src_out": 107 * SEC}),
    );
    let doc = p.h.snapshot().doc.clone();
    let ver = version_doc(&doc, uid(&alt)).unwrap();
    assert_eq!(parts(&ver, &fx.cut)[2].2, 107);
    let list = p.human("openings_list", json!({"cut": fx.cut})).output;
    assert_eq!(list["versions"][1]["total_ms"], 6000);
}

#[test]
fn at_most_three_alternates_and_no_twins() {
    let (p, fx, moment) = short("open-max");
    p.human(
        "opening_add",
        json!({"cut": fx.cut, "from": {"moment": moment}}),
    );
    // the same opening again, or the main one, is not a new version
    let e = p
        .try_human(
            "opening_add",
            json!({"cut": fx.cut, "from": {"moment": moment}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    let main = json!({"cut": fx.cut, "from": {"span": {
        "start": {"raw": {"time": {"clock": "src", "t": 100 * SEC}, "reason": "picked"}},
        "end": {"raw": {"time": {"clock": "src", "t": 102 * SEC}, "reason": "picked"}}}}});
    assert_eq!(
        p.try_human("opening_add", main).unwrap_err().code(),
        "Invalid"
    );
    // picked by hand: the words selected in the transcript
    let picked = p.human(
        "opening_add",
        json!({"cut": fx.cut, "label": "the body line", "from": {"span": {
            "start": {"word_start": word(2, 103.2, 103.6, "body")},
            "end": {"word_end": word(2, 103.2, 103.6, "body")}}}}),
    );
    let doc = p.h.snapshot().doc.clone();
    let o = doc
        .get(uid::<Opening>(picked.output["id"].as_str().unwrap()))
        .unwrap();
    assert_eq!(o.from, OpeningFrom::Manual);
    assert_eq!(o.label, "the body line");
    assert_eq!(o.spans[0].media, MediaRef::Source(uid(&fx.source)));
    // a story suggestion's span
    let s = p.human(
        "story_suggestions_set",
        json!({"cut": fx.cut, "suggestions": [{"span": {
            "start": {"raw": {"time": {"clock": "src", "t": 300 * SEC}, "reason": "gather"}},
            "end": {"raw": {"time": {"clock": "src", "t": 304 * SEC}, "reason": "gather"}}}, "role": "payoff", "relevance": 0.9, "quote": "the payoff"}]}),
    );
    let sug = s.output["ids"][0].as_str().unwrap().to_owned();
    p.human(
        "opening_add",
        json!({"cut": fx.cut, "from": {"suggestion": sug}, "hook_text": "Payoff first"}),
    );
    let fourth = json!({"cut": fx.cut, "from": {"span": {
        "start": {"raw": {"time": {"clock": "src", "t": 400 * SEC}, "reason": "picked"}},
        "end": {"raw": {"time": {"clock": "src", "t": 402 * SEC}, "reason": "picked"}}}}});
    let e = p.try_human("opening_add", fourth.clone()).unwrap_err();
    assert_eq!(e.code(), "Invalid", "a fourth alternate");
    // removing one makes room
    p.human("opening_delete", json!({"opening": picked.output["id"]}));
    p.human("opening_add", fourth);
    let list = p.human("openings_list", json!({"cut": fx.cut})).output;
    assert_eq!(list["versions"].as_array().unwrap().len(), 4);
    assert_eq!(list["versions"][2]["label"], "the payoff");
    assert_eq!(list["versions"][2]["hook_text"], "Payoff first");
}

#[test]
fn the_human_chooses_the_main_version_in_one_step() {
    let (p, fx, moment) = short("open-choose");
    let alt = p
        .human(
            "opening_add",
            json!({"cut": fx.cut, "from": {"moment": moment}}),
        )
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // an agent may propose a version, not choose one
    let by_agent = p
        .agent(
            "a",
            "opening_add",
            json!({"cut": fx.cut, "from": {"span": {
                "start": {"raw": {"time": {"clock": "src", "t": 500 * SEC}, "reason": "agent"}},
                "end": {"raw": {"time": {"clock": "src", "t": 503 * SEC}, "reason": "agent"}}}}}),
            p.as_of(),
        )
        .unwrap();
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(
        doc.get(uid::<Opening>(by_agent.output["id"].as_str().unwrap()))
            .unwrap()
            .origin,
        EditOrigin::Agent
    );
    let e = p
        .agent("a", "opening_choose", json!({"opening": alt}), p.as_of())
        .unwrap_err();
    assert_eq!(e.code(), "Unauthorized");

    let before_doc = p.h.snapshot().doc.clone();
    let before = parts(&before_doc, &fx.cut);
    let main_sig = spans_sig(&gmclip_core::commands::opening::main_spans(
        &before_doc,
        uid(&fx.cut),
    ));
    let chosen = p.human("opening_choose", json!({"opening": alt})).output;
    let doc = p.h.snapshot().doc.clone();
    let now = parts(&doc, &fx.cut);
    assert_eq!(now[0].0, before[0].0, "the opening part keeps its id");
    assert_eq!((now[0].1, now[0].2), (200, 203));
    assert_eq!(now[1..], before[1..]);
    assert!(doc.get(uid::<Opening>(&alt)).is_none());
    let was = doc
        .get(uid::<Opening>(chosen["was_main"].as_str().unwrap()))
        .unwrap()
        .clone();
    assert_eq!(was.from, OpeningFrom::WasMain);
    assert_eq!(spans_sig(&was.spans), main_sig);
    assert_eq!(was.hook_text.as_deref(), Some("Look at this"));
    assert!(was.label.starts_with("Before: So look"), "{}", was.label);
    let h = hook(&doc, &fx.cut);
    assert_eq!(h.text.as_ref().unwrap().runs[1].text, "67 seconds");
    assert_eq!(opening_parts(&doc, uid(&fx.cut)).len(), 1);
    // one undo step
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(parts(&doc, &fx.cut), before);
    assert!(doc.get(uid::<Opening>(&alt)).is_some());
    assert_eq!(
        hook(&doc, &fx.cut).text.unwrap().runs[0].text,
        "Look at this"
    );
    // choosing back is the same gesture
    p.human("redo", json!({}));
    p.human("opening_choose", json!({"opening": chosen["was_main"]}));
    let doc = p.h.snapshot().doc.clone();
    let back = parts(&doc, &fx.cut);
    assert_eq!((back[0].1, back[0].2), (100, 102));
    assert_eq!(
        hook(&doc, &fx.cut).text.unwrap().runs[0].text,
        "Look at this"
    );
    let list = p.human("openings_list", json!({"cut": fx.cut})).output;
    // main + the moment's (kept as was_main again) + the agent's
    assert_eq!(list["versions"].as_array().unwrap().len(), 3);
}

#[test]
fn a_two_span_opening_adds_hook_parts_and_a_shorter_one_removes_them() {
    let (p, fx, _moment) = short("open-spans");
    // the cut's opening is its first two parts once both are the hook
    p.human(
        "part_set_role",
        json!({"part": fx.parts[1], "role": "hook"}),
    );
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(opening_parts(&doc, uid(&fx.cut)).len(), 2);
    let alt = p
        .human(
            "opening_add",
            json!({"cut": fx.cut, "from": {"span": {
                "start": {"raw": {"time": {"clock": "src", "t": 600 * SEC}, "reason": "picked"}},
                "end": {"raw": {"time": {"clock": "src", "t": 604 * SEC}, "reason": "picked"}}}}}),
        )
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let doc = p.h.snapshot().doc.clone();
    let ver = version_doc(&doc, uid(&alt)).unwrap();
    let v = parts(&ver, &fx.cut);
    assert_eq!(v.len(), 2, "two hook parts became one: {v:?}");
    assert_eq!((v[0].1, v[0].2, v[1].1), (600, 604, 106));
    // choosing it and back restores both hook parts (the second gets a new id)
    let was = p.human("opening_choose", json!({"opening": alt})).output["was_main"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("opening_choose", json!({"opening": was}));
    let doc = p.h.snapshot().doc.clone();
    let back = parts(&doc, &fx.cut);
    assert_eq!(
        back.iter().map(|x| (x.1, x.2)).collect::<Vec<_>>(),
        vec![(100, 102), (103, 105), (106, 108)]
    );
    let roles: Vec<PartRole> = doc
        .children::<Part>(uid::<Cut>(&fx.cut).entity())
        .filter_map(|id| doc.get(id).map(|x| x.role))
        .collect();
    assert_eq!(roles[..2], [PartRole::Hook, PartRole::Hook]);
}

#[test]
fn an_opening_sits_on_the_cut_frame_grid() {
    let (p, fx, _) = short("open-grid");
    // 10 ms past a frame edge at both ends (word edges are never on the grid)
    let o = p.human(
        "opening_add",
        json!({"cut": fx.cut, "from": {"span": {
            "start": {"raw": {"time": {"clock": "src", "t": 300 * SEC + SEC / 100}, "reason": "picked"}},
            "end": {"raw": {"time": {"clock": "src", "t": 302 * SEC + SEC / 100}, "reason": "picked"}}}}}),
    );
    let doc = p.h.snapshot().doc.clone();
    let op = doc
        .get(uid::<Opening>(o.output["id"].as_str().unwrap()))
        .unwrap()
        .clone();
    let fpf = SEC / 30;
    assert_eq!(op.spans[0].src_in.0, 300 * SEC);
    assert_eq!(op.spans[0].src_out.0, 302 * SEC);
    assert!(
        op.spans
            .iter()
            .all(|s| s.src_in.0 % fpf == 0 && s.src_out.0 % fpf == 0)
    );
}
