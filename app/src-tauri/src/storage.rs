//! What gmclip keeps on this Mac, and "Free up N GB" (SPEC §12.5 R-ERR-2 [M3]; lane S).
//!
//! Settings shows each kind with its size. Only rebuildable caches can be freed: scrub
//! proxies, moment clips, cut-out mask tracks, search-by-meaning vectors and thumbnails —
//! each is made again from the recordings when it is next needed. Project data (the
//! project databases, their homes, exports) and the backups are shown and never freed
//! here; the speech models are shown and kept (transcription needs them). A cache that an
//! open project is using (its proxies and clips) is kept and said so.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// The kinds "Free up" may remove.
pub const FREEABLE: &[&str] = &["proxies", "clips", "masks", "embeddings", "thumbs"];

/// Bytes under `p`, not following symbolic links (a linked folder counts as its link).
pub fn size_of(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if !m.is_dir() {
        return m.len();
    }
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

fn cache(core: &AppCore) -> PathBuf {
    crate::proxy::cache_root(&core.dirs.app_data)
}

/// The cut-out mask tracks' folder (`$GMCLIP_MATTE_CACHE`, else the OS cache).
fn masks_dir(core: &AppCore) -> PathBuf {
    std::env::var_os("GMCLIP_MATTE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache(core).join("mattes"))
}

/// Per-project cache folders `<cache>/<project id>/<kind>`.
fn per_project(core: &AppCore, kind: &str) -> Vec<(String, PathBuf)> {
    std::fs::read_dir(cache(core))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let id = e.file_name().to_str()?.to_owned();
                    gmclip_model::Ulid::parse(&id).ok()?;
                    let p = e.path().join(kind);
                    p.is_dir().then_some((id, p))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn embeddings_bytes(core: &AppCore) -> u64 {
    let Ok(c) = rusqlite::Connection::open_with_flags(
        core.dirs.app_data.join("library.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return 0;
    };
    c.query_row("SELECT COALESCE(SUM(LENGTH(v)), 0) FROM ix_vec", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_or(0, |n| n.max(0) as u64)
}

fn row(id: &str, label: &str, bytes: u64, freeable: bool, what: &str) -> Value {
    json!({"id": id, "label": label, "bytes": bytes, "freeable": freeable, "what": what})
}

/// Sizes by kind, plus the total that "Free up" would remove now.
pub fn usage(core: &AppCore) -> Value {
    let app = &core.dirs.app_data;
    let projects = size_of(&app.join("projects"));
    let backups = size_of(&app.join("backups"));
    let open: Vec<String> = core.projects().iter().map(|p| p.info.id.clone()).collect();
    let sum = |kind: &str, skip_open: bool| -> (u64, u64) {
        per_project(core, kind)
            .into_iter()
            .fold((0, 0), |(all, free), (id, p)| {
                let n = size_of(&p);
                (
                    all + n,
                    if skip_open && open.contains(&id) {
                        free
                    } else {
                        free + n
                    },
                )
            })
    };
    let (proxies, proxies_free) = sum("proxies", true);
    let (clips, clips_free) = sum("clips", true);
    let masks = size_of(&masks_dir(core));
    let thumbs = size_of(&cache(core).join("thumbs"));
    let embeddings = embeddings_bytes(core);
    let models = size_of(&cache(core).join("models"));
    let rows = vec![
        row(
            "projects",
            "Your projects",
            projects,
            false,
            "edits, cuts, transcripts — never removed here",
        ),
        row(
            "backups",
            "Backups",
            backups,
            false,
            "earlier copies of each project (the newest 5, one a day for a week)",
        ),
        row(
            "models",
            "Speech model",
            models,
            false,
            "needed to transcribe; kept",
        ),
        row(
            "proxies",
            "Scrub proxies",
            proxies,
            true,
            "small copies for smooth scrubbing; made again when you open the video",
        ),
        row(
            "clips",
            "Moment clips",
            clips,
            true,
            "short clips for reviewing moments; made again when you play one",
        ),
        row(
            "masks",
            "Cut-out masks",
            masks,
            true,
            "tracked masks for cut-outs; tracked again on the next export",
        ),
        row(
            "embeddings",
            "Search by meaning",
            embeddings,
            true,
            "vectors for search by meaning; rebuilt in the background",
        ),
        row(
            "thumbs",
            "Thumbnails",
            thumbs,
            true,
            "video thumbnails on Home; made again when shown",
        ),
    ];
    let freeable = proxies_free + clips_free + masks + embeddings + thumbs;
    let kept_open = (proxies - proxies_free) + (clips - clips_free);
    json!({"rows": rows, "freeable_bytes": freeable, "kept_open_bytes": kept_open,
           "app_data": app, "cache": cache(core)})
}

fn remove(p: &Path) -> u64 {
    let n = size_of(p);
    let gone = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).is_ok(),
        Ok(_) => std::fs::remove_file(p).is_ok(),
        Err(_) => false,
    };
    if gone { n } else { 0 }
}

/// Removes the chosen rebuildable caches (`kinds` ⊆ [`FREEABLE`]; empty = all). Proxies
/// and clips of open projects are kept. Never touches project data or backups.
pub fn free(core: &AppCore, kinds: &[String]) -> AppResult<Value> {
    if let Some(k) = kinds.iter().find(|k| !FREEABLE.contains(&k.as_str())) {
        return Err(AppError::invalid(format!(
            "“{}” cannot be freed (only {})",
            k.chars().take(40).collect::<String>(),
            FREEABLE.join(", ")
        )));
    }
    let want = |k: &str| kinds.is_empty() || kinds.iter().any(|x| x == k);
    let open: Vec<String> = core.projects().iter().map(|p| p.info.id.clone()).collect();
    let mut freed = 0u64;
    let mut kept: Vec<String> = Vec::new();
    for kind in ["proxies", "clips"] {
        if !want(kind) {
            continue;
        }
        for (id, p) in per_project(core, kind) {
            if open.contains(&id) {
                if size_of(&p) > 0 {
                    kept.push(format!("{kind} of an open project"));
                }
                continue;
            }
            freed += remove(&p);
        }
    }
    if want("masks") {
        let d = masks_dir(core);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                freed += remove(&e.path());
            }
        }
    }
    if want("thumbs") {
        freed += remove(&cache(core).join("thumbs"));
    }
    if want("embeddings") {
        let before = embeddings_bytes(core);
        if before > 0
            && let Ok(c) = rusqlite::Connection::open(core.dirs.app_data.join("library.sqlite"))
            && c.execute_batch("BEGIN; DELETE FROM ix_vec; DELETE FROM ix_vec_done; COMMIT;")
                .is_ok()
        {
            let _ = c.execute_batch("VACUUM");
            freed += before;
        }
    }
    kept.dedup();
    Ok(json!({"freed_bytes": freed, "kept": kept, "usage": usage(core)}))
}
