//! The fonts captions, hooks and pills draw with (Neometric Heavy for the story style).
//! Font files are never stored in a project (SPEC §4.4): a project names a face by
//! family, style and SHA-256, and the app finds the file.
//!
//! Where the app looks, in order:
//! 1. **the app bundle**: `gmclip.app/Contents/Resources/fonts/fonts.json` and the files
//!    beside it, copied there at bundle time (`app/scripts/bundle-mac.sh`; on Windows
//!    `resources/fonts` beside the installed executable, `bundle-win.ps1`), so deleting a
//!    worktree cannot break captions;
//! 2. the faces the repo knows (`assets/fonts/faces.json`, when running from a
//!    checkout), each found in the user's font folders (`~/Library/Fonts`,
//!    `/Library/Fonts`; on Windows the per-user and system `Fonts` folders) by its
//!    SHA-256;
//! 3. the repo's free faces (`assets/fonts`).
//!
//! A file is used only when its SHA-256 is the one the catalogue names.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// One face.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Face {
    /// The ASS font name (`Neometric Heavy`).
    pub fontname: String,
    pub family: String,
    pub style: String,
    pub sha256: String,
    pub em_per_size: f64,
    /// Absolute path of the file (resolved; not serialised in the bundle manifest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// File name inside the bundle's fonts folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

fn sha_ok(p: &Path, want: &str) -> bool {
    gmclip_ml::sha256::file_hex(p).is_ok_and(|(h, _)| h == want)
}

/// The bundle's fonts folder, when running from `gmclip.app` (`Contents/Resources/fonts`)
/// or an installed Windows app (`resources\fonts` beside the executable).
pub fn bundle_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let d = if cfg!(windows) {
        exe.parent()?.join("resources").join("fonts")
    } else {
        exe.parent()?.parent()?.join("Resources/fonts")
    };
    d.join("fonts.json").is_file().then_some(d)
}

fn from_bundle() -> Vec<Face> {
    let Some(d) = bundle_dir() else {
        return Vec::new();
    };
    let Ok(b) = std::fs::read(d.join("fonts.json")) else {
        return Vec::new();
    };
    let faces: Vec<Face> = serde_json::from_slice(&b).unwrap_or_default();
    faces
        .into_iter()
        .filter_map(|mut f| {
            let file = f.file.clone()?;
            if file.contains('/') || file.starts_with('.') {
                return None;
            }
            let p = d.join(file);
            sha_ok(&p, &f.sha256).then(|| {
                f.path = Some(p);
                f
            })
        })
        .collect()
}

/// Faces the app knows but does not ship (`assets/fonts/faces.json`: a commercial face is
/// never committed), found by SHA-256 in the user's font folders.
fn from_known_faces() -> Vec<Face> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/faces.json");
    let Ok(b) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<Face>>(&b)
        .unwrap_or_default()
        .into_iter()
        .map(|f| Face {
            path: None,
            file: None,
            ..f
        })
        .collect()
}

/// The bundled OFL faces of the repo (`assets/fonts`): Figtree Black, the free caption
/// face of the built-in caption presets and brand kits (SPEC §8; OFL 1.1, notice beside it).
fn from_repo_assets() -> Vec<Face> {
    const FIGTREE: &str = "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef";
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/figtree/Figtree-Black.ttf");
    let Ok(face) = gmclip_render::text::font::FontFace::open(&p) else {
        return Vec::new();
    };
    vec![Face {
        fontname: "Figtree Black".into(),
        family: "Figtree".into(),
        style: "Black".into(),
        sha256: FIGTREE.into(),
        em_per_size: face.em_per_size(),
        path: Some(p),
        file: None,
    }]
}

fn user_font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(windows) {
        // per-user installs, then the system's
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(
                PathBuf::from(l)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        if let Some(w) = std::env::var_os("SystemRoot") {
            v.push(PathBuf::from(w).join("Fonts"));
        }
        return v;
    }
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join("Library/Fonts"));
    }
    v.push(PathBuf::from("/Library/Fonts"));
    v
}

/// Finds a face's file in the user's font folders by its hash.
fn in_user_fonts(sha: &str) -> Option<PathBuf> {
    for d in user_font_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ["otf", "ttf", "ttc"].contains(&ext.as_str()) && sha_ok(&p, sha) {
                return Some(p);
            }
        }
    }
    None
}

/// The faces this app can draw, each with a file whose hash is checked (once per launch).
pub fn catalogue() -> &'static [Face] {
    static C: OnceLock<Vec<Face>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out: Vec<Face> = from_bundle();
        for mut f in from_known_faces() {
            if out.iter().any(|o| o.sha256 == f.sha256) {
                continue;
            }
            let ok = f.path.as_deref().is_some_and(|p| sha_ok(p, &f.sha256));
            if !ok {
                f.path = in_user_fonts(&f.sha256);
            }
            if f.path.is_some() {
                out.push(f);
            }
        }
        for f in from_repo_assets() {
            let ok = f.path.as_deref().is_some_and(|p| sha_ok(p, &f.sha256));
            if ok && !out.iter().any(|o| o.sha256 == f.sha256) {
                out.push(f);
            }
        }
        out
    })
}

/// A face by its font name (`Neometric Heavy`).
pub fn by_name(name: &str) -> Option<&'static Face> {
    catalogue().iter().find(|f| f.fontname == name)
}

/// `gmclip-app bundle-fonts <dir>` (bundle time): copies every face of the catalogue into
/// `dir` with a `fonts.json` naming them, so the app bundle carries its fonts.
pub fn export(dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for f in catalogue() {
        let Some(src) = f.path.as_ref() else { continue };
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or("a font without a file name")?;
        let dst = dir.join(&name);
        std::fs::copy(src, &dst).map_err(|e| format!("{name}: {e}"))?;
        if !sha_ok(&dst, &f.sha256) {
            return Err(format!("{name}: the copy is not the pinned font"));
        }
        // an OFL face ships with its licence beside it (Figtree: assets/fonts/figtree/OFL.txt)
        if let Some(ofl) = src
            .parent()
            .map(|d| d.join("OFL.txt"))
            .filter(|p| p.is_file())
        {
            std::fs::copy(&ofl, dir.join(format!("{name}.OFL.txt")))
                .map_err(|e| format!("{name} licence: {e}"))?;
        }
        rows.push(Face {
            path: None,
            file: Some(name),
            ..f.clone()
        });
    }
    std::fs::write(
        dir.join("fonts.json"),
        serde_json::to_vec_pretty(&rows).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(rows.len())
}
