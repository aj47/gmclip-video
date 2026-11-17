//! Client binaries by absolute path (R-AGT-1, SPEC §10.7 "Binaries").
//!
//! The launcher finds the client itself: an Owner-configured absolute path, else a search
//! of its own `PATH` entries (absolute entries only). It never asks a shell, so an alias
//! (AJ's `claude` adds `--dangerously-skip-permissions`) or a function (his `codex` adds
//! `--yolo --search`) cannot apply. The path is canonicalised (symlinks resolved) and that
//! file is what runs, so a symlink swapped after the hash cannot redirect the spawn
//! without the recorded path changing too.
//!
//! Codex is installed through npm as a `#!/usr/bin/env node` wrapper (`bin/codex.js`)
//! that spawns a vendored native binary. Running the wrapper would need `node` on the
//! child's `PATH` and an interpreter in the chain, so the launcher resolves the wrapper to
//! that native binary itself, following the wrapper's own lookup
//! (`node_modules/@openai/codex-<platform>/vendor/<triple>/bin/codex`).
//!
//! pi ships only as a `#!/usr/bin/env node` script (`dist/bundle/cli.js`). The launcher
//! never lets `env` pick the interpreter from a `PATH`: it runs the script with the
//! `node` that sits beside the path the script was found at (`/opt/homebrew/bin/node`
//! for `/opt/homebrew/bin/pi`, `~/.nvm/versions/node/<v>/bin/node` for nvm), by absolute
//! path, and records both. A pi with no `node` beside it is refused with the reason.
//! The script is the client's identity (hashed, version-gated); its bundle imports
//! content-hashed chunk names, so a changed bundle changes the script's hash too.
//!
//! Windows (decisions/0019, [`super::winshim`]): `PATH` is searched for `<name>.exe` and
//! `<name>.cmd` in `PATHEXT` order. A `.cmd` (npm's shim) is never run: its target is
//! read out of it, and the same rules then apply to that target as on Unix. pi's `node`
//! is `node.exe` beside the shim, else `%ProgramFiles%\nodejs\node.exe` (the official
//! installer's; npm's global shims live apart from it in `%APPDATA%\npm`). A script with
//! an absolute `#!` interpreter cannot run on Windows and is refused as a script.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::Harness;

/// What resolution found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The path as found (Owner setting or `PATH` entry joined with the name).
    pub found: PathBuf,
    /// The canonical file that is executed.
    pub exec: PathBuf,
    /// How `exec` was reached from `found`.
    pub via: &'static str,
    /// The interpreter `exec` runs under (pi: `node`), absolute and canonical; `None` for
    /// a native binary.
    pub interpreter: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// An Owner-configured path that is not absolute.
    NotAbsolute(PathBuf),
    NotFound {
        name: String,
    },
    /// The file exists but is not a regular executable file.
    NotExecutable(PathBuf),
    /// A Codex npm wrapper whose native binary is missing.
    WrapperWithoutNative(PathBuf),
    /// A script whose interpreter is looked up on `PATH` (`#!/usr/bin/env …`).
    EnvShebang(PathBuf),
    /// A script client where only native binaries are accepted (the version-gated path).
    Script(PathBuf),
    /// A node-script client (pi) with no `node` program beside the path it was found at.
    NoInterpreter(PathBuf),
    /// Windows: a batch file that is not an npm shim gmclip can read its target from
    /// (a `.cmd` is never run, CVE-2024-24576).
    Batch(PathBuf),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotAbsolute(p) => {
                write!(f, "client path {} is not absolute", p.display())
            }
            ResolveError::NotFound { name } => write!(
                f,
                "`{name}` was not found on the launcher's PATH; set the client's absolute path"
            ),
            ResolveError::NotExecutable(p) => {
                write!(f, "{} is not an executable file", p.display())
            }
            ResolveError::Script(p) => write!(
                f,
                "{} is a script; the launcher runs native client binaries only",
                p.display()
            ),
            ResolveError::EnvShebang(p) => write!(
                f,
                "{} runs through `#!/usr/bin/env`, which picks its interpreter from PATH",
                p.display()
            ),
            ResolveError::NoInterpreter(p) => write!(
                f,
                "{} is a node script and there is no `node` beside it; set the client's path to an install with node next to it",
                p.display()
            ),
            ResolveError::WrapperWithoutNative(p) => write!(
                f,
                "{} is the Codex npm wrapper but its native binary is missing",
                p.display()
            ),
            ResolveError::Batch(p) => write!(
                f,
                "{} is a batch file gmclip will not run, and not an npm shim it can follow; set the client's path to its .exe or .js",
                p.display()
            ),
        }
    }
}

/// The executable name searched for on `PATH`.
pub fn binary_name(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => "claude",
        Harness::Codex => "codex",
        Harness::Pi => "pi",
        Harness::OpenCode => "opencode",
    }
}

/// Resolves `h` from an Owner override or the given `PATH` value (the launcher's own).
pub fn resolve(
    h: Harness,
    owner_path: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Result<Resolved, ResolveError> {
    let found = match owner_path {
        Some(p) if !p.is_absolute() => return Err(ResolveError::NotAbsolute(p.to_owned())),
        Some(p) => p.to_owned(),
        None => search_path(binary_name(h), path_var).ok_or_else(|| ResolveError::NotFound {
            name: binary_name(h).to_owned(),
        })?,
    };
    // Windows: an npm `.cmd` shim stands for the file it names (never run itself)
    #[cfg(windows)]
    let target = if super::winshim::is_batch(&found) {
        shim_target(&found)?
    } else {
        found.clone()
    };
    #[cfg(not(windows))]
    let target = found.clone();
    let canonical = std::fs::canonicalize(&target).map_err(|_| ResolveError::NotFound {
        name: found.display().to_string(),
    })?;
    if !is_executable_file(&canonical) {
        return Err(ResolveError::NotExecutable(canonical));
    }
    if h == Harness::Codex && is_codex_npm_wrapper(&canonical) {
        let native = codex_native_for_wrapper(&canonical)
            .ok_or_else(|| ResolveError::WrapperWithoutNative(canonical.clone()))?;
        let native = std::fs::canonicalize(&native)
            .map_err(|_| ResolveError::WrapperWithoutNative(canonical.clone()))?;
        if !is_executable_file(&native) {
            return Err(ResolveError::NotExecutable(native));
        }
        return Ok(Resolved {
            found,
            exec: native,
            via: "npm wrapper → vendored native binary",
            interpreter: None,
        });
    }
    if h == Harness::Pi && is_node_script(&canonical) {
        let node = node_beside(&found).ok_or_else(|| ResolveError::NoInterpreter(found.clone()))?;
        return Ok(Resolved {
            found,
            exec: canonical,
            via: NODE_SCRIPT_VIA,
            interpreter: Some(node),
        });
    }
    if starts_with_shebang(&canonical) {
        // An interpreter found on PATH is a lookup the launcher did not make: refused.
        // A script with an absolute interpreter (the test doubles) runs, recorded as such.
        if shebang_is_env(&head(&canonical, 256)) {
            return Err(ResolveError::EnvShebang(canonical));
        }
        // Windows has no `#!`: the script cannot be started as a program
        if cfg!(windows) {
            return Err(ResolveError::Script(canonical));
        }
        return Ok(Resolved {
            found,
            exec: canonical,
            via: "script (interpreter from its #! line)",
            interpreter: None,
        });
    }
    Ok(Resolved {
        found,
        exec: canonical,
        via: "direct",
        interpreter: None,
    })
}

/// How a node-script client (pi) is recorded: run by the launcher's own absolute `node`.
pub const NODE_SCRIPT_VIA: &str = "node script, run by the node beside it";

/// `#!/usr/bin/env node` (optionally `env -S node …`): a script meant for node.
fn is_node_script(p: &Path) -> bool {
    let h = head(p, 128);
    let line = h.split(|b| *b == b'\n').next().unwrap_or(&[]);
    shebang_is_env(&h)
        && line
            .split(|b| *b == b' ' || *b == b'\t')
            .any(|t| t == b"node")
}

/// The `node` next to the path a script was found at (never a `PATH` lookup): the
/// directory npm, Homebrew and nvm link both into. Canonical, executable, not a script.
/// Windows: then the official installer's `%ProgramFiles%\nodejs\node.exe`, a fixed
/// admin-owned path (npm's default global folder holds the shims, not node).
fn node_beside(found: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    let ok = |p: PathBuf| {
        let node = std::fs::canonicalize(p).ok()?;
        (is_executable_file(&node) && !starts_with_shebang(&node)).then_some(node)
    };
    let beside = ok(found.parent()?.join(exe));
    #[cfg(windows)]
    let beside = beside.or_else(|| {
        let pf = std::env::var_os("ProgramFiles").filter(|v| !v.is_empty())?;
        ok(PathBuf::from(pf).join("nodejs").join(exe))
    });
    beside
}

#[cfg(not(windows))]
fn search_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        // relative PATH entries ("", ".") would make the result depend on the cwd
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|p| is_executable_file(p))
}

/// `<name>.exe` / `<name>.cmd` in `PATHEXT` order, absolute entries only.
#[cfg(windows)]
fn search_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    let pathext = std::env::var("PATHEXT").ok();
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| super::winshim::candidates_in(&dir, name, pathext.as_deref()))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
}

/// The file an npm `.cmd` shim runs: inside the shim's own folder, existing, and not
/// itself a batch file.
#[cfg(windows)]
fn shim_target(shim: &Path) -> Result<PathBuf, ResolveError> {
    let refuse = || ResolveError::Batch(shim.to_owned());
    let bytes = head(shim, super::winshim::SHIM_MAX + 1);
    if bytes.len() > super::winshim::SHIM_MAX {
        return Err(refuse());
    }
    let text = String::from_utf8_lossy(&bytes);
    let rel = super::winshim::cmd_shim_target(&text).ok_or_else(refuse)?;
    let dir = std::fs::canonicalize(shim.parent().ok_or_else(refuse)?).map_err(|_| refuse())?;
    let target = std::fs::canonicalize(dir.join(&rel)).map_err(|_| refuse())?;
    if !target.starts_with(&dir) || super::winshim::is_batch(&target) {
        return Err(refuse());
    }
    Ok(target)
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    // Windows: a native `.exe`, or a script the rules above run under an interpreter
    // (never a batch file)
    #[cfg(windows)]
    {
        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) || starts_with_shebang(p)
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

fn head(p: &Path, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let Ok(mut f) = std::fs::File::open(p) else {
        return Vec::new();
    };
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    buf.truncate(got);
    buf
}

/// `#!` then optional blanks, an interpreter path whose last component is `env`.
fn shebang_is_env(h: &[u8]) -> bool {
    let line = h.split(|b| *b == b'\n').next().unwrap_or(&[]);
    let rest = line.strip_prefix(b"#!").unwrap_or(&[]);
    let interp = rest
        .split(|b| *b == b' ' || *b == b'\t')
        .find(|t| !t.is_empty())
        .unwrap_or(&[]);
    interp.rsplit(|b| *b == b'/').next() == Some(b"env".as_slice())
}

fn starts_with_shebang(p: &Path) -> bool {
    head(p, 2) == b"#!"
}

fn is_codex_npm_wrapper(p: &Path) -> bool {
    p.file_name() == Some(OsStr::new("codex.js"))
        && head(p, 64).starts_with(b"#!/usr/bin/env node")
        && p.parent().and_then(Path::file_name) == Some(OsStr::new("bin"))
}

/// `<pkg>/bin/codex.js` → `<pkg>/node_modules/@openai/codex-<plat>/vendor/<triple>/bin/codex`
/// (the wrapper's own `findCodexExecutable`), else `<pkg>/vendor/<triple>/bin/codex`.
fn codex_native_for_wrapper(wrapper: &Path) -> Option<PathBuf> {
    let (plat, triple) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("darwin-arm64", "aarch64-apple-darwin"),
        ("macos", "x86_64") => ("darwin-x64", "x86_64-apple-darwin"),
        ("linux", "aarch64") => ("linux-arm64", "aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => ("linux-x64", "x86_64-unknown-linux-musl"),
        ("windows", "x86_64") => ("win32-x64", "x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => ("win32-arm64", "aarch64-pc-windows-msvc"),
        _ => return None,
    };
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    let pkg = wrapper.parent()?.parent()?;
    let candidates = [
        pkg.join("node_modules/@openai")
            .join(format!("codex-{plat}"))
            .join("vendor")
            .join(triple)
            .join("bin")
            .join(exe),
        // a hoisted install: the platform package beside @openai/codex
        pkg.parent()?
            .join(format!("codex-{plat}"))
            .join("vendor")
            .join(triple)
            .join("bin")
            .join(exe),
        pkg.join("vendor").join(triple).join("bin").join(exe),
    ];
    candidates.into_iter().find(|c| c.is_file())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-resolve-w-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    fn put(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    const SHIM: &str = "@ECHO off\r\nSETLOCAL\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n)\r\nendLocal & \"%_prog%\"  \"%dp0%\\TARGET\" %*\r\n";

    /// npm's global folder: `pi.cmd` beside `node.exe` (nvm-windows' layout) runs as
    /// `node.exe cli.js`; the `.cmd` itself is never the program.
    #[test]
    fn an_npm_shim_is_followed_never_run() {
        let d = tmp("shim");
        let rel = r"node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.js";
        put(&d.join(rel), "#!/usr/bin/env node\n");
        put(&d.join("pi.cmd"), &SHIM.replace("TARGET", rel));
        put(&d.join("node.exe"), "MZ");
        let r = resolve(Harness::Pi, Some(&d.join("pi.cmd")), None).unwrap();
        assert_eq!(r.found, d.join("pi.cmd"));
        assert_eq!(r.exec, std::fs::canonicalize(d.join(rel)).unwrap());
        assert_eq!(
            r.interpreter,
            Some(std::fs::canonicalize(d.join("node.exe")).unwrap())
        );
        assert!(!winshim_is_batch(&r.exec));
        // a Claude Code shim to a node script is refused exactly as on Unix
        put(&d.join("claude.cmd"), &SHIM.replace("TARGET", rel));
        assert!(matches!(
            resolve(Harness::ClaudeCode, Some(&d.join("claude.cmd")), None),
            Err(ResolveError::EnvShebang(_))
        ));
        // a native target runs directly
        put(&d.join(r"node_modules\oc\bin\opencode.exe"), "MZ");
        put(
            &d.join("opencode.cmd"),
            &SHIM.replace("TARGET", r"node_modules\oc\bin\opencode.exe"),
        );
        let r = resolve(Harness::OpenCode, Some(&d.join("opencode.cmd")), None).unwrap();
        assert_eq!(r.via, "direct");
        assert!(r.exec.to_string_lossy().ends_with("opencode.exe"));
    }

    #[test]
    fn a_batch_file_that_is_not_a_shim_or_leaves_its_folder_is_refused() {
        let d = tmp("bat");
        put(
            &d.join("claude.cmd"),
            "@echo off\r\nC:\\tools\\claude.exe %*\r\n",
        );
        assert!(matches!(
            resolve(Harness::ClaudeCode, Some(&d.join("claude.cmd")), None),
            Err(ResolveError::Batch(_))
        ));
        put(&d.join("outside.exe"), "MZ");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        put(
            &d.join(r"bin\codex.cmd"),
            &SHIM.replace("TARGET", r"..\outside.exe"),
        );
        assert!(matches!(
            resolve(Harness::Codex, Some(&d.join(r"bin\codex.cmd")), None),
            Err(ResolveError::Batch(_))
        ));
        // a shim naming another batch file
        put(&d.join("x.bat"), "@echo off\r\n");
        put(&d.join("pi.cmd"), &SHIM.replace("TARGET", "x.bat"));
        assert!(matches!(
            resolve(Harness::Pi, Some(&d.join("pi.cmd")), None),
            Err(ResolveError::Batch(_))
        ));
    }

    #[test]
    fn path_search_takes_exe_then_cmd_and_skips_relative_entries() {
        let d = tmp("path");
        put(&d.join(r"a\claude.cmd"), "@echo off\r\n");
        put(&d.join(r"b\claude.exe"), "MZ");
        put(&d.join(r"b\claude.bat"), "@echo off\r\n");
        let path = std::env::join_paths([PathBuf::from("rel"), d.join("b"), d.join("a")]).unwrap();
        let r = resolve(Harness::ClaudeCode, None, Some(&path)).unwrap();
        assert_eq!(
            r.exec,
            std::fs::canonicalize(d.join(r"b\claude.exe")).unwrap()
        );
    }

    fn winshim_is_batch(p: &Path) -> bool {
        super::super::winshim::is_batch(p)
    }
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
        let d = std::env::temp_dir().join(format!("gmclip-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    #[test]
    fn path_search_skips_relative_entries_and_non_executables() {
        let d = tmp("path");
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::write(d.join("a/claude"), "not executable").unwrap();
        exe(&d.join("b/claude"), "\x7fELF");
        let path = std::env::join_paths([PathBuf::from("."), d.join("a"), d.join("b")]).unwrap();
        let r = resolve(Harness::ClaudeCode, None, Some(&path)).unwrap();
        assert_eq!(r.exec, d.join("b/claude"));
        assert_eq!(r.via, "direct");
        assert!(matches!(
            resolve(Harness::ClaudeCode, None, Some(OsStr::new(""))),
            Err(ResolveError::NotFound { .. })
        ));
    }

    #[test]
    fn owner_path_must_be_absolute_and_symlinks_are_canonicalised() {
        assert!(matches!(
            resolve(Harness::ClaudeCode, Some(Path::new("bin/claude")), None),
            Err(ResolveError::NotAbsolute(_))
        ));
        let d = tmp("link");
        exe(&d.join("real/claude.exe"), "\x7fELF");
        std::os::unix::fs::symlink(d.join("real/claude.exe"), d.join("claude")).unwrap();
        let r = resolve(Harness::ClaudeCode, Some(&d.join("claude")), None).unwrap();
        assert_eq!(r.found, d.join("claude"));
        assert_eq!(r.exec, d.join("real/claude.exe"));
    }

    /// The real npm layout: `bin/codex` → `../lib/node_modules/@openai/codex/bin/codex.js`.
    #[test]
    fn codex_npm_wrapper_resolves_to_the_vendored_native_binary() {
        let d = tmp("codex");
        let pkg = d.join("lib/node_modules/@openai/codex");
        exe(
            &pkg.join("bin/codex.js"),
            "#!/usr/bin/env node\n// wrapper\n",
        );
        let (plat, triple) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => ("darwin-arm64", "aarch64-apple-darwin"),
            ("macos", "x86_64") => ("darwin-x64", "x86_64-apple-darwin"),
            ("linux", "aarch64") => ("linux-arm64", "aarch64-unknown-linux-musl"),
            _ => ("linux-x64", "x86_64-unknown-linux-musl"),
        };
        let native = pkg.join(format!(
            "node_modules/@openai/codex-{plat}/vendor/{triple}/bin/codex"
        ));
        exe(&native, "\x7fELF");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::os::unix::fs::symlink(
            "../lib/node_modules/@openai/codex/bin/codex.js",
            d.join("bin/codex"),
        )
        .unwrap();
        let path = std::env::join_paths([d.join("bin")]).unwrap();
        let r = resolve(Harness::Codex, None, Some(&path)).unwrap();
        assert_eq!(r.exec, native);
        assert_eq!(r.via, "npm wrapper → vendored native binary");
        std::fs::remove_file(&native).unwrap();
        assert!(matches!(
            resolve(Harness::Codex, None, Some(&path)),
            Err(ResolveError::WrapperWithoutNative(_))
        ));
    }

    /// pi is `#!/usr/bin/env node`: run by the `node` beside where it was found, never
    /// one from PATH; no node beside it, no run.
    #[test]
    fn pi_runs_under_the_node_beside_it() {
        let d = tmp("pi");
        let pkg = d.join("lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle");
        exe(
            &pkg.join("cli.js"),
            "#!/usr/bin/env node\nimport './chunks/c-ABC.js';\n",
        );
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::os::unix::fs::symlink(pkg.join("cli.js"), d.join("bin/pi")).unwrap();
        // no node beside it: refused, even with a node on PATH
        let elsewhere = d.join("other");
        exe(&elsewhere.join("node"), "\x7fELF");
        let path = std::env::join_paths([d.join("bin"), elsewhere.clone()]).unwrap();
        assert!(matches!(
            resolve(Harness::Pi, None, Some(&path)),
            Err(ResolveError::NoInterpreter(_))
        ));
        exe(&d.join("real/node"), "\x7fELF");
        std::os::unix::fs::symlink(d.join("real/node"), d.join("bin/node")).unwrap();
        let r = resolve(Harness::Pi, None, Some(&path)).unwrap();
        assert_eq!(r.exec, pkg.join("cli.js"));
        assert_eq!(
            r.interpreter.as_deref(),
            Some(d.join("real/node").as_path())
        );
        assert_eq!(r.via, NODE_SCRIPT_VIA);
        // a script-shaped `node` beside it is not an interpreter
        let d2 = tmp("pi2");
        exe(&d2.join("pi"), "#!/usr/bin/env node\n");
        exe(&d2.join("node"), "#!/bin/sh\n");
        assert!(matches!(
            resolve(Harness::Pi, Some(&d2.join("pi")), None),
            Err(ResolveError::NoInterpreter(_))
        ));
        // only pi gets this: a claude that is a node script is still refused
        assert!(matches!(
            resolve(Harness::ClaudeCode, Some(&pkg.join("cli.js")), None),
            Err(ResolveError::EnvShebang(_))
        ));
    }

    #[test]
    fn env_shebang_scripts_are_refused_absolute_interpreters_recorded() {
        let d = tmp("script");
        exe(&d.join("claude"), "#!/usr/bin/env node\n");
        assert!(matches!(
            resolve(Harness::ClaudeCode, Some(&d.join("claude")), None),
            Err(ResolveError::EnvShebang(_))
        ));
        for (i, line) in [
            "#! /usr/bin/env sh",
            "#!\t/opt/x/env node",
            "#!/usr/bin/env -S node",
        ]
        .iter()
        .enumerate()
        {
            let p = d.join(format!("c{i}"));
            exe(&p, &format!("{line}\n"));
            assert!(
                matches!(
                    resolve(Harness::ClaudeCode, Some(&p), None),
                    Err(ResolveError::EnvShebang(_))
                ),
                "{line}"
            );
        }
        exe(&d.join("claude2"), "#!/bin/sh\nexit 0\n");
        let r = resolve(Harness::ClaudeCode, Some(&d.join("claude2")), None).unwrap();
        assert!(r.via.starts_with("script"));
    }
}
