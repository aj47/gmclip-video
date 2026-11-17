//! Layouts, layers, crops, places, visual splits (SPEC §4.3, §4.3.1).

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::geometry::{NormRect, Place};
use gmclip_model::{Flicks, Id, OrderKey};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Created, Done, created};
use crate::command::{Command, Cx, model_err};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutCreateArgs {
    pub cut: Id<Cut>,
    pub name: String,
    #[serde(default)]
    pub caption_place: Option<Place>,
}

#[derive(Debug)]
pub struct LayoutCreate;

#[command(
    name = "layout_create",
    scope = "edit",
    undo = "undoable",
    doc = "Create a cut-scoped layout (a reusable layer stack) that parts can link per framing.",
    example = r#"{"cut": "01J00000000000000000000001", "name": "panel + face"}"#
)]
impl Command for LayoutCreate {
    type Args = LayoutCreateArgs;
    type Output = Created;
    fn run(a: LayoutCreateArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        cx.tx.get(a.cut)?;
        super::bounded(&a.name, "/name", 200)?;
        if let Some(p) = &a.caption_place {
            check_place(p, "/caption_place")?;
        }
        let id: Id<Layout> = cx.tx.mint();
        cx.tx.create(Layout {
            id,
            cut: Some(a.cut),
            scope: LayoutScope::Cut,
            name: a.name,
            aspect_class: AspectClass::Portrait,
            caption_place: a.caption_place,
            region_relative: false,
        })?;
        Ok(created(id))
    }
}

pub(crate) fn check_crop(c: &NormRect, path: &str) -> Result<(), CmdError> {
    if c.is_valid() {
        Ok(())
    } else {
        Err(CmdError::invalid(
            path,
            "a crop inside [0,1] with positive size",
        ))
    }
}

/// A sparse patch's values are in range (crop and place as above; opacity in [0, 1];
/// z within ±10 000; feathers in [0, 1]).
pub(crate) fn check_patch(p: &LayerPatch, path: &str) -> Result<(), CmdError> {
    if let Some(c) = &p.crop {
        check_crop(c, path)?;
    }
    if let Some(pl) = &p.place {
        check_place(pl, path)?;
    }
    if p.opacity.is_some_and(|o| !(0.0..=1.0).contains(&o)) {
        return Err(CmdError::invalid(path, "opacity in [0, 1]"));
    }
    if p.z.is_some_and(|z| z.abs() > 10_000) {
        return Err(CmdError::invalid(path, "z within ±10 000"));
    }
    if let Some(f) = &p.feather
        && [f.l, f.r, f.t, f.b]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
    {
        return Err(CmdError::invalid(path, "feathers in [0, 1]"));
    }
    if let Some(OptSet::Set(f)) = &p.frame {
        check_frame(f, path)?;
    }
    Ok(())
}

/// A layer frame's values are in range (decision 0046): radius 0–0.5 of the placed short
/// side; shadow blur 0–0.5, offset −0.5–0.5 of the canvas's short side, opacity 0–1;
/// border width 0–0.1 of the canvas's short side; colours `#RRGGBB`.
pub(crate) fn check_frame(f: &LayerFrame, path: &str) -> Result<(), CmdError> {
    let within = |v: f64, lo: f64, hi: f64| v.is_finite() && (lo..=hi).contains(&v);
    let colour = |c: &str| gmclip_plan::scalars::Rgb::new(c.to_owned()).is_ok();
    if !within(f.radius, 0.0, 0.5) {
        return Err(CmdError::invalid(
            &format!("{path}/radius"),
            "0–0.5 of the layer's short side",
        ));
    }
    if let Some(s) = &f.shadow
        && !(within(s.blur, 0.0, 0.5)
            && within(s.offset_y, -0.5, 0.5)
            && within(s.opacity, 0.0, 1.0)
            && colour(&s.colour))
    {
        return Err(CmdError::invalid(
            &format!("{path}/shadow"),
            "blur 0–0.5 and offset_y −0.5–0.5 of the canvas's short side, opacity 0–1, colour #RRGGBB",
        ));
    }
    if let Some(b) = &f.border
        && !(within(b.width, 0.0, 0.1) && colour(&b.colour))
    {
        return Err(CmdError::invalid(
            &format!("{path}/border"),
            "width 0–0.1 of the canvas's short side, colour #RRGGBB",
        ));
    }
    Ok(())
}

/// Records the index reads `base_layers` makes for a part's stack in a framing.
fn record_stack(cx: &mut Cx<'_>, p: &Part, framing: Id<Framing>) {
    let _ = cx.tx.children::<Layer>(framing.entity());
    let _ = cx.tx.children::<Layer>(p.id.entity());
    if let Some(l) = p.look.get(&framing).and_then(|l| l.layout_ref) {
        let _ = cx.tx.children::<Layer>(l.entity());
    }
}

pub(crate) fn check_place(p: &Place, path: &str) -> Result<(), CmdError> {
    let ok = [p.x, p.y, p.w]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 16.0)
        && p.w > 0.0;
    if ok {
        Ok(())
    } else {
        Err(CmdError::invalid(
            path,
            "finite, |x|,|y| ≤ 16 canvases, w > 0",
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerDeleteArgs {
    /// A layer a part adds in one framing (a detail inset) or a clip's layer; layers of a
    /// layout or a framing's default stack are shared and are hidden, not deleted.
    pub layer: Id<Layer>,
}

#[derive(Debug)]
pub struct LayerDelete;

#[command(
    name = "layer_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Delete a layer a part adds (a detail inset) or a clip's layer.",
    example = r#"{"layer": "01J00000000000000000000009"}"#
)]
impl Command for LayerDelete {
    type Args = LayerDeleteArgs;
    type Output = Done;
    fn run(a: LayerDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let l = cx.tx.get(a.layer)?;
        if !matches!(
            l.owner,
            LayerOwner::PartOverride { .. } | LayerOwner::Clip(_)
        ) {
            return Err(CmdError::invalid(
                "/layer",
                "a layout's or a framing's layer is shared by parts: hide it instead (layer_set visible)",
            ));
        }
        cx.tx.delete(a.layer)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerAddArgs {
    pub owner: LayerOwner,
    pub role: LayerRole,
    #[serde(default)]
    pub kind: Option<LayerKind>,
    #[serde(default)]
    pub member: Option<Id<SyncMember>>,
    pub crop: NormRect,
    pub place: Place,
    #[serde(default)]
    pub z: i32,
    /// A cover layer (`kind: cover`, SPEC §5.7): its crop is drawn over its place
    /// blurred, pixelated or as a solid colour (sizes in canvas px). Required with
    /// `kind: cover` (and `kind` defaults to `cover` when this is set).
    #[serde(default)]
    pub cover: Option<CoverStyle>,
}

/// A cover style's sizes and colour (SPEC §5.7: blur, pixelate, solid).
pub(crate) fn check_cover(c: &CoverStyle, path: &str) -> Result<(), CmdError> {
    let ok = match c {
        CoverStyle::Blur { radius } => radius.is_finite() && (1.0..=400.0).contains(radius),
        CoverStyle::Pixelate { block } => block.is_finite() && (1.0..=400.0).contains(block),
        CoverStyle::Solid { colour } => *colour <= 0x00ff_ffff,
    };
    if ok {
        Ok(())
    } else {
        Err(CmdError::invalid(
            path,
            "blur radius or pixel block 1–400 canvas px; a solid colour 0xRRGGBB",
        ))
    }
}

#[derive(Debug)]
pub struct LayerAdd;

#[command(
    name = "layer_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a layer to a framing's default stack, a layout, a part's own additions or a clip.",
    example = r#"{"owner": {"framing": "01J00000000000000000000005"}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}"#
)]
impl Command for LayerAdd {
    type Args = LayerAddArgs;
    type Output = Created;
    fn run(a: LayerAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        check_crop(&a.crop, "/crop")?;
        check_place(&a.place, "/place")?;
        let kind = match (a.kind, &a.cover) {
            (None, Some(_)) | (Some(LayerKind::Cover), Some(_)) => LayerKind::Cover,
            (Some(LayerKind::Cover), None) => {
                return Err(CmdError::invalid("/cover", "a cover layer needs its style"));
            }
            (Some(_), Some(_)) => {
                return Err(CmdError::invalid(
                    "/cover",
                    "only a cover layer has a style",
                ));
            }
            (k, None) => k.unwrap_or(LayerKind::SourceView),
        };
        if let Some(c) = &a.cover {
            check_cover(c, "/cover")?;
        }
        let owner = match a.owner {
            LayerOwner::Layout(l) => cx.tx.get(l).map(|x| x.id.entity())?,
            LayerOwner::Framing(f) => cx.tx.get(f).map(|x| x.id.entity())?,
            LayerOwner::PartOverride { part, framing } => {
                let fc = cx.tx.get(framing)?.cut;
                let p = cx.tx.get(part)?;
                if p.cut != fc {
                    return Err(CmdError::invalid(
                        "/owner",
                        "framing and part are in different cuts",
                    ));
                }
                p.id.entity()
            }
            LayerOwner::Clip(c) => cx.tx.get(c).map(|x| x.id.entity())?,
        };
        let last = {
            let ids = cx.tx.children::<Layer>(owner);
            ids.last()
                .and_then(|l| cx.tx.peek().get(*l))
                .map(|l| l.order.clone())
        };
        let order = OrderKey::between(last.as_ref(), None)
            .map_err(|_| CmdError::invalid("/owner", "stack is full"))?;
        let id: Id<Layer> = cx.tx.mint();
        cx.tx.create(Layer {
            id,
            owner: a.owner,
            order,
            kind,
            member: a.member,
            role: a.role,
            crop: a.crop,
            place: a.place,
            z: a.z,
            opacity: 1.0,
            visible: true,
            mirror: false,
            feather: Feather::default(),
            grade: None,
            matte: Matte::None,
            time_map: TimeMap::Live,
            out_of_range: OutOfRange::Hide,
            cover: a.cover,
            track: None,
            frame: None,
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutSetArgs {
    pub part: Id<Part>,
    pub framing: Id<Framing>,
    /// The layout to link; absent or null unlinks (back to the framing's default stack).
    #[serde(default)]
    pub layout: Option<Id<Layout>>,
}

#[derive(Debug)]
pub struct LayoutSet;

#[command(
    name = "layout_set",
    scope = "edit",
    undo = "undoable",
    doc = "Link a layout to a part in one framing (live: later layout edits show), or unlink it.",
    example = r#"{"part": "01J00000000000000000000003", "framing": "01J00000000000000000000005", "layout": "01J00000000000000000000006"}"#
)]
impl Command for LayoutSet {
    type Args = LayoutSetArgs;
    type Output = Done;
    fn run(a: LayoutSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let cut = cx.tx.get(a.part)?.cut;
        if cx.tx.get(a.framing)?.cut != cut {
            return Err(CmdError::invalid(
                "/framing",
                "not a framing of the part's cut",
            ));
        }
        if let Some(l) = a.layout {
            cx.tx.get(l)?;
        }
        edit::link_layout(cx.tx.staging(), a.part, a.framing, a.layout).map_err(model_err)?;
        Ok(Done {})
    }
}

/// Where a crop or place goes: the layer itself, or one part's override in a framing.
pub(crate) fn in_stack(
    cx: &mut Cx<'_>,
    layer: Id<Layer>,
    part: Id<Part>,
    framing: Id<Framing>,
) -> Result<(), CmdError> {
    cx.tx.get(layer)?;
    cx.tx.get(framing)?;
    let p = cx.tx.get(part)?.clone();
    record_stack(cx, &p, framing);
    if !gmclip_model::effective::base_layers(cx.tx.peek(), &p, framing).contains(&layer) {
        return Err(CmdError::invalid(
            "/layer",
            "not in this part's stack for that framing",
        ));
    }
    Ok(())
}

pub(crate) fn patch_part(
    cx: &mut Cx<'_>,
    part: Id<Part>,
    framing: Id<Framing>,
    layer: Id<Layer>,
    f: impl FnOnce(&mut LayerPatch),
) -> Result<(), CmdError> {
    cx.tx.update(part, |p| {
        let look = p.look.entry(framing).or_default();
        f(look.overrides.entry(layer).or_default());
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CropSetArgs {
    pub layer: Id<Layer>,
    pub crop: NormRect,
    /// With `part` and `framing`, the crop is that part's override; else the layer's own.
    #[serde(default)]
    pub part: Option<Id<Part>>,
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Debug)]
pub struct CropSet;

#[command(
    name = "crop_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set a layer's crop (normalised source rectangle), on the layer or as one part's override.",
    example = r#"{"layer": "01J00000000000000000000007", "crop": {"x": 0.1, "y": 0.2, "w": 0.5, "h": 0.4}}"#
)]
impl Command for CropSet {
    type Args = CropSetArgs;
    type Output = Done;
    fn run(a: CropSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        check_crop(&a.crop, "/crop")?;
        match (a.part, a.framing) {
            (Some(part), Some(framing)) => {
                in_stack(cx, a.layer, part, framing)?;
                patch_part(cx, part, framing, a.layer, |p| p.crop = Some(a.crop))?;
            }
            (None, None) => {
                cx.tx.update(a.layer, |l| l.crop = a.crop)?;
            }
            _ => return Err(CmdError::invalid("/part", "part and framing go together")),
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceSetArgs {
    pub layer: Id<Layer>,
    pub place: Place,
    #[serde(default)]
    pub part: Option<Id<Part>>,
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Debug)]
pub struct PlaceSet;

#[command(
    name = "place_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set where a layer sits on the canvas, on the layer or as one part's override.",
    example = r#"{"layer": "01J00000000000000000000007", "place": {"x": 0.148, "y": 0.583, "w": 0.704}, "part": "01J00000000000000000000003", "framing": "01J00000000000000000000005"}"#
)]
impl Command for PlaceSet {
    type Args = PlaceSetArgs;
    type Output = Done;
    fn run(a: PlaceSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        check_place(&a.place, "/place")?;
        match (a.part, a.framing) {
            (Some(part), Some(framing)) => {
                in_stack(cx, a.layer, part, framing)?;
                patch_part(cx, part, framing, a.layer, |p| p.place = Some(a.place))?;
            }
            (None, None) => {
                cx.tx.update(a.layer, |l| l.place = a.place)?;
            }
            _ => return Err(CmdError::invalid("/part", "part and framing go together")),
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerFrameSetArgs {
    pub layer: Id<Layer>,
    /// The frame (rounded corners, shadow, border); absent or null clears it.
    #[serde(default)]
    pub frame: Option<LayerFrame>,
    /// With `part` and `framing`, the frame is that part's override; else the layer's own.
    #[serde(default)]
    pub part: Option<Id<Part>>,
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Debug)]
pub struct LayerFrameSet;

#[command(
    name = "layer_frame_set",
    scope = "edit",
    undo = "undoable",
    doc = "Give a layer a frame (decision 0046): corner radius (fraction of its placed short side; 0.5 on a square place is a circle), an optional shadow {blur, offset_y (fractions of the canvas's short side), opacity, colour} and border {width (fraction of the canvas's short side), colour}; null clears it. On the layer, or as one part's override with part and framing.",
    example = r##"{"layer": "01J00000000000000000000007", "frame": {"radius": 0.03, "shadow": {"blur": 0.05, "offset_y": 0.015, "opacity": 0.45, "colour": "#000000"}}}"##
)]
impl Command for LayerFrameSet {
    type Args = LayerFrameSetArgs;
    type Output = Done;
    fn run(a: LayerFrameSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(f) = &a.frame {
            check_frame(f, "/frame")?;
            if cx.tx.get(a.layer)?.kind == LayerKind::Cover {
                return Err(CmdError::invalid("/frame", "a cover layer has no frame"));
            }
        }
        match (a.part, a.framing) {
            (Some(part), Some(framing)) => {
                in_stack(cx, a.layer, part, framing)?;
                let v = a.frame.map_or(OptSet::Clear, OptSet::Set);
                patch_part(cx, part, framing, a.layer, |p| p.frame = Some(v))?;
            }
            (None, None) => {
                cx.tx.update(a.layer, |l| l.frame = a.frame)?;
            }
            _ => return Err(CmdError::invalid("/part", "part and framing go together")),
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualSplitAddArgs {
    pub part: Id<Part>,
    pub framing: Id<Framing>,
    /// Source time the new look starts at (inside the part).
    pub at: Flicks,
    #[serde(default)]
    pub overrides: BTreeMap<Id<Layer>, LayerPatch>,
    #[serde(default)]
    pub transition: Option<Transition>,
}

#[derive(Debug)]
pub struct VisualSplitAdd;

#[command(
    name = "visual_split_add",
    scope = "edit",
    undo = "undoable",
    doc = "From a source time inside a part, give one framing's layers a new look (crop, place, …), cut or eased.",
    example = r#"{"part": "01J00000000000000000000003", "framing": "01J00000000000000000000005", "at": 1842900000000, "overrides": {"01J00000000000000000000007": {"crop": {"x": 0.2, "y": 0.2, "w": 0.3, "h": 0.3}}}, "transition": {"ease": {"dur_ms": 300, "curve": "ease_in_out"}}}"#
)]
impl Command for VisualSplitAdd {
    type Args = VisualSplitAddArgs;
    type Output = Created;
    fn run(a: VisualSplitAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let p = cx.tx.get(a.part)?.clone();
        if cx.tx.get(a.framing)?.cut != p.cut {
            return Err(CmdError::invalid(
                "/framing",
                "not a framing of the part's cut",
            ));
        }
        if !(p.src_in <= a.at && a.at < p.src_out) {
            return Err(CmdError::invalid("/at", "inside the part"));
        }
        record_stack(cx, &p, a.framing);
        if a.overrides.len() > 256 {
            return Err(CmdError::invalid("/overrides", "at most 256 layers"));
        }
        for patch in a.overrides.values() {
            check_patch(patch, "/overrides")?;
        }
        let stack = gmclip_model::effective::base_layers(cx.tx.peek(), &p, a.framing);
        for (l, patch) in &a.overrides {
            if !stack.contains(l) {
                return Err(CmdError::invalid(
                    "/overrides",
                    "a layer not in this part's stack",
                ));
            }
            if let Some(c) = &patch.crop {
                check_crop(c, "/overrides/crop")?;
            }
            if let Some(pl) = &patch.place {
                check_place(pl, "/overrides/place")?;
            }
        }
        let _ = cx.tx.children::<VisualSplit>(a.part.entity());
        let id: Id<VisualSplit> = cx.tx.mint();
        cx.tx.create(VisualSplit {
            id,
            part: a.part,
            framing: a.framing,
            at_src: a.at,
            overrides: a.overrides,
            transition: a.transition.unwrap_or(Transition::Cut),
            tag: None,
        })?;
        Ok(created(id))
    }
}
