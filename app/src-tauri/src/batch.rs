//! "Send an agent to these N cuts" (E.01 overnight batch + U.16; SPEC §10.7.1, R-AGT-5).
//!
//! One batch, one budget (the user's own plan: $0 to gmclip), one wall clock (8 h by
//! default), one run per cut, **one at a time**:
//! - each run is an ordinary launcher run (R-AGT-3: started by this human click, through
//!   the launcher), so its credential is `launched:<run>`, bound to that cut, refused on
//!   any other cut and after its own 90-minute wall clock (the holder enforces both), and
//!   it begins with a restore point;
//! - a cut the human edited after the batch started is skipped ("skipped: you edited it"):
//!   its fingerprint (every entity revision under the cut) is taken at the start and
//!   compared before its run;
//! - when the batch's wall clock runs out, the run in flight is stopped (its launched
//!   credential ended; its own wall clock never outlasts the batch's) and reported
//!   `stopped`, the cuts after it `not_run`, and the batch `stopped`.
//!
//! Starting a batch is an Owner decision, asked once on the native sheet.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gmclip_model::entities::Cut;
use serde::Serialize;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The default wall clock for a whole batch.
pub const WALL_CLOCK: Duration = Duration::from_secs(8 * 3600);
/// The default (and longest) wall clock of one cut's run.
pub const PER_CUT: Duration = Duration::from_secs(90 * 60);
/// A budget above this asks for its own confirmation on the launch sheet (§10.7.1).
pub const BUDGET_CONFIRM_USD: f64 = 5.0;

/// What the person set on the batch sheet (SPEC §10.7.1): one budget, one wall clock, the
/// per-cut limit and which agent (decision 0012). The pre-approval for AI passes within
/// the budget is recorded at launch from these.
#[derive(Clone, Debug)]
pub struct BatchOptions {
    pub wall_clock: Duration,
    pub per_cut: Duration,
    /// Dollars the batch may spend on metered AI (`0` on the person's own plan).
    pub budget_usd: f64,
    /// `claude-code`, `codex`, `pi`, `opencode`; the default agent when `None`.
    pub harness: Option<String>,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            wall_clock: WALL_CLOCK,
            per_cut: PER_CUT,
            budget_usd: 0.0,
            harness: None,
        }
    }
}

impl BatchOptions {
    /// Checks the sheet's numbers: wall clock 10 min–24 h, per cut 5–90 min and not
    /// longer than the batch, budget $0–$100, a known agent.
    pub fn check(&self) -> AppResult<()> {
        let w = self.wall_clock.as_secs();
        if !(600..=24 * 3600).contains(&w) {
            return Err(AppError::invalid(
                "the batch's wall clock is 10 minutes to 24 hours",
            ));
        }
        let c = self.per_cut.as_secs();
        if !(300..=PER_CUT.as_secs()).contains(&c) || c > w {
            return Err(AppError::invalid(
                "a cut's run is 5 to 90 minutes, and no longer than the batch",
            ));
        }
        if !self.budget_usd.is_finite() || !(0.0..=100.0).contains(&self.budget_usd) {
            return Err(AppError::invalid("a budget of $0 to $100"));
        }
        if let Some(h) = &self.harness
            && gmclip_ai::launcher::Harness::parse(h).is_none()
        {
            return Err(AppError::invalid(
                "harness is claude-code, codex, pi or opencode",
            ));
        }
        Ok(())
    }
}

/// The pre-approval a batch records at launch (§10.7.1): AI passes within the budget
/// need no further approval while it runs; anything over the budget still does.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct PreApproval {
    pub covers: Vec<String>,
    pub budget_usd: f64,
    pub recorded_ms: i64,
    pub by: String,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct BatchCut {
    pub cut: String,
    pub title: String,
    /// `queued`, `running`, `done`, `failed`, `skipped` (edited after the start),
    /// `stopped` (the wall clock ran out mid-run) or `not_run` (never reached).
    pub state: String,
    pub run: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct BatchView {
    pub id: String,
    pub project: String,
    pub pass: String,
    pub started_ms: i64,
    pub wall_clock_s: u64,
    /// What the batch may spend on metered AI (`0`: the person's own plan).
    pub budget_usd: f64,
    /// One cut's run at most.
    pub per_cut_s: u64,
    /// The agent each cut runs with (decision 0012).
    pub harness: Option<String>,
    pub preapproval: PreApproval,
    pub cuts: Vec<BatchCut>,
    /// `running`, `done`, or `stopped` (its wall clock ran out before every cut ran).
    pub state: String,
}

/// The batches started in this app session.
#[derive(Debug, Default)]
pub struct Batches {
    list: Mutex<BTreeMap<String, BatchView>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A cut's fingerprint: the revisions of the cut and everything under it.
pub fn fingerprint(core: &AppCore, project: &str, cut: &str) -> Option<String> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut).ok()?;
    let root = cid.entity();
    let mut revs: Vec<(String, u64)> = doc
        .entity_ids()
        .into_iter()
        .filter(|e| {
            let mut at = Some(*e);
            while let Some(x) = at {
                if x == root {
                    return true;
                }
                at = doc.parent_of(x);
            }
            false
        })
        .filter_map(|e| doc.entity_rev(e).map(|r| (e.to_string(), r)))
        .collect();
    revs.sort();
    Some(format!("{revs:?}"))
}

/// How one cut's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CutEnd {
    /// It finished (its run id).
    Done(String),
    /// It failed (the run's own message).
    Failed(String),
    /// The batch's wall clock ran out while it ran: it was stopped (its run id, if it had
    /// one). Its restore point stays.
    Stopped(Option<String>),
}

/// Runs one cut and waits for its run to end, stopping it at `deadline` (the batch's
/// wall clock) if it is still running then.
pub type RunOne = dyn Fn(&str, Instant) -> CutEnd + Send + Sync;

/// What a cut the batch never reached says.
pub const NOT_RUN: &str = "not run: the batch's wall clock ran out";
/// What a cut stopped mid-run says.
pub const STOPPED: &str =
    "stopped: the batch's wall clock ran out while it ran (its restore point is kept)";

/// Starts a batch over `cuts` (in order) with `pass`, after the Owner's sheet.
pub fn start(
    core: &Arc<AppCore>,
    project: &str,
    cuts: &[String],
    pass: &str,
    opts: &BatchOptions,
) -> AppResult<Value> {
    check_args(cuts, pass)?;
    opts.check()?;
    // R-PROF-3: style-file changes are offered once, batched, before the batch's agents
    // start; declined, they run on the version accepted before
    let _ = crate::owner::style_changes_sheet(core, false);
    let (title, text) = sheet_text(cuts.len(), pass, opts);
    if !crate::sheet::confirm(&title, &text, "Start the batch") {
        return Err(AppError::new("Unauthorized", "the batch was not approved"));
    }
    start_opts(
        core,
        project,
        cuts,
        pass,
        opts,
        real_runner(core, project, pass, opts),
    )
}

fn check_args(cuts: &[String], pass: &str) -> AppResult<()> {
    if cuts.is_empty() || cuts.len() > 50 {
        return Err(AppError::invalid("1–50 cuts"));
    }
    if !matches!(pass, "edit_pass" | "grow_story") {
        return Err(AppError::invalid(
            "a batch runs the edit pass or grow story",
        ));
    }
    Ok(())
}

fn hm(d: Duration) -> String {
    let m = d.as_secs() / 60;
    match (m / 60, m % 60) {
        (h, r) if h < 2 && (h == 0 || r != 0) => format!("{m} minutes"),
        (h, 0) => format!("{h} hour{}", if h == 1 { "" } else { "s" }),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The launch sheet (R-OWN-1): what the batch does, its limits and its pre-approval; a
/// budget over $5 is said in the title and needs this same confirmation (§10.7.1).
pub fn sheet_text(n: usize, pass: &str, opts: &BatchOptions) -> (String, String) {
    let agent = opts
        .harness
        .as_deref()
        .and_then(gmclip_ai::launcher::Harness::parse)
        .map_or("your default agent", gmclip_ai::launcher::Harness::label);
    let spend = if opts.budget_usd > 0.0 {
        format!(
            "• It may spend up to ${:.2} on metered AI. AI passes within that are approved now, for this batch only; anything over it stops and waits for you.",
            opts.budget_usd
        )
    } else {
        "• It runs on your own plan: gmclip spends nothing. AI passes are approved now, for this batch only, at $0.".to_owned()
    };
    let text = format!(
        "• {agent} works on {n} cut{s} of this project, one at a time, with the {brief} brief.\n• Each run may change only its own cut, for {per} at most; the whole batch stops after {all}.\n{spend}\n• A cut you edit after this starts is skipped. Every run starts with a restore point.",
        s = if n == 1 { "" } else { "s" },
        brief = if pass == "edit_pass" {
            "editing pass"
        } else {
            "grow story"
        },
        per = hm(opts.per_cut),
        all = hm(opts.wall_clock),
    );
    let title = if opts.budget_usd > BUDGET_CONFIRM_USD {
        format!(
            "Send your agent to these cuts, with a ${:.2} budget?",
            opts.budget_usd
        )
    } else {
        "Send your agent to these cuts?".to_owned()
    };
    (title, text)
}

/// Each cut through the launcher (`agent::run_pass`, R-AGT-3), waiting for its run.
pub fn real_runner(
    core: &Arc<AppCore>,
    project: &str,
    pass: &str,
    opts: &BatchOptions,
) -> Arc<RunOne> {
    let weak = Arc::downgrade(core);
    let (proj, p2) = (project.to_owned(), pass.to_owned());
    let harness = opts
        .harness
        .as_deref()
        .and_then(gmclip_ai::launcher::Harness::parse);
    let per_cut = opts.per_cut;
    Arc::new(move |cut: &str, deadline: Instant| {
        let Some(core) = weak.upgrade() else {
            return CutEnd::Failed("the app is closing".into());
        };
        // the run's own wall clock ends no later than the batch's (the holder refuses its
        // credential after it)
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return CutEnd::Stopped(None);
        }
        let r = match crate::agent::run_pass_within(
            &core,
            &proj,
            cut,
            &p2,
            "",
            false,
            harness,
            Some(per_cut.min(left)),
        ) {
            Ok(r) => r,
            Err(e) => return CutEnd::Failed(e.message),
        };
        let id = r["run"]
            .as_str()
            .or_else(|| r["id"].as_str())
            .unwrap_or_default()
            .to_owned();
        wait_run(&core, &id, deadline)
    })
}

/// Waits for the app run `id` to end; at `deadline` it is stopped (its launched
/// credential ended, so every further call it makes is refused) and reported stopped.
pub fn wait_run(core: &AppCore, id: &str, deadline: Instant) -> CutEnd {
    loop {
        let Some(v) = core.agents.run(id) else {
            return CutEnd::Failed("the run disappeared".into());
        };
        match v.state.as_str() {
            "done" => return CutEnd::Done(id.to_owned()),
            "failed" => return CutEnd::Failed(format!("{id}: {}", v.error.unwrap_or_default())),
            _ if Instant::now() >= deadline => {
                if let Some(l) = v.launched() {
                    let _ = crate::board::stop_run(core, l);
                }
                return CutEnd::Stopped(Some(id.to_owned()));
            }
            _ => std::thread::sleep(
                Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
            ),
        }
    }
}

/// Dev bridge builds only: a batch without the sheet (the e2e), through the launcher.
#[cfg(feature = "dev-bridge")]
pub fn dev_start(
    core: &Arc<AppCore>,
    project: &str,
    cuts: &[String],
    pass: &str,
    opts: &BatchOptions,
) -> AppResult<Value> {
    check_args(cuts, pass)?;
    opts.check()?;
    start_opts(
        core,
        project,
        cuts,
        pass,
        opts,
        real_runner(core, project, pass, opts),
    )
}

/// [`start`] without the sheet, with the runner and wall clock given (tests).
pub fn start_with(
    core: &Arc<AppCore>,
    project: &str,
    cuts: &[String],
    pass: &str,
    wall_clock: Duration,
    run_one: Arc<RunOne>,
) -> AppResult<Value> {
    let opts = BatchOptions {
        wall_clock,
        per_cut: PER_CUT.min(wall_clock),
        ..BatchOptions::default()
    };
    start_opts(core, project, cuts, pass, &opts, run_one)
}

/// [`start`] after the sheet: the batch with its options and runner.
pub fn start_opts(
    core: &Arc<AppCore>,
    project: &str,
    cuts: &[String],
    pass: &str,
    opts: &BatchOptions,
    run_one: Arc<RunOne>,
) -> AppResult<Value> {
    let wall_clock = opts.wall_clock;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let mut rows = Vec::new();
    let mut prints = Vec::new();
    for c in cuts {
        let cid = ulid_id::<Cut>(c)?;
        let title = doc
            .get(cid)
            .map(|x| x.title.clone())
            .ok_or_else(|| AppError::not_found("a cut that is not in the project"))?;
        prints.push(fingerprint(core, project, c));
        rows.push(BatchCut {
            cut: c.clone(),
            title,
            state: "queued".into(),
            run: None,
            reason: None,
        });
    }
    let id = format!("b{}", now_ms());
    let view = BatchView {
        id: id.clone(),
        project: project.to_owned(),
        pass: pass.to_owned(),
        started_ms: now_ms(),
        wall_clock_s: wall_clock.as_secs(),
        budget_usd: opts.budget_usd,
        per_cut_s: opts.per_cut.as_secs(),
        harness: opts.harness.clone(),
        preapproval: PreApproval {
            covers: vec!["ai_pass".into()],
            budget_usd: opts.budget_usd,
            recorded_ms: now_ms(),
            by: "human".into(),
        },
        cuts: rows,
        state: "running".into(),
    };
    if let Ok(mut g) = core.batches.list.lock() {
        g.insert(id.clone(), view.clone());
    }
    let weak = Arc::downgrade(core);
    let (bid, proj) = (id.clone(), project.to_owned());
    std::thread::Builder::new()
        .name("gmclip-batch".into())
        .spawn(move || {
            let deadline = Instant::now() + wall_clock;
            let mut ran_out = false;
            let update = |f: &dyn Fn(&mut BatchView)| {
                let Some(core) = weak.upgrade() else { return };
                let snap = core.batches.list.lock().ok().and_then(|mut g| {
                    g.get_mut(&bid).map(|b| {
                        f(b);
                        b.clone()
                    })
                });
                if let Some(b) = snap {
                    core.bus.project(
                        &proj,
                        crate::events::ProjectEvent::Batch { batch: Box::new(b) },
                    );
                }
            };
            for (k, before) in prints.iter().enumerate() {
                let Some(core) = weak.upgrade() else { return };
                let cut = core
                    .batches
                    .list
                    .lock()
                    .ok()
                    .and_then(|g| g.get(&bid).map(|b| b.cuts[k].cut.clone()))
                    .unwrap_or_default();
                if ran_out || Instant::now() >= deadline {
                    ran_out = true;
                    update(&|b| {
                        b.cuts[k].state = "not_run".into();
                        b.cuts[k].reason = Some(NOT_RUN.into());
                    });
                    continue;
                }
                if fingerprint(&core, &proj, &cut) != *before {
                    update(&|b| {
                        b.cuts[k].state = "skipped".into();
                        b.cuts[k].reason = Some("skipped: you edited it".into());
                    });
                    continue;
                }
                update(&|b| b.cuts[k].state = "running".into());
                drop(core);
                let r = run_one(&cut, deadline);
                ran_out |= matches!(r, CutEnd::Stopped(_));
                update(&|b| match &r {
                    CutEnd::Done(run) => {
                        b.cuts[k].state = "done".into();
                        b.cuts[k].run = Some(run.clone());
                    }
                    CutEnd::Failed(e) => {
                        b.cuts[k].state = "failed".into();
                        b.cuts[k].reason = Some(e.chars().take(300).collect());
                    }
                    CutEnd::Stopped(run) => {
                        b.cuts[k].state = "stopped".into();
                        b.cuts[k].run = run.clone();
                        b.cuts[k].reason = Some(STOPPED.into());
                    }
                });
            }
            // `stopped`: the wall clock ended it (a cut stopped or never reached)
            update(&|b| b.state = if ran_out { "stopped" } else { "done" }.into());
        })
        .map_err(|_| AppError::internal("the batch thread"))?;
    Ok(serde_json::to_value(view).unwrap_or(Value::Null))
}

/// The batches still running, in any project (Home's running work).
pub fn running(core: &AppCore) -> Vec<BatchView> {
    core.batches
        .list
        .lock()
        .map(|g| {
            g.values()
                .filter(|b| b.state == "running")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The batches of a project, newest first.
pub fn list(core: &AppCore, project: &str) -> Value {
    let mut v: Vec<BatchView> = core
        .batches
        .list
        .lock()
        .map(|g| {
            g.values()
                .filter(|b| b.project == project)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|b| std::cmp::Reverse(b.started_ms));
    json!({"batches": v})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_says_the_limits_and_asks_for_a_budget_over_5() {
        let o = BatchOptions::default();
        assert!(o.check().is_ok());
        let (title, text) = sheet_text(3, "edit_pass", &o);
        assert_eq!(title, "Send your agent to these cuts?");
        assert!(text.contains("90 minutes at most") && text.contains("after 8 hours"));
        assert!(text.contains("approved now, for this batch only, at $0"));
        let o = BatchOptions {
            wall_clock: Duration::from_secs(2 * 3600 + 30 * 60),
            per_cut: Duration::from_secs(40 * 60),
            budget_usd: 12.5,
            harness: Some("pi".into()),
        };
        assert!(o.check().is_ok());
        let (title, text) = sheet_text(1, "grow_story", &o);
        assert!(title.contains("$12.50 budget"), "{title}");
        assert!(text.contains("pi works on 1 cut"), "{text}");
        assert!(text.contains("40 minutes at most") && text.contains("after 2 h 30 min"));
        assert!(text.contains("up to $12.50"));
        let (title, _) = sheet_text(
            1,
            "edit_pass",
            &BatchOptions {
                budget_usd: 5.0,
                ..o.clone()
            },
        );
        assert!(!title.contains("budget"), "$5 needs no extra confirmation");
    }

    #[test]
    fn options_out_of_range_are_refused() {
        let ok = BatchOptions::default();
        for bad in [
            BatchOptions {
                wall_clock: Duration::from_secs(60),
                ..ok.clone()
            },
            BatchOptions {
                wall_clock: Duration::from_secs(25 * 3600),
                ..ok.clone()
            },
            BatchOptions {
                per_cut: Duration::from_secs(91 * 60),
                ..ok.clone()
            },
            BatchOptions {
                per_cut: Duration::from_secs(60),
                ..ok.clone()
            },
            BatchOptions {
                wall_clock: Duration::from_secs(20 * 60),
                per_cut: Duration::from_secs(30 * 60),
                ..ok.clone()
            },
            BatchOptions {
                budget_usd: -1.0,
                ..ok.clone()
            },
            BatchOptions {
                budget_usd: f64::NAN,
                ..ok.clone()
            },
            BatchOptions {
                budget_usd: 500.0,
                ..ok.clone()
            },
            BatchOptions {
                harness: Some("bash".into()),
                ..ok.clone()
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
    }
}
