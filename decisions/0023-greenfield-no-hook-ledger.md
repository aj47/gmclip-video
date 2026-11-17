# 0023 — Greenfield: no Hook Ledger import or compatibility

Date: 2026-10-16 · Decided by: AJ (owner) · Status: adopted

## Decision
AJ, verbatim: "gmclip need not be backwards compat with hook ledger. it is greenfield and new users wont even know what hook ledger is so we dont need to keep any references at all".

- The Hook Ledger importer and everything that existed only for it are removed: `gmclip import-hl`, `gmclip import-rt` and the RT → RenderPlan converter, `gmclip profile import-hl`, the app's import, re-import (A.08) and "Set up like my Hook Ledger" (U.13, A.07), the MVP report that read the ledger's files, imported delivery (🎙) scores, and the RenderPlan input roles `rt_*`. SPEC R-MIG-1 and FZ17 are dropped.
- The model drops `Moment.delivery`, the `mined` and `import` moment origins and the `import` payoff source; the caption grouping `rt_story_v5` is `story_v5` and the built-in preset `rt_story` is `story`. `MODEL_EPOCH` 3, the same epoch that adds decision 0024's `Opening` and `CopyDraft` (one epoch since the 2026-10-17 merge); nothing older was deployed, so there are no migrations and no shims.
- Generic features that came from it stay under neutral names: moments and states, stories and suggestions, captions (`story_v5`), layouts, kits, profiles. Clippy (hiclippy.com publishing, Send to Clippy, the Clippy calendar) stays: it is a separate product.
- What the lab tests took from the importer and the converter was frozen once into fixtures gmclip wrote (`lab/fixtures/`: six stream projects, the golden-11 RenderPlans with `render_sig` unchanged, the density references, AJ's profile content). Lab media is read through one folder of symlinks, `GMCLIP_LAB_MEDIA` (default `~/Development/gmclip-lab-media`, made by `tools/lab_media.sh`).
- Dated records (`decisions/`, `critique/`, `milestones/`, `evals/results/`) stay as written.

## Sections changed
- `spec/SPEC.md` §18 R-MIG-1, §12.5 "Set up like my Hook Ledger", §17.1 (f) migration set: dropped; the rest neutral wording.
- `spec/security-requirements.md` FZ17: dropped. `spec/renderplan-v0.md` and `schemas/renderplan@{0,1}.json`: no `rt_*` input roles.
- `plan/m2-backlog.md` U.13, A.07, A.08 and `plan/m1-backlog-draft.md` A32, A33: dropped.
