//! The timeline's editable lanes (U.08, SPEC §5.3): a cut's own overlays (text hooks,
//! pills, run clocks), its gain segments (the gain-envelope lane) and the measurements it
//! shows (brackets), all on the output clock; and the drags that change them.
//!
//! A drag is a human gesture (R-GEST-1): pointer down holds the entity in the core
//! (`ProjectHandle::gesture_begin`, not a registered command, so no agent surface reaches
//! it) and agent writes to it meanwhile get `Conflict{by: human}`; the preview stays in
//! the UI; release commits **one** registry command (`overlay_set` / `gain_segment_set`,
//! one undo step) and ends the gesture in the same call; Esc ends it with no write.
//! Frames → anchors happen here (raw source-clock times, so a moved overlay follows its
//! words when the cut is re-trimmed); the UI does no model math (R-CMD-2).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gmclip_model::anchor::Anchor;
use gmclip_model::entities::{
    AssetOrSpan, Cut, GainSegment, MeasureEnd, MediaRef, Measurement, Overlay, OverlayEnd, OverlayKind,
    OverlayParent,
};
use gmclip_model::{Doc, Flicks, Id};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// Open timeline gestures: (project, gesture id, since). A gesture the UI never ended
/// (the page went away mid-drag) is ended after [`GESTURE_TTL`].
static OPEN: Mutex<Vec<(String, u64, Instant)>> = Mutex::new(Vec::new());
const GESTURE_TTL: Duration = Duration::from_secs(120);

/// `(flicks per output frame, output frames)`.
fn fpf_of(core: &AppCore, project: &str, cut: &str) -> AppResult<(i64, u64)> {
    let pl = core.planned(project, cut)?;
    Ok((crate::planner::fpf(&pl.plan), pl.plan.output.frames))
}

/// An anchor's output frame (nearest-earlier), when the cut shows it.
fn out_frame(doc: &Doc, cid: Id<Cut>, a: &Anchor, fpf: i64) -> Option<i64> {
    let r = gmclip_core::anchors::resolve(doc, Some(cid), a, "/").ok()?;
    // on the anchor's own media (a bare source time could land in another video)
    let t = r.cut_t?;
    Some(t.0 / fpf)
}

fn measure_frame(doc: &Doc, cid: Id<Cut>, e: &MeasureEnd, fpf: i64) -> Option<i64> {
    match e {
        MeasureEnd::Anchor(a) => out_frame(doc, cid, a, fpf),
        MeasureEnd::Frame(fr) => {
            let r = gmclip_core::anchors::resolve(doc, Some(cid), &Anchor::Frame(*fr), "/").ok()?;
            let t = r
                .cut_t
                .or_else(|| gmclip_core::anchors::to_cut(doc, cid, r.src, Some(fr.source)))?;
            Some(t.0 / fpf)
        }
        MeasureEnd::Event(_) => None,
    }
}

/// The cut's overlays, gain segments and measurements on the output clock.
/// `overlays_editable` / `gains_editable` are false for an imported RenderPlan (its text
/// and envelopes live in the base plan, not in model entities: the timeline shows the
/// plan's own, read-only).
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let (fpf, total) = fpf_of(core, project, cut)?;
    let overlays: Vec<Value> = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .filter(|o| o.parent == OverlayParent::Cut(cid))
        .filter_map(|o| {
            let text: String = o
                .text
                .as_ref()
                .map(|t| t.runs.iter().map(|r| r.text.as_str()).collect())
                .unwrap_or_default();
            let a = out_frame(&doc, cid, &o.start, fpf)?;
            let b = match &o.end {
                OverlayEnd::At(e) => out_frame(&doc, cid, e, fpf)?,
                OverlayEnd::DurMs(ms) => a + i64::from(*ms) * FLICKS / 1000 / fpf,
            };
            let b = b.min(total as i64);
            // decision 0042: what it is, and where it sits among the picture's layers
            let kind = match o.kind {
                OverlayKind::Text => "text",
                OverlayKind::Image => "image",
                OverlayKind::Video => "video",
                OverlayKind::Shape => "shape",
                OverlayKind::Counter => "counter",
            };
            let matte = o.video.as_ref().is_some_and(|v| {
                matches!(v.matte, Some(gmclip_model::entities::Matte::Roto { .. }))
            });
            // what it shows (the pieces of one cut overlay share it: one layer row)
            let media = match (&o.video, &o.image) {
                (Some(v), _) => match &v.media {
                    AssetOrSpan::Asset(x) => Some(x.0.to_string()),
                    AssetOrSpan::Span { media, .. } => Some(match media {
                        MediaRef::Source(s) => s.0.to_string(),
                        MediaRef::Group(g) => g.0.to_string(),
                    }),
                },
                (None, Some(im)) => Some(im.asset.0.to_string()),
                _ => None,
            };
            (b > a).then(|| {
                json!({"id": o.id.0.to_string(), "role": o.role, "text": text, "out": [a, b],
                       "counter": o.counter.is_some(), "kind": kind, "under": o.under,
                       "matte": matte, "hidden": o.opacity <= 0.0, "media": media})
            })
        })
        .collect();
    let imported = core.imported(project, cut).is_ok();
    // the picture's layers by z (the first part's role for each), for "behind <role>"
    let mut picture_layers: Vec<(i64, String)> = Vec::new();
    if overlays
        .iter()
        .any(|o| matches!(o["kind"].as_str(), Some("image" | "video")))
        && let Ok(pl) = core.planned(project, cut)
    {
        for l in pl.plan.parts.iter().flat_map(|p| &p.layers) {
            if l.cover.is_none() && !picture_layers.iter().any(|(z, _)| *z == l.z) {
                picture_layers.push((l.z, l.role.as_str().to_owned()));
            }
        }
        picture_layers.sort();
    }
    // An imported RenderPlan's envelopes are the plan's (read-only, like its text).
    let plan_gains: Vec<Value> = if imported {
        let pl = core.planned(project, cut)?;
        // output sample → frame: 14 700 flicks a sample
        let fpf_u = fpf.max(1) as u64;
        let frame_of = |s: u64| s * (FLICKS as u64 / 48_000) / fpf_u;
        let frame_end = |s: u64| (s * (FLICKS as u64 / 48_000)).div_ceil(fpf_u);
        pl.plan
            .audio
            .envelopes
            .iter()
            .flatten()
            .map(|e| {
                json!({"id": e.id.as_str(), "gain_db": e.gain_db,
                       "out": [frame_of(e.out_samples.start()), frame_end(e.out_samples.end())],
                       "ramp_in_ms": e.ramp_in_samples / 48, "ramp_out_ms": e.ramp_out_samples / 48})
            })
            .collect()
    } else {
        Vec::new()
    };
    let gains: Vec<Value> = doc
        .children::<GainSegment>(cid.entity())
        .filter_map(|g| doc.get(g))
        .filter_map(|g| {
            let a = out_frame(&doc, cid, &g.start, fpf)?;
            let b = out_frame(&doc, cid, &g.end, fpf)?;
            (b > a).then(|| {
                json!({"id": g.id.0.to_string(), "gain_db": g.gain_db, "out": [a, b],
                       "ramp_in_ms": g.ramp_in_ms, "ramp_out_ms": g.ramp_out_ms})
            })
        })
        .chain(plan_gains)
        .collect();
    let measurements: Vec<Value> = doc
        .iter::<Measurement>()
        .filter_map(|m| {
            let a = measure_frame(&doc, cid, &m.from, fpf)?;
            let b =
                m.to.as_ref()
                    .and_then(|e| measure_frame(&doc, cid, e, fpf))?;
            (b > a).then(|| {
                json!({"id": m.id.0.to_string(), "value": m.value, "unit": m.unit,
                       "method": m.method, "out": [a, b],
                       "evidence": m.evidence_frames.len()})
            })
        })
        .collect();
    Ok(json!({
        "overlays_editable": !imported,
        "gains_editable": !imported,
        "overlays": overlays,
        "picture_layers": picture_layers
            .iter()
            .map(|(z, role)| json!({"z": z, "role": role}))
            .collect::<Vec<_>>(),
        "gains": gains,
        "measurements": measurements,
    }))
}

/// Pointer down on an overlay or a gain segment: holds it in the core until
/// [`gesture_end`] (or the commit that ends it).
pub fn gesture_begin(core: &AppCore, project: &str, kind: &str, id: &str) -> AppResult<Value> {
    gesture_begin_with(core, project, kind, id, None)
}

/// [`gesture_begin`] for every gizmo (U.12): `overlay` and `gain` (the lanes), `layer`
/// (a crop drag: the model layer, and `part` when given, whose look holds the crop),
/// `framing` (the caption seat) and `part` (one part's seat).
pub fn gesture_begin_with(
    core: &AppCore,
    project: &str,
    kind: &str,
    id: &str,
    part: Option<&str>,
) -> AppResult<Value> {
    use gmclip_model::entities::{Framing, Layer, Part};
    let mut entities = vec![match kind {
        "overlay" => ulid_id::<Overlay>(id)?.entity(),
        "gain" => ulid_id::<GainSegment>(id)?.entity(),
        "layer" => ulid_id::<Layer>(id)?.entity(),
        "framing" => ulid_id::<Framing>(id)?.entity(),
        "part" => ulid_id::<Part>(id)?.entity(),
        _ => {
            return Err(AppError::invalid(
                "`kind` is overlay, gain, layer, framing or part",
            ));
        }
    }];
    if let Some(p) = part {
        entities.push(ulid_id::<Part>(p)?.entity());
    }
    let p = core.project(project)?;
    // a gesture the page never ended (it went away mid-drag) is let go first
    if let Ok(mut open) = OPEN.lock() {
        open.retain(|(pr, g, at)| {
            let stale = at.elapsed() >= GESTURE_TTL;
            if stale && let Ok(x) = core.project(pr) {
                x.handle.gesture_end(*g);
            }
            !stale
        });
    }
    let g = p.handle.gesture_begin(entities).map_err(AppError::from)?;
    if let Ok(mut open) = OPEN.lock() {
        open.push((project.to_owned(), g, Instant::now()));
    }
    Ok(json!({"gesture": g}))
}

/// Ends a gesture (Esc, a click that moved nothing, or after its commit).
pub fn gesture_end(core: &AppCore, project: &str, gesture: u64) {
    if let Ok(mut open) = OPEN.lock() {
        open.retain(|(pr, g, _)| !(pr == project && *g == gesture));
    }
    if let Ok(p) = core.project(project) {
        p.handle.gesture_end(gesture);
    }
}

/// A raw source-clock anchor for the start of output frame `f`.
fn start_anchor(doc: &Doc, cid: Id<Cut>, cut: &str, f: i64, fpf: i64) -> AppResult<Value> {
    if f <= 0 {
        return Ok(json!({"cut_start": {"cut": cut}}));
    }
    let (_, t, media) = crate::sound::at_frame(doc, cid, f, fpf)?;
    Ok(crate::sound::raw(media, t, "placed on the timeline"))
}

/// A raw source-clock anchor for the end of output frame `f − 1` (the exclusive end
/// `f`), on the clock of the part that shows frame `f − 1`; the cut's end at its last
/// frame.
fn end_anchor(
    doc: &Doc,
    cid: Id<Cut>,
    cut: &str,
    f: i64,
    fpf: i64,
    total: u64,
) -> AppResult<Value> {
    if f >= total as i64 {
        return Ok(json!({"cut_end": {"cut": cut}}));
    }
    let (_, t, media) = crate::sound::at_frame(doc, cid, (f - 1).max(0), fpf)?;
    Ok(crate::sound::raw(
        media,
        Flicks(t.0 + fpf),
        "placed on the timeline",
    ))
}

fn checked_span(from: Option<i64>, to: Option<i64>, total: u64) -> AppResult<()> {
    let t = total as i64;
    if from.is_some_and(|a| a < 0 || a >= t) || to.is_some_and(|b| b <= 0 || b > t) {
        return Err(AppError::invalid("the span is outside the cut"));
    }
    if let (Some(a), Some(b)) = (from, to)
        && b <= a
    {
        return Err(AppError::invalid("the end is before the start"));
    }
    Ok(())
}

/// Re-times an overlay to output frames `[from, to)` (either side alone for an edge
/// drag) as one `overlay_set`, then ends `gesture`.
pub fn overlay_time(
    core: &AppCore,
    project: &str,
    cut: &str,
    overlay: &str,
    from: Option<i64>,
    to: Option<i64>,
    gesture: Option<u64>,
) -> AppResult<Value> {
    let r = (|| {
        let (fpf, total) = fpf_of(core, project, cut)?;
        checked_span(from, to, total)?;
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = ulid_id::<Cut>(cut)?;
        ulid_id::<Overlay>(overlay)?;
        let mut args = json!({"overlay": overlay});
        if let Some(a) = from {
            args["start"] = start_anchor(&doc, cid, cut, a, fpf)?;
        }
        if let Some(b) = to {
            args["end"] = json!({"at": end_anchor(&doc, cid, cut, b, fpf, total)?});
        }
        core.call(project, "overlay_set", args)
    })();
    if let Some(g) = gesture {
        gesture_end(core, project, g);
    }
    r
}

/// Cuts an overlay at output frame `f` (core `overlay_split`): on an edge between two
/// parts each piece anchors to its own part; inside a part, to the source time there.
pub fn overlay_split_at(
    core: &AppCore,
    project: &str,
    cut: &str,
    overlay: &str,
    f: i64,
) -> AppResult<Value> {
    let (fpf, total) = fpf_of(core, project, cut)?;
    if f <= 0 || f >= total as i64 {
        return Err(AppError::invalid("the frame is outside the cut"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    ulid_id::<Overlay>(overlay)?;
    let at = start_anchor(&doc, cid, cut, f, fpf)?;
    core.call(project, "overlay_split", json!({"overlay": overlay, "at": at}))
}

/// Changes a gain segment's span (output frames) and/or gain as one `gain_segment_set`,
/// then ends `gesture`.
#[allow(clippy::too_many_arguments)]
pub fn gain_set(
    core: &AppCore,
    project: &str,
    cut: &str,
    segment: &str,
    from: Option<i64>,
    to: Option<i64>,
    gain_db: Option<f64>,
    gesture: Option<u64>,
) -> AppResult<Value> {
    let r = (|| {
        let (fpf, total) = fpf_of(core, project, cut)?;
        checked_span(from, to, total)?;
        if gain_db.is_some_and(|g| !(-40.0..=24.0).contains(&g)) {
            return Err(AppError::invalid("a gain from −40 to +24 dB"));
        }
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = ulid_id::<Cut>(cut)?;
        ulid_id::<GainSegment>(segment)?;
        let mut args = json!({"segment": segment});
        if let Some(a) = from {
            let (_, t, media) = crate::sound::at_frame(&doc, cid, a, fpf)?;
            args["start"] = crate::sound::raw(media, t, "gain from here");
        }
        if let Some(b) = to {
            let (_, t, media) = crate::sound::at_frame(&doc, cid, (b - 1).max(0), fpf)?;
            args["end"] = crate::sound::raw(media, Flicks(t.0 + fpf), "gain to here");
        }
        if let Some(g) = gain_db {
            args["gain_db"] = json!((g * 10.0).round() / 10.0);
        }
        core.call(project, "gain_segment_set", args)
    })();
    if let Some(g) = gesture {
        gesture_end(core, project, g);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_checked() {
        assert!(checked_span(Some(0), Some(30), 60).is_ok());
        assert!(checked_span(None, Some(60), 60).is_ok());
        assert!(checked_span(Some(-1), None, 60).is_err());
        assert!(checked_span(Some(60), None, 60).is_err());
        assert!(checked_span(None, Some(61), 60).is_err());
        assert!(checked_span(Some(30), Some(30), 60).is_err());
    }
}
