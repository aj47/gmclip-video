//! A client build outside the tested range (Claude Code auto-updates; pi and opencode
//! have no tested version yet, decision 0012): before the first run on it, the app runs
//! the S7 isolation probe on that exact binary — zero spend, a
//! loopback stub model and a canary home, the same checks lane E wrote
//! (`gmclip_eval::s7`) — and records the outcome in app data, keyed by version and the
//! binary's SHA-256 (`verified-clients.json`).
//!
//! - Every check passes: the build is verified, and runs launch with
//!   `VersionPolicy::Verified { sha256 }` (that binary only; a known S7 failure is still
//!   refused by the launcher).
//! - A check fails: runs are refused with the failing checks named; "Run anyway" needs
//!   the Owner sheet, and is recorded on the same row (`owner_allowed`).
//!
//! The probe runs once per binary; a binary that changes again is probed again. It covers
//! Claude Code and Codex (G.06 adds pi and opencode): for those two the row is recorded
//! as not covered, so their first run asks the Owner to allow that exact build. Codex is
//! no longer refused for its known S7 leaks (AJ, 2026-10-16: "dont worry about codex
//! safety", decision 0012): its tested 0.160.0 runs, a newer build is probed like a new
//! Claude Code and, when a check fails, runs once the Owner says "Run anyway".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gmclip_ai::launcher::{self, Harness, ProcessContext, VersionPolicy, version};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};

const FILE: &str = "verified-clients.json";
/// What the probe took on AJ's Mac (Claude Code 2.1.300: ~1–5 s), for the message.
pub const PROBE_ESTIMATE_S: u32 = 5;

static PROBING: Mutex<()> = Mutex::new(());

/// One probed build.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Verified {
    pub harness: String,
    pub version: String,
    pub sha256: String,
    pub exec: PathBuf,
    pub at_ms: i64,
    /// `passed`, `failed` or `owner_allowed` (failed, then "Run anyway" on the sheet).
    pub result: String,
    /// The checks: `{id, what, pass, detail}`.
    pub checks: Vec<Value>,
}

/// The build `client` resolves to.
#[derive(Clone, Debug, Serialize)]
pub struct Build {
    pub harness: Harness,
    pub exec: PathBuf,
    pub version: String,
    pub sha256: String,
    /// Inside the tested range.
    pub tested: bool,
}

/// Whether gmclip's isolation probe (S7) runs on `h`'s builds (else the Owner allows an
/// untested build directly).
pub fn probe_covers(h: Harness) -> bool {
    matches!(h, Harness::ClaudeCode | Harness::Codex)
}

fn file(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join(FILE)
}

pub fn load(core: &AppCore) -> Vec<Verified> {
    std::fs::read(file(core))
        .ok()
        .filter(|b| b.len() < 4 << 20)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(core: &AppCore, row: Verified) -> AppResult<()> {
    let mut all = load(core);
    all.retain(|r| r.sha256 != row.sha256);
    all.push(row);
    let _ = std::fs::create_dir_all(&core.dirs.app_data);
    let f = file(core);
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all).unwrap_or_default())
        .and_then(|()| std::fs::rename(&tmp, &f))
        .map_err(|e| AppError::internal(format!("saving {FILE}: {}", e.kind())))
}

/// Resolves `client` as the launcher will, hashes it and asks its version.
pub fn build_of(client: &Path, h: Harness) -> AppResult<Build> {
    let r = launcher::resolve::resolve(h, Some(client), None)
        .map_err(|e| AppError::invalid(e.to_string()))?;
    let out = launcher::ask_version(
        &r.exec,
        r.interpreter.as_deref(),
        &ProcessContext::current(),
    );
    let ver = version::parse(&out);
    let sha256 =
        launcher::sha256::file_hex(&r.exec).map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Build {
        harness: h,
        exec: r.exec,
        version: ver.map_or_else(|| out.trim().chars().take(40).collect(), |v| v.to_string()),
        sha256,
        tested: version::check(h, ver) == version::Verdict::Supported,
    })
}

/// The probe's outcome (what [`decide`] needs from it).
#[derive(Clone, Debug)]
pub struct Probed {
    pub supported: bool,
    pub checks: Vec<Value>,
}

/// Runs the S7 probe on `exec`; `stub` is the executable serving `mcp-stub`
/// (`gmclip-app` itself). A client the probe does not cover yet gets one failed check
/// saying so (nothing runs).
pub fn run_probe(core: &AppCore, h: Harness, exec: &Path, stub: &Path) -> AppResult<Probed> {
    if !probe_covers(h) {
        return Ok(Probed {
            supported: false,
            checks: vec![
                json!({"id": "S7.0", "what": gmclip_eval::s7::not_covered(h),
                "pass": false, "detail": "allow this exact build to run it (Run anyway)"}),
            ],
        });
    }
    // Under the per-user temp root real runs use, never under app data: the home folder
    // above app data holds the user's own `.claude`/`.codex`, which fails the probe's
    // ancestor check before it starts.
    let _ = core;
    let base = launcher::rundir::default_root()
        .ok()
        .and_then(|r| r.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    // private like the runs root below it: made first by plain std it would inherit its
    // parent's ACL (Windows: the drive root's) and the runs root check would refuse it
    launcher::rundir::mkdir_private(&base)
        .map_err(|e| AppError::internal(format!("the isolation check could not run: {e}")))?;
    let work = base.join("s7").join(format!("probe-{}", now_ms()));
    let r = gmclip_eval::s7::probe(h, exec, &work, stub)
        .map_err(|e| AppError::internal(format!("the isolation check could not run: {e}")))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(Probed {
        supported: r.supported,
        checks: r
            .checks
            .iter()
            .map(|c| {
                json!({"id": c.id, "what": c.what, "pass": c.pass,
                            "detail": c.detail.chars().take(300).collect::<String>()})
            })
            .collect(),
    })
}

/// Why a run cannot start on this build.
#[derive(Clone, Debug, Serialize)]
pub struct Blocked {
    pub version: String,
    pub sha256: String,
    pub failed: Vec<Value>,
}

/// The launch policy for `build`: tested → `Enforce`; probed and passed (or allowed by
/// the Owner) → `Verified`; probed and failed → `Blocked`; never probed → `checking()`
/// is called, then the probe runs and its outcome is recorded.
pub fn decide(
    core: &AppCore,
    build: &Build,
    checking: &dyn Fn(),
    probe: &dyn Fn() -> AppResult<Probed>,
) -> AppResult<Result<VersionPolicy, Blocked>> {
    if build.tested {
        return Ok(Ok(VersionPolicy::Enforce));
    }
    let verified = || {
        VersionPolicy::verified_hex(&build.sha256)
            .ok_or_else(|| AppError::internal("the binary's hash"))
    };
    let _one = PROBING
        .lock()
        .map_err(|_| AppError::internal("probe lock"))?;
    let row = match load(core).into_iter().find(|r| r.sha256 == build.sha256) {
        Some(r) => r,
        None => {
            if probe_covers(build.harness) {
                checking();
            }
            let p = probe()?;
            let row = Verified {
                harness: build.harness.as_str().into(),
                version: build.version.clone(),
                sha256: build.sha256.clone(),
                exec: build.exec.clone(),
                at_ms: now_ms(),
                result: if p.supported { "passed" } else { "failed" }.into(),
                checks: p.checks,
            };
            save(core, row.clone())?;
            row
        }
    };
    if row.result == "passed" || row.result == "owner_allowed" {
        return Ok(Ok(verified()?));
    }
    Ok(Err(Blocked {
        version: row.version,
        sha256: row.sha256,
        failed: row
            .checks
            .into_iter()
            .filter(|c| c["pass"] == false)
            .collect(),
    }))
}

/// "Run anyway" on a build that failed the probe: the Owner sheet names the failed checks.
pub fn allow(core: &AppCore, sha256: &str) -> AppResult<Value> {
    let mut row = load(core)
        .into_iter()
        .find(|r| r.sha256 == sha256)
        .ok_or_else(|| AppError::not_found("no checked build with that hash"))?;
    if row.result != "failed" {
        return Ok(json!({"result": row.result}));
    }
    let failed: Vec<String> = row
        .checks
        .iter()
        .filter(|c| c["pass"] == false)
        .map(|c| {
            format!(
                "• {}: {}",
                c["id"].as_str().unwrap_or("?"),
                c["what"].as_str().unwrap_or("")
            )
        })
        .collect();
    let label = Harness::parse(&row.harness).map_or("this agent", Harness::label);
    let text = format!(
        "{label} {} did not pass gmclip's isolation check:\n{}\n\nRunning it anyway means your agent may see settings, files or tools from outside this run. Only this exact build is allowed; an update asks again.",
        row.version,
        failed.join("\n")
    );
    if !crate::sheet::confirm(&format!("Run this {label} anyway?"), &text, "Run anyway") {
        return Err(AppError::new(
            "Unauthorized",
            "not allowed (the Owner did not approve it on the sheet)",
        ));
    }
    row.result = "owner_allowed".into();
    row.at_ms = now_ms();
    save(core, row)?;
    Ok(json!({"result": "owner_allowed"}))
}
