//! The UI's event payloads (SPEC §12.1 `gm_subscribe`; backlog A2.01): one tagged enum per
//! scope, the only thing [`crate::bus::Bus`] publishes, so what the webview receives is
//! what these types say. `cargo xt gen` writes their TypeScript (`app/ui/src/gen/
//! events.ts`, through gmclip-core's schema → TypeScript mapping) and `tests/ts_events.rs`
//! fails while the committed file and these types differ.
//!
//! Every scope can also get `{"kind": "resync"}` when a subscriber falls behind (the bus's
//! own, with no numbers; the project scope's core resync carries `seq` and `rev`).

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::RunView;
use crate::batch::BatchView;
use crate::jobs::JobView;
use crate::sessions::{Entry, SessionView};
use crate::viewer::ViewerState;

/// The `viewer` scope: the render thread's state and errors.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerEvent {
    /// The subscriber fell behind: read `viewer_state` again.
    Resync,
    /// The viewer's state changed (at most every 66 ms, and at once on a seek or drag).
    State { state: Box<ViewerState> },
    /// The last frame failed to render (`null`: it renders again); `cut`, the cut it was
    /// rendering when known (the UI drops an error about a cut no longer on screen).
    Error {
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<String>,
    },
    /// A cut's mask tracks (R.01) finished building or changed state.
    Mattes {
        project: String,
        cut: String,
        state: Option<String>,
    },
}

// (Large payloads are boxed: events move through the bus by value.)
/// The `jobs` scope: exports.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobsEvent {
    Resync,
    /// One export job's progress or end.
    Job {
        job: Box<JobView>,
    },
}

/// The `app` scope: the native menu and the speech model download.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppEvent {
    Resync,
    /// A native menu item was chosen (its id).
    Menu {
        id: String,
    },
    /// The speech model download: `running`, `paused`, `done` or `failed`.
    Models {
        state: ModelsState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelsState {
    Running,
    Paused,
    Done,
    Failed,
}

/// The `home` scope: what Home lists changed.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HomeEvent {
    Resync,
    /// Something Home lists changed (an export, a project opened or closed): read it again.
    Changed,
    /// Files dropped on the window (paths as the system gave them), and where: the
    /// pointer in CSS px of the window (decision 0042: a drop on the timeline lands there).
    Dropped {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
    },
    /// R-MCP-1c: the first connection from a new program (the activity rail shows it with
    /// Revoke). `identity` and `exe` are the program's own claims, shown as text.
    FirstSeen {
        ts: i64,
        identity: String,
        exe: String,
    },
    /// A review link asks for a click (Fix with agent, approve): Home's Needs you.
    NeedsYou {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<String>,
    },
}

/// The `project:<id>` scope: the core's committed transactions, and the app's own news
/// about the project (analysis, agent runs, batches, clips, proxies, review-link writes).
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectEvent {
    /// One committed transaction (SPEC §3.10).
    Committed {
        seq: u64,
        rev: u64,
        by: String,
        command: String,
        triples: Vec<gmclip_proto::EventTriple>,
        display: String,
    },
    /// The subscriber fell behind: re-read the project (the core's carries the rev).
    Resync {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<u64>,
    },
    /// Something about the project changed outside a transaction: read it again.
    Changed,
    /// A source was added, relinked or removed.
    Sources,
    /// A source's analysis or transcription progressed.
    Analysis(AnalysisProgress),
    /// A source's transcript was stored.
    Transcript { source: String },
    /// A launched agent run's state.
    AgentRun { run: Box<RunView> },
    /// A batch's progress (E.01).
    Batch { batch: Box<BatchView> },
    /// A moment's clip (H.10) is ready, or failed.
    Clip {
        moment: String,
        ready: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        picture: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        render_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A source's scrub proxy progressed.
    Proxy {
        project: String,
        source: String,
        phase: String,
        covered: u64,
        frames: u64,
    },
    /// A review link wrote a review on a cut.
    Review { cut: String },
    /// Comments or suggestions on a cut changed (`null`: some cut of the project).
    Comments {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<String>,
    },
    /// An export's file was verified again.
    ExportVerify { cut: String, export: String },
    /// The framing a cut shows changed.
    Output { cut: String },
}

/// The `agents` scope: agent sessions (decision 0035).
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsEvent {
    /// Read `session_list` (and the open session) again.
    Resync,
    /// A session started or changed.
    Session { session: Box<SessionView> },
    /// A transcript entry was added (`index` = its length before) or replaced.
    Entry {
        session: String,
        index: u32,
        entry: Box<Entry>,
    },
    /// Streamed text was added to the entry at `index`.
    Append {
        session: String,
        index: u32,
        text: String,
    },
    /// A session was removed from the list.
    Removed { session: String },
}

/// Where one source's analysis is (the `analysis` event).
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AnalysisProgress {
    pub source: String,
    /// `transcribing`, `analysing`, `done`, `failed`.
    pub phase: String,
    pub done_s: f64,
    pub total_s: f64,
    /// Words heard so far.
    pub words: u64,
    pub error: Option<String>,
    pub asr_error: Option<String>,
    pub job: Option<String>,
    pub resumed_at_s: Option<f64>,
    pub paused: bool,
    pub pauses: u32,
}

impl From<gmclip_proto::Event> for ProjectEvent {
    fn from(e: gmclip_proto::Event) -> Self {
        match e {
            gmclip_proto::Event::Committed {
                seq,
                rev,
                by,
                command,
                triples,
                display,
            } => Self::Committed {
                seq,
                rev,
                by,
                command,
                triples,
                display,
            },
            gmclip_proto::Event::Resync { seq, rev } => Self::Resync {
                seq: Some(seq),
                rev: Some(rev),
            },
        }
    }
}

/// A string schema limited to `values` (a field held as a plain string in Rust whose
/// values are a fixed few, typed as their union in TypeScript).
pub(crate) fn one_of(values: &[&str]) -> Schema {
    schemars::json_schema!({"type": "string", "enum": values})
}

/// [`one_of`] or null.
pub(crate) fn one_of_or_null(values: &[&str]) -> Schema {
    let mut v: Vec<Value> = values.iter().map(|x| json!(x)).collect();
    v.push(Value::Null);
    schemars::json_schema!({"enum": v})
}

/// The scopes and their event types, in the order the generated file lists them.
pub fn scope_schemas() -> Vec<(&'static str, &'static str, Value)> {
    fn s<T: JsonSchema>() -> Value {
        serde_json::to_value(SchemaGenerator::default().into_root_schema_for::<T>())
            .unwrap_or(Value::Null)
    }
    vec![
        ("viewer", "ViewerEvent", s::<ViewerEvent>()),
        ("jobs", "JobsEvent", s::<JobsEvent>()),
        ("app", "AppEvent", s::<AppEvent>()),
        ("home", "HomeEvent", s::<HomeEvent>()),
        ("agents", "AgentsEvent", s::<AgentsEvent>()),
        ("project", "ProjectEvent", s::<ProjectEvent>()),
    ]
}

/// The TypeScript module of the event payloads (`app/ui/src/gen/events.ts`).
pub fn events_ts() -> String {
    let scopes = scope_schemas();
    let (mut out, names) = gmclip_core::tsgen::module_ts(
        "// Generated by `cargo xt gen` from the app's event types (app/src-tauri src/events.rs) \
         through gmclip-core's src/tsgen.rs. Do not edit: change the Rust types and run it again.\n\
         // app/src-tauri tests/ts_events.rs fails while this file and the types disagree.\n",
        scopes
            .iter()
            .map(|(_, name, schema)| ((*name).to_owned(), schema.clone())),
    );
    out.push_str(
        "/** Each `gm_subscribe` scope's events (`project` is every `project:<id>`). */\nexport interface Scopes {\n",
    );
    for ((scope, _, _), name) in scopes.iter().zip(&names) {
        out.push_str(&format!("  {scope}: {name};\n"));
    }
    out.push_str("}\n");
    out
}

/// Gives the schema generator's `&mut` to [`one_of`] in a `schema_with` attribute.
pub(crate) fn job_phase(_: &mut SchemaGenerator) -> Schema {
    one_of(&["captions", "audio", "video"])
}

/// `scrub_path`: where the last frame's pictures came from.
pub(crate) fn scrub_path(_: &mut SchemaGenerator) -> Schema {
    one_of_or_null(&["exact", "proxy", "keyframe"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialise_with_their_kind_tag() {
        let v = serde_json::to_value(HomeEvent::Dropped {
            paths: vec!["/a.mp4".into()],
            at: None,
        })
        .unwrap();
        assert_eq!(v, json!({"kind": "dropped", "paths": ["/a.mp4"]}));
        assert_eq!(
            serde_json::to_value(ViewerEvent::Resync).unwrap(),
            json!({"kind": "resync"})
        );
        let core = gmclip_proto::Event::Resync { seq: 3, rev: 9 };
        assert_eq!(
            serde_json::to_value(ProjectEvent::from(core.clone())).unwrap(),
            serde_json::to_value(core).unwrap(),
            "the core's events pass through unchanged"
        );
        assert_eq!(
            serde_json::to_value(AppEvent::Models {
                state: ModelsState::Paused,
                done: None,
                total: None,
                file: None,
                error: None
            })
            .unwrap(),
            json!({"kind": "models", "state": "paused"})
        );
    }

    #[test]
    fn the_module_names_every_scope() {
        let ts = events_ts();
        for s in [
            "viewer: ViewerEvent;",
            "project: ProjectEvent;",
            "export type JobView = {",
        ] {
            assert!(ts.contains(s), "{s} missing:\n{ts}");
        }
        assert!(ts.contains("\"captions\" | \"audio\" | \"video\""), "{ts}");
    }
}
