//! Strike and restore words (SPEC §5.4) on the source ranges their fingerprints give.
//! Until gmclip ASR lands (A16), the words come from the caller as `WordRef`s; the edit
//! on the parts is real.

use gmclip_macros::command;
use gmclip_model::anchor::WordRef;
use gmclip_model::edit;
use gmclip_model::entities::{Cut, MediaRef, Part};
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::part::MIN_PART;
use super::record_part_children;
use crate::command::{Command, Cx, model_err};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WordsArgs {
    pub cut: Id<Cut>,
    /// The span's words (their fingerprints): every one in order, or just its first and
    /// last; the range is first start → last end. A longer list with a hole is refused
    /// (strike separate spans with separate ops).
    pub words: Vec<WordRef>,
    /// The video they are from (a source or sync group). Needed only when the cut plays
    /// several videos and the words' transcript is not known here.
    #[serde(default)]
    pub media: Option<MediaRef>,
}

/// The video the words are from: the one named, else the one whose transcript they are
/// in, else (a cut of one video) any.
fn words_media(cx: &mut Cx<'_>, a: &WordsArgs) -> Result<Option<MediaRef>, CmdError> {
    if a.media.is_some() {
        return Ok(a.media);
    }
    let doc = cx.tx.peek();
    if let Some(m) = a
        .words
        .first()
        .and_then(|w| cx.analysis.word(w))
        .and_then(|(o, _)| super::topic::media_of_owner(doc, o))
    {
        return Ok(Some(m));
    }
    let mut seen: Vec<MediaRef> = Vec::new();
    for p in doc.children::<Part>(a.cut.entity()) {
        if let Some(p) = doc.get(p)
            && !seen.contains(&p.media)
        {
            seen.push(p.media);
        }
    }
    if seen.len() > 1 {
        return Err(CmdError::invalid(
            "/media",
            "this cut plays several videos: name the one the words are from",
        ));
    }
    Ok(None)
}

fn span(words: &[WordRef]) -> Result<(Flicks, Flicks), CmdError> {
    let (Some(f), Some(l)) = (words.first(), words.last()) else {
        return Err(CmdError::invalid("/words", "at least one word"));
    };
    if words.len() > 10_000 {
        return Err(CmdError::invalid("/words", "at most 10 000 words"));
    }
    if l.t_src_end <= f.t_src_start {
        return Err(CmdError::invalid("/words", "words in order"));
    }
    for w in words {
        super::bounded(&w.word_id.asr_revision, "/words/word_id", 128)?;
        super::bounded(&w.normalised_text, "/words/normalised_text", 200)?;
    }
    // the range runs from the first word to the last: a longer list with a hole would
    // take the words in the hole too (decision 0040: an agent listed the words it meant
    // to drop and lost the ones between), so it must name every word of the span; two
    // words are the span's ends
    for (i, p) in words.windows(2).enumerate().filter(|_| words.len() > 2) {
        if p[1].word_id.asr_revision != p[0].word_id.asr_revision
            || p[1].word_id.ordinal != p[0].word_id.ordinal + 1
        {
            return Err(CmdError::invalid(
                &format!("/words/{}", i + 1),
                "words must be every word of one span, consecutive ids of one transcript in order (or just its first and last word): strike separate spans with separate ops",
            ));
        }
    }
    Ok((f.t_src_start, l.t_src_end))
}

/// The span's cut points on the voice (decision 0049): from 40 ms before the first word
/// is heard (a soft onset within 250 ms kept) to 120 ms after the last word is, never into
/// a neighbour's voice. The words' ASR edges stay their fingerprint; an end whose word is
/// unmeasured, or whose time is not the ASR's (an edge the human tuned), is kept.
fn voice_span(cx: &Cx<'_>, words: &[WordRef], s: Flicks, e: Flicks) -> (Flicks, Flicks) {
    let (Some(f), Some(l)) = (words.first(), words.last()) else {
        return (s, e);
    };
    let asr = |w: &WordRef| {
        cx.analysis
            .word(w)
            .map(|(o, x)| (o.to_owned(), w.word_id.ordinal as usize, x.start, x.end))
    };
    let (Some((fo, fi, fs, _)), Some((lo, li, _, le))) = (asr(f), asr(l)) else {
        return (s, e);
    };
    if fo != lo || li < fi {
        return (s, e);
    }
    let (vs, ve) = cx.analysis.voice_span(&fo, &f.word_id.asr_revision, fi, li);
    let s2 = vs.filter(|_| f.t_src_start.0 == fs).unwrap_or(s.0);
    let e2 = ve.filter(|_| l.t_src_end.0 == le).unwrap_or(e.0);
    if e2 <= s2 {
        return (s, e);
    }
    (Flicks(s2), Flicks(e2))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Struck {
    /// Parts trimmed, split or removed.
    pub parts_changed: usize,
    pub parts_removed: usize,
    /// Pieces left under 0.12 s beside the struck words (no part may be shorter),
    /// dropped with them.
    pub fragments_dropped: usize,
}

#[derive(Debug)]
pub struct WordsStrike;

#[command(
    name = "words_strike",
    scope = "edit",
    undo = "undoable",
    doc = "Strike words from a cut: cut where they are heard (40 ms before the first word's voice to 120 ms after the last's; the ASR's edges where the voice is not analysed) out of every part that plays it, and report any piece under 0.12 s dropped with them. Name every word of one span in order, or just its first and last; separate spans are separate ops.",
    example = r#"{"cut": "01J00000000000000000000001", "words": [{"word_id": {"asr_revision": "r1", "ordinal": 5}, "t_src_start": 7056000000, "t_src_end": 7408800000, "normalised_text": "um"}]}"#
)]
impl Command for WordsStrike {
    type Args = WordsArgs;
    type Output = Struck;
    fn run(a: WordsArgs, cx: &mut Cx<'_>) -> Result<Struck, CmdError> {
        let (s, e) = span(&a.words)?;
        cx.tx.get(a.cut)?;
        let media = words_media(cx, &a)?;
        let (s, e) = voice_span(cx, &a.words, s, e);
        let (s, e) = widen_to_quiet_edges(cx, a.cut, media, s, e);
        let (s, e) = snap_words(cx, a.cut, s, e);
        let (changed, removed, dropped) = remove_span(cx, a.cut, s, e, media)?;
        if changed == 0 {
            return Err(CmdError::invalid(
                "/words",
                "no part of this cut plays them",
            ));
        }
        Ok(Struck {
            parts_changed: changed,
            parts_removed: removed,
            fragments_dropped: dropped,
        })
    }
}

/// A strike that reaches a part's first (last) word takes the part's wordless head
/// (tail) with it, so no sliver of silence is left as a part of its own (decision
/// 0040; the window's strikes did this already). Only where the media's active
/// transcript says nothing between the strike and the part's edge.
fn widen_to_quiet_edges(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    media: Option<MediaRef>,
    s: Flicks,
    e: Flicks,
) -> (Flicks, Flicks) {
    let doc = cx.tx.peek();
    let parts: Vec<(MediaRef, i64, i64)> = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .filter(|p| media.is_none_or(|m| m == p.media))
        .map(|p| (p.media, p.src_in.0, p.src_out.0))
        .collect();
    let (mut s2, mut e2) = (s.0, e.0);
    for (m, pi, po) in parts {
        if po <= s.0 || pi >= e.0 {
            continue;
        }
        let owner = match m {
            MediaRef::Source(x) => x.0.to_string(),
            MediaRef::Group(x) => x.0.to_string(),
        };
        let Some(t) = cx.analysis.active(doc, &owner) else {
            continue;
        };
        let quiet = |a: i64, b: i64| {
            !t.words
                .iter()
                .any(|w| crate::script::plays_in(w.start, w.end, &[(a, b)]).is_some())
        };
        if pi < s.0 && quiet(pi, s.0) {
            s2 = s2.min(pi);
        }
        if po > e.0 && quiet(e.0, po) {
            e2 = e2.max(po);
        }
    }
    (Flicks(s2), Flicks(e2))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Restored {
    pub part: String,
}

#[derive(Debug)]
pub struct WordsRestore;

#[command(
    name = "words_restore",
    scope = "edit",
    undo = "undoable",
    doc = "Restore struck words: grow the part that ends (or starts) next to them over where they are heard (as words_strike cuts them), ≤ 0.6 s per word.",
    example = r#"{"cut": "01J00000000000000000000001", "words": [{"word_id": {"asr_revision": "r1", "ordinal": 5}, "t_src_start": 7056000000, "t_src_end": 7408800000, "normalised_text": "um"}]}"#
)]
impl Command for WordsRestore {
    type Args = WordsArgs;
    type Output = Restored;
    fn run(a: WordsArgs, cx: &mut Cx<'_>) -> Result<Restored, CmdError> {
        let (s, e) = span(&a.words)?;
        cx.tx.get(a.cut)?;
        let (s, e) = voice_span(cx, &a.words, s, e);
        let (s, e) = snap_words(cx, a.cut, s, e);
        let media = words_media(cx, &a)?;
        let parts = cx.tx.children::<Part>(a.cut.entity());
        let reach = Flicks(
            705_600_000 * 6 / 10 * i64::try_from(a.words.len()).unwrap_or(i64::MAX / 1_000_000_000),
        );
        let mut grow: Option<(Id<Part>, Flicks, Flicks)> = None;
        for id in &parts {
            let p = cx.tx.get(*id)?.clone();
            if media.is_some_and(|m| m != p.media) {
                continue;
            }
            if p.src_in < e && s < p.src_out && p.src_in <= s && e <= p.src_out {
                return Err(CmdError::invalid("/words", "already in the cut"));
            }
            // a part ending at or just before the words grows forward
            if p.src_in < s && p.src_out <= e && s.0 - p.src_out.0 <= reach.0 {
                grow = Some((*id, p.src_in, e));
                break;
            }
            // a part starting at or just after them grows back
            if p.src_in >= s && p.src_in.0 <= e.0 + reach.0 && p.src_out > e {
                grow = Some((*id, s, p.src_out));
                break;
            }
        }
        let (id, i, o) = grow.ok_or_else(|| CmdError::invalid("/words", "no part next to them"))?;
        edit::trim_part(cx.tx.staging(), id, i, o).map_err(model_err)?;
        Ok(Restored {
            part: id.0.to_string(),
        })
    }
}

/// `[s, e)` on the cut's frame grid (nearest frame, ties to the earlier, SPEC §4.1.1).
fn snap_span(cx: &Cx<'_>, cut: Id<Cut>, s: Flicks, e: Flicks) -> (Flicks, Flicks) {
    let fpf = super::longform::stream_fpf(cx.tx.peek(), cut);
    (
        Flicks(super::longform::nearest_frame(s.0, fpf)),
        Flicks(super::longform::nearest_frame(e.0, fpf)),
    )
}

/// [`snap_span`] for a span of words: one shorter than a frame keeps one frame (a word is
/// never struck to nothing).
fn snap_words(cx: &Cx<'_>, cut: Id<Cut>, s: Flicks, e: Flicks) -> (Flicks, Flicks) {
    let fpf = super::longform::stream_fpf(cx.tx.peek(), cut);
    let (a, b) = snap_span(cx, cut, s, e);
    (
        a,
        if e > s && b <= a {
            Flicks(a.0 + fpf)
        } else {
            b
        },
    )
}

/// Removes the source span `[s, e)` from every part of `cut` that plays it (of `media`
/// only, when given): a part is trimmed, split around the span, or deleted; a head or
/// tail shorter than 0.12 s is dropped. Returns `(parts changed, parts removed,
/// fragments dropped)`.
pub(crate) fn remove_span(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    s: Flicks,
    e: Flicks,
    media: Option<gmclip_model::entities::MediaRef>,
) -> Result<(usize, usize, usize), CmdError> {
    let (s, e) = snap_span(cx, cut, s, e);
    if e <= s {
        return Ok((0, 0, 0));
    }
    let parts = cx.tx.children::<Part>(cut.entity());
    let (mut changed, mut removed, mut dropped) = (0, 0, 0);
    for id in parts {
        let p = cx.tx.get(id)?.clone();
        if p.src_out <= s || p.src_in >= e || media.is_some_and(|m| m != p.media) {
            continue;
        }
        record_part_children(cx, id);
        let st = cx.tx.staging();
        let head = s.0 - p.src_in.0;
        let tail = p.src_out.0 - e.0;
        let keep_head = head >= MIN_PART.0;
        let keep_tail = tail >= MIN_PART.0;
        dropped += usize::from(head > 0 && !keep_head) + usize::from(tail > 0 && !keep_tail);
        match (keep_head, keep_tail) {
            (false, false) => {
                edit::delete_part(st, id).map_err(model_err)?;
                removed += 1;
            }
            (true, false) => edit::trim_part(st, id, p.src_in, s).map_err(model_err)?,
            (false, true) => edit::trim_part(st, id, e, p.src_out).map_err(model_err)?,
            (true, true) => {
                // split at the span's end, then end the head at its start
                edit::split_part(st, id, e).map_err(model_err)?;
                edit::trim_part(st, id, p.src_in, s).map_err(model_err)?;
            }
        }
        changed += 1;
    }
    Ok((changed, removed, dropped))
}

// ---------------------------------------------------------------------------------------
// Find & replace (SPEC §5.4, R-WORD-1; backlog A2.04), server-side
// ---------------------------------------------------------------------------------------

/// A word's bare form for find: lowercase letters, digits and apostrophes.
pub fn find_norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’')
        .flat_map(char::to_lowercase)
        .map(|c| if c == '’' { '\'' } else { c })
        .collect()
}

/// One query token: a whole word, or a prefix (`gen*`).
#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    Prefix(String),
}

/// Parses a find query: whole words in their bare form, a phrase when several, `*` at
/// the end of a word as a prefix wildcard (`gen*` finds "generate", "generated").
fn parse_query(q: &str) -> Result<Vec<Tok>, CmdError> {
    super::bounded(q, "/query", 200)?;
    let toks: Vec<Tok> = q
        .split_whitespace()
        .filter_map(|raw| {
            let prefix = raw
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '*')
                .ends_with('*');
            let n = find_norm(raw);
            match (n.is_empty(), prefix) {
                (true, _) => None,
                (false, true) => Some(Tok::Prefix(n)),
                (false, false) => Some(Tok::Word(n)),
            }
        })
        .collect();
    if toks.is_empty() || toks.len() > 12 {
        return Err(CmdError::invalid("/query", "1–12 words"));
    }
    Ok(toks)
}

/// Every match of `toks` in `words`: `[first, last]` word ordinals, in order, not
/// overlapping.
fn find_in(words: &[crate::analysis::Word], toks: &[Tok]) -> Vec<(usize, usize)> {
    let norms: Vec<String> = words.iter().map(|w| find_norm(&w.text)).collect();
    let ok = |w: &str, t: &Tok| match t {
        Tok::Word(x) => w == x,
        Tok::Prefix(x) => w.starts_with(x.as_str()),
    };
    let n = toks.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= norms.len() {
        if (0..n).all(|j| ok(&norms[i + j], &toks[j])) {
            out.push((i, i + n - 1));
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

/// The transcript find/replace reads: `revision` of `owner`, else its active one.
fn transcript_for<'a>(
    cx: &'a Cx<'_>,
    owner: &str,
    revision: Option<&str>,
) -> Result<&'a std::sync::Arc<crate::analysis::Transcript>, CmdError> {
    super::bounded(owner, "/owner", 64)?;
    match revision {
        Some(r) => {
            super::bounded(r, "/revision", 300)?;
            cx.analysis.transcript(owner, r)
        }
        None => cx.analysis.active(cx.tx.peek(), owner),
    }
    .ok_or_else(|| {
        CmdError::invalid(
            "/owner",
            "no transcript for this media (source_analyze transcribes it)",
        )
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WordsFindArgs {
    /// The source or sync group whose transcript is searched.
    pub owner: String,
    /// Whole words in their bare form; several are a phrase; `gen*` is a prefix.
    pub query: String,
    /// A transcript revision other than the active one.
    #[serde(default)]
    pub revision: Option<String>,
    /// At most this many matches listed (default 500, ≤ 10 000); `total` counts all.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WordsMatch {
    /// The first and last word ordinals of the match.
    pub first: u32,
    pub last: u32,
    /// Source seconds.
    pub start_s: f64,
    pub end_s: f64,
    /// The words as said (untrusted).
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WordsFound {
    pub revision: String,
    pub total: usize,
    pub matches: Vec<WordsMatch>,
}

#[derive(Debug)]
pub struct WordsFind;

#[command(
    name = "words_find",
    scope = "read",
    undo = "read_only",
    since = "M2",
    doc = "Find words in a source's transcript: whole words in their bare form, a phrase when several, `*` at a word's end as a prefix (\"gen*\"); returns each match's word ordinals and times.",
    example = r#"{"owner": "01J00000000000000000000009", "query": "comfy ui"}"#,
    example = r#"{"owner": "01J00000000000000000000009", "query": "gen*", "limit": 50}"#
)]
impl Command for WordsFind {
    type Args = WordsFindArgs;
    type Output = WordsFound;
    fn run(a: WordsFindArgs, cx: &mut Cx<'_>) -> Result<WordsFound, CmdError> {
        let toks = parse_query(&a.query)?;
        let limit = a.limit.unwrap_or(500).min(10_000);
        let t = transcript_for(cx, &a.owner, a.revision.as_deref())?;
        let hits = find_in(&t.words, &toks);
        let secs = |f: i64| (f as f64 / 705_600_000.0 * 1000.0).round() / 1000.0;
        let matches = hits
            .iter()
            .take(limit)
            .map(|&(i, j)| WordsMatch {
                first: i as u32,
                last: j as u32,
                start_s: secs(t.words[i].start),
                end_s: secs(t.words[j].end),
                text: t.words[i..=j]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect();
        Ok(WordsFound {
            revision: t.revision.clone(),
            total: hits.len(),
            matches,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WordsReplaceArgs {
    pub cut: Id<Cut>,
    /// The source or sync group whose transcript is searched.
    pub owner: String,
    /// As for `words_find`.
    pub query: String,
    /// What the captions show instead (audio untouched); empty clears earlier
    /// re-spellings of the matches.
    pub to: String,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WordsReplaced {
    pub revision: String,
    pub matches: usize,
    /// Caption words changed (a phrase re-spelled shows on its first word; the others
    /// drop).
    pub words: usize,
}

#[derive(Debug)]
pub struct WordsReplace;

#[command(
    name = "words_replace",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Find and replace in the captions: every match of a words_find query re-spelled (audio untouched), as one undoable transaction; an empty replacement clears earlier re-spellings.",
    example = r#"{"cut": "01J00000000000000000000001", "owner": "01J00000000000000000000009", "query": "xd dev", "to": "exe.dev"}"#
)]
impl Command for WordsReplace {
    type Args = WordsReplaceArgs;
    type Output = WordsReplaced;
    fn run(a: WordsReplaceArgs, cx: &mut Cx<'_>) -> Result<WordsReplaced, CmdError> {
        use gmclip_model::anchor::WordId;
        use gmclip_model::entities::{CaptionDoc, CaptionEdit};
        let to = a.to.trim().to_owned();
        if to.chars().count() > 200 {
            return Err(CmdError::invalid("/to", "at most 200 characters"));
        }
        let toks = parse_query(&a.query)?;
        let t = std::sync::Arc::clone(transcript_for(cx, &a.owner, a.revision.as_deref())?);
        let hits = find_in(&t.words, &toks);
        if hits.len() > 10_000 {
            return Err(CmdError::invalid("/query", "more than 10 000 matches"));
        }
        cx.tx.get(a.cut)?;
        let cd = cx
            .tx
            .children::<CaptionDoc>(a.cut.entity())
            .first()
            .copied()
            .ok_or(CmdError::NotFound)?;
        let mut words = 0;
        let id = |o: usize| WordId {
            asr_revision: t.revision.clone(),
            ordinal: o as u32,
        };
        cx.tx.update(cd, |c| {
            for &(i, j) in &hits {
                for o in i..=j {
                    words += 1;
                    if to.is_empty() {
                        c.edits.remove(&id(o));
                    } else if o == i {
                        c.edits.insert(id(o), CaptionEdit::Text(to.clone()));
                    } else {
                        c.edits.insert(id(o), CaptionEdit::Drop);
                    }
                }
            }
        })?;
        Ok(WordsReplaced {
            revision: t.revision.clone(),
            matches: hits.len(),
            words,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(s: &str) -> Vec<crate::analysis::Word> {
        s.split_whitespace()
            .enumerate()
            .map(|(i, t)| crate::analysis::Word {
                text: t.into(),
                start: i as i64 * 100,
                end: i as i64 * 100 + 90,
                confidence: None,
            })
            .collect()
    }

    #[test]
    fn whole_words_phrases_and_prefixes() {
        let w = ws("We generated it. Generation is the GENERAL thing, the general idea; general's");
        let q = |s: &str| find_in(&w, &parse_query(s).unwrap());
        assert_eq!(q("general"), vec![(6, 6), (9, 9)], "whole words only");
        assert_eq!(q("the general"), vec![(5, 6), (8, 9)]);
        assert_eq!(q("gen*"), vec![(1, 1), (3, 3), (6, 6), (9, 9), (11, 11)]);
        assert_eq!(q("general's"), vec![(11, 11)]);
        assert_eq!(q("THE"), vec![(5, 5), (8, 8)]);
        assert!(q("nothing").is_empty());
        assert!(parse_query("  * ").is_err());
        assert!(parse_query("").is_err());
        // overlapping matches count once each, left to right
        let w = ws("ha ha ha ha");
        assert_eq!(
            find_in(&w, &parse_query("ha ha").unwrap()),
            vec![(0, 1), (2, 3)]
        );
    }
}
