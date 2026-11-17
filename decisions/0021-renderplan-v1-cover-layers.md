# 0021 — RenderPlan `/1`: cover layers (`layers[].cover`)

Date: 2026-10-16 · Decided by: agent (lane-r2, A2.17's "Cover chat and alerts" remainder) · Status: adopted, AJ may veto

## Context
SPEC §5.7 [M2] has a **cover layer** (`kind: cover`, blur / pixelate / solid over a region) and a one-click "Cover chat and alerts" for sources with `private`/`chat`/`alerts` regions; §5.13's `private_region_under_text` lint names it as its fix. The model has carried `LayerKind::Cover` and `CoverStyle` since M1, but RenderPlan could not say it, so the planner refused cover layers and the compositor never drew one. A cover changes pixels, so it needs a plan field and a new schema item (rule 12).

## Decision
- **`gmclip.renderplan/1` adds `parts[].layers[].cover`**, one of `{kind: blur, radius}`, `{kind: pixelate, block}` (both canvas px, 1–400) or `{kind: solid, rgb}` (`schemas/renderplan@1.json` `$defs/layer_cover`). A cover layer is an ordinary layer (crop, place, z, opacity, time map, ease) whose crop is drawn obscured; it carries no matte (validation, `…/cover`). A plan with no cover is written `/0` as before (`schema_for`), so every existing plan, golden and `render_sig` is byte-identical.
- **Drawing** (the one `frame_draw` → `FrameComposer::compose` path the preview and the export share, so R-COMP-1 holds by construction): blur and pixelate draw the crop through the ordinary layer path into a small *grid* picture of `round(place / k)` texels (computed from the plan's canvas px, so a smaller preview resamples through the same picture), then back into the place — a cubic B-spline of the grid (blur, σ ≈ 0.6·radius, the blurred fill's up-pass) or its nearest texel (pixelate); solid fills the place and reads no picture. Coverage × opacity is the alpha, premultiplied over, at the layer's z. CPU (`cpu::draw_cover`) and GPU (`COVER_SHADER`) are the same arithmetic.
- **Model → plan**: `kind: cover` with a style plans as a cover layer of any region role (`chat`, `alerts`, `private`, … — `planner::cover_role_name`); the solid colour `0xRRGGBB` becomes `#rrggbb`. `layer_add` takes `cover` (kind defaults to `cover` with it; a cover without a style, or a style on another kind, is `Invalid`); `layer_set` changes a cover layer's style.
- **"Cover chat and alerts"**: `private_region_under_text` skips a region a visible, opaque cover layer of the part already holds where it lands, and every finding carries one fix for the cut and framing — a `layout_edit` adding, per part and per uncovered private region a layer shows, a blurred cover (24 px on a 1080-wide canvas) as the part's own layer, crop = the region inside the layer's crop, place = where it lands, just above that layer. Applied only by the human's click (`check_fix`).

## Evidence
- `gmclip-plan/tests/v1.rs::layer_cover` (accepted under `/1`, refused under `/0`, bounds, no matte, the grid).
- `gmclip-render/tests/cover_layers.rs`: solid fills its place exactly (partial edge blended, half opacity halfway), pixelate draws whole blocks, blur is smooth (max neighbour step < 0.03 against > 0.3 for the noise screen) and nothing outside moves, a layer above a cover draws over it, a plan's cover reaches the draw list with its grid, a pinned 8-bit golden (`tests/data/cover_layers.txt`); lab `gpu_covers_match_the_cpu`: max |Δ| 1 code value for all three styles at opacity 1 and 0.5.
- R-COMP-1 (`rcomp1_lab::preview_equals_export_with_cover_layers`, lab, release): h79 v4 with a blur, pixelate or solid cover on every part's screen — 0 of 50 sampled frames differ between the paused full-quality preview and the export's encoder input (export 48.0 s on the loaded M1 Pro).
- `gmclip-core/tests/lint.rs::the_m2_core_set`: the fix is one `layout_edit` (one cover per part), applied → the lint is quiet, the cover lands at y 986 over the chat band, a half-width cover leaves the error, the cut plans `/1` with a `chat` blur cover in the chat's source and canvas pixels; `layer_add`/`layer_set` refusals.

## Limits
- No command draws a source's regions yet (Draw your layout is M3; the HL import gives camera regions only), so in the app the lint and its fix fire only for regions an import or a later layout tool supplies.
- SPEC §5.7's "seeding covers private regions intersecting a placed crop by default" is not built (seeding has no private regions to read either).

## Sections changed
- `schemas/renderplan@1.json` (`layer.cover`, `$defs/layer_cover`); `spec/renderplan-v0.md` is not edited (this file is the delta, as 0014).
