//! The one-writer lock (R-STORE-3): an exclusive OS advisory lock on `<db>.lock`, whose
//! content is `{pid, host, kind}` and nothing else (never a socket path: clients find
//! the holder at the fixed per-user socket, R-ARCH-5a). The same content is mirrored in
//! `<db>.lock.holder`, because Windows locks are mandatory and nobody else can read a
//! held `<db>.lock` there.
//!
//! The OS releases the lock when its holder dies, so a lock file that still names a
//! holder but can be locked is **stale**: its holder died without closing. A stale lock
//! from this machine — or content that does not parse, e.g. a write torn by power loss
//! — is taken over (the caller runs an integrity check first); one naming another
//! machine means the database is on shared storage, which R-STORE-4a forbids, and is
//! refused unless forced. A clean close empties the file before unlocking.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::StoreError;

/// What kind of process holds a project.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderKind {
    App,
    Headless,
    Cli,
    Test,
}

/// The lock file's content: exactly these three keys.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockInfo {
    pub pid: u32,
    pub host: String,
    pub kind: HolderKind,
}

/// A held lock. Dropping it releases the lock without emptying the file (as a crash
/// would); [`ProjectLock::release`] is the clean close.
#[derive(Debug)]
pub struct ProjectLock {
    file: File,
    db: PathBuf,
    pub info: LockInfo,
}

/// What acquiring found.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Acquired {
    /// The file was empty or new.
    Fresh,
    /// A stale lock from this machine (or unreadable content): the caller must run an
    /// integrity check.
    TookOverStale(LockInfo),
    /// A stale lock naming another machine, taken because the caller forced it.
    ForcedOtherHost(LockInfo),
}

fn sibling(db: &Path, ext: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

/// `<db>.lock` for a database path.
pub fn lock_path(db: &Path) -> PathBuf {
    sibling(db, ".lock")
}

/// `<db>.lock.holder`, the readable mirror of the lock file's content.
pub fn holder_path(db: &Path) -> PathBuf {
    sibling(db, ".lock.holder")
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
}

/// A stable identity for this machine, for deciding whether a stale lock is ours: the
/// hardware UUID on macOS (`hostname` there follows DHCP and changes between
/// networks), `/etc/machine-id` on Linux, `COMPUTERNAME` on Windows; else `hostname`;
/// `unknown-host` if nothing works.
pub fn local_host() -> String {
    let id = if cfg!(target_os = "macos") {
        run("/usr/sbin/ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"]).and_then(|out| {
            out.lines()
                .find(|l| l.contains("IOPlatformUUID"))
                .and_then(|l| l.rsplit('"').nth(1).map(str::to_owned))
        })
    } else if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        std::fs::read_to_string("/etc/machine-id").ok()
    };
    id.or_else(|| run("hostname", &[]))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_owned())
}

enum Content {
    Empty,
    Parsed(LockInfo),
    Unparsed,
}

impl ProjectLock {
    /// Takes the lock for `db`, or reports who holds it (`Leased`).
    pub fn acquire(
        db: &Path,
        info: LockInfo,
        force_other_host: bool,
    ) -> Result<(Self, Acquired), StoreError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(db))
            .map_err(|e| StoreError::io("open lock file", &e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = match read_content(&mut file) {
                    Ok(Content::Parsed(p)) => Some(p),
                    _ => peek_holder(db),
                };
                return Err(StoreError::Leased { holder });
            }
            Err(TryLockError::Error(e)) => return Err(StoreError::io("lock", &e)),
        }
        // We hold it: whatever the file still says is a holder that died.
        let acquired = match read_content(&mut file).unwrap_or(Content::Unparsed) {
            Content::Empty => Acquired::Fresh,
            Content::Unparsed => Acquired::TookOverStale(LockInfo {
                pid: 0,
                host: info.host.clone(),
                kind: info.kind,
            }),
            Content::Parsed(p) if p.host == info.host => Acquired::TookOverStale(p),
            Content::Parsed(p) if force_other_host => Acquired::ForcedOtherHost(p),
            Content::Parsed(p) => return Err(StoreError::StaleOtherHost { holder: p }),
        };
        write_info(&mut file, Some(&info))?;
        let body = serde_json::to_vec(&info).map_err(|_| StoreError::Corrupt("lock info"))?;
        std::fs::write(holder_path(db), body)
            .map_err(|e| StoreError::io("write lock holder", &e))?;
        Ok((
            Self {
                file,
                db: db.to_owned(),
                info,
            },
            acquired,
        ))
    }

    /// The clean close: empties the file and its mirror, then unlocks.
    pub fn release(mut self) -> Result<(), StoreError> {
        write_info(&mut self.file, None)?;
        let _ = std::fs::remove_file(holder_path(&self.db));
        self.file.unlock().map_err(|e| StoreError::io("unlock", &e))
    }

    pub fn path(&self) -> PathBuf {
        lock_path(&self.db)
    }
}

/// Reads the holder recorded for `db` without locking (status displays): the lock
/// file, or its mirror where the lock file cannot be read while held (Windows).
pub fn peek(db: &Path) -> Option<LockInfo> {
    let from_lock = File::open(lock_path(db))
        .ok()
        .and_then(|mut f| match read_content(&mut f) {
            Ok(Content::Parsed(p)) => Some(p),
            _ => None,
        });
    from_lock.or_else(|| peek_holder(db))
}

fn peek_holder(db: &Path) -> Option<LockInfo> {
    serde_json::from_slice(&std::fs::read(holder_path(db)).ok()?).ok()
}

fn read_content(f: &mut File) -> Result<Content, StoreError> {
    let mut b = Vec::new();
    f.seek(SeekFrom::Start(0))
        .and_then(|_| f.take(4096).read_to_end(&mut b))
        .map_err(|e| StoreError::io("read lock file", &e))?;
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(Content::Empty);
    }
    Ok(match serde_json::from_slice(&b) {
        Ok(p) => Content::Parsed(p),
        Err(_) => Content::Unparsed,
    })
}

fn write_info(f: &mut File, info: Option<&LockInfo>) -> Result<(), StoreError> {
    let body = match info {
        Some(i) => serde_json::to_string(i).map_err(|_| StoreError::Corrupt("lock info"))?,
        None => String::new(),
    };
    f.set_len(0)
        .and_then(|_| f.seek(SeekFrom::Start(0)))
        .and_then(|_| f.write_all(body.as_bytes()))
        .and_then(|_| f.sync_all())
        .map_err(|e| StoreError::io("write lock file", &e))
}
