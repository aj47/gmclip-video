//! Follow the cursor (SPEC §5.7; [`crate::cursor`]): `crop_follow_cursor` (answered by
//! the host, which reads frames) finds the cursor at 2 fps and plans a zoomed crop path;
//! `cursor_follow_apply` writes it as eased visual splits of one layer, replacing the
//! previous follow of that part and layer — one transaction, one undo step.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use super::look::{check_crop, in_stack};
use crate::command::{Command, Cx};

/// Moves a follow may write on one part.
pub const MAX_KEYS: usize = 2048;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CropFollowCursorArgs {
    pub cut: Id<Cut>,
    /// The part whose screen recording to follow.
    pub part: Id<Part>,
    /// The framing (default: the cut's first).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// The screen layer (default: the part's `screen` layer).
    #[serde(default)]
    pub layer: Option<Id<Layer>>,
    /// How far in the crop zooms on the cursor (1.2–4, default 2).
    #[serde(default)]
    pub max_zoom: Option<f64>,
    /// Each move's ease, ms (100–2000, default 600).
    #[serde(default)]
    pub ease_ms: Option<u32>,
    /// Remove the part's follow instead (back to its own crop).
    #[serde(default)]
    pub off: Option<bool>,
    /// Preview only (default true); false applies as one transaction under `base`.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug)]
pub struct CropFollowCursor;

#[command(
    name = "crop_follow_cursor",
    scope = "edit",
    undo = "undoable",
    doc = "Follow the mouse cursor on a screen recording: find the cursor at 2 fps (frame differences + an arrow template), then zoom the part's screen crop in on it (max_zoom), moving with eased visual splits when it leaves the middle of the crop and zooming out when it is gone; off removes the follow. Previews by default (dry_run), applies as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "part": "01J00000000000000000000003", "max_zoom": 2, "dry_run": true}"#
)]
impl Command for CropFollowCursor {
    type Args = CropFollowCursorArgs;
    type Output = Done;
    fn run(_: CropFollowCursorArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(CmdError::Unsupported {
            platform: "core".into(),
            reason: "answered by the gmclip host (it reads media): call it through MCP or the app"
                .into(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CropZoomFocusArgs {
    pub cut: Id<Cut>,
    /// The part whose screen recording to zoom (its source needs gmclip's input log).
    pub part: Id<Part>,
    /// The framing (default: the cut's first).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// The screen layer (default: the part's `screen` layer).
    #[serde(default)]
    pub layer: Option<Id<Layer>>,
    /// How far in the crop zooms on a caret or a click (1.2–4, default 2).
    #[serde(default)]
    pub max_zoom: Option<f64>,
    /// Each move's ease, ms (100–2000, default 600).
    #[serde(default)]
    pub ease_ms: Option<u32>,
    /// Remove the part's zoom (or follow) instead (back to its own crop).
    #[serde(default)]
    pub off: Option<bool>,
    /// Preview only (default true); false applies as one transaction under `base`.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug)]
pub struct CropZoomFocus;

#[command(
    name = "crop_zoom_focus",
    scope = "edit",
    undo = "undoable",
    doc = "Zoom to focus and clicks on a screen recording made with gmclip (its input log, decision 0045): punch the part's screen crop in on the caret (else the focused field) while typing and on each click, zoom back out after 2.5 s without either; moves at least 1 s apart, eased (600 ms). Writes the part's crop path as crop_follow_cursor does, replacing a follow. Previews by default (dry_run), applies as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "part": "01J00000000000000000000003", "max_zoom": 2, "dry_run": true}"#
)]
impl Command for CropZoomFocus {
    type Args = CropZoomFocusArgs;
    type Output = Done;
    fn run(_: CropZoomFocusArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(CmdError::Unsupported {
            platform: "core".into(),
            reason: "answered by the gmclip host (it reads the input log): call it through MCP"
                .into(),
        })
    }
}

/// One move of a follow: from `at` (source time inside the part, on the frame grid) the
/// layer's crop eases to `crop`, or back to its own crop when `crop` is absent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorKey {
    pub at: Flicks,
    #[serde(default)]
    pub crop: Option<NormRect>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorFollowApplyArgs {
    pub framing: Id<Framing>,
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    /// The moves, by time; empty removes the follow.
    pub keys: Vec<CursorKey>,
    /// Each move's ease (ms, 100–2000).
    pub ease_ms: u32,
}

#[derive(Debug)]
pub struct CursorFollowApply;

/// Whether `s` is a follow's split of `layer`: eased, untagged (a punch-in owns its own,
/// decision 0025), and patching nothing but that layer's crop (or nothing: a zoom out).
pub fn is_follow_split(s: &VisualSplit, layer: Id<Layer>) -> bool {
    s.tag.is_none()
        && matches!(s.transition, Transition::Ease { .. })
        && s.overrides.iter().all(|(id, p)| {
            *id == layer
                && *p
                    == LayerPatch {
                        crop: p.crop,
                        ..LayerPatch::default()
                    }
        })
}

#[command(
    name = "cursor_follow_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Apply a crop_follow_cursor or crop_zoom_focus result as one transaction (internal: run either).",
    example = r#"{"framing": "01J00000000000000000000005", "part": "01J00000000000000000000003", "layer": "01J00000000000000000000007", "keys": [{"at": 1842800000000, "crop": {"x": 0.2, "y": 0.1, "w": 0.5, "h": 0.5}}], "ease_ms": 600}"#
)]
impl Command for CursorFollowApply {
    type Args = CursorFollowApplyArgs;
    type Output = Done;
    fn run(a: CursorFollowApplyArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if a.keys.len() > MAX_KEYS {
            return Err(CmdError::invalid("/keys", "at most 2048 moves"));
        }
        if !(100..=2000).contains(&a.ease_ms) {
            return Err(CmdError::invalid("/ease_ms", "100–2000 ms"));
        }
        let framing = cx.tx.get(a.framing)?.clone();
        let p = cx.tx.get(a.part)?.clone();
        if p.cut != framing.cut {
            return Err(CmdError::invalid("/part", "not in the framing's cut"));
        }
        in_stack(cx, a.layer, a.part, a.framing)?;
        for (i, k) in a.keys.iter().enumerate() {
            if !(p.src_in < k.at && k.at < p.src_out) {
                return Err(CmdError::invalid(
                    &format!("/keys/{i}/at"),
                    "inside the part",
                ));
            }
            if i > 0 && k.at <= a.keys[i - 1].at {
                return Err(CmdError::invalid(
                    &format!("/keys/{i}/at"),
                    "after the move before",
                ));
            }
            if let Some(c) = &k.crop {
                check_crop(c, &format!("/keys/{i}/crop"))?;
            }
        }
        // the previous follow of this part and layer goes
        let old: Vec<Id<VisualSplit>> = cx
            .tx
            .children::<VisualSplit>(a.part.entity())
            .into_iter()
            .filter(|id| {
                cx.tx
                    .staging()
                    .get(*id)
                    .is_some_and(|s| s.framing == a.framing && is_follow_split(s, a.layer))
            })
            .collect();
        for id in old {
            cx.tx.delete(id)?;
        }
        for k in &a.keys {
            let mut overrides = BTreeMap::new();
            if let Some(c) = k.crop {
                overrides.insert(
                    a.layer,
                    LayerPatch {
                        crop: Some(c),
                        ..LayerPatch::default()
                    },
                );
            }
            let id: Id<VisualSplit> = cx.tx.mint();
            cx.tx.create(VisualSplit {
                id,
                part: a.part,
                framing: a.framing,
                at_src: k.at,
                overrides,
                transition: Transition::Ease {
                    dur_ms: a.ease_ms,
                    curve: Curve::EaseInOut,
                },
                tag: None,
            })?;
        }
        Ok(Done {})
    }
}
