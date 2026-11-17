//! Scrub proxies in the app (H.05; SPEC §6.3): when a project opens or a video is added,
//! every video source gets a proxy built in the background (`gmclip_render::export::proxy`:
//! VideoToolbox, ≤ 640 px, GOP 4, no B-frames), one at a time, in
//! `<OS cache>/gmclip/<project id>/proxies/<content key>/`. The viewer scrubs from it
//! where it covers the playhead and from the master's keyframes elsewhere; playback and
//! paused frames always come from the master. Thumbnails for the timeline's filmstrip
//! are read from it too (`gm://data/<project>/filmstrip`).
//!
//! Progress goes out on the project's scope (`kind: proxy`) after every segment (a
//! minute of source).
//! `GMCLIP_NO_PROXY=1` turns building off (tests that do not want the load).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use gmclip_model::entities::Source;
use serde::Serialize;
use serde_json::{Value, json};

use crate::core::AppCore;

/// One source's proxy.
#[derive(Clone, Debug, Serialize)]
pub struct ProxyJob {
    pub project: String,
    pub source: String,
    pub path: String,
    /// The content key, once read.
    pub key: Option<String>,
    pub dir: Option<PathBuf>,
    /// `queued`, `building`, `done`, `failed`, `stopped`.
    pub phase: String,
    /// Source frames the proxy covers, and the source's frames.
    pub covered: u64,
    pub frames: u64,
    pub seconds: f64,
    pub error: Option<String>,
}

#[derive(Default)]
struct Inner {
    jobs: Mutex<BTreeMap<String, ProxyJob>>,
    queue: Mutex<VecDeque<String>>,
    wake: Condvar,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
    /// Frames covered by the running build (read live by `status`).
    live: Arc<AtomicU64>,
    /// Filmstrip thumbnails: `(proxy dir, proxy keyframe, width)` → `(w, h, rgb)`; a
    /// master's keyframe is keyed `(master:<path>, its decode index, width)`.
    thumbs: Mutex<HashMap<(PathBuf, u64, u32), Thumb>>,
    /// Masters read for the filmstrip before their proxy covers them, kept open.
    #[cfg(any(target_os = "macos", windows))]
    masters: Mutex<HashMap<String, gmclip_render::export::proxy::MasterThumbs>>,
}

/// One RGB thumbnail.
type Thumb = Arc<(u32, u32, Vec<u8>)>;

/// Thumbnails kept (≈ 15 KB each at 96 px).
const THUMB_CACHE: usize = 3000;
/// Thumbnails per filmstrip request at most (`every` grows to fit).
const THUMBS_MAX: u64 = 400;
/// Masters kept open for filmstrip keyframes.
const MASTERS_OPEN: usize = 4;
/// Time one filmstrip request spends decoding masters' keyframes; the rest come back
/// empty and arrive on the next request (cached keyframes cost nothing).
const MASTER_BUDGET: std::time::Duration = std::time::Duration::from_millis(1200);

/// The app's proxy builder: one background worker, jobs keyed by source path.
#[derive(Default, Clone)]
pub struct Proxies {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Proxies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxies").finish_non_exhaustive()
    }
}

/// `<OS cache>/gmclip` for the app data folder in use (a test's throwaway HOME gets its
/// own); `<app data>/cache` for a layout that is not the standard one.
pub fn cache_root(app_data: &Path) -> PathBuf {
    let std_suffix = if cfg!(target_os = "macos") {
        "Library/Application Support/gmclip"
    } else {
        ".local/share/gmclip"
    };
    let cache = if cfg!(target_os = "macos") {
        "Library/Caches/gmclip"
    } else {
        ".cache/gmclip"
    };
    match app_data.to_str().and_then(|s| s.strip_suffix(std_suffix)) {
        Some(home) => PathBuf::from(home).join(cache),
        None => app_data.join("cache"),
    }
}

fn disabled() -> bool {
    std::env::var_os("GMCLIP_NO_PROXY").is_some_and(|v| !v.is_empty() && v != "0")
}

fn is_video(s: &Source) -> bool {
    s.probe.tracks.iter().any(|t| t.width.is_some())
}

impl Proxies {
    /// Queues a proxy for every video source of `project` that has none (or a stopped
    /// or failed one).
    pub fn ensure(&self, core: &Arc<AppCore>, project: &str) {
        if disabled() {
            return;
        }
        let Ok(p) = core.project(project) else { return };
        let doc = p.handle.snapshot().doc.clone();
        let mut added = false;
        for s in doc.iter::<Source>().filter(|s| is_video(s)) {
            if !Path::new(&s.path).is_file() {
                continue;
            }
            let Ok(mut jobs) = self.inner.jobs.lock() else {
                return;
            };
            if jobs
                .get(&s.path)
                .is_some_and(|j| !matches!(j.phase.as_str(), "failed" | "stopped"))
            {
                continue;
            }
            jobs.insert(
                s.path.clone(),
                ProxyJob {
                    project: project.to_owned(),
                    source: s.id.0.to_string(),
                    path: s.path.clone(),
                    key: None,
                    dir: None,
                    phase: "queued".into(),
                    covered: 0,
                    frames: 0,
                    seconds: 0.0,
                    error: None,
                },
            );
            drop(jobs);
            if let Ok(mut q) = self.inner.queue.lock() {
                q.push_back(s.path.clone());
            }
            added = true;
        }
        if added {
            self.start_worker(core);
            self.inner.wake.notify_all();
        }
    }

    fn start_worker(&self, core: &Arc<AppCore>) {
        let Ok(mut w) = self.inner.worker.lock() else {
            return;
        };
        if w.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let inner = self.inner.clone();
        let core = Arc::downgrade(core);
        *w = std::thread::Builder::new()
            .name("gmclip-proxy".into())
            .spawn(move || worker(&inner, &core))
            .ok();
    }

    /// Proxy folders by source path, for every source whose key is known.
    pub fn dirs(&self) -> HashMap<String, PathBuf> {
        self.inner
            .jobs
            .lock()
            .map(|g| {
                g.values()
                    .filter_map(|j| j.dir.clone().map(|d| (j.path.clone(), d)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The proxy folder of a source path, once its key is known.
    pub fn dir_of(&self, path: &str) -> Option<PathBuf> {
        self.inner
            .jobs
            .lock()
            .ok()?
            .get(path)
            .and_then(|j| j.dir.clone())
    }

    /// Source frames the proxy of `path` covers while it is still queued or building;
    /// `None` when it is done, failed, stopped or never asked for.
    pub fn building(&self, path: &str) -> Option<u64> {
        let live = self.inner.live.load(Ordering::Relaxed);
        let g = self.inner.jobs.lock().ok()?;
        let j = g.get(path)?;
        match j.phase.as_str() {
            "queued" => Some(j.covered),
            "building" => Some(j.covered.max(live)),
            _ => None,
        }
    }

    /// Every job, with the running one's live coverage.
    pub fn status(&self, project: Option<&str>) -> Value {
        let live = self.inner.live.load(Ordering::Relaxed);
        let jobs: Vec<ProxyJob> = self
            .inner
            .jobs
            .lock()
            .map(|g| {
                g.values()
                    .filter(|j| project.is_none_or(|p| j.project == p))
                    .cloned()
                    .map(|mut j| {
                        if j.phase == "building" {
                            j.covered = j.covered.max(live);
                        }
                        j
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({"jobs": jobs})
    }

    /// Stops the running build (its finished segments stay) and the worker.
    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.inner.wake.notify_all();
        let h = self.inner.worker.lock().ok().and_then(|mut w| w.take());
        if let Some(h) = h {
            let _ = h.join();
        }
    }
}

fn set(inner: &Inner, path: &str, f: impl FnOnce(&mut ProxyJob)) -> Option<ProxyJob> {
    let mut g = inner.jobs.lock().ok()?;
    let j = g.get_mut(path)?;
    f(j);
    Some(j.clone())
}

fn publish(core: &Weak<AppCore>, j: &ProxyJob) {
    if let Some(c) = core.upgrade() {
        c.bus.project(
            &j.project,
            crate::events::ProjectEvent::Proxy {
                project: j.project.clone(),
                source: j.source.clone(),
                phase: j.phase.clone(),
                covered: j.covered,
                frames: j.frames,
            },
        );
    }
}

fn worker(inner: &Arc<Inner>, core: &Weak<AppCore>) {
    loop {
        let next = {
            let Ok(mut q) = inner.queue.lock() else {
                return;
            };
            loop {
                if inner.stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(p) = q.pop_front() {
                    break p;
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
        build_one(inner, core, &next);
    }
}

#[cfg(any(target_os = "macos", windows))]
fn build_one(inner: &Arc<Inner>, core: &Weak<AppCore>, path: &str) {
    use gmclip_render::export::proxy;
    let Some(job) = set(inner, path, |j| j.phase = "building".into()) else {
        return;
    };
    let Some(c) = core.upgrade() else { return };
    let root = cache_root(&c.dirs.app_data)
        .join(&job.project)
        .join("proxies");
    drop(c);
    let key = match proxy::content_key(Path::new(path)) {
        Ok(k) => k,
        Err(e) => {
            if let Some(j) = set(inner, path, |j| {
                j.phase = "failed".into();
                j.error = Some(format!("reading the file ({})", e.kind()));
            }) {
                publish(core, &j);
            }
            return;
        }
    };
    let dir = root.join(&key);
    if let Err(e) = gmclip_mcp::paths::mkdir_private(&dir) {
        if let Some(j) = set(inner, path, |j| {
            j.phase = "failed".into();
            j.error = Some(format!("the cache folder ({})", e.kind()));
        }) {
            publish(core, &j);
        }
        return;
    }
    let covered = proxy::Manifest::read(&dir)
        .filter(|m| m.key == key)
        .map_or(0, |m| m.covered());
    if let Some(j) = set(inner, path, |j| {
        j.key = Some(key.clone());
        j.dir = Some(dir.clone());
        j.covered = covered;
    }) {
        publish(core, &j);
    }
    inner.live.store(covered, Ordering::Relaxed);
    let opts = proxy::BuildOptions {
        cancel: Some(inner.stop.clone()),
        progress: Some(inner.live.clone()),
        until: None,
        on_segment: Some(Arc::new({
            let inner = inner.clone();
            let core = core.clone();
            let path = path.to_owned();
            move |m: &proxy::Manifest| {
                if let Some(j) = set(&inner, &path, |j| {
                    j.covered = m.covered();
                    j.frames = m.source_frames;
                }) {
                    publish(&core, &j);
                }
            }
        })),
    };
    let r = proxy::build(Path::new(path), &dir, &key, &opts);
    let j = set(inner, path, |j| match &r {
        Ok(rep) => {
            j.frames = rep.frames;
            j.seconds = rep.seconds;
            if rep.cancelled {
                j.phase = "stopped".into();
            } else {
                j.phase = "done".into();
                j.covered = rep.frames;
            }
        }
        Err(e) => {
            j.phase = "failed".into();
            j.error = Some(e.to_string());
        }
    });
    if let Some(j) = j {
        publish(core, &j);
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn build_one(inner: &Arc<Inner>, core: &Weak<AppCore>, path: &str) {
    // Windows/Linux encoders: M5/M6 (SPEC §6.3's Linux image-sequence fallback).
    if let Some(j) = set(inner, path, |j| {
        j.phase = "failed".into();
        j.error = Some("scrub proxies are macOS-only for now".into());
    }) {
        publish(core, &j);
    }
}

/// `gm://data/<project>/filmstrip?cut=<ulid>&every=<n>&w=<px>` (H.05): thumbnails of the
/// cut's picture every `every` output frames (default 30; grown so that at most
/// [`THUMBS_MAX`] come back), `w` wide (default 96), read from the scrub proxy at its
/// keyframe at or before each frame — or, where the proxy does not cover the frame yet,
/// from the master's keyframe at or before it (lane K5: a 3 h recording has a filmstrip
/// before its proxy exists; decoding masters is capped at [`MASTER_BUDGET`] a request,
/// keyframes cached). Body after the 16-byte header (kind 3, count, plan rev): per
/// thumbnail `out_frame u32, w u16, h u16` then `w × h × 3` RGB bytes; `w = h = 0` where
/// neither has a picture yet (the UI asks again on the next `proxy` event).
#[cfg(any(target_os = "macos", windows))]
pub fn filmstrip(
    core: &AppCore,
    pid: &str,
    cut: &str,
    every: Option<u32>,
    width: Option<u32>,
) -> Option<Vec<u8>> {
    use gmclip_render::export::proxy::ProxyReader;
    let pl = core.planned(pid, cut).ok()?;
    let plan = &pl.plan;
    let frames = plan.output.frames;
    let mut every = u64::from(every.unwrap_or(30).max(1));
    every = every.max(frames.div_ceil(THUMBS_MAX));
    let w = width.unwrap_or(96);
    let dirs = core.proxies.dirs();
    let started = std::time::Instant::now();
    let mut readers: HashMap<String, Option<ProxyReader>> = HashMap::new();
    let mut body = Vec::new();
    let mut count = 0u32;
    let mut f = 0u64;
    while f < frames {
        let thumb = (|| {
            let part = plan
                .parts
                .iter()
                .find(|p| p.out.start() <= f && f < p.out.end())?;
            let src = plan.sources.iter().find(|s| s.id == part.picture.source)?;
            let sf =
                gmclip_plan::retime::nominal_frame(part, f - part.out.start(), plan.output.fps);
            let covered = dirs.get(&src.path).and_then(|dir| {
                readers
                    .entry(src.path.clone())
                    .or_insert_with(|| ProxyReader::open(dir))
                    .as_mut()
                    .map(|r| r.covers(sf))
            });
            if covered != Some(true) {
                // no proxy here yet: the master's keyframe
                return master_thumb(core, &src.path, sf, w, started);
            }
            let dir = dirs.get(&src.path)?;
            let r = readers.get_mut(&src.path)?.as_mut()?;
            let key = (dir.clone(), r.keyframe_of(sf), w);
            if let Some(t) = core.proxies.inner.thumbs.lock().ok()?.get(&key) {
                return Some(t.clone());
            }
            let t: Thumb = Arc::new(r.thumbnail(sf, w).ok()?);
            if let Ok(mut g) = core.proxies.inner.thumbs.lock() {
                if g.len() >= THUMB_CACHE {
                    g.clear();
                }
                g.insert(key, t.clone());
            }
            Some(t)
        })();
        body.extend_from_slice(&(f.min(u64::from(u32::MAX)) as u32).to_le_bytes());
        match thumb {
            Some(t) => {
                body.extend_from_slice(&(t.0 as u16).to_le_bytes());
                body.extend_from_slice(&(t.1 as u16).to_le_bytes());
                body.extend_from_slice(&t.2);
            }
            None => body.extend_from_slice(&[0, 0, 0, 0]),
        }
        count += 1;
        f += every;
    }
    let mut out = Vec::with_capacity(16 + body.len());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&pl.rev.to_le_bytes());
    out.extend_from_slice(&body);
    Some(out)
}

/// The filmstrip picture of source frame `sf` of `path` from its master's keyframe (cached
/// by keyframe; decoded only while the request is inside [`MASTER_BUDGET`]).
#[cfg(any(target_os = "macos", windows))]
fn master_thumb(
    core: &AppCore,
    path: &str,
    sf: u64,
    w: u32,
    started: std::time::Instant,
) -> Option<Thumb> {
    use gmclip_render::export::proxy::MasterThumbs;
    let mut open = core.proxies.inner.masters.lock().ok()?;
    if !open.contains_key(path) {
        if started.elapsed() >= MASTER_BUDGET {
            return None;
        }
        if open.len() >= MASTERS_OPEN {
            open.clear();
        }
        open.insert(path.to_owned(), MasterThumbs::open(Path::new(path)).ok()?);
    }
    let m = open.get_mut(path)?;
    let key = (
        PathBuf::from(format!("master:{path}")),
        m.keyframe_of(sf)?,
        w,
    );
    if let Some(t) = core.proxies.inner.thumbs.lock().ok()?.get(&key) {
        return Some(t.clone());
    }
    if started.elapsed() >= MASTER_BUDGET {
        return None;
    }
    let t: Thumb = Arc::new(m.thumbnail(sf, w).ok()?);
    if let Ok(mut g) = core.proxies.inner.thumbs.lock() {
        if g.len() >= THUMB_CACHE {
            g.clear();
        }
        g.insert(key, t.clone());
    }
    Some(t)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn filmstrip(
    _core: &AppCore,
    _pid: &str,
    _cut: &str,
    _every: Option<u32>,
    _width: Option<u32>,
) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_root_follows_app_data() {
        if cfg!(target_os = "macos") {
            assert_eq!(
                cache_root(Path::new("/Users/x/Library/Application Support/gmclip")),
                PathBuf::from("/Users/x/Library/Caches/gmclip")
            );
        }
        assert_eq!(
            cache_root(Path::new("/tmp/t/appdata")),
            PathBuf::from("/tmp/t/appdata/cache")
        );
    }
}
