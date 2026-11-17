//! Library search (L.01; SPEC §4.3 Library, §12.6): everything said across every project
//! in the library, from the library index (`<app data>/library.sqlite`, A2.16).
//!
//! - **What is indexed**: each project's active transcript per source or sync group (the
//!   `words`/`lines` analysis blobs), read from the live database through a read-only
//!   view (`gmclip_store::ReadOnlyStore`), so closed projects are searched too and an open
//!   one is read beside its holder. A transcript is re-indexed only when its ASR revision
//!   changes; a project is looked at again only when its database changed. Topics
//!   (decision 0011) are kept beside the lines so a hit says which topic it is in.
//! - **Words**: SQLite FTS5 (the bundled SQLite has it) over windows of two consecutive
//!   lines, so a phrase that runs across a line break is still found; the hit is the
//!   exact words matched, word-edged (each word's own start and end).
//! - **Meaning**: [`crate::libmeaning`] (L.02) adds sentence-embedding hits; `hybrid`
//!   merges both lists.
//! - **Topics** (K.04): every topic's title is a result too, across projects and videos,
//!   with the long-form that plays it (its chapter there); opening one lands in that
//!   long-form at the chapter, else in the stream edit at the topic's start.
//! - The index is a cache: deleting it loses nothing (the next search rebuilds it).
//!
//! Transcript text is source-derived: the UI renders it as text, and the MCP surface
//! returns it canonicalised and marked untrusted (R-INJ-1, R-INJ-6).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use gmclip_model::Doc;
use gmclip_model::entities::{MediaRef, ProposalState, Source, SyncGroup, SyncMember, Topic};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ix_projects (
    project TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL, stamp TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ix_media (
    project TEXT NOT NULL, owner TEXT NOT NULL, kind TEXT NOT NULL, revision TEXT NOT NULL,
    name TEXT NOT NULL, words INTEGER NOT NULL, PRIMARY KEY (project, owner));
CREATE TABLE IF NOT EXISTS ix_lines (
    id INTEGER PRIMARY KEY, project TEXT NOT NULL, owner TEXT NOT NULL,
    w0 INTEGER NOT NULL, words TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS ix_lines_po ON ix_lines (project, owner);
CREATE VIRTUAL TABLE IF NOT EXISTS ix_fts USING fts5 (text, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS ix_topics (
    project TEXT NOT NULL, owner TEXT NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
    title TEXT NOT NULL, state TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS ix_topics_p ON ix_topics (project, owner);
CREATE TABLE IF NOT EXISTS ix_topic_rows (
    project TEXT NOT NULL, owner TEXT NOT NULL, topic TEXT NOT NULL, title TEXT NOT NULL,
    state TEXT NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
    long TEXT NOT NULL, long_title TEXT NOT NULL, at_ms INTEGER NOT NULL,
    kind TEXT NOT NULL, mname TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS ix_topic_rows_p ON ix_topic_rows (project);
";

fn ierr(e: impl std::fmt::Display) -> AppError {
    AppError::internal(format!("library index: {e}"))
}

/// The library index with the search tables.
pub(crate) fn open(core: &AppCore) -> AppResult<Connection> {
    let c = crate::library::open(core)?;
    // an index made before topics were results (K.04): look at every project again once
    // (transcripts whose revision is unchanged are not re-read)
    let had_topic_rows: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'ix_topic_rows'",
            [],
            |r| r.get(0),
        )
        .map_err(ierr)?;
    c.execute_batch(SCHEMA).map_err(ierr)?;
    if had_topic_rows == 0 {
        c.execute("UPDATE ix_projects SET stamp = ''", [])
            .map_err(ierr)?;
    }
    sweep_orphans(&c)?;
    // the index is a cache: a lost write costs a re-read, never data
    let _ = c.pragma_update(None, "journal_mode", "WAL");
    let _ = c.pragma_update(None, "synchronous", "OFF");
    Ok(c)
}

/// Full-text rows and lines out of step (decision 0039: AJ's index had 420 full-text rows
/// past the last line, so the next line's id met one and every refresh failed with
/// "constraint failed"): the transcript tables are emptied and every project re-read
/// (the index is a cache).
fn sweep_orphans(c: &Connection) -> AppResult<()> {
    let (fts, lines): (i64, i64) = c
        .query_row(
            "SELECT (SELECT count(*) FROM ix_fts), (SELECT count(*) FROM ix_lines)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(ierr)?;
    if fts != lines {
        c.execute_batch(
            "BEGIN;
             DELETE FROM ix_fts;
             DELETE FROM ix_lines;
             DELETE FROM ix_media;
             UPDATE ix_projects SET stamp = '';
             COMMIT;",
        )
        .map_err(ierr)?;
    }
    Ok(())
}

/// Reads one analysis blob `(owner, kind, revision)`.
type BlobGetter = Box<dyn Fn(&str, &str, &str) -> Option<Vec<u8>>>;

/// A project's document and analysis blobs, from the open project or its database.
pub(crate) struct ProjectData {
    pub doc: Doc,
    keys: Vec<(String, String, String)>,
    get: BlobGetter,
}

impl ProjectData {
    pub(crate) fn analysis(&self, owner: &str, kind: &str, rev: &str) -> Option<Vec<u8>> {
        (self.get)(owner, kind, rev)
    }

    /// Every owner with a transcript and the revision the project uses for it (its
    /// `active_asr`, else the newest stored).
    pub(crate) fn transcripts(&self) -> Vec<(String, String)> {
        let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (o, k, r) in &self.keys {
            if k == "words" {
                by.entry(o.clone()).or_default().push(r.clone());
            }
        }
        let active = self
            .doc
            .iter::<gmclip_model::entities::Project>()
            .next()
            .map(|p| p.active_asr.clone())
            .unwrap_or_default();
        by.into_iter()
            .filter_map(|(o, revs)| {
                let r = active
                    .get(&o)
                    .filter(|a| revs.contains(a))
                    .cloned()
                    .or_else(|| revs.iter().max().cloned())?;
                Some((o, r))
            })
            .collect()
    }
}

/// Reads a project: the open one's snapshot, else its database read-only.
pub(crate) fn project_data(core: &AppCore, id: &str) -> Option<ProjectData> {
    if let Ok(p) = core.project(id) {
        let doc = p.handle.snapshot().doc.clone();
        let keys = p.handle.analysis_keys();
        let p2 = p.clone();
        return Some(ProjectData {
            doc,
            keys,
            get: Box::new(move |o, k, r| p2.handle.analysis(o, k, r)),
        });
    }
    let db = gmclip_mcp::project::db_path(&core.dirs.app_data, id).ok()?;
    if !db.is_file() {
        return None;
    }
    let ro = gmclip_core::ReadOnlyStore::open(&db).ok()?;
    let doc = ro.doc().ok()?;
    let keys = ro.analysis_keys().ok()?;
    let ro = std::sync::Mutex::new(ro);
    Some(ProjectData {
        doc,
        keys,
        get: Box::new(move |o, k, r| ro.lock().ok()?.analysis(o, k, r).ok().flatten()),
    })
}

/// What changes when a project's searchable state may have changed.
pub(crate) fn stamp(core: &AppCore, id: &str) -> String {
    if let Ok(p) = core.project(id) {
        return format!("open:{}", p.handle.snapshot().doc.rev());
    }
    let Ok(db) = gmclip_mcp::project::db_path(&core.dirs.app_data, id) else {
        return "none".into();
    };
    let one = |p: &Path| {
        std::fs::metadata(p).map_or((0, 0), |m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (t, m.len())
        })
    };
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    format!("db:{:?}:{:?}", one(&db), one(&wal))
}

/// `(kind, name)` of a source or sync group: the file's name (a group: its picture's).
fn media_name(doc: &Doc, owner: &str) -> (&'static str, String) {
    let Ok(u) = gmclip_model::Ulid::parse(owner) else {
        return ("source", "a recording".into());
    };
    let file = |s: &Source| {
        Path::new(&s.path).file_stem().map_or_else(
            || "a recording".to_owned(),
            |x| x.to_string_lossy().into_owned(),
        )
    };
    if let Some(s) = doc.get(gmclip_model::Id::<Source>::new(u)) {
        return ("source", file(s));
    }
    if let Some(g) = doc.get(gmclip_model::Id::<SyncGroup>::new(u)) {
        let pic = g
            .picture
            .and_then(|m| doc.get::<SyncMember>(m))
            .and_then(|m| doc.get(m.source));
        return ("group", pic.map_or_else(|| "a recording".to_owned(), file));
    }
    ("source", "a recording".into())
}

pub(crate) fn media_json(kind: &str, owner: &str) -> Value {
    if kind == "group" {
        json!({"group": owner})
    } else {
        json!({"source": owner})
    }
}

/// Topic spans per owner (milliseconds), rejected ones left out.
fn topics(doc: &Doc) -> Vec<(String, i64, i64, String, &'static str)> {
    doc.iter::<Topic>()
        .filter(|t| t.state != ProposalState::Rejected)
        .filter_map(|t| {
            let a = gmclip_model::clock::anchor_src(doc, &t.span.start)?;
            let b = gmclip_model::clock::anchor_src(doc, &t.span.end)?;
            let owner = match t.media {
                MediaRef::Source(s) => s.0.to_string(),
                MediaRef::Group(g) => g.0.to_string(),
            };
            let state = if t.state == ProposalState::Accepted {
                "accepted"
            } else {
                "proposed"
            };
            Some((
                owner,
                a.0 * 1000 / FLICKS,
                b.0 * 1000 / FLICKS,
                t.title.clone(),
                state,
            ))
        })
        .collect()
}

/// One topic as a search result (K.04): where it is, and the long-form that plays it.
struct TopicRow {
    owner: String,
    topic: String,
    title: String,
    state: &'static str,
    start_ms: i64,
    end_ms: i64,
    /// The first long-form whose chapters include it ("" when none does), and the
    /// chapter's start there.
    long: String,
    long_title: String,
    at_ms: i64,
}

fn topic_rows(doc: &Doc) -> Vec<TopicRow> {
    use gmclip_model::entities::{Cut, CutKind};
    let mut longs: Vec<&Cut> = doc
        .iter::<Cut>()
        .filter(|c| c.kind == CutKind::Long)
        .collect();
    longs.sort_by_key(|c| c.id.0);
    doc.iter::<Topic>()
        .filter(|t| t.state != ProposalState::Rejected)
        .filter_map(|t| {
            let a = gmclip_model::clock::anchor_src(doc, &t.span.start)?;
            let b = gmclip_model::clock::anchor_src(doc, &t.span.end)?;
            let owner = match t.media {
                MediaRef::Source(s) => s.0.to_string(),
                MediaRef::Group(g) => g.0.to_string(),
            };
            let tid = t.id.0.to_string();
            let long = longs.iter().find_map(|c| {
                c.lineage.topics.contains(&t.id).then(|| {
                    let at = gmclip_core::commands::longform::chapters(doc, c.id)
                        .into_iter()
                        .find(|ch| ch.topic == tid)
                        .map_or(0, |ch| ch.at_ms);
                    (c.id.0.to_string(), c.title.clone(), at)
                })
            });
            let (long, long_title, at_ms) = long.unwrap_or_default();
            Some(TopicRow {
                owner,
                topic: tid,
                title: t.title.clone(),
                state: if t.state == ProposalState::Accepted {
                    "accepted"
                } else {
                    "proposed"
                },
                start_ms: a.0 * 1000 / FLICKS,
                end_ms: b.0 * 1000 / FLICKS,
                long,
                long_title,
                at_ms,
            })
        })
        .collect()
}

/// Topics whose titles match the query (K.04, library search by topic): every query word
/// starts a word of the title (accepted topics first, then the most words matched, then
/// the shortest title); with `meaning`, titles close in meaning too (MiniLM, cosine ≥ 0.5).
pub fn topic_hits(
    core: &Arc<AppCore>,
    c: &Connection,
    query: &str,
    project: Option<&str>,
    meaning: bool,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let q = tokens(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut st = c
        .prepare(
            "SELECT t.project, t.owner, t.topic, t.title, t.state, t.start_ms, t.end_ms, t.long, t.long_title, t.at_ms,
                    p.name, p.path, t.kind, t.mname
             FROM ix_topic_rows t JOIN ix_projects p ON p.project = t.project
             WHERE ?1 IS NULL OR t.project = ?1",
        )
        .map_err(ierr)?;
    type Row = (
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
    );
    let rows: Vec<Row> = st
        .query_map(params![project], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
            ))
        })
        .map_err(ierr)?
        .flatten()
        .collect();
    // (accepted, words matched, meaning, -title length, index)
    let mut scored: Vec<(bool, usize, f32, i64, usize, &'static str)> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let tt = tokens(&r.3);
        let n = q
            .iter()
            .filter(|w| tt.iter().any(|t| t.starts_with(w.as_str())))
            .count();
        if n == q.len() {
            scored.push((r.4 == "accepted", n, 1.0, -(r.3.len() as i64), i, "words"));
        }
    }
    if meaning && crate::libmeaning::model_present() && !rows.is_empty() {
        let m = crate::libmeaning::of(core);
        let titles: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
        if let (Ok(qv), Ok(tv)) = (m.embed(&[query]), m.embed(&titles)) {
            let qv = qv.into_iter().next().unwrap_or_default();
            for (i, v) in tv.iter().enumerate() {
                let cos = gmclip_ml::embed::cosine(&qv, v);
                if cos >= 0.5 && !scored.iter().any(|s| s.4 == i) {
                    scored.push((
                        rows[i].4 == "accepted",
                        0,
                        cos,
                        -(rows[i].3.len() as i64),
                        i,
                        "meaning",
                    ));
                }
            }
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.total_cmp(&a.2))
            .then(b.3.cmp(&a.3))
    });
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(_, _, _, _, i, by)| {
            let r = &rows[i];
            json!({
                "project": r.0, "project_name": r.10, "home": r.11, "open": core.is_open(&r.0),
                "media": media_json(&r.12, &r.1), "media_name": r.13,
                "topic": r.2, "title": r.3, "state": r.4,
                "start_s": r.5 as f64 / 1000.0, "end_s": r.6 as f64 / 1000.0,
                "long": (!r.7.is_empty()).then(|| json!({"cut": r.7, "title": r.8, "at_s": r.9 as f64 / 1000.0})),
                "by": by,
            })
        })
        .collect())
}

/// `library_topic_open{project, home?, topic}`: the long-form that plays the topic, at its
/// chapter; else the stream edit, at the topic's start.
pub fn open_topic(
    core: &Arc<AppCore>,
    pid: &str,
    home: Option<&str>,
    topic: &str,
) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, CutKind};
    ensure_open(core, pid, home)?;
    let p = core.project(pid)?;
    let doc = p.handle.snapshot().doc.clone();
    let tid: gmclip_model::Id<Topic> = crate::planner::ulid_id(topic)?;
    let t = doc
        .get(tid)
        .ok_or_else(|| AppError::not_found("that topic is gone"))?;
    let mut longs: Vec<&Cut> = doc
        .iter::<Cut>()
        .filter(|c| c.kind == CutKind::Long && c.lineage.topics.contains(&tid))
        .collect();
    longs.sort_by_key(|c| c.id.0);
    if let Some(c) = longs.first() {
        let cut = c.id.0.to_string();
        let at = gmclip_core::commands::longform::chapters(&doc, c.id)
            .into_iter()
            .find(|ch| ch.topic == topic)
            .map_or(0, |ch| ch.at_ms);
        let fpf = core
            .planned(pid, &cut)
            .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
        let frame = u64::try_from(at * (FLICKS / 1000) / fpf).unwrap_or(0);
        return Ok(
            json!({"project": pid, "cut": cut, "frame": frame, "in": "long", "title": c.title}),
        );
    }
    let start = gmclip_model::clock::anchor_src(&doc, &t.span.start)
        .ok_or_else(|| AppError::not_found("that topic's words are gone"))?;
    let (cut, frame) =
        cut_showing(core, pid, t.media, start.0).unwrap_or((t.cut.0.to_string(), None));
    Ok(json!({"project": pid, "cut": cut, "frame": frame, "in": "stream"}))
}

/// Lowercase alphanumeric tokens (the FTS tokenizer's split, near enough to locate).
pub(crate) fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// One indexed window: words `[text, start_ms, end_ms]` from ordinal `w0`.
type WinWord = (String, i64, i64);

/// What [`refresh`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Refreshed {
    pub projects: usize,
    pub looked_at: usize,
    pub transcripts_indexed: usize,
    pub lines_indexed: usize,
    pub ms: u64,
}

/// Brings the index up to date with every home in the library (and the open projects).
pub fn refresh(core: &AppCore) -> AppResult<Refreshed> {
    let t0 = std::time::Instant::now();
    let rows = crate::library::library(core)?;
    let mut c = open(core)?;
    let mut out = Refreshed {
        projects: rows.len(),
        ..Refreshed::default()
    };
    let mut seen = BTreeSet::new();
    for row in &rows {
        let (Some(id), Some(path)) = (row["id"].as_str(), row["path"].as_str()) else {
            continue;
        };
        let name = row["name"].as_str().unwrap_or("");
        seen.insert(id.to_owned());
        let st = stamp(core, id);
        let had: Option<String> = c
            .query_row(
                "SELECT stamp FROM ix_projects WHERE project = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(ierr)?;
        if had.as_deref() == Some(st.as_str()) {
            continue;
        }
        out.looked_at += 1;
        let Some(data) = project_data(core, id) else {
            continue;
        };
        let tx = c.transaction().map_err(ierr)?;
        let wanted = data.transcripts();
        let have: BTreeMap<String, String> = {
            let mut s = tx
                .prepare("SELECT owner, revision FROM ix_media WHERE project = ?1")
                .map_err(ierr)?;
            s.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(ierr)?
                .flatten()
                .collect()
        };
        for (owner, _) in have
            .iter()
            .filter(|(o, _)| !wanted.iter().any(|w| &w.0 == *o))
        {
            drop_owner(&tx, id, owner)?;
        }
        for (owner, rev) in &wanted {
            let (kind, mname) = media_name(&data.doc, owner);
            if have.get(owner) == Some(rev) {
                tx.execute(
                    "UPDATE ix_media SET name = ?3, kind = ?4 WHERE project = ?1 AND owner = ?2",
                    params![id, owner, mname, kind],
                )
                .map_err(ierr)?;
                continue;
            }
            drop_owner(&tx, id, owner)?;
            let Some(body) = data.analysis(owner, "words", rev) else {
                continue;
            };
            let Some((_, words)) = gmclip_core::analysis::decode_words(&body) else {
                continue;
            };
            let lines = data
                .analysis(owner, "lines", rev)
                .and_then(|b| gmclip_core::analysis::decode_lines(&b))
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| gmclip_core::analysis::make_lines(&words));
            let n = index_lines(&tx, id, owner, &words, &lines)?;
            tx.execute(
                "INSERT OR REPLACE INTO ix_media (project, owner, kind, revision, name, words)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, owner, kind, rev, mname, words.len() as i64],
            )
            .map_err(ierr)?;
            out.transcripts_indexed += 1;
            out.lines_indexed += n;
        }
        tx.execute("DELETE FROM ix_topics WHERE project = ?1", params![id])
            .map_err(ierr)?;
        for (owner, a, b, title, state) in topics(&data.doc) {
            tx.execute(
                "INSERT INTO ix_topics (project, owner, start_ms, end_ms, title, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, owner, a, b, title, state],
            )
            .map_err(ierr)?;
        }
        tx.execute("DELETE FROM ix_topic_rows WHERE project = ?1", params![id])
            .map_err(ierr)?;
        for r in topic_rows(&data.doc) {
            tx.execute(
                "INSERT INTO ix_topic_rows (project, owner, topic, title, state, start_ms, end_ms, long, long_title, at_ms, kind, mname)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![id, r.owner, r.topic, r.title, r.state, r.start_ms, r.end_ms, r.long, r.long_title, r.at_ms,
                        media_name(&data.doc, &r.owner).0, media_name(&data.doc, &r.owner).1],
            )
            .map_err(ierr)?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO ix_projects (project, path, name, stamp) VALUES (?1, ?2, ?3, ?4)",
            params![id, path, name, st],
        )
        .map_err(ierr)?;
        tx.commit().map_err(ierr)?;
    }
    // projects that left the library
    let gone: Vec<String> = {
        let mut s = c.prepare("SELECT project FROM ix_projects").map_err(ierr)?;
        s.query_map([], |r| r.get::<_, String>(0))
            .map_err(ierr)?
            .flatten()
            .filter(|p| !seen.contains(p))
            .collect()
    };
    for p in gone {
        let tx = c.transaction().map_err(ierr)?;
        let owners: Vec<String> = {
            let mut s = tx
                .prepare("SELECT owner FROM ix_media WHERE project = ?1")
                .map_err(ierr)?;
            s.query_map(params![p], |r| r.get(0))
                .map_err(ierr)?
                .flatten()
                .collect()
        };
        for o in owners {
            drop_owner(&tx, &p, &o)?;
        }
        tx.execute("DELETE FROM ix_topics WHERE project = ?1", params![p])
            .map_err(ierr)?;
        tx.execute("DELETE FROM ix_topic_rows WHERE project = ?1", params![p])
            .map_err(ierr)?;
        tx.execute("DELETE FROM ix_projects WHERE project = ?1", params![p])
            .map_err(ierr)?;
        tx.commit().map_err(ierr)?;
    }
    out.ms = t0.elapsed().as_millis() as u64;
    Ok(out)
}

fn drop_owner(tx: &rusqlite::Transaction<'_>, project: &str, owner: &str) -> AppResult<()> {
    tx.execute(
        "DELETE FROM ix_fts WHERE rowid IN (SELECT id FROM ix_lines WHERE project = ?1 AND owner = ?2)",
        params![project, owner],
    )
    .map_err(ierr)?;
    tx.execute(
        "DELETE FROM ix_lines WHERE project = ?1 AND owner = ?2",
        params![project, owner],
    )
    .map_err(ierr)?;
    tx.execute(
        "DELETE FROM ix_media WHERE project = ?1 AND owner = ?2",
        params![project, owner],
    )
    .map_err(ierr)?;
    Ok(())
}

/// One window per line: that line's words and the next line's (a phrase across a line
/// break is found; the duplicate hit it can make is dropped at search time).
fn index_lines(
    tx: &rusqlite::Transaction<'_>,
    project: &str,
    owner: &str,
    words: &[gmclip_core::analysis::Word],
    lines: &[gmclip_core::analysis::Line],
) -> AppResult<usize> {
    let mut ins_line = tx
        .prepare_cached("INSERT INTO ix_lines (project, owner, w0, words) VALUES (?1, ?2, ?3, ?4)")
        .map_err(ierr)?;
    let mut ins_fts = tx
        .prepare_cached("INSERT OR REPLACE INTO ix_fts (rowid, text) VALUES (?1, ?2)")
        .map_err(ierr)?;
    let mut n = 0;
    for (i, l) in lines.iter().enumerate() {
        let w0 = l.words[0] as usize;
        let w1 = lines
            .get(i + 1)
            .map_or(l.words[1], |x| x.words[1])
            .min(words.len() as u32) as usize;
        if w0 >= w1 {
            continue;
        }
        let ws: Vec<WinWord> = words[w0..w1]
            .iter()
            .map(|w| {
                (
                    w.text.clone(),
                    w.start * 1000 / FLICKS,
                    w.end * 1000 / FLICKS,
                )
            })
            .collect();
        let text = ws
            .iter()
            .map(|w| w.0.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let body = serde_json::to_string(&ws).map_err(ierr)?;
        ins_line
            .execute(params![project, owner, w0 as i64, body])
            .map_err(ierr)?;
        let rowid = tx.last_insert_rowid();
        ins_fts.execute(params![rowid, text]).map_err(ierr)?;
        n += 1;
    }
    Ok(n)
}

/// Where the query's tokens are in a window: `(first word, last word)` (window indexes).
/// `phrase`: consecutive; else the span over each token's first occurrence.
fn locate(ws: &[WinWord], q: &[String], phrase: bool) -> Option<(usize, usize)> {
    let sub: Vec<(String, usize)> = ws
        .iter()
        .enumerate()
        .flat_map(|(i, w)| tokens(&w.0).into_iter().map(move |t| (t, i)))
        .collect();
    if q.is_empty() || sub.is_empty() {
        return None;
    }
    if phrase {
        let k = sub
            .windows(q.len())
            .position(|win| win.iter().zip(q).all(|(a, b)| &a.0 == b))?;
        return Some((sub[k].1, sub[k + q.len() - 1].1));
    }
    let mut lo = usize::MAX;
    let mut hi = 0;
    for t in q {
        let i = sub.iter().find(|s| &s.0 == t)?.1;
        lo = lo.min(i);
        hi = hi.max(i);
    }
    Some((lo, hi))
}

/// How to search.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Words,
    Meaning,
    Hybrid,
}

impl Mode {
    pub fn parse(s: Option<&str>) -> AppResult<Mode> {
        Ok(match s.unwrap_or("hybrid") {
            "words" => Mode::Words,
            "meaning" => Mode::Meaning,
            "hybrid" | "" => Mode::Hybrid,
            _ => return Err(AppError::invalid("mode is words, meaning or hybrid")),
        })
    }
}

/// One hit, before it is shaped for the UI or an agent.
#[derive(Clone, Debug)]
pub struct Hit {
    pub project: String,
    pub owner: String,
    /// Word ordinal (in `revision`) of the first and last word.
    pub w0: usize,
    pub w1: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    /// The hit's words, and a few words either side.
    pub text: String,
    pub before: String,
    pub after: String,
    /// Higher is better, comparable within one kind.
    pub score: f64,
    pub by: &'static str,
}

/// Exact word/phrase hits (best first): a phrase when it has several words, else every
/// word near each other (within 12 tokens) when the phrase is never said.
pub fn word_hits(
    c: &Connection,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> AppResult<Vec<Hit>> {
    let q = tokens(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let quoted: Vec<String> = q.iter().map(|t| format!("\"{t}\"")).collect();
    let phrase = format!("\"{}\"", q.join(" "));
    let near = format!("NEAR({}, 12)", quoted.join(" "));
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (fts, is_phrase) in [(phrase, true), (near, false)] {
        if !out.is_empty() || (!is_phrase && q.len() == 1) {
            break;
        }
        let sql = "SELECT l.project, l.owner, l.w0, l.words, bm25(ix_fts) AS r
                   FROM ix_fts JOIN ix_lines l ON l.id = ix_fts.rowid
                   WHERE ix_fts MATCH ?1 AND (?2 IS NULL OR l.project = ?2)
                   ORDER BY r LIMIT ?3";
        let mut st = c.prepare_cached(sql).map_err(ierr)?;
        let rows = st
            .query_map(params![fts, project, (limit * 4) as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })
            .map_err(ierr)?;
        for row in rows.flatten() {
            let (p, o, w0, body, rank) = row;
            let Ok(ws) = serde_json::from_str::<Vec<WinWord>>(&body) else {
                continue;
            };
            let Some((a, b)) = locate(&ws, &q, is_phrase) else {
                continue;
            };
            let (wa, wb) = (w0 as usize + a, w0 as usize + b);
            if !seen.insert((p.clone(), o.clone(), wa)) {
                continue;
            }
            let join = |r: &[WinWord]| r.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ");
            out.push(Hit {
                project: p,
                owner: o,
                w0: wa,
                w1: wb,
                start_ms: ws[a].1,
                end_ms: ws[b].2,
                text: join(&ws[a..=b]),
                before: join(&ws[a.saturating_sub(8)..a]),
                after: join(&ws[(b + 1).min(ws.len())..(b + 9).min(ws.len())]),
                score: -rank,
                by: "words",
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Shapes hits for the UI: project and media names, open state, topic.
pub fn shape(c: &Connection, core: &AppCore, hits: &[Hit]) -> AppResult<Vec<Value>> {
    let mut out = Vec::new();
    for h in hits {
        let (pname, path): (String, String) = c
            .query_row(
                "SELECT name, path FROM ix_projects WHERE project = ?1",
                params![h.project],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(ierr)?
            .unwrap_or_default();
        let (kind, mname, rev): (String, String, String) = c
            .query_row(
                "SELECT kind, name, revision FROM ix_media WHERE project = ?1 AND owner = ?2",
                params![h.project, h.owner],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(ierr)?
            .unwrap_or_else(|| ("source".into(), String::new(), String::new()));
        let mid = (h.start_ms + h.end_ms) / 2;
        let topic: Option<(String, String)> = c
            .query_row(
                "SELECT title, state FROM ix_topics WHERE project = ?1 AND owner = ?2
                 AND start_ms <= ?3 AND ?3 < end_ms ORDER BY state = 'accepted' DESC LIMIT 1",
                params![h.project, h.owner, mid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(ierr)?;
        out.push(json!({
            "project": h.project, "project_name": pname, "home": path,
            "open": core.is_open(&h.project),
            "media": media_json(&kind, &h.owner), "media_name": mname, "revision": rev,
            "w0": h.w0, "w1": h.w1,
            "start_s": h.start_ms as f64 / 1000.0, "end_s": h.end_ms as f64 / 1000.0,
            "text": h.text, "before": h.before, "after": h.after,
            "topic": topic.map(|(t, s)| json!({"title": t, "state": s})),
            "score": (h.score * 1000.0).round() / 1000.0, "by": h.by,
        }));
    }
    Ok(out)
}

/// Searches the library (refreshing the index first).
pub fn search(
    core: &Arc<AppCore>,
    query: &str,
    mode: Mode,
    limit: usize,
    project: Option<&str>,
) -> AppResult<Value> {
    let q = query.trim();
    if q.chars().count() > 300 {
        return Err(AppError::invalid("a search is at most 300 characters"));
    }
    let limit = limit.clamp(1, 50);
    let refreshed = refresh(core)?;
    let c = open(core)?;
    let t0 = std::time::Instant::now();
    let words = if mode == Mode::Meaning {
        Vec::new()
    } else {
        word_hits(&c, q, project, limit)?
    };
    let (hits, meaning) = match mode {
        Mode::Words => (words, Value::Null),
        _ => {
            let (m, state) = crate::libmeaning::hits(core, &c, q, project, limit)?;
            (crate::libmeaning::merge(words, m, mode, limit), state)
        }
    };
    let topics = topic_hits(core, &c, q, project, mode != Mode::Words, 10)?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(json!({
        "query": q, "hits": shape(&c, core, &hits)?, "topics": topics, "meaning": meaning,
        "index": refreshed, "query_ms": (ms * 10.0).round() / 10.0,
    }))
}

/// The library index as the agent surface reads it (`library_search`): every text field
/// canonicalised and marked untrusted, and each hit's `moment_mark` args.
#[derive(Debug)]
pub struct Host(pub Weak<AppCore>);

impl Host {
    fn core(&self) -> Result<Arc<AppCore>, gmclip_proto::CmdError> {
        self.0.upgrade().ok_or(gmclip_proto::CmdError::Internal {
            trace_id: "app-gone".into(),
        })
    }
}

fn cmd_err(e: AppError) -> gmclip_proto::CmdError {
    gmclip_proto::CmdError::invalid("/args", &e.message)
}

impl gmclip_mcp::holder::LibraryHost for Host {
    fn search(&self, args: &Value) -> Result<Value, gmclip_proto::CmdError> {
        use gmclip_mcp::canon::{Kind, block, field};
        let core = self.core()?;
        let q = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| gmclip_proto::CmdError::invalid("/args/query", "required"))?;
        let mode = Mode::parse(args.get("mode").and_then(Value::as_str)).map_err(cmd_err)?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        let project = args.get("project").and_then(Value::as_str);
        let v = search(&core, q, mode, limit, project).map_err(cmd_err)?;
        let mut display = Vec::new();
        let hits: Vec<Value> = v["hits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|h| {
                let text = h["text"].as_str().unwrap_or("");
                display.push(format!(
                    "- {} · {} [{:.2}–{:.2} s]{} {}",
                    block(h["project_name"].as_str().unwrap_or(""), Kind::Name),
                    block(h["media_name"].as_str().unwrap_or(""), Kind::Name),
                    h["start_s"].as_f64().unwrap_or(0.0),
                    h["end_s"].as_f64().unwrap_or(0.0),
                    h["topic"]["title"]
                        .as_str()
                        .map(|t| format!(" topic {}", block(t, Kind::Text)))
                        .unwrap_or_default(),
                    block(text, Kind::Text)
                ));
                json!({
                    "project": h["project"], "home": h["home"], "open": h["open"],
                    "project_name": field(h["project_name"].as_str().unwrap_or(""), Kind::Name),
                    "media": h["media"], "media_name": field(h["media_name"].as_str().unwrap_or(""), Kind::Name),
                    "start_s": h["start_s"], "end_s": h["end_s"], "by": h["by"], "score": h["score"],
                    "text": field(text, Kind::Text),
                    "before": field(h["before"].as_str().unwrap_or(""), Kind::Text),
                    "after": field(h["after"].as_str().unwrap_or(""), Kind::Text),
                    "topic": h["topic"]["title"].as_str().map(|t| json!({"title": field(t, Kind::Text), "state": h["topic"]["state"]})),
                    "moment_mark": {"media": h["media"], "start_s": h["start_s"], "end_s": h["end_s"],
                                    "snap": "word", "from": "search"},
                })
            })
            .collect();
        let n = hits.len();
        let topics: Vec<Value> = v["topics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                let title = t["title"].as_str().unwrap_or("");
                display.push(format!(
                    "- topic {} · {} · {}{}",
                    block(title, Kind::Text),
                    block(t["project_name"].as_str().unwrap_or(""), Kind::Name),
                    block(t["media_name"].as_str().unwrap_or(""), Kind::Name),
                    t["long"]["title"]
                        .as_str()
                        .map(|l| format!(" in long-form {}", block(l, Kind::Text)))
                        .unwrap_or_default(),
                ));
                json!({
                    "project": t["project"], "home": t["home"], "topic": t["topic"], "state": t["state"],
                    "title": field(title, Kind::Text),
                    "project_name": field(t["project_name"].as_str().unwrap_or(""), Kind::Name),
                    "media": t["media"], "media_name": field(t["media_name"].as_str().unwrap_or(""), Kind::Name),
                    "start_s": t["start_s"], "end_s": t["end_s"],
                    "long": t["long"]["cut"].as_str().map(|c| json!({"cut": c, "at_s": t["long"]["at_s"],
                        "title": field(t["long"]["title"].as_str().unwrap_or(""), Kind::Text)})),
                })
            })
            .collect();
        Ok(json!({
            "hits": hits, "topics": topics, "meaning": v["meaning"],
            "display": format!("{n} hit(s) for {}\n{}", block(q, Kind::Text), display.join("\n")),
        }))
    }

    fn chat_spikes(&self, project: &str, args: &Value) -> Result<Value, gmclip_proto::CmdError> {
        let core = self.core()?;
        crate::chat::spikes_for_agent(&core, project, args).map_err(cmd_err)
    }
}

// ---- the UI's commands -----------------------------------------------------------------

/// Opens `project` from its home when it is not open (a hit in a closed project).
pub(crate) fn ensure_open(core: &Arc<AppCore>, project: &str, home: Option<&str>) -> AppResult<()> {
    if core.is_open(project) {
        return Ok(());
    }
    let home = home.ok_or_else(|| AppError::invalid("that project is closed: `home` is needed"))?;
    let v = core.open_home(Path::new(home))?;
    if v["id"].as_str() != Some(project) {
        return Err(AppError::invalid("that folder holds another project"));
    }
    Ok(())
}

fn media_arg(a: &Value) -> AppResult<MediaRef> {
    serde_json::from_value(a.get("media").cloned().unwrap_or(Value::Null))
        .map_err(|_| AppError::invalid("`media` is {\"source\": id} or {\"group\": id}"))
}

fn secs_arg(a: &Value, k: &str) -> AppResult<f64> {
    a.get(k)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is seconds")))
}

/// The cut that shows source time `t` of `media`: the stream edit first, else any cut
/// with a part over it; and the output frame there.
fn cut_showing(
    core: &AppCore,
    project: &str,
    media: MediaRef,
    t: i64,
) -> Option<(String, Option<u64>)> {
    use gmclip_model::entities::{Cut, CutKind, Part};
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let mut cuts: Vec<&Cut> = doc.iter::<Cut>().collect();
    cuts.sort_by_key(|c| (c.kind != CutKind::Stream, c.id.0));
    let mut first_on_media = None;
    for c in cuts {
        for sp in gmclip_model::clock::cut_layout(&doc, c.id) {
            let Some(pt) = doc.get::<Part>(sp.part) else {
                continue;
            };
            if pt.media != media {
                continue;
            }
            first_on_media.get_or_insert_with(|| c.id.0.to_string());
            if pt.src_in.0 <= t && t < pt.src_out.0 {
                let cut = c.id.0.to_string();
                let fpf = core
                    .planned(project, &cut)
                    .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
                let f = (sp.start.0 + (t - pt.src_in.0)) / fpf;
                return Some((cut, Some(u64::try_from(f).unwrap_or(0))));
            }
        }
    }
    first_on_media.map(|c| (c, None))
}

/// "＋ Make moment" from a hit (`{project, home, media, start_s, end_s}`): its project
/// opened when closed, a word-edged moment by the human (origin `manual`), `note` on
/// its card.
pub fn make_moment(core: &Arc<AppCore>, hit: &Value, note: &str) -> AppResult<Value> {
    let pid = hit
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("`project` is required"))?;
    ensure_open(core, pid, hit.get("home").and_then(Value::as_str))?;
    let media = media_arg(hit)?;
    let r = core.call(
        pid,
        "moment_mark",
        json!({"media": media, "start_s": secs_arg(hit, "start_s")?, "end_s": secs_arg(hit, "end_s")?,
               "snap": "word", "from": "search",
               "note": note.chars().take(120).collect::<String>()}),
    )?;
    Ok(json!({"project": pid, "moment": r["output"]}))
}

/// `library_search`, `library_play`, `library_open`, `library_topic_open`, `library_moment`.
pub fn ipc(ctx: &crate::ipc::Ctx, command: &str, a: &Value) -> AppResult<Value> {
    let core = &ctx.core;
    let project = || {
        a.get("project")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid("`project` is required"))
    };
    let home = a.get("home").and_then(Value::as_str);
    match command {
        "library_search" => {
            let q = a.get("query").and_then(Value::as_str).unwrap_or("");
            let mode = Mode::parse(a.get("mode").and_then(Value::as_str))?;
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(30) as usize;
            search(core, q, mode, limit, a.get("only").and_then(Value::as_str))
        }
        // ▶ the hit's span, looping, in the viewer (its project opened when closed)
        "library_play" => {
            let pid = project()?;
            ensure_open(core, pid, home)?;
            let media = media_arg(a)?;
            let (t0, t1) = (secs_arg(a, "start_s")?, secs_arg(a, "end_s")?);
            if t1 <= t0 || t1 - t0 > 600.0 {
                return Err(AppError::invalid("a span of at most 10 minutes"));
            }
            // a beat either side so the words are heard whole
            let a0 = ((t0 - 0.3).max(0.0) * FLICKS as f64) as i64;
            let a1 = ((t1 + 0.6) * FLICKS as f64) as i64;
            let p = core.project(pid)?;
            let doc = p.handle.snapshot().doc.clone();
            let (plan, at0) = crate::review::stream_plan(
                &doc,
                media,
                "search hit",
                (gmclip_model::Flicks(a0), gmclip_model::Flicks(a1)),
            )?;
            ctx.viewer.send(crate::viewer::ViewerMsg::LoadPlan {
                key: (pid.to_owned(), format!("span:{a0}:{a1}")),
                plan: Arc::new(plan),
                looping: true,
                at: None,
            });
            Ok(
                json!({"project": pid, "start_s": at0.0 as f64 / FLICKS as f64,
                      "end_s": a1 as f64 / FLICKS as f64}),
            )
        }
        // open-in-project: the cut that shows these words, and the frame there
        "library_open" => {
            let pid = project()?;
            ensure_open(core, pid, home)?;
            let media = media_arg(a)?;
            let t = (secs_arg(a, "start_s")? * FLICKS as f64) as i64;
            let (cut, frame) = cut_showing(core, pid, media, t).ok_or_else(|| {
                AppError::not_found("no cut in that project plays this recording")
            })?;
            Ok(json!({"project": pid, "cut": cut, "frame": frame}))
        }
        // a topic hit: the long-form that plays it, else the stream edit (K.04)
        "library_topic_open" => open_topic(
            core,
            project()?,
            home,
            a.get("topic")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::invalid("`topic` is required"))?,
        ),
        // ＋ Make moment from a hit (word-edged, origin manual)
        "library_moment" => {
            project()?;
            make_moment(core, a, a.get("note").and_then(Value::as_str).unwrap_or(""))
        }
        _ => Err(AppError::invalid("unknown command")),
    }
}
