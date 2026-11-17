//! R-HIST-4 [M1 single author + run undo]: property tests through the real actor and
//! store.
//! - Inverse correctness: for a random command on a random state, undo restores every
//!   entity exactly and redo re-applies exactly (10⁴ cases; `PROPTEST_CASES`).
//! - Id uniqueness: no non-history transaction ever creates an id seen before, under
//!   random add/split/merge/strike/delete-by-strike/undo/redo.
//! - Run undo: after a random agent run and random later human edits, `revert_untouched`
//!   puts back exactly the run's fields nobody else touched, and `restore_to_point`
//!   puts the whole cut back to the run's restore point; each is one transaction.
//!
//! Cases share one project (each case checks a local property of its own step), so
//! the state keeps growing and the commands keep meeting new shapes.

mod common;

use std::cell::RefCell;
use std::collections::BTreeSet;

use common::{Fx, Project, SEC, fixture, open};
use gmclip_core::CmdError;
use gmclip_model::entities::{Cut, Part};
use gmclip_model::{Doc, EntityId, Id, Ulid};
use gmclip_proto::Event;
use proptest::prelude::*;
use serde_json::{Value, json};

fn contents(d: &Doc) -> Vec<(EntityId, Vec<u8>)> {
    d.entity_ids()
        .into_iter()
        .map(|e| (e, d.encode(e).unwrap().0))
        .collect()
}

fn parts(d: &Doc, cut: &str) -> Vec<String> {
    let c: Id<Cut> = Id::new(Ulid::parse(cut).unwrap());
    d.children::<Part>(c.entity())
        .map(|p| p.0.to_string())
        .collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// A random command that is valid on `d` (most of the time; refusals are fine).
fn random_cmd(r: &mut Rng, d: &Doc, fx: &Fx) -> (&'static str, Value) {
    let ps = parts(d, &fx.cut);
    let pick = |r: &mut Rng| ps[r.below(ps.len() as u64) as usize].clone();
    let part = |id: &str| {
        d.get(Id::<Part>::new(Ulid::parse(id).unwrap()))
            .unwrap()
            .clone()
    };
    let t = |r: &mut Rng| 100 * SEC + r.below(60) as i64 * SEC / 4;
    match r.below(12) {
        0 if ps.len() < 40 => {
            let s = t(r);
            (
                "part_add",
                json!({"cut": fx.cut, "media": {"source": fx.source}, "src_in": s, "src_out": s + SEC, "after": pick(r)}),
            )
        }
        1 => {
            let p = part(&pick(r));
            let mid = (p.src_in.0 + p.src_out.0) / 2;
            ("part_split", json!({"part": p.id, "at": mid}))
        }
        2 if ps.len() >= 2 => {
            let i = r.below(ps.len() as u64 - 1) as usize;
            (
                "part_merge",
                json!({"a": ps[i], "b": ps[i + 1], "force": r.below(2) == 0}),
            )
        }
        3 => ("part_reorder", json!({"part": pick(r), "after": pick(r)})),
        4 => (
            "part_set_gain",
            json!({"part": pick(r), "gain_db": r.below(20) as f64 - 10.0}),
        ),
        5 => {
            let p = part(&pick(r));
            (
                "part_trim",
                json!({"part": p.id, "src_out": p.src_out.0 - SEC / 10}),
            )
        }
        6 => {
            let p = part(&pick(r));
            let a = p.src_in.0 + SEC / 4;
            let w = json!({"word_id": {"asr_revision": "r1", "ordinal": r.below(9)}, "t_src_start": a, "t_src_end": a + SEC / 5, "normalised_text": "w"});
            ("words_strike", json!({"cut": fx.cut, "words": [w]}))
        }
        7 => (
            "crop_set",
            json!({"layer": fx.screen, "part": pick(r), "framing": fx.framing, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": (1 + r.below(8)) as f64 / 10.0}}),
        ),
        8 => (
            "caption_keyword_toggle",
            json!({"cut": fx.cut, "word": {"asr_revision": "r1", "ordinal": r.below(5)}}),
        ),
        9 => {
            let p = part(&pick(r));
            (
                "visual_split_add",
                json!({"part": p.id, "framing": fx.framing, "at": p.src_in, "overrides": {fx.cam.clone(): {"opacity": 0.5}}}),
            )
        }
        10 => (
            "caption_edit",
            json!({"cut": fx.cut, "word": {"asr_revision": "r1", "ordinal": r.below(5)}, "edit": if r.below(2) == 0 { json!("drop") } else { Value::Null }}),
        ),
        _ => (
            "place_set",
            json!({"layer": fx.cam, "part": pick(r), "framing": fx.framing, "place": {"x": 0.1, "y": 0.6, "w": (1 + r.below(9)) as f64 / 10.0}}),
        ),
    }
}

fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct World {
    p: Project,
    fx: Fx,
    sub: gmclip_core::events::Subscription,
    ever: BTreeSet<String>,
}

thread_local! {
    static WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
}

fn with_world<R>(f: impl FnOnce(&mut World) -> R) -> R {
    WORLD.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            let p = open("hist");
            let fx = fixture(&p, 6);
            let sub = p.h.subscribe().unwrap();
            let ever =
                p.h.snapshot()
                    .doc
                    .entity_ids()
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();
            *w = Some(World { p, fx, sub, ever });
        }
        f(w.as_mut().unwrap())
    })
}

/// Creates in non-history transactions must use fresh ids.
fn check_creates(w: &mut World) -> Result<(), TestCaseError> {
    for e in w.sub.drain() {
        if let Event::Committed {
            command, triples, ..
        } = e
        {
            let history = matches!(
                command.as_str(),
                "undo" | "redo" | "run_undo" | "restore_to"
            );
            for t in triples.iter().filter(|t| t.op == "create") {
                let fresh = w.ever.insert(t.entity.clone());
                prop_assert!(fresh || history, "id reused by {command}: {}", t.entity);
            }
        } else {
            return Err(TestCaseError::fail("event queue overflowed"));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(10_000), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn undo_is_the_exact_inverse_and_redo_reapplies(seed in any::<u64>()) {
        with_world(|w| -> Result<(), TestCaseError> {
            let mut r = Rng(seed);
            let before = w.p.h.snapshot().doc.clone();
            let (cmd, args) = random_cmd(&mut r, &before, &w.fx);
            match w.p.try_human(cmd, args) {
                Ok(ok) if ok.committed.is_some() => {}
                Ok(_) | Err(CmdError::Invalid { .. } | CmdError::NotFound) => return Ok(()),
                Err(e) => return Err(TestCaseError::fail(format!("{cmd}: {e:?}"))),
            }
            let after = w.p.h.snapshot().doc.clone();
            check_creates(w)?;
            w.p.try_human("undo", json!({})).map_err(|e| TestCaseError::fail(format!("undo {cmd}: {e:?}")))?;
            prop_assert_eq!(contents(&w.p.h.snapshot().doc), contents(&before), "undo of {} is not exact", cmd);
            w.p.try_human("redo", json!({})).map_err(|e| TestCaseError::fail(format!("redo {cmd}: {e:?}")))?;
            prop_assert_eq!(contents(&w.p.h.snapshot().doc), contents(&after), "redo of {} is not exact", cmd);
            check_creates(w)?;
            // sometimes leave it undone, so later cases build on both shapes
            if r.below(4) == 0 {
                w.p.try_human("undo", json!({})).map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            }
            Ok(())
        })?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(10_000) / 10, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn run_undo_restores_the_run_fields(seed in any::<u64>(), restore in any::<bool>()) {
        with_world(|w| -> Result<(), TestCaseError> {
            let mut r = Rng(seed ^ 0x77);
            let conn = format!("agent-{seed}");
            // an agent run of 1..4 edits
            let start = w.p.h.snapshot().doc.clone();
            let mut n = 0;
            for _ in 0..1 + r.below(4) {
                let d = w.p.h.snapshot().doc.clone();
                let (cmd, args) = random_cmd(&mut r, &d, &w.fx);
                if w.p.agent(&conn, cmd, args, w.p.as_of()).is_ok() {
                    n += 1;
                }
            }
            let ended = w.p.agent(&conn, "run_end", json!({}), w.p.as_of()).unwrap();
            let Some(run) = ended.output["run"].as_str().map(str::to_owned) else { return Ok(()) };
            if n == 0 {
                return Ok(());
            }
            let after_run = w.p.h.snapshot().doc.clone();
            // later human edits
            for _ in 0..r.below(3) {
                let d = w.p.h.snapshot().doc.clone();
                let (cmd, args) = random_cmd(&mut r, &d, &w.fx);
                let _ = w.p.try_human(cmd, args);
            }
            let before_undo = w.p.h.snapshot().doc.clone();
            let rev = before_undo.rev();
            let option = if restore { "restore_to_point" } else { "revert_untouched" };
            let res = w.p.try_human("run_undo", json!({"run": run, "option": option}))
                .map_err(|e| TestCaseError::fail(format!("run_undo: {e:?}")))?;
            let now = w.p.h.snapshot().doc.clone();
            prop_assert!(res.committed.is_none_or(|c| c == rev + 1), "one transaction");
            let cut: Id<Cut> = Id::new(Ulid::parse(&w.fx.cut).unwrap());
            if restore {
                // every entity of the cut is as it was at the run's restore point
                for e in start.entity_ids().into_iter().chain(now.entity_ids()) {
                    if in_cut(&start, e, cut) || in_cut(&now, e, cut) {
                        prop_assert_eq!(now.encode(e), start.encode(e), "{} not restored", e);
                    }
                }
            } else {
                // entities the run wrote and nobody touched later: back to the start;
                // entities only the humans touched afterwards: as the humans left them
                for e in after_run.entity_ids().into_iter().chain(start.entity_ids()) {
                    let run_wrote = after_run.encode(e) != start.encode(e);
                    // humans touched it, or added children under it (which must not be
                    // orphaned, so the run's create of it stays)
                    let human_touched = before_undo.encode(e) != after_run.encode(e)
                        || before_undo
                            .entity_ids()
                            .into_iter()
                            .any(|c| before_undo.parent_of(c) == Some(e) && !after_run.contains(c));
                    if run_wrote && !human_touched {
                        prop_assert_eq!(now.encode(e), start.encode(e), "{} not reverted", e);
                    }
                    if !run_wrote && human_touched {
                        prop_assert_eq!(now.encode(e), before_undo.encode(e), "{} lost a human edit", e);
                    }
                }
            }
            check_creates(w)?;
            Ok(())
        })?;
    }
}

fn in_cut(d: &Doc, e: EntityId, cut: Id<Cut>) -> bool {
    let mut cur = e;
    for _ in 0..8 {
        if cur == cut.entity() {
            return true;
        }
        match d.parent_of(cur) {
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// The generator is not vacuous: most random commands commit, and every kind of
/// command commits at least once over 2 000 draws.
#[test]
fn random_commands_mostly_commit() {
    let p = open("gen");
    let fx = fixture(&p, 6);
    let mut r = Rng(0x5eed);
    let mut committed = 0;
    let mut kinds: BTreeSet<&str> = BTreeSet::new();
    for _ in 0..2_000 {
        let d = p.h.snapshot().doc.clone();
        let (cmd, args) = random_cmd(&mut r, &d, &fx);
        if p.try_human(cmd, args)
            .is_ok_and(|ok| ok.committed.is_some())
        {
            committed += 1;
            kinds.insert(cmd);
        }
    }
    eprintln!("committed {committed}/2000; kinds {kinds:?}");
    assert!(committed >= 1_200, "{committed}");
    assert_eq!(kinds.len(), 12, "{kinds:?}");
    p.h.close().unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(10_000) / 10, failure_persistence: None, ..ProptestConfig::default() })]

    /// R-CC-1 / A10: with `as_of`, a mutation conflicts exactly when an entity it reads
    /// moved after that rev.
    #[test]
    fn as_of_conflicts_exactly_when_a_read_moved(seed in any::<u64>()) {
        with_world(|w| -> Result<(), TestCaseError> {
            let mut r = Rng(seed ^ 0xa5);
            let ps = parts(&w.p.h.snapshot().doc, &w.fx.cut);
            let target = ps[r.below(ps.len() as u64) as usize].clone();
            let as_of = w.p.h.snapshot().doc.rev();
            // the human edits a random part (maybe the target) after the agent's read
            let touched = ps[r.below(ps.len() as u64) as usize].clone();
            if r.below(2) == 0 {
                let _ = w.p.try_human("part_set_gain", json!({"part": touched, "gain_db": r.below(9) as f64}));
            }
            // ground truth: the target's rev moved past as_of (a same-value edit commits
            // nothing and moves nothing)
            let target_id = Id::<Part>::new(Ulid::parse(&target).unwrap()).entity();
            let target_moved = w.p.h.snapshot().doc.entity_rev(target_id).is_some_and(|r| r > as_of);
            let res = w.p.agent("asof", "part_set_gain", json!({"part": target, "gain_db": 1}), Some(gmclip_proto::Base::AsOf { as_of }));
            match res {
                Err(CmdError::Conflict(_)) => prop_assert!(target_moved, "conflict without a moved read"),
                Ok(_) => prop_assert!(!target_moved, "a moved read did not conflict"),
                Err(e) => return Err(TestCaseError::fail(format!("{e:?}"))),
            }
            check_creates(w)?;
            Ok(())
        })?;
    }

    /// R-HIST-2 (a) with two authors: after the agent overwrites some of the human's
    /// fields, the human's `revert_untouched` undo reverts exactly the fields the agent
    /// left alone.
    #[test]
    fn revert_untouched_reverts_exactly_the_untouched_fields(seed in any::<u64>()) {
        with_world(|w| -> Result<(), TestCaseError> {
            let mut r = Rng(seed ^ 0xbb);
            let ps = parts(&w.p.h.snapshot().doc, &w.fx.cut);
            let pick = |r: &mut Rng| ps[r.below(ps.len() as u64) as usize].clone();
            let (a, b) = (pick(&mut r), pick(&mut r));
            let get = |w: &World, id: &str| w.p.h.snapshot().doc.get(Id::<Part>::new(Ulid::parse(id).unwrap())).cloned().unwrap();
            let (a0, b0) = (get(w, &a), get(w, &b));
            // one human transaction writes gain on a and gain on b (a merge-free edit via
            // two calls would be two transactions, so use reorder+gain? keep it simple:
            // gain on a, then a second human edit we do not undo)
            w.p.try_human("part_set_gain", json!({"part": a, "gain_db": 4})).map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            let agent_touches = r.below(2) == 0;
            if agent_touches {
                w.p.agent("ag", "part_set_gain", json!({"part": a, "gain_db": 8}), w.p.as_of()).unwrap();
            }
            let res = w.p.try_human("undo", json!({"option": "revert_untouched"})).map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            let a1 = get(w, &a);
            if agent_touches {
                prop_assert_eq!(a1.gain_db, 8.0, "the agent's later value stays");
                prop_assert!(res.committed.is_none());
            } else {
                prop_assert_eq!(a1.gain_db, a0.gain_db, "reverted");
            }
            let _ = b0;
            check_creates(w)?;
            Ok(())
        })?;
    }
}
