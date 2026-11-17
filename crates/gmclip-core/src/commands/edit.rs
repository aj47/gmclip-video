//! The atomic edits the agent vocabulary (`cut_edit`, `layout_edit`, SPEC §11.2.1) needs
//! beyond the hot commands: delete a part, set a part's role, a cut's title and notes,
//! an overlay's text/band/z/opacity, delete an overlay, a layer's visible/z/opacity.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Done, bounded, record_part_children};
use crate::command::{Command, Cx, model_err};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartDeleteArgs {
    pub part: Id<Part>,
}

/// Deletes a part (its overrides and visual splits go with it).
#[derive(Debug)]
pub struct PartDelete;

#[command(
    name = "part_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Delete a part from its cut; its own layers and visual splits go with it.",
    example = r#"{"part": "01J00000000000000000000002"}"#
)]
impl Command for PartDelete {
    type Args = PartDeleteArgs;
    type Output = Done;
    fn run(a: PartDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let cut = cx.tx.get(a.part)?.cut;
        let _ = cx.tx.children::<Part>(cut.entity());
        record_part_children(cx, a.part);
        super::story::reject_on_delete(cx, a.part)?;
        edit::delete_part(cx.tx.staging(), a.part).map_err(model_err)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetRoleArgs {
    pub part: Id<Part>,
    pub role: PartRole,
}

/// Sets a part's story role.
#[derive(Debug)]
pub struct PartSetRole;

#[command(
    name = "part_set_role",
    scope = "edit",
    undo = "undoable",
    doc = "Set a part's story role (hook, setup, evidence, payoff, reaction, punchline…).",
    example = r#"{"part": "01J00000000000000000000002", "role": "payoff"}"#
)]
impl Command for PartSetRole {
    type Args = PartSetRoleArgs;
    type Output = Done;
    fn run(a: PartSetRoleArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.update(a.part, |p| p.role = a.role)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutSetArgs {
    pub cut: Id<Cut>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// The story brief (SPEC §5.2.1; additive, lane U).
    #[serde(default)]
    pub brief: Option<String>,
}

/// Sets a cut's title and/or notes.
#[derive(Debug)]
pub struct CutSet;

#[command(
    name = "cut_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set a cut's title and/or its notes (the handoff text a human reads).",
    example = r#"{"cut": "01J00000000000000000000001", "title": "My PC made this in 67 seconds"}"#
)]
impl Command for CutSet {
    type Args = CutSetArgs;
    type Output = Done;
    fn run(a: CutSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(t) = &a.title {
            bounded(t, "/title", 200)?;
        }
        if let Some(n) = &a.notes {
            bounded(n, "/notes", 20_000)?;
        }
        if let Some(b) = &a.brief {
            bounded(b, "/brief", 20_000)?;
        }
        if a.title.is_none() && a.notes.is_none() && a.brief.is_none() {
            return Err(CmdError::invalid("/", "set title, notes or brief"));
        }
        cx.tx.update(a.cut, |c| {
            if let Some(t) = a.title {
                c.title = t;
            }
            if let Some(n) = a.notes {
                c.notes = n;
            }
            if let Some(b) = a.brief {
                c.brief = b;
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlaySetArgs {
    pub overlay: Id<Overlay>,
    #[serde(default)]
    pub text: Option<OverlayText>,
    #[serde(default)]
    pub band: Option<String>,
    #[serde(default)]
    pub z: Option<i32>,
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default)]
    pub start: Option<gmclip_model::anchor::Anchor>,
    #[serde(default)]
    pub end: Option<OverlayEnd>,
    /// Replaces the overlay's number provenance.
    #[serde(default)]
    pub number_provenance: Option<Vec<NumberProvenance>>,
    /// A callout's shape (colour, width, arrow ends).
    #[serde(default)]
    pub shape: Option<OverlayShape>,
    /// A box callout's box (fractions of the canvas).
    #[serde(default)]
    pub rect: Option<gmclip_model::geometry::CanvasBox>,
}

/// Sets an overlay's text, band, z, opacity or timing.
#[derive(Debug)]
pub struct OverlaySet;

#[command(
    name = "overlay_set",
    scope = "edit",
    undo = "undoable",
    doc = "Change an overlay's text, band, z, opacity, start or end, a callout's shape or box (position: overlay_move).",
    example = r#"{"overlay": "01J00000000000000000000005", "opacity": 0.8, "end": {"dur_ms": 5000}}"#
)]
impl Command for OverlaySet {
    type Args = OverlaySetArgs;
    type Output = Done;
    fn run(a: OverlaySetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(b) = &a.band {
            bounded(b, "/band", 64)?;
        }
        if let Some(t) = &a.text {
            let n: usize = t.runs.iter().map(|r| r.text.chars().count()).sum();
            if n > 4_000 || t.runs.len() > 200 {
                return Err(CmdError::invalid("/text", "too long"));
            }
        }
        if a.opacity.is_some_and(|o| !(0.0..=1.0).contains(&o)) {
            return Err(CmdError::invalid("/opacity", "in [0, 1]"));
        }
        if a.z.is_some_and(|z| z.abs() > 10_000) {
            return Err(CmdError::invalid("/z", "within ±10 000"));
        }
        if let Some(p) = &a.number_provenance {
            super::overlay::provenance_ok(p, cx)?;
        }
        if a.shape.is_some() || a.rect.is_some() {
            let o = cx.tx.get(a.overlay)?;
            if o.kind != OverlayKind::Shape {
                return Err(CmdError::invalid("/shape", "not a shape overlay"));
            }
            let rect = a.rect.or(match o.geom {
                OverlayGeom::Box(b) => Some(b),
                OverlayGeom::Place(_) => None,
            });
            let shape = a.shape.clone().or_else(|| o.shape.clone());
            if let Some(sh) = &shape {
                super::overlay::shape_ok(sh, rect.as_ref())?;
            }
        }
        if a.start.is_some() || a.end.is_some() {
            let cut = match cx.tx.get(a.overlay)?.parent {
                OverlayParent::Cut(c) => c,
                OverlayParent::Clip(c) => cx.tx.get(c)?.cut,
            };
            let _ = cx.tx.children::<Part>(cut.entity());
            if let Some(st) = &a.start {
                crate::anchors::resolve_in(
                    cx.tx.peek(),
                    Some(cx.analysis),
                    Some(cut),
                    st,
                    "/start",
                )?;
            }
            if let Some(OverlayEnd::At(e)) = &a.end {
                crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), Some(cut), e, "/end")?;
            }
        }
        cx.tx.update(a.overlay, |o| {
            if let Some(p) = a.number_provenance {
                o.number_provenance = p;
            }
            if let Some(t) = a.text {
                o.text = Some(t);
            }
            if let Some(b) = a.band {
                o.band = Some(b);
            }
            if let Some(z) = a.z {
                o.z = z;
            }
            if let Some(op) = a.opacity {
                o.opacity = op;
            }
            if let Some(s) = a.start {
                o.start = s;
            }
            if let Some(e) = a.end {
                o.end = e;
            }
            if let Some(sh) = a.shape {
                o.shape = Some(sh);
            }
            if let Some(r) = a.rect {
                o.geom = OverlayGeom::Box(r);
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayDeleteArgs {
    pub overlay: Id<Overlay>,
}

/// Deletes an overlay and its keyframes.
#[derive(Debug)]
pub struct OverlayDelete;

#[command(
    name = "overlay_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Delete an overlay (and its keyframes).",
    example = r#"{"overlay": "01J00000000000000000000005"}"#
)]
impl Command for OverlayDelete {
    type Args = OverlayDeleteArgs;
    type Output = Done;
    fn run(a: OverlayDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.get(a.overlay)?;
        for k in cx.tx.children::<Keyframe>(a.overlay.entity()) {
            cx.tx.delete(k)?;
        }
        cx.tx.delete(a.overlay)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerSetArgs {
    pub layer: Id<Layer>,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub z: Option<i32>,
    #[serde(default)]
    pub opacity: Option<f64>,
    /// `live`, `freeze_first`, or `{"hold": {"at_src": …}}` / `{"hold": {"frame": …}}`.
    #[serde(default)]
    pub time_map: Option<TimeMap>,
    /// A cover layer's style (blur, pixelate, solid; SPEC §5.7). Cover layers only.
    #[serde(default)]
    pub cover: Option<CoverStyle>,
}

/// Sets a layer's visibility, z, opacity, time map or cover style.
#[derive(Debug)]
pub struct LayerSet;

#[command(
    name = "layer_set",
    scope = "edit",
    undo = "undoable",
    doc = "Show or hide a layer, change its z or opacity, or its time map (live, hold a frame, freeze the first) (crop: crop_set, place: place_set).",
    example = r#"{"layer": "01J00000000000000000000004", "visible": false}"#
)]
impl Command for LayerSet {
    type Args = LayerSetArgs;
    type Output = Done;
    fn run(a: LayerSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if a.opacity.is_some_and(|o| !(0.0..=1.0).contains(&o)) {
            return Err(CmdError::invalid("/opacity", "in [0, 1]"));
        }
        if a.z.is_some_and(|z| z.abs() > 10_000) {
            return Err(CmdError::invalid("/z", "within ±10 000"));
        }
        if a.visible.is_none()
            && a.z.is_none()
            && a.opacity.is_none()
            && a.time_map.is_none()
            && a.cover.is_none()
        {
            return Err(CmdError::invalid(
                "/",
                "set visible, z, opacity, time_map or cover",
            ));
        }
        if let Some(c) = &a.cover {
            super::look::check_cover(c, "/cover")?;
            if cx.tx.get(a.layer)?.kind != LayerKind::Cover {
                return Err(CmdError::invalid("/cover", "not a cover layer"));
            }
        }
        if let Some(TimeMap::Hold(HoldAt::Frame(f))) = &a.time_map {
            cx.tx.get(f.source)?;
        }
        cx.tx.update(a.layer, |l| {
            if let Some(v) = a.visible {
                l.visible = v;
            }
            if let Some(z) = a.z {
                l.z = z;
            }
            if let Some(o) = a.opacity {
                l.opacity = o;
            }
            if let Some(t) = a.time_map {
                l.time_map = t;
            }
            if let Some(c) = a.cover {
                l.cover = Some(c);
            }
        })?;
        Ok(Done {})
    }
}
