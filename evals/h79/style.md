# Agent notes

`CLAUDE.md` is the full agent guide (ledger, editing pass, publishing, manager). Read it first;
this file adds AJ's standing preferences for the Reels and Shorts we cut from the ledger.

## Reel / Short style: payoff-first story (AJ, ACTIVE since 2026-10-04)

Origin: AJ's brief while remaking a staged hook (AJ found a ~20 s cut "a bit too jumpy, and hard to follow… we can make it go for ~1 minute" and approved a ~60 s cut). [redacted for an eval: the worked example, its files and build script]

1. **Open on the result.** Frame 0 is the finished thing, for example the generated clip
   mid-line. No intro, logo or greeting.
   - A big text hook shows from frame 0: 5–8 words with a specific number, in its own band
     (y≈220–400), never over captions.
   - It stays up through the spoken claim (~4–6 s) and compresses the voice line instead of
     repeating it. [redacted for an eval: the example is the eval's own hook and voice line]
2. **Then the story, in order.** Prompt → run → wait → result → twist → reaction → restated
   result.
   - Each beat is a whole sentence in a 3–9 s take. Jump-cut only dead air and retakes.
   - A cut every 1–3 s is too jumpy for AJ.
   - About 60 s when the moment is a real story; 15–30 s only for a single proof.
3. **One steady layout.** Screen panel at the top (y≈250–1010, with the app context visible),
   captions at y≈1065, facecam `[160,1120,760,520]` from the stream's cam inset `[2060,634,496,340]`.
   - Per section, change only the panel's crop or inset (prompt line, Run button and progress,
     taskbar clock, player).
   - Go full-screen on the subject only for payoff beats and when the source camera drops out
     (color bars or frozen frames). The camera layer is then hidden with a tiny dummy crop.
4. **Captions on.** 1–3 words, Neometric Heavy 86, outline 12, shadow 7, white, with key words in
   yellow `&H00D4FF&`.
   - Group by phrase: never split a number from its unit or a product name, and don't end a cue on
     a weak word.
   - Drop um/uh from captions only.
   - Skip captions where on-video text already carries the words.
5. **Pills orient the viewer** (Neometric Heavy 50, boxed, top-left at (48,262)). First a context
   label, then a measured run clock that counts while the job runs and ends on the measured total.
   [redacted for an eval: the example pill and clock texts]
6. **Numbers come from the footage.**
   - Measure a run time from the app's own state change (for example a job queue going 0→1 active) to the
     first frame the result is visible. [redacted for an eval: the measured example]
   - Drop spoken lines the screen contradicts. [redacted for an eval: the examples]
7. **Cut points sit on measured energy minima.** Use parakeet word times plus 10–40 ms RMS scans
   of `cache/preview/<vid>.proxy.mp4` audio.
   - Keep a short natural "uh" when removing it would clip the next word.
   - Re-transcribe the encoded file to prove every line is intact.
8. **Source quality.** Render from 1440p source windows, not `cache/` proxies. See
   `production/hires-1440-2026-10-03/` for
   cutting a new window: picture from the 1440p MKV at frame offset −1642, audio from the proxy,
   video timescale 1/15360, alignment checked at −1/0/+1.
9. **Design for Instagram's overlays.** Keep the hook, captions and face out of the top ~220 px,
   the bottom ~360 px, and the right ~120 px between y 1000 and 1560.
10. **Loop.** The last frame matches the opening layout.
    - Original audio only.
    - Lift quiet on-screen audio (e.g. a generated voice) with `program_gain_envelopes`.
      [redacted for an eval: the gain used on the eval cut]

## Tooling: the reel builder (added 2026-10-05)

Use this to cut a story reel from a plan instead of writing a new build script per hook.

- `story_reel.py PLAN.json` handles layout → render → transcription of the render's own audio →
  1–3 word captions → final render → QA (contact sheet with the IG overlay zones, loudness,
  round-trip transcript). The plan holds cuts in native seconds, named layouts, pills, hook text,
  `keywords`, `keep` pairs and `cap_sub`/`fix` spelling repairs. A cut can carry
  `hold_at: <native s>`, which keeps the screen and insets on one earlier frame while camera and
  audio stay live; use it when the screen changes to something unrelated mid-sentence.
  Worked plans: `production/reels-2026-10-05/<hook>/plan.json`.
- `rt.py` is per-stream source config (`SOURCES`: 1440p MKV plus frame offset, or a native
  source). It also has `window()` (verified 1440p source windows, cached), `stream_words`/`words`
  (parakeet), `energy` (RMS scans for cut points), `sheet`/`frame` (labelled contact sheets),
  and `camera_state` (black or frozen camera spans).
- `render_reel_rt.py` is the staged-reels renderer plus `screen_still` and `screen_hold`.
- `review_page.py <batch dir>` writes a phone review page (`index.html`) from each plan, its build
  log and `review-notes.json`. The 8878 server answers byte ranges, so it plays on iOS.
- `register_finals.py <batch dir> --apply` puts the renders on the dashboard's 🎬 Rendered list:
  the plan becomes the story record, the run's manifest is synced to it, the reel becomes
  `edit-runs/<hook>/vertical/renders/final.mp4` (the old one moves to `superseded/`), and it is
  registered through `/api/edit_station/final`, so the final is current rather than stale.
  An archived hook is staged again. Use `--spec <render-spec.json> --mp4 <file>` for a one-off
  build. Without `--apply` it only prints. A 🎬 Render final in the station
  afterwards replaces the reel with a station render.
- Captions come from the rendered cut, not the stream transcript. Stream word times drift by up
  to ~0.3 s, which drops or adds edge words. Spelling is fixed per plan with `cap_sub`/`fix`;
  always read the cue list in `build.log` before calling a cut done.
- Hook text and captions shrink themselves to fit 1000 px (libass draws this font at 0.838× the
  Pillow width). Keep hook lines to about 22 characters anyway.
- The hook and the pills share one band, so a pill waits for the hook to clear. When the hook
  outlasts the opening cut, keep the opening's lower panel (the `*_hook` layout) until the hook
  ends, and split the next cut at a word boundary there (h53).
- Captions are 1–3 words. A 4-word cue is allowed only when fast speech would otherwise flash a
  cue for under 0.3 s. Nothing shorter is left on screen.
- **Look at what is behind the face cam.** On `loc_086da141` AJ is keyed over the desktop, so a
  window opened behind him shows around his head. h03's last 5 s had a private Discord DM there;
  the cut now ends before it. Before calling a cut done, scan the camera crop as well as the
  screen.
- When the screen switches to something unrelated mid-sentence (another app, a scroll past other
  posts), hold the screen on the last relevant frame with `hold_at` (h25, h03, h41).

## Keeping these preferences current

- When AJ reacts to a render, add a dated line here with his exact words and the render path.
  Mirror it in the shared rulebook, the 2026-10-04 entry of
  `~/.agents/skills/vertical-video-editing/references/aj-current-style-directives.md`, which all
  agents read for vertical edits.
- Mark replaced rules as superseded instead of deleting them.
- After a cut posts, measure retention with creator-content-ops (`retention_measure.py`) and
  update theory `reel-story-60s-payoff-first`. Change a rule only on AJ's direction or on
  measured evidence.

### Feedback log

- 2026-10-04: a ~20 s cut: "its a bit too jumpy, and hard to follow. we can make it go for ~ 1 minute".
- 2026-10-04: a ~1 minute cut: "ok this is great".
  [redacted for an eval: which cut and render these were]
