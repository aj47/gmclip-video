# 0013 — Roto mattes in RenderPlan `/0`, cut visual splits as plan parts

Date: 2026-10-15 · Decided by: agent (lane R, AJ's "Work on" for "Cut out my face with no green screen", "Preview roto cut-out quality per part and fix edges with a brush" and "Have the screen crop follow whatever is changing on screen") · Status: adopted, AJ may veto

## Context
SPEC §5.8 puts roto at [M4] (Apple Vision on Mac) and §5.7 the activity crop at [M3]; AJ picked both to work on now. RenderPlan v0 could not say a matte (RP §2.6), and the M1 planner refused any visual split inside a part, which the activity crop's "split at the move" needs.

## Decision
- **`layers[].matte {kind: "roto", model, soften?, strokes?[]}`** is added **within `/0`** under rule 12 (an optional field inside an existing object), with `$defs.matte` and `$defs.matte_stroke` (`mode: add|subtract`, `radius` and `points` in source pixels, `feather` 0–1). RP rule 5 says what it does: the layer's alpha is multiplied, in source pixels before the resample, by the mask track of the crop's integer bounds (snapped to even) over the part's frames, softened by `soften` source pixels (two box passes), then each stroke in order. Plans without a matte are byte-identical (the field is skipped), so every golden and `render_sig` is unchanged.
- Model: `Matte::Roto` gains `strokes: Vec<MatteStroke>` (skipped when empty; strokes in normalised source coordinates, radius a fraction of the source height). Brush strokes live on a part's layer override, so they correct that part only; `matte_set` keeps them.
- Mask tracks: Apple Vision `VNGeneratePersonSegmentationRequest` through the Objective-C runtime (`objc2` `msg_send!`, no binding crate), `.accurate` through one `VNSequenceRequestHandler` per span for export, `.fast` per frame for the live draft; resampled to the crop's source pixels, smoothed `(prev + 2·cur + next)/4`, coded with our own PackBits RLE (no compression crate is in the tree), cached by `(source fingerprint, span, crop, model)` in `~/Library/Caches/gmclip/mattes` (2 GiB cap, LRU by file mtime). Export waits for accurate tracks unless `ExportOptions.draft_mattes` (the user's confirm), and then lists the layers in the record's `draft_mattes[]`.
- The planner turns each **cut** visual split inside a part into its own plan part (continuous join, no fade); eased splits are still refused (no keyframes in `/0`).
- RenderPlan `/1` (lane U's decision on overlays) must carry the same `layer.matte` and `$defs` when the two meet: its `v1_extends_v0` test will say so.

## Evidence
- `crates/gmclip-plan` tests (schema file = RP §2.2, schema = types); `crates/gmclip-core/tests/planner.rs::cut_visual_splits_and_roto_mattes_reach_the_plan`.
- `crates/gmclip-render/tests/roto_lab.rs` (lab): preview = export byte-identical on 30 matted frames of h79; Vision timings in `milestones/m2/perf.md`.

## Sections changed
- `spec/renderplan-v0.md` rule 5: + the `matte` sentence; §2.2 `layer.properties.matte`, `$defs.matte`, `$defs.matte_stroke`.
- `schemas/renderplan@0.json`.
