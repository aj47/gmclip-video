//! Long-forms from topics, the parent diff and pull, chapters, extra framings and the
//! project map (decision 0011, SPEC §5.17).
//!
//! - `longform_from_topics` makes a 16:9 `Cut{kind: long}` whose parts are the stream
//!   edit's kept stretches inside the chosen topics, in chapter order, and records
//!   `lineage{forked_from: stream edit, topics, parent_base}`.
//! - **Nothing propagates silently** (AJ: a long-form always asks): `cut_parent_diff`
//!   compares what the stream edit keeps inside those topics *now* with `parent_base`,
//!   what it kept when the long-form last took its changes, and lists the edits;
//!   `cut_pull_from_parent` applies them on an explicit call, one transaction. The
//!   long-form's own trims are never in the diff: only the parent's movement is.
//! - Topics are the chapters (`longform_chapters`), titled live from the topics.
//! - A cut may carry several framings of one aspect (`framing_add`: "screen focus" and
//!   "face big" on one short), sharing its words and captions.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::clock::cut_layout;
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id, Rational};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::topic::{F, id_of, span_range};
use crate::command::{Command, Cx};

fn ms(f: i64) -> i64 {
    f * 1000 / F
}

/// What the stream edit keeps inside `topic` now: its parts on the topic's media, cut
/// to the topic's range, in source order (each part on its own: parts never straddle a
/// picture-offset change).
pub fn kept_in(doc: &Doc, stream: Id<Cut>, t: &Topic) -> Vec<(i64, i64)> {
    let (t0, t1) = span_range(&t.span);
    // the topic's word edges on the stream edit's frame grid (nearest frame, ties to the
    // earlier, SPEC §4.1.1): part edges off the grid do not plan
    let fpf = stream_fpf(doc, stream);
    let (t0, t1) = (nearest_frame(t0, fpf), nearest_frame(t1, fpf));
    let mut v: Vec<(i64, i64)> = doc
        .children::<Part>(stream.entity())
        .filter_map(|id| doc.get(id))
        .filter(|p| p.media == t.media && p.src_in.0 < t1 && t0 < p.src_out.0)
        .map(|p| (p.src_in.0.max(t0), p.src_out.0.min(t1)))
        .filter(|(a, b)| b > a)
        .collect();
    v.sort_unstable();
    v
}

/// Flicks per frame of the stream edit's rate (its first framing's), else 30 fps's.
pub(crate) fn stream_fpf(doc: &Doc, stream: Id<Cut>) -> i64 {
    doc.children::<Framing>(stream.entity())
        .find_map(|f| doc.get(f))
        .and_then(|f| crate::planner::framing_fpf(f.fps))
        .unwrap_or(F / 30)
}

/// `t` at the nearest frame of a grid of `fpf` flicks per frame, ties to the earlier.
pub(crate) fn nearest_frame(t: i64, fpf: i64) -> i64 {
    crate::planner::nearest_frame(t, fpf)
}

/// What the stream edits keep inside `topics` (each topic on its own video's stream
/// edit), in the topics' order.
fn base_of(doc: &Doc, topics: &[(Id<Topic>, Topic)]) -> Vec<BaseRange> {
    topics
        .iter()
        .flat_map(|(id, t)| {
            kept_in(doc, t.cut, t)
                .into_iter()
                .map(move |(a, b)| BaseRange {
                    topic: *id,
                    media: t.media,
                    start: Flicks(a),
                    end: Flicks(b),
                })
        })
        .collect()
}

/// `a − b` as interval sets (both sorted, non-overlapping within themselves).
fn minus(a: &[(i64, i64)], b: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for &(s, e) in a {
        let mut cur = s;
        for &(bs, be) in b {
            if be <= cur || bs >= e {
                continue;
            }
            if bs > cur {
                out.push((cur, bs));
            }
            cur = cur.max(be);
            if cur >= e {
                break;
            }
        }
        if cur < e {
            out.push((cur, e));
        }
    }
    out
}

fn merged(mut v: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    v.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// One parent edit a long-form has not taken.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParentEdit {
    /// `struck` (the stream edit dropped it, or the topic shrank) or `restored` (the
    /// stream edit plays it again, or the topic grew).
    pub kind: String,
    pub topic: String,
    /// The topic's title (a human's, or an agent's proposal: untrusted).
    pub topic_title: String,
    pub media: MediaRef,
    pub from: Flicks,
    pub to: Flicks,
    pub from_ms: i64,
    pub dur_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParentDiff {
    pub cut: String,
    pub parent: String,
    pub edits: Vec<ParentEdit>,
    pub struck_ms: i64,
    pub restored_ms: i64,
    /// Topics this long-form was made from that the stream edit no longer has.
    pub topics_gone: Vec<String>,
}

/// The diff of a long-form against its parent (see the module docs).
pub fn parent_diff(doc: &Doc, cut: Id<Cut>) -> Result<ParentDiff, CmdError> {
    let c = doc.get(cut).ok_or(CmdError::NotFound)?;
    let parent = c
        .lineage
        .forked_from
        .filter(|_| !c.lineage.topics.is_empty())
        .ok_or_else(|| {
            CmdError::invalid(
                "/cut",
                "this cut was not made from the stream edit's topics",
            )
        })?;
    let mut out = ParentDiff {
        cut: cut.0.to_string(),
        parent: parent.0.to_string(),
        edits: Vec::new(),
        struck_ms: 0,
        restored_ms: 0,
        topics_gone: Vec::new(),
    };
    for tid in &c.lineage.topics {
        let base: Vec<(i64, i64)> = merged(
            c.lineage
                .parent_base
                .iter()
                .filter(|b| b.topic == *tid)
                .map(|b| (b.start.0, b.end.0))
                .collect(),
        );
        let media = c
            .lineage
            .parent_base
            .iter()
            .find(|b| b.topic == *tid)
            .map(|b| b.media);
        let (now, title, media) = match doc.get(*tid) {
            Some(t) if t.state == ProposalState::Accepted => {
                (merged(kept_in(doc, t.cut, t)), t.title.clone(), t.media)
            }
            _ => {
                out.topics_gone.push(tid.0.to_string());
                let Some(m) = media else { continue };
                (Vec::new(), String::new(), m)
            }
        };
        for (kind, set) in [
            ("struck", minus(&base, &now)),
            ("restored", minus(&now, &base)),
        ] {
            for (a, b) in set {
                let dur = ms(b - a);
                if kind == "struck" {
                    out.struck_ms += dur;
                } else {
                    out.restored_ms += dur;
                }
                out.edits.push(ParentEdit {
                    kind: kind.into(),
                    topic: tid.0.to_string(),
                    topic_title: title.clone(),
                    media,
                    from: Flicks(a),
                    to: Flicks(b),
                    from_ms: ms(a),
                    dur_ms: dur,
                });
            }
        }
    }
    out.edits.sort_by_key(|e| (e.from.0, e.kind.clone()));
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// longform_from_topics
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopicOrder {
    /// Chapters in stream order.
    #[default]
    Source,
    /// Chapters in the order given.
    Given,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LongformArgs {
    /// Accepted topics of the stream edit (1–100).
    pub topics: Vec<Id<Topic>>,
    #[serde(default)]
    pub order: TopicOrder,
    /// Default: the first topic's title.
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LongformMade {
    pub cut: String,
    pub framing: String,
    pub parts: usize,
    pub duration_ms: i64,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug)]
pub struct LongformFromTopics;

#[command(
    name = "longform_from_topics",
    scope = "edit",
    undo = "undoable",
    doc = "Make a 16:9 long-form from topics of the stream edit: its parts are what the stream edit keeps inside those topics, one chapter per topic, in stream order or the order given.",
    example = r#"{"topics": ["01J00000000000000000000007", "01J00000000000000000000008"], "order": "source", "title": "Local AI video, part 1"}"#
)]
impl Command for LongformFromTopics {
    type Args = LongformArgs;
    type Output = LongformMade;
    fn run(a: LongformArgs, cx: &mut Cx<'_>) -> Result<LongformMade, CmdError> {
        if a.topics.is_empty() || a.topics.len() > 100 {
            return Err(CmdError::invalid("/topics", "1 to 100 topics"));
        }
        // Topics may be on several videos of the stream edit (a long-form across sources);
        // the stream edit is the one it is forked from.
        let mut ts: Vec<(Id<Topic>, Topic)> = Vec::new();
        let mut streams: Vec<Id<Cut>> = Vec::new();
        for id in &a.topics {
            let t = cx.tx.get(*id)?.clone();
            let on_stream = cx.tx.get(t.cut).is_ok_and(|c| c.kind == CutKind::Stream);
            if !on_stream || t.state != ProposalState::Accepted {
                return Err(CmdError::invalid(
                    "/topics",
                    "accepted topics of the stream edits",
                ));
            }
            if !streams.contains(&t.cut) {
                streams.push(t.cut);
            }
            if ts.iter().any(|x| x.0 == *id) {
                return Err(CmdError::invalid("/topics", "each topic once"));
            }
            ts.push((*id, t));
        }
        if a.order == TopicOrder::Source {
            // stream order: video by video (in the order their stream edits were made),
            // then time
            streams.sort_by_key(|s| s.0);
            let videos: Vec<Vec<MediaRef>> = streams
                .iter()
                .map(|s| super::topic::stream_medias(cx.tx.peek(), *s))
                .collect();
            ts.sort_by_key(|(_, t)| {
                let at = streams.iter().position(|s| *s == t.cut);
                (
                    at,
                    at.and_then(|i| videos[i].iter().position(|m| *m == t.media)),
                    span_range(&t.span).0,
                )
            });
        }
        // the parts it reads (recorded)
        for s in &streams {
            for p in cx.tx.children::<Part>(s.entity()) {
                cx.tx.get(p)?;
            }
        }
        let stream = ts[0].1.cut;
        let stream_cut = cx.tx.get(stream)?.clone();
        let base = base_of(cx.tx.peek(), &ts);
        if base.is_empty() {
            return Err(CmdError::invalid(
                "/topics",
                "the stream edit keeps nothing inside those topics",
            ));
        }
        let title = match a.title.as_deref().map(super::topic::clean_text) {
            Some(t) if !t.is_empty() => {
                super::bounded(&t, "/title", 200)?;
                t
            }
            _ => ts[0].1.title.chars().take(200).collect(),
        };
        let made = super::cut::CutCreate::run(
            super::cut::CutCreateArgs {
                title,
                moment: None,
            },
            cx,
        )?;
        let cut: Id<Cut> = id_of(&made.cut)?;
        let topics: Vec<Id<Topic>> = ts.iter().map(|x| x.0).collect();
        let pb = base.clone();
        let mut videos: Vec<MediaRef> = Vec::new();
        for (_, t) in &ts {
            if !videos.contains(&t.media) {
                videos.push(t.media);
            }
        }
        cx.tx.update(cut, |c| {
            c.kind = CutKind::Long;
            c.lineage.forked_from = Some(stream);
            c.lineage.topics = topics;
            c.lineage.parent_base = pb;
            c.brief = if videos.len() > 1 {
                format!(
                    "Long-form from {} videos of the stream edit «{}»",
                    videos.len(),
                    stream_cut.title.chars().take(80).collect::<String>()
                )
            } else {
                format!(
                    "Long-form from the stream edit «{}»",
                    stream_cut.title.chars().take(80).collect::<String>()
                )
            };
        })?;
        if let Some(cd) = cx.tx.children::<CaptionDoc>(cut.entity()).first() {
            cx.tx.update(*cd, |c| c.enabled = false)?;
        }
        // the stream edit's landscape framing sets the rate; 1920×1080 always
        let fps = cx
            .tx
            .children::<Framing>(stream.entity())
            .iter()
            .find_map(|f| cx.tx.peek().get(*f).map(|x| x.fps))
            .filter(|r| crate::planner::framing_fpf(*r).is_some())
            .unwrap_or(Rational::new(30, 1));
        let fr = super::cut::FramingCreate::run(
            super::cut::FramingCreateArgs {
                cut,
                name: "16:9".into(),
                w: 1920,
                h: 1080,
                fps,
            },
            cx,
        )?;
        let framing: Id<Framing> = id_of(&fr.id)?;
        full_frame_layer(cx, framing)?;
        let mut after: Option<Id<Part>> = None;
        let mut n = 0;
        for b in &base {
            let p = super::part::PartAdd::run(
                super::part::PartAddArgs {
                    cut,
                    after,
                    media: b.media,
                    src_in: b.start,
                    src_out: b.end,
                    role: Some(if n == 0 {
                        PartRole::Hook
                    } else {
                        PartRole::Other
                    }),
                },
                cx,
            )?;
            after = Some(id_of(&p.id)?);
            n += 1;
        }
        let doc = cx.tx.peek();
        let duration_ms = ms(cut_layout(doc, cut).iter().map(|s| s.dur.0).sum());
        let chapters = chapters(doc, cut);
        Ok(LongformMade {
            cut: cut.0.to_string(),
            framing: framing.0.to_string(),
            parts: n,
            duration_ms,
            chapters,
        })
    }
}

fn full_frame_layer(cx: &mut Cx<'_>, framing: Id<Framing>) -> Result<(), CmdError> {
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
    Ok(())
}

// ---------------------------------------------------------------------------------------
// chapters
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Chapter {
    pub topic: String,
    /// The topic's title now (untrusted when an agent wrote it).
    pub title: String,
    pub at_ms: i64,
    /// `m:ss` or `h:mm:ss`, as YouTube wants it.
    pub at: String,
}

fn clock_text(ms: i64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A long-form's chapters: a new one wherever the part's source time falls in another
/// of its topics; parts in no topic stay in the chapter before them.
pub fn chapters(doc: &Doc, cut: Id<Cut>) -> Vec<Chapter> {
    let Some(c) = doc.get(cut) else {
        return Vec::new();
    };
    // a topic's range on the frame grid its parts were cut on (`kept_in`, its stream
    // edit's): a word starting just after a frame edge gives a part starting on that edge
    let topics: Vec<(Id<Topic>, &Topic, (i64, i64))> = c
        .lineage
        .topics
        .iter()
        .filter_map(|id| {
            doc.get(*id).map(|t| {
                let (a, b) = span_range(&t.span);
                let fpf = stream_fpf(doc, t.cut);
                (*id, t, (nearest_frame(a, fpf), nearest_frame(b, fpf)))
            })
        })
        .collect();
    let mut out: Vec<Chapter> = Vec::new();
    let mut cur: Option<Id<Topic>> = None;
    for sp in cut_layout(doc, cut) {
        let Some(p) = doc.get(sp.part) else { continue };
        let hit = topics
            .iter()
            .find(|(_, t, r)| t.media == p.media && r.0 <= p.src_in.0 && p.src_in.0 < r.1);
        if let Some((id, t, _)) = hit
            && cur != Some(*id)
        {
            cur = Some(*id);
            let at_ms = if out.is_empty() { 0 } else { ms(sp.start.0) };
            out.push(Chapter {
                topic: id.0.to_string(),
                title: t.title.clone(),
                at_ms,
                at: clock_text(at_ms),
            });
        }
    }
    out
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutOnlyArgs {
    pub cut: Id<Cut>,
}

/// YouTube's rules for chapters in a description: the first at 00:00, each at least
/// 10 s long, at least three.
pub const MIN_CHAPTER_MS: i64 = 10_000;
pub const MIN_CHAPTERS: usize = 3;

/// One chapter as YouTube reads it, on the long-form's output timeline.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct YoutubeChapter {
    /// The title (one line; untrusted when an agent titled the topic).
    pub title: String,
    pub at_ms: i64,
    /// `00:00`, or `h:mm:ss` from an hour on.
    pub at: String,
    pub dur_ms: i64,
    /// The topics it covers (more than one when a short one was folded in).
    pub topics: Vec<String>,
}

/// `00:00` (`mm:ss`), or `h:mm:ss` from an hour on: the form YouTube's chapter list
/// shows.
pub fn chapter_clock(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A long-form's chapters for a YouTube description, from its topics on its output
/// timeline (starts on its frame grid): a chapter under 10 s is folded into its shorter
/// neighbour (the longer one's title kept), the first starts at 00:00; with fewer than
/// three left, `why_not` says why YouTube will not show them. Returns the chapters, what
/// was folded, and why not.
pub fn youtube_chapters(
    doc: &Doc,
    cut: Id<Cut>,
) -> (Vec<YoutubeChapter>, Vec<String>, Option<String>) {
    let fpf = stream_fpf(doc, cut);
    let total_ms = ms(nearest_frame(
        cut_layout(doc, cut).iter().map(|s| s.dur.0).sum(),
        fpf,
    ));
    let raw = chapters(doc, cut);
    let mut v: Vec<YoutubeChapter> = raw
        .iter()
        .map(|c| YoutubeChapter {
            title: c.title.split_whitespace().collect::<Vec<_>>().join(" "),
            at_ms: if c.at_ms == 0 {
                0
            } else {
                ms(nearest_frame(c.at_ms * F / 1000, fpf))
            },
            at: String::new(),
            dur_ms: 0,
            topics: vec![c.topic.clone()],
        })
        .collect();
    let durs = |v: &mut Vec<YoutubeChapter>| {
        for i in 0..v.len() {
            let end = v.get(i + 1).map_or(total_ms, |n| n.at_ms);
            v[i].dur_ms = end - v[i].at_ms;
        }
    };
    durs(&mut v);
    let mut notes = Vec::new();
    while v.len() > 1 {
        let Some((i, _)) = v
            .iter()
            .enumerate()
            .filter(|(_, c)| c.dur_ms < MIN_CHAPTER_MS)
            .min_by_key(|(_, c)| c.dur_ms)
        else {
            break;
        };
        // fold into the shorter neighbour (the first chapter into the next, the last
        // into the one before)
        let j = match (i.checked_sub(1), (i + 1 < v.len()).then_some(i + 1)) {
            (Some(a), Some(b)) => {
                if v[a].dur_ms <= v[b].dur_ms {
                    a
                } else {
                    b
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let (lo, hi) = (i.min(j), i.max(j));
        let short = v[i].clone();
        let other = v[j].clone();
        let keep = if other.dur_ms >= short.dur_ms {
            &other
        } else {
            &short
        };
        notes.push(format!(
            "“{}” ({} s) joined “{}”: YouTube needs 10 s per chapter",
            short.title,
            short.dur_ms / 1000,
            other.title
        ));
        let mut topics = v[lo].topics.clone();
        topics.extend(v[hi].topics.clone());
        let merged = YoutubeChapter {
            title: keep.title.clone(),
            at_ms: v[lo].at_ms,
            at: String::new(),
            dur_ms: 0,
            topics,
        };
        v.splice(lo..=hi, [merged]);
        durs(&mut v);
    }
    if let Some(f) = v.first_mut() {
        f.at_ms = 0;
    }
    durs(&mut v);
    for c in &mut v {
        c.at = chapter_clock(c.at_ms);
    }
    let why_not = if raw.is_empty() {
        Some("this long-form was not made from topics, so it has no chapters (make one from topics, or label topics on the stream edit)".to_owned())
    } else if v.len() < MIN_CHAPTERS {
        Some(format!(
            "YouTube shows chapters only when there are at least {MIN_CHAPTERS}, each 10 s or longer: this long-form has {} {}{}",
            v.len(),
            if v.len() == 1 { "chapter" } else { "chapters" },
            if notes.is_empty() {
                " (add a topic, or make it from more topics)".to_owned()
            } else {
                " once the short ones are folded in".to_owned()
            }
        ))
    } else {
        None
    };
    (v, notes, why_not)
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Chapters {
    /// One per topic it was made from, in cut order.
    pub chapters: Vec<Chapter>,
    /// The chapters YouTube reads, on the output timeline: short ones folded in.
    pub youtube_chapters: Vec<YoutubeChapter>,
    /// The description lines (`00:00 Setup`), one per line.
    pub youtube: String,
    /// YouTube will show them (at least three, each ≥ 10 s, the first at 00:00).
    pub valid: bool,
    /// Why YouTube will not, when it will not.
    pub why_not: Option<String>,
    /// Chapters folded into a neighbour, and why.
    pub notes: Vec<String>,
}

/// [`Chapters`] for a long-form.
pub fn chapters_for(doc: &Doc, cut: Id<Cut>) -> Chapters {
    let (yc, notes, why_not) = youtube_chapters(doc, cut);
    let youtube = yc
        .iter()
        .map(|c| format!("{} {}", c.at, c.title))
        .collect::<Vec<_>>()
        .join("\n");
    Chapters {
        chapters: chapters(doc, cut),
        youtube_chapters: yc,
        youtube,
        valid: why_not.is_none(),
        why_not,
        notes,
    }
}

#[derive(Debug)]
pub struct LongformChapters;

#[command(
    name = "longform_chapters",
    scope = "read",
    undo = "read_only",
    doc = "A long-form's YouTube chapters from its topics, on its output timeline: `00:00 Title` lines (first at 00:00, each at least 10 s: a shorter one is folded into its neighbour), whether YouTube will show them (at least three) and why not; also one chapter per topic as made.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for LongformChapters {
    type Args = CutOnlyArgs;
    type Output = Chapters;
    fn run(a: CutOnlyArgs, cx: &mut Cx<'_>) -> Result<Chapters, CmdError> {
        cx.tx.get(a.cut)?;
        let _ = cx.tx.children::<Part>(a.cut.entity());
        Ok(chapters_for(cx.tx.peek(), a.cut))
    }
}

// ---------------------------------------------------------------------------------------
// longform_fork / longform_compare / longform_pick (Ideas 2026-10-16, K.03)
// ---------------------------------------------------------------------------------------

fn long_form(cx: &mut Cx<'_>, cut: Id<Cut>, path: &str) -> Result<Cut, CmdError> {
    let c = cx.tx.get(cut)?.clone();
    if c.kind != CutKind::Long {
        return Err(CmdError::invalid(path, "a long-form"));
    }
    Ok(c)
}

/// One chapter's parts: its topic (none for parts in no topic) and the parts in order.
type ChapterGroup = (Option<Id<Topic>>, Vec<Id<Part>>);

/// A long-form's parts in output order, grouped by chapter: `(topic, parts)`; a part in
/// no topic stays with the chapter before it (the first chapter when it leads).
fn chapter_groups(doc: &Doc, cut: Id<Cut>) -> Vec<ChapterGroup> {
    let Some(c) = doc.get(cut) else {
        return Vec::new();
    };
    let topics: Vec<(Id<Topic>, &Topic, (i64, i64))> = c
        .lineage
        .topics
        .iter()
        .filter_map(|id| {
            doc.get(*id).map(|t| {
                let (a, b) = span_range(&t.span);
                let fpf = stream_fpf(doc, t.cut);
                (*id, t, (nearest_frame(a, fpf), nearest_frame(b, fpf)))
            })
        })
        .collect();
    let mut out: Vec<ChapterGroup> = Vec::new();
    for sp in cut_layout(doc, cut) {
        let Some(p) = doc.get(sp.part) else { continue };
        let hit = topics
            .iter()
            .find(|(_, t, r)| t.media == p.media && r.0 <= p.src_in.0 && p.src_in.0 < r.1)
            .map(|x| x.0);
        match (hit, out.last_mut()) {
            (Some(t), Some(last)) if last.0 == Some(t) => last.1.push(sp.part),
            (Some(t), _) => out.push((Some(t), vec![sp.part])),
            (None, Some(last)) => last.1.push(sp.part),
            (None, None) => out.push((None, vec![sp.part])),
        }
    }
    // leading parts in no topic join the first chapter
    if out.len() > 1 && out[0].0.is_none() {
        let lead = out.remove(0).1;
        out[0].1.splice(0..0, lead);
    }
    out
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LongformForkArgs {
    /// The long-form to fork.
    pub cut: Id<Cut>,
    /// The sibling's title (default: the long-form's title + " (fork)").
    #[serde(default)]
    pub title: Option<String>,
    /// A new chapter order: every topic of the long-form once. Absent: the same order.
    #[serde(default)]
    pub topics: Option<Vec<Id<Topic>>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LongformForked {
    pub cut: String,
    pub title: String,
    pub framing: String,
    pub parts: usize,
    pub duration_ms: i64,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug)]
pub struct LongformFork;

#[command(
    name = "longform_fork",
    scope = "edit",
    undo = "undoable",
    doc = "Fork a long-form into a sibling (same stream edit, topics and parent base) to try another order: its parts copied in chapter order, or reordered chapter by chapter when a new topic order is given. The original is untouched.",
    example = r#"{"cut": "01J00000000000000000000001", "topics": ["01J00000000000000000000008", "01J00000000000000000000007"]}"#
)]
impl Command for LongformFork {
    type Args = LongformForkArgs;
    type Output = LongformForked;
    fn run(a: LongformForkArgs, cx: &mut Cx<'_>) -> Result<LongformForked, CmdError> {
        let orig = long_form(cx, a.cut, "/cut")?;
        for p in cx.tx.children::<Part>(a.cut.entity()) {
            cx.tx.get(p)?;
        }
        let groups = chapter_groups(cx.tx.peek(), a.cut);
        if groups.is_empty() {
            return Err(CmdError::invalid("/cut", "a long-form with parts"));
        }
        let ordered: Vec<Id<Part>> = match &a.topics {
            None => groups.iter().flat_map(|g| g.1.iter().copied()).collect(),
            Some(order) => {
                let mut want = orig.lineage.topics.clone();
                let mut got = order.clone();
                want.sort();
                got.sort();
                let dup = got.windows(2).any(|w| w[0] == w[1]);
                if dup || want != got {
                    return Err(CmdError::invalid(
                        "/topics",
                        "every topic of the long-form, once each",
                    ));
                }
                let mut out = Vec::new();
                for t in order {
                    for g in groups.iter().filter(|g| g.0 == Some(*t)) {
                        out.extend(g.1.iter().copied());
                    }
                }
                // parts in no topic at all (a long-form of only those) keep their place
                for g in groups.iter().filter(|g| g.0.is_none()) {
                    out.extend(g.1.iter().copied());
                }
                out
            }
        };
        let title = match a.title.as_deref().map(super::topic::clean_text) {
            Some(t) if !t.is_empty() => {
                super::bounded(&t, "/title", 200)?;
                t
            }
            _ => format!(
                "{} (fork)",
                orig.title.chars().take(190).collect::<String>()
            ),
        };
        let made = super::cut::CutCreate::run(
            super::cut::CutCreateArgs {
                title: title.clone(),
                moment: None,
            },
            cx,
        )?;
        let cut: Id<Cut> = id_of(&made.cut)?;
        let lineage = orig.lineage.clone();
        let brief = format!(
            "Fork of the long-form «{}»",
            orig.title.chars().take(80).collect::<String>()
        );
        cx.tx.update(cut, |c| {
            c.kind = CutKind::Long;
            c.lineage = lineage;
            c.brief = brief;
        })?;
        let captions_on = cx
            .tx
            .children::<CaptionDoc>(a.cut.entity())
            .first()
            .and_then(|d| cx.tx.peek().get(*d))
            .is_some_and(|d| d.enabled);
        if let Some(cd) = cx.tx.children::<CaptionDoc>(cut.entity()).first() {
            cx.tx.update(*cd, |c| c.enabled = captions_on)?;
        }
        // the original's framing (canvas and rate), full frame as long-forms are
        let ofr = cx
            .tx
            .children::<Framing>(a.cut.entity())
            .first()
            .and_then(|f| cx.tx.peek().get(*f))
            .cloned();
        let (name, w, h, fps) = ofr
            .map_or(("16:9".to_owned(), 1920, 1080, Rational::new(30, 1)), |f| {
                (f.name.clone(), f.canvas.w, f.canvas.h, f.fps)
            });
        let fr = super::cut::FramingCreate::run(
            super::cut::FramingCreateArgs {
                cut,
                name,
                w,
                h,
                fps,
            },
            cx,
        )?;
        let framing: Id<Framing> = id_of(&fr.id)?;
        full_frame_layer(cx, framing)?;
        let mut after: Option<Id<Part>> = None;
        for (n, pid) in ordered.iter().enumerate() {
            let p = cx.tx.get(*pid)?.clone();
            let made = super::part::PartAdd::run(
                super::part::PartAddArgs {
                    cut,
                    after,
                    media: p.media,
                    src_in: p.src_in,
                    src_out: p.src_out,
                    role: Some(if n == 0 { PartRole::Hook } else { p.role }),
                },
                cx,
            )?;
            after = Some(id_of(&made.id)?);
        }
        let doc = cx.tx.peek();
        Ok(LongformForked {
            cut: cut.0.to_string(),
            title,
            framing: framing.0.to_string(),
            parts: ordered.len(),
            duration_ms: ms(cut_layout(doc, cut).iter().map(|s| s.dur.0).sum()),
            chapters: chapters(doc, cut),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LongformCompareArgs {
    pub a: Id<Cut>,
    pub b: Id<Cut>,
}

/// One chapter as the comparison shows it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ComparedChapter {
    pub topic: String,
    pub title: String,
    pub colour: String,
    pub at_ms: i64,
    pub dur_ms: i64,
    pub parts: usize,
}

/// One side of the comparison.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ComparedLong {
    pub cut: String,
    pub title: String,
    pub brief: String,
    pub duration_ms: i64,
    pub parts: usize,
    pub chapters: Vec<ComparedChapter>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LongformComparison {
    pub a: ComparedLong,
    pub b: ComparedLong,
    /// Both play the same topics.
    pub same_topics: bool,
    /// …in the same order.
    pub same_order: bool,
    /// Topics in both at a different chapter position.
    pub moved: Vec<String>,
    /// Topics only one of them plays.
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
}

fn compared(doc: &Doc, cut: Id<Cut>) -> ComparedLong {
    let c = doc.get(cut);
    let layout = cut_layout(doc, cut);
    let total: i64 = layout.iter().map(|s| s.dur.0).sum();
    let mut at = 0i64;
    let mut chapters = Vec::new();
    for (t, ps) in chapter_groups(doc, cut) {
        let dur: i64 = ps
            .iter()
            .filter_map(|p| layout.iter().find(|s| s.part == *p))
            .map(|s| s.dur.0)
            .sum();
        let topic = t.and_then(|t| doc.get(t));
        chapters.push(ComparedChapter {
            topic: t.map(|t| t.0.to_string()).unwrap_or_default(),
            title: topic.map_or_else(|| "(no topic)".to_owned(), |x| x.title.clone()),
            colour: topic.map_or_else(|| "#808080".to_owned(), |x| x.colour.clone()),
            at_ms: ms(at),
            dur_ms: ms(at + dur) - ms(at),
            parts: ps.len(),
        });
        at += dur;
    }
    ComparedLong {
        cut: cut.0.to_string(),
        title: c.map(|c| c.title.clone()).unwrap_or_default(),
        brief: c.map(|c| c.brief.clone()).unwrap_or_default(),
        duration_ms: ms(total),
        parts: layout.len(),
        chapters,
    }
}

#[derive(Debug)]
pub struct LongformCompare;

#[command(
    name = "longform_compare",
    scope = "read",
    undo = "read_only",
    doc = "Compare two long-forms side by side: each one's chapters (topics) in order with their lengths, the total length, and which topics moved or only one of them plays.",
    example = r#"{"a": "01J00000000000000000000001", "b": "01J00000000000000000000002"}"#
)]
impl Command for LongformCompare {
    type Args = LongformCompareArgs;
    type Output = LongformComparison;
    fn run(a: LongformCompareArgs, cx: &mut Cx<'_>) -> Result<LongformComparison, CmdError> {
        long_form(cx, a.a, "/a")?;
        long_form(cx, a.b, "/b")?;
        for c in [a.a, a.b] {
            for p in cx.tx.children::<Part>(c.entity()) {
                cx.tx.get(p)?;
            }
        }
        let doc = cx.tx.peek();
        let (x, y) = (compared(doc, a.a), compared(doc, a.b));
        let order = |l: &ComparedLong| -> Vec<String> {
            let mut v: Vec<String> = Vec::new();
            for c in &l.chapters {
                if !c.topic.is_empty() && !v.contains(&c.topic) {
                    v.push(c.topic.clone());
                }
            }
            v
        };
        let (oa, ob) = (order(&x), order(&y));
        let only = |p: &[String], q: &[String]| -> Vec<String> {
            p.iter().filter(|t| !q.contains(t)).cloned().collect()
        };
        let (only_a, only_b) = (only(&oa, &ob), only(&ob, &oa));
        let ca: Vec<&String> = oa.iter().filter(|t| ob.contains(t)).collect();
        let cb: Vec<&String> = ob.iter().filter(|t| oa.contains(t)).collect();
        let moved = ca
            .iter()
            .enumerate()
            .filter(|(i, t)| cb.get(*i) != Some(*t))
            .map(|(_, t)| (*t).clone())
            .collect();
        Ok(LongformComparison {
            same_topics: only_a.is_empty() && only_b.is_empty(),
            same_order: oa == ob,
            moved,
            only_a,
            only_b,
            a: x,
            b: y,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LongformPickArgs {
    /// The long-form to keep.
    pub keep: Id<Cut>,
    /// The long-form to remove (with its parts, framings and overlays).
    pub drop: Id<Cut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LongformPicked {
    pub kept: String,
    pub dropped: String,
    pub dropped_title: String,
}

#[derive(Debug)]
pub struct LongformPick;

#[command(
    name = "longform_pick",
    scope = "edit",
    undo = "undoable",
    doc = "Pick one of two long-forms after comparing them: the other is removed (its parts, framings and overlays) in one undoable step. Keeping both is doing nothing.",
    example = r#"{"keep": "01J00000000000000000000002", "drop": "01J00000000000000000000001"}"#
)]
impl Command for LongformPick {
    type Args = LongformPickArgs;
    type Output = LongformPicked;
    fn run(a: LongformPickArgs, cx: &mut Cx<'_>) -> Result<LongformPicked, CmdError> {
        long_form(cx, a.keep, "/keep")?;
        let d = long_form(cx, a.drop, "/drop")?;
        if a.keep == a.drop {
            return Err(CmdError::invalid(
                "/drop",
                "another long-form than the one kept",
            ));
        }
        super::source::delete_subtree(cx, a.drop.entity())?;
        Ok(LongformPicked {
            kept: a.keep.0.to_string(),
            dropped: a.drop.0.to_string(),
            dropped_title: d.title,
        })
    }
}

// ---------------------------------------------------------------------------------------
// cut_parent_diff / cut_pull_from_parent
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct CutParentDiff;

#[command(
    name = "cut_parent_diff",
    scope = "read",
    undo = "read_only",
    doc = "Show what changed in the stream edit inside a long-form's topics since the long-form last took its changes (stream edit changed: N edits): stretches struck or restored, per topic. Nothing is applied.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for CutParentDiff {
    type Args = CutOnlyArgs;
    type Output = ParentDiff;
    fn run(a: CutOnlyArgs, cx: &mut Cx<'_>) -> Result<ParentDiff, CmdError> {
        cx.tx.get(a.cut)?;
        parent_diff(cx.tx.peek(), a.cut)
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Pulled {
    pub applied: usize,
    pub parts_changed: usize,
    pub parts_added: usize,
    pub parts_removed: usize,
}

#[derive(Debug)]
pub struct CutPullFromParent;

#[command(
    name = "cut_pull_from_parent",
    scope = "edit",
    undo = "undoable",
    doc = "Pull the stream edit's changes into a long-form (the edits cut_parent_diff lists), as one undoable transaction: stretches the stream edit struck go, stretches it restored come back in their chapter. Only on this explicit call.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for CutPullFromParent {
    type Args = CutOnlyArgs;
    type Output = Pulled;
    fn run(a: CutOnlyArgs, cx: &mut Cx<'_>) -> Result<Pulled, CmdError> {
        let c = cx.tx.get(a.cut)?.clone();
        let diff = parent_diff(cx.tx.peek(), a.cut)?;
        let parent: Id<Cut> = id_of(&diff.parent)?;
        // the reads the diff made (recorded, so a concurrent parent edit conflicts): the
        // parts of every stream edit its topics are on
        let mut streams = vec![parent];
        for t in &c.lineage.topics {
            if let Ok(x) = cx.tx.get(*t)
                && !streams.contains(&x.cut)
            {
                streams.push(x.cut);
            }
        }
        for s in streams {
            for p in cx.tx.children::<Part>(s.entity()) {
                cx.tx.get(p)?;
            }
        }
        let mut out = Pulled {
            applied: 0,
            parts_changed: 0,
            parts_added: 0,
            parts_removed: 0,
        };
        // struck first, from the end backwards
        let mut struck: Vec<&ParentEdit> =
            diff.edits.iter().filter(|e| e.kind == "struck").collect();
        struck.sort_by_key(|e| std::cmp::Reverse(e.from.0));
        for e in struck {
            let (ch, rm, _) = super::words::remove_span(cx, a.cut, e.from, e.to, Some(e.media))?;
            out.parts_changed += ch;
            out.parts_removed += rm;
            out.applied += 1;
        }
        for e in diff.edits.iter().filter(|e| e.kind == "restored") {
            restore_into(cx, a.cut, &c, e, &mut out)?;
            out.applied += 1;
        }
        // the new base: what the parent keeps inside the topics now
        let topics: Vec<(Id<Topic>, Topic)> = c
            .lineage
            .topics
            .iter()
            .filter_map(|id| {
                cx.tx
                    .peek()
                    .get(*id)
                    .filter(|t| t.state == ProposalState::Accepted)
                    .map(|t| (*id, t.clone()))
            })
            .collect();
        let base = base_of(cx.tx.peek(), &topics);
        cx.tx.update(a.cut, |x| x.lineage.parent_base = base)?;
        Ok(out)
    }
}

/// Brings a stretch the parent restored back into the long-form: a part ending (or
/// starting) at it grows over it; else a new part goes after the chapter's last part
/// before it (or before its first, or at the end).
fn restore_into(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    c: &Cut,
    e: &ParentEdit,
    out: &mut Pulled,
) -> Result<(), CmdError> {
    let (s, t) = (e.from.0, e.to.0);
    let parts: Vec<(Id<Part>, Part)> = cx
        .tx
        .children::<Part>(cut.entity())
        .into_iter()
        .filter_map(|id| cx.tx.peek().get(id).map(|p| (id, p.clone())))
        .collect();
    let same = |p: &Part| p.media == e.media;
    if parts
        .iter()
        .any(|(_, p)| same(p) && p.src_in.0 <= s && t <= p.src_out.0)
    {
        return Ok(());
    }
    if let Some((id, p)) = parts
        .iter()
        .find(|(_, p)| same(p) && p.src_out.0 >= s && p.src_out.0 < t && p.src_in.0 < s)
    {
        super::record_part_children(cx, *id);
        gmclip_model::edit::trim_part(cx.tx.staging(), *id, p.src_in, Flicks(t))
            .map_err(crate::command::model_err)?;
        out.parts_changed += 1;
        return Ok(());
    }
    if let Some((id, p)) = parts
        .iter()
        .find(|(_, p)| same(p) && p.src_in.0 <= t && p.src_in.0 > s && p.src_out.0 > t)
    {
        super::record_part_children(cx, *id);
        gmclip_model::edit::trim_part(cx.tx.staging(), *id, Flicks(s), p.src_out)
            .map_err(crate::command::model_err)?;
        out.parts_changed += 1;
        return Ok(());
    }
    // on its stream edit's frame grid, as the long-form's parts were cut (`kept_in`)
    let topic_range = cx.tx.peek().get(id_of::<Topic>(&e.topic)?).map(|x| {
        let (a, b) = span_range(&x.span);
        let fpf = stream_fpf(cx.tx.peek(), x.cut);
        (nearest_frame(a, fpf), nearest_frame(b, fpf))
    });
    let in_topic =
        |p: &Part| same(p) && topic_range.is_some_and(|r| r.0 <= p.src_in.0 && p.src_in.0 < r.1);
    let after = parts
        .iter()
        .filter(|(_, p)| in_topic(p) && p.src_in.0 < s)
        .map(|(id, _)| *id)
        .next_back()
        .or_else(|| {
            // before the chapter's first part: after the part before that one
            let first = parts.iter().position(|(_, p)| in_topic(p))?;
            first.checked_sub(1).map(|i| parts[i].0)
        })
        .or_else(|| {
            if parts.iter().any(|(_, p)| in_topic(p)) {
                None
            } else {
                parts.last().map(|x| x.0)
            }
        });
    let _ = c;
    super::part::PartAdd::run(
        super::part::PartAddArgs {
            cut,
            after,
            media: e.media,
            src_in: Flicks(s),
            src_out: Flicks(t),
            role: None,
        },
        cx,
    )?;
    out.parts_added += 1;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// framing_add
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum Aspect {
    #[serde(rename = "9:16")]
    Portrait,
    #[serde(rename = "16:9")]
    Landscape,
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "4:5")]
    Feed,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FramingAddArgs {
    pub cut: Id<Cut>,
    /// A name unique in the cut, e.g. "screen focus" or "face big".
    pub name: String,
    pub aspect: Aspect,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FramingAdded {
    pub framing: String,
    /// The framing of the same aspect it started from (layers, per-part layouts and
    /// crops, caption seat), if the cut had one.
    pub copied_from: Option<String>,
}

#[derive(Debug)]
pub struct FramingAdd;

#[command(
    name = "framing_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add another framing (a 9:16, 16:9, 1:1 or 4:5 canvas with its own crops and layouts) to a cut, e.g. a second vertical crop of the same short; the cut's words and captions are shared. It starts as a copy of the cut's framing of that aspect.",
    example = r#"{"cut": "01J00000000000000000000001", "name": "face big", "aspect": "9:16"}"#
)]
impl Command for FramingAdd {
    type Args = FramingAddArgs;
    type Output = FramingAdded;
    fn run(a: FramingAddArgs, cx: &mut Cx<'_>) -> Result<FramingAdded, CmdError> {
        let name = super::topic::clean_text(&a.name);
        if name.is_empty() {
            return Err(CmdError::invalid("/name", "a name"));
        }
        super::bounded(&name, "/name", 60)?;
        cx.tx.get(a.cut)?;
        let (w, h, class) = match a.aspect {
            Aspect::Portrait => (1080, 1920, AspectClass::Portrait),
            Aspect::Landscape => (1920, 1080, AspectClass::Landscape),
            Aspect::Square => (1080, 1080, AspectClass::Square),
            Aspect::Feed => (1080, 1350, AspectClass::Portrait),
        };
        let mut sibs: Vec<Framing> = Vec::new();
        for f in cx.tx.children::<Framing>(a.cut.entity()) {
            sibs.push(cx.tx.get(f)?.clone());
        }
        if sibs.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
            return Err(CmdError::invalid(
                "/name",
                "the cut already has a framing of that name",
            ));
        }
        if sibs.len() >= 12 {
            return Err(CmdError::invalid("/cut", "at most 12 framings per cut"));
        }
        let model = sibs
            .iter()
            .find(|f| f.canvas.w == w && f.canvas.h == h)
            .or_else(|| sibs.iter().find(|f| f.aspect_class == class))
            .cloned();
        let fps = model
            .as_ref()
            .map(|f| f.fps)
            .or_else(|| sibs.first().map(|f| f.fps))
            .unwrap_or(Rational::new(30, 1));
        let id: Id<Framing> = cx.tx.mint();
        let base = model.clone().unwrap_or(Framing {
            id,
            cut: a.cut,
            name: String::new(),
            canvas: Canvas { w, h },
            fps,
            aspect_class: class,
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
        });
        cx.tx.create(Framing {
            id,
            cut: a.cut,
            name,
            canvas: Canvas { w, h },
            fps,
            aspect_class: class,
            ..base
        })?;
        let Some(m) = model else {
            full_frame_layer(cx, id)?;
            return Ok(FramingAdded {
                framing: id.0.to_string(),
                copied_from: None,
            });
        };
        // the default stack, with new ids
        let mut map: BTreeMap<Id<Layer>, Id<Layer>> = BTreeMap::new();
        for l in cx.tx.children::<Layer>(m.id.entity()) {
            let x = cx.tx.get(l)?.clone();
            let nid: Id<Layer> = cx.tx.mint();
            map.insert(l, nid);
            cx.tx.create(Layer {
                id: nid,
                owner: LayerOwner::Framing(id),
                ..x
            })?;
        }
        // each part's look and caption seat in the model framing, remapped
        for p in cx.tx.children::<Part>(a.cut.entity()) {
            let part = cx.tx.get(p)?.clone();
            let look = part.look.get(&m.id).cloned();
            let seat = part.caption_place_override.get(&m.id).copied();
            if look.is_none() && seat.is_none() {
                continue;
            }
            let look = look.map(|mut l| {
                l.overrides = l
                    .overrides
                    .into_iter()
                    .filter_map(|(k, v)| map.get(&k).map(|n| (*n, v)))
                    .collect();
                l
            });
            cx.tx.update(p, |x| {
                if let Some(l) = look {
                    x.look.insert(id, l);
                }
                if let Some(s) = seat {
                    x.caption_place_override.insert(id, s);
                }
            })?;
        }
        Ok(FramingAdded {
            framing: id.0.to_string(),
            copied_from: Some(m.id.0.to_string()),
        })
    }
}

// ---------------------------------------------------------------------------------------
// project_map
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectMapArgs {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MapFraming {
    pub id: String,
    pub name: String,
    /// `9:16`, `16:9`, `1:1`, `4:5` or `w×h`.
    pub aspect: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MapTopic {
    pub id: String,
    pub title: String,
    pub colour: String,
    pub origin: EditOrigin,
    pub state: ProposalState,
    pub conflict: bool,
    pub replaces: Vec<String>,
    pub media: MediaRef,
    pub from: Flicks,
    pub to: Flicks,
    pub from_ms: i64,
    pub to_ms: i64,
    /// What the stream edit keeps of it.
    pub kept_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MapStream {
    pub cut: String,
    pub title: String,
    /// Its video when it holds one (`None` when it holds several: `videos`).
    pub media: Option<MediaRef>,
    /// Every video it holds, in order (decision 0036).
    pub videos: Vec<MediaRef>,
    /// The stream's whole length and what the stream edit keeps.
    pub total_ms: i64,
    pub kept_ms: i64,
    pub framings: Vec<MapFraming>,
    pub topics: Vec<MapTopic>,
    /// Fluff proposals waiting for the human.
    pub fluff_proposed: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MapCut {
    pub cut: String,
    pub title: String,
    pub kind: CutKind,
    pub duration_ms: i64,
    pub framings: Vec<MapFraming>,
    /// Long-forms: their topics (chapter order). Shorts: the topic holding their first
    /// part, if any.
    pub topics: Vec<String>,
    /// Source ranges it plays on the stream's media, merged (`[from_ms, to_ms]`):
    /// "Shorts here" and the coverage chips.
    pub ranges: Vec<[i64; 2]>,
    /// Long-forms: stream-edit edits not taken yet.
    pub parent_edits: usize,
    /// The last review verdict, if any (`keep`, `change`, `drop`).
    pub review: Option<String>,
    /// The sources and sync groups its parts play, in first-use order (a cut may mix
    /// videos).
    pub media: Vec<MediaRef>,
}

/// One video of the project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MapSource {
    pub id: String,
    /// The file's name without its extension.
    pub name: String,
    pub path: String,
    pub duration_ms: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<Rational>,
    pub audio_tracks: usize,
    /// The same-recording group it is a member of, and its role there.
    pub group: Option<String>,
    pub group_role: Option<MemberRole>,
    /// The media its words and stream edit are on: the source, or its sync group.
    pub media: MediaRef,
    /// The stream edit, when it plays this video (decision 0036: one holds them all).
    pub stream: Option<String>,
    /// Cuts other than its stream edit with parts on it (or on its group): it cannot be
    /// removed while there are any.
    pub used_by: Vec<String>,
    pub moments: usize,
    /// Its stream layout's name ("Draw your layout once", SPEC §5.7.2), when it has one.
    pub layout: Option<String>,
    /// The boxes that layout names, by role (`cam`, `screen`, `chat`, `private`…).
    pub layout_roles: Vec<RegionRole>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectMap {
    /// The stream edit: the main cut, holding every video (decision 0036).
    pub stream: Option<MapStream>,
    /// The same, as a list (one at most).
    pub streams: Vec<MapStream>,
    pub sources: Vec<MapSource>,
    pub longs: Vec<MapCut>,
    pub shorts: Vec<MapCut>,
}

/// The media of each source: its sync group's when it is a member, else its own.
pub fn media_of_source(
    doc: &Doc,
    s: Id<Source>,
) -> (MediaRef, Option<(Id<SyncGroup>, MemberRole)>) {
    doc.iter::<SyncMember>()
        .find(|m| m.source == s)
        .map_or((MediaRef::Source(s), None), |m| {
            (MediaRef::Group(m.group), Some((m.group, m.role)))
        })
}

/// The cuts (not stream edits) whose parts play `media`.
pub fn cuts_using(doc: &Doc, media: &[MediaRef]) -> Vec<Id<Cut>> {
    doc.iter::<Cut>()
        .filter(|c| c.kind != CutKind::Stream)
        .filter(|c| {
            doc.children::<Part>(c.id.entity())
                .filter_map(|p| doc.get(p))
                .any(|p| media.contains(&p.media))
        })
        .map(|c| c.id)
        .collect()
}

/// The project's videos as the map lists them, in the order they were added.
pub fn map_sources(doc: &Doc) -> Vec<MapSource> {
    doc.iter::<Source>()
        .map(|s| {
            let (media, group) = media_of_source(doc, s.id);
            let mut uses = vec![MediaRef::Source(s.id)];
            if media != MediaRef::Source(s.id) {
                uses.push(media);
            }
            let v = s.probe.tracks.iter().find(|t| t.kind == TrackKind::Video);
            let path = s.path.rsplit(['/', '\\']).next().unwrap_or(&s.path);
            let name = path.rsplit_once('.').map_or(path, |(n, _)| n);
            MapSource {
                id: s.id.0.to_string(),
                name: name.chars().take(200).collect(),
                path: s.path.clone(),
                duration_ms: ms(s.probe.duration.0),
                width: v.and_then(|t| t.width),
                height: v.and_then(|t| t.height),
                fps: v.and_then(|t| t.fps),
                audio_tracks: s.probe.audio_tracks.len(),
                group: group.map(|g| g.0.0.to_string()),
                group_role: group.map(|g| g.1),
                media,
                stream: super::topic::stream_edit_for(doc, media).map(|c| c.0.to_string()),
                used_by: cuts_using(doc, &uses)
                    .into_iter()
                    .map(|c| c.0.to_string())
                    .collect(),
                moments: doc
                    .iter::<Moment>()
                    .filter(|m| uses.contains(&m.media))
                    .count(),
                layout: s
                    .stream_layout
                    .and_then(|l| doc.get(l))
                    .map(|l| l.name.chars().take(200).collect()),
                layout_roles: s
                    .stream_layout
                    .and_then(|l| doc.get(l))
                    .map(|l| l.regions.iter().map(|r| r.role).collect())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn aspect_name(f: &Framing) -> String {
    let (w, h) = (f.canvas.w, f.canvas.h);
    match (w, h) {
        _ if w * 16 == h * 9 => "9:16".into(),
        _ if w * 9 == h * 16 => "16:9".into(),
        _ if w == h => "1:1".into(),
        _ if w * 5 == h * 4 => "4:5".into(),
        _ => format!("{w}×{h}"),
    }
}

fn framings_of(doc: &Doc, cut: Id<Cut>) -> Vec<MapFraming> {
    doc.children::<Framing>(cut.entity())
        .filter_map(|f| doc.get(f))
        .map(|f| MapFraming {
            id: f.id.0.to_string(),
            name: f.name.clone(),
            aspect: aspect_name(f),
        })
        .collect()
}

/// Total length of a media: a source's probed duration, a group's audio member's.
fn media_total(doc: &Doc, m: MediaRef) -> i64 {
    let src = match m {
        MediaRef::Source(s) => Some(s),
        MediaRef::Group(g) => doc
            .get(g)
            .and_then(|g| g.master_audio)
            .and_then(|a| doc.get(a))
            .map(|a| a.source),
    };
    src.and_then(|s| doc.get(s))
        .map_or(0, |s| s.probe.duration.0)
}

fn map_stream(doc: &Doc, s: Id<Cut>) -> Option<MapStream> {
    let c = doc.get(s)?;
    let topics: Vec<(Id<Topic>, Topic, (i64, i64))> = doc
        .children::<Topic>(s.entity())
        .filter_map(|id| doc.get(id).map(|t| (id, t.clone(), span_range(&t.span))))
        .collect();
    let layout = cut_layout(doc, s);
    let kept: i64 = layout.iter().map(|x| x.dur.0).sum();
    // every video it holds, whole (decision 0036)
    let videos = super::topic::stream_medias(doc, s);
    let total = videos
        .iter()
        .map(|m| media_total(doc, *m))
        .sum::<i64>()
        .max(kept);
    Some(MapStream {
        cut: s.0.to_string(),
        title: c.title.clone(),
        media: match videos.as_slice() {
            [m] => Some(*m),
            _ => None,
        },
        videos,
        total_ms: ms(total),
        kept_ms: ms(kept),
        framings: framings_of(doc, s),
        topics: topics
            .iter()
            .map(|(id, t, r)| MapTopic {
                id: id.0.to_string(),
                title: t.title.clone(),
                colour: t.colour.clone(),
                origin: t.origin,
                state: t.state,
                conflict: t.conflict,
                replaces: t.replaces.iter().map(|x| x.0.to_string()).collect(),
                media: t.media,
                from: Flicks(r.0),
                to: Flicks(r.1),
                from_ms: ms(r.0),
                to_ms: ms(r.1),
                kept_ms: ms(kept_in(doc, s, t).iter().map(|(a, b)| b - a).sum()),
            })
            .collect(),
        fluff_proposed: doc
            .children::<FluffProposal>(s.entity())
            .filter_map(|f| doc.get(f))
            .filter(|f| f.state == ProposalState::Proposed)
            .count(),
    })
}

/// The project as the map navigator shows it (see the module docs).
pub fn project_map(doc: &Doc) -> ProjectMap {
    let main = super::topic::stream_edit(doc);
    let streams: Vec<MapStream> = main.and_then(|s| map_stream(doc, s)).into_iter().collect();
    // every video's topics: a short groups under the topic holding its first part
    let topics: Vec<(Id<Topic>, Topic, (i64, i64))> = main
        .into_iter()
        .flat_map(|s| {
            doc.children::<Topic>(s.entity())
                .filter_map(|id| doc.get(id).map(|t| (id, t.clone(), span_range(&t.span))))
        })
        .collect();
    let mut map = ProjectMap {
        stream: None,
        streams: Vec::new(),
        sources: map_sources(doc),
        longs: Vec::new(),
        shorts: Vec::new(),
    };
    for c in doc.iter::<Cut>() {
        if c.kind == CutKind::Stream {
            continue;
        }
        let parts: Vec<&Part> = cut_layout(doc, c.id)
            .iter()
            .filter_map(|sp| doc.get(sp.part))
            .collect();
        let mut media: Vec<MediaRef> = Vec::new();
        for p in &parts {
            if !media.contains(&p.media) {
                media.push(p.media);
            }
        }
        let duration_ms = ms(cut_layout(doc, c.id).iter().map(|x| x.dur.0).sum());
        let ranges = merged(parts.iter().map(|p| (p.src_in.0, p.src_out.0)).collect())
            .into_iter()
            .map(|(a, b)| [ms(a), ms(b)])
            .collect();
        let review = doc
            .children::<Review>(c.id.entity())
            .filter_map(|r| doc.get(r))
            .max_by_key(|r| r.at_ms)
            .map(|r| {
                match r.verdict {
                    Verdict::Keep => "keep",
                    Verdict::Change => "change",
                    Verdict::Drop => "drop",
                }
                .to_owned()
            });
        let (topic_ids, parent_edits) = if c.kind == CutKind::Long && !c.lineage.topics.is_empty() {
            (
                c.lineage.topics.iter().map(|t| t.0.to_string()).collect(),
                parent_diff(doc, c.id).map_or(0, |d| d.edits.len()),
            )
        } else {
            // the earliest stretch of the first video it plays
            let first = parts
                .iter()
                .filter(|p| media.first() == Some(&p.media))
                .min_by_key(|p| p.src_in.0);
            // on the topic's stream edit's frame grid, as parts are cut (`kept_in`)
            let t = first.and_then(|p| {
                topics.iter().find(|(_, t, r)| {
                    let fpf = stream_fpf(doc, t.cut);
                    t.state == ProposalState::Accepted
                        && t.media == p.media
                        && nearest_frame(r.0, fpf) <= p.src_in.0
                        && p.src_in.0 < nearest_frame(r.1, fpf)
                })
            });
            (t.map(|x| vec![x.0.0.to_string()]).unwrap_or_default(), 0)
        };
        let row = MapCut {
            cut: c.id.0.to_string(),
            title: c.title.clone(),
            kind: c.kind,
            duration_ms,
            framings: framings_of(doc, c.id),
            topics: topic_ids,
            ranges,
            parent_edits,
            review,
            media,
        };
        match c.kind {
            CutKind::Long => map.longs.push(row),
            _ => map.shorts.push(row),
        }
    }
    map.stream = streams.first().cloned();
    map.streams = streams;
    map
}

#[derive(Debug)]
pub struct ProjectMapCmd;

#[command(
    name = "project_map",
    scope = "read",
    undo = "read_only",
    doc = "The project map: its videos (sources: length, size, rate, sync group, the cuts using each), each video's stream edit (length kept, topics with their state), long-forms (topics, pending stream-edit edits) and shorts grouped by topic, each with framings, duration, review state, the videos it plays and the source ranges.",
    example = r#"{}"#
)]
impl Command for ProjectMapCmd {
    type Args = ProjectMapArgs;
    type Output = ProjectMap;
    fn run(_: ProjectMapArgs, cx: &mut Cx<'_>) -> Result<ProjectMap, CmdError> {
        Ok(project_map(cx.tx.peek()))
    }
}
