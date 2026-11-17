//! A2.08 (R-PROP-1, R-STORY-1): a story pass writes suggestions and the arc and changes
//! no part; "use the proposed cut" is one transaction (one undo step); a part the human
//! deleted is never re-added, by a later apply or by a gather that proposes it again.

mod common;

use common::{Project, SEC, fixture, open};
use gmclip_model::entities::{Cut, Part, StorySuggestion, SuggestionState};
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};

fn raw(src: &str, t: i64) -> Value {
    json!({"raw": {"time": {"clock": "src", "source": src, "t": t}, "reason": "gather"}})
}

fn spans(src: &str) -> Value {
    let one = |a: i64, role: &str| json!({"span": {"start": raw(src, a * SEC), "end": raw(src, (a + 2) * SEC)}, "role": role, "relevance": 0.7, "quote": format!("at {a}")});
    json!([
        one(110, "setup"),
        one(120, "evidence"),
        one(130, "payoff"),
        one(140, "reaction")
    ])
}

/// The cut's parts as (id, src_in seconds), in order.
fn parts(p: &Project, cut: &str) -> Vec<(String, i64)> {
    let d = p.h.snapshot().doc.clone();
    let c: Id<Cut> = Id::new(Ulid::parse(cut).unwrap());
    d.children::<Part>(c.entity())
        .filter_map(|id| d.get(id).map(|x| (id.0.to_string(), x.src_in.0 / SEC)))
        .collect()
}

#[test]
fn gather_changes_no_part_apply_is_one_step_deleted_stays_deleted() {
    let p = open("story");
    let fx = fixture(&p, 1);
    let cut = fx.cut.clone();
    p.human(
        "part_set_role",
        json!({"part": fx.parts[0], "role": "hook"}),
    );
    p.human(
        "story_brief_set",
        json!({"cut": cut, "brief": "the local run"}),
    );
    let before = parts(&p, &cut);

    // gather (an agent's pass): suggestions and the arc only
    let g = p
        .agent(
            "a",
            "story_suggestions_set",
            json!({"cut": cut, "suggestions": spans(&fx.source), "arc": [2, 0, 3, 1]}),
            p.as_of(),
        )
        .unwrap();
    assert_eq!(g.output["ids"].as_array().unwrap().len(), 4);
    assert_eq!(parts(&p, &cut), before, "gather changes no part");

    // use the proposed cut: hook, then the arc
    let a = p.human("story_apply", json!({"cut": cut}));
    assert_eq!(a.output["added"].as_array().unwrap().len(), 4);
    let after: Vec<i64> = parts(&p, &cut).iter().map(|x| x.1).collect();
    assert_eq!(after, vec![100, 130, 110, 140, 120]);
    // one undo step
    p.human("undo", json!({}));
    assert_eq!(parts(&p, &cut), before, "apply is one undo step");
    p.human("redo", json!({}));
    assert_eq!(parts(&p, &cut).len(), 5);

    // the human deletes the 130 s part: its suggestion is rejected
    let gone = parts(&p, &cut)
        .iter()
        .find(|x| x.1 == 130)
        .unwrap()
        .0
        .clone();
    p.human("part_delete", json!({"part": gone}));
    let d = p.h.snapshot().doc.clone();
    let rejected = d
        .iter::<StorySuggestion>()
        .filter(|s| s.state == SuggestionState::Rejected)
        .count();
    assert_eq!(rejected, 1);
    // apply again: not re-added
    let a = p.human("story_apply", json!({"cut": cut}));
    assert_eq!(a.output["added"].as_array().unwrap().len(), 0);
    assert_eq!(a.output["skipped_rejected"], 1);
    assert!(parts(&p, &cut).iter().all(|x| x.1 != 130));
    // a new gather proposing the same span: stored rejected, still not re-added
    let g = p
        .agent(
            "a",
            "story_suggestions_set",
            json!({"cut": cut, "suggestions": spans(&fx.source)}),
            p.as_of(),
        )
        .unwrap();
    assert_eq!(g.output["rejected"], 1);
    let n = parts(&p, &cut).len();
    p.human("story_apply", json!({"cut": cut}));
    assert!(parts(&p, &cut).iter().all(|x| x.1 != 130));
    assert_eq!(
        parts(&p, &cut).len(),
        n,
        "everything else was already in the cut"
    );
    p.h.close().unwrap();
}

/// Property: over random gathers, applies and human deletes, a deleted span never comes
/// back, and gathers never change the parts.
#[test]
fn deleted_spans_never_come_back() {
    let mut seed: u64 = 0x5eed;
    let mut next = move |m: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for round in 0..8 {
        let p = open(&format!("storyprop{round}"));
        let fx = fixture(&p, 1);
        let cut = fx.cut.clone();
        let mut deleted: Vec<i64> = Vec::new();
        for _ in 0..12 {
            match next(3) {
                0 => {
                    let before = parts(&p, &cut);
                    let k = 1 + next(4) as usize;
                    let all = spans(&fx.source);
                    let some: Vec<Value> = all.as_array().unwrap()[..k].to_vec();
                    p.agent(
                        "a",
                        "story_suggestions_set",
                        json!({"cut": cut, "suggestions": some}),
                        p.as_of(),
                    )
                    .unwrap();
                    assert_eq!(parts(&p, &cut), before, "gather changed parts");
                }
                1 => {
                    p.human("story_apply", json!({"cut": cut}));
                }
                _ => {
                    let ps = parts(&p, &cut);
                    if ps.len() > 1 {
                        let (id, t) = ps[1 + next(ps.len() as u64 - 1) as usize].clone();
                        p.human("part_delete", json!({"part": id}));
                        deleted.push(t);
                    }
                }
            }
            for t in &deleted {
                assert!(
                    parts(&p, &cut).iter().all(|x| x.1 != *t),
                    "span at {t} s came back"
                );
            }
        }
        p.h.close().unwrap();
    }
}
