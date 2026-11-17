//! Effective state (SPEC §4.3.1): the one pure fold the planner, lint, `see` and
//! `cut_get{resolved}` read layers through.
//!
//! `effective(part, framing, t_src)` folds, later wins per layer id and field:
//! 1. the stack: the linked layout's layers when `part.look[framing].layout_ref` names
//!    one (live), else the framing's default layers; plus the part's own additions
//!    (layers owned by `PartOverride{part, framing}`). A layout is a complete stack
//!    (SPEC §4.3 "its stack = its ordered Layer children") and every layer is one entity
//!    with one owner, so no layer id is shared between a framing and a layout: linking
//!    *replaces* the default stack, and "later wins per layer id" applies from step 2;
//! 2. the part's sparse overrides `look[framing].overrides` (tombstone = `removed`);
//! 3. the **last** `VisualSplit` of this part and framing with `at_src ≤ t_src`
//!    (an `ease` interpolates crop, place and opacity from the state at its start, over
//!    `dur_ms` on the source clock; other fields switch at `at_src`);
//! 4. active `ContentHold`s (time map → hold) then `HideRoles` (visible → false) of
//!    the cut, unless this framing opted out. Holds are source-time ranges scoped to the
//!    media their anchors name.
//!
//! Removed layers are dropped at the end, so a patch on a removed layer stays inert
//! unless a later `removed: Some(false)` revives it. The result is sorted by
//! `(z, stack before additions, order key, id)`.

use std::collections::BTreeMap;

use crate::clock::{anchor_src, ms_to_flicks};
use crate::doc::Doc;
use crate::entities::*;
use crate::geometry::{NormRect, Place};
use crate::ids::Id;
use crate::order::OrderKey;
use crate::time::Flicks;

/// One layer as it renders at a time.
#[derive(Clone, PartialEq, Debug)]
pub struct ResolvedLayer {
    pub id: Id<Layer>,
    pub order: OrderKey,
    /// Added by the part (owner `PartOverride`), not from the framing or layout stack.
    pub addition: bool,
    pub values: LayerValues,
}

/// A resolved layer's values (everything but its identity), compared by the
/// R-MODEL-5 invariance tests.
#[derive(Clone, PartialEq, Debug)]
pub struct LayerValues {
    pub kind: LayerKind,
    pub member: Option<Id<SyncMember>>,
    pub role: LayerRole,
    pub crop: NormRect,
    pub place: Place,
    pub z: i32,
    pub opacity: f64,
    pub visible: bool,
    pub mirror: bool,
    pub feather: Feather,
    pub grade: Option<Grade>,
    pub matte: Matte,
    pub time_map: TimeMap,
    pub out_of_range: OutOfRange,
    pub cover: Option<CoverStyle>,
    pub frame: Option<LayerFrame>,
}

impl LayerValues {
    fn of(l: &Layer) -> Self {
        LayerValues {
            kind: l.kind,
            member: l.member,
            role: l.role,
            crop: l.crop,
            place: l.place,
            z: l.z,
            opacity: l.opacity,
            visible: l.visible,
            mirror: l.mirror,
            feather: l.feather,
            grade: l.grade.clone(),
            matte: l.matte.clone(),
            time_map: l.time_map,
            out_of_range: l.out_of_range,
            cover: l.cover,
            frame: l.frame.clone(),
        }
    }

    fn patch(&mut self, p: &LayerPatch) {
        if let Some(v) = p.crop {
            self.crop = v;
        }
        if let Some(v) = p.place {
            self.place = v;
        }
        if let Some(v) = p.z {
            self.z = v;
        }
        if let Some(v) = p.opacity {
            self.opacity = v;
        }
        if let Some(v) = p.visible {
            self.visible = v;
        }
        if let Some(v) = p.mirror {
            self.mirror = v;
        }
        if let Some(v) = p.feather {
            self.feather = v;
        }
        if let Some(v) = &p.grade {
            self.grade = v.get();
        }
        if let Some(v) = &p.matte {
            self.matte = v.clone();
        }
        if let Some(v) = p.time_map {
            self.time_map = v;
        }
        if let Some(v) = p.out_of_range {
            self.out_of_range = v;
        }
        if let Some(v) = &p.cover {
            self.cover = v.get();
        }
        if let Some(v) = &p.frame {
            self.frame = v.get();
        }
    }
}

/// The stack for one part, framing and source time.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResolvedStack {
    pub layers: Vec<ResolvedLayer>,
    /// The visual split in force, if any.
    pub split: Option<Id<VisualSplit>>,
}

impl ResolvedStack {
    /// The layers' values in stack order, without ids (R-MODEL-5 comparisons).
    pub fn values(&self) -> Vec<LayerValues> {
        self.layers.iter().map(|l| l.values.clone()).collect()
    }
}

#[derive(Clone)]
struct Entry {
    order: OrderKey,
    addition: bool,
    removed: bool,
    v: LayerValues,
}

type Work = BTreeMap<Id<Layer>, Entry>;

fn apply_patches(work: &mut Work, patches: &BTreeMap<Id<Layer>, LayerPatch>) {
    for (id, p) in patches {
        if let Some(e) = work.get_mut(id) {
            if let Some(r) = p.removed {
                e.removed = r;
            }
            e.v.patch(p);
        }
    }
}

/// Visual splits of `part` in `framing`, by `at_src` then id.
pub fn splits_of(doc: &Doc, part: Id<Part>, framing: Id<Framing>) -> Vec<&VisualSplit> {
    let mut v: Vec<&VisualSplit> = doc
        .children::<VisualSplit>(part.entity())
        .filter_map(|id| doc.get(id))
        .filter(|s| s.framing == framing)
        .collect();
    v.sort_by(|a, b| a.at_src.cmp(&b.at_src).then(a.id.cmp(&b.id)));
    v
}

/// The state split `i` gives at `t` (≥ its `at_src`), over the part's patched base.
/// Only the split in force applies (no accumulation); an `ease` interpolates crop, place
/// and opacity from the state at its own start — the previous split's state *at that
/// moment*, recursively, so an ease that starts during another one continues from where
/// that one was rather than from its target.
fn split_state(base: &Work, splits: &[&VisualSplit], i: usize, t: Flicks, settle: bool) -> Work {
    let s = splits[i];
    let mut after = base.clone();
    apply_patches(&mut after, &s.overrides);
    if let Transition::Ease { dur_ms, curve } = s.transition
        && !settle
    {
        let dur = ms_to_flicks(i64::from(dur_ms));
        let into = t.0 - s.at_src.0;
        if dur.0 > 0 && into < dur.0 {
            let before = match i.checked_sub(1) {
                Some(j) => split_state(base, splits, j, s.at_src, false),
                None => base.clone(),
            };
            #[allow(clippy::cast_precision_loss)]
            let pr = curve.apply(into as f64 / dur.0 as f64);
            for (id, e) in &mut after {
                if let Some(b) = before.get(id) {
                    e.v.crop = b.v.crop.lerp(e.v.crop, pr);
                    e.v.place = b.v.place.lerp(e.v.place, pr);
                    e.v.opacity = crate::geometry::lerp(b.v.opacity, e.v.opacity, pr);
                }
            }
        }
    }
    after
}

/// The media an anchor's time is on, when the anchor says (part edges, sourced raw
/// times, frames). Word and line refs name an ASR revision, not media: `None`.
fn anchor_media(doc: &Doc, a: &crate::anchor::Anchor) -> Option<MediaRef> {
    use crate::anchor::Anchor;
    use crate::clock::ModelTime;
    match a {
        Anchor::PartStart(e) | Anchor::PartEnd(e) => doc.get(e.part).map(|p| p.media),
        Anchor::Frame(f) => Some(MediaRef::Source(f.source)),
        Anchor::Raw(r) => match &r.time {
            ModelTime::Src {
                source: Some(s), ..
            } => Some(MediaRef::Source(*s)),
            ModelTime::Src { group: Some(g), .. } => Some(MediaRef::Group(*g)),
            ModelTime::Local { part: Some(p), .. } => doc.get(*p).map(|p| p.media),
            _ => None,
        },
        _ => None,
    }
}

/// The layers making up the stack before any patch: the linked layout's (if linked and
/// present) or the framing's defaults, then the part's own additions.
pub fn base_layers(doc: &Doc, part: &Part, framing: Id<Framing>) -> Vec<Id<Layer>> {
    let look = part.look.get(&framing);
    let mut out: Vec<Id<Layer>> = match look
        .and_then(|l| l.layout_ref)
        .filter(|l| doc.get(*l).is_some())
    {
        Some(layout) => doc
            .children::<Layer>(layout.entity())
            .filter(|id| {
                doc.get(*id)
                    .is_some_and(|l| l.owner == LayerOwner::Layout(layout))
            })
            .collect(),
        None => doc
            .children::<Layer>(framing.entity())
            .filter(|id| {
                doc.get(*id)
                    .is_some_and(|l| l.owner == LayerOwner::Framing(framing))
            })
            .collect(),
    };
    out.extend(doc.children::<Layer>(part.id.entity()).filter(|id| {
        doc.get(*id).is_some_and(|l| {
            l.owner
                == LayerOwner::PartOverride {
                    part: part.id,
                    framing,
                }
        })
    }));
    out
}

/// The fold of SPEC §4.3.1. `None` when the part or framing does not exist.
pub fn effective(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    t_src: Flicks,
) -> Option<ResolvedStack> {
    fold(doc, part, framing, t_src, false)
}

/// [`effective`] with the split in force fully applied: the state an `ease` is heading
/// for (what it shows once `dur_ms` has passed). The planner writes this as an eased
/// plan part's own values and [`effective`] at the split as where the ease starts.
pub fn effective_settled(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    t_src: Flicks,
) -> Option<ResolvedStack> {
    fold(doc, part, framing, t_src, true)
}

fn fold(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    t_src: Flicks,
    settle: bool,
) -> Option<ResolvedStack> {
    let p = doc.get(part)?;
    doc.get(framing)?;
    let mut work: Work = BTreeMap::new();
    for id in base_layers(doc, p, framing) {
        if let Some(l) = doc.get(id) {
            work.insert(
                id,
                Entry {
                    order: l.order.clone(),
                    addition: matches!(l.owner, LayerOwner::PartOverride { .. }),
                    removed: false,
                    v: LayerValues::of(l),
                },
            );
        }
    }
    if let Some(look) = p.look.get(&framing) {
        apply_patches(&mut work, &look.overrides);
    }

    // 3. last visual split with at_src ≤ t_src
    let splits = splits_of(doc, part, framing);
    let idx = splits.iter().rposition(|s| s.at_src <= t_src);
    let mut split = None;
    if let Some(i) = idx {
        split = Some(splits[i].id);
        work = split_state(&work, &splits, i, t_src, settle);
    }

    // 4. holds then hides, unless this framing opted out
    // A hold is a source-time range: it applies to this part only when its anchors are
    // on this part's media (when they say which), and `from ≤ t < to`.
    let in_range = |from: &crate::anchor::Anchor, to: &crate::anchor::Anchor| -> bool {
        let same_media = [from, to]
            .iter()
            .all(|a| anchor_media(doc, a).is_none_or(|m| m == p.media));
        match (anchor_src(doc, from), anchor_src(doc, to)) {
            (Some(a), Some(b)) => same_media && a <= t_src && t_src < b,
            _ => false,
        }
    };
    let selected = |role: LayerRole, roles: &[LayerRole]| -> bool {
        if roles.is_empty() {
            role != LayerRole::Region(RegionRole::Cam)
        } else {
            roles.iter().any(|r| role.selected_by(*r))
        }
    };
    for id in doc.children::<ContentHold>(p.cut.entity()) {
        let Some(h) = doc.get(id) else { continue };
        if h.per_framing.get(&framing).is_some_and(|o| o.ignore) || !in_range(&h.from, &h.to) {
            continue;
        }
        for e in work.values_mut() {
            if selected(e.v.role, &h.roles) {
                e.v.time_map = TimeMap::Hold(h.at);
            }
        }
    }
    for id in doc.children::<HideRoles>(p.cut.entity()) {
        let Some(h) = doc.get(id) else { continue };
        if h.per_framing.get(&framing).is_some_and(|o| o.ignore) || !in_range(&h.from, &h.to) {
            continue;
        }
        for e in work.values_mut() {
            if selected(e.v.role, &h.roles) {
                e.v.visible = false;
            }
        }
    }

    let mut layers: Vec<ResolvedLayer> = work
        .into_iter()
        .filter(|(_, e)| !e.removed)
        .map(|(id, e)| ResolvedLayer {
            id,
            order: e.order,
            addition: e.addition,
            values: e.v,
        })
        .collect();
    layers.sort_by(|a, b| {
        a.values
            .z
            .cmp(&b.values.z)
            .then_with(|| a.addition.cmp(&b.addition))
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.id.cmp(&b.id))
    });
    Some(ResolvedStack { layers, split })
}

/// `effective_caption_place(part, framing)` = last of framing → profile seat for the
/// linked layout → layout → part override (SPEC §4.3.1). The profile seat is passed in
/// (profiles live outside the document).
pub fn effective_caption_place(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    profile_seat: Option<&dyn Fn(Id<Layout>) -> Option<Place>>,
) -> Option<Place> {
    let p = doc.get(part)?;
    let f = doc.get(framing)?;
    let mut place = f.caption_place;
    if let Some(layout) = p.look.get(&framing).and_then(|l| l.layout_ref) {
        if let Some(seat) = profile_seat.and_then(|s| s(layout)) {
            place = Some(seat);
        }
        if let Some(lp) = doc.get(layout).and_then(|l| l.caption_place) {
            place = Some(lp);
        }
    }
    if let Some(o) = p.caption_place_override.get(&framing) {
        place = Some(*o);
    }
    place
}

/// An overlay's place in a framing: `base ⊕ per_framing` (keyframes: M4).
pub fn effective_overlay_place(o: &Overlay, framing: Id<Framing>) -> (bool, OverlayGeom) {
    match o.per_framing.get(&framing) {
        Some(pf) => (
            pf.visible,
            match pf.place {
                Some(p) => OverlayGeom::Place(p),
                None => o.geom,
            },
        ),
        None => (true, o.geom),
    }
}

/// Where one value of a resolved layer comes from (SPEC §5.7 "inherited values shown
/// with their source"; A2.09).
#[derive(Copy, Clone, Eq, PartialEq, Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    /// The visual split in force sets it.
    Split,
    /// The part's own override in this framing.
    Part,
    /// The linked layout's layer.
    Layout,
    /// The framing's default stack.
    Framing,
    /// A layer the part added itself (all its values are its own).
    Own,
}

/// Where each field of one layer of a part's stack comes from, at a source time: the
/// owner (`layout`, `framing` or `own`), the layout's id when linked, the split in
/// force, and per field the first of split → part override → owner that sets it.
#[derive(Clone, PartialEq, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct LayerSources {
    pub owner: ValueSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<Id<Layout>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<Id<VisualSplit>>,
    pub crop: ValueSource,
    pub place: ValueSource,
    pub z: ValueSource,
    pub opacity: ValueSource,
    pub visible: ValueSource,
    pub mirror: ValueSource,
    pub feather: ValueSource,
    pub grade: ValueSource,
    pub matte: ValueSource,
    pub time_map: ValueSource,
    pub cover: ValueSource,
}

/// [`LayerSources`] for `layer` in `part`'s stack in `framing` at `t_src`; `None` when the
/// layer is not in that stack. The fold is [`effective`]'s (steps 1–3); holds and hides
/// (step 4) are the cut's own entities and are listed with it.
pub fn value_sources(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    layer: Id<Layer>,
    t_src: Flicks,
) -> Option<LayerSources> {
    let p = doc.get(part)?;
    if !base_layers(doc, p, framing).contains(&layer) {
        return None;
    }
    let l = doc.get(layer)?;
    let (owner, layout) = match l.owner {
        LayerOwner::Layout(id) => (ValueSource::Layout, Some(id)),
        LayerOwner::Framing(_) => (ValueSource::Framing, None),
        _ => (ValueSource::Own, None),
    };
    let over = p.look.get(&framing).and_then(|lk| lk.overrides.get(&layer));
    let split = effective(doc, part, framing, t_src).and_then(|st| st.split);
    let sp = split
        .and_then(|id| doc.get::<VisualSplit>(id))
        .and_then(|v| v.overrides.get(&layer));
    let pick = |f: fn(&LayerPatch) -> bool| {
        if sp.is_some_and(f) {
            ValueSource::Split
        } else if over.is_some_and(f) {
            ValueSource::Part
        } else {
            owner
        }
    };
    Some(LayerSources {
        owner,
        layout,
        split,
        crop: pick(|o| o.crop.is_some()),
        place: pick(|o| o.place.is_some()),
        z: pick(|o| o.z.is_some()),
        opacity: pick(|o| o.opacity.is_some()),
        visible: pick(|o| o.visible.is_some()),
        mirror: pick(|o| o.mirror.is_some()),
        feather: pick(|o| o.feather.is_some()),
        grade: pick(|o| o.grade.is_some()),
        matte: pick(|o| o.matte.is_some()),
        time_map: pick(|o| o.time_map.is_some()),
        cover: pick(|o| o.cover.is_some()),
    })
}
