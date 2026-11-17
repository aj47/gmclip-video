//! Draw your layout once (SPEC §5.7.2; Ideas 2026-10-16, K.01).
//!
//! On one frame of a video ("different frame" picks another), the human drags and names
//! up to eight boxes — **screen**, **camera**, **chat**, **private** (and alerts) — and
//! saves them as the video's stream layout (`source_stream_layout_set`, one undoable
//! step per video). From there:
//!
//! - every new cut seeded from that video takes its face from the camera box and its
//!   panel from the screen box (A2.20 seeding already reads the stream layout);
//! - chat, alerts and private boxes are private-class regions, so the private-region lint
//!   and its "Cover chat and alerts" fix see them;
//! - "also on the other videos of this size" gives the same layout to the project's other
//!   videos whose picture is the same size, in the same call;
//! - "save as a preset" keeps it for the active profile in the app data beside the layout
//!   presets (the profile folder is content gmclip never writes), so the sheet offers it
//!   as a starting point on any video of that size, and the 9:16 layout presets list it.
//!
//! The frame comes from the video file itself (one decode, ~960 px wide).

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_model::entities::{RegionRole, Source, StreamLayout, StreamRegion, TrackKind};
use gmclip_model::{Doc, Id};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The frame's width (px): enough to draw on in the sheet.
pub const FRAME_W: u32 = 960;

/// The roles the sheet offers, in its order, with the words it shows.
pub const ROLES: &[(RegionRole, &str)] = &[
    (RegionRole::Screen, "Screen"),
    (RegionRole::Cam, "Camera"),
    (RegionRole::Chat, "Chat"),
    (RegionRole::Private, "Private"),
    (RegionRole::Alerts, "Alerts"),
];

fn source<'a>(doc: &'a Doc, id: &str) -> AppResult<&'a Source> {
    let sid: Id<Source> = ulid_id(id)?;
    doc.get(sid)
        .ok_or_else(|| AppError::not_found("no such video in this project"))
}

/// The picture's `(w, h, fps)` from the probe.
fn picture(s: &Source) -> Option<(u32, u32, f64)> {
    let v = s.probe.tracks.iter().find(|t| t.kind == TrackKind::Video)?;
    let fps = v
        .fps
        .map(|r| r.num as f64 / r.den.max(1) as f64)
        .filter(|f| *f > 0.0)
        .unwrap_or(30.0);
    Some((v.width?, v.height?, fps))
}

fn regions_json(rs: &[StreamRegion]) -> Value {
    json!(
        rs.iter()
            .map(|r| json!({"role": r.role, "rect": r.rect}))
            .collect::<Vec<_>>()
    )
}

// ---- presets (per profile, in the app data) ------------------------------------------

fn store_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("stream-layout-presets.json")
}

type Store = BTreeMap<String, Vec<StreamLayout>>;

fn load_store(core: &AppCore) -> Store {
    std::fs::read(store_path(core))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_store(core: &AppCore, s: &Store) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&store_path(core), &bytes)
        .map_err(|e| AppError::internal(format!("saving the layout preset: {}", e.kind())))
}

/// The stream layouts drawn and saved as presets under the active profile (as the
/// profile's own `stream-layouts.json` presets are read: [`crate::takelayout`]).
pub fn saved_presets(core: &AppCore) -> Vec<gmclip_core::profile::layouts::LayoutPreset> {
    load_store(core)
        .remove(&crate::takelayout::profile_key(core))
        .unwrap_or_default()
        .into_iter()
        .map(|layout| gmclip_core::profile::layouts::LayoutPreset {
            layout,
            note: String::new(),
        })
        .collect()
}

// ---- the sheet -----------------------------------------------------------------------

/// `layout_draw_view{source}`: the video's picture size and length, the layout it has
/// (or none), the presets drawn for a picture of its size, and the project's other
/// videos of that size.
pub fn view(core: &AppCore, project: &str, src: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let s = source(&doc, src)?;
    let (w, h, fps) =
        picture(s).ok_or_else(|| AppError::new("Unsupported", "this file has no picture"))?;
    let have = s.stream_layout.and_then(|l| doc.get(l));
    let presets: Vec<Value> = crate::takelayout::profile_layouts(core)
        .into_iter()
        .filter(|x| [x.layout.canvas_size.w, x.layout.canvas_size.h] == [w, h])
        .map(|x| json!({"name": x.layout.name, "regions": regions_json(&x.layout.regions)}))
        .collect();
    let siblings: Vec<Value> = doc
        .iter::<Source>()
        .filter(|o| o.id != s.id && picture(o).is_some_and(|(a, b, _)| (a, b) == (w, h)))
        .map(|o| {
            let file = o.path.rsplit(['/', '\\']).next().unwrap_or(&o.path);
            json!({"source": o.id.0.to_string(), "name": file,
                   "layout": o.stream_layout.and_then(|l| doc.get(l)).map(|l| l.name.clone())})
        })
        .collect();
    let file = s.path.rsplit(['/', '\\']).next().unwrap_or(&s.path);
    Ok(json!({
        "source": s.id.0.to_string(), "file": file,
        "canvas": {"w": w, "h": h}, "fps": fps,
        "duration_s": s.probe.duration.0 as f64 / crate::planner::FLICKS as f64,
        "name": have.map_or("Main scene", |l| l.name.as_str()),
        "has_layout": have.is_some(),
        "regions": have.map_or(json!([]), |l| regions_json(&l.regions)),
        "roles": ROLES.iter().map(|(r, label)| json!({"role": r, "label": label, "private": r.is_private_class()})).collect::<Vec<_>>(),
        "presets": presets, "siblings": siblings,
    }))
}

/// Where the sheet's first frame is: a tenth in, past an intro (1 s … 2 min).
pub fn default_at(duration_s: f64) -> f64 {
    (duration_s * 0.1).clamp(1.0_f64.min(duration_s / 2.0), 120.0)
}

/// One frame of the video `at_s` seconds in (default [`default_at`]): `{w, h, rgb
/// (base64), at_s}`, decoded from the file.
pub fn frame(core: &AppCore, project: &str, src: &str, at_s: Option<f64>) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let s = source(&doc, src)?;
    let (_, _, fps) =
        picture(s).ok_or_else(|| AppError::new("Unsupported", "this file has no picture"))?;
    let dur = s.probe.duration.0 as f64 / crate::planner::FLICKS as f64;
    let at = at_s
        .filter(|t| t.is_finite())
        .unwrap_or_else(|| default_at(dur))
        .clamp(0.0, (dur - 0.05).max(0.0));
    let n = (at * fps).floor() as u64;
    let (w, h, rgb) = decode(std::path::Path::new(&s.path), n)?;
    Ok(json!({"w": w, "h": h, "rgb": crate::recentthumbs::base64(&rgb), "at_s": at}))
}

#[cfg(any(target_os = "macos", windows))]
fn decode(path: &std::path::Path, n: u64) -> AppResult<(u32, u32, Vec<u8>)> {
    if !path.is_file() {
        return Err(AppError::not_found("the video file moved or was deleted"));
    }
    gmclip_render::export::proxy::file_thumbnail(path, n, FRAME_W)
        .map_err(|e| AppError::new("Render", format!("reading a frame: {e}")))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn decode(_: &std::path::Path, _: u64) -> AppResult<(u32, u32, Vec<u8>)> {
    Err(AppError::new(
        "Unsupported",
        "reading frames is not on this platform yet",
    ))
}

/// Parses the sheet's boxes: `[{role, rect: {x, y, w, h}}]`, normalised to the picture.
pub fn parse_regions(v: &Value) -> AppResult<Vec<StreamRegion>> {
    let rs: Vec<StreamRegion> = serde_json::from_value(v.clone()).map_err(|_| {
        AppError::invalid("boxes are [{role, rect: {x, y, w, h}}] with roles screen, cam, chat, private or alerts")
    })?;
    if rs.is_empty() || rs.len() > 8 {
        return Err(AppError::invalid("draw 1 to 8 boxes"));
    }
    if rs.iter().any(|r| !ROLES.iter().any(|(x, _)| *x == r.role)) {
        return Err(AppError::invalid(
            "a box is screen, camera, chat, private or alerts",
        ));
    }
    // tiny boxes are slips of the mouse, not regions
    if rs.iter().any(|r| r.rect.w < 0.01 || r.rect.h < 0.01) {
        return Err(AppError::invalid(
            "a box is at least 1% of the picture a side",
        ));
    }
    Ok(rs)
}

/// `layout_draw_save{source, name, regions, also?, preset?}`: the layout on this video
/// (and on `also`, other videos of the project with the same picture size), one
/// undoable step each; `preset` keeps it for the active profile.
pub fn save(core: &AppCore, project: &str, a: &Value) -> AppResult<Value> {
    let src = a
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("`source` is required"))?;
    let name: String = a
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Main scene")
        .chars()
        .take(200)
        .collect();
    let regions = parse_regions(a.get("regions").unwrap_or(&Value::Null))?;
    let also: Vec<String> = a
        .get("also")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let s = source(&doc, src)?;
    let (w, h, _) =
        picture(s).ok_or_else(|| AppError::new("Unsupported", "this file has no picture"))?;
    // the others: this project's videos with the same picture size
    for o in &also {
        let x = source(&doc, o)?;
        if picture(x).is_none_or(|(a, b, _)| (a, b) != (w, h)) {
            return Err(AppError::invalid(
                "another video gets this layout only when its picture is the same size",
            ));
        }
    }
    let args = |id: &str| {
        json!({"source": id, "name": name, "canvas": {"w": w, "h": h},
               "regions": regions.iter().map(|r| json!({"role": r.role, "rect": r.rect})).collect::<Vec<_>>()})
    };
    core.call(project, "source_stream_layout_set", args(src))?;
    let mut done = 1;
    for o in also.iter().filter(|o| o.as_str() != src) {
        core.call(project, "source_stream_layout_set", args(o))?;
        done += 1;
    }
    let preset = a.get("preset").and_then(Value::as_bool).unwrap_or(false);
    if preset {
        let key = crate::takelayout::profile_key(core);
        let mut st = load_store(core);
        let list = st.entry(key).or_default();
        list.retain(|l| !(l.name == name && [l.canvas_size.w, l.canvas_size.h] == [w, h]));
        list.push(StreamLayout {
            id: Id::new(gmclip_model::Ulid(0)),
            name: name.clone(),
            canvas_size: gmclip_model::entities::Canvas { w, h },
            fingerprint: String::new(),
            regions: regions.clone(),
            audio_roles: None,
            default_template: None,
        });
        if list.len() > 100 {
            list.remove(0);
        }
        save_store(core, &st)?;
    }
    let private = regions.iter().filter(|r| r.role.is_private_class()).count();
    Ok(
        json!({"name": name, "videos": done, "preset": preset, "private": private,
              "camera": regions.iter().any(|r| r.role == RegionRole::Cam)}),
    )
}

/// `layout_draw_*`.
pub fn ipc(core: &AppCore, command: &str, project: &str, a: &Value) -> AppResult<Value> {
    let src = || {
        a.get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid("`source` is required"))
    };
    match command {
        "layout_draw_view" => view(core, project, src()?),
        "layout_draw_frame" => frame(core, project, src()?, a.get("at_s").and_then(Value::as_f64)),
        "layout_draw_save" => save(core, project, a),
        _ => Err(AppError::invalid("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_is_a_tenth_in_within_bounds() {
        assert_eq!(default_at(30.0), 3.0);
        assert_eq!(default_at(3600.0), 120.0);
        assert_eq!(default_at(5.0), 1.0);
        assert_eq!(default_at(1.0), 0.5);
    }

    #[test]
    fn boxes_are_named_regions_inside_the_picture() {
        let ok = json!([{"role": "screen", "rect": {"x": 0.0, "y": 0.0, "w": 0.75, "h": 1.0}},
                        {"role": "cam", "rect": {"x": 0.75, "y": 0.7, "w": 0.25, "h": 0.3}},
                        {"role": "private", "rect": {"x": 0.75, "y": 0.0, "w": 0.25, "h": 0.2}}]);
        let r = parse_regions(&ok).unwrap();
        assert_eq!(r.len(), 3);
        assert!(r[2].role.is_private_class());
        for bad in [
            json!([]),
            json!([{"role": "banner", "rect": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 0.5}}]),
            json!([{"role": "cam", "rect": {"x": 0.0, "y": 0.0, "w": 0.001, "h": 0.5}}]),
            json!({"role": "cam"}),
        ] {
            assert!(parse_regions(&bad).is_err(), "{bad}");
        }
    }
}
