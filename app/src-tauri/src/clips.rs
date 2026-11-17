//! Moment clips (H.10; SPEC §5.2, §5.14.1): a moment's review span — its excerpt's first
//! word to its last word + 1 s, or its whole span when the excerpt cannot be placed —
//! rendered to a small MP4 (≤ 720p) with its own sound, for the review link (the phone's
//! review mode) and for anything else that wants the moment as a file.
//!
//! - **Picture** from the scrub proxy (H.05) when it covers the span: its coded samples
//!   are copied, not re-encoded (`gmclip_render::export::clip`; 640 px wide, GOP 4, so the
//!   clip starts on the proxy keyframe at or before the first word — at most 3 frames
//!   early). Else from the master, rendered by the export pipeline into 1280 × 720. The
//!   sound is the master's, mixed and encoded as an export does.
//! - **Cached** in `<OS cache>/gmclip/<project id>/clips/` as `<moment id>.mp4` plus
//!   `<moment id>.clip.json`, keyed by `(moment, asr_revision, span)`: a new transcript
//!   or an edited excerpt makes a new clip; anything else reuses the file. The record
//!   carries the file's size and SHA-256, which the review link re-checks before serving
//!   (R-REV-2) — the same layout `gmclip review serve --clips` reads.
//! - One at a time on a background worker: review mode queues the moment it shows and the
//!   next ones, the review link queues the moments it lists; `moment_clip` makes one now.
//!   A queued moment whose picture's proxy is still being built waits for it.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

use gmclip_model::entities::{Moment, Source};
use gmclip_model::{Flicks, Id};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

/// `<moment>.clip.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipRecord {
    pub schema: String,
    pub moment: String,
    /// The transcript revision the span was placed with (`""` without one).
    pub asr_revision: String,
    /// The span on the moment's media clock, milliseconds `[start, end)`.
    pub span_ms: [i64; 2],
    pub size: u64,
    pub sha256: String,
    /// `proxy` (the scrub proxy's samples, copied), `proxy-rendered` (rendered from the
    /// proxy: the copy was refused) or `master` (rendered from the master).
    pub picture: String,
    pub w: u32,
    pub h: u32,
    pub frames: u64,
    pub render_ms: u64,
}

pub const SCHEMA: &str = "gmclip.moment-clip/0";
/// Clips render at most this size (SPEC §5.14.1: ≤ 720p).
pub const MAX_BOX: (u32, u32) = (1280, 720);

/// What a clip is made from, decided before rendering (the cache key's inputs).
#[derive(Clone, Debug)]
struct Want {
    moment: String,
    asr_revision: String,
    span: (Flicks, Flicks),
    span_ms: [i64; 2],
}

/// The project's clip folder.
pub fn clip_dir(core: &AppCore, project: &str) -> PathBuf {
    crate::proxy::cache_root(&core.dirs.app_data)
        .join(project)
        .join("clips")
}

fn want(core: &AppCore, project: &str, moment: &str) -> AppResult<(Want, Moment)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let id: Id<Moment> = crate::planner::ulid_id(moment)?;
    let m = doc
        .get(id)
        .ok_or_else(|| AppError::not_found("no such moment"))?
        .clone();
    let owner = crate::review::owner_of(&m);
    let lt = core.transcripts.latest_rev(&p, &owner);
    let (_, review) =
        crate::review::spans(&doc, &m, lt.as_ref().map(|x| &*x.1)).ok_or_else(|| {
            AppError::new(
                "Unsupported",
                "this moment's times need its transcript (line anchors)",
            )
        })?;
    let ms = |f: Flicks| (i128::from(f.0) * 1000 / i128::from(FLICKS)) as i64;
    Ok((
        Want {
            moment: moment.to_owned(),
            asr_revision: lt.map(|x| x.0).unwrap_or_default(),
            span: review,
            span_ms: [ms(review.0), ms(review.1)],
        },
        m,
    ))
}

/// The cached clip of `moment` when it is current (same transcript revision and span,
/// file present with the recorded size).
pub fn cached(core: &AppCore, project: &str, moment: &str) -> Option<(ClipRecord, PathBuf)> {
    let (w, _) = want(core, project, moment).ok()?;
    current(&clip_dir(core, project), &w)
}

fn current(dir: &Path, w: &Want) -> Option<(ClipRecord, PathBuf)> {
    if !w.moment.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let rec: ClipRecord =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{}.clip.json", w.moment))).ok()?)
            .ok()?;
    let mp4 = dir.join(format!("{}.mp4", w.moment));
    let size = std::fs::symlink_metadata(&mp4).ok()?.len();
    (rec.schema == SCHEMA
        && rec.moment == w.moment
        && rec.asr_revision == w.asr_revision
        && rec.span_ms == w.span_ms
        && rec.size == size)
        .then_some((rec, mp4))
}

/// Whether `moment`'s clip should wait for the scrub proxy: its picture source's proxy is
/// still being built and does not cover the span yet (a background render would decode
/// the master meanwhile, then the proxy would make it again for a fraction of the cost).
fn waits_for_proxy(core: &AppCore, project: &str, moment: &str) -> bool {
    let Ok((w, m)) = want(core, project, moment) else {
        return false;
    };
    let Ok(p) = core.project(project) else {
        return false;
    };
    let doc = p.handle.snapshot().doc.clone();
    let Some((pic, tp, _, _)) = crate::review::media_at(&doc, m.media, w.span.0.0) else {
        return false;
    };
    let Some(ps) = doc.get(pic) else { return false };
    let Ok((_, _, fps)) = crate::review::picture_info(ps) else {
        return false;
    };
    let fpf = crate::review::picture_rate(ps)
        .map_or(FLICKS / fps.max(1), |r| r.flicks_per_frame() as i64);
    let end = (tp + fpf / 2).div_euclid(fpf).max(0) as u64
        + ((w.span.1.0 - w.span.0.0 + fpf / 2) / fpf).max(1) as u64;
    core.proxies.building(&ps.path).is_some_and(|c| c < end)
}

/// The clip of `moment`, rendered now unless a current one is cached; `true` when it was
/// rendered by this call.
pub fn ensure(
    core: &AppCore,
    project: &str,
    moment: &str,
) -> AppResult<(ClipRecord, PathBuf, bool)> {
    let (w, m) = want(core, project, moment)?;
    let dir = clip_dir(core, project);
    if let Some((rec, mp4)) = current(&dir, &w) {
        return Ok((rec, mp4, false));
    }
    gmclip_mcp::paths::mkdir_private(&dir)
        .map_err(|e| AppError::internal(format!("the clip folder ({})", e.kind())))?;
    let (rec, mp4) = render(core, project, &m, &w, &dir)?;
    Ok((rec, mp4, true))
}

/// The clip's RenderPlan: the span's picture (proxy segments or the master) and the
/// master's sound, `(plan, picture kind)`.
fn clip_plan(
    core: &AppCore,
    project: &str,
    m: &Moment,
    w: &Want,
) -> AppResult<(gmclip_plan::Plan, &'static str)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let (t0, t1) = w.span;
    let (pic, tp, aud, ta) = crate::review::media_at(&doc, m.media, t0.0)
        .ok_or_else(|| AppError::not_found("the stream is not in this project"))?;
    let ps: &Source = doc
        .get(pic)
        .ok_or_else(|| AppError::not_found("no picture source"))?;
    let asrc: &Source = doc
        .get(aud)
        .ok_or_else(|| AppError::not_found("no audio source"))?;
    let (sw, sh, int_fps) = crate::review::picture_info(ps)?;
    // the picture's own rate (NTSC exactly, decision 0020), so index addressing cannot
    // drift from the sound
    let rate = crate::review::picture_rate(ps).unwrap_or_else(|| {
        gmclip_plan::types::Fps::new(int_fps.max(1) as u64, 1)
            .unwrap_or_else(|_| gmclip_plan::types::Fps::integer(30))
    });
    let (fps_json, schema) = crate::review::rate_json(rate);
    let fps = rate.rounded() as i64;
    let fpf = rate.flicks_per_frame() as i64;
    let mut first = (tp + fpf / 2).div_euclid(fpf).max(0) as u64;
    let mut n = ((t1.0 - t0.0 + fpf / 2) / fpf).clamp(1, 30 * 60 * FLICKS / fpf) as u64;
    let mut first_sample = (i128::from(ta.max(0)) * 48_000 / i128::from(FLICKS)) as i64;
    // S(f) on this rate (rule 4; NTSC rounds down)
    let s_at = |f: u64| gmclip_plan::validate::out_samples(f, rate).unwrap_or(0);
    // the proxy, when it covers every frame of the span
    #[cfg(any(target_os = "macos", windows))]
    let proxy = core.proxies.dir_of(&ps.path).and_then(|d| {
        let m = gmclip_render::export::proxy::Manifest::read(&d)?;
        (m.covered() >= first + n && u64::from(m.source_w) == u64::from(sw)).then_some((d, m))
    });
    #[cfg(not(any(target_os = "macos", windows)))]
    let proxy: Option<(PathBuf, ())> = None;
    let mut sources = vec![json!({"id": "aud", "path": asrc.path})];
    let mut parts = Vec::new();
    let (mut cw, mut ch, mut kind) = (2u32, 2u32, "master");
    #[cfg(any(target_os = "macos", windows))]
    if let Some((dir, man)) = &proxy {
        (cw, ch, kind) = (man.w, man.h, "proxy");
        // start on the proxy keyframe at or before the first frame (the samples are
        // copied, and a clip starts with a keyframe), the sound moved with it
        let mut back = first % u64::from(man.gop.max(1));
        while back > 0 && s_at(back) > first_sample.max(0) as u64 {
            back -= 1;
        }
        first -= back;
        n += back;
        first_sample -= s_at(back) as i64;
        // one part per proxy segment the span crosses
        let mut f = first;
        let mut out = 0u64;
        while out < n {
            let seg = f / man.seg_frames;
            let in_seg = f - seg * man.seg_frames;
            let len = (man.seg_frames - in_seg).min(n - out);
            let id = format!("seg{seg}");
            if !sources.iter().any(|s| s["id"] == id.as_str()) {
                sources.push(
                    json!({"id": id, "path": gmclip_render::export::proxy::segment_path(dir, seg)}),
                );
            }
            parts.push(json!({
                "id": format!("p{}", parts.len()),
                "out": [out, out + len],
                "picture": {"source": id, "first_frame": in_seg},
                "audio": {"source": "aud", "first_sample": first_sample + s_at(out) as i64, "rate": 48000},
                "layers": [{"id": "stream", "role": "other",
                            "crop": {"x": 0, "y": 0, "w": man.w, "h": man.h},
                            "place": {"x": 0, "y": 0, "w": cw, "h": ch}, "z": 0}]
            }));
            out += len;
            f += len;
        }
    }
    if parts.is_empty() {
        let s = (f64::from(MAX_BOX.0) / f64::from(sw))
            .min(f64::from(MAX_BOX.1) / f64::from(sh))
            .min(1.0);
        let even = |v: f64| ((v.round() as u32) & !1).max(2);
        (cw, ch, kind) = (even(f64::from(sw) * s), even(f64::from(sh) * s), "master");
        sources.push(json!({"id": "pic", "path": ps.path}));
        parts.push(json!({
            "id": "p0",
            "out": [0, n],
            "picture": {"source": "pic", "first_frame": first},
            "audio": {"source": "aud", "first_sample": first_sample, "rate": 48000},
            "layers": [{"id": "stream", "role": "other",
                        "crop": {"x": 0, "y": 0, "w": sw, "h": sh},
                        "place": {"x": 0, "y": 0, "w": cw, "h": ch}, "z": 0}]
        }));
    }
    let _ = &proxy;
    let v = json!({
        "schema": schema,
        "meta": {"title": format!("moment {}", w.moment)},
        "output": {
            "canvas": {"w": cw, "h": ch},
            "fps": fps_json,
            "frames": n,
            "video": {"codec": "h264", "bframes": 0, "gop_frames": fps,
                      "rate": {"mode": "vbr", "target_bps": 2_000_000, "max_bps": 4_000_000}},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}
        },
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": sources,
        "parts": parts,
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap_or_default()).map_err(|e| {
        AppError::invalid(format!(
            "the clip plan is invalid: {} ({})",
            e.reason(),
            e.path()
        ))
    })?;
    Ok((plan, kind))
}

#[cfg(any(target_os = "macos", windows))]
fn render(
    core: &AppCore,
    project: &str,
    m: &Moment,
    w: &Want,
    dir: &Path,
) -> AppResult<(ClipRecord, PathBuf)> {
    let t = Instant::now();
    let (plan, kind) = clip_plan(core, project, m, w)?;
    let mp4 = dir.join(format!("{}.mp4", w.moment));
    // a stale clip and its records go first (no superseded/ for clips)
    for f in [
        mp4.clone(),
        dir.join(format!("{}.clip.json", w.moment)),
        dir.join(format!("{}.export.json", w.moment)),
    ] {
        let _ = std::fs::remove_file(f);
    }
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    // from the proxy: copy its samples; anything the remux refuses is rendered instead
    let remuxed = match kind {
        "proxy" => gmclip_render::export::clip::remux_clip(&plan, &mut inputs, &mp4).ok(),
        _ => None,
    };
    let (frames, sha256, how) = match remuxed {
        Some(r) => (r.frames, r.sha256, "proxy"),
        None => {
            let rep = gmclip_render::export::export(
                &plan,
                &mut inputs,
                &mp4,
                &gmclip_render::export::ExportOptions::default(),
            )
            .map_err(|e| AppError::internal(format!("the clip did not render: {e}")))?;
            let er = gmclip_media::export_record::ExportRecord::read(&mp4)
                .map_err(|e| AppError::internal(format!("the clip's record ({})", e.kind())))?;
            (
                rep.frames,
                er.sha256,
                if kind == "proxy" {
                    "proxy-rendered"
                } else {
                    "master"
                },
            )
        }
    };
    let size = std::fs::symlink_metadata(&mp4)
        .map_err(|e| AppError::internal(format!("the clip ({})", e.kind())))?
        .len();
    let rec = ClipRecord {
        schema: SCHEMA.into(),
        moment: w.moment.clone(),
        asr_revision: w.asr_revision.clone(),
        span_ms: w.span_ms,
        size,
        sha256,
        picture: how.into(),
        w: plan.output.canvas.w.get() as u32,
        h: plan.output.canvas.h.get() as u32,
        frames,
        render_ms: t.elapsed().as_millis() as u64,
    };
    let tmp = dir.join(format!("{}.clip.json.tmp", w.moment));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&rec).unwrap_or_default())
        .and_then(|()| std::fs::rename(&tmp, dir.join(format!("{}.clip.json", w.moment))))
        .map_err(|e| AppError::internal(format!("the clip record ({})", e.kind())))?;
    Ok((rec, mp4))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn render(
    _core: &AppCore,
    _project: &str,
    _m: &Moment,
    _w: &Want,
    _dir: &Path,
) -> AppResult<(ClipRecord, PathBuf)> {
    Err(AppError::new(
        "Unsupported",
        "moment clips render on macOS only for now",
    ))
}

#[derive(Default)]
struct Inner {
    queue: Mutex<VecDeque<(String, String)>>,
    /// Queued or rendering, so a moment is never queued twice.
    pending: Mutex<BTreeSet<(String, String)>>,
    wake: Condvar,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// The background clip renderer (one at a time).
#[derive(Default, Clone)]
pub struct Clips {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Clips {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clips").finish_non_exhaustive()
    }
}

impl Clips {
    /// Queues clips for `moments` of `project` that have no current one.
    pub fn queue(&self, core: &Arc<AppCore>, project: &str, moments: &[String]) {
        let mut added = false;
        for m in moments {
            let key = (project.to_owned(), m.clone());
            if cached(core, project, m).is_some() {
                continue;
            }
            if let Ok(mut p) = self.inner.pending.lock()
                && p.insert(key.clone())
                && let Ok(mut q) = self.inner.queue.lock()
            {
                q.push_back(key);
                added = true;
            }
        }
        if !added {
            return;
        }
        if let Ok(mut w) = self.inner.worker.lock()
            && w.as_ref().is_none_or(|h| h.is_finished())
        {
            let inner = self.inner.clone();
            let core = Arc::downgrade(core);
            *w = std::thread::Builder::new()
                .name("gmclip-clips".into())
                .spawn(move || worker(&inner, &core))
                .ok();
        }
        self.inner.wake.notify_all();
    }

    /// Moments queued or rendering.
    pub fn pending(&self) -> usize {
        self.inner.pending.lock().map_or(0, |p| p.len())
    }
}

fn worker(inner: &Arc<Inner>, core: &Weak<AppCore>) {
    loop {
        let next = {
            let Ok(mut q) = inner.queue.lock() else {
                return;
            };
            loop {
                if let Some(k) = q.pop_front() {
                    break k;
                }
                match inner
                    .wake
                    .wait_timeout(q, std::time::Duration::from_secs(30))
                {
                    Ok((g, t)) if t.timed_out() && g.is_empty() => return,
                    Ok((g, _)) => q = g,
                    Err(_) => return,
                }
            }
        };
        let Some(c) = core.upgrade() else { return };
        // a moment whose proxy is still coming waits for it (behind the others)
        if waits_for_proxy(&c, &next.0, &next.1) {
            if let Ok(mut q) = inner.queue.lock() {
                q.push_back(next);
            }
            drop(c);
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        }
        let r = ensure(&c, &next.0, &next.1);
        if let Ok(mut p) = inner.pending.lock() {
            p.remove(&next);
        }
        let ev = match r {
            Ok((rec, _, _)) => crate::events::ProjectEvent::Clip {
                moment: next.1.clone(),
                ready: true,
                picture: Some(rec.picture),
                render_ms: Some(rec.render_ms),
                error: None,
            },
            Err(e) => crate::events::ProjectEvent::Clip {
                moment: next.1.clone(),
                ready: false,
                picture: None,
                render_ms: None,
                error: Some(e.message),
            },
        };
        c.bus.project(&next.0, ev);
    }
}
