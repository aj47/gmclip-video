//! Ops and transactions (implementer Part C; SPEC §3.2, §3.6).
//!
//! An op is a field triple `(entity, FieldPath, before, after)` or a whole-entity
//! create/delete carrying the entity's canonical CBOR. A transaction is the unit of
//! history, replay and undo; its ops are the **net** change from the document it was
//! built on, so `Doc::apply(txn)` and the inverse ops are exact.
//!
//! On disk an op names its field by **name**, not index, so reordering struct fields
//! is not a format change.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::entity::{FieldPath, FieldValue, ModelError};
use crate::ids::{EntityId, EntityKind, RequestId, Rev, RunId, TxnId, Ulid};
use crate::kinds::kind_ops;

/// One field triple, or a whole-entity create/delete.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Op {
    Set {
        e: EntityId,
        f: FieldPath,
        before: FieldValue,
        after: FieldValue,
    },
    Create {
        e: EntityId,
        value: FieldValue,
    },
    Delete {
        e: EntityId,
        value: FieldValue,
    },
}

impl Op {
    pub fn entity(&self) -> EntityId {
        match self {
            Op::Set { e, .. } | Op::Create { e, .. } | Op::Delete { e, .. } => *e,
        }
    }

    /// The op that undoes this one.
    pub fn inverse(&self) -> Op {
        match self {
            Op::Set {
                e,
                f,
                before,
                after,
            } => Op::Set {
                e: *e,
                f: f.clone(),
                before: after.clone(),
                after: before.clone(),
            },
            Op::Create { e, value } => Op::Delete {
                e: *e,
                value: value.clone(),
            },
            Op::Delete { e, value } => Op::Create {
                e: *e,
                value: value.clone(),
            },
        }
    }

    /// The `(entity, field path)` this op writes; `None` for create/delete, which
    /// write every field.
    pub fn path(&self) -> Option<&FieldPath> {
        match self {
            Op::Set { f, .. } => Some(f),
            _ => None,
        }
    }
}

/// The inverse of a list of ops: each op inverted, in reverse order.
pub fn inverse_ops(ops: &[Op]) -> Vec<Op> {
    ops.iter().rev().map(Op::inverse).collect()
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum OpWire {
    Set {
        k: EntityKind,
        id: Ulid,
        f: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        before: FieldValue,
        after: FieldValue,
    },
    Create {
        k: EntityKind,
        id: Ulid,
        value: FieldValue,
    },
    Delete {
        k: EntityKind,
        id: Ulid,
        value: FieldValue,
    },
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let w = match self {
            Op::Set {
                e,
                f,
                before,
                after,
            } => {
                let ops =
                    kind_ops(e.kind).ok_or_else(|| serde::ser::Error::custom("unknown kind"))?;
                let name = ops
                    .fields()
                    .get(usize::from(f.field.0))
                    .ok_or_else(|| serde::ser::Error::custom("unknown field"))?
                    .name;
                OpWire::Set {
                    k: e.kind,
                    id: e.id,
                    f: name.to_owned(),
                    key: f.key.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }
            }
            Op::Create { e, value } => OpWire::Create {
                k: e.kind,
                id: e.id,
                value: value.clone(),
            },
            Op::Delete { e, value } => OpWire::Delete {
                k: e.kind,
                id: e.id,
                value: value.clone(),
            },
        };
        w.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match OpWire::deserialize(d)? {
            OpWire::Set {
                k,
                id,
                f,
                key,
                before,
                after,
            } => {
                let ops = kind_ops(k).ok_or_else(|| serde::de::Error::custom("unknown kind"))?;
                let desc = ops
                    .fields()
                    .iter()
                    .find(|d| d.name == f)
                    .ok_or_else(|| serde::de::Error::custom("unknown field"))?;
                if desc.map != key.is_some() {
                    return Err(serde::de::Error::custom("map key mismatch"));
                }
                Op::Set {
                    e: EntityId { kind: k, id },
                    f: FieldPath {
                        field: desc.id,
                        key,
                    },
                    before,
                    after,
                }
            }
            OpWire::Create { k, id, value } => Op::Create {
                e: EntityId { kind: k, id },
                value,
            },
            OpWire::Delete { k, id, value } => Op::Delete {
                e: EntityId { kind: k, id },
                value,
            },
        })
    }
}

/// Who wrote a transaction. Human ops are never in a run (SPEC §3.6).
#[derive(
    Copy,
    Clone,
    Eq,
    Ord,
    PartialEq,
    PartialOrd,
    Hash,
    Debug,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    #[default]
    Human,
    Run(RunId),
    System,
}

impl Author {
    /// `human`, `run:<id>` or `system` (R-CMD-6 `Conflict.entities[].by`).
    pub fn label(&self) -> String {
        match self {
            Author::Human => "human".into(),
            Author::Run(r) => format!("run:{}", r.0),
            Author::System => "system".into(),
        }
    }
}

/// `name@version` of the command that produced a transaction.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CommandKey {
    pub name: String,
    pub version: u16,
}

/// One entry of a transaction's recorded read set: an entity at a rev, or (with
/// `children`) the membership/order index of one child kind under an owner.
#[derive(
    Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
pub struct ReadRef {
    pub e: EntityId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<EntityKind>,
    pub rev: Rev,
}

/// A committed transaction: the unit of history, replay and undo.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Txn {
    pub rev: Rev,
    pub id: TxnId,
    pub author: Author,
    pub run: Option<RunId>,
    pub command: CommandKey,
    pub request_id: Option<RequestId>,
    /// The transaction this one undoes, if it is an undo.
    pub reverts: Option<TxnId>,
    /// The recorded read set.
    pub reads: Vec<ReadRef>,
    pub ops: Vec<Op>,
    /// Commit time, ms since the Unix epoch.
    pub ts: i64,
}

impl Txn {
    /// Canonical CBOR, the op-row form (SPEC §4.4).
    pub fn to_cbor(&self) -> Vec<u8> {
        crate::cbor::to_vec(self)
    }

    pub fn from_cbor(b: &[u8]) -> Result<Self, ModelError> {
        crate::cbor::from_slice(b)
    }

    /// The ops that undo this transaction.
    pub fn inverse_ops(&self) -> Vec<Op> {
        inverse_ops(&self.ops)
    }
}
