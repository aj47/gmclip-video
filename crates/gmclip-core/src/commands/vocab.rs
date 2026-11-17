//! The rest of the M1 edit vocabulary (SPEC §11.2.1 / §4.3): gain segments, content
//! holds, hide-roles, caption settings, framing caption settings, per-part caption
//! place, visual split set/delete. Every anchor is resolved on the server (R-ANC-1) and
//! the result carries each resolution (`{src_s, cut_s, method, provenance}`).

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, WordId};
use gmclip_model::entities::*;
use gmclip_model::geometry::Place;
use gmclip_model::order::OrderKey;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use super::look::{check_patch, check_place};
use crate::anchors::{self, Resolved};
use crate::command::{Command, Cx};

/// What an anchored add returns: the new id and how its anchors resolved.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Anchored {
    pub id: String,
    pub from: Resolved,
    pub to: Resolved,
}

fn gain_ok(g: f64) -> Result<(), CmdError> {
    if !g.is_finite() || !(-60.0..=24.0).contains(&g) {
        return Err(CmdError::invalid("/gain_db", "between -60 and +24 dB"));
    }
    Ok(())
}

fn ramp_ok(ms: u32, path: &str) -> Result<(), CmdError> {
    if ms > 10_000 {
        return Err(CmdError::invalid(path, "at most 10 000 ms"));
    }
    Ok(())
}

/// Resolves a span against the cut, recording the read of the cut's part order.
pub(crate) fn span(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    from: &Anchor,
    to: &Anchor,
    path: &str,
) -> Result<(Resolved, Resolved), CmdError> {
    cx.tx.get(cut)?;
    let _ = cx.tx.children::<Part>(cut.entity());
    for a in [from, to] {
        if let Some(p) = a.part() {
            cx.tx.get(p)?;
        }
    }
    anchors::span_in(cx.tx.peek(), Some(cx.analysis), cut, from, to, path)
}

// ---------------------------------------------------------------------------------------
// Gain segments
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GainSegmentAddArgs {
    pub cut: Id<Cut>,
    pub start: Anchor,
    pub end: Anchor,
    pub gain_db: f64,
    #[serde(default)]
    pub ramp_in_ms: Option<u32>,
    #[serde(default)]
    pub ramp_out_ms: Option<u32>,
    /// The `measure{loudness}` this gain is graded against (agent grading needs one).
    #[serde(default)]
    pub measurement_ref: Option<Id<Measurement>>,
}

#[derive(Debug)]
pub struct GainSegmentAdd;

#[command(
    name = "gain_segment_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a gain segment (dB over a span between two anchors, with ramps) to a cut's audio; cite a loudness measurement when grading.",
    example = r#"{"cut": "01J00000000000000000000001", "start": {"part_start": {"part": "01J00000000000000000000002"}}, "end": {"part_end": {"part": "01J00000000000000000000002"}}, "gain_db": 10}"#
)]
impl Command for GainSegmentAdd {
    type Args = GainSegmentAddArgs;
    type Output = Anchored;
    fn run(a: GainSegmentAddArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        gain_ok(a.gain_db)?;
        let (ri, ro) = (a.ramp_in_ms.unwrap_or(80), a.ramp_out_ms.unwrap_or(120));
        ramp_ok(ri, "/ramp_in_ms")?;
        ramp_ok(ro, "/ramp_out_ms")?;
        if let Some(m) = a.measurement_ref {
            cx.tx.get(m)?;
        }
        let (from, to) = span(cx, a.cut, &a.start, &a.end, "")?;
        let last = cx
            .tx
            .children::<GainSegment>(a.cut.entity())
            .last()
            .and_then(|g| cx.tx.peek().get(*g).map(|x| x.order.clone()));
        let order = OrderKey::between(last.as_ref(), None).map_err(|_| CmdError::Internal {
            trace_id: "order".into(),
        })?;
        let id: Id<GainSegment> = cx.tx.mint();
        cx.tx.create(GainSegment {
            id,
            cut: a.cut,
            order,
            start: a.start,
            end: a.end,
            gain_db: a.gain_db,
            ramp_in_ms: ri,
            ramp_out_ms: ro,
            curve: GainCurve::LinearDb,
            measurement_ref: a.measurement_ref,
        })?;
        Ok(Anchored {
            id: id.0.to_string(),
            from,
            to,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GainSegmentSetArgs {
    pub segment: Id<GainSegment>,
    #[serde(default)]
    pub start: Option<Anchor>,
    #[serde(default)]
    pub end: Option<Anchor>,
    #[serde(default)]
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub ramp_in_ms: Option<u32>,
    #[serde(default)]
    pub ramp_out_ms: Option<u32>,
    #[serde(default)]
    pub measurement_ref: Option<Id<Measurement>>,
}

#[derive(Debug)]
pub struct GainSegmentSet;

#[command(
    name = "gain_segment_set",
    scope = "edit",
    undo = "undoable",
    doc = "Change a gain segment's span, gain, ramps or measurement reference.",
    example = r#"{"segment": "01J00000000000000000000007", "gain_db": 8}"#
)]
impl Command for GainSegmentSet {
    type Args = GainSegmentSetArgs;
    type Output = Anchored;
    fn run(a: GainSegmentSetArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        let g = cx.tx.get(a.segment)?.clone();
        if let Some(x) = a.gain_db {
            gain_ok(x)?;
        }
        if let Some(m) = a.measurement_ref {
            cx.tx.get(m)?;
        }
        let start = a.start.unwrap_or(g.start);
        let end = a.end.unwrap_or(g.end);
        let (from, to) = span(cx, g.cut, &start, &end, "")?;
        if let Some(r) = a.ramp_in_ms {
            ramp_ok(r, "/ramp_in_ms")?;
        }
        if let Some(r) = a.ramp_out_ms {
            ramp_ok(r, "/ramp_out_ms")?;
        }
        cx.tx.update(a.segment, |s| {
            s.start = start;
            s.end = end;
            if let Some(x) = a.gain_db {
                s.gain_db = x;
            }
            if let Some(r) = a.ramp_in_ms {
                s.ramp_in_ms = r;
            }
            if let Some(r) = a.ramp_out_ms {
                s.ramp_out_ms = r;
            }
            if a.measurement_ref.is_some() {
                s.measurement_ref = a.measurement_ref;
            }
        })?;
        Ok(Anchored {
            id: a.segment.0.to_string(),
            from,
            to,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GainSegmentDeleteArgs {
    pub segment: Id<GainSegment>,
}

#[derive(Debug)]
pub struct GainSegmentDelete;

#[command(
    name = "gain_segment_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a gain segment from a cut.",
    example = r#"{"segment": "01J00000000000000000000007"}"#
)]
impl Command for GainSegmentDelete {
    type Args = GainSegmentDeleteArgs;
    type Output = Done;
    fn run(a: GainSegmentDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.segment)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentHoldDeleteArgs {
    pub hold: Id<ContentHold>,
}

#[derive(Debug)]
pub struct ContentHoldDelete;

#[command(
    name = "content_hold_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a content hold from a cut.",
    example = r#"{"hold": "01J00000000000000000000008"}"#
)]
impl Command for ContentHoldDelete {
    type Args = ContentHoldDeleteArgs;
    type Output = Done;
    fn run(a: ContentHoldDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.hold)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HideRolesDeleteArgs {
    pub hide: Id<HideRoles>,
}

#[derive(Debug)]
pub struct HideRolesDelete;

#[command(
    name = "hide_roles_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a hide-roles span from a cut.",
    example = r#"{"hide": "01J00000000000000000000009"}"#
)]
impl Command for HideRolesDelete {
    type Args = HideRolesDeleteArgs;
    type Output = Done;
    fn run(a: HideRolesDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.hide)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualSplitDeleteArgs {
    pub split: Id<VisualSplit>,
}

#[derive(Debug)]
pub struct VisualSplitDelete;

#[command(
    name = "visual_split_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a visual split (the part's look goes back to what was before it).",
    example = r#"{"split": "01J0000000000000000000000A"}"#
)]
impl Command for VisualSplitDelete {
    type Args = VisualSplitDeleteArgs;
    type Output = Done;
    fn run(a: VisualSplitDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.split)?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// Content holds and hide-roles
// ---------------------------------------------------------------------------------------

fn roles_ok(roles: &[LayerRole], path: &str) -> Result<(), CmdError> {
    if roles.len() > 16 {
        return Err(CmdError::invalid(path, "at most 16 roles"));
    }
    Ok(())
}

fn hold_at_ok(cx: &mut Cx<'_>, at: &HoldAt) -> Result<(), CmdError> {
    match at {
        HoldAt::AtSrc(t) if t.0 < 0 => Err(CmdError::invalid("/at", "a source time ≥ 0")),
        HoldAt::Frame(f) => {
            cx.tx.get(f.source)?;
            if f.frame_index < 0 {
                return Err(CmdError::invalid("/at", "a frame index ≥ 0"));
            }
            Ok(())
        }
        HoldAt::AtSrc(_) => Ok(()),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentHoldAddArgs {
    pub cut: Id<Cut>,
    pub from: Anchor,
    pub to: Anchor,
    /// The frame the held layers keep showing.
    pub at: HoldAt,
    /// Layer roles held; empty = every layer but the cam.
    #[serde(default)]
    pub roles: Vec<LayerRole>,
}

#[derive(Debug)]
pub struct ContentHoldAdd;

#[command(
    name = "content_hold_add",
    scope = "edit",
    undo = "undoable",
    doc = "Hold the screen (or named layer roles) on one source frame between two anchors while camera and audio stay live.",
    example = r#"{"cut": "01J00000000000000000000001", "from": {"part_start": {"part": "01J00000000000000000000002"}}, "to": {"part_end": {"part": "01J00000000000000000000002"}}, "at": {"at_src": 1834560000000}}"#
)]
impl Command for ContentHoldAdd {
    type Args = ContentHoldAddArgs;
    type Output = Anchored;
    fn run(a: ContentHoldAddArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        roles_ok(&a.roles, "/roles")?;
        hold_at_ok(cx, &a.at)?;
        let (from, to) = span(cx, a.cut, &a.from, &a.to, "")?;
        let id: Id<ContentHold> = cx.tx.mint();
        cx.tx.create(ContentHold {
            id,
            cut: a.cut,
            from: a.from,
            to: a.to,
            at: a.at,
            roles: a.roles,
            per_framing: BTreeMap::new(),
        })?;
        Ok(Anchored {
            id: id.0.to_string(),
            from,
            to,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HideRolesAddArgs {
    pub cut: Id<Cut>,
    pub from: Anchor,
    pub to: Anchor,
    pub roles: Vec<LayerRole>,
}

#[derive(Debug)]
pub struct HideRolesAdd;

#[command(
    name = "hide_roles_add",
    scope = "edit",
    undo = "undoable",
    doc = "Hide layer roles (e.g. the cam when the source camera drops out) between two anchors.",
    example = r#"{"cut": "01J00000000000000000000001", "from": {"part_start": {"part": "01J00000000000000000000002"}}, "to": {"part_end": {"part": "01J00000000000000000000002"}}, "roles": [{"region": "cam"}]}"#
)]
impl Command for HideRolesAdd {
    type Args = HideRolesAddArgs;
    type Output = Anchored;
    fn run(a: HideRolesAddArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        if a.roles.is_empty() {
            return Err(CmdError::invalid("/roles", "name at least one role"));
        }
        roles_ok(&a.roles, "/roles")?;
        let (from, to) = span(cx, a.cut, &a.from, &a.to, "")?;
        let id: Id<HideRoles> = cx.tx.mint();
        cx.tx.create(HideRoles {
            id,
            cut: a.cut,
            from: a.from,
            to: a.to,
            roles: a.roles,
            per_framing: BTreeMap::new(),
        })?;
        Ok(Anchored {
            id: id.0.to_string(),
            from,
            to,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Captions
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoCaptionAdd {
    pub from: Anchor,
    pub to: Anchor,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionSetArgs {
    pub cut: Id<Cut>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub source: Option<CaptionSource>,
    #[serde(default)]
    pub drop_fillers: Option<bool>,
    /// Words to mark as keywords (highlighted).
    #[serde(default)]
    pub keywords_add: Vec<WordId>,
    #[serde(default)]
    pub keywords_remove: Vec<WordId>,
    /// Spans with no captions (e.g. where on-screen text already says it).
    #[serde(default)]
    pub no_caption_add: Vec<NoCaptionAdd>,
    /// Ids from an earlier `no_caption_added`.
    #[serde(default)]
    pub no_caption_remove: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptionSetResult {
    pub no_caption_added: Vec<Anchored>,
}

#[derive(Debug)]
pub struct CaptionSet;

#[command(
    name = "caption_set",
    scope = "edit",
    undo = "undoable",
    doc = "Captions for a cut: on or off, their source, filler dropping, keywords, and spans with no captions (word re-spellings: caption_edit).",
    example = r#"{"cut": "01J00000000000000000000001", "enabled": true, "keywords_add": [{"asr_revision": "r1", "ordinal": 42}], "no_caption_add": [{"from": {"part_start": {"part": "01J00000000000000000000002"}}, "to": {"part_end": {"part": "01J00000000000000000000002"}}}]}"#
)]
impl Command for CaptionSet {
    type Args = CaptionSetArgs;
    type Output = CaptionSetResult;
    fn run(a: CaptionSetArgs, cx: &mut Cx<'_>) -> Result<CaptionSetResult, CmdError> {
        if a.keywords_add.len()
            + a.keywords_remove.len()
            + a.no_caption_add.len()
            + a.no_caption_remove.len()
            > 2_000
        {
            return Err(CmdError::invalid("/", "at most 2 000 changes per call"));
        }
        for w in a.keywords_add.iter().chain(&a.keywords_remove) {
            super::bounded(&w.asr_revision, "/keywords", 128)?;
        }
        let cd = cx
            .tx
            .children::<CaptionDoc>(a.cut.entity())
            .first()
            .copied()
            .ok_or(CmdError::NotFound)?;
        let mut added = Vec::new();
        let mut new_ranges = Vec::new();
        for (i, r) in a.no_caption_add.iter().enumerate() {
            let (from, to) = span(cx, a.cut, &r.from, &r.to, &format!("/no_caption_add/{i}"))?;
            let id: Id<CaptionDoc> = cx.tx.mint();
            let key = id.0.to_string();
            added.push(Anchored {
                id: key.clone(),
                from,
                to,
            });
            new_ranges.push((
                key,
                NoCaptionRange {
                    from: r.from.clone(),
                    to: r.to.clone(),
                },
            ));
        }
        let mut kw_ids: Vec<(String, WordId)> = Vec::new();
        for w in &a.keywords_add {
            let id: Id<CaptionDoc> = cx.tx.mint();
            kw_ids.push((id.0.to_string(), w.clone()));
        }
        cx.tx.update(cd, |c| {
            if let Some(e) = a.enabled {
                c.enabled = e;
            }
            if let Some(s) = a.source {
                c.source = s;
            }
            if let Some(d) = a.drop_fillers {
                c.drop_fillers = d;
            }
            for w in &a.keywords_remove {
                c.keywords.retain(|_, x| x != w);
            }
            for (k, w) in kw_ids {
                if !c.keywords.values().any(|x| *x == w) {
                    c.keywords.insert(k, w);
                }
            }
            for k in &a.no_caption_remove {
                c.no_caption_ranges.remove(k);
            }
            for (k, r) in new_ranges {
                c.no_caption_ranges.insert(k, r);
            }
        })?;
        Ok(CaptionSetResult {
            no_caption_added: added,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FramingSetArgs {
    pub framing: Id<Framing>,
    #[serde(default)]
    pub caption_grouping: Option<CaptionGrouping>,
    #[serde(default)]
    pub caption_place: Option<Place>,
    /// Clears the framing's caption place (back to the layout's or the profile's seat).
    #[serde(default)]
    pub clear_caption_place: bool,
    /// The export's loudness for this framing: `"as_recorded"` or `{"static":
    /// {"integrated_lufs": -16, "true_peak_dbtp": -1.5}}` (RenderPlan rule 8).
    #[serde(default)]
    pub loudness: Option<LoudnessMode>,
    /// The named text style captions draw with (a `text_style_set` name).
    #[serde(default)]
    pub caption_style: Option<String>,
    /// Caption animation and keyword look; `{}` is the reference's (none, yellow).
    #[serde(default)]
    pub caption_look: Option<CaptionLook>,
    /// `{"solid": {"colour": "#0c1118"}}`, `"blurred_source"` (the part's own picture,
    /// blurred and dimmed, under the layers: vertical framings of 16:9 sources) or
    /// `{"backdrop": {"asset", "loop", "fit": "cover"|"contain"}}` (a stored video or still
    /// under every part, running on across joins; decision 0028).
    #[serde(default)]
    pub background: Option<Background>,
}

#[derive(Debug)]
pub struct FramingSet;

#[command(
    name = "framing_set",
    scope = "edit",
    undo = "undoable",
    doc = "A framing's caption grouping (story_v5, single_word, adaptive_words, readable_phrase, phrase), caption style (a named text style), caption look (animation none/bounce/pop/fade, keyword animation none/pop, keyword colour), caption place, background (solid colour, blurred_source, a plate of the style library, or a backdrop: a stored video or still under every part), and export loudness (as_recorded or static LUFS/true peak).",
    example = r#"{"framing": "01J00000000000000000000003", "caption_grouping": "story_v5", "caption_place": {"x": 0.5, "y": 0.55, "w": 0.9}}"#
)]
impl Command for FramingSet {
    type Args = FramingSetArgs;
    type Output = Done;
    fn run(a: FramingSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(p) = &a.caption_place {
            check_place(p, "/caption_place")?;
        }
        if a.caption_grouping.is_none()
            && a.caption_place.is_none()
            && !a.clear_caption_place
            && a.loudness.is_none()
            && a.caption_style.is_none()
            && a.caption_look.is_none()
            && a.background.is_none()
        {
            return Err(CmdError::invalid(
                "/",
                "set caption_grouping, caption_style, caption_look, caption_place, background or loudness",
            ));
        }
        match &a.background {
            Some(Background::Solid { colour })
                if gmclip_plan::scalars::Rgb::new(colour.clone()).is_err() =>
            {
                return Err(CmdError::invalid("/background/solid/colour", "#RRGGBB"));
            }
            // decision 0029: a plate of the project's style library, or a built-in one
            Some(Background::Plate { name })
                if super::stylelib::plate_stops(cx.tx.peek(), name).is_none() =>
            {
                return Err(CmdError::invalid(
                    "/background/plate/name",
                    "a plate of the style library (style_library_list kind plate)",
                ));
            }
            // decision 0028: a stored video or a still (a PNG with its size), fitted by
            // cover or contain (RenderPlan keeps a picture at its own aspect)
            Some(Background::Backdrop { asset, fit, .. }) => {
                if *fit == BackdropFit::Stretch {
                    return Err(CmdError::invalid(
                        "/background/backdrop/fit",
                        "cover or contain",
                    ));
                }
                let a = cx.tx.get(*asset)?;
                let ok = match a.media_type.as_str() {
                    "image/png" => a.probe.is_some(),
                    "video/mp4" | "video/quicktime" => a
                        .probe
                        .is_some_and(|p| p.fps.is_some() && p.frames.is_some_and(|n| n > 0)),
                    _ => false,
                };
                if !ok {
                    return Err(CmdError::invalid(
                        "/background/backdrop/asset",
                        "a stored video, or a PNG with its size (asset_add with its probe)",
                    ));
                }
            }
            _ => {}
        }
        if let Some(n) = &a.caption_style {
            let known = cx
                .tx
                .peek()
                .iter::<Styles>()
                .next()
                .is_some_and(|s| s.entries.contains_key(n));
            if !known {
                return Err(CmdError::invalid(
                    "/caption_style",
                    "a named text style of the project (text_style_set first)",
                ));
            }
        }
        if let Some(c) = a
            .caption_look
            .as_ref()
            .and_then(|l| l.keyword_colour.as_ref())
            && gmclip_plan::scalars::Rgba::new(c.clone()).is_err()
        {
            return Err(CmdError::invalid(
                "/caption_look/keyword_colour",
                "#RRGGBBAA",
            ));
        }
        if let Some(LoudnessMode::Static {
            integrated_lufs,
            true_peak_dbtp,
        }) = a.loudness
            && (!(-40.0..=-5.0).contains(&integrated_lufs)
                || !(-9.0..=0.0).contains(&true_peak_dbtp))
        {
            return Err(CmdError::invalid(
                "/loudness",
                "integrated −40…−5 LUFS, true peak −9…0 dBTP",
            ));
        }
        cx.tx.update(a.framing, |f| {
            if let Some(g) = a.caption_grouping {
                f.caption_grouping = g;
            }
            if a.clear_caption_place {
                f.caption_place = None;
            }
            if let Some(p) = a.caption_place {
                f.caption_place = Some(p);
            }
            if let Some(l) = a.loudness {
                f.loudness_mode = Some(l);
            }
            if let Some(n) = a.caption_style {
                f.caption_style_ref = Some(n);
            }
            if let Some(l) = a.caption_look {
                f.caption_look = (!l.is_plain()).then_some(l);
            }
            if let Some(b) = a.background {
                f.background = b;
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartCaptionPlaceArgs {
    pub part: Id<Part>,
    pub framing: Id<Framing>,
    /// Null clears the part's override.
    #[serde(default)]
    pub place: Option<Place>,
}

#[derive(Debug)]
pub struct PartCaptionPlace;

#[command(
    name = "part_caption_place",
    scope = "edit",
    undo = "undoable",
    doc = "Move one part's captions in a framing (null clears the part's override).",
    example = r#"{"part": "01J00000000000000000000002", "framing": "01J00000000000000000000003", "place": {"x": 0.5, "y": 0.62, "w": 0.9}}"#
)]
impl Command for PartCaptionPlace {
    type Args = PartCaptionPlaceArgs;
    type Output = Done;
    fn run(a: PartCaptionPlaceArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(p) = &a.place {
            check_place(p, "/place")?;
        }
        let pc = cx.tx.get(a.part)?.cut;
        if cx.tx.get(a.framing)?.cut != pc {
            return Err(CmdError::invalid(
                "/framing",
                "not a framing of the part's cut",
            ));
        }
        cx.tx.update(a.part, |p| match a.place {
            Some(pl) => {
                p.caption_place_override.insert(a.framing, pl);
            }
            None => {
                p.caption_place_override.remove(&a.framing);
            }
        })?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// Visual splits
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualSplitSetArgs {
    pub split: Id<VisualSplit>,
    /// Source time the look starts at (inside the part).
    #[serde(default)]
    pub at: Option<Flicks>,
    /// Replaces the split's overrides.
    #[serde(default)]
    pub overrides: Option<BTreeMap<Id<Layer>, LayerPatch>>,
    #[serde(default)]
    pub transition: Option<Transition>,
}

#[derive(Debug)]
pub struct VisualSplitSet;

#[command(
    name = "visual_split_set",
    scope = "edit",
    undo = "undoable",
    doc = "Move a visual split, replace its layer overrides, or change its transition.",
    example = r#"{"split": "01J0000000000000000000000A", "overrides": {"01J00000000000000000000004": {"opacity": 0.5}}}"#
)]
impl Command for VisualSplitSet {
    type Args = VisualSplitSetArgs;
    type Output = Done;
    fn run(a: VisualSplitSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let v = cx.tx.get(a.split)?.clone();
        let p = cx.tx.get(v.part)?.clone();
        if let Some(t) = a.at
            && (t < p.src_in || t >= p.src_out)
        {
            return Err(CmdError::invalid("/at", "inside the part"));
        }
        if let Some(ov) = &a.overrides {
            if ov.len() > 256 {
                return Err(CmdError::invalid("/overrides", "at most 256 layers"));
            }
            let stack = gmclip_model::effective::base_layers(cx.tx.peek(), &p, v.framing);
            for (l, patch) in ov {
                if !stack.contains(l) {
                    return Err(CmdError::invalid(
                        "/overrides",
                        "a layer not in this part's stack",
                    ));
                }
                check_patch(patch, "/overrides")?;
            }
        }
        cx.tx.update(a.split, |s| {
            if let Some(t) = a.at {
                s.at_src = t;
            }
            if let Some(o) = a.overrides {
                s.overrides = o;
            }
            if let Some(t) = a.transition {
                s.transition = t;
            }
        })?;
        Ok(Done {})
    }
}
