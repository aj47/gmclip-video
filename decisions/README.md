# Decisions

SPEC v5 is frozen (`spec/SPEC.md` §22). Every later change to the spec or its normative companions is recorded here, one file per decision, committed together with the spec edit it describes. Fired tripwires and kill fallbacks are recorded the same way, including the requirements they retag (§19.1).

- File name: `NNNN-<slug>.md`, numbered in order (`0001-…`, `0002-…`). AJ's one-word answers to §21 go in `section-21-answers.md`, the one unnumbered file (recorded during W0).
- Keep it to one screen. Link evidence; don't paste it.
- A decision that adds a requirement names the gate it lands in and what leaves that gate to make room. If nothing leaves, it is a backlog item, not a MUST.

## Template

```markdown
# NNNN — <title>

Date: YYYY-MM-DD · Decided by: AJ | agent (go from AJ on YYYY-MM-DD) · Status: adopted | superseded by NNNN

## Context
What prompted this: the tripwire, measurement, review or question. One paragraph.

## Decision
What changes, in the spec's own terms (requirement ids, gate tags, numbers).

## Evidence
Links to the measurement, spike README, eval results, PR or review that justify it.

## Sections changed
- `spec/SPEC.md` §x.y: <old → new, one line each>
- companions touched, if any
```
