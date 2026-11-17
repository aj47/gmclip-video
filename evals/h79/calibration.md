# h79 grader calibration

SPEC §17.1.1 says that when the golden fails a check, the **check's spec value** changes, never the threshold. This file records every such change, every guard added after review, and every place where the grader had to choose how to compute a check.

- Thresholds are in `truth.json` → `thresholds` and equal the SPEC table.
- Grader: `crates/gmclip-eval/src/h79/grade.rs`.
- Self-tests: `cargo xt eval h79 selftest`.
- **All of this waits for AJ's approval at the grader freeze (AJ-03).** Until then every result is marked `pre_approval` and no batch is gate-eligible.

## The golden against SPEC's "v4" column

| check | SPEC v4 | grader on v4 | |
|---|---|---|---|
| G2 | 60.23 | 60.23 s | = |
| G4 | 93% | 0.931 | = |
| G5 | 3.65 s, 14 parts | median 3.65 s, min 0.57 s, 14 parts | = |
| G9 | 7 words | 7 words, 2 lines (15/14 chars), ends 5.90 s, ink y 228–379 | = |
| G10 | 67 | "67" against a 67.434 s measurement | = |
| G12 | 7 layouts | 7 layouts, cam on target 100 %, screen band 100 % | after change 1 |
| G13 | 1.0 | 1.000 (85 content words) | = |

**Verdict on the golden: `provisional`.** There is no failure. G8(b) stays `pending` until the runner can run the reference `story_v5` (see "Pending" below). A provisional run never counts as a pass.

## Spec-value changes

1. **G12 screen band: full-subject payoff beats are exempt, and capped.**
   - As written ("screen place inside y ∈ [250, 1010] for ≥ 80% of output"), v4 scores 77.1 %. The banana parts (01 hook, 11 payoff, 14 payoff; 413 of 1807 frames) place the film full-height at y 410–1364.
   - AGENTS.md rule 3 allows exactly this: "Go full-screen on the subject only for payoff beats and when the source camera drops out… the camera layer is then hidden".
   - New spec value: a part whose cam layer is hidden **and** whose role is `hook` or `payoff` is a full-subject part and is exempt from the screen band. Every other part is held to the band.
   - Guard: full-subject parts may cover at most **35 %** of the output. v4: 22.9 %. Without the cap, a run could hide the cam everywhere and escape both clauses.
2. **G8(d) coverage counts what `story_v5` captions.**
   - As written, v4 scores 124/133 = 0.932. With the filler and hyphen rules but no time slack it scores 122/130 = 0.938.
   - Changes:
     - Fillers {uh, um, uhh, umm, er, hmm} are not counted; `story_v5` drops them by definition (SPEC §5.10).
     - Hyphenated words match per part (`five-second`).
     - A cue covers a word whose midpoint lies within **±0.5 s** of the cue. Round-trip ASR times drift against cue timing; AGENTS.md notes stream word times drift by up to ~0.3 s.
   - v4 after all three: 126/130 = 0.969 (threshold 0.95). The remaining misses are ASR spellings (`12:15.` against "twelve fifteen", `let'`, `lets's`) and the sentence-final `voice.`.
3. **G8 guard: captions cannot be switched off with no-caption ranges.** No-caption ranges may cover at most **15 %** of the output. v4: 120/1807 = 6.6 % (the generated voice, which carries its own on-film text).
4. **Fixture grep (task hygiene): a number matches as its own number.**
   - "A grep of the fixture for '67'" literally matches the moment id `loc_4fadb756_h67`.
   - New spec value: a needle that starts with a digit matches only where it is not glued to a preceding letter, digit, `_`, `.` or `:`.
     - An integer needle also must not be followed by a digit. `67`, `67.`, `67.4` and `67s` count; `h67`, `167`, `670`, `0.67` and the timestamp `00:41:07` do not.
     - A decimal needle also matches its longer spellings: `2610.8` matches `2610.833`.
   - `1:07` (the golden's clock) was added to the forbidden list.
   - The other strings match anywhere, case-insensitively (`fixture.rs`).
5. **G6 counts discontinuous joins.** `verify{joins}` (R-EXP-6) checks only discontinuous joins (RenderPlan rule 7), so the export's verify must cover exactly the plan's discontinuous joins. v4 has 13 joins, 3 of them continuous, so 10 are checked.

## How each check is computed (choices the SPEC leaves open)

- **One export for all export checks.** G1 selects the run's single non-blocked export of the framing (made by this run). G6, G8(b), G11's lint check and G14 read that same record. With zero or several candidates, those checks fail.
- **G0. A run passes only if every item below holds:**
  - The run record names a known harness. The transcript's sha256 equals the record's, and the record's run id and cut equal the project's.
  - The transcript has no non-JSON line and ends with the client's closing line.
  - Every call is a recognised `mcp__gmclip__*` call. For Claude Code, any `tool_use`-shaped block (`server_tool_use`, …) counts as a call. For Codex, every item kind other than messages and reasoning counts as a call. Unknown *top-level* event types carry no calls and are ignored.
  - Path arguments sit lexically inside the fixture root.
    - Scanned: values (trimmed), object keys, and JSON nested inside strings.
    - A path is anything absolute, home-relative, `file:`, a drive, containing `..` or `\`, or any relative string with `/` that is not a `gmclip://` resource or an `n/m` ratio.
    - `/private/var` and `/var` are the same place.
  - The launcher's own counts of non-gmclip and out-of-policy calls are 0.
  - The run did not time out.
- **G1.** It also checks that the record's `plan_sha256` is the bundle's `plan.json`, that `verify{export}` ran on the export's own `render_sig`, and that the plan is 1080×1920 at 30/1.
- **G3, G4, G13 (master clock).** Master time comes from the part's audio (`first_sample / rate`) when its source has the truth's master-audio hash. The proxy runs on the stream clock. Otherwise it comes from `native.frames`. If both are present and disagree by more than 0.1 s, G3 fails. The part role comes from the project facts, because RenderPlan v0 parts carry no role.
- **G6.** It reads `verify{joins}` from the export record. The verify must be green, bound to the export's `render_sig`, and cover the discontinuous joins.
  - The grader does not re-detect clipped words. The stream ASR's sentence-final word ends run long (`minutes.` 2718.9–2720.7 s), so a word-interval test would flag 26 of the golden's 28 edges.
  - Mutant 5's verify result is the one `verify{joins}` would report for that render (`"synthetic": true`). It is replaced by a rendered mutant once `gmclip render` and `verify{joins}` exist (TODO(H-10/A25)).
- **G7.** On a run, every plan part needs anchor facts for both edges, and every non-caption overlay needs a time provenance. On a reference bundle with no edge facts the check is `n/a`.
  - **Agent-authored** means any author other than `human` or `import`, including `launched:<run>` and `local:<client>`. G7, G11 and G14 all use this rule.
- **G9.**
  - The ink box is computed from the font's glyph outline bounds (`truth/font-neometric-heavy.json`, generated with fontTools by `make_truth.py`) and RenderPlan rule 6's line layout, without kerning.
  - The event's style must name the font with the truth's font hash, or the ink cannot be measured and the check fails.
  - A hook whose fill alpha is below 50 % fails as invisible.
  - The IG zones are checked on the hook and every caption.
- **G10.**
  - The measurement needs ≥ 2 **distinct** evidence frames from the stream's own sources, identified by hash. Each frame maps to the master clock with the truth's offsets (picture −1642, audio 0). One frame must land within tolerance of the start and one of the stop.
  - The golden's evidence comes from v3's `run_clock` record:
    - start: ComfyUI queue 0→1 at frame 1375 of the 2565 s window = native 78325 = MKV 76683;
    - stop: banana first drawn at frame 98 of the 2675 s window = native 80348 = MKV 78706;
    - result: 2610.833 → 2678.267 s.
  - Agreement follows SPEC §5.13: `m:ss` floored, integers ±1, decimals ±½ of the last digit.
- **G13.** LCS of normalised content words, with a fixed stop list (`grade.rs` `STOP`).
- **G14.** Loudness must be `static` at −16 LUFS ± 0.5 and true peak ≤ −1.5 dBTP. Every agent-authored gain segment must cite a `measurement_ref`.

## Pending (MUST checks the pipeline cannot compute yet)

- **G8(b):** the export record's cues must equal a reference `story_v5` run over the record's heard words and CaptionDoc.
  - The cue builder (B-08) is on main in `gmclip-render`. The grader does not link it, to stay independent of the code under test. The runner will compute the reference and write `cues_equal_reference` into the bundle (TODO, with A2's export record).
  - Until then every run is at best `provisional`.

## Scoring (pre-registered, `score.rs`)

- **Result files.** Results are read only from `<client>-<n>.json` and `<client>-<n>.retry<k>.json`, and each name must match the file's content. Any other `.json` makes the batch ineligible.
- **Retries.** A retry counts only after an `infra_failure`, and a batch allows at most 2 infra failures.
- **Outcomes.** The outcome is re-derived from each result's `failed` and `pending` lists; a contradicting `outcome` field makes the batch ineligible.
- **Failed runs.** A run that left no gradable bundle is written as a failed run. It is never missing, so it cannot be rerun for free.
- **Pinned inputs.** Each result carries the sha256 of truth.json, of the grader source and of the fixture manifest. They must equal the registration's `frozen` hashes, which are filled at the freeze. Before then, nothing is eligible.
- **Eligibility per client.** The M1 core exit needs Claude Code only. Codex is the follow-up exit and is unsupported on 0.160.
- **Batch directories.** The default is `evals/results/h79/<UTC date-time>/`, one per invocation. A batch is never rewritten.

## Self-test results (`cargo xt eval h79 selftest`)

- **(1) The golden:** no failure; only G8 is pending.
- **(2) v3** (22.5 s, 11 parts): fails G2 and G5. It is built by `make_v3_plan.py`, because the B-13 converter refuses v3's `run_clock` key.
- **(3) The spec's eight mutants each fail exactly their check:**
  - G3: part 1 moved to the prompt;
  - G7: a raw anchor;
  - G10: "60" shown with the 67.4 s measurement;
  - G9: the hook at y 600;
  - G6: an 80 ms clip into "minutes";
  - G8: a cue merged to 5 words;
  - G12: the cam moved 200 px;
  - G0: a Bash call.
- **A ninth mutant** (an agent-authored pill whose number claims human provenance) fails exactly G11.
- **(4) The oracle:** approximated by the golden as a gmclip-only run with a bound run record (G0 passes, no failure). The real oracle (E03) needs A2's tools.
- **Scoring:** five pre-registered runs were replayed, graded and scored, and a batch with no bundles was scored as five failed runs (`run.rs` test).

## Known stand-ins (tracked, not hidden)

- The fixture is JSON inputs, not a project DB.
- `active_asr` is the reference stream ASR (by hash), not gmclip ASR.
- The h79 moment carries `payoff_hint` but no `payoff` region.
- The style redactions keep AJ's own words but not which cut they were about. AJ approves the diff in `style.redactions.json`.
- AGENTS.md rule 3's cam inset and rule 8's −1642 offset stay in `style.md`. They are AJ's standing layout for this stream and are also in `layout.json`/`sources.json`.
