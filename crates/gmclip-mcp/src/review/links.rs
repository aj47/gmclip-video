//! Review links (SR R-REV-2): 256-bit tokens that travel only in a URL fragment, stored
//! here as their SHA-256 only, expiring after 7 days, listed and revocable one by one.
//! The file is `review-links.json` in app data (0700 folder, written by temp + rename).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths;
use crate::wire;

/// How long a link lives.
pub const LINK_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
/// The file name in app data.
pub const LINKS_FILE: &str = "review-links.json";

/// One link (its token is never stored).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// Public id (16 hex), used in authorship (`review-link:<id>`) and to revoke.
    pub id: String,
    pub token_sha256: String,
    pub created_ms: i64,
    pub expires_ms: i64,
    #[serde(default)]
    pub revoked: bool,
    /// In tailnet mode, the only tailnet login this link admits (default: the Owner's).
    #[serde(default)]
    pub login: Option<String>,
    /// `editor` (decision 0015): an outside editor's link, which comments and suggests
    /// edits only (no verdicts, no agent runs, no approvals). Absent: a reviewer's link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The roles a link can have (decision 0015).
pub const ROLES: &[&str] = &["editor"];

fn file(app: &Path) -> PathBuf {
    app.join(LINKS_FILE)
}

/// All links (missing or unreadable → none: never wider).
pub fn load(app: &Path) -> Vec<Link> {
    std::fs::read(file(app))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(app: &Path, links: &[Link]) -> std::io::Result<()> {
    paths::mkdir_private(app)?;
    let tmp = app.join(format!("{LINKS_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(links).unwrap_or_default())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, file(app))
}

/// `hex(sha256(token))`.
pub fn token_hash(token: &str) -> String {
    wire::hex(&Sha256::digest(token.as_bytes()))
}

/// Runs `f` holding `review-links.lock` (an exclusive create; a stale lock older than
/// 10 s is broken), so a create and a revoke never undo each other.
fn locked<T>(app: &Path, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    paths::mkdir_private(app)?;
    let lock = app.join("review-links.lock");
    let start = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(10));
                if stale {
                    let _ = std::fs::remove_file(&lock);
                } else if start.elapsed() > std::time::Duration::from_secs(5) {
                    return Err(std::io::Error::other("the review links are locked"));
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            Err(e) => return Err(e),
        }
    }
    let r = f();
    let _ = std::fs::remove_file(&lock);
    r
}

/// Makes a link; returns it and its token (shown once, never stored).
pub fn create(app: &Path, now_ms: i64, login: Option<String>) -> std::io::Result<(Link, String)> {
    create_with_role(app, now_ms, login, None)
}

/// [`create`] with a role (`Some("editor")`, decision 0015); an unknown role is refused.
pub fn create_with_role(
    app: &Path,
    now_ms: i64,
    login: Option<String>,
    role: Option<String>,
) -> std::io::Result<(Link, String)> {
    if role.as_deref().is_some_and(|r| !ROLES.contains(&r)) {
        return Err(std::io::Error::other("unknown role"));
    }
    locked(app, || create_unlocked(app, now_ms, login, role))
}

fn create_unlocked(
    app: &Path,
    now_ms: i64,
    login: Option<String>,
    role: Option<String>,
) -> std::io::Result<(Link, String)> {
    let token = wire::nonce(); // 256 bits, hex
    let id = wire::nonce()[..16].to_owned();
    let link = Link {
        id,
        token_sha256: token_hash(&token),
        created_ms: now_ms,
        expires_ms: now_ms + LINK_TTL_MS,
        revoked: false,
        login,
        role,
    };
    let mut all = load(app);
    all.retain(|l| l.expires_ms > now_ms - LINK_TTL_MS); // drop long-dead links
    all.push(link.clone());
    save(app, &all)?;
    Ok((link, token))
}

/// Revokes one link (`Some(id)`) or all (`None`). Returns how many changed.
pub fn revoke(app: &Path, id: Option<&str>) -> std::io::Result<usize> {
    locked(app, || revoke_unlocked(app, id))
}

fn revoke_unlocked(app: &Path, id: Option<&str>) -> std::io::Result<usize> {
    let mut all = load(app);
    let mut n = 0;
    for l in &mut all {
        if !l.revoked && id.is_none_or(|i| i == l.id) {
            l.revoked = true;
            n += 1;
        }
    }
    save(app, &all)?;
    Ok(n)
}

/// The live link a token opens, if any (constant-time hash compare).
pub fn find(app: &Path, token: &str, now_ms: i64) -> Option<Link> {
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = token_hash(token);
    load(app).into_iter().find(|l| {
        let eq = l.token_sha256.len() == h.len()
            && l.token_sha256
                .bytes()
                .zip(h.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        eq && !l.revoked && l.expires_ms > now_ms
    })
}

/// Whether a link (by id) is still live (sessions re-check this on every request).
pub fn live(app: &Path, id: &str, now_ms: i64) -> bool {
    load(app)
        .iter()
        .any(|l| l.id == id && !l.revoked && l.expires_ms > now_ms)
}
