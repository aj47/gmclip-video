# 0024 — Alternate openings, hook lines on measured numbers, copy drafts, YouTube chapters

Date: 2026-10-16 · Decided by: agent (go from AJ on 2026-10-16: his "Ideas" picks) · Status: adopted

## Context
AJ picked four ideas on 2026-10-16: three openings for one short, compared side by side; a text hook built on a number the clip measurably shows; title and description drafts per platform from an agent; a long-form's YouTube chapters from its topics. Built by lane K1 with defaults while AJ was away.

## Decision
- **Model epoch 3** (R-MODEL-3; these two kinds are additive like 0011 and 0015, and share epoch 3 with decision 0023's removals, one epoch since the 2026-10-17 merge): `Opening {cut, label, spans[{media, src_in, src_out}], hook_text?, from: moment|suggestion|manual|was_main, from_ref?, origin, at_ms}` and `CopyDraft {cut, platform: youtube|shorts|tiktok|instagram, title, description, state: proposed|accepted|rejected, origin, edited, pass_ref?, author, at_ms}`.
- **Openings are versions of one cut, not copies.** The cut is the main version; its opening is its leading hook parts (role `hook` or `from: hook`, at least the first part). An alternate is computed on demand (`version_doc`: the opening parts swapped for its spans, the first keeping its id and look; never committed), so every edit after the opening is in every version. At most three alternates; spans sit on the cut's frame grid. `opening_choose` (the human only, one undo step) swaps an alternate in and keeps the old opening as a `was_main` alternate. Every export records its version (`opening_sig`), so retention imported onto it is that opening's.
- **Hook lines** (`hook_text_suggest`, read, deterministic): 5–8 words on a measured run (whole seconds under 100 s, else `m:ss`, floored), the count of measured runs, or a number said in the cut (quoted as said). Each carries its `number_provenance` (R-AI-4), recorded on the hook when applied; a spoken duration no measured run agrees with is flagged. No number is invented.
- **Copy drafts** come only through the pass contract `publish_copy@1` (`ai_pass{begin_self}` / `pass_step`; launched runs may call `run_end` only): an agent's draft cites only numbers the cut backs and carries no link. `copy_drafts_add`, `copy_draft_state` and `opening_choose` are not on the agent surface (`AGENT_DENY`). Nothing publishes or approves.
- **Chapters** (`longform_chapters`): on the long-form's output timeline, `00:00 Title` lines (h:mm:ss from an hour), the first at 00:00, each ≥ 10 s (a shorter one joins its shorter neighbour under the longer one's title), `valid`/`why_not` below three. On `cut_get` of a long-form for agents.

## Evidence
- `crates/gmclip-core/tests/{openings,hooktext,copydrafts}.rs`, `tests/topics.rs::chapters_follow_youtube_rules`; `crates/gmclip-mcp/tests/packaging_tools.rs` (scripted agents on the real MCP surface); `app/src-tauri/tests/openings.rs`.
- e2e: `app/ui/tests/e2e/{hooklines,openings,copydrafts,chapters}.spec.ts`.

## Sections changed
- `spec/SPEC.md` unchanged; this record is the addition (§4.3 gains `Opening`, `CopyDraft`; §5.17 chapters rules; §10.3 pass `publish_copy`). Backlog: `plan/m2-backlog.md` §2.17.
