//! `hold_screen_here` (SPEC §5.3 "Hold screen here", `h`; backlog A2.07): hold every
//! non-camera layer on the frame the playhead shows, from there to the end of the sentence
//! being said, as one transaction.
//!
//! Holds cover whole parts (the planner refuses a hold that starts or ends inside one), so
//! the part is split at the playhead's frame and again on the first frame after the
//! sentence's last word; the hold spans the piece between, by its part edges. With no
//! transcript (or no word after the playhead in the part) it runs to the end of the part.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, FrameRef, PartEdge};
use gmclip_model::edit;
use gmclip_model::entities::{
    ContentHold, Cut, Framing, HoldAt, MediaRef, MemberRole, Part, Source, SyncGroup, SyncMember,
    SyncSegment,
};
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::part::MIN_PART;
use super::record_part_children;
use crate::command::{Command, Cx, model_err};

const SEC: i64 = 705_600_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoldScreenHereArgs {
    pub cut: Id<Cut>,
    /// The output frame the playhead shows (the cut's framing rate).
    pub at_frame: u64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HeldHere {
    /// The content hold.
    pub hold: String,
    /// The part the hold covers (split out of the one under the playhead).
    pub part: String,
    /// `sentence` (to the end of the sentence being said) or `part` (no word to end on).
    pub until: String,
    /// The sentence's last word (untrusted), when it ends there.
    pub word: Option<String>,
    /// The held span, source seconds.
    pub from_s: f64,
    pub to_s: f64,
}

/// The picture's own source and frame index for media time `t` (a sync group's picture
/// member, at its offset), at the source's frame rate.
pub(crate) fn picture_frame(doc: &Doc, media: MediaRef, t: i64) -> Result<FrameRef, CmdError> {
    let (s, pic_t): (Id<Source>, i64) = match media {
        MediaRef::Source(s) => (s, t),
        MediaRef::Group(g) => {
            let grp: &SyncGroup = doc.get(g).ok_or(CmdError::NotFound)?;
            let members: Vec<&SyncMember> = doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .collect();
            let m = grp
                .picture
                .and_then(|m| doc.get(m))
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
                })
                .ok_or_else(|| CmdError::invalid("/cut", "the group has no picture member"))?;
            let off = doc
                .children::<SyncSegment>(m.id.entity())
                .filter_map(|x| doc.get(x))
                .find(|x| x.src_range.start.0 <= t && t < x.src_range.end.0)
                .map_or(0, |x| x.offset.0);
            (m.source, t - off)
        }
    };
    let rate = doc
        .get(s)
        .and_then(|so| so.probe.tracks.iter().find_map(|t| t.fps))
        .map_or(30.0, |r| r.num as f64 / f64::from(r.den.max(1)));
    Ok(FrameRef {
        source: s,
        frame_index: ((pic_t as f64 / SEC as f64) * rate).round().max(0.0) as i64,
    })
}

fn owner_of(m: MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

#[derive(Debug)]
pub struct HoldScreenHere;

#[command(
    name = "hold_screen_here",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Hold the screen (every layer but the camera) on the frame at the playhead until the end of the sentence being said; camera and audio stay live. Splits the part there, one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "at_frame": 120}"#
)]
impl Command for HoldScreenHere {
    type Args = HoldScreenHereArgs;
    type Output = HeldHere;
    fn run(a: HoldScreenHereArgs, cx: &mut Cx<'_>) -> Result<HeldHere, CmdError> {
        cx.tx.get(a.cut)?;
        let framings = cx.tx.children::<Framing>(a.cut.entity());
        let fpf = framings
            .first()
            .and_then(|f| cx.tx.peek().get(*f))
            .and_then(|f| crate::planner::framing_fpf(f.fps))
            .ok_or_else(|| CmdError::invalid("/cut", "the cut has no framing"))?;
        let parts = cx.tx.children::<Part>(a.cut.entity());
        let t = i64::try_from(a.at_frame)
            .ok()
            .and_then(|f| f.checked_mul(fpf))
            .ok_or_else(|| CmdError::invalid("/at_frame", "too large"))?;
        let doc = cx.tx.peek();
        let in_cut = gmclip_model::clock::cut_layout(doc, a.cut)
            .last()
            .is_some_and(|l| t < l.start.0 + l.dur.0);
        let (pid, src) = gmclip_model::clock::cut_to_src(doc, a.cut, Flicks(t))
            .filter(|_| in_cut)
            .ok_or_else(|| CmdError::invalid("/at_frame", "not a frame of the cut"))?;
        debug_assert!(parts.contains(&pid));
        let pt = cx.tx.get(pid)?.clone();
        let at = picture_frame(cx.tx.peek(), pt.media, src.0)?;
        // the held piece starts on the playhead's frame (whole frames of the framing)
        let mut s0 = src.0.div_euclid(fpf) * fpf;
        if s0 - pt.src_in.0 < MIN_PART.0 {
            s0 = pt.src_in.0;
        }
        // … and ends on the first frame after the sentence's last word
        let words = cx
            .analysis
            .active(cx.tx.peek(), &owner_of(pt.media))
            .map(|t| t.words.clone())
            .unwrap_or_default();
        let sentence_end = words
            .iter()
            .position(|w| w.end > s0)
            .filter(|&i| words[i].start < pt.src_out.0)
            .and_then(|i| {
                super::moment::sentences(&words)
                    .into_iter()
                    .find(|&(x, y)| x <= i && i <= y)
                    .map(|(_, y)| y)
            });
        let (mut e, mut until, mut word) = (pt.src_out.0, "part", None);
        if let Some(j) = sentence_end {
            let end = (words[j].end + fpf - 1).div_euclid(fpf) * fpf;
            if end < pt.src_out.0 {
                e = end;
                until = "sentence";
                word = Some(words[j].text.chars().take(64).collect::<String>());
            }
        }
        if pt.src_out.0 - e < MIN_PART.0 {
            e = pt.src_out.0;
        }
        if e - s0 < MIN_PART.0 {
            return Err(CmdError::invalid(
                "/at_frame",
                "less than 0.12 s to hold before the sentence ends",
            ));
        }
        record_part_children(cx, pid);
        let held = if s0 > pt.src_in.0 {
            edit::split_part(cx.tx.staging(), pid, Flicks(s0)).map_err(model_err)?
        } else {
            pid
        };
        if e < pt.src_out.0 {
            edit::split_part(cx.tx.staging(), held, Flicks(e)).map_err(model_err)?;
        }
        let id: Id<ContentHold> = cx.tx.mint();
        cx.tx.create(ContentHold {
            id,
            cut: a.cut,
            from: Anchor::PartStart(PartEdge {
                part: held,
                offset_ms: None,
            }),
            to: Anchor::PartEnd(PartEdge {
                part: held,
                offset_ms: None,
            }),
            at: HoldAt::Frame(at),
            roles: Vec::new(),
            per_framing: BTreeMap::new(),
        })?;
        let secs = |t: i64| (t as f64 / SEC as f64 * 1000.0).round() / 1000.0;
        Ok(HeldHere {
            hold: id.0.to_string(),
            part: held.0.to_string(),
            until: until.into(),
            word,
            from_s: secs(s0),
            to_s: secs(e),
        })
    }
}
