//! Three openings, one short (decision 0024) in the app: the versions of a cut side by
//! side, each playable in the viewer and exportable, with the retention its exports
//! carry.
//!
//! - A version is the cut with an alternate opening swapped in
//!   (`gmclip_core::commands::opening::version_doc`, never committed), planned the same
//!   way the cut is (the model planner, or an imported cut's base plan).
//! - Every export records the version it is (`ExportEntry.opening_sig`, the opening's
//!   spans), so a curve imported onto an export is that opening's: the view gives each
//!   version its newest curve's retention at 3 s, at the end of its opening and at the
//!   end.

use std::sync::Arc;

use gmclip_core::commands::opening::{main_spans, spans_sig, version_doc};
use gmclip_model::entities::{Cut, Opening};
use serde_json::{Value, json};

use crate::core::{AppCore, Planned};
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The plan of one version: the cut with `opening` swapped in.
pub fn planned_version(
    core: &AppCore,
    project: &str,
    cut: &str,
    opening: &str,
) -> AppResult<Planned> {
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let oid = ulid_id::<Opening>(opening)?;
    match snap.doc.get(oid) {
        Some(o) if o.cut.0.to_string() == cut => {}
        _ => return Err(AppError::not_found("no such opening on this cut")),
    }
    let doc = version_doc(&snap.doc, oid).map_err(|e| AppError::invalid(e.to_string()))?;
    let framing = core.active_framing(project, cut);
    let (plan, index, edited) =
        core.plan_doc(project, cut, &doc, &snap.analysis, framing.as_deref())?;
    Ok(Planned {
        plan: Arc::new(plan),
        index: Arc::new(index),
        edited,
        rev: snap.doc.rev(),
        heard: snap.analysis.heard_len(),
    })
}

/// The signature and name of a version (`None`: the main one, the cut as it is).
pub fn version_of(
    core: &AppCore,
    project: &str,
    cut: &str,
    opening: Option<&str>,
) -> AppResult<(String, String)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    Ok(match opening {
        None => (spans_sig(&main_spans(&doc, cid)), "Main".to_owned()),
        Some(o) => {
            let o = doc
                .get(ulid_id::<Opening>(o)?)
                .ok_or_else(|| AppError::not_found("no such opening"))?;
            (spans_sig(&o.spans), o.label.clone())
        }
    })
}

fn pct(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// The versions of a cut, side by side: `openings_list` plus each version's exports and
/// the newest retention one of them carries.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let mut list = core.call(project, "openings_list", json!({"cut": cut}))?["output"].take();
    let exports: Vec<_> = core
        .exports(project)?
        .into_iter()
        .filter(|e| e.cut == cut)
        .collect();
    if let Some(vs) = list["versions"].as_array_mut() {
        for v in vs {
            let sig = v["sig"].as_str().unwrap_or_default().to_owned();
            let opening_s = v["opening_ms"].as_f64().unwrap_or(0.0) / 1000.0;
            let mine: Vec<_> = exports
                .iter()
                .filter(|e| e.opening_sig.as_deref() == Some(sig.as_str()))
                .collect();
            v["exports"] = json!(mine.len());
            v["last_export"] = json!(mine.iter().max_by_key(|e| e.at_ms).map(|e| e.id.clone()));
            let best = mine
                .iter()
                .filter_map(|e| {
                    crate::retention::newest_curve(core, project, &e.id).map(|c| (c, e))
                })
                .max_by_key(|(c, _)| c.imported_ms);
            v["retention"] = match best {
                Some((c, e)) => {
                    let at = |t: f64| pct(crate::retention::at(&c.points, t.min(c.duration_s)));
                    json!({
                        "export": e.id,
                        "platform": c.platform,
                        "source": c.source,
                        "at_3s": at(3.0),
                        "at_opening_end": at(opening_s),
                        "at_end": at(c.duration_s),
                        "duration_s": c.duration_s,
                    })
                }
                None => Value::Null,
            };
        }
    }
    Ok(list)
}

/// An alternate opening from words selected in the transcript (ordinals of the cut's
/// transcript, first to last, inclusive): `opening_add` from their word edges.
pub fn add_words(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    label: Option<&str>,
    hook_text: Option<&str>,
) -> AppResult<Value> {
    let (a, b) = (from.min(to), from.max(to));
    let start = crate::short::word_ref(core, project, cut, a)?;
    let end = crate::short::word_ref(core, project, cut, b)?;
    let mut args = json!({"cut": cut, "from": {"span": {"start": {"word_start": start}, "end": {"word_end": end}}}});
    if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
        args["label"] = json!(l);
    }
    if let Some(h) = hook_text.filter(|h| !h.trim().is_empty()) {
        args["hook_text"] = json!(h);
    }
    Ok(core.call(project, "opening_add", args)?["output"].take())
}
