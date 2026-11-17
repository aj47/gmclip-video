# 0014 — RenderPlan `/1`: overlays, caption animation, blurred-source fill

Date: 2026-10-15 · Decided by: agent (lane-u, AJ's "Work on" for the visual-polish stories: U.09 / A2.09 rest) · Status: adopted, AJ may veto (Needs AJ 1.17)

## Context
RP §2.6 lists what `/0` cannot say. The visual-polish stories AJ picked need four of them: caption animation (word bounce, SPEC §5.10), image overlays (SPEC §5.9, decoded per R-MEDIA-SEC-3), shape overlays (callouts: a box or an arrow on a screen region) and the blurred-source background (SPEC §5.7). New top-level keys and a new enum value need a new schema version (rule 12).

## Decision
- **`gmclip.renderplan/1`** = `/0` plus, and nothing else changed (`schemas/renderplan@1.json`; `tests/schema_matches_types.rs::v1_extends_v0` checks every `/0` key and value survives):
  - `text_events[].anim {kind?: bounce|pop|fade, pop_runs?: [run index]}` — curves in `crates/gmclip-plan/src/anim.rs` (frame counts scale with fps: 1/15 s steps; bounce 72 → 118 → 100 % in with alpha ⅓ → 1, 100 → 76 % out over the last 1/15 s when the event outlasts in + out; pop 85 → 108 → 100 %; fade 0.1 s each way; `pop_runs` pulse the event to 112 % over 0.2 s and bring those runs' fill in from the style fill over 0.1 s). Scaling keeps the block's centre and scales every px quantity of the style.
  - `images[] {id, path, sha256, w, h}`, `image_events[] {id, out, layer?, image, place, opacity?}` (`dependentRequired: image_events → images`).
  - `shape_events[] {id, out, layer?, shape: box{rect, width, stroke?, fill?, radius?} | arrow{from, to, width, stroke, head?}}`.
  - `background {kind: blurred_source, rgb, blur: 2..64, dim: 0..1}`.
- The schema pass checks a plan against the file its `schema` names (`/1`, else `/0`). The types accept both versions; **a plan that uses none of the additions is written `/0`** (the planner's `schema_for`), so every existing plan, golden and `render_sig` is byte-identical. `render_sig` appends each `images[].sha256` after the fonts' (absent when an image has none), so it still covers every input file.
- Overlay draw order: every image, shape and text event above all layers, by `layer`, then images, shapes, text, then array order.
- **Caption animation is rasterised at each frame's animated size**, not at the maximum scale and sampled down (SPEC §7's sentence). Same bytes on every machine (the B-09 raster, deterministic f64 state per frame), sharper glyphs at every scale, and a settled frame reuses the plain event's tile, so a held cue still rasterises once; the cost is one raster per animated frame (≤ 6 per cue).
- The `/0` invalid fixture for an unknown version now says `gmclip.renderplan/2`.

## Evidence
- `crates/gmclip-plan/tests/v1.rs`: a plan using every addition parses and round-trips; each addition under `/0` is refused at its pointer; the `/1` rules (references, ranges, ids, run indices) each fire.
- `crates/gmclip-render/tests/overlay_goldens.rs` + `tests/data/overlay_*.tsv`: per-frame tile hashes of the overlay path the export and the preview share; `tests/compositor.rs` pins the CPU blurred fill.
- R-COMP-1 (`rcomp1_lab::preview_equals_export_with_the_v1_looks`, lab, release): h79 v4 with the blurred fill, bounce + keyword pop on every caption, an image and two callouts — 0 of 50 sampled frames differ between the paused full-quality preview and the export's encoder input; export 53.5 s on a loaded M1 Pro (≈ 20 s without the looks).
- Fonts: the non-AJ built-in caption presets and brand kits use the bundled Figtree Black (SIL OFL 1.1, `assets/fonts/figtree/OFL.txt`; the app bundle copies the licence beside the face).

## Sections changed
- `schemas/renderplan@1.json` (new); `spec/renderplan-v0.md` §2.6 items are expressible in `/1` (this file is the delta; RP v0 is not edited).
