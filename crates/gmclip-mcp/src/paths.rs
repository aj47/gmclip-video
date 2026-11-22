//! Where things live (R-ARCH-5, R-ARCH-5a, R-STORE-4a).
//!
//! - **Socket**: a pure function of the user, never read from a file. macOS:
//!   `$TMPDIR/gmclip-<uid>/s` (the per-user `/var/folders/…/T/` launchd sets; std falls
//!   back to `confstr(_CS_DARWIN_USER_TEMP_DIR)` when it is unset). Linux:
//!   `$XDG_RUNTIME_DIR/gmclip/s`, else `<app data>/run/s`; never `/tmp`. The parent must
//!   be owned by us and not group/world-writable (a shared directory is refused, never
//!   fallen back to); the socket directory is 0700, ours, and not a symlink, checked on
//!   every bind and every connect. The path must fit `sun_path`; if it does not, gmclip
//!   refuses and names it. A macOS client whose `TMPDIR` has no holder also tries the
//!   per-user temp dir's socket (`getconf DARWIN_USER_TEMP_DIR`), taking it only when the
//!   server there is the gmclip it expects (decision 0048: agent sandboxes set `TMPDIR`).
//! - **App data**: macOS `~/Library/Application Support/gmclip`, Linux
//!   `${XDG_DATA_HOME:-~/.local/share}/gmclip`. Holds the Owner settings, the audit and
//!   connection logs, the pointer file and the live project databases.
//! - **Windows** (decisions/0019): the "socket" is the named pipe
//!   `\\.\pipe\gmclip-<user SID>` (no directory: the pipe carries its own owner-only
//!   DACL), app data is `%LOCALAPPDATA%\gmclip`, and a private directory is one this user
//!   owns whose DACL lets nobody else in (created that way, never chmod-ed after).

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// `sun_path` is 104 bytes on macOS and 108 on Linux, NUL included.
#[cfg(target_os = "macos")]
pub const SUN_PATH_MAX: usize = 103;
#[cfg(not(target_os = "macos"))]
pub const SUN_PATH_MAX: usize = 107;
/// A pipe name is at most 256 characters (`\\.\pipe\` included).
#[cfg(windows)]
const PIPE_NAME_MAX: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    NoHome,
    /// The socket path does not fit `sun_path` (the path is named: it is ours).
    TooLong(PathBuf),
    /// A directory is not ours, is shared, is a symlink, or has the wrong mode.
    Unsafe(PathBuf, &'static str),
    Io(&'static str, String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::NoHome if cfg!(windows) => write!(f, "USERPROFILE is not set"),
            PathError::NoHome => write!(f, "HOME is not set"),
            PathError::TooLong(p) => write!(
                f,
                "the socket path {} is longer than {SUN_PATH_MAX} bytes; gmclip will not fall back to a shared directory",
                p.display()
            ),
            PathError::Unsafe(p, why) => write!(f, "{}: {why}", p.display()),
            PathError::Io(what, e) => write!(f, "{what}: {e}"),
        }
    }
}

impl std::error::Error for PathError {}

/// The current user's uid.
#[cfg(unix)]
pub fn our_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Windows has no uid: the connection log records 0, and identity checks use the user
/// SID ([`our_sid`]).
#[cfg(windows)]
pub fn our_uid() -> u32 {
    0
}

/// The current user's SID (`S-1-5-21-…`).
#[cfg(windows)]
pub fn our_sid() -> Result<String, PathError> {
    gmclip_platform::windows::current_user_sid()
        .map_err(|e| PathError::Io("current user SID", e.to_string()))
}

fn home() -> Result<PathBuf, PathError> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or(PathError::NoHome)
}

/// `<app data>`.
pub fn app_data() -> Result<PathBuf, PathError> {
    if cfg!(windows) {
        // local, not roaming: the live project databases must not sync between machines
        let base = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .map_or_else(|| home().map(|h| h.join("AppData").join("Local")), Ok)?;
        return Ok(base.join("gmclip"));
    }
    if cfg!(target_os = "macos") {
        Ok(home()?.join("Library/Application Support/gmclip"))
    } else {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .map_or_else(|| home().map(|h| h.join(".local/share")), Ok)?;
        Ok(base.join("gmclip"))
    }
}

/// The directory holding the socket.
#[cfg(unix)]
pub fn socket_dir() -> Result<PathBuf, PathError> {
    let uid = our_uid();
    if cfg!(target_os = "macos") {
        Ok(std::env::temp_dir().join(format!("gmclip-{uid}")))
    } else {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(rt) => Ok(PathBuf::from(rt).join("gmclip")),
            None => Ok(app_data()?.join("run")),
        }
    }
}

/// The socket path, checked against `sun_path`.
#[cfg(unix)]
pub fn socket_path() -> Result<PathBuf, PathError> {
    let p = socket_dir()?.join("s");
    if p.as_os_str().len() > SUN_PATH_MAX {
        return Err(PathError::TooLong(p));
    }
    Ok(p)
}

/// The pipe name for a user SID: `\\.\pipe\gmclip-<sid>`. A pure function of the user,
/// like the Unix socket path; never read from a file.
pub fn pipe_name_for(sid: &str) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\gmclip-{sid}"))
}

/// `GMCLIP_PIPE_TAG`: a separate holder for this process tree (`gmclip-<SID>-<tag>`),
/// what `TMPDIR` gives on Unix; tests isolate with it. Ignored unless it is a short
/// `[A-Za-z0-9_-]` token. The pipe stays owner-only and server-checked either way.
#[cfg(windows)]
fn pipe_tag() -> Option<String> {
    std::env::var("GMCLIP_PIPE_TAG").ok().filter(|t| {
        !t.is_empty()
            && t.len() <= 32
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// The holder's named pipe (decisions/0019).
#[cfg(windows)]
pub fn socket_path() -> Result<PathBuf, PathError> {
    let sid = our_sid()?;
    let p = pipe_name_for(&match pipe_tag() {
        Some(t) => format!("{sid}-{t}"),
        None => sid,
    });
    if p.as_os_str().len() > PIPE_NAME_MAX {
        return Err(PathError::TooLong(p));
    }
    Ok(p)
}

/// The parent of the socket directory must be ours and not writable by others.
#[cfg(unix)]
fn check_parent(dir: &Path) -> Result<(), PathError> {
    let parent = dir
        .parent()
        .ok_or(PathError::Unsafe(dir.to_owned(), "no parent"))?;
    let m = fs::metadata(parent)
        .map_err(|e| PathError::Io("stat socket parent", e.kind().to_string()))?;
    if m.uid() != our_uid() {
        return Err(PathError::Unsafe(
            parent.to_owned(),
            "not owned by this user (a shared directory)",
        ));
    }
    if m.permissions().mode() & 0o022 != 0 {
        return Err(PathError::Unsafe(
            parent.to_owned(),
            "writable by others (a shared directory)",
        ));
    }
    Ok(())
}

/// Checks a directory is ours, mode 0700 and not a symlink (`lstat`).
#[cfg(unix)]
pub fn check_private_dir(dir: &Path) -> Result<(), PathError> {
    let m = fs::symlink_metadata(dir)
        .map_err(|e| PathError::Io("stat socket dir", e.kind().to_string()))?;
    if m.file_type().is_symlink() {
        return Err(PathError::Unsafe(dir.to_owned(), "is a symlink"));
    }
    if !m.is_dir() {
        return Err(PathError::Unsafe(dir.to_owned(), "is not a directory"));
    }
    if m.uid() != our_uid() {
        return Err(PathError::Unsafe(dir.to_owned(), "not owned by this user"));
    }
    if m.permissions().mode() & 0o777 != 0o700 {
        return Err(PathError::Unsafe(dir.to_owned(), "mode is not 0700"));
    }
    Ok(())
}

/// Creates `dir` as 0700 if missing, then checks it (and its parent).
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> Result<(), PathError> {
    check_parent(dir)?;
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(PathError::Io("create socket dir", e.kind().to_string())),
    }
    check_private_dir(dir)
}

/// Creates a directory (and parents) with mode 0700.
#[cfg(unix)]
pub fn mkdir_private(d: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(d)
}

/// Checks a directory is ours, lets nobody else in (its DACL), and is not a symlink or
/// junction (read without following it).
#[cfg(windows)]
pub fn check_private_dir(dir: &Path) -> Result<(), PathError> {
    let m = fs::symlink_metadata(dir)
        .map_err(|e| PathError::Io("stat private dir", e.kind().to_string()))?;
    if m.file_type().is_symlink() {
        return Err(PathError::Unsafe(
            dir.to_owned(),
            "is a symlink or junction",
        ));
    }
    if !m.is_dir() {
        return Err(PathError::Unsafe(dir.to_owned(), "is not a directory"));
    }
    let sec = gmclip_platform::windows::file_security(dir, false)
        .map_err(|e| PathError::Io("read the directory's ACL", e.to_string()))?;
    let me = our_sid()?;
    if sec.owner_sid != me {
        return Err(PathError::Unsafe(dir.to_owned(), "not owned by this user"));
    }
    if !sec.dacl_private_to(&me) {
        return Err(PathError::Unsafe(
            dir.to_owned(),
            "its ACL lets other accounts in",
        ));
    }
    Ok(())
}

/// Creates `dir` owner-only if missing, then checks it.
#[cfg(windows)]
pub fn ensure_private_dir(dir: &Path) -> Result<(), PathError> {
    match gmclip_platform::windows::create_dir_owner_only(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(PathError::Io("create private dir", e.kind().to_string())),
    }
    check_private_dir(dir)
}

/// Creates a directory and its missing parents, each with an owner-only DACL (the
/// Windows 0700); existing components are left as they are, as on Unix.
#[cfg(windows)]
pub fn mkdir_private(d: &Path) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(d);
    while let Some(c) = cur {
        if c.as_os_str().is_empty() || fs::metadata(c).is_ok() {
            break;
        }
        missing.push(c.to_owned());
        cur = c.parent();
    }
    for c in missing.into_iter().rev() {
        match gmclip_platform::windows::create_dir_owner_only(&c) {
            Ok(()) => {}
            // created in between (another thread or process): fine, as on Unix
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && c.is_dir() => {}
            Err(e) => return Err(e),
        }
    }
    if d.is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other("not a directory"))
    }
}

/// Where one holder or client process keeps its state: computed from the environment
/// in production, given explicitly in tests (which never touch the real home).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    pub app_data: PathBuf,
    pub socket: PathBuf,
}

impl Env {
    /// From `HOME`, `TMPDIR` / `XDG_RUNTIME_DIR` and the uid (Windows: `LOCALAPPDATA`
    /// and the user SID).
    pub fn from_env() -> Result<Env, PathError> {
        Ok(Env {
            app_data: app_data()?,
            socket: socket_path()?,
        })
    }

    /// The socket's directory.
    pub fn socket_dir(&self) -> &Path {
        self.socket.parent().unwrap_or(Path::new("/"))
    }

    /// Is a holder's socket there? Unix: the socket file exists. Windows: the pipe name
    /// is listed under `\\.\pipe\` (listing never connects; opening the name to stat it
    /// would take an instance from the holder).
    pub fn socket_present(&self) -> bool {
        #[cfg(windows)]
        {
            let Some(want) = self.socket.file_name() else {
                return false;
            };
            let want = want.to_string_lossy().to_lowercase();
            fs::read_dir(r"\\.\pipe\").is_ok_and(|rd| {
                rd.flatten()
                    .any(|e| e.file_name().to_string_lossy().to_lowercase() == want)
            })
        }
        #[cfg(not(windows))]
        {
            self.socket.exists()
        }
    }

    /// macOS: the same user's socket under the per-user temp dir launchd gives the app
    /// (`getconf DARWIN_USER_TEMP_DIR`), when this process tree's `TMPDIR` points
    /// elsewhere (an agent's sandbox often sets its own; decision 0048). `None` when it is
    /// this socket already, or off macOS. Computed from the OS, never read from a file.
    pub fn user_fallback(&self) -> Option<Env> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let out = std::process::Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let dir = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
        let socket = dir.join(format!("gmclip-{}", our_uid())).join("s");
        (dir.is_absolute() && socket != self.socket && socket.as_os_str().len() <= SUN_PATH_MAX)
            .then(|| Env {
                app_data: self.app_data.clone(),
                socket,
            })
    }

    /// Writes the pointer file (for people and diagnostics only; clients never read it).
    pub fn write_pointer(&self) {
        let d = self.app_data.join("run");
        if mkdir_private(&d).is_ok() {
            let _ = fs::write(
                d.join("socket-path"),
                self.socket.as_os_str().as_encoded_bytes(),
            );
        }
    }
}

/// The executable of a process, from the kernel (not its argv[0], which a process sets
/// itself). macOS: `lsof`'s text segment; Linux: `/proc/<pid>/exe`; Windows:
/// `QueryFullProcessImageNameW`.
#[cfg(windows)]
pub fn exe_of(pid: u32) -> Option<PathBuf> {
    gmclip_platform::windows::process_image(pid).ok()
}

/// The parent process of `pid`, from the kernel. On Windows a hint only: the parent's
/// id may have been reused since it exited (decisions/0019).
pub fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(windows)]
    {
        gmclip_platform::windows::parent_pid(pid).ok()
    }
    #[cfg(unix)]
    {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    }
}

/// The executable of a process, from the kernel (not its argv[0], which a process sets
/// itself). macOS: `lsof`'s text segment; Linux: `/proc/<pid>/exe`.
#[cfg(unix)]
pub fn exe_of(pid: u32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return fs::read_link(format!("/proc/{pid}/exe")).ok();
    }
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "txt", "-Fn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(PathBuf::from)
}

/// Our own executable, canonical.
pub fn our_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
}

/// Do two executable paths name the same file?
pub fn same_exe(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_owned());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_owned());
    ca == cb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_of_reads_the_kernel_not_argv0() {
        let me = exe_of(std::process::id()).expect("our exe");
        assert!(same_exe(&me, &our_exe().unwrap()), "{me:?}");
    }

    #[test]
    fn the_pipe_name_is_a_function_of_the_sid() {
        assert_eq!(
            pipe_name_for("S-1-5-21-1-2-3-1001").as_os_str(),
            r"\\.\pipe\gmclip-S-1-5-21-1-2-3-1001"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_dirs_are_created_owner_only() {
        let base = std::env::temp_dir().join(format!("gmclip-paths-w-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let d = base.join("a").join("b");
        mkdir_private(&d).unwrap();
        check_private_dir(&d).unwrap();
        check_private_dir(&base).unwrap();
        // created by plain std: inherits the temp dir's ACL, not private
        let plain = std::env::temp_dir().join(format!("gmclip-paths-p-{}", std::process::id()));
        fs::create_dir_all(&plain).unwrap();
        assert!(check_private_dir(&plain).is_err());
        assert!(
            socket_path()
                .unwrap()
                .to_string_lossy()
                .starts_with(r"\\.\pipe\gmclip-S-1-")
        );
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&plain);
    }

    #[cfg(unix)]
    #[test]
    fn private_dirs_are_checked() {
        let base = std::env::temp_dir().join(format!("gmclip-paths-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
        let d = base.join("d");
        ensure_private_dir(&d).unwrap();
        // wrong mode
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            check_private_dir(&d),
            Err(PathError::Unsafe(_, "mode is not 0700"))
        ));
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        // a symlink in its place
        let l = base.join("l");
        std::os::unix::fs::symlink(&d, &l).unwrap();
        assert!(matches!(
            check_private_dir(&l),
            Err(PathError::Unsafe(_, "is a symlink"))
        ));
        // a shared parent
        let shared = base.join("shared");
        fs::DirBuilder::new().mode(0o777).create(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_private_dir(&shared.join("x")).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    /// A sandbox's own `TMPDIR` falls back to the app's socket in the per-user temp dir;
    /// that socket itself has no fallback (decision 0048).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_sandboxed_tmpdir_falls_back_to_the_users_socket() {
        let sandboxed = Env {
            app_data: PathBuf::from("/tmp/a"),
            socket: PathBuf::from("/private/tmp/sandbox/gmclip-1/s"),
        };
        let alt = sandboxed
            .user_fallback()
            .expect("getconf DARWIN_USER_TEMP_DIR");
        assert!(
            alt.socket.starts_with("/var/folders"),
            "{}",
            alt.socket.display()
        );
        assert!(alt.socket.ends_with(format!("gmclip-{}/s", our_uid())));
        assert_eq!(alt.app_data, sandboxed.app_data);
        assert_eq!(alt.user_fallback(), None);
    }
}
