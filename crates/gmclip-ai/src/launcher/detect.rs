//! Finding the user's agents (backlog G.02): which of the four clients are installed,
//! where, at what version, and whether a launched run may start on that build.
//!
//! The same rules as a launch (R-AGT-1): files only, never a shell, so an alias or a
//! function cannot apply; every candidate is resolved by [`super::resolve`] (symlinks
//! canonicalised, the Codex npm wrapper to its native binary, pi to the `node` beside
//! it) and the first that resolves wins. An app started from the Dock has a bare `PATH`,
//! so after the launcher's own `PATH` it looks where npm, nvm, bun, Homebrew and the
//! clients' own installers put them.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::clients;
use super::resolve::{self, ResolveError};
use super::version::{self, Verdict};
use super::{Harness, ProcessContext};

/// The system-wide install folders searched last.
#[cfg(not(windows))]
pub const SYSTEM_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];
/// Windows: none fixed; `%ProgramFiles%\nodejs` is added from the environment
/// ([`candidates`]).
#[cfg(windows)]
pub const SYSTEM_DIRS: &[&str] = &[];

/// Where to look.
#[derive(Clone, Copy, Debug)]
pub struct Places<'a> {
    /// The launcher's own `PATH`.
    pub path_var: Option<&'a OsStr>,
    /// The user's home (nvm, ~/.local/bin, ~/.bun/bin, …).
    pub home: Option<&'a Path>,
    /// [`SYSTEM_DIRS`] (tests pass none).
    pub system: &'a [&'a str],
}

/// The places a client is looked for, in order: the given `PATH` (absolute entries),
/// nvm's node versions (newest first), then the per-user and system install folders.
#[cfg(not(windows))]
pub fn candidates(h: Harness, places: &Places<'_>) -> Vec<PathBuf> {
    let extra: Vec<PathBuf> = places
        .home
        .map(|home| match h {
            Harness::ClaudeCode => vec![home.join(".claude/local/claude")],
            Harness::OpenCode => vec![home.join(".opencode/bin/opencode")],
            Harness::Codex | Harness::Pi => vec![],
        })
        .unwrap_or_default();
    candidates_in(resolve::binary_name(h), &extra, places)
}

/// [`candidates`] for any program name (the ACP adapters, `npx`; decision 0035).
#[cfg(not(windows))]
pub fn candidates_named(name: &str, places: &Places<'_>) -> Vec<PathBuf> {
    candidates_in(name, &[], places)
}

#[cfg(not(windows))]
fn candidates_in(name: &str, extra: &[PathBuf], places: &Places<'_>) -> Vec<PathBuf> {
    let (path_var, home) = (places.path_var, places.home);
    let mut out: Vec<PathBuf> = path_var
        .map(|v| {
            std::env::split_paths(v)
                .filter(|d| d.is_absolute())
                .map(|d| d.join(name))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = home {
        let mut nvm: Vec<PathBuf> = std::fs::read_dir(home.join(".nvm/versions/node"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path().join("bin").join(name))
                    .collect()
            })
            .unwrap_or_default();
        let ver = |p: &Path| -> Vec<u64> {
            p.ancestors()
                .nth(2)
                .and_then(|d| d.file_name())
                .map(|n| {
                    n.to_string_lossy()
                        .trim_start_matches('v')
                        .split('.')
                        .map(|x| x.parse().unwrap_or(0))
                        .collect()
                })
                .unwrap_or_default()
        };
        nvm.sort_by_key(|p| std::cmp::Reverse(ver(p)));
        out.extend(nvm);
        for d in [".local/bin", ".bun/bin", ".npm-global/bin", ".volta/bin"] {
            out.push(home.join(d).join(name));
        }
        out.extend(extra.iter().cloned());
    }
    for d in places.system {
        out.push(Path::new(d).join(name));
    }
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Windows (decisions/0019): the same order with Windows' folders, each as `<name>.exe`
/// and `<name>.cmd` in `PATHEXT` order: the given `PATH`, the per-user install folders
/// (`%USERPROFILE%\.local\bin`, `%APPDATA%\npm`, bun, Volta, opencode), nvm-windows'
/// node versions (newest first), then `%ProgramFiles%\nodejs`. A `.cmd` found here is
/// followed to its target by [`resolve`], never run.
#[cfg(windows)]
pub fn candidates(h: Harness, places: &Places<'_>) -> Vec<PathBuf> {
    candidates_named(resolve::binary_name(h), places)
}

/// [`candidates`] for any program name (the ACP adapters, `npx`; decision 0035).
#[cfg(windows)]
pub fn candidates_named(name: &str, places: &Places<'_>) -> Vec<PathBuf> {
    use super::winshim;
    let pathext = std::env::var("PATHEXT").ok();
    let env_dir = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let mut dirs: Vec<PathBuf> = places
        .path_var
        .map(|v| {
            std::env::split_paths(v)
                .filter(|d| d.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = places.home {
        let appdata = env_dir("APPDATA");
        let local = env_dir("LOCALAPPDATA");
        dirs.extend(winshim::user_dirs(
            home,
            appdata.as_deref(),
            local.as_deref(),
        ));
        let nvm_root = env_dir("NVM_HOME")
            .or_else(|| appdata.map(|a| a.join("nvm")))
            .unwrap_or_else(|| home.join("AppData").join("Roaming").join("nvm"));
        let entries = std::fs::read_dir(nvm_root)
            .map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default();
        dirs.extend(winshim::nvm_dirs(entries));
    }
    if let Some(pf) = env_dir("ProgramFiles") {
        dirs.push(pf.join("nodejs"));
    }
    dirs.extend(places.system.iter().map(PathBuf::from));
    let mut out: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| winshim::candidates_in(d, name, pathext.as_deref()))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|p| seen.insert(p.to_string_lossy().to_lowercase()));
    out
}

/// What the agent picker shows for one client.
#[derive(Clone, Debug, Serialize)]
pub struct Detected {
    pub harness: Harness,
    pub label: &'static str,
    pub installed: bool,
    /// Where it was found (an Owner-set path, or the first candidate that resolved).
    pub found: Option<PathBuf>,
    pub exec: Option<PathBuf>,
    pub interpreter: Option<PathBuf>,
    pub via: Option<&'static str>,
    pub set_by_owner: bool,
    pub version: Option<String>,
    pub sha256: Option<String>,
    /// Inside the S7-tested range.
    pub tested: bool,
    pub verdict: Option<Verdict>,
    pub info: clients::ClientInfo,
    /// Why it is not installed or cannot be used (resolution, interpreter, …).
    pub problem: Option<String>,
    /// Candidates that exist but did not resolve, with why (an env-shebang wrapper, a pi
    /// with no node beside it, …): shown so the user knows what was skipped.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Detects `h`: the Owner path when set, else the first candidate that resolves.
pub fn detect(
    h: Harness,
    owner_path: Option<&Path>,
    places: &Places<'_>,
    ctx: &ProcessContext,
) -> Detected {
    let mut d = Detected {
        harness: h,
        label: h.label(),
        installed: false,
        found: None,
        exec: None,
        interpreter: None,
        via: None,
        set_by_owner: owner_path.is_some(),
        version: None,
        sha256: None,
        tested: false,
        verdict: None,
        info: clients::info(h),
        problem: None,
        skipped: Vec::new(),
    };
    let resolved = match owner_path {
        Some(p) => resolve::resolve(h, Some(p), None).map_err(|e| e.to_string()),
        None => {
            let mut hit = None;
            for c in candidates(h, places) {
                if std::fs::metadata(&c).is_err() {
                    continue;
                }
                match resolve::resolve(h, Some(&c), None) {
                    Ok(r) if !r.via.starts_with("script") => {
                        hit = Some(r);
                        break;
                    }
                    Ok(r) => d
                        .skipped
                        .push((c, ResolveError::Script(r.exec).to_string())),
                    Err(e) => d.skipped.push((c, e.to_string())),
                }
            }
            hit.ok_or_else(|| {
                format!(
                    "{} was not found; set its path (the absolute path of the `{}` program)",
                    h.label(),
                    resolve::binary_name(h)
                )
            })
        }
    };
    let r = match resolved {
        Ok(r) => r,
        Err(why) => {
            d.problem = Some(why);
            return d;
        }
    };
    let out = super::ask_version(&r.exec, r.interpreter.as_deref(), ctx);
    let ver = version::parse(&out);
    let verdict = version::check(h, ver);
    d.installed = true;
    d.tested = verdict == Verdict::Supported;
    d.version = Some(ver.map_or_else(|| out.chars().take(40).collect(), |v| v.to_string()));
    d.sha256 = super::sha256::file_hex(&r.exec).ok();
    d.verdict = Some(verdict);
    d.found = Some(r.found);
    d.exec = Some(r.exec);
    d.interpreter = r.interpreter;
    d.via = Some(r.via);
    d
}

/// Every client.
pub fn detect_all(
    owner_paths: &dyn Fn(Harness) -> Option<PathBuf>,
    places: &Places<'_>,
    ctx: &ProcessContext,
) -> Vec<Detected> {
    Harness::ALL
        .into_iter()
        .map(|h| detect(h, owner_paths(h).as_deref(), places, ctx))
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn exe(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-detect-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    fn ctx() -> ProcessContext {
        ProcessContext {
            vars: vec![("HOME".into(), "/nonexistent".into())],
        }
    }

    #[test]
    fn candidates_cover_nvm_newest_first_and_the_install_folders() {
        let home = tmp("cands");
        for v in ["v20.1.0", "v24.1.0", "v22.5.1"] {
            std::fs::create_dir_all(home.join(".nvm/versions/node").join(v).join("bin")).unwrap();
        }
        let c = candidates(
            Harness::Pi,
            &Places {
                path_var: Some(OsStr::new("/a:rel:/b")),
                home: Some(&home),
                system: SYSTEM_DIRS,
            },
        );
        let s: Vec<String> = c.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(s[0], "/a/pi");
        assert_eq!(s[1], "/b/pi");
        assert!(
            s[2].ends_with("v24.1.0/bin/pi") && s[4].ends_with("v20.1.0/bin/pi"),
            "{s:?}"
        );
        assert!(s.iter().any(|p| p.ends_with("/.bun/bin/pi")));
        assert!(s.iter().any(|p| p == "/opt/homebrew/bin/pi"));
        let oc = candidates(
            Harness::OpenCode,
            &Places {
                path_var: None,
                home: Some(&home),
                system: &[],
            },
        );
        assert!(oc.iter().any(|p| p.ends_with(".opencode/bin/opencode")));
        assert!(!c.iter().any(|p| p.ends_with("rel/pi")));
    }

    /// A pi wrapper with no node beside it is skipped (and said so) for the next
    /// candidate that has one; versions come from the client itself, never a shell.
    #[test]
    fn the_first_candidate_that_resolves_wins_and_skips_are_reported() {
        let d = tmp("pick");
        exe(&d.join("bun/pi"), "#!/usr/bin/env node\n");
        exe(
            &d.join("brew/pi"),
            "#!/usr/bin/env node\nconsole.log('x')\n",
        );
        exe(&d.join("brew/node"), "#!/bin/sh\n");
        // the `node` beside must be native: a script is not an interpreter
        let path = std::env::join_paths([d.join("bun"), d.join("brew")]).unwrap();
        let only = |p| Places {
            path_var: Some(p),
            home: None,
            system: &[],
        };
        let r = detect(Harness::Pi, None, &only(&path), &ctx());
        assert!(!r.installed);
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        // opencode: a native-looking fake answers --version
        exe(&d.join("oc/opencode"), "#!/bin/sh\necho 1.18.0\n");
        let path = std::env::join_paths([d.join("oc")]).unwrap();
        let r = detect(Harness::OpenCode, None, &only(&path), &ctx());
        // a #!/bin/sh fake resolves as a script: skipped on the gated path, said why
        assert!(
            !r.installed && r.skipped[0].1.contains("script"),
            "{:?}",
            r.skipped
        );
        // the Owner's path is used as given (no search)
        let r = detect(
            Harness::OpenCode,
            Some(&d.join("oc/opencode")),
            &only(OsStr::new("")),
            &ctx(),
        );
        assert!(r.installed && r.set_by_owner);
        assert_eq!(r.version.as_deref(), Some("1.18.0"));
        assert!(!r.tested, "no opencode version is S7-tested yet");
        assert!(r.sha256.as_deref().is_some_and(|h| h.len() == 64));
    }
}
