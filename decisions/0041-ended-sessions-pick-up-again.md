# 0041 — Ended agent sessions pick up again (`session/resume`)

Date: 2026-10-19 · Decided by: agent (AJ: "continue agent sessions that have ended (if possible)") · Status: adopted, AJ may veto

## Context
Decision 0035 resumed an ended session only through ACP `session/load`, behind `agentCapabilities.loadSession`. Claude Code's adapter (`claude-code-acp` 0.12.6) does not offer `loadSession`. It offers `sessionCapabilities.resume` (`session/resume`, which reopens the Claude session without replaying it). So every kept session said `resumable: false`, "Pick up again" never showed, and an ended session's composer was dead.

## Decision
- The ACP client reads `sessionCapabilities.resume` (`Initialized.resume_session`) and sends `session/resume` (`Connection::resume_session`).
- `session_resume {session, message?}` asks the agent's `initialize` rather than the saved `resumable`, because sessions kept before this decision saved `false`. It loads when the agent offers `loadSession`: the transcript is cleared there, at the point the replay is known, not before. Otherwise it resumes when the agent offers `resume`, and the kept transcript goes on. When the agent offers neither, the session fails with "cannot pick a session up again", and the transcript is kept. `message`, if given, is the reopened session's first turn.
- `resumable` = `loadSession || resume`.
- UI: an ended session that once opened (it has a folder) shows "Pick up again" in the Agents view and the dock. Its composer stays enabled: sending picks the session up with that message.
- Unchanged from 0035: a reopened cut session gets no new cut run. Its tools connect as the agent's local identity, so "Undo its edits" covers only the edits made before it ended.

## Evidence
- `app/src-tauri/tests/sessions.rs`:
  - `an_ended_session_is_resumed_with_its_transcript_and_the_next_message`
  - `an_agent_that_cannot_resume_says_so`
- `app/ui/tests/e2e/agent-sessions.spec.ts`: a message in an ended session resumes it. The fake agent offers `resume`.

## Sections changed
- None in the spec. This extends decision 0035's resume.
