# Agent guidance

## Prefer gmclip's MCP for editing projects

For editing a gmclip project, use gmclip's built-in MCP interface (or the in-app agent when that is the available, authorized route) as the default. MCP is the lower-overhead, more precise route: it edits the semantic model directly, supports undoable transactions, and exposes structured state for verification. It is also a visual-review surface: use MCP's rendered-frame inspection (`see`) to inspect the current or proposed composition, compare relevant moments, and make framing decisions from actual images—not guessed coordinates or descriptions.

- Identify and open the intended project explicitly; do not assume the currently visible project is the requested one.
- Discover commands and schemas through MCP (`commands_search`, `command_describe`, then the appropriate task tools / `command_run`). Follow command base/revision requirements and verify edits with structured reads, lint, rendered-frame inspection, and preview results.
- For visual choices, inspect representative frames with `see`, make semantic crop/place/layout/zoom/camera edits through MCP, then inspect the affected frames again. Repeat until the composition is right. Use the rendered evidence to center the subject and keep important screen content unobstructed.
- Treat MCP as the full-feature agent interface for all agent-reachable project editing and visual review. If an editing capability used in the UI lacks an equivalent agent-reachable MCP operation, treat that as a product parity gap: discover/report the missing capability rather than quietly switching to GUI automation. Owner-only and explicitly human-only actions remain governed by gmclip's security contract; do not bypass those boundaries.
- Keep edits scoped to the user's request. Do not export, publish, or overwrite source media unless explicitly asked.
- If MCP connection/authentication fails, diagnose the supported gmclip MCP connection or ask for help; do not work around authentication or silently switch to UI automation.

## GUI interaction

GUI is not the fallback for visual judgment: MCP can return rendered images via `see`, and visual decisions should be made from those images. Use GUI only when the user explicitly requests GUI interaction or when a documented MCP gap makes a particular task impossible and that gap is reported. Never claim full parity for human-only gesture previews or Owner-protected actions; follow the authorization model rather than bypassing it.
