//! Decision 0040 on a real cut: what it says (`gmclip_core::script`) with ASR word times
//! that run on through the silence after a word, and a sweep that leaves no wordless
//! sliver behind.

mod common;

use common::{SEC, fixture, open};
use gmclip_model::entities::{Cut, Part};
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

fn f(s: f64) -> i64 {
    (s * SEC as f64) as i64
}

#[test]
fn a_word_whose_time_runs_on_through_silence_is_played_and_its_silence_heard() {
    let p = open("scriptlong");
    let fx = fixture(&p, 1); // one part, 100–102 s
    // "BB." is given 100.9–104.9 s by the ASR (the silence after it folded in)
    words(
        &p,
        &fx.source,
        &[("Install", 100.2, 100.6), ("BB.", 100.9, 104.9)],
    );
    let snap = p.h.snapshot();
    let sc = gmclip_core::script::script(&snap.doc, &snap.analysis, cut_id(&fx.cut));
    let part = &sc.parts[0];
    assert_eq!(
        part.text(),
        "Install BB.",
        "the part plays BB. (its onset is inside)"
    );
    assert!(
        part.cuts_word_out.is_none(),
        "the edge is in BB.'s silence, not in the word"
    );
    // spoken until 101.7 at most, so the part's last 0.3 s are heard as silence
    assert!(
        part.trail > SEC / 4 && part.trail < SEC / 2,
        "{}",
        part.trail
    );
    p.h.close().unwrap();
}

#[test]
fn a_word_timed_from_long_before_it_is_said_plays_where_it_is_said() {
    let p = open("scriptfront");
    let fx = fixture(&p, 1); // one part, 100–102 s
    // "Continue" is given 99.0–100.4 s (the pause before it folded in); the part starts
    // at 100 s, where it is said: it plays there, and no edge is said to cut it
    words(
        &p,
        &fx.source,
        &[("Continue", 99.0, 100.4), ("on.", 100.4, 100.7)],
    );
    let snap = p.h.snapshot();
    let sc = gmclip_core::script::script(&snap.doc, &snap.analysis, cut_id(&fx.cut));
    assert_eq!(sc.parts[0].text(), "Continue on.");
    assert!(sc.parts[0].cuts_word_in.is_none());
    p.h.close().unwrap();
}

#[test]
fn a_sweep_leaves_no_wordless_sliver() {
    let p = open("sliver");
    let fx = fixture(&p, 1); // one part, 100–102 s
    words(
        &p,
        &fx.source,
        &[("one.", 100.1, 100.4), ("two.", 101.5, 101.8)],
    );
    // two spans with 0.2 s of quiet between them: that piece says nothing and goes
    p.human(
        "sweep_apply",
        json!({"cut": fx.cut, "kind": "pauses", "revision": "quiet@1/fp", "spans": [
            {"media": {"source": fx.source}, "from": f(100.5), "to": f(100.8)},
            {"media": {"source": fx.source}, "from": f(101.0), "to": f(101.4)}]}),
    );
    let ps = parts(&p.h.snapshot().doc, &fx.cut);
    assert_eq!(ps.len(), 2, "one. and two., no sliver between: {ps:?}");
    assert!(ps.iter().all(|(a, b)| b - a >= SEC / 2), "{ps:?}");
    p.h.close().unwrap();
}

/// Stores a voice for `owner`: speech (−20 dB) over `spoken`, the room (−70 dB) elsewhere,
/// as the analysis host writes it (`energy_10ms`, `silences_40ms`).
fn voice(p: &common::Project, owner: &str, spoken: &[(f64, f64)]) {
    let levels: Vec<(f64, f64, f32)> = spoken.iter().map(|&(a, b)| (a, b, -20.0)).collect();
    voice_levels(p, owner, &levels);
}

/// [`voice`] with each stretch at its own level (dB); the room (−70 dB, under the quiet
/// grid's −48 dB threshold) elsewhere.
fn voice_levels(p: &common::Project, owner: &str, levels: &[(f64, f64, f32)]) {
    use gmclip_analysis::blob::{Blob, Param};
    use gmclip_analysis::energy::{Energy, Hop};
    let (rate, window, hop) = (16_000u32, 160usize, 80usize);
    let db = (0..103 * 200)
        .map(|i| {
            let t = (i * hop + window / 2) as f64 / f64::from(rate);
            levels
                .iter()
                .find(|&&(a, b, _)| a <= t && t < b)
                .map_or(-70.0, |&(_, _, d)| d)
        })
        .collect();
    let e = Energy {
        rate,
        window,
        hop,
        db,
    };
    let body = e.to_blob(Hop::Ms10).encode().unwrap();
    p.h.put_analysis(owner, "energy_10ms", "rms@1/test", body)
        .unwrap();
    let mut s = Blob {
        kind: "silences_40ms".into(),
        ..Default::default()
    };
    s.params.insert("threshold_db".into(), Param::Float(-48.0));
    p.h.put_analysis(
        owner,
        "silences_40ms",
        "quiet_grid@1/test",
        s.encode().unwrap(),
    )
    .unwrap();
}

/// Decision 0048: with the voice analysed, a word is judged where it is heard, not where
/// the ASR timed it.
#[test]
fn words_are_judged_where_the_voice_is_heard() {
    let p = open("scriptheard");
    let fx = fixture(&p, 1); // one part, 100–102 s
    // "better" is timed 101.7–102.26 s but heard only to 101.85: the part's edge at 102 s
    // is in its silence; "you." is timed 100.55–101.5 s, all of it the room
    words(
        &p,
        &fx.source,
        &[
            ("for", 100.2, 100.5),
            ("you.", 100.55, 101.5),
            ("better", 101.7, 102.26),
        ],
    );
    let snap = p.h.snapshot();
    let sc = gmclip_core::script::script(&snap.doc, &snap.analysis, cut_id(&fx.cut));
    assert!(
        sc.parts[0].cuts_word_out.is_some(),
        "unmeasured, the ASR's time is all there is"
    );
    voice(&p, &fx.source, &[(100.2, 100.5), (101.7, 101.85)]);
    let snap = p.h.snapshot();
    let sc = gmclip_core::script::script(&snap.doc, &snap.analysis, cut_id(&fx.cut));
    let part = &sc.parts[0];
    assert!(
        part.cuts_word_out.is_none(),
        "102 s is after better is heard"
    );
    assert!(
        part.gaps.iter().any(|g| g.len > SEC),
        "you. is not heard: 100.5–101.7 s is a silence, {:?}",
        part.gaps
    );
    p.h.close().unwrap();
}

/// A word of the test transcript as an edit names it: its ASR edges are its fingerprint.
fn word_ref(ordinal: u32, (text, s, e): (&str, f64, f64)) -> Value {
    json!({"word_id": {"asr_revision": "asr-test@1", "ordinal": ordinal}, "t_src_start": f(s), "t_src_end": f(e), "normalised_text": text})
}

/// Within a frame (1/30 s) of `want` seconds.
fn near(got: i64, want: f64) -> bool {
    (got - f(want)).abs() <= SEC / 30
}

fn secs(t: i64) -> f64 {
    t as f64 / SEC as f64
}

/// Decision 0049: a strike cuts where the struck word is heard, from 40 ms before its
/// voice to 120 ms after, not on the ASR's edges (which here run "um" on into the pause
/// after it).
#[test]
fn a_strike_cuts_on_the_voice() {
    let ws = [
        ("So", 100.2, 100.5),
        ("um", 100.6, 101.3),
        ("then", 101.4, 101.8),
    ];
    let strike_um = |tag: &str, heard: bool| {
        let p = open(tag);
        let fx = fixture(&p, 1); // one part, 100–102 s
        words(&p, &fx.source, &ws);
        if heard {
            voice(
                &p,
                &fx.source,
                &[(100.25, 100.45), (100.7, 100.85), (101.5, 101.8)],
            );
        }
        let out = p.human(
            "words_strike",
            json!({"cut": fx.cut, "words": [word_ref(1, ws[1])]}),
        );
        let ps = parts(&p.h.snapshot().doc, &fx.cut);
        p.h.close().unwrap();
        (out.output, ps)
    };
    // unmeasured: the ASR's edges
    let (_, ps) = strike_um("strikeasr", false);
    assert!(near(ps[0].1, 100.6) && near(ps[1].0, 101.3), "{ps:?}");
    // heard: um's voice, 100.70–100.85 s, goes with 40 ms before it and 120 ms after
    let (out, ps) = strike_um("strikeheard", true);
    assert_eq!(ps.len(), 2, "{ps:?}");
    assert!(near(ps[0].1, 100.66), "cut in at {}", secs(ps[0].1));
    assert!(near(ps[1].0, 100.97), "cut out at {}", secs(ps[1].0));
    assert_eq!(out["fragments_dropped"], 0);
}

/// A quiet first word's softer onset (over the room, under the voice) is kept when it runs
/// into the voice from within 250 ms: a restore grows the part back over it.
#[test]
fn a_restore_keeps_a_soft_onset() {
    let p = open("restoresoft");
    let fx = fixture(&p, 1); // one part, 100–102 s
    let ws = [("I", 100.3, 100.6), ("think", 100.7, 101.0)];
    words(&p, &fx.source, &ws);
    // "I" comes in at −40 dB from 100.15 s and is voiced from 100.3 s
    voice_levels(
        &p,
        &fx.source,
        &[
            (100.15, 100.3, -40.0),
            (100.3, 100.55, -20.0),
            (100.7, 101.0, -20.0),
        ],
    );
    p.human(
        "part_trim",
        json!({"part": fx.parts[0], "src_in": f(100.7), "src_out": f(102.0)}),
    );
    p.human(
        "words_restore",
        json!({"cut": fx.cut, "words": [word_ref(0, ws[0])]}),
    );
    let ps = parts(&p.h.snapshot().doc, &fx.cut);
    assert!(near(ps[0].0, 100.11), "restored from {}", secs(ps[0].0));
    p.h.close().unwrap();
}

/// A piece too short to be a part (under 0.12 s) left beside a strike is dropped with it,
/// and the strike says so.
#[test]
fn a_strike_reports_the_fragment_it_drops() {
    let p = open("strikefrag");
    let fx = fixture(&p, 1); // one part, 100–102 s
    let ws = [
        ("I", 100.0, 100.06),
        ("mean", 100.08, 100.9),
        ("so", 101.2, 101.5),
    ];
    words(&p, &fx.source, &ws);
    let out = p.human(
        "words_strike",
        json!({"cut": fx.cut, "words": [word_ref(1, ws[1])]}),
    );
    assert_eq!(out.output["fragments_dropped"], 1, "{}", out.output);
    assert_eq!(parts(&p.h.snapshot().doc, &fx.cut).len(), 1);
    p.h.close().unwrap();
}

/// A strike that reaches a part's first word takes the part's quiet head with it
/// (decision 0040): no sliver of silence is left as a part.
#[test]
fn a_strike_at_a_parts_first_word_takes_its_quiet_head() {
    let p = open("strikehead");
    let fx = fixture(&p, 1); // one part, 100–102 s
    let ws = [("one", 100.5, 100.8), ("two", 101.2, 101.5)];
    words(&p, &fx.source, &ws);
    p.human(
        "words_strike",
        json!({"cut": fx.cut, "words": [word_ref(0, ws[0])]}),
    );
    let ps = parts(&p.h.snapshot().doc, &fx.cut);
    assert_eq!(ps.len(), 1, "{ps:?}");
    assert!(near(ps[0].0, 100.8), "{ps:?}");
    p.h.close().unwrap();
}
