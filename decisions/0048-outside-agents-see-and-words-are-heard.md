# 0048 — Outside agents: media by default, the app's socket under a sandbox, and words judged where they are heard

Date: 2026-10-21 · Decided by: AJ (choices to "can we fix issues with mcp", after editing "bb mobile demo" from a terminal) · Status: adopted

## Context
On 2026-10-21, Claude Code edited "bb mobile demo" through MCP from a terminal and hit four problems:
- **Socket.** Its sandbox set its own `TMPDIR`, so it reached a second holder of its own, which found the project `Leased`. One thread stalled on this.
- **No media.** `see`, `listen` and `analysis_get` faces came back `Unauthorized(read_media)`. The agent framed the camera from ffmpeg frames and OpenCV on face.mov instead, which it can read anyway as the same user.
- **Dead air left in.** `sweep_pauses` left 2.8 s of silence inside a part. The ASR timed "you." from 111.80 to 114.68 s, while the voice says it before 111.80. Inside that span the loudest frame was the room (about −73 dB), so no 18 dB drop followed, `audible_end` fell back to the ASR's end, and the whole silence was protected as the word.
- **False lint warnings.** Lint's `cuts_word` and the words view's "an edge cuts a word" judged edges on ASR times ("better" 67.16–67.72 s, heard only to 67.33). Every flagged edge was in measured silence. Acting on them, the agent pulled an "uh" back into the cut.

## Decision
- **R-AGT outside scopes.** A connection gmclip did not launch gets `OUTSIDE_SCOPES` = the defaults + `read_media`, with no Owner code, for the reason decision 0043 gives for full-access sessions. Private regions stay masked: unmasking still needs a launched run with the Owner code. Launched runs are unchanged.
- **R-ARCH-5 socket.** On macOS, a client whose `TMPDIR` has no holder also tries `<getconf DARWIN_USER_TEMP_DIR>/gmclip-<uid>/s`, before auto-starting a holder. It takes that socket only if the server there passes `verify_server` (the expected gmclip executable). The socket is still a pure function of the user, never read from a file. Tests keep isolating by `TMPDIR`; they run another executable, so a dev machine's app is never theirs.
- **Sweeps (§5.3).** In `pause_cuts`, a word whose peak in its ASR span is under the quiet grid's threshold protects none of the pause.
- **Word edges (§9).** `AnalysisView` measures each transcript's words on the owner's stored `energy_10ms`, with `silences_40ms`'s `threshold_db` (`audible(owner, revision)`). The result is `[start, end]` at the 18 dB drop, or `[start, start]` when the span is the room. It is used only when the owner has exactly one of each blob. It is computed on load and when either blob is stored, so existing projects get it without re-running ASR. No stored format changes.
- **Script.** `script` (lint, the words view) judges `cuts_word` on the measured span and ends `spoken_end` at the measured end. A word starts at the earlier of its ASR and measured starts, because soft onsets measure late. Unmeasured words keep the ASR rules.

## Evidence
- `crates/gmclip-mcp/tests/agent_surface.rs::an_outside_agent_reads_media_without_a_code` (fails on the old scopes).
- `crates/gmclip-mcp/src/paths.rs::a_sandboxed_tmpdir_falls_back_to_the_users_socket`.
- `crates/gmclip-cli/src/analysis.rs::a_word_timed_into_the_silence_after_it_keeps_none_of_it` (fails without the threshold check).
- `crates/gmclip-core/tests/script_cut.rs::words_are_judged_where_the_voice_is_heard`.
- On "bb mobile demo"'s stored blobs, 602 of 602 words were measured (17 as the room). "better" ends at 67.33 s, "now." at 118.35 s and "you." at 111.80 s, matching RMS read off face.mov.

## Sections changed
- `spec/SPEC.md` §11.4: an outside agent's default scopes include `read_media`.
- §R-ARCH-5: the macOS client fallback to the per-user temp dir's socket.
- §9 "Word edges": measured word extents serve script and lint, not only sweeps.
