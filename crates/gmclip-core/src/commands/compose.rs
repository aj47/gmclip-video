//! The agent edit vocabulary (SPEC §11.2.1, A24): `cut_edit` and `layout_edit` carry
//! typed ops and run them in order on **one** `Tx`, so a call is one transaction with
//! one read set and one `base` check. Each op is an existing command run on the same
//! transaction; an op's `cut` may be left out and defaults to the edit's cut.
//!
//! The M1 vocabulary (frozen by AJ's go before the §11.2.1 oracle runs):
//! - `cut_edit`: part add / trim / split / merge / delete / reorder / set{role, gain_db};
//!   overlay add / move / set / delete; caption edit / keyword toggle / word-edge nudge
//!   and `caption_set{enabled, source, drop_fillers, keywords, no_caption}`; words strike
//!   / restore; gain segment add / set / delete; content hold add / delete; hide-roles
//!   add / delete; cut set{title, notes}.
//! - `layout_edit`: framing create / set{caption_grouping, caption_place}; layout create
//!   / link / unlink; layer add / crop / place / set{visible, z, opacity, time_map};
//!   per-part caption place; visual split add / set / delete.
//!
//! Every anchor is resolved on the server and each op's result says how (R-ANC-1).

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::caption::{CaptionEditArgs, KeywordToggleArgs, WordEdgeNudgeArgs};
use super::cut::FramingCreateArgs;
use super::edit::{CutSetArgs, LayerSetArgs, OverlayDeleteArgs, OverlaySetArgs, PartDeleteArgs};
use super::look::{
    CropSetArgs, LayerAddArgs, LayoutCreateArgs, LayoutSetArgs, PlaceSetArgs, VisualSplitAddArgs,
};
use super::overlay::{OverlayAddArgs, OverlayMoveArgs};
use super::part::{PartAddArgs, PartMergeArgs, PartReorderArgs, PartSplitArgs, PartTrimArgs};
use super::words::WordsArgs;
use crate::command::{Command, Cx};

/// At most this many ops per call.
pub const MAX_OPS: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetArgs {
    pub part: Id<Part>,
    #[serde(default)]
    pub role: Option<PartRole>,
    #[serde(default)]
    pub gain_db: Option<f64>,
}

/// One `cut_edit` op.
// parsed once per op and consumed at once; never stored, so boxing buys nothing
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CutOp {
    PartAdd(PartAddArgs),
    PartTrim(PartTrimArgs),
    PartSplit(PartSplitArgs),
    PartMerge(PartMergeArgs),
    PartDelete(PartDeleteArgs),
    PartReorder(PartReorderArgs),
    PartSet(PartSetArgs),
    OverlayAdd(OverlayAddArgs),
    OverlayMove(OverlayMoveArgs),
    OverlaySet(OverlaySetArgs),
    OverlayDelete(OverlayDeleteArgs),
    CaptionEdit(CaptionEditArgs),
    CaptionKeywordToggle(KeywordToggleArgs),
    WordEdgeNudge(WordEdgeNudgeArgs),
    WordsStrike(WordsArgs),
    WordsRestore(WordsArgs),
    CutSet(CutSetArgs),
    GainSegmentAdd(super::vocab::GainSegmentAddArgs),
    GainSegmentSet(super::vocab::GainSegmentSetArgs),
    GainSegmentDelete(super::vocab::GainSegmentDeleteArgs),
    ContentHoldAdd(super::vocab::ContentHoldAddArgs),
    ContentHoldDelete(super::vocab::ContentHoldDeleteArgs),
    HideRolesAdd(super::vocab::HideRolesAddArgs),
    HideRolesDelete(super::vocab::HideRolesDeleteArgs),
    CaptionSet(super::vocab::CaptionSetArgs),
    /// The hi-res picture's offset (a sync group of this cut), e.g. with the part split
    /// where it changes, as one undo step.
    SyncOffsetSet(super::cut::SyncOffsetSetArgs),
}

/// One `layout_edit` op.
// parsed once per op and consumed at once; never stored, so boxing buys nothing
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LayoutOp {
    FramingCreate(FramingCreateArgs),
    LayoutCreate(LayoutCreateArgs),
    /// Link a layout to a part in a framing (`layout` required).
    LayoutLink(LayoutSetArgs),
    /// Unlink: the part falls back to the framing's default layers.
    LayoutUnlink(LayoutUnlinkArgs),
    LayerAdd(LayerAddArgs),
    LayerDelete(super::look::LayerDeleteArgs),
    LayerCrop(CropSetArgs),
    LayerPlace(PlaceSetArgs),
    LayerSet(LayerSetArgs),
    VisualSplitAdd(VisualSplitAddArgs),
    VisualSplitSet(super::vocab::VisualSplitSetArgs),
    VisualSplitDelete(super::vocab::VisualSplitDeleteArgs),
    FramingSet(super::vocab::FramingSetArgs),
    CaptionPlace(super::vocab::PartCaptionPlaceArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutUnlinkArgs {
    pub part: Id<Part>,
    pub framing: Id<Framing>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutEditArgs {
    pub cut: Id<Cut>,
    /// Applied in order as one transaction; an op's `cut` defaults to this one.
    #[schemars(with = "Vec<CutOp>")]
    pub ops: Vec<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutEditArgs {
    pub cut: Id<Cut>,
    #[schemars(with = "Vec<LayoutOp>")]
    pub ops: Vec<Value>,
}

/// Each op's result, in order (`{op, ...}`; created ids under `id`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditResults {
    pub results: Vec<Value>,
}

const CUT_OPS_WITH_CUT: &[&str] = &[
    "part_add",
    "overlay_add",
    "caption_edit",
    "caption_keyword_toggle",
    "word_edge_nudge",
    "words_strike",
    "words_restore",
    "cut_set",
    "gain_segment_add",
    "content_hold_add",
    "hide_roles_add",
    "caption_set",
];
const LAYOUT_OPS_WITH_CUT: &[&str] = &["framing_create", "layout_create"];

/// Fills a missing `cut`, refuses another cut, and parses one op; errors name `/ops/i`.
fn prepare<T: serde::de::DeserializeOwned>(
    mut v: Value,
    i: usize,
    cut: Id<Cut>,
    with_cut: &[&str],
) -> Result<(String, T), CmdError> {
    let path = format!("/ops/{i}");
    let obj = v
        .as_object_mut()
        .ok_or_else(|| CmdError::invalid(&path, "an op is an object with an `op` field"))?;
    let name = obj
        .get("op")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CmdError::invalid(&path, "missing `op`"))?;
    if with_cut.contains(&name.as_str()) {
        let want = Value::String(cut.0.to_string());
        match obj.get("cut") {
            None => {
                obj.insert("cut".into(), want);
            }
            Some(c) if *c == want => {}
            Some(_) => {
                return Err(CmdError::invalid(
                    &format!("{path}/cut"),
                    "not this edit's cut",
                ));
            }
        }
    }
    let op = serde_json::from_value(v).map_err(|e| {
        CmdError::invalid(
            &path,
            match e.classify() {
                serde_json::error::Category::Data => {
                    "unknown op, or a wrong, missing or unknown field"
                }
                _ => "malformed",
            },
        )
    })?;
    Ok((name, op))
}

/// Prefixes an `Invalid` path with the op's position.
fn at(i: usize, e: CmdError) -> CmdError {
    match e {
        CmdError::Invalid { path, reason } => CmdError::Invalid {
            path: format!("/ops/{i}{}", if path == "/" { "" } else { &path }),
            reason,
        },
        other => other,
    }
}

fn run_op<C: Command>(args: C::Args, cx: &mut Cx<'_>) -> Result<Value, CmdError> {
    let out = C::run(args, cx)?;
    serde_json::to_value(out).map_err(|_| CmdError::Internal {
        trace_id: "serialise-output".into(),
    })
}

fn part_in(cx: &mut Cx<'_>, part: Id<Part>, cut: Id<Cut>) -> Result<(), CmdError> {
    if cx.tx.get(part)?.cut != cut {
        return Err(CmdError::invalid("/part", "not a part of this cut"));
    }
    Ok(())
}

fn overlay_in(cx: &mut Cx<'_>, ov: Id<Overlay>, cut: Id<Cut>) -> Result<(), CmdError> {
    let c = match cx.tx.get(ov)?.parent {
        OverlayParent::Cut(c) => c,
        OverlayParent::Clip(c) => cx.tx.get(c)?.cut,
    };
    if c != cut {
        return Err(CmdError::invalid("/overlay", "not an overlay of this cut"));
    }
    Ok(())
}

fn framing_in(cx: &mut Cx<'_>, f: Id<Framing>, cut: Id<Cut>) -> Result<(), CmdError> {
    if cx.tx.get(f)?.cut != cut {
        return Err(CmdError::invalid("/framing", "not a framing of this cut"));
    }
    Ok(())
}

/// The cut a layer owner belongs to (`None` for library layouts: not editable through
/// a cut's `layout_edit`).
fn owner_cut(cx: &mut Cx<'_>, o: &LayerOwner) -> Result<Option<Id<Cut>>, CmdError> {
    Ok(match o {
        LayerOwner::Framing(f) => Some(cx.tx.get(*f)?.cut),
        LayerOwner::Layout(l) => cx.tx.get(*l)?.cut,
        LayerOwner::PartOverride { part, .. } => Some(cx.tx.get(*part)?.cut),
        LayerOwner::Clip(c) => Some(cx.tx.get(*c)?.cut),
    })
}

fn owner_in(cx: &mut Cx<'_>, o: &LayerOwner, cut: Id<Cut>) -> Result<(), CmdError> {
    if owner_cut(cx, o)? != Some(cut) {
        return Err(CmdError::invalid("/owner", "not owned by this cut"));
    }
    if let LayerOwner::PartOverride { framing, .. } = o {
        framing_in(cx, *framing, cut)?;
    }
    Ok(())
}

fn layer_in(cx: &mut Cx<'_>, layer: Id<Layer>, cut: Id<Cut>) -> Result<(), CmdError> {
    let owner = cx.tx.get(layer)?.owner;
    owner_in(cx, &owner, cut).map_err(|_| CmdError::invalid("/layer", "not a layer of this cut"))
}

/// A layout a part in this cut may link: this cut's own, or a library layout.
fn layout_linkable(cx: &mut Cx<'_>, l: Id<Layout>, cut: Id<Cut>) -> Result<(), CmdError> {
    match cx.tx.get(l)?.cut {
        Some(c) if c != cut => Err(CmdError::invalid("/layout", "another cut's layout")),
        _ => Ok(()),
    }
}

fn tagged(name: String, mut out: Value) -> Value {
    match out.as_object_mut() {
        Some(o) => {
            o.insert("op".into(), Value::String(name));
            out
        }
        None => serde_json::json!({ "op": name, "result": out }),
    }
}

fn child_in(of: Id<Cut>, cut: Id<Cut>) -> Result<(), CmdError> {
    if of != cut {
        return Err(CmdError::invalid("/", "not part of this cut"));
    }
    Ok(())
}

fn cut_op(cx: &mut Cx<'_>, cut: Id<Cut>, op: CutOp) -> Result<Value, CmdError> {
    use super::caption::{CaptionEdit, CaptionKeywordToggle, WordEdgeNudge};
    use super::edit::{
        CutSet, OverlayDelete, OverlaySet, PartDelete, PartSetRole, PartSetRoleArgs,
    };
    use super::overlay::{OverlayAdd, OverlayMove};
    use super::part::{
        PartAdd, PartMerge, PartReorder, PartSetGain, PartSetGainArgs, PartSplit, PartTrim,
    };
    use super::vocab as V;
    use super::words::{WordsRestore, WordsStrike};
    match op {
        CutOp::PartAdd(a) => run_op::<PartAdd>(a, cx),
        CutOp::PartTrim(a) => {
            part_in(cx, a.part, cut)?;
            run_op::<PartTrim>(a, cx)
        }
        CutOp::PartSplit(a) => {
            part_in(cx, a.part, cut)?;
            run_op::<PartSplit>(a, cx)
        }
        CutOp::PartMerge(a) => {
            part_in(cx, a.a, cut)?;
            run_op::<PartMerge>(a, cx)
        }
        CutOp::PartDelete(a) => {
            part_in(cx, a.part, cut)?;
            run_op::<PartDelete>(a, cx)
        }
        CutOp::PartReorder(a) => {
            part_in(cx, a.part, cut)?;
            run_op::<PartReorder>(a, cx)
        }
        CutOp::PartSet(a) => {
            part_in(cx, a.part, cut)?;
            if a.role.is_none() && a.gain_db.is_none() {
                return Err(CmdError::invalid("/", "set role, gain_db or both"));
            }
            if let Some(role) = a.role {
                run_op::<PartSetRole>(PartSetRoleArgs { part: a.part, role }, cx)?;
            }
            if let Some(gain_db) = a.gain_db {
                run_op::<PartSetGain>(
                    PartSetGainArgs {
                        part: a.part,
                        gain_db,
                    },
                    cx,
                )?;
            }
            Ok(serde_json::json!({}))
        }
        CutOp::OverlayAdd(a) => run_op::<OverlayAdd>(a, cx),
        CutOp::OverlayMove(a) => {
            overlay_in(cx, a.overlay, cut)?;
            run_op::<OverlayMove>(a, cx)
        }
        CutOp::OverlaySet(a) => {
            overlay_in(cx, a.overlay, cut)?;
            run_op::<OverlaySet>(a, cx)
        }
        CutOp::OverlayDelete(a) => {
            overlay_in(cx, a.overlay, cut)?;
            run_op::<OverlayDelete>(a, cx)
        }
        CutOp::CaptionEdit(a) => run_op::<CaptionEdit>(a, cx),
        CutOp::CaptionKeywordToggle(a) => run_op::<CaptionKeywordToggle>(a, cx),
        CutOp::WordEdgeNudge(a) => run_op::<WordEdgeNudge>(a, cx),
        CutOp::WordsStrike(a) => run_op::<WordsStrike>(a, cx),
        CutOp::WordsRestore(a) => run_op::<WordsRestore>(a, cx),
        CutOp::CutSet(a) => run_op::<CutSet>(a, cx),
        CutOp::GainSegmentAdd(a) => run_op::<V::GainSegmentAdd>(a, cx),
        CutOp::GainSegmentSet(a) => {
            child_in(cx.tx.get(a.segment)?.cut, cut)?;
            run_op::<V::GainSegmentSet>(a, cx)
        }
        CutOp::GainSegmentDelete(a) => {
            child_in(cx.tx.get(a.segment)?.cut, cut)?;
            run_op::<V::GainSegmentDelete>(a, cx)
        }
        CutOp::ContentHoldAdd(a) => run_op::<V::ContentHoldAdd>(a, cx),
        CutOp::ContentHoldDelete(a) => {
            child_in(cx.tx.get(a.hold)?.cut, cut)?;
            run_op::<V::ContentHoldDelete>(a, cx)
        }
        CutOp::HideRolesAdd(a) => run_op::<V::HideRolesAdd>(a, cx),
        CutOp::HideRolesDelete(a) => {
            child_in(cx.tx.get(a.hide)?.cut, cut)?;
            run_op::<V::HideRolesDelete>(a, cx)
        }
        CutOp::CaptionSet(a) => run_op::<V::CaptionSet>(a, cx),
        CutOp::SyncOffsetSet(a) => {
            let group = cx.tx.get(a.member)?.group;
            let parts = cx.tx.children::<Part>(cut.entity());
            let mine = parts.iter().any(|p| {
                cx.tx
                    .peek()
                    .get(*p)
                    .is_some_and(|p| p.media == MediaRef::Group(group))
            });
            if !mine {
                return Err(CmdError::invalid(
                    "/member",
                    "not a sync member of this cut's media",
                ));
            }
            run_op::<super::cut::SyncOffsetSet>(a, cx)
        }
    }
}

fn layout_op(cx: &mut Cx<'_>, cut: Id<Cut>, op: LayoutOp) -> Result<Value, CmdError> {
    use super::cut::FramingCreate;
    use super::edit::LayerSet;
    use super::look::{CropSet, LayerAdd, LayoutCreate, LayoutSet, PlaceSet, VisualSplitAdd};
    match op {
        LayoutOp::FramingCreate(a) => run_op::<FramingCreate>(a, cx),
        LayoutOp::LayoutCreate(a) => run_op::<LayoutCreate>(a, cx),
        LayoutOp::LayoutLink(a) => {
            part_in(cx, a.part, cut)?;
            framing_in(cx, a.framing, cut)?;
            if let Some(l) = a.layout {
                layout_linkable(cx, l, cut)?;
            }
            if a.layout.is_none() {
                return Err(CmdError::invalid(
                    "/layout",
                    "layout_link needs a layout (layout_unlink clears it)",
                ));
            }
            run_op::<LayoutSet>(a, cx)
        }
        LayoutOp::LayoutUnlink(a) => {
            part_in(cx, a.part, cut)?;
            framing_in(cx, a.framing, cut)?;
            run_op::<LayoutSet>(
                LayoutSetArgs {
                    part: a.part,
                    framing: a.framing,
                    layout: None,
                },
                cx,
            )
        }
        LayoutOp::LayerAdd(a) => {
            owner_in(cx, &a.owner, cut)?;
            run_op::<LayerAdd>(a, cx)
        }
        LayoutOp::LayerDelete(a) => {
            layer_in(cx, a.layer, cut)?;
            run_op::<super::look::LayerDelete>(a, cx)
        }
        LayoutOp::LayerCrop(a) => {
            layer_in(cx, a.layer, cut)?;
            if let Some(p) = a.part {
                part_in(cx, p, cut)?;
            }
            if let Some(f) = a.framing {
                framing_in(cx, f, cut)?;
            }
            run_op::<CropSet>(a, cx)
        }
        LayoutOp::LayerPlace(a) => {
            layer_in(cx, a.layer, cut)?;
            if let Some(p) = a.part {
                part_in(cx, p, cut)?;
            }
            if let Some(f) = a.framing {
                framing_in(cx, f, cut)?;
            }
            run_op::<PlaceSet>(a, cx)
        }
        LayoutOp::LayerSet(a) => {
            layer_in(cx, a.layer, cut)?;
            run_op::<LayerSet>(a, cx)
        }
        LayoutOp::VisualSplitSet(a) => {
            let part = cx.tx.get(a.split)?.part;
            part_in(cx, part, cut)?;
            run_op::<super::vocab::VisualSplitSet>(a, cx)
        }
        LayoutOp::VisualSplitDelete(a) => {
            let part = cx.tx.get(a.split)?.part;
            part_in(cx, part, cut)?;
            run_op::<super::vocab::VisualSplitDelete>(a, cx)
        }
        LayoutOp::FramingSet(a) => {
            framing_in(cx, a.framing, cut)?;
            run_op::<super::vocab::FramingSet>(a, cx)
        }
        LayoutOp::CaptionPlace(a) => {
            part_in(cx, a.part, cut)?;
            framing_in(cx, a.framing, cut)?;
            run_op::<super::vocab::PartCaptionPlace>(a, cx)
        }
        LayoutOp::VisualSplitAdd(a) => {
            part_in(cx, a.part, cut)?;
            framing_in(cx, a.framing, cut)?;
            run_op::<VisualSplitAdd>(a, cx)
        }
    }
}

/// `cut_edit`: typed ops on one cut, one transaction.
#[derive(Debug)]
pub struct CutEdit;

#[command(
    name = "cut_edit",
    scope = "edit",
    undo = "undoable",
    doc = "Edit a cut with typed ops applied in order as one transaction: part add/trim/split/merge/delete/reorder/set role or gain, overlay add/move/set/delete, gain segments, content holds, hide-roles, captions (edit, keywords, settings, no-caption spans), words strike/restore, title and notes.",
    example = r#"{"cut": "01J00000000000000000000001", "ops": [{"op": "part_trim", "part": "01J00000000000000000000002", "src_out": 73382400000}, {"op": "part_set", "part": "01J00000000000000000000002", "role": "hook", "gain_db": 3}, {"op": "cut_set", "title": "My PC made this in 67 seconds"}]}"#
)]
impl Command for CutEdit {
    type Args = CutEditArgs;
    type Output = EditResults;
    fn run(a: CutEditArgs, cx: &mut Cx<'_>) -> Result<EditResults, CmdError> {
        if a.ops.is_empty() || a.ops.len() > MAX_OPS {
            return Err(CmdError::invalid("/ops", "1..=200 ops"));
        }
        cx.tx.get(a.cut)?;
        let mut results = Vec::with_capacity(a.ops.len());
        for (i, v) in a.ops.into_iter().enumerate() {
            let (name, op) = prepare::<CutOp>(v, i, a.cut, CUT_OPS_WITH_CUT)?;
            let out = cut_op(cx, a.cut, op).map_err(|e| at(i, e))?;
            results.push(tagged(name, out));
        }
        Ok(EditResults { results })
    }
}

/// `layout_edit`: typed framing/layout/layer ops on one cut, one transaction.
#[derive(Debug)]
pub struct LayoutEdit;

#[command(
    name = "layout_edit",
    scope = "edit",
    undo = "undoable",
    doc = "Edit a cut's framings, layouts and layers with typed ops as one transaction: framing create/set (caption grouping and place), layout create/link/unlink, layer add/crop/place/set (visible, z, opacity, time map), per-part caption place, visual split add/set/delete.",
    example = r#"{"cut": "01J00000000000000000000001", "ops": [{"op": "layer_crop", "layer": "01J00000000000000000000004", "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}}, {"op": "layer_set", "layer": "01J00000000000000000000006", "visible": false}]}"#
)]
impl Command for LayoutEdit {
    type Args = LayoutEditArgs;
    type Output = EditResults;
    fn run(a: LayoutEditArgs, cx: &mut Cx<'_>) -> Result<EditResults, CmdError> {
        if a.ops.is_empty() || a.ops.len() > MAX_OPS {
            return Err(CmdError::invalid("/ops", "1..=200 ops"));
        }
        cx.tx.get(a.cut)?;
        let mut results = Vec::with_capacity(a.ops.len());
        for (i, v) in a.ops.into_iter().enumerate() {
            let (name, op) = prepare::<LayoutOp>(v, i, a.cut, LAYOUT_OPS_WITH_CUT)?;
            let out = layout_op(cx, a.cut, op).map_err(|e| at(i, e))?;
            results.push(tagged(name, out));
        }
        Ok(EditResults { results })
    }
}
