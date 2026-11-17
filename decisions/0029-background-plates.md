# 0029 — Background plates: drawn, and a style-library kind

Date: 2026-10-17 · Decided by: agent (lane K3, AJ's pick of 2026-10-16 "background plates library") · Status: adopted, AJ may veto

## Context
SPEC §5.7 has a "plate gradient [M2]" background and §8 lists plates in the style library, and RenderPlan `/0` has always carried `background {kind: plate, stops}` — but RP never said how a plate draws, the compositor refused it ("background plate (M2)"), the model's `Background::Plate {name}` named nothing, and `framing_set` refused it.

## Decision
- **What a plate draws** (RP `/0`, no schema change; nothing could draw one before, so no plan or golden changes): `stops` (2+ `#RRGGBB`) spread evenly from the canvas's top to its bottom. Row `y` (centre `y + ½`) sits at `t = (y + ½) / H · (n − 1)` and is the straight mix of the two stops around it in 8-bit R'G'B' codes, rounded half up; every column of a row is the same. It is an opaque tile (`text_layer::plate_tile`) drawn right over the clear (the first stop) and under every layer — the still-backdrop path of decision 0028 — so the CPU and GPU composes and the preview and export draw the same bytes. Plans with a plate stay `/0`.
- **A style-library kind** (`LibraryItem::Plate {stops}`, additive): `style_library_save {name, item: {plate: {stops}}}` (2–8 valid stops), `style_library_list` (with the four built-in plates — Midnight, Stream blue, Ember, Studio grey — marked `builtin: true` unless the project saved one of that name), `style_library_apply {kind: plate, name, to: {framing}}` sets the framing's background to `Plate {name}` and keeps the plate in the project's library under that name (a built-in or mine is copied in), one undo step. `framing_set` takes `{plate: {name}}` when the library or the built-ins have it. The planner resolves the name (project first, then built-in).
- **App**: `plate_save {name, stops}` saves in the project and in mine (the profile's library, app-wide); `plates_view {cut}` lists built-in + project + mine and the current one; `style_item_apply` applies one (mine passes its item). The Inspector's background area gets a **Plate** picker and a two-colour **Save plate** row.

## Evidence
- `gmclip-render/tests/backdrop_clips.rs::a_plate_is_a_vertical_gradient_under_the_layers` (exact codes at the middle stop and the half-row ends; the CPU compose darker at the top; the clear colour is the first stop).
- `gmclip-core/tests/style_library.rs::plates_save_apply_and_plan` (built-ins listed, a plate saved, bad stops refused, a built-in applied → framing background + kept in the library, planned `/0` with its stops, mine by name, an unknown name refused).
- App `tests/style_library.rs::my_plates`; e2e `backdrop.spec.ts` "background plates".
- R-COMP-1 (`rcomp1_lab::preview_equals_export_with_a_plate`, lab, release): h79 v4 over a three-stop plate — 0 of 50 sampled frames differ (export 46.3 s).

## Limits
- Vertical only; an angle or radial plate needs a RenderPlan field.
- 8-bit stops are mixed without dithering, so a long dark gradient can band.

## Sections changed
- `spec/renderplan-v0.md` is not edited: this file defines the plate's drawing (RP §2.6 listed plates as not drawn). SPEC §8 plates are built as a style-library kind.
