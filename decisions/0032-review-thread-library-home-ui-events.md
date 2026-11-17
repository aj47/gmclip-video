# 0032 — The board's review thread, Home across the library, typed UI events

Date: 2026-10-17 · Decided by: agent (AJ's overnight go to work the coordinator's list, 2026-10-17) · Status: adopted

## Context
Lane K6 closed U.10 (library-wide Home, the review thread), U.12 (text drags preview in the frame) and A2.01 (typed core calls and event payloads). Three choices the spec leaves open needed a default.

## Decision
- **One review thread per export.** An export is known on review links by the first 16 hex characters of its file's SHA-256 (the review server's export id; no record, no key). The thread on its board tile is, oldest first: the `Review` notes and the `Comment`s whose `export_ref` is that key, the replies to those comments, and the link notes with no `export_ref` (on the cut's newest export only). Your reply is one `comment_add` with that `export_ref`, under the thread's first comment when there is one (else it starts the thread); no new entity kind, no new command. Everything not written by `human` is shown as text only in the quarantined container (R-UI-SEC-2).
- **Home across the library.** The library index (A2.16) also keeps each project's moments (count, staged, and the staged ones no cut was made from), read through the same read-only view of a closed project's database the library search uses, re-read when that database changes; the index carries a layout version and rebuilds when it changes. Home's Needs you gains "staged moment, no cut yet → Grow" for every project (at most five listed per project, then a count); Running gains agent runs and batches; a Recent exports list shows the eight newest exports across the library.
- **UI events are Rust types.** Each `gm_subscribe` scope has one tagged enum (`app/src-tauri/src/events.rs`), the only thing the bus publishes; `cargo xt gen` writes `app/ui/src/gen/events.ts` from their JSON Schemas through core's tsgen, and `tests/ts_events.rs` fails on drift. The app depends on `schemars` (already in the tree), no new crate.
- **Text drag previews stay off the op log.** `viewer_text_drag` is an app-shell viewer route; the preview session draws the moved events through the shared compositor (`TextShift`), export never sets one, and a committed drag's move is dropped when the re-planned cut arrives (3 s cap).

## Evidence
- `app/src-tauri/tests/{board_thread,library,batch,gesture,reviewlink,ts_events}.rs`; `crates/gmclip-render` `text_layer` tests; e2e `board`, `library`, `gizmos` specs. Numbers in `plan/m2-backlog.md` rows U.10, U.12, A2.01, A.03, E.01.

## Sections changed
- `spec/SPEC.md` unchanged; §5.14's "review thread" and §12.6's "across the library" are given these defaults.
