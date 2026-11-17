//! The client side of the socket: the `gmclip mcp` stdio shim and the CLI's control
//! requests. A client verifies the **server** (uid ours, executable the installed gmclip,
//! directory 0700 and ours) before it sends anything derived from a credential
//! (R-ARCH-5a). When no holder runs, the shim starts `gmclip serve --headless`.
//! Windows: the same checks over the named pipe (decisions/0019).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::holder::Control;
use crate::paths::{self, Env};
use crate::transport::{self, ClientStream, Peer};
use crate::wire::{self, ClientHello, Hello, Mode, Welcome};

#[derive(Debug)]
pub enum ClientError {
    /// No holder answered (and none could be started).
    NoHolder(String),
    /// The socket's directory or server failed a check: nothing was sent.
    NotTrusted(String),
    /// The server refused us.
    Refused(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NoHolder(s) => write!(f, "no gmclip holder: {s}"),
            ClientError::NotTrusted(s) => write!(f, "refusing the gmclip socket: {s}"),
            ClientError::Refused(s) => write!(f, "gmclip refused the connection: {s}"),
            ClientError::Io(e) => write!(f, "socket: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The expected server executable: ours, unless the caller names another (tests that
/// run the holder in-process pass their own exe, which is the same thing).
fn expected_exe() -> Option<PathBuf> {
    paths::our_exe()
}

/// Who serves `s`, from the OS.
#[cfg(unix)]
fn server_peer(s: &ClientStream) -> Result<Peer, ClientError> {
    let cred = s.peer_cred().map_err(ClientError::Io)?;
    let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
    Ok(if cred.uid() == paths::our_uid() {
        Peer::Ours(pid)
    } else {
        Peer::Other(pid)
    })
}

/// Who serves `s`, from the OS (the pipe server's pid and its token's user SID).
#[cfg(windows)]
fn server_peer(s: &ClientStream) -> Result<Peer, ClientError> {
    transport::server_peer(s)
        .ok_or_else(|| ClientError::NotTrusted("cannot identify the pipe's server".into()))
}

/// Verifies the process at the other end of `s` before any credential is used.
pub async fn verify_server(s: &ClientStream, expect: Option<&Path>) -> Result<(), ClientError> {
    let pid = match server_peer(s)? {
        Peer::Other(_) => {
            return Err(ClientError::NotTrusted(
                "the server runs as another user".into(),
            ));
        }
        Peer::Ours(pid) => pid.ok_or_else(|| ClientError::NotTrusted("no server pid".into()))?,
    };
    let exe = tokio::task::spawn_blocking(move || paths::exe_of(pid))
        .await
        .ok()
        .flatten();
    let ok = match (exe.as_deref(), expect) {
        (Some(e), Some(want)) => paths::same_exe(e, want),
        _ => false,
    };
    if !ok {
        // which binary to run instead: the one serving the socket, when it is the gmclip
        // you run (the app's own binary, say); this one never talks to it
        return Err(ClientError::NotTrusted(match exe {
            Some(p) => format!(
                "the server is not gmclip as this program knows it: the socket is served by {}. If that is your gmclip (the app you run), connect with its own binary instead: '{}' mcp (or '{}' call …)",
                p.display(),
                p.display(),
                p.display()
            ),
            None => "the server is not gmclip (its executable is unknown)".into(),
        }));
    }
    Ok(())
}

#[cfg(unix)]
async fn connect_once(env: &Env) -> Result<ClientStream, ClientError> {
    paths::check_private_dir(env.socket_dir()).map_err(|e| match e {
        paths::PathError::Io(..) => ClientError::NoHolder(e.to_string()),
        other => ClientError::NotTrusted(other.to_string()),
    })?;
    ClientStream::connect(&env.socket)
        .await
        .map_err(|e| ClientError::NoHolder(e.to_string()))
}

/// No directory to check on Windows: the pipe's own DACL and the server check after
/// connecting stand in for it (decisions/0019).
#[cfg(windows)]
async fn connect_once(env: &Env) -> Result<ClientStream, ClientError> {
    transport::open(&env.socket)
        .await
        .map_err(|e| ClientError::NoHolder(e.to_string()))
}

/// When this process tree's `TMPDIR` has no holder, the app's own in the per-user temp
/// dir ([`Env::user_fallback`]), taken only if it is the gmclip `expect`ed: an agent whose
/// sandbox sets `TMPDIR` reaches the open app instead of starting a second holder that
/// finds the project `Leased`. Tests isolate by `TMPDIR` and run another executable, so
/// the app on a dev machine is never theirs.
async fn user_holder(env: &Env, expect: Option<&Path>) -> Option<ClientStream> {
    let alt = env.user_fallback()?;
    let s = connect_once(&alt).await.ok()?;
    verify_server(&s, expect).await.ok()?;
    Some(s)
}

/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`: the holder has no console and is not
/// in the shim's Ctrl-C group, so it outlives the shim (the Windows `process_group(0)`).
#[cfg(windows)]
const HOLDER_CREATION_FLAGS: u32 = 0x0000_0008 | 0x0000_0200;

/// The environment a spawned holder keeps (never the agent's credential): the names
/// below, compared without case on Windows, where `Path` and `PATH` are one variable.
fn holder_env_keeps(k: &std::ffi::OsStr) -> bool {
    #[cfg(windows)]
    {
        const KEEP: &[&str] = &[
            "USERPROFILE",
            "USERNAME",
            "LOCALAPPDATA",
            "APPDATA",
            "PATH",
            "PATHEXT",
            "SYSTEMROOT",
            "SYSTEMDRIVE",
            "WINDIR",
            "TEMP",
            "TMP",
            "LANG",
            "GMCLIP_HOLDER_IDLE_S",
            // the pipe this process tree uses (the Windows TMPDIR)
            "GMCLIP_PIPE_TAG",
        ];
        k.to_str()
            .is_some_and(|k| KEEP.iter().any(|w| w.eq_ignore_ascii_case(k)))
    }
    #[cfg(not(windows))]
    {
        matches!(
            k.to_str(),
            Some(
                "HOME"
                    | "USER"
                    | "LOGNAME"
                    | "PATH"
                    | "TMPDIR"
                    | "XDG_RUNTIME_DIR"
                    | "XDG_DATA_HOME"
                    | "LANG"
                    | "GMCLIP_HOLDER_IDLE_S"
            )
        )
    }
}

/// Starts `gmclip serve --headless` detached (its own process group, so it outlives the
/// shim), with stderr to a log in app data.
pub fn spawn_holder(env: &Env) -> Result<(), ClientError> {
    let exe = std::env::current_exe().map_err(ClientError::Io)?;
    let _ = paths::mkdir_private(&env.app_data);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env.app_data.join("holder.log"))
        .map_err(ClientError::Io)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["serve", "--headless"])
        // not the agent's environment, credential or working directory
        .env_clear()
        .envs(std::env::vars_os().filter(|(k, _)| holder_env_keeps(k)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd.current_dir("/"), 0);
    #[cfg(windows)]
    {
        std::os::windows::process::CommandExt::creation_flags(
            cmd.current_dir(&env.app_data),
            HOLDER_CREATION_FLAGS,
        );
        // the holder outlives this shim: it must not inherit the shim's stdio pipes, or
        // the client would wait on them until the holder stops
        gmclip_platform::windows::keep_std_handles_to_self();
    }
    cmd.spawn().map_err(ClientError::Io)?;
    Ok(())
}

/// A launched run's credential (`GMCLIP_RUN`), held only in memory.
#[derive(Clone)]
pub struct RunCredential {
    pub run: String,
    credential: Vec<u8>,
}

impl std::fmt::Debug for RunCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCredential")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

impl RunCredential {
    pub fn new(run: String, credential: Vec<u8>) -> RunCredential {
        RunCredential { run, credential }
    }

    /// From `GMCLIP_RUN` (`<run>:<hex>`), if set.
    pub fn from_env() -> Option<RunCredential> {
        std::env::var("GMCLIP_RUN")
            .ok()
            .as_deref()
            .and_then(wire::parse_run_env)
            .map(|(run, credential)| RunCredential { run, credential })
    }
}

/// Connects, verifies the server, and completes the handshake in `mode` (as a launched
/// run when `run` is given; its credential is used only after the server checks out).
pub async fn connect(
    env: &Env,
    mode: Mode,
    auto_start: bool,
) -> Result<(ClientStream, Welcome), ClientError> {
    connect_as(env, mode, auto_start, None, expected_exe().as_deref()).await
}

/// [`connect`] with an explicit credential and expected server executable.
pub async fn connect_as(
    env: &Env,
    mode: Mode,
    auto_start: bool,
    run: Option<&RunCredential>,
    expect: Option<&Path>,
) -> Result<(ClientStream, Welcome), ClientError> {
    connect_with(env, mode, auto_start, run, None, expect).await
}

/// [`connect_as`] as one connection of an agent session (`session`: its label, decision
/// 0050).
pub async fn connect_with(
    env: &Env,
    mode: Mode,
    auto_start: bool,
    run: Option<&RunCredential>,
    session: Option<&str>,
    expect: Option<&Path>,
) -> Result<(ClientStream, Welcome), ClientError> {
    let first = match connect_once(env).await {
        Err(ClientError::NoHolder(why)) => match user_holder(env, expect).await {
            Some(s) => Ok(s),
            None => Err(ClientError::NoHolder(why)),
        },
        r => r,
    };
    let mut stream = match first {
        Ok(s) => s,
        Err(ClientError::NoHolder(why)) if auto_start => {
            spawn_holder(env)?;
            let t0 = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(25)).await;
                match connect_once(env).await {
                    Ok(s) => break s,
                    Err(ClientError::NoHolder(_)) if t0.elapsed() < Duration::from_secs(5) => {}
                    Err(e) => {
                        return Err(if matches!(e, ClientError::NoHolder(_)) {
                            ClientError::NoHolder(why)
                        } else {
                            e
                        });
                    }
                }
            }
        }
        Err(e) => return Err(e),
    };
    verify_server(&stream, expect).await?;
    let hello: Hello = tokio::time::timeout(Duration::from_secs(10), wire::read_json(&mut stream))
        .await
        .map_err(|_| ClientError::Refused("no hello".into()))?
        .map_err(ClientError::Io)?;
    let client_nonce = wire::nonce();
    // the run credential is used only now, after the server checked out
    let (run, proof) = match run {
        Some(r) => (
            Some(r.run.clone()),
            Some(wire::proof(
                &r.credential,
                &hello.server_nonce,
                &client_nonce,
            )),
        ),
        None => (None, None),
    };
    #[cfg(unix)]
    let parent = rustix::process::getppid().map(|p| p.as_raw_nonzero().get().cast_unsigned());
    // a hint on Windows (a parent's pid can be reused): it only labels the connection log
    #[cfg(windows)]
    let parent = paths::parent_of(std::process::id());
    let client_exe = parent
        .and_then(paths::exe_of)
        .map(|p| p.display().to_string());
    wire::write_json(
        &mut stream,
        &ClientHello {
            mode,
            client_nonce,
            run,
            proof,
            client_exe,
            session: session.map(str::to_owned),
        },
    )
    .await
    .map_err(ClientError::Io)?;
    let welcome: Welcome = wire::read_json(&mut stream)
        .await
        .map_err(ClientError::Io)?;
    if !welcome.ok {
        return Err(ClientError::Refused(welcome.error.unwrap_or_default()));
    }
    Ok((stream, welcome))
}

/// One control request and its answer.
pub async fn control(env: &Env, req: &Control, auto_start: bool) -> Result<Value, ClientError> {
    let (mut s, _) = connect(env, Mode::Control, auto_start).await?;
    request(&mut s, req).await
}

/// A request on an open control connection.
pub async fn request(s: &mut ClientStream, req: &Control) -> Result<Value, ClientError> {
    wire::write_json(s, req).await.map_err(ClientError::Io)?;
    wire::read_json(s).await.map_err(ClientError::Io)
}

/// `gmclip mcp`: proxies stdio to the holder until either side closes.
pub fn run_shim(env: Env) -> Result<(), ClientError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ClientError::Io)?;
    rt.block_on(async move {
        // a set but malformed GMCLIP_RUN is refused, never downgraded to a local session
        // (which would drop the run's cut binding)
        let cred = RunCredential::from_env();
        if cred.is_none() && std::env::var_os("GMCLIP_RUN").is_some() {
            return Err(ClientError::Refused(
                "GMCLIP_RUN is set but malformed: a launched run must prove its credential".into(),
            ));
        }
        let session = session_from_env()?;
        proxy(
            &env,
            cred.as_ref(),
            session.as_deref(),
            expected_exe().as_deref(),
            tokio::io::stdin(),
            tokio::io::stdout(),
            RECONNECT_FOR,
        )
        .await
    })
}

/// `GMCLIP_SESSION`, if set: the agent session this process's calls belong to (decision
/// 0050). A malformed one is refused rather than dropped (the calls would scatter into
/// runs of their own).
pub fn session_from_env() -> Result<Option<String>, ClientError> {
    match std::env::var("GMCLIP_SESSION") {
        Ok(s) if s.is_empty() => Ok(None),
        Ok(s) => wire::session_label(&s)
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| {
                ClientError::Refused("GMCLIP_SESSION: 1–64 letters, digits, '.', '_' or '-'".into())
            }),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ClientError::Refused("GMCLIP_SESSION is not UTF-8".into()))
        }
    }
}

/// The tool call `gmclip call <name> '<json>'` makes: a tool by its name with the JSON as
/// its arguments, else `command_run` of that command, the JSON its `args` (its
/// `project`, `base` and `request_id` go to `command_run` itself).
pub fn call_request(name: &str, args: Value) -> (String, Value) {
    if crate::tools::defs().iter().any(|d| d.name == name) {
        return (name.to_owned(), args);
    }
    let mut args = match args {
        Value::Object(m) => m,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    let mut run = serde_json::Map::new();
    run.insert("command".into(), Value::String(name.to_owned()));
    for k in ["project", "base", "request_id"] {
        if let Some(v) = args.remove(k) {
            run.insert(k.into(), v);
        }
    }
    run.insert("args".into(), Value::Object(args));
    ("command_run".into(), Value::Object(run))
}

/// `gmclip call <tool|command> '<json>'` (decision 0050): one tool call over a fresh
/// connection, as this process's agent session (`GMCLIP_SESSION`) or launched run
/// (`GMCLIP_RUN`), so a script's calls share one identity and one run. Returns the
/// result's JSON and whether it is an error.
pub fn run_call(env: Env, name: &str, args: Value) -> Result<(Value, bool), ClientError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ClientError::Io)?;
    let cred = RunCredential::from_env();
    if cred.is_none() && std::env::var_os("GMCLIP_RUN").is_some() {
        return Err(ClientError::Refused(
            "GMCLIP_RUN is set but malformed: a launched run must prove its credential".into(),
        ));
    }
    let session = session_from_env()?;
    rt.block_on(call_once(
        &env,
        cred.as_ref(),
        session.as_deref(),
        expected_exe().as_deref(),
        name,
        args,
    ))
}

/// [`run_call`]'s one call, with the credential, session and expected server given.
pub async fn call_once(
    env: &Env,
    cred: Option<&RunCredential>,
    session: Option<&str>,
    expect: Option<&Path>,
    name: &str,
    args: Value,
) -> Result<(Value, bool), ClientError> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (tool, arguments) = call_request(name, args);
    {
        let (stream, _) = connect_with(env, Mode::Mcp, true, cred, session, expect).await?;
        let (r, mut w) = transport::split(stream);
        let mut r = BufReader::new(r);
        let send = |v: Value| format!("{v}\n");
        // the answer to request `id` (notifications in between are skipped)
        async fn answer<R: tokio::io::AsyncBufRead + Unpin>(
            r: &mut R,
            id: u64,
        ) -> Result<Value, ClientError> {
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).await.map_err(ClientError::Io)? == 0 {
                    return Err(ClientError::NoHolder("the holder closed".into()));
                }
                if let Ok(v) = serde_json::from_str::<Value>(l.trim_end())
                    && v.get("id").and_then(Value::as_u64) == Some(id)
                    && v.get("method").is_none()
                {
                    return Ok(v);
                }
            }
        }
        w.write_all(
            send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "gmclip-call", "version": env!("CARGO_PKG_VERSION")}}}))
            .as_bytes(),
        )
        .await
        .map_err(ClientError::Io)?;
        answer(&mut r, 1).await?;
        w.write_all(
            send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .as_bytes(),
        )
        .await
        .map_err(ClientError::Io)?;
        w.write_all(
            send(
                serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}}),
            )
            .as_bytes(),
        )
        .await
        .map_err(ClientError::Io)?;
        let v = answer(&mut r, 2).await?;
        let _ = w.shutdown().await;
        if let Some(e) = v.get("error") {
            return Err(ClientError::Refused(
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the call failed")
                    .to_owned(),
            ));
        }
        let res = &v["result"];
        let text = res["content"][0]["text"].as_str().unwrap_or("null");
        let out = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
        Ok((out, res["isError"].as_bool() == Some(true)))
    }
}

/// How long the shim looks for a holder again after its own went away (a handover to
/// the app, R-ARCH-7) before it gives up.
pub const RECONNECT_FOR: Duration = Duration::from_secs(30);

/// The error an agent gets for a request in flight when its holder handed over.
pub const MOVED: &str = "gmclip moved this project to the app while this call ran; ask again (and watch from scratch: project_watch answers with a resync)";

/// The MCP session as newline-delimited JSON between `input`/`output` (the client) and
/// the holder. When the holder goes away while the client is still talking (a handover:
/// the app took the projects over), the shim connects to the next holder **without**
/// starting one (`gmclip serve` must not race the app for the socket), replays the
/// client's `initialize` and `notifications/initialized` (the new holder's answer to the
/// replay is swallowed) and answers each request that was in flight with [`MOVED`].
pub async fn proxy<R, W>(
    env: &Env,
    cred: Option<&RunCredential>,
    session: Option<&str>,
    expect: Option<&Path>,
    input: R,
    mut output: W,
    reconnect_for: Duration,
) -> Result<(), ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (stream, _) = connect_with(env, Mode::Mcp, true, cred, session, expect).await?;
    let mut input = BufReader::new(input);
    let (sr, mut sw) = transport::split(stream);
    let mut server = BufReader::new(sr);
    let mut init: Option<(String, Value)> = None;
    let mut initialized: Option<String> = None;
    let mut pending: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut client_open = true;
    let (mut up, mut down) = (String::new(), String::new());
    loop {
        tokio::select! {
            r = input.read_line(&mut up), if client_open => {
                match r {
                    Ok(0) | Err(_) => {
                        client_open = false;
                        let _ = sw.shutdown().await;
                        // a named pipe has no half-close (shutdown only flushes): the
                        // holder never sees our EOF, so we end once nothing is in flight
                        if cfg!(windows) && pending.is_empty() {
                            return Ok(());
                        }
                    }
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_str::<Value>(up.trim_end()) {
                            match v.get("method").and_then(Value::as_str) {
                                Some("initialize") => init = Some((up.clone(), v["id"].clone())),
                                Some("notifications/initialized") => initialized = Some(up.clone()),
                                _ => {}
                            }
                            if v.get("method").is_some() && let Some(id) = v.get("id") {
                                pending.insert(id.to_string());
                            }
                        }
                        if sw.write_all(up.as_bytes()).await.is_err() {
                            // the holder is gone; the line is answered as moved below
                        }
                        up.clear();
                    }
                }
            }
            r = server.read_line(&mut down) => {
                match r {
                    Ok(n) if n > 0 => {
                        if let Ok(v) = serde_json::from_str::<Value>(down.trim_end())
                            && v.get("method").is_none()
                            && let Some(id) = v.get("id")
                        {
                            pending.remove(&id.to_string());
                        }
                        output.write_all(down.as_bytes()).await.map_err(ClientError::Io)?;
                        output.flush().await.map_err(ClientError::Io)?;
                        down.clear();
                        // Windows, after the client's EOF: the last answer is out
                        if cfg!(windows) && !client_open && pending.is_empty() {
                            return Ok(());
                        }
                    }
                    _ => {
                        // the session ends when the server side closes after our EOF
                        if !client_open {
                            return Ok(());
                        }
                        for id in std::mem::take(&mut pending) {
                            let id: Value = serde_json::from_str(&id).unwrap_or(Value::Null);
                            let e = serde_json::json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": -32001, "message": MOVED}});
                            output.write_all(format!("{e}\n").as_bytes()).await.map_err(ClientError::Io)?;
                        }
                        output.flush().await.map_err(ClientError::Io)?;
                        let t0 = Instant::now();
                        let stream = loop {
                            match connect_with(env, Mode::Mcp, false, cred, session, expect).await {
                                Ok((s, _)) => break s,
                                Err(_) if t0.elapsed() < reconnect_for => {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                                Err(e) => return Err(e),
                            }
                        };
                        let (r2, w2) = transport::split(stream);
                        server = BufReader::new(r2);
                        sw = w2;
                        down.clear();
                        if let Some((line, id)) = &init {
                            sw.write_all(line.as_bytes()).await.map_err(ClientError::Io)?;
                            // swallow the replay's answer
                            loop {
                                let mut l = String::new();
                                if server.read_line(&mut l).await.map_err(ClientError::Io)? == 0 {
                                    return Err(ClientError::NoHolder("the new holder closed".into()));
                                }
                                if serde_json::from_str::<Value>(l.trim_end())
                                    .is_ok_and(|v| v.get("id") == Some(id) && v.get("method").is_none())
                                {
                                    break;
                                }
                            }
                            if let Some(n) = &initialized {
                                sw.write_all(n.as_bytes()).await.map_err(ClientError::Io)?;
                            }
                        }
                    }
                }
            }
        }
    }
}
