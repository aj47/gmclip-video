//! The run's handoff summary (SPEC §5.14 review board, §10.2 "AI EDIT PASS"; backlog
//! A.04): `run_summary_set` records the summary text (untrusted) and the run's
//! `reversible_choices[]`, each naming the fields that undo one judgement call and the
//! value each goes back to; `run_choice_restore` puts exactly those values back in one
//! transaction.
//!
//! Values are checked when recorded: each must decode as that field of that entity, and
//! is stored as the canonical JSON of what the field then holds, so a restore writes the
//! same bytes the record was checked with. Every entity named must belong to the cut, and
//! no restore may move an entity to another parent.

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::kinds::{kind_by_name, kind_ops};
use gmclip_model::{Doc, EntityId, FieldPath, FieldValue, Id, Op, RunId, Ulid};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{Command, Cx, model_err};

/// Bounds on what a run may record.
pub const MAX_TEXT_CHARS: usize = 4_000;
pub const MAX_CHOICES: usize = 20;
pub const MAX_FIELDS: usize = 16;
pub const MAX_LABEL_CHARS: usize = 300;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// One field a choice restores.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreArg {
    /// `<kind>:<ULID>`, e.g. `part:01J…`; the cut itself or an entity inside it.
    pub entity: String,
    /// The field's name, as `cut_get` shows it.
    pub field: String,
    /// For a map field, the key.
    #[serde(default)]
    pub key: Option<String>,
    /// The value the restore puts back (what the field held before the run chose).
    pub value: Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChoiceArg {
    /// What the run chose, in a sentence.
    pub label: String,
    pub restore: Vec<RestoreArg>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunSummarySetArgs {
    pub cut: Id<Cut>,
    pub text: String,
    #[serde(default)]
    pub reversible_choices: Vec<ChoiceArg>,
    /// The run summarised. An agent's is its own open run (the server fills it in and
    /// ignores what is given); other callers name one.
    #[serde(default)]
    pub run: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunSummaryRecorded {
    pub run: String,
    pub choices: usize,
}

/// Records a run's handoff summary on its cut.
#[derive(Debug)]
pub struct RunSummarySet;

#[command(
    name = "run_summary_set",
    scope = "edit",
    undo = "undoable",
    doc = "Record this run's handoff summary on its cut: the summary text and its reversible choices, each the judgement call in a sentence plus the fields (and their values before the choice) that undo it. The cut's status pill (beside its export) shows it, marked as the agent's words, with a one-click restore per choice.",
    example = r#"{"cut": "01J00000000000000000000001", "text": "Trimmed the open to the result; kept the uh at 0:12.", "reversible_choices": [{"label": "Started on the result, not the prompt", "restore": [{"entity": "part:01J00000000000000000000002", "field": "src_in", "value": 70560000000}]}]}"#
)]
impl Command for RunSummarySet {
    type Args = RunSummarySetArgs;
    type Output = RunSummaryRecorded;
    fn run(a: RunSummarySetArgs, cx: &mut Cx<'_>) -> Result<RunSummaryRecorded, CmdError> {
        let run = a
            .run
            .as_deref()
            .ok_or_else(|| CmdError::invalid("/run", "required"))
            .and_then(|r| {
                Ulid::parse(r)
                    .map(RunId)
                    .map_err(|_| CmdError::invalid("/run", "a run id"))
            })?;
        if a.text.chars().count() > MAX_TEXT_CHARS {
            return Err(CmdError::invalid("/text", "at most 4000 characters"));
        }
        if a.reversible_choices.len() > MAX_CHOICES {
            return Err(CmdError::invalid("/reversible_choices", "at most 20"));
        }
        cx.tx.get(a.cut)?;
        let mut choices = Vec::with_capacity(a.reversible_choices.len());
        for (i, c) in a.reversible_choices.iter().enumerate() {
            let at = format!("/reversible_choices/{i}");
            if c.label.trim().is_empty() || c.label.chars().count() > MAX_LABEL_CHARS {
                return Err(CmdError::invalid(
                    &format!("{at}/label"),
                    "1–300 characters",
                ));
            }
            if c.restore.is_empty() || c.restore.len() > MAX_FIELDS {
                return Err(CmdError::invalid(&format!("{at}/restore"), "1–16 fields"));
            }
            let mut restore = Vec::with_capacity(c.restore.len());
            for (j, r) in c.restore.iter().enumerate() {
                let at = format!("{at}/restore/{j}");
                let entity = parse_entity(&r.entity)
                    .ok_or_else(|| CmdError::invalid(&format!("{at}/entity"), "<kind>:<ULID>"))?;
                let text = serde_json::to_string(&r.value).unwrap_or_default();
                if text.len() > MAX_VALUE_BYTES {
                    return Err(CmdError::invalid(&format!("{at}/value"), "too large"));
                }
                let fr = FieldRestore {
                    entity,
                    field: r.field.clone(),
                    key: r.key.clone(),
                    value: text,
                };
                let (_, canonical) = resolve(cx.tx.peek(), a.cut, &fr, &at)?;
                // stored as what the field would hold, so a restore writes these bytes
                let back: Value = gmclip_model::cbor::from_slice(&canonical.0)
                    .map_err(|_| CmdError::invalid(&format!("{at}/value"), "not a JSON value"))?;
                restore.push(FieldRestore {
                    value: serde_json::to_string(&back).unwrap_or_default(),
                    ..fr
                });
            }
            choices.push(ReversibleChoice {
                label: c.label.clone(),
                restore,
            });
        }
        let n = choices.len();
        cx.tx.update(a.cut, |cut| {
            cut.last_run_summary = Some(RunSummary {
                run,
                text: a.text.clone(),
                reversible_choices: choices,
            });
        })?;
        Ok(RunSummaryRecorded {
            run: run.0.to_string(),
            choices: n,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunChoiceRestoreArgs {
    pub cut: Id<Cut>,
    /// The choice's index in the cut's `last_run_summary.reversible_choices`.
    pub choice: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChoiceRestored {
    /// Fields written.
    pub restored: usize,
    /// Fields that already held the recorded value.
    pub unchanged: usize,
}

/// Restores one recorded choice.
#[derive(Debug)]
pub struct RunChoiceRestore;

#[command(
    name = "run_choice_restore",
    scope = "edit",
    undo = "undoable",
    doc = "Undo one judgement call of the cut's last run: every field the choice recorded goes back to exactly its recorded value, in one undoable transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "choice": 0}"#
)]
impl Command for RunChoiceRestore {
    type Args = RunChoiceRestoreArgs;
    type Output = ChoiceRestored;
    fn run(a: RunChoiceRestoreArgs, cx: &mut Cx<'_>) -> Result<ChoiceRestored, CmdError> {
        let choice = cx
            .tx
            .get(a.cut)?
            .last_run_summary
            .as_ref()
            .and_then(|s| s.reversible_choices.get(a.choice as usize))
            .cloned()
            .ok_or_else(|| CmdError::invalid("/choice", "no such recorded choice"))?;
        let mut ops = Vec::new();
        let mut unchanged = 0;
        for (j, fr) in choice.restore.iter().enumerate() {
            let at = format!("/restore/{j}");
            // every entity written counts as read when the transaction finishes
            let e = fr.entity;
            let (path, after) = resolve(cx.tx.peek(), a.cut, fr, &at)?;
            let before = kind_ops(e.kind)
                .ok_or(CmdError::NotFound)?
                .field(cx.tx.peek(), e.id, &path)
                .map_err(model_err)?;
            if before == after {
                unchanged += 1;
                continue;
            }
            ops.push(Op::Set {
                e,
                f: path,
                before,
                after,
            });
        }
        let restored = ops.len();
        cx.tx.staging().apply_ops(&ops).map_err(model_err)?;
        Ok(ChoiceRestored {
            restored,
            unchanged,
        })
    }
}

/// `part:01J…` → the entity id.
pub fn parse_entity(s: &str) -> Option<EntityId> {
    let (k, id) = s.split_once(':')?;
    Some(EntityId {
        kind: kind_by_name(k)?,
        id: Ulid::parse(id).ok()?,
    })
}

/// `v` with each integer turned into a float where `like` (the field's current value)
/// has a float in the same place.
fn floats_like(v: &Value, like: &Value) -> Value {
    match (v, like) {
        (Value::Number(n), Value::Number(l)) if l.is_f64() && !n.is_f64() => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Value::Number),
        (Value::Object(o), Value::Object(l)) => Value::Object(
            o.iter()
                .map(|(k, x)| {
                    (
                        k.clone(),
                        l.get(k).map_or_else(|| x.clone(), |y| floats_like(x, y)),
                    )
                })
                .collect(),
        ),
        (Value::Array(a), Value::Array(l)) => Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, x)| {
                    l.get(i)
                        .or(l.first())
                        .map_or_else(|| x.clone(), |y| floats_like(x, y))
                })
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// The cut an entity belongs to (walking parents).
fn cut_of(doc: &Doc, mut e: EntityId) -> Option<Id<Cut>> {
    for _ in 0..64 {
        if e.kind == <Cut as gmclip_model::Entity>::KIND {
            return Some(Id::new(e.id));
        }
        e = doc.parent_of(e)?;
    }
    None
}

/// Checks a recorded field against `doc`: the entity exists inside `cut`, the field is
/// one of its kind's (with a key exactly when it is a map), the value decodes as that
/// field, and writing it would not move the entity. Returns the path and the canonical
/// bytes.
fn resolve(
    doc: &Doc,
    cut: Id<Cut>,
    fr: &FieldRestore,
    at: &str,
) -> Result<(FieldPath, FieldValue), CmdError> {
    let e = fr.entity;
    let ops =
        kind_ops(e.kind).ok_or_else(|| CmdError::invalid(&format!("{at}/entity"), "a kind"))?;
    if cut_of(doc, e) != Some(cut) {
        return Err(CmdError::invalid(
            &format!("{at}/entity"),
            "an entity of this cut (it is gone, or belongs elsewhere)",
        ));
    }
    let desc = ops
        .fields()
        .iter()
        .find(|d| d.name == fr.field)
        .ok_or_else(|| CmdError::invalid(&format!("{at}/field"), "a field of that entity"))?;
    if desc.map != fr.key.is_some() {
        return Err(CmdError::invalid(
            &format!("{at}/key"),
            if desc.map {
                "that field is a map: name the key"
            } else {
                "that field is not a map"
            },
        ));
    }
    let path = FieldPath {
        field: desc.id,
        key: fr.key.clone(),
    };
    let json: Value = serde_json::from_str(&fr.value)
        .map_err(|_| CmdError::invalid(&format!("{at}/value"), "JSON"))?;
    let before = ops.field(doc, e.id, &path).map_err(model_err)?;
    let canonical = |v: &Value| {
        let raw = gmclip_model::cbor::try_to_vec(v).ok()?;
        ops.canonical_field(doc, e.id, &path, &FieldValue(raw)).ok()
    };
    // JSON has one number type: where the field holds a float, `0` means `0.0`
    let like: Option<Value> = gmclip_model::cbor::from_slice(&before.0).ok();
    let canonical = canonical(&json)
        .or_else(|| {
            like.as_ref()
                .and_then(|l| canonical(&floats_like(&json, l)))
        })
        .ok_or_else(|| CmdError::invalid(&format!("{at}/value"), "a value of that field's type"))?;
    // never re-parent: the entity keeps its owner
    let mut trial = doc.clone();
    if before != canonical {
        trial
            .apply_ops(
                &[Op::Set {
                    e,
                    f: path.clone(),
                    before,
                    after: canonical.clone(),
                }],
                doc.rev() + 1,
                gmclip_model::Author::System,
            )
            .map_err(|_| CmdError::invalid(&format!("{at}/value"), "a value of that field"))?;
        if trial.parent_of(e) != doc.parent_of(e) {
            return Err(CmdError::invalid(
                &format!("{at}/field"),
                "a field that does not move the entity",
            ));
        }
    }
    Ok((path, canonical))
}
