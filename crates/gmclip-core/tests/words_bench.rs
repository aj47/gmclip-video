//! R-WORD-1 [M2] (backlog A2.04): words at scale on a 3-hour transcript (~29 000 words)
//! through the actor (handler + commit + round trip):
//! - strike, restore and keyword toggle, each one undo step, p99 ≤ 16 ms;
//! - server-side find of a common word over the whole transcript ≤ 50 ms;
//! - find & replace in the captions as one transaction.
//!
//! The bounds are the M2 gate (mac-ref, release). A debug build is checked against
//! looser bounds; the numbers in `milestones/m2/perf.md` come from
//! `cargo test --release -p gmclip-core --test words_bench -- --nocapture`
//! (`GMCLIP_BENCH_N` sets the calls per command, default 200).

mod common;

use std::time::{Duration, Instant};

use common::{SEC, open};
use serde_json::{Value, json};

const REV: &str = "asr-bench@1";
/// A word every 0.37 s, 0.3 s long: ~2.7 words per second, AJ's talking pace.
const STEP: i64 = SEC * 37 / 100;
const LEN: i64 = SEC * 30 / 100;
const VOCAB: &[&str] = &[
    "the", "model", "is", "running", "on", "my", "GPU", "and", "it", "took", "about", "a",
    "minute", "to", "generate", "this", "video", "so", "we", "can", "see", "the", "result", "now",
    "let's", "try", "another", "prompt", "with", "ComfyUI",
];

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn word_text(i: usize) -> &'static str {
    VOCAB[(i + i / 97) % VOCAB.len()]
}

fn word_ref(i: usize) -> Value {
    let s = i as i64 * STEP;
    json!({"word_id": {"asr_revision": REV, "ordinal": i}, "t_src_start": s, "t_src_end": s + LEN,
           "normalised_text": word_text(i).to_lowercase()})
}

#[test]
fn words_at_scale_on_a_3_hour_transcript() {
    let n: usize = std::env::var("GMCLIP_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let p = open("words-3h");
    let total = 3 * 3600 * SEC;
    let nwords = (total / STEP) as usize;
    let source = common::id(&p.human(
        "source_add",
        json!({"path": "/m/stream.mkv", "fingerprint": "fp3h"}),
    ));
    let words: Vec<Value> = (0..nwords)
        .map(|i| {
            let s = i as i64 * STEP;
            json!({"text": word_text(i), "start": s, "end": s + LEN})
        })
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": REV, "words": words});
    p.h.put_analysis(&source, "words", REV, body.to_string().into_bytes())
        .unwrap();
    let cut = p.human("cut_create", json!({"title": "3 h"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    );
    p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": source}, "src_in": 0, "src_out": total}),
    );
    let parts = |p: &common::Project| {
        p.h.snapshot()
            .doc
            .iter::<gmclip_model::entities::Part>()
            .count()
    };

    // n distinct words struck across the 3 hours (the cut grows to n + 1 parts), then
    // restored in reverse; each is one transaction
    let picks: Vec<usize> = (0..n)
        .map(|i| 10 + (i * (nwords - 20) / n.max(1)))
        .collect();
    let (mut ts, mut tr, mut tk) = (Vec::new(), Vec::new(), Vec::new());
    for &k in &picks {
        p.clock.advance(1);
        let t0 = Instant::now();
        p.h.human("words_strike", json!({"cut": cut, "words": [word_ref(k)]}))
            .unwrap_or_else(|e| panic!("strike {k}: {e:?}"));
        ts.push(t0.elapsed());
    }
    assert_eq!(parts(&p), n + 1);
    // one undo step per strike
    p.human("undo", json!({}));
    assert_eq!(parts(&p), n);
    p.human("redo", json!({}));
    assert_eq!(parts(&p), n + 1);
    for &k in picks.iter().rev() {
        p.clock.advance(1);
        let t0 = Instant::now();
        p.h.human("words_restore", json!({"cut": cut, "words": [word_ref(k)]}))
            .unwrap_or_else(|e| panic!("restore {k}: {e:?}"));
        tr.push(t0.elapsed());
    }
    for &k in &picks {
        p.clock.advance(1);
        let t0 = Instant::now();
        p.h.human(
            "caption_keyword_toggle",
            json!({"cut": cut, "word": {"asr_revision": REV, "ordinal": k}}),
        )
        .unwrap();
        tk.push(t0.elapsed());
    }

    // find a common word over the whole transcript (best of 5, warm)
    let mut tf = Vec::new();
    let mut found = Value::Null;
    for _ in 0..5 {
        let t0 = Instant::now();
        found = p
            .human(
                "words_find",
                json!({"owner": source, "query": "the", "limit": 10000}),
            )
            .output;
        tf.push(t0.elapsed());
    }
    let common_hits = found["total"].as_u64().unwrap();
    assert!(common_hits > 1000, "{common_hits}");
    let phrase = p
        .human("words_find", json!({"owner": source, "query": "my gpu"}))
        .output;
    let phrase_hits = phrase["total"].as_u64().unwrap();
    assert!(phrase_hits > 0);
    assert_eq!(phrase["matches"][0]["text"], "my GPU");

    // replace in the captions: every match, one transaction, one undo step
    let t0 = Instant::now();
    let r = p
        .human(
            "words_replace",
            json!({"cut": cut, "owner": source, "query": "comfy*", "to": "Comfy UI"}),
        )
        .output;
    let t_replace = t0.elapsed();
    assert!(r["matches"].as_u64().unwrap() > 100);
    let edits = |p: &common::Project| {
        p.h.snapshot()
            .doc
            .iter::<gmclip_model::entities::CaptionDoc>()
            .map(|c| c.edits.len())
            .sum::<usize>()
    };
    assert_eq!(edits(&p) as u64, r["words"].as_u64().unwrap());
    p.human("undo", json!({}));
    assert_eq!(edits(&p), 0);

    let (s99, r99, k99) = (pct(&mut ts, 0.99), pct(&mut tr, 0.99), pct(&mut tk, 0.99));
    let find = *tf.iter().min().unwrap();
    eprintln!(
        "R-WORD-1, 3 h transcript ({nwords} words), {n} calls each (handler + commit + actor round trip):"
    );
    eprintln!(
        "  strike   p50 {:>9.3?}  p99 {s99:>9.3?}",
        pct(&mut ts, 0.5)
    );
    eprintln!(
        "  restore  p50 {:>9.3?}  p99 {r99:>9.3?}",
        pct(&mut tr, 0.5)
    );
    eprintln!(
        "  keyword  p50 {:>9.3?}  p99 {k99:>9.3?}",
        pct(&mut tk, 0.5)
    );
    eprintln!("  find \"the\" ({common_hits} hits)  best {find:?}");
    eprintln!(
        "  replace \"comfy*\" ({} matches, one transaction)  {t_replace:?}",
        r["matches"]
    );
    let (word_bound, find_bound) = if cfg!(debug_assertions) {
        (Duration::from_millis(100), Duration::from_millis(250))
    } else {
        (Duration::from_millis(16), Duration::from_millis(50))
    };
    assert!(
        s99.max(r99).max(k99) <= word_bound,
        "{s99:?} {r99:?} {k99:?}"
    );
    assert!(find <= find_bound, "{find:?}");
    p.h.close().unwrap();
}
