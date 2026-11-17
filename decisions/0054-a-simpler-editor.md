# 0054 — A simpler editor: one cut view, three side tabs, the rest behind Developer

Date: 2026-10-26 · Decided by: agent (go from AJ on 2026-10-26: the UI audit's removal plan, approved) · Status: adopted

## Context
The UI audit of 2026-10-26 read copies of AJ's 31 project databases, 39 backups and app data. Of 1,378 edits, AJ's own by hand were trims, splits, layout, place, crop and framing (`part_trim` 169, `part_split` 100, `layout_edit` 39, `place_set` 26, `crop_set` 21, `framing_set` 18). Moments, reviews, comments, suggested edits, story suggestions, topics, thumbnails, copy drafts, openings and the brand kit were never used; no cut came from a moment. 741 of 754 agent connections were outside agents; the last in-app session was on Oct 20. No saved layouts; hover scrub turned off, safe zones and render stats off. Golden cuts, layout presets, remote-send (Clippy) and review-link files were never created. The views AJ does not use cost him space and attention in the one he does.

## Decision
Nothing here removes a command: `moment_*`, `matte_set`, `comment_*`, `review_*`, `run_summary_set`, `run_choice_restore`, `clippy_*`, `retention_*`, `chat_*`, `reactions_find`, `project_consolidate`, `batch_*` and the rest stay for agents and the CLI.
- **Removed.**
  - The Moments view, its Home button, route and ⌘K entries ("Moments", "Find laughs and reactions", "Process this stream"), with the recipe strip and the chat import panel only it showed. Agents draft cuts from moments; tap marks (`,` `.`) still make moments in the cut.
  - The review board view and its route; Board leaves the top bar (the Cut/Board switch goes; off a cut, the bar keeps the project's way back). Verdicts, review threads and replies, style feedback from a tile, retention import, Send to Clippy and Retention from Clippy leave the UI (Send and Retention from Clippy could never render: `origin_ref` is never set).
  - Home: the project row's Consolidate…, the RenderPlan import field and New project (a project with nothing in it). A dropped `.renderplan.json` still imports from a row; "A consolidated project…" still opens one.
  - The topic and chat lanes pane (⌥5) and the retention lane. Topics stay in the transcript.
  - From the View menu: the built-in layouts, Save or edit layouts…, Read the full transcript (⌥R, ⌘K and the Words tab still open it). Layouts saved before stay listed.
  - Duplicates: the map's left-edge tab (the top bar's toggle and ⌥1 stay), the viewer's safe-zones toggle (the View menu's stays), the timeline's hover-scrub toggle (⌥S, the View menu, ⌘K stay).
- **Merged.**
  - The board into the cut: the export popover shows the newest export's file (Reveal in Finder, Share…, drag out) and the status pill the agent's handoff note with a Restore per choice ("Agent's note" in the pill). The export chip reveals the file. Home keeps the recent exports. The MCP instructions and `run_summary_set` say where the note shows.
  - Side tabs: **Words** (the transcript; Tighten folded inside), **Look** (layout, then the part's inspector, one scroll; its sound and music rows from 0051 included) and **Captions** (the Short tab). The panel opens on Look.
- **Hidden.**
  - Story, Polish, Thumbnail, Review and Agent: ⌘K's "More panels", in the bar while open.
  - **Settings › Developer** (`developer` UI pref, off) brings back the review link panel, Pair with Clippy…, starting agents inside gmclip (Ask an agent… in the cut and on Home's rows, Send an agent…, ⌘K's New agent session and Ask an agent about this cut, the floating agent in a cut), every panel in the bar and the View menu, and the viewer's render stats and S2a measuring. The top bar's Agents and its badge, and Home's sun, stay for everyone.
- A layout saved with an older side tab name still loads: transcript and tighten open Words, inspector and layout open Look, short opens Captions, and a tab off the bar opens Words (`tabFrom`; `uiprefs.rs` accepts both sets of names).

## Evidence
- The audit: `~/.bb/thread-storage/thr_3bcxeh7cvi/gmclip-ui-audit-2026-10-26.md` (usage counts, file references, risks).
- `app/ui/tests/e2e/board.spec.ts`: export → the popover's file (Share refused headless, "no window") → the handoff in the pill as text (markup stays text), Restore then "Already as recorded"; Developer off hides the review link and Pair with Clippy, on shows them.
- `app/ui/tests/e2e/workspaces.spec.ts`: a `ui-prefs.json` layout naming `short` opens Captions and one naming `review` opens Words; `uiprefs.rs` `a_layout_saved_before_the_tabs_merged_still_loads`.
- `app/ui/tests/e2e/ux-new-panels.spec.ts`: the View menu's three tabs at 1440 × 900 and 480 × 360; ⌘K's "More panels" opens each hidden panel; Tighten opens folded out in Words.

## Sections changed
None edited: the requirements stand, and only where the app shows them moves. Read these with this decision:
- `spec/SPEC.md` §5.2: the Moments panel and review mode have no view in the app; moments are the agents' and the CLI's.
- `spec/SPEC.md` §5.14, §10.7.1: the review board's tile is read in the cut header; "Send an agent to these N cuts" is behind the developer setting.
- `spec/SPEC.md` §12.6 Home: no New project and no plan import field.
