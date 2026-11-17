//! Tap marking while watching (L.04): `,` drops an in, `.` an out, live, in the cut view
//! and in review mode, at any playback speed. The pair becomes a moment by the human
//! (origin `manual`), its edges snapped to the nearest sentence start and end as
//! human-typed times are (SPEC §5.2): a tap a beat late (or early) lands on the sentence
//! it meant, which also absorbs the preview's latency at 2×.
//!
//! The UI sends the output frames it showed; this maps them to the media clock through
//! the cut's parts (or the review span's start) — the UI does no model math (R-CMD-2).

use gmclip_model::Flicks;
use gmclip_model::entities::{Cut, MediaRef, Part};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// `(media, media time)` an output frame of a cut shows.
fn cut_frame_src(
    core: &AppCore,
    project: &str,
    cut: &str,
    frame: u64,
) -> AppResult<(MediaRef, i64)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let t = i64::try_from(frame).unwrap_or(i64::MAX / 2 / fpf.max(1)) * fpf;
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    let sp = layout
        .iter()
        .find(|s| s.start.0 <= t && t < s.start.0 + s.dur.0)
        .or(layout.last())
        .ok_or_else(|| AppError::invalid("the cut plays nothing"))?;
    let pt: &Part = doc
        .get(sp.part)
        .ok_or_else(|| AppError::not_found("that part is gone"))?;
    let local = (t - sp.start.0).clamp(0, sp.dur.0);
    let src = gmclip_model::clock::local_to_src(pt, gmclip_model::Flicks(local)).0;
    Ok((pt.media, src.min(pt.src_out.0)))
}

/// `(media, media time)` an output frame of a moment's review plays.
fn review_frame_src(
    core: &AppCore,
    project: &str,
    moment: &str,
    frame: u64,
) -> AppResult<(MediaRef, i64)> {
    let p = core.project(project)?;
    let (plan, (a0, _)) = crate::review::review_for(&p, &core.transcripts, moment)?;
    let fps = plan.output.fps.as_f64();
    let doc = p.handle.snapshot().doc.clone();
    let m = doc
        .get(ulid_id::<gmclip_model::entities::Moment>(moment)?)
        .ok_or_else(|| AppError::not_found("no such moment"))?;
    let t = a0 + frame as f64 / fps.max(1.0);
    Ok((m.media, (t * FLICKS as f64).round() as i64))
}

/// Makes the tapped in/out a moment (sentence edges, origin manual).
pub fn mark(core: &AppCore, project: &str, a: &Value) -> AppResult<Value> {
    let f = |k: &str| {
        a.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is a frame")))
    };
    let (fi, fo) = (f("in_frame")?, f("out_frame")?);
    if fo <= fi {
        return Err(AppError::invalid("the out comes after the in"));
    }
    let at = |fr: u64| match (
        a.get("cut").and_then(Value::as_str),
        a.get("moment").and_then(Value::as_str),
    ) {
        (Some(c), _) => cut_frame_src(core, project, c, fr),
        (None, Some(m)) => review_frame_src(core, project, m, fr),
        _ => Err(AppError::invalid("`cut` or `moment` is required")),
    };
    let ((mi, ti), (mo, to)) = (at(fi)?, at(fo)?);
    if mi != mo {
        return Err(AppError::invalid(
            "the in and the out are on different recordings",
        ));
    }
    if to <= ti {
        return Err(AppError::invalid(
            "the out comes after the in on the recording",
        ));
    }
    let secs = |t: i64| Flicks(t).0 as f64 / FLICKS as f64;
    let r = core.call(
        project,
        "moment_mark",
        json!({"media": mi, "start_s": secs(ti), "end_s": secs(to), "snap": "sentence", "from": "tap",
               "note": format!("{:.1}–{:.1} s tapped", secs(ti), secs(to))}),
    )?;
    Ok(json!({"moment": r["output"], "tapped": [secs(ti), secs(to)]}))
}
