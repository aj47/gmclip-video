//! The four clients gmclip can launch, and how each is held to gmclip's tools
//! (SPEC §10.6/§10.7, R-AGT-1, R-AGT-4; `decisions/0012`).
//!
//! Every client runs headless on the user's own plan login, with **only** the run's
//! enumerated gmclip tools, its settings and MCP config in the run's throwaway
//! `config/` directory, and never a write to the user's own `~/.claude`, `~/.codex`,
//! `~/.pi` or `~/.config/opencode`:
//!
//! | client | tools | gmclip MCP | user instructions | login |
//! |---|---|---|---|---|
//! | Claude Code | `--tools ""` + `--allowedTools` + per-run settings | `--strict-mcp-config --mcp-config` | `--setting-sources ""` (S7 passed 2.1.300) | keychain via `HOME` |
//! | Codex | shell/apps/… `--disable`d, `enabled_tools` | `-c mcp_servers.gmclip.*` | `--ignore-user-config` (fails S7-X1/X5 on 0.160; runs anyway, AJ 2026-10-16) | `CODEX_HOME/auth.json` |
//! | pi | `--no-tools --tools <list>` | gmclip's own extension (pi has no MCP), `--no-extensions -e` | `--no-context-files --no-skills --no-prompt-templates`, `--system-prompt`/`--append-system-prompt` files replace `SYSTEM.md`/`APPEND_SYSTEM.md` | `~/.pi/agent/auth.json` (read in place) |
//! | opencode | `permission: {"*": "deny", <tool>: "allow"…}` | `mcp.gmclip` in `OPENCODE_CONFIG` | `XDG_CONFIG_HOME` = the run's, `OPENCODE_DISABLE_CLAUDE_CODE`, `…_EXTERNAL_SKILLS`, `…_PROJECT_CONFIG`, `--pure` | `XDG_DATA_HOME/opencode/auth.json` (read in place) |
//!
//! Every client goes through the same version gate (`version`): its S7-tested build runs,
//! any other runs once gmclip's one-time check passes or the Owner allows that exact
//! build. Codex 0.160's known S7 leaks ($CODEX_HOME/AGENTS.md reaches the session; the
//! collaboration and `request_user_input` tools stay offered) no longer refuse it: AJ,
//! 2026-10-16, "dont worry about codex safety" (decision 0012). The absolute path, the
//! tool allow-list, the run folder and the per-run credential hold for it as for the
//! others.

use serde::Serialize;

use super::Harness;

/// What the app and `gmclip agent run` say about a client.
#[derive(Clone, Debug, Serialize)]
pub struct ClientInfo {
    pub harness: Harness,
    pub label: &'static str,
    /// One line: how the run is confined.
    pub how: &'static str,
    /// Where the plan login is read from (never written by gmclip).
    pub login: &'static str,
    /// "Continue with a note" resumes the client's own session (else a fresh run seeded
    /// with the previous run's summary).
    pub resumes: bool,
    /// Why it cannot resume, when it cannot.
    pub resume_note: Option<&'static str>,
}

pub fn info(h: Harness) -> ClientInfo {
    match h {
        Harness::ClaudeCode => ClientInfo {
            harness: h,
            label: h.label(),
            how: "built-in tools off, the run's gmclip tools allow-listed, gmclip the only MCP server, your settings and CLAUDE.md not loaded",
            login: "your Claude login (keychain)",
            resumes: false,
            resume_note: Some(
                "Claude Code keeps sessions under ~/.claude, which gmclip never writes; a continued run starts fresh with the last run's summary",
            ),
        },
        Harness::Codex => ClientInfo {
            harness: h,
            label: h.label(),
            how: "shell, apps, browser and plugins off, the run's gmclip tools enabled, your config.toml not loaded (Codex 0.160 still reads $CODEX_HOME/AGENTS.md and keeps its collaboration tools; run anyway, as you chose)",
            login: "your Codex login ($CODEX_HOME/auth.json)",
            resumes: false,
            resume_note: Some(
                "Codex runs --ephemeral (no session files); a continued run starts fresh with the last run's summary",
            ),
        },
        Harness::Pi => ClientInfo {
            harness: h,
            label: h.label(),
            how: "built-in tools and your extensions, skills, prompts and AGENTS.md off; gmclip's tools come from gmclip's own extension in the run folder",
            login: "your pi login (~/.pi/agent/auth.json, read in place)",
            resumes: true,
            resume_note: None,
        },
        Harness::OpenCode => ClientInfo {
            harness: h,
            label: h.label(),
            how: "every tool denied except the run's gmclip tools; config, plugins, rules and skills from the run folder only",
            login: "your opencode login (~/.local/share/opencode/auth.json, read in place)",
            resumes: true,
            resume_note: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pi_and_opencode_resume_in_their_run_folder() {
        let r: Vec<Harness> = Harness::ALL
            .into_iter()
            .filter(|h| info(*h).resumes)
            .collect();
        assert_eq!(r, [Harness::Pi, Harness::OpenCode]);
        for h in Harness::ALL {
            assert_eq!(info(h).resumes, info(h).resume_note.is_none());
            assert_eq!(Harness::parse(h.as_str()), Some(h));
        }
    }
}
