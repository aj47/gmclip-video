//! The caption builder (SPEC §5.4, AGENTS.md "captions on"): heard captions from the
//! cut's own words — the active transcript of each part's media, placed on the output
//! clock through the parts — grouped into cues exactly as the reference's `story_reel.caption_cues`
//! groups them (`gmclip_plan::story_cues`, the port R-CAP-1 checks byte for byte):
//!
//! - a sentence ends at `.?!` or a pause over 0.9 s; within it a cost search picks 1–3
//!   word cues that do not end on a weak word ("the", "to", "my", …), keep pairs
//!   ("two minutes", the project's own) and a number with its unit, and use 4 words only
//!   to avoid a cue under 0.3 s;
//! - a cue holds up to 0.5 s past its last word, never into the next; the reference's frame rounding
//!   (30 fps) is kept so the boundaries land where the reference's do;
//! - fillers (um, uh, …) are dropped from captions only, when the doc says so;
//! - re-spellings and drops (`edits`), keywords (yellow runs) and no-caption spans apply;
//! - hand-made boundaries (`cue_breaks`: split here / join with the word before) override
//!   the search.
//!
//! Words come from the cut's own audio ([`super::heard`]): a part identical to its source
//! reuses the source transcript's words; the rest come from [`Hearing`], the words heard
//! over each stretch of the rendered voice mix. At a seam between the two:
//! - a reused part keeps an edge word it plays most of, or ≥ 0.1 s of (as it always
//!   has), except at a continuous join into a heard stretch, where the word that
//!   straddles the join is the heard side's;
//! - a heard word is the stretch's when its middle is inside the stretch, when it
//!   straddles the stretch's edge, or when its middle is within [`EDGE_TOL`] of that edge
//!   (the recogniser's timing drifts that much);
//! - a heard word is dropped when a kept reused word says the same thing within
//!   [`EDGE_TOL`], or overlaps it from the reused side: one word is said once.
//!
//! A heard word that matches a source word of the part it plays in (same normalised text,
//! middles within [`EDGE_TOL`]) keeps that word's id, so caption edits, keywords, time
//! overrides and hand-made cue breaks made on the source words still apply.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gmclip_model::anchor::{Anchor, WordId};
use gmclip_model::clock::ModelTime;
use gmclip_model::entities::{
    CaptionCorrections, CaptionDoc, CaptionEdit, CaptionGrouping, CueBreak, KeepPairs, MediaRef,
    Part,
};
use gmclip_model::{Doc, Id};
use gmclip_plan::story_cues;

use super::heard::HeardStretch;
use crate::analysis::AnalysisView;

const SEC: i64 = 705_600_000;
/// How far a heard word's middle may sit outside its stretch and still be its own, and
/// how close two sayings of one word are taken to be the same (0.35 s).
pub const EDGE_TOL: i64 = SEC * 35 / 100;
/// One reference frame (1/30 s) in flicks.
const STORY_FRAME: i64 = SEC / story_cues::FPS;
/// Keyword yellow (the reference's `&H00D4FF&`).
pub const KEYWORD_FILL: &str = "#FFD400FF";

const FILLERS: &[&str] = &[
    "um", "uh", "uhm", "umm", "uhh", "erm", "er", "hmm", "mm", "ah",
];

fn norm(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

/// One caption word on the output clock (flicks).
#[derive(Clone, Debug)]
pub struct CapWord {
    /// The transcript word it is.
    pub id: WordId,
    pub text: String,
    pub start: i64,
    pub end: i64,
    pub keyword: bool,
    /// Index of the part (output order) it plays in.
    pub part: usize,
}

/// One cue: output flicks and its words.
#[derive(Clone, Debug)]
pub struct CapCue {
    pub start: i64,
    pub end: i64,
    pub words: Vec<CapWord>,
    pub part: usize,
}

fn owner_key(m: &MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// A no-caption span on one media's source clock (the reference's `no_caption`, imported as raw
/// source times): it covers a word whose middle it holds, whether or not the cut plays
/// the span's edges (reference: the word's native midpoint in `[lo, hi)`).
#[derive(Clone, Debug)]
pub struct SrcSpan {
    pub media: MediaRef,
    pub from: i64,
    pub to: i64,
}

impl SrcSpan {
    pub fn covers(&self, media: &MediaRef, mid: i64) -> bool {
        *media == self.media && self.from <= mid && mid < self.to
    }
}

/// `from`–`to` as a [`SrcSpan`] when both are raw times on the same media's clock.
pub fn src_span(from: &Anchor, to: &Anchor) -> Option<SrcSpan> {
    let src = |a: &Anchor| match a {
        Anchor::Raw(r) => match &r.time {
            ModelTime::Src {
                source: None,
                group: Some(g),
                t,
            } => Some((MediaRef::Group(*g), t.0)),
            ModelTime::Src {
                source: Some(s),
                group: None,
                t,
            } => Some((MediaRef::Source(*s), t.0)),
            _ => None,
        },
        _ => None,
    };
    let ((m, a), (n, z)) = (src(from)?, src(to)?);
    (m == n && z > a).then_some(SrcSpan {
        media: m,
        from: a,
        to: z,
    })
}

/// The words heard over one stretch of a cut's voice mix, for [`words`].
#[derive(Clone, Debug)]
pub struct HeardUse {
    /// Model parts `[i, j)` (indices into [`words`]'s `parts`) whose captions it gives.
    pub parts: [usize; 2],
    /// Output flicks whose words it gives.
    pub out: [i64; 2],
    /// The output flick of the window's first sample (its words count from there).
    pub window0: i64,
    pub words: Arc<HeardStretch>,
}

/// Where a cut's caption words come from: every model part not in a [`HeardUse`] reuses
/// its media's transcript.
#[derive(Clone, Debug, Default)]
pub struct Hearing {
    pub heard: Vec<HeardUse>,
}

impl Hearing {
    /// Model part `i`'s words are heard.
    pub fn heard_part(&self, i: usize) -> bool {
        self.heard.iter().any(|h| h.parts[0] <= i && i < h.parts[1])
    }
}

/// A source offset `d` (flicks from `src_in`) of part `p` on its output clock: `d / speed`
/// by the one tie rule, or through its ramp (decisions 0030, 0033; the model clock's own
/// conversion).
fn src_to_out(p: &Part, d: i64) -> i64 {
    gmclip_model::clock::src_to_local(p, gmclip_model::Flicks(d)).0
}

/// An output offset `d` of part `p` on its source clock: `d · speed`, or through its ramp
/// (decisions 0030, 0033).
fn out_to_src(p: &Part, d: i64) -> i64 {
    gmclip_model::clock::local_to_src(p, gmclip_model::Flicks(d)).0 - p.src_in.0
}

/// The cut's caption words: `parts` are `(part, output start in flicks)` in output order;
/// `skip(t, media, mid)` says a word starting at output time `t`, whose middle is `mid`
/// on `media`'s clock, is in a no-caption span; `hearing` gives the heard stretches.
pub fn words(
    doc: &Doc,
    view: &AnalysisView,
    cd: &CaptionDoc,
    parts: &[(Id<Part>, i64)],
    skip: &dyn Fn(i64, &MediaRef, i64) -> bool,
    hearing: &Hearing,
) -> Result<Vec<CapWord>, String> {
    let keywords: BTreeSet<&WordId> = cd.keywords.values().collect();
    let mut out = Vec::new();
    let mut any_transcript = false;
    for (pi, (pid, at)) in parts.iter().enumerate() {
        if hearing.heard_part(pi) {
            continue;
        }
        let p = doc.get(*pid).ok_or("a part is gone")?;
        let owner = owner_key(&p.media);
        let Some(t) = view.active(doc, &owner) else {
            continue;
        };
        any_transcript = true;
        let (a, b) = (p.src_in.0, p.src_out.0);
        // A join that continues the source (one take starts where the one before ends):
        // a word straddling it is said once, by the side that plays more of it.
        let same = |q: Option<&(Id<Part>, i64)>| q.and_then(|(q, _)| doc.get(*q));
        let joined_before = pi > 0
            && same(parts.get(pi - 1)).is_some_and(|q| q.media == p.media && q.src_out.0 == a);
        let joined_after =
            same(parts.get(pi + 1)).is_some_and(|q| q.media == p.media && q.src_in.0 == b);
        // a word straddling a continuous join into a heard stretch is the heard side's
        let heard_before = pi > 0 && hearing.heard_part(pi - 1);
        let heard_after = hearing.heard_part(pi + 1);
        for (i, w) in t.words.iter().enumerate() {
            let len = (w.end - w.start).max(1);
            let overlap = w.end.min(b) - w.start.max(a);
            if overlap <= 0 {
                continue;
            }
            let at_join_before = joined_before && w.start < a && a < w.end;
            let at_join_after = joined_after && w.start < b && b < w.end;
            let keep = if at_join_before {
                !heard_before && a - w.start < w.end - a
            } else if at_join_after {
                !heard_after && b - w.start >= w.end - b
            } else {
                // a word the take plays most of, or at least 0.1 s of (a take that starts
                // or ends inside a word still says it: the reference captions what the render says)
                overlap * 2 >= len || overlap >= 70_560_000
            };
            if !keep {
                continue;
            }
            let id = WordId {
                asr_revision: t.revision.clone(),
                ordinal: u32::try_from(i).map_err(|_| "a transcript too long")?,
            };
            let (start, end) = match cd.word_time_overrides.get(&id) {
                Some(o) => (
                    o.start.map_or(w.start, |f| f.0),
                    o.end.map_or(w.end, |f| f.0),
                ),
                None => (w.start, w.end),
            };
            let text = match cd.edits.get(&id) {
                Some(CaptionEdit::Drop) => continue,
                Some(CaptionEdit::Text(t)) => t.clone(),
                None => {
                    if cd.drop_fillers && FILLERS.contains(&norm(&w.text).as_str()) {
                        continue;
                    }
                    w.text.clone()
                }
            };
            if text.trim().is_empty() {
                continue;
            }
            let s = at + src_to_out(p, start.max(a) - a);
            let e = at + src_to_out(p, end.min(b) - a);
            if skip(s, &p.media, start + (end - start) / 2) {
                continue;
            }
            out.push(CapWord {
                keyword: keywords.contains(&id),
                id,
                text: text.trim().to_owned(),
                start: s,
                end: e.max(s + 1),
                part: pi,
            });
        }
    }
    if !any_transcript && hearing.heard.is_empty() {
        return Err(
            "heard captions need a transcript of the cut's media (still transcribing?)".into(),
        );
    }
    heard_words(doc, view, cd, parts, skip, hearing, &keywords, &mut out)?;
    out.sort_by_key(|w| w.start);
    let mut out = match doc.iter::<CaptionCorrections>().next() {
        Some(c) => correct(out, &c.entries),
        None => out,
    };
    star_terms(&mut out, &cd.keyword_terms);
    Ok(out)
}

/// Adds each stretch's heard words to `out` (which holds the reused ones), by the seam
/// rules in the module docs.
#[allow(clippy::too_many_arguments)]
fn heard_words(
    doc: &Doc,
    view: &AnalysisView,
    cd: &CaptionDoc,
    parts: &[(Id<Part>, i64)],
    skip: &dyn Fn(i64, &MediaRef, i64) -> bool,
    hearing: &Hearing,
    keywords: &BTreeSet<&WordId>,
    out: &mut Vec<CapWord>,
) -> Result<(), String> {
    if hearing.heard.is_empty() {
        return Ok(());
    }
    // each model part: output [at, end), its part
    let mut spans: Vec<(i64, i64, &Part)> = Vec::with_capacity(parts.len());
    for (pid, at) in parts {
        let p = doc.get(*pid).ok_or("a part is gone")?;
        spans.push((*at, *at + gmclip_model::clock::part_duration(p).0, p));
    }
    let Some(total) = spans.last().map(|s| s.1) else {
        return Ok(());
    };
    let span_at = |t: i64| {
        let t = t.clamp(0, total - 1);
        spans
            .iter()
            .position(|(a, z, _)| *a <= t && t < *z)
            .unwrap_or(spans.len() - 1)
    };
    let reused: Vec<(i64, i64, String)> = out
        .iter()
        .map(|w| (w.start, w.end, norm(&w.text)))
        .collect();
    for h in &hearing.heard {
        let [o0, o1] = h.out;
        let tag = h.words.sig.get(..16).unwrap_or(&h.words.sig);
        for (k, hw) in h.words.words.iter().enumerate() {
            let s = (h.window0 + hw.start).clamp(0, total);
            let e = (h.window0 + hw.end).clamp(s, total);
            if e <= s {
                continue;
            }
            let mid = s + (e - s) / 2;
            let inside = o0 <= mid && mid < o1;
            let straddles = (s < o0 && o0 < e) || (s < o1 && o1 < e);
            let near = (mid - o0).abs() <= EDGE_TOL || (mid - o1).abs() <= EDGE_TOL;
            if !(inside || straddles || near) {
                continue;
            }
            let n = norm(&hw.text);
            let said = reused.iter().any(|(rs, re, rn)| {
                let overlaps = *rs < e && s < *re;
                let same = !n.is_empty() && *rn == n;
                (overlaps && (same || !inside)) || (same && ((rs + re) / 2 - mid).abs() <= EDGE_TOL)
            });
            if said {
                continue;
            }
            let pi = span_at(mid);
            let (at, _, p) = spans[pi];
            let (a, b) = (p.src_in.0, p.src_out.0);
            let src_mid = a + out_to_src(p, mid - at);
            // the source word this one is, if the part's media has it: same text, near
            let adopted = view.active(doc, &owner_key(&p.media)).and_then(|t| {
                // words start in time order; none starting > 2 s before is near
                let lo = src_mid - EDGE_TOL - 2 * SEC;
                let i0 = t.words.partition_point(|w| w.start < lo);
                t.words
                    .iter()
                    .enumerate()
                    .skip(i0)
                    .take_while(|(_, w)| w.start <= src_mid + EDGE_TOL + SEC)
                    .filter(|(_, w)| norm(&w.text) == n && !n.is_empty())
                    .min_by_key(|(_, w)| (w.start + (w.end - w.start) / 2 - src_mid).abs())
                    .filter(|(_, w)| (w.start + (w.end - w.start) / 2 - src_mid).abs() <= EDGE_TOL)
                    .map(|(i, w)| (t.revision.clone(), i, w.clone()))
            });
            let (id, text, start, end) = match adopted {
                Some((rev, i, w)) => {
                    let id = WordId {
                        asr_revision: rev,
                        ordinal: u32::try_from(i).map_err(|_| "a transcript too long")?,
                    };
                    let text = match cd.edits.get(&id) {
                        Some(CaptionEdit::Drop) => continue,
                        Some(CaptionEdit::Text(t)) => t.clone(),
                        None => hw.text.clone(),
                    };
                    // a time override made on the source word moves this one too
                    let (s, e) = match cd.word_time_overrides.get(&id) {
                        Some(o) => {
                            let ws = o.start.map_or(w.start, |f| f.0);
                            let we = o.end.map_or(w.end, |f| f.0);
                            (
                                at + src_to_out(p, ws.max(a) - a),
                                at + src_to_out(p, we.min(b) - a),
                            )
                        }
                        None => (s, e),
                    };
                    (id, text, s, e)
                }
                None => (
                    WordId {
                        asr_revision: format!("heard:{tag}"),
                        ordinal: u32::try_from(k).map_err(|_| "too many heard words")?,
                    },
                    hw.text.clone(),
                    s,
                    e,
                ),
            };
            let edited = cd.edits.contains_key(&id);
            if !edited && cd.drop_fillers && FILLERS.contains(&norm(&text).as_str()) {
                continue;
            }
            if text.trim().is_empty() || skip(start, &p.media, src_mid) {
                continue;
            }
            out.push(CapWord {
                keyword: keywords.contains(&id),
                id,
                text: text.trim().to_owned(),
                start,
                end: end.max(start + 1),
                part: pi,
            });
        }
    }
    Ok(())
}

/// The reference's markup (`{Y}`, `{W}`, …) left out of shown text.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = false;
    for c in s.chars() {
        match c {
            '{' => depth = true,
            '}' if depth => depth = false,
            _ if !depth => out.push(c),
            _ => {}
        }
    }
    out
}

/// The sentence mark a word ends with, kept when its re-spelling does not end with it
/// (the reference's `fix` / `cap_sub` tail).
fn tail(heard: &str, shown: &str) -> String {
    match heard.chars().last() {
        Some(c) if ".,?!".contains(c) && !shown.ends_with(c) => c.to_string(),
        _ => String::new(),
    }
}

/// The project's corrections (the reference's `fix` and `cap_sub`, imported as
/// `CaptionCorrections`): a one-word entry re-spells a word (empty drops it), a longer one
/// replaces the run of words it matches with one word spanning them. Matching is on
/// [`story_cues::norm`] tokens, as the reference's `caption_cues` does.
pub fn correct(words: Vec<CapWord>, entries: &BTreeMap<String, String>) -> Vec<CapWord> {
    let toks = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(story_cues::norm)
            .filter(|t| !t.is_empty())
            .collect()
    };
    let mut fix: BTreeMap<String, String> = BTreeMap::new();
    let mut subs: Vec<(Vec<String>, String)> = Vec::new();
    for (k, v) in entries {
        let t = toks(k);
        match t.len() {
            0 => {}
            1 => {
                fix.insert(t[0].clone(), plain(v));
            }
            _ => subs.push((t, plain(v))),
        }
    }
    let mut ws: Vec<CapWord> = Vec::with_capacity(words.len());
    for mut w in words {
        if let Some(f) = fix.get(&story_cues::norm(&w.text)) {
            w.text = format!("{f}{}", tail(&w.text, f));
        }
        if !w.text.trim().is_empty() {
            ws.push(w);
        }
    }
    if subs.is_empty() {
        return ws;
    }
    let mut out = Vec::with_capacity(ws.len());
    let mut i = 0;
    while i < ws.len() {
        let hit = subs.iter().find(|(old, _)| {
            ws.len() - i >= old.len()
                && ws[i..i + old.len()]
                    .iter()
                    .zip(old)
                    .all(|(w, o)| story_cues::norm(&w.text) == *o)
        });
        match hit {
            Some((old, new)) => {
                let last = &ws[i + old.len() - 1];
                let mut m = ws[i].clone();
                m.text = format!("{new}{}", tail(&last.text, new));
                m.end = last.end;
                m.keyword |= ws[i..i + old.len()].iter().any(|w| w.keyword);
                out.push(m);
                i += old.len();
            }
            None => {
                out.push(ws[i].clone());
                i += 1;
            }
        }
    }
    out
}

/// Stars every word in a run matching a keyword term (the reference's `norm` on each token: case
/// and punctuation ignored); a term of several tokens must match consecutive tokens.
pub fn star_terms(words: &mut [CapWord], terms: &[String]) {
    let terms: Vec<Vec<String>> = terms
        .iter()
        .map(|t| {
            t.split_whitespace()
                .map(story_cues::norm)
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return;
    }
    // tokens of the shown text, each with the word it is in
    let toks: Vec<(String, usize)> = words
        .iter()
        .enumerate()
        .flat_map(|(i, w)| {
            w.text
                .split_whitespace()
                .map(story_cues::norm)
                .filter(|x| !x.is_empty())
                .map(move |t| (t, i))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut hit = vec![false; words.len()];
    for t in &terms {
        for s in 0..toks.len() {
            if toks.len() - s >= t.len()
                && toks[s..s + t.len()].iter().zip(t).all(|(a, b)| a.0 == *b)
            {
                for k in &toks[s..s + t.len()] {
                    hit[k.1] = true;
                }
            }
        }
    }
    for (w, h) in words.iter_mut().zip(hit) {
        w.keyword |= h;
    }
}

/// The keep pairs for a project: the reference's base pairs plus the project's `KeepPairs` (an entry
/// of more than two words keeps each neighbouring pair).
pub fn keep_pairs(doc: &Doc) -> BTreeSet<(String, String)> {
    let mut extra = Vec::new();
    for kp in doc.iter::<KeepPairs>() {
        for words in kp.entries.values() {
            for w in words.windows(2) {
                extra.push((w[0].clone(), w[1].clone()));
            }
        }
    }
    story_cues::keep_set(&extra)
}

/// Groups words into cues (the reference's grouping, see the module doc). `total` is the cut's
/// length in flicks; `breaks` are the doc's hand-made boundaries.
pub fn group(
    words: &[CapWord],
    grouping: CaptionGrouping,
    breaks: &BTreeMap<WordId, CueBreak>,
    keep: &BTreeSet<(String, String)>,
    total: i64,
) -> Vec<CapCue> {
    // A joined word rides in its neighbour's unit, so the search never splits them.
    let mut units: Vec<(story_cues::Word, std::ops::Range<usize>)> = Vec::new();
    let mut split: BTreeSet<usize> = BTreeSet::new();
    for (i, w) in words.iter().enumerate() {
        let (a, z) = (w.start as f64 / SEC as f64, w.end as f64 / SEC as f64);
        match (breaks.get(&w.id), units.last_mut()) {
            (Some(CueBreak::Join), Some((u, r))) => {
                u.w = format!("{} {}", u.w, w.text);
                u.z = u.z.max(z);
                r.end = i + 1;
            }
            (b, _) => {
                if b == Some(&CueBreak::Split) {
                    split.insert(units.len());
                }
                units.push((
                    story_cues::Word {
                        w: w.text.clone(),
                        a,
                        z,
                    },
                    i..i + 1,
                ));
            }
        }
    }
    let ws: Vec<story_cues::Word> = units.iter().map(|(u, _)| u.clone()).collect();
    let frames = (total + STORY_FRAME / 2) / STORY_FRAME;
    let groups = story_cues::group_words(
        &ws,
        keep,
        &|i| split.contains(&i),
        grouping == CaptionGrouping::SingleWord,
        frames,
    );
    let mut cues: Vec<CapCue> = Vec::with_capacity(groups.len());
    for g in groups {
        let (Some(first), Some(last)) = (units.get(g.words.start), units.get(g.words.end - 1))
        else {
            continue;
        };
        let mut ws: Vec<CapWord> = words[first.1.start..last.1.end].to_vec();
        // the reference capitalises a cue that opens a sentence.
        let opens = cues
            .last()
            .and_then(|c: &CapCue| c.words.last())
            .is_none_or(|w| story_cues::ends_with_any(&w.text, ".?!"));
        if opens && let Some(w0) = ws.first_mut() {
            w0.text = story_cues::capitalise(&w0.text);
        }
        cues.push(CapCue {
            start: g.t0 * STORY_FRAME,
            end: (g.t1 * STORY_FRAME).min(total),
            part: ws[0].part,
            words: ws,
        });
    }
    cues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: f64, e: f64) -> CapWord {
        CapWord {
            id: WordId {
                asr_revision: "t".into(),
                ordinal: (s * 100.0) as u32,
            },
            text: text.into(),
            start: (s * SEC as f64) as i64,
            end: (e * SEC as f64) as i64,
            keyword: false,
            part: 0,
        }
    }

    fn texts(c: &[CapCue]) -> Vec<String> {
        c.iter()
            .map(|c| {
                c.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn groups_like_rt() {
        let ws = [
            w("We", 0.0, 0.2),
            w("generated", 0.2, 0.6),
            w("that", 0.6, 0.8),
            w("in", 0.8, 0.9),
            w("two", 0.9, 1.1),
            w("minutes.", 1.1, 1.5),
            w("Hi,", 2.5, 2.7),
            w("I'm", 2.7, 2.9),
            w("a", 2.9, 3.0),
            w("banana", 3.0, 3.4),
        ];
        let c = group(
            &ws,
            CaptionGrouping::StoryV5,
            &BTreeMap::new(),
            &story_cues::keep_set(&[]),
            4 * SEC,
        );
        assert_eq!(
            texts(&c),
            [
                "We generated that",
                "in two minutes.",
                "Hi,",
                "I'm a banana"
            ]
        );
        // Cues hold to the next when close, never overlap.
        assert!(c.windows(2).all(|p| p[0].end <= p[1].start));
        let single = group(
            &ws,
            CaptionGrouping::SingleWord,
            &BTreeMap::new(),
            &story_cues::keep_set(&[]),
            4 * SEC,
        );
        // the reference drops a cue squeezed under 4 frames ("in", "a": 0.1 s before the next word).
        assert!(single.iter().all(|c| c.words.len() == 1));
        assert_eq!(single.len(), ws.len() - 2);
    }

    fn by_ordinal(ws: &[CapWord], text: &str) -> WordId {
        ws.iter()
            .find(|w| w.text == text)
            .map(|w| w.id.clone())
            .unwrap()
    }

    #[test]
    fn project_corrections_respell_like_rt() {
        let ws = vec![
            w("um", 0.0, 0.1),
            w("the", 0.1, 0.2),
            w("tech", 0.2, 0.4),
            w("friend", 0.4, 0.6),
            w("butts.", 0.6, 0.9),
            w("Tech", 1.0, 1.2),
            w("Friend", 1.2, 1.4),
        ];
        let entries: BTreeMap<String, String> = [
            ("um", ""),
            ("butts", "bots"),
            ("tech friend", "{Y}TechFren{W}"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
        let out = correct(ws, &entries);
        let t: Vec<(&str, i64, i64)> = out
            .iter()
            .map(|w| (w.text.as_str(), w.start, w.end))
            .collect();
        let s = |x: f64| (x * SEC as f64) as i64;
        assert_eq!(
            t,
            [
                ("the", s(0.1), s(0.2)),
                ("TechFren", s(0.2), s(0.6)),
                ("bots.", s(0.6), s(0.9)),
                ("TechFren", s(1.0), s(1.4)),
            ]
        );
    }

    #[test]
    fn keyword_terms_star_every_match() {
        let mut ws = vec![
            w("in", 0.0, 0.2),
            w("two", 0.2, 0.4),
            w("minutes.", 0.4, 0.8),
            w("Two", 1.0, 1.2),
            w("cats", 1.2, 1.4),
            w("MiniMax H3", 1.4, 1.8),
        ];
        star_terms(&mut ws, &["two minutes".into(), "h3".into(), " ".into()]);
        let starred: Vec<&str> = ws
            .iter()
            .filter(|w| w.keyword)
            .map(|w| w.text.as_str())
            .collect();
        assert_eq!(starred, ["two", "minutes.", "MiniMax H3"]);
    }

    #[test]
    fn hand_breaks_override_the_search() {
        let ws = [
            w("We", 0.0, 0.2),
            w("generated", 0.2, 0.6),
            w("that", 0.6, 0.8),
            w("in", 0.8, 0.9),
            w("two", 0.9, 1.1),
            w("minutes.", 1.1, 1.5),
        ];
        let keep = story_cues::keep_set(&[]);
        let mut br = BTreeMap::new();
        br.insert(by_ordinal(&ws, "generated"), CueBreak::Split);
        br.insert(by_ordinal(&ws, "that"), CueBreak::Join);
        let c = group(&ws, CaptionGrouping::StoryV5, &br, &keep, 4 * SEC);
        assert_eq!(texts(&c), ["We", "generated that", "in two minutes."]);
        // A join may make a cue longer than the search would (the hand wins).
        let mut br = BTreeMap::new();
        for t in ["generated", "that", "in", "two", "minutes."] {
            br.insert(by_ordinal(&ws, t), CueBreak::Join);
        }
        let c = group(&ws, CaptionGrouping::StoryV5, &br, &keep, 4 * SEC);
        assert_eq!(texts(&c), ["We generated that in two minutes."]);
    }

    #[test]
    fn keeps_project_pairs_and_numbers() {
        let ws = [
            w("on", 0.0, 0.2),
            w("an", 0.2, 0.3),
            w("NVIDIA", 0.3, 0.7),
            w("5080", 0.7, 1.2),
            w("GPU", 1.2, 1.6),
            w("today.", 1.6, 2.0),
        ];
        let mut doc = Doc::new();
        doc.insert_for_test(KeepPairs {
            id: Id::new(gmclip_model::Ulid(9)),
            entries: [(
                "nvidia gpu".to_owned(),
                vec!["NVIDIA".to_owned(), "5080".to_owned(), "GPU".to_owned()],
            )]
            .into_iter()
            .collect(),
        });
        let c = group(
            &ws,
            CaptionGrouping::StoryV5,
            &BTreeMap::new(),
            &keep_pairs(&doc),
            3 * SEC,
        );
        assert!(
            texts(&c).iter().any(|t| t.contains("NVIDIA 5080 GPU")),
            "{:?}",
            texts(&c)
        );
    }
}
