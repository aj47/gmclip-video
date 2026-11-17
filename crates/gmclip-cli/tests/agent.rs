//! `gmclip agent run` refusals (R-AGT-1, R-AGT-3, R-OWN-3): without an interactive
//! terminal nothing starts (not even the client's `--version`), Owner grants need a
//! terminal, and a script posing as a client is refused. No real client is ever run here.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gmclip-cli-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn gmclip(args: &[&str], logs: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gmclip"))
        .args(args)
        .env("GMCLIP_LOG_DIR", logs)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn err(out: &Output) -> Value {
    serde_json::from_slice(&out.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)))
}

/// A fake client that leaves a marker if it ever runs past `--version`. On Windows the
/// same `#!` file cannot start at all; the launcher must refuse it as a script there too.
fn fake(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("claude");
    let marker = dir.join("RAN");
    std::fs::write(
        &p,
        format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.300 (Claude Code)'; exit 0; fi\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

#[test]
fn media_without_a_terminal_is_refused_first() {
    let d = scratch("media");
    let out = gmclip(
        &[
            "agent",
            "run",
            "--harness",
            "claude-code",
            "--cut",
            "c",
            "--brief",
            "x",
            "--media",
            "--client-path",
            "/nonexistent/claude",
        ],
        &d,
    );
    assert_eq!(out.status.code(), Some(1));
    let e = err(&out);
    assert!(
        e["reason"]
            .as_str()
            .unwrap()
            .contains("interactive terminal"),
        "{e}"
    );
}

#[test]
fn nothing_starts_and_scripts_are_refused() {
    let d = scratch("script");
    let client = fake(&d);
    let base = [
        "agent",
        "run",
        "--harness",
        "claude-code",
        "--cut",
        "c",
        "--brief",
        "x",
        "--client-path",
        client.to_str().unwrap(),
    ];
    // without a terminal: refused before the client is touched (R-AGT-3)
    let out = gmclip(&base, &d);
    assert_eq!(
        out.status.code(),
        Some(77),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        err(&out)["reason"]
            .as_str()
            .unwrap()
            .contains("interactive terminal")
    );
    // a dry run inspects, and refuses the script on the gated path
    let mut args = base.to_vec();
    args.push("--dry-run");
    let out = gmclip(&args, &d);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(err(&out)["reason"].as_str().unwrap().contains("script"));
    assert!(!d.join("RAN").exists(), "the fake client ran");
}

#[test]
fn bad_harness_and_wall_clock_are_usage_errors() {
    let d = scratch("usage");
    for args in [
        vec![
            "agent",
            "run",
            "--harness",
            "cursor",
            "--cut",
            "c",
            "--brief",
            "x",
        ],
        vec![
            "agent",
            "run",
            "--harness",
            "codex",
            "--cut",
            "c",
            "--brief",
            "x",
            "--wall-clock",
            "30s",
        ],
        vec!["agent", "run", "--harness", "codex", "--cut", "c"],
    ] {
        let out = gmclip(&args, &d);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A set but malformed `GMCLIP_RUN` is refused by the shim, never downgraded to a local
/// session (which would drop the run's cut binding). Sandboxed HOME/TMPDIR: no holder is
/// ever reached.
#[test]
fn a_malformed_run_credential_is_refused_not_downgraded() {
    let d = scratch("badrun");
    for bad in ["garbage", "run:", ":abcd", "run:zz", "01ABC:abc"] {
        let out = Command::new(env!("CARGO_BIN_EXE_gmclip"))
            .arg("mcp")
            .env("HOME", &d)
            .env("TMPDIR", &d)
            .env("XDG_RUNTIME_DIR", &d)
            .env("USERPROFILE", &d)
            .env("LOCALAPPDATA", d.join("AppData").join("Local"))
            .env("GMCLIP_LOG_DIR", &d)
            .env("GMCLIP_RUN", bad)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad}: accepted");
        let e = String::from_utf8_lossy(&out.stderr);
        assert!(e.contains("malformed"), "{bad}: {e}");
        assert!(
            !e.contains(
                bad.split(':')
                    .nth(1)
                    .filter(|s| s.len() > 2)
                    .unwrap_or("\u{0}")
            ),
            "{bad}: echoed: {e}"
        );
    }
    // refused before any connection: no holder was started, so no app data
    assert!(!d.join("Library/Application Support/gmclip").exists());
    assert!(!d.join(".local/share/gmclip").exists());
    assert!(!d.join("AppData").join("Local").join("gmclip").exists());
    let _ = std::fs::remove_dir_all(&d);
}
