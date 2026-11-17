//! Sections (decision 0046): spans of a cut drawn as lanes above the parts — a zoom on
//! the screen, or a layout for a while — dragged freely across part edges.
//!
//! Callers speak cut milliseconds (`start_ms`, `end_ms`); the section stores anchors, as
//! an overlay dragged on the timeline does (the app's `lanes::overlay_time`): the cut's
//! start and end are cut-edge anchors, any other time a raw source-clock anchor on the
//! media of the part that plays it (an end on the part that plays the millisecond before
//! it), so trims and reorders carry a section as they carry overlays. `section_list`
//! answers them in cut milliseconds again.

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, CutEdge, RawTime};
use gmclip_model::clock::{self, FLICKS_PER_MS, ModelTime};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Created, Done, created};
use crate::command::{Command, Cx};

/// Sections one cut may hold.
pub const MAX_SECTIONS: usize = 2000;
/// The reason stored on a section's raw anchors.
pub const SECTION_REASON: &str = "section placed on the timeline";

/// The cut's length in flicks.
fn cut_len(doc: &Doc, cut: Id<Cut>) -> i64 {
    clock::cut_layout(doc, cut).iter().map(|s| s.dur.0).sum()
}

/// Rounds flicks to the nearest ms.
fn to_ms(f: i64) -> i64 {
    (f + FLICKS_PER_MS / 2).div_euclid(FLICKS_PER_MS)
}

/// A raw source-clock anchor at `t` on `media`'s clock.
fn raw(media: MediaRef, t: Flicks) -> Anchor {
    let time = match media {
        MediaRef::Source(s) => ModelTime::Src {
            source: Some(s),
            group: None,
            t,
        },
        MediaRef::Group(g) => ModelTime::Src {
            source: None,
            group: Some(g),
            t,
        },
    };
    Anchor::Raw(RawTime {
        time,
        reason: SECTION_REASON.into(),
    })
}

/// The anchor for cut millisecond `ms` (`end`: an exclusive end, anchored on the part
/// that plays the millisecond before it). The cut's start and end are cut edges.
pub fn anchor_at_ms(doc: &Doc, cut: Id<Cut>, ms: i64, end: bool) -> Option<Anchor> {
    let total = cut_len(doc, cut);
    let t = clock::ms_to_flicks(ms).0;
    if !end && t <= 0 {
        return Some(Anchor::CutStart(CutEdge {
            cut,
            offset_ms: None,
        }));
    }
    if end && t >= total {
        return Some(Anchor::CutEnd(CutEdge {
            cut,
            offset_ms: None,
        }));
    }
    if t <= 0 || t >= total {
        return None;
    }
    if end {
        let (pid, src) = clock::cut_to_src(doc, cut, Flicks(t - FLICKS_PER_MS))?;
        let p = doc.get(pid)?;
        Some(raw(
            p.media,
            Flicks((src.0 + FLICKS_PER_MS).min(p.src_out.0)),
        ))
    } else {
        let (pid, src) = clock::cut_to_src(doc, cut, Flicks(t))?;
        Some(raw(doc.get(pid)?.media, src))
    }
}

/// Where `a` falls on `cut`'s clock, in ms (the planner's reading: the anchor's source
/// time, then the first part, in order, that plays it); `None` when no part plays it.
pub fn ms_of(doc: &Doc, cut: Id<Cut>, a: &Anchor) -> Option<i64> {
    match a {
        Anchor::CutStart(e) if e.cut == cut && e.offset_ms.unwrap_or(0) == 0 => return Some(0),
        Anchor::CutEnd(e) if e.cut == cut && e.offset_ms.unwrap_or(0) == 0 => {
            return Some(to_ms(cut_len(doc, cut)));
        }
        _ => {}
    }
    let r = crate::anchors::resolve(doc, Some(cut), a, "/anchor").ok()?;
    Some(to_ms(r.cut_t?.0))
}

/// Checks a section's kind against the cut.
fn kind_ok(k: &SectionKind, cut: Id<Cut>, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match k {
        SectionKind::Zoom(z) => {
            if !(z.depth.is_finite() && (1.0..=6.0).contains(&z.depth)) {
                return Err(CmdError::invalid("/kind/zoom/depth", "1–6"));
            }
            if z.ease_in_ms > 10_000 || z.ease_out_ms > 10_000 {
                return Err(CmdError::invalid(
                    "/kind/zoom",
                    "ease_in_ms and ease_out_ms 0–10 000 ms",
                ));
            }
            if let ZoomTarget::Point { x, y } = z.target
                && !([x, y]
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
            {
                return Err(CmdError::invalid(
                    "/kind/zoom/point",
                    "target point x and y 0–1 (in the layer's crop)",
                ));
            }
            if let Some(s) = z.camera_scale
                && !(s.is_finite() && (0.3..=1.0).contains(&s))
            {
                return Err(CmdError::invalid("/kind/zoom", "camera_scale 0.3–1"));
            }
        }
        SectionKind::Layout { layout, ease_ms } => {
            let l = cx.tx.get(*layout)?;
            if l.cut.is_some_and(|c| c != cut) {
                return Err(CmdError::invalid(
                    "/kind/layout",
                    "a layout of this cut, the project or the library",
                ));
            }
            if *ease_ms > 10_000 {
                return Err(CmdError::invalid("/kind/layout", "ease_ms 0–10 000 ms"));
            }
        }
    }
    Ok(())
}

/// The anchors for `[start_ms, end_ms)` on `cut`.
fn span_anchors(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    start_ms: i64,
    end_ms: i64,
) -> Result<(Anchor, Anchor), CmdError> {
    // the cut's parts are read
    let _ = cx.tx.children::<Part>(cut.entity());
    let doc = cx.tx.peek();
    let total = to_ms(cut_len(doc, cut));
    if total <= 0 {
        return Err(CmdError::invalid("/cut", "the cut has no parts"));
    }
    if start_ms < 0 || start_ms >= total {
        return Err(CmdError::invalid("/start_ms", "inside the cut"));
    }
    if end_ms <= start_ms {
        return Err(CmdError::invalid("/end_ms", "after start_ms"));
    }
    let a = anchor_at_ms(doc, cut, start_ms, false)
        .ok_or_else(|| CmdError::invalid("/start_ms", "inside the cut"))?;
    let z = anchor_at_ms(doc, cut, end_ms.min(total), true)
        .ok_or_else(|| CmdError::invalid("/end_ms", "inside the cut"))?;
    Ok((a, z))
}

fn framing_ok(cx: &mut Cx<'_>, cut: Id<Cut>, f: Option<Id<Framing>>) -> Result<(), CmdError> {
    if let Some(f) = f
        && cx.tx.get(f)?.cut != cut
    {
        return Err(CmdError::invalid("/framing", "not a framing of this cut"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// section_add
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectionAddArgs {
    pub cut: Id<Cut>,
    /// The framing it applies to (absent: every framing of the cut).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// Cut milliseconds, `[start_ms, end_ms)`; an end past the cut is the cut's end.
    pub start_ms: i64,
    pub end_ms: i64,
    /// `{"zoom": {depth?, target?, ease_in_ms?, ease_out_ms?, layer?, camera_scale?,
    /// motion_blur?, auto?}}` or `{"layout": {layout, ease_ms?}}`.
    pub kind: SectionKind,
}

#[derive(Debug)]
pub struct SectionAdd;

#[command(
    name = "section_add",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Add a section to a cut over cut milliseconds [start_ms, end_ms) (decision 0046), drawn as a lane above the parts and free to cross part edges: a zoom (`{\"zoom\": {depth 1–6 (2), target \"cursor\" | \"focus\" | {\"point\": {x, y}} (cursor), ease_in_ms (700), ease_out_ms (700), layer \"screen\" | \"cursor\" | \"all\" (screen), camera_scale 0.3–1?, motion_blur (false)}}`: the layer is drawn through a crop depth× tighter, centred on the recorded cursor, the caret/focused field, or a fixed point) or a layout (`{\"layout\": {layout, ease_ms (400)}}`). The times are anchored as a dragged overlay's are, so trims and reorders carry it. One undo step.",
    example = r#"{"cut": "01J00000000000000000000001", "start_ms": 1200, "end_ms": 4800, "kind": {"zoom": {"depth": 2, "target": "cursor"}}}"#
)]
impl Command for SectionAdd {
    type Args = SectionAddArgs;
    type Output = Created;
    fn run(a: SectionAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        cx.tx.get(a.cut)?;
        framing_ok(cx, a.cut, a.framing)?;
        kind_ok(&a.kind, a.cut, cx)?;
        if cx.tx.children::<Section>(a.cut.entity()).len() >= MAX_SECTIONS {
            return Err(CmdError::invalid("/cut", "at most 2000 sections"));
        }
        let (start, end) = span_anchors(cx, a.cut, a.start_ms, a.end_ms)?;
        let id: Id<Section> = cx.tx.mint();
        cx.tx.create(Section {
            id,
            cut: a.cut,
            framing: a.framing,
            start,
            end,
            kind: a.kind,
        })?;
        Ok(created(id))
    }
}

// ---------------------------------------------------------------------------------------
// section_set
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectionSetArgs {
    pub section: Id<Section>,
    /// New start, cut ms (absent: kept).
    #[serde(default)]
    pub start_ms: Option<i64>,
    /// New end, cut ms (absent: kept).
    #[serde(default)]
    pub end_ms: Option<i64>,
    /// The whole new kind (absent: kept).
    #[serde(default)]
    pub kind: Option<SectionKind>,
}

#[derive(Debug)]
pub struct SectionSet;

#[command(
    name = "section_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Change a section (decision 0046): its span in cut milliseconds (either end alone for an edge drag) and/or its whole kind. Moving an automatic zoom (zoom_auto's) without giving a kind makes it hand-made, so zoom_auto leaves it alone. One undo step.",
    example = r#"{"section": "01J0000000000000000000000S", "start_ms": 1500, "end_ms": 5200}"#
)]
impl Command for SectionSet {
    type Args = SectionSetArgs;
    type Output = Done;
    fn run(a: SectionSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let s = cx.tx.get(a.section)?.clone();
        if let Some(k) = &a.kind {
            kind_ok(k, s.cut, cx)?;
        }
        let moved = a.start_ms.is_some() || a.end_ms.is_some();
        let span = if moved {
            let doc = cx.tx.peek();
            let start = match a.start_ms {
                Some(v) => v,
                None => ms_of(doc, s.cut, &s.start).ok_or_else(|| {
                    CmdError::invalid("/start_ms", "the start no longer resolves: give it")
                })?,
            };
            let end = match a.end_ms {
                Some(v) => v,
                None => ms_of(doc, s.cut, &s.end).ok_or_else(|| {
                    CmdError::invalid("/end_ms", "the end no longer resolves: give it")
                })?,
            };
            Some(span_anchors(cx, s.cut, start, end)?)
        } else {
            None
        };
        cx.tx.update(a.section, |x| {
            if let Some((start, end)) = span {
                // keep the stored anchor of an edge that did not move
                if a.start_ms.is_some() {
                    x.start = start;
                }
                if a.end_ms.is_some() {
                    x.end = end;
                }
            }
            match a.kind {
                Some(k) => x.kind = k,
                None if moved => {
                    if let SectionKind::Zoom(z) = &mut x.kind {
                        z.auto = false;
                    }
                }
                None => {}
            }
        })?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// section_remove
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectionRemoveArgs {
    pub section: Id<Section>,
}

#[derive(Debug)]
pub struct SectionRemove;

#[command(
    name = "section_remove",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Remove a section (decision 0046). One undo step.",
    example = r#"{"section": "01J0000000000000000000000S"}"#
)]
impl Command for SectionRemove {
    type Args = SectionRemoveArgs;
    type Output = Done;
    fn run(a: SectionRemoveArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.section)?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// section_list
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectionListArgs {
    pub cut: Id<Cut>,
}

/// One section as the timeline draws it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SectionView {
    pub id: String,
    /// Absent: every framing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<String>,
    /// Cut ms; null when the anchor no longer lands in the cut (its media was cut out).
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub kind: SectionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SectionsListed {
    /// By start (unplaced last).
    pub sections: Vec<SectionView>,
    /// The cut's length, ms.
    pub cut_ms: i64,
}

#[derive(Debug)]
pub struct SectionList;

#[command(
    name = "section_list",
    scope = "read",
    undo = "read_only",
    since = "M2",
    doc = "List a cut's sections (decision 0046) by start: id, framing (absent: every framing), start_ms/end_ms on the cut's clock (null when the anchored media is no longer in the cut) and kind; with the cut's length in ms.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for SectionList {
    type Args = SectionListArgs;
    type Output = SectionsListed;
    fn run(a: SectionListArgs, cx: &mut Cx<'_>) -> Result<SectionsListed, CmdError> {
        cx.tx.get(a.cut)?;
        let ids = cx.tx.children::<Section>(a.cut.entity());
        let _ = cx.tx.children::<Part>(a.cut.entity());
        let doc = cx.tx.peek();
        let mut sections: Vec<SectionView> = ids
            .iter()
            .filter_map(|id| doc.get(*id))
            .map(|s| SectionView {
                id: s.id.0.to_string(),
                framing: s.framing.map(|f| f.0.to_string()),
                start_ms: ms_of(doc, a.cut, &s.start),
                end_ms: ms_of(doc, a.cut, &s.end),
                kind: s.kind.clone(),
            })
            .collect();
        sections.sort_by(|x, y| {
            (x.start_ms.is_none(), x.start_ms, &x.id).cmp(&(
                y.start_ms.is_none(),
                y.start_ms,
                &y.id,
            ))
        });
        Ok(SectionsListed {
            sections,
            cut_ms: to_ms(cut_len(doc, a.cut)),
        })
    }
}

// ---------------------------------------------------------------------------------------
// zoom_auto (host-answered) and section_auto_apply
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoomAutoArgs {
    pub cut: Id<Cut>,
    /// The framing whose screen layer's crop it reads, and that the sections apply to
    /// (absent: the cut's first framing is read, and the sections apply to every framing).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// How far in a caret or click zoom goes (1.2–4, default 2).
    #[serde(default)]
    pub max_zoom: Option<f64>,
    /// Preview only (default true); false replaces the earlier automatic zooms as one
    /// transaction under `base`.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug)]
pub struct ZoomAuto;

#[command(
    name = "zoom_auto",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Turn a gmclip recording's input log (decision 0045) into zoom sections (decision 0046) by the zoom-to-focus rules: typing zooms on the caret or the focused field (target focus), a click on the click (target cursor), out after 2.5 s without either. Replaces the cut's earlier automatic zooms of that framing and leaves hand-made sections alone. Previews by default (dry_run), applies as one transaction. Parts whose screen has no input log are skipped.",
    example = r#"{"cut": "01J00000000000000000000001", "max_zoom": 2, "dry_run": true}"#
)]
impl Command for ZoomAuto {
    type Args = ZoomAutoArgs;
    type Output = Done;
    fn run(_: ZoomAutoArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(CmdError::Unsupported {
            platform: "core".into(),
            reason: "answered by the gmclip host (it reads the input log): call it through MCP or the app"
                .into(),
        })
    }
}

/// One section `section_auto_apply` writes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoSection {
    pub start_ms: i64,
    pub end_ms: i64,
    pub zoom: ZoomSpec,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectionAutoApplyArgs {
    pub cut: Id<Cut>,
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// The new automatic zooms (each marked `auto`); empty removes them.
    pub sections: Vec<AutoSection>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AutoApplied {
    /// The sections written.
    pub ids: Vec<String>,
    /// Earlier automatic zooms removed.
    pub removed: usize,
}

#[derive(Debug)]
pub struct SectionAutoApply;

#[command(
    name = "section_auto_apply",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Apply a zoom_auto result as one transaction (internal: run zoom_auto): the cut's automatic zoom sections of that framing are replaced by these.",
    example = r#"{"cut": "01J00000000000000000000001", "sections": [{"start_ms": 1000, "end_ms": 4200, "zoom": {"depth": 2, "target": "focus", "auto": true}}]}"#
)]
impl Command for SectionAutoApply {
    type Args = SectionAutoApplyArgs;
    type Output = AutoApplied;
    fn run(a: SectionAutoApplyArgs, cx: &mut Cx<'_>) -> Result<AutoApplied, CmdError> {
        cx.tx.get(a.cut)?;
        framing_ok(cx, a.cut, a.framing)?;
        if a.sections.len() > MAX_SECTIONS {
            return Err(CmdError::invalid("/sections", "at most 2000"));
        }
        let old: Vec<Id<Section>> = cx
            .tx
            .children::<Section>(a.cut.entity())
            .into_iter()
            .filter(|id| {
                cx.tx.staging().get(*id).is_some_and(|s| {
                    s.framing == a.framing && matches!(&s.kind, SectionKind::Zoom(z) if z.auto)
                })
            })
            .collect();
        let removed = old.len();
        for id in old {
            cx.tx.delete(id)?;
        }
        let mut ids = Vec::with_capacity(a.sections.len());
        for (i, s) in a.sections.into_iter().enumerate() {
            let mut z = s.zoom;
            z.auto = true;
            let kind = SectionKind::Zoom(z);
            kind_ok(&kind, a.cut, cx).map_err(|e| match e {
                CmdError::Invalid { reason, .. } => {
                    CmdError::invalid(&format!("/sections/{i}/zoom"), &reason)
                }
                e => e,
            })?;
            let (start, end) = span_anchors(cx, a.cut, s.start_ms, s.end_ms)?;
            let id: Id<Section> = cx.tx.mint();
            cx.tx.create(Section {
                id,
                cut: a.cut,
                framing: a.framing,
                start,
                end,
                kind,
            })?;
            ids.push(id.0.to_string());
        }
        Ok(AutoApplied { ids, removed })
    }
}
