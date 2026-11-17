# 0009 — RenderPlan sources name their audio track

Date: 2026-10-15 · Decided by: agent (coordinator's go, 2026-10-15) · Status: adopted

## Context
AJ's OBS recordings often carry the mic on its own audio track. The import sheet (U.15) lets him pick it and transcripts follow it (`Source.audio_roles.voice`), but RenderPlan v0 had no way to say which track a source's audio comes from, so every export carried the file's first track — game audio or music instead of his voice.

## Decision
- `sources[].audio_track` (optional integer ≥ 1): the container track id (MP4 `track_ID`, Matroska `TrackNumber`) every part's audio from that source is read from; absent = the first audio track (unchanged behaviour). Added **within `/0`** under rule 12 (an optional field inside an existing object): no `gmclip.renderplan/1`; a renderer that predates it rejects plans using it, as rule 12 intends.
- A track the file lacks is `Invalid{/sources/i/audio_track}` at open; `0` is `Invalid` at validation (fixture `audio-track-zero`).
- The model planner fills it from the source's audio roles (`voice`, when the probe lists that track) and takes `expect.audio` from that track. RT imports never set it, so their plans — and `render_sig`s, which hash the canonical JSON — are byte-identical.
- Export reads the named track; `listen` and transcription already used the voice track; preview has no audio yet (its waveform lanes come from analysis of the voice track); export verify reads the delivered file, which has one track.

## Evidence
- `app/src-tauri/tests/importsheet.rs::the_export_carries_the_chosen_mic` (lab): a file with a 440 Hz tone on track 1 and AJ's voice on track 2. Mic export −20.7 dB, −22.2 dB with 440 Hz notched out (the voice); first-track export −34.6 dB → −81.9 dB notched (the tone).
- `crates/gmclip-plan` tests: schema file = RP §2.2, schema = types, invalid fixtures.

## Sections changed
- `spec/renderplan-v0.md` rule 4: + the `audio_track` sentence; §2.2 `sources.items.properties` + `audio_track`.
- `schemas/renderplan@0.json`, `schemas/examples/make_examples.py` (+ `invalid/audio-track-zero.json`).
