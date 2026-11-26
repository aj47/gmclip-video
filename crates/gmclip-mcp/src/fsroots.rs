//! Agent file scope (R-FS-1).
//!
//! - **Roots are handles.** Each agent read root is opened once as a `cap_std::fs::Dir`;
//!   every agent file access is relative to that handle, so `..`, absolute symlinks and
//!   symlinks leaving the root fail inside cap-std (`openat` beneath the root), not in a
//!   string check that a rename could race.
//! - **Roots** are the human's home folder unless the Owner removed it (decision 0057),
//!   the recordings folder chosen at setup, roots the Owner added (read or write), and the
//!   homes of projects already open or in gmclip's library. Source directories are never
//!   roots on their own; inside the home folder they are readable because the home is.
//! - **Refused forms** (both grammars, before any filesystem call): empty, relative,
//!   NUL, any `..` or `.` component, a trailing dot or space, backslashes; and on the
//!   Windows grammar UNC (`\\server\…`), device paths (`\\.\`, `\\?\`), drive-relative
//!   (`C:foo`), alternate data streams (`:` after the drive), reserved device names.
//! - Errors never echo path bytes (R-FS-1: at most 16; these echo none).

use std::path::{Component, Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use gmclip_proto::CmdError;

pub use crate::pathform::Grammar;

fn refused(reason: &'static str) -> CmdError {
    CmdError::invalid("/path", reason)
}

/// Checks the path *form* (no filesystem access) and returns its components.
pub fn validate(path: &str, g: Grammar) -> Result<Vec<String>, CmdError> {
    crate::pathform::validate(path, g).map_err(refused)
}

/// The host's grammar.
pub fn host_grammar() -> Grammar {
    if cfg!(windows) {
        Grammar::Windows
    } else {
        Grammar::Unix
    }
}

/// One root: the configured path, its canonical form, and the handle opened once.
#[derive(Debug)]
pub struct Root {
    pub path: PathBuf,
    pub canon: PathBuf,
    pub dir: Dir,
}

impl Root {
    /// Opens a root directory (Owner-chosen, so its own symlinks are resolved once here).
    pub fn open(path: &Path) -> std::io::Result<Root> {
        let canon = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&canon, ambient_authority())?;
        Ok(Root {
            path: path.to_owned(),
            canon,
            dir,
        })
    }

    /// The part of `p` below this root, if `p` is lexically inside it.
    fn relative<'a>(&self, p: &'a Path) -> Option<&'a Path> {
        p.strip_prefix(&self.canon)
            .ok()
            .or_else(|| p.strip_prefix(&self.path).ok())
    }
}

/// The agent read roots in force for one call.
#[derive(Debug, Default)]
pub struct Roots {
    pub read: Vec<Root>,
}

impl Roots {
    /// Opens each path that exists (missing roots are skipped, not errors).
    pub fn open(paths: &[PathBuf]) -> Roots {
        Roots {
            read: paths.iter().filter_map(|p| Root::open(p).ok()).collect(),
        }
    }

    /// Opens the directory at `path` beneath one of the roots, through that root's
    /// handle. Escapes (symlinks out, `..`) fail in cap-std.
    pub fn open_dir(&self, path: &str) -> Result<(PathBuf, Dir), CmdError> {
        validate(path, host_grammar())?;
        let p = Path::new(path);
        for root in &self.read {
            let Some(rel) = root.relative(p) else {
                continue;
            };
            if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
                return Err(refused("`..` is not allowed"));
            }
            let dir = if rel.as_os_str().is_empty() {
                root.dir.try_clone().map_err(|_| refused("cannot open"))?
            } else {
                root.dir.open_dir(rel).map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => CmdError::NotFound,
                    _ => CmdError::Unauthorized {
                        scope: "file_roots".into(),
                    },
                })?
            };
            // the root itself is its canonical path, with no trailing separator
            let canon = if rel.as_os_str().is_empty() {
                root.canon.clone()
            } else {
                root.canon.join(rel)
            };
            return Ok((canon, dir));
        }
        Err(CmdError::Unauthorized {
            scope: "file_roots".into(),
        })
    }
}

impl Roots {
    /// A regular file beneath one of the roots, reached through that root's handle
    /// (symlinks out and `..` fail), as its canonical path.
    pub fn check_file(&self, path: &str) -> Result<PathBuf, CmdError> {
        validate(path, host_grammar())?;
        let p = Path::new(path);
        for root in &self.read {
            let Some(rel) = root.relative(p) else {
                continue;
            };
            if rel.as_os_str().is_empty()
                || rel.components().any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(refused("`..` is not allowed"));
            }
            let f = root.dir.open(rel).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => CmdError::NotFound,
                _ => CmdError::Unauthorized {
                    scope: "file_roots".into(),
                },
            })?;
            let is_file = f.metadata().is_ok_and(|m| m.is_file());
            if !is_file {
                return Err(refused("not a file"));
            }
            return Ok(root.canon.join(rel));
        }
        Err(CmdError::Unauthorized {
            scope: "file_roots".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad(p: &str, g: Grammar) {
        let e = validate(p, g).unwrap_err();
        let CmdError::Invalid { reason, path } = &e else {
            panic!("{e:?}")
        };
        assert_eq!(path, "/path");
        // no path bytes echoed
        assert!(!reason.contains("secret"), "{reason}");
    }

    #[test]
    fn refused_forms_unix() {
        for p in [
            "",
            "relative/secret",
            "/a/../secret",
            "/a/./secret",
            "/a/secret\0x",
            "/a\\secret",
            "/a/secret.",
            "/a/secret ",
        ] {
            bad(p, Grammar::Unix);
        }
        assert!(validate("/Users/aj/Movies/x.gmclip", Grammar::Unix).is_ok());
    }

    #[test]
    fn refused_forms_windows() {
        for p in [
            r"\\server\share\secret",
            r"\\.\PhysicalDrive0",
            r"\\?\C:\secret",
            r"C:secret",
            r"C:\a\secret:stream",
            r"C:\a\CON",
            r"C:\a\nul.txt",
            r"C:\a\..\secret",
            r"C:\a\secret.",
            r"relative\secret",
        ] {
            bad(p, Grammar::Windows);
        }
        assert!(validate(r"C:\Users\aj\x.gmclip", Grammar::Windows).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn handles_refuse_symlinks_out_of_a_root() {
        let base = std::env::temp_dir().join(format!("gmclip-roots-{}", std::process::id()));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("inside.gmclip")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape.gmclip")).unwrap();
        std::os::unix::fs::symlink("../outside", root.join("rel-escape.gmclip")).unwrap();
        let roots = Roots::open(std::slice::from_ref(&root));
        let ok = roots.open_dir(root.join("inside.gmclip").to_str().unwrap());
        assert!(ok.is_ok(), "{ok:?}");
        for name in ["escape.gmclip", "rel-escape.gmclip"] {
            let r = roots.open_dir(root.join(name).to_str().unwrap());
            assert!(
                matches!(r, Err(CmdError::Unauthorized { ref scope }) if scope == "file_roots"),
                "{name}: {r:?}"
            );
        }
        // outside every root
        let r = roots.open_dir(outside.to_str().unwrap());
        assert!(matches!(r, Err(CmdError::Unauthorized { .. })));
        // a source's own directory is not a root (only listed roots are)
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_swapped_in_after_the_check_is_still_refused() {
        // TOCTOU: the open goes through the root handle, so swapping a directory for a
        // symlink between calls cannot redirect it outside.
        let base = std::env::temp_dir().join(format!("gmclip-toctou-{}", std::process::id()));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("p.gmclip")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let roots = Roots::open(std::slice::from_ref(&root));
        assert!(
            roots
                .open_dir(root.join("p.gmclip").to_str().unwrap())
                .is_ok()
        );
        std::fs::remove_dir(root.join("p.gmclip")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("p.gmclip")).unwrap();
        assert!(
            roots
                .open_dir(root.join("p.gmclip").to_str().unwrap())
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
