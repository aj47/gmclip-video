//! Commands (SPEC §3.2; implementer Part C). A [`Command`] runs synchronously on the
//! project actor; a [`JobCommand`] plans on the actor and executes off it. Handlers do
//! no I/O: everything reaches them through [`Cx`].
//!
//! [`Tx`] is the tracking transaction: reads through it are **recorded** (entity reads
//! at their rev, index reads at the owner's index rev), writes become net field triples
//! (via the model's `Staging`), and every entity written counts as read. The actor
//! checks the caller's `base` against that read set after the handler returns and
//! before it commits (R-CC-1).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_model::{
    Doc, EntityId, EntityKind, Id, ModelError, ReadRef, Rev, RunId, Staging, Stored,
};
use gmclip_proto::{CallerKind, CmdError};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Scope a command requires (SPEC §11.4).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    ReadMedia,
    Edit,
    Render,
    Ai,
    Admin,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::ReadMedia => "read_media",
            Scope::Edit => "edit",
            Scope::Render => "render",
            Scope::Ai => "ai",
            Scope::Admin => "admin",
        }
    }
}

/// Execution class.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Exec {
    /// Runs on the actor ([`Command`]).
    Sync,
    /// Plans on the actor, runs on a pool ([`JobCommand`]).
    Job,
}

/// Undo class (R-CMD-1).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum UndoClass {
    /// Writes entities; undoable as field triples.
    Undoable,
    /// Reads only.
    ReadOnly,
    /// Destructive (§11.4): confirm, `NeedsApproval`, or refuse.
    Destructive,
}

/// UI metadata, or `Hidden` (R-CMD-1).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum UiMeta {
    Hidden,
    Action {
        label: &'static str,
        menu: &'static str,
    },
}

/// Static description of a command (R-CMD-1). Names match `^[a-z0-9_]{1,64}$`.
#[derive(Copy, Clone, Debug)]
pub struct CommandMeta {
    pub name: &'static str,
    pub version: u16,
    pub since: &'static str,
    pub scope: Scope,
    pub exec: Exec,
    pub undo: UndoClass,
    pub ui: UiMeta,
    pub doc: &'static str,
    /// JSON examples, validated against `args_schema` in tests.
    pub examples: &'static [&'static str],
    pub args_schema: fn() -> schemars::Schema,
    pub output_schema: fn() -> schemars::Schema,
}

/// The caller of a command, built by the transport.
#[derive(Clone, Debug)]
pub struct Caller {
    /// `local:<client>`, `launched:<run>`, `human`, `system`.
    pub identity: String,
    /// The connection id (idempotency and implicit runs are scoped to it, R-CMD-7).
    pub connection: String,
    pub kind: CallerKind,
    pub scopes: Vec<Scope>,
    /// A launched run this connection is bound to (F9), if any.
    pub run: Option<RunId>,
    /// A launched run's cut (R-AGT-5): writes to any other cut are
    /// `Unauthorized{scope: "cut_binding"}`.
    pub cut_binding: Option<gmclip_model::Id<gmclip_model::entities::Cut>>,
}

impl Caller {
    /// The local human with every scope (UI, Owner CLI, tests).
    pub fn human() -> Self {
        Caller {
            identity: "human".into(),
            connection: "ui".into(),
            kind: CallerKind::Human,
            scopes: vec![
                Scope::Read,
                Scope::ReadMedia,
                Scope::Edit,
                Scope::Render,
                Scope::Ai,
                Scope::Admin,
            ],
            run: None,
            cut_binding: None,
        }
    }

    /// The holder's own background work (analysis jobs, the ASR switch they make, the
    /// measurements it records): every scope, no base needed, its own connection.
    pub fn system(connection: &str) -> Self {
        Caller {
            identity: "system".into(),
            connection: connection.into(),
            kind: CallerKind::System,
            scopes: vec![
                Scope::Read,
                Scope::ReadMedia,
                Scope::Edit,
                Scope::Render,
                Scope::Ai,
                Scope::Admin,
            ],
            run: None,
            cut_binding: None,
        }
    }

    /// An agent connection with the default agent scopes (`read, edit, render, ai`).
    pub fn agent(identity: &str, connection: &str) -> Self {
        Caller {
            identity: identity.into(),
            connection: connection.into(),
            kind: CallerKind::Agent,
            scopes: vec![Scope::Read, Scope::Edit, Scope::Render, Scope::Ai],
            run: None,
            cut_binding: None,
        }
    }
}

/// Read-only analysis view (transcripts now; energy and quiet grids are read by the
/// host from their blobs).
pub use crate::analysis::AnalysisView;

/// Deterministic services handed to handlers (the ULID source is [`Tx::mint`]).
#[derive(Debug, Clone, Copy)]
pub struct Services {
    /// Commit time of this transaction, ms since the Unix epoch.
    pub now_ms: i64,
}

/// What a read recorded: an entity, or the children of one kind under an owner.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug)]
pub struct ReadKey {
    pub e: EntityId,
    pub children: Option<EntityKind>,
}

/// A read-tracking, diffing transaction over an immutable document.
pub struct Tx<'a> {
    st: Staging<'a>,
    reads: BTreeMap<ReadKey, Rev>,
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx")
            .field("reads", &self.reads.len())
            .finish()
    }
}

pub(crate) fn model_err(e: ModelError) -> CmdError {
    match e {
        ModelError::NotFound => CmdError::NotFound,
        ModelError::Invalid(r) => CmdError::invalid("/args", r),
        ModelError::Exists => CmdError::invalid("/args", "already exists"),
        other => CmdError::Internal {
            trace_id: format!("model:{}", shape(&other)),
        },
    }
}

fn shape(e: &ModelError) -> &'static str {
    match e {
        ModelError::UnknownField(_) => "unknown-field",
        ModelError::NeedsKey(_) => "needs-key",
        ModelError::NoKey(_) => "no-key",
        ModelError::BadKey => "bad-key",
        ModelError::BadValue(_) => "bad-value",
        ModelError::UnknownKind => "unknown-kind",
        ModelError::NotFound => "not-found",
        ModelError::Exists => "exists",
        ModelError::Mismatch => "mismatch",
        ModelError::RevOrder { .. } => "rev-order",
        ModelError::NotImplemented(_) => "not-implemented",
        ModelError::Invalid(_) => "invalid",
    }
}

impl<'a> Tx<'a> {
    pub fn new(st: Staging<'a>) -> Self {
        Self {
            st,
            reads: BTreeMap::new(),
        }
    }

    fn record(&mut self, e: EntityId) {
        if let Some(rev) = self.st.base().entity_rev(e) {
            self.reads
                .entry(ReadKey { e, children: None })
                .or_insert(rev);
        }
    }

    /// Reads an entity (as of this transaction's earlier writes) and records it.
    pub fn get<E: Stored>(&mut self, id: Id<E>) -> Result<&E, CmdError> {
        self.record(id.entity());
        self.st.get(id).ok_or(CmdError::NotFound)
    }

    /// The children of `E` under `owner`, in order; records an index read.
    pub fn children<E: Stored>(&mut self, owner: EntityId) -> Vec<Id<E>> {
        let rev = self.st.base().index_rev(owner, E::KIND);
        self.reads
            .entry(ReadKey {
                e: owner,
                children: Some(E::KIND),
            })
            .or_insert(rev);
        self.st.doc().children::<E>(owner).collect()
    }

    /// A fresh id from the actor's deterministic minter (R-MODEL-1).
    pub fn mint<E>(&mut self) -> Id<E> {
        self.st.mint()
    }

    pub fn create<E: Stored>(&mut self, e: E) -> Result<Id<E>, CmdError> {
        self.st.create(e).map_err(model_err)
    }

    pub fn update<E: Stored, R>(
        &mut self,
        id: Id<E>,
        f: impl FnOnce(&mut E) -> R,
    ) -> Result<R, CmdError> {
        self.record(id.entity());
        self.st.update(id, f).map_err(model_err)
    }

    pub fn delete<E: Stored>(&mut self, id: Id<E>) -> Result<E, CmdError> {
        self.record(id.entity());
        self.st.delete(id).map_err(model_err)
    }

    /// The model's staging, for the structural edits in `gmclip_model::edit`. Every
    /// entity they write is recorded as read when the transaction finishes.
    pub fn staging(&mut self) -> &mut Staging<'a> {
        &mut self.st
    }

    /// The working document, unrecorded (for resolving display strings only).
    pub fn peek(&self) -> &Doc {
        self.st.doc()
    }

    /// Finishes: the net ops and the read set (every written entity included).
    pub fn finish(mut self) -> (Vec<gmclip_model::Op>, Vec<ReadRef>) {
        for e in self.st.touched().to_vec() {
            self.record(e);
        }
        let ops = self.st.ops();
        let reads = self
            .reads
            .into_iter()
            .map(|(k, rev)| ReadRef {
                e: k.e,
                children: k.children,
                rev,
            })
            .collect();
        (ops, reads)
    }
}

/// Everything a handler may touch.
#[derive(Debug)]
pub struct Cx<'a> {
    pub tx: Tx<'a>,
    pub caller: &'a Caller,
    pub analysis: &'a AnalysisView,
    pub services: Services,
}

/// A synchronous command on the project actor.
pub trait Command: Sized + 'static {
    type Args: DeserializeOwned + JsonSchema;
    type Output: Serialize + JsonSchema;
    const META: CommandMeta;
    /// Reads through `cx.tx` are recorded (the read set); writes become field triples.
    fn run(args: Self::Args, cx: &mut Cx<'_>) -> Result<Self::Output, CmdError>;
}

/// A command that plans on the actor and runs off it.
pub trait JobCommand: Sized + 'static {
    type Args: DeserializeOwned + JsonSchema;
    type Output: Serialize + JsonSchema;
    const META: CommandMeta;
    /// Validates on the actor, pins a snapshot, returns the work to do off-actor.
    fn plan(args: Self::Args, cx: &mut Cx<'_>) -> Result<Box<dyn JobWork>, CmdError>;
}

/// Off-actor work of a [`JobCommand`].
pub trait JobWork: Send + 'static {
    fn run(self: Box<Self>, jx: JobCx) -> Result<JobOutcome, CmdError>;
}

/// What a job hands back.
#[derive(Debug)]
pub enum JobOutcome {
    Value(serde_json::Value),
    /// A proposal (SPEC §3.7). Shape: M2.
    Proposal(serde_json::Value),
    /// A stored analysis blob, by key.
    Analysis(String),
}

/// A job's context: pinned document and cancellation.
#[derive(Clone, Debug)]
pub struct JobCx {
    pub doc: Arc<Doc>,
    cancel: Arc<AtomicBool>,
}

impl JobCx {
    pub fn new(doc: Arc<Doc>, cancel: Arc<AtomicBool>) -> Self {
        Self { doc, cancel }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
