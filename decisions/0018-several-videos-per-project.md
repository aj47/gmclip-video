# 0018 — Several videos in one project: a stream edit per video

Date: 2026-10-15 · Decided by: agent (lane-ms, AJ's brief: "i should be able to load multiple video files into one project") · Status: adopted, AJ may veto; which cut a project opens on superseded by 0034, and the stream edit per video by 0036 (one main cut holding every video)

## Context
The model always allowed `Project.sources[]` and sync groups (§4.3), but the app made one project per opened file, and decision 0011 gave each project *one* stream edit. AJ wants several recordings (a second camera, a phone take, part 2 of a stream) in one project, with cuts mixing them.

## Decision
- **A stream edit per video.** Each source not in a sync group, and each sync group, has its own `Cut{kind: stream}` (§5.17 "one per project" → "one per video"). `stream_edit_ensure{media}` finds or makes the one of that media; topic and fluff commands resolve the stream edit of the cut, media or word they are given and refuse to guess between videos. A stream edit plays its own video only (`part_add` of another media into it is refused).
- **Long-forms may take topics of several stream edits** (`longform_from_topics`); `lineage.forked_from` is the first topic's stream edit, and the parent diff and pull read each topic on its own stream edit.
- **Strikes name their video**: `words_strike` / `words_restore` take `media` (inferred from the words' transcript when known; refused as ambiguous when the cut plays several videos and it is not).
- **Looks per picture**: a new part copies the look of the part it follows only when both pictures are the same size; the app's layout presets make one layout per picture shape (and per source screen/camera boxes when the source has them) and link each part to its own.
- **Sources**: `source_list` (read), `source_remove` (refused while any long-form or short plays it, naming them; otherwise the video, its stream edit, its moments and its sync group go in one undo step), `source_relink` (a path: human only), `source_import` (MCP: the holder checks the file is inside the agent read roots, R-FS-1, before the app probes and adds it).
- **Import** (§5.1): several files at once from a drop or the panel open as one project (asked, default yes); dropping on an open project adds to it; "Are these the same recording?" is asked for two files whose lengths agree within 10 % (default yes): a sync group (the larger picture with the other file's sound, offset measured by the pair's auto-sync, else 0 s), transcribed once. Transcription runs one video at a time (`queued` while waiting).

## Evidence
`crates/gmclip-core/tests/multi_source.rs`, `crates/gmclip-mcp/tests/source_import.rs`, `app/src-tauri/tests/multi_video.rs` (a mixed short's frames equal each video's own short back to back, by SHA-256), `app/ui/tests/e2e/multivideo.spec.ts`.

## Sections changed
- `spec/SPEC.md` §5.17: "one `Cut{kind: stream}` per project" → "one per video (source or sync group)"
- `spec/SPEC.md` §5.1: "adding two sources at once asks" → adds "(two files whose lengths agree within 10 %; several files open as one project)"
