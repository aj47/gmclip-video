//! Cut checks (SPEC §5.13 [M2], A2.17): the core lint (agent set + the M2 core set,
//! judged against the project's transcripts) plus the two rules that need the app's
//! export records — `export_stale` (the last export is not this cut any more) and
//! `sibling_exported` (a part plays more than 0.5 s that another, exported cut of the
//! project already used). Warnings, never refusals.

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;
use gmclip_model::entities::{Cut, Part};

const SEC: i64 = 705_600_000;

/// The checks for one cut, most severe first.
pub fn checks(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let cid = ulid_id::<Cut>(cut)?;
    let mut out: Vec<Value> = gmclip_core::lint::lint_cut_in(doc, Some(&snap.analysis), cid)
        .into_iter()
        .map(|f| json!({"rule": f.rule, "level": f.level, "message": f.message, "fix": f.fix}))
        .collect();
    // export_stale
    if let Ok(v) = core.cut_view(project, cut)
        && v["export"]["state"] == "stale"
    {
        // SPEC §12.6: a stale export's one-word action is "re-export"
        out.push(json!({"rule": "export_stale", "level": "warning",
                        "message": "the last export is not this cut any more: export again before you send it",
                        "fix": {"label": "Export again", "action": "export"}}));
    }
    // sibling_exported
    let exported: std::collections::BTreeSet<String> = core
        .exports(project)
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.cut)
        .filter(|c| c != cut)
        .collect();
    let mine: Vec<&Part> = doc
        .children::<Part>(cid.entity())
        .filter_map(|x| doc.get(x))
        .collect();
    for other in &exported {
        let Ok(oid) = ulid_id::<Cut>(other) else {
            continue;
        };
        let Some(oc) = doc.get(oid) else { continue };
        let theirs: Vec<&Part> = doc
            .children::<Part>(oid.entity())
            .filter_map(|x| doc.get(x))
            .collect();
        let shared: i64 = mine
            .iter()
            .flat_map(|a| theirs.iter().map(move |b| (a, b)))
            .filter(|(a, b)| a.media == b.media)
            .map(|(a, b)| (a.src_out.0.min(b.src_out.0) - a.src_in.0.max(b.src_in.0)).max(0))
            .sum();
        if shared > SEC / 2 {
            let title: String = oc.title.chars().take(40).collect();
            out.push(json!({"rule": "sibling_exported", "level": "warning",
                            "message": format!("{:.1} s of this cut is already in the exported cut “{title}”", shared as f64 / SEC as f64)}));
        }
    }
    let rank = |v: &Value| match v["level"].as_str() {
        Some("error") => 0,
        Some("warning") => 1,
        _ => 2,
    };
    out.sort_by_key(rank);
    Ok(json!({"findings": out}))
}

/// Runs the one-click fix of finding `n` of [`checks`] (the human's click). `rule` must
/// still be that finding's rule: the list is worked out again here, never trusted from the
/// page, and a fix is a registry command the core checks like any other edit.
pub fn fix(core: &AppCore, project: &str, cut: &str, n: usize, rule: &str) -> AppResult<Value> {
    let v = checks(core, project, cut)?;
    let f = v["findings"]
        .get(n)
        .filter(|f| f["rule"] == rule)
        .ok_or_else(|| AppError::new("Conflict", "the checks changed: look at them again"))?;
    let command = f["fix"]["command"].as_str().ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "this check has no fix here (Export again is the export button)",
        )
    })?;
    if gmclip_core::registry::find(command).is_none() {
        return Err(AppError::internal("a fix names an unknown command"));
    }
    core.call(project, command, f["fix"]["args"].clone())
}
