# 0031 — Motion tracking: overlays pinned to a moving point (`Overlay.track`, RenderPlan `/1` `tracks`)

Date: 2026-10-17 · Decided by: agent (lane K3, AJ's pick of 2026-10-16 "motion tracking") · Status: adopted, AJ may veto

## Context
AJ asked to pin a caption, callout or cover to a moving point: on macOS with Apple Vision's object tracking through the existing objc2 runtime pattern (no new crates), on Windows an honest "not on Windows yet", and the track stored as keys on the overlay so export never needs Vision. Nothing in the model, the plan or the compositor could move an overlay over time (overlay keyframes were refused, "M4").

## Decision
- **Tracking** (`gmclip-gpu` `apple::Tracker`): `VNTrackObjectRequest` (accurate level) seeded with a `VNDetectedObjectObservation` box, run through one `VNSequenceRequestHandler`, each frame's observation fed back as the next `inputObservation`; boxes converted to a top-left origin. `gmclip_render::export::track::track_point` decodes a source's frames in order (the export's decoders), hands Vision a square box of 15 % of the picture's height around the point on the first frame (Vision sees that frame too, so the next answer is a move), and stops where confidence drops below 0.3 (lost). On Windows the same API returns "motion tracking is not on Windows yet (it uses Apple Vision)".
- **Model (additive, epoch 3)**: `Overlay.track {media, keys: [{t, x, y}], method}` — the point on a video's picture (normalised, origin top left) at times on that video's clock. Stored on the source clock, not the output, so trims, reorders and a new layout keep the pin on the thing it follows. `overlay_track_set {overlay, track | null}` (registry, M2): text, picture or callout overlays; keys strictly increasing, points near the picture; one undo step.
- **Planner**: each key a part of the cut plays becomes an output frame (`model::clock::src_to_cut`, the one tie rule) inside the overlay's span, and the canvas pixel where the topmost visible picture layer whose crop holds the point draws it (place from crop, SPEC §4.2's derived height). Keys that no part plays are dropped.
- **`gmclip.renderplan/1` adds `tracks[] {kind: text|image|shape, event, keys: [{frame, x, y}]}`**: on output frame `f` the event is drawn moved by `p(f) − p(out[0])` rounded to whole pixels, `p` the keys' straight-line interpolation, held before the first key and after the last. Validation: the event exists, is tracked once, keys increase inside its `out`. The text layer moves the event's tile (same pixels, cached by content and offset), so CPU and GPU, preview and export draw it alike (R-COMP-1). Plans without tracks are unchanged (`schema_for`).
- **App**: `overlay_track {cut, overlay}` ("Pin to motion"): the overlay's pin point (a box's centre, an arrow's tip, a picture's centre, a label's anchor) at its start, the plan layer under it there, that layer's source frames through the overlay's end inside the take; Vision follows it; the keys are smoothed over 5 frames (Vision's box jitters a few pixels) and stored with `overlay_track_set`. The Inspector's overlay rows get **Pin to motion** / **Pinned** · **Unpin**; the notice says when the point was lost and for how long it was followed.

## Evidence
- `gmclip-gpu` `track::boxes_flip_between_origins`; `gmclip-render` `track::the_start_box_is_square_in_pixels_and_inside`.
- Lab `gmclip-render/tests/track_lab.rs` (Vision): a 40 px gridded square crossing colour bars at 130 px/s on 640×360 followed for 81 frames, never lost, **mean error 3.1 px, worst 7.0 px**, 0.5 s.
- `gmclip-plan/tests/v1.rs::event_tracks` (interpolation, holds, whole-pixel offsets, `/0` refusal, rules at their pointers); `gmclip-render/tests/backdrop_clips.rs::a_tracked_callout_moves_with_its_point`.
- `gmclip-core/tests/backdrop_clips.rs::a_pinned_callout_plans_its_track_through_the_layout` (16 keys → frames 0–15, canvas pixels through the screen layer exact, offset (162, 0), refusals, unpin).
- App `tests/backdrop.rs::pin_to_motion_follows_a_moving_square` (an 80 px square at 130 px/s in 1280×720: 60 keys, not lost, the callout moves within 3 % of the true 255 px); e2e `pinmotion.spec.ts`.
- R-COMP-1 (`rcomp1_lab::preview_equals_export_with_pinned_overlays`, lab, release): h79 v4 with a pinned callout and a pinned label — 0 of 50 sampled frames differ (export 51.6 s).

## Limits
- Cover layers are not pinned yet (a moving blur over a face would need a layer track in the plan); overlays are. *(Pinned since: decision 0033, `layers[].track`.)*
- A track follows one take: past the take's end the overlay holds its last move. *(Since decision 0033 it follows every take of the video the overlay spans.)*
- Synced groups (several cameras) are refused for now: pin on a take of one video. *(Still refused after 0033.)*

## Sections changed
- `schemas/renderplan@1.json` (`tracks`, `$defs/event_track`); `spec/renderplan-v0.md` is not edited (this file is the `/1` delta, as 0014). SPEC §4.3 `Overlay` gains `track` (this record; `Keyframe` entities are still not planned).
