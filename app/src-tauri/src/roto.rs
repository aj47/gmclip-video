//! Cut-outs and the activity crop in the app (R.01–R.03): the matte status of a cut
//! (each matted part's quality from its accurate mask track, and the background build of
//! missing tracks), and "Follow the action" (the activity crop, applied as one undoable
//! `crop_follow_apply`). Turning a cut-out on and brushing its edges are the registry's
//! `matte_set` / `matte_brush`, called straight from the UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// A background build of a cut's accurate mask tracks.
#[derive(Debug)]
struct Build {
    total: u64,
    done: Arc<AtomicU64>,
    finished: AtomicBool,
    error: Mutex<Option<String>>,
    /// The tracks it builds, in build order: `(track key, frames)`.
    order: Vec<(String, u64)>,
    /// The plan revision it was started for.
    rev: u64,
    /// Started on its own (R.01's background build) rather than by the button.
    auto: bool,
    cancel: Arc<AtomicBool>,
}

impl Build {
    /// Frames of `key` built so far (the build goes track by track, in `order`).
    fn done_of(&self, key: &str) -> Option<u64> {
        let done = self.done.load(Ordering::Relaxed);
        let mut before = 0;
        for (k, n) in &self.order {
            if k == key {
                return Some(done.saturating_sub(before).min(*n));
            }
            before += n;
        }
        None
    }
}

/// Builds by `(project, cut)`.
type Builds = Mutex<HashMap<(String, String), Arc<Build>>>;

fn builds() -> &'static Builds {
    static B: OnceLock<Builds> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn build_json(b: Option<&Arc<Build>>) -> Value {
    match b {
        None => Value::Null,
        Some(b) => json!({
            "state": if b.error.lock().ok().is_some_and(|e| e.is_some()) {
                "failed"
            } else if b.finished.load(Ordering::Relaxed) {
                "done"
            } else {
                "running"
            },
            "done": b.done.load(Ordering::Relaxed),
            "total": b.total,
            "error": b.error.lock().ok().and_then(|e| e.clone()),
            "auto": b.auto,
        }),
    }
}

fn build_of(project: &str, cut: &str) -> Option<Arc<Build>> {
    builds()
        .lock()
        .ok()
        .and_then(|g| g.get(&(project.to_owned(), cut.to_owned())).cloned())
}

/// `matte_status {cut}`: every matted part/layer of the cut with its quality (from the
/// accurate track, soften and strokes applied) or `missing`, worst first, and the
/// background build's progress.
#[cfg(any(target_os = "macos", windows))]
pub fn status(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let planned = core.planned(project, cut)?;
    let store = gmclip_render::matte::store::shared();
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let rows =
        gmclip_render::export::mattes::quality_report(&planned.plan, &mut inputs, &store, false)?;
    let mut out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let model = planned.index.model_layer(&r.plan_part, &r.layer);
            json!({
                "plan_part": r.plan_part,
                "layer": r.layer,
                "part": model.map(|m| m.0),
                "model_layer": model.map(|m| m.1),
                "status": r.status,
                "quality": r.quality,
                "out": planned.plan.parts.iter().find(|p| p.id.as_str() == r.plan_part).map(|p| p.out.0),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let s = |v: &Value| v["quality"]["score"].as_f64().unwrap_or(-1.0);
        s(a).total_cmp(&s(b))
    });
    let b = builds()
        .lock()
        .ok()
        .and_then(|g| g.get(&(project.to_owned(), cut.to_owned())).cloned());
    Ok(json!({ "parts": out, "build": build_json(b.as_ref()) }))
}

/// `matte_progress {cut}`: the timeline's progress ticks (R.01) — per plan part with a
/// cut-out, its output frames and how many of its frames have an accurate mask track
/// (built, or built so far by the running build), and the build's state.
#[cfg(any(target_os = "macos", windows))]
pub fn progress(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    use gmclip_render::export::mattes::TrackQuality as SegQuality;
    let planned = core.planned(project, cut)?;
    let plan = &planned.plan;
    let store = gmclip_render::matte::store::shared();
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let needs = gmclip_render::export::mattes::track_needs(
        plan,
        &mut inputs,
        &store,
        SegQuality::Accurate,
    )?;
    let b = build_of(project, cut);
    let running = b.as_ref().filter(|b| !b.finished.load(Ordering::Relaxed));
    let mut parts: Vec<Value> = Vec::new();
    for p in &plan.parts {
        let id = p.id.as_str();
        let mine: Vec<_> = needs
            .iter()
            .filter(|n| n.layers.iter().any(|l| l.split('/').next() == Some(id)))
            .collect();
        if mine.is_empty() {
            continue;
        }
        // a part is as far along as its least built track
        let (mut done, mut total) = (u64::MAX, 0u64);
        for n in &mine {
            let d = if n.cached {
                n.span.1
            } else {
                running.and_then(|b| b.done_of(&n.key)).unwrap_or(0)
            };
            let frac_done = d.saturating_mul(p.out.len()) / n.span.1.max(1);
            done = done.min(frac_done);
            total = p.out.len();
        }
        parts.push(
            json!({"plan_part": id, "out": p.out.0, "done": done.min(total), "total": total}),
        );
    }
    Ok(json!({ "rev": planned.rev, "parts": parts, "build": build_json(b.as_ref()) }))
}

/// `matte_build {cut}`: builds the cut's missing accurate mask tracks on a background
/// thread (Apple Vision `.accurate`, ~11 frames/s on an M1 Pro); `matte_status` reports
/// progress. One build per cut at a time. Cut-outs also build on their own
/// ([`start_auto`]); this starts one now.
#[cfg(target_os = "macos")]
pub fn build(core: &Arc<AppCore>, project: &str, cut: &str) -> AppResult<Value> {
    build_with(core, project, cut, false)
}

/// Off macOS mask tracks are not built (Apple Vision); tracks built on a Mac are applied.
#[cfg(not(target_os = "macos"))]
pub fn build(_: &Arc<AppCore>, _: &str, _: &str) -> AppResult<Value> {
    Err(AppError::new(
        "Unsupported",
        "mask tracks are built with Apple Vision on a Mac; this PC applies them",
    ))
}

#[cfg(target_os = "macos")]
fn build_with(core: &Arc<AppCore>, project: &str, cut: &str, auto: bool) -> AppResult<Value> {
    use gmclip_render::export::mattes::TrackQuality as SegQuality;
    let key = (project.to_owned(), cut.to_owned());
    if let Some(b) = builds().lock().ok().and_then(|g| g.get(&key).cloned())
        && !b.finished.load(Ordering::Relaxed)
    {
        return Ok(json!({ "build": build_json(Some(&b)) }));
    }
    let planned = core.planned(project, cut)?;
    let rev = planned.rev;
    // a background build that failed is not retried for the same plan (a button press is)
    if auto
        && let Some(b) = builds().lock().ok().and_then(|g| g.get(&key).cloned())
        && b.rev == rev
        && b.error.lock().ok().is_some_and(|e| e.is_some())
    {
        return Ok(json!({ "build": build_json(Some(&b)) }));
    }
    let plan = planned.plan.clone();
    let store = gmclip_render::matte::store::shared();
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let needs = gmclip_render::export::mattes::track_needs(
        &plan,
        &mut inputs,
        &store,
        SegQuality::Accurate,
    )?;
    let total: u64 = needs.iter().filter(|n| !n.cached).map(|n| n.span.1).sum();
    if auto && total == 0 {
        return Ok(json!({ "build": Value::Null }));
    }
    let b = Arc::new(Build {
        total,
        done: Arc::new(AtomicU64::new(0)),
        finished: AtomicBool::new(total == 0),
        error: Mutex::new(None),
        order: needs
            .iter()
            .filter(|n| !n.cached)
            .map(|n| (n.key.clone(), n.span.1))
            .collect(),
        rev,
        auto,
        cancel: Arc::new(AtomicBool::new(false)),
    });
    if let Ok(mut g) = builds().lock() {
        g.insert(key, b.clone());
    }
    if total > 0 {
        let core = core.clone();
        let (project, cut) = (project.to_owned(), cut.to_owned());
        let b2 = b.clone();
        std::thread::Builder::new()
            .name("gmclip-mattes".into())
            .spawn(move || {
                // default QoS, not utility: Vision `.accurate` on a utility thread made no
                // progress at all under a loaded machine (load 30+, roto_auto)
                let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
                let r = gmclip_render::export::mattes::build_tracks(
                    &plan,
                    &mut inputs,
                    &store,
                    SegQuality::Accurate,
                    Some(&b2.done),
                    Some(&b2.cancel),
                );
                let ok = r.is_ok();
                if let Err(e) = r
                    && let Ok(mut g) = b2.error.lock()
                {
                    *g = Some(e.to_string());
                }
                b2.finished.store(true, Ordering::Relaxed);
                core.bus.viewer(crate::events::ViewerEvent::Mattes {
                    project: project.clone(),
                    cut: cut.clone(),
                    state: Some("done".into()),
                });
                // the cut may have changed while it built: look again
                if ok {
                    mark(&core, &project);
                }
            })
            .map_err(|_| AppError::internal("could not start the mask-track thread"))?;
    }
    Ok(json!({ "build": build_json(Some(&b)) }))
}

/// How long a project must be still after a commit before its cut-outs build on their
/// own (a crop drag or a trim settles first).
pub const AUTO_SETTLE: std::time::Duration = std::time::Duration::from_millis(1000);

/// Projects changed since their cut-outs were last looked at, and when, per app core.
type Dirty = Mutex<HashMap<(usize, String), std::time::Instant>>;

fn dirty() -> &'static Dirty {
    static D: OnceLock<Dirty> = OnceLock::new();
    D.get_or_init(Default::default)
}

fn core_key(core: &AppCore) -> usize {
    std::ptr::from_ref(core) as usize
}

/// Asks for a project's cut-outs to be looked at (after it settles).
pub fn mark(core: &AppCore, project: &str) {
    if let Ok(mut g) = dirty().lock() {
        g.insert(
            (core_key(core), project.to_owned()),
            std::time::Instant::now(),
        );
    }
}

/// The cuts of a project with a cut-out (a roto matte on a layer or in a part's override).
pub fn matted_cuts(doc: &gmclip_model::Doc) -> Vec<String> {
    use gmclip_model::entities::{Framing, Layer, LayerOwner, Layout, Matte, Part};
    let mut cuts = std::collections::BTreeSet::new();
    for l in doc.iter::<Layer>() {
        if !matches!(l.matte, Matte::Roto { .. }) {
            continue;
        }
        let cut = match l.owner {
            LayerOwner::Layout(x) => doc.get::<Layout>(x).and_then(|x| x.cut),
            LayerOwner::Framing(f) => doc.get::<Framing>(f).map(|f| f.cut),
            LayerOwner::PartOverride { part, .. } => doc.get::<Part>(part).map(|p| p.cut),
            LayerOwner::Clip(_) => None,
        };
        cuts.extend(cut);
    }
    for p in doc.iter::<Part>() {
        if p.look.values().any(|lk| {
            lk.overrides
                .values()
                .any(|o| matches!(o.matte, Some(Matte::Roto { .. })))
        }) {
            cuts.insert(p.cut);
        }
    }
    // a video overlay cut out (decision 0042)
    for o in doc.iter::<gmclip_model::entities::Overlay>() {
        if o.video
            .as_ref()
            .is_some_and(|v| matches!(v.matte, Some(Matte::Roto { .. })))
        {
            cuts.extend(match o.parent {
                gmclip_model::entities::OverlayParent::Cut(c) => Some(c),
                gmclip_model::entities::OverlayParent::Clip(c) => {
                    doc.get::<gmclip_model::entities::Clip>(c).map(|c| c.cut)
                }
            });
        }
    }
    cuts.into_iter().map(|c| c.0.to_string()).collect()
}

/// R.01: accurate mask tracks build in the background on their own. Every commit marks
/// its project; once it has been still for [`AUTO_SETTLE`], each of its cuts with a
/// cut-out starts a build of the tracks it lacks (one per cut; a failed one is not
/// retried for the same plan). Progress is `matte_progress`, drawn as ticks on the
/// timeline. Stops when the app does ([`stop_all`]).
pub fn start_auto(core: &Arc<AppCore>) {
    let weak = Arc::downgrade(core);
    {
        let weak = weak.clone();
        core.add_commit_hook(Box::new(move |p| {
            if let Some(c) = weak.upgrade() {
                mark(&c, p);
            }
        }));
    }
    let _ = std::thread::Builder::new()
        .name("gmclip-matte-auto".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let Some(core) = weak.upgrade() else { return };
                let me = core_key(&core);
                let due: Vec<(usize, String)> = dirty()
                    .lock()
                    .map(|mut g| {
                        let due: Vec<(usize, String)> = g
                            .iter()
                            .filter(|((c, _), t)| *c == me && t.elapsed() >= AUTO_SETTLE)
                            .map(|(k, _)| k.clone())
                            .collect();
                        for k in &due {
                            g.remove(k);
                        }
                        due
                    })
                    .unwrap_or_default();
                for (_, p) in due {
                    auto_project(&core, &p);
                }
            }
        });
}

#[cfg(target_os = "macos")]
fn auto_project(core: &Arc<AppCore>, project: &str) {
    let Ok(p) = core.project(project) else {
        return;
    };
    let doc = p.handle.snapshot().doc.clone();
    for cut in matted_cuts(&doc) {
        if let Ok(v) = build_with(core, project, &cut, true)
            && !v["build"].is_null()
        {
            core.bus.viewer(crate::events::ViewerEvent::Mattes {
                project: project.to_owned(),
                cut: cut.clone(),
                state: v["build"]["state"].as_str().map(str::to_owned),
            });
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn auto_project(_: &Arc<AppCore>, _: &str) {}

/// Cancels every running mask-track build (the app is quitting).
pub fn stop_all() {
    if let Ok(g) = builds().lock() {
        for b in g.values() {
            b.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Reads frames for the activity crop; nothing else of the media seam.
#[derive(Debug)]
pub(crate) struct Frames;

impl gmclip_core::MediaRender for Frames {
    fn name(&self) -> &'static str {
        "app-frames"
    }
    fn see(
        &self,
        _: &gmclip_plan::Plan,
        _: &gmclip_core::host::SeeRequest,
    ) -> Result<gmclip_core::host::SeeImage, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("frames only"))
    }
    fn export(
        &self,
        _: &gmclip_plan::Plan,
        _: &std::path::Path,
        _: Arc<AtomicBool>,
    ) -> Result<gmclip_core::host::ExportDone, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("frames only"))
    }
    fn verify(
        &self,
        _: &gmclip_plan::Plan,
        _: &std::path::Path,
    ) -> Result<Value, gmclip_core::host::HostError> {
        Err(gmclip_core::host::HostError::Unsupported("frames only"))
    }
    #[cfg(any(target_os = "macos", windows))]
    fn luma_frames(
        &self,
        plan: &gmclip_plan::Plan,
        source: &str,
        frames: &[u64],
        max_w: u32,
    ) -> Result<Vec<gmclip_core::activity::LumaFrame>, gmclip_core::host::HostError> {
        let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
        gmclip_render::export::mattes::luma_frames(plan, &mut inputs, source, frames, max_w)
            .map(|v| {
                v.into_iter()
                    .map(|(width, height, data)| gmclip_core::activity::LumaFrame {
                        width,
                        height,
                        data,
                    })
                    .collect()
            })
            .map_err(|e| {
                gmclip_core::host::HostError::Failed(e.to_string().chars().take(120).collect())
            })
    }
}

/// `follow_action {cut, part?, dry_run?}`: the activity crop of the cut's screen layers
/// (or one part's), applied as one undoable transaction unless `dry_run`.
pub fn follow_action(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: Option<&str>,
    dry_run: bool,
) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, Part};
    let p = core.project(project)?;
    let snap = p.handle.snapshot();
    let cid = crate::planner::ulid_id::<Cut>(cut)?;
    let pid = part.map(crate::planner::ulid_id::<Part>).transpose()?;
    let framing = core.active_framing(project, cut);
    let planner = gmclip_core::host::ModelPlanner(core.plan_host());
    let f =
        gmclip_mcp::matte::follow(&planner, &Frames, &snap, cid, framing.as_deref(), pid, None)?;
    let mut v = json!({
        "parts": f.listed(),
        "skipped": f.skipped,
        "splits": f.splits(),
        "dry_run": dry_run,
    });
    if !dry_run && !f.parts.is_empty() {
        let r = core.call(project, "crop_follow_apply", f.apply_args())?;
        v["committed"] = r["committed"].clone();
    }
    Ok(v)
}
