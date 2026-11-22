//! The run's directories (R-AGT-1 "cwd").
//!
//! ```text
//! <runs root>/<run>/          0700, created fresh (refused if it exists)
//!   cwd/                      0700, empty: the child's working directory
//!   config/                   0700: the per-run MCP config and settings, 0600 each
//!   transcript.jsonl          the client's stdout (stream-json / --json)
//!   stderr.log
//!   run.json                  the run record
//! ```
//!
//! The runs root defaults to `$TMPDIR/gmclip-<uid>/agent-runs`, not SPEC's
//! `<app data>/agent-runs`: app data lives under `$HOME`, and `$HOME` holds `.claude/`
//! and `.codex/` on every machine that has the clients, so R-AGT-1's "no `.claude/` or
//! `.codex/` in any ancestor up to `/`" can never hold there (`spikes/S7/README.md`,
//! finding S7-6). macOS's per-user `$TMPDIR` (`/var/folders/<xx>/<id>/T/`) is the same
//! root R-ARCH-5a uses for the socket.
//!
//! The ancestor check is real: if any ancestor of `cwd` (canonical path, up to `/`)
//! contains an instruction or config file a client discovers by walking up, the launch is
//! refused.
//!
//! Windows (decisions/0019): the root is `%SystemDrive%\gmclip-<user SID>\agent-runs`.
//! Not `%TEMP%`: that is `%USERPROFILE%\AppData\Local\Temp`, and the profile holds
//! `.claude\` and `.codex\` exactly like `$HOME` (S7-6), so the ancestry check would
//! refuse every run. At the drive root the only ancestor is the root itself; a folder of
//! that name made by another account fails the owner check below (closed, never shared).
//! "0700, ours" is an owner-only DACL, set when each directory is created
//! (never chmod-ed after), and an existing component must be owned by this user's SID
//! with a DACL that lets nobody else in. "Up to `/`" is up to the drive or share root of
//! the canonical (`\\?\`) path, and names are matched without case, as NTFS does.

use std::io;
use std::path::{Path, PathBuf};

/// Files and directories a client finds by walking up from its cwd.
pub const INSTRUCTION_NAMES: &[&str] = &[
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".claude",
    ".mcp.json",
    "AGENTS.md",
    "AGENTS.override.md",
    ".codex",
    ".agents",
    // pi (project `.pi/` settings, extensions, SYSTEM.md) and opencode (`.opencode/`,
    // `opencode.json[c]`, CONTEXT.md) walk up from the cwd too
    ".pi",
    ".opencode",
    "opencode.json",
    "opencode.jsonc",
    "CONTEXT.md",
];

/// The directories of one run.
#[derive(Clone, Debug)]
pub struct RunDirs {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub config: PathBuf,
}

impl RunDirs {
    pub fn transcript(&self) -> PathBuf {
        self.root.join("transcript.jsonl")
    }
    pub fn stderr(&self) -> PathBuf {
        self.root.join("stderr.log")
    }
    pub fn record(&self) -> PathBuf {
        self.root.join("run.json")
    }
}

#[derive(Debug)]
pub enum RunDirError {
    Io(io::Error),
    /// The runs root is a symlink, not ours, or not 0700.
    UnsafeRoot(PathBuf, &'static str),
    /// An ancestor holds a file a client would discover.
    InstructionFile(PathBuf),
    Exists(PathBuf),
}

impl std::fmt::Display for RunDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunDirError::Io(e) => write!(f, "run directory: {e}"),
            RunDirError::UnsafeRoot(p, why) => write!(f, "runs root {} {why}", p.display()),
            RunDirError::InstructionFile(p) => write!(
                f,
                "{} would be read by the client (an ancestor of the run's cwd)",
                p.display()
            ),
            RunDirError::Exists(p) => write!(f, "run directory {} already exists", p.display()),
        }
    }
}

impl From<io::Error> for RunDirError {
    fn from(e: io::Error) -> Self {
        RunDirError::Io(e)
    }
}

/// The default runs root: `$TMPDIR/gmclip-<uid>/agent-runs` (Windows:
/// `%SystemDrive%\gmclip-<SID>\agent-runs`).
pub fn default_root() -> io::Result<PathBuf> {
    Ok(default_location(&std::env::temp_dir())?.0)
}

/// The default runs root for a per-user temp dir, and the trusted directory above the
/// part gmclip creates and checks (the temp dir; Windows: the drive root).
pub fn default_location(tmp: &Path) -> io::Result<(PathBuf, PathBuf)> {
    #[cfg(windows)]
    {
        let _ = tmp;
        let drive = std::env::var_os("SystemDrive")
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "C:".into());
        let mut root = PathBuf::from(drive);
        root.push(std::path::MAIN_SEPARATOR_STR);
        let sid = gmclip_platform::windows::current_user_sid()?;
        Ok((root.join(format!("gmclip-{sid}")).join("agent-runs"), root))
    }
    #[cfg(not(windows))]
    {
        Ok((default_root_in(tmp)?, tmp.to_owned()))
    }
}

/// `<tmp>/gmclip-<uid>/agent-runs` for a given per-user temp dir.
#[cfg(not(windows))]
pub fn default_root_in(tmp: &Path) -> io::Result<PathBuf> {
    let uid = current_uid(tmp)?;
    Ok(tmp.join(format!("gmclip-{uid}")).join("agent-runs"))
}

/// A run id is one path component: `^[A-Za-z0-9_-]{1,64}$` (it comes from the holder).
pub fn valid_run_id(run: &str) -> bool {
    !run.is_empty()
        && run.len() <= 64
        && run
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(unix)]
fn current_uid(tmp: &Path) -> io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    // The owner of a file we just created is our effective uid (no libc, no unsafe).
    let probe = tmp.join(format!(
        ".gmclip-uid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let probe = PathBuf::from(probe.to_string_lossy().replace(['(', ')', ' '], ""));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let uid = f.metadata()?.uid();
    drop(f);
    let _ = std::fs::remove_file(&probe);
    Ok(uid)
}

#[cfg(not(any(unix, windows)))]
fn current_uid(_tmp: &Path) -> io::Result<u32> {
    Ok(0)
}

#[cfg(unix)]
fn mkdir_0700(p: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(p)
}

/// The Windows 0700: created with an owner-only DACL (and this user as owner).
#[cfg(windows)]
fn mkdir_0700(p: &Path) -> io::Result<()> {
    gmclip_platform::windows::create_dir_owner_only(p)
}

#[cfg(not(any(unix, windows)))]
fn mkdir_0700(p: &Path) -> io::Result<()> {
    std::fs::create_dir(p)
}

/// Is `a` the same directory as `b`? Windows: verbatim prefix and case do not matter.
fn same_dir(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        gmclip_platform::winpath::same(&a.to_string_lossy(), &b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Creates `p` and its missing parents with mode 0700; an existing component must be a
/// real directory (not a symlink) owned by us. Only the components below `stop` are
/// checked for ownership (the system temp dir above it is not ours).
fn ensure_private_dir(p: &Path, stop: &Path) -> Result<(), RunDirError> {
    let mut chain = Vec::new();
    let mut cur = p;
    while !same_dir(cur, stop) {
        chain.push(cur.to_owned());
        cur = match cur.parent() {
            Some(c) => c,
            None => break,
        };
    }
    for dir in chain.into_iter().rev() {
        match std::fs::symlink_metadata(&dir) {
            Ok(m) => {
                if m.file_type().is_symlink() || !m.is_dir() {
                    return Err(RunDirError::UnsafeRoot(dir, "is not a plain directory"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    let me = current_uid(stop)?;
                    if m.uid() != me {
                        return Err(RunDirError::UnsafeRoot(dir, "is not owned by this user"));
                    }
                    if m.permissions().mode() & 0o077 != 0 {
                        return Err(RunDirError::UnsafeRoot(
                            dir,
                            "is readable by other users (want 0700)",
                        ));
                    }
                }
                #[cfg(windows)]
                {
                    use gmclip_platform::windows as win;
                    let me = win::current_user_sid()?;
                    let sec = win::file_security(&dir, false)?;
                    if sec.owner_sid != me {
                        return Err(RunDirError::UnsafeRoot(dir, "is not owned by this user"));
                    }
                    if !sec.dacl_private_to(&me) {
                        return Err(RunDirError::UnsafeRoot(
                            dir,
                            "lets other accounts in (want an owner-only ACL)",
                        ));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => mkdir_0700(&dir)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Returns the first instruction file found in `dir` or any ancestor.
pub fn instruction_file_in_ancestry(dir: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    for anc in canonical.ancestors() {
        for name in INSTRUCTION_NAMES {
            let p = anc.join(name);
            if std::fs::symlink_metadata(&p).is_ok() {
                return Some(p);
            }
        }
        // Windows: also by listing, without case (a folder can be case-sensitive, and
        // the clients match these names as Windows does)
        #[cfg(windows)]
        if let Some(p) = listed_instruction_file(anc) {
            return Some(p);
        }
    }
    None
}

/// An entry of `dir` whose name is one of [`INSTRUCTION_NAMES`] in any case.
#[cfg(windows)]
fn listed_instruction_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let n = e.file_name();
        let n = n.to_string_lossy();
        INSTRUCTION_NAMES
            .iter()
            .any(|w| w.eq_ignore_ascii_case(&n))
            .then(|| e.path())
    })
}

/// Creates the run's directories under `runs_root` (whose missing components are created
/// 0700 below `trusted_parent`) and checks the cwd's ancestry.
pub fn create(runs_root: &Path, trusted_parent: &Path, run: &str) -> Result<RunDirs, RunDirError> {
    if !valid_run_id(run) {
        return Err(RunDirError::Io(io::Error::other(
            "the run id is not a plain path component",
        )));
    }
    ensure_private_dir(runs_root, trusted_parent)?;
    let root = runs_root.join(run);
    if std::fs::symlink_metadata(&root).is_ok() {
        return Err(RunDirError::Exists(root));
    }
    mkdir_0700(&root)?;
    let cwd = root.join("cwd");
    let config = root.join("config");
    mkdir_0700(&cwd)?;
    mkdir_0700(&config)?;
    let root = std::fs::canonicalize(&root)?;
    let dirs = RunDirs {
        cwd: root.join("cwd"),
        config: root.join("config"),
        root,
    };
    if let Some(found) = instruction_file_in_ancestry(&dirs.cwd) {
        return Err(RunDirError::InstructionFile(found));
    }
    Ok(dirs)
}

/// Creates a private (0700) directory inside a run; an existing plain directory is kept,
/// anything else (a symlink, a file) is refused.
pub fn mkdir_private(p: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::other("not a plain directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => mkdir_0700(p),
        Err(e) => Err(e),
    }
}

/// Writes a private (0600) file, refusing to follow or replace an existing one.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    #[cfg(windows)]
    let mut f = gmclip_platform::windows::create_file_owner_only(path)?;
    #[cfg(not(windows))]
    let mut f = {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(path)?
    };
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// Under the drive root, as real runs are: `%TEMP%` is inside the profile, whose
    /// `.claude\` would fail every ancestry check on a machine with the clients.
    fn tmp(tag: &str) -> PathBuf {
        let (_, drive) = default_location(&std::env::temp_dir()).unwrap();
        let d = drive.join(format!("gmclip-rundir-w-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    #[test]
    fn creates_owner_only_dirs_and_refuses_shared_ones() {
        let base = tmp("ok");
        let root = base.join("u").join("agent-runs");
        // the stop dir given in another case and without the verbatim prefix still stops
        let stop = PathBuf::from(
            gmclip_platform::winpath::strip_verbatim(&base.to_string_lossy()).to_uppercase(),
        );
        let dirs = create(&root, &stop, "run1").unwrap();
        let me = gmclip_platform::windows::current_user_sid().unwrap();
        for d in [&dirs.root, &dirs.cwd, &dirs.config, &root] {
            let sec = gmclip_platform::windows::file_security(d, false).unwrap();
            assert!(sec.is_private_to(&me), "{}: {sec:?}", d.display());
        }
        write_private(&dirs.config.join("a.json"), b"{}").unwrap();
        assert!(write_private(&dirs.config.join("a.json"), b"{}").is_err());
        // a component made by plain std inherits %TEMP%'s ACL: refused
        let shared = tmp("shared");
        std::fs::create_dir_all(shared.join("agent-runs")).unwrap();
        assert!(matches!(
            create(&shared.join("agent-runs"), &shared, "r"),
            Err(RunDirError::UnsafeRoot(..))
        ));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&shared);
    }

    /// The default root is at the drive root, named for this user, outside the profile.
    #[test]
    fn default_root_is_outside_the_profile() {
        let (root, trusted) = default_location(&std::env::temp_dir()).unwrap();
        assert!(gmclip_platform::winpath::is_root(
            &trusted.to_string_lossy()
        ));
        let sid = gmclip_platform::windows::current_user_sid().unwrap();
        assert_eq!(
            root,
            trusted.join(format!("gmclip-{sid}")).join("agent-runs")
        );
        let profile = std::env::var("USERPROFILE").unwrap();
        assert!(
            !gmclip_platform::winpath::starts_with(&root.to_string_lossy(), &profile),
            "{}",
            root.display()
        );
    }

    #[test]
    fn ancestry_is_checked_to_the_drive_root_without_case() {
        let base = tmp("anc");
        std::fs::create_dir_all(base.join("a").join("b")).unwrap();
        assert!(instruction_file_in_ancestry(&base.join("a").join("b")).is_none());
        std::fs::write(base.join("a").join("claude.MD"), "x").unwrap();
        let found = instruction_file_in_ancestry(&base.join("a").join("b")).unwrap();
        assert!(
            found
                .to_string_lossy()
                .to_lowercase()
                .ends_with("claude.md"),
            "{}",
            found.display()
        );
        // the walk reaches the drive root of the verbatim path
        let c = std::fs::canonicalize(&base).unwrap();
        let last = c.ancestors().last().unwrap().to_string_lossy().into_owned();
        assert!(gmclip_platform::winpath::is_root(&last), "{last}");
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-rundir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    #[test]
    fn creates_private_empty_dirs_and_refuses_reuse() {
        let base = tmp("ok");
        let root = base.join("u/agent-runs");
        let dirs = create(&root, &base, "run1").unwrap();
        for d in [&dirs.root, &dirs.cwd, &dirs.config, &root] {
            let mode = std::fs::metadata(d).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", d.display());
        }
        assert_eq!(std::fs::read_dir(&dirs.cwd).unwrap().count(), 0);
        assert!(matches!(
            create(&root, &base, "run1"),
            Err(RunDirError::Exists(_))
        ));
        write_private(&dirs.config.join("a.json"), b"{}").unwrap();
        let mode = std::fs::metadata(dirs.config.join("a.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            write_private(&dirs.config.join("a.json"), b"{}").is_err(),
            "never replaced"
        );
    }

    #[test]
    fn an_instruction_file_in_any_ancestor_refuses_the_run() {
        for name in INSTRUCTION_NAMES {
            let base = tmp(&format!("anc{}", name.replace('.', "_")));
            let p = base.join(name);
            if name.ends_with(".md") || name.ends_with(".json") {
                std::fs::write(&p, "canary").unwrap();
            } else {
                std::fs::create_dir(&p).unwrap();
            }
            let r = create(&base.join("deep/agent-runs"), &base, "r");
            assert!(
                matches!(r, Err(RunDirError::InstructionFile(ref f)) if f == &p),
                "{name}: {r:?}"
            );
        }
    }

    #[test]
    fn a_symlinked_or_shared_root_is_refused() {
        let base = tmp("sym");
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        assert!(matches!(
            create(&base.join("link/agent-runs"), &base, "r"),
            Err(RunDirError::UnsafeRoot(_, _))
        ));
        std::fs::create_dir(base.join("open")).unwrap();
        std::fs::set_permissions(base.join("open"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(matches!(
            create(&base.join("open/agent-runs"), &base, "r"),
            Err(RunDirError::UnsafeRoot(_, _))
        ));
    }

    /// The default root on this machine has a clean ancestry (on AJ's Mac `$HOME` has
    /// `.claude/` and `.codex/`, which is why the root is not under app data).
    #[test]
    fn default_root_is_outside_home() {
        let root = default_root().unwrap();
        if let Some(home) = std::env::var_os("HOME") {
            assert!(!root.starts_with(&home), "{}", root.display());
        }
    }
}
