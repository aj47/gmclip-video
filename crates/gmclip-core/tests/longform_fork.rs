//! Ideas 2026-10-16 (K.03): fork a long-form into a sibling to try another order, compare
//! two long-forms side by side (topics, order, durations), and pick one or keep both.

mod common;

use common::{Project, SEC, open};
use gmclip_model::entities::{Cut, CutKind, Part};
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};

const HOUR: i64 = 3600 * SEC;

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// A word every 0.4 s: word `n` starts at `n·0.4 s` and lasts 0.3 s.
fn word(n: u64) -> Value {
    let t = n as i64 * SEC * 2 / 5;
    json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": t, "t_src_end": t + SEC * 3 / 10, "normalised_text": format!("w{n}")})
}

fn w_at(s: i64) -> u64 {
    (s * 5 / 2) as u64
}

fn stream(tag: &str) -> (Project, String) {
    let p = open(tag);
    p.human(
        "source_add",
        json!({"path": "/rec/stream.mkv", "fingerprint": "fp",
               "probe": {"duration": HOUR, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080,
                          "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}], "audio_tracks": []}}),
    );
    let cut = p.human("stream_edit_ensure", json!({})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    (p, cut)
}

fn label(p: &Project, from_s: i64, to_s: i64, title: &str) -> String {
    p.human(
        "topic_set",
        json!({"start": word(w_at(from_s)), "end": word(w_at(to_s)), "title": title}),
    )
    .output["topic"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn parts(p: &Project, cut: &str) -> Vec<(i64, i64)> {
    let d = p.h.snapshot().doc.clone();
    gmclip_model::clock::cut_layout(&d, uid::<Cut>(cut))
        .iter()
        .filter_map(|sp| d.get::<Part>(sp.part).map(|x| (x.src_in.0, x.src_out.0)))
        .collect()
}

fn code(e: &gmclip_core::CmdError) -> String {
    serde_json::to_value(e).unwrap()["code"]
        .as_str()
        .unwrap_or("?")
        .to_owned()
}

#[test]
fn a_fork_is_a_sibling_in_another_order_and_compare_says_what_moved() {
    let (p, stream_cut) = stream("fork");
    let a = label(&p, 100, 400, "Setup");
    let b = label(&p, 1000, 1600, "GPU prices");
    let c = label(&p, 2000, 2300, "Results");
    p.human(
        "words_strike",
        json!({"cut": stream_cut, "words": [word(w_at(200)), word(w_at(230))]}),
    );
    let made = p
        .human(
            "longform_from_topics",
            json!({"topics": [a, b, c], "order": "source", "title": "Local AI video"}),
        )
        .output;
    let one = made["cut"].as_str().unwrap().to_owned();
    let before = parts(&p, &one);
    assert_eq!(before.len(), 4, "Setup in two stretches around the strike");

    // the same order: a copy with the same parts and lineage
    let f = p
        .human("longform_fork", json!({"cut": one, "title": "Copy"}))
        .output;
    let copy = f["cut"].as_str().unwrap().to_owned();
    assert_eq!(parts(&p, &copy), before);
    let d = p.h.snapshot().doc.clone();
    let (o, k) = (
        d.get(uid::<Cut>(&one)).unwrap(),
        d.get(uid::<Cut>(&copy)).unwrap(),
    );
    assert_eq!(k.kind, CutKind::Long);
    assert_eq!(
        k.lineage, o.lineage,
        "a sibling: same stream edit, topics, base"
    );
    assert!(k.brief.contains("Local AI video"), "{}", k.brief);
    // the parent diff still works on the fork (nothing pending)
    let diff = p.human("cut_parent_diff", json!({"cut": copy})).output;
    assert_eq!(diff["edits"].as_array().unwrap().len(), 0);

    // another order: results first, then setup (both its stretches), then GPU prices
    let f = p
        .human("longform_fork", json!({"cut": one, "topics": [c, a, b]}))
        .output;
    let two = f["cut"].as_str().unwrap().to_owned();
    let after = parts(&p, &two);
    assert_eq!(after, [before[3], before[0], before[1], before[2]]);
    let ch: Vec<&str> = f["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(ch, ["Results", "Setup", "GPU prices"]);
    assert_eq!(f["title"], "Local AI video (fork)");

    // compare: same topics and length, the order differs
    let cmp = p
        .human("longform_compare", json!({"a": one, "b": two}))
        .output;
    assert_eq!(cmp["same_topics"], true);
    assert_eq!(cmp["same_order"], false);
    assert_eq!(cmp["a"]["duration_ms"], cmp["b"]["duration_ms"]);
    let order_b: Vec<&str> = cmp["b"]["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(order_b, ["Results", "Setup", "GPU prices"]);
    // each chapter's length adds up to the cut's
    let sum: i64 = cmp["b"]["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["dur_ms"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, cmp["b"]["duration_ms"].as_i64().unwrap());
    assert_eq!(cmp["moved"].as_array().unwrap().len(), 3);
    assert!(cmp["only_a"].as_array().unwrap().is_empty());

    // pick the fork: the other goes, in one undoable step
    let n_cuts = p.h.snapshot().doc.iter::<Cut>().count();
    let picked = p
        .human("longform_pick", json!({"keep": two, "drop": one}))
        .output;
    assert_eq!(picked["kept"], two.as_str());
    let d = p.h.snapshot().doc.clone();
    assert!(d.get(uid::<Cut>(&one)).is_none());
    assert_eq!(d.iter::<Cut>().count(), n_cuts - 1);
    p.human("undo", json!({}));
    assert_eq!(parts(&p, &one), before);
}

#[test]
fn fork_and_pick_refuse_what_is_not_a_long_form() {
    let (p, stream_cut) = stream("fork-refuse");
    let a = label(&p, 100, 400, "Setup");
    let b = label(&p, 1000, 1600, "GPU prices");
    let one = p
        .human("longform_from_topics", json!({"topics": [a, b]}))
        .output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    // the stream edit is not a long-form
    let e = p
        .try_human("longform_fork", json!({"cut": stream_cut}))
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    let e = p
        .try_human("longform_pick", json!({"keep": one, "drop": stream_cut}))
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    let e = p
        .try_human("longform_pick", json!({"keep": one, "drop": one}))
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    // an order that is not the long-form's topics
    let e = p
        .try_human("longform_fork", json!({"cut": one, "topics": [a]}))
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    let e = p
        .try_human("longform_fork", json!({"cut": one, "topics": [a, a]}))
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    // compare a long-form with itself is fine (keep both is "do nothing")
    let cmp = p
        .human("longform_compare", json!({"a": one, "b": one}))
        .output;
    assert_eq!(cmp["same_order"], true);
}
