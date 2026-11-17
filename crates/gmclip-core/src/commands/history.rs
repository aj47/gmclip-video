//! History commands (SPEC §3.6, R-HIST-2/3): undo, redo, runs, restore points. They
//! read history (transactions, runs, restore points), not just the document, so the
//! actor runs them itself (`crate::actor`); their registry entries carry the metadata,
//! schemas and examples like every other command, and `run` is never called.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::Cut;
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, Cx};

/// What to do when later edits touched the same fields (R-HIST-2).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UndoOption {
    /// (a) revert only the fields no later transaction touched.
    RevertUntouched,
    /// For runs: restore the cut to the run's restore point, dropping the later edits.
    RestoreToPoint,
    /// (c) do nothing.
    Cancel,
}

fn actor_only() -> CmdError {
    CmdError::Internal {
        trace_id: "actor-command".into(),
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UndoArgs {
    #[serde(default)]
    pub option: Option<UndoOption>,
    /// The human's yes to undoing (or redoing) an edit from an earlier session; without
    /// it the first such undo answers `needs_confirm` and changes nothing (R-HIST-1).
    #[serde(default)]
    pub confirm: bool,
}

/// What an undo, redo or restore did.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryResult {
    /// The rev of the transaction it committed, if any.
    pub rev: Option<u64>,
    /// The transaction it undid or redid.
    pub target: Option<String>,
    /// Later edits it dropped or left alone, as `<kind>:<id>.<field>[<key>]`.
    pub skipped: Vec<String>,
}

#[derive(Debug)]
pub struct Undo;

#[command(
    name = "undo",
    scope = "edit",
    undo = "undoable",
    doc = "Undo the caller's last transaction (the human's stack, or the agent's current run).",
    example = r#"{}"#,
    example = r#"{"option": "revert_untouched"}"#
)]
impl Command for Undo {
    type Args = UndoArgs;
    type Output = HistoryResult;
    fn run(_: UndoArgs, _: &mut Cx<'_>) -> Result<HistoryResult, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug)]
pub struct Redo;

#[command(
    name = "redo",
    scope = "edit",
    undo = "undoable",
    doc = "Redo the caller's last undone transaction.",
    example = r#"{}"#
)]
impl Command for Redo {
    type Args = UndoArgs;
    type Output = HistoryResult;
    fn run(_: UndoArgs, _: &mut Cx<'_>) -> Result<HistoryResult, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunBeginArgs {
    pub label: String,
    #[serde(default)]
    pub brief: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunBegun {
    pub run: String,
    /// The project-wide restore point written before the run.
    pub restore_point: String,
}

#[derive(Debug)]
pub struct RunBegin;

#[command(
    name = "run_begin",
    scope = "edit",
    undo = "read_only",
    doc = "Begin a run: group this connection's edits under one undoable unit, after a restore point.",
    example = r#"{"label": "tighten the open"}"#
)]
impl Command for RunBegin {
    type Args = RunBeginArgs;
    type Output = RunBegun;
    fn run(_: RunBeginArgs, _: &mut Cx<'_>) -> Result<RunBegun, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunEndArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunEnded {
    pub run: Option<String>,
    pub transactions: usize,
}

#[derive(Debug)]
pub struct RunEnd;

#[command(
    name = "run_end",
    scope = "edit",
    undo = "read_only",
    doc = "End this connection's current run.",
    example = r#"{}"#
)]
impl Command for RunEnd {
    type Args = RunEndArgs;
    type Output = RunEnded;
    fn run(_: RunEndArgs, _: &mut Cx<'_>) -> Result<RunEnded, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunUndoArgs {
    pub run: String,
    #[serde(default)]
    pub option: Option<UndoOption>,
}

#[derive(Debug)]
pub struct RunUndo;

#[command(
    name = "run_undo",
    scope = "edit",
    undo = "undoable",
    doc = "Undo a whole run: put every field it wrote back to its restore point (R-HIST-2).",
    example = r#"{"run": "01J00000000000000000000009"}"#,
    example = r#"{"run": "01J00000000000000000000009", "option": "restore_to_point"}"#
)]
impl Command for RunUndo {
    type Args = RunUndoArgs;
    type Output = HistoryResult;
    fn run(_: RunUndoArgs, _: &mut Cx<'_>) -> Result<HistoryResult, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestorePointArgs {
    /// A name makes it a named snapshot; without one it is a restore point.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RestorePointMade {
    pub id: String,
    pub rev: u64,
}

#[derive(Debug)]
pub struct RestorePointCreate;

#[command(
    name = "restore_point_create",
    scope = "edit",
    undo = "read_only",
    doc = "Record a restore point (or, with a name, a named snapshot) of the whole project now.",
    example = r#"{}"#,
    example = r#"{"name": "before the agent pass"}"#
)]
impl Command for RestorePointCreate {
    type Args = RestorePointArgs;
    type Output = RestorePointMade;
    fn run(_: RestorePointArgs, _: &mut Cx<'_>) -> Result<RestorePointMade, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreToArgs {
    /// A restore point or named snapshot id.
    pub snapshot: String,
    /// Restore only this cut's entities; the whole project when absent.
    #[serde(default)]
    pub cut: Option<Id<Cut>>,
}

#[derive(Debug)]
pub struct RestoreTo;

#[command(
    name = "restore_to",
    scope = "edit",
    undo = "undoable",
    doc = "Restore the project (or one cut) to a restore point or named snapshot, as one undoable step.",
    example = r#"{"snapshot": "01J0000000000000000000000A"}"#
)]
impl Command for RestoreTo {
    type Args = RestoreToArgs;
    type Output = HistoryResult;
    fn run(_: RestoreToArgs, _: &mut Cx<'_>) -> Result<HistoryResult, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryListArgs {
    /// Summarise each version for this cut (its parts and length then and now).
    #[serde(default)]
    pub cut: Option<Id<Cut>>,
    /// Newest first, at most this many (default 50, ≤ 200).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Only how many versions there are (`total`), for "↶ N earlier versions".
    #[serde(default)]
    pub count_only: bool,
}

/// One version in the history list.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryRow {
    pub id: String,
    /// `restore_point`, `named` or `run`.
    pub kind: String,
    pub name: Option<String>,
    pub rev: u64,
    /// ms since the Unix epoch.
    pub ts: i64,
    /// The agent run it was taken before, if any.
    pub run: Option<String>,
    /// With `cut`: its parts and seconds at that version (`null` when it did not exist).
    pub parts: Option<u32>,
    pub seconds: Option<f64>,
    /// Who edited the project after this version, up to the next newer one (or now):
    /// `human`, `local:<client>`, `launched:<run>`, `review-link:<id>`.
    pub then_by: Vec<String>,
    /// For a pre-run version: the run's identity and label.
    pub run_by: Option<String>,
    pub run_label: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryList {
    /// How many versions the project has in all.
    pub total: u32,
    pub versions: Vec<HistoryRow>,
    /// With `cut`: its parts and seconds now.
    pub now_parts: Option<u32>,
    pub now_seconds: Option<f64>,
}

#[derive(Debug)]
pub struct HistoryListCmd;

#[command(
    name = "history_list",
    scope = "read",
    undo = "read_only",
    doc = "List restore points, named snapshots and pre-run versions, newest first; with a cut, how many parts and seconds it had at each.",
    example = r#"{"cut": "01J00000000000000000000001", "limit": 20}"#
)]
impl Command for HistoryListCmd {
    type Args = HistoryListArgs;
    type Output = HistoryList;
    fn run(_: HistoryListArgs, _: &mut Cx<'_>) -> Result<HistoryList, CmdError> {
        Err(actor_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryDiffArgs {
    /// The version to compare from (a restore point, named snapshot or pre-run version).
    pub snapshot: String,
    pub cut: Id<Cut>,
    /// The version to compare to; the cut as it is now when absent.
    #[serde(default)]
    pub against: Option<String>,
}

/// One part of a cut at one version.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DiffPart {
    pub part: String,
    /// Its place in the cut (0-based).
    pub index: u32,
    /// The source its picture comes from.
    pub source: Option<String>,
    pub src_in_s: f64,
    pub src_out_s: f64,
    /// Where it starts on the cut's clock, and how long it plays.
    pub out_start_s: f64,
    pub dur_s: f64,
}

/// A part in both versions whose source range changed.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffTrim {
    pub from: DiffPart,
    pub to: DiffPart,
}

/// A cut between two versions (A2.13's visual diff).
#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryDiff {
    /// Parts and seconds at each side (`null` when the cut did not exist there).
    pub from_parts: Option<u32>,
    pub from_seconds: Option<f64>,
    pub to_parts: Option<u32>,
    pub to_seconds: Option<f64>,
    /// Every part at each side, in cut order (for drawing the two timelines).
    pub from: Vec<DiffPart>,
    pub to: Vec<DiffPart>,
    pub added: Vec<DiffPart>,
    pub removed: Vec<DiffPart>,
    pub trimmed: Vec<DiffTrim>,
    /// Parts in both whose order among the kept parts changed.
    pub moved: Vec<String>,
    /// Who changed the project between the two (`human`, `local:<client>`,
    /// `launched:<run>`, `review-link:<id>`), in order of their first edit.
    pub by: Vec<String>,
}

#[derive(Debug)]
pub struct HistoryDiffCmd;

#[command(
    name = "history_diff",
    scope = "read",
    undo = "read_only",
    doc = "Compare a cut at a version with now (or another version): parts added, removed, trimmed and moved, its length at each, and who edited in between.",
    example = r#"{"snapshot": "01J0000000000000000000000A", "cut": "01J00000000000000000000001"}"#
)]
impl Command for HistoryDiffCmd {
    type Args = HistoryDiffArgs;
    type Output = HistoryDiff;
    fn run(_: HistoryDiffArgs, _: &mut Cx<'_>) -> Result<HistoryDiff, CmdError> {
        Err(actor_only())
    }
}
