//! Viewer gizmos the render thread does not draw (U.12, SPEC §7 gizmos): the text
//! overlays on the current frame (a hook, a pill, a run clock: drag to move) and the
//! caption seat (drag the captions up or down). The boxes come from the same text layout
//! the compositor rasterises (`TextRaster::geometry` on the plan's own events and fonts),
//! so a box sits on the glyphs it moves.
//!
//! A drag is a human gesture (R-GEST-1): pointer down holds the overlay (or the framing,
//! or the part for a one-part seat) through [`crate::lanes::gesture_begin_with`]; agent
//! writes to it meanwhile get `Conflict{by: human}` (R-CC-2); release commits **one**
//! registry command (`overlay_move`, `layout_edit` framing_set, `part_caption_place`) and
//! ends the gesture in the same call; Esc ends it with nothing written. While it drags the
//! frame itself previews the move: the UI hands the render thread the text events' role,
//! anchor and offset (`viewer_text_drag`), and the shared compositor draws them there.

use std::sync::Mutex;

use gmclip_model::entities::{Cut, Framing, Overlay, OverlayGeom, OverlayKind, Part};
use gmclip_model::geometry::Place;
use gmclip_plan::types::{Plan, TextEvent, TextRole};
use gmclip_render::text::font::FontFace;
use gmclip_render::text::raster::TextRaster;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The raster the boxes are measured with, kept for the plan's font files.
static RASTER: Mutex<Option<(Vec<String>, TextRaster)>> = Mutex::new(None);

/// `[x, y, w, h]` canvas px of an event's ink (outline, box and fill), as drawn.
fn event_box(plan: &Plan, ev: &TextEvent) -> Option<[f64; 4]> {
    let style = plan.text_styles.as_ref()?.get(&ev.style)?;
    let fonts: Vec<String> = plan
        .fonts
        .iter()
        .flatten()
        .map(|f| format!("{}={}", f.id.as_str(), f.path))
        .collect();
    let mut g = RASTER.lock().ok()?;
    if g.as_ref().is_none_or(|(k, _)| *k != fonts) {
        let mut r = TextRaster::new();
        for f in plan.fonts.iter().flatten() {
            let face = FontFace::open(std::path::Path::new(&f.path)).ok()?;
            r.add_font(f.id.as_str(), face).ok()?;
        }
        *g = Some((fonts, r));
    }
    let (_, raster) = g.as_mut()?;
    let geo = raster.geometry(ev, style).ok()?;
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let pts = geo
        .boxes
        .iter()
        .flatten()
        .chain(geo.outline.iter().flat_map(|(_, c)| c.iter().flatten()))
        .chain(geo.fills.iter().flat_map(|(_, c)| c.iter().flatten()));
    for p in pts {
        b = [
            b[0].min(p[0]),
            b[1].min(p[1]),
            b[2].max(p[0]),
            b[3].max(p[1]),
        ];
    }
    (b[0] <= b[2]).then(|| [b[0], b[1], b[2] - b[0], b[3] - b[1]])
}

fn role_name(r: TextRole) -> &'static str {
    gmclip_core::planner::text_role_name(&r)
}

fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The model overlay a plan text event was planned from: same role, same place in this
/// framing (to the pixel), and the same words (a run clock's label is computed).
fn overlay_of<'a>(
    ovs: &[&'a Overlay],
    ev: &TextEvent,
    framing: gmclip_model::Id<Framing>,
    canvas: (f64, f64),
) -> Option<&'a Overlay> {
    let role = role_name(ev.role);
    let words = plain(&ev.runs.iter().map(|r| r.text.as_str()).collect::<String>());
    let at = |o: &Overlay| match gmclip_model::effective::effective_overlay_place(o, framing).1 {
        OverlayGeom::Place(p) => {
            (p.x * canvas.0 - ev.pos.x).abs() < 0.5 && (p.y * canvas.1 - ev.pos.y).abs() < 0.5
        }
        OverlayGeom::Box(_) => false,
    };
    let same_text = |o: &Overlay| {
        ev.role == TextRole::RunClock
            || o.counter.is_some()
            || o.text.as_ref().is_some_and(|t| {
                plain(&t.runs.iter().map(|r| r.text.as_str()).collect::<String>()) == words
            })
    };
    ovs.iter()
        .copied()
        .filter(|o| o.role.as_deref() == Some(role) && at(o))
        .find(|o| same_text(o))
}

/// What can be dragged on output frame `frame`: `texts` (overlay id, role, label, its
/// box, its place and its events' anchor `at`), and `caption` (the seat: y in canvas px, the part showing, and the
/// box of the cue on screen when there is one). Imported cuts (whose overlays are the
/// golden render's own) list no texts.
pub fn view(core: &AppCore, project: &str, cut: &str, frame: u64) -> AppResult<Value> {
    let pl = core.planned(project, cut)?;
    let plan = &pl.plan;
    let (cw, ch) = (
        plan.output.canvas.w.get() as f64,
        plan.output.canvas.h.get() as f64,
    );
    let imported = core.imported(project, cut).is_ok();
    let framing = core.active_framing(project, cut);
    let snap = core.project(project)?.handle.snapshot();
    let doc = &snap.doc;
    let cid = ulid_id::<Cut>(cut)?;
    let ovs: Vec<&Overlay> = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .filter(|o| o.kind == OverlayKind::Text)
        .collect();
    let on = |e: &TextEvent| e.out.start() <= frame && frame < e.out.end();
    let part_ix = plan
        .parts
        .iter()
        .position(|p| p.out.start() <= frame && frame < p.out.end());
    let model_part = part_ix
        .and_then(|i| {
            pl.index
                .parts
                .iter()
                .find(|x| x.plan_id == plan.parts[i].id.as_str())
        })
        .map(|x| x.model.clone());
    let mut texts = Vec::new();
    let mut cue: Option<&TextEvent> = None;
    for ev in plan.text_events.iter().flatten() {
        if ev.role == TextRole::Caption {
            if on(ev) {
                cue = Some(ev);
            }
            continue;
        }
        if !on(ev) || imported {
            continue;
        }
        let Some(fid) = framing.as_deref().and_then(|f| ulid_id::<Framing>(f).ok()) else {
            continue;
        };
        let Some(o) = overlay_of(&ovs, ev, fid, (cw, ch)) else {
            continue;
        };
        let Some(b) = event_box(plan, ev) else {
            continue;
        };
        let OverlayGeom::Place(p) = gmclip_model::effective::effective_overlay_place(o, fid).1
        else {
            continue;
        };
        texts.push(json!({
            "overlay": o.id.0.to_string(),
            "role": role_name(ev.role),
            "label": ev.runs.iter().map(|r| r.text.as_str()).collect::<String>().replace('\n', " "),
            "box": b,
            "place": {"x": p.x, "y": p.y, "w": p.w},
            // the events' anchor (canvas px): a drag previews every event of this role
            // anchored here (`viewer_text_drag`)
            "at": [ev.pos.x, ev.pos.y],
        }));
    }
    // the added videos and images on this frame (AJ 2026-10-19: "position other layers
    // easily"): their box and place, matched to their overlay by its place in this framing
    let mut pictures = Vec::new();
    if let (false, Some(fid)) = (
        imported,
        framing.as_deref().and_then(|f| ulid_id::<Framing>(f).ok()),
    ) {
        let pics: Vec<&Overlay> = doc
            .children::<Overlay>(cid.entity())
            .filter_map(|id| doc.get(id))
            .filter(|o| matches!(o.kind, OverlayKind::Image | OverlayKind::Video))
            .collect();
        let events = plan
            .clip_events
            .iter()
            .flatten()
            .map(|e| ("video", e.out, e.place))
            .chain(
                plan.image_events
                    .iter()
                    .flatten()
                    .map(|e| ("image", e.out, e.place)),
            );
        for (kind, out, pl) in events {
            if !(out.start() <= frame && frame < out.end()) {
                continue;
            }
            let found = pics.iter().find_map(|o| {
                let OverlayGeom::Place(p) =
                    gmclip_model::effective::effective_overlay_place(o, fid).1
                else {
                    return None;
                };
                ((p.x * cw - pl.x).abs() < 0.5
                    && (p.y * ch - pl.y).abs() < 0.5
                    && (p.w * cw - pl.w).abs() < 0.5)
                    .then_some((o, p))
            });
            if let Some((o, p)) = found {
                pictures.push(json!({
                    "overlay": o.id.0.to_string(),
                    "kind": kind,
                    "box": [pl.x, pl.y, pl.w, pl.h],
                    "place": {"x": p.x, "y": p.y, "w": p.w},
                }));
            }
        }
    }
    // the seat: the cue on screen, else the first cue of this part, else of the cut
    let caps = || {
        plan.text_events
            .iter()
            .flatten()
            .filter(|e| e.role == TextRole::Caption)
    };
    let seat = cue
        .or_else(|| {
            let i = part_ix?;
            let r = &plan.parts[i].out;
            caps().find(|e| e.out.start() >= r.start() && e.out.start() < r.end())
        })
        .or_else(|| caps().next());
    let caption = seat.map(|e| {
        json!({
            "y": e.pos.y,
            "box": cue.and_then(|c| event_box(plan, c)),
            "part": model_part,
            "editable": !imported && framing.is_some(),
        })
    });
    Ok(json!({
        "frame": frame,
        "canvas": {"w": cw, "h": ch},
        "texts": texts,
        "pictures": pictures,
        "caption": caption,
    }))
}

/// Ends the gesture after `f` (its commit, or its failure).
fn ending<T>(core: &AppCore, project: &str, gesture: Option<u64>, r: AppResult<T>) -> AppResult<T> {
    if let Some(g) = gesture {
        crate::lanes::gesture_end(core, project, g);
    }
    r
}

/// Moves a text overlay to `(x, y)` (fractions of the canvas; its width kept) in the
/// framing shown, as one `overlay_move`, then ends `gesture`.
pub fn overlay_drag(
    core: &AppCore,
    project: &str,
    cut: &str,
    overlay: &str,
    xy: (f64, f64),
    gesture: Option<u64>,
) -> AppResult<Value> {
    let r = (|| {
        let (x, y) = xy;
        if !(x.is_finite()
            && y.is_finite()
            && (-0.5..=1.5).contains(&x)
            && (-0.5..=1.5).contains(&y))
        {
            return Err(AppError::invalid("a place on or near the frame"));
        }
        let framing = core
            .active_framing(project, cut)
            .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
        let fid = ulid_id::<Framing>(&framing)?;
        let oid = ulid_id::<Overlay>(overlay)?;
        let snap = core.project(project)?.handle.snapshot();
        let o = snap
            .doc
            .get(oid)
            .ok_or_else(|| AppError::not_found("that overlay is not in the cut any more"))?;
        let OverlayGeom::Place(p) = gmclip_model::effective::effective_overlay_place(o, fid).1
        else {
            return Err(AppError::invalid("a box overlay moves by its numbers"));
        };
        core.call(
            project,
            "overlay_move",
            json!({"overlay": overlay, "framing": framing,
                   "place": {"x": x, "y": y, "w": p.w}}),
        )
    })();
    ending(core, project, gesture, r)
}

/// Seats the captions at `y_px` (canvas px): the framing's seat, or with `part` only that
/// part's (its override), keeping the seat's x and width; one command, then ends `gesture`.
pub fn caption_seat(
    core: &AppCore,
    project: &str,
    cut: &str,
    y_px: f64,
    part: Option<&str>,
    gesture: Option<u64>,
) -> AppResult<Value> {
    let r = (|| {
        let Some(part) = part else {
            return crate::short::caption_y_set(core, project, cut, y_px);
        };
        let framing = core
            .active_framing(project, cut)
            .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
        let pl = core.planned(project, cut)?;
        let ch = pl.plan.output.canvas.h.get() as f64;
        if !y_px.is_finite() || y_px <= 0.0 || y_px >= ch {
            return Err(AppError::invalid("a height inside the frame, in pixels"));
        }
        let snap = core.project(project)?.handle.snapshot();
        let seat = gmclip_model::effective::effective_caption_place(
            &snap.doc,
            ulid_id::<Part>(part)?,
            ulid_id::<Framing>(&framing)?,
            None,
        )
        .unwrap_or(Place {
            x: 0.5,
            y: 0.55,
            w: 0.9,
        });
        core.call(
            project,
            "part_caption_place",
            json!({"part": part, "framing": framing,
                   "place": {"x": seat.x, "y": y_px / ch, "w": seat.w}}),
        )
    })();
    ending(core, project, gesture, r)
}
