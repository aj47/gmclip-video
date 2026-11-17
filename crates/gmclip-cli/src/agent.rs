//! `gmclip agent run` (SPEC §10.7, R-AGT-1..5): the CLI face of `gmclip_ai::launcher`.
//!
//! At M1 this is the only way a harness starts (R-AGT-3): a human at an interactive
//! terminal. The run credential is registered with the running holder over the verified
//! socket (`Control::LaunchRegister`), bound to the project, the cut, the scopes and the
//! wall clock, and ended when the client exits (`Control::LaunchEnd`). `--media` and
//! `--private-regions` use the holder's Owner challenge (`launch_grant`): the holder prints
//! the code, the human types it here, the holder checks it. The tool surface is the MCP
//! server's own (`gmclip_mcp::tools::defs`). `--dry-run` shows what would run.
//!
//! The project must already be open in the holder (an agent session or the app opened
//! it); there is no control request to open one yet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gmclip_ai::launcher::{
    self, Grants, Harness, LaunchError, LaunchRequest, ProcessContext, VersionPolicy, argv, tools,
};
use serde_json::{Value, json};

use crate::error::{CliError, Code};
use crate::socket;

/// Parsed `gmclip agent run` arguments.
#[derive(Debug)]
pub struct RunArgs {
    pub harness: String,
    pub project: Option<String>,
    pub cut: String,
    pub brief: Option<String>,
    pub brief_file: Option<PathBuf>,
    pub media: bool,
    pub private_regions: bool,
    pub wall_clock: String,
    pub transcript_out: Option<PathBuf>,
    pub model: Option<String>,
    pub effort: String,
    pub client_path: Option<PathBuf>,
    pub dry_run: bool,
}

/// The holder's tool surface, as the launcher's allow-list input.
fn surface() -> Vec<tools::ToolDef> {
    use gmclip_core::command::Scope as S;
    gmclip_mcp::tools::defs()
        .into_iter()
        .filter_map(|d| {
            let scope = match d.scope {
                S::Read => tools::Scope::Read,
                S::ReadMedia => tools::Scope::ReadMedia,
                S::Edit => tools::Scope::Edit,
                S::Render => tools::Scope::Render,
                S::Ai => tools::Scope::Ai,
                S::Admin => return None,
            };
            Some(tools::ToolDef {
                name: d.name,
                scope,
            })
        })
        .collect()
}

fn holder_err(e: &CliError) -> String {
    e.reason
        .clone()
        .unwrap_or_else(|| e.code.as_str().to_owned())
}

/// Registers runs with the running holder (R-AGT-5).
#[derive(Debug, Default)]
struct HolderRegistrar;

impl launcher::Registrar for HolderRegistrar {
    fn register(
        &mut self,
        reg: &launcher::Registration<'_>,
    ) -> Result<launcher::Registered, String> {
        let req = gmclip_mcp::holder::Control::LaunchRegister(gmclip_mcp::holder::LaunchRegister {
            project: reg.project.to_owned(),
            cut: reg.cut.to_owned(),
            credential: reg.credential_hex.to_owned(),
            wall_clock_s: reg.wall_clock_s,
            media: reg.grants.media,
            private_regions: reg.grants.private_regions,
            code: reg.owner_code.map(str::to_owned),
            brief_sha256: Some(reg.brief_sha256.to_owned()),
            // The CLI launcher never starts a full-access session (decision 0043).
            full_access: false,
            grant: None,
            scopes: Some(reg.scopes.iter().map(|s| s.as_str().to_owned()).collect()),
        });
        let v = socket::control(&req, false).map_err(|e| holder_err(&e))?;
        let run = v["run"]
            .as_str()
            .ok_or("the holder returned no run id")?
            .to_owned();
        Ok(launcher::Registered {
            identity: v["identity"]
                .as_str()
                .map_or_else(|| format!("launched:{run}"), str::to_owned),
            run,
            with_holder: true,
        })
    }

    fn end(&mut self, run: &str) {
        let _ = socket::control(
            &gmclip_mcp::holder::Control::LaunchEnd {
                run: run.to_owned(),
            },
            false,
        );
    }
}

/// The holder's Owner challenge for `launch_grant`: the holder prints the code.
#[derive(Debug, Default)]
struct HolderChallenger;

impl launcher::grant::Challenger for HolderChallenger {
    fn issue(
        &mut self,
        req: &launcher::grant::GrantRequest<'_>,
    ) -> std::io::Result<launcher::grant::Issued> {
        let begin = gmclip_mcp::holder::Control::OwnerBegin {
            command: "launch_grant".into(),
            // exactly the arguments the holder checks the code against in launch_register
            args: gmclip_mcp::holder::launch_grant_args(
                req.project,
                req.cut,
                req.grants.media,
                req.grants.private_regions,
            ),
        };
        let v =
            socket::control(&begin, false).map_err(|e| std::io::Error::other(holder_err(&e)))?;
        Ok(launcher::grant::Issued::ShownOn(
            v["shown_on"]
                .as_str()
                .unwrap_or("the holder's terminal")
                .to_owned(),
        ))
    }
}

/// `90m`, `1h`, `45s`, or a bare number of minutes; 1 min ..= 24 h.
pub fn parse_wall_clock(s: &str) -> Option<Duration> {
    let (num, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => s.split_at(i),
        None => (s, "m"),
    };
    let n: u64 = num.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        _ => return None,
    };
    (60..=24 * 3600)
        .contains(&secs)
        .then(|| Duration::from_secs(secs))
}

fn usage(reason: &str) -> CliError {
    CliError::new(Code::Usage).step("agent.run").reason(reason)
}

fn from_launch(e: &LaunchError) -> CliError {
    use launcher::resolve::ResolveError as R;
    let (code, reason): (Code, String) = match e {
        LaunchError::Brief(why) => (Code::Invalid, format!("brief: {why}")),
        LaunchError::Resolve(R::NotFound { .. }) => (
            Code::NotFound,
            "the client was not found on PATH; pass --client-path with its absolute path".into(),
        ),
        LaunchError::Resolve(R::NotAbsolute(_)) => {
            (Code::Usage, "--client-path must be absolute".into())
        }
        LaunchError::Resolve(R::NotExecutable(_)) => (
            Code::Invalid,
            "the client path is not an executable file".into(),
        ),
        LaunchError::Resolve(R::EnvShebang(_)) => (
            Code::Unsupported,
            "the client is a #!/usr/bin/env script; point --client-path at the native binary"
                .into(),
        ),
        LaunchError::Resolve(R::NoInterpreter(_)) => (
            Code::NotFound,
            "pi is a node script and there is no node beside it; pass --client-path for an install with node next to it".into(),
        ),
        LaunchError::Resolve(R::WrapperWithoutNative(_)) => (
            Code::NotFound,
            "the Codex npm wrapper's native binary is missing; reinstall Codex".into(),
        ),
        LaunchError::Resolve(R::Batch(_)) => (
            Code::Unsupported,
            "the client is a batch file that is not an npm shim; pass --client-path with its .exe".into(),
        ),
        LaunchError::Version(why) => (Code::Unsupported, why.clone()),
        LaunchError::Surface(s) => (Code::Internal, s.to_string()),
        LaunchError::Grant(g) => (Code::Invalid, g.to_string()),
        // the holder's own reason (no secret in it): not running, not trusted, the project
        // not open, the cut unknown, the grant code wrong
        LaunchError::Register(why) => (
            Code::Unauthorized,
            format!("the holder refused the run: {why}"),
        ),
        LaunchError::RunDir(_) => (
            Code::Io,
            "the run directory could not be created safely".into(),
        ),
        LaunchError::Resolve(R::Script(_)) => (
            Code::Unsupported,
            "the client is a script; point --client-path at the native binary".into(),
        ),
        LaunchError::Arg(why) => (Code::Usage, (*why).into()),
        LaunchError::Refused(why) => (Code::Unsupported, (*why).into()),
        LaunchError::Io(io) => (Code::Io, io.kind().to_string()),
    };
    CliError::new(code).step("agent.run").reason(reason)
}

/// Runs `gmclip agent run`.
pub fn run(a: RunArgs) -> Result<Value, CliError> {
    let harness = Harness::parse(&a.harness)
        .ok_or_else(|| usage("--harness is claude-code, codex, pi or opencode"))?;
    let brief = match (a.brief, a.brief_file) {
        (Some(b), None) => b,
        (None, Some(f)) => {
            let bytes = std::fs::read(&f).map_err(|e| CliError::io("agent.run.brief", &e))?;
            String::from_utf8(bytes).map_err(|_| usage("the brief file is not UTF-8"))?
        }
        _ => return Err(usage("give exactly one of --brief or --brief-file")),
    };
    let wall_clock = parse_wall_clock(&a.wall_clock)
        .ok_or_else(|| usage("--wall-clock is like 90m, 2h or 600s (1 min to 24 h)"))?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| CliError::io("agent.run.self", &e))?;
    let req = LaunchRequest {
        harness,
        client_path: a.client_path,
        mcp_command: exe,
        mcp_args: vec!["mcp".into()],
        // a home path is matched by its exact string in the holder: make it absolute
        project: match a.project {
            Some(p) if Path::new(&p).exists() => socket::abs(Path::new(&p))?,
            Some(p) => p,
            None => String::new(),
        },
        cut: a.cut,
        brief,
        grants: Grants {
            media: a.media,
            private_regions: a.private_regions,
        },
        wall_clock,
        model: a.model,
        effort: a.effort,
        transcript_out: a.transcript_out,
        runs_root: None,
        foreground: true,
        continue_from: None,
    };
    // Owner grants are refused without a terminal before anything else happens (R-OWN-3)
    if !a.dry_run
        && req.grants.any()
        && !launcher::grant::Tty::interactive(&launcher::grant::ProcessTty)
    {
        return Err(from_launch(&LaunchError::Grant(
            launcher::grant::GrantError::NoTty,
        )));
    }
    // R-AGT-3 at M1: the only start is a human at a terminal; checked before the client
    // is even asked its version
    if !a.dry_run && !launcher::grant::Tty::interactive(&launcher::grant::ProcessTty) {
        return Err(CliError::new(Code::Unauthorized).step("agent.run").reason(
            "an agent run starts only from an interactive terminal (R-AGT-3); use --dry-run to inspect",
        ));
    }
    let surface = surface();
    let checked = launcher::check(
        &req,
        &surface,
        &ProcessContext::current(),
        VersionPolicy::Enforce,
    )
    .map_err(|e| from_launch(&e))?;
    if !a.dry_run {
        if req.project.is_empty() {
            return Err(usage(
                "--project is required (the holder's project id or home path)",
            ));
        }
        // A2.10: the Owner's providers (AI off, or a model allowlist) bound every launch
        let env = socket::env()?;
        gmclip_mcp::settings::OwnerSettings::load(&env.app_data)
            .agent_run_allowed(req.model.as_deref())
            .map_err(|why| {
                CliError::new(Code::Unauthorized)
                    .step("agent.run")
                    .reason(why)
            })?;
        let mut registrar = HolderRegistrar;
        let mut tty = launcher::grant::ProcessTty;
        let prepared = launcher::prepare(
            &req,
            launcher::Deps {
                surface: &surface,
                registrar: &mut registrar,
                tty: &mut tty,
                challenger: &mut HolderChallenger,
                ctx: ProcessContext::current(),
                policy: VersionPolicy::Enforce,
            },
        )
        .map_err(|e| from_launch(&e))?;
        let run = prepared.record.run.clone();
        // a panic past this point must still end the registration (main's catch_unwind
        // would otherwise leave the credential live until the wall clock)
        let record = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prepared.execute(&mut HolderRegistrar)
        }))
        .unwrap_or_else(|_| {
            launcher::Registrar::end(&mut HolderRegistrar, &run);
            Err(LaunchError::Refused(
                "the launcher panicked; the run was ended",
            ))
        })
        .map_err(|e| from_launch(&e))?;
        return serde_json::to_value(&record)
            .map_err(|_| CliError::new(Code::Internal).step("agent.run"));
    }
    let placeholder = Path::new("<runs root>/<run>");
    let cwd = placeholder.join("cwd");
    let config = placeholder.join("config");
    let inputs = argv::ArgvInputs {
        client: &checked.binary.exec,
        interpreter: checked.binary.interpreter.as_deref(),
        cwd: &cwd,
        config: &config,
        resume: None,
        mcp_command: &req.mcp_command,
        mcp_args: &req.mcp_args,
        tools: &checked.tools,
        model: req.model.as_deref(),
        effort: &req.effort,
    };
    let argv = match harness {
        Harness::ClaudeCode => argv::claude(
            &inputs,
            &placeholder.join("config/mcp.json"),
            &placeholder.join("config/settings.json"),
        ),
        Harness::Codex => argv::codex(&inputs),
        Harness::Pi => argv::pi(&inputs),
        Harness::OpenCode => argv::opencode(&inputs),
    };
    let env_keys = launcher::env::keys(&launcher::env::child_env(
        harness,
        std::env::vars_os(),
        checked
            .binary
            .interpreter
            .as_deref()
            .unwrap_or(&checked.binary.exec),
        "<run>",
        &launcher::credential::RunCredential::mint()
            .map_err(|e| CliError::io("agent.run.random", &e))?,
        &config,
    ));
    Ok(json!({
        "dry_run": true,
        "harness": harness.as_str(),
        "binary": checked.binary,
        "scopes": checked.scopes,
        "tools": checked.tools.iter().map(|t| tools::qualified(t)).collect::<Vec<_>>(),
        "grants_need_tty_code": req.grants.any(),
        "argv": argv.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "env_keys": env_keys,
        "wall_clock_s": wall_clock.as_secs(),
        "tcc_disclaimed": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_forms() {
        assert_eq!(parse_wall_clock("90m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_wall_clock("90"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_wall_clock("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_wall_clock("600s"), Some(Duration::from_secs(600)));
        for bad in ["", "m", "30s", "25h", "1d", "-5m", "1.5h"] {
            assert_eq!(parse_wall_clock(bad), None, "{bad}");
        }
    }
}
