//! The app's in-process core (R-ARCH-2): one process hosts every open project's actor.
//!
//! - Projects live where SPEC §3.8 puts them: a home `<library>/<name>.gmclip/` with
//!   `owner.json`, the live database in `<app data>/projects/<id>.sqlite` (gmclip-mcp's
//!   `project` module, shared with the headless holder so both agree on layout).
//! - The app opens a database as `HolderKind::App`. A project held by `gmclip serve`
//!   (headless) is refused with a plain message for now; R-ARCH-7's takeover is next.
//! - Imported golden cuts keep their base RenderPlan and import map in
//!   `<home>/imports/<cut>.json`; [`crate::planner`] re-plans them from the model.
//! - A pump thread per project forwards the actor's events to the UI bus and re-plans the
//!   cut the viewer shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use gmclip_core::{
    CallRequest, Caller, HolderKind, OpenOptions, ProjectHandle, StoreError, StoreOptions,
};
use gmclip_mcp::project::{self, HomeInfo};
use gmclip_model::entities::{Cut, Part};
use gmclip_plan::Plan;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bus::Bus;
use crate::error::{AppError, AppResult};
use crate::planner::{
    self, ImportMap, ImportedCut, LayerSeed, PlanIndex, SourceMap, frame_of, time_of, ulid_id,
};

thread_local! {
    /// The agent this thread's app command runs for (decision 0050), if any.
    static ACTING: std::cell::RefCell<Option<Caller>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` (one of the app's commands an agent ran through MCP) with every registry
/// call it makes on this thread as `caller`: the agent's, in its run, so the run's undo
/// covers it and History names the agent (decision 0050). Work the command hands to
/// other threads (an export's render) stays the app's.
pub fn acting_as<T>(caller: Caller, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Caller>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            ACTING.with(|a| *a.borrow_mut() = prev);
        }
    }
    let _restore = Restore(ACTING.with(|a| a.borrow_mut().replace(caller)));
    f()
}

/// Who this thread's registry calls are made as: the agent inside [`acting_as`], else
/// the human.
pub fn acting_caller() -> Caller {
    ACTING
        .with(|a| a.borrow().clone())
        .unwrap_or_else(Caller::human)
}

/// Where the app keeps things (SPEC §3.8, §4.4).
#[derive(Clone, Debug)]
pub struct Dirs {
    pub app_data: PathBuf,
    /// Default library: `~/Movies/gmclip/Library`.
    pub library: PathBuf,
    /// Exports go to `<exports>/<project name>/` (the `export_dir` default).
    pub exports: PathBuf,
}

impl Dirs {
    pub fn from_env() -> AppResult<Dirs> {
        // the user's home: HOME on macOS; USERPROFILE on Windows, which sets no HOME
        // (an app started from the Start menu has none)
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var_os(var)
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| AppError::internal(format!("{var} is not set")))?;
        let app_data = gmclip_mcp::paths::app_data()
            .map_err(|_| AppError::internal("no app data directory"))?;
        // under the system's videos folder: ~/Movies on macOS, %USERPROFILE%\Videos on
        // Windows (joined by component: one separator style)
        let videos = home.join(if cfg!(windows) { "Videos" } else { "Movies" });
        Ok(Dirs {
            app_data,
            library: videos.join("gmclip").join("Library"),
            exports: videos.join("gmclip"),
        })
    }
}

/// One finished export (the review board's tile, `<home>/exports.json`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExportEntry {
    pub id: String,
    /// Output frame rate (so the board can say how long the file is).
    #[serde(default = "default_fps")]
    pub fps: u64,
    /// 1001 at an NTSC rate (`fps / fps_den`, decision 0020).
    #[serde(default = "default_fps_den", skip_serializing_if = "is_one")]
    pub fps_den: u64,
    pub cut: String,
    pub title: String,
    pub file: PathBuf,
    pub frames: u64,
    pub seconds: f64,
    pub render_sig: Option<String>,
    pub at_ms: i64,
    pub notes: Vec<String>,
    /// The plan's digest at export (freshness when `render_sig` is absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_sig: Option<String>,
    /// The human's keep / change / drop on the review board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// The last `verify{export}` of the file (R-EXP-6): the renderer's at export, or a
    /// re-verify (H.06).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<crate::exportverify::Verify>,
    /// Which version of the cut this is (decision 0024): its opening's spans
    /// (`spans_sig`), so retention imported onto it is that opening's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_sig: Option<String>,
    /// The version's name when it was exported ("Main", or an alternate's label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_label: Option<String>,
    /// The framing exported (its id): a cut's 9:16 and 16:9 exports are told apart, and
    /// each framing's freshness reads its own. Absent on exports made before it existed,
    /// which count for whichever framing the cut shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<String>,
}

fn default_fps() -> u64 {
    30
}

fn default_fps_den() -> u64 {
    1
}

fn is_one(v: &u64) -> bool {
    *v == 1
}

impl ExportEntry {
    /// Frames per second.
    pub fn rate(&self) -> f64 {
        self.fps.max(1) as f64 / self.fps_den.max(1) as f64
    }
}

/// An open project.
#[derive(Debug)]
pub struct OpenProject {
    pub info: HomeInfo,
    pub home: PathBuf,
    pub handle: ProjectHandle,
    pub imports: RwLock<BTreeMap<String, Arc<ImportedCut>>>,
    /// The framing each cut shows and exports (the output toggle, 9:16 / 16:9):
    /// `<home>/framing.json`, cut id → framing id. Absent: the cut's first framing.
    pub framing: RwLock<BTreeMap<String, String>>,
    pub exports: Mutex<Vec<ExportEntry>>,
}

/// A planned cut, cached until the next commit.
#[derive(Clone, Debug)]
pub struct Planned {
    pub plan: Arc<Plan>,
    pub index: Arc<PlanIndex>,
    pub edited: bool,
    pub rev: u64,
    /// Heard caption stretches the plan was made with (new words, new plan).
    pub heard: usize,
}

/// Called with a project id after each of its commits.
pub type CommitHook = Box<dyn Fn(&str) + Send + Sync>;

/// The app's state, shared by IPC, the viewer, exports and the dev bridge.
pub struct AppCore {
    pub dirs: Dirs,
    pub bus: Bus,
    projects: RwLock<BTreeMap<String, Arc<OpenProject>>>,
    planned: Mutex<BTreeMap<(String, String), Planned>>,
    /// Called after every commit on a project (the viewer re-plans its cut).
    on_commit: Mutex<Vec<CommitHook>>,
    /// Background jobs yield to playback: paused after a dropped frame (D.01).
    pub qos: crate::qos::QosGate,
    /// Home: exports finished after this are "done since you last looked".
    pub seen_ms: Mutex<i64>,
    /// Font files the model planner may use ([`crate::fonts`]' catalogue).
    fonts: AppFonts,
    /// Parsed transcripts (analysis blobs), per project, owner and revision.
    pub transcripts: crate::review::Transcripts,
    /// Background analysis and ASR of opened videos.
    pub analysis: crate::analysis::Analyzer,
    /// The holder agents reach this app through, and the runs started from it.
    pub agents: crate::agent::Agents,
    /// Live agent sessions over ACP (decision 0035).
    pub sessions: crate::sessions::Sessions,
    /// The review-link server, when serving (U.10).
    pub review: crate::reviewlink::ReviewState,
    /// Overnight batches started from this app (E.01 / U.16).
    pub batches: crate::batch::Batches,
    /// Exports a takeover stopped (`(project, cut)`), for the caller to restart (R-ARCH-7).
    pub taken: Mutex<Vec<(String, String)>>,
    /// Scrub proxies built in the background (H.05).
    pub proxies: crate::proxy::Proxies,
    /// Moment clips rendered in the background (H.10).
    pub clips: crate::clips::Clips,
}

/// The model planner's file lookup: font faces (the app's catalogue, by sha256) and
/// overlay images (`<app data>/assets/<sha256>.png`, written by `studio`).
#[derive(Debug, Default, Clone)]
pub struct AppFonts {
    fonts: Vec<(String, String)>,
    assets: Option<PathBuf>,
}

impl AppFonts {
    /// The faces the app can draw ([`crate::fonts`]: the bundle, the faces it knows found
    /// in the user's font folders, the repo's free faces), and the default app data's
    /// assets.
    pub fn load() -> AppFonts {
        AppFonts {
            fonts: crate::fonts::catalogue()
                .iter()
                .filter_map(|f| {
                    Some((
                        f.sha256.clone(),
                        f.path.as_ref()?.to_string_lossy().into_owned(),
                    ))
                })
                .collect(),
            assets: gmclip_mcp::paths::app_data().ok().map(|d| d.join("assets")),
        }
    }

    /// Overlay images from `dir` (an app core's own app data).
    pub fn with_assets(mut self, dir: PathBuf) -> AppFonts {
        self.assets = Some(dir);
        self
    }

    /// Where an asset with this sha256 is stored.
    pub fn asset_file(&self, sha256: &str) -> Option<PathBuf> {
        self.asset_file_typed(sha256, "image/png")
    }

    /// Where an asset of this type is stored: `<sha256>.png`, `.mp4` or `.mov`
    /// (decision 0028: backdrop and video overlay clips), `.json` (a recording's input
    /// track, decision 0045).
    pub fn asset_file_typed(&self, sha256: &str, media_type: &str) -> Option<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let ext = match media_type {
            "video/mp4" => "mp4",
            "video/quicktime" => "mov",
            // music and sound effects (decision 0051)
            "audio/mp4" => "m4a",
            "audio/wav" => "wav",
            gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE => "json",
            _ => "png",
        };
        Some(self.assets.as_ref()?.join(format!("{sha256}.{ext}")))
    }
}

impl gmclip_core::planner::PlanHost for AppFonts {
    fn font_path(&self, font: &gmclip_model::entities::Font) -> Option<String> {
        let want = font.sha256.as_deref()?;
        self.fonts
            .iter()
            .find(|(h, _)| h == want)
            .map(|(_, p)| p.clone())
    }

    fn asset_path(&self, asset: &gmclip_model::entities::Asset) -> Option<String> {
        let p = self.asset_file_typed(&asset.sha256, &asset.media_type)?;
        p.is_file().then(|| p.to_string_lossy().into_owned())
    }
}

/// Plans a cut with lane A's model planner in its first framing; the index maps plan
/// parts to model parts in order and plan layers to the effective stack in order (the
/// planner emits both in those orders).
fn model_plan(
    doc: &gmclip_model::Doc,
    view: &gmclip_core::analysis::AnalysisView,
    cut: &str,
    framing: Option<&str>,
    fonts: &AppFonts,
) -> AppResult<(Plan, PlanIndex, bool)> {
    model_plan_preset(doc, view, cut, framing, fonts, None)
}

/// [`model_plan`] at an upload preset (SPEC §6.5.1) instead of the framing's own output.
fn model_plan_preset(
    doc: &gmclip_model::Doc,
    view: &gmclip_core::analysis::AnalysisView,
    cut: &str,
    framing: Option<&str>,
    fonts: &AppFonts,
    preset: Option<gmclip_core::planner::ExportPreset>,
) -> AppResult<(Plan, PlanIndex, bool)> {
    use gmclip_model::entities::Framing;
    let cid = ulid_id::<Cut>(cut)?;
    let want = framing.and_then(|f| ulid_id::<Framing>(f).ok());
    let framing = doc
        .children::<Framing>(cid.entity())
        .find(|f| want.is_none_or(|w| *f == w))
        .or_else(|| doc.children::<Framing>(cid.entity()).next())
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing to plan"))?;
    let planned = match preset {
        Some(p) => gmclip_core::planner::plan_cut_preset(doc, cid, framing, fonts, Some(view), p),
        None => gmclip_core::planner::plan_cut_traced(doc, cid, framing, fonts, Some(view)),
    };
    let (plan, traces) = planned
        .map_err(|e| AppError::new("Unsupported", format!("the cut does not plan yet: {}", e.0)))?;
    let mut index = PlanIndex {
        cues: traces.into_iter().map(|t| (t.event, t.words)).collect(),
        ..PlanIndex::default()
    };
    // The planner emits parts in output order (the cut's layout); a part with cut
    // visual splits inside it is several plan parts (R.03).
    let order = gmclip_core::planner::plan_part_owners(doc, cid, framing);
    let fpf = crate::planner::fpf(&plan);
    let mut first: std::collections::BTreeMap<_, u64> = std::collections::BTreeMap::new();
    for (pp, pid) in plan.parts.iter().zip(order) {
        let Some(mp) = doc.get(pid) else { continue };
        let f0 = *first.entry(pid).or_insert(pp.picture.first_frame);
        let at = gmclip_model::Flicks(
            mp.src_in.0 + (pp.picture.first_frame.saturating_sub(f0)) as i64 * fpf,
        );
        let stack = gmclip_model::effective::effective(doc, pid, framing, at);
        let layers = stack
            .map(|st| {
                pp.layers
                    .iter()
                    .zip(st.layers.iter())
                    .map(|(l, rl)| (l.id.as_str().to_owned(), rl.id.0.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        index.parts.push(crate::planner::PartIndex {
            plan_id: pp.id.as_str().to_owned(),
            model: pid.0.to_string(),
            base: index.parts.len(),
            layers,
        });
    }
    Ok((plan, index, true))
}

impl std::fmt::Debug for AppCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCore")
            .field("dirs", &self.dirs)
            .finish_non_exhaustive()
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn store_err(e: &StoreError) -> AppError {
    match e {
        StoreError::Leased {
            holder: Some(h), ..
        } if h.kind == HolderKind::Headless => AppError::new(
            "Leased",
            format!(
                "This project is open in gmclip's headless server (pid {}), so an agent may be working on it. \
                 Stop `gmclip serve` (or wait for its run to finish) and open the project again.",
                h.pid
            ),
        ),
        StoreError::Leased { holder, .. } => AppError::new(
            "Leased",
            match holder {
                Some(h) => format!(
                    "This project is open in another gmclip process (pid {}).",
                    h.pid
                ),
                None => "This project is open in another gmclip process.".into(),
            },
        ),
        StoreError::StaleOtherHost { .. } => AppError::new(
            "Leased",
            "This project was last opened on another Mac; open it there or copy it here first.",
        ),
        StoreError::Io(step, kind) => AppError::internal(format!(
            "the project database did not open ({step}: {kind})"
        )),
        _ => AppError::internal("the project database did not open"),
    }
}

/// A file-name-safe version of a title: at most 80 characters, cut at a word edge when
/// there is one, never ending on a space, dot, dash or underscore.
pub fn safe_name(s: &str) -> String {
    const MAX: usize = 80;
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut t: String = mapped.trim().trim_start_matches('.').to_owned();
    if t.chars().count() > MAX {
        let head: String = t.chars().take(MAX).collect();
        let next_is_space = t.chars().nth(MAX) == Some(' ');
        t = match head.rfind(' ') {
            _ if next_is_space => head,
            // a whole word, unless that would leave only a few characters
            Some(i) if head[..i].chars().count() >= MAX / 4 => head[..i].to_owned(),
            _ => head,
        };
    }
    let t = t.trim_end_matches([' ', '.', '-', '_']);
    if t.is_empty() {
        "untitled".into()
    } else {
        t.to_owned()
    }
}

/// The at-exit end of a process that loaded ONNX Runtime (registered after each load, so
/// it runs ahead of the static destructors the load registered). Every binary that links
/// the core has a `main` returning `()`: the app, and the test harnesses. So an exit that is
/// `main` returning has status 0 and ends here with it; an explicit `exit(code)` (a failed
/// test's 101) goes on with its own status.
#[cfg(target_os = "macos")]
extern "C" fn end_past_onnx_runtime() {
    gmclip_ml::runtime::quiesce();
    gmclip_gpu::apple::exit::end_now_if_main_returned();
}

impl AppCore {
    pub fn new(dirs: Dirs) -> Arc<AppCore> {
        Self::with_analyzer(dirs, crate::analysis::Analyzer::default())
    }

    /// [`AppCore::new`] with the analysis runner given (tests: a fake host).
    pub fn with_analyzer(dirs: Dirs, analysis: crate::analysis::Analyzer) -> Arc<AppCore> {
        // A process that ends through `exit` (a test binary; the app ends with `_exit`)
        // must not tear ONNX Runtime down: at exit, wait for the calls inside it and let
        // no new one in (gmclip_ml::runtime's exit gate), then, when the exit is `main`
        // returning, end there with 0 before ONNX Runtime's static teardown, which races
        // its own telemetry thread with no call in flight (gmclip_gpu::apple::exit).
        #[cfg(target_os = "macos")]
        gmclip_ml::runtime::set_exit_hook(|| {
            let _ = gmclip_gpu::apple::exit::at_exit(end_past_onnx_runtime);
        });
        let fonts = AppFonts::load().with_assets(dirs.app_data.join("assets"));
        let core = Arc::new(AppCore {
            dirs,
            bus: Bus::default(),
            projects: RwLock::new(BTreeMap::new()),
            planned: Mutex::new(BTreeMap::new()),
            on_commit: Mutex::new(Vec::new()),
            qos: crate::qos::QosGate::default(),
            seen_ms: Mutex::new(now_ms()),
            fonts,
            transcripts: crate::review::Transcripts::default(),
            analysis,
            agents: crate::agent::Agents::default(),
            sessions: crate::sessions::Sessions::default(),
            review: crate::reviewlink::ReviewState::default(),
            batches: crate::batch::Batches::default(),
            taken: Mutex::new(Vec::new()),
            proxies: crate::proxy::Proxies::default(),
            clips: crate::clips::Clips::default(),
        });
        // an earlier app on this data dir may have stopped the meaning index on quitting
        crate::libmeaning::resume(&core);
        // cut-outs build their accurate tracks on their own (R.01)
        crate::roto::start_auto(&core);
        core
    }

    /// The model planner's font lookup (shared with the app's holder).
    pub fn fonts(&self) -> &AppFonts {
        &self.fonts
    }

    pub fn add_commit_hook(&self, f: CommitHook) {
        if let Ok(mut g) = self.on_commit.lock() {
            g.push(f);
        }
    }

    pub fn project(&self, id: &str) -> AppResult<Arc<OpenProject>> {
        self.projects
            .read()
            .ok()
            .and_then(|g| g.get(id).cloned())
            .ok_or_else(|| AppError::not_found("that project is not open"))
    }

    /// The project `id`, opened from the library when it is there and not open yet
    /// (decision 0040: after a restart, a session's Open and Undo its edits named a
    /// project the window had not opened and failed with "that project is not open").
    pub fn ensure_open(self: &Arc<Self>, id: &str) -> AppResult<Arc<OpenProject>> {
        if let Ok(p) = self.project(id) {
            return Ok(p);
        }
        // the library folder itself, not its index (the index may be busy refreshing
        // for Home at the same moment)
        let home = std::fs::read_dir(&self.dirs.library)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|h| h.extension().is_some_and(|x| x == "gmclip"))
            .find(|h| {
                cap_std::fs::Dir::open_ambient_dir(h, cap_std::ambient_authority())
                    .ok()
                    .and_then(|d| project::read_home(&d).ok())
                    .is_some_and(|info| info.id == id)
            })
            .ok_or_else(|| {
                AppError::not_found(
                    "that project is not open, and the library has no project with that id",
                )
            })?;
        self.open_home(&home)?;
        self.project(id)
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.projects.read().is_ok_and(|g| g.contains_key(id))
    }

    pub fn projects(&self) -> Vec<Arc<OpenProject>> {
        self.projects
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Creates a project home in the library and opens it.
    pub fn create_project(self: &Arc<Self>, name: &str) -> AppResult<Value> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(AppError::invalid("a project name is 1–120 characters"));
        }
        std::fs::create_dir_all(&self.dirs.library)
            .map_err(|e| AppError::internal(format!("library folder: {}", e.kind())))?;
        let stem = safe_name(name);
        let mut home = self.dirs.library.join(format!("{stem}.gmclip"));
        let mut n = 2;
        while home.exists() {
            home = self.dirs.library.join(format!("{stem} {n}.gmclip"));
            n += 1;
        }
        project::create_home(&self.dirs.app_data, &home, name)
            .map_err(|e| AppError::internal(format!("creating the project: {}", e.kind())))?;
        self.open_home(&home)
    }

    /// Opens a project home (a human choice: any folder the person picks).
    pub fn open_home(self: &Arc<Self>, home: &Path) -> AppResult<Value> {
        let dir = cap_std::fs::Dir::open_ambient_dir(home, cap_std::ambient_authority())
            .map_err(|_| AppError::not_found("no project folder there"))?;
        let info = project::read_home(&dir).map_err(AppError::from)?;
        let canon = home.canonicalize().unwrap_or_else(|_| home.to_owned());
        project::check_home(&self.dirs.app_data, &info.id, &canon).map_err(AppError::from)?;
        if let Ok(p) = self.project(&info.id) {
            return Ok(self.project_json(&p));
        }
        let db = project::db_path(&self.dirs.app_data, &info.id).map_err(AppError::from)?;
        let opts = || OpenOptions {
            store: StoreOptions::default(),
            ..OpenOptions::default()
        };
        let handle = match ProjectHandle::open(&db, opts()) {
            Ok(h) => h,
            // held by this app's own agent server (an agent's `project_open`): share its
            // handle. A takeover here would hand the project over to ourselves and stop the
            // server, taking the gmclip socket away from every agent.
            Err(StoreError::Leased {
                holder: Some(h), ..
            }) if h.kind == HolderKind::Headless
                && h.pid == std::process::id()
                && let Some(op) = self
                    .agents
                    .holder()
                    .and_then(|hd| hd.resolve(Some(&info.id)).ok()) =>
            {
                op.handle
            }
            // held by `gmclip serve`: the app takes it over (R-ARCH-7), then opens it
            Err(StoreError::Leased {
                holder: Some(h), ..
            }) if h.kind == HolderKind::Headless => {
                let t = crate::takeover::takeover(self)?;
                if let Ok(mut g) = self.taken.lock() {
                    g.extend(t.exports.iter().cloned());
                }
                if let Ok(p) = self.project(&info.id) {
                    return Ok(self.project_json(&p));
                }
                ProjectHandle::open(&db, opts()).map_err(|e| store_err(&e))?
            }
            Err(e) => return Err(store_err(&e)),
        };
        let p = Arc::new(OpenProject {
            info: info.clone(),
            home: canon.clone(),
            handle,
            imports: RwLock::new(load_imports(&canon)),
            framing: RwLock::new(
                std::fs::read(canon.join("framing.json"))
                    .ok()
                    .filter(|b| b.len() < 1 << 20)
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default(),
            ),
            exports: Mutex::new(load_exports(&canon)),
        });
        if let Ok(mut g) = self.projects.write() {
            g.insert(info.id.clone(), p.clone());
        }
        self.agents.adopt(&p);
        self.spawn_pump(&p);
        // Home lists it now, whoever opened it (an agent's project_new too)
        self.bus.home(crate::events::HomeEvent::Changed);
        Ok(self.project_json(&p))
    }

    pub fn close_project(&self, id: &str) -> AppResult<()> {
        let p = self
            .projects
            .write()
            .ok()
            .and_then(|mut g| g.remove(id))
            .ok_or_else(|| AppError::not_found("that project is not open"))?;
        self.analysis.cancel_project(id);
        self.agents.release(id);
        let _ = p.handle.close();
        Ok(())
    }

    /// Closes everything cleanly (on quit).
    pub fn shutdown(&self) {
        // transcription and the meaning index stop while the projects close (between
        // steps; they are waited for below)
        self.analysis.cancel_all();
        crate::sessions::stop_all(self);
        crate::libmeaning::stop(self, std::time::Duration::ZERO);
        crate::roto::stop_all();
        let ids: Vec<String> = self
            .projects
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        let mut closed = Vec::new();
        for id in ids {
            let (name, rev) = self
                .project(&id)
                .map(|p| (p.info.name.clone(), p.handle.snapshot().doc.rev()))
                .unwrap_or_default();
            let _ = self.close_project(&id);
            closed.push((id, name, rev));
        }
        self.proxies.shutdown();
        // Transcription threads end before the process does: one still inside ONNX
        // Runtime at exit trips its static teardown ("recursive_mutex lock failed").
        if !self.analysis.shutdown(std::time::Duration::from_secs(10)) {
            eprintln!("gmclip: an analysis job did not stop within 10 s of quitting");
        }
        // the library's meaning index embeds with ONNX Runtime on its own thread too
        if !crate::libmeaning::stop(self, std::time::Duration::from_secs(10)) {
            eprintln!("gmclip: the meaning index did not stop within 10 s of quitting");
        }
        // a copy of each project that changed, taken once everything has stopped (the
        // projects are closed: nothing is writing them)
        for (id, name, rev) in closed {
            let _ = crate::backups::backup_closed(self, &id, &name, rev);
        }
    }

    fn spawn_pump(self: &Arc<Self>, p: &Arc<OpenProject>) {
        let Some(sub) = p.handle.subscribe() else {
            return;
        };
        let weak = Arc::downgrade(self);
        let me = Arc::downgrade(p);
        let id = p.info.id.clone();
        let _ = std::thread::Builder::new()
            .name("gmclip-pump".into())
            .spawn(move || {
                loop {
                    let evs = sub.wait(std::time::Duration::from_millis(500));
                    let Some(core) = weak.upgrade() else { return };
                    // This open of the project, not a later reopen with the same id.
                    let current = core.project(&id).ok();
                    if !current.is_some_and(|c| me.upgrade().is_some_and(|m| Arc::ptr_eq(&c, &m))) {
                        return;
                    }
                    if evs.is_empty() {
                        continue;
                    }
                    core.invalidate(&id);
                    for e in evs {
                        core.bus.project(&id, e.into());
                    }
                    if let Ok(hooks) = core.on_commit.lock() {
                        for h in hooks.iter() {
                            h(&id);
                        }
                    }
                    core.bus.home(crate::events::HomeEvent::Changed);
                }
            });
    }

    fn invalidate(&self, project: &str) {
        if let Ok(mut g) = self.planned.lock() {
            g.retain(|(p, _), _| p != project);
        }
    }

    pub fn project_json(&self, p: &OpenProject) -> Value {
        let doc = &p.handle.snapshot().doc;
        let imports = p.imports.read().map(|g| g.clone()).unwrap_or_default();
        let cuts: Vec<Value> = doc
            .iter::<Cut>()
            .map(|c| {
                let id = c.id.0.to_string();
                json!({
                    "id": id,
                    "title": c.title,
                    "imported": imports.contains_key(&id),
                    "framed": doc
                        .children::<gmclip_model::entities::Framing>(c.id.entity())
                        .next()
                        .is_some(),
                    "parts": doc.children::<Part>(c.id.entity()).count(),
                })
            })
            .collect();
        json!({
            "id": p.info.id,
            "name": p.info.name,
            "home": p.home,
            "rev": doc.rev(),
            "cuts": cuts,
        })
    }

    /// A human call on the project's actor (R-CMD-2: the UI mutates only through these);
    /// inside [`acting_as`], the agent's call instead, based on what is there now.
    pub fn call(&self, project: &str, command: &str, args: Value) -> AppResult<Value> {
        let p = self.project(project)?;
        let caller = acting_caller();
        let base =
            (caller.kind == gmclip_proto::CallerKind::Agent).then(|| gmclip_proto::Base::AsOf {
                as_of: p.handle.snapshot().doc.rev(),
            });
        let ok = p
            .handle
            .call(CallRequest {
                command: command.into(),
                args,
                base,
                request_id: None,
                caller,
            })
            .map_err(AppError::from)?;
        Ok(json!({ "output": ok.output, "as_of": ok.as_of, "committed": ok.committed }))
    }

    /// Imports a RenderPlan file as a new cut: sources, the cut, a 9:16 framing, one part
    /// per plan part and one part-override layer per plan layer, all through registry
    /// commands. Relative file paths in the plan resolve against the plan's folder.
    pub fn import(self: &Arc<Self>, project: &str, path: &Path) -> AppResult<Value> {
        let p = self.project(project)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let regular = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file());
        if !regular(path) {
            return Err(AppError::not_found("that is not a file"));
        }
        if !name.ends_with(".renderplan.json") {
            return Err(AppError::invalid(
                "not a RenderPlan (a .renderplan.json file)",
            ));
        }
        let mut plan = {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .and_then(|f| {
                    std::io::Read::read_to_end(
                        &mut std::io::Read::take(f, MAX_PLAN + 1),
                        &mut bytes,
                    )
                })
                .map_err(|e| AppError::not_found(format!("cannot read the plan ({})", e.kind())))?;
            if bytes.len() as u64 > MAX_PLAN {
                return Err(AppError::invalid("the plan file is larger than 64 MB"));
            }
            gmclip_plan::parse(&bytes).map_err(|e| {
                AppError::invalid(format!(
                    "not a valid RenderPlan: {} ({})",
                    e.reason(),
                    e.path()
                ))
            })?
        };
        if let Some(dir) = path.parent() {
            plan.resolve_paths(dir);
        }
        let errs = gmclip_plan::check_plan(&plan);
        if let Some(e) = errs.first() {
            return Err(AppError::invalid(format!(
                "the plan is invalid: {} ({})",
                e.reason(),
                e.path()
            )));
        }
        let title = plan
            .meta
            .as_ref()
            .and_then(|m| m.title.clone())
            .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());
        // Everything below commits; on a failure the commits made so far are undone, so a
        // half-imported cut never stays behind.
        let commits = std::cell::Cell::new(0usize);
        let r = self.seed(project, &p, plan, &title, &commits);
        if r.is_err() {
            for _ in 0..commits.get() {
                let _ = self.call(project, "undo", json!({}));
            }
        }
        r
    }

    fn counted(
        &self,
        n: &std::cell::Cell<usize>,
        project: &str,
        command: &str,
        args: Value,
    ) -> AppResult<Value> {
        let v = self.call(project, command, args)?;
        if !v["committed"].is_null() {
            n.set(n.get() + 1);
        }
        Ok(v["output"].clone())
    }

    fn seed(
        self: &Arc<Self>,
        project: &str,
        p: &OpenProject,
        plan: Plan,
        title: &str,
        commits: &std::cell::Cell<usize>,
    ) -> AppResult<Value> {
        // Sources (and their picture size, for normalising crops).
        let mut sources_map = BTreeMap::new();
        for s in &plan.sources {
            let ix = crate::inputs::index_file(Path::new(&s.path))?;
            let (w, h) = ix.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
            let fp = s
                .fingerprint
                .as_ref()
                .and_then(|f| f.sha256.as_ref())
                .map_or_else(
                    || "unhashed".to_owned(),
                    |h| format!("sha256:{}", h.as_str()),
                );
            let out = self.counted(
                commits,
                project,
                "source_add",
                json!({"path": s.path, "fingerprint": fp}),
            )?;
            sources_map.insert(
                s.id.as_str().to_owned(),
                SourceMap {
                    model: out["id"].as_str().unwrap_or_default().to_owned(),
                    width: w,
                    height: h,
                },
            );
        }
        let cut_title: String = title.chars().take(200).collect();
        let created = self.counted(commits, project, "cut_create", json!({"title": cut_title}))?;
        let cut = created["cut"].as_str().unwrap_or_default().to_owned();
        let (cw, ch) = (plan.output.canvas.w.get(), plan.output.canvas.h.get());
        let framing = self.counted(
            commits,
            project,
            "framing_create",
            json!({"cut": cut, "name": if cw < ch { "9:16" } else { "16:9" }, "w": cw, "h": ch,
                   "fps": {"num": plan.output.fps.num.get(), "den": plan.output.fps.den.get()}}),
        )?["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let fps = planner::fps(&plan);
        let mut parts = BTreeMap::new();
        let mut layers = BTreeMap::new();
        let mut after: Option<String> = None;
        for (pi, bp) in plan.parts.iter().enumerate() {
            let src = &sources_map[bp.picture.source.as_str()];
            let first = bp.picture.first_frame as i64;
            let role = if pi == 0 { "hook" } else { "other" };
            let mut args = json!({
                "cut": cut,
                "media": {"source": src.model},
                "src_in": time_of(first, fps).0,
                "src_out": time_of(first + bp.out.len() as i64, fps).0,
                "role": role,
            });
            if let Some(a) = &after {
                args["after"] = json!(a);
            }
            let part = self.counted(commits, project, "part_add", args)?["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            for (li, l) in bp.layers.iter().enumerate() {
                let ls = l.picture_source.as_ref().unwrap_or(&bp.picture.source);
                let dims = &sources_map[ls.as_str()];
                let (sw, sh) = (f64::from(dims.width.max(1)), f64::from(dims.height.max(1)));
                let crop = [l.crop.x / sw, l.crop.y / sh, l.crop.w / sw, l.crop.h / sh];
                let place = [
                    l.place.x / cw as f64,
                    l.place.y / ch as f64,
                    l.place.w / cw as f64,
                ];
                let role = match l.role.as_str() {
                    "detail" => json!("detail"),
                    "cam" | "screen" | "chat" | "alerts" | "banner" | "private"
                    | "captions_burned" => {
                        json!({"region": l.role.as_str()})
                    }
                    _ => json!("other"),
                };
                let lid = self.counted(
                    commits,
                    project,
                    "layer_add",
                    json!({
                        "owner": {"part_override": {"part": part, "framing": framing}},
                        "role": role,
                        "crop": {"x": crop[0], "y": crop[1], "w": crop[2], "h": crop[3]},
                        "place": {"x": place[0], "y": place[1], "w": place[2]},
                        "z": l.z,
                    }),
                )?["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if !l.is_visible() || l.opacity() < 1.0 {
                    self.counted(
                        commits,
                        project,
                        "layer_set",
                        json!({"layer": lid, "visible": l.is_visible(), "opacity": l.opacity()}),
                    )?;
                }
                layers.insert(
                    lid,
                    LayerSeed {
                        part: pi,
                        layer: li,
                        crop,
                        place,
                    },
                );
            }
            parts.insert(part.clone(), pi);
            after = Some(part);
        }
        let imp = ImportedCut {
            base: plan,
            map: ImportMap {
                version: 1,
                cut: cut.clone(),
                framing,
                sources: sources_map,
                parts,
                layers,
            },
        };
        let dir = p.home.join("imports");
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::internal(format!("imports folder: {}", e.kind())))?;
        let bytes = serde_json::to_vec(&imp).map_err(|_| AppError::internal("serialise import"))?;
        write_atomic(&dir.join(format!("{cut}.json")), &bytes)
            .map_err(|e| AppError::internal(format!("saving the import: {}", e.kind())))?;
        if let Ok(mut g) = p.imports.write() {
            g.insert(cut.clone(), Arc::new(imp));
        }
        self.invalidate(project);
        self.bus.home(crate::events::HomeEvent::Changed);
        Ok(json!({"cut": cut, "title": title}))
    }

    pub fn imported(&self, project: &str, cut: &str) -> AppResult<Arc<ImportedCut>> {
        let p = self.project(project)?;
        p.imports
            .read()
            .ok()
            .and_then(|g| g.get(cut).cloned())
            .ok_or_else(|| {
                AppError::new(
                    "Unsupported",
                    "only imported cuts can be previewed and exported until the model planner lands (A18)",
                )
            })
    }

    /// The font lookup the model planner uses (the activity crop plans with it, R.03).
    pub fn plan_host(&self) -> AppFonts {
        self.fonts.clone()
    }

    /// The plan for a cut now (cached per commit).
    pub fn planned(&self, project: &str, cut: &str) -> AppResult<Planned> {
        self.planned_in(project, cut, None)
    }

    /// The framing a cut shows (the output toggle), if it has one.
    pub fn active_framing(&self, project: &str, cut: &str) -> Option<String> {
        use gmclip_model::entities::Framing;
        let p = self.project(project).ok()?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = ulid_id::<Cut>(cut).ok()?;
        let pref = p.framing.read().ok().and_then(|g| g.get(cut).cloned());
        let all: Vec<String> = doc
            .children::<Framing>(cid.entity())
            .map(|f| f.0.to_string())
            .collect();
        pref.filter(|f| all.contains(f))
            .or_else(|| all.first().cloned())
    }

    /// Chooses the framing a cut shows and exports, and remembers it in the home.
    pub fn set_active_framing(&self, project: &str, cut: &str, framing: &str) -> AppResult<()> {
        let p = self.project(project)?;
        let map = {
            let mut g = p.framing.write().map_err(|_| AppError::internal("lock"))?;
            g.insert(cut.to_owned(), framing.to_owned());
            g.clone()
        };
        let bytes = serde_json::to_vec_pretty(&map).unwrap_or_default();
        write_atomic(&p.home.join("framing.json"), &bytes)
            .map_err(|e| AppError::internal(format!("saving the output choice: {}", e.kind())))?;
        self.invalidate(project);
        Ok(())
    }

    /// A cut planned in a given framing (`None`: the one it shows).
    pub fn planned_in(
        &self,
        project: &str,
        cut: &str,
        framing: Option<&str>,
    ) -> AppResult<Planned> {
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        let rev = snap.doc.rev();
        let active = match framing {
            Some(f) => Some(f.to_owned()),
            None => self.active_framing(project, cut),
        };
        let key = (
            project.to_owned(),
            format!("{cut}/{}", active.as_deref().unwrap_or("")),
        );
        let heard = snap.analysis.heard_len();
        if let Some(pl) = self.planned.lock().ok().and_then(|g| g.get(&key).cloned())
            && pl.rev == rev
            && pl.heard == heard
        {
            return Ok(pl);
        }
        // An imported RenderPlan re-tiles through the import planner; any other cut
        // through the model planner (A18).
        let (plan, index, edited) =
            self.plan_doc(project, cut, &snap.doc, &snap.analysis, active.as_deref())?;
        let pl = Planned {
            plan: Arc::new(plan),
            index: Arc::new(index),
            edited,
            rev,
            heard,
        };
        if let Ok(mut g) = self.planned.lock() {
            g.insert(key, pl.clone());
        }
        Ok(pl)
    }

    /// Plans `cut` on a given document (the project's, or a version of it with another
    /// opening, decision 0024): an imported cut through its base plan, any other through
    /// the model planner.
    pub fn plan_doc(
        &self,
        project: &str,
        cut: &str,
        doc: &gmclip_model::Doc,
        analysis: &gmclip_core::analysis::AnalysisView,
        framing: Option<&str>,
    ) -> AppResult<(Plan, PlanIndex, bool)> {
        match self.imported(project, cut) {
            Ok(imp) => planner::plan_cut(doc, &imp),
            Err(_) => model_plan(doc, analysis, cut, framing, &self.fonts),
        }
    }

    /// The cut planned at an upload preset (`youtube-1080p`, `youtube-1440p`) in its
    /// active framing, for an export (never cached: the preset is the export's).
    pub fn planned_preset(&self, project: &str, cut: &str, preset: &str) -> AppResult<Planned> {
        let preset = gmclip_core::planner::ExportPreset::parse(preset)
            .ok_or_else(|| AppError::invalid("no such export preset"))?;
        if !self
            .upload_presets(project, cut)
            .iter()
            .any(|p| p == preset.name())
        {
            return Err(AppError::new(
                "Unsupported",
                format!(
                    "{} is not offered for this cut's framing and source",
                    preset.name()
                ),
            ));
        }
        if self.imported(project, cut).is_ok() {
            return Err(AppError::new(
                "Unsupported",
                "an imported RenderPlan exports at its own output",
            ));
        }
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        let active = self.active_framing(project, cut);
        let (plan, index, edited) = model_plan_preset(
            &snap.doc,
            &snap.analysis,
            cut,
            active.as_deref(),
            &self.fonts,
            Some(preset),
        )?;
        Ok(Planned {
            plan: Arc::new(plan),
            index: Arc::new(index),
            edited,
            rev: snap.doc.rev(),
            heard: snap.analysis.heard_len(),
        })
    }

    /// Hears the stretches of a cut's captions that have no words yet (SPEC §5.10,
    /// decision 0022): the parts whose voice mix differs from their source (a gain, an
    /// envelope, no transcript of that audio) are rendered and transcribed, and the words
    /// stored under the stretch's signature (`heard_words`), so every later plan of any
    /// cut with the same audio has them. `Ok(0)`: nothing to hear (or an imported plan).
    pub fn hear_captions(
        &self,
        project: &str,
        cut: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> AppResult<usize> {
        use gmclip_core::planner::heard;
        use gmclip_model::entities::Framing;
        if self.imported(project, cut).is_ok() {
            return Ok(0);
        }
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        let cid = ulid_id::<Cut>(cut)?;
        let active = self.active_framing(project, cut);
        let Some(framing) = snap
            .doc
            .children::<Framing>(cid.entity())
            .find(|f| active.as_deref().is_none_or(|a| f.0.to_string() == a))
        else {
            return Ok(0);
        };
        let (plan, _, audit) = gmclip_core::planner::plan_cut_heard(
            &snap.doc,
            cid,
            framing,
            &self.fonts,
            Some(&snap.analysis),
            heard::HearPolicy::Rule,
        )
        .map_err(|e| AppError::new("Unsupported", format!("the cut does not plan yet: {}", e.0)))?;
        if audit.missing.is_empty() {
            return Ok(0);
        }
        let cli = self
            .analysis
            .cli()
            .ok_or_else(|| AppError::new("Unsupported", "this build has no speech model host"))?;
        let hearer = crate::analysis::PlanHearer { host: &cli, cancel };
        let mut words = heard::Hearer::hear(&hearer, &plan, &audit.missing)
            .map_err(|e| AppError::new("Failed", format!("hearing the captions: {e}")))?;
        // the correction pass on the cut's own audio too: the term list's names spelt
        let terms = crate::termlist::term_list(self, &snap.doc);
        for w in &mut words {
            gmclip_core::terms::correct_heard(&mut w.words, &terms);
        }
        for w in &words {
            p.handle
                .put_analysis(heard::OWNER, heard::KIND, &w.sig, w.encode())
                .map_err(|e| AppError::internal(format!("storing heard captions: {e}")))?;
        }
        Ok(words.len())
    }

    /// The upload presets the export offers for a cut (SPEC §5.14): YouTube 1080p on a
    /// 16:9 framing, and 1440p when its picture sources have ≥ 1440 lines.
    pub fn upload_presets(&self, project: &str, cut: &str) -> Vec<String> {
        use gmclip_model::entities::{Framing, Part};
        let Ok(p) = self.project(project) else {
            return Vec::new();
        };
        let doc = p.handle.snapshot().doc.clone();
        let Ok(cid) = ulid_id::<Cut>(cut) else {
            return Vec::new();
        };
        let active = self.active_framing(project, cut);
        let f = doc
            .children::<Framing>(cid.entity())
            .filter_map(|id| doc.get(id))
            .find(|f| active.as_deref().is_none_or(|a| f.id.0.to_string() == a));
        let Some(f) = f else { return Vec::new() };
        if u64::from(f.canvas.w) * 9 != u64::from(f.canvas.h) * 16 {
            return Vec::new();
        }
        let lines = doc
            .children::<Part>(cid.entity())
            .filter_map(|id| doc.get(id))
            .map(|pt| crate::framing::picture_dims(&doc, &pt.media).1)
            .fold(0.0f64, f64::max);
        let mut out = vec!["youtube-1080p".to_owned()];
        if lines >= 1440.0 {
            out.push("youtube-1440p".to_owned());
        }
        if lines >= 2160.0 {
            out.push("youtube-2160p".to_owned());
        }
        out
    }

    /// What the cut view shows: parts (with their model ids), layers, the caption lane.
    /// Each layer says where its crop and place come from (`from`, [`value_sources`]).
    pub fn cut_view(&self, project: &str, cut: &str) -> AppResult<Value> {
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        let doc = &snap.doc;
        let cid = ulid_id::<Cut>(cut)?;
        let c = doc
            .get(cid)
            .ok_or_else(|| AppError::not_found("no such cut"))?;
        let pl = self.planned(project, cut)?;
        let plan = &pl.plan;
        let fps = planner::fps(plan);
        let fid = self
            .active_framing(project, cut)
            .and_then(|f| ulid_id::<gmclip_model::entities::Framing>(&f).ok());
        let mut parts = Vec::new();
        for (k, (pp, ix)) in plan.parts.iter().zip(pl.index.parts.iter()).enumerate() {
            let mp = doc.get(ulid_id::<Part>(&ix.model)?);
            // the source time this plan part starts at (a visual split starts a new one)
            let t_src = pp
                .native
                .as_ref()
                .and_then(|n| n.frames.as_ref())
                .map(|r| planner::time_of(r.start() as i64, fps));
            let (src_first, src_end) = mp.map_or((0, 0), |m| {
                (
                    frame_of(m.src_in, fps).unwrap_or(0),
                    frame_of(m.src_out, fps).unwrap_or(0),
                )
            });
            let dims = |sid: &str| -> (f64, f64) {
                plan.sources
                    .iter()
                    .find(|s| s.id.as_str() == sid)
                    .and_then(|s| s.expect.as_ref()?.video.as_ref())
                    .map_or((0.0, 0.0), |v| {
                        (v.w.unwrap_or(0) as f64, v.h.unwrap_or(0) as f64)
                    })
            };
            let layers: Vec<Value> = pp
                .layers
                .iter()
                .map(|l| {
                    let src = l
                        .picture_source
                        .as_ref()
                        .unwrap_or(&pp.picture.source)
                        .as_str();
                    let (sw, sh) = dims(src);
                    let crop_n = (sw > 0.0 && sh > 0.0)
                        .then(|| [l.crop.x / sw, l.crop.y / sh, l.crop.w / sw, l.crop.h / sh]);
                    let model = ix
                        .layers
                        .iter()
                        .find(|(a, _)| a == l.id.as_str())
                        .map(|x| x.1.clone());
                    let addition = model
                        .as_deref()
                        .is_some_and(|m| crate::takelayout::is_addition(doc, m));
                    let from = match (mp, fid, model.as_deref(), t_src) {
                        (Some(m), Some(f), Some(l), Some(t)) => value_sources(doc, m, f, l, t),
                        _ => Value::Null,
                    };
                    json!({
                        "id": l.id.as_str(), "model": model, "role": l.role.as_str(), "addition": addition,
                        "crop": [l.crop.x, l.crop.y, l.crop.w, l.crop.h],
                        "place": [l.place.x, l.place.y, l.place.w, l.place.h],
                        "z": l.z, "visible": l.is_visible(), "opacity": l.opacity(),
                        "crop_n": crop_n, "src_size": [sw, sh],
                        // the video its crop is of (a recording's camera: the face file)
                        "source": src,
                        // a roto cut-out (R.01): {kind, model, soften?, strokes?} in source px
                        "matte": l.matte,
                        // rounded corners, shadow, border (decision 0046)
                        "frame": l.frame,
                        // where the crop and place come from (linked layouts, U.09)
                        "from": from,
                    })
                })
                .collect();
            parts.push(json!({
                "n": k + 1,
                "id": ix.model,
                "plan_id": pp.id.as_str(),
                "label": format!("{}", k + 1),
                "role": mp.map(|m| format!("{:?}", m.role).to_lowercase()),
                "out": [pp.out.start(), pp.out.end()],
                "src": [src_first, src_end],
                "source": pp.picture.source.as_str(),
                // its timeline group (decision 0037): grouped parts move together
                "group": mp.and_then(|m| m.part_group).map(|g| g.0.to_string()),
                "layers": layers,
                // follow-cursor moves on this part in the framing shown (§5.7)
                "follow_moves": match (mp, fid) {
                    (Some(m), Some(f)) => crate::cursorfollow::moves(doc, m, f),
                    _ => 0,
                },
            }));
        }
        let captions: Vec<Value> = plan
            .text_events
            .as_ref()
            .map(|evs| {
                evs.iter()
                    .map(|e| {
                        let text: String = e.runs.iter().map(|r| r.text.as_str()).collect();
                        json!({"id": e.id.as_str(), "out": [e.out.start(), e.out.end()],
                               "role": serde_json::to_value(e.role).unwrap_or(Value::Null), "text": text})
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sig = sig_of(plan);
        // the last export of the framing shown (an export from before framings were
        // recorded counts for it)
        let shown = self.active_framing(project, cut);
        let last = p.exports.lock().ok().and_then(|g| {
            g.iter()
                .filter(|e| e.cut == cut && (e.framing.is_none() || e.framing == shown))
                .max_by_key(|e| e.at_ms)
                .cloned()
        });
        // what the export will do to loudness, and what the last export measured
        let loudness = {
            let l = &plan.audio.loudness;
            let measured = last
                .as_ref()
                .and_then(|e| gmclip_media::export_record::ExportRecord::read(&e.file).ok())
                .and_then(|r| r.loudness)
                .map(|r| json!({"integrated": r.integrated, "true_peak": r.true_peak}));
            match l {
                gmclip_plan::types::Loudness::AsRecorded => {
                    json!({"mode": "as_recorded", "last": measured})
                }
                gmclip_plan::types::Loudness::Static {
                    integrated_lufs,
                    true_peak_dbtp,
                } => {
                    json!({"mode": "static", "target_lufs": integrated_lufs, "true_peak": true_peak_dbtp, "last": measured})
                }
            }
        };
        let mut export_state = match &last {
            None => json!({"state": "never"}),
            Some(e) if e.render_sig.is_some() && e.render_sig == sig => {
                json!({"state": "fresh", "at_ms": e.at_ms, "id": e.id})
            }
            // no render_sig (a source opened without its content hash): the plan itself
            Some(e)
                if e.render_sig.is_none()
                    && e.plan_sig.is_some()
                    && e.plan_sig == plan_sig(plan) =>
            {
                json!({"state": "fresh", "at_ms": e.at_ms, "id": e.id})
            }
            Some(e) => json!({"state": "stale", "at_ms": e.at_ms, "id": e.id,
                              "reason": "the cut changed after this export"}),
        };
        // the last verify of that file (R-EXP-6, H.06)
        if let Some(v) = last.as_ref().and_then(|e| e.verify.as_ref()) {
            export_state["verify"] = serde_json::to_value(v).unwrap_or(Value::Null);
        }
        Ok(json!({
            "project": project,
            "cut": cut,
            "title": c.title,
            "kind": c.kind.name(),
            "rev": pl.rev,
            "edited": pl.edited,
            "fps": {"num": fps.0, "den": fps.1},
            "canvas": {"w": plan.output.canvas.w.get(), "h": plan.output.canvas.h.get()},
            "frames": plan.output.frames,
            "parts": parts,
            "captions": captions,
            "words": {"state": "never", "note": "no transcript yet: ASR lands with M1 (parakeet)"},
            "render_sig": sig,
            "export": export_state,
            "loudness": loudness,
            "framing": crate::framing::framings(self, project, cut),
            // a paired hi-res picture: its offset against the sound (seconds)
            "picture_offset_s": crate::video::picture_offset(self, project, cut).ok().flatten().map(|x| x.1),
            "picture_offsets": crate::video::picture_offsets_view(self, project, cut),
            "upload_presets": self.upload_presets(project, cut),
            "agent_working": self.agent_working(project, &p.handle, cut),
        }))
    }

    /// R-CC-4 until M4: the agent run open on this cut now (an implicit or launched run
    /// that has written it, or a launched run bound to it), else `null`.
    pub fn agent_working(
        &self,
        project: &str,
        handle: &gmclip_core::ProjectHandle,
        cut: &str,
    ) -> Value {
        if let Some(r) = handle
            .open_runs()
            .into_iter()
            .find(|r| r.cut.as_deref() == Some(cut))
        {
            return json!({"run": r.run, "identity": r.identity});
        }
        self.agents
            .holder()
            .and_then(|h| {
                h.launched_runs()
                    .into_iter()
                    .find(|(_, p, c)| p == project && c == cut)
            })
            .map_or(
                Value::Null,
                |(r, _, _)| json!({"run": r, "identity": format!("launched:{r}")}),
            )
    }

    /// Trims a part's in or out edge to an absolute source frame (the UI never converts
    /// Moves a part's in or out edge by `delta` frames (the timeline's drag, the
    /// inspector's nudges). The core maps and clamps: the edge stays inside the source
    /// and on its side of the other edge; the 0.12 s minimum is `part_trim`'s (R-CMD-2:
    /// no frame or flicks arithmetic in the UI).
    pub fn trim_by(
        &self,
        project: &str,
        cut: &str,
        part: &str,
        edge: &str,
        delta: i64,
    ) -> AppResult<Value> {
        let (src_in, src_out) = self.trim_range(project, cut, part, edge, delta)?;
        let args = match edge {
            "in" => json!({"part": part, "src_in": src_in.0}),
            _ => json!({"part": part, "src_out": src_out.0}),
        };
        self.call(project, "part_trim", args)
    }

    /// The source range `part` has after [`Self::trim_by`] moves its `edge` by `delta`
    /// output frames.
    fn trim_range(
        &self,
        project: &str,
        cut: &str,
        part: &str,
        edge: &str,
        delta: i64,
    ) -> AppResult<(gmclip_model::time::Flicks, gmclip_model::time::Flicks)> {
        let pl = self.planned(project, cut)?;
        let fps = planner::fps(&pl.plan);
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        let mp = snap
            .doc
            .get(ulid_id::<Part>(part)?)
            .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
        let (a, b) = (
            frame_of(mp.src_in, fps).ok_or_else(|| AppError::invalid("src_in"))?,
            frame_of(mp.src_out, fps).ok_or_else(|| AppError::invalid("src_out"))?,
        );
        let delta = delta.clamp(-1_000_000, 1_000_000);
        match edge {
            "in" => Ok((time_of((a + delta).clamp(0, b - 1), fps), mp.src_out)),
            "out" => Ok((mp.src_in, time_of((b + delta).max(a + 1), fps))),
            _ => Err(AppError::invalid("edge is in or out")),
        }
    }

    /// The timeline's trim monitor (decision 0053): the cut planned as [`Self::trim_by`]
    /// would leave it, on a copy of the document (nothing committed or logged), and the
    /// output frame on the moved edge: the part's new first frame (in) or last (out).
    pub fn trim_peek(
        &self,
        project: &str,
        cut: &str,
        part: &str,
        edge: &str,
        delta: i64,
    ) -> AppResult<(Arc<Plan>, u64)> {
        use gmclip_model::staging::Staging;
        let (src_in, src_out) = self.trim_range(project, cut, part, edge, delta)?;
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        // a trim mints no ids
        let mut mint = || gmclip_model::ids::Ulid(0);
        let mut s = Staging::new(
            snap.doc.clone(),
            gmclip_model::op::Author::System,
            &mut mint,
        );
        gmclip_model::edit::trim_part(&mut s, ulid_id::<Part>(part)?, src_in, src_out)
            .map_err(|e| AppError::invalid(e.to_string()))?;
        let doc = s.into_work();
        let framing = self.active_framing(project, cut);
        let (plan, index, _) =
            self.plan_doc(project, cut, &doc, &snap.analysis, framing.as_deref())?;
        // a model part may be several plan parts (a visual split)
        let spans = plan
            .parts
            .iter()
            .zip(&index.parts)
            .filter(|(_, ix)| ix.model == part)
            .map(|(pp, _)| (pp.out.start(), pp.out.end()));
        let at = match edge {
            "in" => spans.map(|s| s.0).min(),
            _ => spans.map(|s| s.1).max().map(|e| e.saturating_sub(1)),
        }
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
        Ok((Arc::new(plan), at))
    }

    /// Ripple-deletes output frames `[from, to)` (the I/O marks): parts inside go, parts
    /// across an edge are trimmed to it, and a range inside one part splits it at `to`
    /// and trims the first half's out to `from` (R-MODEL-2: the first half keeps its id).
    /// One `cut_edit`, so one undo step; refused when nothing would be left.
    pub fn delete_range(&self, project: &str, cut: &str, from: u64, to: u64) -> AppResult<Value> {
        if to <= from {
            return Err(AppError::invalid("mark an in before the out (I, then O)"));
        }
        let pl = self.planned(project, cut)?;
        let fps = planner::fps(&pl.plan);
        let p = self.project(project)?;
        let snap = p.handle.snapshot();
        // the model parts on the output clock (a part held or split visually is several
        // plan parts of one model part)
        let mut spans: Vec<(&str, u64, u64)> = Vec::new();
        for (pp, ix) in pl.plan.parts.iter().zip(pl.index.parts.iter()) {
            match spans.last_mut() {
                Some(l) if l.0 == ix.model && l.2 == pp.out.start() => l.2 = pp.out.end(),
                _ => spans.push((ix.model.as_str(), pp.out.start(), pp.out.end())),
            }
        }
        let mut ops = Vec::new();
        let mut kept = 0u64;
        let mut removed = 0u64;
        for &(model, s, e) in &spans {
            let (a, b) = (from.max(s), to.min(e));
            if a >= b {
                kept += e - s;
                continue;
            }
            let mp = snap
                .doc
                .get(ulid_id::<Part>(model)?)
                .ok_or_else(|| AppError::not_found("a part of the range is gone"))?;
            let si = frame_of(mp.src_in, fps).ok_or_else(|| AppError::invalid("src_in"))?;
            let so = frame_of(mp.src_out, fps).ok_or_else(|| AppError::invalid("src_out"))?;
            if so - si != (e - s) as i64 {
                return Err(AppError::invalid(
                    "the range crosses a part at another speed; trim that part with ⇧I / ⇧O",
                ));
            }
            let src = |f: u64| time_of(si + (f - s) as i64, fps).0;
            removed += b - a;
            kept += (e - s) - (b - a);
            if a == s && b == e {
                ops.push(json!({"op": "part_delete", "part": model}));
            } else if a > s && b < e {
                ops.push(json!({"op": "part_split", "part": model, "at": src(b)}));
                ops.push(json!({"op": "part_trim", "part": model, "src_out": src(a)}));
            } else if a > s {
                ops.push(json!({"op": "part_trim", "part": model, "src_out": src(a)}));
            } else {
                ops.push(json!({"op": "part_trim", "part": model, "src_in": src(b)}));
            }
        }
        if ops.is_empty() {
            return Err(AppError::invalid("the marked range is past the cut's end"));
        }
        if kept == 0 {
            return Err(AppError::invalid(
                "that would remove the whole cut; mark a shorter range",
            ));
        }
        self.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))?;
        Ok(json!({"removed": removed, "frames": kept}))
    }

    /// Splits a part at an output frame (inside it).
    pub fn split_at_frame(&self, project: &str, cut: &str, out_frame: u64) -> AppResult<Value> {
        let pl = self.planned(project, cut)?;
        let fps = planner::fps(&pl.plan);
        let (pp, ix) = pl
            .plan
            .parts
            .iter()
            .zip(pl.index.parts.iter())
            .find(|(p, _)| p.out.start() < out_frame && out_frame < p.out.end())
            .ok_or_else(|| AppError::invalid("the playhead is not inside a part"))?;
        let src = pp.picture.first_frame as i64 + (out_frame - pp.out.start()) as i64;
        self.call(
            project,
            "part_split",
            json!({"part": ix.model, "at": time_of(src, fps).0}),
        )
    }

    /// Sets a layer's place from canvas pixels (the viewer's gizmo commit).
    #[allow(clippy::too_many_arguments)]
    pub fn place_px(
        &self,
        project: &str,
        cut: &str,
        plan_part: &str,
        plan_layer: &str,
        x: f64,
        y: f64,
        w: f64,
        rev: Option<u64>,
    ) -> AppResult<Value> {
        self.place_px_group(project, cut, plan_part, plan_layer, x, y, w, rev, &[])
    }

    /// [`Self::place_px`], on every layer of `group` (model `(part, layer)`s) too when this
    /// one is in it: the same place on each grouped part, one transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn place_px_group(
        &self,
        project: &str,
        cut: &str,
        plan_part: &str,
        plan_layer: &str,
        x: f64,
        y: f64,
        w: f64,
        rev: Option<u64>,
        group: &[(String, String)],
    ) -> AppResult<Value> {
        let pl = self.planned(project, cut)?;
        stale(rev, pl.rev)?;
        let (part, layer) = pl
            .index
            .model_layer(plan_part, plan_layer)
            .ok_or_else(|| AppError::not_found("that layer is not in the cut any more"))?;
        let (cw, ch) = (
            pl.plan.output.canvas.w.get() as f64,
            pl.plan.output.canvas.h.get() as f64,
        );
        let mut args = json!({"layer": layer, "place": {"x": x / cw, "y": y / ch, "w": w / cw}});
        // A model-planned cut moves the layer for this part only (its override in the
        // framing shown); "apply to all parts" copies it on. Imported cuts keep their
        // one-layer-per-part import map.
        if self.imported(project, cut).is_err()
            && let Some(f) = self.active_framing(project, cut)
        {
            if let Some(r) = crate::framing::set_on_group(
                self,
                project,
                cut,
                &f,
                part,
                layer,
                group,
                "layer_place",
                "place",
                &args["place"],
            ) {
                return r;
            }
            args["part"] = json!(part);
            args["framing"] = json!(f);
        }
        self.call(project, "place_set", args)
    }

    /// Sets a layer's crop from source pixels (the inspector).
    pub fn crop_px(
        &self,
        project: &str,
        cut: &str,
        plan_part: &str,
        plan_layer: &str,
        r: [f64; 4],
        rev: Option<u64>,
    ) -> AppResult<Value> {
        self.crop_px_group(project, cut, plan_part, plan_layer, r, rev, &[])
    }

    /// [`Self::crop_px`], on every layer of `group` too when this one is in it (the same
    /// fraction of each one's picture).
    #[allow(clippy::too_many_arguments)]
    pub fn crop_px_group(
        &self,
        project: &str,
        cut: &str,
        plan_part: &str,
        plan_layer: &str,
        r: [f64; 4],
        rev: Option<u64>,
        group: &[(String, String)],
    ) -> AppResult<Value> {
        let pl = self.planned(project, cut)?;
        stale(rev, pl.rev)?;
        let Ok(imp) = self.imported(project, cut) else {
            // A model-planned cut crops this part only (its override in the framing shown),
            // in the pixels of the layer's own picture source.
            let (part, layer) = pl
                .index
                .model_layer(plan_part, plan_layer)
                .ok_or_else(|| AppError::not_found("that layer is not in the cut any more"))?;
            let pp = pl
                .plan
                .parts
                .iter()
                .find(|p| p.id.as_str() == plan_part)
                .ok_or_else(|| AppError::not_found("no such part"))?;
            let l = pp
                .layers
                .iter()
                .find(|l| l.id.as_str() == plan_layer)
                .ok_or_else(|| AppError::not_found("no such layer"))?;
            let src = l.picture_source.as_ref().unwrap_or(&pp.picture.source);
            let (sw, sh) = pl
                .plan
                .sources
                .iter()
                .find(|s| s.id.as_str() == src.as_str())
                .and_then(|s| s.expect.as_ref()?.video.as_ref())
                .and_then(|v| Some((v.w? as f64, v.h? as f64)))
                .ok_or_else(|| AppError::internal("no source size"))?;
            return crate::framing::crop_norm_group(
                self,
                project,
                cut,
                part,
                layer,
                [r[0] / sw, r[1] / sh, r[2] / sw, r[3] / sh],
                group,
            );
        };
        let (_, layer) = pl
            .index
            .model_layer(plan_part, plan_layer)
            .ok_or_else(|| AppError::not_found("that layer is not in the cut any more"))?;
        let pp = pl
            .plan
            .parts
            .iter()
            .find(|p| p.id.as_str() == plan_part)
            .ok_or_else(|| AppError::not_found("no such part"))?;
        let l = pp
            .layers
            .iter()
            .find(|l| l.id.as_str() == plan_layer)
            .ok_or_else(|| AppError::not_found("no such layer"))?;
        let src = l.picture_source.as_ref().unwrap_or(&pp.picture.source);
        let d = imp
            .map
            .sources
            .get(src.as_str())
            .ok_or_else(|| AppError::internal("no source size"))?;
        let (sw, sh) = (f64::from(d.width), f64::from(d.height));
        self.call(
            project,
            "crop_set",
            json!({"layer": layer, "crop": {"x": r[0] / sw, "y": r[1] / sh, "w": r[2] / sw, "h": r[3] / sh}}),
        )
    }

    pub fn set_export_verdict(
        &self,
        project: &str,
        export: &str,
        v: Option<String>,
    ) -> AppResult<()> {
        let p = self.project(project)?;
        let mut g = p.exports.lock().map_err(|_| AppError::internal("lock"))?;
        let e = g
            .iter_mut()
            .find(|e| e.id == export)
            .ok_or_else(|| AppError::not_found("no such export"))?;
        e.verdict = v;
        let bytes = serde_json::to_vec_pretty(&*g).map_err(|_| AppError::internal("serialise"))?;
        write_atomic(&p.home.join("exports.json"), &bytes)
            .map_err(|e| AppError::internal(format!("saving the export list: {}", e.kind())))
    }

    /// Records a verify of one export (H.06).
    pub fn set_export_verify(
        &self,
        project: &str,
        export: &str,
        v: crate::exportverify::Verify,
    ) -> AppResult<()> {
        let p = self.project(project)?;
        let mut g = p.exports.lock().map_err(|_| AppError::internal("lock"))?;
        let e = g
            .iter_mut()
            .find(|e| e.id == export)
            .ok_or_else(|| AppError::not_found("no such export"))?;
        e.verify = Some(v);
        let bytes = serde_json::to_vec_pretty(&*g).map_err(|_| AppError::internal("serialise"))?;
        write_atomic(&p.home.join("exports.json"), &bytes)
            .map_err(|e| AppError::internal(format!("saving the export list: {}", e.kind())))
    }

    pub fn add_export(&self, project: &str, e: ExportEntry) -> AppResult<()> {
        let p = self.project(project)?;
        let mut g = p.exports.lock().map_err(|_| AppError::internal("lock"))?;
        g.push(e);
        let bytes = serde_json::to_vec_pretty(&*g).map_err(|_| AppError::internal("serialise"))?;
        write_atomic(&p.home.join("exports.json"), &bytes)
            .map_err(|e| AppError::internal(format!("saving the export list: {}", e.kind())))
    }

    pub fn exports(&self, project: &str) -> AppResult<Vec<ExportEntry>> {
        let p = self.project(project)?;
        Ok(p.exports.lock().map(|g| g.clone()).unwrap_or_default())
    }

    /// Exports dir for a project: `<exports>/<project name>/`.
    pub fn export_dir(&self, project: &str) -> AppResult<PathBuf> {
        let p = self.project(project)?;
        Ok(self.dirs.exports.join(safe_name(&p.info.name)))
    }
}

/// Refuses an edit addressed by plan ids from an older revision of the cut (ids of split
/// pieces follow layout order, so after a reorder the same id can name another part).
fn stale(asked: Option<u64>, now: u64) -> AppResult<()> {
    match asked {
        Some(r) if r != now => Err(AppError::new(
            "Conflict",
            "the cut changed since this was shown; look again and retry",
        )),
        _ => Ok(()),
    }
}

/// Largest RenderPlan file the importer reads.
const MAX_PLAN: u64 = 64 << 20;

/// `render_sig` of a plan, when every source is hashed.
/// The plan's own digest (canonical JSON), for freshness when `render_sig` is absent.
pub fn plan_sig(plan: &Plan) -> Option<String> {
    serde_json::to_vec(plan)
        .ok()
        .map(|b| gmclip_ai::launcher::sha256::hex(&b))
}

pub fn sig_of(plan: &Plan) -> Option<String> {
    match gmclip_media::export_record::render_sig_of(plan) {
        gmclip_media::export_record::RenderSig::Sig(s) => Some(s),
        gmclip_media::export_record::RenderSig::Absent(_) => None,
    }
}

fn load_imports(home: &Path) -> BTreeMap<String, Arc<ImportedCut>> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(home.join("imports")) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.len() > 64 << 20 {
            continue;
        }
        if let Ok(imp) = serde_json::from_slice::<ImportedCut>(&bytes)
            && gmclip_plan::check_plan(&imp.base).is_empty()
        {
            out.insert(imp.map.cut.clone(), Arc::new(imp));
        }
    }
    out
}

pub(crate) fn load_exports(home: &Path) -> Vec<ExportEntry> {
    std::fs::read(home.join("exports.json"))
        .ok()
        .filter(|b| b.len() < 16 << 20)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Where a layer's crop and place come from in a part at source time `t` (U.09, SPEC
/// §5.7 "inherited values shown with their source"): `{crop, place, owner, layout?,
/// split_s?, fields}` with each of crop/place one of `split` (a visual split in force sets
/// it), `part` (the part's own override), `layout` (the linked layout's layer),
/// `framing` (the framing's default stack) or `own` (a layer the part added); `fields`
/// is the core's whole answer (`gmclip_model::effective::value_sources`, A2.09), the
/// same the agents' `cut_get{look}` reads.
pub fn value_sources(
    doc: &gmclip_model::Doc,
    part: &Part,
    framing: gmclip_model::Id<gmclip_model::entities::Framing>,
    layer: &str,
    t: gmclip_model::Flicks,
) -> Value {
    use gmclip_model::entities::{Layer, VisualSplit};
    let Ok(lid) = ulid_id::<Layer>(layer) else {
        return Value::Null;
    };
    let Some(v) = gmclip_model::effective::value_sources(doc, part.id, framing, lid, t) else {
        return Value::Null;
    };
    let layout = v.layout.and_then(|id| doc.get(id)).map(|x| x.name.clone());
    let split = v.split.and_then(|id| doc.get::<VisualSplit>(id));
    json!({
        "crop": v.crop,
        "place": v.place,
        "owner": v.owner,
        "layout": layout,
        "split_s": split.map(|s| (s.at_src.0 - part.src_in.0) as f64 / planner::FLICKS as f64),
        "fields": v,
    })
}
