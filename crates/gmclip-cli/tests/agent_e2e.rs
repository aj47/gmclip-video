//! End to end through the real binaries (A19–A22, A26, A28): an rmcp client runs
//! `gmclip mcp` over stdio; the shim starts `gmclip serve --headless`, verifies it, and
//! proxies. The agent opens a fixture project, edits a cut, reads it back and undoes its
//! run. Every test runs with its own HOME and TMPDIR — never the real ones (Windows: its
//! own USERPROFILE, LOCALAPPDATA and holder pipe, `GMCLIP_PIPE_TAG`).

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::project;
use gmclip_mcp::settings::OwnerSettings;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};

const GMCLIP: &str = env!("CARGO_BIN_EXE_gmclip");
const SEC: i64 = 705_600_000;

static N: AtomicU64 = AtomicU64::new(0);

/// HOME, TMPDIR and a recordings root under one short temp base.
struct Sandbox {
    base: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    rec: PathBuf,
    /// This sandbox's holder pipe (Windows; Unix isolates by TMPDIR).
    #[cfg_attr(not(windows), allow(dead_code))]
    tag: String,
}

impl Sandbox {
    fn new() -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("ge{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mk = |p: &Path| {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            b.mode(0o700);
            b.create(p).unwrap()
        };
        let (home, tmp, rec) = (base.join("h"), base.join("t"), base.join("rec"));
        for d in [&base, &home, &tmp, &rec] {
            mk(d);
        }
        Sandbox {
            base,
            home,
            tmp,
            rec,
            tag: format!("e2e{}x{n}", std::process::id()),
        }
    }

    fn app_data(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home.join("Library/Application Support/gmclip")
        } else if cfg!(windows) {
            self.home.join("AppData").join("Local").join("gmclip")
        } else {
            self.home.join(".local/share/gmclip")
        }
    }

    fn cmd(&self, args: &[&str]) -> StdCommand {
        let mut c = StdCommand::new(GMCLIP);
        c.args(args)
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", &self.tmp)
            .env("GMCLIP_HOLDER_IDLE_S", "60")
            .env_remove("GMCLIP_RUN");
        #[cfg(windows)]
        c.env("USERPROFILE", &self.home)
            .env("LOCALAPPDATA", self.home.join("AppData").join("Local"))
            .env("TEMP", &self.tmp)
            .env("TMP", &self.tmp)
            .env("GMCLIP_PIPE_TAG", &self.tag);
        c
    }

    /// The holder's pipe for this sandbox.
    #[cfg(windows)]
    fn pipe(&self) -> PathBuf {
        let sid = gmclip_mcp::paths::our_sid().unwrap();
        gmclip_mcp::paths::pipe_name_for(&format!("{sid}-{}", self.tag))
    }

    #[cfg(windows)]
    fn pipe_present(&self) -> bool {
        gmclip_mcp::paths::Env {
            app_data: self.app_data(),
            socket: self.pipe(),
        }
        .socket_present()
    }

    #[cfg(unix)]
    fn socket_dir(&self) -> PathBuf {
        let uid = std::fs::metadata(&self.home)
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .unwrap();
        if cfg!(target_os = "macos") {
            self.tmp.join(format!("gmclip-{uid}"))
        } else {
            self.tmp.join("gmclip")
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.cmd(&["stop"]).output();
        std::thread::sleep(Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A project home under the recordings root, with a 3-part cut, built as the human.
fn fixture(sb: &Sandbox) -> (PathBuf, String, Vec<String>) {
    let home = sb.rec.join("show.gmclip");
    let info = project::create_home(&sb.app_data(), &home, "show").unwrap();
    let h = ProjectHandle::open(
        &project::db_path(&sb.app_data(), &info.id).unwrap(),
        OpenOptions::default(),
    )
    .unwrap();
    let call = |cmd: &str, a: Value| {
        h.call(CallRequest {
            command: cmd.into(),
            args: a,
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
        .unwrap()
        .output
    };
    let cut = call("cut_create", json!({"title": "Banana"}))["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let src = call(
        "source_add",
        json!({"path": "/m/s.mkv", "fingerprint": "fp"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut parts: Vec<String> = Vec::new();
    for i in 0..3i64 {
        let mut a = json!({"cut": cut, "media": {"source": src}, "src_in": 100 * SEC + i * 3 * SEC, "src_out": 102 * SEC + i * 3 * SEC});
        if let Some(p) = parts.last() {
            a["after"] = json!(p);
        }
        parts.push(call("part_add", a)["id"].as_str().unwrap().to_owned());
    }
    h.close().unwrap();
    OwnerSettings {
        read_roots: vec![sb.rec.clone()],
        ..Default::default()
    }
    .save(&sb.app_data())
    .unwrap();
    (home, cut, parts)
}

async fn call_tool(
    c: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>,
    tool: &str,
    args: Value,
) -> Result<Value, Value> {
    let r = c
        .call_tool(
            CallToolRequestParams::new(tool.to_owned())
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_edits_a_cut_over_stdio_and_undoes_its_run() {
    let sb = Sandbox::new();
    let (home, cut, parts) = fixture(&sb);
    let cmd = tokio::process::Command::from(sb.cmd(&["mcp"]));
    let t0 = Instant::now();
    let cfg = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "2.1.300"),
    );
    let c = cfg
        .serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap();
    let tools = c.list_all_tools().await.unwrap();
    let cold = t0.elapsed();
    eprintln!("cold start (shim + holder + initialize + tools/list): {cold:?}");
    assert_eq!(tools.len(), 20);
    assert_eq!(tools[0].name, "project_overview");

    let opened = call_tool(
        &c,
        "command_run",
        json!({"command": "project_open", "args": {"path": home.to_str().unwrap()}}),
    )
    .await
    .unwrap();
    assert_eq!(opened["name"], "show");
    let ov = call_tool(&c, "project_overview", json!({})).await.unwrap();
    let as_of = ov["as_of"].as_u64().unwrap();
    let got = call_tool(&c, "cut_get", json!({"cut": cut, "projection": "parts"}))
        .await
        .unwrap();
    assert_eq!(got["parts"].as_array().unwrap().len(), 3);

    let edit = call_tool(
        &c,
        "cut_edit",
        json!({"cut": cut, "base": {"as_of": as_of}, "ops": [
            {"op": "part_trim", "part": parts[1], "src_out": 104 * SEC + SEC / 2},
            {"op": "part_delete", "part": parts[2]},
            {"op": "cut_set", "title": "Talking banana"},
        ]}),
    )
    .await
    .unwrap();
    let rev = edit["committed"].as_u64().unwrap();
    let back = call_tool(&c, "cut_get", json!({"cut": cut, "projection": "parts"}))
        .await
        .unwrap();
    assert_eq!(back["parts"].as_array().unwrap().len(), 2);
    assert_eq!(back["parts"][1]["src_out"], json!(104 * SEC + SEC / 2));
    assert_eq!(back["cut"]["title"]["text"], "Talking banana");
    assert_eq!(back["as_of"], json!(rev));

    let ended = call_tool(
        &c,
        "command_run",
        json!({"command": "run_end", "args": {}, "base": {"as_of": rev}}),
    )
    .await
    .unwrap();
    let run = ended["output"]["run"].as_str().unwrap().to_owned();
    call_tool(
        &c,
        "command_run",
        json!({"command": "run_undo", "args": {"run": run}, "base": {"as_of": rev}}),
    )
    .await
    .unwrap();
    let undone = call_tool(&c, "cut_get", json!({"cut": cut, "projection": "parts"}))
        .await
        .unwrap();
    assert_eq!(undone["parts"].as_array().unwrap().len(), 3, "{undone}");
    assert_eq!(undone["parts"][1]["src_out"], json!(105 * SEC));
    assert_eq!(undone["cut"]["title"]["text"], "Banana");
    c.cancel().await.unwrap();

    // the holder is still up: status, the connection log, a second launch forwards
    let st = sb.cmd(&["status"]).output().unwrap();
    let st: Value = serde_json::from_slice(&st.stdout).unwrap();
    assert_eq!(st["running"], true, "{st}");
    assert_eq!(st["holder"]["projects"][0]["name"], "show");
    let conns: Value =
        serde_json::from_slice(&sb.cmd(&["connections"]).output().unwrap().stdout).unwrap();
    assert!(
        conns["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["identity"] == "local:claude-code"),
        "{conns}"
    );
    let second = sb.cmd(&["serve", "--headless"]).output().unwrap();
    assert!(second.status.success());
    let v: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert!(v["forwarded"]["ok"].as_bool().unwrap(), "{v}");
    #[cfg(unix)]
    {
        // the socket directory is 0700 and ours; the socket file 0600
        let dir = sb.socket_dir();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.join("s"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        // stop: the socket goes
        assert!(sb.cmd(&["stop"]).output().unwrap().status.success());
        let t = Instant::now();
        while dir.join("s").exists() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!dir.join("s").exists());
    }
    #[cfg(windows)]
    {
        // the holder serves this sandbox's own pipe (owner-only DACL: paths tests);
        // stop: the pipe goes
        assert!(sb.pipe_present(), "{}", sb.pipe().display());
        assert!(sb.cmd(&["stop"]).output().unwrap().status.success());
        let t = Instant::now();
        while sb.pipe_present() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!sb.pipe_present());
    }
}

#[test]
fn owner_commands_refuse_without_a_terminal() {
    let sb = Sandbox::new();
    let root = sb.rec.to_string_lossy().into_owned();
    for args in [
        vec!["owner", "roots-add", root.as_str()],
        vec!["owner", "connect", "claude-code"],
    ] {
        let o = sb
            .cmd(&args)
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(77), "{args:?}");
        let e: Value = serde_json::from_slice(&o.stderr).unwrap();
        assert_eq!(e["code"], "Unauthorized");
        assert!(
            e["reason"]
                .as_str()
                .unwrap()
                .contains("interactive terminal")
        );
    }
    // and nothing was written to the client configs
    assert!(!sb.home.join(".claude.json").exists());
    assert!(!sb.home.join(".claude").exists());
}

/// A squatter on the holder's pipe (this test process: the same user, not gmclip): the
/// shim checks the server's image, refuses it and sends nothing (not even a value
/// derived from the run credential).
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fake_server_is_refused_and_learns_nothing() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let sb = Sandbox::new();
    let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .create(sb.pipe())
        .unwrap();
    let squat = tokio::spawn(async move {
        server.connect().await.unwrap();
        server
            .write_all(b"{\"gmclip\":1,\"server_nonce\":\"00\"}\n")
            .await
            .unwrap();
        let mut got = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), server.read_to_end(&mut got)).await;
        got
    });
    let mut c = sb.cmd(&["mcp"]);
    c.env("GMCLIP_RUN", format!("01JRUN:{}", "ab".repeat(32)))
        .stdin(std::process::Stdio::null());
    let o = tokio::process::Command::from(c).output().await.unwrap();
    let got = squat.await.unwrap();
    assert_eq!(
        o.status.code(),
        Some(77),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("not gmclip"));
    assert_eq!(got, b"", "the client sent nothing");
}

/// A squatter on the socket path: the shim refuses it and sends nothing (not even a
/// value derived from the run credential).
#[cfg(unix)]
#[test]
fn a_fake_server_is_refused_and_learns_nothing() {
    let sb = Sandbox::new();
    let dir = sb.socket_dir();
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let sock = dir.join("s");
    let got = sb.base.join("got");
    let script = format!(
        "import socket,time\ns=socket.socket(socket.AF_UNIX)\ns.bind({:?})\ns.listen(1)\nc,_=s.accept()\nc.sendall(b'{{\"gmclip\":1,\"server_nonce\":\"00\"}}\\n')\nc.settimeout(2)\nn=b''\ntry:\n  while True:\n    d=c.recv(4096)\n    if not d: break\n    n+=d\nexcept Exception: pass\nopen({:?},'wb').write(n)\n",
        sock.to_str().unwrap(),
        got.to_str().unwrap()
    );
    let mut py = StdCommand::new("/usr/bin/python3")
        .args(["-c", &script])
        .spawn()
        .unwrap();
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let o = sb
        .cmd(&["mcp"])
        .env("GMCLIP_RUN", format!("01JRUN:{}", "ab".repeat(32)))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let _ = py.wait();
    assert_eq!(
        o.status.code(),
        Some(77),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("not gmclip"));
    assert_eq!(std::fs::read(&got).unwrap(), b"", "the client sent nothing");
}

/// (Windows has no socket directory: the pipe carries its own owner-only DACL.)
#[cfg(unix)]
#[test]
fn a_socket_directory_with_the_wrong_mode_is_refused() {
    let sb = Sandbox::new();
    let dir = sb.socket_dir();
    std::fs::DirBuilder::new().mode(0o755).create(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // a client refuses to connect
    let o = sb
        .cmd(&["mcp"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(77),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("0700"));
    // and a holder refuses to bind there
    let o = sb
        .cmd(&["serve", "--headless", "--idle-s", "1"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("0700"));
}

/// The whole M1 export path through the binaries: lane A's model planner, lane H's
/// export pipeline on gmclip-media's demuxers, B-15 verify — started by an agent over
/// stdio, waited on with `job_wait`. Dev machines only (ffmpeg makes the source).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_exports_a_cut_and_it_verifies() {
    let ffmpeg = "/opt/homebrew/bin/ffmpeg";
    if !Path::new(ffmpeg).exists() {
        eprintln!("skipped: no {ffmpeg}");
        return;
    }
    let sb = Sandbox::new();
    let media = sb.rec.join("media");
    std::fs::create_dir_all(&media).unwrap();
    let src = media.join("src.mp4");
    let ok = StdCommand::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "30",
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-b:a",
            "128k",
            "-shortest",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(ok.success());
    // the project, as the human (import would do this): a probed source, a 9:16 framing
    // with a screen layer, one 1.5 s part
    let home = sb.rec.join("show.gmclip");
    let info = project::create_home(&sb.app_data(), &home, "show").unwrap();
    let h = ProjectHandle::open(
        &project::db_path(&sb.app_data(), &info.id).unwrap(),
        OpenOptions::default(),
    )
    .unwrap();
    let call = |cmd: &str, a: Value| {
        h.call(CallRequest {
            command: cmd.into(),
            args: a,
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
        .unwrap()
        .output
    };
    let source = call(
        "source_add",
        json!({"path": src.display().to_string(), "fingerprint": "", "probe": {
            "duration": 2 * SEC,
            "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 640, "height": 360, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 1.0}],
            "audio_tracks": [{"index": 1, "codec": "aac", "rate": 48000, "layout": "stereo", "bitrate": 128000, "speech_ratio": null}]}}),
    )["id"]
    .as_str()
    .unwrap()
    .to_owned();
    let cut = call("cut_create", json!({"title": "Export test"}))["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let framing = call(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.3416, "w": 1}}),
    );
    call(
        "part_add",
        json!({"cut": cut, "media": {"source": source}, "src_in": SEC / 5, "src_out": 17 * SEC / 10}),
    );
    // heard captions need the caption builder (not in the M1 planner): off
    call("caption_set", json!({"cut": cut, "enabled": false}));
    h.close().unwrap();
    OwnerSettings {
        read_roots: vec![sb.rec.clone()],
        ..Default::default()
    }
    .save(&sb.app_data())
    .unwrap();

    let mut cmd = tokio::process::Command::new(GMCLIP);
    cmd.arg("mcp")
        .env("HOME", &sb.home)
        .env("TMPDIR", &sb.tmp)
        .env("XDG_RUNTIME_DIR", &sb.tmp)
        .env("GMCLIP_HOLDER_IDLE_S", "60")
        .env_remove("GMCLIP_RUN");
    let c = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "2.1.300"),
    )
    .serve(TokioChildProcess::new(cmd).unwrap())
    .await
    .unwrap();
    call_tool(
        &c,
        "command_run",
        json!({"command": "project_open", "args": {"path": home.to_str().unwrap()}}),
    )
    .await
    .unwrap();
    let as_of = call_tool(&c, "cut_get", json!({"cut": cut})).await.unwrap()["as_of"]
        .as_u64()
        .unwrap();
    let lint = call_tool(&c, "lint", json!({"cut": cut})).await.unwrap();
    assert_eq!(lint["errors"], 0, "{lint}");
    let job = call_tool(&c, "export", json!({"cut": cut, "base": {"as_of": as_of}}))
        .await
        .unwrap();
    let id = job["job"].as_str().unwrap().to_owned();
    let done = call_tool(
        &c,
        "command_run",
        json!({"command": "job_wait", "args": {"job": id, "timeout_s": 120}}),
    )
    .await
    .unwrap();
    assert_eq!(done["state"], "done", "{done}");
    let r = &done["result"];
    assert_eq!(r["frames"], 45);
    assert!(
        Path::new(r["out"].as_str().unwrap()).starts_with(sb.home.join("Movies/gmclip/Exports")),
        "{r}"
    );
    assert_eq!(r["verify"]["passed"], true, "{r}");
    let v = call_tool(
        &c,
        "verify",
        json!({"cut": cut, "kind": "export", "export": id}),
    )
    .await
    .unwrap();
    assert_eq!(v["report"]["passed"], true);
    // joins need the speech model, which this HOME does not have: said, not faked
    let j = call_tool(
        &c,
        "verify",
        json!({"cut": cut, "kind": "joins", "export": id}),
    )
    .await
    .unwrap_err();
    assert_eq!(j["code"], "Unsupported", "{j}");
    // the source was analysed in the background when the project opened (lane D's
    // DSP on gmclip-media's AAC decode); its silences and energy read back
    let mut sil = Value::Null;
    for _ in 0..30 {
        sil = call_tool(
            &c,
            "analysis_get",
            json!({"source": source, "kind": "energy", "from_s": 0.0, "to_s": 2.0}),
        )
        .await
        .unwrap();
        match sil["job"].as_str() {
            Some(job) => {
                call_tool(
                    &c,
                    "command_run",
                    json!({"command": "job_wait", "args": {"job": job, "timeout_s": 60}}),
                )
                .await
                .unwrap();
            }
            None => break,
        }
    }
    let pts = sil["points"].as_array().unwrap_or_else(|| panic!("{sil}"));
    assert!(!pts.is_empty());
    // a 440 Hz tone at full scale: loud throughout
    assert!(pts.iter().all(|p| p[1].as_f64().unwrap() > -30.0), "{sil}");
    let lo = call_tool(
        &c,
        "analysis_get",
        json!({"source": source, "kind": "loudness", "from_s": 0.2, "to_s": 1.7}),
    )
    .await
    .unwrap();
    assert!(lo["integrated_lufs"].as_f64().unwrap() < 0.0, "{lo}");
    // no transcript: the failed ASR is reported, not retried forever
    let t = call_tool(&c, "transcript_read", json!({"source": source}))
        .await
        .unwrap_err();
    assert_eq!(t["code"], "Unsupported", "{t}");
    c.cancel().await.unwrap();
}
