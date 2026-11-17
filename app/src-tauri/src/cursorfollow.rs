//! "Follow cursor" on a part (SPEC §5.7; AJ 2026-10-15: "only needed some times, but a
//! nice feature to have"): opt-in per part. On: the holder's own `follow_cursor` (the
//! agent tool `crop_follow_cursor` runs the same code) finds the cursor at 2 fps and the
//! zoomed crop path is written as eased visual splits — one undo step; off removes them.
//! The preview draws them through the shared compose, as the export does.

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::AppResult;

/// `follow_cursor {cut, part, on, max_zoom?}`.
pub fn follow_cursor(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    on: bool,
    max_zoom: Option<f64>,
) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, Framing, Part};
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let pid = crate::planner::ulid_id::<Part>(part)?;
    let framing = core
        .active_framing(project, cut)
        .and_then(|f| crate::planner::ulid_id::<Framing>(&f).ok());
    let planner = gmclip_core::host::ModelPlanner(core.plan_host());
    let t0 = std::time::Instant::now();
    let f = gmclip_mcp::cursor::follow_cursor(
        &planner,
        &crate::roto::Frames,
        &snap,
        cid,
        pid,
        framing,
        None,
        max_zoom,
        None,
        !on,
    )?;
    let ms = t0.elapsed().as_millis() as u64;
    let r = core.call(project, "cursor_follow_apply", f.apply_args())?;
    Ok(json!({
        "on": on, "samples": f.samples, "found": f.found, "moves": f.keys.len(),
        "max_zoom": f.max_zoom, "ms": ms, "committed": r["committed"].clone(),
        // decision 0045: a recording follows its logged cursor, anything else the pixels
        "method": f.method, "note": f.note,
    }))
}

/// How many follow moves a part has in a framing (the switch's state).
pub fn moves(
    doc: &gmclip_model::Doc,
    part: &gmclip_model::entities::Part,
    framing: gmclip_model::Id<gmclip_model::entities::Framing>,
) -> usize {
    use gmclip_model::entities::VisualSplit;
    let Some(stack) = gmclip_model::effective::effective(doc, part.id, framing, part.src_in) else {
        return 0;
    };
    doc.children::<VisualSplit>(part.id.entity())
        .filter_map(|s| doc.get(s))
        .filter(|s| {
            s.framing == framing
                && matches!(
                    s.transition,
                    gmclip_model::entities::Transition::Ease { .. }
                )
                && stack
                    .layers
                    .iter()
                    .any(|l| gmclip_core::commands::cursor::is_follow_split(s, l.id))
        })
        .count()
}
