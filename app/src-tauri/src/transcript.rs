//! The transcript pane's data (AJ 2026-10-14: "see transcript … cut the transcript"):
//! every word of the cut's source in source order, which ones the cut plays (and at which
//! output frame), lines for paragraphs, keywords, and the analysis progress while words
//! are still arriving.
//!
//! Columnar so a stream's 30 000 words stay a small reply: `text[i]`, `start_ms[i]`,
//! `frame[i]` (−1 when the cut does not play the word). The pane addresses words by
//! ordinal `i` in `revision`; striking and restoring go through `words_strike_ord` /
//! `words_restore_ord` (core `words_strike` / `words_restore`, undoable).

use gmclip_model::Id;
use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{CaptionDoc, Cut, MediaRef, Part};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

fn owner(m: &MediaRef) -> String {
    match m {
        MediaRef::Group(g) => g.0.to_string(),
        MediaRef::Source(s) => s.0.to_string(),
    }
}

pub fn transcript_view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    transcript_view_of(core, project, cut, None)
}

/// What `cut` plays, part by part in output order: `(owner, src_in, src_out, start)`,
/// `start` on the cut's clock (flicks).
fn plays(doc: &gmclip_model::Doc, cut: Id<Cut>) -> Vec<(String, i64, i64, i64)> {
    cut_layout(doc, cut)
        .iter()
        .filter_map(|sp| {
            let pt: &Part = doc.get(sp.part)?;
            Some((owner(&pt.media), pt.src_in.0, pt.src_out.0, sp.start.0))
        })
        .collect()
}

/// The project's videos as the pane's source picker lists them: each analysis owner (a
/// source, or a same-recording group) with its file name, whether this cut plays it.
pub fn videos(doc: &gmclip_model::Doc, cut: Id<Cut>) -> Vec<Value> {
    let played: Vec<String> = plays(doc, cut).into_iter().map(|x| x.0).collect();
    crate::analysis::owners(doc)
        .into_iter()
        .map(|(o, s)| {
            let file = crate::file_part(&s.path);
            let name = file.rsplit_once('.').map_or(file, |(n, _)| n);
            json!({"owner": o, "name": name, "in_cut": played.contains(&o)})
        })
        .collect()
}

/// [`transcript_view`] of one video of the project (`owner`, the source picker), the
/// cut's first video's when `None`: which of its words this cut plays.
pub fn transcript_view_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    owner_id: Option<&str>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let played = plays(&doc, cid);
    let first_owner = match owner_id {
        Some(o) => Some(o.to_owned()),
        None => played.first().map(|x| x.0.clone()),
    };
    let vids = videos(&doc, cid);
    let progress = first_owner
        .as_deref()
        .and_then(|o| core.analysis.progress(project, o))
        .map(|pr| {
            json!({"phase": pr.phase, "done_s": pr.done_s, "total_s": pr.total_s,
                   "error": pr.error, "asr_error": pr.asr_error,
                   "eta_s": crate::analysis::eta_s(&pr).map(f64::round)})
        });
    let Some((o, rev, t, pending)) =
        crate::words::transcript_for(core, &p, &doc, cid, first_owner.as_deref())
    else {
        return Ok(
            json!({"state": "none", "progress": progress, "text": [], "start_ms": [], "dur_ms": [],
                         "frame": [], "lines": [], "keywords": [], "owner": first_owner, "videos": vids}),
        );
    };
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    // The cut's parts that play this owner, by source in (output order kept for frames).
    let mut spans: Vec<(i64, i64, i64)> = played
        .iter()
        .filter(|x| x.0 == o)
        .map(|x| (x.1, x.2, x.3))
        .collect();
    spans.sort_by_key(|s| s.0);
    // the human's tuned edges and re-spellings (sparse)
    let ov = crate::words::overrides(&doc, cid);
    let cd = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id));
    let mut tuned = serde_json::Map::new();
    let mut edits = serde_json::Map::new();
    let n = t.words.len();
    let mut text = Vec::with_capacity(n);
    let mut start_ms = Vec::with_capacity(n);
    let mut dur_ms = Vec::with_capacity(n);
    let mut frame = Vec::with_capacity(n);
    for (i, w0) in t.words.iter().enumerate() {
        let id = gmclip_model::anchor::WordId {
            asr_revision: rev.clone(),
            ordinal: u32::try_from(i).unwrap_or(u32::MAX),
        };
        let mut w = w0.clone();
        if let Some((s, e)) = ov.get(&id) {
            w.start = s.map_or(w.start, |f| f.0);
            w.end = e.map_or(w.end, |f| f.0);
            tuned.insert(
                i.to_string(),
                json!([
                    (w.start - w0.start) * 1000 / FLICKS,
                    (w.end - w0.end) * 1000 / FLICKS
                ]),
            );
        }
        if let Some(e) = cd.and_then(|c| c.edits.get(&id)) {
            edits.insert(
                i.to_string(),
                match e {
                    gmclip_model::entities::CaptionEdit::Drop => json!(""),
                    gmclip_model::entities::CaptionEdit::Text(t) => json!(t),
                },
            );
        }
        let w = &w;
        text.push(Value::from(w.text.as_str()));
        start_ms.push(Value::from(w.start * 1000 / FLICKS));
        dur_ms.push(Value::from((w.end - w.start).max(0) * 1000 / FLICKS));
        // The last part starting at or before the word's middle, if it plays the middle.
        let mid = (w.start + w.end) / 2;
        let k = spans.partition_point(|s| s.0 <= mid);
        let f = k
            .checked_sub(1)
            .map(|j| spans[j])
            .filter(|s| mid < s.1)
            .map_or(-1, |s| (s.2 + (w.start.max(s.0) - s.0)) / fpf);
        frame.push(Value::from(f));
    }
    let lines: Vec<Value> = if t.lines.is_empty() {
        let ws: Vec<gmclip_core::analysis::Word> = t
            .words
            .iter()
            .map(|w| gmclip_core::analysis::Word {
                text: w.text.clone(),
                start: w.start,
                end: w.end,
                confidence: None,
            })
            .collect();
        gmclip_core::analysis::make_lines(&ws)
            .iter()
            .map(|l| json!(l.words))
            .collect()
    } else {
        t.lines.iter().map(|l| json!(l.words)).collect()
    };
    let keywords: Vec<u32> = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id))
        .map(|c| {
            c.keywords
                .values()
                .filter(|w| w.asr_revision == rev)
                .map(|w| w.ordinal)
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "state": if pending { "transcribing" } else { "ok" },
        "progress": progress,
        "revision": rev,
        // frames per second (29.97… at an NTSC rate)
        "fps": FLICKS as f64 / fpf as f64,
        "text": text,
        "start_ms": start_ms,
        "dur_ms": dur_ms,
        "frame": frame,
        "lines": lines,
        "keywords": keywords,
        "tuned": tuned,
        "edits": edits,
        "owner": o,
        "videos": vids,
    }))
}
