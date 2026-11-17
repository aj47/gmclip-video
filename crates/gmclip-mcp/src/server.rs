//! `gmclip serve --headless` (R-ARCH-3, R-ARCH-5): one holder process, a Unix socket in
//! a 0700 per-user directory (Windows: an owner-only named pipe, decisions/0019), peers
//! verified, MCP JSON-RPC over rmcp (2026-07-28 stateless, plus `initialize` for
//! 2025-06-18 and 2025-11-25).

use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gmclip_core::CmdError;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

use crate::holder::{Conn, Control, Holder};
use crate::paths::{self, Env, PathError};
use crate::tools;
use crate::transport::{self, Listener, Peer, ServerStream};
use crate::wire::{self, ClientHello, Hello, Mode, Welcome};

/// The versions served (R-MCP: 2026-07-28 stateless, plus initialize for two older).
pub const VERSIONS: [ProtocolVersion; 3] = [
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2026_07_28,
];

/// One MCP connection's handler.
#[derive(Clone, Debug)]
pub struct McpConn {
    pub holder: Arc<Holder>,
    pub conn: Arc<Conn>,
}

impl McpConn {
    /// Sets `local:<client>` from the client's own name the first time it is known, and
    /// logs the connection (R-MCP-1, R-MCP-1c).
    fn identify(&self, ctx: &RequestContext<RoleServer>) {
        if let Ok(mut id) = self.conn.identity.lock()
            && id.is_none()
        {
            let name = ctx
                .client_info()
                .map_or_else(|| "unknown".to_owned(), |i| i.name);
            *id = Some(self.conn.local_identity(&name));
        }
        self.holder.log_conn(&self.conn, "mcp");
    }
}

/// One text block carrying the JSON result (with its `display`), sent once: no
/// `structuredContent` copy, so the R-MCP-5 sizes are what the model receives.
fn result_of(r: Result<Value, CmdError>, help: Option<Value>) -> CallToolResult {
    match r {
        Ok(mut v) => {
            // `see`: the PNG goes in an image block, the facts in the text block
            let img = v.as_object_mut().and_then(|o| o.remove("_image_png_b64"));
            let mut blocks = vec![ContentBlock::text(tools::text_of(&v))];
            if let Some(Value::String(b64)) = img {
                blocks.push(ContentBlock::image(b64, "image/png"));
            }
            CallToolResult::success(blocks)
        }
        Err(e) => {
            let mut v = json!({ "error": e, "display": e.to_string() });
            // a refused path says which folders may be read and how the human adds one
            if let Some(h) = help {
                let readable = h["readable"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default();
                v["display"] = Value::String(format!(
                    "{}: that path is outside the folders gmclip's tools may read for you. Readable: {readable}. {}",
                    e,
                    h["how"].as_str().unwrap_or_default()
                ));
                v["help"] = h;
            }
            CallToolResult::error(vec![ContentBlock::text(tools::text_of(&v))])
        }
    }
}

/// At most this many connections at once.
pub const MAX_CONNECTIONS: usize = 64;
/// No MCP message is longer (rmcp's own codec has no limit).
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// A control connection idle this long is closed.
pub const CONTROL_IDLE: Duration = Duration::from_secs(300);

/// A stream whose reads fail once a line exceeds `max` bytes.
#[derive(Debug)]
pub struct LineCap<S> {
    inner: S,
    since_nl: usize,
    max: usize,
}

impl<S> LineCap<S> {
    pub fn new(inner: S, max: usize) -> Self {
        LineCap {
            inner,
            since_nl: 0,
            max,
        }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for LineCap<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let this = &mut *self;
        let r = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &r {
            for &b in &buf.filled()[before..] {
                this.since_nl = if b == b'\n' { 0 } else { this.since_nl + 1 };
            }
            if this.since_nl > this.max {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "message too large",
                )));
            }
        }
        r
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for LineCap<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        b: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The parent of a process and that parent's executable, from the kernel.
fn parent_exe(pid: u32) -> Option<String> {
    let ppid = paths::parent_of(pid)?;
    paths::exe_of(ppid).map(|p| p.display().to_string())
}

/// Printable: canonical, no control characters (it reaches a terminal and a log).
fn printable(s: &str) -> String {
    crate::canon::canonical(crate::canon::clip(s, 512), crate::canon::Kind::Name)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

impl ServerHandler for McpConn {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gmclip", env!("CARGO_PKG_VERSION")))
            .with_instructions(tools::INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(VERSIONS.to_vec())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        self.identify(&ctx);
        std::future::ready(Ok(ListToolsResult::with_all_items(tools::tool_list_for(
            &self.conn,
        ))))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + MaybeSendFuture + '_ {
        self.identify(&ctx);
        let holder = Arc::clone(&self.holder);
        let conn = Arc::clone(&self.conn);
        async move {
            let name = request.name.to_string();
            let args = Value::Object(request.arguments.unwrap_or_default());
            let (r, help) = tokio::task::spawn_blocking(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tools::call(&holder, &conn, &name, args)
                }))
                .unwrap_or_else(|_| {
                    Err(CmdError::Internal {
                        trace_id: "tool-panic".into(),
                    })
                });
                let help =
                    matches!(&r, Err(CmdError::Unauthorized { scope }) if scope == "file_roots")
                        .then(|| holder.file_roots_help(&conn));
                (r, help)
            })
            .await
            .unwrap_or_else(|_| {
                (
                    Err(CmdError::Internal {
                        trace_id: "tool-join".into(),
                    }),
                    None,
                )
            });
            Ok(CallToolResponse::from(result_of(r, help)))
        }
    }
}

/// Why a holder could not start.
#[derive(Debug)]
pub enum ServeError {
    Path(PathError),
    Io(&'static str, std::io::Error),
    /// Another holder is running: the arguments were forwarded to it (second launch).
    Forwarded(Value),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Path(e) => write!(f, "{e}"),
            ServeError::Io(w, e) => write!(f, "{w}: {e}"),
            ServeError::Forwarded(v) => write!(f, "forwarded to the running gmclip: {v}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// Binds the socket: checks the directory, forwards to a live holder (second launch),
/// unlinks a stale socket only after the checks, binds under umask 077.
#[cfg(unix)]
pub async fn bind(env: &Env, forward_args: &[String]) -> Result<Listener, ServeError> {
    paths::ensure_private_dir(env.socket_dir()).map_err(ServeError::Path)?;
    if env.socket.exists() {
        match crate::client::control(
            env,
            &Control::Forward {
                args: forward_args.to_vec(),
            },
            false,
        )
        .await
        {
            Ok(v) => return Err(ServeError::Forwarded(v)),
            Err(_) => {
                // nobody verified answers: a stale socket (the directory is ours and 0700)
                let _ = std::fs::remove_file(&env.socket);
            }
        }
    }
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let l = Listener::bind(&env.socket);
    rustix::process::umask(old);
    let l = l.map_err(|e| ServeError::Io("bind socket", e))?;
    env.write_pointer();
    Ok(l)
}

/// Creates the named pipe (decisions/0019): forwards to a live, verified holder first
/// (second launch); otherwise creates the pipe's first instance, owner-only. A name
/// already in use by anything that did not verify (another user squatting it) is an
/// error, never shared: there is no stale state to clean up, since a pipe goes away
/// with the process that made it.
#[cfg(windows)]
pub async fn bind(env: &Env, forward_args: &[String]) -> Result<Listener, ServeError> {
    if let Ok(v) = crate::client::control(
        env,
        &Control::Forward {
            args: forward_args.to_vec(),
        },
        false,
    )
    .await
    {
        return Err(ServeError::Forwarded(v));
    }
    let l = Listener::bind(&env.socket).map_err(|e| ServeError::Io("create the pipe", e))?;
    env.write_pointer();
    Ok(l)
}

/// The accept loop. Returns when told to shut down or after `idle` with no connection.
pub async fn serve(holder: Arc<Holder>, mut listener: Listener, idle: Duration) {
    loop {
        tokio::select! {
            r = transport::accept(&mut listener) => {
                if let Ok(stream) = r {
                    // handing over to the app (R-ARCH-7): nobody new connects to this one
                    if holder.handing_over.load(Ordering::Relaxed) {
                        drop(stream);
                        continue;
                    }
                    let h = Arc::clone(&holder);
                    tokio::spawn(async move { handle(h, stream).await });
                }
            }
            () = holder.shutdown.notified() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                let quiet = holder.last_activity.lock().map(|g| g.elapsed()).unwrap_or_default();
                if holder.active.load(Ordering::Relaxed) == 0 && quiet >= idle {
                    break;
                }
            }
        }
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(&holder.env.socket);
    // Windows: the pipe goes away with its last instance
    drop(listener);
    let h = Arc::clone(&holder);
    let _ = tokio::task::spawn_blocking(move || h.close_all()).await;
}

struct Active(Arc<Holder>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

async fn handle(holder: Arc<Holder>, mut stream: ServerStream) {
    let n = holder.active.fetch_add(1, Ordering::Relaxed);
    let _guard = Active(Arc::clone(&holder));
    if n >= MAX_CONNECTIONS {
        return;
    }
    holder.touch();
    // 1. the peer is this user, or nothing
    let Some(peer) = transport::peer(&stream) else {
        return;
    };
    let pid = match peer {
        Peer::Ours(pid) => pid,
        Peer::Other(pid) => {
            crate::settings::audit(
                &holder.env.app_data,
                "refused",
                "connect",
                "",
                pid,
                None,
                "wrong uid",
            );
            return;
        }
    };
    let (exe, client_exe) = match pid {
        Some(p) => tokio::task::spawn_blocking(move || (paths::exe_of(p), parent_exe(p)))
            .await
            .unwrap_or((None, None)),
        None => (None, None),
    };
    // 2. hello with a fresh nonce
    let server_nonce = wire::nonce();
    if wire::write_json(
        &mut stream,
        &Hello {
            gmclip: wire::VERSION,
            server_nonce: server_nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let Ok(Ok(hello)) = tokio::time::timeout(
        Duration::from_secs(10),
        wire::read_json::<_, ClientHello>(&mut stream),
    )
    .await
    else {
        return;
    };
    // 3. identity: a launched run's proof, or local (named by the client's MCP name)
    let proven = match holder.prove(&hello, &server_nonce) {
        Ok(p) => p,
        Err(e) => {
            let _ = wire::write_json(
                &mut stream,
                &Welcome {
                    ok: false,
                    identity: None,
                    error: Some(e.to_string()),
                },
            )
            .await;
            return;
        }
    };
    let launched = proven.launched;
    let identity = launched.as_ref().map(|l| format!("launched:{}", l.run));
    // a session label groups a local agent's connections (decision 0050); a launched
    // run's proof wins over it, and a malformed one is refused, never shortened
    let session = match hello.session.as_deref() {
        Some(s) if wire::session_label(s).is_none() => {
            let _ = wire::write_json(
                &mut stream,
                &Welcome {
                    ok: false,
                    identity: None,
                    error: Some("GMCLIP_SESSION: 1–64 letters, digits, '.', '_' or '-'".into()),
                },
            )
            .await;
            return;
        }
        Some(s) if launched.is_none() && hello.mode == Mode::Mcp => Some(s.to_owned()),
        _ => None,
    };
    let conn = Arc::new(Conn {
        id: holder.next_conn_id(),
        pid,
        exe,
        // the kernel's answer, not the client's claim (the first-seen key)
        client_exe: client_exe.as_deref().map(printable),
        identity: std::sync::Mutex::new(identity.clone()),
        launched,
        granted: proven.granted,
        session,
        touched: std::sync::Mutex::new(Default::default()),
        logged: std::sync::Mutex::new(false),
    });
    if wire::write_json(
        &mut stream,
        &Welcome {
            ok: true,
            identity,
            error: None,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    match hello.mode {
        // a launched run never gets control requests (no registering, ending or stopping)
        Mode::Control if conn.launched.is_some() => {}
        Mode::Control => {
            if let Ok(mut id) = conn.identity.lock()
                && id.is_none()
            {
                *id = Some("local:gmclip-cli".into());
            }
            holder.log_conn(&conn, "control");
            control_loop(&holder, &conn, stream).await;
        }
        Mode::Mcp => {
            let handler = McpConn {
                holder: Arc::clone(&holder),
                conn: Arc::clone(&conn),
            };
            if let Ok(running) = handler.serve(LineCap::new(stream, MAX_MESSAGE)).await {
                // a session ends with its holder (a handover, R-ARCH-7): the client's
                // shim then finds the next one
                let stop = holder.shutdown.notified();
                tokio::select! {
                    _ = running.waiting() => {}
                    () = stop => {}
                }
            }
            let h = Arc::clone(&holder);
            let c = Arc::clone(&conn);
            let _ = tokio::task::spawn_blocking(move || h.end_conn(&c)).await;
        }
    }
}

async fn control_loop(holder: &Arc<Holder>, conn: &Arc<Conn>, mut stream: ServerStream) {
    loop {
        // a connection ends with its holder (as an MCP session does): on Windows an open
        // instance keeps the pipe name present, and a successor waiting for it to go
        // (takeover) would wait for this client instead
        let line = tokio::select! {
            r = tokio::time::timeout(CONTROL_IDLE, wire::read_line(&mut stream)) => match r {
                Ok(Ok(l)) => l,
                _ => return,
            },
            () = holder.shutdown.notified() => return,
        };
        let reply = match serde_json::from_str::<Control>(&line) {
            Ok(req) => {
                let h = Arc::clone(holder);
                let c = Arc::clone(conn);
                tokio::task::spawn_blocking(move || h.control(req, &c))
                    .await
                    .unwrap_or_else(|_| json!({"ok": false, "error": CmdError::Internal { trace_id: "control".into() }}))
            }
            Err(_) => {
                json!({"ok": false, "error": CmdError::invalid("/", "unknown control request")})
            }
        };
        if wire::write_json(&mut stream, &reply).await.is_err() {
            return;
        }
    }
}

/// Runs a holder in this process until it stops (the CLI's `serve --headless`).
/// Serves `holder` on its socket until shut down or `idle` passes with no connection: the
/// app's embedded holder (it keeps the `Arc` to adopt the projects it opens).
pub fn run_with(holder: Arc<Holder>, idle: Duration) -> Result<(), ServeError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| ServeError::Io("start runtime", e))?;
    rt.block_on(async move {
        let listener = bind(&holder.env, &[]).await?;
        serve(holder, listener, idle).await;
        Ok(())
    })
}

pub fn run_headless(
    env: Env,
    idle: Duration,
    forward_args: Vec<String>,
    planner: Arc<dyn gmclip_core::Planner>,
    media: Arc<dyn gmclip_core::MediaRender>,
    analysis: Arc<dyn gmclip_core::AnalysisHost>,
) -> Result<(), ServeError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| ServeError::Io("start runtime", e))?;
    rt.block_on(async move {
        let listener = bind(&env, &forward_args).await?;
        let holder = Arc::new(Holder::with_hosts(env, planner, media).with_analysis(analysis));
        serve(holder, listener, idle).await;
        Ok(())
    })
}
