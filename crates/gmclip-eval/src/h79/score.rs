//! Scoring 5 pre-registered runs (SPEC §17.1.1 "Scoring 5 runs"; backlog §2.5, E04).
//!
//! - A batch is one directory `evals/results/h79/<batch>/` holding the batch's
//!   `preregistration.json` (copied from `evals/h79/preregistration.json` before the first
//!   run) and one result per attempt: `<client>-<n>.json`, then `<client>-<n>.retry<k>.json`
//!   for infrastructure reruns. Any other `.json` (except `batch.json`) makes the batch
//!   ineligible: results are never picked by file name.
//! - A file's name must match its content (`client`, `n`). A retry counts only when the
//!   attempt before it was an `infra_failure` (auth expired, network 5xx before the first
//!   tool call, a lab reboot); at most `infra_rerun_max` infra failures per batch. A gmclip
//!   crash, hang or timeout is a **failed run**, never infra.
//! - The outcome is re-derived from the result's `failed`/`pending` lists; a file whose
//!   written `outcome` disagrees makes the batch ineligible.
//! - Every result carries the sha256 of the truth, the grader source and the fixture
//!   manifest; they must equal the registration's (frozen at `m1-eval-freeze`). A batch
//!   graded before the freeze, or with `truth.json` not yet approved by AJ, is scored but
//!   is not gate-eligible.
//! - Eligibility is decided per client (the M1 core exit needs Claude Code alone; Codex
//!   is the follow-up exit), and a client passes with ≥ `pass_threshold` passing runs.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::grade::{Report, Status, Verdict};

/// `evals/h79/preregistration.json`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Preregistration {
    pub schema: String,
    /// The gate batch is the first complete batch after this tag.
    pub freeze_tag: String,
    pub clients: Vec<String>,
    pub runs_per_client: usize,
    pub pass_threshold: usize,
    /// `interleaved`: CC1, CX1, CC2, CX2, …
    pub order: String,
    pub infra_rerun_max: usize,
    /// Filled at the freeze (AJ's go): the grader inputs every result must carry.
    #[serde(default)]
    pub frozen: Option<Frozen>,
}

/// The grader inputs a batch is pinned to.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Frozen {
    pub truth_sha256: String,
    pub grader_sha256: String,
    pub fixture_manifest_sha256: String,
}

/// One attempt's result file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunResult {
    pub schema: String,
    pub client: String,
    pub n: usize,
    /// `pass` | `provisional` | `fail` | `infra_failure`.
    pub outcome: String,
    #[serde(default)]
    pub infra_reason: Option<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    #[serde(default)]
    pub pending: Vec<String>,
    /// `{check id: {status, value}}`.
    #[serde(default)]
    pub checks: BTreeMap<String, Value>,
    #[serde(default)]
    pub transcript_sha256: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens: Option<Value>,
    #[serde(default)]
    pub wall_clock_ms: Option<u64>,
    /// The grader inputs this result was graded with.
    #[serde(default)]
    pub graded_with: Option<Frozen>,
    /// `truth.json` was not yet approved by AJ when this was graded.
    #[serde(default)]
    pub pre_approval: bool,
}

/// What the result's lists say the outcome is.
fn derived_outcome(r: &RunResult) -> &'static str {
    if !r.failed.is_empty() {
        "fail"
    } else if !r.pending.is_empty() {
        "provisional"
    } else {
        "pass"
    }
}

impl RunResult {
    /// The result file for a graded run.
    pub fn from_report(
        client: &str,
        n: usize,
        rep: &Report,
        run: Option<&Value>,
        transcript: Option<&str>,
    ) -> Self {
        let outcome = match rep.verdict {
            Verdict::Pass => "pass",
            Verdict::Provisional => "provisional",
            Verdict::Fail => "fail",
        };
        let checks = rep
            .checks
            .iter()
            .map(|c| {
                (
                    c.id.to_owned(),
                    serde_json::json!({"status": c.status, "value": c.value}),
                )
            })
            .collect();
        let model = run.and_then(|r| {
            let argv: Vec<&str> = r["argv"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            argv.windows(2)
                .find(|w| w[0] == "--model" || w[0] == "-m")
                .map(|w| w[1].to_owned())
        });
        Self {
            schema: "gmclip.eval.h79.result/0".into(),
            client: client.into(),
            n,
            outcome: outcome.into(),
            infra_reason: None,
            failed: rep.failed.iter().map(|s| (*s).to_owned()).collect(),
            pending: rep.pending.iter().map(|s| (*s).to_owned()).collect(),
            checks,
            transcript_sha256: transcript.map(|t| gmclip_ai::launcher::sha256::hex(t.as_bytes())),
            client_version: run
                .and_then(|r| r.pointer("/binary/version_output"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            model,
            tokens: run.and_then(|r| r.get("usage")).cloned(),
            wall_clock_ms: run.and_then(|r| r["elapsed_ms"].as_u64()),
            graded_with: None,
            pre_approval: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ClientScore {
    pub client: String,
    pub runs: usize,
    pub passes: usize,
    pub provisional: usize,
    pub fails: usize,
    pub missing: Vec<usize>,
    pub client_passes: bool,
    /// This client's runs may count toward its gate.
    pub eligible: bool,
    pub ineligible_because: Vec<String>,
    /// `{check: share of counted runs where it passed}`.
    pub check_pass_rate: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct BatchScore {
    pub schema: &'static str,
    pub batch: String,
    pub clients: Vec<ClientScore>,
    pub infra_failures: Vec<String>,
    /// Every registered client is eligible.
    pub gate_eligible: bool,
    /// Reasons that apply to the whole batch.
    pub ineligible_because: Vec<String>,
}

/// The run order a registration implies (`claude-code-1`, `codex-1`, `claude-code-2`, …).
pub fn run_order(p: &Preregistration) -> Vec<(String, usize)> {
    let mut v = Vec::new();
    for n in 1..=p.runs_per_client {
        for c in &p.clients {
            v.push((c.clone(), n));
        }
    }
    v
}

/// `(client, n, retry k)` from a result file name, against the registration.
fn parse_name(name: &str, p: &Preregistration) -> Option<(String, usize, usize)> {
    let stem = name.strip_suffix(".json")?;
    let (base, k) = match stem.split_once(".retry") {
        Some((b, k)) => (b, k.parse::<usize>().ok().filter(|k| *k >= 1)?),
        None => (stem, 0),
    };
    let (client, n) = base.rsplit_once('-')?;
    let n: usize = n
        .parse()
        .ok()
        .filter(|n| (1..=p.runs_per_client).contains(n))?;
    if !p.clients.iter().any(|c| c == client) || n.to_string() != base.rsplit_once('-')?.1 {
        return None;
    }
    Some((client.to_owned(), n, k))
}

/// Scores the batch in `dir` against the registration `expected` (the repo's copy).
pub fn score(dir: &Path, expected: &Preregistration) -> Result<BatchScore, String> {
    let mut why = Vec::new();
    let reg: Option<Preregistration> = std::fs::read(dir.join("preregistration.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    match &reg {
        None => why.push(
            "no preregistration.json in the batch (registered before the first run)".to_owned(),
        ),
        Some(r) if r != expected => {
            why.push("the batch's registration differs from evals/h79/preregistration.json".into())
        }
        _ => {}
    }
    if expected.frozen.is_none() {
        why.push("the grader is not frozen yet (no hashes in the registration; AJ's go at m1-eval-freeze)".into());
    }
    let mut attempts: BTreeMap<(String, usize), BTreeMap<usize, RunResult>> = BTreeMap::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") || name == "preregistration.json" || name == "batch.json" {
            continue;
        }
        let Some((client, n, k)) = parse_name(&name, expected) else {
            why.push(format!(
                "{name}: not a result file name this registration allows"
            ));
            continue;
        };
        let r: RunResult = match std::fs::read(e.path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(r) => r,
            None => {
                why.push(format!("{name}: unreadable result"));
                continue;
            }
        };
        if r.client != client || r.n != n {
            why.push(format!("{name}: names {}-{} inside", r.client, r.n));
            continue;
        }
        attempts.entry((client, n)).or_default().insert(k, r);
    }
    let mut infra = Vec::new();
    let mut clients = Vec::new();
    for c in &expected.clients {
        let mut cwhy = Vec::new();
        let mut s = ClientScore {
            client: c.clone(),
            runs: 0,
            passes: 0,
            provisional: 0,
            fails: 0,
            missing: Vec::new(),
            client_passes: false,
            eligible: false,
            ineligible_because: Vec::new(),
            check_pass_rate: BTreeMap::new(),
        };
        let mut check_passes: BTreeMap<String, usize> = BTreeMap::new();
        for n in 1..=expected.runs_per_client {
            let Some(list) = attempts.get(&(c.clone(), n)) else {
                s.missing.push(n);
                continue;
            };
            // attempts must be 0, 1, 2 … with every earlier one an infra failure
            let ks: Vec<usize> = list.keys().copied().collect();
            if ks != (0..ks.len()).collect::<Vec<_>>() {
                cwhy.push(format!(
                    "{c}-{n}: attempts {ks:?} are not consecutive from the first"
                ));
            }
            for (k, r) in list {
                if r.outcome == "infra_failure" {
                    infra.push(format!(
                        "{c}-{n} attempt {k}: {}",
                        r.infra_reason.as_deref().unwrap_or("?")
                    ));
                } else if *k + 1 < list.len() {
                    cwhy.push(format!(
                        "{c}-{n}: attempt {k} was {} and was rerun anyway",
                        r.outcome
                    ));
                }
            }
            let Some(last) = list.values().last() else {
                continue;
            };
            if last.outcome == "infra_failure" {
                s.missing.push(n);
                continue;
            }
            let outcome = derived_outcome(last);
            if outcome != last.outcome {
                cwhy.push(format!(
                    "{c}-{n}: written outcome {} contradicts its failed/pending lists",
                    last.outcome
                ));
            }
            match (&expected.frozen, &last.graded_with) {
                (Some(f), Some(g)) if f == g => {}
                (Some(_), _) => cwhy.push(format!(
                    "{c}-{n}: graded with other grader inputs than the frozen ones"
                )),
                (None, _) => {}
            }
            if last.pre_approval {
                cwhy.push(format!("{c}-{n}: graded before AJ approved truth.json"));
            }
            s.runs += 1;
            match outcome {
                "pass" => s.passes += 1,
                "provisional" => s.provisional += 1,
                _ => s.fails += 1,
            }
            for (id, v) in &last.checks {
                let passed = v["status"] == serde_json::to_value(Status::Pass).unwrap_or_default();
                *check_passes.entry(id.clone()).or_default() += usize::from(passed);
            }
        }
        for (id, k) in check_passes {
            s.check_pass_rate.insert(
                id,
                if s.runs == 0 {
                    0.0
                } else {
                    k as f64 / s.runs as f64
                },
            );
        }
        s.client_passes = s.passes >= expected.pass_threshold;
        if !s.missing.is_empty() {
            cwhy.push(format!("{c}: runs {:?} missing (incomplete)", s.missing));
        }
        if s.provisional > 0 {
            cwhy.push(format!(
                "{c}: {} provisional run(s) (a MUST check still pending)",
                s.provisional
            ));
        }
        s.ineligible_because = cwhy;
        clients.push(s);
    }
    if infra.len() > expected.infra_rerun_max {
        why.push(format!(
            "{} infrastructure failures (max {} reruns)",
            infra.len(),
            expected.infra_rerun_max
        ));
    }
    for s in &mut clients {
        s.eligible = why.is_empty() && s.ineligible_because.is_empty();
    }
    Ok(BatchScore {
        schema: "gmclip.eval.h79.batch/0",
        batch: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        gate_eligible: clients.iter().all(|c| c.eligible),
        clients,
        infra_failures: infra,
        ineligible_because: why,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen() -> Frozen {
        Frozen {
            truth_sha256: "t".into(),
            grader_sha256: "g".into(),
            fixture_manifest_sha256: "f".into(),
        }
    }

    fn reg() -> Preregistration {
        let mut r: Preregistration = serde_json::from_slice(
            &std::fs::read(super::super::bundle::h79_dir().join("preregistration.json")).unwrap(),
        )
        .unwrap();
        r.frozen = Some(frozen());
        r
    }

    fn result(client: &str, n: usize, outcome: &str) -> RunResult {
        let status = if outcome == "pass" { "pass" } else { "fail" };
        RunResult {
            schema: "gmclip.eval.h79.result/0".into(),
            client: client.into(),
            n,
            outcome: outcome.into(),
            infra_reason: (outcome == "infra_failure").then(|| "auth expired".into()),
            failed: if outcome == "fail" {
                vec!["G2".into()]
            } else {
                vec![]
            },
            pending: if outcome == "provisional" {
                vec!["G8".into()]
            } else {
                vec![]
            },
            checks: [(
                "G2".to_owned(),
                serde_json::json!({"status": status, "value": 60.0}),
            )]
            .into_iter()
            .collect(),
            transcript_sha256: None,
            client_version: None,
            model: None,
            tokens: None,
            wall_clock_ms: None,
            graded_with: Some(frozen()),
            pre_approval: false,
        }
    }

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn batch(tag: &str, files: &[(String, RunResult)], p: &Preregistration) -> Tmp {
        let d = std::env::temp_dir().join(format!("gmclip-h79-batch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("preregistration.json"),
            serde_json::to_vec(p).unwrap(),
        )
        .unwrap();
        for (name, r) in files {
            std::fs::write(d.join(name), serde_json::to_vec(r).unwrap()).unwrap();
        }
        Tmp(d)
    }

    fn full(cc_pass: usize, cx_pass: usize) -> Vec<(String, RunResult)> {
        let mut v = Vec::new();
        for n in 1..=5 {
            v.push((
                format!("claude-code-{n}.json"),
                result("claude-code", n, if n <= cc_pass { "pass" } else { "fail" }),
            ));
            v.push((
                format!("codex-{n}.json"),
                result("codex", n, if n <= cx_pass { "pass" } else { "fail" }),
            ));
        }
        v
    }

    #[test]
    fn three_of_five_passes_and_order_is_interleaved() {
        let p = reg();
        assert_eq!((p.runs_per_client, p.pass_threshold), (5, 3));
        let order = run_order(&p);
        assert_eq!(
            order[..2],
            [("claude-code".to_owned(), 1), ("codex".to_owned(), 1)]
        );
        let t = batch("ok", &full(3, 2), &p);
        let s = score(&t.0, &p).unwrap();
        assert!(
            s.gate_eligible,
            "{:?} {:?}",
            s.ineligible_because, s.clients
        );
        assert!(s.clients[0].client_passes && s.clients[0].passes == 3);
        assert!(!s.clients[1].client_passes && s.clients[1].passes == 2);
        assert_eq!(s.clients[0].check_pass_rate["G2"], 0.6);
    }

    /// The reviewer's exploit: 2 real passes, a "retry" after a genuine fail, and a stray
    /// file. None of it can lift the client to a pass.
    #[test]
    fn results_cannot_be_picked_after_the_fact() {
        let p = reg();
        let mut files = full(2, 0);
        files.push((
            "claude-code-3.retry1.json".into(),
            result("claude-code", 3, "pass"),
        ));
        let mut stray = result("claude-code", 4, "pass");
        stray.failed = vec!["G3".into()];
        files.push(("zz-anything-at-all.json".into(), stray.clone()));
        let mut lying = result("claude-code", 5, "pass");
        lying.failed = vec!["G9".into()];
        files.retain(|(n, _)| n != "claude-code-5.json");
        files.push(("claude-code-5.json".into(), lying));
        files.push(("claude-code-1.json.bak.json".into(), stray));
        let t = batch("pick", &files, &p);
        let s = score(&t.0, &p).unwrap();
        let cc = &s.clients[0];
        assert!(!s.gate_eligible);
        assert!(!cc.eligible, "{cc:?}");
        assert!(
            s.ineligible_because
                .iter()
                .any(|w| w.contains("zz-anything")),
            "{:?}",
            s.ineligible_because
        );
        assert!(
            cc.ineligible_because
                .iter()
                .any(|w| w.contains("rerun anyway")),
            "{:?}",
            cc.ineligible_because
        );
        assert!(
            cc.ineligible_because
                .iter()
                .any(|w| w.contains("contradicts")),
            "{:?}",
            cc.ineligible_because
        );
        // the lying claude-code-5 counts as the fail its lists say
        assert!(cc.passes <= 3);
    }

    #[test]
    fn infra_reruns_count_the_last_attempt_and_are_capped() {
        let p = reg();
        let mut files = full(5, 0);
        files.retain(|(n, _)| n != "claude-code-1.json");
        files.push((
            "claude-code-1.json".into(),
            result("claude-code", 1, "infra_failure"),
        ));
        files.push((
            "claude-code-1.retry1.json".into(),
            result("claude-code", 1, "pass"),
        ));
        let t = batch("infra", &files, &p);
        let s = score(&t.0, &p).unwrap();
        assert!(
            s.gate_eligible,
            "{:?} {:?}",
            s.ineligible_because, s.clients
        );
        assert_eq!(s.clients[0].passes, 5);
        assert_eq!(s.infra_failures.len(), 1);
        files.retain(|(n, _)| n != "claude-code-2.json");
        files.push((
            "claude-code-2.json".into(),
            result("claude-code", 2, "infra_failure"),
        ));
        files.push((
            "claude-code-2.retry1.json".into(),
            result("claude-code", 2, "infra_failure"),
        ));
        let t = batch("infra3", &files, &p);
        let s = score(&t.0, &p).unwrap();
        assert!(!s.gate_eligible);
        assert!(s.clients[0].missing.contains(&2));
    }

    #[test]
    fn eligibility_is_per_client_and_needs_the_freeze() {
        let p = reg();
        let only_cc: Vec<_> = full(4, 0)
            .into_iter()
            .filter(|(n, _)| n.starts_with("claude-code"))
            .collect();
        let t = batch("cc-only", &only_cc, &p);
        let s = score(&t.0, &p).unwrap();
        assert!(
            s.clients[0].eligible && s.clients[0].client_passes,
            "{:?}",
            s.clients[0]
        );
        assert!(!s.clients[1].eligible && !s.gate_eligible);
        // before the freeze nothing is eligible
        let mut unfrozen = p.clone();
        unfrozen.frozen = None;
        let t = batch("unfrozen", &only_cc, &unfrozen);
        let s = score(&t.0, &unfrozen).unwrap();
        assert!(!s.clients[0].eligible);
        // a provisional run is never a pass
        let mut files = full(5, 5);
        files[0].1 = result("claude-code", 1, "provisional");
        let t = batch("prov", &files, &p);
        let s = score(&t.0, &p).unwrap();
        assert!(!s.clients[0].eligible && s.clients[0].passes == 4);
    }
}
