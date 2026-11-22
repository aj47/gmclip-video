//! Agent sessions over the Agent Client Protocol (decision 0035): gmclip is the *client*
//! of a coding agent's ACP server (newline-delimited JSON-RPC 2.0 on the agent's stdio).
//! One protocol replaces a flag set and a transcript format per client: gmclip passes
//! itself in as the session's MCP server, streams the agent's messages, tool calls and
//! plan as they happen, answers its permission requests, and sends follow-up prompts,
//! cancels and mode changes into a live session.
//!
//! The same rules as the launcher where they apply (R-AGT-1, R-AGT-2): every program by
//! absolute path, never a shell; an environment whitelist (API keys dropped, so the
//! agent runs on the user's plan login); the child in its own process group so a stop
//! ends its whole tree. This client offers the agent no file system and no terminals:
//! the agent uses its own tools, which gmclip sees and gates through permission
//! requests ([`Handler::permission`]).
//!
//! Sync and std threads only, like the rest of this crate: one reader thread per
//! connection; [`Connection::request`] blocks its caller until the answer.

pub mod update;

pub use update::{PermissionOption, PermissionRequest, PlanEntry, ToolCall, ToolContent, Update};

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use crate::launcher::Harness;
use crate::launcher::detect::{self, Places};

/// The ACP version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// A line longer than this from an agent is dropped (a broken or hostile agent cannot
/// make the reader hold unbounded memory).
pub const MAX_LINE: usize = 8 << 20;

/// How long `initialize` may take: the first `npx` run downloads the adapter.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(180);

/// How long `session/new` and `session/load` may take.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

/// How one agent speaks ACP: its adapter (or its own `acp` command) and how it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    pub harness: Harness,
    /// Absolute.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// What the picker shows ("claude-code-acp", "npx @zed-industries/codex-acp").
    pub via: String,
    /// Put first on the child's `PATH` (the folder the program was found in, which holds
    /// the `node` an npm-installed adapter's `#!/usr/bin/env node` needs).
    pub path_dirs: Vec<PathBuf>,
}

/// Where each agent's ACP server comes from: an installed adapter by name (first found
/// wins), else its npm package through `npx`.
struct Known {
    local: &'static [&'static str],
    local_args: &'static [&'static str],
    package: Option<&'static str>,
}

fn known(h: Harness) -> Known {
    match h {
        Harness::ClaudeCode => Known {
            local: &["claude-agent-acp", "claude-code-acp"],
            local_args: &[],
            package: Some("@agentclientprotocol/claude-agent-acp"),
        },
        Harness::Codex => Known {
            local: &["codex-acp"],
            local_args: &[],
            package: Some("@zed-industries/codex-acp"),
        },
        Harness::Pi => Known {
            local: &["pi-acp"],
            local_args: &[],
            package: Some("pi-acp"),
        },
        // opencode speaks ACP itself
        Harness::OpenCode => Known {
            local: &["opencode"],
            local_args: &["acp"],
            package: None,
        },
    }
}

fn executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        m.is_file()
    }
}

fn first_found(name: &str, places: &Places<'_>) -> Option<PathBuf> {
    detect::candidates_named(name, places)
        .into_iter()
        .find(|p| executable(p))
}

/// Finds how `h` speaks ACP: an installed adapter, else `npx <package>` (which needs the
/// network the first time). Files only, never a shell, so an alias cannot apply.
pub fn find_adapter(h: Harness, places: &Places<'_>) -> Result<Adapter, String> {
    let k = known(h);
    for name in k.local {
        if let Some(p) = first_found(name, places) {
            let dir = p.parent().map(Path::to_path_buf);
            return Ok(Adapter {
                harness: h,
                args: k.local_args.iter().map(|a| (*a).to_owned()).collect(),
                via: std::iter::once(*name)
                    .chain(k.local_args.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" "),
                program: p,
                path_dirs: dir.into_iter().collect(),
            });
        }
    }
    let Some(pkg) = k.package else {
        return Err(format!("{} is not installed", h.label()));
    };
    let Some(npx) = first_found("npx", places) else {
        return Err(format!(
            "no {} adapter and no npx to fetch {pkg}: install Node.js or `npm i -g {pkg}`",
            h.label()
        ));
    };
    Ok(Adapter {
        harness: h,
        args: vec!["-y".into(), pkg.into()],
        via: format!("npx {pkg}"),
        path_dirs: npx.parent().map(Path::to_path_buf).into_iter().collect(),
        program: npx,
    })
}

/// The variables a session's agent inherits: who and where the user is, locale, the
/// clients' own config homes; never API keys, proxies, loader or Node options. `PATH` is
/// the adapter's folders then the system's.
pub fn child_env(
    vars: &[(OsString, OsString)],
    path_dirs: &[PathBuf],
) -> Vec<(OsString, OsString)> {
    const KEEP: &[&str] = &[
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "TMPDIR",
        "SHELL",
        "TERM",
        "TZ",
        "CODEX_HOME",
        "PI_CODING_AGENT_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        // Linux: where the holder socket lives (gmclip-mcp paths)
        "XDG_RUNTIME_DIR",
        // Windows
        "SystemRoot",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "COMSPEC",
    ];
    let mut out: Vec<(OsString, OsString)> = vars
        .iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            KEEP.contains(&k.as_ref()) || k.starts_with("LC_")
        })
        .cloned()
        .collect();
    let mut path: Vec<PathBuf> = path_dirs.to_vec();
    path.extend(detect::SYSTEM_DIRS.iter().map(PathBuf::from));
    #[cfg(unix)]
    path.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    #[cfg(windows)]
    if let Some(root) = vars
        .iter()
        .find(|(k, _)| k == "SystemRoot")
        .map(|(_, v)| PathBuf::from(v))
    {
        path.push(root.join("System32"));
    }
    let mut seen = std::collections::BTreeSet::new();
    path.retain(|p| seen.insert(p.clone()));
    if let Ok(p) = std::env::join_paths(path) {
        out.push(("PATH".into(), p));
    }
    out
}

/// A JSON-RPC error (or this client's own, for a dead connection or a timeout).
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    /// The agent stopped (or its pipe broke) before answering.
    pub const CLOSED: i64 = -32099;
    /// No answer in time.
    pub const TIMEOUT: i64 = -32098;
    /// ACP's "authentication required".
    pub const AUTH_REQUIRED: i64 = -32000;

    fn closed() -> Self {
        Self {
            code: Self::CLOSED,
            message: "the agent stopped".into(),
            data: None,
        }
    }

    pub fn is_auth_required(&self) -> bool {
        self.code == Self::AUTH_REQUIRED
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg =
            update::text_of(&json!({"type": "text", "text": self.message})).unwrap_or_default();
        match self
            .data
            .as_ref()
            .and_then(|d| d.get("details").or(Some(d)))
            .and_then(Value::as_str)
        {
            Some(d) => write!(
                f,
                "{msg}: {}",
                crate::launcher::transcript::canonicalise_untrusted(d)
            ),
            None => f.write_str(&msg),
        }
    }
}

impl std::error::Error for RpcError {}

/// What the connection hands to its owner.
pub trait Handler: Send + Sync + 'static {
    /// A `session/update` for `session`.
    fn update(&self, session: &str, u: Update);
    /// The agent asks before running a tool: answer through `reply`, from any thread,
    /// whenever (the agent waits). Dropping `reply` unanswered cancels the request.
    fn permission(&self, req: PermissionRequest, reply: PermissionReply);
    /// The agent's process ended (`error`: why, when it was not asked to).
    fn closed(&self, error: Option<String>);
}

/// The answer to one permission request; sent once.
#[derive(Debug)]
pub struct PermissionReply {
    inner: Weak<Inner>,
    id: Value,
    sent: bool,
}

impl PermissionReply {
    /// Picks one of the request's options.
    pub fn select(mut self, option_id: &str) {
        self.send(json!({"outcome": {"outcome": "selected", "optionId": option_id}}));
    }

    /// Answers "cancelled" (the turn is being cancelled, or nobody will answer).
    pub fn cancel(mut self) {
        self.send(json!({"outcome": {"outcome": "cancelled"}}));
    }

    fn send(&mut self, result: Value) {
        self.sent = true;
        if let Some(i) = self.inner.upgrade() {
            let _ = i.write(&json!({"jsonrpc": "2.0", "id": self.id, "result": result}));
        }
    }
}

impl Drop for PermissionReply {
    fn drop(&mut self) {
        if !self.sent {
            self.send(json!({"outcome": {"outcome": "cancelled"}}));
        }
    }
}

type Pending = Mutex<HashMap<u64, mpsc::Sender<Result<Value, RpcError>>>>;

struct Inner {
    stdin: Mutex<Option<ChildStdin>>,
    pending: Pending,
    next: AtomicU64,
    child: Mutex<Child>,
    pid: u32,
    closed: AtomicBool,
    stopping: AtomicBool,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("pid", &self.pid)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl Inner {
    fn write(&self, msg: &Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(msg).map_err(|e| RpcError {
            code: -32603,
            message: e.to_string(),
            data: None,
        })?;
        line.push(b'\n');
        let mut g = self.stdin.lock().map_err(|_| RpcError::closed())?;
        let w = g.as_mut().ok_or_else(RpcError::closed)?;
        w.write_all(&line)
            .and_then(|()| w.flush())
            .map_err(|_| RpcError::closed())
    }

    fn fail_pending(&self) {
        if let Ok(mut g) = self.pending.lock() {
            for (_, tx) in g.drain() {
                let _ = tx.send(Err(RpcError::closed()));
            }
        }
    }
}

/// What `initialize` told us about the agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Initialized {
    pub protocol_version: u64,
    pub load_session: bool,
    /// `sessionCapabilities.resume`: `session/resume` reopens a session without replaying it.
    pub resume_session: bool,
    pub images: bool,
    pub auth_methods: Vec<AuthMethod>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// An MCP server the agent starts for the session (stdio).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    /// Absolute.
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A session mode the agent offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// A model the agent offers for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub name: String,
}

/// A new (or loaded) session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opened {
    pub session_id: String,
    pub modes: Vec<Mode>,
    pub current_mode: Option<String>,
    pub models: Vec<Model>,
    pub current_model: Option<String>,
    /// The `configOptions` entry the models came from (`session/set_config_option`);
    /// `None` when they came from `models` (`session/set_model`).
    pub model_config: Option<String>,
}

/// How to start an agent.
#[derive(Debug)]
pub struct Spawn<'a> {
    pub adapter: &'a Adapter,
    pub cwd: &'a Path,
    pub env: Vec<(OsString, OsString)>,
    /// The agent's stderr goes here (else nowhere).
    pub stderr_to: Option<&'a Path>,
}

/// A live connection to one agent process. Cheap to clone; the process ends with
/// [`Connection::stop`] (or when the agent exits).
#[derive(Clone, Debug)]
pub struct Connection(Arc<Inner>);

impl Connection {
    /// Starts the agent and its reader thread.
    pub fn spawn(sp: Spawn<'_>, handler: Arc<dyn Handler>) -> io::Result<Self> {
        let mut cmd = Command::new(&sp.adapter.program);
        cmd.args(&sp.adapter.args)
            .current_dir(sp.cwd)
            .env_clear()
            .envs(sp.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        match sp.stderr_to {
            Some(p) => {
                cmd.stderr(std::fs::File::create(p)?);
            }
            None => {
                cmd.stderr(Stdio::null());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let inner = Arc::new(Inner {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            pid: child.id(),
            child: Mutex::new(child),
            closed: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("gmclip-acp-reader".into())
            .spawn(move || read_loop(stdout, &weak, handler.as_ref()))?;
        Ok(Self(inner))
    }

    /// The agent's process id.
    pub fn pid(&self) -> u32 {
        self.0.pid
    }

    /// Whether the agent is still running.
    pub fn alive(&self) -> bool {
        !self.0.closed.load(Ordering::SeqCst)
    }

    /// Sends a request and waits for its answer (`None`: no limit).
    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        if !self.alive() {
            return Err(RpcError::closed());
        }
        let id = self.0.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.0
            .pending
            .lock()
            .map_err(|_| RpcError::closed())?
            .insert(id, tx);
        if let Err(e) = self
            .0
            .write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            if let Ok(mut g) = self.0.pending.lock() {
                g.remove(&id);
            }
            return Err(e);
        }
        let got = match timeout {
            Some(t) => rx.recv_timeout(t).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => RpcError {
                    code: RpcError::TIMEOUT,
                    message: format!("{method}: no answer in {} s", t.as_secs()),
                    data: None,
                },
                mpsc::RecvTimeoutError::Disconnected => RpcError::closed(),
            }),
            None => rx.recv().map_err(|_| RpcError::closed()),
        };
        if got.is_err()
            && let Ok(mut g) = self.0.pending.lock()
        {
            g.remove(&id);
        }
        got?
    }

    /// Sends a notification.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.0
            .write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// `initialize`: this client offers no file system and no terminals.
    pub fn initialize(&self) -> Result<Initialized, RpcError> {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "gmclip", "version": env!("CARGO_PKG_VERSION")},
            }),
            Some(INIT_TIMEOUT),
        )?;
        let caps = r.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        let info = r.get("agentInfo").cloned().unwrap_or(Value::Null);
        let st = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(crate::launcher::transcript::canonicalise_untrusted)
        };
        Ok(Initialized {
            protocol_version: r
                .get("protocolVersion")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            load_session: caps
                .get("loadSession")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resume_session: caps
                .pointer("/sessionCapabilities/resume")
                .is_some_and(|r| !r.is_null()),
            images: caps
                .pointer("/promptCapabilities/image")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            auth_methods: r
                .get("authMethods")
                .and_then(Value::as_array)
                .map(|ms| {
                    ms.iter()
                        .filter_map(|m| {
                            Some(AuthMethod {
                                id: m.get("id").and_then(Value::as_str)?.to_owned(),
                                name: st(m, "name").unwrap_or_default(),
                                description: st(m, "description"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            agent_name: st(&info, "title").or_else(|| st(&info, "name")),
            agent_version: st(&info, "version"),
        })
    }

    /// `authenticate` with one of the methods `initialize` listed.
    pub fn authenticate(&self, method_id: &str) -> Result<(), RpcError> {
        self.request(
            "authenticate",
            json!({"methodId": method_id}),
            Some(SESSION_TIMEOUT),
        )
        .map(drop)
    }

    /// `session/new` in `cwd` with `mcp` servers; `meta` is passed as `_meta` (adapters
    /// read their own keys there and ignore the rest).
    pub fn new_session(
        &self,
        cwd: &Path,
        mcp: &[McpServer],
        meta: Option<Value>,
    ) -> Result<Opened, RpcError> {
        let mut p = json!({"cwd": cwd, "mcpServers": mcp_json(mcp)});
        if let Some(m) = meta {
            p["_meta"] = m;
        }
        let r = self.request("session/new", p, Some(SESSION_TIMEOUT))?;
        let id = r
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| RpcError {
                code: -32603,
                message: "session/new: no sessionId".into(),
                data: None,
            })?;
        Ok(opened(id, &r))
    }

    /// `session/load`: the agent replays the session's history as updates first.
    pub fn load_session(
        &self,
        id: &str,
        cwd: &Path,
        mcp: &[McpServer],
        meta: Option<Value>,
    ) -> Result<Opened, RpcError> {
        let mut p = json!({"sessionId": id, "cwd": cwd, "mcpServers": mcp_json(mcp)});
        if let Some(m) = meta {
            p["_meta"] = m;
        }
        let r = self.request("session/load", p, Some(SESSION_TIMEOUT))?;
        Ok(opened(id, &r))
    }

    /// `session/resume`: the session opens again where it left off; nothing is replayed.
    pub fn resume_session(
        &self,
        id: &str,
        cwd: &Path,
        mcp: &[McpServer],
        meta: Option<Value>,
    ) -> Result<Opened, RpcError> {
        let mut p = json!({"sessionId": id, "cwd": cwd, "mcpServers": mcp_json(mcp)});
        if let Some(m) = meta {
            p["_meta"] = m;
        }
        let r = self.request("session/resume", p, Some(SESSION_TIMEOUT))?;
        Ok(opened(id, &r))
    }

    /// `session/prompt`: blocks until the turn ends; the stop reason (`end_turn`,
    /// `cancelled`, `max_tokens`, `max_turn_requests`, `refusal`).
    pub fn prompt(&self, session: &str, blocks: Vec<Value>) -> Result<String, RpcError> {
        let r = self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": blocks}),
            None,
        )?;
        Ok(r.get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned())
    }

    /// `session/cancel`: the running turn ends with `cancelled`.
    pub fn cancel(&self, session: &str) -> Result<(), RpcError> {
        self.notify("session/cancel", json!({"sessionId": session}))
    }

    /// `session/set_mode`.
    pub fn set_mode(&self, session: &str, mode: &str) -> Result<(), RpcError> {
        self.request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": mode}),
            Some(Duration::from_secs(30)),
        )
        .map(drop)
    }

    /// The session's model: `session/set_config_option` on the agent's model option
    /// (`config`), else `session/set_model`.
    pub fn set_model(
        &self,
        session: &str,
        model: &str,
        config: Option<&str>,
    ) -> Result<(), RpcError> {
        let (method, p) = match config {
            Some(c) => (
                "session/set_config_option",
                json!({"sessionId": session, "configId": c, "value": model}),
            ),
            None => (
                "session/set_model",
                json!({"sessionId": session, "modelId": model}),
            ),
        };
        self.request(method, p, Some(Duration::from_secs(30)))
            .map(drop)
    }

    /// Ends the agent's process tree: stdin closed, then TERM, then KILL after `grace`.
    pub fn stop(&self, grace: Duration) {
        self.0.stopping.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.0.stdin.lock() {
            g.take();
        }
        if self.0.closed.load(Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        crate::launcher::spawn::signal_group(self.0.pid, "-TERM");
        let t0 = std::time::Instant::now();
        while t0.elapsed() < grace {
            let done = self
                .0
                .child
                .lock()
                .map(|mut c| matches!(c.try_wait(), Ok(Some(_))))
                .unwrap_or(true);
            if done {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        #[cfg(unix)]
        crate::launcher::spawn::signal_group(self.0.pid, "-KILL");
        if let Ok(mut c) = self.0.child.lock() {
            let _ = c.kill();
        }
    }
}

fn mcp_json(mcp: &[McpServer]) -> Value {
    Value::Array(
        mcp.iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "command": m.command,
                    "args": m.args,
                    "env": m.env.iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn opened(id: &str, r: &Value) -> Opened {
    let st = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(crate::launcher::transcript::canonicalise_untrusted)
    };
    let modes = r.get("modes").cloned().unwrap_or(Value::Null);
    let (models, current_model, model_config) = models_of(r);
    Opened {
        models,
        current_model,
        model_config,
        session_id: id.to_owned(),
        modes: modes
            .get("availableModes")
            .and_then(Value::as_array)
            .map(|ms| {
                ms.iter()
                    .filter_map(|m| {
                        Some(Mode {
                            id: m.get("id").and_then(Value::as_str)?.to_owned(),
                            name: st(m, "name").unwrap_or_default(),
                            description: st(m, "description"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        current_mode: modes
            .get("currentModeId")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// The models a `session/new` (or `session/load`) result offers: the `configOptions`
/// select whose category is `model` (the current protocol), else the older `models`.
fn models_of(r: &Value) -> (Vec<Model>, Option<String>, Option<String>) {
    let name = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(crate::launcher::transcript::canonicalise_untrusted)
    };
    let id = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .map(str::to_owned)
    };
    let option = r
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|os| {
            os.iter()
                .find(|o| o.get("category").and_then(Value::as_str) == Some("model"))
        });
    if let Some(o) = option {
        // a select's options are flat, or in groups of their own `options`
        let mut flat = vec![];
        for x in o
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match x.get("options").and_then(Value::as_array) {
                Some(g) => flat.extend(g.iter()),
                None => flat.push(x),
            }
        }
        let models = flat
            .iter()
            .filter_map(|m| {
                let v = id(m, "value")?;
                Some(Model {
                    name: name(m, "name").unwrap_or_else(|| v.clone()),
                    id: v,
                })
            })
            .collect();
        return (models, id(o, "currentValue"), id(o, "id"));
    }
    let m = r.get("models").cloned().unwrap_or(Value::Null);
    let models = m
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .filter_map(|x| {
                    let v = id(x, "modelId")?;
                    Some(Model {
                        name: name(x, "name").unwrap_or_else(|| v.clone()),
                        id: v,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (models, id(&m, "currentModelId"), None)
}

fn read_loop(stdout: std::process::ChildStdout, weak: &Weak<Inner>, handler: &dyn Handler) {
    let mut r = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read_line_capped(&mut r, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            continue; // not JSON (an adapter's stray log line)
        };
        let Some(inner) = weak.upgrade() else { break };
        dispatch(&inner, msg, handler);
    }
    let Some(inner) = weak.upgrade() else { return };
    inner.closed.store(true, Ordering::SeqCst);
    inner.fail_pending();
    let status = inner.child.lock().ok().and_then(|mut c| c.wait().ok());
    let error = if inner.stopping.load(Ordering::SeqCst) {
        None
    } else {
        match status {
            Some(s) if s.success() => None,
            Some(s) => Some(format!("the agent exited ({s})")),
            None => Some("the agent exited".into()),
        }
    };
    handler.closed(error);
}

/// Reads one line, keeping at most [`MAX_LINE`] + 1 bytes of it (the rest is skipped).
fn read_line_capped(r: &mut impl BufRead, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut total = 0;
    loop {
        let (done, used) = {
            let avail = r.fill_buf()?;
            if avail.is_empty() {
                return Ok(total);
            }
            match avail.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    if out.len() <= MAX_LINE {
                        out.extend_from_slice(&avail[..i]);
                    }
                    (true, i + 1)
                }
                None => {
                    if out.len() <= MAX_LINE {
                        out.extend_from_slice(avail);
                    }
                    (false, avail.len())
                }
            }
        };
        r.consume(used);
        total += used;
        if done {
            return Ok(total);
        }
    }
}

fn dispatch(inner: &Arc<Inner>, msg: Value, handler: &dyn Handler) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned().filter(|i| !i.is_null());
    match (method, id) {
        // an answer to one of ours
        (None, Some(id)) => {
            let Some(n) = id.as_u64() else { return };
            let tx = inner.pending.lock().ok().and_then(|mut g| g.remove(&n));
            if let Some(tx) = tx {
                let res = match msg.get("error") {
                    Some(e) => Err(RpcError {
                        code: e.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: e
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                            .to_owned(),
                        data: e.get("data").cloned(),
                    }),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(res);
            }
        }
        // the agent asks something of us
        (Some(m), Some(id)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            match (m, update::parse_permission(&params)) {
                ("session/request_permission", Some(req)) => handler.permission(
                    req,
                    PermissionReply {
                        inner: Arc::downgrade(inner),
                        id,
                        sent: false,
                    },
                ),
                _ => {
                    let _ = inner.write(&json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": format!("gmclip does not offer {m}")},
                    }));
                }
            }
        }
        // a notification
        (Some("session/update"), None) => {
            let p = msg.get("params").cloned().unwrap_or(Value::Null);
            if let (Some(s), Some(u)) =
                (p.get("sessionId").and_then(Value::as_str), p.get("update"))
            {
                handler.update(s, update::parse(u));
            }
        }
        _ => {}
    }
}

#[cfg(all(test, unix))]
mod tests;
