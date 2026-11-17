//! `gmclip profile …` and the style half of `gmclip owner style …` (SPEC §10.5,
//! R-PROF-1..5).
//!
//! `profile` commands make and read profile folders; they set no authority. A profile's
//! suggested settings (harness, budgets, scopes, roots, style path…) are printed with
//! the Owner command that would apply each, and nothing applies them here (R-PROF-4).
//! There is deliberately no CLI command that appends style feedback (R-PROF-2: a human
//! action in the app only).

use std::io::Write;
use std::path::PathBuf;

use clap::Subcommand;
use gmclip_core::profile;
use serde_json::json;

use crate::Output;
use crate::error::{CliError, Code};
use crate::socket;

#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// Make a new profile folder from a kit (gaming-variety, screen-face, talking-head,
    /// blank). The kit's style rules go to `<dir>/style.md`; pin them with
    /// `gmclip owner style set <dir>/style.md` if you want agents to read them.
    New {
        dir: PathBuf,
        #[arg(long)]
        kit: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Show a profile: its content fields, and the settings it suggests (never applied).
    Show { dir: PathBuf },
    /// List the kits.
    Kits,
}

#[derive(Debug, Subcommand)]
pub enum FeedbackCmd {
    /// Print the feedback log as AGENTS.md-style lines (`- YYYY-MM-DD: text (author,
    /// via …)`) for your own agent to merge; gmclip writes no instruction file (D16).
    Export {
        /// The only format: `agents-md`.
        #[arg(long)]
        format: String,
        /// The profile folder (default: the profile the Owner set).
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Only the last N records.
        #[arg(long)]
        last: Option<usize>,
    },
}

/// `gmclip feedback …` (read only: there is no command that appends, R-PROF-2).
pub fn feedback(cmd: FeedbackCmd) -> Result<crate::Output, CliError> {
    match cmd {
        FeedbackCmd::Export {
            format,
            profile: dir,
            last,
        } => {
            if format != "agents-md" {
                return Err(CliError::new(Code::Invalid)
                    .step("format")
                    .reason("the only format is agents-md"));
            }
            let dir = match dir {
                Some(d) => PathBuf::from(socket::abs(&d)?),
                None => gmclip_mcp::settings::OwnerSettings::load(&socket::env()?.app_data)
                    .profile
                    .ok_or_else(|| {
                        CliError::new(Code::NotFound)
                            .step("profile")
                            .reason("no profile is set: pass --profile <dir>")
                    })?,
            };
            if !dir.is_dir() {
                return Err(CliError::new(Code::NotFound)
                    .step("profile")
                    .reason("no such profile folder"));
            }
            let recs = profile::feedback::last(&dir, last.unwrap_or(usize::MAX));
            Ok(crate::Output::Document(
                profile::feedback::export_agents_md(&recs),
            ))
        }
    }
}

fn err(step: &str, e: impl std::fmt::Display) -> CliError {
    CliError::new(Code::Invalid)
        .step(step)
        .reason(e.to_string())
}

pub fn run(cmd: ProfileCmd) -> Result<Output, CliError> {
    match cmd {
        ProfileCmd::New { dir, kit, name } => {
            let dir = socket::abs(&dir).map(PathBuf::from)?;
            let (kit_name, _, _) = profile::kits::kit(&kit)
                .ok_or_else(|| err("profile", "no such kit (gmclip profile kits)"))?;
            profile::kits::create_from_kit(&dir, &kit, name.as_deref().unwrap_or(kit_name))
                .map_err(|e| err("profile", e))?;
            // made here: a local profile (a folder from anyone else stays external)
            let env = socket::env()?;
            profile::register_local(&env.app_data, &dir).map_err(|e| err("profile", e))?;
            show(&dir)
        }
        ProfileCmd::Show { dir } => show(&dir),
        ProfileCmd::Kits => Ok(Output::Summary(json!({
            "kits": profile::kits::KITS.iter().map(|k| json!({"id": k.0, "name": k.1})).collect::<Vec<_>>()
        }))),
    }
}

fn show(dir: &std::path::Path) -> Result<Output, CliError> {
    let env = socket::env()?;
    let p = profile::load_in(&env.app_data, dir).map_err(|e| err("profile", e))?;
    Ok(Output::Summary(json!({
        "dir": p.dir,
        "name": p.name,
        "origin": p.origin,
        "kit": p.kit,
        "content_fields": p.content.keys().collect::<Vec<_>>(),
        "suggested_settings_not_applied": p.suggestions,
        "ignored_fields": p.ignored,
    })))
}

/// `gmclip owner style set <path>`: shows the file (every hidden character made
/// visible) and its hash, then the Owner code confirms exactly that version.
pub fn style_set(path: &std::path::Path) -> Result<Output, CliError> {
    let abs = PathBuf::from(socket::abs(path)?);
    let (p, sha, shown) = profile::style::preview(&abs)
        .map_err(|e| CliError::new(Code::Invalid).step("style").reason(e))?;
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "Style rules file {} (sha256 {sha}):\n", p.display());
    let _ = write!(e, "{shown}");
    let _ = writeln!(e);
    drop(e);
    socket::owner_described(
        "style_set",
        json!({"path": p.to_string_lossy(), "sha256": sha}),
        &format!(
            "let agents read this file as your style rules (sha256 {})",
            &sha[..12.min(sha.len())]
        ),
    )
}

/// `gmclip owner style show`: the pin and the file now (no code needed: read only).
pub fn style_show() -> Result<Output, CliError> {
    let env = socket::env()?;
    match profile::style::status(&env.app_data) {
        None => Ok(Output::Summary(json!({"style": null}))),
        Some(s) => Ok(Output::Summary(json!({
            "path": s.path,
            "pinned_sha256": s.pinned_sha256,
            "read_at": s.read_at,
            "current": s.current.as_ref().map_or_else(|e| json!({"error": e}), |h| json!(h)),
            "changed_on_disk": s.changed,
        }))),
    }
}

/// `gmclip owner style accept`: shows the diff on the terminal, then the Owner code
/// confirms exactly that version (its hash is the command's argument).
pub fn style_accept() -> Result<Output, CliError> {
    let env = socket::env()?;
    let s = profile::style::status(&env.app_data).ok_or_else(|| {
        CliError::new(Code::NotFound)
            .step("style")
            .reason("no style file is set")
    })?;
    let current = s
        .current
        .map_err(|e| CliError::new(Code::Invalid).step("style").reason(e))?;
    if !s.changed {
        return Ok(Output::Summary(
            json!({"changed_on_disk": false, "sha256": current}),
        ));
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "Changes to {} since it was pinned:\n", s.path.display());
    let _ = write!(e, "{}", s.diff);
    drop(e);
    socket::owner_described(
        "style_accept",
        json!({"sha256": current}),
        &format!(
            "let agents read the style file as shown above (sha256 {})",
            &current[..12.min(current.len())]
        ),
    )
}
