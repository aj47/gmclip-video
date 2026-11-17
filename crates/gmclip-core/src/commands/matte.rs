//! Cut-outs (SPEC §5.8 roto) and the activity crop (§5.7): `matte_set` turns a person
//! cut-out on or off for a layer or one part, `matte_brush` paints edge corrections on
//! one part's matte, `matte_quality` (holder) scores each part's matte, and
//! `crop_follow_action` (holder) seeds screen crops from where the screen changes,
//! applied through `crop_follow_apply`.

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use super::look::{check_crop, in_stack, patch_part};
use crate::command::{Command, Cx};

/// The roto model families this build segments with. `packed_alpha` segments nothing:
/// the video carries its own alpha as grey in its bottom half (its crop shows the top
/// half), for graphics rendered with transparency.
pub const ROTO_MODELS: &[&str] = &["apple_vision", PACKED_ALPHA];
/// The matte model whose mask is the picture's own bottom half (luma = alpha).
pub const PACKED_ALPHA: &str = "packed_alpha";
/// Strokes on one part's matte.
pub const MAX_STROKES: usize = 256;
/// Points in one stroke.
pub const MAX_POINTS: usize = 4096;

fn holder_only() -> CmdError {
    CmdError::Unsupported {
        platform: "core".into(),
        reason: "answered by the gmclip host (it reads media): call it through MCP or the app"
            .into(),
    }
}

/// A matte as `matte_set` takes it.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MatteArg {
    /// No cut-out.
    None,
    /// A person cut-out with Apple Vision; `soften` in source pixels (0–32, default 2).
    Roto {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        soften: Option<f64>,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatteSetArgs {
    /// The layer to cut out; or `overlay`, a video overlay (decision 0042).
    #[serde(default)]
    pub layer: Option<Id<Layer>>,
    #[serde(default)]
    pub overlay: Option<Id<Overlay>>,
    pub matte: MatteArg,
    /// With `part` and `framing`, the matte is that part's override; else the layer's own.
    #[serde(default)]
    pub part: Option<Id<Part>>,
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Debug)]
pub struct MatteSet;

fn strokes_of(m: &Matte) -> Vec<MatteStroke> {
    match m {
        Matte::Roto { strokes, .. } => strokes.clone(),
        _ => Vec::new(),
    }
}

/// The matte a part shows for a layer in a framing: the part's override, else the
/// layer's own.
fn effective_matte(
    cx: &mut Cx<'_>,
    layer: Id<Layer>,
    part: Id<Part>,
    framing: Id<Framing>,
) -> Result<Matte, CmdError> {
    let base = cx.tx.get(layer)?.matte.clone();
    let p = cx.tx.get(part)?;
    Ok(p.look
        .get(&framing)
        .and_then(|l| l.overrides.get(&layer))
        .and_then(|o| o.matte.clone())
        .unwrap_or(base))
}

#[command(
    name = "matte_set",
    scope = "edit",
    undo = "undoable",
    doc = "Cut the person out of a layer or a video overlay with no green screen (roto, Apple Vision) or turn the cut-out off; on the layer, as one part's override, or on the overlay (`overlay` instead of `layer`). Brush corrections on a part are kept.",
    example = r#"{"layer": "01J00000000000000000000007", "matte": {"roto": {"soften": 2}}, "part": "01J00000000000000000000003", "framing": "01J00000000000000000000005"}"#
)]
impl Command for MatteSet {
    type Args = MatteSetArgs;
    type Output = Done;
    fn run(a: MatteSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let new = |keep: Vec<MatteStroke>| -> Result<Matte, CmdError> {
            Ok(match &a.matte {
                MatteArg::None => Matte::None,
                MatteArg::Roto { model, soften } => {
                    let model = model.clone().unwrap_or_else(|| ROTO_MODELS[0].to_owned());
                    if !ROTO_MODELS.contains(&model.as_str()) {
                        return Err(CmdError::invalid(
                            "/matte/roto/model",
                            "apple_vision or packed_alpha",
                        ));
                    }
                    let soften = soften.unwrap_or(2.0);
                    if !(0.0..=32.0).contains(&soften) {
                        return Err(CmdError::invalid(
                            "/matte/roto/soften",
                            "0–32 source pixels",
                        ));
                    }
                    Matte::Roto {
                        model,
                        soften,
                        strokes: keep,
                    }
                }
            })
        };
        let layer = match (a.layer, a.overlay) {
            (Some(l), None) => l,
            (None, Some(o)) => {
                if a.part.is_some() || a.framing.is_some() {
                    return Err(CmdError::invalid(
                        "/overlay",
                        "an overlay's cut-out is its own: no part or framing",
                    ));
                }
                let ov = cx.tx.get(o)?;
                let Some(v) = &ov.video else {
                    return Err(CmdError::invalid("/overlay", "not a video overlay"));
                };
                let keep = v.matte.as_ref().map(strokes_of).unwrap_or_default();
                let m = Some(new(keep)?).filter(|m| *m != Matte::None);
                cx.tx.update(o, |o| {
                    if let Some(v) = o.video.as_mut() {
                        v.matte = m;
                    }
                })?;
                return Ok(Done {});
            }
            _ => return Err(CmdError::invalid("/layer", "a layer or an overlay")),
        };
        match (a.part, a.framing) {
            (Some(part), Some(framing)) => {
                in_stack(cx, layer, part, framing)?;
                let keep = strokes_of(&effective_matte(cx, layer, part, framing)?);
                let m = new(keep)?;
                patch_part(cx, part, framing, layer, |p| p.matte = Some(m))?;
            }
            (None, None) => {
                let keep = strokes_of(&cx.tx.get(layer)?.matte);
                let m = new(keep)?;
                cx.tx.update(layer, |l| l.matte = m)?;
            }
            _ => return Err(CmdError::invalid("/part", "part and framing go together")),
        }
        Ok(Done {})
    }
}

/// One brush stroke as `matte_brush` takes it.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StrokeArg {
    pub mode: StrokeMode,
    /// Brush radius as a fraction of the source height (0 < r ≤ 0.5).
    pub radius: f64,
    /// Soft edge, a fraction of the radius (0–1, default 0.5).
    #[serde(default)]
    pub feather: Option<f64>,
    /// The polyline in normalised source coordinates `[x, y]` (1–4096 points).
    pub points: Vec<[f64; 2]>,
    /// The source time of the frame painted on.
    #[serde(default)]
    pub at_src: Option<Flicks>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatteBrushArgs {
    pub layer: Id<Layer>,
    pub part: Id<Part>,
    pub framing: Id<Framing>,
    /// Strokes to add, in order.
    #[serde(default)]
    pub strokes: Vec<StrokeArg>,
    /// Remove this part's strokes first (with no `strokes`: clear them).
    #[serde(default)]
    pub clear: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MatteBrushed {
    /// Strokes on the part's matte now.
    pub strokes: usize,
}

#[derive(Debug)]
pub struct MatteBrush;

#[command(
    name = "matte_brush",
    scope = "edit",
    undo = "undoable",
    doc = "Fix a part's cut-out edges with a brush: add or subtract strokes (normalised source coordinates, radius a fraction of the source height) painted on a reference frame; they apply to every frame of that part, on top of the tracked mask.",
    example = r#"{"layer": "01J00000000000000000000007", "part": "01J00000000000000000000003", "framing": "01J00000000000000000000005", "strokes": [{"mode": "subtract", "radius": 0.02, "points": [[0.81, 0.44], [0.83, 0.47]]}]}"#
)]
impl Command for MatteBrush {
    type Args = MatteBrushArgs;
    type Output = MatteBrushed;
    fn run(a: MatteBrushArgs, cx: &mut Cx<'_>) -> Result<MatteBrushed, CmdError> {
        if a.strokes.is_empty() && !a.clear {
            return Err(CmdError::invalid(
                "/strokes",
                "at least one stroke, or clear",
            ));
        }
        for (i, s) in a.strokes.iter().enumerate() {
            let path = format!("/strokes/{i}");
            if !(s.radius.is_finite() && s.radius > 0.0 && s.radius <= 0.5) {
                return Err(CmdError::invalid(
                    &format!("{path}/radius"),
                    "0 < radius ≤ 0.5",
                ));
            }
            if s.feather.is_some_and(|f| !(0.0..=1.0).contains(&f)) {
                return Err(CmdError::invalid(&format!("{path}/feather"), "0–1"));
            }
            if s.points.is_empty() || s.points.len() > MAX_POINTS {
                return Err(CmdError::invalid(
                    &format!("{path}/points"),
                    "1–4096 points",
                ));
            }
            if s.points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || !(-0.5..=1.5).contains(v))
            {
                return Err(CmdError::invalid(
                    &format!("{path}/points"),
                    "normalised source coordinates (−0.5 to 1.5)",
                ));
            }
        }
        in_stack(cx, a.layer, a.part, a.framing)?;
        let Matte::Roto {
            model,
            soften,
            strokes,
        } = effective_matte(cx, a.layer, a.part, a.framing)?
        else {
            return Err(CmdError::invalid(
                "/layer",
                "this part's layer has no cut-out: matte_set it to roto first",
            ));
        };
        let mut strokes = if a.clear { Vec::new() } else { strokes };
        strokes.extend(a.strokes.into_iter().map(|s| MatteStroke {
            mode: s.mode,
            radius: s.radius,
            feather: s.feather.unwrap_or(0.5),
            points: s.points,
            at_src: s.at_src,
        }));
        if strokes.len() > MAX_STROKES {
            return Err(CmdError::invalid(
                "/strokes",
                "at most 256 strokes on a part",
            ));
        }
        let n = strokes.len();
        let m = Matte::Roto {
            model,
            soften,
            strokes,
        };
        patch_part(cx, a.part, a.framing, a.layer, |p| p.matte = Some(m))?;
        Ok(MatteBrushed { strokes: n })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatteQualityArgs {
    pub cut: Id<Cut>,
    /// The framing (default: the cut's first).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// Build missing accurate mask tracks first (slow: Apple Vision on every frame).
    #[serde(default)]
    pub build: Option<bool>,
}

#[derive(Debug)]
pub struct MatteQuality;

#[command(
    name = "matte_quality",
    scope = "read",
    undo = "read_only",
    doc = "Score every part's cut-out (coverage, unsure edges, flicker, lost frames → 0–100) so bad parts are found fast; build: true makes the accurate mask tracks first.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for MatteQuality {
    type Args = MatteQualityArgs;
    type Output = Done;
    fn run(_: MatteQualityArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CropFollowActionArgs {
    pub cut: Id<Cut>,
    /// The framing (default: the cut's first).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// One part (default: every part with a screen layer).
    #[serde(default)]
    pub part: Option<Id<Part>>,
    /// The screen layer (default: each part's `screen` layer).
    #[serde(default)]
    pub layer: Option<Id<Layer>>,
    /// Preview only (default true); false applies as one transaction under `base`.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug)]
pub struct CropFollowAction;

#[command(
    name = "crop_follow_action",
    scope = "edit",
    undo = "undoable",
    doc = "Follow the action: seed each part's screen crop from where the screen changes (frame differences on the scrub proxy at 2 fps; smallest box holding 80% of the change, zoomed at least 1.6× the full-width scale), splitting a part at a word boundary when the action moves across the screen; previews by default (dry_run), applies as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "dry_run": true}"#
)]
impl Command for CropFollowAction {
    type Args = CropFollowActionArgs;
    type Output = Done;
    fn run(_: CropFollowActionArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(holder_only())
    }
}

/// One crop a `crop_follow_apply` sets: the part's own (no `at`) or from `at` on (a cut
/// visual split).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FollowCrop {
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    #[serde(default)]
    pub at: Option<Flicks>,
    pub crop: NormRect,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CropFollowApplyArgs {
    pub framing: Id<Framing>,
    pub crops: Vec<FollowCrop>,
}

#[derive(Debug)]
pub struct CropFollowApply;

#[command(
    name = "crop_follow_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Apply a crop_follow_action result as one transaction (internal: run crop_follow_action).",
    example = r#"{"framing": "01J00000000000000000000005", "crops": [{"part": "01J00000000000000000000003", "layer": "01J00000000000000000000007", "crop": {"x": 0.2, "y": 0.1, "w": 0.5, "h": 0.4}}]}"#
)]
impl Command for CropFollowApply {
    type Args = CropFollowApplyArgs;
    type Output = Done;
    fn run(a: CropFollowApplyArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if a.crops.is_empty() || a.crops.len() > 1024 {
            return Err(CmdError::invalid("/crops", "1–1024 crops"));
        }
        let framing = cx.tx.get(a.framing)?.clone();
        for (i, c) in a.crops.iter().enumerate() {
            check_crop(&c.crop, &format!("/crops/{i}/crop"))?;
            let p = cx.tx.get(c.part)?.clone();
            if p.cut != framing.cut {
                return Err(CmdError::invalid(
                    &format!("/crops/{i}/part"),
                    "not in the framing's cut",
                ));
            }
            in_stack(cx, c.layer, c.part, a.framing)?;
            match c.at {
                None => patch_part(cx, c.part, a.framing, c.layer, |o| o.crop = Some(c.crop))?,
                Some(at) => {
                    if !(p.src_in < at && at < p.src_out) {
                        return Err(CmdError::invalid(
                            &format!("/crops/{i}/at"),
                            "inside the part",
                        ));
                    }
                    let _ = cx.tx.children::<VisualSplit>(c.part.entity());
                    let id: Id<VisualSplit> = cx.tx.mint();
                    let mut overrides = std::collections::BTreeMap::new();
                    overrides.insert(
                        c.layer,
                        LayerPatch {
                            crop: Some(c.crop),
                            ..LayerPatch::default()
                        },
                    );
                    cx.tx.create(VisualSplit {
                        id,
                        part: c.part,
                        framing: a.framing,
                        at_src: at,
                        overrides,
                        transition: Transition::Cut,
                        tag: None,
                    })?;
                }
            }
        }
        Ok(Done {})
    }
}
