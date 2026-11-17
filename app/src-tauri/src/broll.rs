//! B-roll from your own library (Ideas 2026-10-16, K.05): for a selected span of a cut,
//! moments from other recordings that talk about the same thing, by the search-by-meaning
//! index (MiniLM sentence windows, L.02) over the span's own words. "Show the same thing"
//! is not measured: the index holds what was said, not what was on screen.
//!
//! One click adds a suggestion to the cut:
//!
//! - a moment from **another video of this project** goes in as a **part** right after the
//!   part holding the selection's last word (picture and sound of that moment);
//! - a moment from **another project** goes in as a **picture**: its frame, as an image
//!   overlay over the selected span (picture-in-picture, top right).
//!
//! Each suggestion says which of the two it will be (`add_as`, `note`). Since the clip
//! layer (decision 0028) either can instead go in as a **clip** (`as: "clip"`): the
//! moment's own video, silent, picture-in-picture over the selection — a span of the
//! project's video, or for another project a short excerpt stored with the cut.

use std::sync::Arc;

use gmclip_model::entities::{Cut, MediaRef, Part, Source, SyncGroup, SyncMember};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// The words of `cut`'s transcript `from..=to` and their owner (`owner`, `text`,
/// `start`, `end` in source flicks).
fn span_words(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
) -> AppResult<(String, String, i64, i64)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let (owner, _, t, _) = crate::words::transcript_of(core, &p, &doc, cid)
        .ok_or_else(|| AppError::new("Unsupported", "no transcript for this cut yet"))?;
    if to < from || to - from > 400 {
        return Err(AppError::invalid("select 1 to 400 words"));
    }
    let ws = t
        .words
        .get(from as usize..=to as usize)
        .ok_or_else(|| AppError::invalid("no such words"))?;
    let text = ws
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok((
        owner,
        text,
        ws.first().map_or(0, |w| w.start),
        ws.last().map_or(0, |w| w.end),
    ))
}

/// `broll_suggest{cut, from, to}`: meaning hits for the selected words in every other
/// recording of the library (not the recording the words are from), best first.
pub fn suggest(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
) -> AppResult<Value> {
    let (owner, text, _, _) = span_words(core, project, cut, from, to)?;
    crate::libsearch::refresh(core)?;
    let c = crate::libsearch::open(core)?;
    let (hits, state) = crate::libmeaning::hits(core, &c, &text, None, 30)?;
    let others: Vec<crate::libsearch::Hit> = hits
        .into_iter()
        .filter(|h| !(h.project == project && h.owner == owner))
        .take(8)
        .collect();
    let mut rows = crate::libsearch::shape(&c, core, &others)?;
    for r in &mut rows {
        let same = r["project"].as_str() == Some(project);
        r["add_as"] = json!(if same { "part" } else { "image" });
        r["also"] = json!("clip");
        r["note"] = json!(if same {
            "Goes in as a part after the selection, or as a clip over it"
        } else {
            "As a picture its frame goes over the selection; as a clip, a short excerpt of it"
        });
    }
    Ok(json!({"suggestions": rows, "meaning": state, "words": text.split_whitespace().count()}))
}

/// The source file whose picture a media shows (a group: its picture member's).
pub(crate) fn picture_path(doc: &gmclip_model::Doc, media: &MediaRef) -> Option<(String, f64)> {
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
        .find(|t| t.kind == gmclip_model::entities::TrackKind::Video)
        .and_then(|t| t.fps)
        .map_or(30.0, |r| r.num as f64 / r.den.max(1) as f64);
    Some((s.path.clone(), fps))
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) fn frame_png(path: &str, at_s: f64, fps: f64) -> AppResult<Vec<u8>> {
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return Err(AppError::not_found(
            "that recording's file moved or was deleted",
        ));
    }
    let n = (at_s.max(0.0) * fps).floor() as u64;
    let (w, h, rgb) = gmclip_render::export::proxy::file_thumbnail(p, n, 960)
        .map_err(|e| AppError::new("Render", format!("reading a frame: {e}")))?;
    let mut px = Vec::with_capacity(rgb.len() / 3 * 4);
    for c in rgb.chunks_exact(3) {
        px.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px,
    })
    .map_err(|e| AppError::internal(e.to_string()))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn frame_png(_: &str, _: f64, _: f64) -> AppResult<Vec<u8>> {
    Err(AppError::new(
        "Unsupported",
        "reading frames is not on this platform yet",
    ))
}

fn secs(a: &Value, k: &str) -> AppResult<f64> {
    a.get(k)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is seconds")))
}

/// `broll_add{cut, from, to, hit}`: the suggestion `hit` (`{project, home, media,
/// start_s, end_s}`) on the cut — a part after the selection when it is a video of this
/// project, else its frame as a picture over the selection. One undoable step.
pub fn add(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    hit: &Value,
    as_clip: bool,
) -> AppResult<Value> {
    let (_, _, w0, w1) = span_words(core, project, cut, from, to)?;
    let hp = hit
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("`hit.project` is required"))?;
    let media: MediaRef = serde_json::from_value(hit.get("media").cloned().unwrap_or(Value::Null))
        .map_err(|_| AppError::invalid("`hit.media` is {\"source\": id} or {\"group\": id}"))?;
    let (s0, s1) = (secs(hit, "start_s")?, secs(hit, "end_s")?);
    if s1 <= s0 || s1 - s0 > 120.0 {
        return Err(AppError::invalid("a suggestion of at most 2 minutes"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let pl = core.planned(project, cut).ok();
    let fpf = pl
        .as_ref()
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    // the part holding the selection's last word, and the selection on the output clock
    let mut after = None;
    let mut out0 = None;
    let mut out1 = None;
    for sp in &layout {
        let Some(pt) = doc.get::<Part>(sp.part) else {
            continue;
        };
        let at = |t: i64| sp.start.0 + (t - pt.src_in.0);
        if pt.src_in.0 <= w0 && w0 < pt.src_out.0 && out0.is_none() {
            out0 = Some(at(w0));
        }
        let mid = w1 - 1;
        if pt.src_in.0 <= mid && mid < pt.src_out.0 {
            after = Some(sp.part);
            out1 = Some(at(w1.min(pt.src_out.0)));
        }
    }
    let after = after.ok_or_else(|| AppError::invalid("the selection is not in the cut"))?;
    if as_clip {
        // decision 0028: the moment's own video, silent, over the selection
        let (o0, o1) = (out0.unwrap_or(0), out1.unwrap_or(0));
        let frame = u64::try_from(o0 / fpf).unwrap_or(0);
        let dur_ms = ((o1 - o0).max(FLICKS) * 1000 / FLICKS).clamp(1000, 60_000) as u64;
        let r = if hp == project {
            let source = match media {
                MediaRef::Source(s) => s,
                MediaRef::Group(g) => {
                    let g: &SyncGroup = doc
                        .get(g)
                        .ok_or_else(|| AppError::not_found("that recording is gone"))?;
                    let m: &SyncMember = g
                        .picture
                        .and_then(|m| doc.get(m))
                        .ok_or_else(|| AppError::not_found("that recording has no picture"))?;
                    m.source
                }
            };
            crate::backdrop::video_overlay_add(
                core,
                project,
                cut,
                &json!({"frame": frame, "dur_ms": dur_ms,
                        "span": {"source": source.0.to_string(), "from_s": s0, "to_s": s1}}),
            )?
        } else {
            let data = crate::libsearch::project_data(core, hp)
                .ok_or_else(|| AppError::not_found("that project is gone from the library"))?;
            let (path, _) = picture_path(&data.doc, &media)
                .ok_or_else(|| AppError::not_found("that recording has no picture"))?;
            let (asset, probe) = crate::backdrop::video_asset(
                core,
                project,
                std::path::Path::new(&path),
                s0,
                (s1 - s0).min(15.0),
                960,
            )?;
            let _ = probe;
            crate::backdrop::video_overlay_add(
                core,
                project,
                cut,
                &json!({"frame": frame, "dur_ms": dur_ms, "loop": true, "asset": asset}),
            )?
        };
        return Ok(json!({"added": "clip", "overlay": r["overlay"], "seconds": r["seconds"]}));
    }
    if hp == project {
        // a video of this project: a part, on the cut's frame grid
        let k0 = ((s0 * FLICKS as f64) as i64 + fpf - 1) / fpf;
        let k1 = (((s1 * FLICKS as f64) as i64) / fpf).max(k0 + 1);
        let r = core.call(
            project,
            "part_add",
            json!({"cut": cut, "after": after.0.to_string(), "media": media,
                   "src_in": k0 * fpf, "src_out": k1 * fpf, "role": "evidence"}),
        )?;
        return Ok(json!({"added": "part", "part": r["output"]["id"],
                         "seconds": ((k1 - k0) * fpf) as f64 / FLICKS as f64}));
    }
    // another project: its frame as a picture over the selection
    let data = crate::libsearch::project_data(core, hp)
        .ok_or_else(|| AppError::not_found("that project is gone from the library"))?;
    let (path, fps) = picture_path(&data.doc, &media)
        .ok_or_else(|| AppError::not_found("that recording has no picture"))?;
    let png = frame_png(&path, (s0 + s1) / 2.0, fps)?;
    let name = std::path::Path::new(&path).file_stem().map_or_else(
        || "b-roll".to_owned(),
        |n| format!("b-roll {}", n.to_string_lossy()),
    );
    let (asset, w, h) = crate::studio::store_image(core, project, &name, &png)?;
    let (o0, o1) = (out0.unwrap_or(0), out1.unwrap_or(0));
    let frame = u64::try_from(o0 / fpf).unwrap_or(0);
    let dur_ms = ((o1 - o0).max(FLICKS) * 1000 / FLICKS).clamp(1000, 60_000);
    let start = crate::studio::anchor_at(core, project, cut, frame)?;
    // picture-in-picture: 40 % of the width, top right, under the top overlay zone
    let width = 0.4;
    let pl = pl.ok_or_else(|| AppError::new("Unsupported", "the cut does not plan yet"))?;
    let canvas = pl.plan.output.canvas;
    let aspect = canvas.w.get() as f64 / canvas.h.get() as f64;
    let hfrac = width * aspect * f64::from(h) / f64::from(w);
    let r = core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "image", "start": start, "end": {"dur_ms": dur_ms},
               "place": {"x": 1.0 - width - 0.04, "y": (0.13f64).min(1.0 - hfrac), "w": width},
               "image": {"asset": asset, "w": w, "h": h}}),
    )?;
    Ok(
        json!({"added": "image", "overlay": r["output"]["id"], "asset": asset,
              "seconds": dur_ms as f64 / 1000.0}),
    )
}

/// `broll_suggest`, `broll_add`.
pub fn ipc(core: &Arc<AppCore>, command: &str, project: &str, a: &Value) -> AppResult<Value> {
    let cut = a
        .get("cut")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("`cut` is required"))?;
    let n = |k: &str| {
        a.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is a word")))
    };
    match command {
        "broll_suggest" => suggest(core, project, cut, n("from")?, n("to")?),
        "broll_add" => add(
            core,
            project,
            cut,
            n("from")?,
            n("to")?,
            a.get("hit").unwrap_or(&Value::Null),
            a.get("as").and_then(Value::as_str) == Some("clip"),
        ),
        _ => Err(AppError::invalid("unknown command")),
    }
}
