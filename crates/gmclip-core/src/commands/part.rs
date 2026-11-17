//! Parts: add, trim, split, merge, reorder, gain, speed and speed ramps.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Created, Done, created, record_part_children};
use crate::command::{Command, Cx, model_err};

/// Minimum part length (SPEC §5.3): 0.12 s.
pub const MIN_PART: Flicks = Flicks(705_600_000 * 12 / 100);

fn check_range(src_in: Flicks, src_out: Flicks) -> Result<(), CmdError> {
    let len = src_out.0.checked_sub(src_in.0);
    if src_in.0 < 0 || len.is_none_or(|l| l < MIN_PART.0) {
        return Err(CmdError::invalid(
            "/src_out",
            "a part is at least 0.12 s and starts at ≥ 0",
        ));
    }
    Ok(())
}

/// The cut's frame grid (its first framing's rate), else 30 fps.
fn cut_fpf(cx: &Cx<'_>, cut: Id<Cut>) -> i64 {
    super::longform::stream_fpf(cx.tx.peek(), cut)
}

/// Nearest frame, ties to the earlier (SPEC §4.1.1): word times and agent `cut_edit`
/// splits are not on the grid, and the planner refuses those edges.
fn snap_edge(t: Flicks, fpf: i64) -> Flicks {
    Flicks(super::longform::nearest_frame(t.0, fpf))
}

fn snap_range(a: Flicks, b: Flicks, fpf: i64) -> Result<(Flicks, Flicks), CmdError> {
    let (a, b) = (snap_edge(a, fpf), snap_edge(b, fpf));
    check_range(a, b)?;
    Ok((a, b))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartAddArgs {
    pub cut: Id<Cut>,
    /// Insert after this part; first when absent.
    #[serde(default)]
    pub after: Option<Id<Part>>,
    pub media: MediaRef,
    pub src_in: Flicks,
    pub src_out: Flicks,
    #[serde(default)]
    pub role: Option<PartRole>,
}

#[derive(Debug)]
pub struct PartAdd;

#[command(
    name = "part_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a part (a source range in flicks, snapped to the cut's frame grid) to a cut, after a given part or first.",
    example = r#"{"cut": "01J00000000000000000000001", "media": {"source": "01J00000000000000000000002"}, "src_in": 1842232320000, "src_out": 1843537680000, "role": "hook"}"#
)]
impl Command for PartAdd {
    type Args = PartAddArgs;
    type Output = Created;
    fn run(a: PartAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        cx.tx.get(a.cut)?;
        let (src_in, src_out) = snap_range(a.src_in, a.src_out, cut_fpf(cx, a.cut))?;
        match a.media {
            MediaRef::Source(s) => {
                cx.tx.get(s)?;
            }
            MediaRef::Group(g) => {
                cx.tx.get(g)?;
            }
        }
        let siblings = cx.tx.children::<Part>(a.cut.entity());
        // a new take looks like the one it follows (or the cut's first): its linked
        // layout and overrides per framing, so it is not a bare frame. From a video of
        // another picture size it starts from the framing's own stack instead: crops are
        // fractions of the picture they were made on.
        let model = a.after.or_else(|| siblings.first().copied());
        let look = match model {
            Some(m) => {
                let mp = cx.tx.get(m)?.clone();
                let doc = cx.tx.peek();
                if mp.media == a.media || picture_size(doc, mp.media) == picture_size(doc, a.media)
                {
                    mp.look
                } else {
                    BTreeMap::new()
                }
            }
            None => BTreeMap::new(),
        };
        let order = edit::key_after(cx.tx.staging(), a.cut, a.after, None).map_err(model_err)?;
        let id: Id<Part> = cx.tx.mint();
        cx.tx.create(Part {
            id,
            cut: a.cut,
            order,
            media: a.media,
            src_in,
            src_out,
            audio_from: None,
            role: a.role.unwrap_or(PartRole::Other),
            from: if cx.caller.kind == gmclip_proto::CallerKind::Human {
                PartFrom::Manual
            } else {
                PartFrom::Agent
            },
            lineage: PartLineage::default(),
            gain_db: 0.0,
            speed: 1.0,
            ramp: None,
            part_group: None,
            under_db: None,
            look,
            caption_place_override: BTreeMap::new(),
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartTrimArgs {
    pub part: Id<Part>,
    #[serde(default)]
    pub src_in: Option<Flicks>,
    #[serde(default)]
    pub src_out: Option<Flicks>,
}

#[derive(Debug)]
pub struct PartTrim;

#[command(
    name = "part_trim",
    scope = "edit",
    undo = "undoable",
    doc = "Trim a part: set its source in and/or out point (flicks, snapped to the cut's frame grid).",
    example = r#"{"part": "01J00000000000000000000003", "src_out": 1843000000000}"#
)]
impl Command for PartTrim {
    type Args = PartTrimArgs;
    type Output = Done;
    fn run(a: PartTrimArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let p = cx.tx.get(a.part)?.clone();
        let (i, o) = snap_range(
            a.src_in.unwrap_or(p.src_in),
            a.src_out.unwrap_or(p.src_out),
            cut_fpf(cx, p.cut),
        )?;
        edit::trim_part(cx.tx.staging(), a.part, i, o).map_err(model_err)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSplitArgs {
    pub part: Id<Part>,
    /// The source time to split at, strictly inside the part.
    pub at: Flicks,
}

#[derive(Debug)]
pub struct PartSplit;

#[command(
    name = "part_split",
    scope = "edit",
    undo = "undoable",
    doc = "Split a part at a source time (snapped to the cut's frame grid); the first half keeps the id, the second gets a new one (R-MODEL-2).",
    example = r#"{"part": "01J00000000000000000000003", "at": 1842800000000}"#
)]
impl Command for PartSplit {
    type Args = PartSplitArgs;
    type Output = Created;
    fn run(a: PartSplitArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let p = cx.tx.get(a.part)?.clone();
        let cut = p.cut;
        let at = snap_edge(a.at, cut_fpf(cx, cut));
        let (head, tail) = (at.0.checked_sub(p.src_in.0), p.src_out.0.checked_sub(at.0));
        if head.is_none_or(|h| h < MIN_PART.0) || tail.is_none_or(|t| t < MIN_PART.0) {
            return Err(CmdError::invalid(
                "/at",
                "both halves must be at least 0.12 s",
            ));
        }
        let _ = cx.tx.children::<Part>(cut.entity());
        record_part_children(cx, a.part);
        let b = edit::split_part(cx.tx.staging(), a.part, at).map_err(model_err)?;
        Ok(created(b))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartMergeArgs {
    /// The earlier part; it keeps its id.
    pub a: Id<Part>,
    /// The part right after it, continuous in the same source.
    pub b: Id<Part>,
    /// Merge even when their linked layouts differ.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug)]
pub struct PartMerge;

#[command(
    name = "part_merge",
    scope = "edit",
    undo = "undoable",
    doc = "Merge a part with the next one when they are continuous in the source (R-MODEL-2).",
    example = r#"{"a": "01J00000000000000000000003", "b": "01J00000000000000000000004"}"#
)]
impl Command for PartMerge {
    type Args = PartMergeArgs;
    type Output = Done;
    fn run(a: PartMergeArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let cut = cx.tx.get(a.a)?.cut;
        cx.tx.get(a.b)?;
        let _ = cx.tx.children::<Part>(cut.entity());
        record_part_children(cx, a.a);
        record_part_children(cx, a.b);
        edit::merge_parts(cx.tx.staging(), a.a, a.b, a.force).map_err(model_err)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartReorderArgs {
    pub part: Id<Part>,
    /// Place after this part; first when absent.
    #[serde(default)]
    pub after: Option<Id<Part>>,
}

#[derive(Debug)]
pub struct PartReorder;

#[command(
    name = "part_reorder",
    scope = "edit",
    undo = "undoable",
    doc = "Move a part to just after another part (or to the front). Writes only that part.",
    example = r#"{"part": "01J00000000000000000000003", "after": "01J00000000000000000000004"}"#
)]
impl Command for PartReorder {
    type Args = PartReorderArgs;
    type Output = Done;
    fn run(a: PartReorderArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let cut = cx.tx.get(a.part)?.cut;
        if let Some(after) = a.after
            && cx.tx.get(after)?.cut != cut
        {
            return Err(CmdError::invalid("/after", "not in the same cut"));
        }
        let _ = cx.tx.children::<Part>(cut.entity());
        edit::reorder_part(cx.tx.staging(), a.part, a.after).map_err(model_err)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartsGroupArgs {
    pub cut: Id<Cut>,
    /// Two or more parts of the cut. A part already in a group brings its whole group.
    pub parts: Vec<Id<Part>>,
}

/// The group the parts now share.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Grouped {
    pub group: String,
    /// Every part in the group.
    pub parts: Vec<String>,
}

#[derive(Debug)]
pub struct PartsGroup;

#[command(
    name = "parts_group",
    scope = "edit",
    undo = "undoable",
    doc = "Group parts of a cut so they move together on the timeline (decision 0037). Parts already grouped bring their whole group into the new one.",
    example = r#"{"cut": "01J00000000000000000000001", "parts": ["01J00000000000000000000003", "01J00000000000000000000004"]}"#
)]
impl Command for PartsGroup {
    type Args = PartsGroupArgs;
    type Output = Grouped;
    fn run(a: PartsGroupArgs, cx: &mut Cx<'_>) -> Result<Grouped, CmdError> {
        let members = group_members(cx, a.cut, &a.parts)?;
        if members.len() < 2 {
            return Err(CmdError::invalid("/parts", "two or more parts to group"));
        }
        let g: Id<PartGroup> = cx.tx.mint();
        for id in &members {
            cx.tx.update(*id, |p| p.part_group = Some(g))?;
        }
        Ok(Grouped {
            group: g.0.to_string(),
            parts: members.iter().map(|p| p.0.to_string()).collect(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartsUngroupArgs {
    pub cut: Id<Cut>,
    /// Parts of the cut: each one's whole group is ungrouped.
    pub parts: Vec<Id<Part>>,
}

#[derive(Debug)]
pub struct PartsUngroup;

#[command(
    name = "parts_ungroup",
    scope = "edit",
    undo = "undoable",
    doc = "Ungroup the groups the given parts are in (decision 0037).",
    example = r#"{"cut": "01J00000000000000000000001", "parts": ["01J00000000000000000000003"]}"#
)]
impl Command for PartsUngroup {
    type Args = PartsUngroupArgs;
    type Output = Done;
    fn run(a: PartsUngroupArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let members = group_members(cx, a.cut, &a.parts)?;
        let mut any = false;
        for id in &members {
            if cx.tx.get(*id)?.part_group.is_some() {
                cx.tx.update(*id, |p| p.part_group = None)?;
                any = true;
            }
        }
        if !any {
            return Err(CmdError::invalid(
                "/parts",
                "none of these parts is grouped",
            ));
        }
        Ok(Done {})
    }
}

/// The parts named and every other part of the groups they are in, in the cut's order.
fn group_members(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    parts: &[Id<Part>],
) -> Result<Vec<Id<Part>>, CmdError> {
    let mut groups = Vec::new();
    for id in parts {
        let p = cx.tx.get(*id)?;
        if p.cut != cut {
            return Err(CmdError::invalid("/parts", "a part of another cut"));
        }
        if let Some(g) = p.part_group {
            groups.push(g);
        }
    }
    let mut out = Vec::new();
    for id in cx.tx.children::<Part>(cut.entity()) {
        let p = cx.tx.get(id)?;
        if parts.contains(&id) || p.part_group.is_some_and(|g| groups.contains(&g)) {
            out.push(id);
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetGainArgs {
    pub part: Id<Part>,
    /// dB against the source, −60..=+24.
    pub gain_db: f64,
}

#[derive(Debug)]
pub struct PartSetGain;

#[command(
    name = "part_set_gain",
    scope = "edit",
    undo = "undoable",
    doc = "Set a part's gain in dB (0 = as recorded).",
    example = r#"{"part": "01J00000000000000000000003", "gain_db": 10}"#
)]
impl Command for PartSetGain {
    type Args = PartSetGainArgs;
    type Output = Done;
    fn run(a: PartSetGainArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if !(a.gain_db.is_finite() && (-60.0..=24.0).contains(&a.gain_db)) {
            return Err(CmdError::invalid("/gain_db", "between -60 and +24 dB"));
        }
        cx.tx.update(a.part, |p| p.gain_db = a.gain_db)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetSpeedArgs {
    pub part: Id<Part>,
    /// ¼× to 4× (to the nearest hundredth); 1 = as recorded.
    pub speed: f64,
}

/// What a speed change did.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpeedSet {
    /// The speed the part plays at.
    pub speed: f64,
    /// Source seconds trimmed off the part's end so it lasts whole output frames.
    pub trimmed_s: f64,
    /// Whether its sound is time-stretched (pitch kept) rather than muted.
    pub sound: bool,
}

#[derive(Debug)]
pub struct PartSetSpeed;

#[command(
    name = "part_set_speed",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Play a part faster or slower: ¼× to 4× (1 = as recorded). The picture is read by source time; the sound is time-stretched with its pitch kept from ½× to 2× and muted beyond; captions follow. The part's end is trimmed by under a few frames so it lasts whole output frames. Replaces a speed ramp. One undo step.",
    example = r#"{"part": "01J00000000000000000000003", "speed": 1.5}"#
)]
impl Command for PartSetSpeed {
    type Args = PartSetSpeedArgs;
    type Output = SpeedSet;
    fn run(a: PartSetSpeedArgs, cx: &mut Cx<'_>) -> Result<SpeedSet, CmdError> {
        let (lo, hi) = crate::planner::SPEED_RANGE;
        if !(a.speed.is_finite() && (lo..=hi).contains(&a.speed)) {
            return Err(CmdError::invalid("/speed", "¼× to 4×"));
        }
        let speed = (a.speed * 100.0).round() / 100.0;
        let p = cx.tx.get(a.part)?.clone();
        let sp = crate::planner::plan_speed(speed)
            .map_err(|_| CmdError::invalid("/speed", "¼× to 4×"))?;
        let mut src_out = p.src_out;
        if let Some(sp) = sp {
            // the cut's frame grid: its first framing's rate
            let fpf = cx
                .tx
                .peek()
                .iter::<Framing>()
                .find(|f| f.cut == p.cut)
                .and_then(|f| crate::planner::framing_fpf(f.fps))
                .unwrap_or(705_600_000 / 30);
            let len = (p.src_out.0 - p.src_in.0) / fpf;
            let num = i64::try_from(sp.num).unwrap_or(i64::MAX);
            // whole output frames: the source length a multiple of `num` frames
            let keep = len / num * num;
            if keep < num {
                return Err(CmdError::invalid(
                    "/speed",
                    "the part is too short for this speed",
                ));
            }
            if (p.src_out.0 - p.src_in.0) % fpf == 0 {
                src_out = gmclip_model::Flicks(p.src_in.0 + keep * fpf);
            }
        }
        let trimmed = (p.src_out.0 - src_out.0) as f64 / 705_600_000.0;
        cx.tx.update(a.part, |x| {
            x.speed = speed;
            x.src_out = src_out;
            // a constant speed replaces a ramp (decision 0033)
            x.ramp = None;
        })?;
        Ok(SpeedSet {
            speed,
            trimmed_s: (trimmed * 1000.0).round() / 1000.0,
            sound: sp.is_none_or(|s| s.stretches()),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RampArgs {
    /// The speed the ramp reaches, ¼× to 4× (to the nearest hundredth); the part's own
    /// speed is where it starts.
    pub to: f64,
    /// Where the ramp starts: ms from the part's start, as it plays (default 0).
    #[serde(default)]
    pub start_ms: u32,
    /// How long the ramp takes as it plays, ms (at least one frame).
    pub dur_ms: u32,
    /// The ramp's curve (default `ease_in_out`).
    #[serde(default)]
    pub curve: Option<Curve>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetRampArgs {
    pub part: Id<Part>,
    /// The ramp; null removes it (the part plays at its own speed).
    #[serde(default)]
    pub ramp: Option<RampArgs>,
}

/// What a ramp change did.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RampSet {
    /// The part's length as it plays now, seconds.
    pub seconds: f64,
    /// Whether its sound is time-stretched (pitch kept) rather than muted: both ends of
    /// the ramp between ½× and 2×.
    pub sound: bool,
    /// The source span the ramp covers (flicks), when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<[Flicks; 2]>,
}

#[derive(Debug)]
pub struct PartSetRamp;

#[command(
    name = "part_set_ramp",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Ease a part's speed from its own speed to another (¼× to 4×) over a stretch of it: from `start_ms` into the part for `dur_ms` as it plays, along a curve (linear, ease_in, ease_out, ease_in_out); the part plays at the new speed after it. The ramp is kept on the source clock, so trims and splits keep it on the same footage. The picture is read by the speed's integral, the sound is time-stretched with its pitch kept while both speeds are ½× to 2× and muted otherwise, and captions follow. null removes it. One undo step.",
    example = r#"{"part": "01J00000000000000000000003", "ramp": {"to": 2, "start_ms": 1000, "dur_ms": 500, "curve": "ease_in_out"}}"#
)]
impl Command for PartSetRamp {
    type Args = PartSetRampArgs;
    type Output = RampSet;
    fn run(a: PartSetRampArgs, cx: &mut Cx<'_>) -> Result<RampSet, CmdError> {
        let p = cx.tx.get(a.part)?.clone();
        let ramp = match a.ramp {
            None => None,
            Some(r) => {
                let (lo, hi) = crate::planner::SPEED_RANGE;
                if !(r.to.is_finite() && (lo..=hi).contains(&r.to)) {
                    return Err(CmdError::invalid("/ramp/to", "¼× to 4×"));
                }
                let to = (r.to * 100.0).round() / 100.0;
                let from = p.speed;
                if to == from {
                    return Err(CmdError::invalid(
                        "/ramp/to",
                        "another speed than the part's own (set that with part_set_speed)",
                    ));
                }
                let fpf = cx
                    .tx
                    .peek()
                    .iter::<Framing>()
                    .find(|f| f.cut == p.cut)
                    .and_then(|f| crate::planner::framing_fpf(f.fps))
                    .unwrap_or(705_600_000 / 30);
                let ms = |v: u32| i64::from(v) * 705_600;
                if ms(r.dur_ms) < fpf {
                    return Err(CmdError::invalid("/ramp/dur_ms", "at least one frame"));
                }
                let curve = r.curve.unwrap_or(Curve::EaseInOut);
                // before the ramp the part plays at its own speed; over it at the average
                let start = p.src_in.0 + (ms(r.start_ms) as f64 * from).round() as i64;
                let avg = from + (to - from) * curve.integral(1.0);
                let end = start + (ms(r.dur_ms) as f64 * avg).round() as i64;
                if end > p.src_out.0 {
                    return Err(CmdError::invalid(
                        "/ramp",
                        "the ramp runs past the part's end: start it earlier, make it shorter or ease to a slower speed",
                    ));
                }
                Some(SpeedRamp {
                    to,
                    src_start: Flicks(start),
                    src_end: Flicks(end),
                    curve,
                })
            }
        };
        cx.tx.update(a.part, |x| x.ramp = ramp)?;
        let now = cx.tx.get(a.part)?;
        let (s_lo, s_hi) = crate::planner::STRETCH_RANGE;
        let ok = |v: f64| (s_lo..=s_hi).contains(&v);
        Ok(RampSet {
            seconds: (gmclip_model::clock::part_duration(now).0 as f64 / 705_600.0).round()
                / 1000.0,
            sound: ok(now.speed) && ramp.is_none_or(|r| ok(r.to)),
            src: ramp.map(|r| [r.src_start, r.src_end]),
        })
    }
}

/// The picture size of a part's media: a source's video track, a group's picture member's.
pub(crate) fn picture_size(doc: &gmclip_model::Doc, m: MediaRef) -> Option<(u32, u32)> {
    let src = match m {
        MediaRef::Source(s) => Some(s),
        MediaRef::Group(g) => doc
            .get(g)
            .and_then(|g| g.picture)
            .and_then(|pm| doc.get(pm))
            .map(|pm| pm.source),
    }?;
    let v = doc
        .get(src)?
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)?;
    Some((v.width?, v.height?))
}
