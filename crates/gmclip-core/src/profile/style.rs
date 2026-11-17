//! The style rules file (SR R-PROF-3/5; SPEC §10.5): one Markdown file whose path is an
//! **Owner setting**, read **read-only** into
//! a pinned snapshot `{path, sha256, read_at, text}` kept in app data
//! (`style-pin.json`). When the file changes on disk nothing new is served until the
//! Owner accepts the diff ([`accept`] takes the hash the human saw); until then
//! `help{topic:"style"}` serves the pinned text, marked untrusted with its hash.
//!
//! Confinement: at most 256 KB, UTF-8, a regular file; a symlink as the final component
//! is refused, the parent is canonicalised (symlinks there resolve), and the open uses
//! `O_NOFOLLOW` where the platform has it, with an fstat/lstat identity check after.
//! gmclip never writes the style file (D16): there is no code path that opens it for
//! writing.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The largest style file served (R-PROF-5).
pub const MAX_STYLE_BYTES: u64 = 256 * 1024;
/// The pin's file name in app data.
pub const PIN_FILE: &str = "style-pin.json";

/// The pinned snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub path: PathBuf,
    pub sha256: String,
    /// ms since the epoch.
    pub read_at: i64,
    pub bytes: u64,
    pub text: String,
    /// The file sits in a profile folder gmclip did not make here (someone else's):
    /// served as `<untrusted source="external-profile">` (R-PROF-5).
    #[serde(default)]
    pub external: bool,
}

/// The file on disk against the pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub path: PathBuf,
    pub pinned_sha256: String,
    pub read_at: i64,
    /// The file's hash now, or why it cannot be read.
    pub current: Result<String, String>,
    pub changed: bool,
    /// A line diff, pinned → current (empty when unchanged or unreadable).
    pub diff: String,
}

/// `hex(sha256(bytes))`.
pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
pub(crate) const O_NOFOLLOW: i32 = 0x0100;
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
pub(crate) const O_NOFOLLOW: i32 = 0o100000;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")),
    not(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))
))]
pub(crate) const O_NOFOLLOW: i32 = 0o400000;

/// Windows has no `O_NOFOLLOW` (W.9): `FILE_FLAG_OPEN_REPARSE_POINT` opens a symlink or
/// junction itself instead of what it points at, and the opened handle's attributes then
/// say whether it was one (`FILE_ATTRIBUTE_REPARSE_POINT`): refused, with no window
/// between the check and the open.
#[cfg(windows)]
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// `true` when an opened file is a reparse point (a symlink or junction opened with
/// [`FILE_FLAG_OPEN_REPARSE_POINT`]).
#[cfg(windows)]
pub(crate) fn is_reparse_point(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Reads `path` as UTF-8 text, confined as the module docs say. Errors are shape-only.
pub fn read_confined(path: &Path, max: u64) -> Result<String, String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("cannot read it ({})", e.kind()))?;
    if meta.file_type().is_symlink() {
        return Err("it is a symlink (refused)".into());
    }
    if !meta.is_file() {
        return Err("it is not a regular file".into());
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let f = o
        .open(path)
        .map_err(|e| format!("cannot open it ({})", e.kind()))?;
    let fm = f
        .metadata()
        .map_err(|e| format!("cannot stat it ({})", e.kind()))?;
    #[cfg(windows)]
    if is_reparse_point(&fm) || !fm.is_file() {
        return Err("it is a symlink or junction (refused)".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fm.dev() != meta.dev() || fm.ino() != meta.ino() {
            return Err("it changed while being opened (refused)".into());
        }
    }
    if fm.len() > max {
        return Err(format!("it is larger than {max} bytes"));
    }
    let mut buf = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read it ({})", e.kind()))?;
    if buf.len() as u64 > max {
        return Err(format!("it is larger than {max} bytes"));
    }
    String::from_utf8(buf).map_err(|_| "it is not UTF-8".into())
}

/// The path as it will be pinned: absolute, parent canonicalised, file name kept.
pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("the style file path must be absolute".into());
    }
    let name = path.file_name().ok_or("the path names no file")?;
    let parent = path.parent().ok_or("the path has no parent")?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("its folder cannot be resolved ({})", e.kind()))?;
    Ok(plain(parent).join(name))
}

/// A canonical path in its plain form: on Windows `canonicalize` gives a verbatim path
/// (`\\?\C:\x`), which the owner commands refuse as a device path; the pin, the sheet and
/// the profile carry `C:\x`. Elsewhere unchanged.
fn plain(p: PathBuf) -> PathBuf {
    if cfg!(windows)
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    p
}

fn read_style(path: &Path) -> Result<(PathBuf, String, String), String> {
    let p = canonical(path)?;
    let text = read_confined(&p, MAX_STYLE_BYTES)?;
    let sha = sha256_hex(text.as_bytes());
    Ok((p, text, sha))
}

fn pin_path(app: &Path) -> PathBuf {
    app.join(PIN_FILE)
}

fn save_pin(app: &Path, pin: &Pin) -> Result<(), String> {
    std::fs::create_dir_all(app).map_err(|e| format!("app data ({})", e.kind()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(app, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = app.join(format!("{PIN_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(pin).unwrap_or_default())
        .map_err(|e| format!("write the pin ({})", e.kind()))?;
    std::fs::rename(&tmp, pin_path(app)).map_err(|e| format!("write the pin ({})", e.kind()))
}

/// The pinned snapshot, if the Owner set a style file.
pub fn pinned(app: &Path) -> Option<Pin> {
    let b = std::fs::read(pin_path(app)).ok()?;
    serde_json::from_slice(&b).ok()
}

/// Whether the file sits in a profile folder gmclip did not make on this machine.
fn is_external(app: &Path, file: &Path) -> bool {
    file.parent()
        .is_some_and(|d| d.join(super::PROFILE_FILE).exists() && !super::is_local(app, d))
}

/// What the Owner is shown before pinning or accepting: the file's hash and its text
/// with every control, bidi, zero-width and tag character made visible.
pub fn preview(path: &Path) -> Result<(PathBuf, String, String), String> {
    let (p, text, sha) = read_style(path)?;
    Ok((p, sha, visible(&text)))
}

/// Owner: pins `path` as the style file. `seen_sha256`, when given, must be the hash of
/// the version the Owner was shown (`preview`).
pub fn set(app: &Path, path: &Path, now_ms: i64, seen_sha256: Option<&str>) -> Result<Pin, String> {
    let (p, text, sha) = read_style(path)?;
    if seen_sha256.is_some_and(|s| s != sha) {
        return Err("the file changed since it was shown; review it again".into());
    }
    let pin = Pin {
        external: is_external(app, &p),
        path: p,
        sha256: sha,
        read_at: now_ms,
        bytes: text.len() as u64,
        text,
    };
    save_pin(app, &pin)?;
    Ok(pin)
}

/// Whether the file on disk differs from the pin (hash only; cheap).
pub fn changed(app: &Path) -> bool {
    pinned(app).is_some_and(|pin| read_style(&pin.path).is_ok_and(|(_, _, sha)| sha != pin.sha256))
}

/// `text` with every character that could hide or reorder what a terminal shows made
/// visible as `⟦U+XXXX⟧` (C0/C1 controls but `\n` and `\t`, ESC sequences, bidi
/// controls, zero-width characters, tags, variation selectors) — R-INJ-6 for the Owner's
/// own eyes.
pub fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let u = c as u32;
        let hide = (c.is_control() && c != '\n' && c != '\t')
            || matches!(u, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF | 0x061C | 0x180E)
            || (0xE0000..=0xE007F).contains(&u)
            || (0xFE00..=0xFE0F).contains(&u)
            || (0xE0100..=0xE01EF).contains(&u);
        if hide {
            out.push_str(&format!("⟦U+{u:04X}⟧"));
        } else {
            out.push(c);
        }
    }
    out
}

/// The file on disk against the pin (`None` when no style file is set), with the diff
/// (made [`visible`]) — for the Owner's accept; agents' reads use [`changed`].
pub fn status(app: &Path) -> Option<Status> {
    let pin = pinned(app)?;
    let now = read_style(&pin.path);
    let (current, changed, diff) = match &now {
        Ok((_, text, sha)) => {
            let changed = *sha != pin.sha256;
            (
                Ok(sha.clone()),
                changed,
                if changed {
                    diff(&pin.text, text)
                } else {
                    String::new()
                },
            )
        }
        Err(e) => (Err(e.clone()), false, String::new()),
    };
    Some(Status {
        path: pin.path,
        pinned_sha256: pin.sha256,
        read_at: pin.read_at,
        current,
        changed,
        diff,
    })
}

/// Owner: accepts the file's current text, which must still hash to `seen_sha256`
/// (the version whose diff the human was shown).
pub fn accept(app: &Path, seen_sha256: &str, now_ms: i64) -> Result<Pin, String> {
    let pin = pinned(app).ok_or("no style file is set")?;
    let (p, text, sha) = read_style(&pin.path)?;
    if sha != seen_sha256 {
        return Err("the file changed again since its diff was shown; review it again".into());
    }
    let pin = Pin {
        external: is_external(app, &p),
        path: p,
        sha256: sha,
        read_at: now_ms,
        bytes: text.len() as u64,
        text,
    };
    save_pin(app, &pin)?;
    Ok(pin)
}

/// A line diff (`-`/`+`/` ` prefixes, 2 lines of context, `@@` between hunks). Large
/// inputs whose comparison table would exceed 4·10⁷ cells are summarised instead.
pub fn diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len().saturating_mul(b.len()) > 40_000_000 {
        return format!(
            "(too large to diff: {} lines → {} lines)\n",
            a.len(),
            b.len()
        );
    }
    // LCS table, then walk
    let (n, m) = (a.len(), b.len());
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[at(i, j)] = if a[i] == b[j] {
                t[at(i + 1, j + 1)] + 1
            } else {
                t[at(i + 1, j)].max(t[at(i, j + 1)])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || t[at(i + 1, j)] >= t[at(i, j + 1)]) {
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    let keep: Vec<bool> = (0..ops.len())
        .map(|k| {
            let lo = k.saturating_sub(2);
            let hi = (k + 2).min(ops.len() - 1);
            (lo..=hi).any(|x| ops[x].0 != ' ')
        })
        .collect();
    let mut out = String::new();
    let mut last_kept = true;
    for (k, (c, l)) in ops.iter().enumerate() {
        if keep[k] {
            if !last_kept {
                out.push_str("@@\n");
            }
            out.push(*c);
            out.push(' ');
            out.push_str(l);
            out.push('\n');
        }
        last_kept = keep[k];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_path_is_never_a_device_path() {
        let d = tmp("plain");
        let p = canonical(&d.join("AGENTS.md")).unwrap();
        let s = p.to_string_lossy();
        assert!(!s.starts_with(r"\\?\"), "{s}");
        assert!(p.is_absolute() && s.ends_with("AGENTS.md"), "{s}");
        if cfg!(windows) {
            assert_eq!(
                plain(PathBuf::from(r"\\?\C:\x\y")),
                PathBuf::from(r"C:\x\y")
            );
            // a verbatim path that is not a drive path stays as it is
            let v = PathBuf::from(r"\\?\Volume{0}\x");
            assert_eq!(plain(v.clone()), v);
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("gmclip-style-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    /// SR R-PROF-3's acceptance: an edit on disk is not served until accepted, and the
    /// accept must name the hash whose diff was shown.
    #[test]
    fn edits_wait_for_the_owner() {
        let d = tmp("pin");
        let app = d.join("app");
        let f = d.join("AGENTS.md");
        std::fs::write(&f, "rule one\nrule two\n").unwrap();
        let pin = set(&app, &f, 1, None).unwrap();
        std::fs::write(&f, "rule one\nrule 2\nrule three\n").unwrap();
        let st = status(&app).unwrap();
        assert!(st.changed);
        assert!(
            st.diff.contains("- rule two") && st.diff.contains("+ rule three"),
            "{}",
            st.diff
        );
        assert_eq!(
            pinned(&app).unwrap().sha256,
            pin.sha256,
            "still the old text"
        );
        assert!(
            accept(&app, &pin.sha256, 2).is_err(),
            "not the hash the human saw"
        );
        let seen = st.current.unwrap();
        let p2 = accept(&app, &seen, 3).unwrap();
        assert!(p2.text.contains("rule three"));
        assert!(!status(&app).unwrap().changed);
        // the style file itself is untouched
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "rule one\nrule 2\nrule three\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn confinement() {
        let d = tmp("conf");
        let big = d.join("big.md");
        std::fs::write(&big, vec![b'a'; 300 * 1024]).unwrap();
        assert!(
            read_confined(&big, MAX_STYLE_BYTES)
                .unwrap_err()
                .contains("larger")
        );
        let bin = d.join("bin.md");
        std::fs::write(&bin, [0xffu8, 0xfe]).unwrap();
        assert!(
            read_confined(&bin, MAX_STYLE_BYTES)
                .unwrap_err()
                .contains("UTF-8")
        );
        #[cfg(unix)]
        {
            let ok = d.join("ok.md");
            std::fs::write(&ok, "x").unwrap();
            let link = d.join("link.md");
            std::os::unix::fs::symlink(&ok, &link).unwrap();
            assert!(
                set(&d.join("app"), &link, 0, None)
                    .unwrap_err()
                    .contains("symlink")
            );
        }
        // Windows (W.9; awaits the PC): a symlink and a junction are refused too, and a
        // regular file opened with FILE_FLAG_OPEN_REPARSE_POINT still reads. Creating a
        // symlink needs Developer Mode or the privilege: skipped when it is refused.
        #[cfg(windows)]
        {
            let ok = d.join("ok.md");
            std::fs::write(&ok, "x").unwrap();
            assert_eq!(read_confined(&ok, MAX_STYLE_BYTES).unwrap(), "x");
            let link = d.join("link.md");
            if std::os::windows::fs::symlink_file(&ok, &link).is_ok() {
                assert!(read_confined(&link, MAX_STYLE_BYTES).is_err());
                assert!(set(&d.join("app"), &link, 0, None).is_err());
            }
        }
        assert!(set(&d.join("app"), Path::new("relative.md"), 0, None).is_err());
        let ok2 = d.join("ok2.md");
        std::fs::write(&ok2, "x\u{1b}[2K\u{202E}y").unwrap();
        let (_, sha, shown) = preview(&ok2).unwrap();
        assert_eq!(shown, "x⟦U+001B⟧[2K⟦U+202E⟧y");
        assert!(
            set(&d.join("app"), &ok2, 0, Some("00")).is_err(),
            "not the version shown"
        );
        assert!(set(&d.join("app"), &ok2, 0, Some(&sha)).is_ok());
        assert!(read_confined(&d, MAX_STYLE_BYTES).is_err(), "a folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn diffs_show_context_and_hunks() {
        let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let b = "1\n2\nX\n4\n5\n6\n7\n8\nY\n";
        let d = diff(a, b);
        assert!(d.contains("- 3\n+ X\n"), "{d}");
        assert!(d.contains("@@"), "{d}");
        assert_eq!(diff("same\n", "same\n").trim(), "");
    }
}
