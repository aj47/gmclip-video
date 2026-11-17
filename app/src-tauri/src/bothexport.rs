//! 9:16 and 16:9 together (lane K5): one action exports both framings of a cut, the
//! vertical and the horizontal, as two export jobs with their own records (each names
//! its framing, so each framing's freshness is its own). A cut with only one of them is
//! offered the other from the layout defaults first (`framing::ensure`: the kit's
//! template and caption seat for a 9:16, the full picture for a 16:9), without changing
//! the framing the cut shows.
//!
//! - `export_framings{cut}`: the cut's 9:16 and 16:9 framings with each one's export
//!   state, and the aspects it lacks;
//! - `framing_make{cut, aspect}`: makes the missing one (undoable edits, as the output
//!   toggle's);
//! - `export_both{cut, make_missing}`: starts both exports (9:16 first), making the
//!   missing framing first when asked; refused when one is missing and not asked.

use std::sync::Arc;

use gmclip_model::entities::{Cut, Framing};
use serde_json::{Value, json};

use crate::core::{AppCore, plan_sig, sig_of};
use crate::error::{AppError, AppResult};
use crate::jobs::Jobs;
use crate::planner::ulid_id;

/// The two shapes, in export order.
pub const ASPECTS: [&str; 2] = ["9:16", "16:9"];

/// The framing that stands for one aspect of a cut.
struct Chosen {
    aspect: &'static str,
    id: String,
    name: String,
    w: u32,
    h: u32,
}

/// The framing that stands for each aspect the cut has: the one it shows when it has that
/// shape, else its first of that shape.
fn chosen(core: &AppCore, project: &str, cut: &str) -> AppResult<Vec<Chosen>> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let shown = core.active_framing(project, cut);
    let all: Vec<&Framing> = doc
        .children::<Framing>(cid.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    let mut out = Vec::new();
    for a in ASPECTS {
        let of: Vec<&&Framing> = all
            .iter()
            .filter(|f| crate::framing::aspect_of(f.canvas.w, f.canvas.h) == a)
            .collect();
        let pick = of
            .iter()
            .find(|f| Some(f.id.0.to_string()) == shown)
            .or_else(|| of.first());
        if let Some(f) = pick {
            out.push(Chosen {
                aspect: a,
                id: f.id.0.to_string(),
                name: f.name.clone(),
                w: f.canvas.w,
                h: f.canvas.h,
            });
        }
    }
    Ok(out)
}

/// The export state of one framing: `never`, `fresh` or `stale` against the plan of
/// that framing now.
fn export_state(core: &AppCore, project: &str, cut: &str, framing: &str) -> Value {
    let shown = core.active_framing(project, cut);
    let last = core.exports(project).ok().and_then(|all| {
        all.into_iter()
            .filter(|e| {
                e.cut == cut
                    && match &e.framing {
                        Some(f) => f == framing,
                        None => shown.as_deref() == Some(framing),
                    }
            })
            .max_by_key(|e| e.at_ms)
    });
    let Some(e) = last else {
        return json!({"state": "never"});
    };
    let fresh = core
        .planned_in(project, cut, Some(framing))
        .is_ok_and(|pl| {
            let sig = sig_of(&pl.plan);
            (e.render_sig.is_some() && e.render_sig == sig)
                || (e.render_sig.is_none()
                    && e.plan_sig.is_some()
                    && e.plan_sig == plan_sig(&pl.plan))
        });
    json!({"state": if fresh { "fresh" } else { "stale" }, "at_ms": e.at_ms, "id": e.id,
           "file": e.file})
}

/// `export_framings{cut}`.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let have = chosen(core, project, cut)?;
    let missing: Vec<&str> = ASPECTS
        .iter()
        .copied()
        .filter(|a| !have.iter().any(|h| h.aspect == *a))
        .collect();
    // an imported RenderPlan has one output of its own
    let supported = core.imported(project, cut).is_err();
    let framings: Vec<Value> = have
        .iter()
        .map(|c| {
            json!({"aspect": c.aspect, "id": c.id, "name": c.name, "w": c.w, "h": c.h,
                   "export": export_state(core, project, cut, &c.id)})
        })
        .collect();
    Ok(
        json!({"cut": cut, "framings": framings, "missing": missing, "supported": supported,
              "why_not": (!supported).then_some("an imported RenderPlan exports at its own output only")}),
    )
}

/// `framing_make{cut, aspect}`: the missing framing from the layout defaults; the cut
/// keeps showing the one it shows.
pub fn make(core: &AppCore, project: &str, cut: &str, aspect: &str) -> AppResult<Value> {
    if core.imported(project, cut).is_ok() {
        return Err(AppError::new(
            "Unsupported",
            "an imported RenderPlan exports at its own output only",
        ));
    }
    let shown = core.active_framing(project, cut);
    let (framing, made) = crate::framing::ensure(core, project, cut, aspect)?;
    // a cut with no framing remembered keeps the one it showed before this one existed
    if let Some(s) = shown {
        core.set_active_framing(project, cut, &s)?;
    }
    core.bus.project(
        project,
        crate::events::ProjectEvent::Output {
            cut: cut.to_owned(),
        },
    );
    Ok(json!({"framing": framing, "aspect": aspect, "made": made}))
}

/// `export_both{cut, make_missing}`: both framings' exports, 9:16 first.
pub fn start(
    core: &Arc<AppCore>,
    jobs: &Arc<Jobs>,
    project: &str,
    cut: &str,
    make_missing: bool,
) -> AppResult<Value> {
    if core.imported(project, cut).is_ok() {
        return Err(AppError::new(
            "Unsupported",
            "an imported RenderPlan exports at its own output only",
        ));
    }
    let mut made = Vec::new();
    let have = chosen(core, project, cut)?;
    for a in ASPECTS {
        if have.iter().any(|h| h.aspect == a) {
            continue;
        }
        if !make_missing {
            return Err(AppError::new(
                "Unsupported",
                format!("this cut has no {a} framing yet: make it from the layout defaults first"),
            ));
        }
        make(core, project, cut, a)?;
        made.push(a);
    }
    let mut started = Vec::new();
    for c in chosen(core, project, cut)? {
        let j = jobs.start_export_framing(core, project, cut, &c.id)?;
        started.push(json!({"aspect": c.aspect, "framing": c.id, "name": c.name, "job": j["job"]}));
    }
    Ok(json!({"jobs": started, "made": made}))
}
