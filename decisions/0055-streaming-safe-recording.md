# 0055 — Streaming-safe recording while OBS streams

Date: 2026-10-29 · Decided by: AJ (drafted 2026-10-20 after the live stream dropped during the first gmclip recording; AJ said "merge all" on 2026-10-29, the caps not yet re-measured) · Status: adopted

## Context
AJ's first recording (0045) ran while OBS streamed to YouTube, TikTok, Twitch and Restream (VideoToolbox hardware H.264, 2560×1440 30 fps). The recording was one 5K-backed display at 60 fps, H.264 4096×2304, and dropped 585 of 3540 frames: the M1 Pro's one hardware video encoder was full. OBS's outputs logged ~1,400 frames drawn but never delivered (normally 35–180), about 46 s at 30 fps, which is how long gmclip had been recording when all four RTMP connections failed within 70 ms (14:58:26). OBS lagged no rendering frames (33 over the whole stream), so the encoder, not the compositor, starved. OBS only reconnected at 14:58:39, half a second after gmclip released its encoder at stop.

## Decision
- **Streaming-safe** (`RecordOptions.streaming_safe`, `recording_prepare {streaming_safe?}`, `gmclip record start --streaming-safe <bool>`): the screen records at **30 fps at most** with each picture **within 2560×1440 pixels** (scaled keeping its aspect; the input log maps to the file's pixels as before). That is about OBS's own load on the encoder again, not four times it.
- **On by default while a live encoder runs** (OBS Studio, Streamlabs Desktop, Ecamm Live, Wirecast, by bundle id); off otherwise. `false` turns it off.
- The session lists it in `missing` ("full size: OBS Studio is running, so the screen records at 30 fps within 2560×1440 (streaming-safe)"), and the Record sheet says so under Frame rate.
- **Dropped frames are surfaced**: opening a session whose screen file left out more than 2 % of its frames adds a note with the count (and "try 30 fps" at 60).
- The face file (camera at 30 fps) is unchanged.

## Evidence
- OBS log `2026-10-20 14-19-32.txt` lines 787–858; the session's `session.json` (`fps 60, frames 2955, dropped 585`) and `events.ndjson` (the frame-slot gaps hold steady at ~9 drops a second for the whole take).
- Tests: `gmclip-gpu` `capture::screen::tests::a_pixel_cap_scales_the_picture_down_keeping_its_aspect`, `capture::sources::tests::obs_is_a_live_encoder_and_a_browser_is_not`; `gmclip-media` `rec::record::tests::obs_running_records_at_30_within_1440p_and_says_so`, `rec::tests::a_take_that_dropped_frames_says_so`.
- Not yet measured: a streaming-safe take beside a live (unlisted) OBS stream, watching OBS's frames output against frames drawn.

## Sections changed
- `spec/SPEC.md` §5.18 points at 0045; this refines its screen settings (no spec text changes).
