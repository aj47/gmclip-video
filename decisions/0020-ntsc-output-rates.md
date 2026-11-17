# 0020 — RenderPlan `/1`: NTSC output rates; a video's cut takes its exact rate

Date: 2026-10-15 · Decided by: agent (lane-fps, AJ blocked on 29.97 fps files; follows 0017) · Status: adopted, AJ may veto

## Context
Decision 0017 made any picture rate render into any output rate. But cuts made from a 29.97 file were still framed at 30. The file then rendered with one repeated frame every 33 s and its cut ran 0.1 % off the video's own clock. RP rule 4 kept NTSC outputs `Unsupported` "until audio-sample rounding for them is specified in `/1`". This decision specifies that rounding.

## Decision
- **`gmclip.renderplan/1` allows `output.fps` ∈ {24000, 30000, 60000} over 1001** (23.976, 29.97, 59.94). The integer rates keep `den` 1. Any other pairing is `Invalid{/output/fps}`. `schemas/renderplan@1.json` widens `num`'s enum and turns `den`'s `const 1` into `enum [1, 1001]`. That is the one place `@1` changes an `@0` value rather than adding one, and `v1_extends_v0` lists it. `/0` is unchanged and refuses NTSC.
- **Rule 4 at an NTSC rate:** `S(f) = ⌊f · 48000 · den / num⌋`. Integer rates are unchanged and exact. A 29.97 frame carries 1601 or 1602 samples, and part spans still telescope (`S(out[1]) − S(out[0])`), so the total is exactly `S(frames)`.
- **The planner** takes a framing at any RenderPlan rate. Grids run in flicks per frame, which is exact for every allowed rate (29.97: 23 543 520). A part's first sample is its time rounded down to a sample. A part that continues the previous part's sound (same source, abutting time) continues its sample count exactly, so continuous joins stay continuous (no click-guard fades) and stay within a sample of their time. A plan at an NTSC rate is written `/1`. Integer plans are unchanged and stay `/0` with the same `render_sig`.
- **Framings** made from a video take `planner::output_rate_for`: the video's rate exactly when RenderPlan outputs it (NTSC included), else the integer rate of 0017. This covers video open, stream edit, long-form, new cut, cut-like and consolidate grids. Existing framings, including ones saved at a rounded 30, are not rewritten and plan as they are (0017 converts their pictures).
- **Encode and mux:** an NTSC output's video timescale is its numerator (30000 / 24000 / 60000), one frame = 1001 ticks (`mux::video_timescale`; SPEC §6.5 step 4's 90 kHz holds 29.97 but not 23.976 or 59.94 exactly). VideoToolbox rate hints and GOPs use the rounded rate. Verify checks duration against `frames · den / num` and audio against `S(frames)`. The export record carries `fps_den`.
- **Review and clip plans** (moment review, stream view, moment clips) run at the picture's own NTSC rate rather than 30, so their index addressing no longer drifts 0.1 % from the sound.
- **The app and UI** read rates as `num/den`: `planner::fpf`, `ExportEntry::rate`, `rate()` in `actions.ts`. The viewer's play clock is `S(f)` and its inverse, and the UI gets `fps` as a float.

## Evidence
- `gmclip-plan/tests/v1.rs::ntsc_output_rates`: each NTSC rate is valid under `/1` and refused under `/0`. Per-frame samples are ⌊48000·1001/num⌋ or one more, and the parts total `S(frames)`. 30000/1 and 30/1001 are `Invalid`.
- `gmclip-core/tests/planner.rs::ntsc_framings_plan_at_their_exact_rate`: 29.97, 23.976 and 59.94 framings over sources at the same rate, 30, 23.976 and 60. First sample rounded down, the continuous join exact with no fade, and within a sample of its time. `a_video_makes_a_cut_at_an_exported_rate`.
- `gmclip-render/tests/rate_conversion.rs::a_29_97_output_maps_ntsc_frame_for_frame_and_converts_30`: the export's own verify passes. ffprobe reads `30000/1001` with the planned frame count, and every frame shows the expected source frame (29.97 frame for frame, 30 converted).
- `app/ui/tests/e2e/fps.spec.ts`: 29.97 and 59.94 fixtures open with cuts at 30000/1001 and 60000/1001, and the 29.97 one exports.

## Sections changed
- `schemas/renderplan@1.json` `output.fps`; `spec/renderplan-v0.md` rule 4 keeps describing `/0` (this file is the `/1` delta).
- SPEC §6.5 step 4 (video timescale): NTSC rates use their numerator, not 90 000.
