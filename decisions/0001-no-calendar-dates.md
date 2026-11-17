# 0001 — No calendar dates; gates are sequence- and evidence-driven

Date: 2026-10-14 · Decided by: AJ · Status: adopted

## Context
AJ, 2026-10-14: *"remove dates — we will be working 24/7 and likely finish faster; it's Oct 14 now so dates make no sense."* SPEC v5 and its companions scheduled every gate, tripwire, kill criterion, review and to-do against a calendar sized for weekday work. With agents running around the clock, those dates are wrong, and a date-based slip rule would fire on the clock instead of on evidence.

## Decision
- The plan runs on order and evidence, not on the calendar. Gates W0 → M0 → … → M7 run in order, overlapping where dependencies allow (the "Order / depends on" column in SPEC §19.1). A gate is done when its exit criteria pass. Work runs continuously with agents.
- No calendar dates, no durations mapped to dates, no Start/End columns. Agent-day estimates stay as sizing.
- Kill criteria (§20) and tripwires (§19.3, `plan/m0-backlog.md` §4.4, `plan/m1-backlog-draft.md` §4.4) fire on **evidence at a named checkpoint**, for example "at M0 exit", "after the first A/B round", "once S5 reports", "after 3 iteration cycles".
- The slip rule `end + 0.5 × (end − start)` is replaced by: **if a gate's exit fails twice in a row after fixes, run its fallback.**
- To-dos are gate-relative ("during W0", "before M0 exit", "before M3 entry"). §22's reviews are tied to gates: M1 backlog final before M1 starts; the cut-only review, then the M2 backlog, before M2 starts.
- AJ's §21 answers move from `decisions/2026-10-16.md` to `decisions/section-21-answers.md`.
- Kept: legal and external dates that are facts rather than plan (EU AI Act and CRA dates, MCP protocol revision ids), dates of measurements and work already done, the spec's version date, dates inside quoted evidence and file paths.

## Evidence
AJ's instruction above, given in session on 2026-10-14. No measurement is needed: the change removes calendar assumptions and keeps every requirement, exit criterion and estimate.

## Sections changed
- `spec/SPEC.md` header (companion and parity rows); §0 D3, D11; §2 platform table (Windows, mobile); §6.2 export decode; §6.5 RenderPlan freeze; R-ML-2; §11.2.1 vocabulary freeze; §13 mac-ref; §15.0 counsel; §15.2 beta entitlement; §15.7 key backup; §16 external review; §17.1 h79 targets; §17.1.1 grader freeze; §19.1 gate rule and table (Start/End → Order / depends on; M2 judged on the 10th short; M3 judged at exit); §19.2 Lane S, status cadence, WhatsApp; §19.3 S1/S3/S5 and tripwires; §20 K2–K8, K10–K12; §21 Q3, Q4, Q6, Q7, Q12, Q18, Q19 and the answers file; §21.1 "calendar and costs" → "to-dos and costs" (By → When); §22 reviews.
- `spec/OVERVIEW.md`: "What you get, and when" → "in order"; to-dos; one-word list; risks become kill checkpoints.
- `plan/m0-backlog.md`: W0 heading; W0-07/08/09/10/13, B-06, B-17, H-03, H-05b, H-10, H-14a/b, H-FB2, A-10…16, §3.5; §4 rewritten as dependency order, path, verdict and evidence tripwires.
- `plan/m1-backlog-draft.md`: gate line; F13, F18, A08, A24, Lane H, H01, E02, AJ table, effort; §4 rewritten as dependency order, path, verdict and cut.
- `STATUS.md`: header, gate table, to-dos, tripwires (date-bearing lines only).
- `spec/security-requirements.md` §3.2 fuzz rule, pre-beta checklist external review; `spec/renderplan-v0.md` freeze line; `decisions/README.md` answers file.
- `spec/threat-model.md`: no plan dates; unchanged.

## Remaining grep hits, all kept on purpose
Command: `grep -nE '20(26|27)-|\b(0?[1-9]|1[0-2])-[0-3][0-9]\b|Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec'` over the ten files.
- **Spec version and history date**: SPEC header `v5 (2026-10-14)`; OVERVIEW "frozen 2026-10-14".
- **Quoted evidence**: SPEC "AJ's brief, 2026-10-14"; this record's Context (AJ's words, "Oct 14").
- **Dates of work or measurements already done**: m1-backlog "written 2026-10-14"; renderplan §2.7 "Validation record (2026-10-14)"; STATUS "Today: 2026-10-14" and "W0-01 skeleton landed 2026-10-14"; this record's Date line and Evidence.
- **File and directory names**: `research/usage-2026-10.md` (SPEC ×5, m0-backlog), `reels-2026-10-05/`, `h79-v3-2026-10-04/` (SPEC, renderplan ×4, m1-backlog ×2), `hires-1440-2026-10-03/` (m1-backlog), OVERVIEW "reels-2026-10-05 batch".
- **External facts**: MCP protocol revisions `2026-07-28`, `2025-06-18`, `2025-11-25` (SPEC §11 rmcp line and §17 test table; m1-backlog A20 row); EU CRA full obligations "before 2027-12-11" (SPEC §15.6).
- **This record itself**: the old answers-file name `decisions/2026-10-16.md` (cited once in Decision) and the grep pattern above.
- **Not dates** (month abbreviations inside words): "Decision(s)", "Decode", "VTDecoderXPCService", "Marco", "Mark", "Marketing", "Markdown", in SPEC, OVERVIEW, STATUS, m1-backlog, security-requirements, threat-model, decisions/README.
