//! `gmclip owner connect claude-code|codex` (R-MCP-2, R-AGT-4, D16).
//!
//! Writes the client's MCP server entry, and for Claude Code the allow-entries for the
//! **read-only** gmclip tools only (never a wildcard, never a mutating tool). Uses the
//! client's own CLI when it is installed (`claude mcp add --scope user`, `codex mcp add`,
//! resolved to an absolute path without a shell); otherwise edits its config file. Every
//! file edit is shown as a diff before it is written. These are the only third-party
//! writes gmclip makes.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The read-only task tools R-AGT-4 names that exist at M1 (checked against the tool
/// list in a test); a static client may call these without asking.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "project_overview",
    "transcript_read",
    "cut_get",
    "moments_list",
    "lint",
    "see",
    "listen",
];

/// The disclosure shown before connecting (R-MCP-2).
pub const DISCLOSURE: &str = "This agent can read your transcripts, captions and moments and will send what it reads to its own AI provider.";

/// One file edit: what is there now and what will be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
}

/// The plan for one client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The client CLI command to run (absolute program, args), when the CLI exists.
    pub cli: Option<(PathBuf, Vec<String>)>,
    pub edits: Vec<FileEdit>,
    pub notes: Vec<String>,
}

/// Finds an executable on `PATH` (no shell, no aliases or functions).
#[cfg(unix)]
pub fn find_on_path(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let pv = path_var
        .map(str::to_owned)
        .or_else(|| std::env::var("PATH").ok())?;
    pv.split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Finds `<name>.exe` on `PATH` (absolute entries only; no shell). Never a `.cmd` or
/// `.bat`: the plan runs the CLI with arguments, and a batch file would hand them to
/// `cmd.exe` (CVE-2024-24576). An npm-installed client (a `.cmd` shim) is configured by
/// editing its files instead, as when no CLI is found.
#[cfg(windows)]
pub fn find_on_path(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let pv = path_var
        .map(str::to_owned)
        .or_else(|| std::env::var("PATH").ok())?;
    pv.split(';')
        .map(Path::new)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(format!("{name}.exe")))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

fn json_obj(text: Option<&str>) -> Result<serde_json::Map<String, Value>, String> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(serde_json::Map::new()),
        Some(t) => match serde_json::from_str::<Value>(t) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err("existing config is not a JSON object; not touching it".into()),
        },
    }
}

/// Claude Code: `~/.claude.json` `mcpServers.gmclip` (or `claude mcp add`), and
/// `~/.claude/settings.json` `permissions.allow` += the read-only tools.
pub fn claude_plan(
    home: &Path,
    gmclip: &Path,
    claude_cli: Option<PathBuf>,
) -> Result<Plan, String> {
    let mut edits = Vec::new();
    let cli = claude_cli.map(|c| {
        (
            c,
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "gmclip",
                "--",
                &gmclip.display().to_string(),
                "mcp",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        )
    });
    if cli.is_none() {
        let p = home.join(".claude.json");
        let before = read(&p);
        let mut m = json_obj(before.as_deref())?;
        let servers = m.entry("mcpServers").or_insert_with(|| json!({}));
        let Some(servers) = servers.as_object_mut() else {
            return Err("mcpServers is not an object; not touching it".into());
        };
        servers.insert("gmclip".into(), json!({"type": "stdio", "command": gmclip.display().to_string(), "args": ["mcp"], "env": {}}));
        edits.push(FileEdit {
            path: p,
            before,
            after: serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default() + "\n",
        });
    }
    let p = home.join(".claude").join("settings.json");
    let before = read(&p);
    let mut m = json_obj(before.as_deref())?;
    let perms = m.entry("permissions").or_insert_with(|| json!({}));
    let Some(perms) = perms.as_object_mut() else {
        return Err("permissions is not an object; not touching it".into());
    };
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    let Some(allow) = allow.as_array_mut() else {
        return Err("permissions.allow is not a list; not touching it".into());
    };
    for t in READ_ONLY_TOOLS {
        let e = Value::String(format!("mcp__gmclip__{t}"));
        if !allow.contains(&e) {
            allow.push(e);
        }
    }
    let after = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default() + "\n";
    if before.as_deref() != Some(after.as_str()) {
        edits.push(FileEdit {
            path: p,
            before,
            after,
        });
    }
    Ok(Plan {
        cli,
        edits,
        notes: vec![
            "Other gmclip tools (edits, exports, AI passes) will ask before each call.".into(),
        ],
    })
}

/// Codex: `codex mcp add`, else `~/.codex/config.toml` `[mcp_servers.gmclip]`.
pub fn codex_plan(home: &Path, gmclip: &Path, codex_cli: Option<PathBuf>) -> Result<Plan, String> {
    let notes = vec!["gmclip writes no Codex allow-list: Codex asks before gmclip calls under its own approval policy.".into()];
    if let Some(c) = codex_cli {
        return Ok(Plan {
            cli: Some((
                c,
                [
                    "mcp",
                    "add",
                    "gmclip",
                    "--",
                    &gmclip.display().to_string(),
                    "mcp",
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            )),
            edits: Vec::new(),
            notes,
        });
    }
    let p = home.join(".codex").join("config.toml");
    let before = read(&p);
    let text = before.clone().unwrap_or_default();
    if text.lines().any(|l| l.trim() == "[mcp_servers.gmclip]") {
        return Ok(Plan {
            cli: None,
            edits: Vec::new(),
            notes,
        });
    }
    let exe = gmclip
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let mut after = text.clone();
    if !after.is_empty() && !after.ends_with('\n') {
        after.push('\n');
    }
    after.push_str(&format!(
        "\n[mcp_servers.gmclip]\ncommand = \"{exe}\"\nargs = [\"mcp\"]\n"
    ));
    Ok(Plan {
        cli: None,
        edits: vec![FileEdit {
            path: p,
            before,
            after,
        }],
        notes,
    })
}

/// A line diff (LCS), for showing an edit before it is written.
pub fn diff(e: &FileEdit) -> String {
    let a: Vec<&str> = e.before.as_deref().unwrap_or("").lines().collect();
    let b: Vec<&str> = e.after.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut l = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i][j] = if a[i] == b[j] {
                l[i + 1][j + 1] + 1
            } else {
                l[i + 1][j].max(l[i][j + 1])
            };
        }
    }
    let mut out = format!("--- {}\n+++ {}\n", e.path.display(), e.path.display());
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < m && (i == n || l[i][j + 1] >= l[i + 1][j]) {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        }
    }
    out
}

/// Writes the plan's file edits: through a symlink to its target, with the file's own
/// mode (0600 for a new file), atomically; refused if a file changed since the diff was
/// shown (the client may have rewritten it in between).
pub fn apply_edits(plan: &Plan) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    for e in &plan.edits {
        if std::fs::read_to_string(&e.path).ok() != e.before {
            return Err(std::io::Error::other(format!(
                "{} changed since it was shown; run the command again",
                e.path.display()
            )));
        }
    }
    for e in &plan.edits {
        let target = match std::fs::symlink_metadata(&e.path) {
            Ok(m) if m.file_type().is_symlink() => e.path.canonicalize()?,
            _ => e.path.clone(),
        };
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = target.with_extension("gmclip-tmp");
        let _ = std::fs::remove_file(&tmp);
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&target).map_or(0o600, |m| m.permissions().mode() & 0o777);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&tmp)?;
            f.write_all(e.after.as_bytes())?;
            f.sync_all()?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        // Windows: the new file takes its directory's ACL (the user's profile)
        #[cfg(windows)]
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(e.after.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory per call: unique by process and an atomic counter (the clock
    /// alone repeats across parallel tests), emptied first.
    fn home() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let h = std::env::temp_dir().join(format!("gmclip-connect-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    /// R-AGT-4 acceptance: no `*`, and the allow set equals the read-only list.
    #[test]
    fn claude_config_allows_exactly_the_read_only_tools() {
        let h = home();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude/settings.json"),
            r#"{"permissions": {"allow": ["Bash(ls)"]}, "theme": "dark"}"#,
        )
        .unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"mcpServers": {"other": {"command": "x"}}, "numStartups": 3}"#,
        )
        .unwrap();
        let plan = claude_plan(
            &h,
            Path::new("/Applications/gmclip.app/Contents/MacOS/gmclip"),
            None,
        )
        .unwrap();
        apply_edits(&plan).unwrap();
        let s: Value = serde_json::from_str(
            &std::fs::read_to_string(h.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let allow: Vec<&str> = s["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let gm: Vec<&str> = allow
            .iter()
            .copied()
            .filter(|a| a.starts_with("mcp__gmclip"))
            .collect();
        assert_eq!(
            gm,
            READ_ONLY_TOOLS
                .iter()
                .map(|t| format!("mcp__gmclip__{t}"))
                .collect::<Vec<_>>()
        );
        assert!(allow.iter().all(|a| !a.contains('*')));
        assert!(allow.contains(&"Bash(ls)"), "the user's own entries stay");
        assert_eq!(s["theme"], "dark");
        for t in [
            "cut_edit",
            "layout_edit",
            "export",
            "ai_pass",
            "command_run",
            "command_batch",
        ] {
            assert!(
                !gm.iter().any(|a| a.ends_with(t)),
                "{t} must never be allow-listed"
            );
        }
        let c: Value =
            serde_json::from_str(&std::fs::read_to_string(h.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(c["mcpServers"]["gmclip"]["args"], json!(["mcp"]));
        assert_eq!(c["mcpServers"]["other"]["command"], "x");
        assert_eq!(c["numStartups"], 3);
        // idempotent: a second plan changes nothing
        let again = claude_plan(
            &h,
            Path::new("/Applications/gmclip.app/Contents/MacOS/gmclip"),
            None,
        )
        .unwrap();
        assert!(
            again
                .edits
                .iter()
                .all(|e| e.before.as_deref() == Some(e.after.as_str()))
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn codex_config_gets_the_server_entry_once() {
        let h = home();
        std::fs::create_dir_all(h.join(".codex")).unwrap();
        std::fs::write(h.join(".codex/config.toml"), "model = \"o9\"\n").unwrap();
        let plan = codex_plan(&h, Path::new("/opt/gmclip"), None).unwrap();
        assert!(diff(&plan.edits[0]).contains("+[mcp_servers.gmclip]"));
        apply_edits(&plan).unwrap();
        let t = std::fs::read_to_string(h.join(".codex/config.toml")).unwrap();
        assert!(t.starts_with("model = \"o9\"\n"));
        assert!(t.contains("[mcp_servers.gmclip]\ncommand = \"/opt/gmclip\"\nargs = [\"mcp\"]"));
        assert!(!t.contains('*'));
        assert!(
            codex_plan(&h, Path::new("/opt/gmclip"), None)
                .unwrap()
                .edits
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn the_client_cli_is_used_when_installed() {
        let p = claude_plan(
            Path::new("/nonexistent"),
            Path::new("/opt/gmclip"),
            Some("/usr/local/bin/claude".into()),
        )
        .unwrap();
        let (prog, args) = p.cli.unwrap();
        assert_eq!(prog, PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(
            args,
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "gmclip",
                "--",
                "/opt/gmclip",
                "mcp"
            ]
        );
        assert!(
            p.edits.iter().all(|e| !e.path.ends_with(".claude.json")),
            "the server entry goes through the CLI"
        );
    }

    #[test]
    fn read_only_tools_exist_and_are_read_scope_or_read_media() {
        use gmclip_core::command::Scope;
        for t in READ_ONLY_TOOLS {
            let s = crate::tools::scope_of(t).unwrap_or_else(|| panic!("{t} is not a tool"));
            assert!(matches!(s, Scope::Read | Scope::ReadMedia), "{t}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn edits_keep_the_mode_follow_symlinks_and_refuse_a_changed_file() {
        use std::os::unix::fs::PermissionsExt;
        let h = home();
        let real = h.join("real.json");
        std::fs::write(&real, "{}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, h.join(".claude.json")).unwrap();
        let plan = claude_plan(&h, Path::new("/opt/gmclip"), None).unwrap();
        apply_edits(&plan).unwrap();
        assert!(
            std::fs::symlink_metadata(h.join(".claude.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(std::fs::read_to_string(&real).unwrap().contains("gmclip"));
        assert_eq!(
            std::fs::metadata(h.join(".claude/settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // changed between the diff and the write: refused, nothing written
        std::fs::write(h.join(".claude/settings.json"), "{}").unwrap();
        let plan = claude_plan(&h, Path::new("/opt/gmclip2"), None).unwrap();
        std::fs::write(h.join(".claude/settings.json"), r#"{"theme": "light"}"#).unwrap();
        assert!(apply_edits(&plan).is_err());
        assert!(!std::fs::read_to_string(&real).unwrap().contains("gmclip2"));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_config_that_is_not_an_object_is_left_alone() {
        let h = home();
        std::fs::write(h.join(".claude.json"), "[1,2]").unwrap();
        assert!(claude_plan(&h, Path::new("/opt/gmclip"), None).is_err());
        let _ = std::fs::remove_dir_all(&h);
    }
}
