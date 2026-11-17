# 0027 — RenderPlan `/1`: upload rates past 25 Mbit/s (YouTube 1440p60 at 36)

Date: 2026-10-17 · Decided by: agent (lane K3, AJ's pick of 2026-10-16 "1440p60 at full bitrate") · Status: adopted, AJ may veto

## Context
SPEC §6.5.1 gives YouTube 16:9 1440p "H.264 High 5.1, VBR 24 Mbps @30 / 36 @60" and says upload presets have "no caps beyond the level's". The renderer already capped an upload export's 1-s peak at its level's MaxBR, but RenderPlan `/0` bounds `output.video.rate.max_bps` at 25 Mbit/s (the social ceiling, also High 4.0's MaxBR), so the planner clamped every upload preset's peak to 25 and 1440p60 could not reach its 36 Mbit/s target. The bound is in the schema, so lifting it is a schema change (rule 12).

## Decision
- **`gmclip.renderplan/1` widens `output.video.rate.max_bps` to 300 Mbit/s** (High 5.1/5.2's MaxBR). A new `/1` rule (`gmclip_plan::validate`) keeps the old ceiling where it belongs: `max_bps` above 25 Mbit/s is `Invalid` on a portrait or square canvas (the social presets never pass 25 Mbit/s peak), and above the MaxBR of `output.video.level` (`level_max_bps`: 4.0 25, 4.1/4.2 62.5, 5.0 168.75, 5.1/5.2 300 Mbit/s, Table A-1 × 1.25); then `target_bps ≤ max_bps`. `/0` keeps its 25 Mbit/s bound.
- **The planner** writes an upload preset's peak as 1.5 × its target with no 25 Mbit/s clamp: YouTube 1440p 24/36 @30 and 36/54 @60 at High 5.1, 1080p 12/18 @30 (unchanged, `/0`) and 18/27 @60 at 4.2. `schema_for` names `/1` when `max_bps` passes 25 Mbit/s, so every plan under the ceiling (every social plan, 1080p30) is byte-identical, with the same `render_sig`.
- **The renderer** takes the 1-s peak cap from the same table (`peak_cap`), and opens the encoder through `open_encoder`: an upload rate the OS encoder refuses (a Media Foundation encoder may cap its bitrate) is retried once at 24 Mbit/s average, 25 peak, at the same level, and the export record's notes say "encoder: refused 2560×1440 at 60 fps, 36 Mbit/s (peak 54), level High51 (…); encoded at 24 Mbit/s (peak 25)". A refusal there, or of a social rate, fails the export in words ("this computer's H.264 encoder refused … ; export at a lower preset (YouTube 1080p) instead"). Same code on macOS and Windows.

## Evidence
- `gmclip-plan/tests/v1.rs::upload_rates_above_the_social_ceiling` (accepted at 5.1 and 4.2 up to their MaxBR; `/0` refuses past 25 Mbit/s; a portrait canvas, a rate over the level, a target over the peak each refused at their pointer); `schema_matches_types::v1_extends_v0` lists the widened bound.
- `gmclip-core/tests/planner.rs::youtube_presets_at_60_fps_plan_their_full_rate` (1440p60 36/54 @5.1 and 1080p60 18/27 @4.2, both `/1` and valid) and the 30 fps preset test (1440p30 24/36 `/1`; 1080p30 still `/0`).
- `gmclip-render` `pipeline::encoder_open_tests` (the plan's rate when taken; a refused upload rate falls back once and says so; refusals in words).
- Lab `export_lab::youtube_1440p60_reaches_its_rate` on the M1 Pro (VideoToolbox): a 4 s grainy 2560×1440 60 fps source exports as High@5.1, 240 frames, **36.05 Mbit/s average**, 1-s peak 39.1 Mbit/s, rung 0, verify green, 4.1 s.

## Limits
- The Windows fallback is exercised by unit tests only; the MF encoders on the lab PC have not been run at 54 Mbit/s yet.

## Sections changed
- `schemas/renderplan@1.json` (`output.video.rate.max_bps` maximum 25 000 000 → 300 000 000); `spec/renderplan-v0.md` is not edited (this file is the `/1` delta, as 0014). SPEC §6.5.1 already says these rates.
