//! The M1 socket subcommands: `mcp`, `serve --headless`, `status`, `connections`,
//! `stop`, and the Owner path `owner …` (R-ARCH-5, R-OWN-3 [M1], R-OWN-4, R-MCP-2).
//!
//! Owner commands run only from an interactive terminal: gmclip asks the holder for a
//! one-time code bound to the command and its arguments, prints it, and the human types
//! it back. Honest limit (SPEC R-OWN-3): a same-user process driving a pty can script
//! this; M3 replaces it with OS authentication.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gmclip_mcp::client::{self, ClientError};
use gmclip_mcp::holder::Control;
use gmclip_mcp::paths::Env;
use gmclip_mcp::{connect, server};
use serde_json::{Value, json};

use crate::Output;
use crate::error::{CliError, Code};

pub fn env() -> Result<Env, CliError> {
    Env::from_env().map_err(|e| {
        CliError::new(Code::Unsupported)
            .step("socket")
            .reason(e.to_string())
    })
}

pub fn client_err(e: &ClientError) -> CliError {
    let code = match e {
        ClientError::NotTrusted(_) | ClientError::Refused(_) => Code::Unauthorized,
        ClientError::NoHolder(_) | ClientError::Io(_) => Code::Io,
    };
    CliError::new(code).step("socket").reason(e.to_string())
}

fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::io("runtime", &e))
}

/// A control request; `auto_start` starts a holder if none runs.
pub(crate) fn control(req: &Control, auto_start: bool) -> Result<Value, CliError> {
    let env = env()?;
    let v = runtime()?
        .block_on(client::control(&env, req, auto_start))
        .map_err(|e| client_err(&e))?;
    unwrap(v)
}

fn unwrap(v: Value) -> Result<Value, CliError> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    match v
        .get("error")
        .cloned()
        .and_then(|e| serde_json::from_value::<gmclip_core::CmdError>(e).ok())
    {
        Some(e) => Err(e.into()),
        None => Err(CliError::new(Code::Internal)
            .step("socket")
            .reason("malformed reply")),
    }
}

pub fn abs(p: &Path) -> Result<String, CliError> {
    let a = if p.is_absolute() {
        p.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| CliError::io("cwd", &e))?
            .join(p)
    };
    a.to_str()
        .map(str::to_owned)
        .ok_or_else(|| CliError::new(Code::Invalid).reason("the path is not UTF-8"))
}

pub fn serve(
    headless: bool,
    idle_s: Option<u64>,
    forward: Vec<String>,
) -> Result<Output, CliError> {
    if !headless {
        return Err(CliError::new(Code::Unsupported)
            .reason("M1 has no window: use gmclip serve --headless"));
    }
    let idle = idle_s
        .or_else(|| {
            std::env::var("GMCLIP_HOLDER_IDLE_S")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(900);
    // the model planner (A18) behind the seam, with this machine's font files
    let planner: std::sync::Arc<dyn gmclip_core::Planner> = std::sync::Arc::new(
        gmclip_core::host::ModelPlanner(crate::host::HolderFonts::load()),
    );
    // analysis and ASR on lane D's DSP and models (from the OS model cache)
    let analysis = std::sync::Arc::new(crate::analysis::CliAnalysis::new());
    let media: std::sync::Arc<dyn gmclip_core::MediaRender> =
        std::sync::Arc::new(crate::host::CliMedia::new(std::sync::Arc::clone(&analysis)));
    match server::run_headless(
        env()?,
        Duration::from_secs(idle),
        forward,
        planner,
        media,
        analysis,
    ) {
        Ok(()) => Ok(Output::Summary(json!({"stopped": true}))),
        Err(server::ServeError::Forwarded(v)) => Ok(Output::Summary(json!({"forwarded": v}))),
        Err(e) => Err(CliError::new(Code::Io).step("serve").reason(e.to_string())),
    }
}

pub fn status() -> Result<Output, CliError> {
    match control(&Control::Status, false) {
        Ok(v) => Ok(Output::Summary(json!({"running": true, "holder": v}))),
        Err(e) if e.code == Code::Io => Ok(Output::Summary(json!({"running": false}))),
        Err(e) => Err(e),
    }
}

pub fn connections(n: usize) -> Result<Output, CliError> {
    let env = env()?;
    // the log is a file: readable with or without a holder
    let list = gmclip_mcp::settings::connections(&env.app_data, n);
    Ok(Output::Summary(json!({"connections": list})))
}

pub fn stop() -> Result<Output, CliError> {
    match control(&Control::Shutdown, false) {
        Ok(_) => Ok(Output::Summary(json!({"stopping": true}))),
        Err(e) if e.code == Code::Io => Ok(Output::Summary(json!({"running": false}))),
        Err(e) => Err(e),
    }
}

/// The R-OWN-3 [M1] gate: an interactive terminal on stdin and stderr.
fn require_tty() -> Result<(), CliError> {
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(CliError::new(Code::Unauthorized)
            .step("owner")
            .reason("Owner commands need an interactive terminal (R-OWN-3); run this yourself in a terminal"))
    }
}

/// Asks the holder for a code, shows it, reads it back, and runs the Owner command.
fn owner_call(command: &str, args: &Value, describe: &str) -> Result<Value, CliError> {
    require_tty()?;
    let env = env()?;
    let rt = runtime()?;
    rt.block_on(async {
        let (mut s, _) = client::connect(&env, gmclip_mcp::wire::Mode::Control, true)
            .await
            .map_err(|e| client_err(&e))?;
        let begin = Control::OwnerBegin {
            command: command.into(),
            args: args.clone(),
        };
        let code = unwrap(
            client::request(&mut s, &begin)
                .await
                .map_err(|e| client_err(&e))?,
        )?;
        let shown_on = code.get("shown_on").and_then(Value::as_str).unwrap_or("your terminal").to_owned();
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "\nOwner action: {describe}\nTo confirm, type the code gmclip just printed on {shown_on} and press return."
        );
        let _ = write!(err, "> ");
        let _ = err.flush();
        drop(err);
        let mut typed = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut typed)
            .map_err(|e| CliError::io("read code", &e))?;
        let call = Control::OwnerCall {
            command: command.into(),
            args: args.clone(),
            code: typed.trim().to_ascii_uppercase(),
        };
        unwrap(
            client::request(&mut s, &call)
                .await
                .map_err(|e| client_err(&e))?,
        )
    })
}

/// An Owner command with its own description (shown before the code prompt).
pub fn owner_described(command: &str, args: Value, describe: &str) -> Result<Output, CliError> {
    owner_call(command, &args, describe).map(Output::Summary)
}

pub fn owner(command: &str, args: Value) -> Result<Output, CliError> {
    let describe = format!("{command} {args}");
    owner_call(command, &args, &describe).map(Output::Summary)
}

/// `owner connect claude-code|codex`: show the disclosure and every change as a diff,
/// confirm with the Owner code, then write through the client's CLI or its config.
pub fn owner_connect(codex: bool) -> Result<Output, CliError> {
    require_tty()?;
    // Windows sets USERPROFILE, not HOME
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = PathBuf::from(
        std::env::var_os(var)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| CliError::new(Code::Unsupported).reason(format!("{var} is not set")))?,
    );
    let gmclip = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| CliError::io("current exe", &e))?;
    let (client, plan) = if codex {
        (
            "codex",
            connect::codex_plan(&home, &gmclip, connect::find_on_path("codex", None)),
        )
    } else {
        (
            "claude-code",
            connect::claude_plan(&home, &gmclip, connect::find_on_path("claude", None)),
        )
    };
    let plan = plan.map_err(|e| CliError::new(Code::Invalid).step("connect").reason(e))?;
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}\n", connect::DISCLOSURE);
    if let Some((prog, args)) = &plan.cli {
        let _ = writeln!(err, "will run: {} {}", prog.display(), args.join(" "));
    }
    for e in &plan.edits {
        let _ = write!(err, "{}", connect::diff(e));
    }
    for n in &plan.notes {
        let _ = writeln!(err, "note: {n}");
    }
    drop(err);
    owner_call(
        "connect",
        &json!({"client": client}),
        &format!("connect {client}"),
    )?;
    if let Some((prog, args)) = &plan.cli {
        let st = std::process::Command::new(prog)
            .args(args)
            .status()
            .map_err(|e| CliError::io("client cli", &e))?;
        if !st.success() {
            return Err(CliError::new(Code::Io)
                .step("client cli")
                .reason(format!("{} exited with {st}", prog.display())));
        }
    }
    connect::apply_edits(&plan).map_err(|e| CliError::io("write client config", &e))?;
    Ok(Output::Summary(json!({
        "connected": client,
        "via_cli": plan.cli.is_some(),
        "files": plan.edits.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>(),
        "allowed_without_asking": connect::READ_ONLY_TOOLS,
    })))
}
