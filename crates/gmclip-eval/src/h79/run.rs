//! The h79 runner (`cargo xt eval h79 --client claude-code|codex --runs 5`; backlog E04).
//!
//! Per run, in the pre-registered order (interleaved across clients, export semaphore 1):
//! 1. a fresh copy of the fixture project (`VACUUM INTO`) and a fresh holder in a temp
//!    app-data dir;
//! 2. the launcher, exactly as a human would start it:
//!    `gmclip agent run --harness <client> --cut <h79 eval cut> --brief-file evals/h79/brief.md
//!     --media --wall-clock 90m --transcript-out <batch>/<client>-<n>/`
//!    with the model and effort pinned in `evals/h79/clients.toml`;
//! 3. the bundle (`plan.json`, `project.json`, `transcript.jsonl`, `run.json`, `asr.json`)
//!    read back from the run's project and export;
//! 4. graded at once; `<batch>/<client>-<n>.json` written; infrastructure failures rerun
//!    (≤ 2 per batch) as `<client>-<n>.retry<k>.json`.
//!
//! Steps 1–3 need lane A2's holder, MCP server and export record, so the live driver
//! refuses for now (`TODO(A2)`), and no model is ever called from here before then. The
//! replay driver grades bundles already on disk, which is how the scorer and this file are
//! tested today.

use std::path::{Path, PathBuf};

use super::bundle::{Bundle, Mode};
use super::grade::{self, Reference};
use super::score::{self, Preregistration, RunResult};

/// Where a run's bundle comes from.
#[derive(Clone, Debug)]
pub enum Driver {
    /// Launch the client (needs A2).
    Live,
    /// Grade `<dir>/<client>-<n>/` bundles that already exist.
    Replay(PathBuf),
}

/// The exact launcher command a live run uses (also printed for AJ).
pub fn launch_command(client: &str, cut: &str, out: &Path) -> String {
    format!(
        "gmclip agent run --harness {client} --cut {cut} --brief-file evals/h79/brief.md --media --wall-clock 90m --transcript-out {}",
        out.display()
    )
}

/// Runs (or replays) `runs` runs of `clients` into `batch`, then scores the batch.
pub fn run(
    h79: &Path,
    clients: &[String],
    runs: usize,
    batch: &Path,
    driver: &Driver,
) -> Result<score::BatchScore, String> {
    let reference = Reference::load(h79)?;
    let reg: Preregistration = serde_json::from_slice(
        &std::fs::read(h79.join("preregistration.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if runs != reg.runs_per_client || clients.iter().any(|c| !reg.clients.contains(c)) {
        return Err(format!(
            "the registration says {} runs of {:?}; a different batch is not a gate batch (score it by hand if it is an experiment)",
            reg.runs_per_client, reg.clients
        ));
    }
    std::fs::create_dir_all(batch).map_err(|e| e.to_string())?;
    // registered before the first run; never overwritten
    let reg_path = batch.join("preregistration.json");
    if let Ok(existing) = std::fs::read(&reg_path) {
        let existing: Preregistration =
            serde_json::from_slice(&existing).map_err(|e| e.to_string())?;
        if existing != reg {
            return Err(
                "this batch was registered under another preregistration; start a new batch".into(),
            );
        }
    } else {
        std::fs::write(
            &reg_path,
            serde_json::to_vec_pretty(&reg).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    for (client, n) in score::run_order(&reg)
        .into_iter()
        .filter(|(c, _)| clients.contains(c))
    {
        let out = batch.join(format!("{client}-{n}.json"));
        if out.exists() {
            continue; // kept: a batch is never rewritten
        }
        let bundle_dir = match driver {
            Driver::Live => {
                return Err(format!(
                    "live runs need lane A2's holder, `gmclip mcp` and the export record (TODO(A2)); \
                     the first run would be: {}",
                    launch_command(
                        &client,
                        "<h79 eval cut id>",
                        &batch.join(format!("{client}-{n}"))
                    )
                ));
            }
            Driver::Replay(dir) => dir.join(format!("{client}-{n}")),
        };
        // a run that left no gradable bundle (crash, no export, garbage) is a failed run,
        // recorded, never a missing one that could be rerun for free
        let graded = Bundle::load(&bundle_dir, Mode::Run)
            .and_then(|b| grade::grade(&b, &reference).map(|rep| (b, rep)));
        let mut result = match graded {
            Ok((b, rep)) => {
                RunResult::from_report(&client, n, &rep, b.run.as_ref(), b.transcript.as_deref())
            }
            Err(e) => RunResult {
                schema: "gmclip.eval.h79.result/0".into(),
                client: client.clone(),
                n,
                outcome: "fail".into(),
                infra_reason: None,
                failed: vec![format!("bundle: {e}")],
                pending: vec![],
                checks: Default::default(),
                transcript_sha256: None,
                client_version: None,
                model: None,
                tokens: None,
                wall_clock_ms: None,
                graded_with: None,
                pre_approval: false,
            },
        };
        result.graded_with = Some(graded_with(h79)?);
        result.pre_approval = !reference.truth.approved_by_aj;
        std::fs::write(
            &out,
            serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let s = score::score(batch, &reg)?;
    std::fs::write(
        batch.join("batch.json"),
        serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(s)
}

/// The grader inputs this build grades with: truth.json, the grader's own source, the
/// fixture manifest.
pub fn graded_with(h79: &Path) -> Result<score::Frozen, String> {
    let sha = |p: &Path| {
        std::fs::read(p)
            .map(|b| gmclip_ai::launcher::sha256::hex(&b))
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let grader = [
        include_str!("grade.rs"),
        include_str!("ink.rs"),
        include_str!("bundle.rs"),
    ]
    .concat();
    Ok(score::Frozen {
        truth_sha256: sha(&h79.join("truth.json"))?,
        grader_sha256: gmclip_ai::launcher::sha256::hex(grader.as_bytes()),
        fixture_manifest_sha256: sha(&h79.join("fixture/manifest.json"))?,
    })
}

/// Writes a bundle to `dir` (for replays and inspection).
pub fn write_bundle(dir: &Path, b: &Bundle) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let w = |name: &str, bytes: Vec<u8>| {
        std::fs::write(dir.join(name), bytes).map_err(|e| e.to_string())
    };
    w(
        "plan.json",
        serde_json::to_vec(&b.plan).map_err(|e| e.to_string())?,
    )?;
    w(
        "project.json",
        serde_json::to_vec_pretty(&b.project).map_err(|e| e.to_string())?,
    )?;
    if let Some(t) = &b.transcript {
        w("transcript.jsonl", t.clone().into_bytes())?;
    }
    if let Some(r) = &b.run {
        w(
            "run.json",
            serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?,
        )?;
    }
    if let Some(a) = &b.asr {
        w(
            "asr.json",
            serde_json::to_vec(&serde_json::json!({ "words": a })).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::selftest;
    use super::*;

    /// Five pre-registered runs per client, replayed from self-test bundles, graded and
    /// scored. The golden runs are `provisional` (G8(b) pending), so the batch is scored but
    /// is not gate-eligible: a pending MUST check can never be counted as a pass.
    #[test]
    fn five_registered_runs_replay_grade_and_score() {
        let h79 = super::super::bundle::h79_dir();
        let r = Reference::load(&h79).unwrap();
        let tmp = std::env::temp_dir().join(format!("gmclip-h79-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bundles = tmp.join("bundles");
        let mutants = selftest::mutants(&r).unwrap();
        let g0 = mutants.iter().find(|m| m.1 == "G0").unwrap().2.clone();
        for n in 1..=5 {
            let mut b = if n == 5 {
                g0.clone()
            } else {
                selftest::golden_run(&r).unwrap()
            };
            if n == 4 {
                b.run = Some(
                    serde_json::json!({"harness": "claude-code", "timed_out": true, "elapsed_ms": 5_400_000}),
                );
            }
            write_bundle(&bundles.join(format!("claude-code-{n}")), &b).unwrap();
        }
        let batch = tmp.join("batch");
        let s = run(
            &h79,
            &["claude-code".into()],
            5,
            &batch,
            &Driver::Replay(bundles),
        )
        .unwrap();
        let cc = &s.clients[0];
        assert_eq!(
            (cc.runs, cc.passes, cc.provisional, cc.fails),
            (5, 0, 3, 2),
            "{cc:?}"
        );
        assert!(!cc.client_passes);
        assert!(!s.gate_eligible);
        assert!(
            cc.ineligible_because
                .iter()
                .any(|w| w.contains("provisional")),
            "{:?}",
            cc.ineligible_because
        );
        assert!(
            s.ineligible_because
                .iter()
                .any(|w| w.contains("not frozen")),
            "{:?}",
            s.ineligible_because
        );
        let r4: RunResult =
            serde_json::from_slice(&std::fs::read(batch.join("claude-code-4.json")).unwrap())
                .unwrap();
        assert_eq!(r4.failed, ["G0"], "a timeout is a failed run");
        assert!(batch.join("batch.json").exists() && batch.join("preregistration.json").exists());
        // the live driver refuses until A2 and names the command
        let e = run(&h79, &["codex".into()], 5, &tmp.join("live"), &Driver::Live).unwrap_err();
        assert!(
            e.contains("gmclip agent run --harness codex") && e.contains("--media"),
            "{e}"
        );
        // runs that left no bundle are failed runs, recorded
        let s = run(
            &h79,
            &["codex".into()],
            5,
            &tmp.join("broken"),
            &Driver::Replay(tmp.join("nothing-here")),
        )
        .unwrap();
        assert_eq!(
            (s.clients[1].runs, s.clients[1].fails),
            (5, 5),
            "{:?}",
            s.clients[1]
        );
        let r1: RunResult =
            serde_json::from_slice(&std::fs::read(tmp.join("broken/codex-1.json")).unwrap())
                .unwrap();
        assert!(r1.failed[0].starts_with("bundle:") && r1.pre_approval && r1.graded_with.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
