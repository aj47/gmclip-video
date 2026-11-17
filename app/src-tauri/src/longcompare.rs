//! Compare two long-forms (Ideas 2026-10-16, K.03): core's `longform_compare` (chapters,
//! order, lengths) plus a strip for each side — one small frame at each chapter's start,
//! decoded from the recording — so the two orders can be seen side by side.

use gmclip_model::entities::{Cut, MediaRef, Part, Source, SyncGroup, SyncMember, TrackKind};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::AppResult;
use crate::planner::{FLICKS, ulid_id};

/// Strip frames' width (px).
pub const STRIP_W: u32 = 160;

fn picture(doc: &gmclip_model::Doc, media: &MediaRef) -> Option<(String, f64)> {
    let s: &Source = match media {
        MediaRef::Source(s) => doc.get(*s)?,
        MediaRef::Group(g) => {
            let g: &SyncGroup = doc.get(*g)?;
            let m: &SyncMember = doc.get(g.picture?)?;
            doc.get(m.source)?
        }
    };
    let fps = s
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .and_then(|t| t.fps)
        .map_or(30.0, |r| r.num as f64 / r.den.max(1) as f64);
    Some((s.path.clone(), fps))
}

#[cfg(any(target_os = "macos", windows))]
fn thumb(path: &str, at_s: f64, fps: f64) -> Option<Value> {
    let p = std::path::Path::new(path);
    let n = (at_s.max(0.0) * fps).floor() as u64;
    let (w, h, rgb) = gmclip_render::export::proxy::file_thumbnail(p, n, STRIP_W).ok()?;
    Some(json!({"w": w, "h": h, "rgb": crate::recentthumbs::base64(&rgb)}))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn thumb(_: &str, _: f64, _: f64) -> Option<Value> {
    None
}

/// The frame at output time `at_ms` of `cut` (its part there, that part's picture).
fn frame_at(doc: &gmclip_model::Doc, cut: &str, at_ms: i64) -> Option<Value> {
    let cid = ulid_id::<Cut>(cut).ok()?;
    let t = at_ms * (FLICKS / 1000);
    let layout = gmclip_model::clock::cut_layout(doc, cid);
    let sp = layout
        .iter()
        .find(|s| t >= s.start.0 && t < s.start.0 + s.dur.0)
        .or_else(|| layout.first())?;
    let pt: &Part = doc.get(sp.part)?;
    let src = pt.src_in.0 + (t - sp.start.0).max(0);
    let (path, fps) = picture(doc, &pt.media)?;
    // a beat into the chapter: past a cut's first frame
    thumb(&path, src as f64 / FLICKS as f64 + 0.5, fps)
}

/// `longform_compare_view{a, b}`: the comparison with a strip frame on every chapter.
pub fn view(core: &AppCore, project: &str, a: &str, b: &str) -> AppResult<Value> {
    let mut v = core.call(project, "longform_compare", json!({"a": a, "b": b}))?["output"].take();
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    for side in ["a", "b"] {
        let cut = v[side]["cut"].as_str().unwrap_or_default().to_owned();
        if let Some(chs) = v[side]["chapters"].as_array_mut() {
            for ch in chs {
                let at = ch["at_ms"].as_i64().unwrap_or(0);
                ch["frame"] = frame_at(&doc, &cut, at).unwrap_or(Value::Null);
            }
        }
    }
    Ok(v)
}
