//! Structural edits on a [`Staging`] transaction, shared by gmclip-core's commands and
//! the model's property tests (R-MODEL-2, R-MODEL-5). Pure: no I/O, ids from the
//! staging's minter.
//!
//! **Split keeps `effective()` invariant** (R-MODEL-5). The b-side copies every part
//! field and the part's own added layers (fresh ids, override keys remapped), takes the
//! visual splits after the split point, and gets copies of the split in force at that
//! point plus, when it is an ease, the chain of splits its start state depends on —
//! because only the last split ≤ t is in force, folding them into the b-side's look
//! would leak into its later splits. Anchors on the part's edges are re-resolved to the
//! side containing their time.
//!
//! **Merge undoes a split exactly** and otherwise keeps b's look: b's added layers and
//! pre-roll splits that are split copies of a's are dropped (keys remapped back), b's
//! own splits move to a, and when b's look differs from a's (only possible with
//! `force`, or with differing overrides) a patch converting a's base into b's is laid
//! under every split from the merge point on.

use std::collections::BTreeMap;

use crate::anchor::{Anchor, PartEdge, RawTime};
use crate::clock::{ModelTime, flicks_to_exact_ms, ms_to_flicks};
use crate::effective::splits_of;
use crate::entities::*;
use crate::entity::ModelError;
use crate::ids::{EntityId, Id};
use crate::order::OrderKey;
use crate::staging::Staging;
use crate::time::Flicks;

/// Something an edit could not keep exactly, reported to the caller.
#[derive(Clone, PartialEq, Debug)]
pub enum EditNote {
    /// A clip's `after:` part was deleted; it now follows another (R-MODEL-2).
    ClipReparented { clip: Id<Clip>, to: ClipSlot },
    /// Anchors still name a deleted part.
    DanglingAnchor { entity: EntityId },
    /// A visual split outside a's range would have come into force after a merge.
    DroppedSplit { split: Id<VisualSplit> },
}

/// The parts of `cut` in order.
pub fn siblings(s: &Staging<'_>, cut: Id<Cut>) -> Vec<Id<Part>> {
    s.doc().children::<Part>(cut.entity()).collect()
}

/// A key just after `after` (or first when `None`) among `cut`'s parts, skipping
/// `moving` (the part being placed, when reordering).
pub fn key_after(
    s: &Staging<'_>,
    cut: Id<Cut>,
    after: Option<Id<Part>>,
    moving: Option<Id<Part>>,
) -> Result<OrderKey, ModelError> {
    let doc = s.doc();
    let sibs: Vec<(OrderKey, Id<Part>)> = doc
        .children::<Part>(cut.entity())
        .filter(|id| Some(*id) != moving)
        .filter_map(|id| doc.get(id).map(|p| (p.order.clone(), id)))
        .collect();
    let at = match after {
        None => 0,
        Some(a) => {
            sibs.iter()
                .position(|(_, id)| *id == a)
                .ok_or(ModelError::NotFound)?
                + 1
        }
    };
    let lo = at.checked_sub(1).map(|i| &sibs[i].0);
    let hi = sibs.get(at).map(|x| &x.0);
    OrderKey::between(lo, hi).map_err(|_| ModelError::Invalid("no order key fits"))
}

/// Applies `f` to every anchor stored by `cut`'s content (holds, hides, gain segments,
/// audio overlays, story suggestions, overlays incl. counters and keyframes, cues,
/// no-caption ranges) and by measurements, writing back the ones that change.
pub fn map_cut_anchors(
    s: &mut Staging<'_>,
    cut: Id<Cut>,
    f: &dyn Fn(&Anchor) -> Option<Anchor>,
) -> Result<(), ModelError> {
    let m = |a: &mut Anchor| -> bool {
        match f(a) {
            Some(n) if n != *a => {
                *a = n;
                true
            }
            _ => false,
        }
    };
    let cs = |c: &mut CounterStart| -> bool {
        match c {
            CounterStart::Anchor(a) => m(a),
            CounterStart::Measurement(_) => false,
        }
    };
    let me = |e: &mut MeasureEnd| -> bool {
        match e {
            MeasureEnd::Anchor(a) => m(a),
            _ => false,
        }
    };
    let c = cut.entity();
    macro_rules! each {
        ($ty:ty, $ids:expr, |$e:ident| $body:expr) => {{
            let ids: Vec<Id<$ty>> = $ids;
            for id in ids {
                let mut $e = s.get(id).ok_or(ModelError::NotFound)?.clone();
                if $body {
                    s.replace($e)?;
                }
            }
        }};
    }
    each!(ContentHold, s.doc().children(c).collect(), |e| m(
        &mut e.from
    ) | m(
        &mut e.to
    ));
    each!(HideRoles, s.doc().children(c).collect(), |e| m(&mut e.from)
        | m(&mut e.to));
    each!(GainSegment, s.doc().children(c).collect(), |e| m(
        &mut e.start
    ) | m(
        &mut e.end
    ));
    each!(AudioOverlay, s.doc().children(c).collect(), |e| m(
        &mut e.start
    ) | m(
        &mut e.end
    ));
    each!(StorySuggestion, s.doc().children(c).collect(), |e| m(
        &mut e.span.start
    ) | m(
        &mut e.span.end
    ));
    let overlays: Vec<Id<Overlay>> = s.doc().children(c).collect();
    each!(Overlay, overlays.clone(), |e| {
        let mut changed = m(&mut e.start);
        if let OverlayEnd::At(x) = &mut e.end {
            changed |= m(x);
        }
        if let Some(cn) = &mut e.counter {
            changed |= cs(&mut cn.start);
            if let Some(stop) = &mut cn.stop {
                changed |= cs(stop);
            }
        }
        changed
    });
    for o in overlays {
        each!(Keyframe, s.doc().children(o.entity()).collect(), |e| m(
            &mut e.at_local
        ));
    }
    let docs: Vec<Id<CaptionDoc>> = s.doc().children(c).collect();
    for d in docs {
        each!(Cue, s.doc().children(d.entity()).collect(), |e| m(
            &mut e.from
        ) | m(
            &mut e.to
        ));
        let mut cd = s.get(d).ok_or(ModelError::NotFound)?.clone();
        let mut changed = false;
        for r in cd.no_caption_ranges.values_mut() {
            changed |= m(&mut r.from) | m(&mut r.to);
        }
        if changed {
            s.replace(cd)?;
        }
    }
    // measurements are project-level but may be anchored on this cut's parts
    each!(
        Measurement,
        s.doc().iter::<Measurement>().map(|x| x.id).collect(),
        |e| {
            let a = me(&mut e.from);
            let b = e.to.as_mut().is_some_and(&me);
            a | b
        }
    );
    Ok(())
}

/// `t` as an anchor on `edge` of `part` (`end = true`: offset from `edge` as the part's
/// end; `false`: as its start). Exact ms when possible, else a raw source time on the
/// part's media with its reason (linted, R-ANC-1).
fn reanchor(
    part: Id<Part>,
    media: MediaRef,
    t: Flicks,
    edge: Flicks,
    end: bool,
    why: &str,
) -> Anchor {
    match flicks_to_exact_ms(Flicks(t.0 - edge.0)) {
        Some(ms) => {
            let e = PartEdge {
                part,
                offset_ms: (ms != 0).then_some(ms),
            };
            if end {
                Anchor::PartEnd(e)
            } else {
                Anchor::PartStart(e)
            }
        }
        None => {
            let (source, group) = match media {
                MediaRef::Source(s) => (Some(s), None),
                MediaRef::Group(g) => (None, Some(g)),
            };
            Anchor::Raw(RawTime {
                time: ModelTime::Src { source, group, t },
                reason: why.to_owned(),
            })
        }
    }
}

fn edge_time(edge: Flicks, offset_ms: Option<i64>) -> Flicks {
    Flicks(edge.0 + ms_to_flicks(offset_ms.unwrap_or(0)).0)
}

fn remap_patches(
    m: &BTreeMap<Id<Layer>, LayerPatch>,
    remap: &BTreeMap<Id<Layer>, Id<Layer>>,
) -> BTreeMap<Id<Layer>, LayerPatch> {
    m.iter()
        .map(|(k, v)| (remap.get(k).copied().unwrap_or(*k), v.clone()))
        .collect()
}

/// Splits `part` at source time `at` (strictly inside it). Returns the b-side's id.
pub fn split_part(s: &mut Staging<'_>, part: Id<Part>, at: Flicks) -> Result<Id<Part>, ModelError> {
    let a = s.get(part).ok_or(ModelError::NotFound)?.clone();
    if !(a.src_in < at && at < a.src_out) {
        return Err(ModelError::Invalid(
            "split point must be strictly inside the part",
        ));
    }
    let b_id: Id<Part> = s.mint();

    // the part's own added layers, copied to the b-side
    let mut remap: BTreeMap<Id<Layer>, Id<Layer>> = BTreeMap::new();
    let additions: Vec<Id<Layer>> = s.doc().children::<Layer>(part.entity()).collect();
    for lid in additions {
        let l = s.get(lid).ok_or(ModelError::NotFound)?.clone();
        if let LayerOwner::PartOverride { framing, .. } = l.owner {
            let nid: Id<Layer> = s.mint();
            remap.insert(lid, nid);
            let mut copy = l;
            copy.id = nid;
            copy.owner = LayerOwner::PartOverride {
                part: b_id,
                framing,
            };
            s.create(copy)?;
        }
    }

    // b-side: every field copied (a speed ramp divided at `at`, decision 0033)
    let ((a_speed, a_ramp), (b_speed, b_ramp)) = crate::clock::split_ramp(&a, at);
    let mut b = a.clone();
    b.id = b_id;
    b.src_in = at;
    b.speed = b_speed;
    b.ramp = b_ramp;
    b.order = key_after(s, a.cut, Some(part), None)?;
    b.lineage.split_of = Some(part);
    for look in b.look.values_mut() {
        look.overrides = remap_patches(&look.overrides, &remap);
    }

    // visual splits, per framing: those after `at` move; the one in force at `at` and
    // the ease chain its state depends on are copied
    let mut framings: Vec<Id<Framing>> = s
        .doc()
        .children::<VisualSplit>(part.entity())
        .filter_map(|id| s.get(id).map(|v| v.framing))
        .collect();
    framings.sort();
    framings.dedup();
    let mut moved: Vec<Id<VisualSplit>> = Vec::new();
    let mut copies: Vec<VisualSplit> = Vec::new();
    for fr in framings {
        let list: Vec<VisualSplit> = splits_of(s.doc(), part, fr).into_iter().cloned().collect();
        moved.extend(list.iter().filter(|v| v.at_src > at).map(|v| v.id));
        if let Some(i) = list.iter().rposition(|v| v.at_src <= at) {
            let mut chain = vec![i];
            let mut j = i;
            // split j's start state is split j−1's state at j's start; that depends on
            // j−2 only while j−1 is an ease still running then
            while matches!(list[j].transition, Transition::Ease { .. })
                && let Some(k) = j.checked_sub(1)
            {
                chain.push(k);
                match list[k].transition {
                    Transition::Ease { dur_ms, .. }
                        if list[j].at_src.0 - list[k].at_src.0
                            < ms_to_flicks(i64::from(dur_ms)).0 =>
                    {
                        j = k;
                    }
                    _ => break,
                }
            }
            chain.reverse();
            copies.extend(chain.into_iter().map(|k| list[k].clone()));
        }
    }
    s.create(b)?;
    for vid in moved {
        s.update(vid, |v| {
            v.part = b_id;
            v.overrides = remap_patches(&v.overrides, &remap);
        })?;
    }
    // copies keep their relative order: ids are minted in list order and the minter is
    // monotonic (ties on `at_src` sort by id)
    for mut c in copies {
        c.id = s.mint();
        c.part = b_id;
        c.overrides = remap_patches(&c.overrides, &remap);
        s.create(c)?;
    }
    s.update(part, |p| {
        p.src_out = at;
        p.speed = a_speed;
        p.ramp = a_ramp;
    })?;

    // anchors on the old edges, re-resolved to the side containing their time
    let (a_in, a_out, media) = (a.src_in, a.src_out, a.media);
    let why = "re-resolved after a part split";
    map_cut_anchors(s, a.cut, &|x: &Anchor| match x {
        Anchor::PartEnd(e) if e.part == part => {
            let t = edge_time(a_out, e.offset_ms);
            Some(if t >= at {
                Anchor::PartEnd(PartEdge {
                    part: b_id,
                    offset_ms: e.offset_ms,
                })
            } else {
                reanchor(part, media, t, at, true, why)
            })
        }
        Anchor::PartStart(e) if e.part == part => {
            let t = edge_time(a_in, e.offset_ms);
            (t >= at).then(|| reanchor(b_id, media, t, at, false, why))
        }
        _ => None,
    })?;
    // `after:` clips follow the b-side
    let clips: Vec<Id<Clip>> = s.doc().children::<Clip>(a.cut.entity()).collect();
    for cid in clips {
        if s.get(cid).is_some_and(|c| c.slot == ClipSlot::After(part)) {
            s.update(cid, |c| c.slot = ClipSlot::After(b_id))?;
        }
    }
    Ok(b_id)
}

/// Patch maps folded: `later` wins per layer and field.
pub fn fold(
    base: &BTreeMap<Id<Layer>, LayerPatch>,
    later: &BTreeMap<Id<Layer>, LayerPatch>,
) -> BTreeMap<Id<Layer>, LayerPatch> {
    let mut out = base.clone();
    for (k, p) in later {
        let merged = out.get(k).map_or_else(|| p.clone(), |b| b.then(p));
        out.insert(*k, merged);
    }
    out
}

/// The patch that, laid over `a`'s overrides, gives what `b`'s overrides give: `b`'s
/// fields, and for fields only `a` sets, the layer's own value (what `b` showed).
fn rebase_patch(a: &LayerPatch, b: &LayerPatch, l: &Layer) -> LayerPatch {
    fn pick<T: Clone>(a: &Option<T>, b: &Option<T>, own: T) -> Option<T> {
        match (a, b) {
            (_, Some(v)) => Some(v.clone()),
            (Some(_), None) => Some(own),
            (None, None) => None,
        }
    }
    let opt = |v: &Option<Grade>| v.clone().map_or(OptSet::Clear, OptSet::Set);
    LayerPatch {
        removed: pick(&a.removed, &b.removed, false),
        crop: pick(&a.crop, &b.crop, l.crop),
        place: pick(&a.place, &b.place, l.place),
        z: pick(&a.z, &b.z, l.z),
        opacity: pick(&a.opacity, &b.opacity, l.opacity),
        visible: pick(&a.visible, &b.visible, l.visible),
        mirror: pick(&a.mirror, &b.mirror, l.mirror),
        feather: pick(&a.feather, &b.feather, l.feather),
        grade: pick(&a.grade, &b.grade, opt(&l.grade)),
        matte: pick(&a.matte, &b.matte, l.matte.clone()),
        time_map: pick(&a.time_map, &b.time_map, l.time_map),
        out_of_range: pick(&a.out_of_range, &b.out_of_range, l.out_of_range),
        cover: pick(
            &a.cover,
            &b.cover,
            l.cover.map_or(OptSet::Clear, OptSet::Set),
        ),
        frame: pick(
            &a.frame,
            &b.frame,
            l.frame.clone().map_or(OptSet::Clear, OptSet::Set),
        ),
    }
}

/// Merges `b` into `a` (`b` immediately after `a`, same media, `a.src_out ==
/// b.src_in`). Keeps `a`'s id and records `lineage.merged`; refuses when the linked
/// layouts differ unless `force` (R-MODEL-2). See the module docs.
pub fn merge_parts(
    s: &mut Staging<'_>,
    a_id: Id<Part>,
    b_id: Id<Part>,
    force: bool,
) -> Result<Vec<EditNote>, ModelError> {
    let a = s.get(a_id).ok_or(ModelError::NotFound)?.clone();
    let b = s.get(b_id).ok_or(ModelError::NotFound)?.clone();
    if a.cut != b.cut {
        return Err(ModelError::Invalid("parts are in different cuts"));
    }
    let sibs = siblings(s, a.cut);
    let ia = sibs
        .iter()
        .position(|x| *x == a_id)
        .ok_or(ModelError::NotFound)?;
    if sibs.get(ia + 1) != Some(&b_id) {
        return Err(ModelError::Invalid("parts are not adjacent"));
    }
    if a.media != b.media || a.src_out != b.src_in {
        return Err(ModelError::Invalid(
            "parts are not continuous in the source",
        ));
    }
    let layouts = |p: &Part| -> BTreeMap<Id<Framing>, Option<Id<Layout>>> {
        p.look.iter().map(|(f, l)| (*f, l.layout_ref)).collect()
    };
    if !force && layouts(&a) != layouts(&b) {
        return Err(ModelError::Invalid("layouts differ"));
    }
    let mut notes = Vec::new();
    let at = b.src_in;

    // b's added layers: split copies of a's are dropped (keys remapped), others move
    let added = |p: Id<Part>, s: &Staging<'_>| -> Vec<Layer> {
        s.doc()
            .children::<Layer>(p.entity())
            .filter_map(|id| s.get(id).cloned())
            .filter(|l| matches!(l.owner, LayerOwner::PartOverride { .. }))
            .collect()
    };
    let mut a_free: Vec<Layer> = added(a_id, s);
    let mut remap: BTreeMap<Id<Layer>, Id<Layer>> = BTreeMap::new();
    for lb in added(b_id, s) {
        let twin = a_free.iter().position(|la| {
            let mut x = lb.clone();
            x.id = la.id;
            x.owner = la.owner;
            x == *la
        });
        match twin {
            Some(i) => {
                remap.insert(lb.id, a_free.remove(i).id);
                s.delete(lb.id)?;
            }
            None => {
                s.update(lb.id, |l| {
                    if let LayerOwner::PartOverride { framing, .. } = l.owner {
                        l.owner = LayerOwner::PartOverride {
                            part: a_id,
                            framing,
                        };
                    }
                })?;
            }
        }
    }

    // per framing: the patch turning a's base into b's (empty when the looks agree)
    let mut rebase: BTreeMap<Id<Framing>, BTreeMap<Id<Layer>, LayerPatch>> = BTreeMap::new();
    // every framing either look or b's splits mention (a framing with no look entry
    // can still have splits)
    let b_split_framings: Vec<Id<Framing>> = s
        .doc()
        .children::<VisualSplit>(b_id.entity())
        .filter_map(|id| s.get(id).map(|v| v.framing))
        .collect();
    let mut framings: Vec<Id<Framing>> = a
        .look
        .keys()
        .chain(b.look.keys())
        .chain(b_split_framings.iter())
        .copied()
        .collect();
    framings.sort();
    framings.dedup();
    for f in &framings {
        let ao = a
            .look
            .get(f)
            .map(|l| l.overrides.clone())
            .unwrap_or_default();
        let bo = remap_patches(
            &b.look
                .get(f)
                .map(|l| l.overrides.clone())
                .unwrap_or_default(),
            &remap,
        );
        if ao == bo {
            continue;
        }
        let mut p = BTreeMap::new();
        let ids: std::collections::BTreeSet<Id<Layer>> =
            ao.keys().chain(bo.keys()).copied().collect();
        for id in ids {
            let Some(l) = s.get(id).cloned() else {
                continue;
            };
            let empty = LayerPatch::default();
            let rp = rebase_patch(
                ao.get(&id).unwrap_or(&empty),
                bo.get(&id).unwrap_or(&empty),
                &l,
            );
            if !rp.is_empty() {
                p.insert(id, rp);
            }
        }
        if !p.is_empty() {
            rebase.insert(*f, p);
        }
    }

    // a's splits beyond its end were inert; after the merge they would take over b's
    // range. Splits exactly at the merge point that b carries a copy of are the
    // originals a split left there (several can share one time, and b copied only those
    // its start state depends on): they stay, and b's copies of them go below.
    let copied_at: std::collections::BTreeSet<Id<Framing>> = s
        .doc()
        .children::<VisualSplit>(b_id.entity())
        .filter_map(|id| s.get(id))
        .filter(|v| v.at_src == at)
        .filter(|v| {
            let ov = remap_patches(&v.overrides, &remap);
            splits_of(s.doc(), a_id, v.framing)
                .iter()
                .any(|x| x.at_src == at && x.transition == v.transition && x.overrides == ov)
        })
        .map(|v| v.framing)
        .collect();
    for id in s
        .doc()
        .children::<VisualSplit>(a_id.entity())
        .collect::<Vec<_>>()
    {
        if s.get(id)
            .is_some_and(|v| v.at_src > at || (v.at_src == at && !copied_at.contains(&v.framing)))
        {
            s.delete(id)?;
            notes.push(EditNote::DroppedSplit { split: id });
        }
    }
    // b's splits: pre-roll copies of a's are dropped; b's own move over the rebase patch
    let mut covered: std::collections::BTreeSet<Id<Framing>> = std::collections::BTreeSet::new();
    for f in &framings {
        for v in splits_of(s.doc(), b_id, *f)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            let ov = remap_patches(&v.overrides, &remap);
            if v.at_src <= at {
                let twin = splits_of(s.doc(), a_id, *f).iter().any(|x| {
                    x.at_src == v.at_src && x.transition == v.transition && x.overrides == ov
                });
                if twin {
                    s.delete(v.id)?;
                    continue;
                }
            }
            let ov = rebase.get(f).map_or_else(|| ov.clone(), |p| fold(p, &ov));
            if v.at_src <= at {
                covered.insert(*f);
            }
            s.update(v.id, |x| {
                x.part = a_id;
                x.overrides = ov;
                x.at_src = x.at_src.max(at);
            })?;
        }
    }
    // where b's base differs and no split of b's covers its start, one at the merge point
    for (f, p) in rebase {
        if covered.contains(&f) {
            continue;
        }
        let id = s.mint();
        s.create(VisualSplit {
            id,
            part: a_id,
            framing: f,
            at_src: at,
            overrides: p,
            transition: Transition::Cut,
            tag: None,
        })?;
    }

    let (a_out, b_out, media) = (a.src_out, b.src_out, a.media);
    s.update(a_id, |p| {
        p.src_out = b_out;
        // b's lineage is a copy of a's when b came from splitting a: record each id
        // once, or repeated split/merge would double the list every cycle
        for m in std::iter::once(b_id).chain(b.lineage.merged.iter().copied()) {
            if !p.lineage.merged.contains(&m) {
                p.lineage.merged.push(m);
            }
        }
    })?;
    let a_in = a.src_in;
    let why = "re-resolved after a part merge";
    map_cut_anchors(s, a.cut, &|x: &Anchor| match x {
        Anchor::PartEnd(e) if e.part == b_id => Some(Anchor::PartEnd(PartEdge {
            part: a_id,
            offset_ms: e.offset_ms,
        })),
        // a's end moved to b's end: keep the time
        Anchor::PartEnd(e) if e.part == a_id => Some(reanchor(
            a_id,
            media,
            edge_time(a_out, e.offset_ms),
            b_out,
            true,
            why,
        )),
        Anchor::PartStart(e) if e.part == b_id => Some(reanchor(
            a_id,
            media,
            edge_time(at, e.offset_ms),
            a_in,
            false,
            why,
        )),
        _ => None,
    })?;
    let clips: Vec<Id<Clip>> = s.doc().children::<Clip>(a.cut.entity()).collect();
    for cid in clips {
        if s.get(cid).is_some_and(|c| c.slot == ClipSlot::After(b_id)) {
            s.update(cid, |c| c.slot = ClipSlot::After(a_id))?;
        }
    }
    debug_assert_eq!(s.doc().children::<VisualSplit>(b_id.entity()).count(), 0);
    s.delete(b_id)?;
    Ok(notes)
}

/// Moves `part` to just after `after` (`None` = first). One entity written.
pub fn reorder_part(
    s: &mut Staging<'_>,
    part: Id<Part>,
    after: Option<Id<Part>>,
) -> Result<(), ModelError> {
    if after == Some(part) {
        return Err(ModelError::Invalid("a part cannot follow itself"));
    }
    let cut = s.get(part).ok_or(ModelError::NotFound)?.cut;
    let key = key_after(s, cut, after, Some(part))?;
    s.update(part, |p| p.order = key)
}

/// Deletes `part` with its visual splits and added layers. `after:` clips move to the
/// previous part (or become intros); anchors left naming the part are reported.
pub fn delete_part(s: &mut Staging<'_>, part: Id<Part>) -> Result<Vec<EditNote>, ModelError> {
    let p = s.get(part).ok_or(ModelError::NotFound)?.clone();
    let mut notes = Vec::new();
    let sibs = siblings(s, p.cut);
    let prev = sibs
        .iter()
        .position(|x| *x == part)
        .and_then(|i| i.checked_sub(1))
        .map(|i| sibs[i]);
    for v in s
        .doc()
        .children::<VisualSplit>(part.entity())
        .collect::<Vec<_>>()
    {
        s.delete(v)?;
    }
    for l in s.doc().children::<Layer>(part.entity()).collect::<Vec<_>>() {
        s.delete(l)?;
    }
    for cid in s.doc().children::<Clip>(p.cut.entity()).collect::<Vec<_>>() {
        if s.get(cid).is_some_and(|c| c.slot == ClipSlot::After(part)) {
            let to = prev.map_or(ClipSlot::Intro, ClipSlot::After);
            s.update(cid, |c| c.slot = to)?;
            notes.push(EditNote::ClipReparented { clip: cid, to });
        }
    }
    s.delete(part)?;
    let dangling = std::cell::Cell::new(false);
    map_cut_anchors(s, p.cut, &|x: &Anchor| {
        if x.part() == Some(part) {
            dangling.set(true);
        }
        None
    })?;
    if dangling.get() {
        notes.push(EditNote::DanglingAnchor {
            entity: part.entity(),
        });
    }
    Ok(notes)
}

/// Sets a part's source range (`src_in < src_out`).
pub fn trim_part(
    s: &mut Staging<'_>,
    part: Id<Part>,
    src_in: Flicks,
    src_out: Flicks,
) -> Result<(), ModelError> {
    if src_in >= src_out {
        return Err(ModelError::Invalid("a part's range must not be empty"));
    }
    s.update(part, |p| {
        p.src_in = src_in;
        p.src_out = src_out;
    })
}

/// Links (or with `None` unlinks) a layout for one framing of a part.
pub fn link_layout(
    s: &mut Staging<'_>,
    part: Id<Part>,
    framing: Id<Framing>,
    layout: Option<Id<Layout>>,
) -> Result<(), ModelError> {
    if let Some(l) = layout
        && s.get(l).is_none()
    {
        return Err(ModelError::NotFound);
    }
    s.update(part, |p| {
        let look = p.look.entry(framing).or_default();
        look.layout_ref = layout;
        if look.layout_ref.is_none() && look.overrides.is_empty() {
            p.look.remove(&framing);
        }
    })
}
