# 0016 — RenderPlan `/1`: eased visual splits (`layers[].ease`), and follow the cursor

Date: 2026-10-15 · Decided by: agent (lane-u, AJ's brief: "cursor auto-zoom — only needed some times, but a nice feature to have") · Status: adopted, AJ may veto (Needs AJ 1.18)

## Context
The model has had eased visual splits since M1 (SPEC §4.3.1: an `ease` interpolates crop, place and opacity from the state at its start over `dur_ms`), but RenderPlan v0 has no keyframes, so the planner refused them. A cursor follow is a smooth crop path: cut visual splits every few frames would be jumpy and would multiply plan parts; an eased split per move is the model's own way to say it.

## Decision
- **`gmclip.renderplan/1` adds `parts[].layers[].ease {from_crop, from_place, from_opacity?, frames ≥ 1, curve: linear|ease_in|ease_out|ease_in_out}`** (`schemas/renderplan@1.json` `$defs/layer_ease`). At output frame `k` of the part, `t = curve(k / frames)` (1 from `frames` on) and each value is `from + (own − from)·t`; the curves are the model's `Curve::apply`, byte for byte. A plan with no ease is written `/0` as before (`schema_for`), so every existing plan, golden and `render_sig` is unchanged.
- **The planner** plans an eased split as its own plan part (like a cut split): its layers carry the *settled* values (`effective_settled`, new in `gmclip-model::effective`: the split in force fully applied) and `ease.from_*` = `effective()` at the split (where the ease starts, mid-ease of an earlier split included). An eased split on a cut-out (roto) layer is refused: the mask track is cached per crop.
- **The compositor** applies the ease in `frame_draw`, the one function the preview and the export share (R-COMP-1 holds by construction).
- **Follow the cursor** (`gmclip_core::cursor`, pure): frame differences at 2 fps say where to look, an arrow template (eight heights, dark body + light rim or the inverse, scored with a pixel of slack; the body must widen down its diagonal, a filled glyph does not) says whether the cursor is there; a placement must be new (not in the earlier frame), and not in moving picture (≤ 6 % of the coarse cells around it changed); a sample with no change where the cursor was keeps it. The crop path: `max_zoom` × smaller than the layer's own crop, inside it; holds while the cursor stays in the middle 60 %, moves at most once a second, zooms out after 3 s without it. Each move is an eased split (EaseInOut, 600 ms default) written by `cursor_follow_apply`, which replaces the part's previous follow (eased splits patching only that layer's crop) in one transaction.
- Surfaces: the holder's `crop_follow_cursor` (MCP, dry run first; core declares it so its schema and doc are listed) and the app's per-part "Follow cursor" switch, which run the same `gmclip_mcp::cursor::follow_cursor`.

## Evidence
- `gmclip-plan/tests/v1.rs::layer_ease` (accepted under `/1`, refused under `/0`, frames and curve bounds, halfway value); `gmclip-core/tests/planner.rs::an_eased_split_plans_as_an_ease_matching_effective` (40 frames: the plan's interpolation equals `effective()`); `gmclip-core/tests/cursor_follow.rs` (one transaction, replaced as a whole, hand-made splits kept, off, refusals); `cursor` unit tests (a moving arrow found within 20 px on a text-filled desktop while typing, a white arrow on a dark theme, a scroll unknown, an excluded camera ignored, the crop path's dead zone and zoom-out); `gmclip-mcp/tests/media_tools.rs::follow_the_cursor_is_agent_reachable`; `cursorfollow.spec` on AJ's 55 s 1080p story render (sw08): cursor placed in 63 of 111 samples, 9 moves, 19–45 s on the loaded dev M1 Pro (decoding every frame of the master dominates).

## Limits
- Frames come from the master (as the activity crop's), not the scrub proxy: at 640 px a 1080p render's arrow is 6 px high, too small for the template.
- An I-beam or a hand cursor is not followed; a cursor that does not move is found only once it moves.
