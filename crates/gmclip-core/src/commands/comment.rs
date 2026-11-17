//! Comments, suggested edits and thumbnails on a cut (decision 0015; SPEC §5.14,
//! §5.14.1).
//!
//! - **Comments** are pinned to a source time ([`SrcAt`]: they survive trims, moves and
//!   reorders) with an optional box drawn on the frame, threaded one level deep (a reply
//!   names its thread's first comment), and resolved per thread. People write them in the
//!   app (`human`) or through a review link (a System caller whose identity is
//!   `review-link:<id>[/<login>]`); an agent may only **reply** (what it changed), never
//!   open a thread or resolve one. Text from anyone but `human` is untrusted (R-INJ-1).
//! - **Suggested edits** come only from a review link with the editor role (the server
//!   checks the role; here: a System review-link caller with the `admin` scope agents
//!   never hold). They are recorded as proposals and never applied until the human
//!   accepts one (`edit_proposal_state`), each accept one transaction the human's own ⌘Z
//!   undoes (the R-STORY-1 rule).
//! - **Thumbnails** are kept as versions beside the cut: a cover frame from the footage
//!   and a title. People and agents add versions (an agent's carries up to three
//!   text-hook options); only the human picks one or edits one.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_proto::{CallerKind, CmdError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::part::MIN_PART;
use super::topic::Decide;
use super::{Created, Done, bounded, created};
use crate::command::{Command, Cx, model_err};

/// The longest comment kept.
pub const MAX_COMMENT_CHARS: usize = 2_000;
/// Replies per thread, threads per cut (a link cannot flood a project).
pub const MAX_REPLIES: usize = 200;
pub const MAX_THREADS: usize = 2_000;
/// Thumbnail versions per cut.
pub const MAX_THUMBNAILS: usize = 200;

fn author_ok(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 200
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_:/.@+".contains(c))
}

/// Whether the caller is the review-link server writing as one of its links.
fn link_caller(cx: &Cx<'_>) -> bool {
    cx.caller.kind == CallerKind::System
        && cx
            .caller
            .identity
            .strip_prefix("review-link:")
            .is_some_and(|l| !l.is_empty())
        && author_ok(&cx.caller.identity)
}

/// Who a write is attributed to: `human`, the link or run identity, or the agent's.
fn author_of(cx: &Cx<'_>) -> Result<String, CmdError> {
    match cx.caller.kind {
        CallerKind::Human => Ok("human".into()),
        _ if author_ok(&cx.caller.identity) => Ok(cx.caller.identity.clone()),
        _ => Err(CmdError::Unauthorized {
            scope: "author".into(),
        }),
    }
}

fn check_box(b: &NormRect) -> Result<(), CmdError> {
    if b.is_valid() {
        Ok(())
    } else {
        Err(CmdError::invalid(
            "/frame_box",
            "fractions of the frame, inside it, w and h > 0",
        ))
    }
}

/// `at` names media this project has, and its part (if any) is a part of `cut`.
pub(crate) fn check_at(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    at: &SrcAt,
    path: &str,
) -> Result<(), CmdError> {
    match at.media {
        MediaRef::Source(s) => {
            cx.tx.get(s)?;
        }
        MediaRef::Group(g) => {
            cx.tx.get(g)?;
        }
    }
    if at.at_src.0 < 0 {
        return Err(CmdError::invalid(path, "a source time ≥ 0"));
    }
    if let Some(p) = at.part
        && cx.tx.get(p)?.cut != cut
    {
        return Err(CmdError::invalid(path, "a part of this cut"));
    }
    Ok(())
}

/// A part's length on the cut clock (flicks).
fn cut_len(p: &Part) -> i64 {
    gmclip_model::clock::part_duration(p).0.max(0)
}

/// Where `at` plays in `cut` now: the part playing that media time (the hinted part when
/// it still does, else the first in cut order) and the time on the cut clock (flicks from
/// the cut's start). `None` when the cut no longer plays it.
pub fn locate(doc: &gmclip_model::Doc, cut: Id<Cut>, at: &SrcAt) -> Option<(Id<Part>, i64)> {
    let mut t = 0i64;
    let mut first = None;
    for id in doc.children::<Part>(cut.entity()) {
        let Some(p) = doc.get(id) else { continue };
        if p.media == at.media && p.src_in <= at.at_src && at.at_src < p.src_out {
            let here = t + gmclip_model::clock::src_to_local(
                p,
                gmclip_model::Flicks(at.at_src.0 - p.src_in.0),
            )
            .0;
            if at.part == Some(id) {
                return Some((id, here));
            }
            first = first.or(Some((id, here)));
        }
        t += cut_len(p);
    }
    first
}

/// The source point the cut plays at `cut_t` (flicks from the cut's start): the inverse
/// of [`locate`]. `None` past the cut's end.
pub fn src_at(doc: &gmclip_model::Doc, cut: Id<Cut>, cut_t: i64) -> Option<SrcAt> {
    if cut_t < 0 {
        return None;
    }
    let mut t = 0i64;
    for id in doc.children::<Part>(cut.entity()) {
        let Some(p) = doc.get(id) else { continue };
        let len = cut_len(p);
        if cut_t < t + len {
            let off = gmclip_model::clock::local_to_src(p, gmclip_model::Flicks(cut_t - t)).0
                - p.src_in.0;
            return Some(SrcAt {
                media: p.media,
                at_src: gmclip_model::Flicks((p.src_in.0 + off).min(p.src_out.0 - 1)),
                part: Some(id),
            });
        }
        t += len;
    }
    None
}

fn check_text(t: &str, path: &str, max: usize) -> Result<(), CmdError> {
    if t.trim().is_empty() {
        return Err(CmdError::invalid(path, "some text"));
    }
    bounded(t, path, max)
}

// ---------------------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentAddArgs {
    pub cut: Id<Cut>,
    pub text: String,
    /// The thread this replies to (its first comment). Agents may only reply.
    #[serde(default)]
    pub reply_to: Option<Id<Comment>>,
    /// Where on the source the comment points (threads only).
    #[serde(default)]
    pub at: Option<SrcAt>,
    /// A box on the frame, as fractions of the output canvas (threads only).
    #[serde(default)]
    pub frame_box: Option<NormRect>,
    /// The export it was written on (its export id).
    #[serde(default)]
    pub export_ref: Option<String>,
}

#[derive(Debug)]
pub struct CommentAdd;

#[command(
    name = "comment_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a comment to a cut: a new thread pinned to a source time (and optionally a box on the frame), or a reply to a thread (reply_to). Agents may only reply, e.g. to say what they changed for a comment.",
    example = r#"{"cut": "01J00000000000000000000001", "reply_to": "01J00000000000000000000009", "text": "Trimmed part 2 to start on the result."}"#
)]
impl Command for CommentAdd {
    type Args = CommentAddArgs;
    type Output = Created;
    fn run(a: CommentAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let author = author_of(cx)?;
        let agent = cx.caller.kind == CallerKind::Agent;
        if cx.caller.kind == CallerKind::System
            && !link_caller(cx)
            && !cx.caller.identity.starts_with("launched:")
        {
            return Err(CmdError::Unauthorized {
                scope: "author".into(),
            });
        }
        check_text(&a.text, "/text", MAX_COMMENT_CHARS)?;
        if a.export_ref.as_deref().is_some_and(|e| {
            e.is_empty() || e.len() > 64 || !e.chars().all(|c| c.is_ascii_hexdigit())
        }) {
            return Err(CmdError::invalid("/export_ref", "an export id"));
        }
        cx.tx.get(a.cut)?;
        match a.reply_to {
            Some(root) => {
                let r = cx.tx.get(root)?.clone();
                if r.cut != a.cut {
                    return Err(CmdError::invalid("/reply_to", "a thread on this cut"));
                }
                if r.reply_to.is_some() {
                    return Err(CmdError::invalid("/reply_to", "the thread's first comment"));
                }
                if a.at.is_some() || a.frame_box.is_some() {
                    return Err(CmdError::invalid(
                        "/at",
                        "a reply sits on its thread's time and box",
                    ));
                }
                let n = cx
                    .tx
                    .children::<Comment>(a.cut.entity())
                    .into_iter()
                    .filter(|c| {
                        cx.tx
                            .peek()
                            .get(*c)
                            .is_some_and(|c| c.reply_to == Some(root))
                    })
                    .count();
                if n >= MAX_REPLIES {
                    return Err(CmdError::invalid("/reply_to", "this thread is full"));
                }
            }
            None => {
                if agent {
                    return Err(CmdError::Unauthorized {
                        scope: "agents reply to comments; people start threads".into(),
                    });
                }
                if let Some(at) = &a.at {
                    check_at(cx, a.cut, at, "/at")?;
                }
                if let Some(b) = &a.frame_box {
                    check_box(b)?;
                }
                let n = cx.tx.children::<Comment>(a.cut.entity()).len();
                if n >= MAX_THREADS {
                    return Err(CmdError::invalid("/cut", "this cut has too many comments"));
                }
            }
        }
        let id: Id<Comment> = cx.tx.mint();
        cx.tx.create(Comment {
            id,
            cut: a.cut,
            reply_to: a.reply_to,
            at: a.at,
            frame_box: a.frame_box,
            author,
            text: a.text,
            at_ms: cx.services.now_ms,
            resolved: false,
            export_ref: a.export_ref,
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentResolveArgs {
    /// A thread's first comment.
    pub comment: Id<Comment>,
    pub resolved: bool,
}

#[derive(Debug)]
pub struct CommentResolve;

#[command(
    name = "comment_resolve",
    scope = "edit",
    undo = "undoable",
    doc = "Resolve or reopen a comment thread. People only (in the app or through a review link); an agent replies instead.",
    example = r#"{"comment": "01J00000000000000000000009", "resolved": true}"#
)]
impl Command for CommentResolve {
    type Args = CommentResolveArgs;
    type Output = Done;
    fn run(a: CommentResolveArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        match cx.caller.kind {
            CallerKind::Human => {}
            CallerKind::System if link_caller(cx) => {}
            _ => {
                return Err(CmdError::Unauthorized {
                    scope: "people resolve threads".into(),
                });
            }
        }
        let c = cx.tx.get(a.comment)?;
        if c.reply_to.is_some() {
            return Err(CmdError::invalid("/comment", "a thread's first comment"));
        }
        cx.tx.update(a.comment, |c| c.resolved = a.resolved)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentDeleteArgs {
    pub comment: Id<Comment>,
}

#[derive(Debug)]
pub struct CommentDelete;

#[command(
    name = "comment_delete",
    scope = "edit",
    undo = "undoable",
    doc = "Delete a comment (a thread's first comment takes its replies with it). The human only.",
    example = r#"{"comment": "01J00000000000000000000009"}"#
)]
impl Command for CommentDelete {
    type Args = CommentDeleteArgs;
    type Output = Done;
    fn run(a: CommentDeleteArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if cx.caller.kind != CallerKind::Human {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        let c = cx.tx.get(a.comment)?.clone();
        if c.reply_to.is_none() {
            for r in cx.tx.children::<Comment>(c.cut.entity()) {
                if cx
                    .tx
                    .peek()
                    .get(r)
                    .is_some_and(|x| x.reply_to == Some(a.comment))
                {
                    cx.tx.delete(r)?;
                }
            }
            // suggestions on the thread stay, unattached
            for p in cx.tx.children::<EditProposal>(c.cut.entity()) {
                if cx
                    .tx
                    .peek()
                    .get(p)
                    .is_some_and(|x| x.comment == Some(a.comment))
                {
                    cx.tx.update(p, |x| x.comment = None)?;
                }
            }
        }
        cx.tx.delete(a.comment)?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// Suggested edits
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditSuggestArgs {
    pub cut: Id<Cut>,
    pub edit: SuggestedEdit,
    #[serde(default)]
    pub note: String,
    /// The comment thread it belongs to.
    #[serde(default)]
    pub comment: Option<Id<Comment>>,
}

#[derive(Debug)]
pub struct EditSuggest;

#[command(
    name = "edit_suggest",
    scope = "admin",
    undo = "undoable",
    doc = "Record an edit a review-link editor suggests (trim a part, strike words, move captions) as a proposal; nothing changes until the owner accepts it. Written only by the review-link server.",
    example = r#"{"cut": "01J00000000000000000000001", "edit": {"op": "trim", "part": "01J00000000000000000000003", "src_in": 0, "src_out": 2116800000}, "note": "start on the result"}"#
)]
impl Command for EditSuggest {
    type Args = EditSuggestArgs;
    type Output = Created;
    fn run(a: EditSuggestArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        if !link_caller(cx) {
            return Err(CmdError::Unauthorized {
                scope: "review_link".into(),
            });
        }
        bounded(&a.note, "/note", MAX_COMMENT_CHARS)?;
        cx.tx.get(a.cut)?;
        if let Some(c) = a.comment
            && cx.tx.get(c)?.cut != a.cut
        {
            return Err(CmdError::invalid("/comment", "a comment on this cut"));
        }
        check_edit(cx, a.cut, &a.edit)?;
        if cx.tx.children::<EditProposal>(a.cut.entity()).len() >= MAX_THREADS {
            return Err(CmdError::invalid(
                "/cut",
                "this cut has too many suggestions",
            ));
        }
        let id: Id<EditProposal> = cx.tx.mint();
        cx.tx.create(EditProposal {
            id,
            cut: a.cut,
            edit: a.edit,
            author: cx.caller.identity.clone(),
            note: a.note,
            at_ms: cx.services.now_ms,
            state: ProposalState::Proposed,
            comment: a.comment,
        })?;
        Ok(created(id))
    }
}

/// A suggestion is well formed against the cut as it is now.
fn check_edit(cx: &mut Cx<'_>, cut: Id<Cut>, e: &SuggestedEdit) -> Result<(), CmdError> {
    match e {
        SuggestedEdit::Trim {
            part,
            src_in,
            src_out,
        } => {
            if cx.tx.get(*part)?.cut != cut {
                return Err(CmdError::invalid("/edit/part", "a part of this cut"));
            }
            if src_in.0 < 0 || src_out.0 - src_in.0 < MIN_PART.0 {
                return Err(CmdError::invalid(
                    "/edit/src_out",
                    "in before out, at least 0.12 s apart",
                ));
            }
        }
        SuggestedEdit::Strike {
            media,
            start,
            end,
            words,
        } => {
            bounded(words, "/edit/words", 500)?;
            if start.0 < 0 || end <= start {
                return Err(CmdError::invalid("/edit/end", "start before end"));
            }
            let plays = cx.tx.children::<Part>(cut.entity()).into_iter().any(|p| {
                cx.tx
                    .peek()
                    .get(p)
                    .is_some_and(|p| p.media == *media && p.src_in < *end && *start < p.src_out)
            });
            if !plays {
                return Err(CmdError::invalid(
                    "/edit/start",
                    "no part of this cut plays that stretch",
                ));
            }
        }
        SuggestedEdit::CaptionMove {
            framing,
            part,
            place,
        } => {
            super::look::check_place(place, "/edit/place")?;
            if cx.tx.get(*framing)?.cut != cut {
                return Err(CmdError::invalid("/edit/framing", "a framing of this cut"));
            }
            if let Some(p) = part
                && cx.tx.get(*p)?.cut != cut
            {
                return Err(CmdError::invalid("/edit/part", "a part of this cut"));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditProposalStateArgs {
    /// One or more suggestions (≤ 200), decided in this order.
    pub proposals: Vec<Id<EditProposal>>,
    pub state: Decide,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProposalsDecided {
    pub decided: usize,
    pub parts_changed: usize,
}

#[derive(Debug)]
pub struct EditProposalState;

#[command(
    name = "edit_proposal_state",
    scope = "edit",
    undo = "undoable",
    doc = "Accept suggested edits (apply them, one transaction, undone by one ⌘Z) or reject them. The human only.",
    example = r#"{"proposals": ["01J00000000000000000000009"], "state": "accepted"}"#
)]
impl Command for EditProposalState {
    type Args = EditProposalStateArgs;
    type Output = ProposalsDecided;
    fn run(a: EditProposalStateArgs, cx: &mut Cx<'_>) -> Result<ProposalsDecided, CmdError> {
        if cx.caller.kind != CallerKind::Human {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        if a.proposals.is_empty() || a.proposals.len() > 200 {
            return Err(CmdError::invalid("/proposals", "1 to 200 suggestions"));
        }
        let mut out = ProposalsDecided {
            decided: 0,
            parts_changed: 0,
        };
        for id in &a.proposals {
            let p = cx.tx.get(*id)?.clone();
            if p.state != ProposalState::Proposed {
                continue;
            }
            if a.state == Decide::Accepted {
                out.parts_changed += apply(cx, p.cut, &p.edit)?;
            }
            cx.tx.update(*id, |x| {
                x.state = match a.state {
                    Decide::Accepted => ProposalState::Accepted,
                    Decide::Rejected => ProposalState::Rejected,
                };
            })?;
            out.decided += 1;
        }
        Ok(out)
    }
}

/// Applies one suggestion to the cut as it is now. Returns the parts it changed.
fn apply(cx: &mut Cx<'_>, cut: Id<Cut>, e: &SuggestedEdit) -> Result<usize, CmdError> {
    match e {
        SuggestedEdit::Trim {
            part,
            src_in,
            src_out,
        } => {
            let p = cx.tx.peek().get(*part).cloned().ok_or_else(|| {
                CmdError::invalid("/proposals", "the part this suggestion trims is gone")
            })?;
            if p.cut != cut {
                return Err(CmdError::invalid("/proposals", "a part of another cut"));
            }
            super::record_part_children(cx, *part);
            gmclip_model::edit::trim_part(cx.tx.staging(), *part, *src_in, *src_out)
                .map_err(model_err)?;
            Ok(1)
        }
        SuggestedEdit::Strike {
            media, start, end, ..
        } => {
            let (changed, _, _) = super::words::remove_span(cx, cut, *start, *end, Some(*media))?;
            if changed == 0 {
                return Err(CmdError::invalid(
                    "/proposals",
                    "no part plays those words any more",
                ));
            }
            Ok(changed)
        }
        SuggestedEdit::CaptionMove {
            framing,
            part,
            place,
        } => {
            match part {
                Some(p) => {
                    if cx.tx.peek().get(*p).is_none() {
                        return Err(CmdError::invalid(
                            "/proposals",
                            "the part this suggestion moves is gone",
                        ));
                    }
                    let (pl, f) = (*place, *framing);
                    cx.tx.update(*p, |x| {
                        x.caption_place_override.insert(f, pl);
                    })?;
                }
                None => {
                    let pl = *place;
                    cx.tx.update(*framing, |f| f.caption_place = Some(pl))?;
                }
            }
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------------------
// Thumbnails
// ---------------------------------------------------------------------------------------

fn hex_colour(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn check_style(s: &ThumbStyle) -> Result<(), CmdError> {
    bounded(&s.font, "/style/font", 128)?;
    if !(s.size.is_finite() && (0.02..=0.3).contains(&s.size)) {
        return Err(CmdError::invalid(
            "/style/size",
            "0.02 to 0.3 of the frame height",
        ));
    }
    for (c, p) in [
        (&s.fill, "/style/fill"),
        (&s.outline, "/style/outline"),
        (&s.accent, "/style/accent"),
    ] {
        if !hex_colour(c) {
            return Err(CmdError::invalid(p, "#RRGGBB"));
        }
    }
    Ok(())
}

fn check_layout(l: &ThumbLayout) -> Result<(), CmdError> {
    if l.title_box.is_valid() {
        Ok(())
    } else {
        Err(CmdError::invalid(
            "/layout/title_box",
            "fractions of the frame, inside it",
        ))
    }
}

/// The default look: white, black outline, yellow accent, upper case, a box in the upper
/// third.
pub fn default_thumb_style() -> ThumbStyle {
    ThumbStyle {
        font: String::new(),
        size: 0.075,
        fill: "#FFFFFF".into(),
        outline: "#000000".into(),
        accent: "#FFD400".into(),
        uppercase: true,
    }
}

pub fn default_thumb_layout() -> ThumbLayout {
    ThumbLayout {
        title_box: NormRect {
            x: 0.06,
            y: 0.12,
            w: 0.88,
            h: 0.26,
        },
        align: TextAlign::Centre,
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThumbnailAddArgs {
    pub cut: Id<Cut>,
    /// The cover frame: the source time a part plays.
    pub cover: SrcAt,
    #[serde(default)]
    pub title: String,
    /// Up to three text-hook options (an agent's pass); the title defaults to the first.
    #[serde(default)]
    pub hook_options: Vec<String>,
    #[serde(default)]
    pub style: Option<ThumbStyle>,
    #[serde(default)]
    pub layout: Option<ThumbLayout>,
    #[serde(default)]
    pub notes: String,
    /// Make it the cut's thumbnail (the human only).
    #[serde(default)]
    pub pick: bool,
}

#[derive(Debug)]
pub struct ThumbnailAdd;

#[command(
    name = "thumbnail_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a thumbnail version to a cut: a cover frame (a source time a part plays), a title, and optionally up to three text-hook options, a style and a layout (the title's box). Every version is kept; only the human picks one.",
    example = r#"{"cut": "01J00000000000000000000001", "cover": {"media": {"source": "01J00000000000000000000007"}, "at_src": 8467200000}, "hook_options": ["My PC made this in 67 seconds", "No cloud, 67 seconds", "A 5080 vs a video model"], "layout": {"title_box": {"x": 0.06, "y": 0.1, "w": 0.88, "h": 0.25}, "align": "centre"}}"#
)]
impl Command for ThumbnailAdd {
    type Args = ThumbnailAddArgs;
    type Output = Created;
    fn run(a: ThumbnailAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let author = author_of(cx)?;
        let human = cx.caller.kind == CallerKind::Human;
        if cx.caller.kind == CallerKind::System {
            return Err(CmdError::Unauthorized {
                scope: "author".into(),
            });
        }
        if a.pick && !human {
            return Err(CmdError::Unauthorized {
                scope: "the human picks the thumbnail".into(),
            });
        }
        cx.tx.get(a.cut)?;
        check_at(cx, a.cut, &a.cover, "/cover")?;
        if a.hook_options.len() > 3 {
            return Err(CmdError::invalid("/hook_options", "at most three"));
        }
        for h in &a.hook_options {
            check_text(h, "/hook_options", 120)?;
        }
        bounded(&a.title, "/title", 200)?;
        bounded(&a.notes, "/notes", 2_000)?;
        let title = if a.title.trim().is_empty() {
            a.hook_options.first().cloned().unwrap_or_default()
        } else {
            a.title
        };
        let style = a.style.unwrap_or_else(default_thumb_style);
        check_style(&style)?;
        let layout = a.layout.unwrap_or_else(default_thumb_layout);
        check_layout(&layout)?;
        let existing = cx.tx.children::<Thumbnail>(a.cut.entity());
        if existing.len() >= MAX_THUMBNAILS {
            return Err(CmdError::invalid(
                "/cut",
                "this cut has too many thumbnails",
            ));
        }
        if a.pick {
            for t in existing {
                if cx.tx.peek().get(t).is_some_and(|t| t.picked) {
                    cx.tx.update(t, |t| t.picked = false)?;
                }
            }
        }
        let id: Id<Thumbnail> = cx.tx.mint();
        cx.tx.create(Thumbnail {
            id,
            cut: a.cut,
            cover: a.cover,
            title,
            hook_options: a.hook_options,
            style,
            layout,
            origin: if human {
                EditOrigin::Human
            } else {
                EditOrigin::Agent
            },
            author,
            at_ms: cx.services.now_ms,
            picked: a.pick,
            notes: a.notes,
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThumbnailSetArgs {
    pub thumbnail: Id<Thumbnail>,
    /// Make it the cut's thumbnail (the others are unpicked).
    #[serde(default)]
    pub pick: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub style: Option<ThumbStyle>,
    #[serde(default)]
    pub layout: Option<ThumbLayout>,
    #[serde(default)]
    pub cover: Option<SrcAt>,
}

#[derive(Debug)]
pub struct ThumbnailSet;

#[command(
    name = "thumbnail_set",
    scope = "edit",
    undo = "undoable",
    doc = "Pick a thumbnail version, or change its title (e.g. to one of its text-hook options), style, layout or cover frame. The human only.",
    example = r#"{"thumbnail": "01J00000000000000000000009", "pick": true, "title": "No cloud, 67 seconds"}"#
)]
impl Command for ThumbnailSet {
    type Args = ThumbnailSetArgs;
    type Output = Done;
    fn run(a: ThumbnailSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if cx.caller.kind != CallerKind::Human {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        let t = cx.tx.get(a.thumbnail)?.clone();
        if let Some(s) = &a.style {
            check_style(s)?;
        }
        if let Some(l) = &a.layout {
            check_layout(l)?;
        }
        if let Some(c) = &a.cover {
            check_at(cx, t.cut, c, "/cover")?;
        }
        if let Some(x) = &a.title {
            bounded(x, "/title", 200)?;
        }
        if a.pick {
            for o in cx.tx.children::<Thumbnail>(t.cut.entity()) {
                if o != a.thumbnail && cx.tx.peek().get(o).is_some_and(|o| o.picked) {
                    cx.tx.update(o, |o| o.picked = false)?;
                }
            }
        }
        cx.tx.update(a.thumbnail, |t| {
            if a.pick {
                t.picked = true;
            }
            if let Some(x) = a.title {
                t.title = x;
            }
            if let Some(s) = a.style {
                t.style = s;
            }
            if let Some(l) = a.layout {
                t.layout = l;
            }
            if let Some(c) = a.cover {
                t.cover = c;
            }
        })?;
        Ok(Done {})
    }
}
