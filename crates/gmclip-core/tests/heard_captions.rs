//! Captions from the cut's own audio (SPEC §5.10, decision 0022): a part whose voice mix
//! is identical to its source reuses the source transcript's words; a part with a gain,
//! an envelope or no transcript is heard; and at the seam between the two an edge word is
//! said exactly once. Heard words here are scripted (`HeardStretch` bodies), as the app
//! stores what its recogniser heard.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::analysis::{AnalysisView, Transcript, Word};
use gmclip_core::planner::heard::{self, Change, HearPolicy, HeardStretch, HeardWord, Stretch};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Id, Ulid};
use gmclip_plan::types::TextRole;
use serde_json::json;

struct Fonts;
impl PlanHost for Fonts {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// Seconds (tenths) → flicks.
fn t(tenths: i64) -> i64 {
    tenths * SEC / 10
}

/// Captions on (story grouping) and the source's picture probed.
fn captions_on(p: &common::Project, fx: &common::Fx) {
    p.human(
        "font_add",
        json!({"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}),
    );
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "story"}),
    );
    p.human(
        "caption_set",
        json!({"cut": fx.cut, "enabled": true, "source": "heard", "drop_fillers": true}),
    );
}

fn probed(p: &common::Project, fx: &common::Fx) -> Doc {
    let mut doc = p.h.snapshot().doc.clone();
    let sid: Id<Source> = uid(&fx.source);
    let mut s = doc.get(sid).unwrap().clone();
    s.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(2560),
        height: Some(1440),
        fps: None,
        timescale: 15360,
        gop_max_s: None,
    }];
    doc.insert_for_test(s);
    doc
}

/// A transcript of `(text, start tenths, end tenths)` on the source clock.
fn view(fx: &common::Fx, words: &[(&str, i64, i64)]) -> AnalysisView {
    let mut v = AnalysisView::default();
    v.insert(
        &fx.source,
        Transcript {
            revision: "r1".into(),
            words: words
                .iter()
                .map(|(w, a, b)| Word {
                    text: (*w).into(),
                    start: t(*a),
                    end: t(*b),
                    confidence: None,
                })
                .collect(),
            lines: vec![],
        },
    );
    v
}

fn set_gain(doc: &mut Doc, part: &str, db: f64) {
    let mut pt = doc.get(uid::<Part>(part)).unwrap().clone();
    pt.gain_db = db;
    doc.insert_for_test(pt);
}

/// Scripted words heard over a stretch: `(text, output start tenths, end tenths)`.
fn heard_body(s: &Stretch, words: &[(&str, i64, i64)]) -> Vec<u8> {
    let w0 = s.window[0] as i64 * (SEC / 48_000);
    HeardStretch::new(
        s,
        "asr-test".into(),
        words
            .iter()
            .map(|(w, a, b)| HeardWord {
                text: (*w).into(),
                start: t(*a) - w0,
                end: t(*b) - w0,
                confidence: None,
            })
            .collect(),
    )
    .encode()
}

fn plan(
    doc: &Doc,
    fx: &common::Fx,
    v: &AnalysisView,
    policy: HearPolicy,
) -> (Vec<String>, heard::Audit) {
    let (plan, _, audit) =
        planner::plan_cut_heard(doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(v), policy)
            .unwrap();
    let words = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .flat_map(|e| {
            e.runs
                .iter()
                .map(|r| r.text.clone())
                .collect::<String>()
                .split_whitespace()
                .map(|w| {
                    w.trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    (words, audit)
}

fn count(ws: &[String], w: &str) -> usize {
    ws.iter().filter(|x| *x == w).count()
}

/// Three 2 s takes 1 s apart in the source, four words each.
fn three_takes() -> Vec<(&'static str, i64, i64)> {
    vec![
        ("alpha", 1001, 1005),
        ("bravo", 1005, 1009),
        ("charlie", 1010, 1014),
        ("delta", 1014, 1018),
        ("echo", 1031, 1035),
        ("foxtrot", 1035, 1039),
        ("golf", 1040, 1044),
        ("hotel", 1044, 1048),
        ("india", 1061, 1065),
        ("juliet", 1065, 1069),
        ("kilo", 1070, 1074),
        ("lima", 1074, 1078),
    ]
}

#[test]
fn identical_parts_reuse_and_a_changed_part_is_heard() {
    let p = open("heard-rule");
    let fx = fixture(&p, 3);
    captions_on(&p, &fx);
    let mut doc = probed(&p, &fx);
    let mut v = view(&fx, &three_takes());

    // nothing changed: every part reuses, nothing to hear
    let (ws, a) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert!(a.parts.iter().all(|p| p.identical()), "{:?}", a.parts);
    assert!(a.stretches.is_empty() && a.missing.is_empty());
    assert_eq!(ws.len(), 12, "{ws:?}");

    // a gain on the middle take: it is heard, with 1 s before and 2 s after as context
    set_gain(&mut doc, &fx.parts[1], 4.0);
    let (ws, a) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert_eq!(a.parts[1].changes, vec![Change::Gain]);
    assert!(a.parts[0].identical() && a.parts[2].identical());
    assert_eq!(a.stretches.len(), 1);
    let s = a.stretches[0].clone();
    assert_eq!(s.parts, [1, 2]);
    assert_eq!(s.out, [2 * 48_000, 4 * 48_000]);
    assert_eq!(s.window, [48_000, 6 * 48_000]);
    // not heard yet: the stretch is missing and its part shows the source words meanwhile
    assert_eq!(a.missing, vec![s.clone()]);
    assert_eq!(ws.len(), 12, "{ws:?}");

    // heard: the middle take's captions are what its audio says now
    v.insert_heard(&heard_body(
        &s,
        &[
            ("charlie", 15, 19), // the first take's tail, in the lead context
            ("delta", 19, 20),
            ("echo", 21, 25),
            ("foxtrot", 25, 29),
            ("gulf", 30, 34), // heard differently from the source's "golf"
            ("hotel", 34, 38),
            ("india", 41, 45), // the third take, in the tail context
        ],
    ));
    let (ws, a) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert!(a.missing.is_empty());
    assert_eq!(count(&ws, "gulf"), 1, "{ws:?}");
    assert_eq!(count(&ws, "golf"), 0, "{ws:?}");
    for w in [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "hotel", "india", "juliet",
        "kilo", "lima",
    ] {
        assert_eq!(count(&ws, w), 1, "{w} in {ws:?}");
    }

    // the same audio again (a re-plan, a caption edit) keeps the key; another gain does not
    let (_, again) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert_eq!(again.stretches[0].sig, s.sig);
    set_gain(&mut doc, &fx.parts[1], 5.0);
    let (_, other) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert_ne!(other.stretches[0].sig, s.sig);
    assert_eq!(other.missing.len(), 1);

    // the measurement policies
    let (_, all) = plan(&doc, &fx, &v, HearPolicy::All);
    assert_eq!(all.stretches.len(), 1);
    assert_eq!(all.stretches[0].parts, [0, 3]);
    assert_eq!(all.stretches[0].window, [0, 6 * 48_000]);
    let (_, none) = plan(&doc, &fx, &v, HearPolicy::None);
    assert!(none.stretches.is_empty());
}

#[test]
fn a_part_without_its_transcript_is_heard() {
    let p = open("heard-notr");
    let fx = fixture(&p, 2);
    captions_on(&p, &fx);
    let doc = probed(&p, &fx);
    // a transcript of another source only
    let mut v = AnalysisView::default();
    v.insert(
        "01J0000000000000000000ZZZZ",
        Transcript {
            revision: "r9".into(),
            words: vec![],
            lines: vec![],
        },
    );
    let (_, a) = planner::plan_cut_heard(
        &doc,
        uid(&fx.cut),
        uid(&fx.framing),
        &Fonts,
        Some(&v),
        HearPolicy::Rule,
    )
    .map(|(_, _, a)| ((), a))
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        a.parts
            .iter()
            .all(|p| p.changes == vec![Change::NoTranscript]),
        "{:?}",
        a.parts
    );
    // both parts are one stretch (1 s apart is closer than the context)
    assert_eq!(a.stretches.len(), 1);
    assert_eq!(a.stretches[0].parts, [0, 2]);
}

/// Two takes that continue the source (102.0 s is both the first's end and the second's
/// start), the second with a gain: the word across the join is said once whichever side
/// holds more of it, and context words the first take already says are not repeated.
#[test]
fn a_continuous_join_into_a_heard_part_says_the_straddling_word_once() {
    for (straddle, heard_at) in [
        ((1018, 1022), (19, 23)), // most of it after the join, heard 0.1 s late
        ((1017, 1021), (16, 20)), // most of it before the join, heard 0.1 s early
    ] {
        let p = open("heard-cont");
        let c = p.human("cut_create", json!({"title": "cont"}));
        let cut = c.output["cut"].as_str().unwrap().to_owned();
        let source = id(&p.human(
            "source_add",
            json!({"path": "/m/s.mkv", "fingerprint": "fp"}),
        ));
        let framing = id(&p.human(
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        ));
        p.human(
            "layer_add",
            json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}),
        );
        let a = id(&p.human(
            "part_add",
            json!({"cut": cut, "media": {"source": source}, "src_in": t(1000), "src_out": t(1020)}),
        ));
        let b = id(&p.human(
            "part_add",
            json!({"cut": cut, "after": a, "media": {"source": source}, "src_in": t(1020), "src_out": t(1040)}),
        ));
        let fx = common::Fx {
            cut,
            framing,
            source,
            screen: String::new(),
            cam: String::new(),
            parts: vec![a, b.clone()],
        };
        captions_on(&p, &fx);
        let mut doc = probed(&p, &fx);
        let mut v = view(
            &fx,
            &[
                ("we", 1005, 1009),
                ("built", 1010, 1015),
                ("across", straddle.0, straddle.1),
                ("the", 1025, 1028),
                ("seam", 1029, 1034),
            ],
        );
        set_gain(&mut doc, &b, 6.0);
        let (_, au) = plan(&doc, &fx, &v, HearPolicy::Rule);
        assert_eq!(au.stretches.len(), 1);
        let s = au.stretches[0].clone();
        v.insert_heard(&heard_body(
            &s,
            &[
                ("built", 11, 15), // context the first take says itself
                ("across", heard_at.0, heard_at.1),
                ("the", 25, 28),
                ("seam", 29, 34),
            ],
        ));
        let (ws, au) = plan(&doc, &fx, &v, HearPolicy::Rule);
        assert!(au.missing.is_empty());
        assert_eq!(ws, ["we", "built", "across", "the", "seam"], "{straddle:?}");
    }
}

/// A take that ends inside a word (it plays ≥ 0.1 s of it) next to a heard take, and a
/// heard take followed by one that starts inside a word: the reused side says its edge
/// word, the recogniser's hearing of the same fragment is not added, and nothing is lost.
#[test]
fn cut_edges_next_to_a_heard_part_lose_and_repeat_nothing() {
    let p = open("heard-edge");
    let fx = fixture(&p, 3);
    captions_on(&p, &fx);
    let mut doc = probed(&p, &fx);
    // takes: [100, 102] [103, 105] [106, 108] (output 0–2, 2–4, 4–6)
    let mut v = view(
        &fx,
        &[
            ("first", 1005, 1010),
            ("cutoff", 1017, 1023), // the first take plays 0.3 s of it
            ("middle", 1032, 1038),
            ("words", 1039, 1045),
            ("late", 1057, 1062), // the third take starts 0.2 s into it
            ("last", 1065, 1070),
        ],
    );
    set_gain(&mut doc, &fx.parts[1], 3.0);
    let (_, au) = plan(&doc, &fx, &v, HearPolicy::Rule);
    let s = au.stretches[0].clone();
    v.insert_heard(&heard_body(
        &s,
        &[
            ("cut", 17, 20), // the first take's edge fragment, heard as another word
            ("middle", 22, 28),
            ("words", 29, 35),
            ("late", 40, 42), // the third take's head, in the tail context
        ],
    ));
    let (ws, _) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert_eq!(ws, ["first", "cutoff", "middle", "words", "late", "last"]);
}

/// A caption edit made on a source word still applies when its part is heard and the
/// recogniser hears the same word there.
#[test]
fn heard_words_keep_the_source_word_they_are() {
    let p = open("heard-ids");
    let fx = fixture(&p, 1);
    captions_on(&p, &fx);
    let mut v = view(
        &fx,
        &[
            ("xd", 1002, 1006),
            ("dev", 1006, 1010),
            ("rocks", 1011, 1015),
        ],
    );
    p.human(
        "caption_edit",
        json!({"cut": fx.cut, "word": {"asr_revision": "r1", "ordinal": 0}, "edit": {"text": "exe"}}),
    );
    let mut doc = probed(&p, &fx);
    set_gain(&mut doc, &fx.parts[0], 2.0);
    let (_, au) = plan(&doc, &fx, &v, HearPolicy::Rule);
    let s = au.stretches[0].clone();
    v.insert_heard(&heard_body(
        &s,
        &[("xd", 3, 7), ("dev", 7, 11), ("rocks", 12, 16)],
    ));
    let (ws, au) = plan(&doc, &fx, &v, HearPolicy::Rule);
    assert!(au.missing.is_empty());
    assert_eq!(ws, ["exe", "dev", "rocks"]);
}

/// Decision 0030: a part at another speed is heard (its audio is stretched or muted, never
/// the source's), and its source words, when shown, follow the speed onto the output clock.
#[test]
fn a_sped_part_is_heard_and_its_words_follow_the_speed() {
    let p = open("heard-speed");
    let fx = fixture(&p, 3);
    captions_on(&p, &fx);
    let r = p.human("part_set_speed", json!({"part": fx.parts[1], "speed": 2.0}));
    assert_eq!(r.output["trimmed_s"], 0.0);
    assert_eq!(r.output["sound"], true);
    let doc = probed(&p, &fx);
    let v = view(&fx, &three_takes());
    let (plan, _, a) = planner::plan_cut_heard(
        &doc,
        uid(&fx.cut),
        uid(&fx.framing),
        &Fonts,
        Some(&v),
        HearPolicy::Rule,
    )
    .unwrap();
    assert_eq!(a.parts[1].changes, vec![Change::Speed]);
    assert!(a.parts[0].identical() && a.parts[2].identical());
    // 2 s, 1 s (2×), 2 s
    let outs: Vec<[u64; 2]> = plan.parts.iter().map(|p| p.out.0).collect();
    assert_eq!(outs, [[0, 60], [60, 90], [90, 150]]);
    let sp = plan.parts[1].speed.unwrap();
    assert_eq!((sp.num, sp.den, sp.stretches()), (2, 1, true));
    assert_eq!(plan.parts[1].picture.first_frame, 103 * 30);
    assert_eq!(plan.parts[1].audio.first_sample, 103 * 48_000);
    // never continuous: each side has its de-click
    assert!(plan.parts[1].audio.fade_in().is_some() && plan.parts[1].audio.fade_out().is_some());
    assert_eq!(plan.schema, gmclip_plan::consts::SchemaVersion::V1);
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    // the source words, shown: the 2× take's words land inside its output second (one
    // word a cue, so each cue's start is its word's)
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "caption_grouping": "single_word"}),
    );
    let doc = probed(&p, &fx);
    let (plan, _, _) = planner::plan_cut_heard(
        &doc,
        uid(&fx.cut),
        uid(&fx.framing),
        &Fonts,
        Some(&v),
        HearPolicy::None,
    )
    .unwrap();
    let caps: Vec<(String, [u64; 2])> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .map(|e| {
            (
                e.runs.iter().map(|r| r.text.to_lowercase()).collect(),
                e.out.0,
            )
        })
        .collect();
    let echo = caps.iter().find(|(t, _)| t.contains("echo")).unwrap();
    // "echo" is said 0.1 s into the take: 0.05 s into the 2× part (frame 61)
    assert!((61..=62).contains(&echo.1[0]), "{caps:?}");
    let golf = caps.iter().find(|(t, _)| t.contains("golf")).unwrap();
    assert!(golf.1[0] >= 60 && golf.1[0] < 90, "{caps:?}");
    let india = caps.iter().find(|(t, _)| t.contains("india")).unwrap();
    assert!(india.1[0] >= 90, "{caps:?}");
    // past 2× the sound is muted
    let r = p.human("part_set_speed", json!({"part": fx.parts[1], "speed": 3.0}));
    assert_eq!(r.output["sound"], false);
    let doc = probed(&p, &fx);
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&v)).unwrap();
    assert!(!plan.parts[1].speed.unwrap().stretches());
    assert_eq!(plan.parts[1].out.0, [60, 80]);
    // refusals: outside ¼–4×
    assert!(
        p.try_human("part_set_speed", json!({"part": fx.parts[1], "speed": 5.0}))
            .is_err()
    );
    // back to 1×: as before
    p.human("part_set_speed", json!({"part": fx.parts[1], "speed": 1.0}));
    let doc = probed(&p, &fx);
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&v)).unwrap();
    assert!(plan.parts.iter().all(|p| p.speed.is_none()));
}

/// Decision 0033: a speed ramp inside a part — 1× to 2× over source 103.5–104.25 s (½ s as
/// it plays, linear), then 2× — plans as `/1` `speed.ramp` with the part's frames from the
/// speed's integral; the words follow the ramp; past 2× it is muted; a split inside the
/// ramp gives each side its piece; null removes it.
#[test]
fn a_ramped_part_plans_its_integral_and_its_words_follow() {
    let p = open("heard-ramp");
    let fx = fixture(&p, 3);
    captions_on(&p, &fx);
    let r = p.human(
        "part_set_ramp",
        json!({"part": fx.parts[1], "ramp": {"to": 2.0, "start_ms": 500, "dur_ms": 500, "curve": "linear"}}),
    );
    // 0.5 s at 1×, 0.5 s ramping (0.75 s of source), 0.75 s of source at 2×
    assert_eq!(r.output["seconds"], 1.375);
    assert_eq!(r.output["sound"], true);
    let src = &r.output["src"];
    assert_eq!(src[0], 103_500i64 * 705_600);
    assert_eq!(src[1], 104_250i64 * 705_600);
    let doc = probed(&p, &fx);
    let v = view(&fx, &three_takes());
    let (plan, _, a) = planner::plan_cut_heard(
        &doc,
        uid(&fx.cut),
        uid(&fx.framing),
        &Fonts,
        Some(&v),
        HearPolicy::Rule,
    )
    .unwrap();
    assert_eq!(a.parts[1].changes, vec![Change::Speed]);
    // 15 frames at 1×, 15 ramping (22½ source frames), then 22½ at 2×: 11 more
    let outs: Vec<[u64; 2]> = plan.parts.iter().map(|p| p.out.0).collect();
    assert_eq!(outs, [[0, 60], [60, 101], [101, 161]]);
    let sp = plan.parts[1].speed.unwrap();
    assert_eq!((sp.num, sp.den, sp.stretches()), (1, 1, true));
    let ramp = sp.ramp.unwrap();
    assert_eq!(
        (ramp.to.num, ramp.to.den, ramp.from, ramp.frames),
        (2, 1, 15, 15)
    );
    assert_eq!(ramp.curve, gmclip_plan::types::EaseCurve::Linear);
    assert_eq!(plan.schema, gmclip_plan::consts::SchemaVersion::V1);
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    // the picture reads by the integral: 41 frames reach 59½ of the 60 source frames
    assert_eq!(sp.advance(41), Some((119, 2)));
    // the words, one a cue: "echo" (103.1 s) before the ramp at frame 63; "golf" (104.0 s)
    // inside it, 0.866 s in (frame 86); "hotel" (104.4 s) after it, 1.075 s in (frame 92)
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "caption_grouping": "single_word"}),
    );
    let doc = probed(&p, &fx);
    let (plan, _, _) = planner::plan_cut_heard(
        &doc,
        uid(&fx.cut),
        uid(&fx.framing),
        &Fonts,
        Some(&v),
        HearPolicy::None,
    )
    .unwrap();
    let start = |w: &str| {
        plan.text_events
            .iter()
            .flatten()
            .filter(|e| e.role == TextRole::Caption)
            .find(|e| e.runs.iter().any(|r| r.text.to_lowercase().contains(w)))
            .map(|e| e.out.0[0])
            .unwrap()
    };
    assert_eq!(start("echo"), 63);
    assert!((85..=87).contains(&start("golf")), "{}", start("golf"));
    assert!((91..=93).contains(&start("hotel")), "{}", start("hotel"));
    // to 3×: muted (past 2×)
    let r = p.human(
        "part_set_ramp",
        json!({"part": fx.parts[1], "ramp": {"to": 3.0, "dur_ms": 400}}),
    );
    assert_eq!(r.output["sound"], false);
    let doc = probed(&p, &fx);
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&v)).unwrap();
    let sp = plan.parts[1].speed.unwrap();
    assert!(!sp.stretches());
    assert_eq!(
        sp.ramp.unwrap().curve,
        gmclip_plan::types::EaseCurve::EaseInOut
    );
    // refusals: outside ¼–4×, the part's own speed, past the part's end, under a frame
    for bad in [
        json!({"to": 5.0, "dur_ms": 400}),
        json!({"to": 1.0, "dur_ms": 400}),
        json!({"to": 2.0, "start_ms": 1500, "dur_ms": 1000}),
        json!({"to": 2.0, "dur_ms": 10}),
    ] {
        assert!(
            p.try_human("part_set_ramp", json!({"part": fx.parts[1], "ramp": bad}))
                .is_err(),
            "{bad}"
        );
    }
    // a split inside the ramp: each side ramps over its piece, and the cut still plans
    p.human(
        "part_set_ramp",
        json!({"part": fx.parts[1], "ramp": {"to": 2.0, "start_ms": 500, "dur_ms": 500, "curve": "linear"}}),
    );
    let r = p.human(
        "part_split",
        json!({"part": fx.parts[1], "at": 104_000i64 * 705_600}),
    );
    let doc = probed(&p, &fx);
    let first: &gmclip_model::entities::Part = doc.get(uid(&fx.parts[1])).unwrap();
    let second: &gmclip_model::entities::Part =
        doc.get(uid(r.output["id"].as_str().unwrap())).unwrap();
    assert_eq!(first.ramp.map(|r| r.to), Some(second.speed));
    assert_eq!(second.ramp.map(|r| r.to), Some(2.0));
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&v)).unwrap();
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    assert_eq!(plan.parts.len(), 4);
    // the two halves last what the whole did, within a frame
    let len = plan.parts[1].out.len() + plan.parts[2].out.len();
    assert!((40..=42).contains(&len), "{len}");
    p.human("undo", json!({}));
    // null removes it: 2 s again
    p.human("part_set_ramp", json!({"part": fx.parts[1], "ramp": null}));
    let doc = probed(&p, &fx);
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&v)).unwrap();
    assert!(plan.parts.iter().all(|p| p.speed.is_none()));
    assert_eq!(plan.parts[1].out.0, [60, 120]);
}
