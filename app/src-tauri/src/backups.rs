//! Rolling project backups (SPEC §14, R-STORE-5; lane S): a copy of each project's
//! database in `<app data>/backups/<project id>/`, taken
//! - **at quit**, after the project is closed (nothing writes it then), when it changed
//!   since its last copy;
//! - **every 30 minutes of editing**, when the project has edits newer than its last copy
//!   and everything committed is already Saved (no write is in flight: never while
//!   writing). The copy itself is one WAL snapshot (`gmclip_store::backup_into`).
//!
//! Kept: the newest [`KEEP_RECENT`], plus the newest of each of the last [`KEEP_DAILY`]
//! days. "Restore an earlier copy" (Home) closes the project, first copies the current
//! database as a `before-restore` backup (so a restore can itself be undone), puts the
//! chosen copy in place and reopens the project. Backups are never project data a "free
//! up space" deletes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};

/// Copies kept regardless of age.
pub const KEEP_RECENT: usize = 5;
/// Days that keep their newest copy.
pub const KEEP_DAILY: i64 = 7;
/// A project being edited is copied this often.
pub const EVERY_MS: i64 = 30 * 60 * 1000;
const DAY_MS: i64 = 86_400_000;

/// The folder of a project's backups.
pub fn dir(core: &AppCore, project: &str) -> AppResult<PathBuf> {
    gmclip_model::Ulid::parse(project).map_err(|_| AppError::invalid("not a project id"))?;
    Ok(core.dirs.app_data.join("backups").join(project))
}

fn db(core: &AppCore, project: &str) -> AppResult<PathBuf> {
    gmclip_mcp::project::db_path(&core.dirs.app_data, project).map_err(AppError::from)
}

/// The project's home on this Mac (the registry written when it was made).
fn home_of(core: &AppCore, project: &str) -> Option<PathBuf> {
    let b = std::fs::read(
        core.dirs
            .app_data
            .join("projects")
            .join(format!("{project}.home")),
    )
    .ok()?;
    let s = String::from_utf8(b).ok()?;
    Some(PathBuf::from(s))
}

/// One backup: `<at ms>-<reason>.sqlite` with `<same>.json` beside it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Backup {
    pub id: String,
    pub at_ms: i64,
    pub reason: String,
    pub rev: u64,
    pub bytes: u64,
    pub name: String,
}

fn list_dir(d: &Path) -> Vec<Backup> {
    let mut out: Vec<Backup> = std::fs::read_dir(d)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().is_none_or(|x| x != "json") {
                        return None;
                    }
                    let b = std::fs::read(&p).ok().filter(|b| b.len() < 64 << 10)?;
                    let k: Backup = serde_json::from_slice(&b).ok()?;
                    d.join(format!("{}.sqlite", k.id)).is_file().then_some(k)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));
    out
}

/// A project's backups, newest first.
pub fn list(core: &AppCore, project: &str) -> AppResult<Vec<Backup>> {
    Ok(list_dir(&dir(core, project)?))
}

/// The newest `KEEP_RECENT`, plus the newest of each of the last `KEEP_DAILY` days
/// (counted back from `now`); the rest are deleted. Returns the ids deleted.
fn rotate(d: &Path, now: i64) -> Vec<String> {
    let all = list_dir(d);
    let mut keep: Vec<&str> = all
        .iter()
        .take(KEEP_RECENT)
        .map(|b| b.id.as_str())
        .collect();
    let mut days_seen: Vec<i64> = Vec::new();
    for b in &all {
        let day = b.at_ms.div_euclid(DAY_MS);
        if now.div_euclid(DAY_MS) - day < KEEP_DAILY && !days_seen.contains(&day) {
            days_seen.push(day);
            keep.push(&b.id);
        }
    }
    let mut gone = Vec::new();
    for b in &all {
        if !keep.contains(&b.id.as_str()) {
            let _ = std::fs::remove_file(d.join(format!("{}.sqlite", b.id)));
            let _ = std::fs::remove_file(d.join(format!("{}.json", b.id)));
            gone.push(b.id.clone());
        }
    }
    gone
}

fn write_one(
    core: &AppCore,
    project: &str,
    reason: &str,
    rev: u64,
    name: &str,
) -> AppResult<Backup> {
    let d = dir(core, project)?;
    gmclip_mcp::paths::mkdir_private(&d)
        .map_err(|e| AppError::internal(format!("backup folder: {}", e.kind())))?;
    let mut at = now_ms();
    // ids are unique even for two copies in one millisecond
    while d.join(format!("{at}-{reason}.sqlite")).exists() {
        at += 1;
    }
    let id = format!("{at}-{reason}");
    let bytes = gmclip_core::backup_into(&db(core, project)?, &d.join(format!("{id}.sqlite")))
        .map_err(|e| {
            if e.is_disk_full() {
                AppError::new("DiskFull", "no space for a backup copy")
            } else {
                AppError::internal(format!("backup: {e}"))
            }
        })?;
    let b = Backup {
        id: id.clone(),
        at_ms: at,
        reason: reason.to_owned(),
        rev,
        bytes,
        name: name.to_owned(),
    };
    let body = serde_json::to_vec_pretty(&b).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&d.join(format!("{id}.json")), &body)
        .map_err(|e| AppError::internal(format!("backup record: {}", e.kind())))?;
    rotate(&d, at);
    Ok(b)
}

/// The rev a closed project's database is at (read only, no lock).
fn closed_rev(core: &AppCore, project: &str) -> Option<u64> {
    let s = gmclip_core::ReadOnlyStore::open(&db(core, project).ok()?).ok()?;
    s.doc().ok().map(|d| d.rev())
}

/// Copies an open project now when it changed since its last copy and nothing is being
/// written; `None` when there was nothing to do (or a write was in flight).
pub fn maybe_backup_open(core: &AppCore, project: &str, reason: &str) -> AppResult<Option<Backup>> {
    let p = core.project(project)?;
    let rev = p.handle.snapshot().doc.rev();
    let last = list(core, project)?.first().map(|b| b.rev);
    if last.is_some_and(|r| r >= rev) || rev == 0 {
        return Ok(None);
    }
    // never while writing: everything committed is Saved and no edit waits for space
    let (saved, err) = p.handle.saved();
    if err.is_some() || saved < rev {
        return Ok(None);
    }
    write_one(core, project, reason, rev, &p.info.name).map(Some)
}

/// At quit, for a project just closed at `rev`: a copy when it changed since its last one.
pub fn backup_closed(
    core: &AppCore,
    project: &str,
    name: &str,
    rev: u64,
) -> AppResult<Option<Backup>> {
    if rev == 0 || list(core, project)?.first().is_some_and(|b| b.rev >= rev) {
        return Ok(None);
    }
    write_one(core, project, "quit", rev, name).map(Some)
}

/// Every project with backups, for Home's "Restore an earlier copy": name, whether it is
/// open, and its copies newest first.
pub fn overview(core: &AppCore) -> Value {
    let root = core.dirs.app_data.join("backups");
    let mut rows: Vec<Value> = std::fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let id = e.file_name().to_str()?.to_owned();
                    gmclip_model::Ulid::parse(&id).ok()?;
                    let list = list_dir(&e.path());
                    let newest = list.first()?.clone();
                    let name = core
                        .project(&id)
                        .map_or_else(|_| newest.name.clone(), |p| p.info.name.clone());
                    Some(
                        json!({"project": id, "name": name, "open": core.is_open(&id),
                                "home": home_of(core, &id), "backups": list}),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by_key(|r| std::cmp::Reverse(r["backups"][0]["at_ms"].as_i64().unwrap_or(0)));
    json!({"projects": rows, "keep_recent": KEEP_RECENT, "keep_daily": KEEP_DAILY,
           "every_min": EVERY_MS / 60_000})
}

/// Puts backup `id` back as the project's database: the project is closed, the current
/// database is first copied as a `before-restore` backup, the copy replaces it (its WAL
/// and shared-memory files removed so nothing stale replays over it), and the project
/// is reopened from its home.
pub fn restore(core: &Arc<AppCore>, project: &str, id: &str) -> AppResult<Value> {
    let d = dir(core, project)?;
    let list = list_dir(&d);
    let b = list
        .iter()
        .find(|b| b.id == id)
        .ok_or_else(|| AppError::not_found("no such backup"))?
        .clone();
    let home = core
        .project(project)
        .map(|p| p.home.clone())
        .ok()
        .or_else(|| home_of(core, project))
        .ok_or_else(|| {
            AppError::not_found(format!(
                "the project's folder is not on {}",
                crate::caps::THIS_MACHINE
            ))
        })?;
    let name = core
        .project(project)
        .map_or_else(|_| b.name.clone(), |p| p.info.name.clone());
    if core.is_open(project) {
        core.close_project(project)?;
    }
    let live = db(core, project)?;
    // the current state first, so this restore can be undone
    let before = match closed_rev(core, project) {
        Some(rev) if live.is_file() => {
            Some(write_one(core, project, "before-restore", rev, &name)?)
        }
        _ => None,
    };
    let part = live.with_extension("sqlite.restoring");
    std::fs::copy(d.join(format!("{}.sqlite", b.id)), &part)
        .map_err(|e| AppError::internal(format!("copying the backup: {}", e.kind())))?;
    for ext in ["sqlite-wal", "sqlite-shm"] {
        let _ = std::fs::remove_file(live.with_extension(ext));
    }
    std::fs::rename(&part, &live)
        .map_err(|e| AppError::internal(format!("putting the backup in place: {}", e.kind())))?;
    let opened = core.open_home(&home)?;
    Ok(json!({"restored": b, "before": before, "project": opened}))
}

/// The timer: every minute, each open project edited for 30 minutes since its last copy
/// (or since it was opened) gets one, when nothing is being written.
pub fn start(core: &Arc<AppCore>) {
    let weak: Weak<AppCore> = Arc::downgrade(core);
    // project id → (when its edits were first seen since its last copy, rev then)
    let seen: Mutex<std::collections::BTreeMap<String, (i64, u64)>> = Mutex::default();
    let _ = std::thread::Builder::new()
        .name("gmclip-backups".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let Some(core) = weak.upgrade() else { return };
                tick(&core, &seen, now_ms());
            }
        });
}

/// One pass of the timer (tests call it with their own clock).
pub fn tick(
    core: &AppCore,
    seen: &Mutex<std::collections::BTreeMap<String, (i64, u64)>>,
    now: i64,
) {
    let Ok(mut g) = seen.lock() else { return };
    let open: Vec<String> = core.projects().iter().map(|p| p.info.id.clone()).collect();
    g.retain(|k, _| open.contains(k));
    for id in open {
        let Ok(p) = core.project(&id) else { continue };
        let rev = p.handle.snapshot().doc.rev();
        let last = list(core, &id).ok().and_then(|l| l.first().map(|b| b.rev));
        if last.is_some_and(|r| r >= rev) {
            g.remove(&id);
            continue;
        }
        let since = g.entry(id.clone()).or_insert((now, rev)).0;
        if now - since >= EVERY_MS
            && let Ok(Some(_)) = maybe_backup_open(core, &id, "timer")
        {
            g.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(d: &Path, at: i64) {
        let id = format!("{at}-timer");
        std::fs::write(d.join(format!("{id}.sqlite")), b"x").unwrap();
        let b = Backup {
            id: id.clone(),
            at_ms: at,
            reason: "timer".into(),
            rev: 1,
            bytes: 1,
            name: "p".into(),
        };
        std::fs::write(
            d.join(format!("{id}.json")),
            serde_json::to_vec(&b).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn rotation_keeps_the_newest_five_and_one_a_day_for_a_week() {
        let d = std::env::temp_dir().join(format!("gmclip-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let now = 100 * DAY_MS + 12 * 3_600_000;
        // 3 copies a day for 10 days, every 30 min of the last afternoon
        for day in 0..10 {
            for k in 0..3 {
                fake(&d, now - day * DAY_MS - k * 3_600_000);
            }
        }
        for k in 1..6 {
            fake(&d, now - k * 1_800_000 + 60_000);
        }
        rotate(&d, now);
        let kept = list_dir(&d);
        // the 5 newest, plus the newest of each earlier day within the week
        assert_eq!(kept.len(), 5 + 6, "{kept:#?}");
        assert_eq!(kept[0].at_ms, now);
        let days: std::collections::BTreeSet<i64> =
            kept.iter().map(|b| b.at_ms.div_euclid(DAY_MS)).collect();
        assert_eq!(days.len(), 7, "one per day for the last 7 days");
        assert!(kept.iter().all(|b| now - b.at_ms < 7 * DAY_MS));
        // a sixth rotation in a row changes nothing
        assert!(rotate(&d, now).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
