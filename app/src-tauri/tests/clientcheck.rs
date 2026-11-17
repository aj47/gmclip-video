//! A Claude Code build outside the tested range (it auto-updates): the one-time
//! isolation check (S7, zero spend: a loopback stub model and a canary home, never a real
//! model) runs on that exact binary; passed → `Verified{sha256}`, recorded and not probed
//! again; failed → blocked with the failing checks, and "Run anyway" needs the Owner
//! sheet. Uses AJ's installed Claude Code; skips without it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_ai::launcher::{Harness, VersionPolicy};
use gmclip_app::clientcheck::{self, Build, Probed};
use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// A Claude Code on this machine: `GMCLIP_TEST_CLAUDE`, else the first nvm install.
fn claude() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GMCLIP_TEST_CLAUDE").map(PathBuf::from) {
        return p.exists().then_some(p);
    }
    let nvm = Path::new(&std::env::var_os("HOME")?).join(".nvm/versions/node");
    let mut found: Vec<PathBuf> = std::fs::read_dir(nvm)
        .ok()?
        .filter_map(|v| Some(v.ok()?.path().join("bin/claude")))
        .filter(|p| p.exists())
        .collect();
    found.sort();
    found.pop()
}

static SHEET: AtomicBool = AtomicBool::new(false);

#[test]
fn a_new_build_is_checked_once_and_a_failure_needs_the_owner() {
    let Some(claude) = claude() else {
        eprintln!("skipped: no Claude Code here");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-cc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    gmclip_app::sheet::install(Box::new(|_, _, _| SHEET.load(Ordering::SeqCst)));
    let real = clientcheck::build_of(&claude, Harness::ClaudeCode).unwrap();
    eprintln!("{real:?}");
    let checks = Cell::new(0);
    let checking = || checks.set(checks.get() + 1);
    let never =
        || -> gmclip_app::error::AppResult<Probed> { panic!("probed a build that needs no probe") };

    // the tested build: enforced, no probe
    if real.tested {
        assert_eq!(
            clientcheck::decide(&c, &real, &checking, &never)
                .unwrap()
                .unwrap(),
            VersionPolicy::Enforce
        );
    }

    // the same binary as if it had updated past the tested range: the real S7 probe
    let new = Build {
        tested: false,
        version: "2.1.301".into(),
        ..real.clone()
    };
    let stub = Path::new(env!("CARGO_BIN_EXE_gmclip-app"));
    let t0 = std::time::Instant::now();
    let probe = || clientcheck::run_probe(&c, Harness::ClaudeCode, &new.exec, stub);
    let d = clientcheck::decide(&c, &new, &checking, &probe).unwrap();
    eprintln!("probe {:?}: {d:?}", t0.elapsed());
    assert_eq!(
        checks.get(),
        1,
        "\"Checking the new Claude Code\" shown once"
    );
    let row = clientcheck::load(&c)
        .into_iter()
        .find(|r| r.sha256 == new.sha256)
        .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&row.checks).unwrap());
    assert!(row.checks.len() >= 10, "{} checks", row.checks.len());
    assert_eq!(row.result, "passed", "2.1.300's binary passes S7");
    assert_eq!(
        d.unwrap(),
        VersionPolicy::verified_hex(&new.sha256).unwrap()
    );
    // recorded: not probed again
    assert!(
        clientcheck::decide(&c, &new, &checking, &never)
            .unwrap()
            .is_ok()
    );
    assert_eq!(checks.get(), 1);

    // another build that fails a check: blocked, naming it; Run anyway needs the sheet
    let bad = Build {
        sha256: "ab".repeat(32),
        version: "2.1.302".into(),
        ..new.clone()
    };
    let failing = || {
        Ok(Probed {
            supported: false,
            checks: vec![
                json!({"id": "S7.1", "what": "no user CLAUDE.md", "pass": false, "detail": "canary seen"}),
                json!({"id": "S7.2", "what": "no user MCP", "pass": true, "detail": ""}),
            ],
        })
    };
    let b = clientcheck::decide(&c, &bad, &checking, &failing)
        .unwrap()
        .unwrap_err();
    assert_eq!(b.failed.len(), 1);
    assert_eq!(b.failed[0]["id"], "S7.1");
    assert!(
        clientcheck::decide(&c, &bad, &checking, &never)
            .unwrap()
            .is_err(),
        "still blocked, not re-probed"
    );
    SHEET.store(false, Ordering::SeqCst);
    assert!(clientcheck::allow(&c, &bad.sha256).is_err());
    SHEET.store(true, Ordering::SeqCst);
    assert_eq!(
        clientcheck::allow(&c, &bad.sha256).unwrap()["result"],
        "owner_allowed"
    );
    assert_eq!(
        clientcheck::decide(&c, &bad, &checking, &never)
            .unwrap()
            .unwrap(),
        VersionPolicy::verified_hex(&bad.sha256).unwrap()
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// pi and opencode (decision 0012): the probe does not cover them yet, so a first run is
/// blocked with that one reason and only the Owner's "Run anyway" verifies the build.
/// Codex (amended 2026-10-16, AJ: "dont worry about codex safety"): its tested 0.160.0
/// runs with no probe and no sheet; a newer build is probed like a new Claude Code, and
/// one that fails runs once the Owner says "Run anyway".
#[test]
fn uncovered_clients_need_the_owner_and_codex_is_gated_like_claude_code() {
    let root = std::env::temp_dir().join(format!("gmclip-cc2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    gmclip_app::sheet::install(Box::new(|_, _, _| SHEET.load(Ordering::SeqCst)));
    let pi = Build {
        harness: Harness::Pi,
        exec: "/opt/homebrew/lib/node_modules/pi/dist/bundle/cli.js".into(),
        version: "0.85.1".into(),
        sha256: "cd".repeat(32),
        tested: false,
    };
    let checks = Cell::new(0);
    let checking = || checks.set(checks.get() + 1);
    let stub = Path::new("/nonexistent/stub");
    let probe = || clientcheck::run_probe(&c, Harness::Pi, &pi.exec, stub);
    let b = clientcheck::decide(&c, &pi, &checking, &probe)
        .unwrap()
        .unwrap_err();
    assert_eq!(
        checks.get(),
        0,
        "no \"Checking…\" for a client the probe skips"
    );
    assert_eq!(b.failed.len(), 1);
    assert_eq!(b.failed[0]["id"], "S7.0");
    assert!(b.failed[0]["what"].as_str().unwrap().contains("pi"));
    let row = clientcheck::load(&c)
        .into_iter()
        .find(|r| r.sha256 == pi.sha256)
        .unwrap();
    assert_eq!(row.harness, "pi");
    SHEET.store(true, Ordering::SeqCst);
    assert_eq!(
        clientcheck::allow(&c, &pi.sha256).unwrap()["result"],
        "owner_allowed"
    );
    let never =
        || -> gmclip_app::error::AppResult<Probed> { panic!("probed a build that needs no probe") };
    assert_eq!(
        clientcheck::decide(&c, &pi, &checking, &never)
            .unwrap()
            .unwrap(),
        VersionPolicy::verified_hex(&pi.sha256).unwrap()
    );
    // Codex 0.160.0, the tested build: runs under Enforce, nothing probed, no sheet
    let codex = Build {
        harness: Harness::Codex,
        sha256: "ef".repeat(32),
        version: "0.160.0".into(),
        tested: true,
        ..pi.clone()
    };
    assert_eq!(
        clientcheck::decide(&c, &codex, &checking, &never)
            .unwrap()
            .unwrap(),
        VersionPolicy::Enforce
    );
    // a newer Codex: the one-time check (scripted here: its S7 leaks fail it), then the
    // Owner's "Run anyway" verifies that exact build
    let newer = Build {
        sha256: "f0".repeat(32),
        version: "0.161.0".into(),
        tested: false,
        ..codex.clone()
    };
    let leaks = || -> gmclip_app::error::AppResult<Probed> {
        Ok(Probed {
            supported: false,
            checks: vec![
                serde_json::json!({"id": "S7.3", "what": "no canary text reaches the session", "pass": false, "detail": "leaked [\"codex_home_agents\"]"}),
            ],
        })
    };
    let before = checks.get();
    let b = clientcheck::decide(&c, &newer, &checking, &leaks)
        .unwrap()
        .unwrap_err();
    assert_eq!(
        checks.get(),
        before + 1,
        "\"Checking…\" shows for a new Codex"
    );
    assert_eq!(b.failed[0]["id"], "S7.3");
    assert_eq!(
        clientcheck::allow(&c, &newer.sha256).unwrap()["result"],
        "owner_allowed"
    );
    assert_eq!(
        clientcheck::decide(&c, &newer, &checking, &never)
            .unwrap()
            .unwrap(),
        VersionPolicy::verified_hex(&newer.sha256).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}
