//! Finding clients on Windows (decisions/0019), as pure logic so it is tested on every OS.
//!
//! - **Extensions.** A client on Windows is `<name>.exe` (a native install: Claude Code's
//!   installer puts `claude.exe` in `%USERPROFILE%\.local\bin`) or `<name>.cmd` (npm's
//!   shim in `%APPDATA%\npm`). `PATHEXT` sets the order; nothing else is looked for
//!   (`.bat`, `.ps1`, `.com`, a bare name).
//! - **A `.cmd` is never run.** `std::process::Command` on a batch file hands its
//!   arguments to `cmd.exe`, whose quoting no escaping makes safe (BatBadBut,
//!   CVE-2024-24576), and the launcher's arguments carry the prompt's paths. The shim is
//!   read instead: npm's `cmd-shim` names its target as `"%dp0%\<relative path>"`, which
//!   [`cmd_shim_target`] extracts. The launcher then runs that target the way it runs the
//!   same file on Unix: a native binary directly, the Codex wrapper as its vendored
//!   binary, pi's `cli.js` under an absolute `node.exe`.
//! - **Where.** After the launcher's own `PATH`: the per-user install folders
//!   ([`user_dirs`]), then nvm-windows' node versions, then `%ProgramFiles%\nodejs`.

use std::path::{Path, PathBuf};

/// The extensions a client may have, in `PATHEXT` order (default `.EXE;.CMD`).
pub fn extensions(pathext: Option<&str>) -> Vec<&'static str> {
    let mut out = Vec::new();
    let listed = pathext.filter(|p| !p.trim().is_empty());
    for e in listed.unwrap_or(".EXE;.CMD").split(';') {
        let e = e.trim();
        let ours = if e.eq_ignore_ascii_case(".exe") {
            ".exe"
        } else if e.eq_ignore_ascii_case(".cmd") {
            ".cmd"
        } else {
            continue;
        };
        if !out.contains(&ours) {
            out.push(ours);
        }
    }
    // a PATHEXT without either still finds a native install
    if out.is_empty() {
        out.push(".exe");
    }
    out
}

/// `<dir>\<name><ext>` for each allowed extension.
pub fn candidates_in(dir: &Path, name: &str, pathext: Option<&str>) -> Vec<PathBuf> {
    extensions(pathext)
        .into_iter()
        .map(|e| dir.join(format!("{name}{e}")))
        .collect()
}

/// Is this a batch file (`.cmd` / `.bat`, any case)? Never spawned.
pub fn is_batch(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// The most a shim is read (npm's are ~400 bytes).
pub const SHIM_MAX: usize = 8 * 1024;

/// The target an npm `cmd-shim` runs, relative to the shim's folder: the last
/// `"%dp0%\…"` (or older `"%~dp0\…"`) quoted path that is not `node.exe`. `None` for
/// anything else, including a target that leaves the folder (`..`), is absolute, or
/// carries another variable or quote.
pub fn cmd_shim_target(text: &str) -> Option<String> {
    // (position in the text, path): the last one on the command line wins
    let mut found: Option<(usize, &str)> = None;
    for prefix in ["\"%dp0%\\", "\"%~dp0\\"] {
        let mut from = 0;
        while let Some(i) = text[from..].find(prefix) {
            let at = from + i;
            let after = &text[at + prefix.len()..];
            let Some(end) = after.find('"') else {
                break;
            };
            let rel = &after[..end];
            if !rel.eq_ignore_ascii_case("node.exe")
                && !rel.eq_ignore_ascii_case("node")
                && found.is_none_or(|(p, _)| at > p)
            {
                found = Some((at, rel));
            }
            from = at + prefix.len() + end + 1;
        }
    }
    let (_, rel) = found?;
    let ok = !rel.is_empty()
        && rel.len() <= 1024
        && !rel.contains(['%', '"', ':', '*', '?', '<', '>', '|', '\n', '\r'])
        && !rel.starts_with(['\\', '/'])
        && rel
            .split(['\\', '/'])
            .all(|c| !c.is_empty() && c != "." && c != "..");
    ok.then(|| rel.to_owned())
}

/// The per-user install folders searched after `PATH`, in order: Claude Code's native
/// installer, npm's global prefix, bun, Volta, opencode's installer.
pub fn user_dirs(home: &Path, appdata: Option<&Path>, localappdata: Option<&Path>) -> Vec<PathBuf> {
    let appdata = appdata.map_or_else(|| home.join("AppData").join("Roaming"), Path::to_owned);
    let local = localappdata.map_or_else(|| home.join("AppData").join("Local"), Path::to_owned);
    vec![
        home.join(".local").join("bin"),
        appdata.join("npm"),
        home.join(".bun").join("bin"),
        local.join("Volta").join("bin"),
        home.join(".opencode").join("bin"),
    ]
}

/// nvm-windows' node folders (`%APPDATA%\nvm\v<x.y.z>`, each with its own global
/// shims), newest first.
pub fn nvm_dirs(entries: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let ver = |p: &Path| -> Vec<u64> {
        p.file_name()
            .map(|n| {
                n.to_string_lossy()
                    .trim_start_matches('v')
                    .split('.')
                    .map(|x| x.parse().unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut v: Vec<PathBuf> = entries
        .into_iter()
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with('v') && n[1..].starts_with(|c: char| c.is_ascii_digit())
            })
        })
        .collect();
    v.sort_by_key(|p| std::cmp::Reverse(ver(p)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathext_picks_exe_and_cmd_only_in_its_order() {
        assert_eq!(extensions(None), [".exe", ".cmd"]);
        assert_eq!(
            extensions(Some(".COM;.CMD;.BAT;.EXE;.VBS;.JS;.PS1")),
            [".cmd", ".exe"]
        );
        assert_eq!(extensions(Some(".exe;.EXE")), [".exe"]);
        assert_eq!(extensions(Some(".BAT;.PS1")), [".exe"]);
        assert_eq!(extensions(Some("  ")), [".exe", ".cmd"]);
        let c = candidates_in(Path::new("d"), "claude", None);
        assert_eq!(
            c,
            [
                Path::new("d").join("claude.exe"),
                Path::new("d").join("claude.cmd")
            ]
        );
        assert!(is_batch(Path::new("x/claude.CMD")) && is_batch(Path::new("x.bat")));
        assert!(!is_batch(Path::new("claude.exe")) && !is_batch(Path::new("claude")));
    }

    /// The shims npm 7–11 writes (cmd-shim 4–7), and the older `%~dp0` form.
    #[test]
    fn npm_shims_name_their_target() {
        let modern = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@earendil-works\\pi-coding-agent\\dist\\bundle\\cli.js\" %*\r\n";
        assert_eq!(
            cmd_shim_target(modern).as_deref(),
            Some(r"node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.js")
        );
        let native = "@ECHO off\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\"   %*\r\n";
        assert_eq!(
            cmd_shim_target(native).as_deref(),
            Some(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe")
        );
        let old = "@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n)";
        assert_eq!(
            cmd_shim_target(old).as_deref(),
            Some(r"node_modules\@openai\codex\bin\codex.js")
        );
    }

    #[test]
    fn a_shim_that_leaves_its_folder_or_hides_a_variable_is_refused() {
        for bad in [
            "\"%dp0%\\..\\..\\evil.exe\" %*",
            "\"%dp0%\\a\\..\\b.js\" %*",
            "\"%dp0%\\C:\\x.exe\" %*",
            "\"%dp0%\\%OTHER%\\x.exe\" %*",
            "\"%dp0%\\\\x.exe\" %*",
            "\"%dp0%\\node.exe\" %*",
            "\"%dp0%\\a\\\\b.js\" %*",
            "\"%dp0%\\unterminated.js %*",
            "C:\\tools\\claude.exe %*",
            "",
        ] {
            assert_eq!(cmd_shim_target(bad), None, "{bad}");
        }
    }

    #[test]
    fn user_dirs_and_nvm_order() {
        let home = Path::new("/h");
        let d = user_dirs(home, None, Some(Path::new("/l")));
        assert_eq!(d[0], home.join(".local").join("bin"));
        assert_eq!(d[1], home.join("AppData").join("Roaming").join("npm"));
        assert!(d.contains(&Path::new("/l").join("Volta").join("bin")));
        assert!(d.contains(&home.join(".opencode").join("bin")));
        let n = nvm_dirs(
            ["v20.1.0", "v24.1.0", "settings.txt", "v22.5.1", "vx"]
                .map(|v| Path::new("/a/nvm").join(v)),
        );
        let names: Vec<_> = n
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["v24.1.0", "v22.5.1", "v20.1.0"]);
    }
}
