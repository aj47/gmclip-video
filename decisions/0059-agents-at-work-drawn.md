# 0059 — Agents at work, drawn: a cursor per run from its commits, and half-made cuts that are not errors

Date: 2026-11-24 · Decided by: agent (go from AJ on 2026-11-24: "i want gmclip to be able to work like shown in this video … with the cursors and stuff and not break or show errors while the agent is working") · Status: adopted

## Context
AJ pointed at Editroom's launch video (robbietilton/Editroom, MIT): a dimmed window, dashed outlines where clips will land, then some twenty coloured cursors placing, trimming and grading at once. Its source says what that is. `Magic/AutoPlanner.swift` picks the shots with frame scores and beat detection ("no network and no key"), the whole plan exists before anything moves, and `Magic/MagicShow.swift` then "plays an edit plan as a swarm of cursors: each edit is handed to one of many fake cursors", 14 to 64 of them, stretched to last about 7 s, with input locked. An outside agent over MCP gets one cursor, and its tool call waits for the animation. The edits and the single undo step are real; the count of cursors, their timing and the "thinking" are a show.

gmclip already had the real half: every transaction reaches the window as `committed{by, command, triples}` (§3.10), an outside agent's run is undoable in one step (decision 0050), and the window stays usable while agents edit. It drew none of it: a chip said "Agent working on this cut". And it did show errors while an agent worked. A session that deleted a cut's parts and added new ones (how agents rebuild a cut) got a red banner, the viewer's red line and up to four red toasts saying "the cut does not plan yet: the cut has no parts", the last of them staying 8 s after the cut was whole again (reproduced in `agent-watch.spec.ts` before the change).

## Decision
- **One cursor per run, moved only by its commits.** `presence.ts` reads the project scope's `committed` events of the cut on screen. A commit by `run:<id>` puts that run's cursor on the first entity it changed that a pane can place (made or edited before deleted), at the edge a trim moved, with the command as a word ("Split", "Trim", "Delete", "Edit cut") and the run's name (the session label, from the cut's `agent_working` or History's row). Each changed entity gets a ring for 1.8 s; a deleted one, a dashed outline where it was, closing up in 0.8 s. A change no pane places goes to the cut's title or the viewer. Human and system commits draw nothing. Runs on one cut never share a hue (six hues). A cursor whose part was removed since stays where it last worked.
- **No show.** No cursor moves without a commit, there is one per run however many edits it makes, nothing is replayed or slowed, and the agent's call never waits for the window. A cursor says what it did for 2.6 s, then only its name; it stays while its client keeps calling (`activity`, read every 2 s) and goes 20 s after the last call.
- **The window stays yours.** The layer takes no pointer or key and is hidden from assistive tech (the chip is the status read out). Editroom locks input during its show; gmclip does not.
- **A half-made cut is not an error.** While a run has committed to the project within 4 s, a cut that does not plan shows "<agent> is mid-edit: <what>, for now" in place of the red banner, the viewer's error line is hidden, and plan errors from the viewer and from reads are held. When the run has been quiet 4 s they are said if the cut still does not plan, and dropped if it plans.
- **The viewer's error is said 300 ms late, and only if it still stands**, for everyone: the commit and the frame it broke arrive on two scopes in either order.
- **One subscription per project.** `watchProject` claims a project before it subscribes. Two opens of one project used to race and subscribe twice, so every event was handled twice; nothing showed it until commits were counted (nine counted as eighteen in about one run in ten).
- **Cost.** A batch of events (one per 16 ms) is one pass: each run's count and last commit, at most 32 rings. Places are read then written once per animation frame, and only while a cursor or ring shows.

## Evidence
- `app/ui/tests/e2e/agent-watch.spec.ts`: a session's split puts a cursor named `thread-a` labelled "Split" on the part, on the parts lane, with a ring on the new part; the part under it still takes a click; your own edit draws none; a trim and a delete relabel it; nine commits count nine. A second session's trim is a second cursor, `thread-b`, on its own part, in another hue. Emptying the cut and building it again shows "thread-a is mid-edit", no `cut-error`, and no error toast for 5 s after. Emptying it and stopping shows `cut-error` within 10 s.
- R-EVT-1 with the layer drawing: `app/ui/tests/e2e/evtburst.spec.ts` (ten 1 000-op bursts).

## Not done
- Reads have no place: `see`, `listen` and `transcript_read` from an outside session keep its cursor alive but do not move it (only launched runs record `looking_at`). A `looked` event on the project scope would let the cursor go to the frame or the words it read.
- Words have no locator: `words_strike` rings the part, not the struck words.
- Checked in Chromium against the dev bridge, not yet in the app's WKWebView.

## Sections changed
- `spec/SPEC.md` §3.10: one bullet after R-EVT-1 (what the cut on screen draws from the events, and the 4 s rule for a cut that does not plan).
