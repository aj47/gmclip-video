//! The run's tool surface and its enumerated allow-list (R-AGT-1, R-AGT-4, SPEC §11.2/§11.4).
//!
//! The launcher never writes a wildcard: every allow entry names one gmclip tool, and the
//! set is derived from the tool surface for the run's scopes. The surface is passed in by
//! the caller (the CLI owns the MCP registry); [`M1_SURFACE`] is the provisional copy of
//! lane A2's `gmclip_mcp::tools` table (names and scopes as of A2's working tree), used
//! until that crate is on main.
//!
//! TODO(A2): once `gmclip-mcp` merges, the CLI builds the surface from
//! `gmclip_mcp::tools::defs()` and a test asserts it equals this table (or this table
//! goes away).

use serde::{Deserialize, Serialize};

/// The MCP server name gmclip registers in a client's config. Claude Code and Codex both
/// expose its tools as `mcp__gmclip__<tool>`.
pub const SERVER_NAME: &str = "gmclip";

/// A scope (SPEC §11.4). `Admin` and Owner never reach a launched run.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    ReadMedia,
    Edit,
    Render,
    Ai,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::ReadMedia => "read_media",
            Scope::Edit => "edit",
            Scope::Render => "render",
            Scope::Ai => "ai",
        }
    }
}

/// Default agent scopes (SPEC §11.4): `read, edit, render, ai`. `read_media` only per run.
pub const DEFAULT_SCOPES: [Scope; 4] = [Scope::Read, Scope::Edit, Scope::Render, Scope::Ai];

/// One tool on the MCP surface.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub scope: Scope,
}

const fn t(name: &'static str, scope: Scope) -> ToolDef {
    ToolDef { name, scope }
}

/// Layer 1 (16 task tools) + layers 2–3 (4), in registry order (SPEC §11.2).
pub const M1_SURFACE: &[ToolDef] = &[
    t("project_overview", Scope::Read),
    t("project_watch", Scope::Read),
    t("transcript_read", Scope::Read),
    t("cut_get", Scope::Read),
    t("cut_edit", Scope::Edit),
    t("layout_edit", Scope::Edit),
    t("moments_list", Scope::Read),
    t("analysis_get", Scope::Read),
    t("measure", Scope::Read),
    t("see", Scope::ReadMedia),
    t("listen", Scope::ReadMedia),
    t("verify", Scope::Read),
    t("lint", Scope::Read),
    t("ai_pass", Scope::Ai),
    t("pass_step", Scope::Ai),
    t("export", Scope::Render),
    t("commands_search", Scope::Read),
    t("command_describe", Scope::Read),
    t("command_run", Scope::Read),
    t("command_batch", Scope::Read),
];

/// Tools that must never be on any agent surface (R-PROF-1, R-OWN-1): their presence in
/// a surface handed to the launcher is a bug, and the launcher refuses.
pub const NEVER_ON_AGENT_SURFACE: &[&str] = &["feedback_append", "owner_call", "launch_register"];

/// The client-facing name of a gmclip tool.
pub fn qualified(name: &str) -> String {
    format!("mcp__{SERVER_NAME}__{name}")
}

/// Why a surface cannot be turned into an allow-list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurfaceError {
    /// A name is not `^[a-z][a-z0-9_]{0,63}$` (a wildcard, a pattern, a path, …).
    BadName(String),
    Duplicate(String),
    Forbidden(String),
    /// The run's scopes select no tool at all.
    Empty,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SurfaceError::BadName(n) => write!(f, "tool name {n:?} is not an exact name"),
            SurfaceError::Duplicate(n) => write!(f, "tool {n:?} listed twice"),
            SurfaceError::Forbidden(n) => write!(f, "tool {n:?} may never be on an agent surface"),
            SurfaceError::Empty => write!(f, "the run's scopes allow no tool"),
        }
    }
}

fn exact_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// The run's tools: every surface entry whose scope the run holds, in surface order, as
/// bare names. Refuses wildcards or patterns, duplicates and forbidden tools.
pub fn run_tools(surface: &[ToolDef], scopes: &[Scope]) -> Result<Vec<&'static str>, SurfaceError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for d in surface {
        if !exact_name(d.name) {
            return Err(SurfaceError::BadName(d.name.to_owned()));
        }
        if NEVER_ON_AGENT_SURFACE.contains(&d.name) {
            return Err(SurfaceError::Forbidden(d.name.to_owned()));
        }
        if !seen.insert(d.name) {
            return Err(SurfaceError::Duplicate(d.name.to_owned()));
        }
        if scopes.contains(&d.scope) {
            out.push(d.name);
        }
    }
    if out.is_empty() {
        return Err(SurfaceError::Empty);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scopes_exclude_media_tools() {
        let tools = run_tools(M1_SURFACE, &DEFAULT_SCOPES).unwrap();
        assert_eq!(tools.len(), 18);
        assert!(!tools.contains(&"see") && !tools.contains(&"listen"));
        assert!(tools.contains(&"cut_edit") && tools.contains(&"export"));
    }

    #[test]
    fn media_grant_adds_see_and_listen() {
        let mut s = DEFAULT_SCOPES.to_vec();
        s.push(Scope::ReadMedia);
        let tools = run_tools(M1_SURFACE, &s).unwrap();
        assert_eq!(tools.len(), 20);
        assert_eq!(
            M1_SURFACE.len(),
            20,
            "16 task tools + 4 discovery/layer-3 (SPEC §11.2)"
        );
    }

    #[test]
    fn wildcards_patterns_and_forbidden_tools_are_refused() {
        for bad in [
            "*",
            "mcp__gmclip__*",
            "cut_*",
            "Cut_get",
            "",
            "a b",
            "../x",
            "x(y)",
        ] {
            let surface = [ToolDef {
                name: Box::leak(bad.to_owned().into_boxed_str()),
                scope: Scope::Read,
            }];
            assert!(
                matches!(
                    run_tools(&surface, &DEFAULT_SCOPES),
                    Err(SurfaceError::BadName(_))
                ),
                "{bad:?}"
            );
        }
        let surface = [t("feedback_append", Scope::Edit)];
        assert_eq!(
            run_tools(&surface, &DEFAULT_SCOPES),
            Err(SurfaceError::Forbidden("feedback_append".into()))
        );
        let surface = [t("lint", Scope::Read), t("lint", Scope::Read)];
        assert_eq!(
            run_tools(&surface, &DEFAULT_SCOPES),
            Err(SurfaceError::Duplicate("lint".into()))
        );
        assert_eq!(
            run_tools(&[t("see", Scope::ReadMedia)], &DEFAULT_SCOPES),
            Err(SurfaceError::Empty)
        );
    }

    #[test]
    fn qualified_names() {
        assert_eq!(qualified("cut_get"), "mcp__gmclip__cut_get");
    }
}
