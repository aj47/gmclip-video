//! `gmclip share` (R-REV-7) and `gmclip review serve` end to end: a committed fixture
//! project, a review written through a link landing in the project as
//! `review-link:<id>` via the actor, escaped text everywhere, and link creation refused
//! without a terminal.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

fn scratch(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("gmclip-cli-rs-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn cmd(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_gmclip"));
    c.env("GMCLIP_LOG", "off")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        // Windows app data is %LOCALAPPDATA%\gmclip: never the real one in a test
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home.join("AppData").join("Local"));
    c
}

/// Kills the child on drop, so a failed assertion never leaves a server running.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(home: &Path, args: &[&str]) -> Output {
    cmd(home).args(args).stdin(Stdio::null()).output().unwrap()
}

/// A one-stream project whose story title and notes are hostile HTML (a committed fixture).
fn project(d: &Path, _home: &Path) -> PathBuf {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini-hostile.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let db = d.join("projects/mini.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    gmclip_core::fixture::write_db(&fx, d, &db).unwrap();
    db
}

#[test]
fn the_share_page_is_static_escaped_and_says_who_can_watch() {
    let d = scratch("share");
    let home = d.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = project(&d, &home);
    let mp4 = d.join("e.mp4");
    std::fs::write(&mp4, b"not really a video").unwrap();
    let out = d.join("share");
    let o = run(
        &home,
        &[
            "share",
            "--project",
            db.to_str().unwrap(),
            "--cut",
            &format!("mini_h01={}", mp4.display()),
            "--out",
            out.to_str().unwrap(),
            "--notes",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let html = std::fs::read_to_string(out.join("index.html")).unwrap();
    let quiet = d.join("share-default");
    let q = run(
        &home,
        &[
            "share",
            "--project",
            db.to_str().unwrap(),
            "--cut",
            &format!("mini_h01={}", mp4.display()),
            "--out",
            quiet.to_str().unwrap(),
        ],
    );
    assert!(q.status.success());
    assert!(
        !std::fs::read_to_string(quiet.join("index.html"))
            .unwrap()
            .contains("alert(2)"),
        "notes excluded by default"
    );
    assert!(html.contains("http-equiv=\"Content-Security-Policy\""));
    assert!(html.contains("default-src &#39;none&#39;") || html.contains("default-src 'none'"));
    assert!(html.contains("Anyone who can open this folder can watch these files."));
    assert!(!html.contains("<script"), "no script at all");
    assert!(!html.contains("<img"), "titles are text");
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(html.contains("⟦U+202E⟧"), "bidi shown, not applied");
    assert_eq!(
        std::fs::read(out.join("cut-1.mp4")).unwrap(),
        b"not really a video"
    );
    // a non-empty folder is refused
    let again = run(
        &home,
        &[
            "share",
            "--project",
            db.to_str().unwrap(),
            "--cut",
            &format!("mini_h01={}", mp4.display()),
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert!(!again.status.success());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_review_through_the_link_lands_in_the_project_as_the_link() {
    let d = scratch("serve");
    let home = d.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = project(&d, &home);
    // without a terminal, no link is made
    let o = run(&home, &["review", "link", "new"]);
    assert!(!o.status.success());
    // the app data this HOME gives, and a link made there directly
    let app = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/gmclip")
    } else if cfg!(windows) {
        home.join("AppData").join("Local").join("gmclip")
    } else {
        home.join(".local/share/gmclip")
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let (link, token) = gmclip_mcp::review::links::create(&app, now, None).unwrap();
    let exports = d.join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    let child = cmd(&home)
        .args([
            "review",
            "serve",
            "--project",
            db.to_str().unwrap(),
            "--exports",
            exports.to_str().unwrap(),
            "--port",
            "0",
        ])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = KillOnDrop(child);
    let mut err = BufReader::new(child.0.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    let addr = line
        .split("serving on ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let host = addr.clone();
    let req = |m: &str,
               p: &str,
               hs: &[(&str, &str)],
               body: &str|
     -> (u16, Vec<(String, String)>, String) {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut r = format!(
            "{m} {p} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in hs {
            r.push_str(&format!("{k}: {v}\r\n"));
        }
        r.push_str("\r\n");
        r.push_str(body);
        s.write_all(r.as_bytes()).unwrap();
        let mut b = String::new();
        s.read_to_string(&mut b).unwrap();
        let (head, body) = b.split_once("\r\n\r\n").unwrap_or((&b, ""));
        let mut ls = head.split("\r\n");
        let st = ls
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let hs = ls
            .filter_map(|l| {
                l.split_once(": ")
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
            })
            .collect();
        (st, hs, body.to_owned())
    };
    let origin = format!("http://{host}");
    let (st, hs, _) = req(
        "POST",
        "/session",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        &json!({"token": token}).to_string(),
    );
    assert_eq!(st, 200);
    let cookie = hs
        .iter()
        .find(|h| h.0 == "Set-Cookie")
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (st, _, body) = req("GET", "/api/home", &[("Cookie", &cookie)], "");
    assert_eq!(st, 200);
    let home_v: Value = serde_json::from_str(&body).unwrap();
    let csrf = home_v["csrf"].as_str().unwrap().to_owned();
    let cut = home_v["cuts"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        home_v["cuts"][0]["title"], "<img src=x onerror=alert(1)>",
        "raw in JSON; the page sets it as text"
    );
    let (st, _, b) = req(
        "POST",
        "/api/review",
        &[
            ("Cookie", &cookie),
            ("X-CSRF", &csrf),
            ("Origin", &origin),
            ("Content-Type", "application/json"),
        ],
        &json!({"cut": cut, "text": "tighten the open", "verdict": "change"}).to_string(),
    );
    assert_eq!(st, 200, "{b}");
    let (_, _, body) = req("GET", "/api/home", &[("Cookie", &cookie)], "");
    let v: Value = serde_json::from_str(&body).unwrap();
    let r = &v["cuts"][0]["reviews"][0];
    assert_eq!(r["author"], format!("review-link:{}", link.id));
    assert_eq!(r["verdict"], "change");
    drop(child);
    let _ = std::fs::remove_dir_all(&d);
}

/// Send to Clippy is a human action: without a terminal nothing is paired or sent.
#[test]
fn clippy_needs_a_human_at_a_terminal() {
    let d = scratch("clippy");
    let home = d.join("home");
    std::fs::create_dir_all(&home).unwrap();
    assert!(!run(&home, &["clippy", "pair"]).status.success());
    let app = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/gmclip")
    } else if cfg!(windows) {
        home.join("AppData").join("Local").join("gmclip")
    } else {
        home.join(".local/share/gmclip")
    };
    assert!(!app.join("clippy-pairing.json").exists());
    let o = run(
        &home,
        &[
            "clippy",
            "send",
            "--project",
            "/x",
            "--cut",
            "h",
            "--export",
            "/x.mp4",
            "--endpoint",
            "http://127.0.0.1:9",
        ],
    );
    assert!(!o.status.success());
    let e: Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(
        e["code"], "Unauthorized",
        "refused for want of a terminal, before anything else"
    );
    let _ = std::fs::remove_dir_all(&d);
}
