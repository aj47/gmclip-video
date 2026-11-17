//! Review notes and keep/change/drop verdicts written through the review link (SPEC
//! §5.14.1; SR R-REV-3/4). The only writer is the review-link server, which calls with
//! a system caller whose identity is `review-link:<link id>[/<tailnet login>]`; that
//! identity becomes the review's author, never `human`. Agents cannot reach it: it needs
//! the `admin` scope (never an agent default) and the MCP surface denies it by name.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_proto::{CallerKind, CmdError};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Created, created};
use crate::command::{Command, Cx};

/// The longest review note kept.
pub const MAX_NOTE_CHARS: usize = 2_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewAddArgs {
    pub cut: Id<Cut>,
    #[serde(default)]
    pub text: String,
    pub verdict: Verdict,
    /// The export the note is about (its export id).
    #[serde(default)]
    pub export_ref: Option<String>,
}

/// Adds a review to a cut (review link only).
#[derive(Debug)]
pub struct ReviewAdd;

#[command(
    name = "review_add",
    scope = "admin",
    undo = "undoable",
    doc = "Add a review note and keep/change/drop verdict to a cut, attributed to the review link that wrote it. Written only by the review-link server.",
    example = r#"{"cut": "01J00000000000000000000001", "text": "too jumpy", "verdict": "change"}"#
)]
impl Command for ReviewAdd {
    type Args = ReviewAddArgs;
    type Output = Created;
    fn run(a: ReviewAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let author = cx.caller.identity.clone();
        let link = author.strip_prefix("review-link:").unwrap_or_default();
        if cx.caller.kind != CallerKind::System
            || link.is_empty()
            || author.len() > 200
            || !author
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_:/.@+".contains(c))
        {
            return Err(CmdError::Unauthorized {
                scope: "review_link".into(),
            });
        }
        if a.text.chars().count() > MAX_NOTE_CHARS {
            return Err(CmdError::invalid("/text", "at most 2000 characters"));
        }
        if a.export_ref
            .as_deref()
            .is_some_and(|e| e.len() > 64 || !e.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(CmdError::invalid("/export_ref", "an export id"));
        }
        cx.tx.get(a.cut)?;
        let id: Id<Review> = cx.tx.mint();
        cx.tx.create(Review {
            id,
            cut: a.cut,
            at_ms: cx.services.now_ms,
            author,
            text: a.text,
            verdict: a.verdict,
            export_ref: a.export_ref,
        })?;
        Ok(created(id))
    }
}
