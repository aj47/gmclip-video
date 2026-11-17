# 0036 — One main cut per project: the stream edit holds every video

Date: 2026-10-18 · Decided by: AJ ("we should redesign to mainly work off a 'main project' view each time we start a new project pretty much by importing a file"; "one real cut"; old projects may be deleted) · Status: adopted; supersedes the stream edit per video of 0018 and the whole of 0034

## Context
With 0034 the project's top level was an assembly of windows onto per-video stream edits. It had no parts of its own, so everything that reads a cut's parts found nothing there: the agent session launched on it reported "0 parts, 0 sources" and offered to add footage, the work card counted 0 parts, and part commands were refused with "open the clip". AJ asked for one main view that a project starts from and grows by importing files.

## Decision
- **One stream edit per project, the main cut** (§5.17): `Cut{kind: stream}` holding every video (a source, or a sync group) whole, in source order, in the order the videos were added. The first imported file makes it (its shape and rate are the cut's); each later file is appended after its last part (`stream_edit_ensure{media}` → `appended`, with the parts added). A video of another picture shape gets a fitted layout of its own (the app's `fit_new_part`).
- **Parts of any of the project's videos** may be added to it (`part_add` no longer refuses another media there).
- **Topics and fluff name their video**: from the words' transcript, else the stream edit's only video, else refused ("name the media"): every video's clock starts at 0, so a source time alone is ambiguous. Topics of different videos never overlap each other.
- **Removing a video** takes its parts, topics and fluff out of the stream edit (the stream edit itself goes with its last video); refused while another cut plays it, as before. One undo.
- **Long-forms** from topics of several videos fork from the one stream edit; `source` order is the videos' order on it, then time.
- **The project map** has one stream with `videos[]` (its `media` is set only when it holds one); the app labels it "Main cut".
- **Removed (model epoch 5, not additive; never deployed):** `Cut.kind: assembly`, `Window`, `assembly_ensure`, `window_add/trim/move/remove/break`, the assembly planner, the timeline refusals (`TIMELINE_*`). A project with a project timeline no longer opens.
- **App**: a project opens and reopens on its main cut; the words strip and stream overlay read each part's own video's transcript and tag each word with its video (`owner`), and strikes, restores, nudges and fix-everywhere go per video.

## Evidence
`crates/gmclip-core/tests/multi_source.rs` (one stream edit for both videos, topics per video, long-form across videos, removal of one video and undo, a same-recording pair and another video on one main cut); `app/src-tauri/tests/main_cut.rs` (h79 v4 + v3: the second video goes on the end of the main cut, trim/blade/reorder/Tighten/transcript/remove + undo on its parts; Tighten and sound on two videos); `app/src-tauri/tests/multi_video.rs`.

## Sections changed
- `spec/SPEC.md` §5.17: "one `Cut{kind: stream}` per video" → one per project holding every video; the Project timeline bullet removed; the map lists the main cut
- `decisions/0018`, `decisions/0034`: status lines
- `plan/m2-backlog.md` §2.22: PT.3–PT.5 dropped
