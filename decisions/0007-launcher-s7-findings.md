# 0007 — Launcher flag sets and run directory after the S7 probe

Date: 2026-10-14 · Decided by: agent (defaults per decision 0002; AJ confirms with one word) · Status: adopted

## Context
Lane E built the launcher (`gmclip agent run`, R-AGT-1..5) and ran the S7 probe against the installed clients with a stub model, at zero spend (`spikes/S7/README.md`). Several of SPEC §10.7's unverified assumptions did not hold.

## Decision
- **Claude Code flag set:** add `--verbose`. 2.1.300 refuses `-p --output-format stream-json` without it (S7-C1).
- **Codex flag set:** add the following.
  - `-c mcp_servers.gmclip.env_vars=["GMCLIP_RUN"]`, without which R-AGT-5 cannot work (S7-X2).
  - `-c mcp_servers.gmclip.enabled_tools=[<the run's tools>]` as Codex's per-run allow-list (R-AGT-4).
  - `-c skills.include_instructions=false` (S7-X3).
  - `--disable goals --disable sleep_tool --disable tool_suggest --disable skill_search` (S7-X4).
- **Codex 0.160.x is unsupported.**
  - `$CODEX_HOME/AGENTS.md` reaches the session (S7-X1).
  - Collaboration and `request_user_input` tools stay offered (S7-X5).
  - Per R-AGT-1 the launcher refuses it and `evals/h79/clients.toml` records why. The M1 follow-up gate's Codex run waits for a version or flag that passes S7, or for AJ to accept S7-X1 option (a): a temporary `CODEX_HOME` with an `auth.json` symlink.
- **Run directories:** `$TMPDIR/gmclip-<uid>/agent-runs/<run>/{cwd,config}`, not `<app data>/agent-runs/<run>/`. App data is under `$HOME`, whose `.claude/` and `.codex/` make R-AGT-1's ancestry assertion fail by construction.
- **Env whitelist:** add `CODEX_HOME`, for Codex runs only and only when the parent sets it (§10.7 already says the real `CODEX_HOME` is used for auth).
- **`gmclip-eval` crate (dev only):** may depend on `gmclip-ai` only (`xtask/src/deps.rs`). It reads RenderPlans as plain JSON, so the grader is independent of `gmclip-plan`, the code it grades.
- **R-AGT-2 (TCC disclaim)** is not implemented. It needs `unsafe` FFI outside `gmclip-gpu`. Run records say `tcc_disclaimed: false` until AJ decides where the spawn helper lives.

## Evidence
`spikes/S7/README.md`, `spikes/S7/results/*.json`, `crates/gmclip-eval/tests/s7_lab.rs`.

## Sections changed
- `spec/SPEC.md` §10.7 and `spec/security-requirements.md` R-AGT-1: the flag sets and cwd above. Pending AJ's word, the spec text is left as frozen and this record governs.
