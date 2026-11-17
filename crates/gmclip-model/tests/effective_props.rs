//! R-MODEL-5: `effective()` is invariant under split, reorder, and link/unlink of a
//! layout with identical values. 10⁴ cases each (A04 acceptance); `PROPTEST_CASES`
//! overrides.
//!
//! Scenarios are generated from a seed by a deterministic PRNG (a failure prints the
//! seed, which reproduces it exactly): a cut with sibling parts, a framing with default
//! layers, an optional linked layout, the part's own added layers, sparse overrides with
//! tombstones, visual splits (cut and ease, including ties on `at_src`), and content
//! holds / hides anchored on raw source times, word fingerprints and part edges.

use std::collections::BTreeMap;

use gmclip_model::anchor::{Anchor, PartEdge, RawTime, WordId, WordRef};
use gmclip_model::clock::{FLICKS_PER_MS, ModelTime};
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::fixtures;
use gmclip_model::geometry::{NormRect, Place};
use gmclip_model::{
    Author, CommandKey, Doc, Flicks, Id, OrderKey, Staging, Txn, TxnId, Ulid, effective,
};
use proptest::prelude::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn coin(&mut self, p_percent: u64) -> bool {
        self.below(100) < p_percent
    }
    fn f(&mut self) -> f64 {
        (self.below(1000) as f64) / 1000.0
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }
}

const MS: i64 = FLICKS_PER_MS;
const PART_IN_MS: i64 = 10_000;
const PART_OUT_MS: i64 = 20_000;

struct Scene {
    doc: Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    part: Id<Part>,
    siblings: Vec<Id<Part>>,
    next_id: u128,
}

fn ms(v: i64) -> Flicks {
    Flicks(v * MS)
}

fn roles() -> [LayerRole; 5] {
    [
        LayerRole::Region(RegionRole::Cam),
        LayerRole::Region(RegionRole::Screen),
        LayerRole::Region(RegionRole::Chat),
        LayerRole::Detail,
        LayerRole::Other,
    ]
}

fn rand_layer(r: &mut Rng, id: u128, owner: LayerOwner, order: OrderKey) -> Layer {
    let mut l = fixtures::layer(id, owner, order, r.pick(&roles()));
    l.crop = NormRect {
        x: r.f() * 0.5,
        y: r.f() * 0.5,
        w: 0.1 + r.f() * 0.4,
        h: 0.1 + r.f() * 0.4,
    };
    l.place = Place {
        x: r.f(),
        y: r.f(),
        w: 0.1 + r.f(),
    };
    l.z = r.below(4) as i32;
    l.opacity = r.f();
    l.visible = r.coin(85);
    l.mirror = r.coin(10);
    l
}

fn rand_patch(r: &mut Rng) -> LayerPatch {
    let mut p = LayerPatch::default();
    if r.coin(15) {
        p.removed = Some(r.coin(70));
    }
    if r.coin(50) {
        p.crop = Some(NormRect {
            x: r.f() * 0.5,
            y: r.f() * 0.5,
            w: 0.1 + r.f() * 0.4,
            h: 0.1 + r.f() * 0.4,
        });
    }
    if r.coin(40) {
        p.place = Some(Place {
            x: r.f(),
            y: r.f(),
            w: 0.1 + r.f(),
        });
    }
    if r.coin(30) {
        p.opacity = Some(r.f());
    }
    if r.coin(20) {
        p.z = Some(r.below(4) as i32);
    }
    if r.coin(20) {
        p.visible = Some(r.coin(50));
    }
    if r.coin(10) {
        p.time_map = Some(TimeMap::FreezeFirst);
    }
    p
}

fn rand_patches(r: &mut Rng, targets: &[Id<Layer>]) -> BTreeMap<Id<Layer>, LayerPatch> {
    let mut m = BTreeMap::new();
    if targets.is_empty() {
        return m;
    }
    for _ in 0..r.below(3) {
        m.insert(r.pick(targets), rand_patch(r));
    }
    m
}

/// An anchor somewhere around the part, in one of the analysis-free forms. `src_only`:
/// raw source times and word fingerprints only; `cut_edges`: also anchors on the cut
/// clock (which a reorder legitimately moves, so the reorder test leaves them out).
fn rand_anchor(
    r: &mut Rng,
    part: Id<Part>,
    others: &[Id<Part>],
    cut: Id<Cut>,
    src_only: bool,
    cut_edges: bool,
) -> Anchor {
    let t = PART_IN_MS - 2_000 + r.below(14_000) as i64;
    let kind = if src_only {
        r.below(2)
    } else if cut_edges {
        r.below(7)
    } else {
        r.below(5)
    };
    match kind {
        4 if !others.is_empty() => Anchor::PartStart(PartEdge {
            part: r.pick(others),
            offset_ms: Some(r.below(3_000) as i64),
        }),
        5 => Anchor::CutStart(gmclip_model::anchor::CutEdge {
            cut,
            offset_ms: Some(r.below(40_000) as i64),
        }),
        6 => Anchor::CutEnd(gmclip_model::anchor::CutEdge {
            cut,
            offset_ms: Some(-(r.below(40_000) as i64)),
        }),
        0 => Anchor::Raw(RawTime {
            time: ModelTime::Src {
                source: None,
                group: None,
                t: ms(t),
            },
            reason: "test".into(),
        }),
        1 => Anchor::WordStart(WordRef {
            word_id: WordId {
                asr_revision: "r1".into(),
                ordinal: r.below(1000) as u32,
            },
            t_src_start: ms(t),
            t_src_end: ms(t + 200),
            normalised_text: "word".into(),
            prev2: vec![],
            next2: vec![],
        }),
        2 => Anchor::PartStart(PartEdge {
            part,
            offset_ms: Some(r.below(10_000) as i64),
        }),
        _ => Anchor::PartEnd(PartEdge {
            part,
            offset_ms: Some(-(r.below(10_000) as i64)),
        }),
    }
}

struct Opts {
    /// Allow overrides and splits to target the framing's default layers (not just the
    /// part's additions). Off for the link test, whose layout copies have new ids.
    patch_base: bool,
    link_layout: bool,
    /// Holds and hides may be anchored on the cut clock.
    cut_edges: bool,
}

fn scene(seed: u64, o: &Opts) -> Scene {
    let mut r = Rng(seed);
    let mut d = Doc::new();
    let mut next: u128 = 1;
    let mut id = || {
        next += 1;
        next
    };
    let cut = fixtures::cut(id());
    d.insert_for_test(cut.clone());
    let framing = fixtures::framing(id(), cut.id);
    d.insert_for_test(framing.clone());

    // siblings around the part
    let n_sib = r.below(4) as usize;
    let keys = OrderKey::sequence(None, n_sib + 1).unwrap();
    let at = r.below(n_sib as u64 + 1) as usize;
    let mut siblings = Vec::new();
    let mut part_id = Id::new(Ulid(0));
    for (i, k) in keys.iter().enumerate() {
        let p = if i == at {
            let p = fixtures::part(id(), cut.id, k.clone(), PART_IN_MS * MS, PART_OUT_MS * MS);
            part_id = p.id;
            p
        } else {
            let s = r.below(50_000) as i64;
            let p = fixtures::part(
                id(),
                cut.id,
                k.clone(),
                s * MS,
                (s + 1 + r.below(5_000) as i64) * MS,
            );
            siblings.push(p.id);
            p
        };
        d.insert_for_test(p);
    }

    // framing default layers
    let n_layers = 1 + r.below(4) as usize;
    let lkeys = OrderKey::sequence(None, n_layers).unwrap();
    let mut base: Vec<Id<Layer>> = Vec::new();
    for k in lkeys {
        let l = rand_layer(&mut r, id(), LayerOwner::Framing(framing.id), k);
        base.push(l.id);
        d.insert_for_test(l);
    }

    // a linked layout, sometimes
    let mut look = PartLook::default();
    if o.link_layout && r.coin(40) {
        let lay = fixtures::layout(id(), cut.id);
        d.insert_for_test(lay.clone());
        base.clear();
        let n = 1 + r.below(3) as usize;
        for k in OrderKey::sequence(None, n).unwrap() {
            let l = rand_layer(&mut r, id(), LayerOwner::Layout(lay.id), k);
            base.push(l.id);
            d.insert_for_test(l);
        }
        look.layout_ref = Some(lay.id);
    }

    // the part's own additions
    let mut adds: Vec<Id<Layer>> = Vec::new();
    for k in OrderKey::sequence(None, r.below(3) as usize).unwrap() {
        let l = rand_layer(
            &mut r,
            id(),
            LayerOwner::PartOverride {
                part: part_id,
                framing: framing.id,
            },
            k,
        );
        adds.push(l.id);
        d.insert_for_test(l);
    }
    let targets: Vec<Id<Layer>> = if o.patch_base {
        base.iter().chain(adds.iter()).copied().collect()
    } else {
        adds.clone()
    };
    look.overrides = rand_patches(&mut r, &targets);
    d.insert_for_test({
        let mut p = d.get(part_id).unwrap().clone();
        if look != PartLook::default() {
            p.look.insert(framing.id, look);
        }
        p
    });

    // visual splits, with ties on at_src
    let mut last_at = PART_IN_MS - 1_000;
    for _ in 0..r.below(5) {
        let at_ms = if r.coin(15) {
            last_at
        } else {
            PART_IN_MS - 1_000 + r.below(11_500) as i64
        };
        last_at = at_ms;
        let v = VisualSplit {
            id: Id::new(Ulid(id())),
            part: part_id,
            framing: framing.id,
            at_src: ms(at_ms),
            overrides: rand_patches(&mut r, &targets),
            transition: if r.coin(50) {
                Transition::Cut
            } else {
                Transition::Ease {
                    dur_ms: r.below(3_000) as u32,
                    curve: r.pick(&[
                        Curve::Linear,
                        Curve::EaseIn,
                        Curve::EaseOut,
                        Curve::EaseInOut,
                    ]),
                }
            },
            tag: None,
        };
        d.insert_for_test(v);
    }

    // holds and hides
    let src_only = !o.patch_base;
    for _ in 0..r.below(3) {
        let h = ContentHold {
            id: Id::new(Ulid(id())),
            cut: cut.id,
            from: rand_anchor(&mut r, part_id, &siblings, cut.id, src_only, o.cut_edges),
            to: rand_anchor(&mut r, part_id, &siblings, cut.id, src_only, o.cut_edges),
            at: HoldAt::AtSrc(ms(PART_IN_MS + r.below(10_000) as i64)),
            roles: if r.coin(50) {
                vec![]
            } else {
                vec![r.pick(&roles())]
            },
            per_framing: BTreeMap::new(),
        };
        d.insert_for_test(h);
    }
    for _ in 0..r.below(3) {
        let h = HideRoles {
            id: Id::new(Ulid(id())),
            cut: cut.id,
            from: rand_anchor(&mut r, part_id, &siblings, cut.id, src_only, o.cut_edges),
            to: rand_anchor(&mut r, part_id, &siblings, cut.id, src_only, o.cut_edges),
            roles: vec![r.pick(&roles())],
            per_framing: BTreeMap::new(),
        };
        d.insert_for_test(h);
    }
    Scene {
        doc: d,
        cut: cut.id,
        framing: framing.id,
        part: part_id,
        siblings,
        next_id: 1_000_000,
    }
}

/// Times worth checking: every split and hold edge ±1 flick, the split point ±1, and
/// a spread of others, all inside `[lo, hi)`.
fn probe_times(doc: &Doc, sc: &Scene, lo: Flicks, hi: Flicks, extra: &[Flicks]) -> Vec<Flicks> {
    let mut ts: Vec<i64> = Vec::new();
    for v in doc.iter::<VisualSplit>() {
        ts.extend([v.at_src.0 - 1, v.at_src.0, v.at_src.0 + 1]);
        if let Transition::Ease { dur_ms, .. } = v.transition {
            let e = v.at_src.0 + i64::from(dur_ms) * MS;
            ts.extend([e - 1, e, e + 1, (v.at_src.0 + e) / 2]);
        }
    }
    for h in doc.iter::<ContentHold>() {
        for a in [&h.from, &h.to] {
            if let Some(t) = gmclip_model::clock::anchor_src(doc, a) {
                ts.extend([t.0 - 1, t.0, t.0 + 1]);
            }
        }
    }
    for t in extra {
        ts.extend([t.0 - 1, t.0, t.0 + 1]);
    }
    let span = hi.0 - lo.0;
    for i in 0..16 {
        ts.push(lo.0 + span * i / 16);
    }
    let _ = sc;
    ts.retain(|t| *t >= lo.0 && *t < hi.0);
    ts.sort_unstable();
    ts.dedup();
    ts.into_iter().map(Flicks).collect()
}

fn commit(doc: &Doc, ops: Vec<gmclip_model::Op>) -> Doc {
    let mut next = doc.clone();
    next.apply(&Txn {
        rev: doc.rev() + 1,
        id: TxnId(Ulid(1)),
        author: Author::Human,
        run: None,
        command: CommandKey {
            name: "test".into(),
            version: 1,
        },
        request_id: None,
        reverts: None,
        reads: vec![],
        ops,
        ts: 0,
    })
    .expect("staged ops apply to their base");
    next
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

/// A split point strictly inside the part: on a whole ms, or (a third of the time) off
/// the ms grid, so edge anchors fall back to raw source times.
fn split_point(r: &mut Rng) -> Flicks {
    let at = ms(PART_IN_MS + 1 + r.below((PART_OUT_MS - PART_IN_MS - 2) as u64) as i64);
    if r.coin(33) {
        Flicks(at.0 + 1 + r.below((MS - 1) as u64) as i64)
    } else {
        at
    }
}

fn check_split(seed: u64) -> Result<(), TestCaseError> {
    let sc = scene(
        seed,
        &Opts {
            patch_base: true,
            link_layout: true,
            cut_edges: true,
        },
    );
    let mut r = Rng(seed ^ 0x5151);
    let at = split_point(&mut r);
    let mut n = sc.next_id;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(sc.doc.clone(), Author::Human, &mut mint);
    let b =
        edit::split_part(&mut s, sc.part, at).map_err(|e| TestCaseError::fail(format!("{e}")))?;
    let after = commit(&sc.doc, s.ops());
    let pa = after.get(sc.part).unwrap();
    let pb = after.get(b).unwrap();
    prop_assert_eq!(pa.src_out, at);
    prop_assert_eq!(pb.src_in, at);
    prop_assert_eq!(pb.lineage.split_of, Some(sc.part));
    for t in probe_times(&sc.doc, &sc, ms(PART_IN_MS), ms(PART_OUT_MS), &[at]) {
        let orig = effective(&sc.doc, sc.part, sc.framing, t).unwrap();
        if t < at {
            let now = effective(&after, sc.part, sc.framing, t).unwrap();
            prop_assert_eq!(
                &now.layers,
                &orig.layers,
                "a-side differs at {} (seed {:#x})",
                t.0,
                seed
            );
        } else {
            let now = effective(&after, b, sc.framing, t).unwrap();
            prop_assert_eq!(
                now.values(),
                orig.values(),
                "b-side differs at {} (seed {:#x})",
                t.0,
                seed
            );
        }
    }
    Ok(())
}

fn check_reorder(seed: u64) -> Result<(), TestCaseError> {
    let sc = scene(
        seed,
        &Opts {
            patch_base: true,
            link_layout: true,
            cut_edges: false,
        },
    );
    let mut r = Rng(seed ^ 0xa11ce);
    let after_sib = if sc.siblings.is_empty() || r.coin(25) {
        None
    } else {
        Some(r.pick(&sc.siblings))
    };
    let mut n = sc.next_id;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(sc.doc.clone(), Author::Human, &mut mint);
    edit::reorder_part(&mut s, sc.part, after_sib)
        .map_err(|e| TestCaseError::fail(format!("{e}")))?;
    let ops = s.ops();
    prop_assert!(ops.len() <= 1, "a reorder writes one entity");
    let after = commit(&sc.doc, ops);
    // the part now sits right after `after_sib`
    let order: Vec<_> = after.children::<Part>(sc.cut.entity()).collect();
    let pos = order.iter().position(|p| *p == sc.part).unwrap();
    match after_sib {
        None => prop_assert_eq!(pos, 0),
        Some(a) => prop_assert_eq!(order[pos - 1], a),
    }
    for t in probe_times(&sc.doc, &sc, ms(PART_IN_MS), ms(PART_OUT_MS), &[]) {
        let orig = effective(&sc.doc, sc.part, sc.framing, t).unwrap();
        let now = effective(&after, sc.part, sc.framing, t).unwrap();
        prop_assert_eq!(
            &now.layers,
            &orig.layers,
            "differs at {} (seed {:#x})",
            t.0,
            seed
        );
    }
    Ok(())
}

fn check_link_unlink(seed: u64) -> Result<(), TestCaseError> {
    let sc = scene(
        seed,
        &Opts {
            patch_base: false,
            link_layout: false,
            cut_edges: false,
        },
    );
    // a layout whose layers copy the framing's defaults (new ids, same values and order)
    let mut doc = sc.doc.clone();
    let lay = fixtures::layout(900_000, sc.cut);
    doc.insert_for_test(lay.clone());
    let defaults: Vec<Layer> = doc
        .children::<Layer>(sc.framing.entity())
        .map(|id| doc.get(id).unwrap().clone())
        .collect();
    for (i, l) in defaults.iter().enumerate() {
        let mut c = l.clone();
        c.id = Id::new(Ulid(900_001 + i as u128));
        c.owner = LayerOwner::Layout(lay.id);
        doc.insert_for_test(c);
    }
    let mut n = sc.next_id;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(doc.clone(), Author::Human, &mut mint);
    edit::link_layout(&mut s, sc.part, sc.framing, Some(lay.id)).unwrap();
    let linked = commit(&doc, s.ops());
    let mut s = Staging::new(linked.clone(), Author::Human, &mut mint);
    edit::link_layout(&mut s, sc.part, sc.framing, None).unwrap();
    let unlinked = commit(&linked, s.ops());
    for t in probe_times(&doc, &sc, ms(PART_IN_MS), ms(PART_OUT_MS), &[]) {
        let orig = effective(&doc, sc.part, sc.framing, t).unwrap();
        let l = effective(&linked, sc.part, sc.framing, t).unwrap();
        let u = effective(&unlinked, sc.part, sc.framing, t).unwrap();
        prop_assert_eq!(
            l.values(),
            orig.values(),
            "link differs at {} (seed {:#x})",
            t.0,
            seed
        );
        prop_assert_eq!(
            &u.layers,
            &orig.layers,
            "unlink differs at {} (seed {:#x})",
            t.0,
            seed
        );
    }
    Ok(())
}

/// Split then merge back: `effective()` is unchanged everywhere in the part, for the
/// part and for its siblings (holds on other media must not leak), and the merge drops
/// the split's copies (no duplicate layers or splits).
fn check_split_merge(seed: u64) -> Result<(), TestCaseError> {
    let sc = scene(
        seed,
        &Opts {
            patch_base: true,
            link_layout: true,
            cut_edges: true,
        },
    );
    let mut r = Rng(seed ^ 0x3e3e);
    let at = split_point(&mut r);
    let mut n = sc.next_id;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(sc.doc.clone(), Author::Human, &mut mint);
    let b =
        edit::split_part(&mut s, sc.part, at).map_err(|e| TestCaseError::fail(format!("{e}")))?;
    let split = commit(&sc.doc, s.ops());
    let mut s = Staging::new(split.clone(), Author::Human, &mut mint);
    edit::merge_parts(&mut s, sc.part, b, false)
        .map_err(|e| TestCaseError::fail(format!("merge: {e}")))?;
    let merged = commit(&split, s.ops());
    prop_assert!(merged.get(b).is_none());
    prop_assert_eq!(
        merged.get(sc.part).unwrap().src_out,
        sc.doc.get(sc.part).unwrap().src_out
    );
    prop_assert_eq!(
        merged.children::<Layer>(sc.part.entity()).count(),
        sc.doc.children::<Layer>(sc.part.entity()).count(),
        "added layers duplicated (seed {:#x})",
        seed
    );
    prop_assert_eq!(
        merged.children::<VisualSplit>(sc.part.entity()).count(),
        sc.doc.children::<VisualSplit>(sc.part.entity()).count(),
        "visual splits duplicated (seed {:#x})",
        seed
    );
    for t in probe_times(&sc.doc, &sc, ms(PART_IN_MS), ms(PART_OUT_MS), &[at]) {
        let orig = effective(&sc.doc, sc.part, sc.framing, t).unwrap();
        let now = effective(&merged, sc.part, sc.framing, t).unwrap();
        prop_assert_eq!(
            &now.layers,
            &orig.layers,
            "split+merge differs at {} (seed {:#x})",
            t.0,
            seed
        );
    }
    // siblings (some on other media) see the same holds and hides throughout
    for sib in &sc.siblings {
        let p = sc.doc.get(*sib).unwrap();
        for t in probe_times(&sc.doc, &sc, p.src_in, p.src_out, &[]) {
            for d in [&split, &merged] {
                let orig = effective(&sc.doc, *sib, sc.framing, t).unwrap();
                let now = effective(d, *sib, sc.framing, t).unwrap();
                prop_assert_eq!(
                    &now.layers,
                    &orig.layers,
                    "sibling differs at {} (seed {:#x})",
                    t.0,
                    seed
                );
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn effective_is_invariant_under_split(seed in any::<u64>()) {
        check_split(seed)?;
    }

    #[test]
    fn effective_is_unchanged_by_split_then_merge(seed in any::<u64>()) {
        check_split_merge(seed)?;
    }

    #[test]
    fn effective_is_invariant_under_reorder(seed in any::<u64>()) {
        check_reorder(seed)?;
    }

    #[test]
    fn effective_is_invariant_under_link_and_unlink(seed in any::<u64>()) {
        check_link_unlink(seed)?;
    }
}

/// Known shapes, checked by hand: an ease straddling the split point, a hold on the
/// part's end, a tombstone revived by a split.
#[test]
fn hand_cases() {
    for seed in [0u64, 1, 2, 3, 0xdead_beef, 0x1234_5678_9abc_def0] {
        check_split(seed).unwrap();
        check_split_merge(seed).unwrap();
        check_reorder(seed).unwrap();
        check_link_unlink(seed).unwrap();
    }
}

#[test]
fn an_ease_starting_during_another_continues_from_its_current_value() {
    let mut d = Doc::new();
    let cut = fixtures::cut(1);
    let fr = fixtures::framing(2, cut.id);
    let p = fixtures::part(
        3,
        cut.id,
        OrderKey::first(),
        PART_IN_MS * MS,
        PART_OUT_MS * MS,
    );
    let l = fixtures::layer(
        4,
        LayerOwner::Framing(fr.id),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    d.insert_for_test(cut.clone());
    d.insert_for_test(fr.clone());
    d.insert_for_test(p.clone());
    d.insert_for_test(l.clone());
    let op = |x: f64| {
        BTreeMap::from([(
            l.id,
            LayerPatch {
                opacity: Some(x),
                ..LayerPatch::default()
            },
        )])
    };
    let mut add = |id: u128, at: i64, x: f64, tr: Transition| {
        d.insert_for_test(VisualSplit {
            id: Id::new(Ulid(id)),
            part: p.id,
            framing: fr.id,
            at_src: ms(at),
            overrides: op(x),
            transition: tr,
            tag: None,
        });
    };
    let ease = Transition::Ease {
        dur_ms: 2_000,
        curve: Curve::Linear,
    };
    add(5, 11_000, 0.2, Transition::Cut);
    add(6, 12_000, 1.0, ease);
    add(7, 12_500, 0.5, ease);
    let at = |t: i64, d: &Doc, part: Id<Part>| {
        effective(d, part, fr.id, ms(t)).unwrap().layers[0]
            .values
            .opacity
    };
    // at 12.5 s the first ease is at 0.4; the second starts there, not from 1.0
    assert!((at(12_499, &d, p.id) - 0.3998).abs() < 1e-3);
    assert!((at(12_500, &d, p.id) - 0.4).abs() < 1e-9);
    assert!((at(13_500, &d, p.id) - 0.45).abs() < 1e-9);
    // a split during the second ease copies the whole chain and keeps every value
    let mut n = 100u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
    let b = edit::split_part(&mut s, p.id, ms(13_000)).unwrap();
    let after = commit(&d, s.ops());
    assert_eq!(after.children::<VisualSplit>(b.entity()).count(), 3);
    for t in [13_000, 13_500, 14_499, 14_500, 16_000] {
        assert!((at(t, &after, b) - at(t, &d, p.id)).abs() < 1e-12, "{t}");
    }
}

#[test]
fn split_carries_a_running_ease() {
    let mut d = Doc::new();
    let cut = fixtures::cut(1);
    let fr = fixtures::framing(2, cut.id);
    let p = fixtures::part(
        3,
        cut.id,
        OrderKey::first(),
        PART_IN_MS * MS,
        PART_OUT_MS * MS,
    );
    let l = fixtures::layer(
        4,
        LayerOwner::Framing(fr.id),
        OrderKey::first(),
        LayerRole::Region(RegionRole::Screen),
    );
    d.insert_for_test(cut.clone());
    d.insert_for_test(fr.clone());
    d.insert_for_test(p.clone());
    d.insert_for_test(l.clone());
    let patch = |x: f64| {
        BTreeMap::from([(
            l.id,
            LayerPatch {
                opacity: Some(x),
                ..LayerPatch::default()
            },
        )])
    };
    d.insert_for_test(VisualSplit {
        id: Id::new(Ulid(5)),
        part: p.id,
        framing: fr.id,
        at_src: ms(11_000),
        overrides: patch(0.2),
        transition: Transition::Cut,
        tag: None,
    });
    d.insert_for_test(VisualSplit {
        id: Id::new(Ulid(6)),
        part: p.id,
        framing: fr.id,
        at_src: ms(12_000),
        overrides: patch(1.0),
        transition: Transition::Ease {
            dur_ms: 2_000,
            curve: Curve::Linear,
        },
        tag: None,
    });
    let mid = effective(&d, p.id, fr.id, ms(13_000)).unwrap();
    assert!((mid.layers[0].values.opacity - 0.6).abs() < 1e-12);
    let mut n = 100u128;
    let mut mint = || {
        n += 1;
        Ulid(n)
    };
    let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
    let b = edit::split_part(&mut s, p.id, ms(12_500)).unwrap();
    let after = commit(&d, s.ops());
    let e = effective(&after, b, fr.id, ms(13_000)).unwrap();
    assert!((e.layers[0].values.opacity - 0.6).abs() < 1e-12);
    assert_eq!(after.children::<VisualSplit>(b.entity()).count(), 2);
}

/// Regression (lanes A, B2): this seed duplicated visual splits on merge.
#[test]
fn split_then_merge_regression_seeds() {
    let seed = 11_757_674_002_690_888_522u64;
    check_split(seed).unwrap();
    check_split_merge(seed).unwrap();
}
