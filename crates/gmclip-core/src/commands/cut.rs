//! Cuts and framings.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id, Rational};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Created, created};
use crate::command::{Command, Cx};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutCreateArgs {
    pub title: String,
    #[serde(default)]
    pub moment: Option<Id<Moment>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CutCreated {
    pub cut: String,
    pub caption_doc: String,
}

/// Creates a cut with its caption document.
#[derive(Debug)]
pub struct CutCreate;

#[command(
    name = "cut_create",
    scope = "edit",
    undo = "undoable",
    doc = "Create an empty cut (and its caption document), optionally from a moment.",
    example = r#"{"title": "My PC made this in 67 seconds"}"#
)]
impl Command for CutCreate {
    type Args = CutCreateArgs;
    type Output = CutCreated;
    fn run(a: CutCreateArgs, cx: &mut Cx<'_>) -> Result<CutCreated, CmdError> {
        if a.title.chars().count() > 200 {
            return Err(CmdError::invalid("/title", "at most 200 characters"));
        }
        if let Some(m) = a.moment {
            cx.tx.get(m)?;
        }
        let id: Id<Cut> = cx.tx.mint();
        cx.tx.create(Cut {
            id,
            title: a.title,
            moment: a.moment,
            brief: String::new(),
            notes: String::new(),
            last_run_summary: None,
            lineage: CutLineage::default(),
            story_arc: Vec::new(),
            loop_check: None,
            kind: CutKind::Short,
        })?;
        let cd: Id<CaptionDoc> = cx.tx.mint();
        cx.tx.create(CaptionDoc {
            id: cd,
            cut: id,
            enabled: true,
            source: CaptionSource::Heard,
            edits: BTreeMap::new(),
            word_time_overrides: BTreeMap::new(),
            keywords: BTreeMap::new(),
            keyword_terms: Vec::new(),
            no_caption_ranges: BTreeMap::new(),
            cue_breaks: Default::default(),
            subline: None,
            drop_fillers: true,
        })?;
        Ok(CutCreated {
            cut: id.0.to_string(),
            caption_doc: cd.0.to_string(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FramingCreateArgs {
    pub cut: Id<Cut>,
    pub name: String,
    /// Canvas width and height in pixels (even).
    pub w: u32,
    pub h: u32,
    pub fps: Rational,
}

/// Adds a framing (an output canvas) to a cut.
#[derive(Debug)]
pub struct FramingCreate;

#[command(
    name = "framing_create",
    scope = "edit",
    undo = "undoable",
    doc = "Add a framing (canvas size and frame rate, e.g. 9:16 at 1080x1920) to a cut.",
    example = r#"{"cut": "01J00000000000000000000001", "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}"#
)]
impl Command for FramingCreate {
    type Args = FramingCreateArgs;
    type Output = Created;
    fn run(a: FramingCreateArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        cx.tx.get(a.cut)?;
        super::bounded(&a.name, "/name", 200)?;
        if a.w == 0 || a.h == 0 || a.w % 2 == 1 || a.h % 2 == 1 || a.w > 8192 || a.h > 8192 {
            return Err(CmdError::invalid("/w", "canvas sides are even, 2..=8192"));
        }
        if a.fps.num <= 0 || a.fps.den == 0 {
            return Err(CmdError::invalid("/fps", "a positive rate"));
        }
        let aspect = match a.w.cmp(&a.h) {
            std::cmp::Ordering::Less => AspectClass::Portrait,
            std::cmp::Ordering::Greater => AspectClass::Landscape,
            std::cmp::Ordering::Equal => AspectClass::Square,
        };
        let id: Id<Framing> = cx.tx.mint();
        cx.tx.create(Framing {
            id,
            cut: a.cut,
            name: a.name,
            canvas: Canvas { w: a.w, h: a.h },
            fps: a.fps,
            aspect_class: aspect,
            background: Background::Solid {
                colour: "#0c1118".into(),
            },
            caption_style_ref: None,
            caption_grouping: CaptionGrouping::StoryV5,
            caption_place: None,
            bands: BandSet::default(),
            safe_zones: "instagram".into(),
            clips_enabled: Vec::new(),
            loudness_mode: None,
            export_presets: Vec::new(),
            caption_look: None,
        })?;
        // the brand kit's defaults (caption preset, background), read as part of this edit
        let kit = cx.tx.peek().iter::<BrandKit>().next().map(|k| k.id);
        if let Some(k) = kit {
            cx.tx.get(k)?;
        }
        super::brand::kit_defaults(cx.tx.staging(), id).map_err(crate::command::model_err)?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAddArgs {
    /// Where the media is. Stored only: no file is opened here (probing and the file
    /// roots check, R-FS-1, belong to import).
    pub path: String,
    pub fingerprint: String,
    /// What a probe found (import fills it; the planner needs the picture size).
    #[serde(default)]
    pub probe: Option<Probe>,
}

/// Registers a media source.
#[derive(Debug)]
pub struct SourceAdd;

#[command(
    name = "source_add",
    scope = "edit",
    undo = "undoable",
    doc = "Register a media source by path and fingerprint (no file access; import fills in the probe).",
    example = r#"{"path": "/Users/aj/Movies/2026-10-04 stream.mkv", "fingerprint": "sha256:ab12"}"#
)]
impl Command for SourceAdd {
    type Args = SourceAddArgs;
    type Output = Created;
    fn run(a: SourceAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        if a.path.is_empty() || a.path.len() > 4096 || a.fingerprint.len() > 256 {
            return Err(CmdError::invalid(
                "/path",
                "1..=4096 bytes; fingerprint ≤ 256",
            ));
        }
        if let Some(p) = &a.probe {
            let bad_track = p.tracks.iter().any(|t| {
                t.width == Some(0)
                    || t.height == Some(0)
                    || t.fps.is_some_and(|f| f.num <= 0 || f.den == 0)
                    || t.codec.len() > 32
            });
            if p.duration.0 < 0 || p.tracks.len() > 16 || p.audio_tracks.len() > 16 || bad_track {
                return Err(CmdError::invalid(
                    "/probe",
                    "sizes and rates must be positive; at most 16 tracks",
                ));
            }
        }
        let id: Id<Source> = cx.tx.mint();
        cx.tx.create(Source {
            id,
            path: a.path,
            fingerprint: a.fingerprint,
            probe: a.probe.unwrap_or_default(),
            audio_roles: AudioRoles::default(),
            stream_layout: None,
            relink_hint: None,
            cursor: None,
            markers: Vec::new(),
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAudioRolesSetArgs {
    pub source: Id<Source>,
    /// The mic: the audio track (its index from the source's probe) that transcripts and
    /// moment-finding hear. `null` = the first audio track.
    #[serde(default)]
    pub voice: Option<u32>,
    /// Game audio / music tracks (kept, never transcribed).
    #[serde(default)]
    pub program: Vec<u32>,
    /// Tracks to ignore.
    #[serde(default)]
    pub exclude: Vec<u32>,
}

/// Says which of a source's audio tracks is the mic (SPEC §4.3 audio roles; the import
/// sheet's "Which track is your mic?").
#[derive(Debug)]
pub struct SourceAudioRolesSet;

#[command(
    name = "source_audio_roles_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set which audio track of a source is the voice (mic) that transcripts hear, and which are program audio or excluded. Tracks are the probe's audio_tracks[].index.",
    example = r#"{"source": "01J00000000000000000000002", "voice": 2, "program": [1]}"#
)]
impl Command for SourceAudioRolesSet {
    type Args = SourceAudioRolesSetArgs;
    type Output = super::Done;
    fn run(a: SourceAudioRolesSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        let src = cx.tx.get(a.source)?;
        let known: Vec<u32> = src.probe.audio_tracks.iter().map(|t| t.index).collect();
        let all = a.voice.iter().chain(&a.program).chain(&a.exclude);
        if all.clone().any(|t| !known.contains(t)) {
            return Err(CmdError::invalid(
                "/voice",
                "a track index from the source's audio_tracks",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !all.into_iter().all(|t| seen.insert(*t)) {
            return Err(CmdError::invalid("/program", "each track has one role"));
        }
        cx.tx.update(a.source, |s| {
            s.audio_roles = AudioRoles {
                voice: a.voice,
                program: a.program,
                exclude: a.exclude,
            };
        })?;
        Ok(super::Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncMemberArgs {
    pub source: Id<Source>,
    pub role: MemberRole,
    /// Member time = group time − offset (flicks). The group's clock is usually the
    /// audio member's (offset 0).
    #[serde(default)]
    pub offset: Flicks,
    /// Piecewise offsets instead (a recording whose offset changes, e.g. dropped frames):
    /// each holds from its group time `from` until the next one's; the first also covers
    /// everything before it and the last everything after.
    #[serde(default)]
    pub segments: Option<Vec<OffsetFrom>>,
    /// Hear this member's sound under the group's audio at this gain in dB, −60..=+12
    /// (decision 0051: a recording's system audio under the mic); absent = not heard.
    #[serde(default)]
    pub under_db: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OffsetFrom {
    pub from: Flicks,
    pub offset: Flicks,
}

const DAY: i64 = 24 * 3600 * 705_600_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncGroupCreateArgs {
    /// Two or more sources of the same recording ("Are these the same recording?"):
    /// e.g. the 1440p picture and the proxy with the voice.
    pub members: Vec<SyncMemberArgs>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GroupCreated {
    pub group: String,
    pub members: Vec<String>,
}

/// Groups sources that are the same recording (SPEC §4.3 sync groups): parts on the group
/// take their picture from the picture member and their audio from the audio member.
#[derive(Debug)]
pub struct SyncGroupCreate;

#[command(
    name = "sync_group_create",
    scope = "edit",
    undo = "undoable",
    doc = "Group sources that are the same recording, with a manual offset per member (member time = group time - offset): the picture member gives parts their picture, the audio member their sound.",
    example = r#"{"members": [{"source": "01J00000000000000000000002", "role": "audio", "offset": 0}, {"source": "01J00000000000000000000003", "role": "picture", "offset": 38590080000}]}"#
)]
impl Command for SyncGroupCreate {
    type Args = SyncGroupCreateArgs;
    type Output = GroupCreated;
    fn run(a: SyncGroupCreateArgs, cx: &mut Cx<'_>) -> Result<GroupCreated, CmdError> {
        if a.members.len() < 2 || a.members.len() > 8 {
            return Err(CmdError::invalid("/members", "2 to 8 sources"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, m) in a.members.iter().enumerate() {
            cx.tx.get(m.source)?;
            if !seen.insert(m.source) {
                let _ = i;
                return Err(CmdError::invalid("/members", "each source once"));
            }
            if m.offset.0.abs() > DAY {
                return Err(CmdError::invalid("/members", "offsets of at most 24 h"));
            }
            super::sound::check_under_db(m.under_db, "/members")?;
            if let Some(segs) = &m.segments {
                if segs.is_empty() || segs.len() > 64 {
                    return Err(CmdError::invalid("/members", "1 to 64 offset segments"));
                }
                if segs.windows(2).any(|w| w[1].from <= w[0].from)
                    || segs.iter().any(|x| x.offset.0.abs() > DAY)
                {
                    return Err(CmdError::invalid(
                        "/members",
                        "segments in time order, offsets of at most 24 h",
                    ));
                }
            }
        }
        let has = |r: MemberRole| {
            a.members
                .iter()
                .any(|m| m.role == r || m.role == MemberRole::Both)
        };
        if !has(MemberRole::Picture) || !has(MemberRole::Audio) {
            return Err(CmdError::invalid(
                "/members",
                "a picture member and an audio member",
            ));
        }
        let group: Id<SyncGroup> = cx.tx.mint();
        cx.tx.create(SyncGroup {
            id: group,
            master_audio: None,
            picture: None,
        })?;
        let keys = gmclip_model::OrderKey::sequence(None, a.members.len())
            .map_err(|_| CmdError::invalid("/members", "order keys"))?;
        let forever = i64::MAX / 4;
        let mut ids = Vec::new();
        let (mut pic, mut aud) = (None, None);
        for (m, k) in a.members.iter().zip(keys) {
            let id: Id<SyncMember> = cx.tx.mint();
            cx.tx.create(SyncMember {
                id,
                group,
                order: k,
                source: m.source,
                role: m.role,
                rate_ppm: 0.0,
                method: SyncMethod::Manual,
                confidence: 1.0,
                under_db: m.under_db,
            })?;
            let pieces: Vec<OffsetFrom> = m.segments.clone().unwrap_or_else(|| {
                vec![OffsetFrom {
                    from: Flicks(-forever),
                    offset: m.offset,
                }]
            });
            let okeys = gmclip_model::OrderKey::sequence(None, pieces.len())
                .map_err(|_| CmdError::invalid("/members", "order keys"))?;
            for (i, (x, ok)) in pieces.iter().zip(okeys).enumerate() {
                let seg: Id<SyncSegment> = cx.tx.mint();
                let start = if i == 0 { Flicks(-forever) } else { x.from };
                let end = pieces.get(i + 1).map_or(Flicks(forever), |n| n.from);
                cx.tx.create(SyncSegment {
                    id: seg,
                    member: id,
                    order: ok,
                    src_range: SrcRange { start, end },
                    offset: x.offset,
                })?;
            }
            if pic.is_none() && matches!(m.role, MemberRole::Picture | MemberRole::Both) {
                pic = Some(id);
            }
            if aud.is_none() && matches!(m.role, MemberRole::Audio | MemberRole::Both) {
                aud = Some(id);
            }
            ids.push(id.0.to_string());
        }
        cx.tx.update(group, |g| {
            g.picture = pic;
            g.master_audio = aud;
        })?;
        Ok(GroupCreated {
            group: group.0.to_string(),
            members: ids,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncOffsetSetArgs {
    pub member: Id<SyncMember>,
    /// Member time = group time − offset (flicks).
    pub offset: Flicks,
    /// The group time whose segment changes (needed when the offset is piecewise).
    #[serde(default)]
    pub at: Option<Flicks>,
    /// Start a new segment at `at` with this offset instead of changing the one there
    /// ("the offset changes here").
    #[serde(default)]
    pub split: bool,
}

/// Sets a sync member's manual offset (the hi-res picture a few frames early or late).
#[derive(Debug)]
pub struct SyncOffsetSet;

#[command(
    name = "sync_offset_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set a sync group member's offset (member time = group time - offset), e.g. nudge the hi-res picture into sync with the voice.",
    example = r#"{"member": "01J00000000000000000000004", "offset": 38613600000}"#
)]
impl Command for SyncOffsetSet {
    type Args = SyncOffsetSetArgs;
    type Output = super::Done;
    fn run(a: SyncOffsetSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        cx.tx.get(a.member)?;
        if a.offset.0.abs() > 24 * 3600 * 705_600_000 {
            return Err(CmdError::invalid("/offset", "at most 24 h"));
        }
        let segs: Vec<Id<SyncSegment>> = cx.tx.children::<SyncSegment>(a.member.entity());
        let target = match a.at {
            None if segs.len() == 1 => segs[0],
            None => {
                return Err(CmdError::invalid(
                    "/at",
                    "the group time whose offset changes (this member's offset is piecewise)",
                ));
            }
            Some(t) => {
                let mut hit = None;
                for s in &segs {
                    let r = cx.tx.get(*s)?.src_range;
                    if r.start <= t && t < r.end {
                        hit = Some(*s);
                    }
                }
                hit.ok_or_else(|| CmdError::invalid("/at", "a time the member covers"))?
            }
        };
        if a.split {
            let at =
                a.at.ok_or_else(|| CmdError::invalid("/at", "where the new offset starts"))?;
            let old = cx.tx.get(target)?.clone();
            if at <= old.src_range.start {
                cx.tx.update(target, |x| x.offset = a.offset)?;
                return Ok(super::Done {});
            }
            let next_order = {
                let mut later: Vec<gmclip_model::OrderKey> = Vec::new();
                for s in &segs {
                    let x = cx.tx.get(*s)?;
                    if x.order > old.order {
                        later.push(x.order.clone());
                    }
                }
                later.into_iter().min()
            };
            let order = gmclip_model::OrderKey::between(Some(&old.order), next_order.as_ref())
                .map_err(|_| CmdError::invalid("/at", "order keys"))?;
            cx.tx.update(target, |x| x.src_range.end = at)?;
            let id: Id<SyncSegment> = cx.tx.mint();
            cx.tx.create(SyncSegment {
                id,
                member: a.member,
                order,
                src_range: SrcRange {
                    start: at,
                    end: old.src_range.end,
                },
                offset: a.offset,
            })?;
        } else {
            cx.tx.update(target, |x| x.offset = a.offset)?;
        }
        Ok(super::Done {})
    }
}
