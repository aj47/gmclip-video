//! An in-process holder on a temp socket and app data (never the real home), a fixture
//! project, and an rmcp client that goes through the real handshake.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::client::{self, RunCredential};
use gmclip_mcp::holder::{Control, Holder};
use gmclip_mcp::paths::{self, Env};
use gmclip_mcp::project;
use gmclip_mcp::settings::OwnerSettings;
use gmclip_mcp::wire::Mode;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

pub const SEC: i64 = 705_600_000;

static N: AtomicU64 = AtomicU64::new(0);

/// A temp base (0700) holding app data, the socket dir, a recordings root.
pub struct World {
    pub base: PathBuf,
    pub env: Env,
    pub holder: Arc<Holder>,
    pub rec: PathBuf,
    /// Owner codes the holder would have shown on the requester's terminal.
    pub codes: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Drop for World {
    fn drop(&mut self) {
        self.holder.shutdown.notify_one();
        self.holder.close_all();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Fixture ids.
#[derive(Clone, Debug)]
pub struct Fx {
    pub project: String,
    pub home: PathBuf,
    pub cut: String,
    pub other_cut: String,
    pub framing: String,
    pub screen: String,
    pub parts: Vec<String>,
}

pub fn base() -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let b = std::env::temp_dir().join(format!("gm{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    // 0700 (Windows: an owner-only DACL)
    paths::mkdir_private(&b).unwrap();
    b
}

/// The holder's endpoint for a world at `base`: a socket inside it, or on Windows a pipe
/// of its own (pipes live in `\\.\pipe\`, never in a directory).
pub fn socket_for(base: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        let tag = base.file_name().unwrap().to_string_lossy();
        paths::pipe_name_for(&format!("test-{tag}"))
    }
    #[cfg(not(windows))]
    {
        base.join("run").join("s")
    }
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| v["cut"].as_str().map(str::to_owned))
        .unwrap()
}

/// Builds a project home with a cut of `n` parts as the human, before any holder runs.
pub fn fixture(app_data: &std::path::Path, home: &std::path::Path, n: usize, title: &str) -> Fx {
    let info = project::create_home(app_data, home, "fixture").unwrap();
    let db = project::db_path(app_data, &info.id).unwrap();
    let h = ProjectHandle::open(&db, OpenOptions::default()).unwrap();
    let call = |cmd: &str, args: Value| {
        h.call(CallRequest {
            command: cmd.into(),
            args,
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
        .unwrap()
        .output
    };
    let cut = id(&call("cut_create", json!({"title": title})));
    let other_cut = id(&call("cut_create", json!({"title": "other"})));
    let source = id(&call(
        "source_add",
        json!({"path": "/m/2026-10-04 stream.mkv", "fingerprint": "fp"}),
    ));
    let framing = id(&call(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    ));
    let screen = id(&call(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}),
    ));
    let mut parts: Vec<String> = Vec::new();
    for i in 0..n {
        let t = 100 * SEC + i as i64 * 3 * SEC;
        let mut a =
            json!({"cut": cut, "media": {"source": source}, "src_in": t, "src_out": t + 2 * SEC});
        if let Some(p) = parts.last() {
            a["after"] = json!(p);
        }
        parts.push(id(&call("part_add", a)));
    }
    h.close().unwrap();
    Fx {
        project: info.id,
        home: home.to_owned(),
        cut,
        other_cut,
        framing,
        screen,
        parts,
    }
}

impl World {
    /// A holder serving on a temp socket, with `rec/` as the Owner's read root.
    pub async fn start() -> World {
        Self::start_with(None).await
    }

    /// … planning and rendering with the given hosts.
    pub async fn start_with(
        hosts: Option<(
            Arc<dyn gmclip_core::Planner>,
            Arc<dyn gmclip_core::MediaRender>,
        )>,
    ) -> World {
        Self::start_full(hosts, None).await
    }

    /// … and analysing with `analysis`.
    pub async fn start_full(
        hosts: Option<(
            Arc<dyn gmclip_core::Planner>,
            Arc<dyn gmclip_core::MediaRender>,
        )>,
        analysis: Option<Arc<dyn gmclip_core::AnalysisHost>>,
    ) -> World {
        let base = base();
        let env = Env {
            app_data: base.join("app"),
            socket: socket_for(&base),
        };
        let rec = base.join("rec");
        std::fs::create_dir_all(&rec).unwrap();
        OwnerSettings {
            read_roots: vec![rec.clone()],
            export_dir: Some(base.join("exports")),
            ..Default::default()
        }
        .save(&env.app_data)
        .unwrap();
        let listener = gmclip_mcp::server::bind(&env, &[]).await.unwrap();
        let mut holder = match hosts {
            Some((p, m)) => Holder::with_hosts(env.clone(), p, m),
            None => Holder::new(env.clone()),
        };
        if let Some(a) = analysis {
            holder = holder.with_analysis(a);
        }
        let holder = Arc::new(holder);
        let codes: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&codes);
        holder.set_code_sink(gmclip_mcp::holder::CodeSink(Box::new(move |_, c| {
            sink.lock().unwrap().push(c.to_owned())
        })));
        let h = Arc::clone(&holder);
        tokio::spawn(async move {
            gmclip_mcp::server::serve(h, listener, Duration::from_secs(3600)).await
        });
        World {
            base,
            env,
            holder,
            rec,
            codes,
        }
    }

    pub fn fixture(&self, name: &str, n: usize, title: &str) -> Fx {
        fixture(&self.env.app_data, &self.rec.join(name), n, title)
    }

    /// An MCP client through the real handshake (as `name`, optionally a launched run).
    pub async fn client(&self, name: &str, run: Option<&RunCredential>) -> Client {
        self.client_v(name, run, ProtocolVersion::V_2025_11_25)
            .await
    }

    /// An MCP client in an agent session (`GMCLIP_SESSION`, decision 0050).
    pub async fn client_session(
        &self,
        name: &str,
        run: Option<&RunCredential>,
        session: &str,
    ) -> Client {
        let me = paths::our_exe();
        let (stream, welcome) = client::connect_with(
            &self.env,
            Mode::Mcp,
            false,
            run,
            Some(session),
            me.as_deref(),
        )
        .await
        .unwrap();
        let cfg = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(name, "1"),
        );
        let svc = cfg.serve(stream).await.unwrap();
        Client {
            svc,
            identity: welcome.identity,
        }
    }

    pub async fn client_v(
        &self,
        name: &str,
        run: Option<&RunCredential>,
        v: ProtocolVersion,
    ) -> Client {
        let me = paths::our_exe();
        let (stream, welcome) = client::connect_as(&self.env, Mode::Mcp, false, run, me.as_deref())
            .await
            .unwrap();
        let mut cfg = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(name, "1"),
        );
        cfg.protocol_version = v;
        let svc = cfg.serve(stream).await.unwrap();
        Client {
            svc,
            identity: welcome.identity,
        }
    }

    /// Begins an Owner command and returns the code (from the sink: the socket never
    /// carries it).
    pub async fn owner_code(&self, command: &str, args: &Value) -> String {
        let r = self
            .control(&Control::OwnerBegin {
                command: command.into(),
                args: args.clone(),
            })
            .await;
        assert_eq!(r["ok"], true, "{r}");
        assert!(
            r["result"].get("code").is_none(),
            "the code never travels over the socket"
        );
        self.codes.lock().unwrap().last().cloned().unwrap()
    }

    pub async fn control(&self, req: &Control) -> Value {
        let me = paths::our_exe();
        let (mut s, _) = client::connect_as(&self.env, Mode::Control, false, None, me.as_deref())
            .await
            .unwrap();
        client::request(&mut s, req).await.unwrap()
    }
}

pub struct Client {
    pub svc: RunningService<RoleClient, ClientConfig>,
    pub identity: Option<String>,
}

impl Client {
    /// Calls a tool; `Ok(structured)` or `Err(error json)`.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, Value> {
        let map = args.as_object().cloned().unwrap_or_default();
        let r = self
            .svc
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(map))
            .await
            .unwrap();
        assert!(
            r.structured_content.is_none(),
            "the result is sent once, as text"
        );
        let v: Value = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .and_then(|t| serde_json::from_str(&t.text).ok())
            .unwrap_or(Value::Null);
        if r.is_error == Some(true) {
            Err(v["error"].clone())
        } else {
            Ok(v)
        }
    }

    /// Calls a tool and returns its whole text block, error or not (the `display` and
    /// `help` an error carries).
    pub async fn call_raw(&self, tool: &str, args: Value) -> Value {
        let map = args.as_object().cloned().unwrap_or_default();
        let r = self
            .svc
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(map))
            .await
            .unwrap();
        r.content
            .first()
            .and_then(|c| c.as_text())
            .and_then(|t| serde_json::from_str(&t.text).ok())
            .unwrap_or(Value::Null)
    }

    pub async fn ok(&self, tool: &str, args: Value) -> Value {
        match self.call(tool, args.clone()).await {
            Ok(v) => v,
            Err(e) => panic!("{tool} {args}: {e}"),
        }
    }

    pub async fn tools(&self) -> Vec<String> {
        self.svc
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }
}

/// A raw connection to the holder, below the client library (handshake tests).
#[cfg(unix)]
pub async fn raw_connect(socket: &std::path::Path) -> tokio::net::UnixStream {
    tokio::net::UnixStream::connect(socket).await.unwrap()
}

/// A raw connection to the holder's pipe; waits while every instance is busy.
#[cfg(windows)]
pub async fn raw_connect(
    socket: &std::path::Path,
) -> tokio::net::windows::named_pipe::NamedPipeClient {
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..200 {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(socket) {
            Ok(c) => return c,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("open {}: {e}", socket.display()),
        }
    }
    panic!("{} stayed busy", socket.display())
}

/// A directory symlink, or `false` where the OS refuses one (Windows without Developer
/// Mode or the symlink privilege).
pub fn symlink_dir(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
}

/// A file symlink, or `false` where the OS refuses one (as [`symlink_dir`]).
pub fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

/// The error code of a failed call.
pub fn code(e: &Value) -> &str {
    e["code"].as_str().unwrap_or("?")
}
