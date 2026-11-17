//! Ops, transactions and the document (A03, A05 model half, R-HIST-4 model half):
//! - `apply` and replay are deterministic: replaying the committed op rows (decoded
//!   from canonical CBOR) on an empty document gives an identical document;
//! - the inverse of every transaction takes the document back to the entity state
//!   before it (inverse correctness, 10⁴ cases);
//! - entity ids stay unique under random create/split/merge/delete/reorder;
//! - `Doc::clone` is O(1): flat from 1k to 100k entities.

use std::time::Instant;

use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::fixtures;
use gmclip_model::{
    Author, CommandKey, Doc, EntityId, Flicks, Id, Op, OrderKey, Staging, Txn, TxnId, Ulid,
};
use proptest::prelude::*;

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

fn txn(rev: u64, ops: Vec<Op>) -> Txn {
    Txn {
        rev,
        id: TxnId(Ulid(u128::from(rev))),
        author: Author::Human,
        run: None,
        command: CommandKey {
            name: "random".into(),
            version: 1,
        },
        request_id: None,
        reverts: None,
        reads: vec![],
        ops,
        ts: 0,
    }
}

/// Contents of the given entities (absent = `None`): what an undo must restore. Entities
/// a transaction did not touch are shared structurally and never change.
fn some_contents(d: &Doc, ids: &[EntityId]) -> Vec<Option<Vec<u8>>> {
    ids.iter().map(|e| d.encode(*e).map(|v| v.0)).collect()
}

/// Entity contents only (no revs, no authors): what an undo must restore.
fn contents(d: &Doc) -> Vec<(EntityId, Vec<u8>)> {
    d.entity_ids()
        .into_iter()
        .map(|e| (e, d.encode(e).unwrap().0))
        .collect()
}

/// One random edit on `s`. Returns false when it chose an impossible edit (skipped).
fn random_edit(r: &mut Rng, s: &mut Staging<'_>, cut: Id<Cut>, framing: Id<Framing>) -> bool {
    let parts: Vec<Id<Part>> = s.doc().children::<Part>(cut.entity()).collect();
    let pick = |r: &mut Rng, v: &[Id<Part>]| v[r.below(v.len() as u64) as usize];
    match r.below(9) {
        0 | 1 => {
            // add a part after a random one
            let after = (!parts.is_empty() && r.below(4) > 0).then(|| pick(r, &parts));
            let Ok(key) = edit::key_after(s, cut, after, None) else {
                return false;
            };
            let id = s.mint();
            let t = r.below(100_000) as i64;
            let mut p = fixtures::part(0, cut, key, t, t + 1 + r.below(10_000) as i64);
            p.id = id;
            s.create(p).is_ok()
        }
        2 if !parts.is_empty() => {
            let p = pick(r, &parts);
            let g = r.below(20) as f64 - 10.0;
            s.update(p, |p| p.gain_db = g).is_ok()
        }
        3 if !parts.is_empty() => {
            // a per-framing map key: set or clear
            let p = pick(r, &parts);
            let set = r.below(2) == 0;
            s.update(p, |p| {
                if set {
                    p.caption_place_override.insert(
                        framing,
                        gmclip_model::geometry::Place {
                            x: 0.5,
                            y: 0.5,
                            w: 0.9,
                        },
                    );
                } else {
                    p.caption_place_override.remove(&framing);
                }
            })
            .is_ok()
        }
        4 if !parts.is_empty() => {
            let p = pick(r, &parts);
            let (a, b) = {
                let x = s.get(p).unwrap();
                (x.src_in.0, x.src_out.0)
            };
            if b - a < 2 {
                return false;
            }
            let at = Flicks(a + 1 + r.below((b - a - 1) as u64) as i64);
            edit::split_part(s, p, at).is_ok()
        }
        5 if parts.len() >= 2 => {
            let i = r.below(parts.len() as u64 - 1) as usize;
            edit::merge_parts(s, parts[i], parts[i + 1], r.below(2) == 0).is_ok()
        }
        6 if !parts.is_empty() => {
            let p = pick(r, &parts);
            let after = (r.below(3) > 0)
                .then(|| pick(r, &parts))
                .filter(|a| *a != p);
            edit::reorder_part(s, p, after).is_ok()
        }
        7 if !parts.is_empty() => edit::delete_part(s, pick(r, &parts)).is_ok(),
        8 if !parts.is_empty() => {
            // a visual split on a part
            let p = pick(r, &parts);
            let at = s.get(p).unwrap().src_in;
            let id = s.mint();
            s.create(VisualSplit {
                id,
                part: p,
                framing,
                at_src: at,
                overrides: Default::default(),
                transition: Transition::Cut,
                tag: None,
            })
            .is_ok()
        }
        _ => false,
    }
}

fn base() -> (Doc, Id<Cut>, Id<Framing>) {
    let mut d = Doc::new();
    let c = fixtures::cut(1);
    let f = fixtures::framing(2, c.id);
    d.insert_for_test(c.clone());
    d.insert_for_test(f.clone());
    (d, c.id, f.id)
}

fn run_session(seed: u64, steps: usize) -> Result<(), TestCaseError> {
    let mut r = Rng(seed);
    let (start, cut, framing) = base();
    let mut doc = start.clone();
    let mut log: Vec<Vec<u8>> = Vec::new();
    let mut ever: std::collections::BTreeSet<EntityId> = doc.entity_ids().into_iter().collect();
    let mut n: u128 = 1_000;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    for _ in 0..steps {
        let before = doc.clone();
        let mut s = Staging::new(doc.clone(), Author::Human, &mut mint);
        for _ in 0..1 + r.below(3) {
            random_edit(&mut r, &mut s, cut, framing);
        }
        let ops = s.ops();
        let staged: Vec<EntityId> = s.touched().to_vec();
        let work = s.into_work();
        let t = txn(doc.rev() + 1, ops);
        doc.apply(&t)
            .map_err(|e| TestCaseError::fail(format!("apply: {e}")))?;
        // the applied doc has the staged contents (for every entity the staging touched)
        let touched: Vec<EntityId> = {
            let mut v: Vec<EntityId> = t.ops.iter().map(Op::entity).collect();
            v.sort();
            v.dedup();
            v
        };
        prop_assert_eq!(doc.len(), work.len());
        prop_assert_eq!(some_contents(&doc, &staged), some_contents(&work, &staged));
        prop_assert_eq!(
            some_contents(&doc, &touched),
            some_contents(&work, &touched)
        );
        // inverse correctness
        let mut undone = doc.clone();
        undone
            .apply(&txn(doc.rev() + 1, t.inverse_ops()))
            .map_err(|e| TestCaseError::fail(format!("inverse: {e}")))?;
        prop_assert_eq!(undone.len(), before.len());
        prop_assert_eq!(
            some_contents(&undone, &touched),
            some_contents(&before, &touched)
        );
        // the op row is canonical and round-trips
        let row = t.to_cbor();
        prop_assert!(gmclip_model::cbor::is_canonical(&row));
        prop_assert_eq!(&Txn::from_cbor(&row).unwrap(), &t);
        log.push(row);
        // ids unique: a created id was never used before, even by a deleted entity
        for op in &t.ops {
            if let Op::Create { e, .. } = op {
                prop_assert!(ever.insert(*e), "id reused: {:?}", e);
            }
        }
        // and the cut's index lists each part once, in order-key order
        let ids: Vec<_> = doc.children::<Part>(cut.entity()).collect();
        let keys: Vec<_> = ids
            .iter()
            .map(|p| doc.get(*p).unwrap().order.clone())
            .collect();
        prop_assert!(keys.windows(2).all(|w| w[0] <= w[1]));
        prop_assert_eq!(ids.len(), doc.count::<Part>());
    }
    // replay of the decoded op rows reaches an identical document
    let mut replay = start;
    for row in &log {
        replay.apply(&Txn::from_cbor(row).unwrap()).unwrap();
    }
    prop_assert_eq!(&replay, &doc);
    Ok(())
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn transactions_invert_and_replay(seed in any::<u64>()) {
        run_session(seed, 12)?;
    }
}

#[test]
fn a_long_session_replays() {
    for seed in 0..4 {
        run_session(seed, 400).unwrap();
    }
}

#[test]
fn a_stale_op_is_refused_and_leaves_the_doc_unchanged() {
    let (mut d, cut, _) = base();
    let p = fixtures::part(5, cut, OrderKey::first(), 0, 10);
    d.insert_for_test(p.clone());
    let mut n = 10u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
    s.update(p.id, |p| p.gain_db = 3.0).unwrap();
    let ops = s.ops();
    let mut moved = d.clone();
    moved.apply(&txn(1, ops.clone())).unwrap();
    let snapshot = moved.clone();
    // the same ops again: their before-values no longer match
    assert!(moved.apply(&txn(2, ops)).is_err());
    assert_eq!(moved, snapshot);
    // and a rev out of order is refused
    assert!(moved.apply(&txn(5, vec![])).is_err());
}

/// A03: `Doc::clone` is O(1) — flat from 1k to 100k entities.
#[test]
fn clone_is_flat_from_1k_to_100k() {
    let mut timings = Vec::new();
    for n in [1_000usize, 10_000, 100_000] {
        let (mut d, cut, _) = base();
        let keys = OrderKey::sequence(None, n).unwrap();
        for (i, k) in keys.into_iter().enumerate() {
            d.insert_for_test(fixtures::part(10 + i as u128, cut, k, 0, 10));
        }
        assert_eq!(d.count::<Part>(), n);
        // the best of three rounds: the suite runs this beside the property tests, and one
        // round can lose the CPU for long enough to look like a deep clone
        let reps = 10_000u32;
        let mut per = std::time::Duration::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            let mut keep = Vec::with_capacity(reps as usize);
            for _ in 0..reps {
                keep.push(d.clone());
            }
            per = per.min(t0.elapsed() / reps);
            drop(keep);
        }
        timings.push((n, per));
    }
    eprintln!("Doc::clone per call: {timings:?}");
    let small = timings[0].1.as_nanos().max(1);
    let large = timings[2].1.as_nanos();
    // generous: a deep clone of 100k entities would be ~100× the 1k one
    assert!(large < small * 5 + 2_000, "clone not flat: {timings:?}");
}

#[test]
fn ops_name_fields_by_name_on_disk() {
    let (d, cut, _) = base();
    let p = fixtures::part(5, cut, OrderKey::first(), 0, 10);
    let mut d = d;
    d.insert_for_test(p.clone());
    let mut n = 10u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(d, Author::Human, &mut mint);
    s.update(p.id, |p| p.gain_db = 3.0).unwrap();
    let row = txn(1, s.ops()).to_cbor();
    let v: ciborium::Value = gmclip_model::cbor::from_slice(&row).unwrap();
    let text = format!("{v:?}");
    assert!(text.contains("gain_db"), "{text}");
    let _ = Id::<Part>::new(Ulid(0));
}

/// Review finding: ops must carry canonical bytes, or their inverse cannot apply.
#[test]
fn non_canonical_op_values_are_refused() {
    use gmclip_model::entity::FieldPath;
    use gmclip_model::{Entity, FieldValue};
    let (d, cut, _) = base();
    let p = fixtures::part(5, cut, OrderKey::first(), 0, 10);
    // a Create whose entity carries an unknown extra key
    let mut v: ciborium::Value =
        gmclip_model::cbor::from_slice(&gmclip_model::cbor::to_vec(&p)).unwrap();
    if let ciborium::Value::Map(m) = &mut v {
        m.push((
            ciborium::Value::Text("zz_extra".into()),
            ciborium::Value::Null,
        ));
    }
    let bytes = gmclip_model::cbor::to_vec(&v);
    let mut x = d.clone();
    assert!(
        x.apply(&txn(
            1,
            vec![Op::Create {
                e: p.entity_id(),
                value: FieldValue(bytes)
            }]
        ))
        .is_err()
    );
    // a Set whose `after` is 3.0 written as a double instead of a half
    let mut d2 = d.clone();
    d2.insert_for_test(p.clone());
    let f = FieldPath::named::<Part>("gain_db", None).unwrap();
    let before = d2.field(p.entity_id(), &f).unwrap();
    let after = FieldValue(vec![0xfb, 0x40, 0x08, 0, 0, 0, 0, 0, 0]);
    assert!(
        d2.apply(&txn(
            1,
            vec![Op::Set {
                e: p.entity_id(),
                f,
                before,
                after
            }]
        ))
        .is_err()
    );
}

/// Review finding: −0.0 and 0.0 compare equal, so they must encode the same.
#[test]
fn negative_zero_is_not_a_lost_change() {
    let (d, cut, _) = base();
    let mut p = fixtures::part(5, cut, OrderKey::first(), 0, 10);
    p.gain_db = 0.0;
    let mut d = d;
    d.insert_for_test(p.clone());
    let mut n = 10u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
    s.update(p.id, |p| p.gain_db = -0.0).unwrap();
    let work = s.doc().clone();
    let ops = s.ops();
    assert!(ops.is_empty());
    assert_eq!(contents(&work), contents(&d));
}

/// Hot-bench finding: split → merge cycles must not grow the merged lineage
/// geometrically (it reached the 64 MB decode limit after ~50 cycles).
#[test]
fn split_merge_cycles_keep_lineage_linear() {
    let (mut d, cut, _) = base();
    let p = fixtures::part(5, cut, OrderKey::first(), 0, 1_000);
    d.insert_for_test(p.clone());
    let mut n = 10u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    for i in 0..200 {
        let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
        let b = edit::split_part(&mut s, p.id, Flicks(500)).unwrap();
        edit::merge_parts(&mut s, p.id, b, false).unwrap();
        let ops = s.ops();
        d.apply(&txn(d.rev() + 1, ops)).unwrap();
        assert_eq!(d.get(p.id).unwrap().lineage.merged.len(), i + 1);
    }
}
