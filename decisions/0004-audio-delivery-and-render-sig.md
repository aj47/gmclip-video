# 0004 — AAC delivery tag, render_sig scope, true-peak point

- **Context.** Lane B2 (B-04, B-12, B-16) found: AVFoundation assumes 2112 samples of AAC priming in MP4 without an edit list, so audio plays 44 ms early in QuickTime/Safari; RenderPlan rule 9 signs `meta`, `provenance` and file mtimes, which makes the M1 seam test (converter ≡ planner) impossible; the −1.5 dBTP ceiling is ambiguous between the PCM mix and the delivered AAC.
- **Decision (on decision 0002 defaults; AJ may override in STATUS "Needs AJ").**
  1. `Mp4Writer` writes an `iTunSMPB` tag (priming 0, exact length) alongside the no-`edts` scheme. Platform behaviour is verified in H-14a; if any platform mis-handles it, the tag becomes per-preset.
  2. `render_sig` excludes `meta`, `provenance` and file mtimes; it covers everything that changes pixels or samples (resolved layers, text events, audio plan, encoder settings) plus source and font content hashes.
  3. The true-peak ceiling applies to the **delivered** file: export decodes its own AAC, measures true peak, and if it exceeds −1.5 dBTP lowers the limiter ceiling by the overshoot and re-encodes once.
- **Watch item.** Golden cuts sit near −30 LUFS, so static −16 LUFS needs +14…+17 dB and the limiter reduces > 1 dB on 1–5% of frames (max 7.8 dB). RT used dynamic loudnorm. Listen before the blind A/B (H-13); if it sounds worse, the fallback is a slower-release limiter tuned on the golden set — not dynamic normalisation (SPEC §5.6).
- **Sections changed.** `spec/renderplan-v0.md` rule 9 (render_sig scope); SPEC §5.6 (true-peak point); §6.5 step 4 (iTunSMPB).
