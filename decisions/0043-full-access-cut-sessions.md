# 0043 — Full-access sessions on a cut: media, project-wide commands, the human's folders

Date: 2026-10-19 · Decided by: AJ ("fix the full access issues") · Status: adopted

## Context
In AJ's sessions on a cut, the agent ran with "Full access, no asking" but its gmclip tools were held to a cut-bound run's defaults:
- `analysis_get` faces and visual states, and `see`, came back `Unauthorized(read_media)`. The agent guessed the camera crop and could not look at its own result.
- `videos_add` (an app command) came back `Unauthorized(cut_binding)`. AJ had to add the animation file from Downloads by hand.
- The run reached only the agent read roots, not the home folder that a full-access session on the library reaches (decision 0039).

The same agent already had AJ's home folder and a shell, so these limits protected nothing and only broke the work.

## Decision
- **`LaunchRegister.full_access` and `grant`.** Only gmclip itself may set `full_access`: the registering connection must be gmclip's own executable, so a trusted harness launcher cannot. A run registered this way gets the following:
  - `read_media` with no Owner code. Private regions still need the code.
  - Project-wide commands on its own project only: the app's commands, `source_import`, `asr_run` and `stream_process`. Naming another project is refused.
  - Its session grant's folders (decision 0039), through `Proven.granted`.
- **Edits stay bound to the run's cut.** "Undo its edits" covers them as before.
- **The app.** A cut session with full or bypass access registers the grant (the human's home folder) and the run with `full_access`. A session with gmclip access is unchanged.
- **`see`'s description** no longer says it is unsupported. It is wired, and agents are told to use it to check a layout, crop or overlay before saying they are done.
- **Related fix.** A sweep's revision for a cut of many videos is a digest when the joined revisions pass 200 bytes. 25 sources passed `sweep_apply`'s 300-byte bound, and every "remove pauses" failed with "too long".

## Evidence
- `crates/gmclip-mcp/tests/launch_policy.rs::a_full_access_run_sees_media_and_works_project_wide`:
  - `see` is offered with no Owner call audited.
  - `source_import` is not refused for `cut_binding`.
  - A plain run still gets `cut_binding`.
- `crates/gmclip-mcp/src/analysis.rs::sweep_revision_tests`.
- The gmclip-mcp suite and `app/src-tauri/tests/sessions.rs` pass.
