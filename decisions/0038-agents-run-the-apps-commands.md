# 0038 — Agents run the app's own commands

Date: 2026-10-18 · Decided by: AJ ("everything we can do with gui we should be able to do with agent", after an agent said it could not create a project or import videos) · Status: adopted

## Context
Agents reached the command registry and the holder's commands, but not the app's own: creating a project, opening or adding videos, exports, layouts, captions, overlays, sound and the rest of what the window does through its IPC (`APP_COMMANDS`). An agent asked to make a project from a folder of clips had to stop and ask the human to click.

## Decision
- **The holder takes an `AppHost`** (like the library and the importer): the app lists the commands agents may run with a doc each (`app/src-tauri/src/agentapp.rs`, 186 of them) and runs them through the same IPC dispatch the window uses. `commands_search` and `command_describe` show them (scope `app`); `command_run` / `command_batch` run them. A name the registry or the holder already has is theirs (the list may not shadow one; a test checks).
- **Left out on purpose**: Owner actions and their sheets (R-OWN-1), the human's decisions (verdicts, golden marks, the thumbnail and comment choices, review links, feedback, Clippy), agent and session management, settings and first run, backup restore, and the window's own plumbing (viewer, gestures, native panels, menus, reveal and drag). A test checks none of these is on the list.
- **Checks**: not for a run bound to one cut (`cut_binding`); the connection needs `edit`; every path argument (`path`, `paths`, `hires`, `voice`, `dest`, `home`, `file`, `dir`, `folder`, at any depth) must be an existing file or folder inside the agent read roots, or a new file in a folder inside them (R-FS-1), and goes over canonical. `project` defaults to the open one.
- **Replies are source-derived**: every string is canonicalised and the result sits under `untrusted` (R-INJ-1).

## Consequences
An agent can make a project and fill it from the read roots by itself. The read roots are still the Owner's: with none set, only open projects' homes are readable (the agent panel's "Folders agents may read" adds one through the sheet).

## Evidence
`crates/gmclip-mcp/tests/app_commands.rs` (found and described, run with paths checked and canonical, outside and `..` refused, the open project passed, untrusted reply, none for a launched run, none in a bare holder). `app/src-tauri/tests/agent_app_commands.rs` (the list against `APP_COMMANDS`, the registry, the holder, Owner, deny and human-only lists). A live run on a throwaway home through `gmclip-app mcp`: `project_new` → `videos_add` from a read root (transcription starts) → `project_overview`; a file in Downloads refused (`file_roots`).

## Sections changed
- `spec/SPEC.md` §11.2: a line on the app's commands at layer 3.
