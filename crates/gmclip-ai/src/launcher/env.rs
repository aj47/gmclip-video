//! The child's environment (R-AGT-1 "Environment whitelist", R-AGT-5).
//!
//! The child starts from an empty environment. It gets:
//! - `PATH`, set by the launcher: the client binary's own directory, then the system
//!   directories. Nothing from the parent's `PATH` (which on AJ's machine has ~40 entries,
//!   several user-writable).
//! - `HOME`, `LANG`, `LC_*`, `TMPDIR` from the parent (the client needs `HOME` for its
//!   plan login: Claude Code's keychain entry, Codex's `$CODEX_HOME/auth.json`).
//! - `GMCLIP_RUN` = `<run id>:<credential hex>` (the `gmclip mcp` shim's contract,
//!   `gmclip_mcp::wire::parse_run_env`), only here — never argv or a file.
//! - Codex only: `CODEX_HOME` when the parent sets it, because §10.7 runs Codex "with the
//!   user's real `CODEX_HOME` used for auth only"; dropping it would point Codex at
//!   `~/.codex` instead of the user's real home. (Recorded as an addition to R-AGT-1's
//!   list in `spikes/S7/README.md`.)
//!
//! - pi only: `PI_CODING_AGENT_DIR` when the parent sets it (where pi's login lives, as
//!   `CODEX_HOME` is for Codex).
//! - opencode only: `XDG_DATA_HOME` when the parent sets it (where its `auth.json` is).
//!
//! And the variables the launcher itself sets for a client ([`launcher_set`]): pi's
//! offline and telemetry switches; opencode's config, state, cache and session database
//! pointed into the run's `config/` and its Claude-Code-compatibility, external-skill,
//! project-config, plugin, update and share switches off. Their values are fixed or are
//! paths in the run directory, never the user's.
//!
//! Everything else is dropped, notably `ANTHROPIC_BASE_URL`, `*_API_KEY`, proxy
//! variables, `DYLD_*`, `NODE_OPTIONS`, `CLAUDE_*`, `CODEX_*` other than `CODEX_HOME`.
//!
//! Windows (decisions/0019) adds what a Windows program needs to start and to find its
//! login, compared without case as Windows does ([`WINDOWS_INHERITED`]): the profile
//! folders (`USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `HOMEDRIVE`/`HOMEPATH`),
//! `SystemRoot`/`windir`/`SystemDrive` (Winsock and crypto fail without them), `TEMP`/
//! `TMP`, `USERNAME`, `PATHEXT`. `PATH`'s system part is `%SystemRoot%\System32` and
//! `%SystemRoot%`. Not `ComSpec`: nothing a client runs should find a shell by it.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::Harness;
use super::credential::RunCredential;

/// The credential variable (R-AGT-5).
pub const RUN_VAR: &str = "GMCLIP_RUN";

/// System directories appended to the child's `PATH`.
#[cfg(unix)]
const SYSTEM_PATH: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
#[cfg(not(unix))]
const SYSTEM_PATH: &[&str] = &[];

/// The system part of the child's `PATH`.
fn system_path() -> Vec<std::path::PathBuf> {
    #[allow(unused_mut)]
    let mut v: Vec<std::path::PathBuf> = SYSTEM_PATH.iter().map(std::path::PathBuf::from).collect();
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .filter(|r| !r.is_empty())
            .map_or_else(
                || std::path::PathBuf::from(r"C:\Windows"),
                std::path::PathBuf::from,
            );
        v.push(root.join("System32"));
        v.push(root);
    }
    v
}

/// The Windows variables a child keeps (any case).
#[cfg(any(windows, test))]
pub const WINDOWS_INHERITED: &[&str] = &[
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "TEMP",
    "TMP",
    "USERNAME",
    "PATHEXT",
    // the holder pipe of this process tree (gmclip-mcp paths; the Windows TMPDIR)
    "GMCLIP_PIPE_TAG",
];

#[cfg(any(windows, test))]
fn windows_inherited(key: &str) -> bool {
    WINDOWS_INHERITED
        .iter()
        .any(|w| w.eq_ignore_ascii_case(key))
}

fn inherited(key: &str, h: Harness) -> bool {
    #[cfg(windows)]
    if windows_inherited(key) {
        return true;
    }
    matches!(key, "HOME" | "LANG" | "TMPDIR")
        || key.starts_with("LC_")
        || (h == Harness::Codex && key == "CODEX_HOME")
        || (h == Harness::Pi && key == "PI_CODING_AGENT_DIR")
        || (h == Harness::OpenCode && key == "XDG_DATA_HOME")
}

/// The keys [`launcher_set`] sets for `h`.
pub fn launcher_keys(h: Harness) -> &'static [&'static str] {
    match h {
        Harness::ClaudeCode | Harness::Codex => &[],
        Harness::Pi => &["PI_OFFLINE", "PI_SKIP_VERSION_CHECK", "PI_TELEMETRY"],
        Harness::OpenCode => &[
            "OPENCODE_CONFIG",
            "OPENCODE_DB",
            "OPENCODE_DISABLE_AUTOUPDATE",
            "OPENCODE_DISABLE_CLAUDE_CODE",
            "OPENCODE_DISABLE_DEFAULT_PLUGINS",
            "OPENCODE_DISABLE_EXTERNAL_SKILLS",
            "OPENCODE_DISABLE_LSP_DOWNLOAD",
            "OPENCODE_DISABLE_PROJECT_CONFIG",
            "OPENCODE_DISABLE_SHARE",
            "OPENCODE_PURE",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
        ],
    }
}

/// The variables the launcher sets for `h`, with `config` the run's `config/` directory.
pub fn launcher_set(h: Harness, config: &Path) -> Vec<(OsString, OsString)> {
    let on = |k: &str| (OsString::from(k), OsString::from("1"));
    // `rel` is written with `/`; joined per component so Windows gets native separators
    let path = |k: &str, rel: &str| {
        let p = rel.split('/').fold(config.to_path_buf(), |p, c| p.join(c));
        (OsString::from(k), p.into_os_string())
    };
    let v: Vec<(OsString, OsString)> = match h {
        Harness::ClaudeCode | Harness::Codex => Vec::new(),
        Harness::Pi => vec![
            on("PI_OFFLINE"),
            on("PI_SKIP_VERSION_CHECK"),
            (OsString::from("PI_TELEMETRY"), OsString::from("0")),
        ],
        Harness::OpenCode => vec![
            path("OPENCODE_CONFIG", super::argv::OPENCODE_CONFIG),
            path("OPENCODE_DB", super::argv::OPENCODE_DB),
            on("OPENCODE_DISABLE_AUTOUPDATE"),
            on("OPENCODE_DISABLE_CLAUDE_CODE"),
            on("OPENCODE_DISABLE_DEFAULT_PLUGINS"),
            on("OPENCODE_DISABLE_EXTERNAL_SKILLS"),
            on("OPENCODE_DISABLE_LSP_DOWNLOAD"),
            on("OPENCODE_DISABLE_PROJECT_CONFIG"),
            on("OPENCODE_DISABLE_SHARE"),
            on("OPENCODE_PURE"),
            path("XDG_CACHE_HOME", "xdg/cache"),
            path("XDG_CONFIG_HOME", "xdg/config"),
            path("XDG_STATE_HOME", "xdg/state"),
        ],
    };
    debug_assert!(
        v.iter()
            .all(|(k, _)| launcher_keys(h).contains(&k.to_str().unwrap_or("")))
    );
    v
}

/// The child's environment, sorted by key. `parent` is the launcher's own environment;
/// `config` the run's `config/` directory (for [`launcher_set`]).
pub fn child_env(
    h: Harness,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    client_exec: &Path,
    run: &str,
    credential: &RunCredential,
    config: &Path,
) -> Vec<(OsString, OsString)> {
    let mut out: Vec<(OsString, OsString)> = launcher_set(h, config);
    out.extend(
        parent
            .into_iter()
            .filter(|(k, _)| k.to_str().is_some_and(|k| inherited(k, h))),
    );
    let mut path_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = client_exec.parent() {
        path_dirs.push(dir.to_owned());
    }
    path_dirs.extend(system_path());
    let path = std::env::join_paths(path_dirs).unwrap_or_default();
    out.push((OsString::from("PATH"), path));
    out.push((
        OsString::from(RUN_VAR),
        OsString::from(format!("{run}:{}", credential.expose_hex())),
    ));
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The keys of an environment, for the run record (values are never recorded).
pub fn keys(env: &[(OsString, OsString)]) -> Vec<String> {
    env.iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

/// Whether an env key is one R-AGT-1 permits (the S7 `ps eww` assertion uses this).
pub fn allowed_key(h: Harness, key: &OsStr) -> bool {
    key.to_str().is_some_and(|k| {
        inherited(k, h) || k == "PATH" || k == RUN_VAR || launcher_keys(h).contains(&k)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> Vec<(OsString, OsString)> {
        [
            ("HOME", "/Users/x"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "C"),
            ("TMPDIR", "/var/folders/t/T/"),
            ("PATH", "/evil/bin:/usr/bin"),
            ("ANTHROPIC_BASE_URL", "http://evil"),
            ("ANTHROPIC_API_KEY", "sk-1"),
            ("OPENAI_API_KEY", "sk-2"),
            ("HTTPS_PROXY", "http://p"),
            ("DYLD_INSERT_LIBRARIES", "/x.dylib"),
            ("NODE_OPTIONS", "--require /x.js"),
            ("CLAUDE_CODE_SIMPLE", "1"),
            ("CODEX_HOME", "/Users/x/.codex-real"),
            ("CODEX_MANAGED_BY_NPM", "1"),
            ("GMCLIP_RUN", "stale-from-parent"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    #[test]
    fn only_whitelisted_keys_reach_the_child() {
        let cred = RunCredential::from_bytes([7; 32]);
        let env = child_env(
            Harness::ClaudeCode,
            parent(),
            Path::new("/opt/c/bin/claude"),
            "run1",
            &cred,
            Path::new("/tmp/run/config"),
        );
        assert_eq!(
            keys(&env),
            ["GMCLIP_RUN", "HOME", "LANG", "LC_ALL", "PATH", "TMPDIR"]
        );
        let get = |k: &str| {
            env.iter()
                .find(|(a, _)| a == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(
            get("GMCLIP_RUN"),
            OsString::from(format!("run1:{}", cred.expose_hex()))
        );
        let path = get("PATH").into_string().unwrap();
        assert!(path.starts_with("/opt/c/bin"), "{path}");
        assert!(!path.contains("/evil"), "{path}");
    }

    #[test]
    fn the_windows_list_is_matched_without_case() {
        assert!(windows_inherited("SystemRoot") && windows_inherited("windir"));
        assert!(windows_inherited("UserProfile") && windows_inherited("LOCALAPPDATA"));
        for k in [
            "ComSpec",
            "Path",
            "PSModulePath",
            "NODE_OPTIONS",
            "APPDATA2",
        ] {
            assert!(!windows_inherited(k), "{k}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_children_get_the_profile_and_system_root() {
        let cred = RunCredential::from_bytes([3; 32]);
        let mut p = parent();
        p.push(("SystemRoot".into(), r"C:\Windows".into()));
        p.push(("USERPROFILE".into(), r"C:\Users\x".into()));
        p.push(("ComSpec".into(), r"C:\Windows\system32\cmd.exe".into()));
        let env = child_env(
            Harness::ClaudeCode,
            p,
            Path::new(r"C:\Users\x\.local\bin\claude.exe"),
            "run1",
            &cred,
            Path::new(r"C:\t\config"),
        );
        let k = keys(&env);
        assert!(k.contains(&"SystemRoot".to_owned()) && k.contains(&"USERPROFILE".to_owned()));
        assert!(!k.contains(&"ComSpec".to_owned()));
        let path = env.iter().find(|(a, _)| a == "PATH").unwrap().1.clone();
        assert!(path.to_string_lossy().contains(r"\System32"), "{path:?}");
    }

    #[test]
    fn codex_keeps_codex_home_only() {
        let cred = RunCredential::from_bytes([1; 32]);
        let env = child_env(
            Harness::Codex,
            parent(),
            Path::new("/opt/x/codex"),
            "run1",
            &cred,
            Path::new("/tmp/run/config"),
        );
        let k = keys(&env);
        assert!(k.contains(&"CODEX_HOME".to_owned()));
        assert!(!k.contains(&"CODEX_MANAGED_BY_NPM".to_owned()));
        for (key, _) in &env {
            assert!(allowed_key(Harness::Codex, key), "{key:?}");
        }
        assert!(!allowed_key(Harness::ClaudeCode, OsStr::new("CODEX_HOME")));
    }

    /// opencode's config, state, cache and database point into the run; its login dir
    /// is inherited only when the parent sets one; nothing for one client leaks to another.
    #[test]
    fn opencode_and_pi_get_their_switches_and_run_paths_only() {
        let cred = RunCredential::from_bytes([2; 32]);
        let mut parent = parent();
        parent.push(("XDG_DATA_HOME".into(), "/Users/x/.local/share".into()));
        parent.push(("XDG_CONFIG_HOME".into(), "/Users/x/.config".into()));
        parent.push(("PI_CODING_AGENT_DIR".into(), "/Users/x/.pi/agent".into()));
        parent.push(("OPENCODE_CONFIG".into(), "/Users/x/evil.json".into()));
        let cfg = Path::new("/tmp/run/config");
        let env = child_env(
            Harness::OpenCode,
            parent.clone(),
            Path::new("/opt/o/opencode"),
            "run1",
            &cred,
            cfg,
        );
        let get = |k: &str| env.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        assert_eq!(
            get("XDG_CONFIG_HOME").unwrap(),
            cfg.join("xdg").join("config").into_os_string()
        );
        assert_eq!(
            get("OPENCODE_CONFIG").unwrap(),
            cfg.join("opencode.json").into_os_string()
        );
        assert_eq!(
            get("OPENCODE_DB").unwrap(),
            cfg.join("opencode.db").into_os_string()
        );
        assert_eq!(get("XDG_DATA_HOME").unwrap(), "/Users/x/.local/share");
        assert_eq!(get("OPENCODE_DISABLE_CLAUDE_CODE").unwrap(), "1");
        assert!(get("PI_CODING_AGENT_DIR").is_none() && get("CODEX_HOME").is_none());
        for (key, _) in &env {
            assert!(allowed_key(Harness::OpenCode, key), "{key:?}");
        }
        let env = child_env(
            Harness::Pi,
            parent,
            Path::new("/opt/homebrew/Cellar/node/26/bin/node"),
            "run1",
            &cred,
            cfg,
        );
        let k = keys(&env);
        assert!(
            k.contains(&"PI_CODING_AGENT_DIR".to_owned()) && k.contains(&"PI_OFFLINE".to_owned())
        );
        assert!(
            !k.iter()
                .any(|k| k.starts_with("XDG_") || k.starts_with("OPENCODE"))
        );
        assert!(!allowed_key(Harness::ClaudeCode, OsStr::new("OPENCODE_DB")));
    }
}
