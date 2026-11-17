//! `gmclip` (SPEC §3.1, R-ARCH-5). The M0 subcommands are hand-written clap, run
//! in-process and need no project: `render`, `still`, `plan validate`,
//! `probe`, `remux`. From M1 the registry commands are generated from their schemas
//! (`cargo xt gen`).
//!
//! Every run (A-01, A-02):
//! 1. mints a `trace_id` (ULID) before anything else;
//! 2. logs to rolling JSON files under a `cmd{name, caller, trace_id}` span (`log`);
//! 3. delegates to a library function (`gmclip_plan`, `gmclip_render::ops`,
//!    `gmclip_media::ops`) inside `catch_unwind`;
//! 4. prints one JSON object: the result on stdout (exit 0), or the error on stderr
//!    with its exit code (`error`). Help and `--version` print as text.
#![forbid(unsafe_code)]
// `CliError` is ~150 bytes and only ever built on the way out of a one-shot process;
// boxing it would buy nothing.
#![allow(clippy::result_large_err)]

mod agent;
mod analysis;
mod clippy_cmd;
mod error;
mod hear;
mod host;
mod log;
mod profile_cmd;
mod record_cmd;
mod review_cmd;
mod share_cmd;
mod socket;
mod trace;

use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::error::{CliError, Code};

/// Plans are small; anything bigger is refused rather than read.
const MAX_PLAN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "gmclip", version, about = "gmclip: a video editor with no ffmpeg", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Render a RenderPlan to an MP4 (H-10).
    Render {
        /// RenderPlan JSON (`gmclip.renderplan/0`).
        plan: PathBuf,
        /// Output MP4.
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Render still frames or a contact sheet from a RenderPlan (H-05b).
    Still {
        plan: PathBuf,
        /// Output frame indices.
        #[arg(long, value_delimiter = ',', required = true)]
        at: Vec<u64>,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Plan every cut in a project that has a framing (cuts with no hook id included).
    #[command(name = "dump-plan", hide = true)]
    DumpPlan {
        project: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// The cut whose title contains this (else the first cut with a framing).
        #[arg(long)]
        cut: Option<String>,
    },
    /// Plan a project's cut to a RenderPlan with the model planner (A18).
    #[command(name = "plan-cut")]
    PlanCut {
        /// A project database.
        project: PathBuf,
        /// The cut's hook id (its moment's `origin_ref`).
        #[arg(long)]
        hook: String,
        /// Build the captions instead of keeping the cut's own (`heard`: from the cut's
        /// own audio, SPEC §5.10). The project is not changed.
        #[arg(long, value_enum)]
        captions: Option<PlanCaptions>,
        /// Which parts `--captions heard` transcribes: `rule` (the product: identical
        /// parts reuse the source words), `all` or `none` (measurement).
        #[arg(long, value_enum, default_value_t = HearArg::Rule)]
        hear: HearArg,
        /// Write the caption audit (parts reused, stretches heard, seconds) here.
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// RenderPlan tools.
    Plan {
        #[command(subcommand)]
        command: PlanCmd,
    },
    /// Print a source's tracks, index facts and capability verdict (B-01, B-02).
    Probe { file: PathBuf },
    /// Re-wrap an MP4 sample-for-sample through our demuxer and writer (B-06).
    Remux {
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Agent runs through the isolated launcher (SPEC §10.7).
    Agent {
        #[command(subcommand)]
        command: AgentCmd,
    },
    /// MCP over stdio for agent clients: proxies to the running gmclip, starting one if
    /// none runs (R-ARCH-5, §11.1). Configure your client to run `gmclip mcp`.
    Mcp,
    /// Run the holder: one process hosting projects for agents and the CLI (R-ARCH-3).
    Serve {
        /// No window (the only mode at M1).
        #[arg(long)]
        headless: bool,
        /// Stop after this many idle seconds (default 900, or `GMCLIP_HOLDER_IDLE_S`).
        #[arg(long, hide = true)]
        idle_s: Option<u64>,
        /// Arguments a second launch forwards to the running holder.
        #[arg(trailing_var_arg = true, hide = true)]
        forward: Vec<String>,
    },
    /// What the holder is doing: open projects, connections, first-seen notices.
    Status,
    /// The connection log (R-OWN-4, R-MCP-1c).
    Connections {
        #[arg(long, default_value_t = 50)]
        n: usize,
    },
    /// Stop the running holder (projects close cleanly).
    Stop,
    /// Hand an export to Clippy (paired, human-only; SPEC §5.14.2).
    Clippy {
        #[command(subcommand)]
        command: clippy_cmd::ClippyCmd,
    },
    /// Write a static share page (HTML + MP4, no server) for cuts (R-REV-7).
    Share(share_cmd::ShareArgs),
    /// Review links and the review-link server (SPEC §5.14.1).
    Review {
        #[command(subcommand)]
        command: review_cmd::ReviewCmd,
    },
    /// The style feedback log (SPEC §10.5): read only here — adding feedback is a human
    /// action in the app (R-PROF-2).
    Feedback {
        #[command(subcommand)]
        command: profile_cmd::FeedbackCmd,
    },
    /// Creator profiles (SPEC §10.5): content-only folders.
    Profile {
        #[command(subcommand)]
        command: profile_cmd::ProfileCmd,
    },
    /// Record screens, the camera and the mic as separate files plus the input log
    /// (decision 0045).
    Record {
        #[command(subcommand)]
        command: record_cmd::RecordCmd,
    },
    /// Owner commands: need an interactive terminal and a re-typed code (R-OWN-3).
    Owner {
        #[command(subcommand)]
        command: OwnerCmd,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCmd {
    /// Start your agent (Claude Code, Codex, pi or opencode) on one cut, isolated
    /// (R-AGT-1..5).
    Run {
        /// claude-code, codex, pi or opencode.
        #[arg(long)]
        harness: String,
        /// The cut the run is bound to.
        #[arg(long)]
        cut: String,
        /// The project (default: the holder's open project).
        #[arg(long)]
        project: Option<String>,
        /// The brief, as text.
        #[arg(long, conflicts_with = "brief_file")]
        brief: Option<String>,
        /// The brief, from a UTF-8 file.
        #[arg(long)]
        brief_file: Option<PathBuf>,
        /// Let the agent see frames and hear audio (an Owner grant: asks for a code on this terminal).
        #[arg(long)]
        media: bool,
        /// Let `see` show private regions unmasked (an Owner grant).
        #[arg(long)]
        private_regions: bool,
        /// Wall clock, e.g. 90m.
        #[arg(long, default_value = "90m")]
        wall_clock: String,
        /// Where to copy the transcript and run record.
        #[arg(long)]
        transcript_out: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "high")]
        effort: String,
        /// The client's absolute path (default: searched on PATH, never through a shell).
        #[arg(long)]
        client_path: Option<PathBuf>,
        /// Show the binary, version verdict, tools, argv and env keys; start nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OwnerCmd {
    /// Connect an agent client: writes its MCP entry (and read-only allow-entries).
    Connect {
        #[arg(value_enum)]
        client: ClientKind,
    },
    /// Let agents read under a folder (or write, with --write).
    RootsAdd {
        path: PathBuf,
        #[arg(long)]
        write: bool,
    },
    /// Remove a read or write root.
    RootsRemove { path: PathBuf },
    /// Set the recordings folder (a read root).
    Recordings { path: PathBuf },
    /// Open a project outside the agent read roots.
    OpenAny { path: PathBuf },
    /// Refuse a client from now on (`local:<client>`, as `gmclip connections` shows it).
    Revoke { identity: String },
    /// The style rules file agents read (R-PROF-3/5): pinned by hash, read-only.
    Style {
        #[command(subcommand)]
        command: StyleCmd,
    },
    /// Use a creator profile folder (content only; its suggested settings are shown,
    /// never applied, R-PROF-4).
    Profile { path: PathBuf },
    /// Serve review links through `tailscale serve` at https://<host> (R-REV-1).
    ReviewTailnet {
        /// `<machine>.<tailnet>.ts.net`
        #[arg(long)]
        host: String,
        /// Your tailnet login (the device owner).
        #[arg(long)]
        login: String,
    },
    /// Serve review links on this machine only (the default).
    ReviewLoopback,
    /// The standing remote-fix allowance for "fix with agent" from a review link.
    RemoteFix {
        #[arg(long)]
        usd: Option<f64>,
        #[arg(long)]
        runs: Option<u64>,
        #[arg(long)]
        off: bool,
    },
    /// Spend caps in dollars (R-COST-2; defaults $2 edit pass, $5 run, $20 day, $5 batch,
    /// $1 Ask message). Name the caps to change.
    Budgets {
        #[arg(long)]
        edit_pass: Option<f64>,
        #[arg(long)]
        run: Option<f64>,
        #[arg(long)]
        day: Option<f64>,
        #[arg(long)]
        batch: Option<f64>,
        #[arg(long)]
        ask: Option<f64>,
    },
    /// Which AI runs: `agent-cli` (your own agent CLI, the default) or `none`; with
    /// `--model` (repeatable) only those models may be named by a launched run.
    Providers {
        #[arg(value_enum)]
        ai: AiKind,
        #[arg(long = "model")]
        models: Vec<String>,
    },
    /// Allow (`on`) or refuse (`off`, the default) usage events. This build sends none.
    Telemetry {
        #[arg(value_enum)]
        state: OnOff,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum AiKind {
    AgentCli,
    None,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
enum StyleCmd {
    /// Pin a Markdown file (absolute path; ≤ 256 KB; no symlink) as the style rules.
    Set { path: PathBuf },
    /// Show what changed on disk since the pin and accept it (the diff is shown first).
    Accept,
    /// Show the pin and whether the file changed (no Owner code needed).
    Show,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ClientKind {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Subcommand)]
enum PlanCmd {
    /// Validate a RenderPlan against RP §2.1 rules 1–12 (W0-07).
    Validate { file: PathBuf },
    /// Hear a RenderPlan's voice mix for captions (SPEC §5.10): which parts the rule
    /// reuses (every source taken as transcribed), the stretches it transcribes, and how
    /// long that took.
    Hear {
        file: PathBuf,
        #[arg(long, value_enum, default_value_t = HearArg::Rule)]
        hear: HearArg,
        /// Write the audit and the heard words here.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

/// `plan-cut --captions`.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum PlanCaptions {
    /// The cut's own audio, identical parts reusing the source words (SPEC §5.10).
    Heard,
}

/// `--hear`: which parts are transcribed.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum HearArg {
    Rule,
    All,
    None,
}

impl HearArg {
    fn policy(self) -> gmclip_core::planner::heard::HearPolicy {
        use gmclip_core::planner::heard::HearPolicy;
        match self {
            Self::Rule => HearPolicy::Rule,
            Self::All => HearPolicy::All,
            Self::None => HearPolicy::None,
        }
    }
}

impl Cmd {
    /// The subcommand's name, for the `cmd` span and errors (identifier-shaped, so the
    /// log keeps it by value).
    fn name(&self) -> &'static str {
        match self {
            Cmd::Render { .. } => "render",
            Cmd::Still { .. } => "still",
            Cmd::DumpPlan { .. } => "dump-plan",
            Cmd::PlanCut { .. } => "plan-cut",
            Cmd::Plan {
                command: PlanCmd::Validate { .. },
            } => "plan.validate",
            Cmd::Plan {
                command: PlanCmd::Hear { .. },
            } => "plan.hear",
            Cmd::Probe { .. } => "probe",
            Cmd::Remux { .. } => "remux",
            Cmd::Agent {
                command: AgentCmd::Run { .. },
            } => "agent.run",
            Cmd::Mcp => "mcp",
            Cmd::Serve { .. } => "serve",
            Cmd::Status => "status",
            Cmd::Connections { .. } => "connections",
            Cmd::Stop => "stop",
            Cmd::Owner { .. } => "owner",
            Cmd::Profile { .. } => "profile",
            Cmd::Feedback { .. } => "feedback",
            Cmd::Review { .. } => "review",
            Cmd::Share(_) => "share",
            Cmd::Clippy { .. } => "clippy",
            Cmd::Record { .. } => "record",
        }
    }
}

/// What a subcommand prints on success.
#[derive(Debug)]
enum Output {
    /// A summary object; `trace_id` is added to it.
    Summary(Value),
    /// A document another command reads back (a RenderPlan); printed as is.
    Document(String),
    /// Nothing (stdout belonged to the command: the MCP shim).
    Silent,
}

/// Reads and validates a RenderPlan file (`Invalid{path}` / `Unsupported` / I/O).
fn load_plan(file: &Path) -> Result<gmclip_plan::Plan, CliError> {
    // The path is offered to the log and must come out as a shape (tests/cli.rs).
    tracing::debug!(event = "plan.read", file = %file.display());
    let bytes = read_capped(file)?;
    let report = gmclip_plan::check(&bytes);
    tracing::debug!(
        event = "plan.checked",
        bytes = bytes.len() as u64,
        errors = (report.errors.len() + report.more) as u64
    );
    match (report.plan, report.errors.first()) {
        (Some(plan), None) => Ok(plan),
        (_, first) => {
            let mut e: CliError = first
                .cloned()
                .unwrap_or_else(|| gmclip_plan::PlanError::invalid("", "no plan"))
                .into();
            let further = report.errors.len().saturating_sub(1) + report.more;
            e.more = (further > 0).then_some(further);
            Err(e)
        }
    }
}

fn read_capped(file: &Path) -> Result<Vec<u8>, CliError> {
    let f = std::fs::File::open(file).map_err(|e| CliError::io("read", &e))?;
    let mut bytes = Vec::new();
    f.take(MAX_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CliError::io("read", &e))?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        let mut e = CliError::new(Code::Invalid)
            .step("read")
            .reason("the file is over 64 MiB");
        e.path = Some(String::new());
        return Err(e);
    }
    Ok(bytes)
}

/// `gmclip plan-cut`: the cut of `hook` in `project`, planned; with `captions`, its
/// captions built from that source instead of the cut's own, heard under `policy` (the
/// project is unchanged), and the caption audit.
fn plan_cut(
    project: &Path,
    hook: &str,
    captions: Option<gmclip_model::entities::CaptionSource>,
    policy: gmclip_core::planner::heard::HearPolicy,
) -> Result<(gmclip_plan::Plan, Option<gmclip_core::planner::HeardReport>), CliError> {
    use gmclip_core::planner;
    let host = host::HolderFonts::load();
    let analysis = crate::analysis::CliAnalysis::new();
    let never = std::sync::atomic::AtomicBool::new(false);
    let hearer = crate::analysis::PlanHearer {
        host: &analysis,
        cancel: &never,
    };
    let cuts = match captions {
        None => planner::plan_project(project, &host),
        Some(c) => planner::plan_project_with(
            project,
            &host,
            planner::CaptionsAs {
                source: Some(c),
                policy,
                hearer: Some(&hearer),
                hook: Some(hook),
            },
        ),
    }
    .map_err(|e| {
        CliError::new(Code::Invalid)
            .step("plan-cut")
            .reason(e.to_string())
    })?;
    let c = cuts
        .into_iter()
        .find(|c| c.hook_id.as_deref() == Some(hook))
        .ok_or_else(|| {
            CliError::new(Code::NotFound)
                .step("plan-cut")
                .reason("no cut with a framing for that hook")
        })?;
    let heard = c.heard;
    c.plan.map(|p| (p, heard)).map_err(|e| {
        CliError::new(Code::Unsupported)
            .step("plan-cut")
            .reason(e.to_string())
    })
}

/// Runs one subcommand.
fn dispatch(cmd: Cmd) -> Result<Output, CliError> {
    match cmd {
        Cmd::Render { plan, out } => {
            let plan = load_plan(&plan)?;
            let summary = gmclip_render::ops::render(&plan, &out)?;
            Ok(Output::Summary(json!({ "frames": summary.frames })))
        }
        Cmd::Still { plan, at, out } => {
            let plan = load_plan(&plan)?;
            gmclip_render::ops::still(&plan, &at, &out)?;
            Ok(Output::Summary(json!({ "stills": at.len() })))
        }
        Cmd::DumpPlan { project, out, cut } => {
            let host = host::HolderFonts::load();
            let cuts = gmclip_core::planner::plan_project(&project, &host).map_err(|e| {
                CliError::new(Code::Invalid)
                    .step("dump-plan")
                    .reason(e.to_string())
            })?;
            let c = cuts
                .into_iter()
                .find(|c| cut.as_deref().is_none_or(|t| c.title.contains(t)))
                .ok_or_else(|| {
                    CliError::new(Code::NotFound)
                        .step("dump-plan")
                        .reason("no cut with a framing")
                })?;
            let plan = c.plan.map_err(|e| {
                CliError::new(Code::Unsupported)
                    .step("dump-plan")
                    .reason(e.to_string())
            })?;
            let mut text = serde_json::to_string_pretty(&plan)
                .map_err(|_| CliError::new(Code::Internal).step("dump-plan"))?;
            text.push('\n');
            std::fs::write(&out, text).map_err(|e| CliError::io("write", &e))?;
            // what playback and export build their audio from: a plan it refuses plays silent
            let audio = gmclip_media::audio::graph::AudioPlan::from_plan(&plan)
                .map(|_| "ok".to_owned())
                .unwrap_or_else(|e| e.to_string());
            Ok(Output::Summary(json!({
                "title": c.title,
                "frames": plan.output.frames,
                "parts": plan.parts.len(),
                "audio_plan": audio,
                "out": out,
            })))
        }
        Cmd::PlanCut {
            project,
            hook,
            captions,
            hear,
            report,
            out,
        } => {
            let (plan, heard) = plan_cut(
                &project,
                &hook,
                captions.map(|PlanCaptions::Heard| gmclip_model::entities::CaptionSource::Heard),
                hear.policy(),
            )?;
            if let (Some(path), Some(h)) = (&report, &heard) {
                hear::write_report(path, &serde_json::to_value(h).unwrap_or(Value::Null))?;
            }
            let mut text = serde_json::to_string_pretty(&plan)
                .map_err(|_| CliError::new(Code::Internal).step("plan-cut"))?;
            text.push('\n');
            match out {
                Some(path) => {
                    std::fs::write(&path, text).map_err(|e| CliError::io("write", &e))?;
                    Ok(Output::Summary(
                        json!({ "frames": plan.output.frames, "parts": plan.parts.len() }),
                    ))
                }
                None => Ok(Output::Document(text)),
            }
        }
        Cmd::Plan {
            command: PlanCmd::Hear { file, hear, report },
        } => {
            let plan = load_plan(&file)?;
            hear::plan_hear(&plan, hear.policy(), report.as_deref()).map(Output::Summary)
        }
        Cmd::Plan {
            command: PlanCmd::Validate { file },
        } => {
            let plan = load_plan(&file)?;
            Ok(Output::Summary(json!({
                "ok": true,
                "schema": gmclip_plan::SCHEMA_V0,
                "frames": plan.output.frames,
                "parts": plan.parts.len(),
                "text_events": plan.text_events.as_ref().map_or(0, Vec::len),
            })))
        }
        Cmd::Probe { file } => {
            let report = gmclip_media::ops::probe(&file)?;
            serde_json::from_str(&report.to_json())
                .map(Output::Summary)
                .map_err(|_| CliError::new(Code::Internal).step("probe"))
        }
        Cmd::Remux { input, out } => {
            let summary = gmclip_media::ops::remux(&input, &out)?;
            let mut v = summary.to_json();
            v["ok"] = json!(true);
            Ok(Output::Summary(v))
        }
        Cmd::Agent {
            command:
                AgentCmd::Run {
                    harness,
                    cut,
                    project,
                    brief,
                    brief_file,
                    media,
                    private_regions,
                    wall_clock,
                    transcript_out,
                    model,
                    effort,
                    client_path,
                    dry_run,
                },
        } => agent::run(agent::RunArgs {
            harness,
            project,
            cut,
            brief,
            brief_file,
            media,
            private_regions,
            wall_clock,
            transcript_out,
            model,
            effort,
            client_path,
            dry_run,
        })
        .map(Output::Summary),
        Cmd::Mcp => {
            let env = socket::env()?;
            gmclip_mcp::client::run_shim(env).map_err(|e| socket::client_err(&e))?;
            Ok(Output::Silent)
        }
        Cmd::Serve {
            headless,
            idle_s,
            forward,
        } => socket::serve(headless, idle_s, forward),
        Cmd::Status => socket::status(),
        Cmd::Connections { n } => socket::connections(n),
        Cmd::Stop => socket::stop(),
        Cmd::Owner { command } => match command {
            OwnerCmd::Connect { client } => {
                socket::owner_connect(matches!(client, ClientKind::Codex))
            }
            OwnerCmd::RootsAdd { path, write } => socket::owner(
                "roots_add",
                json!({"path": socket::abs(&path)?, "write": write}),
            ),
            OwnerCmd::RootsRemove { path } => {
                socket::owner("roots_remove", json!({"path": socket::abs(&path)?}))
            }
            OwnerCmd::Recordings { path } => {
                socket::owner("recordings_set", json!({"path": socket::abs(&path)?}))
            }
            OwnerCmd::OpenAny { path } => {
                socket::owner("project_open_any", json!({"path": socket::abs(&path)?}))
            }
            OwnerCmd::Revoke { identity } => socket::owner("revoke", json!({"identity": identity})),
            OwnerCmd::Style { command } => match command {
                StyleCmd::Set { path } => profile_cmd::style_set(&path),
                StyleCmd::Show => profile_cmd::style_show(),
                StyleCmd::Accept => profile_cmd::style_accept(),
            },
            OwnerCmd::ReviewTailnet { host, login } => socket::owner(
                "review_tailnet",
                json!({"host": host, "owner_login": login}),
            ),
            OwnerCmd::ReviewLoopback => socket::owner("review_loopback", json!({})),
            OwnerCmd::RemoteFix { usd, runs, off } => socket::owner(
                "remote_fix_set",
                if off {
                    json!({"off": true})
                } else {
                    json!({"max_usd_per_run": usd, "max_runs_per_day": runs})
                },
            ),
            OwnerCmd::Profile { path } => {
                socket::owner("profile_use", json!({"path": socket::abs(&path)?}))
            }
            OwnerCmd::Budgets {
                edit_pass,
                run,
                day,
                batch,
                ask,
            } => {
                let mut caps = serde_json::Map::new();
                for (k, v) in [
                    ("edit_pass_usd", edit_pass),
                    ("run_usd", run),
                    ("day_usd", day),
                    ("batch_usd", batch),
                    ("ask_usd", ask),
                ] {
                    if let Some(v) = v {
                        caps.insert(k.into(), json!(v));
                    }
                }
                socket::owner("budgets_set", serde_json::Value::Object(caps))
            }
            OwnerCmd::Providers { ai, models } => socket::owner(
                "providers_set",
                json!({"ai": match ai { AiKind::AgentCli => "agent_cli", AiKind::None => "none" }, "models": models}),
            ),
            OwnerCmd::Telemetry { state } => {
                socket::owner("telemetry_set", json!({"on": matches!(state, OnOff::On)}))
            }
        },
        Cmd::Profile { command } => profile_cmd::run(command),
        Cmd::Feedback { command } => profile_cmd::feedback(command),
        Cmd::Review { command } => review_cmd::run(command),
        Cmd::Share(a) => share_cmd::run(a),
        Cmd::Clippy { command } => clippy_cmd::run(command),
        Cmd::Record { command } => record_cmd::run(command),
    }
}

/// Prints `e` as JSON on stderr, logs it, and returns its exit code. A stderr that is
/// gone (EPIPE) is ignored; the exit code still says what happened.
fn fail(e: &CliError, trace_id: &str) -> ExitCode {
    tracing::warn!(
        event = "command.failed",
        code = e.code.as_str(),
        step = e.step.as_deref().unwrap_or("-"),
        pointer = e.path.as_deref().unwrap_or("-"),
        exit_code = u64::from(e.code.exit())
    );
    let _ = writeln!(io::stderr().lock(), "{}", e.to_json(trace_id));
    ExitCode::from(e.code.exit())
}

/// A panic is logged (shape-only, like everything else) and becomes `Internal`; the
/// default hook's stderr text is suppressed so stderr stays one JSON object.
fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(
            event = "panic",
            location = location.as_str(),
            panic = payload.as_str()
        );
    }));
}

/// The `reason` for a clap error: its kind, plus the offending argument cut to 16 bytes
/// (R-CMD-6), never clap's full rendered text.
fn usage_reason(e: &clap::Error) -> String {
    use clap::error::{ContextKind, ContextValue};
    if e.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        return "a subcommand is required; see gmclip --help".to_owned();
    }
    let arg = e
        .get(ContextKind::InvalidArg)
        .or_else(|| e.get(ContextKind::InvalidSubcommand))
        .and_then(|v| match v {
            ContextValue::String(s) => Some(s.clone()),
            ContextValue::Strings(v) => v.first().cloned(),
            _ => None,
        });
    match arg {
        Some(a) => {
            let mut end = a.len().min(16);
            while !a.is_char_boundary(end) {
                end -= 1;
            }
            let cut = if end < a.len() { "…" } else { "" };
            format!("{}: {}{cut}; see gmclip --help", e.kind(), &a[..end])
        }
        None => format!("{}; see gmclip --help", e.kind()),
    }
}

fn main() -> ExitCode {
    let trace_id = trace::mint().to_string();
    log::init(&trace_id);
    install_panic_hook();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            _ => {
                let _span = tracing::info_span!(
                    "cmd",
                    name = "usage",
                    caller = "cli",
                    trace_id = trace_id.as_str()
                )
                .entered();
                return fail(
                    &CliError::new(Code::Usage).reason(usage_reason(&e)),
                    &trace_id,
                );
            }
        },
    };

    let name = cli.command.name();
    let span = tracing::info_span!("cmd", name, caller = "cli", trace_id = trace_id.as_str());
    let _entered = span.enter();
    let started = Instant::now();
    tracing::info!(event = "command.started");
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| dispatch(cli.command)))
        .unwrap_or_else(|_| Err(CliError::new(Code::Internal).step(name)));
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let printed = outcome.and_then(|out| {
        let value = match out {
            Output::Summary(mut v) => {
                if let Value::Object(m) = &mut v {
                    m.insert("trace_id".into(), trace_id.as_str().into());
                }
                v
            }
            Output::Document(text) => {
                return write!(io::stdout().lock(), "{text}")
                    .map_err(|e| CliError::io("stdout", &e));
            }
            Output::Silent => return Ok(()),
        };
        writeln!(io::stdout().lock(), "{value}").map_err(|e| CliError::io("stdout", &e))
    });
    match printed {
        Ok(()) => {
            tracing::info!(
                event = "command.finished",
                code = "ok",
                exit_code = 0_u64,
                elapsed_ms
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            let code = fail(&e, &trace_id);
            tracing::info!(event = "command.finished", elapsed_ms);
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    /// A panic becomes one shape-only log line and an `Internal` error, and its message
    /// (which may hold anything) is never written as text in a release-policy log.
    #[test]
    fn panics_are_logged_shape_only_and_become_internal() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let policy = log::Policy::new(vec!["/Users/panic-home".into()], false);
        let sub = log::JsonLog::new(sink.clone(), policy, tracing::Level::INFO);
        let previous = panic::take_hook();
        install_panic_hook();
        let outcome = tracing::subscriber::with_default(sub, || {
            panic::catch_unwind(AssertUnwindSafe(|| -> Result<Output, CliError> {
                panic!("caption zanzibar in /Users/panic-home/cut.json")
            }))
            .unwrap_or_else(|_| Err(CliError::new(Code::Internal).step("render")))
        });
        panic::set_hook(previous);
        let e = outcome.unwrap_err();
        assert_eq!(e.code.exit(), 70);
        assert_eq!(e.to_json("t")["code"], "Internal");
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["level"], "ERROR");
        assert!(
            line["fields"]["panic"]["len"].as_u64().unwrap() > 0,
            "{line}"
        );
        assert!(
            !text.contains("zanzibar") && !text.contains("panic-home"),
            "{text}"
        );
    }

    #[test]
    fn core_links_with_the_null_renderer() {
        use gmclip_core::Renderer;
        assert_eq!(gmclip_core::NullRenderer.name(), "null");
    }
}
