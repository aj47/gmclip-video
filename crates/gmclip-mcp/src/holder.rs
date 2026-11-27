//! The holder (R-ARCH-2/3/6, A19): one process hosting the project actors that agents
//! and CLIs reach over the socket. It knows which projects are open, which launched runs
//! exist, which Owner challenges are pending, and who is connected.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use gmclip_core::command::Scope;
use gmclip_core::commands::project::ProjectEntry;
use gmclip_core::{
    CallRequest, Caller, CmdError, HolderKind, OpenOptions, ProjectHandle, StoreError, StoreOptions,
};
use gmclip_model::entities::Cut;
use gmclip_model::{Id, Ulid};
use gmclip_proto::Base;
use gmclip_proto::CallerKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::fsroots::{self, Roots};
use crate::paths::Env;
use crate::project::{self, HomeInfo};
use crate::settings::{self, OwnerSettings};
use crate::wire;

/// Default scopes of a local agent (§11.4).
pub const DEFAULT_SCOPES: [Scope; 4] = [Scope::Read, Scope::Edit, Scope::Render, Scope::Ai];

/// Scopes of an outside agent (a client gmclip did not launch, decision 0048): the
/// defaults plus `read_media`. It runs as the user with their shell and can read the media
/// files itself, so `see`, `listen` and face analysis protect nothing by refusing it;
/// private regions stay masked (unmasking is a launched run's, with an Owner code).
pub const OUTSIDE_SCOPES: [Scope; 5] = [
    Scope::Read,
    Scope::Edit,
    Scope::Render,
    Scope::Ai,
    Scope::ReadMedia,
];

/// An open project.
#[derive(Clone, Debug)]
pub struct OpenProject {
    pub handle: ProjectHandle,
    pub info: HomeInfo,
    pub home: PathBuf,
}

/// A launched run's registration (R-AGT-5).
pub struct Launched {
    credential: Vec<u8>,
    pub project: String,
    pub cut: Id<Cut>,
    pub scopes: Vec<Scope>,
    pub private_regions: bool,
    /// sha256 of the human's brief the credential is bound to (R-AGT-5).
    pub brief_sha256: Option<String>,
    /// A full-access session's run (decision 0043): project-wide commands on its own
    /// project, and its session grant's folders.
    pub full_access: bool,
    pub grant: Option<String>,
    expires: Instant,
    used_client_nonces: HashSet<String>,
}

impl std::fmt::Debug for Launched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launched")
            .field("credential", &"<redacted>")
            .field("project", &self.project)
            .field("cut", &self.cut)
            .field("scopes", &self.scopes)
            .field("private_regions", &self.private_regions)
            .field("brief_sha256", &self.brief_sha256)
            .finish_non_exhaustive()
    }
}

/// A pending Owner challenge: a one-time code bound to a command and its arguments.
#[derive(Debug)]
struct OwnerChallenge {
    command: String,
    args_hash: String,
    expires: Instant,
}

/// One connection's identity and what it touched.
#[derive(Debug)]
pub struct Conn {
    /// Unique per connection (implicit runs and idempotency are scoped to it).
    pub id: String,
    pub pid: Option<u32>,
    pub exe: Option<PathBuf>,
    pub client_exe: Option<String>,
    /// `launched:<run>` once proven, else `local:<client>` after the first request.
    pub identity: Mutex<Option<String>>,
    pub launched: Option<LaunchedRef>,
    /// The session grant this connection proved (decision 0039): a session the human
    /// started with full access, whose gmclip tools may open files in the folders the
    /// grant names too. Never set together with `launched`.
    pub granted: Option<String>,
    /// The agent session this connection belongs to (`GMCLIP_SESSION`, decision 0050):
    /// its identity is `local:session-<label>` and its run is the session's, across every
    /// connection that names it. Never set together with `launched`.
    pub session: Option<String>,
    /// Projects this connection wrote to (their implicit runs end on disconnect).
    pub touched: Mutex<BTreeSet<String>>,
    pub logged: Mutex<bool>,
}

/// What a launched connection is bound to.
#[derive(Clone, Debug)]
pub struct LaunchedRef {
    pub run: String,
    pub project: String,
    pub cut: Id<Cut>,
    pub scopes: Vec<Scope>,
    /// The Owner granted this run unmasked private regions (`--private-regions`).
    pub private_regions: bool,
    /// A full-access session's run (decision 0043): may run project-wide commands
    /// (imports, the app's own) on its own project; its edits stay bound to its cut.
    pub full_access: bool,
}

/// A session grant (decision 0039): the app registers one in-process when the human
/// starts an agent session with full access; its shim proves the credential like a
/// launched run, and the connection's file checks add the grant's folders to the agent
/// read roots. It widens nothing else: scopes, identity and implicit runs stay local.
struct Grant {
    credential: Vec<u8>,
    roots: Vec<PathBuf>,
    expires: Instant,
    used_client_nonces: HashSet<String>,
}

/// What a handshake proved: a launched run, a session grant, or neither (local).
#[derive(Debug, Default)]
pub struct Proven {
    pub launched: Option<LaunchedRef>,
    pub granted: Option<String>,
}

impl Conn {
    /// The caller this connection acts as.
    pub fn caller(&self) -> Caller {
        let identity = self
            .identity
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| "local:unknown".into());
        match &self.launched {
            Some(l) => Caller {
                identity,
                // every connection of a launched run shares the run's implicit run
                connection: format!("launched:{}", l.run),
                kind: CallerKind::Agent,
                scopes: l.scopes.clone(),
                run: None,
                cut_binding: Some(l.cut),
            },
            None => Caller {
                identity,
                // a session's connections share its implicit run (and request ids)
                connection: self.run_key(),
                kind: CallerKind::Agent,
                scopes: OUTSIDE_SCOPES.to_vec(),
                run: None,
                cut_binding: None,
            },
        }
    }

    pub fn scopes(&self) -> Vec<Scope> {
        self.launched
            .as_ref()
            .map_or_else(|| OUTSIDE_SCOPES.to_vec(), |l| l.scopes.clone())
    }

    /// What this connection's run is keyed by (the actor's implicit runs, approvals and
    /// their pause, R-PROTO-6): its launched run, its session, or itself.
    pub fn run_key(&self) -> String {
        match (&self.launched, &self.session) {
            (Some(l), _) => format!("launched:{}", l.run),
            (None, Some(s)) => format!("session:{s}"),
            (None, None) => self.id.clone(),
        }
    }

    /// The identity a local connection takes from its client's name, or from its session.
    pub fn local_identity(&self, client: &str) -> String {
        match &self.session {
            Some(s) => format!("local:session-{s}"),
            None => format!("local:{}", client_label(client)),
        }
    }
}

/// A sanitised client name for `local:<name>`.
pub fn client_label(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() { "unknown".into() } else { s }
}

/// The holder. `Debug` shows counts only: the Owner map is keyed by live codes and the
/// launched map holds credentials.
/// At most this many pending approvals per run (R-PROTO-6).
pub const MAX_PENDING_APPROVALS: usize = 3;

/// One approval an agent's call needs (R-PROTO-6, R-UI-SEC-2): the effects are what the
/// server computed; `prose` is what the agent said about it (untrusted, shown as such).
#[derive(Clone, Debug, serde::Serialize)]
pub struct Approval {
    pub id: String,
    pub identity: String,
    /// The run it was asked in ([`Conn::run_key`]): R-PROTO-6 pauses that run, not its
    /// siblings (decision 0050).
    pub run: String,
    pub tool: String,
    pub effects: Value,
    pub prose: Option<String>,
    pub ts_ms: i64,
    /// The run asked a 4th time and is paused until a human reviews it.
    pub run_paused: bool,
    /// What makes two asks the same: the tool, and the cut's revision and findings when
    /// the effects name them (else the whole effects).
    #[serde(skip)]
    same: String,
}

/// What [`Holder::approval_add`] did with an ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// Queued for a human, as this approval.
    New(String),
    /// The same as one already waiting (that one): not counted again.
    Again(String),
    /// The run already has [`MAX_PENDING_APPROVALS`] waiting: it is paused for review.
    Paused,
}

/// Where a human resumes a run paused for asking too often (R-PROTO-6).
pub const RESUME_RUN: &str = "a human approves or dismisses its asks in gmclip: Home → Needs you, or Agents → Outside agents and approvals (“Let it continue”)";

/// Where a human resumes agents after "Pause agents".
pub const RESUME_AGENTS: &str =
    "a human resumes agents in gmclip: Agents → Outside agents and approvals → Resume agents";

/// At most this many approvals wait in all (the oldest go first).
const MAX_APPROVALS: usize = 64;

pub struct Holder {
    pub env: Env,
    projects: Mutex<BTreeMap<String, OpenProject>>,
    launched: Mutex<HashMap<String, Launched>>,
    owner: Mutex<HashMap<String, OwnerChallenge>>,
    conn_seq: AtomicU64,
    pub active: AtomicUsize,
    pub last_activity: Mutex<Instant>,
    pub shutdown: tokio::sync::Notify,
    /// For tests: a fixed clock seed for the actors' ULIDs is not needed; projects open
    /// with the system clock.
    pub started: Instant,
    pub notices: Mutex<Vec<String>>,
    /// Where Owner codes go: the requesting process's own terminal, or (in-process
    /// tests only) a sink.
    code_sink: Mutex<Option<CodeSink>>,
    /// Where first-seen notices go besides stderr and `gmclip status`: the app's activity
    /// rail (R-MCP-1c UI).
    notice_sink: Mutex<Option<NoticeSink>>,
    /// Wrong Owner codes in the current minute (throttled after 5).
    owner_failures: Mutex<(u32, Instant)>,
    /// The read-root handles, reopened only when the list of roots changes.
    roots: Mutex<Option<(Vec<PathBuf>, Arc<Roots>)>>,
    /// Session grants by id (decision 0039).
    grants: Mutex<HashMap<String, Grant>>,
    /// gmclip's library folder (`~/Movies/gmclip/Library`): the projects in it are
    /// readable and openable by name or id (decision 0039). The app sets it; a bare
    /// holder derives it from app data.
    library_dir: Mutex<Option<PathBuf>>,
    /// The human's home folder (decision 0058: an agent read root unless the Owner
    /// removed it). The app sets it; a bare holder derives it from app data.
    human_home: Mutex<Option<PathBuf>>,
    /// The model planner (A18) and the renderer/exporter (CLI or app).
    pub planner: Arc<dyn gmclip_core::Planner>,
    pub media: Arc<dyn gmclip_core::MediaRender>,
    pub jobs: crate::media::Jobs,
    /// Analysis and ASR (lane D's, from the CLI or app; null in a bare holder).
    pub analysis: Arc<dyn gmclip_core::AnalysisHost>,
    /// `<project>:<source>` keys background analysis was started for.
    analysis_started: Mutex<std::collections::HashSet<String>>,
    /// Projects another owner of the handle (the app) opened and closes (R-ARCH-2: the
    /// app hosts the holder in-process).
    adopted: Mutex<std::collections::HashSet<String>>,
    /// "Pause agents" (the activity rail): every agent tool call is refused until resumed.
    paused: std::sync::atomic::AtomicBool,
    /// Pending approvals (R-PROTO-6): what an agent asked a human to approve, with the
    /// server-computed effects; a run with 3 pending is paused at its 4th ask.
    approvals: Mutex<Vec<Approval>>,
    /// Told when the pending approvals change (the app's Home "Needs you").
    approval_sink: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Running transcriptions' windows so far, by job key (`<project>:<source>`): what a
    /// handover gives the app to resume from (R-ARCH-7).
    pub asr_partials: Arc<Mutex<BTreeMap<String, gmclip_core::analysis::chunked::AsrPartial>>>,
    /// Set by a handover: new connections are refused while the holder winds down.
    pub handing_over: std::sync::atomic::AtomicBool,
    /// Launched runs limited to some commands (a pass that may only write its output,
    /// e.g. grow-story → story suggestions, R-STORY-1), by run id.
    run_commands: Mutex<HashMap<String, BTreeSet<String>>>,
    /// The library index (the app's; `library_*` commands answer `Unsupported` without).
    library: Mutex<Option<Arc<dyn LibraryHost>>>,
    /// Adds a video file to a project (the app's; `source_import` answers `Unsupported`
    /// without).
    importer: Mutex<Option<Arc<dyn SourceImporter>>>,
    /// The app's own commands agents may run too (`command_run`; the app's, none in a
    /// bare holder).
    app: Mutex<Option<Arc<dyn AppHost>>>,
    /// App export jobs by id, with who started them (`job_wait` and `verify` answer the
    /// identity that did).
    app_jobs: Mutex<HashMap<String, AppJobOwner>>,
    /// Agent-executed find-moments / full-sweep runs (`ai_pass` / `pass_step`).
    pub pass_runs: crate::findpass::PassRuns,
    /// Agent-executed title/description runs (`ai_pass{pass: publish_copy}`).
    pub copy_runs: crate::copypass::CopyRuns,
}

/// What `source_import` calls once the holder has checked the file is inside the agent
/// read roots (R-FS-1): probe it, add it as a source on the end of the main cut, start its
/// analysis. The app implements it; `path` is canonical and is a regular file.
pub trait SourceImporter: Send + Sync {
    fn import(&self, project: &str, path: &Path, caller: &Caller) -> Result<Value, CmdError>;
}

/// The app's commands an agent may run like the human does in the window (project_new,
/// video_open, export_start, …): what is not a registry command because it is the app's
/// (files, the library, exports). The app implements it and lists which of its commands
/// agents get: never Owner actions, human decisions, agent management or the window's
/// own plumbing. Path arguments are checked against the agent read roots first.
pub trait AppHost: Send + Sync {
    /// `(name, doc)` of each command agents may run.
    fn commands(&self) -> &'static [(&'static str, &'static str)];
    /// Runs one, as the app's IPC would (`project`: the open project it acts on), with
    /// every registry call it makes as `caller` (decision 0050): the agent's, in its run,
    /// so the run's undo covers it and History names the agent.
    fn call(
        &self,
        project: Option<&str>,
        command: &str,
        args: &Value,
        caller: &Caller,
    ) -> Result<Value, CmdError>;
    /// An export job the app runs (`export_start`, `export_both`, `opening_export`),
    /// after waiting up to `wait` for it to finish: `job_wait` and `verify` see the app's
    /// exports as they see the holder's (decision 0050). `None`: no such job.
    fn job(&self, _id: &str, _wait: Duration) -> Option<AppJob> {
        None
    }
}

/// An app export job as `job_wait` and `verify` see it.
#[derive(Clone, Debug)]
pub struct AppJob {
    /// `queued`, `running`, `done`, `failed` or `cancelled`.
    pub state: String,
    pub project: String,
    pub cut: String,
    /// Frames written of the total, while it runs.
    pub progress: Option<Value>,
    /// When it finished: `{out, export}` or `{error}`.
    pub result: Option<Value>,
    /// The plan it rendered and the file it wrote (done only).
    pub plan: Option<Arc<gmclip_core::gmclip_plan::Plan>>,
    pub out: Option<PathBuf>,
}

/// Who started an app export job through the holder (an agent's `command_run`, or a
/// human's "Approve and export" on that agent's approval).
#[derive(Clone, Debug)]
pub struct AppJobOwner {
    pub identity: String,
    pub project: String,
    pub cut: String,
}

/// The argument names that carry a file or folder path in the app's commands.
const APP_PATH_KEYS: &[&str] = &[
    "path", "paths", "hires", "voice", "dest", "home", "file", "dir", "folder",
];

/// What the library-wide commands read (L.01, L.03): the app implements it over its
/// library index; a bare holder (`gmclip serve`) has none. Every text a result carries
/// is source-derived and must come back marked untrusted (R-INJ-1).
pub trait LibraryHost: Send + Sync {
    /// `library_search`.
    fn search(&self, args: &Value) -> Result<Value, CmdError>;
    /// `chat_spikes`: an imported chat log's spikes for one project's source.
    fn chat_spikes(&self, project: &str, args: &Value) -> Result<Value, CmdError>;
}

impl std::fmt::Debug for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = |m: &Mutex<HashMap<String, Launched>>| m.lock().map_or(0, |g| g.len());
        f.debug_struct("Holder")
            .field("env", &self.env)
            .field("projects", &self.projects.lock().map_or(0, |g| g.len()))
            .field("launched", &n(&self.launched))
            .field("owner_pending", &self.owner.lock().map_or(0, |g| g.len()))
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

/// A destination for Owner codes other than a terminal (tests in this process).
pub struct CodeSink(pub SinkFn);

/// `(command, code)` → wherever the test wants it.
pub type SinkFn = Box<dyn Fn(&str, &str) + Send + Sync>;

impl std::fmt::Debug for CodeSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodeSink")
    }
}

/// A destination for first-seen notices (the app publishes them to its UI).
pub struct NoticeSink(pub Box<dyn Fn(&settings::Notice) + Send + Sync>);

impl std::fmt::Debug for NoticeSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoticeSink")
    }
}

/// Max pending Owner challenges, launched runs, in-memory notices.
const MAX_PENDING: usize = 16;
const MAX_LAUNCHED: usize = 256;

/// The controlling terminal of a process, from the kernel (`ps -o tty=`), as a
/// character device owned by this user; `None` without one.
#[cfg(unix)]
fn tty_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if name.is_empty()
        || name.starts_with('?')
        || name.contains("..")
        || name.contains('/') && !name.starts_with("pts/")
    {
        return None;
    }
    let p = PathBuf::from("/dev").join(&name);
    let m = std::fs::metadata(&p).ok()?;
    (m.file_type().is_char_device() && m.uid() == crate::paths::our_uid()).then_some(p)
}

/// Windows has no controlling-terminal device to write an Owner code to, so the Owner
/// path fails closed ("needs an interactive terminal") until M5 designs one
/// (decisions/0019: the console session or the app's own UI).
#[cfg(windows)]
fn tty_of(_pid: u32) -> Option<PathBuf> {
    None
}

fn args_hash(command: &str, args: &Value) -> String {
    let mut h = Sha256::new();
    h.update(command.as_bytes());
    h.update(b"\x00");
    h.update(serde_json::to_string(args).unwrap_or_default().as_bytes());
    wire::hex(&h.finalize())
}

fn owner_denied() -> CmdError {
    CmdError::Unauthorized {
        scope: "owner".into(),
    }
}

fn store_err(e: &StoreError) -> CmdError {
    match e {
        StoreError::Leased { .. } => CmdError::Leased,
        _ => CmdError::Internal {
            trace_id: format!("open:{}", crate::canon::clip(&e.to_string(), 40)),
        },
    }
}

impl Holder {
    pub fn new(env: Env) -> Holder {
        Holder {
            env,
            projects: Mutex::new(BTreeMap::new()),
            launched: Mutex::new(HashMap::new()),
            owner: Mutex::new(HashMap::new()),
            conn_seq: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
            started: Instant::now(),
            notices: Mutex::new(Vec::new()),
            code_sink: Mutex::new(None),
            notice_sink: Mutex::new(None),
            owner_failures: Mutex::new((0, Instant::now())),
            roots: Mutex::new(None),
            grants: Mutex::new(HashMap::new()),
            library_dir: Mutex::new(None),
            human_home: Mutex::new(None),
            planner: Arc::new(gmclip_core::NoPlanner),
            media: Arc::new(gmclip_core::NullMediaRender),
            jobs: crate::media::Jobs::default(),
            analysis: Arc::new(gmclip_core::NullAnalysisHost),
            analysis_started: Mutex::new(std::collections::HashSet::new()),
            adopted: Mutex::new(std::collections::HashSet::new()),
            paused: std::sync::atomic::AtomicBool::new(false),
            approvals: Mutex::new(Vec::new()),
            approval_sink: Mutex::new(None),
            asr_partials: Arc::new(Mutex::new(BTreeMap::new())),
            handing_over: std::sync::atomic::AtomicBool::new(false),
            run_commands: Mutex::new(HashMap::new()),
            library: Mutex::new(None),
            importer: Mutex::new(None),
            app: Mutex::new(None),
            app_jobs: Mutex::new(HashMap::new()),
            pass_runs: crate::findpass::PassRuns::default(),
            copy_runs: crate::copypass::CopyRuns::default(),
        }
    }

    /// Sets the library index the `library_*` commands read (the app does, once).
    pub fn set_library(&self, l: Arc<dyn LibraryHost>) {
        if let Ok(mut g) = self.library.lock() {
            *g = Some(l);
        }
    }

    pub fn set_importer(&self, i: Arc<dyn SourceImporter>) {
        if let Ok(mut g) = self.importer.lock() {
            *g = Some(i);
        }
    }

    pub fn set_app(&self, a: Arc<dyn AppHost>) {
        if let Ok(mut g) = self.app.lock() {
            *g = Some(a);
        }
    }

    /// The app's host when it lets agents run `command`.
    pub fn app_command(&self, command: &str) -> Option<Arc<dyn AppHost>> {
        let a = self.app.lock().ok().and_then(|g| g.clone())?;
        a.commands().iter().any(|(n, _)| *n == command).then_some(a)
    }

    /// The app's host, if this holder serves an app.
    pub fn app(&self) -> Option<Arc<dyn AppHost>> {
        self.app.lock().ok().and_then(|g| g.clone())
    }

    /// Records who started an app export job (the newest 256 are kept).
    pub fn app_job_note(&self, job: &str, owner: AppJobOwner) {
        if let Ok(mut g) = self.app_jobs.lock() {
            if g.len() >= 256 {
                g.clear();
            }
            g.insert(job.to_owned(), owner);
        }
    }

    /// Who started an app export job, if it went through the holder.
    pub fn app_job_owner(&self, job: &str) -> Option<AppJobOwner> {
        self.app_jobs.lock().ok().and_then(|g| g.get(job).cloned())
    }

    /// The app commands agents may run, `(name, doc)`.
    pub fn app_commands(&self) -> &'static [(&'static str, &'static str)] {
        self.app
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|a| a.commands()))
            .unwrap_or(&[])
    }

    /// An app command's path arguments (R-FS-1): each an existing file or folder inside the
    /// agent read roots, or a new file in a folder inside them; each is replaced by its
    /// canonical path.
    pub fn check_app_paths(&self, conn: &Conn, v: &mut Value) -> Result<(), CmdError> {
        let roots = self.roots_for(conn);
        self.check_paths_in(&roots, v)
    }

    fn check_paths_in(&self, roots: &Roots, v: &mut Value) -> Result<(), CmdError> {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if APP_PATH_KEYS.contains(&k.as_str()) {
                        match x {
                            Value::String(p) => *p = self.app_path(roots, p)?,
                            Value::Array(ps) => {
                                for p in ps.iter_mut() {
                                    match p {
                                        Value::String(s) => *s = self.app_path(roots, s)?,
                                        other => self.check_paths_in(roots, other)?,
                                    }
                                }
                            }
                            other => self.check_paths_in(roots, other)?,
                        }
                    } else {
                        self.check_paths_in(roots, x)?;
                    }
                }
                Ok(())
            }
            Value::Array(xs) => xs
                .iter_mut()
                .try_for_each(|x| self.check_paths_in(roots, x)),
            _ => Ok(()),
        }
    }

    fn app_path(&self, roots: &Roots, path: &str) -> Result<String, CmdError> {
        // a project of the library named by its name or id stands for its home
        let library_home;
        let path = if Path::new(path).is_absolute() {
            path
        } else {
            library_home = self.library_home(path)?;
            library_home
                .to_str()
                .ok_or_else(|| CmdError::invalid("/args", "not UTF-8"))?
        };
        let canon = roots
            .check_file(path)
            .or_else(|_| roots.open_dir(path).map(|(p, _)| p))
            .or_else(|e| {
                // a new file: its folder must be inside the roots
                let p = Path::new(path);
                match (p.parent(), p.file_name(), p.exists()) {
                    (Some(d), Some(n), false) => {
                        let d = d
                            .to_str()
                            .ok_or_else(|| CmdError::invalid("/args", "not UTF-8"))?;
                        roots.open_dir(d).map(|(d, _)| d.join(n))
                    }
                    _ => Err(e),
                }
            })?;
        canon
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| CmdError::invalid("/args", "not UTF-8"))
    }

    /// `source_import{path}`: only a file inside the agent read roots (R-FS-1), handed to
    /// the app's importer by its canonical path.
    pub fn import_source(&self, project: &str, path: &str, conn: &Conn) -> Result<Value, CmdError> {
        let file = self.roots_for(conn).check_file(path)?;
        // the agent's import, in its run; a full-access run's (decision 0043) is not held
        // to its cut: the project was checked already
        let mut caller = conn.caller();
        caller.cut_binding = None;
        let caller = &caller;
        let imp = self
            .importer
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| CmdError::Unsupported {
                platform: "headless".into(),
                reason: "adding videos to a project happens in the gmclip app".into(),
            })?;
        imp.import(project, &file, caller)
    }

    /// The library index, if this holder has one.
    pub fn library(&self) -> Result<Arc<dyn LibraryHost>, CmdError> {
        self.library
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| CmdError::Unsupported {
                platform: "headless".into(),
                reason: "the library index lives in the gmclip app".into(),
            })
    }

    /// Limits launched run `run` to these registry commands (reads and tools that are
    /// not commands are unaffected). Set by the in-process launcher after registering.
    pub fn restrict_commands(&self, run: &str, commands: &[&str]) {
        if let Ok(mut g) = self.run_commands.lock() {
            g.insert(
                run.to_owned(),
                commands.iter().map(|c| (*c).to_owned()).collect(),
            );
        }
    }

    /// Whether launched run `run` may run `command`.
    pub fn run_may(&self, run: &str, command: &str) -> bool {
        self.run_commands
            .lock()
            .map(|g| g.get(run).is_none_or(|set| set.contains(command)))
            .unwrap_or(false)
    }

    /// Records an approval an agent's call needs (R-PROTO-6), asked in run `run`
    /// ([`Conn::run_key`]). The same ask again while it waits (the same tool, cut
    /// revision and findings) is that one, not another; a run with
    /// [`MAX_PENDING_APPROVALS`] waiting is paused for review instead. Only that run:
    /// another of the same client is not (decision 0050).
    pub fn approval_add(
        &self,
        identity: &str,
        run: &str,
        tool: &str,
        effects: Value,
        prose: Option<String>,
    ) -> Asked {
        let same = {
            let key = match (effects.get("cut_rev"), effects.get("blocked_by")) {
                (Some(r), Some(b)) => json!([effects.get("cut"), r, b]),
                _ => effects.clone(),
            };
            let mut h = Sha256::new();
            h.update(tool.as_bytes());
            h.update(b"\x00");
            h.update(serde_json::to_string(&key).unwrap_or_default().as_bytes());
            wire::hex(&h.finalize())
        };
        let asked = {
            let Ok(mut g) = self.approvals.lock() else {
                return Asked::Paused;
            };
            // the same ask from the same agent, even from a new connection (an agent that
            // starts gmclip per call with no session), is the one already waiting
            if let Some(a) = g
                .iter()
                .find(|a| (a.run == run || a.identity == identity) && a.same == same)
            {
                return Asked::Again(a.id.clone());
            }
            let pending = g.iter().filter(|a| a.run == run).count();
            if pending >= MAX_PENDING_APPROVALS {
                for a in g.iter_mut().filter(|a| a.run == run) {
                    a.run_paused = true;
                }
                Asked::Paused
            } else {
                if g.len() >= MAX_APPROVALS {
                    g.remove(0);
                }
                let n = self.conn_seq.fetch_add(1, Ordering::SeqCst);
                let id = format!("ap{n}");
                g.push(Approval {
                    id: id.clone(),
                    identity: identity.to_owned(),
                    run: run.to_owned(),
                    tool: tool.to_owned(),
                    effects,
                    prose: prose.map(|p| p.chars().take(500).collect()),
                    ts_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as i64),
                    run_paused: false,
                    same,
                });
                Asked::New(id)
            }
        };
        self.approvals_changed();
        asked
    }

    /// How many asks wait for a human in run `run` when it is paused for review (its
    /// 4th ask arrived); `None` while it may go on.
    pub fn run_paused_for_review(&self, run: &str) -> Option<u32> {
        let g = self.approvals.lock().ok()?;
        let mine: Vec<&Approval> = g.iter().filter(|a| a.run == run).collect();
        mine.iter()
            .any(|a| a.run_paused)
            .then(|| u32::try_from(mine.len()).unwrap_or(u32::MAX))
    }

    /// The pending approvals, oldest first.
    pub fn approvals(&self) -> Vec<Approval> {
        self.approvals.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// One pending approval.
    pub fn approval(&self, id: &str) -> Option<Approval> {
        self.approvals
            .lock()
            .ok()
            .and_then(|g| g.iter().find(|a| a.id == id).cloned())
    }

    /// A human handled one (`id`), or every one of a run (`run`: its run key, or the
    /// identity that asked): they leave the list and, when the run has none left, it may
    /// ask again.
    pub fn approval_resolve(&self, id: Option<&str>, run: Option<&str>) -> usize {
        let n = {
            let Ok(mut g) = self.approvals.lock() else {
                return 0;
            };
            let n = g.len();
            g.retain(|a| {
                !(id.is_some_and(|i| i == a.id)
                    || run.is_some_and(|r| r == a.run || r == a.identity))
            });
            n - g.len()
        };
        if n > 0 {
            self.approvals_changed();
        }
        n
    }

    /// Calls `sink` whenever the pending approvals change (the app's Home and badge).
    pub fn set_approval_sink(&self, sink: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut g) = self.approval_sink.lock() {
            *g = Some(sink);
        }
    }

    fn approvals_changed(&self) {
        if let Ok(g) = self.approval_sink.lock()
            && let Some(f) = g.as_ref()
        {
            f();
        }
    }

    /// Pauses (or resumes) every agent: their tool calls are refused meanwhile, and
    /// launched runs are ended.
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, std::sync::atomic::Ordering::SeqCst);
        if on {
            let runs: Vec<String> = self
                .launched
                .lock()
                .map(|g| g.keys().cloned().collect())
                .unwrap_or_default();
            for r in runs {
                self.launch_end(&r);
            }
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Launched runs alive now: `(run, project, cut)`.
    pub fn launched_runs(&self) -> Vec<(String, String, String)> {
        self.launched
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|(_, l)| Instant::now() <= l.expires)
                    .map(|(k, l)| (k.clone(), l.project.clone(), l.cut.0.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Serves a project the embedding process already holds (the app): agents reach it
    /// through this holder; the embedder keeps closing it.
    pub fn adopt(&self, p: OpenProject) {
        if let Ok(mut a) = self.adopted.lock() {
            a.insert(p.info.id.clone());
        }
        if let Ok(mut g) = self.projects.lock() {
            g.insert(p.info.id.clone(), p);
        }
    }

    /// Stops serving an adopted project (the embedder is closing it).
    pub fn release(&self, id: &str) {
        if let Ok(mut a) = self.adopted.lock() {
            a.remove(id);
        }
        if let Ok(mut g) = self.projects.lock() {
            g.remove(id);
        }
    }

    /// The same holder, analysing with `analysis`.
    pub fn with_analysis(mut self, analysis: Arc<dyn gmclip_core::AnalysisHost>) -> Holder {
        self.analysis = analysis;
        self
    }

    /// Marks background analysis as started for `key`; false the first time.
    pub fn analysis_tried(&self, key: &str) -> bool {
        self.analysis_started
            .lock()
            .map_or(true, |mut g| !g.insert(key.to_owned()))
    }

    /// A holder that plans with `planner` and renders with `media`.
    pub fn with_hosts(
        env: Env,
        planner: Arc<dyn gmclip_core::Planner>,
        media: Arc<dyn gmclip_core::MediaRender>,
    ) -> Holder {
        let mut h = Holder::new(env);
        h.planner = planner;
        h.media = media;
        h
    }

    /// Where exports are written: the Owner's `export_dir`, else `~/Movies/gmclip/Exports`
    /// beside the standard app-data folder (always absolute; a write root, R-FS-1).
    pub fn export_dir(&self) -> Result<PathBuf, CmdError> {
        if let Some(d) = self.settings().export_dir.filter(|d| d.is_absolute()) {
            return Ok(d);
        }
        let home = self.user_home().ok_or(CmdError::Unsupported {
            platform: "holder".into(),
            reason: "no export folder: set one with the Owner's export_dir".into(),
        })?;
        Ok(home.join("Movies/gmclip/Exports"))
    }

    /// Sets the human's home folder (the app's `HOME` / `USERPROFILE`).
    pub fn set_human_home(&self, home: PathBuf) {
        if home.is_absolute()
            && let Ok(mut g) = self.human_home.lock()
        {
            *g = Some(home);
        }
    }

    /// The user's home: the one the app set, else from the standard app-data folder
    /// (none for any other).
    pub fn user_home(&self) -> Option<PathBuf> {
        if let Some(h) = self.human_home.lock().ok().and_then(|g| g.clone()) {
            return Some(h);
        }
        let suffix = if cfg!(target_os = "macos") {
            "Library/Application Support/gmclip"
        } else if cfg!(windows) {
            "AppData\\Local\\gmclip"
        } else {
            ".local/share/gmclip"
        };
        self.env
            .app_data
            .to_str()
            .and_then(|a| a.strip_suffix(suffix))
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
    }

    /// Sends Owner codes to `sink` instead of the requester's terminal (in-process tests;
    /// nothing reachable over the socket can set this).
    pub fn set_code_sink(&self, sink: CodeSink) {
        if let Ok(mut g) = self.code_sink.lock() {
            *g = Some(sink);
        }
    }

    /// Sends each first-seen notice to `sink` as well (the app's activity rail).
    pub fn set_notice_sink(&self, sink: NoticeSink) {
        if let Ok(mut g) = self.notice_sink.lock() {
            *g = Some(sink);
        }
    }

    /// The read-root handles for a connection (opened once per distinct root list): the
    /// agent read roots, plus its session grant's folders while the grant lives.
    pub fn roots_for(&self, conn: &Conn) -> Arc<Roots> {
        let mut want = self.read_roots();
        want.extend(self.grant_roots(conn));
        self.roots_of(want)
    }

    fn roots_of(&self, want: Vec<PathBuf>) -> Arc<Roots> {
        let mut g = match self.roots.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some((have, r)) = g.as_ref()
            && *have == want
        {
            return Arc::clone(r);
        }
        let r = Arc::new(Roots::open(&want));
        *g = Some((want, Arc::clone(&r)));
        r
    }

    pub fn settings(&self) -> OwnerSettings {
        OwnerSettings::load(&self.env.app_data)
    }

    /// A fresh connection id.
    pub fn next_conn_id(&self) -> String {
        let n = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        format!("c{}-{n}", std::process::id())
    }

    pub fn touch(&self) {
        if let Ok(mut g) = self.last_activity.lock() {
            *g = Instant::now();
        }
    }

    // ---------------------------------------------------------------------------------
    // Projects
    // ---------------------------------------------------------------------------------

    fn entry(p: &OpenProject) -> ProjectEntry {
        ProjectEntry {
            id: p.info.id.clone(),
            name: p.info.name.clone(),
            rev: p.handle.snapshot().doc.rev(),
        }
    }

    pub fn project_list(&self) -> Vec<ProjectEntry> {
        self.projects
            .lock()
            .map(|g| g.values().map(Self::entry).collect())
            .unwrap_or_default()
    }

    /// The read roots the Owner's settings give: the human's home folder unless the
    /// Owner removed it (decision 0058), the Owner's roots and the recordings folder.
    pub fn owner_read_roots(&self) -> Vec<PathBuf> {
        let s = self.settings();
        let mut v = Vec::new();
        if s.home_readable()
            && let Some(h) = self.user_home()
        {
            v.push(h);
        }
        v.extend(s.read_roots());
        v
    }

    /// The agent read roots now: [`Holder::owner_read_roots`], plus the homes of open
    /// projects and of gmclip's library.
    pub fn read_roots(&self) -> Vec<PathBuf> {
        let mut v = self.owner_read_roots();
        if let Ok(g) = self.projects.lock() {
            v.extend(g.values().map(|p| p.home.clone()));
        }
        v.extend(self.library_homes());
        v
    }

    /// Sets gmclip's library folder (the app's `~/Movies/gmclip/Library`).
    pub fn set_library_dir(&self, dir: PathBuf) {
        if let Ok(mut g) = self.library_dir.lock() {
            *g = Some(dir);
        }
    }

    /// gmclip's library folder: the app's, else `~/Movies/gmclip/Library` beside the
    /// standard app-data folder (as [`Holder::export_dir`]).
    pub fn library_dir(&self) -> Option<PathBuf> {
        if let Some(d) = self.library_dir.lock().ok().and_then(|g| g.clone()) {
            return Some(d);
        }
        self.user_home()
            .map(|h| h.join("Movies").join("gmclip").join("Library"))
    }

    /// The project homes in the library folder: real `*.gmclip` directories directly in
    /// it (a symlink there is not followed, so it cannot make another folder a root).
    pub fn library_homes(&self) -> Vec<PathBuf> {
        let Some(dir) = self.library_dir() else {
            return vec![];
        };
        let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "gmclip"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// The projects in the library and their homes: `(id, name, home)`.
    pub fn library_projects(&self) -> Vec<(String, String, PathBuf)> {
        self.library_homes()
            .into_iter()
            .filter_map(|home| {
                let dir = Dir::open_ambient_dir(&home, ambient_authority()).ok()?;
                let info = project::read_home(&dir).ok()?;
                Some((info.id, info.name, home))
            })
            .collect()
    }

    /// The home of the library project (or open project) named `q`: its id, its name, or
    /// its folder's name (with or without `.gmclip`), case aside.
    pub fn library_home(&self, q: &str) -> Result<PathBuf, CmdError> {
        let q = q.trim();
        if q.is_empty() {
            return Err(CmdError::invalid("/path", "empty"));
        }
        let stem = q.strip_suffix(".gmclip").unwrap_or(q);
        let mut all = self.library_projects();
        if let Ok(g) = self.projects.lock() {
            for p in g.values() {
                if !all.iter().any(|(id, _, _)| *id == p.info.id) {
                    all.push((p.info.id.clone(), p.info.name.clone(), p.home.clone()));
                }
            }
        }
        if let Some((_, _, h)) = all.iter().find(|(id, _, _)| id == q) {
            return Ok(h.clone());
        }
        let hits: Vec<&PathBuf> = all
            .iter()
            .filter(|(_, name, home)| {
                name.eq_ignore_ascii_case(stem)
                    || home
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.eq_ignore_ascii_case(stem))
            })
            .map(|(_, _, h)| h)
            .collect();
        match hits.as_slice() {
            [h] => Ok((*h).clone()),
            [] => Err(CmdError::invalid(
                "/path",
                "not an absolute path, nor the name or id of a project in gmclip's library (project_list lists them under `library`)",
            )),
            _ => Err(CmdError::invalid(
                "/path",
                "several projects in the library have that name: give its id",
            )),
        }
    }

    /// What a refused path (`file_roots`) is told (decision 0039): the folders this
    /// connection's gmclip tools may read, and how the human adds one. The folders are
    /// the Owner's settings and the grant's, never the refused path.
    pub fn file_roots_help(&self, conn: &Conn) -> Value {
        let mut readable: Vec<String> = self
            .owner_read_roots()
            .iter()
            .chain(self.grant_roots(conn).iter())
            .map(|p| p.display().to_string())
            .collect();
        if let Ok(g) = self.projects.lock() {
            readable.extend(g.values().map(|p| p.home.display().to_string()));
        }
        if let Some(d) = self.library_dir() {
            readable.push(format!("{}/<project>.gmclip (gmclip's library: open one with project_open {{path: its name or id}})", d.display()));
        }
        readable.dedup();
        let how = if conn.launched.is_some() {
            "This run is bound to one cut and adds no files. Ask the human to add the video in gmclip."
        } else {
            "Tell the human the folder: they can add it in gmclip under Settings → Agent access… → Folders agents may read (adding their home folder there lets agents read anywhere in it again), or start a new session with Full access (its gmclip tools may then open files in their home folder). Do not retry other paths outside these folders."
        };
        json!({ "readable": readable, "how": how })
    }

    /// `project_open{path}`. Agents: only inside the read roots (R-ARCH-6, R-FS-1).
    /// `any` is the Owner command `project_open_any`.
    pub fn open_project(&self, path: &str, any: bool) -> Result<ProjectEntry, CmdError> {
        let roots = (!any).then(|| self.roots_of(self.read_roots()));
        self.open_project_in(path, roots.as_deref())
    }

    /// `project_open{path}` for an agent's connection: an absolute home inside its read
    /// roots (its session grant's folders too), or the name or id of a project in
    /// gmclip's library (decision 0039).
    pub fn open_project_as(&self, conn: &Conn, path: &str) -> Result<ProjectEntry, CmdError> {
        let home;
        let path = if Path::new(path).is_absolute() {
            path
        } else {
            home = self.library_home(path)?;
            home.to_str()
                .ok_or_else(|| CmdError::invalid("/path", "not UTF-8"))?
        };
        self.open_project_in(path, Some(&self.roots_for(conn)))
    }

    fn open_project_in(&self, path: &str, roots: Option<&Roots>) -> Result<ProjectEntry, CmdError> {
        let (home, dir) = if let Some(roots) = roots {
            roots.open_dir(path)?
        } else {
            fsroots::validate(path, fsroots::host_grammar())?;
            let d =
                Dir::open_ambient_dir(path, ambient_authority()).map_err(|_| CmdError::NotFound)?;
            (
                std::fs::canonicalize(path).map_err(|_| CmdError::NotFound)?,
                d,
            )
        };
        let info = project::read_home(&dir)?;
        // the id must belong to this home (a planted owner.json cannot borrow another
        // project's database)
        project::check_home(&self.env.app_data, &info.id, &home)?;
        if let Some(p) = self
            .projects
            .lock()
            .ok()
            .and_then(|g| g.get(&info.id).cloned())
        {
            return Ok(Self::entry(&p));
        }
        let db = project::db_path(&self.env.app_data, &info.id)?;
        let handle = ProjectHandle::open(
            &db,
            OpenOptions {
                store: StoreOptions {
                    kind: HolderKind::Headless,
                    ..StoreOptions::default()
                },
                ..OpenOptions::default()
            },
        )
        .map_err(|e| store_err(&e))?;
        let p = OpenProject {
            handle,
            info: info.clone(),
            home,
        };
        let e = Self::entry(&p);
        // sources with no analysis yet get it in the background (SPEC §9)
        crate::analysis::ensure(self, &p);
        if let Ok(mut g) = self.projects.lock() {
            g.insert(info.id, p);
        }
        Ok(e)
    }

    /// Resolves `project` (id or home path; never a name), or the only open project.
    pub fn resolve(&self, project: Option<&str>) -> Result<OpenProject, CmdError> {
        let g = self.projects.lock().map_err(|_| CmdError::Internal {
            trace_id: "lock".into(),
        })?;
        match project {
            None => match g.len() {
                1 => Ok(g.values().next().cloned().ok_or(CmdError::NotFound)?),
                0 => Err(CmdError::invalid(
                    "/project",
                    "no project is open: project_open{path} first",
                )),
                _ => Err(CmdError::invalid(
                    "/project",
                    "several projects are open: name one (id or path)",
                )),
            },
            Some(p) => g
                .values()
                .find(|o| o.info.id == p || o.home.to_str() == Some(p))
                .cloned()
                .ok_or(CmdError::NotFound),
        }
    }

    /// Closes every project cleanly (shutdown).
    pub fn close_all(&self) {
        let all: Vec<OpenProject> = self
            .projects
            .lock()
            .map(|mut g| std::mem::take(&mut *g).into_values().collect())
            .unwrap_or_default();
        let adopted = self.adopted.lock().map(|g| g.clone()).unwrap_or_default();
        for p in all {
            if !adopted.contains(&p.info.id) {
                let _ = p.handle.close();
            }
        }
    }

    /// Ends a disconnected connection's implicit runs (§11.2).
    pub fn end_conn(&self, conn: &Conn) {
        // a launched run outlives its shim connections, and a session's run its calls
        // (it ends idle, §3.6, or with `run_end`)
        if conn.launched.is_some() || conn.session.is_some() {
            return;
        }
        let touched: Vec<String> = conn
            .touched
            .lock()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default();
        for id in touched {
            if let Ok(p) = self.resolve(Some(&id)) {
                let as_of = p.handle.snapshot().doc.rev();
                let _ = p.handle.call(CallRequest {
                    command: "run_end".into(),
                    args: json!({}),
                    base: Some(Base::AsOf { as_of }),
                    request_id: None,
                    caller: conn.caller(),
                });
            }
        }
    }

    // ---------------------------------------------------------------------------------
    // Launched runs (R-AGT-5)
    // ---------------------------------------------------------------------------------

    /// Registers a launched run (called by the launcher over the verified socket). The
    /// credential stays in the holder's memory; shims prove it by HMAC. Grants beyond the
    /// default scopes (`read_media`, private regions) need an Owner code.
    pub fn launch_register(&self, req: &LaunchRegister, conn: &Conn) -> Result<Value, CmdError> {
        // only the launcher's verified executable registers runs (R-AGT-1/3): gmclip
        // itself, or a harness the Owner named in `trusted_launchers` (absolute paths).
        // The exe is read again now and must be the one seen at accept, so a process that
        // connected and then exec'd something else is refused. Limit: a same-user process
        // that can edit `owner-settings.json` or exec gmclip on an inherited socket is
        // already inside the user's trust boundary; grants still need an Owner code.
        let now_exe = conn.pid.and_then(crate::paths::exe_of);
        let trusted = conn.exe.as_deref().is_some_and(|e| {
            now_exe
                .as_deref()
                .is_some_and(|n| crate::paths::same_exe(e, n))
                && (crate::paths::our_exe().is_some_and(|me| crate::paths::same_exe(e, &me))
                    || self
                        .settings()
                        .trusted_launchers
                        .iter()
                        .filter(|t| t.is_absolute())
                        .any(|t| crate::paths::same_exe(e, t)))
        });
        if !trusted {
            settings::audit(
                &self.env.app_data,
                "launch_register",
                "",
                "",
                conn.pid,
                conn.exe.as_deref(),
                "refused: not the launcher",
            );
            return Err(CmdError::Unauthorized {
                scope: "launcher".into(),
            });
        }
        let cred = wire::unhex(&req.credential)
            .filter(|c| c.len() >= 32)
            .ok_or_else(|| CmdError::invalid("/credential", "256-bit hex"))?;
        let p = self.resolve(Some(&req.project))?;
        let cut = Ulid::parse(&req.cut)
            .map(Id::<Cut>::new)
            .map_err(|_| CmdError::invalid("/cut", "not a cut id"))?;
        if p.handle.snapshot().doc.get(cut).is_none() {
            return Err(CmdError::NotFound);
        }
        // a full-access session (decision 0043) is registered by gmclip itself, in
        // process, for a human who chose "Full access": the agent already has their home
        // folder and a shell, so seeing the cut's media needs no Owner code; private
        // regions still do
        let ours = conn.exe.as_deref().is_some_and(|e| {
            crate::paths::our_exe().is_some_and(|me| crate::paths::same_exe(e, &me))
        });
        if req.full_access && !ours {
            return Err(CmdError::Unauthorized {
                scope: "launcher".into(),
            });
        }
        let full_access = req.full_access && ours;
        let mut allowed = DEFAULT_SCOPES.to_vec();
        if req.media || full_access {
            allowed.push(Scope::ReadMedia);
        }
        // the launcher's scopes, if it names them, may narrow but never widen
        let scopes = match &req.scopes {
            None => allowed,
            Some(names) => {
                let mut v = Vec::new();
                for n in names {
                    let s = allowed
                        .iter()
                        .copied()
                        .find(|s| s.name() == n)
                        .ok_or_else(|| {
                            CmdError::invalid(
                                "/scopes",
                                "only read, edit, render, ai, and read_media with media",
                            )
                        })?;
                    if !v.contains(&s) {
                        v.push(s);
                    }
                }
                v
            }
        };
        // the credential is bound to the human's brief (R-AGT-5): required
        if !req
            .brief_sha256
            .as_ref()
            .is_some_and(|b| b.len() == 64 && b.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(CmdError::invalid("/brief_sha256", "64 hex characters"));
        }
        // the Owner code is spent last, after everything else about the request checked
        // out, so a typo elsewhere does not burn it and the audit's "ok" means a run
        if (req.media && !full_access) || req.private_regions {
            let args = launch_grant_args(&req.project, &req.cut, req.media, req.private_regions);
            let checked = self.owner_check("launch_grant", &args, req.code.as_deref());
            settings::audit(
                &self.env.app_data,
                "owner_call",
                "launch_grant",
                &args_hash("launch_grant", &args),
                conn.pid,
                conn.exe.as_deref(),
                if checked.is_ok() { "ok" } else { "refused" },
            );
            checked?;
        }
        let run = Ulid::from_parts(
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(0),
            u128::from_le_bytes(
                wire::unhex(&wire::nonce())
                    .and_then(|b| b[..16].try_into().ok())
                    .unwrap_or([0; 16]),
            ) & ((1 << 80) - 1),
        )
        .to_string();
        let wall = Duration::from_secs(req.wall_clock_s.clamp(60, 24 * 3600));
        let mut g = self.launched.lock().map_err(|_| CmdError::Internal {
            trace_id: "lock".into(),
        })?;
        g.retain(|_, l| l.expires > Instant::now());
        if g.len() >= MAX_LAUNCHED {
            return Err(CmdError::Busy {
                job: "launched runs".into(),
            });
        }
        g.insert(
            run.clone(),
            Launched {
                credential: cred,
                project: p.info.id.clone(),
                cut,
                scopes: scopes.clone(),
                private_regions: req.private_regions,
                brief_sha256: req.brief_sha256.clone(),
                full_access,
                grant: req.grant.clone().filter(|_| full_access),
                expires: Instant::now() + wall,
                used_client_nonces: HashSet::new(),
            },
        );
        drop(g);
        let scope_names: Vec<&str> = scopes.iter().map(|s| s.name()).collect();
        settings::audit(
            &self.env.app_data,
            "launch_register",
            &format!(
                "launched:{run} scopes={} private_regions={}",
                scope_names.join(","),
                req.private_regions
            ),
            req.brief_sha256.as_deref().unwrap_or("-"),
            conn.pid,
            conn.exe.as_deref(),
            "ok",
        );
        Ok(json!({ "run": run, "identity": format!("launched:{run}"), "scopes": scope_names }))
    }

    /// Whether a launched run is still registered and inside its wall clock. Every call a
    /// launched connection makes checks this, so ending or expiring a run cuts off its
    /// live connections too (R-AGT-5), not only new handshakes.
    pub fn launch_alive(&self, run: &str) -> bool {
        let Ok(mut g) = self.launched.lock() else {
            return false;
        };
        match g.get(run) {
            Some(l) if Instant::now() <= l.expires => true,
            Some(_) => {
                g.remove(run);
                false
            }
            None => false,
        }
    }

    /// Ends a launched run (its credential dies with it).
    pub fn launch_end(&self, run: &str) {
        if let Ok(mut g) = self.run_commands.lock() {
            g.remove(run);
        }
        if let Ok(mut g) = self.launched.lock() {
            g.remove(run);
        }
    }

    /// Checks a shim's proof; `Ok(None)` = no proof offered (a local client).
    pub fn prove(&self, hello: &wire::ClientHello, server_nonce: &str) -> Result<Proven, CmdError> {
        let (Some(run), Some(proof)) = (&hello.run, &hello.proof) else {
            return Ok(Proven::default());
        };
        let unauthorized = || CmdError::Unauthorized {
            scope: "launched".into(),
        };
        let is_grant = self.grants.lock().is_ok_and(|g| g.contains_key(run));
        if is_grant {
            let mut g = self.grants.lock().map_err(|_| unauthorized())?;
            let gr = g.get_mut(run).ok_or_else(unauthorized)?;
            if Instant::now() > gr.expires {
                g.remove(run);
                return Err(unauthorized());
            }
            if !wire::verify(&gr.credential, server_nonce, &hello.client_nonce, proof)
                || !gr.used_client_nonces.insert(hello.client_nonce.clone())
            {
                return Err(unauthorized());
            }
            return Ok(Proven {
                launched: None,
                granted: Some(run.clone()),
            });
        }
        let mut g = self.launched.lock().map_err(|_| unauthorized())?;
        let l = g.get_mut(run).ok_or_else(unauthorized)?;
        if Instant::now() > l.expires {
            g.remove(run);
            return Err(unauthorized());
        }
        if !wire::verify(&l.credential, server_nonce, &hello.client_nonce, proof) {
            return Err(unauthorized());
        }
        if !l.used_client_nonces.insert(hello.client_nonce.clone()) {
            return Err(unauthorized()); // a replayed handshake
        }
        Ok(Proven {
            launched: Some(LaunchedRef {
                run: run.clone(),
                project: l.project.clone(),
                cut: l.cut,
                scopes: l.scopes.clone(),
                private_regions: l.private_regions,
                full_access: l.full_access,
            }),
            // a full-access run reaches its session grant's folders too
            granted: l.grant.clone(),
        })
    }

    /// Registers a session grant (decision 0039): only the app calls this, in-process,
    /// for a session the human started with full access; nothing on the socket reaches
    /// it. `roots` are absolute folders the session's gmclip tools may read besides the
    /// agent read roots. Returns the grant's id; its shim proves `credential` (hex,
    /// ≥ 256 bits) as `GMCLIP_RUN=<id>:<hex>`.
    pub fn grant_register(
        &self,
        credential: &str,
        roots: Vec<PathBuf>,
        wall: Duration,
    ) -> Result<String, CmdError> {
        let cred = wire::unhex(credential)
            .filter(|c| c.len() >= 32)
            .ok_or_else(|| CmdError::invalid("/credential", "256-bit hex"))?;
        if roots.iter().any(|r| !r.is_absolute()) {
            return Err(CmdError::invalid("/roots", "absolute folders only"));
        }
        let id = format!("grant-{}", &wire::nonce()[..24]);
        let mut g = self.grants.lock().map_err(|_| CmdError::Internal {
            trace_id: "lock".into(),
        })?;
        g.retain(|_, x| x.expires > Instant::now());
        if g.len() >= MAX_LAUNCHED {
            return Err(CmdError::Busy {
                job: "session grants".into(),
            });
        }
        g.insert(
            id.clone(),
            Grant {
                credential: cred,
                roots,
                expires: Instant::now() + wall,
                used_client_nonces: HashSet::new(),
            },
        );
        Ok(id)
    }

    /// Ends a session grant: its connections' gmclip tools are back to the agent read
    /// roots on their next call.
    pub fn grant_end(&self, id: &str) {
        if let Ok(mut g) = self.grants.lock() {
            g.remove(id);
        }
    }

    /// The folders a connection's live session grant adds (none without one).
    fn grant_roots(&self, conn: &Conn) -> Vec<PathBuf> {
        let Some(id) = &conn.granted else {
            return vec![];
        };
        self.grants
            .lock()
            .ok()
            .and_then(|g| {
                g.get(id)
                    .filter(|x| Instant::now() <= x.expires)
                    .map(|x| x.roots.clone())
            })
            .unwrap_or_default()
    }

    // ---------------------------------------------------------------------------------
    // Owner (R-OWN-3 [M1])
    // ---------------------------------------------------------------------------------

    /// Starts an Owner command: a one-time code bound to it and its arguments (60 s,
    /// single use), written to the **requesting process's own controlling terminal** —
    /// never returned over the socket — so the human reads it there and types it back.
    /// A process with no terminal gets no code. (Honest limit, R-OWN-3 [M1]: a same-user
    /// process driving a pty can still read it.)
    pub fn owner_begin(&self, command: &str, args: &Value, conn: &Conn) -> Result<Value, CmdError> {
        if conn.launched.is_some() {
            return Err(owner_denied());
        }
        if !OWNER_COMMANDS.contains(&command) {
            return Err(CmdError::NotFound);
        }
        self.owner_throttle()?;
        let n = wire::nonce();
        // 8 characters from an unambiguous alphabet
        const A: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
        let code: String = n
            .as_bytes()
            .chunks(2)
            .take(8)
            .map(|c| A[usize::from(c[0] ^ c[1]) % A.len()] as char)
            .collect();
        // deliver first: no terminal, no challenge
        let sink = self
            .code_sink
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.0)(command, &code)));
        let shown_on = match sink {
            Some(()) => "test sink".to_owned(),
            None => {
                let tty = conn
                    .pid
                    .and_then(tty_of)
                    .ok_or_else(|| CmdError::Unauthorized {
                        scope: "owner (needs an interactive terminal)".into(),
                    })?;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&tty)
                    .map_err(|_| owner_denied())?;
                use std::io::Write;
                let _ = write!(f, "\r\ngmclip owner code for {command}: {code}\r\n");
                tty.display().to_string()
            }
        };
        let mut g = self.owner.lock().map_err(|_| owner_denied())?;
        g.retain(|_, c| c.expires > Instant::now());
        if g.len() >= MAX_PENDING {
            return Err(CmdError::Busy {
                job: "owner codes".into(),
            });
        }
        g.insert(
            code,
            OwnerChallenge {
                command: command.into(),
                args_hash: args_hash(command, args),
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
        Ok(json!({ "shown_on": shown_on }))
    }

    /// After 5 wrong codes in a minute, Owner calls are refused until it passes.
    fn owner_throttle(&self) -> Result<(), CmdError> {
        let mut g = self.owner_failures.lock().map_err(|_| owner_denied())?;
        if g.1.elapsed() > Duration::from_secs(60) {
            *g = (0, Instant::now());
        }
        if g.0 >= 5 {
            return Err(CmdError::Busy {
                job: "owner: too many wrong codes, wait a minute".into(),
            });
        }
        Ok(())
    }

    fn owner_failed(&self) {
        if let Ok(mut g) = self.owner_failures.lock() {
            if g.1.elapsed() > Duration::from_secs(60) {
                *g = (0, Instant::now());
            }
            g.0 += 1;
        }
    }

    fn owner_check(&self, command: &str, args: &Value, code: Option<&str>) -> Result<(), CmdError> {
        self.owner_throttle()?;
        let r = self.owner_check_inner(command, args, code);
        if r.is_err() {
            self.owner_failed();
        }
        r
    }

    fn owner_check_inner(
        &self,
        command: &str,
        args: &Value,
        code: Option<&str>,
    ) -> Result<(), CmdError> {
        let code = code.ok_or_else(owner_denied)?;
        let ch = self
            .owner
            .lock()
            .map_err(|_| owner_denied())?
            .remove(code)
            .ok_or_else(owner_denied)?;
        if ch.expires < Instant::now()
            || ch.command != command
            || ch.args_hash != args_hash(command, args)
        {
            return Err(owner_denied());
        }
        Ok(())
    }

    /// Runs an Owner command after its code checks out; every attempt is audited.
    pub fn owner_call(
        &self,
        command: &str,
        args: &Value,
        code: &str,
        conn: &Conn,
    ) -> Result<Value, CmdError> {
        let h = args_hash(command, args);
        let checked = if conn.launched.is_some() {
            Err(owner_denied())
        } else {
            self.owner_check(command, args, Some(code))
        };
        let result = checked.and_then(|()| self.owner_apply(command, args));
        settings::audit(
            &self.env.app_data,
            "owner_call",
            command,
            &h,
            conn.pid,
            conn.exe.as_deref(),
            if result.is_ok() { "ok" } else { "refused" },
        );
        result
    }

    fn owner_apply(&self, command: &str, args: &Value) -> Result<Value, CmdError> {
        let path_arg = || -> Result<PathBuf, CmdError> {
            let p = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| CmdError::invalid("/path", "required"))?;
            fsroots::validate(p, fsroots::host_grammar())?;
            Ok(PathBuf::from(p))
        };
        let mut s = self.settings();
        let save = |s: &OwnerSettings| {
            s.save(&self.env.app_data).map_err(|_| CmdError::Internal {
                trace_id: "save-settings".into(),
            })
        };
        match command {
            "roots_add" => {
                let p = path_arg()?;
                // a root that is not a folder grants nothing and hides a typo (decision 0058)
                if !p.is_dir() {
                    return Err(CmdError::invalid("/path", "no such folder"));
                }
                let write = args.get("write").and_then(Value::as_bool).unwrap_or(false);
                let home = self.user_home();
                // the home folder as a read root is the default's switch (decision 0058)
                if !write && home.as_ref() == Some(&p) {
                    s.home_read = None;
                } else {
                    let list = if write {
                        &mut s.write_roots
                    } else {
                        &mut s.read_roots
                    };
                    if !list.contains(&p) {
                        list.push(p);
                    }
                }
                save(&s)?;
                Ok(
                    json!({"read_roots": s.read_roots, "write_roots": s.write_roots, "home_readable": s.home_readable()}),
                )
            }
            "roots_remove" => {
                let p = path_arg()?;
                if self.user_home().as_ref() == Some(&p) {
                    s.home_read = Some(false);
                }
                s.read_roots.retain(|r| *r != p);
                s.write_roots.retain(|r| *r != p);
                save(&s)?;
                Ok(
                    json!({"read_roots": s.read_roots, "write_roots": s.write_roots, "home_readable": s.home_readable()}),
                )
            }
            "recordings_set" => {
                s.recordings = Some(path_arg()?);
                save(&s)?;
                Ok(json!({"recordings": s.recordings}))
            }
            "revoke" => {
                let id = args
                    .get("identity")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CmdError::invalid("/identity", "required"))?;
                if !id.starts_with("local:") || id.len() > 80 {
                    return Err(CmdError::invalid("/identity", "a local:<client> identity"));
                }
                if !s.revoked.iter().any(|r| r == id) {
                    s.revoked.push(id.to_owned());
                }
                save(&s)?;
                Ok(json!({"revoked": s.revoked}))
            }
            "style_set" => {
                let p = path_arg()?;
                // the hash of the version the Owner was shown (the CLI previews it)
                let seen = args.get("sha256").and_then(Value::as_str);
                let pin = gmclip_core::profile::style::set(&self.env.app_data, &p, now_ms(), seen)
                    .map_err(|e| CmdError::invalid("/path", &e))?;
                Ok(json!({"path": pin.path, "sha256": pin.sha256, "bytes": pin.bytes}))
            }
            "style_accept" => {
                let sha = args
                    .get("sha256")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CmdError::invalid("/sha256", "required"))?;
                let pin = gmclip_core::profile::style::accept(&self.env.app_data, sha, now_ms())
                    .map_err(|e| CmdError::invalid("/sha256", &e))?;
                Ok(json!({"path": pin.path, "sha256": pin.sha256, "bytes": pin.bytes}))
            }
            "review_tailnet" => {
                let get = |k: &str| -> Result<String, CmdError> {
                    let v = args
                        .get(k)
                        .and_then(Value::as_str)
                        .ok_or_else(|| CmdError::invalid(&format!("/{k}"), "required"))?;
                    if v.is_empty()
                        || v.len() > 200
                        || !v
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-._@".contains(c))
                    {
                        return Err(CmdError::invalid(&format!("/{k}"), "a tailnet name"));
                    }
                    Ok(v.to_owned())
                };
                let host = get("host")?;
                if !host.ends_with(".ts.net") {
                    return Err(CmdError::invalid(
                        "/host",
                        "a <machine>.<tailnet>.ts.net name",
                    ));
                }
                s.review_tailnet = Some(settings::ReviewTailnet {
                    host,
                    owner_login: get("owner_login")?,
                });
                save(&s)?;
                Ok(json!({"review_tailnet": s.review_tailnet}))
            }
            "review_link_new" => {
                let login = args
                    .get("login")
                    .and_then(Value::as_str)
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        if l.len() > 200
                            || !l
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-._@+".contains(c))
                        {
                            Err(CmdError::invalid("/login", "a tailnet login"))
                        } else {
                            Ok(l.to_owned())
                        }
                    })
                    .transpose()?;
                let role = match args.get("role").and_then(Value::as_str) {
                    None | Some("") | Some("reviewer") => None,
                    Some(r) if crate::review::links::ROLES.contains(&r) => Some(r.to_owned()),
                    Some(_) => return Err(CmdError::invalid("/role", "reviewer or editor")),
                };
                let (link, token) = crate::review::links::create_with_role(
                    &self.env.app_data,
                    now_ms(),
                    login,
                    role,
                )
                .map_err(|_| CmdError::Internal {
                    trace_id: "review-links".into(),
                })?;
                // the token goes to the Owner's own CLI (it prints it on the terminal)
                Ok(json!({"id": link.id, "token": token, "expires_ms": link.expires_ms}))
            }
            "review_loopback" => {
                s.review_tailnet = None;
                save(&s)?;
                Ok(json!({"review_tailnet": null}))
            }
            "remote_fix_set" => {
                if args.get("off").and_then(Value::as_bool) == Some(true) {
                    s.remote_fix = None;
                } else {
                    let usd = args
                        .get("max_usd_per_run")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 100.0)
                        .ok_or_else(|| CmdError::invalid("/max_usd_per_run", "0 < $ ≤ 100"))?;
                    let runs = args
                        .get("max_runs_per_day")
                        .and_then(Value::as_u64)
                        .filter(|v| (1..=50).contains(v))
                        .ok_or_else(|| CmdError::invalid("/max_runs_per_day", "1..=50"))?;
                    s.remote_fix = Some(settings::RemoteFix {
                        max_usd_per_run: usd,
                        max_runs_per_day: runs as u32,
                    });
                }
                save(&s)?;
                Ok(json!({"remote_fix": s.remote_fix}))
            }
            "remote_send_set" => {
                if args.get("off").and_then(Value::as_bool) == Some(true) {
                    s.remote_send = None;
                } else {
                    let n = args
                        .get("max_sends_per_day")
                        .and_then(Value::as_u64)
                        .filter(|v| (1..=20).contains(v))
                        .ok_or_else(|| CmdError::invalid("/max_sends_per_day", "1..=20"))?;
                    s.remote_send = Some(settings::RemoteSend {
                        max_sends_per_day: n as u32,
                    });
                }
                save(&s)?;
                Ok(json!({"remote_send": s.remote_send}))
            }
            "profile_use" => {
                let p = path_arg()?;
                let prof = gmclip_core::profile::load_in(&self.env.app_data, &p)
                    .map_err(|e| CmdError::invalid("/path", &e.to_string()))?;
                s.profile = Some(p);
                save(&s)?;
                // suggestions are shown, never applied (R-PROF-4)
                Ok(
                    json!({"profile": s.profile, "origin": prof.origin, "suggestions": prof.suggestions}),
                )
            }
            "budgets_set" => {
                // R-COST-2: raising a cap is the Owner's; every cap 0 < $ ≤ 1 000
                let obj = args
                    .as_object()
                    .ok_or_else(|| CmdError::invalid("/", "an object of caps"))?;
                if obj.is_empty() {
                    return Err(CmdError::invalid("/", "name at least one cap"));
                }
                let mut b = s.budgets();
                for (k, v) in obj {
                    let usd = v
                        .as_f64()
                        .filter(|x| x.is_finite() && *x > 0.0 && *x <= settings::BUDGET_MAX_USD)
                        .ok_or_else(|| CmdError::invalid(&format!("/{k}"), "0 < $ ≤ 1 000"))?;
                    if !b.set(k, usd) {
                        return Err(CmdError::invalid(
                            "/",
                            "edit_pass_usd, run_usd, day_usd, batch_usd or ask_usd",
                        ));
                    }
                }
                s.budgets = (b != settings::Budgets::default()).then_some(b);
                save(&s)?;
                Ok(json!({"budgets": s.budgets()}))
            }
            "providers_set" => {
                let ai = match args.get("ai").and_then(Value::as_str) {
                    Some("none") => settings::AiProvider::None,
                    Some("agent_cli") => settings::AiProvider::AgentCli,
                    _ => return Err(CmdError::invalid("/ai", "none or agent_cli")),
                };
                let models: Vec<String> = match args.get("models") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(a)) if a.len() <= 32 => a
                        .iter()
                        .map(|m| {
                            m.as_str()
                                .filter(|m| settings::valid_model_id(m))
                                .map(str::to_owned)
                                .ok_or_else(|| CmdError::invalid("/models", "model ids"))
                        })
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err(CmdError::invalid("/models", "≤ 32 model ids")),
                };
                if args
                    .as_object()
                    .is_some_and(|o| o.keys().any(|k| k != "ai" && k != "models"))
                {
                    // no keys: there is no API-key provider (AJ, 2026-10-15; R-OWN-2 is M3)
                    return Err(CmdError::invalid("/", "ai and models only (no API keys)"));
                }
                let p = settings::Providers { ai, models };
                s.providers = (p != settings::Providers::default()).then_some(p);
                save(&s)?;
                Ok(json!({"providers": s.providers()}))
            }
            "telemetry_set" => {
                s.telemetry = args
                    .get("on")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| CmdError::invalid("/on", "true or false"))?;
                save(&s)?;
                Ok(json!({"telemetry": s.telemetry}))
            }
            "project_open_any" => {
                let p = path_arg()?;
                let e = self.open_project(p.to_str().unwrap_or_default(), true)?;
                Ok(serde_json::to_value(e).unwrap_or(Value::Null))
            }
            "connect" => {
                // the CLI writes the client's config; the holder records the grant
                let client = args
                    .get("client")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !matches!(client, "claude-code" | "codex") {
                    return Err(CmdError::invalid("/client", "claude-code or codex"));
                }
                Ok(json!({"client": client}))
            }
            _ => Err(CmdError::NotFound),
        }
    }

    /// Logs a connection once its identity is known; returns a first-seen notice.
    pub fn log_conn(&self, conn: &Conn, mode: &str) {
        if let Ok(mut logged) = conn.logged.lock() {
            if *logged {
                return;
            }
            *logged = true;
        }
        let rec = settings::ConnRecord {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0)),
            connection: conn.id.clone(),
            identity: conn
                .identity
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_default(),
            uid: crate::paths::our_uid(),
            pid: conn.pid,
            exe: conn.exe.as_ref().map(|p| p.display().to_string()),
            client_exe: conn.client_exe.clone(),
            mode: mode.into(),
        };
        self.record_connection(&rec);
    }

    /// Logs one connection record; the first from an executable path raises the
    /// first-seen notice (stderr, `gmclip status`, and the notice sink) exactly once.
    pub fn record_connection(&self, rec: &settings::ConnRecord) -> Option<settings::Notice> {
        let n = settings::log_connection(&self.env.app_data, rec)?;
        eprintln!("{}", n.notice);
        if let Ok(mut g) = self.notices.lock() {
            if g.len() >= 100 {
                g.remove(0);
            }
            g.push(n.notice.clone());
        }
        if let Ok(g) = self.notice_sink.lock()
            && let Some(sink) = g.as_ref()
        {
            (sink.0)(&n);
        }
        Some(n)
    }

    /// `help{topic}` (a holder command: it reads app data).
    pub fn help(&self, topic: &str) -> Result<Value, CmdError> {
        match topic {
            "style" => {
                let profile = self.settings().profile;
                let mut v =
                    gmclip_core::profile::style_help(&self.env.app_data, profile.as_deref());
                if let Some(st) = v.get_mut("style").filter(|x| !x.is_null()) {
                    let text = st["text"].as_str().unwrap_or_default().to_owned();
                    let source = st["source"].as_str().unwrap_or("style-file").to_owned();
                    st["text"] = crate::canon::field(&text, crate::canon::Kind::Text);
                    st["display"] = Value::String(
                        crate::canon::block(&text, crate::canon::Kind::Text).replacen(
                            "<untrusted>",
                            &format!("<untrusted source=\"{source}\">"),
                            1,
                        ),
                    );
                    if st["pending_change"] == true {
                        eprintln!(
                            "gmclip: the style file changed on disk; agents get the pinned text until `gmclip owner style accept`"
                        );
                    }
                }
                if let Some(fb) = v.get_mut("feedback").and_then(Value::as_array_mut) {
                    // every field of a feedback record is someone's text (R-INJ-1)
                    for r in fb {
                        for k in ["text", "author", "date", "export", "via"] {
                            if let Some(t) = r.get(k).and_then(Value::as_str).map(str::to_owned) {
                                r[k] = crate::canon::field(&t, crate::canon::Kind::Text);
                            }
                        }
                    }
                }
                Ok(json!({"topic": "style", "content": v}))
            }
            _ => Err(CmdError::NotFound),
        }
    }

    /// `gmclip status`.
    pub fn status(&self) -> Value {
        json!({
            "style": gmclip_core::profile::style::status(&self.env.app_data).map(|s| json!({
                "path": s.path,
                "pinned_sha256": s.pinned_sha256,
                "changed_on_disk": s.changed,
                "accept_with": s.changed.then_some("gmclip owner style accept"),
            })),
            "pid": std::process::id(),
            "socket": self.env.socket,
            "projects": self.project_list(),
            "connections_active": self.active.load(Ordering::Relaxed),
            "launched_runs": self.launched.lock().map(|g| g.len()).unwrap_or(0),
            "notices": settings::notices(&self.env.app_data, 20),
        })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Owner commands at M1 (never registered in the command registry, never in
/// `tools/list`, `commands_search` or `command_run`; R-OWN-1).
pub const OWNER_COMMANDS: &[&str] = &[
    "budgets_set",
    "connect",
    "launch_grant",
    "profile_use",
    "project_open_any",
    "providers_set",
    "remote_fix_set",
    "remote_send_set",
    "review_link_new",
    "review_loopback",
    "review_tailnet",
    "recordings_set",
    "revoke",
    "roots_add",
    "roots_remove",
    "style_accept",
    "style_set",
    "telemetry_set",
];

/// The arguments a `launch_grant` Owner code is bound to: the launcher asks for the code
/// and the holder checks it with exactly this value.
pub fn launch_grant_args(project: &str, cut: &str, media: bool, private_regions: bool) -> Value {
    json!({"project": project, "cut": cut, "media": media, "private_regions": private_regions})
}

/// `launch_register` (the launcher's control request). `Debug` never shows the
/// credential or the code.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRegister {
    pub project: String,
    pub cut: String,
    /// The run credential, hex (≥ 256 bits). Sent once, launcher → verified holder.
    pub credential: String,
    #[serde(default = "default_wall")]
    pub wall_clock_s: u64,
    #[serde(default)]
    pub media: bool,
    #[serde(default)]
    pub private_regions: bool,
    /// The Owner code for `media` / `private_regions`.
    #[serde(default)]
    pub code: Option<String>,
    /// sha256 (hex) of the human's brief the run is bound to (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_sha256: Option<String>,
    /// A full-access session (decision 0043; gmclip itself only): `read_media` without an
    /// Owner code, project-wide commands on its own project, and `grant`'s folders.
    #[serde(default)]
    pub full_access: bool,
    /// The session grant (decision 0039) whose folders the run reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    /// The run's scopes by name; may only narrow the defaults (plus `read_media` with
    /// `media`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}

impl std::fmt::Debug for LaunchRegister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchRegister")
            .field("project", &self.project)
            .field("cut", &self.cut)
            .field("credential", &"<redacted>")
            .field("wall_clock_s", &self.wall_clock_s)
            .field("media", &self.media)
            .field("private_regions", &self.private_regions)
            .field("code", &self.code.as_ref().map(|_| "<redacted>"))
            .field("brief_sha256", &self.brief_sha256)
            .field("scopes", &self.scopes)
            .finish()
    }
}

fn default_wall() -> u64 {
    90 * 60
}

/// Control requests (`mode: control`). `Debug` never shows an Owner code or a credential.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Status,
    Connections {
        #[serde(default)]
        n: Option<usize>,
    },
    OwnerBegin {
        command: String,
        args: Value,
    },
    OwnerCall {
        command: String,
        args: Value,
        code: String,
    },
    LaunchRegister(LaunchRegister),
    LaunchEnd {
        run: String,
    },
    /// A second launch forwards its arguments here (M1 stub: logged and acknowledged).
    Forward {
        args: Vec<String>,
    },
    Shutdown,
    /// The app takes the projects over (R-ARCH-7): running jobs stop where they are
    /// (a transcription's windows are written to `<app data>/handover/<job>.json`), every
    /// project closes (snapshot rows written, lock released), the answer lists them, and
    /// this holder stops so the app can serve the socket.
    Handover,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Control::Status => f.write_str("Status"),
            Control::Connections { n } => f.debug_struct("Connections").field("n", n).finish(),
            Control::OwnerBegin { command, .. } => f
                .debug_struct("OwnerBegin")
                .field("command", command)
                .finish_non_exhaustive(),
            Control::OwnerCall { command, .. } => f
                .debug_struct("OwnerCall")
                .field("command", command)
                .field("code", &"<redacted>")
                .finish_non_exhaustive(),
            Control::LaunchRegister(l) => f.debug_tuple("LaunchRegister").field(l).finish(),
            Control::LaunchEnd { run } => f.debug_struct("LaunchEnd").field("run", run).finish(),
            Control::Forward { args } => f
                .debug_struct("Forward")
                .field("args", &args.len())
                .finish(),
            Control::Shutdown => f.write_str("Shutdown"),
            Control::Handover => f.write_str("Handover"),
        }
    }
}

impl Holder {
    /// [`Control::Handover`]: stop the jobs where they are, close every project, stop.
    pub fn handover(self: &Arc<Self>) -> Result<Value, CmdError> {
        self.handing_over.store(true, Ordering::Relaxed);
        let running = self.jobs.running();
        for j in &running {
            j.cancel.store(true, Ordering::Relaxed);
        }
        for j in &running {
            let _ = j.wait(Duration::from_secs(60));
        }
        let dir = self.env.app_data.join("handover");
        let _ = crate::paths::mkdir_private(&dir);
        let partials = self
            .asr_partials
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let mut jobs = Vec::new();
        for j in &running {
            match j.kind {
                "analysis" | "asr" => {
                    let Some((project, source)) = j.key.split_once(':') else {
                        continue;
                    };
                    let partial = partials.get(&j.key).cloned().unwrap_or_default();
                    let done = partial.done;
                    let body = json!({"job": j.id, "project": project, "source": source, "partial": partial});
                    let file = dir.join(format!("{}.json", j.id));
                    let tmp = dir.join(format!("{}.json.tmp", j.id));
                    if std::fs::write(&tmp, body.to_string()).is_ok() {
                        let _ = std::fs::rename(&tmp, &file);
                    }
                    jobs.push(json!({"job": j.id, "kind": "analysis", "project": project, "source": source,
                                     "identity": j.identity, "done_flicks": done}));
                }
                "export" => jobs.push(
                    json!({"job": j.id, "kind": "export", "cut": j.cut, "identity": j.identity}),
                ),
                _ => {}
            }
        }
        let projects: Vec<Value> = self
            .projects
            .lock()
            .map(|g| {
                g.values()
                    .map(|p| json!({"id": p.info.id, "home": p.home.display().to_string()}))
                    .collect()
            })
            .unwrap_or_default();
        self.close_all();
        // stop once the answer is out
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("gmclip-handover".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                me.shutdown.notify_waiters();
                me.shutdown.notify_one();
            });
        Ok(json!({"projects": projects, "jobs": jobs}))
    }

    /// Answers one control request.
    pub fn control(self: &Arc<Self>, req: Control, conn: &Conn) -> Value {
        let r: Result<Value, CmdError> = match req {
            Control::Status => Ok(self.status()),
            Control::Connections { n } => Ok(serde_json::to_value(settings::connections(
                &self.env.app_data,
                n.unwrap_or(50),
            ))
            .unwrap_or(Value::Null)),
            Control::OwnerBegin { command, args } => self.owner_begin(&command, &args, conn),
            Control::OwnerCall {
                command,
                args,
                code,
            } => self.owner_call(&command, &args, &code, conn),
            Control::LaunchRegister(l) => self.launch_register(&l, conn),
            Control::LaunchEnd { run } => {
                self.launch_end(&run);
                settings::audit(
                    &self.env.app_data,
                    "launch_end",
                    &format!("launched:{run}"),
                    "",
                    conn.pid,
                    conn.exe.as_deref(),
                    "ok",
                );
                Ok(json!({}))
            }
            Control::Forward { args } => {
                Ok(json!({"forwarded": args.len(), "pid": std::process::id()}))
            }
            Control::Shutdown => {
                self.shutdown.notify_waiters();
                self.shutdown.notify_one();
                Ok(json!({"stopping": true}))
            }
            Control::Handover => self.handover(),
        };
        match r {
            Ok(v) => json!({"ok": true, "result": v}),
            Err(e) => json!({"ok": false, "error": e}),
        }
    }
}
