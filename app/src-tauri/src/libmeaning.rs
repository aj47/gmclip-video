//! Search by meaning (L.02): "when I talked about GPU prices" finds the talk even when it
//! never says those words.
//!
//! - **Model**: all-MiniLM-L6-v2 (Apache-2.0, `gmclip_ml::embed`) on the process's one
//!   ONNX Runtime, from the pinned model cache; nothing is downloaded here (the speech
//!   model download installs it with the rest of the register).
//! - **Windows**: each transcript is cut into sentence windows — from every sentence
//!   start, whole sentences until ≥ 20 words (≤ 60; a longer sentence is cut in 40-word
//!   pieces) — so a hit is word-edged on whole sentences.
//! - **A job, cached by ASR revision**: windows are embedded in the background (after a
//!   transcript is stored, and whenever the index finds a transcript without vectors)
//!   into `ix_vec` of the library index, keyed `(project, owner, revision, model)`. A
//!   transcript is embedded once per revision; the vectors stay in memory for queries.
//! - **Ranking**: cosine against the query's vector; `hybrid` fuses the word hits and the
//!   meaning hits by reciprocal rank (k = 60) and marks a word hit inside a meaning window
//!   `both`. Meaning hits below the best one by more than 0.25, or under 0.2, are dropped.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use gmclip_ml::embed::{DIM, SentenceEmbedder};
use gmclip_ml::models::{MINILM_L6_V2, ModelCache};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::libsearch::{Hit, Mode};
use crate::planner::FLICKS;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ix_vec (
    project TEXT NOT NULL, owner TEXT NOT NULL, revision TEXT NOT NULL, model TEXT NOT NULL,
    w0 INTEGER NOT NULL, w1 INTEGER NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
    text TEXT NOT NULL, v BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS ix_vec_po ON ix_vec (project, owner);
CREATE TABLE IF NOT EXISTS ix_vec_done (
    project TEXT NOT NULL, owner TEXT NOT NULL, revision TEXT NOT NULL, model TEXT NOT NULL,
    windows INTEGER NOT NULL, ms INTEGER NOT NULL, PRIMARY KEY (project, owner));
";

/// The model id the vectors are keyed by.
pub const MODEL: &str = MINILM_L6_V2.id;

const BATCH: usize = 32;

fn ierr(e: impl std::fmt::Display) -> AppError {
    AppError::internal(format!("meaning index: {e}"))
}

fn open(core: &AppCore) -> AppResult<Connection> {
    let c = crate::libsearch::open(core)?;
    c.execute_batch(SCHEMA).map_err(ierr)?;
    Ok(c)
}

/// One window of a transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub w0: usize,
    pub w1: usize,
    pub text: String,
}

/// Sentence windows of a transcript (see the module docs).
pub fn windows(words: &[gmclip_core::analysis::Word]) -> Vec<Window> {
    let ss = gmclip_core::commands::moment::sentences(words);
    // sentences over 60 words are cut into 40-word pieces first
    let mut units: Vec<(usize, usize)> = Vec::new();
    for (a, b) in ss {
        let mut s = a;
        while b + 1 - s > 60 {
            units.push((s, s + 39));
            s += 40;
        }
        units.push((s, b));
    }
    let mut out = Vec::new();
    for i in 0..units.len() {
        let (a, mut b) = units[i];
        let mut j = i + 1;
        while b + 1 - a < 20 && j < units.len() && units[j].1 + 1 - a <= 60 {
            b = units[j].1;
            j += 1;
        }
        let text = words[a..=b]
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Window { w0: a, w1: b, text });
    }
    out
}

/// One stored vector, in memory.
#[derive(Clone, Debug)]
struct Row {
    project: String,
    owner: String,
    w0: usize,
    w1: usize,
    start_ms: i64,
    end_ms: i64,
    text: String,
    v: Vec<f32>,
}

#[derive(Default)]
struct Job {
    running: bool,
    pending: usize,
    error: Option<String>,
    last: Option<(usize, u64)>,
}

/// One app data dir's meaning index: the model (loaded once), the job, the vectors.
#[derive(Default)]
pub struct Meaning {
    embedder: Mutex<Option<Result<SentenceEmbedder, String>>>,
    job: Mutex<Job>,
    /// `(generation, rows)`; the generation bumps whenever `ix_vec` changes.
    rows: Mutex<(u64, u64, Arc<Vec<Row>>)>,
    generation: std::sync::atomic::AtomicU64,
    /// Set while the app quits: the job stops between batches and no new one starts.
    stop: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Meaning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Meaning")
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Meaning>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<Meaning>>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// The meaning index of this app's data dir.
pub fn of(core: &AppCore) -> Arc<Meaning> {
    let mut g = registry().lock().unwrap_or_else(|e| e.into_inner());
    g.entry(core.dirs.app_data.clone()).or_default().clone()
}

/// The pinned model cache (the OS cache, as for the speech models).
fn cache() -> Result<ModelCache, String> {
    ModelCache::default_cache().map_err(|e| format!("{e}"))
}

/// Whether the pinned model is in the cache (without loading it). Hashing 90 MB takes a
/// few hundred ms, so a verified model is remembered by its files' sizes and mtimes.
pub fn model_present() -> bool {
    static SEEN: Mutex<Option<Vec<(u64, std::time::SystemTime)>>> = Mutex::new(None);
    let Ok(c) = cache() else { return false };
    let stamp: Option<Vec<(u64, std::time::SystemTime)>> = MINILM_L6_V2
        .files
        .iter()
        .map(|f| {
            let m = std::fs::symlink_metadata(c.dir(&MINILM_L6_V2).join(f.name)).ok()?;
            Some((m.len(), m.modified().ok()?))
        })
        .collect();
    let Some(stamp) = stamp else { return false };
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref() == Some(&stamp) {
        return true;
    }
    let ok = c.verify(&MINILM_L6_V2).is_ok();
    *g = ok.then_some(stamp);
    ok
}

impl Meaning {
    fn with_embedder<R>(
        &self,
        f: impl FnOnce(&mut SentenceEmbedder) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut g = self.embedder.lock().map_err(|_| "poisoned".to_owned())?;
        if g.is_none() {
            *g = Some((|| {
                let c = cache()?;
                let m = c
                    .verify(&MINILM_L6_V2)
                    .map_err(|e| format!("no_model: {e}"))?;
                let rt = gmclip_ml::runtime::OrtRuntime::init(&c)
                    .map_err(|e| format!("onnx runtime: {e}"))?;
                SentenceEmbedder::load(rt, &m).map_err(|e| format!("{e}"))
            })());
        }
        match g.as_mut() {
            Some(Ok(e)) => f(e),
            Some(Err(e)) => Err(e.clone()),
            None => Err("not loaded".into()),
        }
    }

    /// Vectors for texts.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.with_embedder(|e| e.embed(texts).map_err(|x| format!("{x}")))
    }

    /// The vectors in memory, reloaded after the job wrote more.
    fn rows(&self, c: &Connection) -> AppResult<Arc<Vec<Row>>> {
        let gen_now = self.generation.load(std::sync::atomic::Ordering::SeqCst);
        // what is stored, from the small table (a count over the vectors reads them all)
        let n_now: u64 = c
            .query_row(
                "SELECT coalesce(sum(windows), 0) * 1000003 + count(*) FROM ix_vec_done WHERE model = ?1",
                params![MODEL],
                |r| r.get::<_, i64>(0),
            )
            .map_err(ierr)? as u64;
        let mut g = self.rows.lock().map_err(|_| ierr("poisoned"))?;
        if g.0 == gen_now && g.1 == n_now && (n_now == 0 || !g.2.is_empty()) {
            return Ok(g.2.clone());
        }
        let mut st = c
            .prepare("SELECT project, owner, w0, w1, start_ms, end_ms, text, v FROM ix_vec WHERE model = ?1")
            .map_err(ierr)?;
        let rows: Vec<Row> = st
            .query_map(params![MODEL], |r| {
                let b: Vec<u8> = r.get(7)?;
                Ok(Row {
                    project: r.get(0)?,
                    owner: r.get(1)?,
                    w0: r.get::<_, i64>(2)? as usize,
                    w1: r.get::<_, i64>(3)? as usize,
                    start_ms: r.get(4)?,
                    end_ms: r.get(5)?,
                    text: r.get(6)?,
                    v: b.chunks_exact(4)
                        .map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]]))
                        .collect(),
                })
            })
            .map_err(ierr)?
            .flatten()
            .filter(|r| r.v.len() == DIM)
            .collect();
        *g = (gen_now, n_now, Arc::new(rows));
        Ok(g.2.clone())
    }
}

/// Transcripts in the index without vectors for their revision: `(project, owner, rev)`.
fn pending(c: &Connection) -> AppResult<Vec<(String, String, String)>> {
    let mut st = c
        .prepare(
            "SELECT m.project, m.owner, m.revision FROM ix_media m
             LEFT JOIN ix_vec_done d ON d.project = m.project AND d.owner = m.owner
               AND d.revision = m.revision AND d.model = ?1
             WHERE d.project IS NULL ORDER BY m.project, m.owner",
        )
        .map_err(ierr)?;
    let out = st
        .query_map(params![MODEL], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(ierr)?
        .flatten()
        .collect();
    Ok(out)
}

/// Vectors of transcripts that left the index (or moved to another revision) go too.
fn drop_stale(c: &Connection) -> AppResult<usize> {
    let stale: Vec<(String, String)> = {
        let mut st = c
            .prepare(
                "SELECT d.project, d.owner FROM ix_vec_done d WHERE NOT EXISTS (SELECT 1 FROM ix_media m
                   WHERE m.project = d.project AND m.owner = d.owner AND m.revision = d.revision)",
            )
            .map_err(ierr)?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(ierr)?
            .flatten()
            .collect()
    };
    for (p, o) in &stale {
        c.execute(
            "DELETE FROM ix_vec WHERE project = ?1 AND owner = ?2",
            params![p, o],
        )
        .map_err(ierr)?;
        c.execute(
            "DELETE FROM ix_vec_done WHERE project = ?1 AND owner = ?2",
            params![p, o],
        )
        .map_err(ierr)?;
    }
    Ok(stale.len())
}

/// Embeds one transcript's windows and stores them (one transaction).
fn embed_one(
    core: &AppCore,
    m: &Meaning,
    project: &str,
    owner: &str,
    rev: &str,
) -> Result<usize, String> {
    let t0 = std::time::Instant::now();
    let data = crate::libsearch::project_data(core, project).ok_or("project unreadable")?;
    let body = data.analysis(owner, "words", rev).ok_or("words gone")?;
    let (_, words) = gmclip_core::analysis::decode_words(&body).ok_or("words unreadable")?;
    let ws = windows(&words);
    let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(ws.len());
    for chunk in ws.chunks(BATCH) {
        if m.stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("stopped (quitting)".into());
        }
        let texts: Vec<&str> = chunk.iter().map(|w| w.text.as_str()).collect();
        vecs.extend(m.embed(&texts)?);
    }
    let mut c = open(core).map_err(|e| e.message)?;
    let tx = c.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM ix_vec WHERE project = ?1 AND owner = ?2",
        params![project, owner],
    )
    .map_err(|e| e.to_string())?;
    {
        let mut ins = tx
            .prepare(
                "INSERT INTO ix_vec (project, owner, revision, model, w0, w1, start_ms, end_ms, text, v)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )
            .map_err(|e| e.to_string())?;
        for (w, v) in ws.iter().zip(&vecs) {
            let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
            ins.execute(params![
                project,
                owner,
                rev,
                MODEL,
                w.w0 as i64,
                w.w1 as i64,
                words[w.w0].start * 1000 / FLICKS,
                words[w.w1].end * 1000 / FLICKS,
                w.text,
                blob
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO ix_vec_done (project, owner, revision, model, windows, ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![project, owner, rev, MODEL, ws.len() as i64, t0.elapsed().as_millis() as i64],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    m.generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(ws.len())
}

/// Starts the embedding job when transcripts wait for vectors (one job at a time, on its
/// own thread). Returns how many wait.
pub fn ensure(core: &Arc<AppCore>) -> AppResult<usize> {
    let c = open(core)?;
    drop_stale(&c)?;
    let todo = pending(&c)?;
    let m = of(core);
    {
        let mut j = m.job.lock().map_err(|_| ierr("poisoned"))?;
        j.pending = todo.len();
        if j.running
            || todo.is_empty()
            || !model_present()
            || m.stop.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(todo.len());
        }
        j.running = true;
        j.error = None;
    }
    let weak: Weak<AppCore> = Arc::downgrade(core);
    let n = todo.len();
    let _ = std::thread::Builder::new()
        .name("gmclip-meaning".into())
        .spawn(move || {
            let t0 = std::time::Instant::now();
            let mut windows = 0;
            let mut err = None;
            for (i, (p, o, r)) in todo.iter().enumerate() {
                let Some(core) = weak.upgrade() else { return };
                match embed_one(&core, &m, p, o, r) {
                    Ok(k) => windows += k,
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                }
                if let Ok(mut j) = m.job.lock() {
                    j.pending = n - i - 1;
                }
            }
            if let Ok(mut j) = m.job.lock() {
                j.running = false;
                j.error = err;
                j.last = Some((windows, t0.elapsed().as_millis() as u64));
            }
            // a transcript stored while this ran
            if let Some(core) = weak.upgrade() {
                let _ = ensure(&core);
            }
        });
    Ok(n)
}

/// After a transcript is stored (analysis time): index it and embed it, in the background.
pub fn kick(core: &Arc<AppCore>) {
    let weak = Arc::downgrade(core);
    let _ = std::thread::Builder::new()
        .name("gmclip-meaning-kick".into())
        .spawn(move || {
            if let Some(core) = weak.upgrade() {
                let _ = crate::libsearch::refresh(&core);
                let _ = ensure(&core);
            }
        });
}

/// Quitting: the embedding job stops between batches, no new one starts, and this waits
/// (up to `wait`) for it to end, so no thread is inside ONNX Runtime when the process
/// exits (its static teardown aborts with "recursive_mutex lock failed"). True when idle.
pub fn stop(core: &AppCore, wait: std::time::Duration) -> bool {
    let m = of(core);
    m.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let t = std::time::Instant::now();
    loop {
        if m.job.lock().is_ok_and(|j| !j.running) {
            // the model lives in a process-wide registry: release its session (and its
            // thread pool) now, not in ONNX Runtime's static teardown at exit (a later
            // search loads it again)
            if let Ok(mut e) = m.embedder.lock() {
                *e = None;
            }
            return true;
        }
        if t.elapsed() > wait {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A new app on this data dir: the job may run again (after a [`stop`]).
pub fn resume(core: &AppCore) {
    of(core)
        .stop
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Waits (tests, benches) until the job is idle; the windows and ms of its last run.
pub fn wait_idle(core: &AppCore, max: std::time::Duration) -> Option<(usize, u64)> {
    let m = of(core);
    let t = std::time::Instant::now();
    loop {
        if let Ok(j) = m.job.lock()
            && !j.running
        {
            return j.last;
        }
        if t.elapsed() > max {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The meaning index's state for the UI.
fn state(core: &AppCore, c: &Connection) -> Value {
    let m = of(core);
    let windows: i64 = c
        .query_row(
            "SELECT coalesce(sum(windows), 0) FROM ix_vec_done WHERE model = ?1",
            params![MODEL],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let j = m.job.lock();
    let (running, pending, error) = j
        .as_ref()
        .map(|j| (j.running, j.pending, j.error.clone()))
        .unwrap_or((false, 0, None));
    let s = if !model_present() {
        "no_model"
    } else if let Some(e) = &error {
        let _ = e;
        "failed"
    } else if running || pending > 0 {
        "building"
    } else {
        "ready"
    };
    json!({"state": s, "windows": windows, "pending": pending, "model": MODEL,
           "error": error.map(|e| e.chars().take(200).collect::<String>())})
}

/// Meaning hits for `query` (best first) and the meaning index's state. Starts the job
/// for transcripts without vectors; hits come from what is embedded so far.
pub fn hits(
    core: &Arc<AppCore>,
    c: &Connection,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> AppResult<(Vec<Hit>, Value)> {
    let _ = ensure(core);
    let st = state(core, c);
    if query.trim().is_empty() || !model_present() {
        return Ok((Vec::new(), st));
    }
    let m = of(core);
    let rows = m.rows(c)?;
    if rows.is_empty() {
        return Ok((Vec::new(), st));
    }
    let q = match m.embed(&[query]) {
        Ok(mut v) => v.pop().unwrap_or_default(),
        Err(e) => {
            return Ok((
                Vec::new(),
                json!({"state": "failed", "model": MODEL, "error": e.chars().take(200).collect::<String>()}),
            ));
        }
    };
    let mut scored: Vec<(f32, usize)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| project.is_none_or(|p| r.project == p))
        .map(|(i, r)| (gmclip_ml::embed::cosine(&q, &r.v), i))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let best = scored.first().map_or(0.0, |s| s.0);
    let floor = (best - 0.25).max(0.2);
    let mut out: Vec<Hit> = Vec::new();
    for (s, i) in scored {
        if s < floor || out.len() >= limit {
            break;
        }
        let r = &rows[i];
        // overlapping windows of one stretch: keep the best
        if out
            .iter()
            .any(|h| h.project == r.project && h.owner == r.owner && h.w0 <= r.w1 && r.w0 <= h.w1)
        {
            continue;
        }
        let text: String = {
            let ws: Vec<&str> = r.text.split_whitespace().collect();
            if ws.len() > 50 {
                format!("{} …", ws[..50].join(" "))
            } else {
                r.text.clone()
            }
        };
        out.push(Hit {
            project: r.project.clone(),
            owner: r.owner.clone(),
            w0: r.w0,
            w1: r.w1,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            text,
            before: String::new(),
            after: String::new(),
            score: f64::from(s),
            by: "meaning",
        });
    }
    Ok((out, st))
}

/// Word and meaning hits as one list. `hybrid`: reciprocal-rank fusion (k = 60); a word
/// hit inside a meaning hit's window is one hit, `both`, with the word hit's exact span.
pub fn merge(words: Vec<Hit>, meaning: Vec<Hit>, mode: Mode, limit: usize) -> Vec<Hit> {
    if mode == Mode::Meaning {
        let mut m = meaning;
        m.truncate(limit);
        return m;
    }
    let mut fused: BTreeMap<usize, (f64, Hit)> = BTreeMap::new();
    let k = 60.0;
    for (r, h) in words.into_iter().enumerate() {
        fused.insert(r, (1.0 / (k + r as f64 + 1.0), h));
    }
    let base = fused.len();
    for (r, h) in meaning.into_iter().enumerate() {
        let add = 1.0 / (k + r as f64 + 1.0);
        let inside = fused.values_mut().find(|(_, w)| {
            w.by != "meaning"
                && w.project == h.project
                && w.owner == h.owner
                && h.w0 <= w.w0
                && w.w1 <= h.w1
        });
        match inside {
            Some((s, w)) => {
                *s += add;
                w.by = "both";
            }
            None => {
                fused.insert(base + r, (add, h));
            }
        }
    }
    let mut all: Vec<(f64, Hit)> = fused.into_values().collect();
    all.sort_by(|a, b| b.0.total_cmp(&a.0));
    all.into_iter()
        .take(limit)
        .map(|(s, mut h)| {
            h.score = s * 1000.0;
            h
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str) -> gmclip_core::analysis::Word {
        gmclip_core::analysis::Word {
            text: t.into(),
            start: 0,
            end: 1,
            confidence: None,
        }
    }

    #[test]
    fn windows_are_whole_sentences_of_20_to_60_words() {
        // "a … a." sentences of 8 words, then one of 70
        let mut words = Vec::new();
        for _ in 0..4 {
            for i in 0..8 {
                words.push(w(if i == 7 { "end." } else { "word" }));
            }
        }
        for i in 0..70 {
            words.push(w(if i == 69 { "end." } else { "long" }));
        }
        let ws = windows(&words);
        // from each 8-word sentence: three sentences (24 words), fewer at the tail
        assert_eq!((ws[0].w0, ws[0].w1), (0, 23));
        assert_eq!((ws[1].w0, ws[1].w1), (8, 31));
        // the 70-word sentence: a 40-word piece then a 30-word piece
        assert!(ws.iter().any(|x| (x.w0, x.w1) == (32, 71)));
        assert!(ws.iter().any(|x| (x.w0, x.w1) == (72, 101)));
        assert!(ws.iter().all(|x| x.w1 + 1 - x.w0 <= 60));
    }
}
