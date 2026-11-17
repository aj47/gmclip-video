# 0015 — Comments, suggested edits, approve from the phone, golden cuts, thumbnails

Date: 2026-10-15 · Decided by: agent (lane V, AJ's "Work on" picks for review/collaboration, thumbnails and agent-workflow stories) · Status: adopted, AJ may veto

## Context
AJ picked: time-pinned comments with a drawn box; an outside editor who comments and suggests edits; Approve on the phone → Clippy's calendar; comments → agent tasks; golden cuts; thumbnails with three text-hook options. SPEC §5.14.1 / R-REV-4 limit a review link to notes and keep/change/drop, the model has no comment, suggestion or thumbnail kind, and "golden exports" is a profile word with nothing behind it.

## Decision
- **Model (epoch 2, additive):** `Comment {cut, reply_to?, at?: SrcAt, frame_box?, author, text, at_ms, resolved, export_ref?}` (pinned to a source time on the media a part plays, so it follows the picture through trims and reorders), `EditProposal {cut, edit: trim|strike|caption_move, author, note, state, comment?}`, `Thumbnail {cut, cover: SrcAt, title, hook_options ≤ 3, style, layout, origin, author, picked, notes}`.
- **Commands:** `comment_add` (people open threads; agents only reply; a review link as `review-link:<id>[/<login>]`), `comment_resolve` (people and links), `comment_delete` (human), `edit_suggest` (`admin`: the review-link server only), `edit_proposal_state` (human only; an accept applies the edit in the same transaction, so one ⌘Z undoes it — the R-STORY-1 rule), `thumbnail_add` (people and agents; only the human picks), `thumbnail_set` (human). Agents are refused `edit_suggest`, `edit_proposal_state`, `comment_resolve`, `comment_delete`, `thumbnail_set` on every surface.
- **R-REV-4 widened:** a link may also write comments, replies and resolves; a link with **role `editor`** (made through the Owner sheet) writes comments and suggested edits only — no verdicts, no fix with agent, no approve. Every new body is parsed into bounded, typed values first (`review::body`, fuzzed as FZ13).
- **Approve (new):** `POST /api/approve {export}` hands the export to Clippy (the minimal Send to Clippy, no sheet: the tap is the human click) only when R-REV-4's (a) tailnet and (b) the Owner's own login hold and (c) the Owner's new standing **remote-approve allowance** (`remote_send_set`, Owner sheet, off by default, ≤ 20 a day, its own ledger that fails closed) has a send left; otherwise a Home "needs you" item (Send · Dismiss). It only registers the final; approving and submitting posts stay a click in Clippy, and a blocked export is never sent.
- **Comments → agent tasks:** passes `fix_comments@1` and `thumbnail@1` (versioned assets). Each comment is a task inside `<untrusted>` with its author named; a fix run edits the cut and replies in each thread as `launched:<run>`; the run is one implicit run with a restore point (run undo). A link's "fix with agent" note becomes a thread and its run (or queued ask) works on it.
- **Golden cuts** are profile content kept in app data per active profile (`golden-cuts.json`; the profile folder is never written), each with a spec measured from the cut's plan; every launched brief gets the goldens' specs appended.
- **Thumbnails:** rendered by the export's own compositor and text renderer from the cut's plan at the cover frame (captions removed, the title as `note` text), PNG beside the project. None of the four launched clients may make images inside a run (their image tools are off under §10.7's flag sets), so the agent path is the layout spec gmclip renders.

## Evidence
- `crates/gmclip-core/tests/comments.rs` (5), `crates/gmclip-mcp/tests/review_collab.rs` (roles, CSRF, canonical text, approve under each failing condition), `crates/gmclip-mcp/tests/comment_fix_pass.rs` (scripted agent, no model), `app/src-tauri/tests/collab.rs`, FZ13 55 M runs clean.

## Sections changed
- `spec/SPEC.md` §5.14.1: link writes and roles, Approve; §5.14.2: Approve is a minimal-send path; §10.5: golden cuts stored per profile in app data.
- `spec/security-requirements.md` R-REV-4: comments, editor role, approve conditions.
