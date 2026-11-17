//! The story panel's data (SPEC §5.2.1, backlog U.07): brief, part rows with typed source
//! in/out and nudges, reorder, the duration chip by the 9:16 band, and the cut's story
//! suggestions. Times the human types (`12:34`, `1:02:03.5`, `754.2`) are parsed here and
//! land on the frame grid (nearest frame, ties to the earlier, SPEC §4.1.1); the UI does
//! no time arithmetic (R-CMD-2).

use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{Cut, Part, StorySuggestion};
use gmclip_model::{Flicks, Id};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, frame_of, time_of, ulid_id};

/// Parses `h:mm:ss(.f)`, `m:ss(.f)` or seconds into flicks.
pub fn parse_time(text: &str) -> Option<Flicks> {
    let t = text.trim();
    if t.is_empty() || t.len() > 32 {
        return None;
    }
    let mut parts: Vec<&str> = t.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let secs: f64 = parts.pop()?.parse().ok()?;
    if !(0.0..60.0).contains(&secs) && !parts.is_empty() {
        return None;
    }
    let mut total = secs;
    let mut mul = 60.0;
    while let Some(p) = parts.pop() {
        let v: u32 = p.parse().ok()?;
        if mul == 60.0 && !parts.is_empty() && v >= 60 {
            return None;
        }
        total += f64::from(v) * mul;
        mul *= 60.0;
    }
    (total.is_finite() && (0.0..30.0 * 86_400.0).contains(&total))
        .then(|| Flicks((total * FLICKS as f64).round() as i64))
}

/// `h:mm:ss.cc` / `m:ss.cc` for a source time.
pub fn fmt_time(f: Flicks) -> String {
    let cs = f.0 * 100 / FLICKS;
    let (h, m, s, c) = (cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{c:02}")
    } else {
        format!("{m}:{s:02}.{c:02}")
    }
}

fn fps_of(core: &AppCore, project: &str, cut: &str) -> (i64, i64) {
    core.planned(project, cut)
        .map_or((30, 1), |pl| crate::planner::fps(&pl.plan))
}

/// The panel's rows.
pub fn story_view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let c = doc
        .get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let mut total = 0i64;
    let parts: Vec<Value> = cut_layout(&doc, cid)
        .iter()
        .enumerate()
        .filter_map(|(n, sp)| {
            let pt = doc.get(sp.part)?;
            total += sp.dur.0;
            Some(json!({
                "n": n + 1,
                "id": pt.id.0.to_string(),
                "role": serde_json::to_value(pt.role).unwrap_or(Value::Null),
                "in": fmt_time(pt.src_in),
                "out": fmt_time(pt.src_out),
                "dur_s": (sp.dur.0 as f64 / FLICKS as f64 * 100.0).round() / 100.0,
            }))
        })
        .collect();
    let secs = total as f64 / FLICKS as f64;
    // The 9:16 band with no kit (SPEC §5.2.1): green 20–100 s, red below, gold above.
    let band = if secs < 20.0 {
        "below"
    } else if secs > 100.0 {
        "above"
    } else {
        "inside"
    };
    // each suggestion's span (source seconds), whether a part plays it, its arc place
    let snap = p.handle.snapshot();
    let view = &snap.analysis;
    let in_cut: Vec<(i64, i64, Option<Id<StorySuggestion>>)> = cut_layout(&doc, cid)
        .iter()
        .filter_map(|sp| doc.get(sp.part))
        .map(|pt| (pt.src_in.0, pt.src_out.0, pt.lineage.suggestion))
        .collect();
    let tenth = FLICKS / 10;
    let suggestions: Vec<Value> = doc
        .children::<StorySuggestion>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|s| {
            let at = |a| {
                gmclip_core::anchors::resolve_in(&doc, Some(view), Some(cid), a, "")
                    .ok()
                    .map(|r| r.src.0)
            };
            let (a, b) = (at(&s.span.start), at(&s.span.end));
            let played = in_cut.iter().any(|(i, o, from)| {
                *from == Some(s.id)
                    || matches!((a, b), (Some(a), Some(b)) if (i - a).abs() <= tenth && (o - b).abs() <= tenth)
            });
            json!({
                "id": s.id.0.to_string(),
                "role": serde_json::to_value(s.role).unwrap_or(Value::Null),
                "relevance": s.relevance,
                "quote": s.quote.chars().take(400).collect::<String>(),
                "state": serde_json::to_value(s.state).unwrap_or(Value::Null),
                "in_s": a.map(|x| (x as f64 / FLICKS as f64 * 100.0).round() / 100.0),
                "out_s": b.map(|x| (x as f64 / FLICKS as f64 * 100.0).round() / 100.0),
                "in": a.map(|x| fmt_time(Flicks(x))),
                "in_cut": played,
                "arc": c.story_arc.iter().position(|x| *x == s.id),
            })
        })
        .collect();
    Ok(json!({
        "brief": c.brief,
        "notes": c.notes.chars().take(20_000).collect::<String>(),
        "parts": parts,
        "duration_s": (secs * 10.0).round() / 10.0,
        "band": band,
        "band_range": [20, 100],
        "suggestions": suggestions,
        "arc_len": c.story_arc.len(),
    }))
}

/// Sets a part's in or out from a typed source time.
pub fn set_time(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    edge: &str,
    text: &str,
) -> AppResult<Value> {
    let t = parse_time(text)
        .ok_or_else(|| AppError::invalid("type a time like 12:34, 1:02:03.5 or 754.2"))?;
    let fps = fps_of(core, project, cut);
    let t = time_of(frame_of(t, fps).unwrap_or(0), fps);
    let args = match edge {
        "in" => json!({"part": part, "src_in": t.0}),
        "out" => json!({"part": part, "src_out": t.0}),
        _ => return Err(AppError::invalid("edge is in or out")),
    };
    core.call(project, "part_trim", args)
}

/// Nudges an edge by `delta_ms` (‹ › 0.5 s, ⇧ 0.1 s), rounded to whole frames (≥ 1).
pub fn nudge_ms(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    edge: &str,
    delta_ms: i64,
) -> AppResult<Value> {
    let fps = fps_of(core, project, cut);
    let frames = (delta_ms * fps.0 + 500 * fps.1 * delta_ms.signum()) / (1000 * fps.1);
    let frames = if frames == 0 {
        delta_ms.signum()
    } else {
        frames
    };
    core.trim_by(project, cut, part, edge, frames)
}

/// Moves a part one place up or down.
pub fn move_part(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    dir: &str,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let order: Vec<Id<Part>> = cut_layout(&doc, cid).iter().map(|s| s.part).collect();
    let me: Id<Part> = ulid_id(part)?;
    let k = order
        .iter()
        .position(|x| *x == me)
        .ok_or_else(|| AppError::not_found("no such part"))?;
    let after = match dir {
        "up" if k >= 2 => Some(order[k - 2]),
        "up" if k == 1 => None,
        "down" if k + 1 < order.len() => Some(order[k + 1]),
        "up" | "down" => return Ok(json!({"moved": false})),
        _ => return Err(AppError::invalid("dir is up or down")),
    };
    let mut args = json!({"part": part});
    if let Some(a) = after {
        args["after"] = json!(a.0.to_string());
    }
    core.call(project, "part_reorder", args)
}

/// Orders the parts by source time (⏱ chronological), as one transaction (`cut_edit`).
pub fn chronological(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let mut parts: Vec<&Part> = cut_layout(&doc, cid)
        .iter()
        .filter_map(|s| doc.get(s.part))
        .collect();
    parts.sort_by_key(|p| p.src_in);
    let mut ops = Vec::new();
    let mut after: Option<String> = None;
    for pt in parts {
        let mut op = json!({"op": "part_reorder", "part": pt.id.0.to_string()});
        if let Some(a) = &after {
            op["after"] = json!(a);
        }
        ops.push(op);
        after = Some(pt.id.0.to_string());
    }
    core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_times() {
        let s = |x: f64| Flicks((x * FLICKS as f64).round() as i64);
        assert_eq!(parse_time("12:34"), Some(s(754.0)));
        assert_eq!(parse_time("1:02:03.5"), Some(s(3723.5)));
        assert_eq!(parse_time("754.2"), Some(s(754.2)));
        assert_eq!(parse_time("1:60"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(parse_time("-3"), None);
        assert_eq!(parse_time("abc"), None);
        assert_eq!(fmt_time(s(3723.5)), "1:02:03.50");
        assert_eq!(fmt_time(s(754.0)), "12:34.00");
    }
}
