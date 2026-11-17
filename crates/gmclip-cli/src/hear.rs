//! `gmclip plan hear`: a RenderPlan's voice mix heard for captions (SPEC §5.10, decision
//! 0022). A plan carries no transcripts, so every source is taken as transcribed: the
//! audit says which parts the rule would reuse and which stretches it transcribes, and
//! the stretches are heard and timed (A.09's long-form measurement).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gmclip_core::planner::heard::{self, HearPolicy, Hearer};
use serde_json::{Value, json};

use crate::error::{CliError, Code};

const RATE: f64 = 48_000.0;

fn secs(n: u64) -> f64 {
    (n as f64 / RATE * 1000.0).round() / 1000.0
}

/// Writes `v` as pretty JSON to `path`.
pub fn write_report(path: &Path, v: &Value) -> Result<(), CliError> {
    let mut t = serde_json::to_string_pretty(v)
        .map_err(|_| CliError::new(Code::Internal).step("report"))?;
    t.push('\n');
    std::fs::write(path, t).map_err(|e| CliError::io("write", &e))
}

pub fn plan_hear(
    plan: &gmclip_plan::Plan,
    policy: HearPolicy,
    report: Option<&Path>,
) -> Result<Value, CliError> {
    let audit = heard::audit_plan(plan, policy);
    let analysis = crate::analysis::CliAnalysis::new();
    let never = AtomicBool::new(false);
    let hearer = crate::analysis::PlanHearer {
        host: &analysis,
        cancel: &never,
    };
    let refuse = |e: String| {
        CliError::new(Code::Unsupported)
            .step("plan.hear")
            .reason(e.chars().take(160).collect::<String>())
    };
    if !audit.stretches.is_empty() {
        hearer.warm().map_err(refuse)?;
    }
    let t0 = Instant::now();
    let words = hearer.hear(plan, &audit.stretches).map_err(refuse)?;
    let hear_seconds = t0.elapsed().as_secs_f64();
    let total: u64 = audit.parts.iter().map(|p| p.out[1] - p.out[0]).sum();
    let summary = json!({
        "policy": policy,
        "parts": audit.parts.len(),
        "parts_reused": audit.parts.iter().filter(|p| p.identical()).count(),
        "stretches": audit.stretches.len(),
        "seconds": {
            "cut": secs(total),
            "reused": secs(audit.reused_samples()),
            "heard": secs(audit.heard_samples()),
            "transcribed": secs(audit.transcribed_samples()),
        },
        "hear_seconds": (hear_seconds * 1000.0).round() / 1000.0,
        "words": words.iter().map(|w| w.words.len()).sum::<usize>(),
    });
    if let Some(p) = report {
        write_report(
            p,
            &json!({"summary": summary, "audit": audit, "heard": words}),
        )?;
    }
    Ok(summary)
}
