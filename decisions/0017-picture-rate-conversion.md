# 0017 — RenderPlan `/1`: picture rate conversion (`parts[].picture.fps`)

Date: 2026-10-15 · Decided by: agent (lane-fps, AJ blocked: "the planned RenderPlan fails validation: unsupported at /sources/0/expect/video/fps") · Status: adopted, AJ may veto

## Context
RP rule 3 renders a picture source only when its nominal rate equals `output.fps`. Output rates are integers (rule 4), and the framings the app makes round a source's rate to one (29.97 → 30). So every 29.97, 23.976 or 59.94 fps file AJ opened failed to plan, along with a 60 fps game capture in a 30 fps short, a 24 fps camera in a 30 fps cut, and any cut that mixes rates.

## Decision
- **`gmclip.renderplan/1` adds `parts[].picture.fps {num, den}`**: the picture source's nominal rate, present only when it differs from `output.fps` (equal is `Invalid`). When the source declares `expect.video.fps`, the two must agree (`Invalid` otherwise).
- **Rule 3 under `picture.fps`.** `first_frame` counts `output.fps` frames on the source's presentation clock (presentation index 0 at time 0). Output frame `f` of part `p` shows the source frame presented **at or before** source time `T = (first_frame + f − out[0]) / output.fps`. That is the largest presentation index whose PTS, minus index 0's, is at most `T` rounded to the nearest tick of the track's timescale (ties up). So a time on a frame's own rounded timestamp shows that frame. There is no blending: a faster source drops frames and a slower one repeats them. The pick is deterministic. It reads timestamps and never assumes even spacing (VFR). A time past the last frame shows the last frame.
  - For an ideal CFR source this is `⌊(first_frame + k)·fps / output.fps⌋` (`gmclip_plan::retime::nominal_frame`). Thumbnails, matte spans and analysis samples use that form, since they have no index.
  - `freeze_first` shows the pick of the part's first output frame. `hold {frame}` is still an absolute presentation index.
- **Rule 3 `Unsupported` stays** for a picture source at another rate when a part shows it without `picture.fps`, or when a layer reads it through `picture_source`.
- **Audio is unchanged.** It is still 48 kHz on the output clock (rule 4) from the same source time, so picture and sound cannot drift.
- **The planner** writes `picture.fps` whenever the picture source's probed rate differs from the framing's. A plan whose sources all match is written `/0` (`schema_for`), so every existing plan, golden and `render_sig` is byte-identical. Existing projects whose framings were saved at a rounded rate plan as they are, with no migration.
- **The renderer** picks in `frame_draw_with`, the draw list the preview and the export share (R-COMP-1), from each source's PTS clock (`Pictures`, built from the demuxer's sample index). The moment-clip remux refuses a converting part, and the caller renders it instead.
- **Probing and framings.** A video's rate is its frame count over the span of presentation times on any clock (`app/src-tauri/src/video.rs::track_fps`). It snaps to the nearest standard rate (24, 25, 30, 48, 50, 60, 23.976, 29.97, 59.94) when that is within 0.5 % **and** moves the last frame by at most half a frame, so addressing frames at the standard rate cannot drift from the sound. Otherwise the measured mean is kept exactly and the planner converts by timestamps. That covers AJ's restream (10 kHz clock, 330–340 tick frames) → 30, a 1 ms Matroska 29.97 → 29.97 (not 30, the nearer rate wins) and a true 30.03 over an hour → kept. A cut made from a video takes an exported rate (`planner::output_rate_for`): the rate itself when allowed, else 15 → 30, 120 → 60, 144 → 48, else the nearest. Exact NTSC framings and outputs: decision 0020.
- A plan that still fails validation is refused in words: "this video's frame rate (59.94 fps, x.mp4) cannot be shown in this cut's 30 fps", "this video's sound is at 44.1 kHz…". The cut view adds what to do.

## Evidence
- `gmclip-plan`: `retime` unit tests (60 → 30 every other frame; 29.97 → 30 one repeat in 1001; 23.976 → 30; VFR by timestamps; rounded-ms timestamps) and `tests/v1.rs::picture_rate_conversion` (each rate valid under `/1`; `/0` refuses the key; equal or disagreeing rates `Invalid`; a non-converting part keeps `Unsupported`).
- `gmclip-core/tests/planner.rs`: 29.97, 23.976, 25, 60 and 59.94 sources into 30 and 60 fps framings; a mixed 60 + 23.976 cut; AJ's 29.97-in-30 case; the refusal wording.
- `app/src-tauri` `video::rate_tests` (restream clock, ms Matroska, exact clocks, a drifting 30.03 kept, 120); `app/ui/tests/e2e/fps.spec.ts` (29.97 and 59.94 fixtures open and plan; it fails without the conversion with "This cut cannot be shown"); `fpslab.spec.ts` (lab: AJ's 67 min restream opens at 30 fps, a 10 s span plans and exports: 300 frames, 10.000 s).
- `gmclip-render/tests/rate_conversion.rs` (macOS, ffmpeg-made sources): one 30 fps cut of a 60 fps, a 23.976 fps and a VFR source. Every output frame of the compose (`render_frames`) and of the exported MP4 (decoded by ffmpeg) shows the source frame that ffmpeg's own decode and PTS put at or before its time.

## Sections changed
- `schemas/renderplan@1.json`: `$defs/part/properties/picture/properties/fps`.
- `spec/renderplan-v0.md` is not edited (this file is the `/1` delta, as for 0014 and 0016).
