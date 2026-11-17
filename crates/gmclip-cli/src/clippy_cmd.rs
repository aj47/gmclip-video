//! `gmclip clippy pair|send` — minimal Send to Clippy (SPEC §5.14.2; SR R-CLIP-1).
//!
//! - **Human only.** Both commands need an interactive terminal, and `send` asks the
//!   human to type the hook id back before anything leaves the machine. No MCP tool,
//!   `command_run` command or socket request reaches them (an agent hand-off would be
//!   `NeedsApproval` once one exists, M4).
//! - **Paired and authenticated.** `pair` makes a 256-bit secret, kept in app data
//!   (0600) and shown once on the terminal for the human to enter in Clippy; `send`
//!   carries it as `Authorization: Bearer …`. Without a pairing nothing is sent.
//! - **Loopback only.** The endpoint must be `http://127.0.0.1:<port>` or
//!   `http://[::1]:<port>`; the `Host` header names it exactly so Clippy can check it.
//! - **Never a blocked export.** An export whose record lists `blocked` reasons, or whose
//!   file no longer matches its record, is refused (§10.7.1).
//!
//! It writes the sidecar `<stem>.clippy.json` beside the export — `{hook_id (the
//! moment's origin_ref), title, text_hook, excerpt, render_sig, duration, framing}` —
//! and posts `{hook_id, path, quality, gmclip: <sidecar>}` to Clippy's local
//! registration `/api/edit_station/final`.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use gmclip_core::{OpenOptions, ProjectHandle};
use gmclip_media::export_record::ExportRecord;
use gmclip_model::entities::{Cut, Framing, Moment};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::Output;
use crate::error::{CliError, Code};
use crate::socket;

/// The pairing file in app data.
pub const PAIRING_FILE: &str = "clippy-pairing.json";

#[derive(Debug, Subcommand)]
pub enum ClippyCmd {
    /// Make the pairing secret (shown once here; enter it in Clippy).
    Pair,
    /// Hand an export to Clippy for scheduling (you confirm by typing the hook id).
    Send {
        /// The project database (`<id>.sqlite`).
        #[arg(long)]
        project: PathBuf,
        /// The cut: its id, or its moment's hook id.
        #[arg(long)]
        cut: String,
        /// The export MP4 (its `<stem>.export.json` record beside it).
        #[arg(long)]
        export: PathBuf,
        /// Clippy's local address.
        #[arg(long, default_value = "http://127.0.0.1:8878")]
        endpoint: String,
    },
}

fn bad(e: impl std::fmt::Display) -> CliError {
    CliError::new(Code::Invalid)
        .step("clippy")
        .reason(e.to_string())
}

fn terminal() -> Result<(), CliError> {
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(CliError::new(Code::Unauthorized)
            .step("clippy")
            .reason("Send to Clippy is a human action at an interactive terminal"))
    }
}

/// The paired secret, if any.
pub fn secret(app: &Path) -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(app.join(PAIRING_FILE)).ok()?).ok()?;
    v.get("secret")
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
}

fn pair(app: &Path) -> Result<String, CliError> {
    let s = gmclip_mcp::wire::nonce();
    gmclip_mcp::paths::mkdir_private(app).map_err(|e| CliError::io("app data", &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(app, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CliError::io("app data", &e))?;
    }
    let f = app.join(PAIRING_FILE);
    let tmp = app.join(format!("{PAIRING_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut t = o.open(&tmp).map_err(|e| CliError::io("pairing", &e))?;
    t.write_all(json!({"secret": s}).to_string().as_bytes())
        .map_err(|e| CliError::io("pairing", &e))?;
    drop(t);
    std::fs::rename(&tmp, &f).map_err(|e| CliError::io("pairing", &e))?;
    Ok(s)
}

/// `http://127.0.0.1:<port>` / `http://[::1]:<port>` → the socket address and the
/// `Host` value; anything else is refused.
pub fn loopback(endpoint: &str) -> Result<(SocketAddr, String), CliError> {
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| {
            bad("the Clippy endpoint must be http://127.0.0.1:<port> or http://[::1]:<port>")
        })?
        .trim_end_matches('/');
    let addr: SocketAddr = rest
        .parse()
        .map_err(|_| bad("the Clippy endpoint must be a loopback address with a port"))?;
    if !addr.ip().is_loopback() {
        return Err(bad("Clippy is reached on this machine only (loopback)"));
    }
    Ok((addr, rest.to_owned()))
}

/// What the hand-off says about one cut and its export.
pub fn sidecar(
    doc: &gmclip_model::Doc,
    cut_key: &str,
    rec: &ExportRecord,
) -> Result<Value, CliError> {
    let found: Vec<&Cut> = doc
        .iter::<Cut>()
        .filter(|c| {
            c.id.0.to_string().eq_ignore_ascii_case(cut_key)
                || c.moment
                    .and_then(|m| doc.get::<Moment>(m))
                    .is_some_and(|m| m.origin_ref.as_deref() == Some(cut_key))
        })
        .collect();
    let cut = match found.as_slice() {
        [c] => *c,
        [] => return Err(bad("no such cut in the project")),
        _ => return Err(bad("several cuts have that hook; name the cut by its id")),
    };
    let moment = cut
        .moment
        .and_then(|m| doc.get::<Moment>(m))
        .ok_or_else(|| bad("the cut has no moment, so no Clippy hook to hand it to"))?;
    let hook = moment
        .origin_ref
        .clone()
        .ok_or_else(|| bad("the cut's moment has no Clippy hook id (origin_ref)"))?;
    let d = &rec.duration_expected;
    let duration = if d.fps_num > 0 {
        d.frames as f64 * d.fps_den as f64 / d.fps_num as f64
    } else {
        0.0
    };
    // the export must be this cut's length (its parts end to end, within half a second)
    let parts: f64 = doc
        .children::<gmclip_model::entities::Part>(cut.id.entity())
        .filter_map(|p| doc.get(p))
        .map(|p| gmclip_model::clock::part_duration(p).0 as f64 / 705_600_000.0)
        .sum();
    if (parts - duration).abs() > 0.5 {
        return Err(bad(format!(
            "the export is {duration:.1} s but the cut's parts add up to {parts:.1} s: not this cut's export"
        )));
    }
    let framing = rec.framing.clone().or_else(|| {
        doc.children::<Framing>(cut.id.entity())
            .filter_map(|f| doc.get(f))
            .next()
            .map(|f| f.name.clone())
    });
    Ok(json!({
        "hook_id": hook,
        "title": cut.title,
        "text_hook": moment.text_hook,
        "excerpt": moment.excerpt.text,
        "render_sig": rec.render_sig,
        "duration": duration,
        "framing": framing,
    }))
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Checks the export against its record; refuses a blocked one. The export and its
/// record are regular files (no symlinks), and the record names this file.
pub fn checked_record(export: &Path) -> Result<ExportRecord, CliError> {
    let regular = |p: &Path, what: &str| -> Result<(), CliError> {
        let m = std::fs::symlink_metadata(p).map_err(|e| CliError::io("export", &e))?;
        if !m.is_file() {
            return Err(bad(format!("the {what} is not a regular file")));
        }
        Ok(())
    };
    regular(export, "export")?;
    regular(&ExportRecord::path_for(export), "export record")?;
    let rec =
        ExportRecord::read(export).map_err(|_| bad("the export has no export record beside it"))?;
    if export.file_name().and_then(|n| n.to_str()) != Some(rec.file.as_str()) {
        return Err(bad("the export record names another file"));
    }
    if !rec.blocked.is_empty() {
        return Err(bad("a blocked export is never handed to Clippy"));
    }
    let sha = sha256_file(export).map_err(|e| CliError::io("export", &e))?;
    if !sha.eq_ignore_ascii_case(&rec.sha256) {
        return Err(bad("the export does not match its record (re-export it)"));
    }
    Ok(rec)
}

/// Posts the hand-off (the human has already confirmed). Returns Clippy's answer.
pub fn post(endpoint: &str, secret: &str, body: &Value) -> Result<Value, CliError> {
    let (addr, host) = loopback(endpoint)?;
    let payload = body.to_string();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| CliError::io("connect to Clippy", &e))?;
    let req = format!(
        "POST /api/edit_station/final HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    s.write_all(req.as_bytes())
        .map_err(|e| CliError::io("send to Clippy", &e))?;
    // one overall deadline for the answer, however slowly it trickles in
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(bad("Clippy did not answer in time"));
        }
        s.set_read_timeout(Some(left)).ok();
        let n = s
            .read(&mut chunk)
            .map_err(|e| CliError::io("Clippy's answer", &e))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 1 << 20 {
            return Err(bad("Clippy's answer is too large"));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let answer: Value = serde_json::from_str(rest).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(bad(format!("Clippy answered {status}")));
    }
    Ok(answer)
}

/// The hand-off. `confirm` gets what will be sent (hook, file, title, duration,
/// render_sig, framing) and returns whether the human confirmed it; the file is hashed
/// again after that, right before the post.
pub fn send(
    app: &Path,
    project: &Path,
    cut: &str,
    export: &Path,
    endpoint: &str,
    confirm: &dyn Fn(&Value) -> bool,
) -> Result<Value, CliError> {
    let secret =
        secret(app).ok_or_else(|| bad("gmclip is not paired with Clippy (gmclip clippy pair)"))?;
    loopback(endpoint)?;
    // resolve the folder (symlinks there resolve), keep the file name: the file itself
    // must not be a symlink
    let name = export
        .file_name()
        .ok_or_else(|| bad("the export path names no file"))?;
    let dir = export
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| CliError::io("export", &e))?;
    let abs = dir.join(name);
    let rec = checked_record(&abs)?;
    let handle = ProjectHandle::open(project, OpenOptions::default())
        .map_err(|e| bad(format!("open the project: {e}")))?;
    let side = sidecar(&handle.snapshot().doc, cut, &rec);
    let _ = handle.close();
    let side = side?;
    let hook = side["hook_id"].as_str().unwrap_or_default().to_owned();
    let mut shown = side.clone();
    shown["file"] = json!(abs);
    if !confirm(&shown) {
        return Err(CliError::new(Code::Unauthorized)
            .step("clippy")
            .reason("not confirmed"));
    }
    // the bytes Clippy will copy are the ones the record vouches for, checked now
    let rec2 = checked_record(&abs)?;
    if rec2.sha256 != rec.sha256 {
        return Err(bad("the export changed while you were confirming"));
    }
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let side_path = dir.join(format!("{stem}.clippy.json"));
    if std::fs::symlink_metadata(&side_path).is_ok_and(|m| !m.is_file()) {
        return Err(bad("the sidecar path is not a regular file"));
    }
    let tmp = dir.join(format!(".{stem}.clippy.json.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| CliError::io("sidecar", &e))?;
    f.write_all(&serde_json::to_vec_pretty(&side).unwrap_or_default())
        .map_err(|e| CliError::io("sidecar", &e))?;
    drop(f);
    std::fs::rename(&tmp, &side_path).map_err(|e| CliError::io("sidecar", &e))?;
    let body = json!({
        "hook_id": hook,
        "path": abs,
        "quality": "1080",
        "gmclip": side,
    });
    let answer = post(endpoint, &secret, &body)?;
    Ok(json!({"hook_id": hook, "sidecar": side_path, "clippy": answer}))
}

pub fn run(cmd: ClippyCmd) -> Result<Output, CliError> {
    terminal()?;
    let app = socket::env()?.app_data;
    match cmd {
        ClippyCmd::Pair => {
            let s = pair(&app)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "Clippy pairing code (enter it in Clippy; shown once):\n{s}"
            );
            Ok(Output::Summary(json!({"paired": true})))
        }
        ClippyCmd::Send {
            project,
            cut,
            export,
            endpoint,
        } => {
            let confirm = |shown: &Value| -> bool {
                let hook = shown["hook_id"].as_str().unwrap_or_default();
                let mut e = std::io::stderr().lock();
                let _ = writeln!(
                    e,
                    "Hand this export to Clippy (it replaces that hook's current final):\n  hook: {}\n  file: {}\n  title: {}\n  duration: {} s\n  framing: {}\n  render_sig: {}",
                    crate::share_cmd::esc(hook),
                    shown["file"].as_str().unwrap_or_default(),
                    crate::share_cmd::esc(shown["title"].as_str().unwrap_or_default()),
                    shown["duration"],
                    shown["framing"],
                    shown["render_sig"],
                );
                let _ = write!(e, "Type the hook id to confirm: ");
                let _ = e.flush();
                drop(e);
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line).is_ok() && line.trim() == hook
            };
            send(&app, &project, &cut, &export, &endpoint, &confirm).map(Output::Summary)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p =
            std::env::temp_dir().join(format!("gmclip-clippy-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    /// A project with one story cut on hook `mini_h01` (a committed fixture).
    fn project(d: &Path) -> PathBuf {
        let fx: Value = serde_json::from_slice(
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini-story.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let db = d.join("projects/mini.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        gmclip_core::fixture::write_db(&fx, d, &db).unwrap();
        db
    }

    fn export(d: &Path, name: &str, blocked: Vec<String>, tamper: bool) -> PathBuf {
        export_f(d, name, blocked, tamper, 75)
    }

    fn export_f(d: &Path, name: &str, blocked: Vec<String>, tamper: bool, frames: u64) -> PathBuf {
        let mp4 = d.join(format!("{name}.mp4"));
        std::fs::write(&mp4, b"an mp4").unwrap();
        let rec = ExportRecord {
            render_sig: Some("sig".into()),
            render_sig_absent: None,
            framing: Some("9:16".into()),
            preset: "social".into(),
            input_rev: None,
            duration_expected: gmclip_media::export_record::DurationExpected {
                frames,
                fps_num: 30,
                fps_den: 1,
                audio_samples: frames * 1600,
            },
            frames_hash: None,
            file: mp4.file_name().unwrap().to_string_lossy().into_owned(),
            sha256: sha256_file(&mp4).unwrap(),
            loudness: None,
            draft_mattes: vec![],
            notes: vec![],
            blocked,
            app_version: "t".into(),
            renderer_version: "t".into(),
        };
        rec.write(&mp4).unwrap();
        if tamper {
            std::fs::write(&mp4, b"changed").unwrap();
        }
        mp4
    }

    /// A mock Clippy on an ephemeral loopback port (never :8878, never the real one).
    fn mock() -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        let g = Arc::clone(&got);
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buf = vec![0u8; 65536];
                let mut all = Vec::new();
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    all.extend_from_slice(&buf[..n]);
                    let t = String::from_utf8_lossy(&all).into_owned();
                    let done = t.split_once("\r\n\r\n").is_some_and(|(h, b)| {
                        let len = h
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        b.len() >= len
                    });
                    if n == 0 || done {
                        break;
                    }
                }
                g.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&all).into_owned());
                let body = r#"{"ok":true}"#;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), got)
    }

    #[test]
    fn a_paired_confirmed_hand_off_reaches_the_mock_and_nothing_else_does() {
        let d = tmp("send");
        let app = d.join("app");
        let db = project(&d);
        let (ep, got) = mock();
        let yes = |_: &Value| true;
        let good = export(&d, "final-0", vec![], false);
        // unpaired: nothing is sent
        assert!(send(&app, &db, "mini_h01", &good, &ep, &yes).is_err());
        assert!(got.lock().unwrap().is_empty());
        let secret = pair(&app).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(app.join(PAIRING_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        // not confirmed, blocked, or not the recorded file: nothing is sent
        assert!(send(&app, &db, "mini_h01", &good, &ep, &|_: &Value| false).is_err());
        assert!(
            send(
                &app,
                &db,
                "mini_h01",
                &export(&d, "blocked", vec!["error lint".into()], false),
                &ep,
                &yes
            )
            .is_err()
        );
        assert!(
            send(
                &app,
                &db,
                "mini_h01",
                &export(&d, "tampered", vec![], true),
                &ep,
                &yes
            )
            .is_err()
        );
        assert!(send(&app, &db, "mini_h01", &good, "http://10.0.0.1:8878", &yes).is_err());
        assert!(
            send(
                &app,
                &db,
                "mini_h01",
                &export_f(&d, "long", vec![], false, 300),
                &ep,
                &yes
            )
            .is_err(),
            "another length: not this cut"
        );
        assert!(got.lock().unwrap().is_empty());
        // a symlink planted at the sidecar path: refused, its target untouched
        let victim = d.join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&victim, d.join("final-0.clippy.json")).unwrap();
            assert!(send(&app, &db, "mini_h01", &good, &ep, &yes).is_err());
            assert!(got.lock().unwrap().is_empty());
            std::fs::remove_file(d.join("final-0.clippy.json")).unwrap();
        }
        // the real hand-off
        let v = send(&app, &db, "mini_h01", &good, &ep, &yes).unwrap();
        assert_eq!(v["hook_id"], "mini_h01");
        let reqs = got.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert!(r.starts_with("POST /api/edit_station/final HTTP/1.1\r\n"));
        assert!(r.contains(&format!("Authorization: Bearer {secret}")));
        assert!(r.contains(&format!("Host: {}", ep.trim_start_matches("http://"))));
        let body: Value = serde_json::from_str(r.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["hook_id"], "mini_h01");
        assert_eq!(body["gmclip"]["text_hook"], "It works");
        assert_eq!(body["gmclip"]["duration"], 2.5);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(body["gmclip"]["render_sig"], "sig");
        let side: Value =
            serde_json::from_slice(&std::fs::read(d.join("final-0.clippy.json")).unwrap()).unwrap();
        assert_eq!(side["title"], "Story");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn loopback_only() {
        assert!(loopback("http://127.0.0.1:8878").is_ok());
        assert!(loopback("http://[::1]:8878").is_ok());
        for e in [
            "http://192.168.1.2:8878",
            "https://127.0.0.1:8878",
            "http://localhost:8878",
            "http://127.0.0.1",
            "http://evil.example:80",
            "http://0.0.0.0:1",
        ] {
            assert!(loopback(e).is_err(), "{e}");
        }
    }
}
