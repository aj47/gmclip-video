//! The words strip's data (SPEC §5.4, backlog U.06): a cut's words in output order from
//! the stream's transcript, with what the human changed (struck, re-spelt, dropped,
//! keyword), and the ordinal-based edits the strip sends.
//!
//! - A word belongs to a part when ≥ ½ of it lies inside the part's source range; words
//!   in a gap ≤ 15 s between two consecutive parts of the same media are listed as
//!   **struck** (restorable).
//! - The UI addresses words by ordinal in the transcript revision the reply names; this
//!   module turns ordinals into the core's `WordRef` fingerprints (`words_strike`,
//!   `words_restore`, `word_edge_nudge`), so no time arithmetic happens in the UI.

use gmclip_model::anchor::{WordId, WordRef};
use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{CaptionDoc, CaptionEdit, Cut, MediaRef, Part};
use gmclip_model::{Doc, Flicks, Id};
use serde_json::{Value, json};

use crate::core::{AppCore, OpenProject};
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};
use crate::review::{Transcript, Transcripts};

/// Struck words further than this from a join are not listed.
const GAP: i64 = 15 * FLICKS;
/// At a jump (the next part plays another place), struck words this close to a part
/// edge are listed (a tail or head the human struck), restorable.
const EDGE: i64 = 3 * FLICKS;

pub(crate) fn owner(m: &MediaRef) -> String {
    match m {
        MediaRef::Group(g) => g.0.to_string(),
        MediaRef::Source(s) => s.0.to_string(),
    }
}

fn norm(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The transcript the cut's parts read: `(owner, revision, transcript, pending)`. The
/// project's active revision for the owner wins, else the newest stored; while a source
/// is still being transcribed (nothing stored yet), the words heard so far, under the
/// revision they will be stored as (`pending`).
pub fn transcript_of(
    core: &AppCore,
    p: &OpenProject,
    doc: &Doc,
    cut: Id<Cut>,
) -> Option<(String, String, std::sync::Arc<Transcript>, bool)> {
    transcript_for(core, p, doc, cut, None)
}

/// [`transcript_of`] for one video of the project (`owner`: a source or sync group id;
/// the transcript pane's source picker), else the cut's first part's.
pub fn transcript_for(
    core: &AppCore,
    p: &OpenProject,
    doc: &Doc,
    cut: Id<Cut>,
    owner_id: Option<&str>,
) -> Option<(String, String, std::sync::Arc<Transcript>, bool)> {
    let o = match owner_id {
        Some(o) => gmclip_core::commands::topic::media_of_owner(doc, o).map(|m| owner(&m))?,
        None => {
            let first = doc
                .children::<Part>(cut.entity())
                .find_map(|id| doc.get(id))?;
            owner(&first.media)
        }
    };
    let ts: &Transcripts = &core.transcripts;
    let active = doc
        .iter::<gmclip_model::entities::Project>()
        .next()
        .and_then(|pr| pr.active_asr.get(&o).cloned());
    let stored = p
        .handle
        .analysis_keys()
        .into_iter()
        .filter(|(ow, k, _)| ow == &o && k == "words")
        .map(|(_, _, r)| r)
        .collect::<Vec<_>>();
    let rev = active
        .filter(|a| stored.contains(a))
        .or_else(|| stored.iter().max().cloned());
    if let Some(rev) = rev {
        let t = ts.get(p, &o, &rev)?;
        return Some((o, rev, t, false));
    }
    let pr = core.analysis.progress(&p.info.id, &o)?;
    if pr.words.is_empty() || pr.revision.is_empty() {
        return None;
    }
    let words: Vec<crate::review::Word> = pr
        .words
        .iter()
        .map(|w| crate::review::Word {
            text: w.text.clone(),
            start: w.start,
            end: w.end,
        })
        .collect();
    Some((
        o,
        pr.revision,
        std::sync::Arc::new(Transcript {
            words,
            lines: Vec::new(),
        }),
        true,
    ))
}

fn word_ref(t: &Transcript, rev: &str, i: usize) -> Option<WordRef> {
    let w = t.words.get(i)?;
    let around = |r: std::ops::Range<usize>| -> Vec<String> {
        r.filter_map(|j| t.words.get(j))
            .map(|w| norm(&w.text))
            .collect()
    };
    Some(WordRef {
        word_id: WordId {
            asr_revision: rev.to_owned(),
            ordinal: u32::try_from(i).ok()?,
        },
        t_src_start: Flicks(w.start),
        t_src_end: Flicks(w.end),
        normalised_text: norm(&w.text),
        prev2: around(i.saturating_sub(2)..i),
        next2: around(i + 1..i + 3),
    })
}

/// The strip's rows: each part's words from its own video's transcript (a cut may play
/// several videos, decision 0036), each row naming its video (`owner`) and revision.
pub fn cut_words(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let cd = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id))
        .cloned();
    // the project's one-word caption corrections ("Fix everywhere"): shown as the captions
    // will show them
    let fixes: std::collections::BTreeMap<String, String> = doc
        .iter::<gmclip_model::entities::CaptionCorrections>()
        .flat_map(|c| c.entries.iter())
        .filter(|(k, v)| k.split_whitespace().count() == 1 && !v.trim().is_empty())
        .map(|(k, v)| (gmclip_plan::story_cues::norm(k), v.clone()))
        .collect();
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let layout = cut_layout(&doc, cid);
    let parts: Vec<&Part> = layout.iter().filter_map(|sp| doc.get(sp.part)).collect();
    // each video's transcript, read once
    let mut ts: std::collections::BTreeMap<String, Option<(String, std::sync::Arc<Transcript>)>> =
        std::collections::BTreeMap::new();
    for pt in &parts {
        let o = owner(&pt.media);
        if let std::collections::btree_map::Entry::Vacant(e) = ts.entry(o) {
            let t = transcript_for(core, &p, &doc, cid, Some(e.key())).map(|(_, r, t, _)| (r, t));
            e.insert(t);
        }
    }
    let Some(first_rev) = ts.values().flatten().map(|(r, _)| r.clone()).next() else {
        return Ok(
            json!({"state": "none", "words": [], "note": "no transcript for this cut's videos yet"}),
        );
    };
    // which part plays a word: the one holding the largest share of it (decision 0040,
    // `gmclip_core::script::plays_in`: ASR times run on through the silence beside a
    // word, so its middle can lie outside the part that plays it)
    let ranges_of = |media: &MediaRef| -> Vec<(i64, i64)> {
        layout
            .iter()
            .filter_map(|sp| doc.get(sp.part))
            .filter(|p| p.media == *media)
            .map(|p| (p.src_in.0, p.src_out.0))
            .collect()
    };
    let covered = |media: &MediaRef, w: &crate::review::Word| {
        gmclip_core::script::plays_in(w.start, w.end, &ranges_of(media)).is_some()
    };
    let mut rows = Vec::new();
    let mut listed: std::collections::BTreeSet<(String, usize)> = std::collections::BTreeSet::new();
    let mut prev: Option<&Part> = None;
    for (n, span) in layout.iter().enumerate() {
        let Some(part) = doc.get(span.part) else {
            continue;
        };
        let o = owner(&part.media);
        let Some((rev, t)) = ts.get(&o).cloned().flatten() else {
            prev = Some(part);
            continue;
        };
        let keywords: std::collections::BTreeSet<u32> = cd
            .as_ref()
            .map(|c| {
                c.keywords
                    .values()
                    .filter(|w| w.asr_revision == rev)
                    .map(|w| w.ordinal)
                    .collect()
            })
            .unwrap_or_default();
        let mut push = |rows: &mut Vec<Value>, i: usize, w: &crate::review::Word, frame, struck| {
            if struck && !listed.insert((o.clone(), i)) {
                return;
            }
            let mut r = row(&cd, &fixes, &keywords, &rev, i, w, n, frame, struck);
            r["owner"] = json!(o);
            r["revision"] = json!(rev);
            rows.push(r);
        };
        let contiguous = prev.is_some_and(|pp| {
            pp.media == part.media
                && part.src_in > pp.src_out
                && part.src_in.0 - pp.src_out.0 <= GAP
        });
        if contiguous {
            // The whole gap since the previous part of the same media (≤ 15 s).
            let pp = prev.unwrap_or(part);
            for (i, w) in t.words.iter().enumerate() {
                let mid = (w.start + w.end) / 2;
                if mid >= pp.src_out.0 && mid < part.src_in.0 && !covered(&part.media, w) {
                    push(&mut rows, i, w, None, true);
                }
            }
        } else {
            // After a jump: struck words just before this part (≤ 3 s), restorable.
            for (i, w) in t.words.iter().enumerate() {
                if w.end <= part.src_in.0
                    && w.start >= part.src_in.0 - EDGE
                    && !covered(&part.media, w)
                {
                    push(&mut rows, i, w, None, true);
                }
            }
        }
        let here = ranges_of(&part.media);
        let me = here
            .iter()
            .position(|&(a, b)| a == part.src_in.0 && b == part.src_out.0);
        for (i, w) in t.words.iter().enumerate() {
            if me.is_some() && gmclip_core::script::plays_in(w.start, w.end, &here) == me {
                let cut_t = span.start.0 + (w.start.max(part.src_in.0) - part.src_in.0);
                push(&mut rows, i, w, Some(cut_t / fpf), false);
            }
        }
        // Before a jump: struck words just after this part (≤ 3 s).
        let next_contiguous = layout
            .get(n + 1)
            .and_then(|sp| doc.get(sp.part))
            .is_some_and(|np| {
                np.media == part.media
                    && np.src_in > part.src_out
                    && np.src_in.0 - part.src_out.0 <= GAP
            });
        if !next_contiguous {
            for (i, w) in t.words.iter().enumerate() {
                if w.start >= part.src_out.0
                    && w.end <= part.src_out.0 + EDGE
                    && !covered(&part.media, w)
                {
                    push(&mut rows, i, w, None, true);
                }
            }
        }
        prev = Some(part);
    }
    // Re-spelling and word-edge tuning feed the caption builder; the M1 model planner
    // draws imported cues verbatim and refuses edits (lane A's caption builder, A.09).
    let respell = false;
    Ok(json!({
        "state": "ok",
        "revision": first_rev,
        "words": rows,
        "caption_source": cd.as_ref().map(|c| serde_json::to_value(c.source).unwrap_or(Value::Null)),
        "respell": respell,
        "respell_why": "Re-spelling and word-edge tuning arrive with the caption builder in the planner (A.09); this cut's captions are imported cues.",
    }))
}

#[allow(clippy::too_many_arguments)]
fn row(
    cd: &Option<CaptionDoc>,
    fixes: &std::collections::BTreeMap<String, String>,
    keywords: &std::collections::BTreeSet<u32>,
    rev: &str,
    i: usize,
    w: &crate::review::Word,
    part: usize,
    frame: Option<i64>,
    struck: bool,
) -> Value {
    let id = WordId {
        asr_revision: rev.to_owned(),
        ordinal: i as u32,
    };
    let edit = cd.as_ref().and_then(|c| c.edits.get(&id));
    let fixed = fixes.get(&gmclip_plan::story_cues::norm(&w.text)).map(|f| {
        let tail = &w.text[w.text.trim_end_matches(['.', ',', '?', '!']).len()..];
        format!("{f}{}", if f.ends_with(tail) { "" } else { tail })
    });
    json!({
        "i": i,
        "text": w.text,
        "shown": match (edit, &fixed) {
            (Some(CaptionEdit::Text(t)), _) => Value::from(t.clone()),
            (Some(CaptionEdit::Drop), _) => Value::Null,
            (None, Some(f)) => Value::from(f.clone()),
            (None, None) => Value::from(w.text.clone()),
        },
        "edited": edit.is_some() || fixed.is_some(),
        "part": part + 1,
        "frame": frame,
        "dur_ms": (w.end - w.start) * 1000 / FLICKS,
        "struck": struck,
        "keyword": keywords.contains(&(i as u32)),
    })
}

/// The cut's word-edge overrides (`word_edge_nudge`), by word.
pub fn overrides(
    doc: &Doc,
    cut: Id<Cut>,
) -> std::collections::BTreeMap<WordId, (Option<Flicks>, Option<Flicks>)> {
    doc.children::<CaptionDoc>(cut.entity())
        .find_map(|id| doc.get(id))
        .map(|c| {
            c.word_time_overrides
                .iter()
                .map(|(k, o)| (k.clone(), (o.start, o.end)))
                .collect()
        })
        .unwrap_or_default()
}

/// The words `from..=to` with their tuned edges (strike, restore and add-as-part cut at
/// the edges the human tuned: "affects export at sample precision", SPEC §5.4).
fn tuned_refs_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    owner_id: Option<&str>,
) -> AppResult<Vec<WordRef>> {
    let mut v = refs_of(core, project, cut, from, to, owner_id)?;
    let p = core.project(project)?;
    let ov = overrides(&p.handle.snapshot().doc, ulid_id(cut)?);
    for w in &mut v {
        if let Some((s, e)) = ov.get(&w.word_id) {
            w.t_src_start = s.unwrap_or(w.t_src_start);
            w.t_src_end = e.unwrap_or(w.t_src_end);
        }
    }
    Ok(v)
}

/// Words `from..=to` of a video's transcript (`owner`; the cut's first video's when
/// `None`) with their ASR edges (the words' fingerprints). A run longer than 10 000 words
/// (an hour of a stream edit struck at once) is sent as its two end words: strike and
/// restore read the range from the first and last (decision 0011).
pub(crate) fn refs_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    owner_id: Option<&str>,
) -> AppResult<Vec<WordRef>> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let (_, rev, t, _) = transcript_for(core, &p, &doc, cid, owner_id)
        .ok_or_else(|| AppError::new("Unsupported", "no transcript for this cut's stream yet"))?;
    let (a, b) = (from.min(to) as usize, from.max(to) as usize);
    if b - a > 10_000 {
        return [a, b]
            .into_iter()
            .map(|i| {
                word_ref(&t, &rev, i)
                    .ok_or_else(|| AppError::invalid("a word outside the transcript"))
            })
            .collect();
    }
    (a..=b)
        .map(|i| {
            word_ref(&t, &rev, i).ok_or_else(|| AppError::invalid("a word outside the transcript"))
        })
        .collect()
}

/// Strikes (or restores) the words with ordinals `from..=to`.
pub fn strike(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    restore: bool,
) -> AppResult<Value> {
    strike_of(core, project, cut, from, to, restore, None)
}

/// [`strike`] on one video's transcript (`owner`), only on that video's parts.
pub fn strike_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    restore: bool,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let media = media_for(core, project, cut, owner_id)?;
    // The words go as their fingerprints (ASR edges, or the edges the human tuned): core
    // cuts where the voice is (decision 0049), takes a part's wordless head or tail with a
    // strike that reaches it (decision 0040) and lands both ends on the frame grid.
    let words = tuned_refs_of(core, project, cut, from, to, owner_id)?;
    let cmd = if restore {
        "words_restore"
    } else {
        "words_strike"
    };
    core.call(
        project,
        cmd,
        json!({"cut": cut, "words": words, "media": media}),
    )
}

/// The media of the transcript the pane shows: the owner named, else the cut's first
/// part's.
fn media_for(
    core: &AppCore,
    project: &str,
    cut: &str,
    owner_id: Option<&str>,
) -> AppResult<MediaRef> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    match owner_id {
        Some(o) => gmclip_core::commands::topic::media_of_owner(&doc, o)
            .ok_or_else(|| AppError::not_found("no such video in this project")),
        None => {
            let cid: Id<Cut> = ulid_id(cut)?;
            cut_layout(&doc, cid)
                .first()
                .and_then(|sp| doc.get(sp.part))
                .map(|pt: &Part| pt.media)
                .ok_or_else(|| AppError::invalid("the cut has no part to take the source from"))
        }
    }
}

/// Nudges one word's start or end by `delta_ms` (captions and export only; SPEC §5.4).
pub fn nudge(
    core: &AppCore,
    project: &str,
    cut: &str,
    ordinal: u64,
    edge: &str,
    delta_ms: i64,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    if edge != "start" && edge != "end" {
        return Err(AppError::invalid("edge is start or end"));
    }
    let w = refs_of(core, project, cut, ordinal, ordinal, owner_id)?
        .pop()
        .ok_or_else(|| AppError::invalid("no word"))?;
    core.call(
        project,
        "word_edge_nudge",
        json!({"cut": cut, "word": w, "edge": edge, "delta_ms": delta_ms.clamp(-2000, 2000)}),
    )
}

/// Re-spells one word in the cut's captions (audio untouched): `text` = the spelling,
/// "" drops it from captions, `None` clears the edit (↺ spelling).
pub fn respell(
    core: &AppCore,
    project: &str,
    cut: &str,
    ordinal: u64,
    text: Option<&str>,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let w = refs_of(core, project, cut, ordinal, ordinal, owner_id)?
        .pop()
        .ok_or_else(|| AppError::invalid("no word"))?;
    let edit = match text.map(str::trim) {
        None => Value::Null,
        Some("") => json!("drop"),
        Some(t) if t.chars().count() > 200 => {
            return Err(AppError::invalid("at most 200 characters"));
        }
        Some(t) => json!({"text": t}),
    };
    core.call(
        project,
        "caption_edit",
        json!({"cut": cut, "word": w.word_id, "edit": edit}),
    )
}

/// ↺ edges: puts a tuned word back on its ASR edges.
pub fn edge_reset(
    core: &AppCore,
    project: &str,
    cut: &str,
    ordinal: u64,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let w = refs_of(core, project, cut, ordinal, ordinal, owner_id)?
        .pop()
        .ok_or_else(|| AppError::invalid("no word"))?;
    let p = core.project(project)?;
    let ov = overrides(&p.handle.snapshot().doc, ulid_id(cut)?);
    let Some((s, e)) = ov.get(&w.word_id).copied() else {
        return Ok(json!({"reset": false}));
    };
    let ms = |a: Flicks, b: Flicks| (b.0 - a.0) * 1000 / FLICKS;
    let mut steps = Vec::new();
    if let Some(s) = s {
        steps.push(("start", ms(s, w.t_src_start)));
    }
    if let Some(e) = e {
        steps.push(("end", ms(e, w.t_src_end)));
    }
    // move the end first when the start goes later (the word stays >= 20 ms throughout)
    if steps.first().is_some_and(|x| x.0 == "start" && x.1 > 0) {
        steps.reverse();
    }
    for (edge, d) in steps {
        if d != 0 {
            core.call(
                project,
                "word_edge_nudge",
                json!({"cut": cut, "word": w, "edge": edge, "delta_ms": d}),
            )?;
        }
    }
    Ok(json!({"reset": true}))
}

/// ＋ add as part: the words `from..=to` (tuned edges, on the frame grid) become a new
/// part of the cut after part `after`, or after the last part.
pub fn add_as_part(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    after: Option<&str>,
) -> AppResult<Value> {
    add_as_part_of(core, project, cut, from, to, after, None)
}

/// ＋ add as part from any video's transcript (`owner`: the pane's source picker): the
/// part plays that video, and takes the look the cut's other parts of that shape have.
pub fn add_as_part_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    after: Option<&str>,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let words = tuned_refs_of(core, project, cut, from, to, owner_id)?;
    let (Some(f), Some(l)) = (words.first(), words.last()) else {
        return Err(AppError::invalid("no words"));
    };
    let media = media_for(core, project, cut, owner_id)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let layout = cut_layout(&doc, cid);
    let after = match after {
        Some(a) => Some(a.to_owned()),
        None => layout.last().map(|sp| sp.part.0.to_string()),
    };
    let fps = core
        .planned(project, cut)
        .map_or((30, 1), |pl| crate::planner::fps(&pl.plan));
    let k0 = crate::planner::frame_of(f.t_src_start, fps).unwrap_or(0);
    let k1 = crate::planner::frame_of(l.t_src_end, fps)
        .unwrap_or(k0 + 1)
        .max(k0 + 1);
    let (a, b) = (
        crate::planner::time_of(k0, fps),
        crate::planner::time_of(k1, fps),
    );
    let mut args = json!({"cut": cut, "media": media, "src_in": a.0, "src_out": b.0});
    if let Some(x) = after {
        args["after"] = json!(x);
    }
    let r = core.call(project, "part_add", args)?;
    // from another video: its look per its own picture (the preset the cut's parts use)
    if let Some(part) = r["output"]["id"].as_str() {
        crate::framing::fit_new_part(core, project, cut, part)?;
    }
    Ok(r)
}

/// ↧ stretch: the cut's last part ends at word `to`'s (tuned) end.
pub fn stretch_last(
    core: &AppCore,
    project: &str,
    cut: &str,
    to: u64,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let w = tuned_refs_of(core, project, cut, to, to, owner_id)?
        .pop()
        .ok_or_else(|| AppError::invalid("no word"))?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let last = cut_layout(&doc, cid)
        .last()
        .map(|sp| sp.part)
        .ok_or_else(|| AppError::invalid("the cut has no parts"))?;
    let part: &Part = doc.get(last).ok_or_else(|| AppError::invalid("no part"))?;
    let fps = core
        .planned(project, cut)
        .map_or((30, 1), |pl| crate::planner::fps(&pl.plan));
    let k = crate::planner::frame_of(w.t_src_end, fps).unwrap_or(0);
    let out = crate::planner::time_of(k, fps);
    if out <= part.src_in {
        return Err(AppError::invalid(
            "that word is before the last part starts",
        ));
    }
    core.call(
        project,
        "part_trim",
        json!({"part": last.0.to_string(), "src_out": out.0}),
    )
}
