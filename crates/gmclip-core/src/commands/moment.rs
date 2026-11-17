//! Moments the human reviews (SPEC §5.2): stage / archive / mark posted, and the excerpt
//! shown on the card. A minimal subset of backlog A2.03, built by lane U for the
//! moments panel and review mode; lane A2 owns the rest (manual moments, list
//! projections).

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::{Excerpt, Moment, MomentState};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::bounded;
use crate::command::{Command, Cx};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentSetStateArgs {
    pub moment: Id<Moment>,
    pub state: MomentState,
}

/// Sets where a moment is in review.
#[derive(Debug)]
pub struct MomentSetState;

#[command(
    name = "moment_set_state",
    scope = "edit",
    undo = "undoable",
    doc = "Move a moment between new, staged (work to do), archived (nothing to do), used and posted.",
    example = r#"{"moment": "01J00000000000000000000009", "state": "staged"}"#
)]
impl Command for MomentSetState {
    type Args = MomentSetStateArgs;
    type Output = super::Done;
    fn run(a: MomentSetStateArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        cx.tx.update(a.moment, |m| m.state = a.state)?;
        Ok(super::Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentExcerptSetArgs {
    pub moment: Id<Moment>,
    /// The words the card shows; empty clears it (the card falls back to the quote).
    pub text: String,
}

/// Whether the excerpt is exact (a contiguous run of whole words of the quote).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ExcerptSet {
    pub exact: bool,
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// `needle` is a contiguous run of whole words of `hay` (punctuation and case ignored).
pub fn is_exact_run(hay: &str, needle: &str) -> bool {
    let (h, n) = (words(hay), words(needle));
    !n.is_empty() && h.windows(n.len()).any(|w| w == n.as_slice())
}

/// Sets a moment's excerpt. A human may set a text that is not an exact quote: it is
/// kept with `exact: false` (SPEC R-AI-1); agents' passes must quote exactly.
#[derive(Debug)]
pub struct MomentExcerptSet;

#[command(
    name = "moment_excerpt_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set the excerpt a moment's card shows (exact when it is a contiguous run of the quote's words).",
    example = r#"{"moment": "01J00000000000000000000009", "text": "my PC made this in 67 seconds"}"#
)]
impl Command for MomentExcerptSet {
    type Args = MomentExcerptSetArgs;
    type Output = ExcerptSet;
    fn run(a: MomentExcerptSetArgs, cx: &mut Cx<'_>) -> Result<ExcerptSet, CmdError> {
        bounded(&a.text, "/text", 2000)?;
        let quote = cx.tx.get(a.moment)?.quote.clone();
        let text = a.text.trim().to_owned();
        let exact = text.is_empty() || is_exact_run(&quote, &text);
        cx.tx
            .update(a.moment, |m| m.excerpt = Excerpt { text, exact })?;
        Ok(ExcerptSet { exact })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentAddArgs {
    /// The media (`{"source": id}` or `{"group": id}`); default: what the caller's cut
    /// plays (a launched run's bound cut, or the project's only cut).
    #[serde(default)]
    pub media: Option<gmclip_model::entities::MediaRef>,
    /// Source seconds: where the moment starts and ends (≤ 10 min long).
    pub start_s: f64,
    pub end_s: f64,
    /// The words said, quoted exactly from the transcript (checked; `exact` reports it).
    pub quote: String,
    /// The excerpt the card shows (a contiguous run of the quote); default the quote.
    #[serde(default)]
    pub excerpt: Option<String>,
    /// 2–5 (5 = would post today).
    pub score: u8,
    /// What kind of moment (`result`, `reaction`, `story`, `tip`, `fail`, …).
    #[serde(rename = "type")]
    pub kind: String,
    /// Why it works, in one line.
    pub why: String,
    #[serde(default)]
    pub text_hook: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MomentAdded {
    pub id: String,
    /// The quote is a contiguous run of the transcript's words in the span.
    pub exact: bool,
}

/// Adds a moment (an agent's find-moments pass, SPEC §10.6; a human's manual moment).
#[derive(Debug)]
pub struct MomentAdd;

#[command(
    name = "moment_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a moment found in the footage: its source span in seconds, the exact quote, a 2–5 score, a type and why it works. It lands in the moments panel as new, labelled with who found it.",
    example = r#"{"start_s": 61.2, "end_s": 84.0, "quote": "we were able to generate that five-second video in less than two minutes", "score": 4, "type": "result", "why": "a measured result with a number"}"#
)]
impl Command for MomentAdd {
    type Args = MomentAddArgs;
    type Output = MomentAdded;
    fn run(a: MomentAddArgs, cx: &mut Cx<'_>) -> Result<MomentAdded, CmdError> {
        use gmclip_model::entities::Cut;
        let doc = cx.tx.peek();
        let media = match a.media {
            Some(m) => m,
            None => {
                let cut = cx
                    .caller
                    .cut_binding
                    .or_else(|| {
                        let mut it = doc.iter::<Cut>();
                        let first = it.next().map(|c| c.id);
                        if it.next().is_some() { None } else { first }
                    })
                    .ok_or_else(|| CmdError::invalid("/media", "name the media (several cuts)"))?;
                doc.children::<gmclip_model::entities::Part>(cut.entity())
                    .find_map(|p| doc.get(p))
                    .map(|p| p.media)
                    .ok_or_else(|| CmdError::invalid("/media", "the cut plays nothing"))?
            }
        };
        let who = cx.caller.identity.clone();
        let origin = if who == "human" {
            gmclip_model::entities::MomentOrigin::Manual
        } else {
            gmclip_model::entities::MomentOrigin::Agent
        };
        let f = FoundMoment {
            start_s: a.start_s,
            end_s: a.end_s,
            quote: a.quote,
            excerpt: a.excerpt,
            score: a.score,
            kind: a.kind,
            why: a.why,
            text_hook: a.text_hook,
        };
        let (id, exact) = add_moment(cx, media, &f, "", origin, None)?;
        Ok(MomentAdded {
            id: id.0.to_string(),
            exact,
        })
    }
}

/// One moment a pass found (or a human adds): source seconds, the quote and the card.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FoundMoment {
    pub start_s: f64,
    pub end_s: f64,
    pub quote: String,
    #[serde(default)]
    pub excerpt: Option<String>,
    /// 2–5.
    pub score: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub why: String,
    #[serde(default)]
    pub text_hook: Option<String>,
}

/// Creates one moment on `media` (checked; `exact` when its quote is a run of the
/// transcript's words in the span), labelled with who found it.
fn add_moment(
    cx: &mut Cx<'_>,
    media: gmclip_model::entities::MediaRef,
    a: &FoundMoment,
    path: &str,
    origin: gmclip_model::entities::MomentOrigin,
    pass_ref: Option<String>,
) -> Result<(Id<Moment>, bool), CmdError> {
    use gmclip_model::Flicks;
    use gmclip_model::anchor::{Anchor, RawTime};
    use gmclip_model::clock::ModelTime;
    use gmclip_model::entities::{Edges, MediaRef, MomentSpan};
    const SEC: f64 = 705_600_000.0;
    let at = |k: &str| format!("{path}/{k}");
    bounded(&a.quote, &at("quote"), 4000)?;
    bounded(&a.kind, &at("type"), 40)?;
    bounded(&a.why, &at("why"), 600)?;
    if let Some(h) = &a.text_hook {
        bounded(h, &at("text_hook"), 200)?;
    }
    if let Some(x) = &a.excerpt {
        bounded(x, &at("excerpt"), 2000)?;
    }
    if a.quote.trim().is_empty() {
        return Err(CmdError::invalid(&at("quote"), "the words said"));
    }
    if !(2..=5).contains(&a.score) {
        return Err(CmdError::invalid(&at("score"), "2–5"));
    }
    if !a.start_s.is_finite()
        || !a.end_s.is_finite()
        || a.start_s < 0.0
        || a.end_s <= a.start_s
        || a.end_s - a.start_s > 600.0
    {
        return Err(CmdError::invalid(
            &at("end_s"),
            "a span after start_s, at most 10 minutes",
        ));
    }
    let doc = cx.tx.peek();
    let owner = match media {
        MediaRef::Source(s) => {
            doc.get(s).ok_or(CmdError::NotFound)?;
            s.0.to_string()
        }
        MediaRef::Group(g) => {
            doc.get(g).ok_or(CmdError::NotFound)?;
            g.0.to_string()
        }
    };
    let (t0, t1) = ((a.start_s * SEC) as i64, (a.end_s * SEC) as i64);
    // Exact when the quote is a run of the words the transcript has in the span.
    let exact = cx.analysis.active(doc, &owner).is_some_and(|t| {
        let said: Vec<&str> = t
            .words
            .iter()
            .filter(|w| w.end > t0 - (SEC as i64) / 2 && w.start < t1 + (SEC as i64) / 2)
            .map(|w| w.text.as_str())
            .collect();
        is_exact_run(&said.join(" "), &a.quote)
    });
    let excerpt = a.excerpt.clone().unwrap_or_else(|| a.quote.clone());
    let excerpt_exact = is_exact_run(&a.quote, &excerpt);
    let who = cx.caller.identity.clone();
    let raw = |t: i64| {
        Anchor::Raw(RawTime {
            time: match media {
                MediaRef::Source(s) => ModelTime::Src {
                    source: Some(s),
                    group: None,
                    t: Flicks(t),
                },
                MediaRef::Group(g) => ModelTime::Src {
                    source: None,
                    group: Some(g),
                    t: Flicks(t),
                },
            },
            reason: format!("moment found by {who}"),
        })
    };
    // A launched run is "your agent" (the panel says so); a connected client by name.
    let label = (!who.starts_with("launched:") && who != "human")
        .then(|| who.strip_prefix("local:").unwrap_or(&who).to_owned());
    let id: Id<Moment> = cx.tx.mint();
    cx.tx.create(Moment {
        id,
        media,
        span: MomentSpan {
            start: raw(t0),
            end: raw(t1),
            edges: Edges::Word,
        },
        payoff: None,
        quote: a.quote.trim().to_owned(),
        excerpt: Excerpt {
            text: excerpt.trim().to_owned(),
            exact: excerpt_exact,
        },
        score: a.score,
        r#type: a.kind.clone(),
        why: a.why.clone(),
        payoff_hint: None,
        text_hook: a.text_hook.clone(),
        origin,
        origin_ref: None,
        entry_angle: None,
        variant_of: None,
        visual: None,
        state: MomentState::New,
        pass_ref,
        harness_label: label,
    })?;
    Ok((id, exact))
}

/// Which pass found them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FoundBy {
    /// Find moments (origin `agent`).
    FindMoments,
    /// The full sweep (origin `sweep`).
    FullSweep,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentsFoundAddArgs {
    pub media: gmclip_model::entities::MediaRef,
    pub by: FoundBy,
    /// The pass's reference and its run: `<pass>@<version>:<hash>/<pass_run>`.
    pub pass_ref: String,
    /// At most 200, each checked as `moment_add` checks one.
    pub moments: Vec<FoundMoment>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MomentsFoundAdded {
    pub ids: Vec<String>,
    /// How many quotes are a run of the transcript's words in their span.
    pub exact: usize,
}

/// Writes one step of an agent-executed pass (SPEC §10.6): the moments gmclip checked,
/// anchored and de-duplicated, in one transaction. The agent surface calls it on a
/// `pass_step`; an agent never reaches it directly (its moments come only through the
/// pass contract).
#[derive(Debug)]
pub struct MomentsFoundAdd;

#[command(
    name = "moments_found_add",
    scope = "edit",
    undo = "undoable",
    doc = "Write the moments one step of a find-moments pass or full sweep found (gmclip's pass contract writes these; agents submit through pass_step).",
    example = r#"{"media": {"source": "01J00000000000000000000003"}, "by": "find_moments", "pass_ref": "find_moments@1:0123456789ab/pr01", "moments": [{"start_s": 61.2, "end_s": 84.0, "quote": "we were able to generate that five-second video in less than two minutes", "score": 4, "type": "result", "why": "a measured result with a number"}]}"#
)]
impl Command for MomentsFoundAdd {
    type Args = MomentsFoundAddArgs;
    type Output = MomentsFoundAdded;
    fn run(a: MomentsFoundAddArgs, cx: &mut Cx<'_>) -> Result<MomentsFoundAdded, CmdError> {
        use gmclip_model::entities::MomentOrigin;
        bounded(&a.pass_ref, "/pass_ref", 200)?;
        if a.moments.len() > 200 {
            return Err(CmdError::invalid("/moments", "at most 200"));
        }
        let origin = match a.by {
            FoundBy::FindMoments => MomentOrigin::Agent,
            FoundBy::FullSweep => MomentOrigin::Sweep,
        };
        let mut ids = Vec::with_capacity(a.moments.len());
        let mut exact = 0;
        for (i, m) in a.moments.iter().enumerate() {
            let (id, ok) = add_moment(
                cx,
                a.media,
                m,
                &format!("/moments/{i}"),
                origin,
                Some(a.pass_ref.clone()),
            )?;
            ids.push(id.0.to_string());
            exact += usize::from(ok);
        }
        Ok(MomentsFoundAdded { ids, exact })
    }
}

/// Where a marked span's edges go (L.01, L.04).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkSnap {
    /// To the edges of the words the span touches (a search hit, a chat spike).
    #[default]
    Word,
    /// To the nearest sentence start and end, as human-typed times do (SPEC §5.2): a
    /// sentence ends at `.?!` or a pause of ≥ 0.6 s.
    Sentence,
}

/// What a marked moment came from (its card says so).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkFrom {
    /// A library search hit (L.01).
    #[default]
    Search,
    /// In and out tapped while watching (L.04).
    Tap,
    /// A chat-log spike (L.03): origin `chat`.
    Chat,
    /// A reaction in the sound and the words (a laugh, "no way"): origin `local`, found
    /// by sound and transcript.
    Reaction,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentMarkArgs {
    /// The media (`{"source": id}` or `{"group": id}`); default as for `moment_add`.
    #[serde(default)]
    pub media: Option<gmclip_model::entities::MediaRef>,
    /// Source seconds, before snapping (≤ 10 min apart).
    pub start_s: f64,
    pub end_s: f64,
    #[serde(default)]
    pub snap: MarkSnap,
    #[serde(default)]
    pub from: MarkFrom,
    /// One line on the card (the search words, the spike's size); untrusted when it
    /// comes from chat.
    #[serde(default)]
    pub note: Option<String>,
    /// 2–5 (default 3); a human may give 1.
    #[serde(default)]
    pub score: Option<u8>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MomentMarked {
    pub id: String,
    /// The snapped span, source seconds (word edges).
    pub start_s: f64,
    pub end_s: f64,
    pub words: u32,
    /// The quote: the words said in the span.
    pub quote: String,
}

/// Sentence `[first, last]` word ranges: a sentence ends at `.?!` or a pause ≥ 0.6 s.
pub fn sentences(words: &[crate::analysis::Word]) -> Vec<(usize, usize)> {
    const SEC: i64 = 705_600_000;
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..words.len() {
        let ends = words[i].text.trim_end().ends_with(['.', '?', '!'])
            || words
                .get(i + 1)
                .is_none_or(|n| n.start - words[i].end >= SEC * 6 / 10);
        if ends {
            out.push((start, i));
            start = i + 1;
        }
    }
    out
}

/// The word range `[a, b]` a span snaps to, or `None` when it holds no word.
pub fn snap_span(
    words: &[crate::analysis::Word],
    t0: i64,
    t1: i64,
    snap: MarkSnap,
) -> Option<(usize, usize)> {
    if words.is_empty() || t1 <= t0 {
        return None;
    }
    match snap {
        MarkSnap::Word => {
            let a = words.partition_point(|w| w.end <= t0);
            let b = words.partition_point(|w| w.start < t1).checked_sub(1)?;
            (a <= b && a < words.len()).then_some((a, b))
        }
        MarkSnap::Sentence => {
            let ss = sentences(words);
            // the sentence start nearest t0, the sentence end nearest t1 after it
            let a = ss
                .iter()
                .map(|s| s.0)
                .min_by_key(|&i| (words[i].start - t0).abs())?;
            let b = ss
                .iter()
                .map(|s| s.1)
                .filter(|&j| j >= a)
                .min_by_key(|&j| (words[j].end - t1).abs())?;
            Some((a, b))
        }
    }
}

fn word_ref_at(t: &crate::analysis::Transcript, i: usize) -> gmclip_model::anchor::WordRef {
    use crate::analysis::norm;
    use gmclip_model::Flicks;
    use gmclip_model::anchor::{WordId, WordRef};
    let w = &t.words[i];
    let around = |r: std::ops::Range<usize>| -> Vec<String> {
        r.filter_map(|j| t.words.get(j))
            .map(|w| norm(&w.text))
            .collect()
    };
    WordRef {
        word_id: WordId {
            asr_revision: t.revision.clone(),
            ordinal: u32::try_from(i).unwrap_or(u32::MAX),
        },
        t_src_start: Flicks(w.start),
        t_src_end: Flicks(w.end),
        normalised_text: norm(&w.text),
        prev2: around(i.saturating_sub(2)..i),
        next2: around(i + 1..i + 3),
    }
}

/// A moment over a marked span (L.01 "＋ Make moment" from a search hit, L.04 tapped
/// in/out, L.03 a chat spike): its edges snapped to words or sentences of the active
/// transcript and stored as word anchors, its quote the words said.
#[derive(Debug)]
pub struct MomentMark;

#[command(
    name = "moment_mark",
    scope = "edit",
    undo = "undoable",
    doc = "Make a moment from a span (a library search hit, a tapped in/out, a chat spike, a laugh or reaction): the edges snap to word edges or to sentence starts/ends of the transcript, the quote is the words said.",
    example = r#"{"start_s": 61.2, "end_s": 64.9, "snap": "word", "from": "search", "note": "gpu prices"}"#
)]
impl Command for MomentMark {
    type Args = MomentMarkArgs;
    type Output = MomentMarked;
    fn run(a: MomentMarkArgs, cx: &mut Cx<'_>) -> Result<MomentMarked, CmdError> {
        use gmclip_model::anchor::Anchor;
        use gmclip_model::entities::{Cut, Edges, MediaRef, MomentOrigin, MomentSpan, Part};
        const SEC: f64 = 705_600_000.0;
        if let Some(n) = &a.note {
            bounded(n, "/note", 300)?;
        }
        if !a.start_s.is_finite()
            || !a.end_s.is_finite()
            || a.start_s < 0.0
            || a.end_s <= a.start_s
            || a.end_s - a.start_s > 600.0
        {
            return Err(CmdError::invalid(
                "/end_s",
                "a span after start_s, at most 10 minutes",
            ));
        }
        let human = cx.caller.identity == "human";
        let score = a.score.unwrap_or(3);
        if !(if human { 1..=5 } else { 2..=5 }).contains(&score) {
            return Err(CmdError::invalid("/score", "2–5 (1 is a human's)"));
        }
        let doc = cx.tx.peek();
        let media = match a.media {
            Some(m) => m,
            None => {
                let cut = cx
                    .caller
                    .cut_binding
                    .or_else(|| {
                        let mut it = doc.iter::<Cut>();
                        let first = it.next().map(|c| c.id);
                        if it.next().is_some() { None } else { first }
                    })
                    .ok_or_else(|| CmdError::invalid("/media", "name the media (several cuts)"))?;
                doc.children::<Part>(cut.entity())
                    .find_map(|p| doc.get(p))
                    .map(|p| p.media)
                    .ok_or_else(|| CmdError::invalid("/media", "the cut plays nothing"))?
            }
        };
        let owner = match media {
            MediaRef::Source(s) => {
                doc.get(s).ok_or(CmdError::NotFound)?;
                s.0.to_string()
            }
            MediaRef::Group(g) => {
                doc.get(g).ok_or(CmdError::NotFound)?;
                g.0.to_string()
            }
        };
        let t = cx
            .analysis
            .active(doc, &owner)
            .cloned()
            .ok_or_else(|| CmdError::invalid("/media", "this media has no transcript yet"))?;
        let (t0, t1) = (
            (a.start_s * SEC).round() as i64,
            (a.end_s * SEC).round() as i64,
        );
        let (i, j) = snap_span(&t.words, t0, t1, a.snap)
            .ok_or_else(|| CmdError::invalid("/start_s", "no words are said in that span"))?;
        let quote: String = t.words[i..=j]
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let quote: String = quote.chars().take(4000).collect();
        let (kind, why) = match a.from {
            MarkFrom::Search => ("search", "From a library search"),
            MarkFrom::Tap => ("marked", "Marked while watching"),
            MarkFrom::Chat => ("chat", "Chat spiked here"),
            MarkFrom::Reaction => ("reaction", "A laugh or a big reaction here"),
        };
        let why = match &a.note {
            Some(n) if !n.trim().is_empty() => format!("{why}: {}", n.trim()),
            _ => why.to_owned(),
        };
        let who = cx.caller.identity.clone();
        let origin = match (a.from, human) {
            (MarkFrom::Chat, _) => MomentOrigin::Chat,
            (MarkFrom::Reaction, _) => MomentOrigin::Local,
            (_, true) => MomentOrigin::Manual,
            (_, false) => MomentOrigin::Agent,
        };
        let label = (!human && !who.starts_with("launched:"))
            .then(|| who.strip_prefix("local:").unwrap_or(&who).to_owned());
        let id: Id<Moment> = cx.tx.mint();
        let (ws, we) = (t.words[i].start, t.words[j].end);
        cx.tx.create(Moment {
            id,
            media,
            span: MomentSpan {
                start: Anchor::WordStart(word_ref_at(&t, i)),
                end: Anchor::WordEnd(word_ref_at(&t, j)),
                edges: Edges::Word,
            },
            payoff: None,
            quote: quote.clone(),
            excerpt: Excerpt {
                text: quote.clone(),
                exact: true,
            },
            score,
            r#type: kind.into(),
            why,
            payoff_hint: None,
            text_hook: None,
            origin,
            origin_ref: None,
            entry_angle: None,
            variant_of: None,
            visual: None,
            state: MomentState::New,
            pass_ref: None,
            harness_label: label,
        })?;
        Ok(MomentMarked {
            id: id.0.to_string(),
            start_s: ws as f64 / SEC,
            end_s: we as f64 / SEC,
            words: u32::try_from(j + 1 - i).unwrap_or(u32::MAX),
            quote,
        })
    }
}

// ---------------------------------------------------------------------------------------
// moments_list (A2.03): the moments panel's projection
// ---------------------------------------------------------------------------------------

/// How `moments_list` orders its rows.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MomentSort {
    /// By media, then by where the moment starts (the panel's default).
    #[default]
    Time,
    /// Highest score first, then by time.
    Score,
    /// New, staged, archived, used, posted; then by time.
    State,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentsListArgs {
    /// Only moments in these states (default: all).
    #[serde(default)]
    pub states: Option<Vec<MomentState>>,
    /// Only moments found this way (default: all).
    #[serde(default)]
    pub origins: Option<Vec<gmclip_model::entities::MomentOrigin>>,
    /// Only moments on this media (a source or sync group id).
    #[serde(default)]
    pub media: Option<String>,
    /// Only moments scored at least this.
    #[serde(default)]
    pub min_score: Option<u8>,
    #[serde(default)]
    pub sort: MomentSort,
    /// Rows to skip (paging) and at most this many rows (default and cap 5 000).
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One row of the moments panel.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MomentRow {
    pub id: String,
    /// The source or sync group id the moment is on.
    pub owner: String,
    pub score: u8,
    #[serde(rename = "type")]
    pub kind: String,
    /// The card's excerpt, and whether it is an exact run of the quote.
    pub excerpt: String,
    pub exact: bool,
    /// The quote (untrusted), at most 600 characters.
    pub quote: String,
    pub why: String,
    pub text_hook: Option<String>,
    pub state: MomentState,
    pub origin: gmclip_model::entities::MomentOrigin,
    pub origin_ref: Option<String>,
    pub harness_label: Option<String>,
    pub variant_of: Option<String>,
    pub entry_angle: Option<String>,
    /// The span in source seconds (`None` when an edge does not resolve, e.g. a line
    /// anchor of a transcript the project no longer holds).
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MomentsListed {
    /// Rows matching the filters (before paging).
    pub total: usize,
    /// Every moment by state, unfiltered (the filter chips' counts).
    pub counts: std::collections::BTreeMap<String, usize>,
    pub moments: Vec<MomentRow>,
}

/// A moment edge in source flicks: line anchors through the owner's transcripts.
fn edge_src(
    doc: &gmclip_model::Doc,
    view: &crate::analysis::AnalysisView,
    owner: &str,
    a: &gmclip_model::anchor::Anchor,
) -> Option<i64> {
    use gmclip_model::anchor::Anchor;
    match a {
        Anchor::LineStart(l) | Anchor::LineEnd(l) => {
            let line = view
                .transcript(owner, &l.asr_revision)
                .or_else(|| view.active(doc, owner))?
                .lines
                .get(l.line as usize)?
                .clone();
            Some(if matches!(a, Anchor::LineStart(_)) {
                line.start
            } else {
                line.end
            })
        }
        Anchor::Raw(r) => gmclip_model::clock::time_to_src(doc, &r.time).map(|t| t.0),
        other => gmclip_model::clock::anchor_src(doc, other).map(|t| t.0),
    }
}

fn state_rank(s: MomentState) -> u8 {
    match s {
        MomentState::New => 0,
        MomentState::Staged => 1,
        MomentState::Archived => 2,
        MomentState::Used => 3,
        MomentState::Posted => 4,
    }
}

/// The moments panel's rows, filtered and sorted in core (A2.03).
#[derive(Debug)]
pub struct MomentsList;

#[command(
    name = "moments_list",
    scope = "read",
    undo = "read_only",
    doc = "List the project's moments for review: score, type, excerpt, state, origin and source span, filtered by state, origin, media or score and sorted by time, score or state.",
    example = r#"{"states": ["new", "staged"], "sort": "score", "limit": 50}"#,
    example = r#"{}"#
)]
impl Command for MomentsList {
    type Args = MomentsListArgs;
    type Output = MomentsListed;
    fn run(a: MomentsListArgs, cx: &mut Cx<'_>) -> Result<MomentsListed, CmdError> {
        use gmclip_model::entities::MediaRef;
        const SEC: f64 = 705_600_000.0;
        if let Some(m) = &a.media {
            bounded(m, "/media", 64)?;
        }
        let limit = a.limit.unwrap_or(5_000).min(5_000);
        let doc = cx.tx.peek();
        let view = cx.analysis;
        let mut counts = std::collections::BTreeMap::new();
        let mut rows: Vec<(MomentRow, usize)> = Vec::new();
        for m in doc.iter::<Moment>() {
            let state = serde_json::to_value(m.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            *counts.entry(state).or_insert(0) += 1;
            let owner = match m.media {
                MediaRef::Source(s) => s.0.to_string(),
                MediaRef::Group(g) => g.0.to_string(),
            };
            if a.states.as_ref().is_some_and(|v| !v.contains(&m.state))
                || a.origins.as_ref().is_some_and(|v| !v.contains(&m.origin))
                || a.media.as_ref().is_some_and(|x| *x != owner)
                || a.min_score.is_some_and(|s| m.score < s)
            {
                continue;
            }
            let secs = |t: i64| (t as f64 / SEC * 1000.0).round() / 1000.0;
            let start = edge_src(doc, view, &owner, &m.span.start);
            let end = edge_src(doc, view, &owner, &m.span.end);
            rows.push((
                MomentRow {
                    id: m.id.0.to_string(),
                    owner,
                    score: m.score,
                    kind: m.r#type.clone(),
                    excerpt: m.excerpt.text.clone(),
                    exact: m.excerpt.exact,
                    quote: m.quote.chars().take(600).collect(),
                    why: m.why.clone(),
                    text_hook: m.text_hook.clone(),
                    state: m.state,
                    origin: m.origin,
                    origin_ref: m.origin_ref.clone(),
                    harness_label: m.harness_label.clone(),
                    variant_of: m.variant_of.map(|v| v.0.to_string()),
                    entry_angle: m.entry_angle.clone(),
                    start_s: start.map(secs),
                    end_s: end.map(secs),
                },
                rows.len(),
            ));
        }
        let at = |r: &MomentRow| r.start_s.unwrap_or(f64::MAX);
        rows.sort_by(|(x, i), (y, j)| {
            let by_time = x
                .owner
                .cmp(&y.owner)
                .then(at(x).total_cmp(&at(y)))
                .then(i.cmp(j));
            match a.sort {
                MomentSort::Time => by_time,
                MomentSort::Score => y.score.cmp(&x.score).then(by_time),
                MomentSort::State => state_rank(x.state).cmp(&state_rank(y.state)).then(by_time),
            }
        });
        let total = rows.len();
        let moments = rows
            .into_iter()
            .skip(a.offset.unwrap_or(0))
            .take(limit)
            .map(|(r, _)| r)
            .collect();
        Ok(MomentsListed {
            total,
            counts,
            moments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_runs() {
        let q = "So we were able to generate that five-second video, in less than two minutes.";
        assert!(is_exact_run(q, "generate that five-second video"));
        assert!(is_exact_run(q, "In less than two MINUTES"));
        assert!(!is_exact_run(q, "generate that video"));
        assert!(!is_exact_run(q, "two minutes flat"));
        assert!(!is_exact_run(q, "   "));
    }

    fn w(text: &str, s: f64, e: f64) -> crate::analysis::Word {
        crate::analysis::Word {
            text: text.into(),
            start: (s * 705_600_000.0) as i64,
            end: (e * 705_600_000.0) as i64,
            confidence: None,
        }
    }

    #[test]
    fn spans_snap_to_words_and_sentences() {
        let ws = vec![
            w("So", 0.0, 0.2),
            w("GPU", 0.3, 0.6),
            w("prices.", 0.7, 1.1),
            w("They", 1.2, 1.4),
            w("went", 1.5, 1.7),
            w("up", 1.8, 2.0),
            w("again", 3.0, 3.4),
        ];
        let f = |s: f64| (s * 705_600_000.0) as i64;
        // words the span touches
        assert_eq!(snap_span(&ws, f(0.5), f(1.0), MarkSnap::Word), Some((1, 2)));
        assert_eq!(snap_span(&ws, f(0.25), f(0.29), MarkSnap::Word), None);
        // sentences: "So GPU prices." · "They went up" (pause ≥ 0.6 s) · "again"
        assert_eq!(sentences(&ws), vec![(0, 2), (3, 5), (6, 6)]);
        // a tap a beat late lands on the sentence start; out mid-sentence on its end
        assert_eq!(
            snap_span(&ws, f(1.35), f(1.9), MarkSnap::Sentence),
            Some((3, 5))
        );
        assert_eq!(
            snap_span(&ws, f(0.4), f(3.0), MarkSnap::Sentence),
            Some((0, 6))
        );
    }
}
