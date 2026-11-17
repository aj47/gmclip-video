//! Packaging a cut (decision 0024): hook lines built on measured numbers, and title /
//! description drafts per platform as proposals.
//!
//! - `hook_text_suggest` (read): on-screen hook lines of 5–8 words, each on a number the
//!   clip measurably shows (a measured run, a count of runs, a number said), each naming
//!   where its number comes from and carrying the `number_provenance` to record with it
//!   (R-AI-4). Deterministic, no model ([`crate::hooktext`]).
//! - Title / description drafts per platform (YouTube, Shorts, TikTok, Instagram) are
//!   `CopyDraft` proposals: an agent pass writes them (`copy_drafts_add`, through the
//!   `publish_copy` pass contract), the human accepts, edits or rejects them
//!   (`copy_draft_state`). An agent's draft cites only numbers the cut backs and carries
//!   no links. Nothing here publishes or approves anything.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use crate::command::{Command, Cx};
use crate::hooktext::{self, HookSuggestion};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookTextSuggestArgs {
    pub cut: Id<Cut>,
    /// At most this many lines (default 6, ≤ 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HookTextSuggestions {
    pub suggestions: Vec<HookSuggestion>,
    /// Measured runs on the cut's video, and numbers said in it, that were read.
    pub measured_runs: usize,
    /// Why there are none, when there are none.
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct HookTextSuggest;

#[command(
    name = "hook_text_suggest",
    scope = "read",
    undo = "read_only",
    since = "M2",
    doc = "Suggest on-screen hook lines (5–8 words) built on a number the clip measurably shows: a measured run (measurement_add, a run clock), a count of measured runs, or a number said in the cut. Each names where its number comes from and gives the number_provenance to record with it; no number is invented.",
    example = r#"{"cut": "01J00000000000000000000001"}"#,
    example = r#"{"cut": "01J00000000000000000000001", "limit": 3}"#
)]
impl Command for HookTextSuggest {
    type Args = HookTextSuggestArgs;
    type Output = HookTextSuggestions;
    fn run(a: HookTextSuggestArgs, cx: &mut Cx<'_>) -> Result<HookTextSuggestions, CmdError> {
        cx.tx.get(a.cut)?;
        let _ = cx.tx.children::<Part>(a.cut.entity());
        let _ = cx.tx.children::<Overlay>(a.cut.entity());
        let limit = a.limit.unwrap_or(hooktext::DEFAULT_LIMIT).clamp(1, 20);
        let doc = cx.tx.peek();
        let runs = hooktext::measured_runs(doc, a.cut).len();
        let suggestions = hooktext::suggest(doc, cx.analysis, a.cut, limit);
        let note = suggestions.is_empty().then(|| {
            if hooktext::cut_words(doc, cx.analysis, a.cut).is_empty() && runs == 0 {
                "nothing to build on yet: no transcript and no measured run (mark a run on the preview, or transcribe the video)".to_owned()
            } else {
                "no number in this cut: measure a run (mark its start and end) to get a hook built on it".to_owned()
            }
        });
        Ok(HookTextSuggestions {
            suggestions,
            measured_runs: runs,
            note,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Title / description drafts (proposals)
// ---------------------------------------------------------------------------------------

/// A platform's limits for a draft: `(title chars, description chars, what the title is)`.
pub fn platform_rules(p: CopyPlatform) -> (usize, usize, &'static str) {
    match p {
        CopyPlatform::Youtube => (100, 5000, "the video title"),
        CopyPlatform::Shorts => (100, 5000, "the Short's title"),
        CopyPlatform::Tiktok => (90, 2200, "the caption's first line"),
        CopyPlatform::Instagram => (100, 2200, "the caption's first line"),
    }
}

/// Drafts a cut keeps (every state).
pub const MAX_DRAFTS: usize = 60;

/// Why `text` cannot be a draft's field, if it cannot.
fn draft_text_problem(text: &str, max: usize, multiline: bool) -> Option<String> {
    if text.trim().is_empty() {
        return Some("empty".into());
    }
    if text.chars().count() > max {
        return Some(format!("longer than {max} characters"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && !(multiline && c == '\n'))
    {
        return Some(if multiline {
            "control characters".into()
        } else {
            "one line".into()
        });
    }
    None
}

fn has_link(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("http://") || l.contains("https://") || l.contains("www.")
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftIn {
    pub platform: CopyPlatform,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyDraftsAddArgs {
    pub cut: Id<Cut>,
    /// At most 8 at a time.
    pub drafts: Vec<DraftIn>,
    /// The pass and prompt version that wrote them (R-AI-5).
    #[serde(default)]
    pub pass_ref: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DraftRefused {
    pub index: usize,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DraftsAdded {
    /// The new drafts' ids, in the order given (refused ones skipped).
    pub ids: Vec<String>,
    pub refused: Vec<DraftRefused>,
}

#[derive(Debug)]
pub struct CopyDraftsAdd;

#[command(
    name = "copy_drafts_add",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Add title/description drafts for a cut, one per platform (youtube, shorts, tiktok, instagram), as proposals the human accepts, edits or rejects. An agent's drafts may cite only numbers the cut backs (said in it, or measured) and carry no links; refused ones are listed with why. Nothing is published or approved.",
    example = r#"{"cut": "01J00000000000000000000001", "drafts": [{"platform": "youtube", "title": "My PC made this video in 67 seconds", "description": "A local video model on one GPU, start to finish."}]}"#
)]
impl Command for CopyDraftsAdd {
    type Args = CopyDraftsAddArgs;
    type Output = DraftsAdded;
    fn run(a: CopyDraftsAddArgs, cx: &mut Cx<'_>) -> Result<DraftsAdded, CmdError> {
        let human = cx.caller.kind == gmclip_proto::CallerKind::Human;
        if cx.caller.kind == gmclip_proto::CallerKind::System {
            return Err(CmdError::Unauthorized {
                scope: "author".into(),
            });
        }
        cx.tx.get(a.cut)?;
        let _ = cx.tx.children::<Part>(a.cut.entity());
        if a.drafts.is_empty() || a.drafts.len() > 8 {
            return Err(CmdError::invalid("/drafts", "one to eight drafts"));
        }
        if let Some(r) = &a.pass_ref {
            super::bounded(r, "/pass_ref", 200)?;
        }
        let existing = cx.tx.children::<CopyDraft>(a.cut.entity()).len();
        if existing + a.drafts.len() > MAX_DRAFTS {
            return Err(CmdError::invalid(
                "/cut",
                "this cut has too many drafts: reject or remove some first",
            ));
        }
        let grounds = hooktext::Grounds::of(cx.tx.peek(), cx.analysis, a.cut);
        let mut ids = Vec::new();
        let mut refused = Vec::new();
        for (index, d) in a.drafts.into_iter().enumerate() {
            let (tmax, dmax, _) = platform_rules(d.platform);
            let problem = draft_text_problem(&d.title, tmax, false)
                .map(|p| format!("title: {p}"))
                .or_else(|| {
                    draft_text_problem(&d.description, dmax, true)
                        .map(|p| format!("description: {p}"))
                })
                .or_else(|| {
                    (!human && (has_link(&d.title) || has_link(&d.description)))
                        .then(|| "links are the human's to add".to_owned())
                })
                .or_else(|| {
                    if human {
                        return None;
                    }
                    grounds
                        .unbacked(&d.title)
                        .or_else(|| grounds.unbacked(&d.description))
                        .map(|n| {
                            format!(
                                "the number {} is not said in this cut or measured in it",
                                crate::lint::clip16(&n)
                            )
                        })
                });
            if let Some(reason) = problem {
                refused.push(DraftRefused { index, reason });
                continue;
            }
            let id: Id<CopyDraft> = cx.tx.mint();
            cx.tx.create(CopyDraft {
                id,
                cut: a.cut,
                platform: d.platform,
                title: d.title.trim().to_owned(),
                description: d.description.trim().to_owned(),
                state: ProposalState::Proposed,
                origin: if human {
                    EditOrigin::Human
                } else {
                    EditOrigin::Agent
                },
                edited: false,
                pass_ref: a.pass_ref.clone(),
                author: cx.caller.identity.clone(),
                at_ms: cx.services.now_ms,
            })?;
            ids.push(id.0.to_string());
        }
        Ok(DraftsAdded { ids, refused })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyDraftStateArgs {
    pub draft: Id<CopyDraft>,
    /// Accept, reject, or put back to proposed. Editing the text accepts it unless a
    /// state is given.
    #[serde(default)]
    pub state: Option<ProposalState>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct CopyDraftState;

#[command(
    name = "copy_draft_state",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Accept, edit or reject a title/description draft (the human only). One accepted draft per platform: accepting one puts the platform's other accepted draft back to proposed. Nothing is published or approved.",
    example = r#"{"draft": "01J00000000000000000000009", "state": "accepted"}"#,
    example = r#"{"draft": "01J00000000000000000000009", "title": "My PC made this in 67 seconds"}"#
)]
impl Command for CopyDraftState {
    type Args = CopyDraftStateArgs;
    type Output = Done;
    fn run(a: CopyDraftStateArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if cx.caller.kind != gmclip_proto::CallerKind::Human {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        let d = cx.tx.get(a.draft)?.clone();
        let (tmax, dmax, _) = platform_rules(d.platform);
        if let Some(t) = &a.title
            && let Some(p) = draft_text_problem(t, tmax, false)
        {
            return Err(CmdError::invalid("/title", &p));
        }
        if let Some(t) = &a.description
            && let Some(p) = draft_text_problem(t, dmax, true)
        {
            return Err(CmdError::invalid("/description", &p));
        }
        let edited = a.title.as_ref().is_some_and(|t| t.trim() != d.title)
            || a.description
                .as_ref()
                .is_some_and(|t| t.trim() != d.description);
        let state = a.state.unwrap_or(if edited {
            ProposalState::Accepted
        } else {
            d.state
        });
        if state == ProposalState::Accepted {
            for o in cx.tx.children::<CopyDraft>(d.cut.entity()) {
                if o != a.draft
                    && cx.tx.peek().get(o).is_some_and(|x| {
                        x.platform == d.platform && x.state == ProposalState::Accepted
                    })
                {
                    cx.tx.update(o, |x| x.state = ProposalState::Proposed)?;
                }
            }
        }
        cx.tx.update(a.draft, |x| {
            if let Some(t) = a.title {
                x.title = t.trim().to_owned();
            }
            if let Some(t) = a.description {
                x.description = t.trim().to_owned();
            }
            x.edited |= edited;
            x.state = state;
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DraftView {
    pub id: String,
    pub platform: CopyPlatform,
    /// Untrusted when an agent wrote it and the human has not edited it.
    pub title: String,
    pub description: String,
    pub state: ProposalState,
    pub origin: EditOrigin,
    pub edited: bool,
    pub pass_ref: Option<String>,
    pub at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PlatformRules {
    pub platform: CopyPlatform,
    pub title_max: usize,
    pub description_max: usize,
    pub title_is: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Drafts {
    /// Newest first.
    pub drafts: Vec<DraftView>,
    pub platforms: Vec<PlatformRules>,
}

/// Every platform's rules.
pub fn all_rules() -> Vec<PlatformRules> {
    CopyPlatform::ALL
        .into_iter()
        .map(|p| {
            let (t, d, what) = platform_rules(p);
            PlatformRules {
                platform: p,
                title_max: t,
                description_max: d,
                title_is: what.into(),
            }
        })
        .collect()
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyDraftsListArgs {
    pub cut: Id<Cut>,
}

#[derive(Debug)]
pub struct CopyDraftsList;

#[command(
    name = "copy_drafts_list",
    scope = "read",
    undo = "read_only",
    since = "M2",
    doc = "List a cut's title/description drafts (newest first) with their state, and each platform's limits.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for CopyDraftsList {
    type Args = CopyDraftsListArgs;
    type Output = Drafts;
    fn run(a: CopyDraftsListArgs, cx: &mut Cx<'_>) -> Result<Drafts, CmdError> {
        cx.tx.get(a.cut)?;
        let ids = cx.tx.children::<CopyDraft>(a.cut.entity());
        let doc = cx.tx.peek();
        let mut drafts: Vec<DraftView> = ids
            .iter()
            .filter_map(|id| doc.get(*id))
            .map(|d| DraftView {
                id: d.id.0.to_string(),
                platform: d.platform,
                title: d.title.clone(),
                description: d.description.clone(),
                state: d.state,
                origin: d.origin,
                edited: d.edited,
                pass_ref: d.pass_ref.clone(),
                at_ms: d.at_ms,
            })
            .collect();
        drafts.sort_by(|x, y| y.at_ms.cmp(&x.at_ms).then(y.id.cmp(&x.id)));
        Ok(Drafts {
            drafts,
            platforms: all_rules(),
        })
    }
}
