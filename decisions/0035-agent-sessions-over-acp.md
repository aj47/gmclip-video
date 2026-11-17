# 0035 — Agent sessions over ACP: live, interactive runs of the user's agents on any target

Date: 2026-10-17 · Decided by: agent (go from AJ on 2026-10-17: "build it") · Status: adopted

## Context
AJ: "i want to greatly improve the agent ui. perhaps we can use acp or something to see active sessions. as well as easily being able to spin up an agent (claude, codex, pi etc) to work on any project or video". The launcher (§10.7, decisions 0007/0012) runs one headless, one-shot pass per click on one cut: no conversation, no steering, no questions back, progress only as "looking at" and a summary, and a flag set plus transcript parser per client. The Agent Client Protocol (JSON-RPC 2.0 on the agent's stdio; registry stable since 2026-03) is spoken by Claude Code (adapter), Codex (adapter), opencode (`opencode acp`) and pi (`pi-acp`).

## Decision
- **Sessions next to the launcher, not instead of it.** The passes, batches and their S7-gated flag sets stay as they are. A *session* is a live ACP connection to the user's agent, started by a human click (R-AGT-3) from the Agents workspace or a "+ Agent" button (cut header, project card, video row, ⌘K), on the **library, a project, one video or one cut**, with follow-up messages, cancel, mode switches, permission answers, resume (agents that can load sessions) and "Undo its edits".
- **Client:** `gmclip_ai::acp` (no new crate: serde_json and std threads). gmclip offers no file system and no terminals; adapters are found by file, never a shell (`claude-agent-acp`/`claude-code-acp`, `codex-acp`, `pi-acp`, `opencode acp`, else `npx <package>`); the environment whitelist drops API keys (plan login), proxies, loader and Node options; the child runs in its own process group and a stop ends its tree. Agent text is canonicalised (R-INJ-6) and shown as text.
- **gmclip is the session's MCP server** (this app's `mcp` shim). A **cut** session registers a launched run (R-AGT-5: credential bound to the cut and the first message, default scopes, `read_media` only after the Owner's sheet) and its core runs are recorded for undo. Other targets connect as the agent's `local:` identity with the default scopes and implicit runs (§11.2).
- **Access:** `gmclip` (default): an empty checked run folder (R-AGT-2 ancestry rule); Claude Code starts with its built-in tools and all setting sources off (adapter `_meta`); any other agent's request to run a non-gmclip tool is refused (read-only tools an agent runs without asking cannot be refused here, and the sheet says so). `full`: the agent as it is, its own tools and settings, in a scratch folder or one the human names; every non-gmclip tool asks the human. gmclip's own tools are approved without asking under both (the holder checks scopes).
- Sessions are kept in `<app_data>/agent-sessions/` when a turn ends; at most 8 live; quitting ends them. UI events on a new `agents` scope (`AgentsEvent`).

## Evidence
`crates/gmclip-ai/src/acp/tests.rs` (a scripted agent: initialize, new with MCP env and `_meta`, streamed updates, a permission round trip, an fs request refused, timeouts, stop ends the tree and fails waiters, adapter lookup). `app/src-tauri/tests/sessions.rs` (full access asks and waits; gmclip access refuses and switches Claude Code's own tools off; follow-ups; persistence; bad starts). `app/ui/tests/e2e/agent-sessions.spec.ts` (the workspace end to end against `fixtures/fake-acp.sh`). The real `claude-code-acp` 0.12.6 spawned with the gmclip-access `_meta` shows `--setting-sources ""` and every built-in tool disallowed (its stderr); no real session was run (no model turn).

## Sections changed
- `spec/SPEC.md` §10.7: a paragraph on agent sessions after the launcher's.

## Amendment 2026-10-18: the model, and Home's sun
AJ: "the agents section on home screen with pause agents is not good … add a big sun like in a project view in the homepage to quickly spawn an agent there. we should be able to select which model to use in the agent too as well as harness".
- **Model.** `session/new` and `session/load` results are read for the agent's models: the `configOptions` select of category `model` (switched with `session/set_config_option`), else the older `models` (`session/set_model`). A session shows them and switches while it runs (`session_model`). An agent lists models only once a session is open, so each harness's last list is kept in `<app_data>/agent-sessions/models.json` and `session_agents` returns it: the dock and the launch sheet offer "Default model" plus that list, remembered per harness, and `session_start` takes `model`, switched right after the session opens (a notice, not a failure, when the agent no longer offers it).
- **Home.** The activity rail (clients, approvals, Pause agents) leaves Home; it stays in the Agents view under "Outside agents and approvals". Home gets the project views' floating sun, bigger, opening on a new session on the library (any project can be picked).

Evidence: `acp/tests.rs` (`models_from_either_shape`; the scripted agent's model option set over `session/set_config_option`), `agent-sessions.spec.ts` ("Home's big sun": the `session/set_model` path, a switch while it runs and a pick before the start).

## Amendment 2026-10-18 (later): access without asking
AJ: "can we make it so we can have full bypass in the agent so we do not have to approve commands" (and "go" after the risk was spelled out). A third access, `bypass` ("Full access, no asking"): `full`'s own tools, settings and folder, but every permission request the agent sends is allowed (its `allow_always`, else `allow_once`) and the session switches to the agent's own no-ask mode when it offers one (`bypassPermissions`, `full-access`), so it mostly stops asking at all. The footage grant still goes through the Owner's sheet. The dock and the launch sheet remember the last access picked; the session header marks it in the danger tone.

Evidence: `agent-sessions.spec.ts` (a bypass session runs the scripted shell request with no question, in `bypassPermissions`; the sheet remembers it).
