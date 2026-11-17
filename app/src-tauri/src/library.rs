//! The library index (A2.16; SPEC §4.3 Library): `<app data>/library.sqlite`, a read-only
//! cache of every project home in the library folder — its id, name, exports and moments
//! — so Home can speak for the whole library without opening each project. It is rebuilt
//! from the homes: a home whose `exports.json` changed is re-read, a project whose
//! database changed has its moments read again (a closed one's read-only), a home that
//! went away is dropped, and deleting the file loses nothing (the next Home rebuilds it).
//!
//! [`home_view`] is Home (SPEC §12.6) across the library: running work in any project
//! (exports, transcripts, agent runs, batches), what needs you (exports to make or
//! remake, staged moments with no cut yet in any project, failed jobs, review-link
//! asks), what is done since you last looked, and every project's recent exports.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};

/// The index's layout; an index written by an older one is rebuilt (it is a cache).
const VERSION: i64 = 2;
/// Recent exports Home lists, newest first, across the library.
pub const RECENT: usize = 8;
/// Staged moments with no cut listed per project (the rest are counted).
const STAGED_LISTED: usize = 5;

fn db_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("library.sqlite")
}

pub(crate) fn open(core: &AppCore) -> AppResult<Connection> {
    std::fs::create_dir_all(&core.dirs.app_data)
        .map_err(|e| AppError::internal(format!("app data ({})", e.kind())))?;
    let c = Connection::open(db_path(core))
        .map_err(|e| AppError::internal(format!("library index: {e}")))?;
    let v: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    if v != VERSION {
        c.execute_batch(&format!(
            "DROP TABLE IF EXISTS homes; PRAGMA user_version = {VERSION};"
        ))
        .map_err(|e| AppError::internal(format!("library index: {e}")))?;
    }
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS homes (
            path TEXT PRIMARY KEY, id TEXT NOT NULL, name TEXT NOT NULL,
            stamp INTEGER NOT NULL, exports TEXT NOT NULL,
            mstamp TEXT NOT NULL DEFAULT '', moments TEXT NOT NULL DEFAULT '{}');",
    )
    .map_err(|e| AppError::internal(format!("library index: {e}")))?;
    Ok(c)
}

/// The homes in the library folder (`*.gmclip` directories) plus the open projects'.
fn homes(core: &AppCore) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(&core.dirs.library)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "gmclip") && p.is_dir())
                .map(|p| p.canonicalize().unwrap_or(p))
                .collect()
        })
        .unwrap_or_default();
    for p in core.projects() {
        let h = p.home.canonicalize().unwrap_or_else(|_| p.home.clone());
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out.sort();
    out
}

/// What changes when a home changes: owner.json and exports.json (mtimes and sizes).
fn stamp(home: &Path) -> i64 {
    let one = |f: &str| {
        std::fs::metadata(home.join(f)).map_or(0i64, |m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as i64);
            t ^ (m.len() as i64).rotate_left(17)
        })
    };
    one("owner.json").wrapping_mul(31) ^ one("exports.json")
}

/// The library, from the index (re-reading only changed homes): one row per project.
pub fn library(core: &AppCore) -> AppResult<Vec<Value>> {
    let mut c = open(core)?;
    let paths = homes(core);
    let tx = c
        .transaction()
        .map_err(|e| AppError::internal(format!("library index: {e}")))?;
    let mut rows = Vec::new();
    for home in &paths {
        let key = home.to_string_lossy().into_owned();
        let st = stamp(home);
        let cached: Option<(String, String, i64, String)> = tx
            .query_row(
                "SELECT id, name, stamp, exports FROM homes WHERE path = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let (id, name, exports) = match cached {
            Some((id, name, s, ex)) if s == st => (id, name, ex),
            _ => {
                let Ok(dir) =
                    cap_std::fs::Dir::open_ambient_dir(home, cap_std::ambient_authority())
                else {
                    continue;
                };
                let Ok(info) = gmclip_mcp::project::read_home(&dir) else {
                    continue;
                };
                let ex: Vec<Value> = crate::core::load_exports(home)
                    .into_iter()
                    .map(|e| json!({"id": e.id, "cut": e.cut, "title": e.title, "at_ms": e.at_ms, "seconds": e.seconds}))
                    .collect();
                let ex = serde_json::to_string(&ex).unwrap_or_else(|_| "[]".into());
                tx.execute(
                    "INSERT INTO homes (path, id, name, stamp, exports) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(path) DO UPDATE SET id = ?2, name = ?3, stamp = ?4, exports = ?5",
                    params![key, info.id, info.name, st, ex],
                )
                .map_err(|e| AppError::internal(format!("library index: {e}")))?;
                (info.id, info.name, ex)
            }
        };
        // its moments: read again only when its database changed (an open one: its rev)
        let ms = crate::libsearch::stamp(core, &id);
        let cached: Option<(String, String)> = tx
            .query_row(
                "SELECT mstamp, moments FROM homes WHERE path = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let moments = match cached {
            Some((s, m)) if s == ms => serde_json::from_str(&m).unwrap_or_else(|_| json!({})),
            _ => {
                let m = moments_of(core, &id);
                tx.execute(
                    "UPDATE homes SET mstamp = ?2, moments = ?3 WHERE path = ?1",
                    params![key, ms, m.to_string()],
                )
                .map_err(|e| AppError::internal(format!("library index: {e}")))?;
                m
            }
        };
        let ex: Vec<Value> = serde_json::from_str(&exports).unwrap_or_default();
        let last = ex
            .iter()
            .max_by_key(|e| e["at_ms"].as_i64().unwrap_or(0))
            .cloned();
        rows.push(
            json!({"path": key, "id": id, "name": name, "open": core.is_open(&id),
                         "exports": ex.len(), "last_export": last, "export_list": ex,
                         "moments": moments}),
        );
    }
    // homes that went away
    let keep: std::collections::BTreeSet<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let gone: Vec<String> = {
        let mut st = tx
            .prepare("SELECT path FROM homes")
            .map_err(|e| AppError::internal(format!("library index: {e}")))?;
        let it = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| AppError::internal(format!("library index: {e}")))?;
        it.flatten().filter(|p| !keep.contains(p)).collect()
    };
    for p in gone {
        let _ = tx.execute("DELETE FROM homes WHERE path = ?1", params![p]);
    }
    tx.commit()
        .map_err(|e| AppError::internal(format!("library index: {e}")))?;
    Ok(rows)
}

/// A project's moments as Home needs them: how many, how many staged, and the staged ones
/// no cut was made from yet (the first few, oldest first: Needs you → Grow).
fn moments_of(core: &AppCore, id: &str) -> Value {
    use gmclip_model::entities::{Cut, Moment, MomentState};
    let Some(data) = crate::libsearch::project_data(core, id) else {
        return json!({"total": 0, "staged": 0, "to_grow": 0, "grow": []});
    };
    let doc = &data.doc;
    let grown: std::collections::BTreeSet<_> = doc.iter::<Cut>().filter_map(|c| c.moment).collect();
    let mut all: Vec<&Moment> = doc.iter::<Moment>().collect();
    all.sort_by_key(|m| m.id.0);
    let staged: Vec<&&Moment> = all
        .iter()
        .filter(|m| m.state == MomentState::Staged)
        .collect();
    let grow: Vec<&&&Moment> = staged.iter().filter(|m| !grown.contains(&m.id)).collect();
    json!({
        "total": all.len(),
        "staged": staged.len(),
        "to_grow": grow.len(),
        "grow": grow.iter().take(STAGED_LISTED).map(|m| json!({
            "id": m.id.0.to_string(),
            "quote": m.quote.chars().take(160).collect::<String>(),
            "score": m.score,
        })).collect::<Vec<_>>(),
    })
}

/// Home (SPEC §12.6) across the library (A2.16): running work with its ETA in any
/// project, what needs you (one line, one word each), what is done since you last looked,
/// every project's recent exports, and the library's projects. A closed project's rows
/// carry `closed_home` (Home opens it first).
/// The asks agents wait on (R-PROTO-6, decision 0050), as Home's "Needs you": what the
/// agent asked for, what blocks it (gmclip's words) and whether "Approve and export" can
/// run it. The cut's title is the project's (the agent's words are never shown here).
fn approval_asks(core: &AppCore) -> Vec<Value> {
    let Some(h) = core.agents.holder() else {
        return vec![];
    };
    h.approvals()
        .into_iter()
        .map(|a| {
            let args = &a.effects["export"]["args"];
            let project = args["project"].as_str().map(str::to_owned);
            let cut = a.effects["cut"].as_str().map(str::to_owned);
            let title = project
                .as_deref()
                .zip(cut.as_deref())
                .and_then(|(p, c)| {
                    let id = crate::planner::ulid_id::<gmclip_model::entities::Cut>(c).ok()?;
                    let p = core.project(p).ok()?;
                    let doc = p.handle.snapshot().doc.clone();
                    doc.get(id).map(|c| c.title.clone())
                })
                .unwrap_or_else(|| a.tool.clone());
            let why = a.effects["blocked_by"][0]["message"].as_str().map_or_else(
                || format!("{} asks to run {}", a.identity, a.tool),
                |m| format!("{} asks to export it, and lint blocks it: {m}", a.identity),
            );
            json!({"project": project, "cut": cut, "title": title, "why": why,
                   "action": "approval", "approval": a.id, "identity": a.identity,
                   "run_paused": a.run_paused, "exportable": a.effects.get("export").is_some()})
        })
        .collect()
}

pub fn home_view(core: &std::sync::Arc<AppCore>, jobs: &crate::jobs::Jobs) -> Value {
    let seen = core.seen_ms.lock().map(|g| *g).unwrap_or(0);
    let jobs = jobs.list();
    let library = library(core).unwrap_or_default();
    let name_of = |pid: &str| {
        library
            .iter()
            .find(|r| r["id"] == pid)
            .and_then(|r| r["name"].as_str())
            .map(str::to_owned)
            .or_else(|| core.project(pid).ok().map(|p| p.info.name.clone()))
    };
    let mut running: Vec<Value> = jobs
        .iter()
        .filter(|j| j.state == "running" || j.state == "queued")
        .map(|j| {
            let eta = (j.done > 0).then(|| {
                let el = (now_ms() - j.started_ms) as f64 / 1000.0;
                (el / j.done as f64 * j.frames.saturating_sub(j.done) as f64).round()
            });
            json!({"kind": "export", "project": j.project, "project_name": name_of(&j.project),
                   "title": j.title, "job": j.id, "done": j.done, "frames": j.frames, "eta_s": eta,
                   "state": j.state})
        })
        .collect();
    // transcripts being heard (SPEC §12.6: running work with ETA)
    for p in core.projects() {
        let st = crate::analysis::status(core, &p.info.id);
        for x in st["sources"].as_array().into_iter().flatten() {
            if matches!(
                x["phase"].as_str(),
                Some("transcribing" | "queued" | "analysing")
            ) {
                running.push(json!({"kind": "transcript", "project": p.info.id,
                    "project_name": p.info.name, "title": x["name"],
                    "phase": x["phase"], "done_s": x["done_s"], "total_s": x["total_s"], "eta_s": x["eta_s"],
                    "state": x["phase"]}));
            }
        }
    }
    // agent runs (mining, editing passes) and batches, in any project
    let st = core.agents.status(None);
    for r in st["runs"].as_array().into_iter().flatten() {
        if matches!(
            r["state"].as_str(),
            Some("starting" | "checking" | "running")
        ) {
            let pid = r["project"].as_str().unwrap_or_default();
            running.push(
                json!({"kind": "agent", "project": pid, "project_name": name_of(pid),
                "run": r["id"], "pass": r["pass"], "harness_label": r["harness_label"],
                "started_ms": r["started_ms"], "state": r["state"]}),
            );
        }
    }
    for b in crate::batch::running(core) {
        let done = b
            .cuts
            .iter()
            .filter(|c| !matches!(c.state.as_str(), "queued" | "running"))
            .count();
        running.push(
            json!({"kind": "batch", "project": b.project, "project_name": name_of(&b.project),
            "batch": b.id, "pass": b.pass, "done": done, "total": b.cuts.len(), "state": b.state}),
        );
    }
    let mut needs = Vec::new();
    let mut done = Vec::new();
    for p in core.projects() {
        let pid = p.info.id.clone();
        let doc = p.handle.snapshot().doc.clone();
        let imports: Vec<String> = p
            .imports
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        let exports = p.exports.lock().map(|g| g.clone()).unwrap_or_default();
        for cut in imports {
            let title = gmclip_model::Ulid::parse(&cut)
                .ok()
                .and_then(|u| doc.get(gmclip_model::Id::<gmclip_model::entities::Cut>::new(u)))
                .map(|c| c.title.clone());
            let Some(title) = title else { continue };
            let busy = jobs
                .iter()
                .any(|j| j.cut == cut && (j.state == "running" || j.state == "queued"));
            if busy {
                continue;
            }
            let last = exports
                .iter()
                .filter(|e| e.cut == cut)
                .max_by_key(|e| e.at_ms);
            let sig = core
                .planned(&pid, &cut)
                .ok()
                .and_then(|pl| crate::core::sig_of(&pl.plan));
            match last {
                None => needs.push(json!({"project": pid, "cut": cut, "title": title, "why": "not exported yet", "action": "export"})),
                Some(e) if e.render_sig.is_none() || e.render_sig != sig => needs.push(
                    json!({"project": pid, "cut": cut, "title": title, "why": "export out of date", "action": "re-export"}),
                ),
                _ => {}
            }
        }
        for e in exports.iter().filter(|e| e.at_ms > seen) {
            done.push(json!({"project": pid, "cut": e.cut, "title": e.title, "export": e.id, "at_ms": e.at_ms}));
        }
    }
    for j in jobs.iter().filter(|j| j.state == "failed") {
        needs.push(json!({"project": j.project, "cut": j.cut, "title": j.title, "why": j.error, "action": "fix"}));
    }
    // decision 0015: review-link asks that wait for a click (fix, approve)
    needs.extend(crate::collab::asks(core));
    // decision 0050: an agent's asks (R-PROTO-6), an export it was refused above all
    needs.extend(approval_asks(core));
    // the whole library from its index: staged moments with no cut yet (Grow), exports
    // since you looked of closed projects too, and the newest exports of every project
    let mut recent: Vec<Value> = Vec::new();
    for row in &library {
        let open = row["open"].as_bool() == Some(true);
        let closed_home = (!open).then(|| row["path"].clone());
        let m = &row["moments"];
        for g in m["grow"].as_array().into_iter().flatten() {
            needs.push(
                json!({"project": row["id"], "project_name": row["name"], "cut": null,
                "moment": g["id"], "title": g["quote"], "score": g["score"],
                "why": "staged moment, no cut yet", "action": "grow", "closed_home": closed_home}),
            );
        }
        let more = m["to_grow"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(STAGED_LISTED as u64);
        if more > 0 {
            needs.push(json!({"project": row["id"], "project_name": row["name"], "cut": null,
                "title": row["name"], "why": format!("{more} more staged moment{} with no cut yet", if more == 1 { "" } else { "s" }),
                "action": "moments", "closed_home": closed_home}));
        }
        for e in row["export_list"].as_array().into_iter().flatten() {
            if !open && e["at_ms"].as_i64().unwrap_or(0) > seen {
                done.push(json!({"project": row["id"], "cut": e["cut"], "title": e["title"], "export": e["id"],
                                 "at_ms": e["at_ms"], "closed_home": row["path"]}));
            }
            recent.push(json!({"project": row["id"], "project_name": row["name"], "cut": e["cut"],
                "title": e["title"], "export": e["id"], "at_ms": e["at_ms"], "seconds": e["seconds"],
                "closed_home": closed_home}));
        }
    }
    recent.sort_by_key(|e| std::cmp::Reverse(e["at_ms"].as_i64().unwrap_or(0)));
    recent.truncate(RECENT);
    let library: Vec<Value> = library
        .into_iter()
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                o.remove("export_list");
                if let Some(m) = o.get_mut("moments").and_then(Value::as_object_mut) {
                    m.remove("grow");
                }
            }
            r
        })
        .collect();
    json!({"running": running, "needs_you": needs, "done": done, "recent": recent, "library": library})
}
