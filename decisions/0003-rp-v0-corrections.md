# 0003 — RenderPlan v0 §2.4: what RT actually shipped (ASS frames, centiseconds, stills)

Date: 2026-10-14 · Decided by: agent (go per decision 0002; AJ confirms in one word, STATUS "Needs AJ") · Status: adopted

## Context
B-13 (RT → RenderPlan converter) and its adversarial review re-derived every golden cut from RT's own files and found three places where `spec/renderplan-v0.md` §2.4 describes RT differently from what it rendered. The converter must reproduce what shipped (§2.4: "imported times are never re-derived"), so the spec text follows the evidence.

## Decision
1. **ASS display rule.** An ASS event `[start_cs, end_cs)` shows on output frames `[F(start_cs), F(end_cs))`, where `F(cs)` is the first frame `f` with `(long long)(f·(1.0/fps)·1000) ≥ 10·cs` — ffmpeg's `ass` filter truncates its millisecond. This replaces `ceil(0.3·cs)`, which is one frame early on 20 event edges of the golden cuts (none in §2.3's 397 frames).
2. **RT's centiseconds.** The story_reel that wrote every golden v5 ASS (hook-ledger 8b47940b) used `round(f·100/30)`, not `floor`; the later working copy uses `floor`. `--cues build` uses the rule that reproduces the cut's own ASS and says which in the import report.
3. **Stills.** `cuts[].still` / `segments[].screen_still` freeze the screen layer only; detail layers stay live, as `render_reel_rt.py` draws them. (Holds still cover screen + detail.)

No requirement is added; §2.1 (normative rules) and §2.2 (schema) are unchanged.

## Evidence
- Converter: `crates/gmclip-render/src/import_rt/rounding.rs` (`ass_frame`, `libass_ms`, `CsRule`), commit f174ee1; reviewer reproduction with RT's own `static_ffmpeg` N-105504 (`settb=1/30,setpts=N,ass=`): an event at 0:00:03.70 first shows on frame 112, not 111.
- B-14 (`crates/gmclip-render/tests/import_rt_golden.rs`, commit f53b763): h79's 0:00:13.30 cue shows from frame 400; build mode with `round` reproduces the hook, pill and clock events of all 10 v5 reels; `git show 8b47940b:production/reeltools/story_reel.py` line 218.
- RT renderer: `render_reel_rt.py` applies the still's `loop` to the screen branch `[s{i}]` only.

## Sections changed
- `spec/renderplan-v0.md` header: amendment note.
- §2.3 "Text event frames": formula → the §2.4 display rule (values unchanged).
- §2.4 "Rounding" / ASS event times: `floor` → `round` (with the later `floor`), `ceil(0.3·cs)` → `F(cs)`.
- §2.4 plan.json table: `hook.until`, `hook.until_s` → `F(cs(e))`; `cuts[].still` → screen layer only.
- §2.4 render-spec table: `segments[].screen_still` → screen layer only.
- §2.4 ASS table: `Dialogue` row → `F(cs)`.
