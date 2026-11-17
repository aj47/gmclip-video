# 0049 — Clean joins on talking-head cuts: no slivers, no repeated first frames, voice-tight edges

Date: 2026-10-25 · Decided by: AJ ("save the way we did this kind of cutting into both gmclip and our skills, so we never have that previous issue again") · Status: adopted (strikes, sweeps, cut points and the lint built; `verify{cuts}` and the cut-away proposal backlog, no gate yet)

## Context
The WorkOS demo (project "WorkOS Team Memory Demo") was cut from a 12-minute talking-head recording with retakes: 137 cuts in 3:05. AJ said the jump cuts felt jumpy, and that something in the rendering was wrong. A frame-difference test found a fault at 119 of the 137 joins:
- **Slivers.** 1–2 frames from another moment of the recording sat between takes, filling a short gap. That reads as jump, flash, jump.
- **Repeated first frames.** Each piece was seeked between frames and passed through an `fps` filter, which repeated its first frame: a freeze at every cut.
- **Zoom pulse.** An alternating 1.0×/1.08× punch-in on every take made 62 of 63 take changes pulse.

gmclip plays parts straight from the source and refuses parts under 0.12 s, so it avoided the first two in that edit. Its pause sweep, lint and verify should keep it that way for any cut an agent builds.

## Decision
- **Fillers continue the previous part.** A part with no words that is shorter than 0.6 s and continues neither neighbour in its source is a sliver of another moment. New lint `part_filler_elsewhere` (warning) names it and offers the fix: trim the previous part out by its length instead.
- **Edges on the voice.** `sweep_pauses` and the arbiter cut point keep a part's audible start minus 40 ms and end plus 120 ms.
  - Breaths and room tone outside that go.
  - A quiet first word ("I", "So", "If") keeps its softer onset when that onset is within 250 ms of the voice.
  - Inside a part, pauses are cut only at 320 ms or more, leaving 100 ms each side; shorter cuts land in stop closures.
- **Cut away instead of jumping.** In a stretch with a cut more often than every 3 s that has a screen or recording layer, `zoom_auto` and the layout tools should propose hiding the cam (`hide_roles_add`) over an alternating punch-in. A punch-in is proposed only where the head jump at a cut is large.
- **Verify the picture at joins.** `verify{cuts}` [backlog] runs beside `verify{joins}` (R-EXP-6). At each discontinuous join, it compares 72×90 luma thumbnails of the frames around the join:
  - **Fail on a flash:** three distinct poses within 3 frames, with no part continuing.
  - **Fail on a repeated frame** at the join that the source does not have.
  - The source's own cadence (a 25 fps camera in a 30 fps cut, decision 0047) is measured and not counted as a fault.

## Evidence
- `~/.agents/skills/video-editing/scripts/check_cut_flashes.py`, the reference check. It measured 0 slivers on the fixed edit and 60 on the same edit list built the old way. Before/after frames: `~/Documents/youtube/workos-team-memory-demo/renders/compare/`.
- Reference build: `~/Documents/youtube/workos-team-memory-demo/rig/th_build.py` (functions `tighten`, `place_footage` and the fill loop).
- Skill guide: `~/.agents/skills/video-editing/guides/talking-head-cuts/guide.md`.
- Tests, each failing without the change: `crates/gmclip-core/tests/script_cut.rs` (`a_strike_cuts_on_the_voice`, `a_restore_keeps_a_soft_onset`, `a_strike_reports_the_fragment_it_drops`), `crates/gmclip-cli/src/analysis.rs` (`a_sweep_puts_a_parts_edges_on_its_voice`, `a_cut_point_keeps_the_voice_either_side`), `crates/gmclip-analysis/src/edges.rs` (`parts_keep_the_voice_and_a_soft_onset_straight_before_it`), `crates/gmclip-core/tests/lint.rs` (`part_filler_elsewhere`). `tools/pauses_ref.py` (the lab sweep's Python reference) follows the rule and gives the same cuts on the sweep test's signal.

## Built (2026-10-26)
- **Strikes cut on the voice.** `words_strike` and `words_restore` cut from the first word's `kept_start` to the last word's `kept_end` (`gmclip_analysis::edges`: audible start −40 ms, a run above the room within 250 ms before it kept; audible end +120 ms), on the owner's measured words (`AnalysisView::voice_span`, decision 0048), never into a neighbour's voice. The `WordRef`'s ASR edges stay its fingerprint: an end is moved only when its time is the ASR's, so an edge the human tuned, or an unmeasured or room word, cuts as before. The app sends the fingerprints and core snaps and widens (the app's own snap and widening are gone).
- **Dropped fragments are reported.** `Struck.fragments_dropped` counts the pieces under 0.12 s dropped with a strike; the transcript pane says so.
- **Sweeps.** `sweep_pauses` cuts a part's head to `kept_start` of its first heard word and its tail from `kept_end` of its last (breaths and room go, short of a non-word sound as loud as the voice; an edge inside a word is left alone), and inside a part keeps `keep_s` **on each side** of a pause (it kept half on each side before). Defaults 0.32 / 0.10 s (were 0.4 / 0.15) in the MCP sweep, the app and the Tighten tab. `measure{edge}`'s energy-min and arbiter cut points move into `[kept_end(w1), kept_start(w2)]` when the gap holds it, and say so in `method`.
- **Lint `part_filler_elsewhere`** (warning), with the fix as one `cut_edit`: delete the filler, trim the previous part out by its length. `dead_air` is unchanged: this decision sets where the sweep cuts, not what lint calls dead air (1 s).
- Backlog: `verify{cuts}`; `zoom_auto` proposing a cut-away; the soft-ending rule of the skill guide (a softer tail within 0.35 s).

## Sections changed
- `spec/SPEC.md` §5.4 cut strip: strike and restore cut where the words are heard (ASR edges where unmeasured or tuned); dropped fragments are reported.
- §5.4 sweeps, Pauses: default 0.4 → 0.32 s; keep 0.15 s except edges → 0.10 s on each side inside a part, the part's edges on its voice.
- §9 "Word edges": strikes, restores, sweeps and cut points keep audible start − 40 ms and end + 120 ms where the gap allows.
- Lint: `part_filler_elsewhere` is documented with decision 0040's script rules in `crates/gmclip-core/src/lint.rs` (§5.13 does not list those).
- No gate takes these: they are agent-surface behaviour, not gate MUSTs. `verify{cuts}` stays backlog until a gate takes it (README rule).
