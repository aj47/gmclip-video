//! Sections (decision 0046) through the planner: each zoom section of the cut (of this
//! framing, or of every framing) becomes a RenderPlan `/1` `zooms[]` entry on the output
//! frames its anchors land on (the overlays' reading, [`super::Ctx::frame`]), naming per
//! part it overlaps the layers it draws tighter:
//! - `screen`: the screen and detail layers;
//! - `cursor`: the layers that draw a recorded cursor;
//! - `all`: every picture layer but covers;
//!
//! falling back to the part's bottom picture layer when none matches. Cut-out (roto)
//! layers are never zoomed (their mask track is cached per crop, as for an eased split).
//! A `cursor` or `focus` target names the `cursors[]` entry of the first zoomed layer that
//! draws one; without one (a source with no log, or its cursor hidden) the zoom holds on
//! the layer's centre. `camera_scale` names the part's camera layers.
//!
//! Layout sections plan as eased splits of the parts they cover (no new plan field):
//! [`layout_spans`] reads them in output frames, the part loop splits its plan parts at
//! their edges ([`inner_edges`]) and asks [`layout_at`] what each plan part's start is
//! in (a layout settled, or an ease from one state to the next), and [`map_layout`] gives
//! the part's own layers the layout's values by role.

use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_plan::scalars::{Id as PId, Range};
use gmclip_plan::types as rp;

use super::{Ctx, PlannerError};

/// `ms` in output frames at `fnum/fden` (nearest, as a layer ease's).
fn ms_frames(ms: u32, fnum: u64, fden: u64) -> u64 {
    (u64::from(ms) * fnum + 500 * fden) / (1000 * fden)
}

/// Whether `l` can be zoomed.
fn zoomable(l: &rp::Layer) -> bool {
    l.cover.is_none() && l.matte.is_none()
}

/// The layers of `p` a zoom of `which` draws tighter.
fn targets(p: &rp::Part, which: ZoomLayer) -> Vec<&rp::Layer> {
    let mut v: Vec<&rp::Layer> = p
        .layers
        .iter()
        .filter(|l| zoomable(l))
        .filter(|l| match which {
            ZoomLayer::Screen => matches!(l.role.as_str(), "screen" | "detail"),
            ZoomLayer::Cursor => l.cursor.is_some(),
            ZoomLayer::All => true,
        })
        .collect();
    if v.is_empty() {
        // the bottom picture layer
        if let Some(b) = p
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| zoomable(l) && l.cover.is_none())
            .min_by_key(|(i, l)| (l.z, *i))
            .map(|(_, l)| l)
        {
            v.push(b);
        }
    }
    v
}

/// The cut's zoom sections in `framing`, as plan zooms. `parts` are the planned parts
/// (with their cursors), `total` the output frames.
pub(super) fn plan_zooms(
    ctx: &Ctx<'_>,
    framing: Id<Framing>,
    parts: &[rp::Part],
    total: u64,
    (fnum, fden): (u64, u64),
) -> Result<Vec<rp::Zoom>, PlannerError> {
    let doc = ctx.doc;
    let mut out: Vec<rp::Zoom> = Vec::new();
    for sid in doc.children::<Section>(ctx.cut.entity()) {
        let Some(s) = doc.get(sid) else { continue };
        if s.framing.is_some_and(|f| f != framing) {
            continue;
        }
        let SectionKind::Zoom(z) = &s.kind else {
            continue;
        };
        // an end that no longer resolves, or a span the edit turned around, plans nothing
        let (Ok((a, _)), Ok((b, _))) = (ctx.frame(&s.start, total), ctx.frame(&s.end, total))
        else {
            continue;
        };
        let b = b.min(total);
        if b <= a {
            continue;
        }
        let mut layers = Vec::new();
        let mut cams = Vec::new();
        let mut cursor: Option<PId> = None;
        for p in parts
            .iter()
            .filter(|p| p.out.start() < b && p.out.end() > a)
        {
            for l in targets(p, z.layer) {
                if cursor.is_none() {
                    cursor = l.cursor.clone();
                }
                layers.push(rp::LayerRef {
                    part: p.id.clone(),
                    layer: l.id.clone(),
                });
            }
            if z.camera_scale.is_some_and(|c| c < 1.0) {
                for l in p
                    .layers
                    .iter()
                    .filter(|l| l.role.as_str() == "cam" && l.cover.is_none())
                {
                    if !layers.iter().any(|r| r.part == p.id && r.layer == l.id) {
                        cams.push(rp::LayerRef {
                            part: p.id.clone(),
                            layer: l.id.clone(),
                        });
                    }
                }
            }
        }
        if layers.is_empty() {
            continue;
        }
        let target = match (z.target, cursor) {
            (ZoomTarget::Cursor, Some(c)) => rp::ZoomTarget::Cursor(c),
            (ZoomTarget::Focus, Some(c)) => rp::ZoomTarget::Focus(c),
            (ZoomTarget::Point { x, y }, _) => {
                rp::ZoomTarget::Point([x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)])
            }
            // no recorded cursor to follow: hold on the centre
            (_, None) => rp::ZoomTarget::Point([0.5, 0.5]),
        };
        // the eases fit inside the span (shortened in proportion when they do not)
        let len = b - a;
        let (mut ei, mut eo) = (
            ms_frames(z.ease_in_ms, fnum, fden),
            ms_frames(z.ease_out_ms, fnum, fden),
        );
        if ei + eo > len {
            let sum = ei + eo;
            ei = ei * len / sum;
            eo = (len - ei).min(eo);
        }
        out.push(rp::Zoom {
            id: PId::new(format!("zoom-{}", out.len() + 1)).map_err(PlannerError)?,
            out: Range([a, b]),
            layers,
            depth: z.depth.clamp(1.0, 6.0),
            target,
            ease_in: ei,
            ease_out: eo,
            camera_scale: z
                .camera_scale
                .filter(|_| !cams.is_empty())
                .map(|c| c.clamp(0.3, 1.0)),
            camera_layers: (!cams.is_empty()).then_some(cams),
            motion_blur: z.motion_blur.then_some(true),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Layout sections
// ---------------------------------------------------------------------------------------

/// A layout section on the output frames `[a, b)`: its layout and its ease in frames.
#[derive(Clone, Debug)]
pub(super) struct LayoutSpan {
    pub a: u64,
    pub b: u64,
    pub layout: Id<Layout>,
    pub ease: u64,
}

/// The cut's layout sections in `framing` (of this framing, or of every framing), in
/// output frames (read as a zoom section's; an end that does not resolve or a span the edit
/// turned around plans nothing), in the cut's section order. A layout that is gone is
/// left out.
pub(super) fn layout_spans(
    ctx: &Ctx<'_>,
    framing: Id<Framing>,
    total: u64,
    (fnum, fden): (u64, u64),
) -> Vec<LayoutSpan> {
    let doc = ctx.doc;
    let mut out = Vec::new();
    for sid in doc.children::<Section>(ctx.cut.entity()) {
        let Some(s) = doc.get(sid) else { continue };
        if s.framing.is_some_and(|f| f != framing) {
            continue;
        }
        let SectionKind::Layout { layout, ease_ms } = &s.kind else {
            continue;
        };
        if doc.get(*layout).is_none() {
            continue;
        }
        let (Ok((a, _)), Ok((b, _))) = (ctx.frame(&s.start, total), ctx.frame(&s.end, total))
        else {
            continue;
        };
        let b = b.min(total);
        if b <= a {
            continue;
        }
        out.push(LayoutSpan {
            a,
            b,
            layout: *layout,
            ease: ms_frames(*ease_ms, fnum, fden),
        });
    }
    out
}

/// The section in force at output frame `g`: the latest-starting one that covers it
/// (index into `spans`).
fn in_force(spans: &[LayoutSpan], g: u64) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.a <= g && g < s.b)
        .max_by_key(|(i, s)| (s.a, *i))
        .map(|(i, _)| i)
}

/// The frames where the section in force changes, ascending.
fn events(spans: &[LayoutSpan]) -> Vec<u64> {
    let mut v: Vec<u64> = spans.iter().flat_map(|s| [s.a, s.b]).collect();
    v.sort_unstable();
    v.dedup();
    v.retain(|&e| (e > 0).then(|| in_force(spans, e - 1)).flatten() != in_force(spans, e));
    v
}

/// What output frame `g` is in.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum LayoutAt {
    /// No section, and no ease back from one: the part's own stack.
    Own,
    /// A section's layout, its ease done.
    Settled(Id<Layout>),
    /// An ease that started at `e` and lasts `n` frames, from `before` (none: the part's
    /// own stack) to `after`.
    Ease {
        before: Option<Id<Layout>>,
        after: Option<Id<Layout>>,
        e: u64,
        n: u64,
    },
}

pub(super) fn layout_at(spans: &[LayoutSpan], g: u64) -> LayoutAt {
    let ev = events(spans);
    let Some(&e) = ev.iter().rev().find(|&&e| e <= g) else {
        return LayoutAt::Own;
    };
    let before = (e > 0).then(|| in_force(spans, e - 1)).flatten();
    let after = in_force(spans, e);
    // a section starting here eases in over its own ease; else the one ending eases out
    let n = match (after, before) {
        (Some(i), _) if spans[i].a == e => spans[i].ease,
        (_, Some(j)) => spans[j].ease,
        (Some(i), None) => spans[i].ease,
        (None, None) => 0,
    };
    if g < e + n {
        LayoutAt::Ease {
            before: before.map(|i| spans[i].layout),
            after: after.map(|i| spans[i].layout),
            e,
            n,
        }
    } else {
        match after {
            Some(i) => LayoutAt::Settled(spans[i].layout),
            None => LayoutAt::Own,
        }
    }
}

/// The section edges strictly inside a part on the output frames `[base, base + len)`,
/// relative to its first frame.
pub(super) fn inner_edges(spans: &[LayoutSpan], base: u64, len: u64) -> Vec<i64> {
    events(spans)
        .into_iter()
        .filter(|&e| e > base && e < base + len)
        .map(|e| (e - base) as i64)
        .collect()
}

/// The values `layout` gives each layer of a part's stack (`None`: a layer it does not
/// map, which keeps its own). Each of the layout's picture layers (in its stack order)
/// maps to the first picture layer of the part with the same role not yet mapped, and
/// gives it its crop, place, z, opacity, visibility and frame; the part's layer keeps its
/// source (member), time map, matte, mirror, feather and cover.
pub(super) fn map_layout(
    doc: &gmclip_model::Doc,
    layout: Id<Layout>,
    stack: &[gmclip_model::effective::ResolvedLayer],
) -> Vec<Option<gmclip_model::effective::LayerValues>> {
    let mut from: Vec<&Layer> = doc
        .children::<Layer>(layout.entity())
        .filter_map(|id| doc.get(id))
        .filter(|l| l.owner == LayerOwner::Layout(layout) && l.kind == LayerKind::SourceView)
        .collect();
    from.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.order.cmp(&b.order)));
    let mut out: Vec<Option<gmclip_model::effective::LayerValues>> = vec![None; stack.len()];
    for ll in from {
        let Some(i) = stack.iter().enumerate().position(|(i, r)| {
            out[i].is_none() && r.values.kind == LayerKind::SourceView && r.values.role == ll.role
        }) else {
            continue;
        };
        let mut v = stack[i].values.clone();
        v.crop = ll.crop;
        v.place = ll.place;
        v.z = ll.z;
        v.opacity = ll.opacity;
        v.visible = ll.visible;
        v.frame = ll.frame.clone();
        out[i] = Some(v);
    }
    out
}
