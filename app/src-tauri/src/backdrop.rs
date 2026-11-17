//! Backdrops and video overlays (SPEC §5.11, decision 0028), the app half.
//!
//! - `backdrop_set {cut, path | asset, loop, fit}`: a video or a still under every part of
//!   the cut's framing, running on across part joins. A picked file is stored once in the
//!   app's assets by its hash (a video up to 20 MB as it is, a longer one as a 10 s
//!   excerpt the exporter renders; a still re-encoded as PNG with its size), registered
//!   with `asset_add` and set with `framing_set`. One undo step each.
//! - `video_overlay_add {cut, frame, path | span, loop}`: a video picture-in-picture at the
//!   playhead — a file (stored like a backdrop clip; a long one as a 15 s excerpt) or a
//!   span of one of the project's own videos — through `overlay_add {kind: video}`.
//!
//! - `overlay_track {cut, overlay}` ("Pin to motion", decision 0031): follows the video
//!   under an overlay's pin point with Apple Vision and pins the overlay to it (macOS;
//!   Windows says it is not there yet). It follows every take of that video the overlay
//!   spans (decision 0033); synced groups are refused in words.
//!
//! - `cover_track {cut, layer, frame?}` (decision 0033): the same for a cover layer, from
//!   the playhead through every take of the video it is on; `cover_add {cut, frame, x, y}`
//!   ("Blur here") puts a blur cover over the picture at a canvas point.
//!
//! The exporter is the only thing that cuts video here (no ffmpeg): an excerpt is a small
//! RenderPlan of the file's span rendered by `gmclip_render::export`.

use std::path::{Path, PathBuf};

use gmclip_model::entities::AssetProbe;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// The app-shell commands this module serves.
pub const COMMANDS: &[&str] = &[
    "backdrop_set",
    "video_overlay_add",
    "overlay_track",
    "cover_track",
    "cover_add",
];

/// The largest stored asset (SPEC §4.3 `Asset`: ≤ 20 MB inline).
pub const MAX_ASSET: u64 = 20 << 20;

/// A backdrop excerpt's length and width cap.
const BACKDROP_EXCERPT_S: f64 = 10.0;
/// A video overlay excerpt's length and width cap.
const OVERLAY_EXCERPT_S: f64 = 15.0;

fn s<'a>(a: &'a Value, k: &str) -> AppResult<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
}

/// Serves one of [`COMMANDS`].
pub fn dispatch(core: &AppCore, project: &str, name: &str, a: &Value) -> AppResult<Value> {
    match name {
        "backdrop_set" => backdrop_set(core, project, s(a, "cut")?, a),
        "video_overlay_add" => video_overlay_add(core, project, s(a, "cut")?, a),
        "overlay_track" => overlay_track(core, project, s(a, "cut")?, a),
        "cover_track" => cover_track(core, project, s(a, "cut")?, a),
        "cover_add" => cover_add(core, project, s(a, "cut")?, a),
        _ => Err(AppError::invalid("unknown command")),
    }
}

/// `video/mp4` or `video/quicktime` by a file's extension (MKV and WebM are excerpted).
fn video_type(p: &Path) -> Option<&'static str> {
    match p
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

fn is_image(p: &Path) -> bool {
    p.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
        )
    })
}

/// A video file's size, rate and frame count (the app's own demuxer).
fn probe_video(p: &Path) -> AppResult<(AssetProbe, crate::video::Probed)> {
    let pr = crate::video::probe(p)?;
    let secs = pr.probe.duration.0 as f64 / gmclip_model::time::FLICKS_PER_SECOND as f64;
    let rate = pr.fps.num as f64 / f64::from(pr.fps.den.max(1));
    let frames = (secs * rate).round().max(1.0) as u64;
    Ok((
        AssetProbe {
            w: pr.dims.0,
            h: pr.dims.1,
            fps: Some(pr.fps),
            frames: Some(frames),
        },
        pr,
    ))
}

/// Stores a video file as an asset as it is (≤ 20 MB, MP4/MOV): `(asset id, probe)`.
fn store_video(
    core: &AppCore,
    project: &str,
    p: &Path,
    name: &str,
) -> AppResult<(String, AssetProbe)> {
    let media_type = video_type(p).ok_or_else(|| AppError::invalid("an MP4 or MOV file"))?;
    let (probe, _) = probe_video(p)?;
    let bytes = std::fs::read(p).map_err(|_| AppError::not_found("that file is gone"))?;
    if bytes.len() as u64 > MAX_ASSET {
        return Err(AppError::invalid("a stored video is at most 20 MB"));
    }
    let sha = gmclip_core::profile::style::sha256_hex(&bytes);
    let dest = core
        .fonts()
        .asset_file_typed(&sha, media_type)
        .ok_or_else(|| AppError::internal("no assets folder"))?;
    if !dest.is_file() {
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| {
                AppError::internal(format!("making the assets folder: {}", e.kind()))
            })?;
        }
        crate::core::write_atomic(&dest, &bytes)
            .map_err(|e| AppError::internal(format!("storing the video: {}", e.kind())))?;
    }
    let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    let out = core.call(
        project,
        "asset_add",
        json!({"name": if name.trim().is_empty() { "video".to_owned() } else { name },
               "sha256": sha, "size": bytes.len(), "media_type": media_type, "probe": probe}),
    )?;
    Ok((
        out["output"]["id"].as_str().unwrap_or_default().to_owned(),
        probe,
    ))
}

/// Renders `max_s` seconds of `p` from `from_s` at most `max_w` px wide into a small MP4
/// (the exporter: a one-part RenderPlan of the file's picture and sound).
#[cfg(any(target_os = "macos", windows))]
fn excerpt(p: &Path, from_s: f64, max_s: f64, max_w: u32, out: &Path) -> AppResult<()> {
    let (_, pr) = probe_video(p)?;
    let secs = pr.probe.duration.0 as f64 / gmclip_model::time::FLICKS_PER_SECOND as f64;
    let from_s = from_s.clamp(0.0, (secs - 0.5).max(0.0));
    let rate = crate::video::framing_fps(pr.fps);
    let (on, od) = (rate.num as u64, u64::from(rate.den));
    let out_fps = on as f64 / od as f64;
    let n = ((max_s.min(secs - from_s)) * out_fps).floor().max(1.0) as u64;
    let first = (from_s * out_fps).round() as u64;
    let (cw, ch) = crate::video::canvas_for(pr.dims, max_w);
    let audio = pr
        .probe
        .audio_tracks
        .first()
        .filter(|t| t.rate == 48_000)
        .ok_or_else(|| {
            AppError::new(
                "Unsupported",
                "that video has no 48 kHz sound track, so gmclip cannot cut a clip from it",
            )
        })?;
    let mut picture = json!({"source": "v", "first_frame": first});
    if u128::from(pr.fps.num as u64) * u128::from(od) != u128::from(on) * u128::from(pr.fps.den) {
        picture["fps"] = json!({"num": pr.fps.num, "den": pr.fps.den});
    }
    let v1 = picture.get("fps").is_some() || od != 1;
    let v = json!({
        "schema": if v1 { "gmclip.renderplan/1" } else { "gmclip.renderplan/0" },
        "meta": {"title": "excerpt"},
        "output": {"canvas": {"w": cw, "h": ch}, "fps": {"num": on, "den": od}, "frames": n,
            "video": {"codec": "h264", "bframes": 0, "gop_frames": on.div_ceil(od),
                      "rate": {"mode": "vbr", "target_bps": 6_000_000, "max_bps": 9_000_000}},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "v", "path": p.to_string_lossy(), "audio_track": audio.index.max(1)}],
        "parts": [{"id": "p0", "out": [0, n], "picture": picture,
            "audio": {"source": "v", "first_sample": (from_s * 48_000.0).round() as u64, "rate": 48000},
            "layers": [{"id": "picture", "role": "other",
                        "crop": {"x": 0, "y": 0, "w": pr.dims.0, "h": pr.dims.1},
                        "place": {"x": 0, "y": 0, "w": cw, "h": ch}, "z": 0}]}],
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap_or_default()).map_err(|e| {
        AppError::invalid(format!(
            "the excerpt cannot be planned: {} ({})",
            e.reason(),
            e.path()
        ))
    })?;
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    gmclip_render::export::export(
        &plan,
        &mut inputs,
        out,
        &gmclip_render::export::ExportOptions::default(),
    )
    .map_err(|e| AppError::new("Render", format!("cutting the clip: {e}")))?;
    let _ = std::fs::remove_file(out.with_extension("export.json"));
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn excerpt(_: &Path, _: f64, _: f64, _: u32, _: &Path) -> AppResult<()> {
    Err(AppError::new(
        "Unsupported",
        "cutting a clip is not on this platform yet",
    ))
}

/// A video file as an asset: as it is when it is a small MP4/MOV, else an excerpt of
/// `max_s` seconds from `from_s` at most `max_w` px wide.
pub fn video_asset(
    core: &AppCore,
    project: &str,
    p: &Path,
    from_s: f64,
    max_s: f64,
    max_w: u32,
) -> AppResult<(String, AssetProbe)> {
    let name = p
        .file_name()
        .map_or_else(|| "video".to_owned(), |n| n.to_string_lossy().into_owned());
    let len = std::fs::metadata(p)
        .map_err(|_| AppError::not_found("that file is gone"))?
        .len();
    if from_s <= 0.0 && len <= MAX_ASSET && video_type(p).is_some() {
        return store_video(core, project, p, &name);
    }
    let tmp: PathBuf = std::env::temp_dir().join(format!(
        "gmclip-excerpt-{}-{}.mp4",
        std::process::id(),
        crate::core::now_ms()
    ));
    let r = excerpt(p, from_s, max_s, max_w, &tmp)
        .and_then(|()| store_video(core, project, &tmp, &format!("{name} (clip)")));
    let _ = std::fs::remove_file(&tmp);
    r
}

fn framing(core: &AppCore, project: &str, cut: &str) -> AppResult<String> {
    core.active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))
}

/// `backdrop_set {cut, path | asset, loop?, fit?}` (see the module docs).
pub fn backdrop_set(core: &AppCore, project: &str, cut: &str, a: &Value) -> AppResult<Value> {
    let fit = a.get("fit").and_then(Value::as_str).unwrap_or("cover");
    if !matches!(fit, "cover" | "contain") {
        return Err(AppError::invalid("`fit` is cover or contain"));
    }
    let lp = a.get("loop").and_then(Value::as_bool).unwrap_or(true);
    let asset = match (
        a.get("asset").and_then(Value::as_str),
        a.get("path").and_then(Value::as_str),
    ) {
        (Some(id), _) => id.to_owned(),
        (None, Some(path)) => {
            let p = Path::new(path);
            if is_image(p) {
                let bytes =
                    std::fs::read(p).map_err(|_| AppError::not_found("that file is gone"))?;
                let name = p.file_name().map_or_else(
                    || "backdrop".to_owned(),
                    |n| n.to_string_lossy().into_owned(),
                );
                crate::studio::store_image_probed(core, project, &name, &bytes)?.0
            } else {
                video_asset(core, project, p, 0.0, BACKDROP_EXCERPT_S, 1080)?.0
            }
        }
        _ => return Err(AppError::invalid("`path` or `asset` is required")),
    };
    let f = framing(core, project, cut)?;
    core.call(
        project,
        "framing_set",
        json!({"framing": f, "background": {"backdrop": {"asset": asset, "loop": lp, "fit": fit}}}),
    )?;
    Ok(json!({"asset": asset, "framing": f}))
}

/// `video_overlay_add {cut, frame, path | asset | span {source, from_s, to_s}, loop?, dur_ms?}`:
/// a video picture-in-picture at output frame `frame`, 40 % of the canvas wide, top right
/// under the top overlay zone. Its span lasts the clip (at most 60 s) unless `dur_ms`.
pub fn video_overlay_add(core: &AppCore, project: &str, cut: &str, a: &Value) -> AppResult<Value> {
    let frame = a.get("frame").and_then(Value::as_u64).unwrap_or(0);
    let lp = a.get("loop").and_then(Value::as_bool).unwrap_or(false);
    let (media, (w, h), secs) = if let Some(id) = a.get("asset").and_then(Value::as_str) {
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let asset = doc
            .get(crate::planner::ulid_id::<gmclip_model::entities::Asset>(
                id,
            )?)
            .ok_or_else(|| AppError::not_found("no such asset"))?;
        let probe = asset
            .probe
            .ok_or_else(|| AppError::invalid("a stored video with its probe"))?;
        let rate = probe
            .fps
            .map_or(30.0, |r| r.num as f64 / f64::from(r.den.max(1)));
        (
            json!({"asset": id}),
            (probe.w, probe.h),
            probe.frames.unwrap_or(1) as f64 / rate,
        )
    } else if let Some(path) = a.get("path").and_then(Value::as_str) {
        let (asset, probe) =
            video_asset(core, project, Path::new(path), 0.0, OVERLAY_EXCERPT_S, 960)?;
        let rate = probe
            .fps
            .map_or(30.0, |r| r.num as f64 / f64::from(r.den.max(1)));
        let secs = probe.frames.unwrap_or(1) as f64 / rate;
        (json!({"asset": asset}), (probe.w, probe.h), secs)
    } else if let Some(sp) = a.get("span") {
        let source = s(sp, "source")?;
        let num = |k: &str| {
            sp.get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| AppError::invalid(format!("`span.{k}` is seconds")))
        };
        let (s0, s1) = (num("from_s")?, num("to_s")?);
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let src = doc
            .get(crate::planner::ulid_id::<gmclip_model::entities::Source>(
                source,
            )?)
            .ok_or_else(|| AppError::not_found("no such video in this project"))?;
        let dims = src
            .probe
            .tracks
            .iter()
            .find_map(|t| Some((t.width?, t.height?)))
            .ok_or_else(|| AppError::invalid("that video has no picture"))?;
        let f = |t: f64| (t * gmclip_model::time::FLICKS_PER_SECOND as f64).round() as i64;
        (
            json!({"span": {"media": {"source": source}, "src_in": f(s0), "src_out": f(s1)}}),
            dims,
            s1 - s0,
        )
    } else {
        return Err(AppError::invalid("`path` or `span` is required"));
    };
    let dur_ms = a
        .get("dur_ms")
        .and_then(Value::as_u64)
        .unwrap_or((secs * 1000.0).round() as u64)
        .clamp(500, 60_000);
    // the framing's canvas and frame grid (from the model: adding a clip never needs the
    // cut to plan first)
    let fid = framing(core, project, cut)?;
    let doc = core.project(project)?.handle.snapshot().doc.clone();
    let fr = doc
        .get(crate::planner::ulid_id::<gmclip_model::entities::Framing>(
            &fid,
        )?)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let aspect = f64::from(fr.canvas.w) / f64::from(fr.canvas.h.max(1));
    let fpf = gmclip_core::planner::framing_fpf(fr.fps).unwrap_or(crate::planner::FLICKS / 30);
    let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut)?;
    let t = frame as i64 * fpf;
    let sp = gmclip_model::clock::cut_layout(&doc, cid)
        .into_iter()
        .find(|s| t >= s.start.0 && t < s.start.0 + s.dur.0)
        .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
    let start = json!({"part_start": {"part": sp.part.0.to_string(),
                                      "offset_ms": (t - sp.start.0) * 1000 / crate::planner::FLICKS}});
    let width = 0.4;
    let hfrac = width * aspect * f64::from(h) / f64::from(w.max(1));
    let out = core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "video", "start": start, "end": {"dur_ms": dur_ms},
               "place": {"x": 1.0 - width - 0.04, "y": (0.13f64).min(1.0 - hfrac), "w": width},
               "video": {"media": media, "loop": lp}}),
    )?;
    Ok(json!({"overlay": out["output"]["id"], "seconds": dur_ms as f64 / 1000.0}))
}

// ---- motion tracking (decision 0031) -------------------------------------------------------

/// An anchor's output frame (nearest-earlier), when the cut shows it.
fn out_frame(
    doc: &gmclip_model::Doc,
    cid: gmclip_model::Id<gmclip_model::entities::Cut>,
    a: &gmclip_model::anchor::Anchor,
    fpf: i64,
) -> Option<u64> {
    let r = gmclip_core::anchors::resolve(doc, Some(cid), a, "/").ok()?;
    // on the anchor's own media (a bare source time could land in another video)
    let t = r.cut_t?;
    u64::try_from(t.0 / fpf).ok()
}

/// The canvas point (fractions) an overlay is pinned by: a box's centre, an arrow's tip,
/// a picture's centre, a label's anchor.
fn pin_point(o: &gmclip_model::entities::Overlay, aspect: f64) -> [f64; 2] {
    use gmclip_model::entities::{OverlayGeom, ShapeKind};
    match (&o.geom, &o.shape) {
        (_, Some(s)) if s.kind == ShapeKind::Arrow => s.to.unwrap_or([0.5, 0.5]),
        (OverlayGeom::Box(b), _) => [b.x + b.w / 2.0, b.y + b.h / 2.0],
        (OverlayGeom::Place(p), _) => match &o.image {
            Some(im) => [
                p.x + p.w / 2.0,
                p.y + p.w * aspect * f64::from(im.h) / f64::from(im.w.max(1)) / 2.0,
            ],
            None => [p.x, p.y],
        },
    }
}

/// The model part behind plan part `k`.
#[cfg(target_os = "macos")]
fn model_part<'d>(
    doc: &'d gmclip_model::Doc,
    pl: &crate::core::Planned,
    k: usize,
) -> AppResult<&'d gmclip_model::entities::Part> {
    let model = pl
        .index
        .parts
        .get(k)
        .map(|x| x.model.clone())
        .ok_or_else(|| AppError::internal("the plan index has no such part"))?;
    doc.get(crate::planner::ulid_id::<gmclip_model::entities::Part>(
        &model,
    )?)
    .ok_or_else(|| AppError::not_found("the take is gone"))
}

/// The source-frame spans a pin follows over output frames `[start, end)` (decision 0033,
/// a track across takes): each plan part `keep` accepts whose picture is plan source `pic`
/// and whose take is of video `sid`, in the order the cut plays them, as presentation
/// indices. Parts of another video (or a synced group) are left out; the pin holds there.
#[cfg(target_os = "macos")]
fn take_spans(
    doc: &gmclip_model::Doc,
    pl: &crate::core::Planned,
    sid: gmclip_model::Id<gmclip_model::entities::Source>,
    pic: &str,
    (start, end): (u64, u64),
    keep: &dyn Fn(usize) -> bool,
) -> Vec<(u64, u64)> {
    use gmclip_model::entities::MediaRef;
    let fps = pl.plan.output.fps;
    let mut spans = Vec::new();
    for (k, q) in pl.plan.parts.iter().enumerate() {
        let (a, b) = (start.max(q.out.start()), end.min(q.out.end()));
        if a >= b || q.picture.source.as_str() != pic || !keep(k) {
            continue;
        }
        if model_part(doc, pl, k).ok().map(|m| m.media) != Some(MediaRef::Source(sid)) {
            continue;
        }
        let at = |f: u64| gmclip_plan::retime::nominal_frame(q, f - q.out.start(), fps);
        let (f0, f1) = (at(a), at(b - 1));
        spans.push((f0, f1.max(f0)));
    }
    spans
}

/// A run's samples as track keys on the video's clock, each the centred mean of the
/// samples within 2 frames of it (Vision's box jitters a few pixels; a take's edge is not
/// averaged with another take's frames).
#[cfg(target_os = "macos")]
fn run_keys(
    run: &gmclip_render::export::track::TrackRun,
    rate: gmclip_model::Rational,
) -> Vec<Value> {
    let s = &run.samples;
    let fl = crate::planner::FLICKS as f64;
    (0..s.len())
        .map(|i| {
            let near: Vec<_> = s[i.saturating_sub(2)..(i + 3).min(s.len())]
                .iter()
                .filter(|o| o.frame.abs_diff(s[i].frame) <= 2)
                .collect();
            let n = near.len().max(1) as f64;
            let t = (s[i].frame as f64 * fl * f64::from(rate.den) / rate.num.max(1) as f64).round()
                as i64;
            json!({"t": t, "x": near.iter().map(|o| o.x).sum::<f64>() / n,
                   "y": near.iter().map(|o| o.y).sum::<f64>() / n})
        })
        .collect()
}

/// How many of a run's samples are on frames the cut plays (the frames between two takes
/// are followed but not shown).
#[cfg(target_os = "macos")]
fn played(run: &gmclip_render::export::track::TrackRun, spans: &[(u64, u64)]) -> usize {
    run.samples
        .iter()
        .filter(|s| spans.iter().any(|(a, b)| (*a..=*b).contains(&s.frame)))
        .count()
}

/// A video's nominal rate (its first track that has one; 30 fps when none does).
#[cfg(target_os = "macos")]
fn source_rate(src: &gmclip_model::entities::Source) -> gmclip_model::Rational {
    src.probe
        .tracks
        .iter()
        .find_map(|t| t.fps)
        .unwrap_or(gmclip_model::Rational::new(30, 1))
}

/// The refusal for a pin on a synced group's take.
#[cfg(target_os = "macos")]
const SYNCED: &str = "pin to motion follows a take of one video; synced cameras are not followed yet (pin on a take of one video)";

/// `overlay_track {cut, overlay}` ("Pin to motion"): follows the video under the overlay's
/// pin point (a box's centre, an arrow's tip, a picture's centre) from the overlay's start
/// to its end with Apple Vision's object tracker — through every take of that video the
/// overlay spans (decision 0033), following the frames between two takes up to 5 s apart
/// and starting again where the point was otherwise — and pins the overlay to it
/// (`overlay_track_set`, one undo step). The keys are smoothed over 5 frames.
#[cfg(target_os = "macos")]
pub fn overlay_track(core: &AppCore, project: &str, cut: &str, a: &Value) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, MediaRef, Overlay, Source};
    let oid = s(a, "overlay")?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let o: &Overlay = doc
        .get(crate::planner::ulid_id::<Overlay>(oid)?)
        .ok_or_else(|| AppError::not_found("no such overlay"))?;
    let pl = core.planned(project, cut)?;
    let plan = &pl.plan;
    let fpf = crate::planner::fpf(plan);
    let total = plan.output.frames;
    let start = out_frame(&doc, cid, &o.start, fpf)
        .ok_or_else(|| AppError::invalid("the overlay's start is not in the cut"))?
        .min(total.saturating_sub(1));
    let end = match &o.end {
        gmclip_model::entities::OverlayEnd::At(e) => out_frame(&doc, cid, e, fpf),
        gmclip_model::entities::OverlayEnd::DurMs(ms) => {
            Some(start + (i64::from(*ms) * crate::planner::FLICKS / 1000 / fpf) as u64)
        }
    }
    .unwrap_or(total)
    .min(total)
    .max(start + 1);
    let (cw, ch) = (
        plan.output.canvas.w.get() as f64,
        plan.output.canvas.h.get() as f64,
    );
    let pt = pin_point(o, cw / ch);
    let (px, py) = (pt[0] * cw, pt[1] * ch);
    // the part and layer under the point at the start
    let k = plan
        .parts
        .iter()
        .position(|q| q.out.start() <= start && start < q.out.end())
        .ok_or_else(|| AppError::invalid("the overlay starts past the cut"))?;
    let draw = gmclip_render::compositor::frame_draw(plan, start)
        .map_err(|e| AppError::new("Render", e.to_string()))?;
    let layer = draw
        .layers
        .iter()
        .rev()
        .filter(|l| !l.is_extra() && l.cover.is_none())
        .find(|l| {
            px >= l.place.x
                && px < l.place.x + l.place.w
                && py >= l.place.y
                && py < l.place.y + l.place.h
        })
        .ok_or_else(|| AppError::invalid("nothing of the video is under the overlay to follow"))?;
    let MediaRef::Source(sid) = model_part(&doc, &pl, k)?.media else {
        return Err(AppError::new("Unsupported", SYNCED));
    };
    let src: &Source = doc
        .get(sid)
        .ok_or_else(|| AppError::not_found("the video is gone"))?;
    let rate = source_rate(src);
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let size = gmclip_render::export::Inputs::video(&mut inputs, plan, &layer.source)
        .map(|v| (f64::from(v.width), f64::from(v.height)))
        .map_err(|e| AppError::new("Render", e.to_string()))?;
    let point = [
        (layer.crop.x + (px - layer.place.x) * layer.crop.w / layer.place.w) / size.0,
        (layer.crop.y + (py - layer.place.y) * layer.crop.h / layer.place.h) / size.1,
    ];
    // every take of this video the overlay spans, from the pin on (decision 0033)
    let mut spans = take_spans(&doc, &pl, sid, &layer.source, (start, end), &|_| true);
    if let Some(first) = spans.first_mut() {
        first.0 = layer.frame;
        first.1 = first.1.max(layer.frame);
    }
    let run = gmclip_render::export::track::track_spans(
        plan,
        &mut inputs,
        &layer.source,
        &spans,
        point,
        gmclip_render::export::track::BOX,
        None,
    )
    .map_err(|e| AppError::new("Render", e.to_string()))?;
    let keys = run_keys(&run, rate);
    let n = keys.len();
    core.call(
        project,
        "overlay_track_set",
        json!({"overlay": oid, "track": {"media": {"source": sid.0.to_string()}, "keys": keys,
               "method": "apple_vision.track_object"}}),
    )?;
    let fps = plan.output.fps.as_f64();
    Ok(json!({
        "keys": n,
        "takes": spans.len(),
        "seconds": played(&run, &spans) as f64 / (rate.num as f64 / f64::from(rate.den.max(1))),
        "lost": run.lost_at.is_some(),
        "until_s": (start as f64 + played(&run, &spans) as f64) / fps,
    }))
}

/// `cover_track {cut, layer, frame?}` ("Pin to motion" on a cover, decision 0033): follows
/// the video under a cover layer's centre from output frame `frame` (the playhead; 0 when
/// absent) to the cut's end, through every take of that video the cover is on, and pins
/// the cover to it (`layer_track_set`, one undo step).
#[cfg(target_os = "macos")]
pub fn cover_track(core: &AppCore, project: &str, cut: &str, a: &Value) -> AppResult<Value> {
    use gmclip_model::entities::{Layer, MediaRef, Source};
    let lid = s(a, "layer")?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let l: &Layer = doc
        .get(crate::planner::ulid_id::<Layer>(lid)?)
        .ok_or_else(|| AppError::not_found("no such layer"))?;
    if l.cover.is_none() {
        return Err(AppError::invalid("only a cover layer is pinned to motion"));
    }
    let pl = core.planned(project, cut)?;
    let plan = &pl.plan;
    let frame = a.get("frame").and_then(Value::as_u64).unwrap_or(0);
    // the plan layer of this cover on plan part k, if it shows there
    let plan_layer = |k: usize| {
        let ip = pl.index.parts.get(k)?;
        let (pid, _) = ip.layers.iter().find(|(_, m)| m == lid)?;
        plan.parts
            .get(k)?
            .layers
            .iter()
            .find(|x| x.id.as_str() == pid)
    };
    let k = (0..plan.parts.len())
        .find(|&k| plan.parts[k].out.end() > frame && plan_layer(k).is_some())
        .ok_or_else(|| AppError::invalid("the cover is not on the cut from the playhead on"))?;
    let q = &plan.parts[k];
    let start = frame.max(q.out.start());
    let MediaRef::Source(sid) = model_part(&doc, &pl, k)?.media else {
        return Err(AppError::new("Unsupported", SYNCED));
    };
    let src: &Source = doc
        .get(sid)
        .ok_or_else(|| AppError::not_found("the video is gone"))?;
    let rate = source_rate(src);
    let pic = q.picture.source.as_str().to_owned();
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let size = gmclip_render::export::Inputs::video(&mut inputs, plan, &pic)
        .map(|v| (f64::from(v.width), f64::from(v.height)))
        .map_err(|e| AppError::new("Render", e.to_string()))?;
    let c = plan_layer(k)
        .map(|x| x.crop)
        .ok_or_else(|| AppError::internal("the cover's plan layer is gone"))?;
    let point = [(c.x + c.w / 2.0) / size.0, (c.y + c.h / 2.0) / size.1];
    let spans = take_spans(&doc, &pl, sid, &pic, (start, plan.output.frames), &|k| {
        plan_layer(k).is_some()
    });
    let run = gmclip_render::export::track::track_spans(
        plan,
        &mut inputs,
        &pic,
        &spans,
        point,
        gmclip_render::export::track::BOX,
        None,
    )
    .map_err(|e| AppError::new("Render", e.to_string()))?;
    let keys = run_keys(&run, rate);
    let n = keys.len();
    core.call(
        project,
        "layer_track_set",
        json!({"layer": lid, "track": {"media": {"source": sid.0.to_string()}, "keys": keys,
               "method": "apple_vision.track_object"}}),
    )?;
    Ok(json!({
        "keys": n,
        "takes": spans.len(),
        "seconds": played(&run, &spans) as f64 / (rate.num as f64 / f64::from(rate.den.max(1))),
        "lost": run.lost_at.is_some(),
    }))
}

#[cfg(not(target_os = "macos"))]
pub fn overlay_track(_: &AppCore, _: &str, _: &str, _: &Value) -> AppResult<Value> {
    Err(AppError::new(
        "Unsupported",
        "Pin to motion is not on Windows yet (it uses Apple Vision): pin it on a Mac; the pinned overlay plays here",
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn cover_track(_: &AppCore, _: &str, _: &str, _: &Value) -> AppResult<Value> {
    Err(AppError::new(
        "Unsupported",
        "Pin to motion is not on Windows yet (it uses Apple Vision): pin it on a Mac; the pinned cover plays here",
    ))
}

/// `cover_add {cut, frame, x, y, w?, style?}` ("Blur here", decision 0033): a cover over
/// the square of `w` (a share of the canvas width, 0.15 by default) centred on canvas
/// point `(x, y)` (shares) on output frame `frame`, over the picture layer drawn there:
/// its crop is that square's region of the picture, its place the square, just above the
/// layer; `style` is `blur` (24 px), `pixelate` (20 px) or `solid` (dark). It joins the
/// stack that take shows — its layout, else the framing's — so the takes sharing it carry
/// it too (`layer_add`, one undo step).
pub fn cover_add(core: &AppCore, project: &str, cut: &str, a: &Value) -> AppResult<Value> {
    let pl = core.planned(project, cut)?;
    let plan = &pl.plan;
    let num = |k: &str| a.get(k).and_then(Value::as_f64);
    let frame = a
        .get("frame")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(plan.output.frames.saturating_sub(1));
    let (x, y) = (
        num("x").unwrap_or(0.5).clamp(0.0, 1.0),
        num("y").unwrap_or(0.5).clamp(0.0, 1.0),
    );
    let w = num("w").unwrap_or(0.15).clamp(0.02, 1.0);
    let (cw, ch) = (
        plan.output.canvas.w.get() as f64,
        plan.output.canvas.h.get() as f64,
    );
    let side = w * cw;
    let (bx, by) = (x * cw - side / 2.0, y * ch - side / 2.0);
    let draw = gmclip_render::compositor::frame_draw(plan, frame)
        .map_err(|e| AppError::new("Render", e.to_string()))?;
    let layer = draw
        .layers
        .iter()
        .rev()
        .filter(|l| !l.is_extra() && l.cover.is_none())
        .find(|l| {
            let (px, py) = (x * cw, y * ch);
            px >= l.place.x
                && px < l.place.x + l.place.w
                && py >= l.place.y
                && py < l.place.y + l.place.h
        })
        .ok_or_else(|| AppError::invalid("nothing of the video is there to cover"))?;
    let k = plan
        .parts
        .iter()
        .position(|q| q.out.start() <= frame && frame < q.out.end())
        .ok_or_else(|| AppError::invalid("the playhead is past the cut"))?;
    let ip = pl
        .index
        .parts
        .get(k)
        .ok_or_else(|| AppError::internal("the plan index has no such part"))?;
    let z = plan.parts[k]
        .layers
        .iter()
        .find(|l| l.id.as_str() == layer.layer_id)
        .map_or(10, |l| l.z + 1);
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let (sw, sh) = gmclip_render::export::Inputs::video(&mut inputs, plan, &layer.source)
        .map(|v| (f64::from(v.width), f64::from(v.height)))
        .map_err(|e| AppError::new("Render", e.to_string()))?;
    let (kx, ky) = (layer.crop.w / layer.place.w, layer.crop.h / layer.place.h);
    // the square's region of the picture, kept on it
    let cx = (layer.crop.x + (bx - layer.place.x) * kx).clamp(0.0, sw);
    let cy = (layer.crop.y + (by - layer.place.y) * ky).clamp(0.0, sh);
    let crop = json!({"x": cx / sw, "y": cy / sh,
        "w": ((side * kx).min(sw - cx)) / sw, "h": ((side * ky).min(sh - cy)) / sh});
    let place = json!({"x": (layer.place.x + (cx - layer.crop.x) / kx) / cw,
        "y": (layer.place.y + (cy - layer.crop.y) / ky) / ch, "w": ((side * kx).min(sw - cx) / kx) / cw});
    let style = match a.get("style").and_then(Value::as_str).unwrap_or("blur") {
        "pixelate" => json!({"pixelate": {"block": 20}}),
        "solid" => json!({"solid": {"colour": 0x0c1118}}),
        _ => json!({"blur": {"radius": 24}}),
    };
    let framing = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
    // on the stack the take shows (its layout, else the framing's), so every take with that
    // stack carries it and a pin follows it through them
    let doc = core.project(project)?.handle.snapshot().doc.clone();
    let fid = crate::planner::ulid_id::<gmclip_model::entities::Framing>(&framing)?;
    let layout = doc
        .get(crate::planner::ulid_id::<gmclip_model::entities::Part>(
            &ip.model,
        )?)
        .and_then(|p| p.look.get(&fid))
        .and_then(|l| l.layout_ref);
    let owner = match layout {
        Some(l) => json!({"layout": l.0.to_string()}),
        None => json!({"framing": framing}),
    };
    let out = core.call(
        project,
        "layer_add",
        json!({"owner": owner, "role": {"region": "private"}, "kind": "cover", "crop": crop,
               "place": place, "z": z, "cover": style}),
    )?;
    Ok(json!({"layer": out["output"]["id"]}))
}
