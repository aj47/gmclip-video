# 0030 — RenderPlan `/1`: a part's constant speed (`parts[].speed`)

Date: 2026-10-17 · Decided by: agent (lane K3, AJ's pick of 2026-10-16 "speed ramps") · Status: adopted, AJ may veto

## Context
The model has carried `Part.speed` since M1 and `model::clock` already scales a part's length and its `local ↔ src` times by it, but RenderPlan could not say a speed, so the planner refused every part not at 1×. AJ asked for constant speed per part (¼–4×) with the sound pitch-preserved or muted for big factors, then ramps only if they could be done cleanly. A speed changes pixels and samples, so it is a plan field (rule 12).

## Decision
- **`gmclip.renderplan/1` adds `parts[].speed {num, den, audio?: stretch|mute}`** (`$defs/part_speed`): `num/den` in [¼, 4] and never 1 (absent at 1×).
  - **Picture** (decision 0017's rule, generalised in `retime`): output frame `k` of the part is source time `(first_frame + k · num/den) / output.fps` — frame `⌊first_frame + k · num/den⌋` at the output rate, or the frame at or before that time on the source's own clock when the part converts its rate. 2× shows every other frame, ½× each twice; no blending.
  - **Sound**: the part reads `⌈n · num/den⌉` source samples from `first_sample` (for its `n` output samples) and is **time-stretched to exactly `n` with its pitch kept** — WSOLA in `gmclip_media::audio::stretch` (40 ms Hann frames at half overlap, ±10 ms search by normalised cross-correlation of a mono mix, coarse every 4 samples then to the sample; first frame not faded in), our own deterministic f32/f64 arithmetic, no crate — or is silent with `audio: mute`. Gain and fades apply after, as for any part. The graph stretches a part whole the first time a mix touches it and keeps it (`AudioPlan::stretched`), so a mix in chunks (playback) equals the whole mix (export, verify).
  - A join touching a sped part is **never continuous** (rule 7), so both sides take their de-click; `verify`'s per-part A/V alignment skips a silent (muted) part, which has nothing to align.
  - Plans without a speed are unchanged (`schema_for`), so every existing plan, golden and `render_sig` is byte-identical.
- **Planner**: `Part.speed` becomes `num/den` in hundredths (reduced); the part lasts `(src_out − src_in) / speed` output frames (the one tie rule); its sound is stretched from ½× to 2× and **muted beyond** (`STRETCH_RANGE`: past 2× either way speech smears). A visual split inside a sped part is refused for now. Captions follow the speed: source words map onto the output clock through the part's speed (`captions::src_to_out`), heard stretches span the part's sped length, and a sped part is **heard** (`Change::Speed`; its mix key carries the speed), never given its source words as identical audio.
- **Command** `part_set_speed {part, speed}` (registry, M2): ¼–4× to the hundredth; trims the part's end by under `num` frames so it lasts whole output frames; says whether the sound plays. One undo step. The Inspector's **Sound and camera** gets a **Speed** menu (¼, ½, ¾, 1, 1¼, 1½, 2, 3, 4×).
- **Ramps are not built**: an eased speed change inside a part needs a time map whose integral gives the source time per output frame and a stretch whose rate varies per frame; doable, but not cleanly in this lane alongside the rest. Split the part and give each piece its own speed meanwhile. *(Built since: decision 0033, `parts[].speed.ramp`.)*

## Evidence
- `gmclip-plan`: `retime::speed_tests::speed_picks_frames_by_source_time` (2×, ½×, 1.5×, 60 fps at 2×, by clock, span); `tests/v1.rs::part_speed` (`/1` only, bounds, `audio`, not continuous, source samples rounded up).
- `gmclip-media`: `stretch` tests (440 Hz stays within 4 Hz at 1.5×, 2×, ½×, 4×, ¼×, level within 0.85–1.1, exact length, deterministic, short inputs); `graph::sped_parts_stretch_or_mute_and_chunk_exactly` (chunked mix = whole mix, muted part silent, joins not continuous).
- `gmclip-core/tests/heard_captions.rs::a_sped_part_is_heard_and_its_words_follow_the_speed` (2 s/1 s/2 s parts, first frame and sample, fades, `/1`; "echo" 0.1 s into the take lands 0.05 s into the 2× part; 3× muted and 20 frames; refusals; back to 1×).
- R-COMP-1 (`rcomp1_lab::preview_equals_export_with_speed_changes`, lab, release): h79 v4 with parts at 2×, ½× (stretched) and 3× (muted) — the export verifies (A/V at all 11 discontinuous joins) and **0 of 50** sampled preview frames differ (export 42.7 s).
- e2e `speed.spec.ts` (55 s → 27 s at 2×, the 4× notice, back to 55 s).

## Sections changed
- `schemas/renderplan@1.json` (`$defs/part.speed`, `$defs/part_speed`); `spec/renderplan-v0.md` is not edited (this file is the `/1` delta, as 0014).
