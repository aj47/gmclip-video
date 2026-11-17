//! Reaction moments (Ideas 2026-10-16, K.06): "Find laughs and reactions" on the moments
//! panel marks candidate moments where the stream laughs or reacts big —
//! `gmclip_analysis::reactions` over each recording's stored level frames and its
//! transcript (a spike over the recording's own baseline with laughter or reaction words
//! near it; thresholds are z-scores of that recording, never a fixed dB).
//!
//! Each candidate becomes a moment (`moment_mark from: reaction`): origin `local`, shown
//! as "Found by sound and transcript", type `reaction`, its span the 15 s before the peak
//! to 3 s after, snapped to sentences, its note what was heard and how big. Running it
//! again adds only what is not marked yet. Precision: **not measured** (no labelled
//! laughter in the lab streams).

use gmclip_analysis::blob::Blob;
use gmclip_analysis::energy::Energy;
use gmclip_analysis::reactions::{CueWord, Reaction, ReactionKind, find};
use gmclip_model::entities::{Moment, MomentOrigin};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::AppResult;
use crate::planner::FLICKS;

/// What precision we can claim (no labelled reactions exist yet).
pub const PRECISION: &str = "not measured";

/// The newest level frames stored for `owner` (40 ms preferred: a 3-hour stream's
/// reactions need no finer grid): `(dB frames, hop ms)`.
pub(crate) fn energy(p: &crate::core::OpenProject, owner: &str) -> Option<(Vec<f32>, u32)> {
    let keys = p.handle.analysis_keys();
    for (kind, hop) in [
        ("energy_40ms", 40),
        ("energy_20ms", 20),
        ("energy_10ms", 10),
    ] {
        let Some(rev) = keys
            .iter()
            .filter(|(o, k, _)| o == owner && k == kind)
            .map(|(_, _, r)| r.clone())
            .max()
        else {
            continue;
        };
        let bytes = p.handle.analysis(owner, kind, &rev)?;
        let e = Energy::from_blob(&Blob::decode(&bytes).ok()?).ok()?;
        return Some((e.db, hop));
    }
    None
}

fn note(r: &Reaction) -> String {
    let what = match r.kind {
        ReactionKind::Laugh => "laughed",
        ReactionKind::Reaction => "reacted",
        ReactionKind::Loud => "a loud moment",
    };
    let cues: Vec<&str> = r.cues.iter().take(4).map(String::as_str).collect();
    let said = if cues.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", cues.join(" "))
    };
    format!(
        "{what}{said} · {:.1}σ over its usual level",
        r.z_level.max(r.z_onset)
    )
}

/// `reactions_find{}`: marks the reaction candidates of every recording in the project
/// as moments (only those not marked yet). Returns what it found and added.
pub fn find_moments(core: &AppCore, project: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let doc = snap.doc.clone();
    let owners = crate::analysis::owners(&doc);
    let mut found = Vec::new();
    let mut added = 0;
    let mut no_sound = Vec::new();
    for (owner, s) in owners {
        let Some(t) = snap.analysis.active(&doc, &owner) else {
            continue;
        };
        let Some((db, hop)) = energy(&p, &owner) else {
            no_sound.push(owner.clone());
            continue;
        };
        let words: Vec<CueWord> = t
            .words
            .iter()
            .map(|w| CueWord {
                text: w.text.clone(),
                start_ms: w.start * 1000 / FLICKS,
                end_ms: w.end * 1000 / FLICKS,
            })
            .collect();
        let media = if owner == s.id.0.to_string() {
            json!({"source": owner})
        } else {
            json!({"group": owner})
        };
        // what is marked already: reaction moments of this recording
        let marked: Vec<(i64, i64)> = doc
            .iter::<Moment>()
            .filter(|m| m.origin == MomentOrigin::Local && m.r#type == "reaction")
            .filter(|m| serde_json::to_value(m.media).ok().as_ref() == Some(&media))
            .filter_map(|m| {
                let a = gmclip_model::clock::anchor_src(&doc, &m.span.start)?;
                let b = gmclip_model::clock::anchor_src(&doc, &m.span.end)?;
                Some((a.0 * 1000 / FLICKS, b.0 * 1000 / FLICKS))
            })
            .collect();
        for r in find(&db, hop, &words) {
            let mut row = json!({
                "media": media, "peak_s": r.peak_ms as f64 / 1000.0,
                "start_s": r.start_ms as f64 / 1000.0, "end_s": r.end_ms as f64 / 1000.0,
                "kind": r.kind, "z_level": r.z_level, "z_onset": r.z_onset,
                "cues": r.cues, "score": r.score, "added": false,
            });
            if marked
                .iter()
                .any(|(a, b)| *a <= r.peak_ms && r.peak_ms <= *b)
            {
                row["already"] = json!(true);
                found.push(row);
                continue;
            }
            let m = core.call(
                project,
                "moment_mark",
                json!({"media": media, "start_s": r.start_ms as f64 / 1000.0,
                       "end_s": r.end_ms as f64 / 1000.0, "snap": "sentence", "from": "reaction",
                       "note": note(&r), "score": r.score}),
            );
            if let Ok(m) = m {
                row["added"] = json!(true);
                row["moment"] = m["output"]["id"].clone();
                added += 1;
            }
            found.push(row);
        }
    }
    Ok(json!({
        "found": found.len(), "added": added, "candidates": found,
        "no_sound": no_sound, "precision": PRECISION,
    }))
}
