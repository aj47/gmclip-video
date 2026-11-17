//! The stream edit, its topics and fluff proposals (decision 0011, SPEC §5.17).
//!
//! - **The stream edit** is the project's one `Cut{kind: stream}`, its main cut (decision
//!   0036): every video of the project, each whole and in source order, in the order the
//!   videos were added. Striking there (`words_strike`) removes fluff for everything made
//!   from it. `stream_edit_ensure` finds it, adopts a whole-video cut as it, or builds it,
//!   and appends a video it does not play yet.
//! - **Topics** are word-edged ranges of the stream edit. Accepted topics never overlap
//!   (one level; gaps are untopiced talk). A human's topic is a decision from the start
//!   (`accepted`); labelling over part of an existing topic re-labels that part (the old
//!   topic is trimmed, split or replaced: the human's own edit, one undo step).
//! - **Agents only propose** (the R-STORY-1 rule): an agent's topic arrives `proposed`,
//!   and an agent can never move, retitle, split, merge or remove an accepted topic. A
//!   split or merge an agent wants becomes proposals naming the topics they would
//!   replace (`replaces`), applied only when the human accepts them. A proposal that
//!   overlaps human work (an accepted topic, or a stretch the human edited after the
//!   pass began, `since`) arrives flagged `conflict` and is never applied silently.
//! - **Fluff proposals** are sweep-style: `fluff_propose` lists stretches to strike,
//!   `fluff_state` accepts (strikes them, one transaction) or rejects them.
//!
//! Times: a topic's start is the source time of its start anchor (`word_start`, or
//! `word_end` of the word before it after a re-label), its end that of its end anchor.

use std::collections::BTreeSet;

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, WordRef};
use gmclip_model::entities::*;
use gmclip_model::{Author, Doc, Flicks, Id, OrderKey, Rational, Rev};
use gmclip_proto::{CallerKind, CmdError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::bounded;
use crate::command::{Command, Cx};

pub(crate) const F: i64 = 705_600_000;
/// Two spans this close at both ends are the same span (a re-proposal), 1 s.
const SAME_SPAN: i64 = F;

/// Topic colours, in the order new topics take them.
pub const PALETTE: &[&str] = &[
    "#4f8cff", "#f59e0b", "#10b981", "#ef4444", "#a855f7", "#ec4899", "#14b8a6", "#eab308",
    "#6366f1", "#f97316",
];

/// An id a command returned as a string.
pub(crate) fn id_of<E>(s: &str) -> Result<Id<E>, CmdError> {
    gmclip_model::Ulid::parse(s)
        .map(Id::new)
        .map_err(|_| CmdError::Internal {
            trace_id: "id".into(),
        })
}

pub(crate) fn is_agent(cx: &Cx<'_>) -> bool {
    cx.caller.kind == CallerKind::Agent
}

/// The project's stream edit, its main cut (decision 0036: one per project, holding
/// every video).
pub fn stream_edit(doc: &Doc) -> Option<Id<Cut>> {
    doc.iter::<Cut>()
        .find(|c| c.kind == CutKind::Stream)
        .map(|c| c.id)
}

/// The videos a cut plays, in the order its parts first play them; for a stream edit
/// also its topics' (a video whose every part is struck is still on it).
pub fn stream_medias(doc: &Doc, cut: Id<Cut>) -> Vec<MediaRef> {
    let mut out: Vec<MediaRef> = Vec::new();
    let parts = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p).map(|p| p.media));
    let topics = doc
        .children::<Topic>(cut.entity())
        .filter_map(|t| doc.get(t).map(|t| t.media));
    for m in parts.chain(topics) {
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// The stream edit, when it plays `media`.
pub fn stream_edit_for(doc: &Doc, media: MediaRef) -> Option<Id<Cut>> {
    stream_edit(doc).filter(|c| stream_medias(doc, *c).contains(&media))
}

/// The media an analysis owner id names (a source's or a sync group's id).
pub fn media_of_owner(doc: &Doc, owner: &str) -> Option<MediaRef> {
    let u = gmclip_model::Ulid::parse(owner).ok()?;
    let s: Id<Source> = Id::new(u);
    if doc.get(s).is_some() {
        return Some(MediaRef::Source(s));
    }
    let g: Id<SyncGroup> = Id::new(u);
    doc.get(g).map(|_| MediaRef::Group(g))
}

/// The source time an anchor stored on a topic or proposal names (word anchors only).
pub fn word_time(a: &Anchor) -> Option<i64> {
    match a {
        Anchor::WordStart(w) | Anchor::AudibleStart(w) => Some(w.t_src_start.0),
        Anchor::WordEnd(w) | Anchor::AudibleEnd(w) => Some(w.t_src_end.0),
        _ => None,
    }
}

/// `[start, end)` of a span on its media clock.
pub fn span_range(s: &Span) -> (i64, i64) {
    (
        word_time(&s.start).unwrap_or(0),
        word_time(&s.end).unwrap_or(0),
    )
}

fn overlaps(a: (i64, i64), b: (i64, i64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Strips what R-INJ-6 strips from untrusted text (tags, bidi controls, zero-width,
/// other controls) and trims it.
pub(crate) fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let u = *c as u32;
            !(c.is_control()
                || (0x200B..=0x200F).contains(&u)
                || (0x202A..=0x202E).contains(&u)
                || (0x2066..=0x2069).contains(&u)
                || u == 0xFEFF
                || (0xE0000..=0xE007F).contains(&u))
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn check_colour(c: &str) -> Result<(), CmdError> {
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(CmdError::invalid("/colour", "#rrggbb"))
    }
}

/// A word reference grounded on the transcript when the holder has it: its times and
/// text are the transcript's, whatever the caller sent (word-edged anchors).
pub(crate) fn grounded(cx: &Cx<'_>, w: &WordRef, path: &str) -> Result<WordRef, CmdError> {
    bounded(&w.word_id.asr_revision, path, 128)?;
    bounded(&w.normalised_text, path, 200)?;
    if w.prev2.len() > 4 || w.next2.len() > 4 {
        return Err(CmdError::invalid(path, "prev2/next2: at most 4 words"));
    }
    if let Some((_, word)) = cx.analysis.word(w) {
        return Ok(WordRef {
            t_src_start: Flicks(word.start),
            t_src_end: Flicks(word.end),
            normalised_text: crate::analysis::norm(&word.text),
            ..w.clone()
        });
    }
    if w.t_src_start.0 < 0 || w.t_src_end <= w.t_src_start {
        return Err(CmdError::invalid(path, "a word that ends after it starts"));
    }
    Ok(w.clone())
}

/// The video a stretch of the stream edit starting at word `w` is on: the video whose
/// transcript the word is from, else the stream edit's only video. Source times alone do
/// not say it: every video's clock starts at 0 (decision 0036).
fn media_for(cx: &Cx<'_>, cut: Id<Cut>, w: Option<&WordRef>) -> Result<MediaRef, CmdError> {
    let doc = cx.tx.peek();
    if let Some(m) = w
        .and_then(|w| cx.analysis.word(w))
        .and_then(|(owner, _)| media_of_owner(doc, owner))
    {
        return Ok(m);
    }
    match stream_medias(doc, cut).as_slice() {
        [] => Err(CmdError::invalid("/cut", "the stream edit has no parts")),
        [m] => Ok(*m),
        _ => Err(CmdError::invalid(
            "/media",
            "the stream edit plays several videos: name the media",
        )),
    }
}

/// Did the human edit the stream edit over `r` after rev `since`: a part the human
/// wrote since has an edge (a strike's or restore's cut) inside `r` (±0.5 s)? A pass's
/// proposal over such a stretch arrives flagged. Reads unrecorded: a flag is not a
/// refusal, so the human's edit must not turn into a `Conflict` for the agent.
fn human_edited_after(doc: &Doc, cut: Id<Cut>, media: MediaRef, r: (i64, i64), since: Rev) -> bool {
    let (lo, hi) = (r.0 - F / 2, r.1 + F / 2);
    doc.children::<Part>(cut.entity()).any(|id| {
        let Some(slot) = doc.slot(id) else {
            return false;
        };
        let p = &slot.e;
        p.media == media
            && slot.by == Author::Human
            && slot.rev > since
            && [p.src_in.0, p.src_out.0]
                .iter()
                .any(|t| lo <= *t && *t <= hi)
    })
}

/// The topics of the stream edit, with their ranges (unrecorded peek).
fn topics_of(doc: &Doc, cut: Id<Cut>) -> Vec<(Id<Topic>, Topic, (i64, i64))> {
    doc.children::<Topic>(cut.entity())
        .filter_map(|id| doc.get(id).map(|t| (id, t.clone(), span_range(&t.span))))
        .collect()
}

/// An order key placing a topic starting at `t0` among the cut's topics.
fn order_for(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    t0: i64,
    skip: Option<Id<Topic>>,
) -> Result<OrderKey, CmdError> {
    let ids = cx.tx.children::<Topic>(cut.entity());
    let doc = cx.tx.peek();
    let sib: Vec<(OrderKey, i64)> = ids
        .iter()
        .filter(|id| Some(**id) != skip)
        .filter_map(|id| {
            doc.get(*id)
                .map(|t| (t.order.clone(), span_range(&t.span).0))
        })
        .collect();
    let at = sib.iter().position(|(_, s)| *s > t0).unwrap_or(sib.len());
    let lo = at.checked_sub(1).map(|i| &sib[i].0);
    let hi = sib.get(at).map(|x| &x.0);
    OrderKey::between(lo, hi).map_err(|_| CmdError::invalid("/start", "no order key fits"))
}

fn next_colour(doc: &Doc, cut: Id<Cut>, avoid: Option<&str>) -> String {
    let n = doc.children::<Topic>(cut.entity()).count();
    let mut i = n % PALETTE.len();
    if avoid == Some(PALETTE[i]) {
        i = (i + 1) % PALETTE.len();
    }
    PALETTE[i].to_owned()
}

/// The stream edit a topic command works on: the one named (which must be it), else the
/// project's.
fn the_stream(cx: &mut Cx<'_>, cut: Option<Id<Cut>>) -> Result<Id<Cut>, CmdError> {
    let id = match cut {
        Some(c) => c,
        None => stream_edit(cx.tx.peek()).ok_or_else(|| {
            CmdError::invalid(
                "/cut",
                "this project has no stream edit yet (stream_edit_ensure)",
            )
        })?,
    };
    if cx.tx.get(id)?.kind != CutKind::Stream {
        return Err(CmdError::invalid("/cut", "topics live on the stream edit"));
    }
    Ok(id)
}

fn own_proposal(t: &Topic) -> bool {
    t.origin == EditOrigin::Agent && t.state == ProposalState::Proposed
}

const HUMANS_TOPIC: &str =
    "an accepted topic is the human's: propose a new topic, a split or a merge instead";

// ---------------------------------------------------------------------------------------
// stream_edit_ensure
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamEditEnsureArgs {
    /// A video (source or sync group) the stream edit must play: appended, whole, after
    /// its last part when it does not yet. The project's only video when absent.
    #[serde(default)]
    pub media: Option<MediaRef>,
    /// Make this whole-video cut the stream edit (a project without one yet).
    #[serde(default)]
    pub adopt: Option<Id<Cut>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StreamEditEnsured {
    pub cut: String,
    /// `existing`, `adopted`, `created` or `appended`.
    pub how: String,
    pub parts: usize,
    /// The parts `appended` added (the video, whole).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
}

#[derive(Debug)]
pub struct StreamEditEnsure;

#[command(
    name = "stream_edit_ensure",
    scope = "edit",
    undo = "undoable",
    doc = "Make sure the project has its stream edit, the main cut holding every video (each whole, in source order, in the order the videos were added), where topics are labelled and fluff struck for every long-form and short made from it. With `media`, a video it does not play yet is appended to it.",
    example = r#"{"media": {"source": "01J00000000000000000000002"}}"#
)]
impl Command for StreamEditEnsure {
    type Args = StreamEditEnsureArgs;
    type Output = StreamEditEnsured;
    fn run(a: StreamEditEnsureArgs, cx: &mut Cx<'_>) -> Result<StreamEditEnsured, CmdError> {
        let _ = cx.tx.peek().iter::<Cut>().count();
        let main = stream_edit(cx.tx.peek());
        if let Some(c) = main {
            cx.tx.get(c)?;
            if a.adopt.is_some_and(|x| x != c) {
                return Err(CmdError::invalid(
                    "/adopt",
                    "the project has its stream edit: add a video to it with `media`",
                ));
            }
            let media = match a.media {
                Some(m) if !stream_medias(cx.tx.peek(), c).contains(&m) => m,
                _ => {
                    let parts = cx.tx.children::<Part>(c.entity()).len();
                    return Ok(StreamEditEnsured {
                        cut: c.0.to_string(),
                        how: "existing".into(),
                        parts,
                        added: Vec::new(),
                    });
                }
            };
            let last = cx.tx.children::<Part>(c.entity()).last().copied();
            let added = add_whole(cx, c, media, last)?;
            let parts = cx.tx.children::<Part>(c.entity()).len();
            return Ok(StreamEditEnsured {
                cut: c.0.to_string(),
                how: "appended".into(),
                parts,
                added: added.iter().map(|p| p.0.to_string()).collect(),
            });
        }
        if let Some(c) = a.adopt {
            let cut = cx.tx.get(c)?.clone();
            if cut.kind != CutKind::Short || cut.lineage.forked_from.is_some() {
                return Err(CmdError::invalid(
                    "/adopt",
                    "a whole-video cut, not a derived one",
                ));
            }
            let parts = cx.tx.children::<Part>(c.entity());
            if parts.is_empty() {
                return Err(CmdError::invalid("/adopt", "a cut with parts"));
            }
            cx.tx.update(c, |x| x.kind = CutKind::Stream)?;
            return Ok(StreamEditEnsured {
                cut: c.0.to_string(),
                how: "adopted".into(),
                parts: parts.len(),
                added: Vec::new(),
            });
        }
        let media = match a.media {
            Some(m) => m,
            None => only_media(cx.tx.peek())?,
        };
        let (_, pic, fps) = whole_media(cx, media)?;
        let title = match media {
            MediaRef::Source(s) => file_title(&cx.tx.get(s)?.path),
            MediaRef::Group(_) => "Stream".to_owned(),
        };
        let made = super::cut::CutCreate::run(
            super::cut::CutCreateArgs {
                title,
                moment: None,
            },
            cx,
        )?;
        let cut: Id<Cut> = id_of(&made.cut)?;
        cx.tx.update(cut, |c| c.kind = CutKind::Stream)?;
        if let Some(cd) = cx.tx.children::<CaptionDoc>(cut.entity()).first() {
            cx.tx.update(*cd, |c| c.enabled = false)?;
        }
        let (w, h) = canvas_for(pic, 1920);
        let fr = super::cut::FramingCreate::run(
            super::cut::FramingCreateArgs {
                cut,
                name: "16:9".into(),
                w,
                h,
                fps,
            },
            cx,
        )?;
        let framing: Id<Framing> = id_of(&fr.id)?;
        super::look::LayerAdd::run(
            super::look::LayerAddArgs {
                owner: LayerOwner::Framing(framing),
                role: LayerRole::Region(RegionRole::Screen),
                kind: None,
                member: None,
                crop: gmclip_model::geometry::NormRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                place: gmclip_model::geometry::Place {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                },
                z: 0,
                cover: None,
            },
            cx,
        )?;
        let added = add_whole(cx, cut, media, None)?;
        Ok(StreamEditEnsured {
            cut: cut.0.to_string(),
            how: "created".into(),
            parts: added.len(),
            added: Vec::new(),
        })
    }
}

/// A whole video as parts of `cut` (one per stretch between picture offset changes),
/// after `after` (first when `None`); the cut's first part is its hook.
fn add_whole(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    media: MediaRef,
    mut after: Option<Id<Part>>,
) -> Result<Vec<Id<Part>>, CmdError> {
    let (pieces, _, _) = whole_media(cx, media)?;
    let first = after.is_none() && cx.tx.children::<Part>(cut.entity()).is_empty();
    let mut out = Vec::new();
    for (i, (s, e)) in pieces.iter().enumerate() {
        let p = super::part::PartAdd::run(
            super::part::PartAddArgs {
                cut,
                after,
                media,
                src_in: Flicks(*s),
                src_out: Flicks(*e),
                role: Some(if first && i == 0 {
                    PartRole::Hook
                } else {
                    PartRole::Other
                }),
            },
            cx,
        )?;
        let id = id_of(&p.id)?;
        out.push(id);
        after = Some(id);
    }
    Ok(out)
}

fn file_title(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let t: String = stem.chars().take(200).collect();
    if t.trim().is_empty() {
        "Stream".into()
    } else {
        t
    }
}

/// The project's only stream-shaped media: its one sync group, else its one source.
fn only_media(doc: &Doc) -> Result<MediaRef, CmdError> {
    let groups: Vec<Id<SyncGroup>> = doc.iter::<SyncGroup>().map(|g| g.id).collect();
    if groups.len() == 1 {
        return Ok(MediaRef::Group(groups[0]));
    }
    let sources: Vec<Id<Source>> = doc.iter::<Source>().map(|s| s.id).collect();
    if groups.is_empty() && sources.len() == 1 {
        return Ok(MediaRef::Source(sources[0]));
    }
    Err(CmdError::invalid("/media", "name the source or sync group"))
}

/// A canvas of the picture's shape at most `max_w` wide (even sides), 1920×1080 when
/// the probe has no size.
pub(crate) fn canvas_for(pic: Option<(u32, u32)>, max_w: u32) -> (u32, u32) {
    let Some((w, h)) = pic.filter(|(w, h)| *w > 0 && *h > 0) else {
        return (1920, 1080);
    };
    let (w, h) = if w > max_w {
        (
            max_w,
            (u64::from(h) * u64::from(max_w) / u64::from(w)) as u32,
        )
    } else {
        (w, h)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

fn video_of(src: &Source) -> (Option<(u32, u32)>, Rational) {
    let v = src.probe.tracks.iter().find(|t| t.kind == TrackKind::Video);
    let dims = v.and_then(|t| Some((t.width?, t.height?)));
    // an exported rate, NTSC exactly (decision 0020); a picture at another converts
    // (decision 0017)
    let fps = v.and_then(|t| t.fps).map_or(Rational::new(30, 1), |f| {
        crate::planner::output_rate_for(f.num, f.den)
    });
    (dims, fps)
}

/// On the output frame grid (`fpf` flicks a frame, exact for every exported rate, NTSC
/// included): the first frame at or after `t` / at or before `t`.
fn grid_up(t: i64, fpf: i64) -> i64 {
    let f = fpf.max(1);
    t.div_euclid(f) * f + if t.rem_euclid(f) != 0 { f } else { 0 }
}

fn grid_down(t: i64, fpf: i64) -> i64 {
    let f = fpf.max(1);
    t.div_euclid(f) * f
}

/// The whole of a source or sync group as parts (one per stretch between picture offset
/// changes, on the frame grid), its picture size and frame rate.
#[allow(clippy::type_complexity)]
fn whole_media(
    cx: &mut Cx<'_>,
    media: MediaRef,
) -> Result<(Vec<(i64, i64)>, Option<(u32, u32)>, Rational), CmdError> {
    let (raw, pic, fps): (Vec<(i64, i64)>, _, _) = match media {
        MediaRef::Source(s) => {
            let src = cx.tx.get(s)?.clone();
            let (dims, fps) = video_of(&src);
            (vec![(0, src.probe.duration.0)], dims, fps)
        }
        MediaRef::Group(g) => {
            let grp = cx.tx.get(g)?.clone();
            let (Some(am), Some(pm)) = (grp.master_audio, grp.picture) else {
                return Err(CmdError::invalid(
                    "/media",
                    "a sync group with picture and audio",
                ));
            };
            let audio = cx.tx.get(am)?.source;
            let picture = cx.tx.get(pm)?.source;
            let a_dur = cx.tx.get(audio)?.probe.duration.0;
            let psrc = cx.tx.get(picture)?.clone();
            let (dims, fps) = video_of(&psrc);
            let mut out = Vec::new();
            for seg in cx.tx.children::<SyncSegment>(pm.entity()) {
                let sg = cx.tx.get(seg)?;
                // member time = group time − offset: the picture covers group time
                // [offset, offset + its duration)
                let lo = sg.src_range.start.0.max(sg.offset.0).max(0);
                let hi = sg
                    .src_range
                    .end
                    .0
                    .min(sg.offset.0.saturating_add(psrc.probe.duration.0))
                    .min(a_dur);
                if hi > lo {
                    out.push((lo, hi));
                }
            }
            (out, dims, fps)
        }
    };
    let fpf = crate::planner::framing_fpf(fps).unwrap_or(F / 30);
    let pieces: Vec<(i64, i64)> = raw
        .into_iter()
        .map(|(a, b)| (grid_up(a, fpf), grid_down(b, fpf)))
        .filter(|(a, b)| b - a >= super::part::MIN_PART.0)
        .collect();
    if pieces.is_empty() {
        return Err(CmdError::invalid(
            "/media",
            "nothing to cover yet (the source has no probed duration)",
        ));
    }
    Ok((pieces, pic, fps))
}

// ---------------------------------------------------------------------------------------
// topic_set
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicSetArgs {
    /// The stream edit (default: the project's).
    #[serde(default)]
    pub cut: Option<Id<Cut>>,
    /// The topic to change; a new one when absent (then `start`, `end`, `title`).
    #[serde(default)]
    pub topic: Option<Id<Topic>>,
    /// The first word of the topic.
    #[serde(default)]
    pub start: Option<WordRef>,
    /// The last word of the topic.
    #[serde(default)]
    pub end: Option<WordRef>,
    /// ≤ 120 characters.
    #[serde(default)]
    pub title: Option<String>,
    /// `#rrggbb` (default: the next palette colour).
    #[serde(default)]
    pub colour: Option<String>,
    /// ≤ 2 000 characters.
    #[serde(default)]
    pub notes: Option<String>,
    /// The source or sync group (default: the stream edit's media at `start`).
    #[serde(default)]
    pub media: Option<MediaRef>,
    /// Agents: the `as_of` the pass began at. Stretches the human edited after it are
    /// "conflicts with your edit".
    #[serde(default)]
    pub since: Option<Rev>,
    /// Agents: the pass that proposes it (`topics_fluff@1:<hash>`).
    #[serde(default)]
    pub pass_ref: Option<String>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct TopicSetOut {
    pub topic: String,
    /// `accepted` (a human's), `proposed` (an agent's) or `rejected` (proposed before
    /// and rejected: nothing created).
    pub state: String,
    pub conflict: bool,
    /// Human re-labels: topics trimmed, split (the new tail) or replaced.
    pub trimmed: Vec<String>,
    pub split_off: Vec<String>,
    pub removed: Vec<String>,
    /// Proposals this human edit now conflicts with.
    pub flagged: Vec<String>,
}

#[derive(Debug)]
pub struct TopicSet;

#[command(
    name = "topic_set",
    scope = "edit",
    undo = "undoable",
    doc = "Label a topic on the stream edit (a word-edged range: first word to last word, with a title), or retitle, recolour or move the edges of one. An agent's topic is only proposed and an agent never changes an accepted topic.",
    example = r#"{"start": {"word_id": {"asr_revision": "r1", "ordinal": 120}, "t_src_start": 84672000000, "t_src_end": 84966000000, "normalised_text": "so"}, "end": {"word_id": {"asr_revision": "r1", "ordinal": 940}, "t_src_start": 762048000000, "t_src_end": 762342000000, "normalised_text": "prices"}, "title": "GPU prices"}"#
)]
impl Command for TopicSet {
    type Args = TopicSetArgs;
    type Output = TopicSetOut;
    fn run(a: TopicSetArgs, cx: &mut Cx<'_>) -> Result<TopicSetOut, CmdError> {
        let title = a.title.as_deref().map(clean_text);
        if let Some(t) = &title {
            bounded(t, "/title", 120)?;
            if t.is_empty() {
                return Err(CmdError::invalid("/title", "a title"));
            }
        }
        let notes = a.notes.as_deref().map(clean_text);
        if let Some(n) = &notes {
            bounded(n, "/notes", 2_000)?;
        }
        if let Some(c) = &a.colour {
            check_colour(c)?;
        }
        if let Some(p) = &a.pass_ref {
            bounded(p, "/pass_ref", 120)?;
        }
        let start = a
            .start
            .as_ref()
            .map(|w| grounded(cx, w, "/start"))
            .transpose()?;
        let end = a
            .end
            .as_ref()
            .map(|w| grounded(cx, w, "/end"))
            .transpose()?;
        match a.topic {
            None => {
                let cut = the_stream(cx, a.cut)?;
                let (Some(s), Some(e), Some(title)) = (start, end, title) else {
                    return Err(CmdError::invalid(
                        "/start",
                        "a new topic needs start, end and title",
                    ));
                };
                let span = Span {
                    start: Anchor::WordStart(s),
                    end: Anchor::WordEnd(e),
                };
                create(
                    cx, cut, span, title, a.colour, notes, a.media, a.since, a.pass_ref,
                )
            }
            Some(id) => {
                let t = cx.tx.get(id)?.clone();
                if a.cut.is_some_and(|c| c != t.cut) {
                    return Err(CmdError::invalid("/topic", "a topic of that cut"));
                }
                if is_agent(cx) && !own_proposal(&t) {
                    return Err(CmdError::invalid("/topic", HUMANS_TOPIC));
                }
                let mut span = t.span.clone();
                if let Some(s) = start {
                    span.start = Anchor::WordStart(s);
                }
                if let Some(e) = end {
                    span.end = Anchor::WordEnd(e);
                }
                let r = span_range(&span);
                if r.1 <= r.0 {
                    return Err(CmdError::invalid("/end", "the end after the start"));
                }
                let mut out = TopicSetOut {
                    topic: id.0.to_string(),
                    state: state_name(t.state).into(),
                    conflict: t.conflict,
                    ..TopicSetOut::default()
                };
                let moved = span != t.span;
                if moved {
                    let others: Vec<_> = topics_of(cx.tx.peek(), t.cut)
                        .into_iter()
                        .filter(|(o, x, _)| *o != id && x.media == t.media)
                        .collect();
                    if t.state == ProposalState::Accepted {
                        // a dragged edge pushes the neighbour's edge; it never swallows it
                        let b = Some(&span);
                        relabel_over(cx, &others, r, b, &mut out, false)?;
                    } else if is_agent(cx) {
                        out.conflict = proposal_conflicts(
                            cx,
                            t.cut,
                            t.media,
                            r,
                            &others,
                            &t.replaces,
                            a.since,
                        );
                    }
                    let order = if r.0 == span_range(&t.span).0 {
                        t.order.clone()
                    } else {
                        order_for(cx, t.cut, r.0, Some(id))?
                    };
                    let conflict = out.conflict;
                    cx.tx.update(id, |x| {
                        x.span = span;
                        x.order = order;
                        x.conflict = conflict;
                    })?;
                }
                cx.tx.update(id, |x| {
                    if let Some(t) = title {
                        x.title = t;
                    }
                    if let Some(c) = a.colour {
                        x.colour = c;
                    }
                    if let Some(n) = notes {
                        x.notes = n;
                    }
                })?;
                Ok(out)
            }
        }
    }
}

fn state_name(s: ProposalState) -> &'static str {
    match s {
        ProposalState::Proposed => "proposed",
        ProposalState::Accepted => "accepted",
        ProposalState::Rejected => "rejected",
    }
}

/// Does a proposal over `r` conflict with human work: an accepted topic (other than the
/// ones it would replace), or the stream edit's parts edited by the human after `since`?
fn proposal_conflicts(
    cx: &Cx<'_>,
    cut: Id<Cut>,
    media: MediaRef,
    r: (i64, i64),
    others: &[(Id<Topic>, Topic, (i64, i64))],
    replaces: &[Id<Topic>],
    since: Option<Rev>,
) -> bool {
    others.iter().any(|(id, x, xr)| {
        x.state == ProposalState::Accepted
            && x.media == media
            && !replaces.contains(id)
            && overlaps(*xr, r)
    }) || since.is_some_and(|s| human_edited_after(cx.tx.peek(), cut, media, r, s))
}

/// A human labels `r`: accepted topics under it are trimmed, split or replaced (with
/// `strict`, a topic `r` would swallow whole is refused instead), and live proposals
/// under it are flagged.
fn relabel_over(
    cx: &mut Cx<'_>,
    others: &[(Id<Topic>, Topic, (i64, i64))],
    r: (i64, i64),
    span: Option<&Span>,
    out: &mut TopicSetOut,
    may_remove: bool,
) -> Result<(), CmdError> {
    let Some(span) = span else { return Ok(()) };
    for (oid, x, xr) in others {
        if !overlaps(*xr, r) {
            continue;
        }
        match x.state {
            ProposalState::Rejected => {}
            ProposalState::Proposed => {
                if !x.conflict {
                    cx.tx.update(*oid, |p| p.conflict = true)?;
                }
                out.flagged.push(oid.0.to_string());
            }
            ProposalState::Accepted => {
                let covers_start = r.0 <= xr.0;
                let covers_end = xr.1 <= r.1;
                match (covers_start, covers_end) {
                    (true, true) => {
                        if !may_remove {
                            return Err(CmdError::invalid(
                                "/end",
                                "that would cover a whole neighbouring topic: move it or remove it first",
                            ));
                        }
                        cx.tx.delete(*oid)?;
                        out.removed.push(oid.0.to_string());
                    }
                    (false, true) => {
                        // the old topic now ends where the new one starts
                        let end = flip_to_end(&span.start);
                        cx.tx.update(*oid, |t| t.span.end = end)?;
                        out.trimmed.push(oid.0.to_string());
                    }
                    (true, false) => {
                        let start = flip_to_start(&span.end);
                        let order = order_for(cx, x.cut, r.1, Some(*oid))?;
                        cx.tx.update(*oid, |t| {
                            t.span.start = start;
                            t.order = order;
                        })?;
                        out.trimmed.push(oid.0.to_string());
                    }
                    (false, false) => {
                        if !may_remove {
                            return Err(CmdError::invalid(
                                "/start",
                                "a topic's edges stay outside its neighbours",
                            ));
                        }
                        // inside: the old topic keeps its head, its tail becomes a new
                        // topic with the same title and colour
                        let end = flip_to_end(&span.start);
                        let tail_start = flip_to_start(&span.end);
                        let tail_end = x.span.end.clone();
                        cx.tx.update(*oid, |t| t.span.end = end)?;
                        let order = order_for(cx, x.cut, r.1, None)?;
                        let id: Id<Topic> = cx.tx.mint();
                        cx.tx.create(Topic {
                            id,
                            order,
                            span: Span {
                                start: tail_start,
                                end: tail_end,
                            },
                            ..x.clone()
                        })?;
                        out.trimmed.push(oid.0.to_string());
                        out.split_off.push(id.0.to_string());
                    }
                }
            }
        }
    }
    Ok(())
}

/// The anchor ending a topic where `a` (a start anchor) begins: the same instant.
fn flip_to_end(a: &Anchor) -> Anchor {
    a.clone()
}

/// The anchor starting a topic where `a` (an end anchor) ends: the same instant.
fn flip_to_start(a: &Anchor) -> Anchor {
    a.clone()
}

#[allow(clippy::too_many_arguments)]
fn create(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    span: Span,
    title: String,
    colour: Option<String>,
    notes: Option<String>,
    media: Option<MediaRef>,
    since: Option<Rev>,
    pass_ref: Option<String>,
) -> Result<TopicSetOut, CmdError> {
    let r = span_range(&span);
    if r.1 <= r.0 {
        return Err(CmdError::invalid("/end", "the last word after the first"));
    }
    let media = match media {
        Some(m) => m,
        None => {
            let w = match &span.start {
                Anchor::WordStart(w) | Anchor::AudibleStart(w) => Some(w.clone()),
                _ => None,
            };
            media_for(cx, cut, w.as_ref())?
        }
    };
    let agent = is_agent(cx);
    let others: Vec<_> = topics_of(cx.tx.peek(), cut)
        .into_iter()
        .filter(|(_, x, _)| x.media == media)
        .collect();
    let mut out = TopicSetOut::default();
    if agent {
        // a span proposed before and rejected stays rejected (R-STORY-1)
        if let Some((rid, _, _)) = others.iter().find(|(_, x, xr)| {
            x.state == ProposalState::Rejected
                && (xr.0 - r.0).abs() <= SAME_SPAN
                && (xr.1 - r.1).abs() <= SAME_SPAN
        }) {
            out.topic = rid.0.to_string();
            out.state = "rejected".into();
            return Ok(out);
        }
        if others
            .iter()
            .any(|(_, x, xr)| x.state == ProposalState::Proposed && overlaps(*xr, r))
        {
            return Err(CmdError::invalid(
                "/start",
                "overlaps another proposed topic: change or withdraw that one",
            ));
        }
        out.conflict = proposal_conflicts(cx, cut, media, r, &others, &[], since);
    } else {
        relabel_over(cx, &others, r, Some(&span), &mut out, true)?;
    }
    let order = order_for(cx, cut, r.0, None)?;
    let colour = colour.unwrap_or_else(|| next_colour(cx.tx.peek(), cut, None));
    let id: Id<Topic> = cx.tx.mint();
    cx.tx.create(Topic {
        id,
        cut,
        order,
        media,
        span,
        title,
        colour,
        origin: if agent {
            EditOrigin::Agent
        } else {
            EditOrigin::Human
        },
        state: if agent {
            ProposalState::Proposed
        } else {
            ProposalState::Accepted
        },
        notes: notes.unwrap_or_default(),
        conflict: out.conflict,
        replaces: Vec::new(),
        pass_ref: if agent { pass_ref } else { None },
    })?;
    out.topic = id.0.to_string();
    out.state = if agent { "proposed" } else { "accepted" }.into();
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// topic_split / topic_merge
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicSplitArgs {
    pub topic: Id<Topic>,
    /// The first word of the second half (the caret).
    pub at: WordRef,
    /// The last word of the first half (default: the first half ends where `at` starts).
    #[serde(default)]
    pub before: Option<WordRef>,
    /// The second half's title (default: the same title).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub since: Option<Rev>,
    #[serde(default)]
    pub pass_ref: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TopicSplitOut {
    /// Applied: the two topics (the first keeps the id). Proposed (an agent on an
    /// accepted topic): the two proposals.
    pub topics: Vec<String>,
    pub proposed: bool,
}

#[derive(Debug)]
pub struct TopicSplit;

#[command(
    name = "topic_split",
    scope = "edit",
    undo = "undoable",
    doc = "Split a topic in two at a word (the caret): the first half keeps the topic, the second becomes a new one. An agent splitting an accepted topic only proposes the split; it applies when the human accepts it.",
    example = r#"{"topic": "01J00000000000000000000007", "at": {"word_id": {"asr_revision": "r1", "ordinal": 500}, "t_src_start": 352800000000, "t_src_end": 353094000000, "normalised_text": "anyway"}}"#
)]
impl Command for TopicSplit {
    type Args = TopicSplitArgs;
    type Output = TopicSplitOut;
    fn run(a: TopicSplitArgs, cx: &mut Cx<'_>) -> Result<TopicSplitOut, CmdError> {
        let t = cx.tx.get(a.topic)?.clone();
        if t.state == ProposalState::Rejected {
            return Err(CmdError::invalid("/topic", "a rejected proposal"));
        }
        let at = grounded(cx, &a.at, "/at")?;
        let before = a
            .before
            .as_ref()
            .map(|w| grounded(cx, w, "/before"))
            .transpose()?;
        let title2 = match a.title.as_deref().map(clean_text) {
            Some(x) if x.is_empty() => return Err(CmdError::invalid("/title", "a title")),
            Some(x) => {
                bounded(&x, "/title", 120)?;
                x
            }
            None => t.title.clone(),
        };
        let (t0, t1) = span_range(&t.span);
        let cut_at = at.t_src_start.0;
        if cut_at <= t0 || cut_at >= t1 {
            return Err(CmdError::invalid(
                "/at",
                "a word inside the topic, after its first",
            ));
        }
        let head_end = match before {
            Some(b) if b.t_src_end.0 <= cut_at && b.t_src_end.0 > t0 => Anchor::WordEnd(b),
            Some(_) => return Err(CmdError::invalid("/before", "the word before `at`")),
            None => Anchor::WordStart(at.clone()),
        };
        let tail_start = Anchor::WordStart(at);
        let colour2 = next_colour(cx.tx.peek(), t.cut, Some(&t.colour));
        if is_agent(cx) && !own_proposal(&t) {
            // a proposed split: two proposals replacing the topic, applied on accept
            let others: Vec<_> = topics_of(cx.tx.peek(), t.cut)
                .into_iter()
                .filter(|(o, x, _)| *o != a.topic && x.media == t.media)
                .collect();
            let conflict =
                proposal_conflicts(cx, t.cut, t.media, (t0, t1), &others, &[a.topic], a.since);
            let mut ids = Vec::new();
            for (span, title, colour) in [
                (
                    Span {
                        start: t.span.start.clone(),
                        end: head_end,
                    },
                    t.title.clone(),
                    t.colour.clone(),
                ),
                (
                    Span {
                        start: tail_start,
                        end: t.span.end.clone(),
                    },
                    title2,
                    colour2,
                ),
            ] {
                let r = span_range(&span);
                let order = order_for(cx, t.cut, r.0, None)?;
                let id: Id<Topic> = cx.tx.mint();
                cx.tx.create(Topic {
                    id,
                    cut: t.cut,
                    order,
                    media: t.media,
                    span,
                    title,
                    colour,
                    origin: EditOrigin::Agent,
                    state: ProposalState::Proposed,
                    notes: String::new(),
                    conflict,
                    replaces: vec![a.topic],
                    pass_ref: a.pass_ref.clone(),
                })?;
                ids.push(id.0.to_string());
            }
            return Ok(TopicSplitOut {
                topics: ids,
                proposed: true,
            });
        }
        let tail_end = t.span.end.clone();
        cx.tx.update(a.topic, |x| x.span.end = head_end)?;
        let order = order_for(cx, t.cut, cut_at, None)?;
        let id: Id<Topic> = cx.tx.mint();
        cx.tx.create(Topic {
            id,
            order,
            span: Span {
                start: tail_start,
                end: tail_end,
            },
            title: title2,
            colour: colour2,
            ..t.clone()
        })?;
        Ok(TopicSplitOut {
            topics: vec![a.topic.0.to_string(), id.0.to_string()],
            proposed: false,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicMergeArgs {
    /// Two or more topics of the stream edit with no other topic between them.
    pub topics: Vec<Id<Topic>>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub since: Option<Rev>,
    #[serde(default)]
    pub pass_ref: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TopicMergeOut {
    /// The merged topic (the earliest keeps its id), or the proposal.
    pub topic: String,
    pub proposed: bool,
    pub removed: Vec<String>,
}

#[derive(Debug)]
pub struct TopicMerge;

#[command(
    name = "topic_merge",
    scope = "edit",
    undo = "undoable",
    doc = "Merge neighbouring topics into one spanning them all (the earliest keeps its id and, by default, its title). An agent merging accepted topics only proposes the merge; it applies when the human accepts it.",
    example = r#"{"topics": ["01J00000000000000000000007", "01J00000000000000000000008"], "title": "Local AI video"}"#
)]
impl Command for TopicMerge {
    type Args = TopicMergeArgs;
    type Output = TopicMergeOut;
    fn run(a: TopicMergeArgs, cx: &mut Cx<'_>) -> Result<TopicMergeOut, CmdError> {
        if a.topics.len() < 2 || a.topics.len() > 50 {
            return Err(CmdError::invalid("/topics", "2 to 50 topics"));
        }
        let mut ts = Vec::new();
        for id in &a.topics {
            let t = cx.tx.get(*id)?.clone();
            if t.state == ProposalState::Rejected {
                return Err(CmdError::invalid("/topics", "a rejected proposal"));
            }
            let r = span_range(&t.span);
            ts.push((*id, t, r));
        }
        ts.sort_by_key(|x| x.2.0);
        let (cut, media) = (ts[0].1.cut, ts[0].1.media);
        if ts.iter().any(|x| x.1.cut != cut || x.1.media != media) {
            return Err(CmdError::invalid("/topics", "topics of one stream"));
        }
        let ids: BTreeSet<Id<Topic>> = ts.iter().map(|x| x.0).collect();
        if ids.len() != ts.len() {
            return Err(CmdError::invalid("/topics", "each topic once"));
        }
        let lo = ts[0].2.0;
        let hi = ts.iter().map(|x| x.2.1).max().unwrap_or(lo);
        let others: Vec<_> = topics_of(cx.tx.peek(), cut)
            .into_iter()
            .filter(|(o, x, _)| !ids.contains(o) && x.media == media)
            .collect();
        if others
            .iter()
            .any(|(_, x, xr)| x.state == ProposalState::Accepted && overlaps(*xr, (lo, hi)))
        {
            return Err(CmdError::invalid(
                "/topics",
                "another topic lies between them",
            ));
        }
        let title = match a.title.as_deref().map(clean_text) {
            Some(x) if x.is_empty() => return Err(CmdError::invalid("/title", "a title")),
            Some(x) => {
                bounded(&x, "/title", 120)?;
                x
            }
            None => ts[0].1.title.clone(),
        };
        let first = &ts[0];
        let last_end = ts
            .iter()
            .max_by_key(|x| x.2.1)
            .map(|x| x.1.span.end.clone())
            .unwrap_or_else(|| first.1.span.end.clone());
        let all_own = ts.iter().all(|x| own_proposal(&x.1));
        if is_agent(cx) && !all_own {
            if ts.iter().any(|x| own_proposal(&x.1)) {
                return Err(CmdError::invalid(
                    "/topics",
                    "merge your own proposals separately from accepted topics",
                ));
            }
            let replaces: Vec<Id<Topic>> = ts.iter().map(|x| x.0).collect();
            let conflict =
                proposal_conflicts(cx, cut, media, (lo, hi), &others, &replaces, a.since);
            let order = order_for(cx, cut, lo, None)?;
            let id: Id<Topic> = cx.tx.mint();
            cx.tx.create(Topic {
                id,
                cut,
                order,
                media,
                span: Span {
                    start: first.1.span.start.clone(),
                    end: last_end,
                },
                title,
                colour: first.1.colour.clone(),
                origin: EditOrigin::Agent,
                state: ProposalState::Proposed,
                notes: String::new(),
                conflict,
                replaces,
                pass_ref: a.pass_ref,
            })?;
            return Ok(TopicMergeOut {
                topic: id.0.to_string(),
                proposed: true,
                removed: Vec::new(),
            });
        }
        let human = !is_agent(cx);
        cx.tx.update(first.0, |t| {
            t.span.end = last_end;
            t.title = title;
            if human {
                t.state = ProposalState::Accepted;
                t.conflict = false;
                t.replaces.clear();
            }
        })?;
        let mut removed = Vec::new();
        for x in &ts[1..] {
            cx.tx.delete(x.0)?;
            removed.push(x.0.0.to_string());
        }
        Ok(TopicMergeOut {
            topic: first.0.0.to_string(),
            proposed: false,
            removed,
        })
    }
}

// ---------------------------------------------------------------------------------------
// topic_state / topic_delete
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decide {
    Accepted,
    Rejected,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicStateArgs {
    pub topic: Id<Topic>,
    pub state: Decide,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TopicStateOut {
    /// Every topic this decided (a proposed split or merge is decided as a whole).
    pub topics: Vec<String>,
    /// Accepted topics a split or merge replaced (removed).
    pub replaced: Vec<String>,
}

#[derive(Debug)]
pub struct TopicState;

#[command(
    name = "topic_state",
    scope = "edit",
    undo = "undoable",
    doc = "Accept or reject a proposed topic (a proposed split or merge is decided as a whole and replaces the topics it names). Only the human accepts; an agent may only withdraw its own proposal.",
    example = r#"{"topic": "01J00000000000000000000007", "state": "accepted"}"#
)]
impl Command for TopicState {
    type Args = TopicStateArgs;
    type Output = TopicStateOut;
    fn run(a: TopicStateArgs, cx: &mut Cx<'_>) -> Result<TopicStateOut, CmdError> {
        let t = cx.tx.get(a.topic)?.clone();
        if is_agent(cx) && !(a.state == Decide::Rejected && own_proposal(&t)) {
            return Err(CmdError::invalid(
                "/state",
                "only the human accepts; an agent may withdraw its own proposal",
            ));
        }
        if t.state == ProposalState::Accepted {
            if a.state == Decide::Accepted {
                return Ok(TopicStateOut {
                    topics: vec![a.topic.0.to_string()],
                    replaced: vec![],
                });
            }
            return Err(CmdError::invalid(
                "/state",
                "remove an accepted topic with topic_delete",
            ));
        }
        // the proposal group: a split's two halves share `replaces`
        let mut group: Vec<Id<Topic>> = vec![a.topic];
        if !t.replaces.is_empty() {
            for (oid, x, _) in topics_of(cx.tx.peek(), t.cut) {
                if oid != a.topic && x.state == ProposalState::Proposed && x.replaces == t.replaces
                {
                    cx.tx.get(oid)?;
                    group.push(oid);
                }
            }
        }
        let mut out = TopicStateOut {
            topics: group.iter().map(|g| g.0.to_string()).collect(),
            replaced: vec![],
        };
        match a.state {
            Decide::Rejected => {
                for g in &group {
                    cx.tx.update(*g, |x| x.state = ProposalState::Rejected)?;
                }
            }
            Decide::Accepted => {
                if t.state == ProposalState::Rejected {
                    return Err(CmdError::invalid("/topic", "a rejected proposal"));
                }
                let all = topics_of(cx.tx.peek(), t.cut);
                for g in &group {
                    let (_, gx, gr) = all
                        .iter()
                        .find(|(o, _, _)| o == g)
                        .ok_or(CmdError::NotFound)?;
                    let clash = all.iter().any(|(o, x, xr)| {
                        !group.contains(o)
                            && !t.replaces.contains(o)
                            && x.state == ProposalState::Accepted
                            && x.media == gx.media
                            && overlaps(*xr, *gr)
                    });
                    if clash {
                        return Err(CmdError::invalid(
                            "/topic",
                            "it overlaps a topic you made: move or remove that first, or reject this proposal",
                        ));
                    }
                }
                for r in &t.replaces {
                    if cx.tx.peek().get(*r).is_some() {
                        cx.tx.delete(*r)?;
                        out.replaced.push(r.0.to_string());
                    }
                }
                for g in &group {
                    cx.tx.update(*g, |x| {
                        x.state = ProposalState::Accepted;
                        x.conflict = false;
                        x.replaces.clear();
                    })?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopicDeleteArgs {
    pub topic: Id<Topic>,
}

#[derive(Debug)]
pub struct TopicDelete;

#[command(
    name = "topic_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Remove a topic label from the stream edit (the talk stays; only the label goes). An agent may only remove its own proposal.",
    example = r#"{"topic": "01J00000000000000000000007"}"#
)]
impl Command for TopicDelete {
    type Args = TopicDeleteArgs;
    type Output = super::Done;
    fn run(a: TopicDeleteArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        let t = cx.tx.get(a.topic)?.clone();
        if is_agent(cx) && !own_proposal(&t) {
            return Err(CmdError::invalid("/topic", HUMANS_TOPIC));
        }
        cx.tx.delete(a.topic)?;
        Ok(super::Done {})
    }
}

// ---------------------------------------------------------------------------------------
// fluff_propose / fluff_state
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FluffItem {
    /// The first word to strike.
    pub start: WordRef,
    /// The last word to strike.
    pub end: WordRef,
    /// Why it is fluff, in a few words (≤ 300 characters).
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FluffProposeArgs {
    /// The stream edit (default: the project's).
    #[serde(default)]
    pub cut: Option<Id<Cut>>,
    /// At most 500.
    pub items: Vec<FluffItem>,
    /// The `as_of` the pass began at: stretches the human edited since arrive flagged.
    #[serde(default)]
    pub since: Option<Rev>,
    #[serde(default)]
    pub pass_ref: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FluffProposed {
    /// The new proposals, in the order given (`null` where skipped).
    pub proposals: Vec<Option<String>>,
    /// How many arrived flagged "conflicts with your edit".
    pub conflicts: usize,
    /// Skipped: already struck, proposed before and rejected, or already proposed.
    pub skipped: Vec<String>,
}

#[derive(Debug)]
pub struct FluffPropose;

#[command(
    name = "fluff_propose",
    scope = "edit",
    undo = "undoable",
    doc = "Propose stretches of the stream edit to strike as fluff (dead air, setup chatter, off-topic tangents), sweep-style: each is previewed and accepted or rejected by the human with fluff_state.",
    example = r#"{"items": [{"start": {"word_id": {"asr_revision": "r1", "ordinal": 300}, "t_src_start": 211680000000, "t_src_end": 211974000000, "normalised_text": "okay"}, "end": {"word_id": {"asr_revision": "r1", "ordinal": 360}, "t_src_start": 254016000000, "t_src_end": 254310000000, "normalised_text": "chat"}, "reason": "reading chat while the render runs"}], "since": 120}"#
)]
impl Command for FluffPropose {
    type Args = FluffProposeArgs;
    type Output = FluffProposed;
    fn run(a: FluffProposeArgs, cx: &mut Cx<'_>) -> Result<FluffProposed, CmdError> {
        if a.items.is_empty() || a.items.len() > 500 {
            return Err(CmdError::invalid("/items", "1 to 500 stretches"));
        }
        if let Some(p) = &a.pass_ref {
            bounded(p, "/pass_ref", 120)?;
        }
        let cut = the_stream(cx, a.cut)?;
        let agent = is_agent(cx);
        let mut out = FluffProposed {
            proposals: Vec::new(),
            conflicts: 0,
            skipped: Vec::new(),
        };
        let existing: Vec<(FluffProposal, (i64, i64))> = cx
            .tx
            .children::<FluffProposal>(cut.entity())
            .into_iter()
            .filter_map(|id| cx.tx.peek().get(id).cloned())
            .map(|f| {
                let r = span_range(&f.span);
                (f, r)
            })
            .collect();
        let mut made: Vec<(MediaRef, (i64, i64))> = Vec::new();
        for (i, it) in a.items.iter().enumerate() {
            let s = grounded(cx, &it.start, "/items/start")?;
            let e = grounded(cx, &it.end, "/items/end")?;
            let reason = clean_text(&it.reason);
            bounded(&reason, "/items/reason", 300)?;
            let r = (s.t_src_start.0, e.t_src_end.0);
            if r.1 <= r.0 {
                return Err(CmdError::invalid("/items", "the last word after the first"));
            }
            let media = media_for(cx, cut, Some(&s))?;
            let doc = cx.tx.peek();
            let plays = doc.children::<Part>(cut.entity()).any(|id| {
                doc.get(id)
                    .is_some_and(|p| p.media == media && overlaps((p.src_in.0, p.src_out.0), r))
            });
            let same =
                |x: (i64, i64)| (x.0 - r.0).abs() <= SAME_SPAN && (x.1 - r.1).abs() <= SAME_SPAN;
            let why = if !plays {
                Some("already struck")
            } else if existing
                .iter()
                .any(|(f, x)| f.state == ProposalState::Rejected && f.media == media && same(*x))
            {
                Some("proposed before and rejected")
            } else if existing.iter().any(|(f, x)| {
                f.state == ProposalState::Proposed && f.media == media && overlaps(*x, r)
            }) || made.iter().any(|(m, x)| *m == media && overlaps(*x, r))
            {
                Some("already proposed")
            } else {
                None
            };
            if let Some(w) = why {
                out.skipped.push(format!("{i}: {w}"));
                out.proposals.push(None);
                continue;
            }
            let conflict = a
                .since
                .is_some_and(|since| human_edited_after(cx.tx.peek(), cut, media, r, since));
            out.conflicts += usize::from(conflict);
            let last = cx
                .tx
                .peek()
                .children::<FluffProposal>(cut.entity())
                .last()
                .and_then(|l| cx.tx.peek().get(l))
                .map(|l| l.order.clone());
            let order = OrderKey::between(last.as_ref(), None)
                .map_err(|_| CmdError::invalid("/items", "too many proposals"))?;
            let id: Id<FluffProposal> = cx.tx.mint();
            cx.tx.create(FluffProposal {
                id,
                cut,
                order,
                media,
                span: Span {
                    start: Anchor::WordStart(s),
                    end: Anchor::WordEnd(e),
                },
                reason,
                origin: if agent {
                    EditOrigin::Agent
                } else {
                    EditOrigin::Human
                },
                state: ProposalState::Proposed,
                conflict,
                pass_ref: a.pass_ref.clone(),
            })?;
            made.push((media, r));
            out.proposals.push(Some(id.0.to_string()));
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FluffStateArgs {
    /// One or more proposals of the stream edit (≤ 500).
    pub proposals: Vec<Id<FluffProposal>>,
    pub state: Decide,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FluffDecided {
    pub decided: usize,
    /// Accepting strikes: parts trimmed, split or removed.
    pub parts_changed: usize,
    pub parts_removed: usize,
}

#[derive(Debug)]
pub struct FluffState;

#[command(
    name = "fluff_state",
    scope = "edit",
    undo = "undoable",
    doc = "Accept fluff proposals (strike their stretches from the stream edit, one transaction) or reject them. Only the human accepts; an agent may only withdraw its own.",
    example = r#"{"proposals": ["01J00000000000000000000009"], "state": "accepted"}"#
)]
impl Command for FluffState {
    type Args = FluffStateArgs;
    type Output = FluffDecided;
    fn run(a: FluffStateArgs, cx: &mut Cx<'_>) -> Result<FluffDecided, CmdError> {
        if a.proposals.is_empty() || a.proposals.len() > 500 {
            return Err(CmdError::invalid("/proposals", "1 to 500 proposals"));
        }
        let agent = is_agent(cx);
        let mut out = FluffDecided {
            decided: 0,
            parts_changed: 0,
            parts_removed: 0,
        };
        let mut todo = Vec::new();
        for id in &a.proposals {
            let f = cx.tx.get(*id)?.clone();
            if agent
                && !(a.state == Decide::Rejected
                    && f.origin == EditOrigin::Agent
                    && f.state == ProposalState::Proposed)
            {
                return Err(CmdError::invalid(
                    "/state",
                    "only the human accepts; an agent may withdraw its own proposal",
                ));
            }
            if f.state != ProposalState::Proposed {
                continue;
            }
            todo.push((*id, f));
        }
        // from the end backwards, so earlier stretches see the parts as proposed
        todo.sort_by_key(|(_, f)| std::cmp::Reverse(span_range(&f.span).0));
        for (id, f) in todo {
            if a.state == Decide::Accepted {
                // on the stream edit's frame grid (part edges off it do not plan)
                let (s, e) = span_range(&f.span);
                let fpf = super::longform::stream_fpf(cx.tx.peek(), f.cut);
                let (s, e) = (
                    super::longform::nearest_frame(s, fpf),
                    super::longform::nearest_frame(e, fpf),
                );
                if e > s {
                    let (c, r, _) =
                        super::words::remove_span(cx, f.cut, Flicks(s), Flicks(e), Some(f.media))?;
                    out.parts_changed += c;
                    out.parts_removed += r;
                }
            }
            cx.tx.update(id, |x| {
                x.state = match a.state {
                    Decide::Accepted => ProposalState::Accepted,
                    Decide::Rejected => ProposalState::Rejected,
                };
            })?;
            out.decided += 1;
        }
        Ok(out)
    }
}
