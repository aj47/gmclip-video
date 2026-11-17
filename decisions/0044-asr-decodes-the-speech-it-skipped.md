# 0044 — ASR decodes the speech it skipped

Date: 2026-10-19 · Decided by: agent (AJ: "fix gmclip", after "how do we fix it for the future so this never occurs") · Status: adopted, AJ may veto

## Context
In session `s1792445891846-0` AJ asked to restore "the part where i say no ones heard of bb" and the agent found no such line in any transcript. It is in the voice-over (`2026-10-19 12-00-36.mp4`, 12.7–18.6 s: "I stopped using Codex and Claude Code harness directly to use something no one's really heard of."). The import hears 20 s first, with 3 s of context (0–23 s). Parakeet decoded that window as "…every other harness directly." and stopped there: it stretched "directly." over 4.6–12.6 s and returned no words for the rest. The next window (17–113 s) heard "No one's really heard of." at 17.1 s, but it keeps only words from 20 s on, so the line was dropped. A decode of the same audio that starts anywhere after 5 s hears the whole sentence, and so do decodes of 0–28 s and 0–33 s. Nothing compared the words with the sound, so the hole was invisible to the agent and to AJ.

## Decision
- `gmclip_ml::holes`: in each decoded window, sound (the window's own §5.5 quiet grid) that no word covers is a hole. A word covers its start to at most 1.2 s later, ± 0.2 s. Uncovered runs within 0.6 s of each other are one hole, and a hole needs at least 0.8 s of sound. Each hole is decoded again on its own, with 0.3 s of context each side. Its new words that start in the hole and fall outside any word's coverage are added, and a word whose end runs into an added word is cut at the added word's start.
- `ParakeetAsr::transcribe_streaming` fills holes in every window before stitching. `transcribe_window` (the arbiter's probes) is unchanged.
- `asr_revision` gains `/holes1` (`HOLES_VERSION`), so transcripts made before this decision can be told apart. Existing transcripts are not re-heard: a source is re-transcribed with `force`.

## Evidence
- `crates/gmclip-ml/src/holes.rs` tests: speech with no words is a hole; covered speech, short sounds and silence are not; only new words inside the hole are kept; a smeared word is cut.
- `crates/gmclip-ml/tests/lab_asr.rs::speech_after_a_blank_decode_is_heard` (lab): the import's first window of the voice-over (`vo-2026-10-19.mp4` in the lab media) now hears the retake. It fails with hole filling off.
- `lab_asr.rs::h79_windows_match_the_reference` still passes: pooled WER 3.35 %, median |Δstart| 0 ms, 39.6× real time.
- On the whole voice-over with the import's windows: 247 → 264 words, the full line at 12.9–18.0 s, no duplicated words.

## Sections changed
- `spec/SPEC.md` §9 (ASR row): a window's uncovered sound is decoded again.
