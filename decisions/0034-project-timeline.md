# 0034 — The project timeline: an assembly of windows onto each video's stream edit

Date: 2026-10-17 · Decided by: AJ (direction and "hide" trims, 2026-10-17) · Status: superseded by 0036 (one main cut per project; the assembly, windows and their commands removed)

## Context
AJ imported three videos into one project and saw one on the timeline: 0018 gives each video its own stream edit and the app opened the first. AJ: "by default the project should be all of the clips visible in a mother project editor view. then we can go into the individual video cuts if we want to. though top level is mostly arrangement, trimming, audio, captions etc. individual clips are more for cropping and finer controls." Trims at the top level only hide footage ("hide sounds safer/easier as long as it doesnt mess performance/playback"); one caption style for the timeline by default, a clip may override it.

## Decision
- **Model (epoch 4, additive):** `Cut.kind: assembly`, one per project, the project timeline. Its children are `Window {of, win_in, win_out, gain_db}` in order: each a window onto a stream edit, edges in **source time** of that stream edit's video (a stream edit plays one video, 0018), so they stay put when words inside it are struck or restored.
- **Hide, not cut:** trimming a window never edits the stream edit; footage outside the window is not planned, previewed or exported, and trimming back brings it back. "Break at playhead" (`window_break`) makes two windows onto one video; nothing is split underneath. Splits, strikes and crops belong to the stream edit and show through.
- **Planning:** a window plans as its stream edit does, on a copy of the doc where that stream edit keeps only the window's stretch (never committed), in its framing of the timeline's canvas and frame rate; the window plans are joined end to end into one flat RenderPlan (ids prefixed `wN.` where they would repeat, each file listed once, output times moved). Playback and export see an ordinary plan: no per-frame indirection. A stream edit with no framing of the timeline's shape gets one when it joins (the video fitted inside).
- **Commands:** `assembly_ensure`, `window_add`, `window_trim`, `window_move`, `window_remove`, `window_break` (agents may call them, like the other edit commands); `planner::assembly::assembly_to_src` maps the timeline's playhead to (window, source time) for the app's trim keys.
- **Not yet (plan/m2-backlog.md §2.22):** the app opening on the timeline, its strip and double-click into a clip, trim keys at both levels, timeline audio and captions (the first window's background and loudness stand in; stream edits keep captions off), "make trim permanent".

## Evidence
`crates/gmclip-core/tests/assembly.rs` (every video in order on one plan; trim hides and restores, the stream edit untouched; a strike inside shows through and the edges stay put; break, move, remove, undo, playhead mapping; empty windows plan nothing; removing a video takes its clips off, undo brings them back). `gmclip-model`, `gmclip-core`, `gmclip-plan`, `gmclip-store`, `gmclip-mcp` suites pass; the app compiles.

## Sections changed
- `spec/SPEC.md` §5.17: one stream edit per video, plus one assembly cut per project holding a window onto each
- `plan/m2-backlog.md` §2.22 (lane PT)
