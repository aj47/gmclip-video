//! Storage (SPEC §3.8, §4.4): the SQLite op log, entity snapshot rows, restore points
//! and named snapshots, epochs (R-MODEL-3), runs and idempotency records, and the
//! one-writer lock file (R-STORE-3).
//!
//! - **A command commits only its op row** (plus, atomically, its idempotency record):
//!   one small SQLite transaction in WAL mode with `synchronous=NORMAL`, so a commit is
//!   crash-durable immediately (R-STORE-7).
//! - **Entity snapshot rows** are written by [`Store::checkpoint`] (the actor calls it
//!   after 500 ops, 30 s idle, or on close): only entities touched since the last one.
//! - **Open** = entity snapshot rows + replay of later op rows ([`Store::open`]).
//! - **"Saved"** = power-durable: a WAL checkpoint with `checkpoint_fullfsync=ON`, run
//!   by a background [`Checkpointer`] on its own connection (≤ 2 s after a commit), so
//!   the actor never waits on a full sync. `wal_autocheckpoint` is off for the same
//!   reason: SQLite would otherwise run that sync inside an unlucky commit.
#![forbid(unsafe_code)]

mod lock;
mod saver;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gmclip_model::{Author, Doc, EntityId, EntityKind, Rev, Txn, Ulid, cbor};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub use lock::{
    Acquired, HolderKind, LockInfo, ProjectLock, holder_path, local_host, lock_path, peek,
};
pub use saver::Checkpointer;

/// Errors from the store. Messages are shape-only (no paths, no user text).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Another live process holds the project (R-STORE-3).
    #[error("the project is open in another process")]
    Leased { holder: Option<LockInfo> },
    /// A stale lock names another host: the database is on shared storage.
    #[error("the project was last opened on another host")]
    StaleOtherHost { holder: LockInfo },
    #[error("the database failed its integrity check")]
    IntegrityFailed,
    #[error("sqlite: {0}")]
    Sqlite(String),
    #[error("i/o during {0}: {1}")]
    Io(&'static str, String),
    #[error("corrupt {0}")]
    Corrupt(&'static str),
    #[error("model: {0}")]
    Model(#[from] gmclip_model::ModelError),
    #[error("op rows are not contiguous at rev {0}")]
    Gap(Rev),
    #[error("schema version {0} is newer than this build")]
    TooNew(i64),
    #[error("{0}")]
    Invalid(&'static str),
}

impl StoreError {
    fn io(what: &'static str, e: &std::io::Error) -> Self {
        StoreError::Io(what, e.kind().to_string())
    }

    /// The disk (or the database's page limit) is full: `SQLITE_FULL`, or an ENOSPC
    /// write (R-ERR-2: the edit is kept and the write retried, never discarded).
    pub fn is_disk_full(&self) -> bool {
        match self {
            StoreError::Sqlite(c) => c == "DiskFull",
            StoreError::Io(_, k) => k == &std::io::ErrorKind::StorageFull.to_string(),
            _ => false,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        // SQLite messages can quote SQL but never row contents; keep the code only.
        match e.sqlite_error_code() {
            Some(c) => StoreError::Sqlite(format!("{c:?}")),
            None => StoreError::Sqlite("error".into()),
        }
    }
}

/// The schema version written to `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ops (
    rev INTEGER PRIMARY KEY,
    txn_id TEXT NOT NULL,
    author TEXT NOT NULL,
    run TEXT,
    ts INTEGER NOT NULL,
    body BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS entities (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    rev INTEGER NOT NULL,
    author BLOB NOT NULL,
    body BLOB NOT NULL,
    PRIMARY KEY (kind, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS snapshots (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('restore_point', 'named', 'epoch')),
    name TEXT,
    rev INTEGER NOT NULL,
    run TEXT,
    ts INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS snapshot_rows (
    snapshot TEXT NOT NULL,
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    rev INTEGER NOT NULL,
    author BLOB NOT NULL,
    body BLOB NOT NULL,
    PRIMARY KEY (snapshot, kind, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS requests (
    identity TEXT NOT NULL,
    scope TEXT NOT NULL,
    request_id TEXT NOT NULL,
    ts INTEGER NOT NULL,
    rev INTEGER,
    result BLOB NOT NULL,
    PRIMARY KEY (identity, scope, request_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS requests_by_id ON requests (request_id, ts);
CREATE TABLE IF NOT EXISTS analysis (
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    revision TEXT NOT NULL,
    body BLOB NOT NULL,
    PRIMARY KEY (owner, kind, revision)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS proposals (id TEXT PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS assets (id TEXT PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;
";

/// How to open a project database.
#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub kind: HolderKind,
    /// Defaults to [`local_host`].
    pub host: Option<String>,
    /// Take over a stale lock naming another host.
    pub force_other_host: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            kind: HolderKind::App,
            host: None,
            force_other_host: false,
        }
    }
}

/// What [`Store::open`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReport {
    pub checkpoint_rev: Rev,
    pub rev: Rev,
    pub replayed: usize,
    pub acquired: Acquired,
    /// `PRAGMA quick_check` ran (always, after taking over a stale lock).
    pub integrity_checked: bool,
    /// The model epoch this open started (R-MODEL-3), when the project was older.
    pub epoch_started: Option<u32>,
}

/// A restore point, named snapshot or epoch start (SPEC Conventions, History terms).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub kind: SnapshotKind,
    pub name: Option<String>,
    /// The document rev the snapshot names.
    pub rev: Rev,
    pub run: Option<String>,
    pub ts: i64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    RestorePoint,
    Named,
    Epoch,
}

impl SnapshotKind {
    fn as_str(self) -> &'static str {
        match self {
            SnapshotKind::RestorePoint => "restore_point",
            SnapshotKind::Named => "named",
            SnapshotKind::Epoch => "epoch",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "restore_point" => SnapshotKind::RestorePoint,
            "named" => SnapshotKind::Named,
            "epoch" => SnapshotKind::Epoch,
            _ => return None,
        })
    }
}

/// An idempotency record (R-CMD-7), keyed `(identity, connection-or-run id, request_id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRecord {
    pub identity: String,
    pub scope: String,
    pub request_id: String,
    pub ts: i64,
    pub rev: Option<Rev>,
    /// The original result, as the caller wants it back (JSON bytes).
    pub result: Vec<u8>,
}

/// The project database (see the crate docs).
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: PathBuf,
    lock: Option<ProjectLock>,
    dirty: BTreeSet<EntityId>,
    last_rev: Rev,
    checkpoint_rev: Rev,
    ops_since_checkpoint: u64,
    /// The rev power-durable at open (after the opening WAL checkpoint).
    saved_at_open: Rev,
}

fn author_text(a: &Author) -> String {
    a.label()
}

fn set_pragmas(conn: &Connection) -> Result<(), StoreError> {
    // R-STORE-7. journal_mode answers with the mode it is in.
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Invalid("the database cannot use WAL here"));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "fullfsync", "OFF")?;
    conn.pragma_update(None, "checkpoint_fullfsync", "ON")?;
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
    Ok(())
}

impl Store {
    /// Opens (creating if needed) the database at `path`, takes the lock, and loads the
    /// document: entity snapshot rows, then every later op row replayed in rev order.
    pub fn open(path: &Path, opts: &OpenOptions) -> Result<(Store, Doc, OpenReport), StoreError> {
        let info = LockInfo {
            pid: std::process::id(),
            host: opts.host.clone().unwrap_or_else(local_host),
            kind: opts.kind,
        };
        let (lock, acquired) = ProjectLock::acquire(path, info, opts.force_other_host)?;
        let conn = Connection::open(path)?;
        // Read-only checks first: nothing is written into a database that is corrupt
        // or from a newer build.
        let mut integrity_checked = false;
        if !matches!(acquired, Acquired::Fresh) {
            integrity_checked = true;
            let ok: String = conn
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .map_err(|_| StoreError::IntegrityFailed)?;
            if ok != "ok" {
                return Err(StoreError::IntegrityFailed);
            }
        }
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::TooNew(version));
        }
        set_pragmas(&conn)?;
        conn.execute_batch(SCHEMA)?;
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        let mut store = Store {
            conn,
            path: path.to_owned(),
            lock: Some(lock),
            dirty: BTreeSet::new(),
            last_rev: 0,
            checkpoint_rev: 0,
            ops_since_checkpoint: 0,
            saved_at_open: 0,
        };
        let (doc, checkpoint_rev, replayed) = store.load()?;
        store.last_rev = doc.rev();
        store.checkpoint_rev = checkpoint_rev;
        store.ops_since_checkpoint = replayed as u64;
        let epoch_started = store.migrate_epoch(&doc)?;
        // whatever a crash left in the WAL becomes power-durable now, so "Saved"
        // starts at the opened rev
        store.saved_at_open = store.wal_checkpoint(false)?.unwrap_or(0);
        let report = OpenReport {
            checkpoint_rev,
            rev: doc.rev(),
            replayed,
            acquired,
            integrity_checked,
            epoch_started,
        };
        Ok((store, doc, report))
    }

    /// R-MODEL-3: the first open of a project older than this build's model epoch
    /// (`gmclip_model::MODEL_EPOCH`) starts a history epoch: a full set of entity rows
    /// kept as the named snapshot "epoch k start". A project with no history yet just
    /// records the epoch. A project from a newer epoch is refused (its rows would not
    /// load here).
    fn migrate_epoch(&mut self, doc: &Doc) -> Result<Option<u32>, StoreError> {
        let want = gmclip_model::MODEL_EPOCH;
        let have: u32 = match self.meta("model_epoch")? {
            Some(b) => cbor::from_slice(&b)?,
            None => 0,
        };
        if have > want {
            return Err(StoreError::TooNew(i64::from(have)));
        }
        if have == want {
            return Ok(None);
        }
        let started = (doc.rev() > 0).then_some(want);
        if started.is_some() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            self.write_epoch(
                doc,
                &format!("epoch-{want}"),
                &format!("epoch {want} start"),
                ts,
            )?;
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('model_epoch', ?1)",
            [cbor::to_vec(&want)],
        )?;
        Ok(started)
    }

    /// The model epoch this project's history is in (R-MODEL-3).
    pub fn model_epoch(&self) -> Result<u32, StoreError> {
        Ok(match self.meta("model_epoch")? {
            Some(b) => cbor::from_slice(&b)?,
            None => 0,
        })
    }

    /// A small app-level value kept with the project (`meta` table): the human's undo
    /// stack across sessions (R-HIST-1) and who wrote a system transaction for a link.
    /// Keys the store itself uses are refused.
    pub fn put_meta(&mut self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        if !key.starts_with("app:") {
            return Err(StoreError::Invalid("app meta keys start with app:"));
        }
        self.conn
            .prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")?
            .execute(params![key, value])?;
        Ok(())
    }

    /// An app-level value written by [`Store::put_meta`].
    pub fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        if !key.starts_with("app:") {
            return Err(StoreError::Invalid("app meta keys start with app:"));
        }
        self.meta(key)
    }

    /// Every app-level value whose key starts with `prefix` (which starts with `app:`).
    pub fn meta_prefixed(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        if !prefix.starts_with("app:") {
            return Err(StoreError::Invalid("app meta keys start with app:"));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT key, value FROM meta WHERE key >= ?1 AND key < ?2 ORDER BY key",
        )?;
        let hi = format!("{prefix}\u{10ffff}");
        let rows = stmt.query_map(params![prefix, hi], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        meta_of(&self.conn, key)
    }

    fn load(&mut self) -> Result<(Doc, Rev, usize), StoreError> {
        let dirty = &mut self.dirty;
        load_doc(&self.conn, &mut |t: &Txn| {
            for op in &t.ops {
                dirty.insert(op.entity());
            }
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fault injection (R-ERR-2 tests and the dev bridge): `true` caps the database at
    /// the pages it has now (`PRAGMA max_page_count`), so the next write that needs a
    /// new page fails with `SQLITE_FULL` exactly as a full disk does; `false` lifts the
    /// cap ("space came back"). Only this connection is capped.
    pub fn simulate_disk_full(&mut self, on: bool) -> Result<(), StoreError> {
        let n: i64 = if on {
            self.conn
                .pragma_query_value(None, "page_count", |r| r.get(0))?
        } else {
            // SQLite's own ceiling (SQLITE_MAX_PAGE_COUNT)
            4_294_967_294
        };
        self.conn.pragma_update(None, "max_page_count", n)?;
        Ok(())
    }

    /// The last committed rev.
    pub fn rev(&self) -> Rev {
        self.last_rev
    }

    pub fn checkpoint_rev(&self) -> Rev {
        self.checkpoint_rev
    }

    /// Ops committed since the last entity snapshot rows.
    pub fn ops_since_checkpoint(&self) -> u64 {
        self.ops_since_checkpoint
    }

    /// Commits one transaction's op row (R-STORE-7). Crash-durable on return.
    pub fn commit(&mut self, t: &Txn) -> Result<(), StoreError> {
        self.commit_with(Some(t), None)
    }

    /// Commits an op row and/or an idempotency record in one SQLite transaction.
    pub fn commit_with(
        &mut self,
        t: Option<&Txn>,
        req: Option<&RequestRecord>,
    ) -> Result<(), StoreError> {
        if let Some(t) = t
            && t.rev != self.last_rev + 1
        {
            return Err(StoreError::Gap(self.last_rev + 1));
        }
        let tx = self.conn.transaction()?;
        if let Some(t) = t {
            let body = t.to_cbor();
            tx.prepare_cached("INSERT INTO ops (rev, txn_id, author, run, ts, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?
                .execute(params![
                    i64::try_from(t.rev).map_err(|_| StoreError::Invalid("rev too large"))?,
                    t.id.0.to_string(),
                    author_text(&t.author),
                    t.run.map(|r| r.0.to_string()),
                    t.ts,
                    body
                ])?;
        }
        if let Some(r) = req {
            tx.prepare_cached(
                "INSERT INTO requests (identity, scope, request_id, ts, rev, result) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                r.identity,
                r.scope,
                r.request_id,
                r.ts,
                r.rev.and_then(|v| i64::try_from(v).ok()),
                r.result
            ])?;
        }
        tx.commit()?;
        if let Some(t) = t {
            self.last_rev = t.rev;
            self.ops_since_checkpoint += 1;
            for op in &t.ops {
                self.dirty.insert(op.entity());
            }
        }
        Ok(())
    }

    /// Writes entity snapshot rows for every entity touched since the last ones, in one
    /// SQLite transaction. `doc` must be at the store's last committed rev.
    pub fn checkpoint(&mut self, doc: &Doc) -> Result<usize, StoreError> {
        if doc.rev() != self.last_rev {
            return Err(StoreError::Invalid(
                "checkpoint of a document not at the last commit",
            ));
        }
        let n = self.dirty.len();
        let tx = self.conn.transaction()?;
        for e in &self.dirty {
            let kind = e.kind.0;
            let id = e.id.to_string();
            match doc.encode(*e) {
                Some(body) => {
                    let rev = doc.entity_rev(*e).unwrap_or(0);
                    let author = cbor::to_vec(&doc.last_author(*e).unwrap_or_default());
                    tx.prepare_cached(
                        "INSERT OR REPLACE INTO entities (kind, id, rev, author, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                    )?
                    .execute(params![
                        kind,
                        id,
                        i64::try_from(rev).map_err(|_| StoreError::Invalid("rev too large"))?,
                        author,
                        body.0
                    ])?;
                }
                None => {
                    tx.prepare_cached("DELETE FROM entities WHERE kind = ?1 AND id = ?2")?
                        .execute(params![kind, id])?;
                }
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('index_revs', ?1)",
            [cbor::to_vec(&doc.index_revs())],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('checkpoint_rev', ?1)",
            [cbor::to_vec(&doc.rev())],
        )?;
        tx.commit()?;
        self.dirty.clear();
        self.checkpoint_rev = doc.rev();
        self.ops_since_checkpoint = 0;
        Ok(n)
    }

    /// A WAL checkpoint on this connection (used on close and by tests). Returns the rev
    /// that is now power-durable, or `None` if the checkpoint did not complete.
    pub fn wal_checkpoint(&mut self, truncate: bool) -> Result<Option<Rev>, StoreError> {
        let rev = self.last_rev;
        let mode = if truncate { "TRUNCATE" } else { "PASSIVE" };
        let (busy, log, done): (i64, i64, i64) =
            self.conn
                .query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
        Ok((busy == 0 && log == done).then_some(rev))
    }

    /// A background checkpointer on its own connection (R-STORE-7 "Saved").
    pub fn checkpointer(&self) -> Result<Checkpointer, StoreError> {
        Checkpointer::start(&self.path, self.saved_at_open)
    }

    /// Committed transactions with `from < rev ≤ to`, in rev order.
    pub fn txns(&self, from: Rev, to: Rev) -> Result<Vec<Txn>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT body FROM ops WHERE rev > ?1 AND rev <= ?2 ORDER BY rev")?;
        let mut rows = stmt.query(params![
            i64::try_from(from).map_err(|_| StoreError::Invalid("rev"))?,
            i64::try_from(to).map_err(|_| StoreError::Invalid("rev"))?
        ])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let b: Vec<u8> = r.get(0)?;
            out.push(Txn::from_cbor(&b)?);
        }
        Ok(out)
    }

    /// Replays every op row from an empty document (the op-replay oracle; also the
    /// check that entity snapshot rows agree with the log).
    pub fn replay_all(&self) -> Result<Doc, StoreError> {
        let mut d = Doc::new();
        for t in self.txns(0, Rev::MAX >> 1)? {
            d.apply(&t)?;
        }
        Ok(d)
    }

    pub fn add_snapshot(&mut self, s: &SnapshotRecord) -> Result<(), StoreError> {
        self.conn
            .prepare_cached("INSERT INTO snapshots (id, kind, name, rev, run, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?
            .execute(params![
                s.id,
                s.kind.as_str(),
                s.name,
                i64::try_from(s.rev).map_err(|_| StoreError::Invalid("rev"))?,
                s.run,
                s.ts
            ])?;
        Ok(())
    }

    pub fn snapshots(&self) -> Result<Vec<SnapshotRecord>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, kind, name, rev, run, ts FROM snapshots ORDER BY rev, ts, id",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let kind: String = r.get(1)?;
            let rev: i64 = r.get(3)?;
            out.push(SnapshotRecord {
                id: r.get(0)?,
                kind: SnapshotKind::parse(&kind).ok_or(StoreError::Corrupt("snapshot kind"))?,
                name: r.get(2)?,
                rev: Rev::try_from(rev).map_err(|_| StoreError::Corrupt("snapshot rev"))?,
                run: r.get(4)?,
                ts: r.get(5)?,
            });
        }
        Ok(out)
    }

    /// R-MODEL-3: a schema migration starts an epoch — a full set of entity rows kept
    /// with the named snapshot `name` (e.g. "epoch 2 start").
    pub fn write_epoch(
        &mut self,
        doc: &Doc,
        id: &str,
        name: &str,
        ts: i64,
    ) -> Result<(), StoreError> {
        if doc.rev() != self.last_rev {
            return Err(StoreError::Invalid(
                "epoch of a document not at the last commit",
            ));
        }
        let tx = self.conn.transaction()?;
        for e in doc.entity_ids() {
            let body = doc.encode(e).ok_or(StoreError::Corrupt("entity"))?;
            let rev = doc.entity_rev(e).unwrap_or(0);
            tx.prepare_cached(
                "INSERT INTO snapshot_rows (snapshot, kind, id, rev, author, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                id,
                e.kind.0,
                e.id.to_string(),
                i64::try_from(rev).map_err(|_| StoreError::Invalid("rev"))?,
                cbor::to_vec(&doc.last_author(e).unwrap_or_default()),
                body.0
            ])?;
        }
        tx.execute(
            "INSERT INTO snapshots (id, kind, name, rev, run, ts) VALUES (?1, 'epoch', ?2, ?3, NULL, ?4)",
            params![id, name, i64::try_from(doc.rev()).map_err(|_| StoreError::Invalid("rev"))?, ts],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The document as an epoch's full rows recorded it.
    pub fn epoch_doc(&self, id: &str) -> Result<Doc, StoreError> {
        let rev: i64 = self
            .conn
            .query_row(
                "SELECT rev FROM snapshots WHERE id = ?1 AND kind = 'epoch'",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::Invalid("no such epoch"))?;
        let mut doc = Doc::new();
        let mut stmt = self
            .conn
            .prepare("SELECT kind, id, rev, author, body FROM snapshot_rows WHERE snapshot = ?1")?;
        let mut rows = stmt.query([id])?;
        while let Some(r) = rows.next()? {
            let kind: String = r.get(0)?;
            let eid: String = r.get(1)?;
            let erev: i64 = r.get(2)?;
            let author: Vec<u8> = r.get(3)?;
            let body: Vec<u8> = r.get(4)?;
            doc.load_row(
                gmclip_model::kinds::kind_by_name(&kind)
                    .ok_or(StoreError::Corrupt("entity kind"))?,
                Ulid::parse(&eid).map_err(|_| StoreError::Corrupt("entity id"))?,
                Rev::try_from(erev).map_err(|_| StoreError::Corrupt("rev"))?,
                cbor::from_slice(&author)?,
                &body,
            )?;
        }
        doc.set_rev(Rev::try_from(rev).map_err(|_| StoreError::Corrupt("rev"))?);
        Ok(doc)
    }

    pub fn put_run(&mut self, id: &str, body: &[u8]) -> Result<(), StoreError> {
        self.conn
            .prepare_cached("INSERT OR REPLACE INTO runs (id, body) VALUES (?1, ?2)")?
            .execute(params![id, body])?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, body FROM runs ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Stores an analysis blob (SPEC §4.4 `analysis`: immutable columnar blobs by
    /// `(source|group, kind, revision)`). Writing the same bytes again is a no-op; other
    /// bytes under an existing key are refused (a new analysis is a new revision).
    pub fn put_analysis(
        &mut self,
        owner: &str,
        kind: &str,
        revision: &str,
        body: &[u8],
    ) -> Result<(), StoreError> {
        match self.analysis(owner, kind, revision)? {
            Some(b) if b == body => Ok(()),
            Some(_) => Err(StoreError::Invalid(
                "analysis blobs are immutable: a new analysis needs a new revision",
            )),
            None => {
                self.conn
                    .prepare_cached(
                        "INSERT INTO analysis (owner, kind, revision, body) VALUES (?1, ?2, ?3, ?4)",
                    )?
                    .execute(params![owner, kind, revision, body])?;
                Ok(())
            }
        }
    }

    /// One analysis blob, if stored.
    pub fn analysis(
        &self,
        owner: &str,
        kind: &str,
        revision: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT body FROM analysis WHERE owner = ?1 AND kind = ?2 AND revision = ?3",
            )?
            .query_row(params![owner, kind, revision], |r| r.get(0))
            .optional()?)
    }

    /// Every stored analysis key `(owner, kind, revision)`, in key order.
    pub fn analysis_keys(&self) -> Result<Vec<(String, String, String)>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT owner, kind, revision FROM analysis ORDER BY owner, kind, revision",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// An idempotency record no older than `since` (ms since the epoch).
    /// The stored results of `request_id` under identities other than `identity` since
    /// `since` (R-AGT-5: the caller refuses a replay of another identity's request).
    pub fn request_used_elsewhere(
        &self,
        identity: &str,
        request_id: &str,
        since: i64,
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut st = self
            .conn
            .prepare_cached("SELECT result FROM requests WHERE request_id = ?1 AND identity <> ?2 AND ts >= ?3 LIMIT 64")?;
        let rows = st.query_map(params![request_id, identity, since], |r| {
            r.get::<_, Vec<u8>>(0)
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn request(
        &self,
        identity: &str,
        scope: &str,
        request_id: &str,
        since: i64,
    ) -> Result<Option<RequestRecord>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT ts, rev, result FROM requests WHERE identity = ?1 AND scope = ?2 AND request_id = ?3 AND ts >= ?4",
            )?
            .query_row(params![identity, scope, request_id, since], |r| {
                let rev: Option<i64> = r.get(1)?;
                Ok(RequestRecord {
                    identity: identity.to_owned(),
                    scope: scope.to_owned(),
                    request_id: request_id.to_owned(),
                    ts: r.get(0)?,
                    rev: rev.and_then(|v| Rev::try_from(v).ok()),
                    result: r.get(2)?,
                })
            })
            .optional()?)
    }

    /// Drops idempotency records older than `before` (ms since the epoch).
    pub fn prune_requests(&mut self, before: i64) -> Result<usize, StoreError> {
        Ok(self
            .conn
            .prepare_cached("DELETE FROM requests WHERE ts < ?1")?
            .execute([before])?)
    }

    /// The live connection's durability settings (R-STORE-7), as SQLite reports them:
    /// `(journal_mode, synchronous, fullfsync, checkpoint_fullfsync, wal_autocheckpoint)`.
    pub fn settings(&self) -> Result<(String, i64, i64, i64, i64), StoreError> {
        let q = |p: &str| -> Result<i64, StoreError> {
            Ok(self.conn.pragma_query_value(None, p, |r| r.get(0))?)
        };
        let mode: String = self
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        Ok((
            mode,
            q("synchronous")?,
            q("fullfsync")?,
            q("checkpoint_fullfsync")?,
            q("wal_autocheckpoint")?,
        ))
    }

    pub fn integrity_check(&self) -> Result<bool, StoreError> {
        let ok: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        Ok(ok == "ok")
    }

    /// The clean close: entity snapshot rows, a truncating WAL checkpoint, the
    /// connection closed, then the lock file emptied and released. Returns whether the
    /// checkpoint completed (everything power-durable); drop any [`Checkpointer`]
    /// first, or its connection may keep the truncation from finishing.
    pub fn close(mut self, doc: &Doc) -> Result<bool, StoreError> {
        self.checkpoint(doc)?;
        let saved = self.wal_checkpoint(true)?.is_some();
        let lock = self.lock.take();
        self.conn.close().map_err(|(_, e)| StoreError::from(e))?;
        if let Some(lock) = lock {
            lock.release()?;
        }
        Ok(saved)
    }

    /// Closes without writing entity snapshot rows (the op log stays the record and the
    /// next open replays it): the close for a document ahead of the op log, or a disk with
    /// no room for the rows. A truncating WAL checkpoint is tried; the lock is released.
    pub fn close_as_is(mut self) -> Result<bool, StoreError> {
        let saved = self.wal_checkpoint(true).ok().flatten().is_some();
        let lock = self.lock.take();
        self.conn.close().map_err(|(_, e)| StoreError::from(e))?;
        if let Some(lock) = lock {
            lock.release()?;
        }
        Ok(saved)
    }

    /// R-MODEL-3: begin schema epoch `k` (a migration): the full set of entity rows,
    /// kept with the named snapshot "epoch k start".
    pub fn begin_epoch(&mut self, doc: &Doc, k: u32, id: &str, ts: i64) -> Result<(), StoreError> {
        self.write_epoch(doc, id, &format!("epoch {k} start"), ts)
    }
}

fn meta_of(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

/// The document a database holds: entity snapshot rows, then every later op row replayed
/// in rev order (`on_op` sees each replayed transaction).
fn load_doc(
    conn: &Connection,
    on_op: &mut dyn FnMut(&Txn),
) -> Result<(Doc, Rev, usize), StoreError> {
    let mut doc = Doc::new();
    let checkpoint_rev: Rev = match meta_of(conn, "checkpoint_rev")? {
        Some(b) => cbor::from_slice(&b)?,
        None => 0,
    };
    {
        let mut stmt =
            conn.prepare("SELECT kind, id, rev, author, body FROM entities ORDER BY kind, id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let rev: i64 = r.get(2)?;
            let author: Vec<u8> = r.get(3)?;
            let body: Vec<u8> = r.get(4)?;
            let kind = gmclip_model::kinds::kind_by_name(&kind)
                .ok_or(StoreError::Corrupt("entity kind"))?;
            let id = Ulid::parse(&id).map_err(|_| StoreError::Corrupt("entity id"))?;
            let author: Author = cbor::from_slice(&author)?;
            let rev = Rev::try_from(rev).map_err(|_| StoreError::Corrupt("entity rev"))?;
            if rev > checkpoint_rev {
                return Err(StoreError::Corrupt("entity row newer than its checkpoint"));
            }
            doc.load_row(kind, id, rev, author, &body)?;
        }
    }
    if let Some(b) = meta_of(conn, "index_revs")? {
        let revs: Vec<(EntityId, EntityKind, Rev)> = cbor::from_slice(&b)?;
        doc.restore_index_revs(revs);
    }
    doc.set_rev(checkpoint_rev);
    let mut replayed = 0;
    let mut stmt = conn.prepare("SELECT rev, body FROM ops WHERE rev > ?1 ORDER BY rev")?;
    let mut rows =
        stmt.query([i64::try_from(checkpoint_rev).map_err(|_| StoreError::Corrupt("rev"))?])?;
    while let Some(r) = rows.next()? {
        let rev: i64 = r.get(0)?;
        let body: Vec<u8> = r.get(1)?;
        let t = Txn::from_cbor(&body)?;
        if i64::try_from(t.rev).ok() != Some(rev) || t.rev != doc.rev() + 1 {
            return Err(StoreError::Gap(doc.rev() + 1));
        }
        doc.apply(&t)?;
        on_op(&t);
        replayed += 1;
    }
    Ok((doc, checkpoint_rev, replayed))
}

/// A read-only view of a project database, for readers that are not its holder (the
/// library index, L.01): the document as of the last commit and the analysis blobs. It
/// takes no lock and writes nothing, so it can sit beside the live holder (WAL readers
/// never block the writer) or read a closed project.
#[derive(Debug)]
pub struct ReadOnlyStore {
    conn: Connection,
}

impl ReadOnlyStore {
    pub fn open(path: &Path) -> Result<ReadOnlyStore, StoreError> {
        use rusqlite::OpenFlags;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::TooNew(version));
        }
        Ok(ReadOnlyStore { conn })
    }

    /// The document as of the last commit.
    pub fn doc(&self) -> Result<Doc, StoreError> {
        // one read transaction: the entity rows and the ops agree
        self.conn.execute_batch("BEGIN")?;
        let r = load_doc(&self.conn, &mut |_| {});
        let _ = self.conn.execute_batch("COMMIT");
        Ok(r?.0)
    }

    /// One analysis blob, if stored.
    pub fn analysis(
        &self,
        owner: &str,
        kind: &str,
        revision: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT body FROM analysis WHERE owner = ?1 AND kind = ?2 AND revision = ?3",
            )?
            .query_row(params![owner, kind, revision], |r| r.get(0))
            .optional()?)
    }

    /// Every stored analysis key `(owner, kind, revision)`, in key order.
    pub fn analysis_keys(&self) -> Result<Vec<(String, String, String)>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT owner, kind, revision FROM analysis ORDER BY owner, kind, revision",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// A consistent copy of the project database at `src` written to `dest` (R-STORE-5):
/// `VACUUM INTO` on a read-only connection of its own, so the copy is one WAL snapshot
/// (a commit is either wholly in it or not at all) and the holder keeps writing. The copy
/// goes to `dest` + `.part` first and is renamed into place, so a crash or a full disk
/// never leaves a half-written backup under the real name. Refuses an existing `dest`.
pub fn backup_into(src: &Path, dest: &Path) -> Result<u64, StoreError> {
    use rusqlite::OpenFlags;
    if dest.exists() {
        return Err(StoreError::Invalid("a backup is already there"));
    }
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let _ = std::fs::remove_file(&part);
    let conn = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
    let to = part
        .to_str()
        .ok_or(StoreError::Invalid("the backup path is not UTF-8"))?;
    if let Err(e) = conn.execute("VACUUM INTO ?1", [to]) {
        let _ = std::fs::remove_file(&part);
        return Err(e.into());
    }
    drop(conn);
    let len = std::fs::metadata(&part)
        .map_err(|e| StoreError::io("backup", &e))?
        .len();
    std::fs::rename(&part, dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        StoreError::io("backup", &e)
    })?;
    Ok(len)
}

/// Writes a fresh project database at `path` holding `doc` as one checkpoint (entity rows,
/// no op rows, so no history) and the analysis blobs `(owner, kind, revision, body)` — a
/// consolidated copy (L.06), or such a copy adopted on another Mac. The file is left with
/// no WAL beside it, so it travels as one file. Refuses an existing file.
pub fn write_fresh(
    path: &Path,
    doc: &Doc,
    analysis: &[(String, String, String, Vec<u8>)],
) -> Result<(), StoreError> {
    if path.exists() {
        return Err(StoreError::Invalid("a database is already there"));
    }
    let mut conn = Connection::open(path)?;
    set_pragmas(&conn)?;
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    let rev = doc.rev();
    let tx = conn.transaction()?;
    for e in doc.entity_ids() {
        let Some(body) = doc.encode(e) else { continue };
        let r = doc.entity_rev(e).unwrap_or(rev).min(rev);
        let author = cbor::to_vec(&doc.last_author(e).unwrap_or_default());
        tx.prepare_cached(
            "INSERT INTO entities (kind, id, rev, author, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            e.kind.0,
            e.id.to_string(),
            i64::try_from(r).map_err(|_| StoreError::Invalid("rev too large"))?,
            author,
            body.0
        ])?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('index_revs', ?1)",
        [cbor::to_vec(&doc.index_revs())],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('checkpoint_rev', ?1)",
        [cbor::to_vec(&rev)],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('model_epoch', ?1)",
        [cbor::to_vec(&gmclip_model::MODEL_EPOCH)],
    )?;
    for (o, k, r, b) in analysis {
        tx.execute(
            "INSERT OR REPLACE INTO analysis (owner, kind, revision, body) VALUES (?1, ?2, ?3, ?4)",
            params![o, k, r, b],
        )?;
    }
    tx.commit()?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    let _: String = conn.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
    drop(conn);
    Ok(())
}
