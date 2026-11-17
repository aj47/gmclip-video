//! A2.06 property: a sweep never removes a word. Whatever spans a sweep lists between
//! words (pauses, fillers' neighbours), `sweep_apply` cuts only inside them (shrunk to
//! whole frames), so every word the cut played before it still plays whole after, and
//! the cut loses no more time than the spans hold. Also: a preview's cut signature
//! refuses a cut that moved (`Conflict`).

mod common;

use common::{SEC, fixture, open};
use gmclip_model::entities::{Cut, Part};
use gmclip_model::{Doc, Id, Ulid};
use serde_json::json;

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: i64) -> i64 {
        (self.next() % n.max(1) as u64) as i64
    }
}

fn parts(doc: &Doc, cut: &str) -> Vec<(i64, i64)> {
    let c: Id<Cut> = Id::new(Ulid::parse(cut).unwrap());
    doc.children::<Part>(c.entity())
        .filter_map(|p| doc.get(p))
        .map(|p| (p.src_in.0, p.src_out.0))
        .collect()
}

fn plays(ps: &[(i64, i64)], w: (i64, i64)) -> bool {
    ps.iter().any(|&(a, b)| a <= w.0 && w.1 <= b)
}

#[test]
fn a_sweep_never_removes_a_word() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for round in 0..48 {
        let p = open("sweep-props");
        // parts of 2 s at 100, 103, 106 … s
        let n = 2 + (round % 5);
        let fx = fixture(&p, n);
        let before = parts(&p.h.snapshot().doc, &fx.cut);
        // words over the parts with random gaps between them
        let mut words = Vec::new();
        let mut t = 100 * SEC;
        let end = before.last().unwrap().1;
        while t < end {
            t += SEC / 20 + rng.below(SEC * 6 / 10);
            // some words shorter than a part may be ("a", "I"): the fragment rule's edge
            let len = SEC / 25 + rng.below(SEC / 4);
            words.push((t, t + len));
            t += len;
        }
        let body = json!({"schema": "gmclip.analysis.words/0", "revision": format!("props-{round}"),
            "words": words.iter().map(|(a, b)| json!({"text": "w", "start": a, "end": b})).collect::<Vec<_>>()});
        p.h.put_analysis(
            &fx.source,
            "words",
            &format!("props-{round}"),
            body.to_string().into_bytes(),
        )
        .unwrap();
        // spans in the gaps between words (some touching a word's edge exactly)
        let mut spans = Vec::new();
        for w in words.windows(2) {
            let (gap_a, gap_b) = (w[0].1, w[1].0);
            if gap_b - gap_a < SEC / 50 || rng.below(4) == 0 {
                continue;
            }
            // half the spans run right up to the words around them
            let a = if rng.below(2) == 0 {
                gap_a
            } else {
                gap_a + rng.below((gap_b - gap_a) / 3 + 1)
            };
            let b = if rng.below(2) == 0 {
                gap_b
            } else {
                gap_b - rng.below((gap_b - a) / 3 + 1)
            };
            if b > a {
                spans.push(json!({"media": {"source": fx.source}, "from": a, "to": b}));
            }
        }
        if spans.is_empty() {
            continue;
        }
        let total_spans: i64 = spans
            .iter()
            .map(|s| s["to"].as_i64().unwrap() - s["from"].as_i64().unwrap())
            .sum();
        let sig = gmclip_core::commands::analysis::cut_sig(
            &p.h.snapshot().doc,
            Id::new(Ulid::parse(&fx.cut).unwrap()),
        );
        p.human(
            "sweep_apply",
            json!({"cut": fx.cut, "kind": "pauses", "spans": spans, "revision": "props@1", "cut_sig": sig}),
        );
        let after = parts(&p.h.snapshot().doc, &fx.cut);
        for w in &words {
            if plays(&before, *w) {
                assert!(plays(&after, *w), "round {round}: a word was cut: {w:?}");
            }
        }
        let len = |ps: &[(i64, i64)]| ps.iter().map(|(a, b)| b - a).sum::<i64>();
        let lost = len(&before) - len(&after);
        // fragments under 0.12 s left between removals are dropped too (SPEC §5.3),
        // never a word: the words check above covers them
        assert!(lost >= 0 && lost <= total_spans + after.len() as i64 * SEC * 12 / 100);
        // the same signature again: the cut moved, so a stale preview conflicts
        let e = p
            .try_human(
                "sweep_apply",
                json!({"cut": fx.cut, "kind": "pauses", "spans": [{"media": {"source": fx.source}, "from": 100 * SEC, "to": 100 * SEC + SEC / 2}], "revision": "props@1", "cut_sig": sig}),
            )
            .unwrap_err();
        assert_eq!(e.code(), "Conflict", "round {round}");
        p.h.close().unwrap();
    }
}
