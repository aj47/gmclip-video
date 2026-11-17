//! "Saved" (R-STORE-7): commits are crash-durable at once and power-durable at the next
//! WAL checkpoint with `checkpoint_fullfsync=ON`. The [`Checkpointer`] runs those
//! checkpoints on its own thread and connection, so the actor never waits on a full
//! sync: after a commit is reported it checkpoints once commits have been quiet for
//! 250 ms, and never later than 2 s after the first unsaved one (the debounce). A
//! checkpoint that cannot finish (a reader holds the WAL) is retried with backoff up to
//! 2 s and reported through [`Checkpointer::last_error`].

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gmclip_model::Rev;
use rusqlite::Connection;

use crate::StoreError;

const QUIET: Duration = Duration::from_millis(250);
const MAX_DELAY: Duration = Duration::from_secs(2);

enum Msg {
    Committed(Rev),
    Flush(mpsc::SyncSender<Option<Rev>>),
    Stop,
}

/// A background WAL checkpointer. `saved()` is the highest rev known power-durable.
#[derive(Debug)]
pub struct Checkpointer {
    tx: Sender<Msg>,
    saved: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
    join: Option<JoinHandle<()>>,
}

impl Checkpointer {
    /// `saved_at_open`: the rev already power-durable when the store opened.
    pub(crate) fn start(db: &Path, saved_at_open: Rev) -> Result<Self, StoreError> {
        let conn = Connection::open(db)?;
        // synchronous stays FULL (the default) on this connection: the checkpoint's
        // syncs are the durability point, with F_FULLFSYNC on macOS
        conn.pragma_update(None, "checkpoint_fullfsync", "ON")?;
        conn.pragma_update(None, "fullfsync", "OFF")?;
        conn.busy_timeout(Duration::from_millis(100))?;
        let (tx, rx) = mpsc::channel::<Msg>();
        let saved = Arc::new(AtomicU64::new(saved_at_open));
        let last_error = Arc::new(Mutex::new(None));
        let (s2, e2) = (Arc::clone(&saved), Arc::clone(&last_error));
        let join = std::thread::Builder::new()
            .name("gmclip-saver".into())
            .spawn(move || run(&conn, &rx, &s2, &e2))
            .map_err(|e| StoreError::io("spawn saver", &e))?;
        Ok(Self {
            tx,
            saved,
            last_error,
            join: Some(join),
        })
    }

    /// Reports a committed rev; a checkpoint follows within the debounce.
    pub fn committed(&self, rev: Rev) {
        let _ = self.tx.send(Msg::Committed(rev));
    }

    /// Checkpoints now and waits; returns the rev now power-durable.
    pub fn flush(&self) -> Option<Rev> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(Msg::Flush(tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// The highest rev known to be power-durable ("Saved").
    pub fn saved(&self) -> Rev {
        self.saved.load(Ordering::Acquire)
    }

    /// Why the most recent checkpoint did not finish (shape-only), if it did not — the
    /// UI's "Not saved: …" state.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn checkpoint(
    conn: &Connection,
    upto: Rev,
    saved: &AtomicU64,
    err: &Mutex<Option<String>>,
) -> Option<Rev> {
    let res: rusqlite::Result<(i64, i64, i64)> =
        conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        });
    let set = |v: Option<String>| {
        if let Ok(mut g) = err.lock() {
            *g = v;
        }
    };
    match res {
        Ok((0, log, done)) if log == done => {
            saved.fetch_max(upto, Ordering::AcqRel);
            set(None);
            Some(upto)
        }
        Ok(_) => {
            set(Some("checkpoint blocked by a reader".into()));
            None
        }
        Err(e) => {
            set(Some(StoreError::from(e).to_string()));
            None
        }
    }
}

fn run(
    conn: &Connection,
    rx: &mpsc::Receiver<Msg>,
    saved: &AtomicU64,
    err: &Mutex<Option<String>>,
) {
    // (highest unsaved rev, first unsaved commit, last commit); `retry` = when to try
    // again after a checkpoint that could not finish, and the current backoff.
    let mut pending: Option<(Rev, Instant, Instant)> = None;
    let mut retry: Option<(Instant, Duration)> = None;
    loop {
        let deadline = match (pending, retry) {
            (None, _) => None,
            (Some(_), Some((at, _))) => Some(at),
            (Some((_, first, last)), None) => Some((last + QUIET).min(first + MAX_DELAY)),
        };
        let timeout = deadline.map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match rx.recv_timeout(timeout) {
            Ok(Msg::Committed(rev)) => {
                let now = Instant::now();
                pending = Some(match pending {
                    None => (rev, now, now),
                    Some((r, first, _)) => (r.max(rev), first, now),
                });
            }
            Ok(Msg::Flush(reply)) => {
                let upto = pending.map_or_else(|| saved.load(Ordering::Acquire), |p| p.0);
                let r = checkpoint(conn, upto, saved, err);
                if r.is_some() {
                    pending = None;
                    retry = None;
                }
                let _ = reply.send(r);
            }
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                if let Some((rev, ..)) = pending {
                    checkpoint(conn, rev, saved, err);
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                let Some((rev, ..)) = pending else { continue };
                if checkpoint(conn, rev, saved, err).is_some() {
                    pending = None;
                    retry = None;
                } else {
                    let back = retry.map_or(QUIET, |(_, b)| (b * 2).min(MAX_DELAY));
                    retry = Some((Instant::now() + back, back));
                }
            }
        }
    }
}
