//! Overlays: add (text, pill, counter) and move.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::anchor::Anchor;
use gmclip_model::entities::*;
use gmclip_model::geometry::{CanvasBox, Place};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::Deserialize;

use super::look::check_place;
use super::{Created, Done, created};
use crate::command::{Command, Cx};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayAddArgs {
    pub cut: Id<Cut>,
    pub kind: OverlayKind,
    pub start: Anchor,
    pub end: OverlayEnd,
    pub place: Place,
    #[serde(default)]
    pub band: Option<String>,
    #[serde(default)]
    pub text: Option<OverlayText>,
    #[serde(default)]
    pub counter: Option<Counter>,
    #[serde(default)]
    pub z: Option<i32>,
    /// Where each number on screen comes from (agent numbers without one are an
    /// error-level lint).
    #[serde(default)]
    pub number_provenance: Vec<NumberProvenance>,
    /// What the text is for, which decides how it renders: `text_hook`, `section_pill`,
    /// `run_clock` or `note`.
    #[serde(default)]
    pub role: Option<String>,
    /// An image overlay's picture (`kind: image`); `place` is its top-left corner and width.
    #[serde(default)]
    pub image: Option<OverlayImage>,
    /// A callout (`kind: shape`): a box on `rect` or an arrow from `from` to `to`.
    #[serde(default)]
    pub shape: Option<OverlayShape>,
    /// A box callout's box (fractions of the canvas).
    #[serde(default)]
    pub rect: Option<CanvasBox>,
    /// A video overlay's media (`kind: video`, decision 0028): a span of a project video
    /// or a stored video asset; `place` is its top-left corner and width.
    #[serde(default)]
    pub video: Option<OverlayVideo>,
    /// An image or video drawn among the part's picture layers (decision 0042).
    #[serde(default)]
    pub under: Option<i32>,
}

/// Checks a video overlay's media (decision 0028): an asset must be a stored video
/// with its probe; a span must lie on a project video, start before it ends and run at
/// most 10 minutes; a crop must lie inside the picture.
pub(crate) fn video_ok(v: &OverlayVideo, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match &v.media {
        AssetOrSpan::Asset(a) => {
            let a = cx.tx.get(*a)?;
            let probed = a
                .probe
                .is_some_and(|p| p.fps.is_some_and(|r| r.num > 0) && p.frames.unwrap_or(0) > 0);
            if !a.media_type.starts_with("video/") || !probed {
                return Err(CmdError::invalid(
                    "/video/media",
                    "a stored video asset (asset_add with its probe)",
                ));
            }
        }
        AssetOrSpan::Span {
            media,
            src_in,
            src_out,
        } => {
            match media {
                MediaRef::Source(s) => {
                    cx.tx.get(*s)?;
                }
                MediaRef::Group(g) => {
                    cx.tx.get(*g)?;
                }
            }
            if src_in.0 < 0 || src_out.0 <= src_in.0 || src_out.0 - src_in.0 > 600 * FLICKS {
                return Err(CmdError::invalid(
                    "/video/media",
                    "a span that starts before it ends, at most 10 minutes",
                ));
            }
        }
    }
    if let Some(l) = &v.lock {
        if !matches!(v.media, AssetOrSpan::Span { .. }) {
            return Err(CmdError::invalid(
                "/video/lock",
                "a locked overlay plays a span of a video",
            ));
        }
        match l.media {
            MediaRef::Source(s) => {
                cx.tx.get(s)?;
            }
            MediaRef::Group(_) => {
                return Err(CmdError::invalid(
                    "/video/lock/media",
                    "a video (a source), not a sync group",
                ));
            }
        }
    }
    match &v.matte {
        None => {}
        Some(Matte::Roto { model, soften, .. })
            if super::matte::ROTO_MODELS.contains(&model.as_str())
                && (0.0..=32.0).contains(soften) => {}
        Some(_) => {
            return Err(CmdError::invalid(
                "/video/matte",
                "a roto cut-out (apple_vision or packed_alpha, soften 0–32)",
            ));
        }
    }
    if let Some(c) = &v.crop {
        let ok = [c.x, c.y, c.w, c.h].iter().all(|v| v.is_finite())
            && c.x >= 0.0
            && c.y >= 0.0
            && c.w > 0.0
            && c.h > 0.0
            && c.x + c.w <= 1.0 + 1e-9
            && c.y + c.h <= 1.0 + 1e-9;
        if !ok {
            return Err(CmdError::invalid("/video/crop", "a box inside the picture"));
        }
    }
    Ok(())
}

/// Flicks per second.
const FLICKS: i64 = 705_600_000;

fn rgba_ok(c: &str, path: &str) -> Result<(), CmdError> {
    gmclip_plan::scalars::Rgba::new(c.to_owned())
        .map(|_| ())
        .map_err(|_| CmdError::invalid(path, "#RRGGBBAA"))
}

/// Checks a callout and its box.
pub(crate) fn shape_ok(s: &OverlayShape, rect: Option<&CanvasBox>) -> Result<(), CmdError> {
    rgba_ok(&s.stroke, "/shape/stroke")?;
    if let Some(f) = &s.fill {
        rgba_ok(f, "/shape/fill")?;
    }
    let len = |v: f64| v.is_finite() && (0.0..=200.0).contains(&v);
    if !len(s.width) || !len(s.radius) {
        return Err(CmdError::invalid("/shape/width", "0–200 short-side units"));
    }
    let pt = |p: Option<[f64; 2]>| {
        p.is_some_and(|p| p.iter().all(|v| v.is_finite() && (-0.5..=1.5).contains(v)))
    };
    match s.kind {
        ShapeKind::Arrow => {
            if !pt(s.from) || !pt(s.to) || s.width <= 0.0 {
                return Err(CmdError::invalid(
                    "/shape",
                    "an arrow needs from, to (canvas fractions) and a width",
                ));
            }
        }
        ShapeKind::Box => {
            let ok = rect.is_some_and(|r| {
                [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite())
                    && r.w > 0.0
                    && r.h > 0.0
                    && r.w <= 2.0
                    && r.h <= 2.0
            });
            if !ok {
                return Err(CmdError::invalid("/rect", "a box callout needs its box"));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct OverlayAdd;

#[command(
    name = "overlay_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add an overlay to a cut between two anchors: text (hook, pill, note), a counter, an image (`image`: an asset_add asset and its size; `place` = top-left and width), a callout (`shape`: a box on `rect` or an arrow from → to, canvas fractions) or a video picture-in-picture (`video`: a span of a project video or a video asset, silent; `place` = top-left and width). Images and videos draw above the whole picture unless `under` puts them among its layers (decision 0042): `under` = a layer's z draws it just under that layer, so a full-frame graphic under a camera layer keeps the person on top (overlay_stack_set changes it later).",
    example = r#"{"cut": "01J00000000000000000000001", "kind": "text", "start": {"cut_start": {"cut": "01J00000000000000000000001"}}, "end": {"dur_ms": 4500}, "place": {"x": 0.5, "y": 0.16, "w": 0.9}, "band": "hook"}"#
)]
impl Command for OverlayAdd {
    type Args = OverlayAddArgs;
    type Output = Created;
    fn run(a: OverlayAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        cx.tx.get(a.cut)?;
        check_place(&a.place, "/place")?;
        if let Some(b) = &a.band {
            super::bounded(b, "/band", 64)?;
        }
        if let Some(t) = &a.text {
            let n: usize = t.runs.iter().map(|r| r.text.chars().count()).sum();
            if n > 4_000 || t.runs.len() > 200 {
                return Err(CmdError::invalid("/text", "too long"));
            }
        }
        if a.kind == OverlayKind::Image {
            let im = a
                .image
                .ok_or_else(|| CmdError::invalid("/image", "an image overlay needs its image"))?;
            if im.w == 0 || im.h == 0 || im.w > 8192 || im.h > 8192 {
                return Err(CmdError::invalid("/image", "1–8192 px a side"));
            }
            cx.tx.get(im.asset)?;
        }
        if a.kind == OverlayKind::Shape {
            let sh = a
                .shape
                .as_ref()
                .ok_or_else(|| CmdError::invalid("/shape", "a shape overlay needs its shape"))?;
            shape_ok(sh, a.rect.as_ref())?;
        }
        if a.kind == OverlayKind::Video {
            let v = a
                .video
                .as_ref()
                .ok_or_else(|| CmdError::invalid("/video", "a video overlay needs its video"))?;
            video_ok(v, cx)?;
        }
        if a.kind == OverlayKind::Counter && a.counter.is_none() {
            return Err(CmdError::invalid(
                "/counter",
                "a counter overlay needs its counter",
            ));
        }
        // R-ANC-1: the anchors resolve now (the caller learns of a bad one at once)
        let _ = cx.tx.children::<Part>(a.cut.entity());
        crate::anchors::resolve_in(
            cx.tx.peek(),
            Some(cx.analysis),
            Some(a.cut),
            &a.start,
            "/start",
        )?;
        if let OverlayEnd::At(e) = &a.end {
            crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), Some(a.cut), e, "/end")?;
        }
        provenance_ok(&a.number_provenance, cx)?;
        if let Some(r) = &a.role
            && !["text_hook", "section_pill", "run_clock", "note"].contains(&r.as_str())
        {
            return Err(CmdError::invalid(
                "/role",
                "text_hook, section_pill, run_clock or note",
            ));
        }
        let id: Id<Overlay> = cx.tx.mint();
        cx.tx.create(Overlay {
            id,
            parent: OverlayParent::Cut(a.cut),
            kind: a.kind,
            start: a.start,
            end: a.end,
            band: a.band,
            z: a.z.unwrap_or(match a.kind {
                OverlayKind::Image | OverlayKind::Video => 10,
                _ => 20,
            }),
            opacity: 1.0,
            geom: match (a.kind, a.rect) {
                (OverlayKind::Shape, Some(r)) => OverlayGeom::Box(r),
                _ => OverlayGeom::Place(a.place),
            },
            per_framing: BTreeMap::new(),
            span_mode: SpanMode::ClampToStretch,
            text: a.text,
            counter: a.counter,
            role: a.role,
            number_provenance: a.number_provenance,
            image: a.image.filter(|_| a.kind == OverlayKind::Image),
            shape: a.shape.filter(|_| a.kind == OverlayKind::Shape),
            video: a.video.filter(|_| a.kind == OverlayKind::Video),
            track: None,
            under: a
                .under
                .filter(|_| matches!(a.kind, OverlayKind::Image | OverlayKind::Video)),
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayMoveArgs {
    pub overlay: Id<Overlay>,
    pub place: Place,
    /// Move it in this framing only; else move the base place.
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Debug)]
pub struct OverlayMove;

#[command(
    name = "overlay_move",
    scope = "edit",
    undo = "undoable",
    doc = "Move an overlay: its base place, or its place in one framing.",
    example = r#"{"overlay": "01J00000000000000000000008", "place": {"x": 0.044, "y": 0.136, "w": 0.6}}"#
)]
impl Command for OverlayMove {
    type Args = OverlayMoveArgs;
    type Output = Done;
    fn run(a: OverlayMoveArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        check_place(&a.place, "/place")?;
        let cut = match cx.tx.get(a.overlay)?.parent {
            OverlayParent::Cut(c) => c,
            OverlayParent::Clip(c) => cx.tx.get(c)?.cut,
        };
        if let Some(f) = a.framing
            && cx.tx.get(f)?.cut != cut
        {
            return Err(CmdError::invalid(
                "/framing",
                "not a framing of the overlay's cut",
            ));
        }
        cx.tx.update(a.overlay, |o| match a.framing {
            Some(f) => {
                let pf = o.per_framing.entry(f).or_insert(OverlayPerFraming {
                    visible: true,
                    place: None,
                });
                pf.place = Some(a.place);
            }
            None => o.geom = OverlayGeom::Place(a.place),
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayStackSetArgs {
    pub overlay: Id<Overlay>,
    /// Drawn just under the first picture layer whose `z` is at least this (the lowest
    /// layer's `z` or less = behind the whole picture); null = above the picture.
    pub under: Option<i32>,
}

#[derive(Debug)]
pub struct OverlayStackSet;

#[command(
    name = "overlay_stack_set",
    scope = "edit",
    undo = "undoable",
    doc = "Put an image or video overlay among the part's picture layers (decision 0042): `under` = a layer's z draws it just under that layer (e.g. behind a cut-out speaker), the lowest layer's z puts it behind the whole picture; null brings it back above the picture.",
    example = r#"{"overlay": "01J00000000000000000000008", "under": 1}"#
)]
impl Command for OverlayStackSet {
    type Args = OverlayStackSetArgs;
    type Output = Done;
    fn run(a: OverlayStackSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let o = cx.tx.get(a.overlay)?;
        if !matches!(o.kind, OverlayKind::Image | OverlayKind::Video) {
            return Err(CmdError::invalid("/overlay", "an image or video overlay"));
        }
        cx.tx.update(a.overlay, |o| o.under = a.under)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlaySplitArgs {
    pub overlay: Id<Overlay>,
    /// Where to cut it: an anchor inside its span. On an edge between two parts the
    /// pieces anchor to those parts' edges; elsewhere both to `at` (a raw source time
    /// anchors to the part that plays there).
    pub at: Anchor,
}

#[derive(Debug)]
pub struct OverlaySplit;

#[command(
    name = "overlay_split",
    scope = "edit",
    undo = "undoable",
    doc = "Cut an overlay in two at an anchor inside its span: the first piece keeps the id and ends there, the second (a new id) starts there with the same look; a video span's second piece plays on from where the cut fell. Cut on an edge between parts, each piece anchors to its own part, so it moves with that part when parts are reordered (overlay_split_parts cuts at every edge). A looping video, a video asset (always played from its first frame) and an overlay with keyframes are refused. One undo step.",
    example = r#"{"overlay": "01J00000000000000000000008", "at": {"part_start": {"part": "01J00000000000000000000003"}}}"#
)]
impl Command for OverlaySplit {
    type Args = OverlaySplitArgs;
    type Output = Created;
    fn run(a: OverlaySplitArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let (o, cut) = splittable(a.overlay, cx)?;
        let (t0, t1) = overlay_span(&o, cut, cx)?;
        let t = cut_time(cx, cut, &a.at, "/at")?;
        if t <= t0 || t >= t1 {
            return Err(CmdError::invalid("/at", "a time inside the overlay's span"));
        }
        let edges = part_edges(cx, cut);
        let (end, start) = match (
            edges.iter().find(|e| e.2 == t),
            edges.iter().find(|e| e.1 == t),
        ) {
            (Some(l), Some(r)) => (part_end(l.0), part_start(r.0)),
            _ => (a.at.clone(), a.at),
        };
        let ids = cut_pieces(cx, o, t0, vec![(t, end, start)], None, None)?;
        Ok(created(ids[1]))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlaySplitPartsArgs {
    pub overlay: Id<Overlay>,
}

/// The pieces, in play order (the first keeps the overlay's id).
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct Pieces {
    pub pieces: Vec<String>,
}

#[derive(Debug)]
pub struct OverlaySplitParts;

#[command(
    name = "overlay_split_parts",
    scope = "edit",
    undo = "undoable",
    doc = "Cut an overlay at every edge between the parts it spans, one piece per part, each anchored to its own part's edges, so the pieces move with their parts when parts are reordered or trimmed; a video span's pieces play on from where each cut fell. Refused as overlay_split refuses. One undo step.",
    example = r#"{"overlay": "01J00000000000000000000008"}"#
)]
impl Command for OverlaySplitParts {
    type Args = OverlaySplitPartsArgs;
    type Output = Pieces;
    fn run(a: OverlaySplitPartsArgs, cx: &mut Cx<'_>) -> Result<Pieces, CmdError> {
        let (o, cut) = splittable(a.overlay, cx)?;
        let (t0, t1) = overlay_span(&o, cut, cx)?;
        let edges = part_edges(cx, cut);
        let cuts: Vec<_> = edges
            .windows(2)
            .filter(|w| w[0].2 == w[1].1 && t0 < w[0].2 && w[0].2 < t1)
            .map(|w| (w[0].2, part_end(w[0].0), part_start(w[1].0)))
            .collect();
        // its own ends onto the part edges they sit on (a cut's start or end included)
        let first = edges.iter().find(|e| e.1 == t0).map(|e| part_start(e.0));
        let last = edges.iter().find(|e| e.2 == t1).map(|e| part_end(e.0));
        let ids = cut_pieces(cx, o, t0, cuts, first, last)?;
        Ok(Pieces {
            pieces: ids.iter().map(|i| i.0.to_string()).collect(),
        })
    }
}

fn part_start(part: Id<Part>) -> Anchor {
    Anchor::PartStart(gmclip_model::anchor::PartEdge {
        part,
        offset_ms: None,
    })
}

fn part_end(part: Id<Part>) -> Anchor {
    Anchor::PartEnd(gmclip_model::anchor::PartEdge {
        part,
        offset_ms: None,
    })
}

/// An overlay that can be cut, and its cut.
fn splittable(id: Id<Overlay>, cx: &mut Cx<'_>) -> Result<(Overlay, Id<Cut>), CmdError> {
    let o = cx.tx.get(id)?.clone();
    let OverlayParent::Cut(cut) = o.parent else {
        return Err(CmdError::invalid("/overlay", "an overlay on a cut"));
    };
    if !cx.tx.children::<Keyframe>(o.id.entity()).is_empty() {
        return Err(CmdError::invalid(
            "/overlay",
            "an overlay without keyframes",
        ));
    }
    if let Some(v) = &o.video {
        if matches!(v.media, AssetOrSpan::Asset(_)) {
            return Err(CmdError::invalid(
                "/overlay",
                "a video span (a video asset always plays from its first frame)",
            ));
        }
        if v.r#loop {
            return Err(CmdError::invalid("/overlay", "a video that does not loop"));
        }
    }
    let _ = cx.tx.children::<Part>(cut.entity());
    Ok((o, cut))
}

/// `a` on the cut's clock (flicks).
fn cut_time(cx: &Cx<'_>, cut: Id<Cut>, a: &Anchor, path: &str) -> Result<i64, CmdError> {
    crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), Some(cut), a, path)?
        .cut_t
        .map(|t| t.0)
        .ok_or_else(|| CmdError::invalid(path, "a time inside the cut"))
}

/// The overlay's span on the cut's clock.
fn overlay_span(o: &Overlay, cut: Id<Cut>, cx: &Cx<'_>) -> Result<(i64, i64), CmdError> {
    let t0 = cut_time(cx, cut, &o.start, "/overlay")?;
    let t1 = match &o.end {
        OverlayEnd::At(e) => cut_time(cx, cut, e, "/overlay")?,
        OverlayEnd::DurMs(ms) => t0 + i64::from(*ms) * FLICKS / 1000,
    };
    Ok((t0, t1))
}

/// The cut's parts and their spans on its clock, in play order.
fn part_edges(cx: &Cx<'_>, cut: Id<Cut>) -> Vec<(Id<Part>, i64, i64)> {
    let mut v: Vec<_> = cx
        .tx
        .peek()
        .children::<Part>(cut.entity())
        .filter_map(|p| {
            let a = cut_time(cx, cut, &part_start(p), "/").ok()?;
            let b = cut_time(cx, cut, &part_end(p), "/").ok()?;
            Some((p, a, b))
        })
        .collect();
    v.sort_by_key(|e| e.1);
    v
}

/// Cuts `o` (starting at `t0` on the cut's clock) at each `(t, end of the piece before,
/// start of the piece after)`, in order; `first` / `last` re-anchor its own ends. A video
/// span's pieces each start their media where the cut fell.
fn cut_pieces(
    cx: &mut Cx<'_>,
    o: Overlay,
    t0: i64,
    cuts: Vec<(i64, Anchor, Anchor)>,
    first: Option<Anchor>,
    last: Option<Anchor>,
) -> Result<Vec<Id<Overlay>>, CmdError> {
    if let (Some(v), Some(c)) = (&o.video, cuts.last())
        && let AssetOrSpan::Span {
            src_in, src_out, ..
        } = &v.media
        && src_in.0 + (c.0 - t0) >= src_out.0
    {
        return Err(CmdError::invalid("/at", "a time before the video ends"));
    }
    let mut ids = vec![o.id];
    // each piece ends where the next starts; the last where the overlay ended (a
    // duration: what is left of it after the last cut)
    let tail = match (last, &o.end) {
        (Some(l), _) => OverlayEnd::At(l),
        (None, OverlayEnd::DurMs(ms)) => {
            let used = cuts.last().map_or(0, |c| (c.0 - t0) * 1000 / FLICKS);
            OverlayEnd::DurMs(ms.saturating_sub(u32::try_from(used).unwrap_or(u32::MAX)))
        }
        (None, e) => e.clone(),
    };
    let ends: Vec<OverlayEnd> = cuts
        .iter()
        .map(|c| OverlayEnd::At(c.1.clone()))
        .chain([tail])
        .collect();
    cx.tx.update(o.id, |x| {
        if let Some(f) = first {
            x.start = f;
        }
        x.end = ends[0].clone();
    })?;
    for (i, (t, _, start)) in cuts.into_iter().enumerate() {
        let mut video = o.video.clone();
        if let Some(v) = &mut video
            && let AssetOrSpan::Span { src_in, .. } = &mut v.media
        {
            src_in.0 += t - t0;
        }
        let id: Id<Overlay> = cx.tx.mint();
        cx.tx.create(Overlay {
            id,
            start,
            end: ends[i + 1].clone(),
            video,
            ..o.clone()
        })?;
        ids.push(id);
    }
    Ok(ids)
}

/// Bounds and checks number provenance (each named measurement must exist).
pub(crate) fn provenance_ok(p: &[NumberProvenance], cx: &mut Cx<'_>) -> Result<(), CmdError> {
    if p.len() > 32 {
        return Err(CmdError::invalid("/number_provenance", "at most 32"));
    }
    for x in p {
        super::bounded(&x.text, "/number_provenance/text", 64)?;
        if let Some(n) = &x.note {
            super::bounded(n, "/number_provenance/note", 400)?;
        }
        if let Some(m) = x.measurement {
            cx.tx.get(m)?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayTrackSetArgs {
    pub overlay: Id<Overlay>,
    /// The moving point to pin it to; null unpins it.
    #[serde(default)]
    pub track: Option<OverlayTrack>,
}

#[derive(Debug)]
pub struct OverlayTrackSet;

#[command(
    name = "overlay_track_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Pin an overlay (a label, picture or callout) to a moving point: keys of the point on a video's picture (normalised, origin top left) at times on that video's clock, as a tracker found them (the app's \"Pin to motion\" makes them with Apple Vision). The overlay is drawn moved by the point's move since its start. null unpins it. One undo step.",
    example = r#"{"overlay": "01J00000000000000000000005", "track": {"media": {"source": "01J00000000000000000000002"}, "keys": [{"t": 70560000000, "x": 0.4, "y": 0.5}, {"t": 70583520000, "x": 0.42, "y": 0.5}], "method": "apple_vision.track_object"}}"#
)]
impl Command for OverlayTrackSet {
    type Args = OverlayTrackSetArgs;
    type Output = Done;
    fn run(a: OverlayTrackSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let o = cx.tx.get(a.overlay)?;
        if o.kind == OverlayKind::Video || o.kind == OverlayKind::Counter {
            return Err(CmdError::invalid(
                "/overlay",
                "a text, picture or callout overlay",
            ));
        }
        if let Some(t) = &a.track {
            check_track(t, cx)?;
        }
        cx.tx.update(a.overlay, |x| x.track = a.track)?;
        Ok(Done {})
    }
}

/// A track's media exists, its keys increase and its points are near the picture.
fn check_track(t: &OverlayTrack, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match t.media {
        MediaRef::Source(s) => {
            cx.tx.get(s)?;
        }
        MediaRef::Group(g) => {
            cx.tx.get(g)?;
        }
    }
    if t.keys.is_empty() || t.keys.len() > 100_000 {
        return Err(CmdError::invalid("/track/keys", "1–100 000 keys"));
    }
    let ok = t.keys.windows(2).all(|w| w[0].t < w[1].t)
        && t.keys.iter().all(|k| {
            k.t.0 >= 0
                && k.x.is_finite()
                && k.y.is_finite()
                && (-0.5..=1.5).contains(&k.x)
                && (-0.5..=1.5).contains(&k.y)
        });
    if !ok {
        return Err(CmdError::invalid(
            "/track/keys",
            "times increasing, points near the picture",
        ));
    }
    super::bounded(&t.method, "/track/method", 64)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerTrackSetArgs {
    pub layer: Id<Layer>,
    /// The moving point to pin it to; null unpins it.
    #[serde(default)]
    pub track: Option<OverlayTrack>,
}

#[derive(Debug)]
pub struct LayerTrackSet;

#[command(
    name = "layer_track_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Pin a cover layer (a blur, pixelate or solid cover) to a moving point: keys of the point on a video's picture (normalised, origin top left) at times on that video's clock, as for overlay_track_set (the app's \"Pin to motion\" makes them with Apple Vision). The cover sits where its crop and place put it when the point is at the first key and moves with the point, on every part of that video the layer shows on. null unpins it. One undo step.",
    example = r#"{"layer": "01J00000000000000000000006", "track": {"media": {"source": "01J00000000000000000000002"}, "keys": [{"t": 70560000000, "x": 0.4, "y": 0.5}, {"t": 70583520000, "x": 0.42, "y": 0.5}], "method": "apple_vision.track_object"}}"#
)]
impl Command for LayerTrackSet {
    type Args = LayerTrackSetArgs;
    type Output = Done;
    fn run(a: LayerTrackSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let l = cx.tx.get(a.layer)?;
        if l.kind != LayerKind::Cover || l.cover.is_none() {
            return Err(CmdError::invalid(
                "/layer",
                "a cover layer (blur, pixelate or solid)",
            ));
        }
        if let Some(t) = &a.track {
            check_track(t, cx)?;
        }
        cx.tx.update(a.layer, |x| x.track = a.track)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetAddArgs {
    pub name: String,
    /// sha256 of the stored file (lower-case hex): how a host finds it.
    pub sha256: String,
    pub size: u64,
    /// `image/png` (overlay images and still backdrops are stored re-encoded as PNG,
    /// R-MEDIA-SEC-3), or `video/mp4` / `video/quicktime` (a backdrop or video overlay
    /// clip, decision 0028).
    pub media_type: String,
    /// What the host measured: a picture's size; a video's size, rate and frame count
    /// (required for a video).
    #[serde(default)]
    pub probe: Option<AssetProbe>,
}

#[derive(Debug)]
pub struct AssetAdd;

#[command(
    name = "asset_add",
    scope = "edit",
    undo = "undoable",
    doc = "Register a stored asset by name, sha256 and size: an overlay image or still backdrop (re-encoded as PNG by the host), or a video (MP4/MOV, with its probe) for a backdrop or a video overlay, or a recording's input track (decision 0045), or music or a sound effect (audio/mp4, audio/wav; decision 0051); returns the existing asset when the same file is there.",
    example = r#"{"name": "logo.png", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "size": 18234, "media_type": "image/png"}"#
)]
impl Command for AssetAdd {
    type Args = AssetAddArgs;
    type Output = Created;
    fn run(a: AssetAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        super::bounded(&a.name, "/name", 200)?;
        if a.sha256.len() != 64
            || !a
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(CmdError::invalid("/sha256", "64 lower-case hex characters"));
        }
        let video = matches!(a.media_type.as_str(), "video/mp4" | "video/quicktime");
        // A recording's input track (decision 0045): an hour of cursor and input is a few
        // tens of MB of JSON.
        let input = a.media_type == gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE;
        // music and sound effects (decision 0051): AAC in MP4, or WAV; minutes of WAV
        // are tens of MB
        let sound = matches!(a.media_type.as_str(), "audio/mp4" | "audio/wav");
        if a.media_type != "image/png" && !video && !input && !sound {
            return Err(CmdError::invalid(
                "/media_type",
                "image/png, video/mp4, video/quicktime, audio/mp4, audio/wav or an input track",
            ));
        }
        if let Some(p) = &a.probe {
            let fps_ok = p
                .fps
                .is_none_or(|r| r.num > 0 && r.den > 0 && r.num <= 1000 * i64::from(r.den));
            if p.w == 0 || p.h == 0 || p.w > 16_384 || p.h > 16_384 || !fps_ok {
                return Err(CmdError::invalid(
                    "/probe",
                    "1–16384 px a side, a rate up to 1000 fps",
                ));
            }
        }
        if video
            && !a
                .probe
                .is_some_and(|p| p.fps.is_some() && p.frames.is_some_and(|n| n > 0))
        {
            return Err(CmdError::invalid(
                "/probe",
                "a video asset needs its size, rate and frame count",
            ));
        }
        if a.size == 0 || a.size > if input || sound { 128 << 20 } else { 20 << 20 } {
            return Err(CmdError::invalid(
                "/size",
                if input || sound {
                    "1 byte – 128 MB"
                } else {
                    "1 byte – 20 MB"
                },
            ));
        }
        let existing = cx
            .tx
            .peek()
            .iter::<Asset>()
            .find(|x| x.sha256 == a.sha256)
            .map(|x| (x.id, x.probe.is_some()));
        if let Some((id, had)) = existing {
            // the same file again, now measured (a still stored as an overlay image, then
            // chosen as a backdrop): keep the probe
            if !had && let Some(p) = a.probe {
                cx.tx.update(id, |x| x.probe = Some(p))?;
            }
            return Ok(created(id));
        }
        let id: Id<Asset> = cx.tx.mint();
        cx.tx.create(Asset {
            id,
            name: a.name,
            sha256: a.sha256,
            size: a.size,
            media_type: a.media_type,
            probe: a.probe,
        })?;
        Ok(created(id))
    }
}

// ---------------------------------------------------------------------------------------
// run_clock_make (A2.07)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunClockMakeArgs {
    /// A text overlay (its style and place are kept).
    pub overlay: Id<Overlay>,
    /// An interval measurement (`from` → `to`): the run the clock times.
    pub measurement: Id<Measurement>,
    /// Shown while the run goes, before the time (default: the overlay's text, else
    /// `RUNNING`).
    #[serde(default)]
    pub running_label: Option<String>,
    /// Shown once the run is done, before the total (default `DONE IN`).
    #[serde(default)]
    pub done_label: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct RunClockMade {
    /// The measured total as the clock shows it when done (floored `m:ss`).
    pub total: String,
    /// What the clock shows once done, e.g. `DONE IN 1:07`.
    pub shows: String,
}

/// Floored `m:ss` of a duration in seconds.
pub fn floored_mss(secs: f64) -> String {
    let s = secs.max(0.0).floor() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Turns a text overlay into a run clock tied to a measurement (R-AI-4).
#[derive(Debug)]
pub struct RunClockMake;

#[command(
    name = "run_clock_make",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Make a text overlay a run clock timed by a measurement: it counts while the measured run goes and then shows the measured total (floored m:ss), with the measurement as its number's provenance.",
    example = r#"{"overlay": "01J00000000000000000000005", "measurement": "01J0000000000000000000000M", "running_label": "GENERATING", "done_label": "DONE IN"}"#
)]
impl Command for RunClockMake {
    type Args = RunClockMakeArgs;
    type Output = RunClockMade;
    fn run(a: RunClockMakeArgs, cx: &mut Cx<'_>) -> Result<RunClockMade, CmdError> {
        for (l, path) in [
            (&a.running_label, "/running_label"),
            (&a.done_label, "/done_label"),
        ] {
            if let Some(l) = l {
                super::bounded(l, path, 40)?;
            }
        }
        let m = cx.tx.get(a.measurement)?.clone();
        if m.to.is_none() || m.kind != MeasurementKind::Interval {
            return Err(CmdError::invalid(
                "/measurement",
                "an interval measurement (from → to)",
            ));
        }
        let secs = if m.unit == "ms" {
            m.value / 1000.0
        } else {
            m.value
        };
        if !secs.is_finite() || secs < 0.0 || !(m.unit == "s" || m.unit == "ms") {
            return Err(CmdError::invalid("/measurement", "a duration in s or ms"));
        }
        let o = cx.tx.get(a.overlay)?.clone();
        let Some(text) = &o.text else {
            return Err(CmdError::invalid("/overlay", "a text overlay"));
        };
        if !matches!(o.kind, OverlayKind::Text | OverlayKind::Counter) {
            return Err(CmdError::invalid("/overlay", "a text overlay"));
        }
        let typed: String = text
            .runs
            .iter()
            .map(|r| r.text.as_str())
            .collect::<String>()
            .trim()
            .chars()
            .take(40)
            .collect();
        let running = a
            .running_label
            .map(|s| s.trim().to_owned())
            .or_else(|| o.counter.as_ref().map(|c| c.running_label.clone()))
            .or_else(|| (!typed.is_empty()).then_some(typed))
            .unwrap_or_else(|| "RUNNING".into());
        let done = a
            .done_label
            .map(|s| s.trim().to_owned())
            .or_else(|| o.counter.as_ref().map(|c| c.done_label.clone()))
            .unwrap_or_else(|| "DONE IN".into());
        let total = floored_mss(secs);
        cx.tx.update(a.overlay, |o| {
            o.kind = OverlayKind::Counter;
            o.role = Some("run_clock".into());
            if let Some(t) = o.text.as_mut() {
                t.runs.clear();
            }
            o.counter = Some(Counter {
                start: CounterStart::Measurement(a.measurement),
                stop: None,
                clock: CounterClock::Src,
                format: String::new(),
                running_label: running,
                done_label: done.clone(),
            });
            o.number_provenance = vec![NumberProvenance {
                text: total.clone(),
                measurement: Some(a.measurement),
                note: Some("run_clock_make: the measured run".into()),
                spoken: None,
            }];
        })?;
        Ok(RunClockMade {
            shows: format!("{done} {total}").trim().to_owned(),
            total,
        })
    }
}
