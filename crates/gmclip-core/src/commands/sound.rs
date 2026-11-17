//! Sound (decision 0051): what a part hears under its voice (a recording's system audio
//! under the mic), per sync member and per part or span; music and sound effects over
//! the cut (`AudioOverlay`), ducked under the voice.

use gmclip_macros::command;
use gmclip_model::anchor::Anchor;
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::vocab::{Anchored, span};
use super::{Done, record_part_children};
use crate::command::{Command, Cx, model_err};

/// A sound over the cut: −18 dB unless said, faded in over 0.5 s and out over 1 s.
pub const OVERLAY_DB: f64 = -18.0;
pub const OVERLAY_FADE_IN_MS: u32 = 500;
pub const OVERLAY_FADE_OUT_MS: u32 = 1000;

fn check_overlay(
    gain_db: Option<f64>,
    fades: [Option<u32>; 2],
    duck: Option<&Duck>,
) -> Result<(), CmdError> {
    if gain_db.is_some_and(|d| !(d.is_finite() && (UNDER_OFF_DB..=12.0).contains(&d))) {
        return Err(CmdError::invalid("/gain_db", "between -60 and +12 dB"));
    }
    if fades.iter().flatten().any(|ms| *ms > 30_000) {
        return Err(CmdError::invalid("/fade_in_ms", "fades of at most 30 s"));
    }
    if let Some(d) = duck
        && (!d.depth_db.is_finite()
            || !(0.0..=40.0).contains(&d.depth_db.abs())
            || !(1..=5000).contains(&d.attack_ms)
            || !(1..=5000).contains(&d.release_ms))
    {
        return Err(CmdError::invalid(
            "/duck",
            "a depth of 0 to 40 dB, attack and release of 1 to 5000 ms",
        ));
    }
    Ok(())
}

/// A sound's file: a stored audio (or video) asset, or a stretch of a source or group.
fn check_media(media: &AssetOrSpan, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match media {
        AssetOrSpan::Asset(a) => {
            let t = cx.tx.get(*a)?.media_type.clone();
            if !(t.starts_with("audio/") || t.starts_with("video/")) {
                return Err(CmdError::invalid("/media", "a sound or video asset"));
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
            if src_in.0 < 0 || src_out <= src_in {
                return Err(CmdError::invalid("/media", "a span with src_in < src_out"));
            }
        }
    }
    Ok(())
}

/// An under gain a command takes: −60 (silent) to +12 dB.
pub(crate) fn check_under_db(db: Option<f64>, at: &str) -> Result<(), CmdError> {
    if db.is_some_and(|d| !(d.is_finite() && (UNDER_OFF_DB..=12.0).contains(&d))) {
        return Err(CmdError::invalid(at, "between -60 and +12 dB"));
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncUnderSetArgs {
    pub member: Id<SyncMember>,
    /// Its sound under the group's audio in dB, −60 (silent) to +12; `null` = not heard.
    pub under_db: Option<f64>,
}

/// Sets how loud a sync member's own sound plays under the group's audio (a recording's
/// system audio under the mic) wherever a part does not say otherwise.
#[derive(Debug)]
pub struct SyncUnderSet;

#[command(
    name = "sync_under_set",
    scope = "edit",
    undo = "undoable",
    doc = "Hear a sync group member's own sound under the group's audio at a gain in dB (-60 silent to +12; null = not heard): a recording's system audio under the mic. Parts may override it (part_set_under).",
    example = r#"{"member": "01J00000000000000000000004", "under_db": -12}"#
)]
impl Command for SyncUnderSet {
    type Args = SyncUnderSetArgs;
    type Output = Done;
    fn run(a: SyncUnderSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        check_under_db(a.under_db, "/under_db")?;
        let m = cx.tx.get(a.member)?.clone();
        let group = cx.tx.get(m.group)?;
        if group.master_audio == Some(a.member) {
            return Err(CmdError::invalid(
                "/member",
                "the group's audio member is the sound itself (part_set_gain)",
            ));
        }
        cx.tx.update(a.member, |x| x.under_db = a.under_db)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetUnderArgs {
    /// The parts whose under sound changes (one when `span` is given).
    pub parts: Vec<Id<Part>>,
    /// Every sound under these parts' audio in dB, −60 (silent) to +12; `null` = each
    /// sync member's own level.
    pub under_db: Option<f64>,
    /// Only this source-time span `[from, to)` of the one part: it is split at each edge
    /// strictly inside it (snapped to the frame grid) and only the middle changes.
    #[serde(default)]
    pub span: Option<[Flicks; 2]>,
}

/// The parts that changed.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UnderSet {
    pub parts: Vec<String>,
}

/// Sets the level of the sounds under a part's audio, for whole parts or a span of one.
#[derive(Debug)]
pub struct PartSetUnder;

#[command(
    name = "part_set_under",
    scope = "edit",
    undo = "undoable",
    doc = "Set how loud the sounds under parts' audio play (a recording's system audio under the mic), in dB from -60 (silent) to +12, or null for each sync member's own level. With span (source times inside one part) the part is split there and only that span changes.",
    example = r#"{"parts": ["01J00000000000000000000003"], "under_db": -20}"#,
    example = r#"{"parts": ["01J00000000000000000000003"], "under_db": -60, "span": [705600000, 2116800000]}"#
)]
impl Command for PartSetUnder {
    type Args = PartSetUnderArgs;
    type Output = UnderSet;
    fn run(a: PartSetUnderArgs, cx: &mut Cx<'_>) -> Result<UnderSet, CmdError> {
        check_under_db(a.under_db, "/under_db")?;
        if a.parts.is_empty() || a.parts.len() > 10_000 {
            return Err(CmdError::invalid("/parts", "1 to 10000 parts"));
        }
        let mut targets = a.parts.clone();
        if let Some([from, to]) = a.span {
            let [only] = a.parts.as_slice() else {
                return Err(CmdError::invalid("/parts", "one part with a span"));
            };
            let p = cx.tx.get(*only)?.clone();
            let fpf = super::longform::stream_fpf(cx.tx.peek(), p.cut);
            let snap = |t: Flicks| Flicks(super::longform::nearest_frame(t.0, fpf));
            let (from, to) = (snap(from), snap(to));
            if from >= to || to <= p.src_in || from >= p.src_out {
                return Err(CmdError::invalid("/span", "a span overlapping the part"));
            }
            let min = super::part::MIN_PART.0;
            let inside = |t: Flicks, lo: Flicks, hi: Flicks| t.0 - lo.0 >= min && hi.0 - t.0 >= min;
            let _ = cx.tx.children::<Part>(p.cut.entity());
            let mut mid = *only;
            if from > p.src_in {
                if !inside(from, p.src_in, p.src_out) {
                    return Err(CmdError::invalid("/span", "each piece at least 0.12 s"));
                }
                record_part_children(cx, mid);
                mid = edit::split_part(cx.tx.staging(), mid, from).map_err(model_err)?;
            }
            let end = cx.tx.get(mid)?.src_out;
            if to < end {
                let start = cx.tx.get(mid)?.src_in;
                if !inside(to, start, end) {
                    return Err(CmdError::invalid("/span", "each piece at least 0.12 s"));
                }
                record_part_children(cx, mid);
                edit::split_part(cx.tx.staging(), mid, to).map_err(model_err)?;
            }
            targets = vec![mid];
        }
        for p in &targets {
            cx.tx.update(*p, |x| x.under_db = a.under_db)?;
        }
        Ok(UnderSet {
            parts: targets.iter().map(|p| p.0.to_string()).collect(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioOverlayAddArgs {
    pub cut: Id<Cut>,
    /// The sound: a stored audio asset (`audio/mp4`, `audio/wav`) or video asset, or a
    /// stretch of a source or group (its sound track) read from `src_in` on.
    pub media: AssetOrSpan,
    /// Where it starts and ends in the cut (it stops early when the file does).
    pub start: Anchor,
    pub end: Anchor,
    /// dB, −60 to +12 (default −18).
    #[serde(default)]
    pub gain_db: Option<f64>,
    /// Linear fades, at most 30 s (defaults 500 ms in, 1000 ms out).
    #[serde(default)]
    pub fade_in_ms: Option<u32>,
    #[serde(default)]
    pub fade_out_ms: Option<u32>,
    /// Lowered by `depth_db` (0–40) while the voice speaks, easing down over `attack_ms`
    /// before a word and back over `release_ms` after one.
    #[serde(default)]
    pub duck: Option<Duck>,
}

/// Lays music or a sound effect over a cut.
#[derive(Debug)]
pub struct AudioOverlayAdd;

#[command(
    name = "audio_overlay_add",
    scope = "edit",
    undo = "undoable",
    doc = "Lay music or a sound effect over a cut between two anchors: a stored audio asset or a stretch of a source's sound, at a gain (default -18 dB) with fades (default 0.5 s in, 1 s out), optionally ducked under the voice (duck: {depth_db, attack_ms, release_ms}).",
    example = r#"{"cut": "01J00000000000000000000001", "media": {"asset": "01J00000000000000000000009"}, "start": {"part_start": {"part": "01J00000000000000000000002"}}, "end": {"part_end": {"part": "01J00000000000000000000003"}}, "duck": {"depth_db": 12, "attack_ms": 200, "release_ms": 600}}"#
)]
impl Command for AudioOverlayAdd {
    type Args = AudioOverlayAddArgs;
    type Output = Anchored;
    fn run(a: AudioOverlayAddArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        check_overlay(a.gain_db, [a.fade_in_ms, a.fade_out_ms], a.duck.as_ref())?;
        check_media(&a.media, cx)?;
        let (from, to) = span(cx, a.cut, &a.start, &a.end, "")?;
        let id: Id<AudioOverlay> = cx.tx.mint();
        cx.tx.create(AudioOverlay {
            id,
            cut: a.cut,
            media: a.media,
            start: a.start,
            end: a.end,
            gain_db: a.gain_db.unwrap_or(OVERLAY_DB),
            fade_in_ms: a.fade_in_ms.unwrap_or(OVERLAY_FADE_IN_MS),
            fade_out_ms: a.fade_out_ms.unwrap_or(OVERLAY_FADE_OUT_MS),
            duck_under_speech: a.duck,
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
pub struct AudioOverlaySetArgs {
    pub overlay: Id<AudioOverlay>,
    #[serde(default)]
    pub start: Option<Anchor>,
    #[serde(default)]
    pub end: Option<Anchor>,
    #[serde(default)]
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub fade_in_ms: Option<u32>,
    #[serde(default)]
    pub fade_out_ms: Option<u32>,
    /// A new duck; with `no_duck` the sound stops ducking.
    #[serde(default)]
    pub duck: Option<Duck>,
    #[serde(default)]
    pub no_duck: bool,
}

/// Changes a sound over the cut: its span, gain, fades or ducking.
#[derive(Debug)]
pub struct AudioOverlaySet;

#[command(
    name = "audio_overlay_set",
    scope = "edit",
    undo = "undoable",
    doc = "Change music or a sound effect over a cut: its span (start/end anchors), gain in dB, fades in ms, or its ducking under the voice (duck, or no_duck: true to stop).",
    example = r#"{"overlay": "01J0000000000000000000000A", "gain_db": -24, "fade_out_ms": 2000}"#,
    example = r#"{"overlay": "01J0000000000000000000000A", "no_duck": true}"#
)]
impl Command for AudioOverlaySet {
    type Args = AudioOverlaySetArgs;
    type Output = Anchored;
    fn run(a: AudioOverlaySetArgs, cx: &mut Cx<'_>) -> Result<Anchored, CmdError> {
        check_overlay(a.gain_db, [a.fade_in_ms, a.fade_out_ms], a.duck.as_ref())?;
        if a.no_duck && a.duck.is_some() {
            return Err(CmdError::invalid(
                "/no_duck",
                "a duck, or no_duck, not both",
            ));
        }
        let o = cx.tx.get(a.overlay)?.clone();
        let start = a.start.unwrap_or(o.start);
        let end = a.end.unwrap_or(o.end);
        let (from, to) = span(cx, o.cut, &start, &end, "")?;
        cx.tx.update(a.overlay, |x| {
            x.start = start;
            x.end = end;
            if let Some(g) = a.gain_db {
                x.gain_db = g;
            }
            if let Some(ms) = a.fade_in_ms {
                x.fade_in_ms = ms;
            }
            if let Some(ms) = a.fade_out_ms {
                x.fade_out_ms = ms;
            }
            if a.no_duck {
                x.duck_under_speech = None;
            } else if a.duck.is_some() {
                x.duck_under_speech = a.duck;
            }
        })?;
        Ok(Anchored {
            id: a.overlay.0.to_string(),
            from,
            to,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioOverlayDeleteArgs {
    pub overlay: Id<AudioOverlay>,
}

/// Removes music or a sound effect from a cut.
#[derive(Debug)]
pub struct AudioOverlayDelete;

#[command(
    name = "audio_overlay_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove music or a sound effect from a cut.",
    example = r#"{"overlay": "01J0000000000000000000000A"}"#
)]
impl Command for AudioOverlayDelete {
    type Args = AudioOverlayDeleteArgs;
    type Output = Done;
    fn run(a: AudioOverlayDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.overlay)?;
        Ok(Done {})
    }
}
