# 0047 — RenderPlan `/1`: a layer of another source at its own rate (`layers[].fps`)

Date: 2026-10-20 · Decided by: agent (AJ: "just tried and hit a lot of errors. please work hard to fix this") · Status: adopted, AJ may veto

## Context
A recording with a face file opens with the camera bubble over the screen (decision 0046). AJ's Cam Link 4K sends 1080p at 25 fps, the HDMI input's rate, and the face file records it as it comes, while the screen and the cut are 30 fps. The bubble is a layer reading the face file live through `picture_source` + `first_frame`, which counts the face file's frames on the part's output clock, so rule 3 refused it: the recording opened to "this video's frame rate (25 fps, face.mov) cannot be shown in this cut's 30 fps" and an empty cut. A part's own picture at another rate has been read by time since decision 0017; a layer of another source could not be.

## Decision
- **`gmclip.renderplan/1` adds `parts[].layers[].fps {num, den}`**: the nominal rate of the layer's `picture_source` when it differs from `output.fps`. Only on a layer whose `picture_source` is not the part's own (`Invalid` otherwise); `Invalid` when equal to `output.fps` or unlike the source's declared `expect.video.fps`.
- **Reading by time.** With `fps`, `first_frame` counts `output.fps` frames on the source's presentation clock, as `picture.fps` does (0017): output frame `f` shows the source frame presented at or before `(first_frame + f − out[0]) / output.fps`, by the source's own timestamps when the compose has them, else `⌊(first_frame + f − out[0]) · fps / output.fps⌋`. `freeze_first` is that pick at `out[0]`; `hold {frame}` is still an absolute presentation index. A 25 fps camera in a 30 fps cut shows each frame once or twice, never blended.
- **Rule 3**: a picture source at another rate validates when every part showing it converts (`picture.fps`) **and** every layer reading it is a layer of another source with `fps`. Without `fps` it stays `Unsupported`.
- **The planner** writes `fps` on a layer of another sync-group member (decision 0046) whenever that member's probed rate differs from the framing's. Its `first_frame` is unchanged, as it was already counted on the output clock. Such a plan was refused before, so no existing plan, golden or `render_sig` changes.
- The live other-source layer stays off parts that change speed (0046). A part converting its own picture rate is allowed: the screen and camera each read their own timestamps from the same output clock; converting the screen rate does not change elapsed time. This also covers a recording whose dropped screen frames make its measured rate differ from the cut rate.

## Evidence
- `gmclip-plan` `tests/v1.rs::another_source_at_another_rate` (valid with `fps`, round trip, nominal picks 25 → 30; `Unsupported` without; `Invalid` equal, unlike the declared rate, or on the part's own source). `schema_matches_types` passes with the new key.
- `gmclip-core` `tests/screen_look.rs::a_camera_at_another_rate_is_read_by_time`: a 25 fps face member beside a 30 fps screen plans with `fps 25/1` on the bubble and validates.
- `gmclip-render` `tests/layer_frames.rs::another_source_at_another_rate_is_read_by_time`: nominal picks, and a clock with a dropped frame shifting the pick.
- AJ's "Recording 2026-10-20 16-27-07" (screen 30 fps, `face.mov` 25 fps) plans and exports with the bubble (1920×1080, 30 fps, 108 frames); a new 18.8 s take ("Recording 2026-10-20 16-58-04", face 472 frames at 25 fps) opens with the bubble in the viewer and exports.

- Regression: `gmclip-core::screen_look::a_camera_beside_a_rate_converting_screen_is_read_by_time` plans a 42 fps screen and 25 fps camera into a 30 fps cut; `gmclip-render::layer_frames::camera_clock_is_independent_of_screen_rate_conversion` checks both frame picks independently.

## Sections changed
- `schemas/renderplan@1.json`: `$defs/layer/properties/fps`.
- `spec/renderplan-v0.md` is not edited (this file is the `/1` delta, as for 0014, 0016 and 0017).
