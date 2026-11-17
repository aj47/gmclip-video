# `story_v5` — constants and pin

The caption cue builder `crates/gmclip-render/src/captions/story_v5.rs` is an exact port of
the reference reel builder's `story_reel.py` (`layout()` spans, `native_of`, `span_at`,
`caption_cues`, `write_ass`, `fit_size`). SPEC §5.10 names it; R-CAP-1 [M0] holds it to the
10 golden `captions-v5.ass` files.

## Pin

| | |
|---|---|
| reference repo commit | `950790b0ed69953236abc91f9dd9e0223d9e537e` (HEAD on 2026-10-14) |
| `story_reel.py` at that commit | git blob `a7fdf5bfe094857ada697d772e25d9542af5ee71`, sha256 `a23ccc6b81ad6e7198917187c32a26d4c4219b41bc829afb6174ac740a10e198`; unchanged since `8b47940b` (2026-10-05, the reel style guide commit) |
| Font | `Neometric Heavy (Bold).otf`, sha256 `9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e` (local only, never committed) |
| Pillow measured with | Pillow 11.3.0, FreeType 2.13.3, libraqm 0.10.1 (raqm layout), Python 3.11.6 (pyenv) |

**The committed revision reproduces the goldens.** The goldens' `captions-v5.ass` were written
at 01:52–02:02 on 2026-10-05, about 6 h before commit `8b47940b`, so history alone does not say
which code made them; the evidence is that the committed code rebuilds all 10 byte for byte.
AJ's working tree has a later, uncommitted `story_reel.py` (sha256 `22a0c234…bf43e`, mtime
2026-10-05 18:41) that (1) clamps a cue's start/end to the jump cut it would cross, (2) stops
the +15-frame hold at the next jump cut, and (3) writes ASS times with `floor` instead of
`round` (plus ASR/render locks). Those would change the goldens, so the port does **not**
include (1)–(3). SPEC §5.10's "never past the next cue or a jump" and
§5.10 "Cue building … clamp to the enclosing continuous stretch" describe that later
revision; adopting it would change cue timing on the goldens and needs a decision record.

Golden inputs and outputs (sha256, first 16 hex; `production/reels-2026-10-05/<hook>/`):

| reel | plan.json | qa/heard.json | captions-v5.ass |
|---|---|---|---|
| loc_086da141_h03 | c6cc72140e777f59 | f95c4292ee3f1687 | f5960ffdf1f07630 |
| loc_086da141_h25 | 2a9d65678ce511d8 | b4888d47fcbc7ebb | 025bbeeeefc17b6e |
| loc_086da141_h47 | 6951bdfdceef24f2 | fe02ddec8d433494 | 21a165c0d35079e5 |
| loc_4fadb756_h34 | 91229abb2f1b48d8 | 5837d1611219acc6 | dc1f712b3f3d7741 |
| loc_4fadb756_h37 | 83bbbedbb6e167ca | 8e9b74064a045a46 | 89b9f50f003fe2a4 |
| loc_4fadb756_h48 | 08bdc958b781889d | 734a152a90ad353c | a716b7f186c88094 |
| loc_bd89f38b_h41 | 3dad5ba2dc05995a | 027d144ae2ba615c | 121583493a4c672b |
| loc_bd89f38b_h43 | 3d5689633cbbbf02 | 5bb07da96921c56b | 01bec88dcc263bd3 |
| loc_bd89f38b_h53 | 0a420eabdb9e0785 | 53a1d86647410d71 | f01ad1ef5915509c |
| loc_bd89f38b_h88 | afa943e07e1abb21 | b06e8ae0bcdc3d80 | 548496ebe70f3964 |

## Input contract (where the port and Python part ways on purpose)

R-CAP-2 says a caption build never fails an export, so the port returns `CueError` where
Python would loop for hours, overflow or raise; the export then goes out without captions.
- Every cut, word, `hook.until_s` and `clock_from` time must be finite and within a day; the
  cut total must be 1 frame to 4 h (`MAX_FRAMES`). `hook_size` must be an integer 1..=1000.
- `plan.json` is parsed strictly: `pills` values, `caption_y` and `hook.y` numbers, `keep`
  pairs must have RT's shapes even when unused (Python only reads what it touches).
- An empty `cap_sub` source is skipped (Python loops forever on it).
- Not ported (parakeet never emits these): Python's `\w` vs Rust `is_alphanumeric` on symbols
  such as `ⓐ`; `label: null` (Python prints `None`, the port `GENERATING`); `until_s: null`
  (Python raises, the port falls back to `until`); bidi controls such as U+202E in fit texts
  (Pillow/raqm reorders before kerning).
- Ported: Python's `$` before a trailing newline, `str.split()` on `\x1c`–`\x1f`, `//` and `%`
  flooring in ASS times, `round` ties-to-even, `int` truncation, `fix` dict order.

## Constants (all from `story_reel.py`)

| name | value | where it bites |
|---|---|---|
| `FPS` | 30 | every frame/second conversion |
| `FILLER` | `uh, um, uhh, umm, er, hmm` | dropped from captions (by `norm`) |
| `WEAK` | `by in to that the a on my and of an if it is with for at or so but` (20) | +3 when a cue ends on one (not at a sentence end) |
| `KEEP_BASE` | 25 pairs: two minutes · less than · more than · under ten · ten minutes · five-second video · Windows PC · blown away · much better · a banana · my local · local Windows · let's see · I'm a · what's the · the clock · start the · per cent · Comfy UI · MiniMax H3 · open weights · text to · to video · on my · in under | +5 to split a pair; plans add `keep` |
| `COL` | `{Y}` → `{\c&H00D4FF&}`, `{G}` → `{\c&H84DC3D&}`, `{W}` → `{\c&HFFFFFF&}` | markup, replaced in that order |
| `LIBASS_PER_PILLOW` | 0.838 | fit width = Pillow `getlength` × 0.838 |
| `norm(w)` | lower-case, keep `[a-z0-9]` | every word comparison |
| sentence break | word ends `.?!`, or gap to the previous word > 0.9 s | runs |
| cue DP cost | 1 per cue; +2 for a 4-word cue (only if ≤ 18 chars and ≤ 0.9 s, else not allowed; > 4 words never, except one cap_sub word); +12·(0.3 − dur) when the cue would show < 0.3 s (dur to the next word's start, or last word end + 0.4 s at the run end); +3 weak last word; +1.5 single word; +5 split keep pair; +5 split after a number (`\d+%?` on the norm); +2 when a word before the last ends with a comma | split choice (strict `<`, k = 1…4) |
| cue frames | `t0 = max(0, round(a·30))`, `t1 = min(total, round(z·30))`, then `max(t1, t0 + 6)` | Python `round` = ties to even |
| hold | `t1 = min(next.t0 or total, max(t1, t0 + 8) + 15)` | after a stable sort by `t0` |
| flash drop | cues with `t1 − t0 < 4` removed | |
| capitalise | first word char of the first cue and of any cue after one ending `[.?!]` (optionally followed by one `{…}`) | `upper()` |
| `fix` | `norm(heard) → shown`, keeping trailing `.,?!` unless `shown` already ends with it; a later key with the same norm wins | per word |
| `cap_sub` | `[heard words, shown]`, first match wins at each position, keeps the last word's trailing punctuation | multi-word |
| `no_caption` | native `[a, z)`; tested at `native_of(round((a+z)/2·30))` | per word |
| caption event | layer 1, `{\pos(540,<caption_y[layout] or 1065>)}`, `\fs` only when the fit (86 at 1000 px) shrinks | layout = cut the cue starts in |
| hook event | layer 2, 0 → `cs(end)`, `end` = `round(until_s·30)` or the end frame of cut `until`; `{\pos(540,<y or 305>)}`, `\fs` only when `fit(hook_size or 88, 1000)` shrinks | |
| pill events | layer 3, `{\an7\pos(48,262)\fs<fit(50, 950)>}` (always written); one event per change of pill text over native frames; starts at `max(start, hook end)`; dropped when empty | |
| run clock | `label + "  {Y}" + m:ss` of `int(max(0, native_s − clock_from))`; label default `GENERATING` | a new event each displayed second |
| ASS time | `cs(f) = min(round(f·100/30), (total·100)//30)`; `H:MM:SS.CC` | libass shows `[⌈0.3·start_cs⌉, ⌈0.3·end_cs⌉)` |
| styles | `Speech` 86, `Hook` hook_size (88), `Pill` 50 — header lines copied verbatim | |

## RT's fit measure

`fit_size(text, size, max_px)`: strip `{…}`, split on the literal `\N`, `w = max(getlength) ×
0.838`, `size if w ≤ max_px else int(size·max_px / w)`. Pillow's `getlength` on AJ's machine
uses raqm (HarfBuzz over FreeType). RP §2.4 says the converter reproduces Pillow's "basic
layout"; that is an erratum: on AJ's machine Pillow lays out with raqm, and basic layout
disagrees with the table on 960 of 965 rows. `src/captions/pillow.rs` reproduces it by shaping with
harfrust at HarfBuzz scale `size·64` and summing `x_advance / 64` — FreeType's unhinted
advance at nominal size `size` rounds to 26.6 exactly as HarfBuzz's `em_scale` does, so the
two agree to the bit. Evidence: `tools/s4_pillow_widths.py` writes `pillow-widths.tsv` (every
golden hook/caption/pill line at the nominal size its fit starts from, plus an ASCII probe
set at 50/86/88; 965 rows); the lab test
`tests/pillow_widths.rs` matches all 965 bit for bit.

## R-CAP-1 [M0] evidence (`cargo test -p gmclip-render --features lab --test rcap1`)

- 10/10 reels: the rebuilt `captions-v5.ass` is **byte-identical** to the golden.
- 508 caption cues: text identical on 508 (100 %); start and end at exactly 0 frames on 508.
- All hook and pill events identical (583 events in total).
- `\fs`: every hook, caption and pill event in all 11 golden files (648 shown events,
  h79 included) equals RT's fit of its text; 2 events shrink (h48 hook 81, h03 caption 84).
