//! Export jobs (SPEC §3.9, §5.14): the cut's plan → `gmclip_render::export` on its own
//! thread, one at a time (the machine-wide export semaphore's default of 1), progress on
//! the `jobs` bus scope every 250 ms, the finished file on the review board.
//!
//! R-ERR-2: a new export is refused when the disk has < 2 GB free or less than twice the
//! export's estimate, with the numbers. R-EXP-4: a failure names its step and frame.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::core::{AppCore, ExportEntry, now_ms, safe_name, sig_of};
use crate::error::{AppError, AppResult};

/// One export job as the UI sees it.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct JobView {
    pub id: String,
    pub kind: &'static str,
    pub project: String,
    pub cut: String,
    pub title: String,
    pub state: String,
    pub frames: u64,
    pub done: u64,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub error: Option<String>,
    pub export: Option<String>,
    /// `captions` (hearing the parts whose audio changed, SPEC §5.10), `audio` (mix,
    /// loudness, AAC: before the first video frame) or `video`.
    #[schemars(schema_with = "crate::events::job_phase")]
    pub phase: &'static str,
}

/// A finished export's plan and the file it wrote.
pub type Rendered = (Arc<gmclip_plan::Plan>, PathBuf);

struct Job {
    view: Mutex<JobView>,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    /// The plan it rendered and the file it wrote, once done (an agent's `verify`).
    done: Mutex<Option<Rendered>>,
}

/// The job list.
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<Vec<Arc<Job>>>,
    /// Held while an export runs (one at a time).
    running: Mutex<()>,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").finish_non_exhaustive()
    }
}

/// R-ERR-2 fault injection (dev bridge only, `dev_free_space`): the free space every
/// export check reads instead of the volume's (0 = the real statfs).
static FAKE_FREE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sets (or with `None` clears) the injected free space.
pub fn fake_free_space(bytes: Option<u64>) {
    FAKE_FREE.store(
        bytes.unwrap_or(0).max(u64::from(bytes.is_some())),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// R-ERR-2: a new export needs 2 GB free or twice its estimate, whichever is more; else
/// the message names both numbers.
pub fn space_check(free: u64, estimate: u64) -> Result<(), String> {
    let need = estimate.saturating_mul(2).max(2_000_000_000);
    if free >= need {
        return Ok(());
    }
    Err(format!(
        "Not enough free space to export: {} free, this export needs {} (2 GB or twice its ~{} estimate).",
        gb(free),
        gb(need),
        gb(estimate)
    ))
}

/// Bytes free on the volume holding `p` (its nearest existing ancestor).
pub fn free_bytes(p: &Path) -> Option<u64> {
    match FAKE_FREE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {}
        n => return Some(n),
    }
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.exists() {
            #[cfg(unix)]
            return rustix::fs::statvfs(c)
                .ok()
                .map(|s| s.f_bavail.saturating_mul(s.f_frsize));
            #[cfg(windows)]
            return gmclip_platform::windows::disk_free_bytes(c).ok();
        }
        cur = c.parent();
    }
    None
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

/// An export's file name (without `.mp4`) from its cut's view: the title, and when the
/// cut has several framings the one exported ("The demo - face big", "… - 9x16"), so two
/// framings of one cut are told apart in Finder.
pub fn export_stem(view: &Value) -> String {
    let title = view["title"].as_str().unwrap_or("cut");
    let f = &view["framing"];
    let list = f["list"].as_array().map_or(&[][..], Vec::as_slice);
    let name = (list.len() > 1)
        .then(|| list.iter().find(|x| x["id"] == f["active"]))
        .flatten()
        .and_then(|x| x["name"].as_str())
        .map(|n| n.replace(':', "x"));
    match name {
        Some(n) if !n.trim().is_empty() => {
            // the framing's name is kept whole: the title gives way
            let tail = format!(" - {}", safe_name(&n));
            let room = 80usize.saturating_sub(tail.chars().count()).max(8);
            let head = safe_name(&title.chars().take(room).collect::<String>());
            format!("{head}{tail}")
        }
        _ => safe_name(title),
    }
}

fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} {n}.mp4"));
        n += 1;
    }
    p
}

impl Jobs {
    pub fn list(&self) -> Vec<JobView> {
        self.jobs
            .lock()
            .map(|g| {
                g.iter()
                    .filter_map(|j| j.view.lock().ok().map(|v| v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One job, after waiting up to `wait` for it to leave `queued` and `running`, with
    /// the plan it rendered and the file it wrote once done (decision 0050: an agent's
    /// `job_wait` and `verify` see the app's exports).
    pub fn wait(&self, id: &str, wait: Duration) -> Option<(JobView, Option<Rendered>)> {
        let job = self.jobs.lock().ok().and_then(|g| {
            g.iter()
                .find(|j| j.view.lock().is_ok_and(|v| v.id == id))
                .cloned()
        })?;
        let t0 = Instant::now();
        loop {
            let v = job.view.lock().ok()?.clone();
            let busy = v.state == "queued" || v.state == "running";
            if !busy || t0.elapsed() >= wait {
                let done = job.done.lock().ok().and_then(|d| d.clone());
                return Some((v, done));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn cancel(&self, id: &str) -> bool {
        self.jobs.lock().is_ok_and(|g| {
            g.iter()
                .find(|j| j.view.lock().is_ok_and(|v| v.id == id))
                .map(|j| j.cancel.store(true, Ordering::Relaxed))
                .is_some()
        })
    }

    /// Starts an export of `cut`.
    pub fn start_export(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
    ) -> AppResult<Value> {
        self.start_export_with(core, project, cut, false)
    }

    /// Starts an export of `cut`; `draft_mattes` (the person confirmed it) exports
    /// roto cut-outs whose accurate mask tracks are not built with live drafts instead
    /// of building them first (SPEC §5.8), and the record lists them.
    pub fn start_export_with(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
        draft_mattes: bool,
    ) -> AppResult<Value> {
        self.start_export_preset(core, project, cut, draft_mattes, None)
    }

    /// [`Jobs::start_export_with`] at an upload preset (`youtube-1080p`, `youtube-1440p`;
    /// SPEC §6.5.1) instead of the framing's own output.
    pub fn start_export_preset(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
        draft_mattes: bool,
        preset: Option<&str>,
    ) -> AppResult<Value> {
        self.start_export_full(core, project, cut, draft_mattes, preset, None, None)
    }

    /// Exports `cut` in one of its framings (`framing`, an id), whichever it shows: the
    /// record names that framing, so a cut's 9:16 and 16:9 exports are each their own.
    pub fn start_export_framing(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
        framing: &str,
    ) -> AppResult<Value> {
        self.start_export_full(core, project, cut, false, None, None, Some(framing))
    }

    /// Exports one version of a short (decision 0024): the cut with alternate
    /// `opening` swapped in, recorded as that version so its retention is told apart.
    pub fn start_export_version(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
        opening: &str,
    ) -> AppResult<Value> {
        self.start_export_full(core, project, cut, false, None, Some(opening), None)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_export_full(
        self: &Arc<Self>,
        core: &Arc<AppCore>,
        project: &str,
        cut: &str,
        draft_mattes: bool,
        preset: Option<&str>,
        opening: Option<&str>,
        framing: Option<&str>,
    ) -> AppResult<Value> {
        let planned = plan_for(core, project, cut, preset, opening, framing)?;
        // the framing exported: the one named, else the one the cut shows
        let framing_id = framing
            .map(str::to_owned)
            .or_else(|| core.active_framing(project, cut));
        // which version this is: its opening (the main one unless an alternate is named)
        let version = crate::openings::version_of(core, project, cut, opening).ok();
        let plan = planned.plan.clone();
        let dir = core.export_dir(project)?;
        let frames = plan.output.frames;
        // the plan's target video rate (16 Mbps for a Short) + 192 kbps audio, doubled
        // below (R-ERR-2).
        let vbps = plan
            .output
            .video
            .rate
            .as_ref()
            .and_then(|r| r.target_bps)
            .unwrap_or(16_000_000) as f64;
        let estimate = (frames as f64 / plan.output.fps.as_f64() * (vbps + 0.2e6) / 8.0) as u64;
        if let Some(free) = free_bytes(&dir)
            && let Err(why) = space_check(free, estimate)
        {
            return Err(AppError::new("LowDisk", why));
        }
        let view = core.cut_view(project, cut).ok();
        let title = view
            .as_ref()
            .and_then(|v| v["title"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "cut".into());
        let stem = view.as_ref().map_or_else(
            || safe_name(&title),
            |v| {
                // the file is named after the framing exported, not the one on screen
                let mut v = v.clone();
                if let Some(f) = &framing_id {
                    v["framing"]["active"] = json!(f);
                }
                export_stem(&v)
            },
        );
        let stem = match (&version, opening) {
            (Some((_, label)), Some(_)) => format!("{stem} - {}", safe_name(label)),
            _ => stem,
        };
        let id = format!("job-{}-{}", now_ms(), self.list().len() + 1);
        let job = Arc::new(Job {
            view: Mutex::new(JobView {
                id: id.clone(),
                kind: "export",
                project: project.into(),
                cut: cut.into(),
                title: title.clone(),
                state: "queued".into(),
                frames,
                done: 0,
                started_ms: now_ms(),
                finished_ms: None,
                error: None,
                export: None,
                phase: "audio",
            }),
            progress: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            done: Mutex::new(None),
        });
        if let Ok(mut g) = self.jobs.lock() {
            g.push(job.clone());
        }
        let me = self.clone();
        let core = core.clone();
        let project = project.to_owned();
        let cut = cut.to_owned();
        let preset = preset.map(str::to_owned);
        let opening = opening.map(str::to_owned);
        let framing = framing.map(str::to_owned);
        let publish = {
            let core = core.clone();
            let job = job.clone();
            move || {
                if let Ok(v) = job.view.lock() {
                    core.bus.jobs(crate::events::JobsEvent::Job {
                        job: Box::new(v.clone()),
                    });
                }
                core.bus.home(crate::events::HomeEvent::Changed);
            }
        };
        publish();
        std::thread::Builder::new()
            .name("gmclip-export".into())
            .spawn(move || {
                let _slot = me.running.lock();
                if let Ok(mut v) = job.view.lock() {
                    v.state = "running".into();
                }
                publish();
                let done = Arc::new(AtomicBool::new(false));
                let ticker = {
                    let done = done.clone();
                    let job = job.clone();
                    let publish = publish.clone();
                    std::thread::spawn(move || {
                        while !done.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(250));
                            if let Ok(mut v) = job.view.lock() {
                                v.done = job.progress.load(Ordering::Relaxed);
                                if v.done > 0 {
                                    v.phase = "video";
                                }
                            }
                            publish();
                        }
                    })
                };
                let t0 = Instant::now();
                // R-JOB-2: the Mac stays awake while it exports (idle sleep held off,
                // the display may still sleep); released when the export ends.
                let _awake = KeepAwake::new();
                // captions from the cut's own audio (SPEC §5.10): the parts whose audio
                // differs from their source are heard first; a caption build never fails
                // an export (R-CAP-2), so a hearing that cannot run is a note
                let (plan, caption_note) = hear_before_export(
                    &core,
                    &project,
                    &cut,
                    preset.as_deref(),
                    opening.as_deref(),
                    framing.as_deref(),
                    &job,
                    plan,
                );
                let result = run_export(&plan, &dir, &stem, &job, draft_mattes);
                done.store(true, Ordering::Relaxed);
                let _ = ticker.join();
                match result {
                    Ok((out, rep)) => {
                        let entry = ExportEntry {
                            id: format!("exp-{}", now_ms()),
                            cut: cut.clone(),
                            title: title.clone(),
                            fps: plan.output.fps.num.get(),
                            fps_den: plan.output.fps.den.get(),
                            file: out,
                            frames: rep.frames,
                            seconds: t0.elapsed().as_secs_f64(),
                            render_sig: sig_of(&plan),
                            plan_sig: crate::core::plan_sig(&plan),
                            at_ms: now_ms(),
                            notes: rep.notes.iter().cloned().chain(caption_note).collect(),
                            verdict: None,
                            verify: crate::exportverify::at_export(&rep.notes, now_ms()),
                            opening_sig: version.as_ref().map(|v| v.0.clone()),
                            opening_label: version.as_ref().map(|v| v.1.clone()),
                            framing: framing_id.clone(),
                        };
                        let eid = entry.id.clone();
                        let file = entry.file.clone();
                        let recorded = core.add_export(&project, entry);
                        if let Ok(mut d) = job.done.lock() {
                            *d = Some((plan.clone(), file));
                        }
                        if let Ok(mut v) = job.view.lock() {
                            if let Err(e) = recorded {
                                v.state = "failed".into();
                                v.error = Some(format!(
                                    "the file was written but not recorded: {}",
                                    e.message
                                ));
                                v.finished_ms = Some(now_ms());
                            } else {
                                v.state = "done".into();
                                v.done = v.frames;
                                v.finished_ms = Some(now_ms());
                                v.export = Some(eid);
                            }
                        }
                    }
                    Err(e) => {
                        if let Ok(mut v) = job.view.lock() {
                            v.state = if job.cancel.load(Ordering::Relaxed) {
                                "cancelled".into()
                            } else {
                                "failed".into()
                            };
                            v.error = Some(e.to_string());
                            v.finished_ms = Some(now_ms());
                        }
                    }
                }
                publish();
            })
            .map_err(|_| AppError::internal("could not start the export thread"))?;
        Ok(json!({"job": id}))
    }
}

/// The plan an export renders: a version (its opening), an upload preset, a named
/// framing, or the cut as it shows.
fn plan_for(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    preset: Option<&str>,
    opening: Option<&str>,
    framing: Option<&str>,
) -> AppResult<crate::core::Planned> {
    match (preset, opening, framing) {
        (_, Some(o), _) => crate::openings::planned_version(core, project, cut, o),
        (Some(p), None, _) => core.planned_preset(project, cut, p),
        (None, None, Some(f)) => core.planned_in(project, cut, Some(f)),
        (None, None, None) => core.planned(project, cut),
    }
}

/// The export's caption phase: hears the stretches of the cut's captions that have no
/// words yet and plans the cut again with them. Returns the plan to export and a note
/// when hearing could not run (the parts that differ then show their source words).
#[allow(clippy::too_many_arguments)]
fn hear_before_export(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    preset: Option<&str>,
    opening: Option<&str>,
    framing: Option<&str>,
    job: &Job,
    plan: Arc<gmclip_plan::Plan>,
) -> (Arc<gmclip_plan::Plan>, Option<String>) {
    if let Ok(mut v) = job.view.lock() {
        v.phase = "captions";
    }
    let heard = core.hear_captions(project, cut, &job.cancel);
    if let Ok(mut v) = job.view.lock() {
        v.phase = "audio";
    }
    match heard {
        Ok(0) => (plan, None),
        Ok(n) => match plan_for(core, project, cut, preset, opening, framing) {
            Ok(pl) => (
                pl.plan,
                Some(format!(
                    "captions: {n} stretch{} heard from the cut's own audio",
                    if n == 1 { "" } else { "es" }
                )),
            ),
            Err(e) => (
                plan,
                Some(format!("captions: heard, not replanned ({})", e.message)),
            ),
        },
        Err(e) => (
            plan,
            Some(format!(
                "captions: the parts whose audio changed were not heard ({}); they show their source words",
                e.message
            )),
        ),
    }
}

/// Holds off idle sleep for its lifetime (`/usr/bin/caffeinate -i`, the OS's own power
/// assertion tool; no extra framework). Not started where caffeinate is missing. On
/// Windows, a power request (gmclip-platform) that `powercfg /requests` lists.
#[derive(Debug)]
pub struct KeepAwake(
    #[cfg(not(windows))] Option<std::process::Child>,
    #[cfg(windows)] Option<gmclip_platform::windows::PowerRequest>,
);

impl KeepAwake {
    #[cfg(not(windows))]
    pub fn new() -> Self {
        let child = std::process::Command::new("/usr/bin/caffeinate")
            .args(["-i", "-w"])
            .arg(std::process::id().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        Self(child)
    }

    #[cfg(windows)]
    pub fn new() -> Self {
        Self(
            gmclip_platform::windows::PowerRequest::system_required("gmclip is exporting a video")
                .ok(),
        )
    }

    /// Whether the assertion is held.
    #[cfg(not(windows))]
    pub fn held(&mut self) -> bool {
        self.0
            .as_mut()
            .is_some_and(|c| c.try_wait().is_ok_and(|s| s.is_none()))
    }

    /// Whether the request is held.
    #[cfg(windows)]
    pub fn held(&mut self) -> bool {
        self.0.is_some()
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        Self::new()
    }
}

// (Windows: the power request releases itself on drop)
#[cfg(not(windows))]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
fn run_export(
    plan: &gmclip_plan::Plan,
    dir: &Path,
    stem: &str,
    job: &Job,
    draft_mattes: bool,
) -> Result<(PathBuf, gmclip_render::export::ExportReport), AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::internal(format!("export folder: {}", e.kind())))?;
    let out = unique_path(dir, stem);
    let mut inputs = gmclip_render::export::inputs::DemuxInputs::new(None);
    let opts = gmclip_render::export::ExportOptions {
        cancel: Some(job.cancel.clone()),
        keep_superseded: Some(3),
        progress: Some(job.progress.clone()),
        mattes: Some(gmclip_render::matte::store::shared()),
        draft_mattes,
        ..Default::default()
    };
    let rep = gmclip_render::export::export(plan, &mut inputs, &out, &opts)?;
    Ok((rep.out.clone(), rep))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn run_export(
    _plan: &gmclip_plan::Plan,
    _dir: &Path,
    _stem: &str,
    _job: &Job,
    _draft_mattes: bool,
) -> Result<(PathBuf, gmclip_render::export::ExportReport), AppError> {
    Err(AppError::new(
        "Unsupported",
        "export runs on macOS and Windows",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_export_needs_two_gigabytes_or_twice_its_estimate() {
        use super::space_check;
        assert!(space_check(2_000_000_000, 100_000_000).is_ok());
        let e = space_check(1_999_999_999, 100_000_000).unwrap_err();
        assert!(
            e.contains("2.0 GB free") && e.contains("needs 2.0 GB"),
            "{e}"
        );
        // a big export: twice its estimate
        assert!(space_check(5_000_000_000, 2_400_000_000).is_ok());
        let e = space_check(4_700_000_000, 2_400_000_000).unwrap_err();
        assert!(
            e.contains("4.7 GB free") && e.contains("needs 4.8 GB") && e.contains("~2.4 GB"),
            "{e}"
        );
    }
}
