//! The review link server (SPEC §5.14.1; SR R-REV-1..6, R-PROTO-7).
//!
//! Serves a read-only Home and review board (one static page, its JS and CSS), export
//! files by export id (`/x/<id>`) and moment clips by moment id (`/m/<id>`), and takes
//! exactly two writes: review notes with a keep/change/drop verdict, and "fix with
//! agent" requests. What it serves comes from a [`Catalog`] the host implements; the
//! server itself reads no project and no folder.
//!
//! - **Binding (R-REV-1):** always `127.0.0.1`. Tailnet mode puts `tailscale serve`
//!   (HTTPS) in front of that port; the server then requires the Tailscale-provided
//!   `Tailscale-User-Login` on every request, refuses to start when the port is
//!   funnelled, and stops within 60 s of a funnel appearing. Never `0.0.0.0`; there is no
//!   plain-HTTP LAN mode (LAN-with-TLS needs a TLS stack: not built).
//! - **Tokens (R-REV-2):** a link's 256-bit token is in the URL fragment; the page posts
//!   it once to `/session` for an `HttpOnly; SameSite=Strict; Path=/` cookie (`Secure`
//!   in tailnet mode), ≤ 12 h and never past the link's 7 days. Every request checks
//!   `Host`; writes need the cookie, the session's `X-CSRF` header and a same-origin
//!   `Origin`. No CORS headers; `Referrer-Policy: no-referrer`, `Cache-Control:
//!   no-store`, `nosniff`, and the R-REV-5 CSP (plus Trusted Types) on HTML.
//! - **Identity (R-REV-3):** writes are `review-link:<link id>[/<tailnet login>]`.
//! - **Fix with agent (R-REV-4):** starts only when (a) tailnet mode, (b) the requester's
//!   login is the Owner's, (c) the Owner's remote-fix allowance has a run left today;
//!   otherwise the request is queued for an in-app click. The [`Catalog`] gets the
//!   decision, never the choice.
//! - **Serving (R-REV-6):** files are opened by the path the catalog's record names,
//!   never a request path; no symlinks (`O_NOFOLLOW` + identity check); size checked on
//!   every serve and SHA-256 at the first; one range per response. Its own one-thread
//!   runtime, ≤ 8 connections (the 9th is closed), 10 s to send the request.

pub mod body;
pub mod http;
pub mod links;

use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};

use http::{Asset, Bad, RangeAnswer, Request, Route, WriteRoute};

/// The page's Content-Security-Policy (R-REV-5) plus Trusted Types.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; require-trusted-types-for 'script'; trusted-types 'none'";
/// A session's longest life.
pub const SESSION_TTL_MS: i64 = 12 * 3600 * 1000;
/// The session cookie's name.
pub const COOKIE: &str = "gmr";
/// How long a client may stop reading a response before it is dropped (R-PROTO-7).
pub const WRITE_IDLE: Duration = Duration::from_secs(30);

const INDEX_HTML: &str = include_str!("page/index.html");
const APP_JS: &str = include_str!("page/app.js");
const APP_CSS: &str = include_str!("page/app.css");

/// A file the catalog vouches for: where it is and what it must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRec {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// What the server decided about a "fix with agent" request (R-REV-4).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum FixDecision {
    /// All three conditions held: start a run within this budget.
    Start { budget_usd: f64 },
    /// Queue a Home → "needs you" item for an in-app click.
    Queue { reasons: Vec<String> },
}

/// A "fix with agent" request as the catalog receives it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FixRequest {
    pub cut: String,
    /// Link-authored: untrusted (R-INJ-1).
    pub note: String,
    pub author: String,
    pub decision: FixDecision,
}

/// What the server decided about an "Approve" (decision 0015): the same three conditions
/// as a remote fix, with the Owner's remote-send allowance as (c).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum ApproveDecision {
    /// Hand the export to Clippy now (the tap is the human click).
    Send,
    /// Queue a Home → "needs you" item for an in-app click.
    Queue { reasons: Vec<String> },
}

/// An "Approve" as the catalog receives it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ApproveRequest {
    pub export: String,
    pub author: String,
    pub decision: ApproveDecision,
}

const NOT_HERE: &str = "this gmclip does not take that from a review link";

/// What the host serves and stores. Text in [`Catalog::home`] is creator- or
/// other-authored; the page renders every field with `textContent`.
pub trait Catalog: Send + Sync + 'static {
    /// Home and the review board: `{project, exports[{id, title, duration_s, blocked}],
    /// cuts[{id, title, notes, reviews[{author, verdict, text}]}], moments[{id, quote,
    /// clip}]}`.
    fn home(&self) -> Value;
    /// An export by export id.
    fn export(&self, id: &str) -> Option<FileRec>;
    /// A moment clip by moment id.
    fn clip(&self, id: &str) -> Option<FileRec>;
    /// Records a review (`author` is the link identity). Returns the review's id.
    fn add_review(
        &self,
        author: &str,
        cut: &str,
        text: &str,
        verdict: &str,
        export_ref: Option<&str>,
    ) -> Result<String, String>;
    /// Acts on a fix request the server already decided.
    fn fix(&self, req: &FixRequest) -> Result<Value, String>;
    /// Records a comment or reply (decision 0015; `author` is the link identity, the
    /// text canonical and untrusted). Returns the comment's id.
    fn comment(&self, _author: &str, _c: &body::CommentBody) -> Result<String, String> {
        Err(NOT_HERE.into())
    }
    /// Resolves or reopens a thread.
    fn resolve(&self, _author: &str, _r: &body::ResolveBody) -> Result<(), String> {
        Err(NOT_HERE.into())
    }
    /// Records a suggested edit (editor links only; never applied here).
    fn suggest(&self, _author: &str, _s: &body::SuggestBody) -> Result<String, String> {
        Err(NOT_HERE.into())
    }
    /// Acts on an "Approve" the server already decided.
    fn approve(&self, _req: &ApproveRequest) -> Result<Value, String> {
        Err(NOT_HERE.into())
    }
}

/// Tailscale facts the server needs (a CLI-backed implementation lives in the host).
pub trait Tailnet: Send + Sync + 'static {
    /// Whether Tailscale Funnel exposes `port` to the internet.
    fn funnel_covers(&self, port: u16) -> Result<bool, String>;
}

/// Where the link is reachable from (an Owner setting).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// This machine only.
    Loopback,
    /// Through `tailscale serve` at `https://<host>`; `owner_login` is the Owner's
    /// tailnet login (recorded when the Owner turned tailnet mode on).
    Tailnet { host: String, owner_login: String },
}

/// The Owner's standing remote-fix allowance (off when absent).
#[derive(Clone, Debug, PartialEq)]
pub struct Allowance {
    pub max_usd_per_run: f64,
    pub max_runs_per_day: u32,
}

/// Server configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub app_data: PathBuf,
    pub mode: Mode,
    /// 0 picks a free port.
    pub port: u16,
    pub allowance: Option<Allowance>,
    pub max_conns: usize,
    pub header_timeout: Duration,
    pub funnel_every: Duration,
}

impl Config {
    pub fn new(app_data: PathBuf, mode: Mode) -> Config {
        Config {
            app_data,
            mode,
            port: 0,
            allowance: None,
            max_conns: 8,
            header_timeout: Duration::from_secs(10),
            funnel_every: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug)]
struct Session {
    link: String,
    csrf: String,
    expires_ms: i64,
    login: Option<String>,
    /// The link's role (`editor`), decision 0015.
    role: Option<String>,
}

impl Session {
    fn editor(&self) -> bool {
        self.role.as_deref() == Some("editor")
    }
}

/// A verified file: (path, size, dev, ino, mtime ns).
type Verified = (PathBuf, u64, u64, u64, i64);

struct State {
    cfg: Config,
    port: u16,
    catalog: Arc<dyn Catalog>,
    sessions: Mutex<HashMap<String, Session>>,
    /// Files whose SHA-256 matched their record: id → (path, size, dev, ino, mtime).
    verified: Mutex<HashMap<String, Verified>>,
    fix_lock: Mutex<()>,
}

/// A running server.
pub struct Server {
    pub addr: SocketAddr,
    stop: Arc<Notify>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("addr", &self.addr).finish()
    }
}

impl Server {
    /// Whether the server stopped by itself (a funnel appeared).
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Stops it and waits for its thread.
    pub fn stop(mut self) {
        self.stop.notify_one();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }

    /// Blocks until the server stops.
    pub fn wait(mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Starts the server on its own thread and one-thread runtime.
pub fn start(
    cfg: Config,
    catalog: Arc<dyn Catalog>,
    tailnet: Option<Arc<dyn Tailnet>>,
) -> Result<Server, String> {
    if let Mode::Tailnet { host, owner_login } = &cfg.mode {
        if host.is_empty() || owner_login.is_empty() {
            return Err("tailnet mode needs the tailnet host name and the Owner's login".into());
        }
        if tailnet.is_none() {
            return Err("tailnet mode needs Tailscale".into());
        }
    }
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", cfg.port))
        .map_err(|e| format!("bind 127.0.0.1 ({})", e.kind()))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("listener ({})", e.kind()))?;
    let addr = std_listener
        .local_addr()
        .map_err(|e| format!("listener ({})", e.kind()))?;
    if let (Mode::Tailnet { .. }, Some(t)) = (&cfg.mode, &tailnet) {
        match t.funnel_covers(addr.port()) {
            Ok(false) => {}
            Ok(true) => {
                return Err("Tailscale Funnel exposes this port to the internet: refused".into());
            }
            Err(e) => return Err(format!("cannot check Tailscale Funnel ({e}): refused")),
        }
    }
    let stop = Arc::new(Notify::new());
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stop2, stopped2) = (Arc::clone(&stop), Arc::clone(&stopped));
    let state = Arc::new(State {
        port: addr.port(),
        cfg,
        catalog,
        sessions: Mutex::new(HashMap::new()),
        verified: Mutex::new(HashMap::new()),
        fix_lock: Mutex::new(()),
    });
    let join = std::thread::Builder::new()
        .name("gmclip-review".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async move {
                let Ok(listener) = TcpListener::from_std(std_listener) else {
                    return;
                };
                let sem = Arc::new(Semaphore::new(state.cfg.max_conns));
                let funnel_every = state.cfg.funnel_every;
                let mut tick = tokio::time::interval(funnel_every);
                tick.tick().await;
                loop {
                    tokio::select! {
                        () = stop2.notified() => break,
                        _ = tick.tick(), if tailnet.is_some() => {
                            if let Some(t) = &tailnet
                                && !matches!(t.funnel_covers(state.port), Ok(false))
                            {
                                eprintln!("gmclip review: Tailscale Funnel now covers the review port (or cannot be checked): stopped");
                                break;
                            }
                        }
                        acc = listener.accept() => {
                            let Ok((stream, peer)) = acc else { continue };
                            match Arc::clone(&sem).try_acquire_owned() {
                                Ok(permit) => {
                                    let st = Arc::clone(&state);
                                    tokio::spawn(async move {
                                        let _permit = permit;
                                        serve_conn(st, stream, peer).await;
                                    });
                                }
                                Err(_) => drop(stream), // over the connection cap: refused
                            }
                        }
                    }
                }
                stopped2.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        })
        .map_err(|e| format!("thread ({})", e.kind()))?;
    Ok(Server {
        addr,
        stop,
        stopped,
        join: Some(join),
    })
}

/// A response before it is written.
struct Resp {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Body,
    /// HEAD: send the headers (with the body's length) and no body.
    head_only: bool,
}

enum Body {
    Bytes(Vec<u8>),
    /// The very file descriptor that was verified (never reopened by path).
    File {
        file: std::fs::File,
        start: u64,
        len: u64,
    },
}

fn resp(status: u16, ctype: &str, body: Vec<u8>) -> Resp {
    Resp {
        status,
        headers: vec![("Content-Type", ctype.to_owned())],
        body: Body::Bytes(body),
        head_only: false,
    }
}

fn json_resp(status: u16, v: &Value) -> Resp {
    resp(
        status,
        "application/json",
        serde_json::to_vec(v).unwrap_or_default(),
    )
}

fn err(status: u16, what: &str) -> Resp {
    json_resp(status, &json!({"error": what}))
}

async fn serve_conn(st: Arc<State>, mut stream: TcpStream, _peer: SocketAddr) {
    let read = tokio::time::timeout(st.cfg.header_timeout, read_request(&mut stream)).await;
    let r = match read {
        Err(_) => return, // too slow: dropped (R-PROTO-7)
        Ok(Err(b)) => match b {
            Bad::Malformed => err(400, "bad request"),
            Bad::TooLarge => err(413, "too large"),
            Bad::Unsupported => err(501, "not supported"),
        },
        // catalog reads, project writes and first-serve hashing are blocking work:
        // off the server's one thread (R-PROTO-7)
        Ok(Ok(req)) => {
            let st2 = Arc::clone(&st);
            match tokio::task::spawn_blocking(move || handle(&st2, &req)).await {
                Ok(r) => r,
                Err(_) => err(500, "internal"),
            }
        }
    };
    let _ = write_resp(&mut stream, r).await;
    let _ = stream.shutdown().await;
}

async fn read_request(s: &mut TcpStream) -> Result<Request, Bad> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(e) = http::header_end(&buf) {
            break e;
        }
        if buf.len() > http::MAX_HEADERS {
            return Err(Bad::TooLarge);
        }
        let n = s.read(&mut chunk).await.map_err(|_| Bad::Malformed)?;
        if n == 0 {
            return Err(Bad::Malformed);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    if end > http::MAX_HEADERS {
        return Err(Bad::TooLarge);
    }
    let (mut req, len) = http::parse_head(&buf[..end - 4])?;
    let mut body = buf[end..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk).await.map_err(|_| Bad::Malformed)?;
        if n == 0 {
            return Err(Bad::Malformed);
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > http::MAX_BODY {
            return Err(Bad::TooLarge);
        }
    }
    body.truncate(len);
    req.body = body;
    Ok(req)
}

async fn write_resp(s: &mut TcpStream, r: Resp) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", r.status, http::reason(r.status));
    let mut hs: Vec<(&str, String)> = vec![
        ("Referrer-Policy", "no-referrer".into()),
        ("X-Content-Type-Options", "nosniff".into()),
        ("X-Frame-Options", "DENY".into()),
        ("Cross-Origin-Resource-Policy", "same-origin".into()),
        ("Cache-Control", "no-store".into()),
        ("Connection", "close".into()),
    ];
    hs.extend(r.headers.iter().map(|(k, v)| (*k, v.clone())));
    let len = match &r.body {
        Body::Bytes(b) => b.len() as u64,
        Body::File { len, .. } => *len,
    };
    hs.push(("Content-Length", len.to_string()));
    for (k, v) in hs {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(&v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    tokio::time::timeout(WRITE_IDLE, s.write_all(head.as_bytes()))
        .await
        .map_err(|_| std::io::Error::other("client stopped reading"))??;
    if r.head_only {
        return s.flush().await;
    }
    match r.body {
        Body::Bytes(b) => tokio::time::timeout(WRITE_IDLE, s.write_all(&b))
            .await
            .map_err(|_| std::io::Error::other("client stopped reading"))??,
        Body::File {
            mut file,
            start,
            len,
        } => {
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(start))?;
            let mut left = len;
            let mut buf = vec![0u8; 256 * 1024];
            while left > 0 {
                let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
                // disk reads off the server thread; a client that stops reading is
                // dropped after WRITE_IDLE (R-PROTO-7)
                let (f2, mut b2, n) = tokio::task::spawn_blocking(move || {
                    let mut f2 = file;
                    let n = f2.read(&mut buf[..want]);
                    (f2, buf, n)
                })
                .await
                .map_err(std::io::Error::other)?;
                file = f2;
                let n = n?;
                if n == 0 {
                    break;
                }
                tokio::time::timeout(WRITE_IDLE, s.write_all(&b2[..n]))
                    .await
                    .map_err(|_| std::io::Error::other("client stopped reading"))??;
                buf = std::mem::take(&mut b2);
                left -= n as u64;
            }
        }
    }
    s.flush().await
}

#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(all(unix, not(target_os = "macos")))]
const O_NOFOLLOW: i32 = 0o400000;
/// Windows (W.9): open a symlink or junction itself, then refuse it by the handle's
/// attributes (no `O_NOFOLLOW` there).
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// Opens a regular file, refusing a symlink as the final component.
fn open_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let f = o.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let fm = f.metadata()?;
        if fm.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !fm.is_file() {
            return Err(std::io::Error::other("a symlink or junction"));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let fm = f.metadata()?;
        if fm.dev() != meta.dev() || fm.ino() != meta.ino() {
            return Err(std::io::Error::other("changed while opening"));
        }
    }
    Ok(f)
}

fn expected_host(st: &State, req: &Request) -> bool {
    if req.dup_host {
        return false;
    }
    let Some(h) = req.header("host") else {
        return false;
    };
    match &st.cfg.mode {
        Mode::Loopback => {
            h == format!("127.0.0.1:{}", st.port) || h == format!("localhost:{}", st.port)
        }
        Mode::Tailnet { host, .. } => h.eq_ignore_ascii_case(host),
    }
}

fn origin_ok(st: &State, req: &Request) -> bool {
    let Some(o) = req.header("origin") else {
        return false;
    };
    match &st.cfg.mode {
        Mode::Loopback => req
            .header("host")
            .is_some_and(|h| o == format!("http://{h}")),
        Mode::Tailnet { host, .. } => o.eq_ignore_ascii_case(&format!("https://{host}")),
    }
}

/// The tailnet login on a request (only in tailnet mode; set by `tailscale serve`).
fn login(st: &State, req: &Request) -> Option<String> {
    match st.cfg.mode {
        Mode::Tailnet { .. } => req
            .header("tailscale-user-login")
            .filter(|l| {
                !l.is_empty()
                    && l.len() <= 200
                    && l.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-._@+".contains(c))
            })
            .map(str::to_owned),
        Mode::Loopback => None,
    }
}

fn session(st: &State, req: &Request) -> Option<Session> {
    let sid = req.cookie(COOKIE)?;
    let now = now_ms();
    let mut g = st.sessions.lock().ok()?;
    g.retain(|_, s| s.expires_ms > now);
    let s = g.get(sid)?.clone();
    drop(g);
    // the link is re-checked every time: a revoke is immediate
    if !links::live(&st.cfg.app_data, &s.link, now) {
        return None;
    }
    // in tailnet mode the same login must make every request
    if s.login != login(st, req) {
        return None;
    }
    Some(s)
}

fn author(s: &Session) -> String {
    match &s.login {
        Some(l) => format!("review-link:{}/{l}", s.link),
        None => format!("review-link:{}", s.link),
    }
}

fn valid_id(id: &str) -> bool {
    http::record_id(id)
}

fn html(body: &str) -> Resp {
    let mut r = resp(200, "text/html; charset=utf-8", body.as_bytes().to_vec());
    r.headers.push(("Content-Security-Policy", CSP.to_owned()));
    r
}

fn handle(st: &State, req: &Request) -> Resp {
    if !expected_host(st, req) {
        return err(403, "host");
    }
    if matches!(st.cfg.mode, Mode::Tailnet { .. }) && login(st, req).is_none() {
        return err(403, "tailnet identity");
    }
    let head_only = req.method == "HEAD";
    match http::route(&req.method, &req.path) {
        Route::Page(Asset::Index) => Resp {
            head_only,
            ..html(INDEX_HTML)
        },
        Route::Page(Asset::Js) => Resp {
            head_only,
            ..resp(
                200,
                "text/javascript; charset=utf-8",
                APP_JS.as_bytes().to_vec(),
            )
        },
        Route::Page(Asset::Css) => Resp {
            head_only,
            ..resp(200, "text/css; charset=utf-8", APP_CSS.as_bytes().to_vec())
        },
        Route::StartSession => start_session(st, req),
        Route::Home => match session(st, req) {
            None => err(401, "session"),
            Some(s) => {
                let mut v = st.catalog.home();
                v["csrf"] = Value::String(s.csrf.clone());
                v["link"] = Value::String(s.link.clone());
                v["role"] = Value::String(if s.editor() { "editor" } else { "reviewer" }.into());
                json_resp(200, &v)
            }
        },
        Route::File { clip, id } => {
            if session(st, req).is_none() {
                return err(401, "session");
            }
            let Some(id) = id else {
                return err(404, "not found");
            };
            let rec = if clip {
                st.catalog.clip(id)
            } else {
                st.catalog.export(id)
            };
            match rec {
                None => err(404, "not found"),
                Some(r) => file(
                    st,
                    &format!("{}{id}", if clip { "m" } else { "x" }),
                    &r,
                    req,
                ),
            }
        }
        Route::Write(WriteRoute::Review) => write(st, req, |s, body| {
            if s.editor() {
                return err(403, "an editor's link comments and suggests edits only");
            }
            review(st, s, body)
        }),
        Route::Write(WriteRoute::Fix) => write(st, req, |s, body| {
            if s.editor() {
                return err(403, "an editor's link comments and suggests edits only");
            }
            fix(st, s, body)
        }),
        Route::Write(WriteRoute::Comment) => write(st, req, |s, b| match body::comment(b) {
            Err(e) => err(400, e),
            Ok(mut c) => {
                // stored canonical (R-INJ-6)
                c.text = crate::canon::canonical(&c.text, crate::canon::Kind::Text);
                match st.catalog.comment(&author(s), &c) {
                    Ok(id) => json_resp(200, &json!({"ok": true, "comment": id})),
                    Err(e) => json_resp(409, &json!({"error": e})),
                }
            }
        }),
        Route::Write(WriteRoute::Resolve) => write(st, req, |s, b| match body::resolve(b) {
            Err(e) => err(400, e),
            Ok(r) => match st.catalog.resolve(&author(s), &r) {
                Ok(()) => json_resp(200, &json!({"ok": true})),
                Err(e) => json_resp(409, &json!({"error": e})),
            },
        }),
        Route::Write(WriteRoute::Suggest) => write(st, req, |s, b| {
            if !s.editor() {
                return err(403, "suggested edits come from an editor's link");
            }
            match body::suggest(b) {
                Err(e) => err(400, e),
                Ok(mut x) => {
                    x.note = crate::canon::canonical(&x.note, crate::canon::Kind::Text);
                    match st.catalog.suggest(&author(s), &x) {
                        Ok(id) => json_resp(200, &json!({"ok": true, "suggestion": id})),
                        Err(e) => json_resp(409, &json!({"error": e})),
                    }
                }
            }
        }),
        Route::Write(WriteRoute::Approve) => write(st, req, |s, b| {
            if s.editor() {
                return err(403, "an editor's link comments and suggests edits only");
            }
            match body::approve(b) {
                Err(e) => err(400, e),
                Ok(a) => approve(st, s, &a),
            }
        }),
        Route::MethodNotAllowed => err(405, "method"),
        Route::NotFound => err(404, "not found"),
    }
}

fn start_session(st: &State, req: &Request) -> Resp {
    if !origin_ok(st, req) {
        return err(403, "origin");
    }
    let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
        return err(400, "body");
    };
    let token = v.get("token").and_then(Value::as_str).unwrap_or_default();
    let now = now_ms();
    let Some(link) = links::find(&st.cfg.app_data, token, now) else {
        return err(401, "link");
    };
    let who = login(st, req);
    if let Mode::Tailnet { owner_login, .. } = &st.cfg.mode {
        let allowed = link.login.as_deref().unwrap_or(owner_login);
        if who.as_deref() != Some(allowed) {
            return err(403, "this link is for another tailnet login");
        }
    }
    let sid = crate::wire::nonce();
    let s = Session {
        link: link.id,
        csrf: crate::wire::nonce(),
        expires_ms: (now + SESSION_TTL_MS).min(link.expires_ms),
        login: who,
        role: link.role.clone(),
    };
    let Ok(mut g) = st.sessions.lock() else {
        return err(500, "sessions");
    };
    g.retain(|_, s| s.expires_ms > now);
    if g.len() >= 256 {
        return err(429, "too many sessions");
    }
    let max_age = (s.expires_ms - now) / 1000;
    g.insert(sid.clone(), s);
    drop(g);
    let secure = if matches!(st.cfg.mode, Mode::Tailnet { .. }) {
        "; Secure"
    } else {
        ""
    };
    let mut r = json_resp(200, &json!({"ok": true}));
    r.headers.push((
        "Set-Cookie",
        format!("{COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}"),
    ));
    r
}

fn write(st: &State, req: &Request, f: impl FnOnce(&Session, &Value) -> Resp) -> Resp {
    let Some(s) = session(st, req) else {
        return err(401, "session");
    };
    if !origin_ok(st, req) {
        return err(403, "origin");
    }
    let csrf = req.header("x-csrf").unwrap_or_default();
    let eq = csrf.len() == s.csrf.len()
        && csrf
            .bytes()
            .zip(s.csrf.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0;
    if !eq {
        return err(403, "csrf");
    }
    if req
        .header("content-type")
        .is_none_or(|c| !c.starts_with("application/json"))
    {
        return err(400, "json only");
    }
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err(400, "body");
    };
    f(&s, &body)
}

fn review(st: &State, s: &Session, body: &Value) -> Resp {
    let cut = body.get("cut").and_then(Value::as_str).unwrap_or_default();
    let text = body.get("text").and_then(Value::as_str).unwrap_or_default();
    let verdict = body
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let export_ref = body.get("export_ref").and_then(Value::as_str);
    if !valid_id(cut)
        || !matches!(verdict, "keep" | "change" | "drop")
        || text.chars().count() > 2000
    {
        return err(400, "review");
    }
    if export_ref.is_some_and(|e| !valid_id(e)) {
        return err(400, "export_ref");
    }
    // stored canonical: Tag, zero-width and bidi characters never reach the project
    // raw (R-INJ-6)
    let text = crate::canon::canonical(text, crate::canon::Kind::Text);
    match st
        .catalog
        .add_review(&author(s), cut, &text, verdict, export_ref)
    {
        Ok(id) => json_resp(200, &json!({"ok": true, "review": id})),
        Err(e) => json_resp(409, &json!({"error": e})),
    }
}

/// R-REV-4's three conditions, then the catalog acts on the decision.
fn fix(st: &State, s: &Session, body: &Value) -> Resp {
    let cut = body.get("cut").and_then(Value::as_str).unwrap_or_default();
    let note = body.get("note").and_then(Value::as_str).unwrap_or_default();
    if !valid_id(cut) || note.chars().count() > 2000 {
        return err(400, "fix");
    }
    let Ok(_g) = st.fix_lock.lock() else {
        return err(500, "fix");
    };
    let mut reasons = Vec::new();
    // the Owner's settings as they are now, not as they were at start (a switch back to
    // loopback or an allowance turned off applies at once)
    let live = crate::settings::OwnerSettings::load(&st.cfg.app_data);
    let still_tailnet = match (&st.cfg.mode, &live.review_tailnet) {
        (Mode::Tailnet { host, owner_login }, Some(t)) => {
            t.host == *host && t.owner_login == *owner_login
        }
        _ => false,
    };
    if matches!(st.cfg.mode, Mode::Tailnet { .. }) && !still_tailnet {
        reasons.push("tailnet mode was turned off (restart the server)".to_owned());
    }
    let allowance = live.remote_fix.map(|r| Allowance {
        max_usd_per_run: r.max_usd_per_run,
        max_runs_per_day: r.max_runs_per_day,
    });
    let owner = match &st.cfg.mode {
        Mode::Tailnet { owner_login, .. } => Some(owner_login.as_str()),
        Mode::Loopback => {
            reasons.push("the link is not in tailnet mode".to_owned());
            None
        }
    };
    if owner.is_some() && s.login.as_deref() != owner {
        reasons.push("the requester is not the device owner".to_owned());
    }
    let day = now_ms() / 86_400_000;
    let ledger = st.cfg.app_data.join("remote-fix.jsonl");
    // an unreadable ledger counts as used up (fail closed)
    let used = match std::fs::read_to_string(&ledger) {
        Ok(t) => t
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["day"].as_i64() == Some(day) && v["decision"] == "start")
            .count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => usize::MAX,
    };
    let budget = match (&allowance, &st.cfg.allowance) {
        // both the live setting and the one the server started with must allow it
        (Some(a), Some(_)) => Some(a),
        _ => None,
    };
    let budget = match budget {
        None => {
            reasons.push("no remote-fix allowance is set".to_owned());
            0.0
        }
        Some(a) if used >= a.max_runs_per_day as usize => {
            reasons.push("today's remote-fix runs are used up".to_owned());
            0.0
        }
        Some(a) if a.max_usd_per_run <= 0.0 => {
            reasons.push("the allowance has no budget per run".to_owned());
            0.0
        }
        Some(a) => a.max_usd_per_run,
    };
    let decision = if reasons.is_empty() {
        FixDecision::Start { budget_usd: budget }
    } else {
        FixDecision::Queue {
            reasons: reasons.clone(),
        }
    };
    let started = matches!(decision, FixDecision::Start { .. });
    let req = FixRequest {
        cut: cut.to_owned(),
        note: crate::canon::canonical(note, crate::canon::Kind::Text),
        author: author(s),
        decision,
    };
    let out = st.catalog.fix(&req);
    let line = json!({"day": day, "ts": now_ms(), "cut": cut, "author": req.author,
        "decision": if started { "start" } else { "queue" }, "ok": out.is_ok()});
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
    match out {
        Ok(v) => json_resp(
            200,
            &json!({"started": started, "reasons": reasons, "result": v}),
        ),
        Err(e) => json_resp(409, &json!({"started": false, "error": e})),
    }
}

/// "Approve" (decision 0015): R-REV-4's (a) tailnet mode and (b) the Owner's own login,
/// with (c) the Owner's live remote-send allowance and today's count of sends under it
/// (an unreadable ledger counts as used up). Otherwise the approve is queued for a
/// click in the app. Never publishes: Clippy's own approve and submit stay a human's.
fn approve(st: &State, s: &Session, a: &body::ApproveBody) -> Resp {
    let Ok(_g) = st.fix_lock.lock() else {
        return err(500, "approve");
    };
    let mut reasons = Vec::new();
    let live = crate::settings::OwnerSettings::load(&st.cfg.app_data);
    match (&st.cfg.mode, &live.review_tailnet) {
        (Mode::Tailnet { host, owner_login }, Some(t))
            if t.host == *host && t.owner_login == *owner_login =>
        {
            if s.login.as_deref() != Some(owner_login.as_str()) {
                reasons.push("the requester is not the device owner".to_owned());
            }
        }
        (Mode::Tailnet { .. }, _) => {
            reasons.push("tailnet mode was turned off (restart the server)".to_owned());
        }
        (Mode::Loopback, _) => reasons.push("the link is not in tailnet mode".to_owned()),
    }
    let day = now_ms() / 86_400_000;
    let ledger = st.cfg.app_data.join("remote-send.jsonl");
    let used = match std::fs::read_to_string(&ledger) {
        Ok(t) => t
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["day"].as_i64() == Some(day) && v["decision"] == "send")
            .count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => usize::MAX,
    };
    match &live.remote_send {
        None => reasons.push("no remote-approve allowance is set".to_owned()),
        Some(r) if used >= r.max_sends_per_day as usize => {
            reasons.push("today's remote approvals are used up".to_owned());
        }
        Some(_) => {}
    }
    let send = reasons.is_empty();
    let req = ApproveRequest {
        export: a.export.clone(),
        author: author(s),
        decision: if send {
            ApproveDecision::Send
        } else {
            ApproveDecision::Queue {
                reasons: reasons.clone(),
            }
        },
    };
    let out = st.catalog.approve(&req);
    let line = json!({"day": day, "ts": now_ms(), "export": a.export, "author": req.author,
        "decision": if send { "send" } else { "queue" }, "ok": out.is_ok()});
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
    match out {
        Ok(v) => json_resp(200, &json!({"sent": send, "reasons": reasons, "result": v})),
        Err(e) => json_resp(409, &json!({"sent": false, "error": e})),
    }
}

fn file(st: &State, key: &str, rec: &FileRec, req: &Request) -> Resp {
    let Ok(mut f) = open_nofollow(&rec.path) else {
        return err(404, "not found");
    };
    let Ok(meta) = f.metadata() else {
        return err(404, "not found");
    };
    let size = meta.len();
    if size != rec.size {
        return err(409, "the file no longer matches its record");
    }
    #[cfg(unix)]
    let ident = {
        use std::os::unix::fs::MetadataExt;
        // a hard link to some other file of the user's is not an export
        if meta.nlink() > 1 {
            return err(409, "the file has other names (hard links): not served");
        }
        (
            meta.dev(),
            meta.ino(),
            meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
        )
    };
    #[cfg(not(unix))]
    let ident = (0u64, 0u64, 0i64);
    let known = st
        .verified
        .lock()
        .ok()
        .and_then(|g| g.get(key).cloned())
        .is_some_and(|v| v == (rec.path.clone(), size, ident.0, ident.1, ident.2));
    if !known {
        // hash the very descriptor that will be served
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => h.update(&buf[..n]),
                Err(_) => return err(500, "read"),
            }
        }
        let got = crate::wire::hex(&h.finalize());
        if !got.eq_ignore_ascii_case(&rec.sha256) {
            return err(409, "the file no longer matches its record");
        }
        if let Ok(mut g) = st.verified.lock() {
            g.insert(
                key.to_owned(),
                (rec.path.clone(), size, ident.0, ident.1, ident.2),
            );
        }
    }
    let ctype = "video/mp4";
    let head = req.method == "HEAD";
    let mut r = match http::range(req.header("range"), size) {
        RangeAnswer::Unsatisfiable => {
            let mut r = resp(416, "text/plain", Vec::new());
            r.headers.push(("Content-Range", format!("bytes */{size}")));
            return r;
        }
        RangeAnswer::Whole => Resp {
            status: 200,
            headers: vec![("Content-Type", ctype.into())],
            body: Body::File {
                file: f,
                start: 0,
                len: size,
            },
            head_only: head,
        },
        RangeAnswer::Part(a, b) => Resp {
            status: 206,
            headers: vec![
                ("Content-Type", ctype.into()),
                ("Content-Range", format!("bytes {a}-{b}/{size}")),
            ],
            body: Body::File {
                file: f,
                start: a,
                len: b - a + 1,
            },
            head_only: head,
        },
    };
    r.headers.push(("Accept-Ranges", "bytes".into()));
    r
}

/// Rows for a JSON `home` from untyped parts (hosts use it to build [`Catalog::home`]).
pub fn rows<T: Serialize>(v: &[T]) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Array(Vec::new()))
}
