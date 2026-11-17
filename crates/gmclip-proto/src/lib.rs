//! Wire types (SPEC §3.2, §4.1.2). schemars 1.x is the source of truth for every
//! schema (`cargo xt gen`, M1); specta TS types are added at M2 (SPEC §3.2 Codegen).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Most bytes of caller input an error may echo (R-CMD-6 `Invalid`).
pub const MAX_ECHO: usize = 16;

/// `s` cut to at most [`MAX_ECHO`] bytes on a char boundary, with `…` when cut.
pub fn clip_echo(s: &str) -> String {
    if s.len() <= MAX_ECHO {
        return s.to_owned();
    }
    let mut end = MAX_ECHO;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Who holds an entity that a conflicting write touched (R-CMD-6): `human`,
/// `run:<id>` or `system`.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ConflictEntity {
    pub entity: String,
    pub current_rev: u64,
    pub by: String,
}

/// What a caller may do about a conflict (R-CMD-6, R-HIST-2).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictOption {
    RevertUntouched,
    RestoreToPoint,
    Cancel,
}

/// R-CMD-6 `Conflict{entities, fields?, current, diff, options?}`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Conflict {
    pub entities: Vec<ConflictEntity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    pub current: serde_json::Value,
    pub diff: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ConflictOption>>,
}

/// The closed error set with stable codes (R-CMD-6, SPEC v5). Where implementer Part C
/// differs, SPEC wins: no `Owner` code (Owner commands are not registered on any agent
/// surface), and `NotFound` / `Leased` carry no fields. Media errors carry
/// `{step, frame?, source?, os_status?}` and are mapped into this set by the caller (M1).
///
/// `Invalid.reason` never echoes more than 16 bytes of caller input.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "code")]
pub enum CmdError {
    /// Boxed: the payload is large and every `Result<_, CmdError>` pays for it.
    Conflict(Box<Conflict>),
    StaleAnalysis {
        kind: String,
        rev: u64,
    },
    NotFound,
    Invalid {
        path: String,
        reason: String,
    },
    Unauthorized {
        /// A scope name (§11.4) or one of `file_roots`, `cut_binding`, `owner`.
        scope: String,
    },
    /// Agents are paused, or this run is (R-PROTO-6: it kept asking): `pending` asks wait
    /// for a human, who resumes it at `resume` (decision 0050). An older `{"code":
    /// "Paused"}` reads as none pending.
    Paused {
        #[serde(default)]
        pending: u32,
        #[serde(default)]
        resume: String,
    },
    Leased,
    Busy {
        job: String,
    },
    Unsupported {
        platform: String,
        reason: String,
    },
    OverBudget {
        estimate: String,
        cap: String,
    },
    NeedsApproval {
        plan: serde_json::Value,
    },
    Cancelled,
    Internal {
        trace_id: String,
    },
}

impl CmdError {
    /// `Invalid` with the JSON-pointer `path` and caller-derived `echo` each clipped to
    /// 16 bytes (R-CMD-6); `reason` is our own text and is not clipped.
    pub fn invalid(path: &str, reason: &str) -> Self {
        CmdError::Invalid {
            path: clip_echo(path),
            reason: reason.to_owned(),
        }
    }

    /// `Invalid` quoting at most 16 bytes of the caller's value.
    pub fn invalid_value(path: &str, reason: &str, echo: &str) -> Self {
        CmdError::Invalid {
            path: clip_echo(path),
            reason: format!("{reason}: {}", clip_echo(echo)),
        }
    }

    /// The stable code, as serialised in the `code` tag.
    pub fn code(&self) -> &'static str {
        match self {
            CmdError::Conflict(_) => "Conflict",
            CmdError::StaleAnalysis { .. } => "StaleAnalysis",
            CmdError::NotFound => "NotFound",
            CmdError::Invalid { .. } => "Invalid",
            CmdError::Unauthorized { .. } => "Unauthorized",
            CmdError::Paused { .. } => "Paused",
            CmdError::Leased => "Leased",
            CmdError::Busy { .. } => "Busy",
            CmdError::Unsupported { .. } => "Unsupported",
            CmdError::OverBudget { .. } => "OverBudget",
            CmdError::NeedsApproval { .. } => "NeedsApproval",
            CmdError::Cancelled => "Cancelled",
            CmdError::Internal { .. } => "Internal",
        }
    }
}

impl core::fmt::Display for CmdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CmdError::Invalid { path, reason } => write!(f, "Invalid at {path}: {reason}"),
            CmdError::Unauthorized { scope } => write!(f, "Unauthorized (scope {scope})"),
            CmdError::Unsupported { platform, reason } => {
                write!(f, "Unsupported on {platform}: {reason}")
            }
            CmdError::Internal { trace_id } => write!(f, "Internal (trace {trace_id})"),
            CmdError::Paused { pending, resume } if !resume.is_empty() => {
                write!(f, "Paused ({pending} waiting for a human; {resume})")
            }
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for CmdError {}

/// A time on one of the four clocks (SPEC §4.1.2). `s` is a decimal string with at
/// most 6 decimal places; only `model::clock` converts between clocks.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "clock", rename_all = "snake_case")]
pub enum WireTime {
    /// Source clock: a source, or a sync group.
    Src {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        s: String,
    },
    /// Parts end to end (the editing clock).
    Cut { cut: String, s: String },
    /// From a part's or clip's start.
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        part: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        s: String,
    },
    /// Output clock of one framing, with enabled clips, quantised.
    Out {
        cut: String,
        framing: String,
        s: String,
    },
}

pub use gmclip_model::Rev;

/// A mutation's concurrency base (R-CC-1): per-entity revs (`{"part:01…": 12}`) or a
/// document rev from any read (`{"as_of": 40}`). `as_of` must be the only key: a mixed
/// object is refused, not read as one form with the rest dropped.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Base {
    AsOf {
        as_of: u64,
    },
    /// Keys are `<kind>:<ulid>` as every read's `display` names them.
    Revs(BTreeMap<String, u64>),
}

impl<'de> Deserialize<'de> for Base {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = BTreeMap::<String, u64>::deserialize(d)?;
        match m.remove("as_of") {
            Some(as_of) if m.is_empty() => Ok(Base::AsOf { as_of }),
            Some(_) => Err(serde::de::Error::custom(
                "`as_of` cannot be mixed with entity revs",
            )),
            None => Ok(Base::Revs(m)),
        }
    }
}

/// Who is calling (identity, connection or run, scopes). Built by the transport, never
/// by the caller (R-MCP-1).
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallerKind {
    /// The local human (UI or Owner CLI); fills `base` automatically.
    Human,
    /// An agent connection (MCP, CLI `command_run`, in-app agent).
    Agent,
    /// gmclip itself (migrations, imports, ASR switches).
    System,
}

/// One change in an event: the entity, and the field (and map key) for a field op.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EventTriple {
    pub entity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `set`, `create` or `delete`.
    pub op: String,
}

/// An event (SPEC §3.10): one committed transaction, or `Resync` after a subscriber's
/// queue overflowed (re-read the snapshot at `rev`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Committed {
        seq: u64,
        rev: u64,
        by: String,
        command: String,
        triples: Vec<EventTriple>,
        display: String,
    },
    Resync {
        seq: u64,
        rev: u64,
    },
}

impl Event {
    pub fn seq(&self) -> u64 {
        match self {
            Event::Committed { seq, .. } | Event::Resync { seq, .. } => *seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_time_is_tagged_by_clock() {
        let t: WireTime = serde_json::from_str(r#"{"clock":"cut","cut":"c1","s":"1.5"}"#).unwrap();
        assert_eq!(
            t,
            WireTime::Cut {
                cut: "c1".into(),
                s: "1.5".into()
            }
        );
    }

    /// A10: one fixture per error code — stable code, tagged JSON, round trip.
    #[test]
    fn every_error_code_has_a_fixture() {
        let all = vec![
            CmdError::Conflict(Box::new(Conflict {
                entities: vec![ConflictEntity {
                    entity: "part:01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                    current_rev: 7,
                    by: "human".into(),
                }],
                fields: Some(vec!["part:01ARZ3NDEKTSV4RRFFQ69G5FAV.gain_db".into()]),
                current: serde_json::json!({"rev": 7}),
                diff: serde_json::json!({"later_edits": ["rev 7 by human (part_set_gain)"]}),
                options: Some(vec![
                    ConflictOption::RevertUntouched,
                    ConflictOption::RestoreToPoint,
                    ConflictOption::Cancel,
                ]),
            })),
            CmdError::StaleAnalysis {
                kind: "energy".into(),
                rev: 3,
            },
            CmdError::NotFound,
            CmdError::invalid("/gain_db", "between -60 and +24 dB"),
            CmdError::Unauthorized {
                scope: "edit".into(),
            },
            CmdError::Paused {
                pending: 3,
                resume: "gmclip: Home → Needs you".into(),
            },
            CmdError::Leased,
            CmdError::Busy {
                job: "export".into(),
            },
            CmdError::Unsupported {
                platform: "linux".into(),
                reason: "no H.264 encoder".into(),
            },
            CmdError::OverBudget {
                estimate: "$2.10".into(),
                cap: "$1.00".into(),
            },
            CmdError::NeedsApproval {
                plan: serde_json::json!({"lint": ["number without provenance"]}),
            },
            CmdError::Cancelled,
            CmdError::Internal {
                trace_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            },
        ];
        let codes: Vec<&str> = all.iter().map(CmdError::code).collect();
        assert_eq!(
            codes,
            [
                "Conflict",
                "StaleAnalysis",
                "NotFound",
                "Invalid",
                "Unauthorized",
                "Paused",
                "Leased",
                "Busy",
                "Unsupported",
                "OverBudget",
                "NeedsApproval",
                "Cancelled",
                "Internal"
            ]
        );
        for e in all {
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["code"], e.code());
            assert_eq!(serde_json::from_value::<CmdError>(v).unwrap(), e);
            assert!(!e.to_string().is_empty());
        }
    }

    /// Decision 0050: `Paused` says how many asks wait and where the human resumes; the
    /// bare form older holders sent still reads.
    #[test]
    fn paused_says_where_to_resume() {
        let old: CmdError = serde_json::from_str(r#"{"code":"Paused"}"#).unwrap();
        assert_eq!(
            old,
            CmdError::Paused {
                pending: 0,
                resume: String::new()
            }
        );
        let e = CmdError::Paused {
            pending: 2,
            resume: "gmclip: Home → Needs you".into(),
        };
        assert!(e.to_string().contains("Home → Needs you"), "{e}");
    }

    #[test]
    fn echoes_are_clipped_to_16_bytes() {
        assert_eq!(clip_echo("short"), "short");
        assert_eq!(clip_echo("0123456789abcdefXYZ"), "0123456789abcdef…");
        // never splits a code point
        let s = "ééééééééé"; // 18 bytes
        assert_eq!(clip_echo(s), "éééééééé…");
        let CmdError::Invalid { path, reason } =
            CmdError::invalid_value("/args/part/that-is-long", "not a ULID", &"x".repeat(100))
        else {
            unreachable!()
        };
        assert!(path.len() <= 16 + 3);
        assert!(reason.matches('x').count() <= 16);
    }

    #[test]
    fn base_parses_both_forms() {
        let a: Base = serde_json::from_str(r#"{"as_of": 4}"#).unwrap();
        assert_eq!(a, Base::AsOf { as_of: 4 });
        let r: Base = serde_json::from_str(r#"{"part:01ARZ3NDEKTSV4RRFFQ69G5FAV": 3}"#).unwrap();
        assert!(matches!(r, Base::Revs(m) if m.len() == 1));
        // mixing the forms is not an as_of with the rest dropped
        let mixed: Result<Base, _> =
            serde_json::from_str(r#"{"as_of": 4, "part:01ARZ3NDEKTSV4RRFFQ69G5FAV": 3}"#);
        assert!(!matches!(mixed, Ok(Base::AsOf { .. })));
    }

    #[test]
    fn schemas_generate() {
        let s = schemars::schema_for!(CmdError);
        assert!(serde_json::to_string(&s).unwrap().contains("Conflict"));
    }
}
