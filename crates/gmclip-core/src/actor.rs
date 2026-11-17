//! The project actor (SPEC §3.11): one OS thread per open project owns the `Doc`, the
//! store, runs, undo stacks and the event hub. Callers reach it through a
//! [`ProjectHandle`]: `call` sends a [`CallRequest`] and waits for the reply; readers
//! use `snapshot()`, an `Arc<Snapshot>` swapped in after each commit (`ArcSwap`), so the
//! render thread and planners never wait on the actor.
//!
//! A mutation goes: scope check (R-OWN/§11.4) → idempotency lookup (R-CMD-7) → `base`
//! required for agents (R-CC-1) → handler on a tracking `Tx` (panics caught, R-ARCH)
//! → `base` checked against the recorded read set → implicit run bookkeeping (§3.6) →
//! one SQLite commit of the op row and idempotency record (R-STORE-7) → apply to the
//! `Doc`, publish the snapshot and one event (§3.10) → undo stacks. Entity snapshot
//! rows are written after 500 ops or 30 s idle; the checkpointer makes commits
//! power-durable within 2 s.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gmclip_model::entities::Cut;
use gmclip_model::{
    Author, CommandKey, Doc, EntityId, FieldPath, Id, Op, ReadRef, Rev, RunId, Staging, Txn, TxnId,
};
use gmclip_proto::{
    Base, CallerKind, CmdError, Conflict, ConflictEntity, ConflictOption, Event, EventTriple,
};
use gmclip_store::{
    Checkpointer, OpenOptions as StoreOpen, RequestRecord, SnapshotKind, SnapshotRecord, Store,
    StoreError,
};
use serde_json::{Value, json};

use crate::command::{AnalysisView, Caller, Cx, Services, Tx, UndoClass};
use crate::commands::history::{
    HistoryResult, RestorePointArgs, RestorePointMade, RestoreToArgs, RunBeginArgs, RunBegun,
    RunEnded, RunUndoArgs, UndoArgs, UndoOption,
};
use crate::events::{EventHub, Subscription};
use crate::planner::heard::KIND as HEARD_KIND;
use crate::registry;
use crate::services::{Clock, SystemClock, UlidGen};

/// Entity snapshot rows after this many ops (SPEC §4.4).
pub const CHECKPOINT_OPS: u64 = 500;
/// … or after this long idle.
pub const CHECKPOINT_IDLE: Duration = Duration::from_secs(30);
/// An implicit run ends after this long with no call (SPEC §3.6).
pub const RUN_IDLE_MS: i64 = 15 * 60 * 1000;
/// Idempotency window (R-CMD-7).
pub const REQUEST_WINDOW_MS: i64 = 24 * 3600 * 1000;

/// What readers see: the document at the last commit, and the rev known saved.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub doc: Doc,
    /// The transcripts the store holds (shared, read-only).
    pub analysis: Arc<AnalysisView>,
}

/// An agent run that is open now (R-CC-4 until M4: "agent working on this cut").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OpenRun {
    pub run: String,
    pub identity: String,
    /// The cut it writes (`None` until its first write).
    pub cut: Option<String>,
    pub last_ms: i64,
}

/// A call to the actor.
#[derive(Debug, Clone)]
pub struct CallRequest {
    pub command: String,
    pub args: Value,
    pub base: Option<Base>,
    pub request_id: Option<String>,
    pub caller: Caller,
}

/// A successful call.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOk {
    pub output: Value,
    /// The document rev after the call (every read's `display` footer, R-CC-1).
    pub as_of: Rev,
    /// The rev this call committed, if it committed.
    pub committed: Option<Rev>,
    /// True when this is the stored result of an earlier identical request (R-CMD-7).
    pub replayed: bool,
}

/// Options for opening a project.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub store: StoreOpen,
    pub clock: Arc<dyn Clock>,
    /// Seeds the ULID minter (tests); random when `None`.
    pub seed: Option<u64>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            store: StoreOpen::default(),
            clock: Arc::new(SystemClock),
            seed: None,
        }
    }
}

type Reply<T> = mpsc::SyncSender<T>;

enum Msg {
    Call(Box<CallRequest>, Reply<Result<CallOk, CmdError>>),
    Subscribe(Reply<Subscription>),
    Watch(u64, usize, Reply<Option<Vec<Event>>>),
    Saved(Reply<(Rev, Option<String>)>),
    Flush(Reply<Option<Rev>>),
    Close(Reply<Result<bool, StoreError>>),
    PutAnalysis(
        Box<(String, String, String, Vec<u8>)>,
        Reply<Result<(), StoreError>>,
    ),
    GetAnalysis(Box<(String, String, String)>, Reply<Option<Vec<u8>>>),
    AnalysisKeys(Reply<Vec<(String, String, String)>>),
    /// The runs open now (not ended, not idle past `RUN_IDLE_MS`).
    OpenRuns(Reply<Vec<OpenRun>>),
    /// R-GEST-1 / R-CC-2: a human gesture holds these entities until it ends.
    GestureBegin(Vec<EntityId>, Reply<u64>),
    GestureEnd(u64),
    /// Fault injection: cap the store at its current size (a full disk) or lift the cap.
    DiskFull(bool, Reply<Result<(), StoreError>>),
}

/// A handle to an open project (cheap to clone).
#[derive(Clone)]
pub struct ProjectHandle {
    tx: Sender<Msg>,
    snap: Arc<ArcSwap<Snapshot>>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for ProjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectHandle")
            .field("rev", &self.snapshot().doc.rev())
            .finish()
    }
}

/// An owner's voice levels, for where its words are heard (decision 0048): its
/// `energy_10ms` and `silences_40ms` blobs when it has exactly one of each (one analysed
/// track, so they are the voice its transcripts were made from).
fn load_voice(store: &Store, view: &mut AnalysisView, owner: &str) -> bool {
    let keys = store.analysis_keys().unwrap_or_default();
    let one = |kind: &str| {
        let mut it = keys.iter().filter(|k| k.0 == owner && k.1 == kind);
        match (it.next(), it.next()) {
            (Some(k), None) => store.analysis(owner, kind, &k.2).ok().flatten(),
            _ => None,
        }
    };
    match (one("energy_10ms"), one("silences_40ms")) {
        (Some(e), Some(s)) => view.insert_voice(owner, &e, &s),
        _ => false,
    }
}

fn gone() -> CmdError {
    CmdError::Internal {
        trace_id: "actor-gone".into(),
    }
}

impl ProjectHandle {
    /// Opens the project database at `path` and starts its actor thread.
    pub fn open(path: &Path, opts: OpenOptions) -> Result<ProjectHandle, StoreError> {
        let (store, doc, _) = Store::open(path, &opts.store)?;
        let saver = store.checkpointer()?;
        // transcripts already stored (imports, earlier ASR jobs)
        let mut view = AnalysisView::default();
        if let Ok(keys) = store.analysis_keys() {
            for (owner, kind, rev) in keys.iter().filter(|k| k.1 == "words") {
                let _ = kind;
                if let Ok(Some(w)) = store.analysis(owner, "words", rev) {
                    let lines = store.analysis(owner, "lines", rev).ok().flatten();
                    view.insert_bodies(owner, &w, lines.as_deref());
                }
            }
            // where their words are heard, on each owner's voice (decision 0048)
            let voiced: std::collections::BTreeSet<&String> = keys
                .iter()
                .filter(|k| k.1 == "energy_10ms")
                .map(|k| &k.0)
                .collect();
            for owner in voiced {
                load_voice(&store, &mut view, owner);
            }
            // words heard from cuts' own audio (SPEC §5.10, decision 0022)
            for (owner, _, rev) in keys.iter().filter(|k| k.1 == HEARD_KIND) {
                if let Ok(Some(b)) = store.analysis(owner, HEARD_KIND, rev) {
                    view.insert_heard(&b);
                }
            }
        }
        let analysis = Arc::new(view);
        // R-HIST-1: the human's own stack survives a reopen (the first ⌘Z asks once)
        let mut stacks: HashMap<Author, Stack> = HashMap::new();
        if let Ok(Some(b)) = store.get_meta(HUMAN_STACK_KEY)
            && let Ok(st) = serde_json::from_slice::<Stack>(&b)
        {
            stacks.insert(Author::Human, st);
        }
        let snap = Arc::new(ArcSwap::from_pointee(Snapshot {
            doc: doc.clone(),
            analysis: Arc::clone(&analysis),
        }));
        let (tx, rx) = mpsc::channel();
        let open_rev = doc.rev();
        let actor = Actor {
            doc,
            store: Some(store),
            saver: Some(saver),
            snap: Arc::clone(&snap),
            ids: UlidGen::new(opts.seed),
            clock: opts.clock,
            open_rev,
            txns: Vec::new(),
            stacks,
            earlier_ok: false,
            runs: BTreeMap::new(),
            conn_runs: HashMap::new(),
            requests: HashMap::new(),
            poisoned: false,
            events: EventHub::default(),
            last_commit: Instant::now(),
            analysis,
            gestures: BTreeMap::new(),
            next_gesture: 1,
            rp_ops: 0,
            rp_last_ms: 0,
            unsaved: Vec::new(),
        };
        let join = std::thread::Builder::new()
            .name("gmclip-project".into())
            .spawn(move || actor.run(&rx))
            .map_err(|e| StoreError::Io("spawn actor", e.kind().to_string()))?;
        Ok(ProjectHandle {
            tx,
            snap,
            join: Arc::new(Mutex::new(Some(join))),
        })
    }

    /// Runs a command and waits for its result.
    pub fn call(&self, req: CallRequest) -> Result<CallOk, CmdError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::Call(Box::new(req), tx))
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }

    /// R-GEST-1: a human gesture starts on `entities` (pointer down on a gizmo); until
    /// [`ProjectHandle::gesture_end`], agent writes to them get `Conflict{by: human}`.
    /// Previews stay in the render thread (never logged); the release commits one
    /// ordinary command as the human; Esc commits nothing. Not a registered command: no
    /// agent surface can reach it.
    pub fn gesture_begin(&self, entities: Vec<EntityId>) -> Result<u64, CmdError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::GestureBegin(entities, tx))
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())
    }

    /// Ends a gesture (after its commit, or on cancel).
    pub fn gesture_end(&self, id: u64) {
        let _ = self.tx.send(Msg::GestureEnd(id));
    }

    /// Convenience: `call` as the local human with no base.
    pub fn human(&self, command: &str, args: Value) -> Result<CallOk, CmdError> {
        self.call(CallRequest {
            command: command.into(),
            args,
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
    }

    /// Stores an immutable analysis blob `(owner, kind, revision)` (SPEC §4.4).
    pub fn put_analysis(
        &self,
        owner: &str,
        kind: &str,
        revision: &str,
        body: Vec<u8>,
    ) -> Result<(), StoreError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::PutAnalysis(
                Box::new((owner.into(), kind.into(), revision.into(), body)),
                tx,
            ))
            .map_err(|_| StoreError::Invalid("closed"))?;
        rx.recv().map_err(|_| StoreError::Invalid("closed"))?
    }

    /// One stored analysis blob.
    pub fn analysis(&self, owner: &str, kind: &str, revision: &str) -> Option<Vec<u8>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::GetAnalysis(
                Box::new((owner.into(), kind.into(), revision.into())),
                tx,
            ))
            .ok()?;
        rx.recv().ok().flatten()
    }

    /// Every stored analysis key.
    pub fn analysis_keys(&self) -> Vec<(String, String, String)> {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.tx.send(Msg::AnalysisKeys(tx)).is_err() {
            return Vec::new();
        }
        rx.recv().unwrap_or_default()
    }

    /// The agent runs open now: not ended and called within `RUN_IDLE_MS` (R-CC-4: the
    /// app shows "agent working on this cut" for their cuts).
    pub fn open_runs(&self) -> Vec<OpenRun> {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.tx.send(Msg::OpenRuns(tx)).is_err() {
            return Vec::new();
        }
        rx.recv().unwrap_or_default()
    }

    /// The document at the last commit (lock-free).
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.load_full()
    }

    /// A bounded event queue (§3.10).
    pub fn subscribe(&self) -> Option<Subscription> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(Msg::Subscribe(tx)).ok()?;
        rx.recv().ok()
    }

    /// `project_watch{since, max}`; `None` = too old, re-read (resync).
    pub fn watch(&self, since: u64, max: usize) -> Option<Vec<Event>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(Msg::Watch(since, max, tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// The rev known power-durable ("Saved") and the last save error.
    pub fn saved(&self) -> (Rev, Option<String>) {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.tx.send(Msg::Saved(tx)).is_err() {
            return (0, Some("closed".into()));
        }
        rx.recv().unwrap_or((0, Some("closed".into())))
    }

    /// Fault injection for R-ERR-2 tests and the dev bridge: `true` makes the project's
    /// database behave as on a full disk (writes needing space fail with `SQLITE_FULL`),
    /// `false` gives the space back. Edits made meanwhile stay in the document, show
    /// "Not saved: disk full" and are written in order once space returns.
    #[doc(hidden)]
    pub fn simulate_disk_full(&self, on: bool) -> Result<(), StoreError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::DiskFull(on, tx))
            .map_err(|_| StoreError::Invalid("closed"))?;
        rx.recv().map_err(|_| StoreError::Invalid("closed"))?
    }

    /// Makes everything committed power-durable now.
    pub fn flush(&self) -> Option<Rev> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(Msg::Flush(tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// Clean close: entity snapshot rows, WAL checkpoint, lock released. Returns whether
    /// the final checkpoint completed.
    pub fn close(&self) -> Result<bool, StoreError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::Close(tx))
            .map_err(|_| StoreError::Invalid("already closed"))?;
        let r = rx
            .recv()
            .map_err(|_| StoreError::Invalid("already closed"))?;
        if let Some(j) = self.join.lock().ok().and_then(|mut g| g.take()) {
            let _ = j.join();
        }
        r
    }
}

/// Undo/redo stacks of one author. The human's is kept in the project (R-HIST-1); an
/// agent's lives with its run.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Stack {
    undo: Vec<Rev>,
    redo: Vec<Rev>,
}

/// A run (SPEC §3.6). Persisted with the project, so a run can be undone after a
/// reopen (decision 0040).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RunState {
    id: RunId,
    identity: String,
    connection: String,
    label: String,
    restore_point: String,
    restore_rev: Rev,
    cut: Option<Id<Cut>>,
    txns: Vec<Rev>,
    #[serde(skip)]
    last_ms: i64,
    ended: bool,
}

/// One `(entity, field path)` or a whole entity (`path: None`, create/delete).
type PathKey = (EntityId, Option<FieldPath>);

fn op_paths(op: &Op) -> PathKey {
    (op.entity(), op.path().cloned())
}

/// Do two path keys overlap (same entity, and one is whole-entity or same path)?
fn overlaps(a: &PathKey, b: &PathKey) -> bool {
    a.0 == b.0 && (a.1.is_none() || b.1.is_none() || a.1 == b.1)
}

fn label(k: &PathKey) -> String {
    match &k.1 {
        None => k.0.to_string(),
        Some(p) => {
            let name = gmclip_model::kinds::kind_ops(k.0.kind)
                .and_then(|o| o.fields().get(usize::from(p.field.0)).map(|d| d.name))
                .unwrap_or("?");
            match &p.key {
                Some(key) => format!("{}.{name}[{key}]", k.0),
                None => format!("{}.{name}", k.0),
            }
        }
    }
}

fn internal(tag: &str) -> CmdError {
    CmdError::Internal {
        trace_id: tag.into(),
    }
}

fn store_err(e: &StoreError) -> CmdError {
    CmdError::Internal {
        trace_id: format!(
            "store:{}",
            e.to_string().chars().take(40).collect::<String>()
        ),
    }
}

/// The actor's state (one thread).
struct Actor {
    doc: Doc,
    store: Option<Store>,
    saver: Option<Checkpointer>,
    snap: Arc<ArcSwap<Snapshot>>,
    ids: UlidGen,
    clock: Arc<dyn Clock>,
    /// Rev at open; `txns` holds every transaction committed since.
    open_rev: Rev,
    txns: Vec<Txn>,
    stacks: HashMap<Author, Stack>,
    /// R-HIST-1: the human said yes to undoing an edit from an earlier session (asked
    /// once per session).
    earlier_ok: bool,
    runs: BTreeMap<RunId, RunState>,
    conn_runs: HashMap<String, RunId>,
    requests: HashMap<ReqKey, (i64, String, CallOk)>,
    /// Set when a panic escaped a call mid-commit: the actor refuses further writes.
    poisoned: bool,
    events: EventHub,
    last_commit: Instant,
    analysis: Arc<AnalysisView>,
    /// Open human gestures: id → the entities they hold (R-CC-2: agent writes to them
    /// get `Conflict{by: human}`).
    gestures: BTreeMap<u64, Vec<EntityId>>,
    next_gesture: u64,
    /// Restore points coalesce (A2.13): one after 20 human edits or 5 minutes of them.
    rp_ops: u32,
    rp_last_ms: i64,
    /// R-ERR-2: transactions applied to the document whose op rows could not be written
    /// (the disk is full), oldest first, with their idempotency records. Retried every
    /// second and before any other write; never dropped while the project is open.
    unsaved: Vec<(Txn, Option<RequestRecord>)>,
}

/// What `saved()` reports while edits wait for space (the UI shows "Not saved: disk full").
pub const DISK_FULL: &str = "disk full";

/// Where the human's undo/redo stack is kept between sessions (R-HIST-1).
const HUMAN_STACK_KEY: &str = "app:stack:human";
/// At most this many entries of each stack are kept between sessions.
const STACK_KEEP: usize = 1_000;
/// Who wrote a system transaction on behalf of someone (`app:by:<rev>`): a review link.
const BY_PREFIX: &str = "app:by:";

/// A restore point is taken after this many human edits …
const RP_OPS: u32 = 20;
/// … or this long after the last one, whichever comes first.
const RP_MS: i64 = 5 * 60 * 1000;

/// `(identity, connection-or-run id, request_id)` (R-CMD-7).
type ReqKey = (String, String, String);

/// A request's fingerprint: the command and a hash of its canonical JSON arguments, so
/// a request id reused for a different call is refused rather than answered with the
/// first call's result.
fn fingerprint(req: &CallRequest) -> String {
    use std::hash::{Hash, Hasher};
    // serde_json maps are sorted (no preserve_order), so the text is canonical
    let text = serde_json::to_string(&req.args).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{}:{}:{:016x}", req.command, text.len(), h.finish())
}

/// What a handler (or history operation) produced, before commit.
struct Staged {
    ops: Vec<Op>,
    reads: Vec<ReadRef>,
    output: Value,
    reverts: Option<TxnId>,
    /// For undo/redo: whose stack, and whether it was a redo. Moved only after the
    /// commit succeeds and only when something committed.
    stack: Option<(Author, bool)>,
}

impl Actor {
    fn run(mut self, rx: &Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Msg::Call(req, reply)) => {
                    let r = if self.poisoned {
                        Err(internal("actor-poisoned: reopen the project"))
                    } else {
                        let rev = self.doc.rev();
                        let srev = self.store.as_ref().map_or(0, Store::rev);
                        match std::panic::catch_unwind(AssertUnwindSafe(|| self.call(*req))) {
                            Ok(r) => r,
                            Err(_) => {
                                // a panic between the SQLite commit and the in-memory apply
                                // would leave them apart: stop writing until reopened
                                if self.store.as_ref().is_some_and(|s| s.rev() != srev)
                                    || self.doc.rev() != rev
                                {
                                    self.poisoned = true;
                                }
                                Err(internal("call-panic"))
                            }
                        }
                    };
                    let _ = reply.send(r);
                    self.maybe_checkpoint(false);
                }
                Ok(Msg::Subscribe(reply)) => {
                    let _ = reply.send(self.events.subscribe());
                }
                Ok(Msg::Watch(since, max, reply)) => {
                    let _ = reply.send(self.events.since(since, max));
                }
                Ok(Msg::Saved(reply)) => {
                    self.flush_unsaved();
                    let s = self
                        .saver
                        .as_ref()
                        .map_or((0, None), |s| (s.saved(), s.last_error()));
                    let s = if self.unsaved.is_empty() {
                        s
                    } else {
                        (s.0, Some(DISK_FULL.to_owned()))
                    };
                    let _ = reply.send(s);
                }
                Ok(Msg::DiskFull(on, reply)) => {
                    let r = match self.store.as_mut() {
                        Some(s) => s.simulate_disk_full(on),
                        None => Err(StoreError::Invalid("closed")),
                    };
                    if !on {
                        self.flush_unsaved();
                    }
                    let _ = reply.send(r);
                }
                Ok(Msg::Flush(reply)) => {
                    let _ = reply.send(self.saver.as_ref().and_then(Checkpointer::flush));
                }
                Ok(Msg::PutAnalysis(k, reply)) => {
                    let (owner, kind, rev, body) = *k;
                    let _ = reply.send(self.put_analysis(&owner, &kind, &rev, &body));
                }
                Ok(Msg::GetAnalysis(k, reply)) => {
                    let (owner, kind, rev) = *k;
                    let _ = reply.send(
                        self.store
                            .as_ref()
                            .and_then(|s| s.analysis(&owner, &kind, &rev).ok().flatten()),
                    );
                }
                Ok(Msg::GestureBegin(es, reply)) => {
                    let id = self.next_gesture;
                    self.next_gesture += 1;
                    self.gestures.insert(id, es);
                    let _ = reply.send(id);
                }
                Ok(Msg::GestureEnd(id)) => {
                    self.gestures.remove(&id);
                }
                Ok(Msg::AnalysisKeys(reply)) => {
                    let _ = reply.send(
                        self.store
                            .as_ref()
                            .and_then(|s| s.analysis_keys().ok())
                            .unwrap_or_default(),
                    );
                }
                Ok(Msg::OpenRuns(reply)) => {
                    let now = self.clock.now_ms();
                    let _ = reply.send(
                        self.runs
                            .values()
                            .filter(|r| !r.ended && now - r.last_ms < RUN_IDLE_MS)
                            .map(|r| OpenRun {
                                run: r.id.0.to_string(),
                                identity: r.identity.clone(),
                                cut: r.cut.map(|c| c.0.to_string()),
                                last_ms: r.last_ms,
                            })
                            .collect(),
                    );
                }
                Ok(Msg::Close(reply)) => {
                    let _ = reply.send(self.close());
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {
                    // R-ERR-2: edits waiting for space are retried every second
                    self.flush_unsaved();
                    self.maybe_checkpoint(true);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.close();
                    return;
                }
            }
        }
    }

    fn close(&mut self) -> Result<bool, StoreError> {
        self.events.close();
        for r in self.runs.values_mut() {
            r.ended = true;
        }
        self.flush_unsaved();
        self.persist_runs();
        drop(self.saver.take());
        if !self.unsaved.is_empty() {
            // still no space: close as the op log stands (entity rows must agree with
            // it, and there is no room for them) and say how many edits could not be
            // written
            let n = self.unsaved.len();
            if let Some(s) = self.store.take() {
                s.close_as_is()?;
            }
            return Err(StoreError::Io(
                "close",
                format!("{DISK_FULL}: {n} edit(s) could not be written"),
            ));
        }
        match self.store.take() {
            Some(s) => s.close(&self.doc),
            None => Ok(true),
        }
    }

    /// Writes the edits that waited for space, oldest first; true when none wait. A
    /// failure keeps them (R-ERR-2: never discarded) for the next try.
    fn flush_unsaved(&mut self) -> bool {
        while !self.unsaved.is_empty() {
            let Some(store) = self.store.as_mut() else {
                return false;
            };
            let (t, r) = &self.unsaved[0];
            if store.commit_with(Some(t), r.as_ref()).is_err() {
                return false;
            }
            let rev = t.rev;
            self.unsaved.remove(0);
            if let Some(s) = &self.saver {
                s.committed(rev);
            }
        }
        true
    }

    /// Stores an analysis blob; a transcript (`words`, `lines`) also enters the view and
    /// a new snapshot is published.
    fn put_analysis(
        &mut self,
        owner: &str,
        kind: &str,
        rev: &str,
        body: &[u8],
    ) -> Result<(), StoreError> {
        let store = self.store.as_mut().ok_or(StoreError::Invalid("closed"))?;
        store.put_analysis(owner, kind, rev, body)?;
        if kind == HEARD_KIND {
            let mut view = (*self.analysis).clone();
            if view.insert_heard(body) {
                self.analysis = Arc::new(view);
                self.snap.store(Arc::new(Snapshot {
                    doc: self.doc.clone(),
                    analysis: Arc::clone(&self.analysis),
                }));
            }
        }
        if kind == "energy_10ms" || kind == "silences_40ms" {
            let mut view = (*self.analysis).clone();
            if load_voice(store, &mut view, owner) {
                self.analysis = Arc::new(view);
                self.snap.store(Arc::new(Snapshot {
                    doc: self.doc.clone(),
                    analysis: Arc::clone(&self.analysis),
                }));
            }
        }
        if kind == "words" || kind == "lines" {
            let words = store.analysis(owner, "words", rev)?;
            let lines = store.analysis(owner, "lines", rev)?;
            if let Some(w) = words {
                let mut view = (*self.analysis).clone();
                if view.insert_bodies(owner, &w, lines.as_deref()) {
                    self.analysis = Arc::new(view);
                    self.snap.store(Arc::new(Snapshot {
                        doc: self.doc.clone(),
                        analysis: Arc::clone(&self.analysis),
                    }));
                }
            }
        }
        Ok(())
    }

    fn maybe_checkpoint(&mut self, idle_tick: bool) {
        // entity rows must agree with the op log: none while edits wait for space
        if !self.unsaved.is_empty() {
            return;
        }
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let n = store.ops_since_checkpoint();
        if n >= CHECKPOINT_OPS
            || (idle_tick && n > 0 && self.last_commit.elapsed() >= CHECKPOINT_IDLE)
        {
            // a failed checkpoint is retried on the next trigger; the op log is the record
            let _ = store.checkpoint(&self.doc);
        }
    }

    fn store(&mut self) -> Result<&mut Store, CmdError> {
        self.store.as_mut().ok_or_else(|| internal("closed"))
    }

    // ---------------------------------------------------------------------------------
    // The call pipeline
    // ---------------------------------------------------------------------------------

    fn call(&mut self, req: CallRequest) -> Result<CallOk, CmdError> {
        let cmd = registry::find(&req.command).ok_or(CmdError::NotFound)?;
        let meta = cmd.meta();
        if !req.caller.scopes.contains(&meta.scope) {
            return Err(CmdError::Unauthorized {
                scope: meta.scope.name().into(),
            });
        }
        let mutating = meta.undo != UndoClass::ReadOnly;
        let now = self.clock.now_ms();
        // the feedback log (R-PROF-2) and the Clippy hand-off (R-CLIP-1) are a human's,
        // whatever an agent's scopes; the MCP layer refuses them first, this holds for
        // every other path into the actor
        if req.caller.kind == CallerKind::Agent && registry::human_only(meta.name) {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        // R-CMD-7: a repeat within 24 h returns the original result
        let key: Option<ReqKey> = req.request_id.as_ref().map(|rid| {
            let scope = req
                .caller
                .run
                .map_or_else(|| req.caller.connection.clone(), |r| format!("run:{}", r.0));
            (req.caller.identity.clone(), scope, rid.clone())
        });
        let fp = fingerprint(&req);
        if let Some(k) = &key {
            if k.2.is_empty() || k.2.len() > 128 {
                return Err(CmdError::invalid("/request_id", "1..=128 bytes"));
            }
            if let Some(hit) = self.lookup_request(k, &fp, now)? {
                return Ok(hit);
            }
        }
        // the writing history commands below are idempotent too, so `undo_class`
        // read-only is not the test for "needs base"
        let writes = mutating
            || matches!(
                meta.name,
                "run_begin" | "run_end" | "restore_point_create" | "history_list"
            );
        if writes && req.caller.kind == CallerKind::Agent {
            match &req.base {
                None => {
                    return Err(CmdError::invalid(
                        "/base",
                        "mutations need `base` ({entity: rev} or {as_of})",
                    ));
                }
                Some(Base::Revs(m)) if m.is_empty() => {
                    return Err(CmdError::invalid("/base", "an empty base checks nothing"));
                }
                Some(_) => {}
            }
        }
        if req.caller.kind == CallerKind::Agent && req.caller.run.is_none() {
            self.expire_run(&req.caller.connection, now);
        }

        // history commands run on the actor itself
        let early = match meta.name {
            "run_begin" => Some(self.run_begin_cmd(&req, now)),
            "run_end" => Some(self.run_end_cmd(&req)),
            "restore_point_create" => Some(self.restore_point_cmd(&req, now)),
            "history_list" => Some(self.history_list_cmd(&req)),
            "history_diff" => Some(self.history_diff_cmd(&req)),
            _ => None,
        };
        if let Some(r) = early {
            let ok = r?;
            if let Some(k) = &key {
                self.remember(k, &fp, now, None, &ok)?;
            }
            return Ok(ok);
        }
        let staged = match meta.name {
            "undo" | "redo" => self.undo_redo(&req, meta.name == "redo")?,
            "run_undo" => self.run_undo(&req, now)?,
            "restore_to" => self.restore_to(&req)?,
            "run_summary_set" if req.caller.kind == CallerKind::Agent => {
                let req = self.with_own_run(&req)?;
                self.handle(cmd, &req, now)?
            }
            _ => self.handle(cmd, &req, now)?,
        };
        // before the base check, so a `Conflict` never reports another cut's revisions
        self.check_read_binding(&req.caller, &staged.reads)?;
        if let Some(base) = &req.base {
            self.check_base(base, &staged.reads)?;
        }
        if staged.ops.is_empty() {
            let ok = CallOk {
                output: staged.output,
                as_of: self.doc.rev(),
                committed: None,
                replayed: false,
            };
            if let (Some(k), true) = (&key, mutating) {
                self.remember(k, &fp, now, None, &ok)?;
            }
            return Ok(ok);
        }
        let author = self.author_for(&req, &staged.ops, now)?;
        let rev = self.doc.rev() + 1;
        self.ids.at(now);
        let mut output = staged.output;
        if staged.stack.is_some()
            && let Some(o) = output.as_object_mut()
        {
            o.insert("rev".into(), json!(rev));
        }
        let txn = Txn {
            rev,
            id: TxnId(self.ids.next_ulid()),
            author,
            run: match author {
                Author::Run(r) => Some(r),
                _ => None,
            },
            command: CommandKey {
                name: meta.name.into(),
                version: meta.version,
            },
            request_id: req.request_id.clone().map(gmclip_model::RequestId),
            reverts: staged.reverts,
            reads: staged.reads,
            ops: staged.ops,
            ts: now,
        };
        let ok = CallOk {
            output,
            as_of: rev,
            committed: Some(rev),
            replayed: false,
        };
        self.commit(txn, key.as_ref().map(|k| (k, fp.as_str(), now, &ok)))?;
        match staged.stack {
            // undo/redo: the target moves to the other stack as the new transaction
            Some((who, redo)) => {
                let st = self.stacks.entry(who).or_default();
                if redo {
                    st.redo.pop();
                    st.undo.push(rev);
                } else {
                    st.undo.pop();
                    st.redo.push(rev);
                }
            }
            // a normal edit goes on its author's undo stack and clears redo
            None => {
                let st = self.stacks.entry(author).or_default();
                st.undo.push(rev);
                st.redo.clear();
            }
        }
        let stack_owner = staged.stack.map_or(author, |(who, _)| who);
        if stack_owner == Author::Human {
            self.persist_human_stack();
        }
        // a link's writes run as the system; keep who it was for the history (A2.13)
        if req.caller.kind == CallerKind::System
            && req.caller.identity != "system"
            && req.caller.identity.len() <= 200
            && let Some(s) = self.store.as_mut()
        {
            let _ = s.put_meta(
                &format!("{BY_PREFIX}{rev:020}"),
                req.caller.identity.as_bytes(),
            );
        }
        // coalesced restore points for the human's edits (A2.13)
        if author == Author::Human {
            if self.rp_last_ms == 0 {
                self.rp_last_ms = now;
            }
            self.rp_ops += 1;
            if self.rp_ops >= RP_OPS || now - self.rp_last_ms >= RP_MS {
                let _ = self.snapshot_now(SnapshotKind::RestorePoint, None, None, now);
                self.rp_ops = 0;
                self.rp_last_ms = now;
            }
        }
        Ok(ok)
    }

    fn handle(
        &mut self,
        cmd: &'static dyn registry::ErasedCommand,
        req: &CallRequest,
        now: i64,
    ) -> Result<Staged, CmdError> {
        self.ids.at(now);
        let base = self.doc.clone();
        let author = match req.caller.kind {
            CallerKind::Human => Author::Human,
            CallerKind::System => Author::System,
            CallerKind::Agent => Author::System, // placeholder; the txn's author is set at commit
        };
        let st = Staging::new(base, author, &mut self.ids);
        let mut cx = Cx {
            tx: Tx::new(st),
            caller: &req.caller,
            analysis: self.analysis.as_ref(),
            services: Services { now_ms: now },
        };
        let args = req.args.clone();
        let res = std::panic::catch_unwind(AssertUnwindSafe(|| cmd.call(args, &mut cx)))
            .map_err(|_| internal("handler-panic"))?;
        let (ops, reads) = cx.tx.finish();
        // checked before the handler's own error: a bound run must not learn another
        // cut's values from which validation error it gets
        self.check_read_binding(&req.caller, &reads)?;
        // R-CC-2: a human gesture in progress on an entity wins over an agent's write
        if req.caller.kind == CallerKind::Agent && !self.gestures.is_empty() {
            // what the command writes, and (when it writes at all) what it looked at to
            // decide: a part override of a held layer reads the layer
            let held: Vec<EntityId> = if ops.is_empty() {
                Vec::new()
            } else {
                ops.iter()
                    .map(Op::entity)
                    .chain(reads.iter().map(|r| r.e))
                    .filter(|e| self.gestures.values().any(|g| g.contains(e)))
                    .collect()
            };
            if let Some(e) = held.first() {
                return Err(CmdError::Conflict(Box::new(Conflict {
                    entities: vec![ConflictEntity {
                        entity: e.to_string(),
                        current_rev: self.doc.entity_rev(*e).unwrap_or(0),
                        by: "human".into(),
                    }],
                    fields: None,
                    current: json!({}),
                    diff: json!({"reason": "the human is dragging this right now"}),
                    options: Some(vec![ConflictOption::Cancel]),
                })));
            }
        }
        let out = res?;
        Ok(Staged {
            ops,
            reads,
            output: out,
            reverts: None,
            stack: None,
        })
    }

    /// R-CC-1: conflicts on recorded reads that moved since the caller's base.
    fn check_base(&self, base: &Base, reads: &[ReadRef]) -> Result<(), CmdError> {
        let mut moved = Vec::new();
        for r in reads {
            let now = match r.children {
                None => self.doc.entity_rev(r.e).unwrap_or(r.rev),
                Some(k) => self.doc.index_rev(r.e, k),
            };
            let stale = match base {
                Base::AsOf { as_of } => now > *as_of,
                Base::Revs(m) => match m.get(&r.e.to_string()) {
                    Some(b) => r.children.is_none() && now != *b,
                    None => false,
                },
            };
            if stale {
                moved.push((r.e, now));
            }
        }
        if moved.is_empty() {
            return Ok(());
        }
        moved.sort();
        moved.dedup();
        let entities: Vec<ConflictEntity> = moved
            .iter()
            .map(|(e, rev)| ConflictEntity {
                entity: e.to_string(),
                current_rev: *rev,
                by: self
                    .doc
                    .last_author(*e)
                    .map_or_else(|| "system".into(), |a| a.label()),
            })
            .collect();
        let current: serde_json::Map<String, Value> = moved
            .iter()
            .map(|(e, rev)| (e.to_string(), json!(rev)))
            .collect();
        Err(CmdError::Conflict(Box::new(Conflict {
            entities,
            fields: None,
            current: Value::Object(current),
            diff: json!({ "as_of_now": self.doc.rev() }),
            options: None,
        })))
    }

    fn lookup_request(
        &mut self,
        k: &ReqKey,
        fp: &str,
        now: i64,
    ) -> Result<Option<CallOk>, CmdError> {
        let reused = || CmdError::invalid("/request_id", "already used for a different call");
        if let Some((ts, f, ok)) = self.requests.get(k)
            && now - ts < REQUEST_WINDOW_MS
        {
            if f != fp {
                return Err(reused());
            }
            let mut ok = ok.clone();
            ok.replayed = true;
            return Ok(Some(ok));
        }
        let since = now - REQUEST_WINDOW_MS;
        // R-AGT-5: replaying another identity's request (same id, same call) is refused;
        // the same id for a different call is that caller's own business
        let others = self
            .store()?
            .request_used_elsewhere(&k.0, &k.2, since)
            .map_err(|e| store_err(&e))?;
        if others.iter().any(|r| {
            serde_json::from_slice::<Value>(r)
                .ok()
                .and_then(|v| v.get("fp").and_then(Value::as_str).map(|f| f == fp))
                .unwrap_or(false)
        }) {
            return Err(CmdError::invalid(
                "/request_id",
                "a replay of a request this caller did not make",
            ));
        }
        let rec = self
            .store()?
            .request(&k.0, &k.1, &k.2, since)
            .map_err(|e| store_err(&e))?;
        let Some(r) = rec else { return Ok(None) };
        let stored: Value =
            serde_json::from_slice(&r.result).map_err(|_| internal("request-record"))?;
        if stored.get("fp").and_then(Value::as_str) != Some(fp) {
            return Err(reused());
        }
        let ok = CallOk {
            as_of: stored.get("as_of").and_then(Value::as_u64).unwrap_or(0),
            committed: r.rev,
            output: stored.get("output").cloned().unwrap_or(Value::Null),
            replayed: true,
        };
        self.requests
            .insert(k.clone(), (r.ts, fp.to_owned(), ok.clone()));
        Ok(Some(ok))
    }

    fn request_record(
        k: &ReqKey,
        fp: &str,
        now: i64,
        rev: Option<Rev>,
        ok: &CallOk,
    ) -> RequestRecord {
        RequestRecord {
            identity: k.0.clone(),
            scope: k.1.clone(),
            request_id: k.2.clone(),
            ts: now,
            rev,
            result: serde_json::to_vec(&json!({"output": ok.output, "as_of": ok.as_of, "fp": fp}))
                .unwrap_or_default(),
        }
    }

    fn cache_request(&mut self, k: &ReqKey, fp: &str, now: i64, ok: &CallOk) {
        if self.requests.len() >= 10_000 {
            self.requests
                .retain(|_, (ts, ..)| now - *ts < REQUEST_WINDOW_MS);
        }
        self.requests
            .insert(k.clone(), (now, fp.to_owned(), ok.clone()));
    }

    fn remember(
        &mut self,
        k: &ReqKey,
        fp: &str,
        now: i64,
        rev: Option<Rev>,
        ok: &CallOk,
    ) -> Result<(), CmdError> {
        let rec = Self::request_record(k, fp, now, rev, ok);
        self.store()?
            .commit_with(None, Some(&rec))
            .map_err(|e| store_err(&e))?;
        self.cache_request(k, fp, now, ok);
        Ok(())
    }

    /// One commit: the op row (and idempotency record) in SQLite, then the document,
    /// the snapshot, the event, the run.
    fn commit(
        &mut self,
        txn: Txn,
        req: Option<(&ReqKey, &str, i64, &CallOk)>,
    ) -> Result<(), CmdError> {
        // apply to a copy first: a staged txn that does not apply is a bug, not a write
        let mut next = self.doc.clone();
        next.apply(&txn)
            .map_err(|e| internal(&format!("apply:{e}")))?;
        let rec = req.map(|(k, fp, now, ok)| Self::request_record(k, fp, now, Some(txn.rev), ok));
        // R-ERR-2: on a full disk the edit is kept (applied, shown "Not saved: disk
        // full") and its op row written once space returns, in order
        let written = if self.flush_unsaved() {
            match self.store()?.commit_with(Some(&txn), rec.as_ref()) {
                Ok(()) => true,
                Err(e) if e.is_disk_full() => false,
                Err(e) => return Err(store_err(&e)),
            }
        } else {
            false
        };
        if !written {
            self.unsaved.push((txn.clone(), rec));
        }
        if let Some((k, fp, now, ok)) = req {
            self.cache_request(k, fp, now, ok);
        }
        self.doc = next;
        self.snap.store(Arc::new(Snapshot {
            doc: self.doc.clone(),
            analysis: Arc::clone(&self.analysis),
        }));
        if written && let Some(s) = &self.saver {
            s.committed(txn.rev);
        }
        self.last_commit = Instant::now();
        if let Some(run) = txn.run
            && let Some(r) = self.runs.get_mut(&run)
        {
            r.txns.push(txn.rev);
        }
        let seq = self.events.next_seq();
        let triples: Vec<EventTriple> = txn
            .ops
            .iter()
            .map(|op| {
                let (field, key) = match op.path() {
                    Some(p) => (
                        gmclip_model::kinds::kind_ops(op.entity().kind).and_then(|k| {
                            k.fields()
                                .get(usize::from(p.field.0))
                                .map(|d| d.name.to_owned())
                        }),
                        p.key.clone(),
                    ),
                    None => (None, None),
                };
                EventTriple {
                    entity: op.entity().to_string(),
                    field,
                    key,
                    op: match op {
                        Op::Set { .. } => "set",
                        Op::Create { .. } => "create",
                        Op::Delete { .. } => "delete",
                    }
                    .into(),
                }
            })
            .collect();
        let display = format!(
            "{} by {}: {} change(s), as_of {}",
            txn.command.name,
            txn.author.label(),
            triples.len(),
            txn.rev
        );
        self.events.publish(
            Event::Committed {
                seq,
                rev: txn.rev,
                by: txn.author.label(),
                command: txn.command.name.clone(),
                triples,
                display,
            },
            txn.rev,
        );
        self.txns.push(txn);
        Ok(())
    }

    // ---------------------------------------------------------------------------------
    // Runs (SPEC §3.6)
    // ---------------------------------------------------------------------------------

    /// A run kept with the project by an earlier opening.
    fn persisted_run(&self, id: &RunId) -> Option<RunState> {
        let want = id.0.to_string();
        self.store
            .as_ref()?
            .runs()
            .ok()?
            .into_iter()
            .find(|(k, _)| *k == want)
            .and_then(|(_, body)| serde_json::from_slice(&body).ok())
    }

    fn persist_runs(&mut self) {
        let rows: Vec<(String, Vec<u8>)> = self
            .runs
            .values()
            .map(|r| {
                (
                    r.id.0.to_string(),
                    serde_json::to_vec(r).unwrap_or_default(),
                )
            })
            .collect();
        if let Some(s) = self.store.as_mut() {
            for (id, body) in rows {
                let _ = s.put_run(&id, &body);
            }
        }
    }

    /// Keeps the human's stack in the project so it survives a reopen (R-HIST-1).
    fn persist_human_stack(&mut self) {
        // it names revs: written once they are on disk
        if !self.unsaved.is_empty() {
            return;
        }
        let Some(st) = self.stacks.get(&Author::Human) else {
            return;
        };
        let keep = |v: &[Rev]| v[v.len().saturating_sub(STACK_KEEP)..].to_vec();
        let body = serde_json::to_vec(&Stack {
            undo: keep(&st.undo),
            redo: keep(&st.redo),
        })
        .unwrap_or_default();
        if let Some(s) = self.store.as_mut() {
            let _ = s.put_meta(HUMAN_STACK_KEY, &body);
        }
    }

    /// Who each run was (`local:<client>`, `launched:<run>`) and its label, from this
    /// session and the runs kept in the project.
    fn run_records(&self) -> HashMap<String, (String, String)> {
        let mut out: HashMap<String, (String, String)> = HashMap::new();
        if let Some(s) = self.store.as_ref()
            && let Ok(rows) = s.runs()
        {
            for (id, body) in rows {
                if let Ok(v) = serde_json::from_slice::<Value>(&body)
                    && let Some(i) = v.get("identity").and_then(Value::as_str)
                {
                    let label = v.get("label").and_then(Value::as_str).unwrap_or_default();
                    out.insert(id, (i.to_owned(), label.to_owned()));
                }
            }
        }
        for r in self.runs.values() {
            out.insert(r.id.0.to_string(), (r.identity.clone(), r.label.clone()));
        }
        out
    }

    /// The author of each transaction in `(from, to]` as people read it: `human`, the
    /// agent's identity, or the review link a system write was made for (A2.13).
    fn authors_between(&self, from: Rev, to: Rev) -> Result<Vec<(Rev, String)>, CmdError> {
        let runs = self.run_records();
        let by: HashMap<Rev, String> = self
            .store
            .as_ref()
            .and_then(|s| s.meta_prefixed(BY_PREFIX).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| {
                let rev: Rev = k.strip_prefix(BY_PREFIX)?.parse().ok()?;
                Some((rev, String::from_utf8(v).ok()?))
            })
            .collect();
        Ok(self
            .txns_after(from)?
            .iter()
            .filter(|t| t.rev <= to)
            .filter_map(|t| {
                let who = match t.author {
                    Author::Human => "human".to_owned(),
                    Author::Run(r) => runs
                        .get(&r.0.to_string())
                        .map_or_else(|| format!("run:{}", r.0), |x| x.0.clone()),
                    Author::System => by.get(&t.rev).cloned()?,
                };
                Some((t.rev, who))
            })
            .collect())
    }

    fn snapshot_now(
        &mut self,
        kind: SnapshotKind,
        name: Option<String>,
        run: Option<RunId>,
        now: i64,
    ) -> Result<String, CmdError> {
        // a snapshot names a rev: only one whose op rows are on disk
        if !self.flush_unsaved() {
            return Err(internal(&format!("store:{DISK_FULL}")));
        }
        self.ids.at(now);
        let id = self.ids.next_ulid().to_string();
        let rec = SnapshotRecord {
            id: id.clone(),
            kind,
            name,
            rev: self.doc.rev(),
            run: run.map(|r| r.0.to_string()),
            ts: now,
        };
        self.store()?
            .add_snapshot(&rec)
            .map_err(|e| store_err(&e))?;
        Ok(id)
    }

    /// Begins a run for a connection, after a project-wide restore point.
    fn begin_run(&mut self, caller: &Caller, label: String, now: i64) -> Result<RunId, CmdError> {
        if let Some(old) = self.conn_runs.get(&caller.connection).copied() {
            self.end_run(old);
        }
        self.ids.at(now);
        let id = RunId(self.ids.next_ulid());
        let rp = self.snapshot_now(SnapshotKind::RestorePoint, None, Some(id), now)?;
        self.runs.insert(
            id,
            RunState {
                id,
                identity: caller.identity.clone(),
                connection: caller.connection.clone(),
                label,
                restore_point: rp,
                restore_rev: self.doc.rev(),
                cut: None,
                txns: Vec::new(),
                last_ms: now,
                ended: false,
            },
        );
        self.conn_runs.insert(caller.connection.clone(), id);
        self.persist_runs();
        Ok(id)
    }

    fn end_run(&mut self, id: RunId) {
        if let Some(r) = self.runs.get_mut(&id) {
            r.ended = true;
            let conn = r.connection.clone();
            if self.conn_runs.get(&conn) == Some(&id) {
                self.conn_runs.remove(&conn);
            }
        }
        self.persist_runs();
    }

    fn expire_run(&mut self, connection: &str, now: i64) {
        if let Some(id) = self.conn_runs.get(connection).copied()
            && self
                .runs
                .get(&id)
                .is_some_and(|r| now - r.last_ms >= RUN_IDLE_MS)
        {
            self.end_run(id);
        }
    }

    /// The cut an entity belongs to, walking parents (project-level entities: `None`).
    /// A launched run bound to a cut (R-AGT-5) may read only that cut and entities that
    /// belong to no cut (sources, moments, library layouts, project maps): a read of
    /// another cut's entity, found or not, refuses the whole request.
    fn check_read_binding(&self, caller: &Caller, reads: &[ReadRef]) -> Result<(), CmdError> {
        let Some(bound) = caller.cut_binding else {
            return Ok(());
        };
        if reads
            .iter()
            .any(|r| Self::cut_of(&self.doc, r.e).is_some_and(|c| c != bound))
        {
            return Err(CmdError::Unauthorized {
                scope: "cut_binding".into(),
            });
        }
        Ok(())
    }

    fn cut_of(doc: &Doc, e: EntityId) -> Option<Id<Cut>> {
        let mut cur = e;
        for _ in 0..8 {
            if cur.kind == <Cut as gmclip_model::Entity>::KIND {
                return Some(Id::new(cur.id));
            }
            cur = doc.parent_of(cur)?;
        }
        None
    }

    /// The author of a transaction; for an agent, its implicit run (opened on first
    /// write, restarted when the connection first writes a second cut).
    fn author_for(&mut self, req: &CallRequest, ops: &[Op], now: i64) -> Result<Author, CmdError> {
        match req.caller.kind {
            CallerKind::Human => return Ok(Author::Human),
            CallerKind::System => return Ok(Author::System),
            CallerKind::Agent => {}
        }
        let cuts: BTreeSet<Id<Cut>> = ops
            .iter()
            .filter_map(|op| {
                Self::cut_of(&self.doc, op.entity())
                    .or_else(|| Self::cut_of(&self.preview(op), op.entity()))
            })
            .collect();
        if cuts.len() > 1 {
            return Err(CmdError::invalid(
                "/",
                "an agent transaction writes one cut at a time",
            ));
        }
        // a launched run is bound to its cut plus the project-level maps (R-AGT-5, §3.6);
        // checked before any run opens. Entities outside every cut (library layouts and
        // their layers, sources, moments) are other cuts' business — except that a run
        // may *add* a moment (a find-moments pass, §10.6): a new card in the panel,
        // labelled as the agent's, never a change to an existing one.
        if let Some(bound) = req.caller.cut_binding {
            const PROJECT_MAPS: &[&str] = &["caption_corrections", "terms", "keep_pairs", "styles"];
            let outside = ops.iter().any(|op| {
                let e = op.entity();
                let c = Self::cut_of(&self.doc, e).or_else(|| Self::cut_of(&self.preview(op), e));
                match c {
                    Some(c) => c != bound,
                    None => {
                        let new_moment = matches!(op, Op::Create { .. })
                            && e.kind
                                == <gmclip_model::entities::Moment as gmclip_model::Entity>::KIND;
                        !PROJECT_MAPS.contains(&e.kind.0) && !new_moment
                    }
                }
            });
            if outside {
                return Err(CmdError::Unauthorized {
                    scope: "cut_binding".into(),
                });
            }
        }
        let mut run = match self.conn_runs.get(&req.caller.connection).copied() {
            Some(r) if !self.runs.get(&r).is_some_and(|s| s.ended) => r,
            _ => self.begin_run(&req.caller, "implicit".into(), now)?,
        };
        if let Some(c) = cuts.iter().next().copied() {
            let current = self.runs.get(&run).and_then(|r| r.cut);
            match current {
                None => {
                    if let Some(r) = self.runs.get_mut(&run) {
                        r.cut = Some(c);
                    }
                }
                Some(bound) if bound != c => {
                    // first write to a second cut: a new run (and restore point)
                    run = self.begin_run(&req.caller, "implicit".into(), now)?;
                    if let Some(r) = self.runs.get_mut(&run) {
                        r.cut = Some(c);
                    }
                }
                Some(_) => {}
            }
        }
        if let Some(r) = self.runs.get_mut(&run) {
            r.last_ms = now;
        }
        Ok(Author::Run(run))
    }

    /// A doc where a create op has been applied, to find a new entity's cut.
    fn preview(&self, op: &Op) -> Doc {
        let mut d = self.doc.clone();
        let _ = d.apply_ops(std::slice::from_ref(op), self.doc.rev() + 1, Author::System);
        d
    }

    fn run_begin_cmd(&mut self, req: &CallRequest, now: i64) -> Result<CallOk, CmdError> {
        let a: RunBeginArgs = parse(&req.args)?;
        if req.caller.kind != CallerKind::Agent {
            return Err(CmdError::invalid(
                "/",
                "runs group agent edits; human edits are never in a run",
            ));
        }
        if a.label.chars().count() > 200
            || a.brief.as_ref().is_some_and(|b| b.chars().count() > 4_000)
        {
            return Err(CmdError::invalid("/label", "at most 200 characters"));
        }
        let id = self.begin_run(&req.caller, a.label, now)?;
        let rp = self
            .runs
            .get(&id)
            .map(|r| r.restore_point.clone())
            .unwrap_or_default();
        self.ok(serde_json::to_value(RunBegun {
            run: id.0.to_string(),
            restore_point: rp,
        }))
    }

    /// `run_summary_set` from an agent: the run it summarises is its own open run (the
    /// connection's, or its launched run), never one it names; and that run must not
    /// have written another cut.
    fn with_own_run(&self, req: &CallRequest) -> Result<CallRequest, CmdError> {
        let run = self
            .conn_runs
            .get(&req.caller.connection)
            .copied()
            .filter(|r| self.runs.get(r).is_some_and(|s| !s.ended))
            .or(req.caller.run)
            .ok_or_else(|| {
                CmdError::invalid(
                    "/run",
                    "no run is open on this connection: record the summary after the run's edits",
                )
            })?;
        let cut = req.args.get("cut").and_then(Value::as_str);
        if let Some(c) = self.runs.get(&run).and_then(|r| r.cut)
            && cut != Some(c.0.to_string().as_str())
        {
            return Err(CmdError::invalid("/cut", "the cut this run edited"));
        }
        let mut req = req.clone();
        if let Some(o) = req.args.as_object_mut() {
            o.insert("run".into(), Value::String(run.0.to_string()));
        }
        Ok(req)
    }

    fn run_end_cmd(&mut self, req: &CallRequest) -> Result<CallOk, CmdError> {
        let _: crate::commands::history::RunEndArgs = parse(&req.args)?;
        let id = self.conn_runs.get(&req.caller.connection).copied();
        let n = id
            .and_then(|i| self.runs.get(&i))
            .map_or(0, |r| r.txns.len());
        if let Some(i) = id {
            self.end_run(i);
        }
        self.ok(serde_json::to_value(RunEnded {
            run: id.map(|i| i.0.to_string()),
            transactions: n,
        }))
    }

    fn restore_point_cmd(&mut self, req: &CallRequest, now: i64) -> Result<CallOk, CmdError> {
        let a: RestorePointArgs = parse(&req.args)?;
        let kind = if a.name.is_some() {
            SnapshotKind::Named
        } else {
            SnapshotKind::RestorePoint
        };
        let id = self.snapshot_now(kind, a.name, None, now)?;
        let rev = self.doc.rev();
        self.ok(serde_json::to_value(RestorePointMade { id, rev }))
    }

    /// `history_list`: versions newest first, with the cut's parts and seconds at each
    /// and who edited after each. One walk back from now (each version's document is
    /// the newer one's with the transactions in between undone).
    fn history_list_cmd(&mut self, req: &CallRequest) -> Result<CallOk, CmdError> {
        use crate::commands::history::{HistoryList, HistoryListArgs, HistoryRow};
        let a: HistoryListArgs = parse(&req.args)?;
        let mut snaps = self
            .store
            .as_ref()
            .ok_or_else(|| internal("closed"))?
            .snapshots()
            .map_err(|e| store_err(&e))?;
        let total = u32::try_from(snaps.len()).unwrap_or(u32::MAX);
        let (now_parts, now_seconds) = match a.cut {
            Some(c) => cut_measure(&self.doc, c),
            None => (None, None),
        };
        if a.count_only {
            return self.ok(serde_json::to_value(HistoryList {
                total,
                versions: Vec::new(),
                now_parts,
                now_seconds,
            }));
        }
        snaps.sort_by(|x, y| y.ts.cmp(&x.ts).then(y.rev.cmp(&x.rev)));
        snaps.truncate(a.limit.unwrap_or(50).clamp(1, 200) as usize);
        let oldest = snaps.iter().map(|s| s.rev).min().unwrap_or(self.doc.rev());
        let authors = self.authors_between(oldest, self.doc.rev())?;
        let runs = self.run_records();
        // newest rev first, so one document walks back through every version
        let mut by_rev: Vec<usize> = (0..snaps.len()).collect();
        by_rev.sort_by(|&x, &y| snaps[y].rev.cmp(&snaps[x].rev));
        let mut measured: HashMap<usize, (Option<u32>, Option<f64>)> = HashMap::new();
        if let Some(c) = a.cut {
            let later = self.txns_after(oldest)?;
            let mut d = self.doc.clone();
            let mut k = later.len();
            for &i in &by_rev {
                let rev = snaps[i].rev;
                while k > 0 && later[k - 1].rev > rev {
                    let t = &later[k - 1];
                    d.apply_ops(&t.inverse_ops(), t.rev, t.author)
                        .map_err(|_| internal("history-diverged"))?;
                    k -= 1;
                }
                measured.insert(i, cut_measure(&d, c));
            }
        }
        // the next newer version's rev, for "who edited after this one"
        let mut next_rev: HashMap<usize, Rev> = HashMap::new();
        for w in by_rev.windows(2) {
            next_rev.insert(w[1], snaps[w[0]].rev);
        }
        let mut versions = Vec::new();
        for (i, sn) in snaps.into_iter().enumerate() {
            let (parts, seconds) = measured.get(&i).copied().unwrap_or((None, None));
            let upto = next_rev.get(&i).copied().unwrap_or(self.doc.rev());
            let mut then_by: Vec<String> = Vec::new();
            for (_, who) in authors.iter().filter(|(r, _)| *r > sn.rev && *r <= upto) {
                if !then_by.contains(who) {
                    then_by.push(who.clone());
                }
            }
            let run = sn.run.as_ref().and_then(|r| runs.get(r));
            versions.push(HistoryRow {
                id: sn.id,
                kind: match (sn.kind, &sn.run) {
                    (_, Some(_)) => "run".into(),
                    (SnapshotKind::Named | SnapshotKind::Epoch, _) => "named".into(),
                    _ => "restore_point".into(),
                },
                name: sn.name,
                rev: sn.rev,
                ts: sn.ts,
                run_by: run.map(|r| r.0.clone()),
                run_label: run.map(|r| r.1.clone()),
                run: sn.run,
                parts,
                seconds,
                then_by,
            });
        }
        self.ok(serde_json::to_value(HistoryList {
            total,
            versions,
            now_parts,
            now_seconds,
        }))
    }

    /// `history_diff`: a cut at one version against now or another version.
    fn history_diff_cmd(&mut self, req: &CallRequest) -> Result<CallOk, CmdError> {
        use crate::commands::history::{DiffTrim, HistoryDiff, HistoryDiffArgs};
        let a: HistoryDiffArgs = parse(&req.args)?;
        if req.caller.cut_binding.is_some_and(|b| b != a.cut) {
            return Err(CmdError::Unauthorized {
                scope: "cut_binding".into(),
            });
        }
        let snaps = self
            .store
            .as_ref()
            .ok_or_else(|| internal("closed"))?
            .snapshots()
            .map_err(|e| store_err(&e))?;
        let rev_of = |id: &str| {
            snaps
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.rev)
                .ok_or(CmdError::NotFound)
        };
        let from_rev = rev_of(&a.snapshot)?;
        let to_rev = match &a.against {
            Some(id) => rev_of(id)?,
            None => self.doc.rev(),
        };
        let from_doc = self.doc_at(from_rev)?;
        let to_doc = if to_rev == self.doc.rev() {
            self.doc.clone()
        } else {
            self.doc_at(to_rev)?
        };
        let from = diff_parts(&from_doc, a.cut);
        let to = diff_parts(&to_doc, a.cut);
        let (from_parts, from_seconds) = cut_measure(&from_doc, a.cut);
        let (to_parts, to_seconds) = cut_measure(&to_doc, a.cut);
        let added: Vec<_> = to
            .iter()
            .filter(|p| !from.iter().any(|q| q.part == p.part))
            .cloned()
            .collect();
        let removed: Vec<_> = from
            .iter()
            .filter(|p| !to.iter().any(|q| q.part == p.part))
            .cloned()
            .collect();
        let mut trimmed = Vec::new();
        for p in &to {
            if let Some(q) = from.iter().find(|q| q.part == p.part)
                && ((q.src_in_s - p.src_in_s).abs() > 1e-6
                    || (q.src_out_s - p.src_out_s).abs() > 1e-6)
            {
                trimmed.push(DiffTrim {
                    from: q.clone(),
                    to: p.clone(),
                });
            }
        }
        // order among the parts both sides keep
        let kept_from: Vec<&str> = from
            .iter()
            .filter(|p| to.iter().any(|q| q.part == p.part))
            .map(|p| p.part.as_str())
            .collect();
        let kept_to: Vec<&str> = to
            .iter()
            .filter(|p| from.iter().any(|q| q.part == p.part))
            .map(|p| p.part.as_str())
            .collect();
        let moved = moved_parts(&kept_from, &kept_to);
        let (lo, hi) = (from_rev.min(to_rev), from_rev.max(to_rev));
        let mut by: Vec<String> = Vec::new();
        for (_, who) in self.authors_between(lo, hi)? {
            if !by.contains(&who) {
                by.push(who);
            }
        }
        self.ok(serde_json::to_value(HistoryDiff {
            from_parts,
            from_seconds,
            to_parts,
            to_seconds,
            from,
            to,
            added,
            removed,
            trimmed,
            moved,
            by,
        }))
    }

    fn ok(&self, v: Result<Value, serde_json::Error>) -> Result<CallOk, CmdError> {
        Ok(CallOk {
            output: v.map_err(|_| internal("serialise-output"))?,
            as_of: self.doc.rev(),
            committed: None,
            replayed: false,
        })
    }

    // ---------------------------------------------------------------------------------
    // History (R-HIST-2/3)
    // ---------------------------------------------------------------------------------

    /// Committed transactions with `rev > from`, oldest first (memory, else the store).
    fn txns_after(&self, from: Rev) -> Result<Vec<Txn>, CmdError> {
        let mut out = Vec::new();
        if from < self.open_rev {
            let s = self.store.as_ref().ok_or_else(|| internal("closed"))?;
            out = s.txns(from, self.open_rev).map_err(|e| store_err(&e))?;
        }
        out.extend(self.txns.iter().filter(|t| t.rev > from).cloned());
        Ok(out)
    }

    fn txn(&self, rev: Rev) -> Result<Txn, CmdError> {
        if rev > self.open_rev {
            return self
                .txns
                .iter()
                .find(|t| t.rev == rev)
                .cloned()
                .ok_or_else(|| internal("missing-txn"));
        }
        let s = self.store.as_ref().ok_or_else(|| internal("closed"))?;
        s.txns(rev - 1, rev)
            .map_err(|e| store_err(&e))?
            .pop()
            .ok_or_else(|| internal("missing-txn"))
    }

    /// The document as it was at `rev`: inverse ops of every later transaction.
    fn doc_at(&self, rev: Rev) -> Result<Doc, CmdError> {
        let mut d = self.doc.clone();
        for t in self.txns_after(rev)?.iter().rev() {
            d.apply_ops(&t.inverse_ops(), t.rev, t.author)
                .map_err(|_| internal("history-diverged"))?;
        }
        d.set_rev(rev);
        Ok(d)
    }

    /// Is `op` still applicable as-is to the current document (its `before` matches)?
    fn applies(&self, op: &Op) -> bool {
        match op {
            Op::Set { e, f, before, .. } => self.doc.field(*e, f).is_ok_and(|v| v == *before),
            Op::Create { e, .. } => !self.doc.contains(*e),
            Op::Delete { e, value } => self.doc.encode(*e).is_some_and(|v| v == *value),
        }
    }

    fn conflict(
        &self,
        keys: &[PathKey],
        options: Vec<ConflictOption>,
        later: &[String],
    ) -> CmdError {
        let mut ents: Vec<EntityId> = keys.iter().map(|k| k.0).collect();
        ents.sort();
        ents.dedup();
        CmdError::Conflict(Box::new(Conflict {
            entities: ents
                .iter()
                .map(|e| ConflictEntity {
                    entity: e.to_string(),
                    current_rev: self.doc.entity_rev(*e).unwrap_or(0),
                    by: self
                        .doc
                        .last_author(*e)
                        .map_or_else(|| "system".into(), |a| a.label()),
                })
                .collect(),
            fields: Some(keys.iter().map(label).collect()),
            current: json!({ "rev": self.doc.rev() }),
            diff: json!({ "later_edits": later }),
            options: Some(options),
        }))
    }

    /// The caller's stack key: the human's own, or the agent's current run.
    fn stack_author(&self, req: &CallRequest) -> Result<Author, CmdError> {
        match req.caller.kind {
            CallerKind::Human => Ok(Author::Human),
            CallerKind::System => Ok(Author::System),
            CallerKind::Agent => self
                .conn_runs
                .get(&req.caller.connection)
                .map(|r| Author::Run(*r))
                .ok_or_else(|| CmdError::invalid("/", "no run on this connection to undo")),
        }
    }

    /// R-HIST-2 for one transaction: append the inverse when no later write touched its
    /// fields; else `Conflict` with (a) revert untouched / (c) cancel.
    fn undo_redo(&mut self, req: &CallRequest, redo: bool) -> Result<Staged, CmdError> {
        let a: UndoArgs = parse(&req.args)?;
        let who = self.stack_author(req)?;
        let st = self.stacks.entry(who).or_default();
        let target = if redo {
            st.redo.last().copied()
        } else {
            st.undo.last().copied()
        };
        let target = target.ok_or_else(|| {
            CmdError::invalid(
                "/",
                if redo {
                    "nothing to redo"
                } else {
                    "nothing to undo"
                },
            )
        })?;
        let t = self.txn(target)?;
        // R-HIST-1: the first undo/redo of an edit made before this session asks once
        if who == Author::Human && target <= self.open_rev && !self.earlier_ok {
            if !a.confirm {
                return Ok(Staged {
                    ops: Vec::new(),
                    reads: Vec::new(),
                    output: json!({
                        "needs_confirm": true,
                        "target": t.id.0.to_string(),
                        "command": t.command.name,
                        "ts": t.ts,
                    }),
                    reverts: None,
                    stack: None,
                });
            }
            self.earlier_ok = true;
        }
        // value check: an inverse op applies iff nothing changed its field since
        let (clean, mut dirty): (Vec<Op>, Vec<Op>) =
            t.inverse_ops().into_iter().partition(|op| self.applies(op));
        // a delete is also dirty when the entity gained children since (an undo must
        // never orphan another author's entities)
        let (clean, orphaning) = self.split_orphans(clean);
        dirty.extend(orphaning);
        let dirty_keys: Vec<PathKey> = dirty.iter().map(op_paths).collect();
        let later = self.later_edits(target, &dirty_keys)?;
        let result = |rev: Option<Rev>| {
            serde_json::to_value(HistoryResult {
                rev,
                target: Some(t.id.0.to_string()),
                skipped: dirty_keys.iter().map(label).collect(),
            })
            .map_err(|_| internal("serialise-output"))
        };
        let ops = match (dirty.is_empty(), a.option) {
            (true, _) => clean,
            (false, None) => {
                return Err(self.conflict(
                    &dirty_keys,
                    vec![ConflictOption::RevertUntouched, ConflictOption::Cancel],
                    &later,
                ));
            }
            (false, Some(UndoOption::RevertUntouched)) => {
                // ops on an entity whose create/delete is dirty go with it
                let whole: BTreeSet<EntityId> = dirty_keys
                    .iter()
                    .filter(|d| d.1.is_none())
                    .map(|d| d.0)
                    .collect();
                clean
                    .into_iter()
                    .filter(|op| !whole.contains(&op.entity()))
                    .collect()
            }
            (false, Some(UndoOption::Cancel | UndoOption::RestoreToPoint)) => Vec::new(),
        };
        let stack = (!ops.is_empty()).then_some((who, redo));
        Ok(Staged {
            output: result(None)?,
            ops,
            reads: Vec::new(),
            reverts: Some(t.id),
            stack,
        })
    }

    /// Splits off the deletes in `ops` that would leave children behind once `ops` are
    /// applied (each such delete is removed with its entity's other ops).
    fn split_orphans(&self, ops: Vec<Op>) -> (Vec<Op>, Vec<Op>) {
        let mut ops = ops;
        let mut out = Vec::new();
        for _ in 0..8 {
            let mut trial = self.doc.clone();
            if trial
                .apply_ops(&ops, self.doc.rev() + 1, Author::System)
                .is_err()
            {
                break;
            }
            let bad: BTreeSet<EntityId> = trial.orphaning_deletes(&ops).into_iter().collect();
            if bad.is_empty() {
                break;
            }
            let (keep, drop): (Vec<Op>, Vec<Op>) =
                ops.into_iter().partition(|op| !bad.contains(&op.entity()));
            ops = keep;
            out.extend(
                drop.into_iter()
                    .filter(|op| matches!(op, Op::Delete { .. })),
            );
        }
        (ops, out)
    }

    /// Transactions after `rev` that wrote any of `keys`, as display lines.
    fn later_edits(&self, rev: Rev, keys: &[PathKey]) -> Result<Vec<String>, CmdError> {
        Ok(self
            .txns_after(rev)?
            .iter()
            .filter(|l| {
                l.ops
                    .iter()
                    .any(|op| keys.iter().any(|k| overlaps(k, &op_paths(op))))
            })
            .map(|l| format!("rev {} by {} ({})", l.rev, l.author.label(), l.command.name))
            .collect())
    }

    /// R-HIST-2 for a run: every field the run wrote goes back to its value at the
    /// run's restore point; later edits by others on those fields → `Conflict` with
    /// revert untouched / restore to point (the whole cut) / cancel.
    fn run_undo(&mut self, req: &CallRequest, _now: i64) -> Result<Staged, CmdError> {
        let a: RunUndoArgs = parse(&req.args)?;
        let id = RunId(
            gmclip_model::Ulid::parse(&a.run)
                .map_err(|_| CmdError::invalid_value("/run", "not a run id", &a.run))?,
        );
        // a run of an earlier opening is read back from the project (decision 0040: a
        // session's "Undo its edits" after the app restarted found nothing to undo)
        let run = match self.runs.get(&id) {
            Some(r) => r.clone(),
            None => self.persisted_run(&id).ok_or(CmdError::NotFound)?,
        };
        // a run is bound to its identity (§3.6): another agent cannot undo it (and is
        // not told it exists); the local human can
        if req.caller.kind == CallerKind::Agent && run.identity != req.caller.identity {
            return Err(CmdError::NotFound);
        }
        let r0 = run.restore_rev;
        let at_r0 = self.doc_at(r0)?;
        let later = self.txns_after(r0)?;
        let mut written: Vec<PathKey> = Vec::new();
        for t in later.iter().filter(|t| t.run == Some(id)) {
            for op in &t.ops {
                let k = op_paths(op);
                if !written.contains(&k) {
                    written.push(k);
                }
            }
        }
        let others: Vec<&Txn> = later
            .iter()
            .filter(|t| t.run != Some(id) && t.rev > r0)
            .collect();
        let touched_by_others: Vec<PathKey> = written
            .iter()
            .filter(|k| {
                others
                    .iter()
                    .any(|t| t.ops.iter().any(|op| overlaps(k, &op_paths(op))))
            })
            .cloned()
            .collect();
        let dropped: Vec<String> = others
            .iter()
            .filter(|t| {
                t.ops
                    .iter()
                    .any(|op| touched_by_others.iter().any(|k| overlaps(k, &op_paths(op))))
            })
            .map(|t| format!("rev {} by {} ({})", t.rev, t.author.label(), t.command.name))
            .collect();
        let restore_paths = |keys: &[PathKey], doc: &Doc| -> Vec<Op> {
            let mut out = Vec::new();
            let mut whole: BTreeSet<EntityId> = BTreeSet::new();
            for (e, p) in keys {
                let exists_now = doc.contains(*e);
                let existed = at_r0.contains(*e);
                if p.is_none() || exists_now != existed {
                    whole.insert(*e);
                }
            }
            let whole_v: Vec<EntityId> = whole.iter().copied().collect();
            out.extend(doc.diff_to(&at_r0, &whole_v));
            for (e, p) in keys {
                let Some(p) = p else { continue };
                if whole.contains(e) {
                    continue;
                }
                let (Ok(now_v), Ok(then_v)) = (doc.field(*e, p), at_r0.field(*e, p)) else {
                    continue;
                };
                if now_v != then_v {
                    out.push(Op::Set {
                        e: *e,
                        f: p.clone(),
                        before: now_v,
                        after: then_v,
                    });
                }
            }
            out
        };
        let mut touched_by_others = touched_by_others;
        let mut dropped = dropped;
        // deleting what the run created must not orphan what others added under it
        let (_, orphaning) = self.split_orphans(restore_paths(&written, &self.doc));
        for op in &orphaning {
            let k = (op.entity(), None);
            if !touched_by_others.contains(&k) {
                touched_by_others.push(k);
                dropped.push(format!("children added under {}", op.entity()));
            }
        }
        let ops = match (touched_by_others.is_empty(), a.option) {
            // an explicit choice applies whether or not anything conflicts
            (true, None | Some(UndoOption::RevertUntouched)) => restore_paths(&written, &self.doc),
            (false, None) => {
                return Err(self.conflict(
                    &touched_by_others,
                    vec![
                        ConflictOption::RevertUntouched,
                        ConflictOption::RestoreToPoint,
                        ConflictOption::Cancel,
                    ],
                    &dropped,
                ));
            }
            (false, Some(UndoOption::RevertUntouched)) => {
                let clean: Vec<PathKey> = written
                    .iter()
                    .filter(|k| !touched_by_others.iter().any(|d| overlaps(k, d)))
                    .cloned()
                    .collect();
                let (ops, _) = self.split_orphans(restore_paths(&clean, &self.doc));
                ops
            }
            (_, Some(UndoOption::RestoreToPoint)) => {
                // the run's cut (and anything else it wrote) back to the restore point
                let mut ids: BTreeSet<EntityId> = written.iter().map(|k| k.0).collect();
                if let Some(cut) = run.cut {
                    for d in [&self.doc, &at_r0] {
                        for e in d.entity_ids() {
                            if Self::cut_of(d, e) == Some(cut) {
                                ids.insert(e);
                            }
                        }
                    }
                }
                let ids: Vec<EntityId> = ids.into_iter().collect();
                self.doc.diff_to(&at_r0, &ids)
            }
            (_, Some(UndoOption::Cancel)) => Vec::new(),
        };
        Ok(Staged {
            ops,
            reads: Vec::new(),
            output: serde_json::to_value(HistoryResult {
                rev: None,
                target: Some(id.0.to_string()),
                skipped: if a.option == Some(UndoOption::RevertUntouched) {
                    touched_by_others.iter().map(label).collect()
                } else {
                    dropped
                },
            })
            .map_err(|_| internal("serialise-output"))?,
            reverts: None,
            stack: None,
        })
    }

    /// Restore to a restore point or named snapshot (R-HIST-3: one ordinary txn).
    fn restore_to(&mut self, req: &CallRequest) -> Result<Staged, CmdError> {
        let a: RestoreToArgs = parse(&req.args)?;
        let snap = self
            .store
            .as_ref()
            .ok_or_else(|| internal("closed"))?
            .snapshots()
            .map_err(|e| store_err(&e))?
            .into_iter()
            .find(|s| s.id == a.snapshot)
            .ok_or(CmdError::NotFound)?;
        let then = self.doc_at(snap.rev)?;
        let mut ids: BTreeSet<EntityId> = self.doc.entity_ids().into_iter().collect();
        ids.extend(then.entity_ids());
        if let Some(cut) = a.cut {
            ids.retain(|e| {
                Self::cut_of(&self.doc, *e) == Some(cut) || Self::cut_of(&then, *e) == Some(cut)
            });
        }
        // a cut-scoped restore that deletes an entity takes its current children too
        let mut ops = self
            .doc
            .diff_to(&then, &ids.iter().copied().collect::<Vec<_>>());
        for _ in 0..8 {
            let mut trial = self.doc.clone();
            if trial
                .apply_ops(&ops, self.doc.rev() + 1, Author::System)
                .is_err()
            {
                break;
            }
            let bad = trial.orphaning_deletes(&ops);
            if bad.is_empty() {
                break;
            }
            let before = ids.len();
            for e in self.doc.entity_ids() {
                if self.doc.parent_of(e).is_some_and(|p| bad.contains(&p)) {
                    ids.insert(e);
                }
            }
            if ids.len() == before {
                break;
            }
            ops = self
                .doc
                .diff_to(&then, &ids.iter().copied().collect::<Vec<_>>());
        }
        Ok(Staged {
            ops,
            reads: Vec::new(),
            output: serde_json::to_value(HistoryResult {
                rev: None,
                target: Some(snap.id),
                skipped: Vec::new(),
            })
            .map_err(|_| internal("serialise-output"))?,
            reverts: None,
            stack: None,
        })
    }
}

/// A cut's parts and seconds in `d` (`None` when it does not exist there).
fn cut_measure(d: &Doc, cut: Id<Cut>) -> (Option<u32>, Option<f64>) {
    if d.get(cut).is_none() {
        return (None, None);
    }
    let spans = gmclip_model::clock::cut_layout(d, cut);
    let flicks: i64 = spans.iter().map(|s| s.dur.0).sum();
    (Some(spans.len() as u32), Some(flicks as f64 / FLICKS_PER_S))
}

const FLICKS_PER_S: f64 = 705_600_000.0;

/// A cut's parts in order, for the history diff.
fn diff_parts(d: &Doc, cut: Id<Cut>) -> Vec<crate::commands::history::DiffPart> {
    use gmclip_model::entities::MediaRef;
    if d.get(cut).is_none() {
        return Vec::new();
    }
    gmclip_model::clock::cut_layout(d, cut)
        .into_iter()
        .enumerate()
        .filter_map(|(i, sp)| {
            let p = d.get(sp.part)?;
            let source = match &p.media {
                MediaRef::Source(s) => Some(s.0.to_string()),
                MediaRef::Group(g) => Some(g.0.to_string()),
            };
            Some(crate::commands::history::DiffPart {
                part: sp.part.0.to_string(),
                index: i as u32,
                source,
                src_in_s: p.src_in.0 as f64 / FLICKS_PER_S,
                src_out_s: p.src_out.0 as f64 / FLICKS_PER_S,
                out_start_s: sp.start.0 as f64 / FLICKS_PER_S,
                dur_s: sp.dur.0 as f64 / FLICKS_PER_S,
            })
        })
        .collect()
}

/// The parts whose place among the kept ones changed: those outside a longest common
/// subsequence of the two orders (so one part dragged elsewhere is the one listed).
fn moved_parts(from: &[&str], to: &[&str]) -> Vec<String> {
    let (n, m) = (from.len(), to.len());
    if n == 0 || m == 0 || n * m > 4_000_000 {
        return Vec::new();
    }
    let mut l = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i][j] = if from[i] == to[j] {
                l[i + 1][j + 1] + 1
            } else {
                l[i + 1][j].max(l[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut keep: BTreeSet<&str> = BTreeSet::new();
    while i < n && j < m {
        if from[i] == to[j] {
            keep.insert(from[i]);
            i += 1;
            j += 1;
        } else if l[i + 1][j] >= l[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    to.iter()
        .filter(|p| !keep.contains(*p))
        .map(|p| (*p).to_owned())
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, CmdError> {
    serde_json::from_value(v.clone())
        .map_err(|_| CmdError::invalid("/args", "does not match the schema"))
}
