# 0056 — `gmclip-core` may depend on `gmclip-analysis`

Date: 2026-10-29 · Decided by: AJ ("3 A keep", on the decision brief for the baseline questions) · Status: adopted

## Context
Decision 0048 (§9 "Word edges") put the measuring of word extents in `AnalysisView` in `gmclip-core`, which needs `gmclip-analysis` (blob and energy decoding, `edges`). `cargo xt deps` forbade that edge. Commit `6170ca0` (2026-10-23) added `"gmclip-analysis"` to `gmclip-core`'s allowlist in `xtask/src/deps.rs` ("approved by AJ") without a decision record, which the file's header requires for any change to the table. Decision 0049 has since built on it (`AnalysisView::voice_span`). The alternative, moving the word-edge code behind `AnalysisHost`, was sized at about 470 changed lines across 6 core files plus `host.rs` and both host implementations.

## Decision
- `gmclip-core` → `gmclip-analysis` is allowed. `gmclip-analysis` depends on `gmclip-model`, `ciborium`, `serde` and `thiserror` only, so R-ARCH-1 holds: core still never reaches render, media or gpu, directly or transitively.
- The word-edge code stays in core (`AnalysisView`, `voice_span`).
- `AnalysisHost`'s doc no longer says core never links analysis: it says core links the pure-data `gmclip-analysis`, never `gmclip-ml`, and the host runs the analysis itself.

## Evidence
- `xtask/src/deps.rs`: the `gmclip-core` entry, line `"gmclip-analysis",` added by `6170ca0`.
- `crates/gmclip-analysis/Cargo.toml` `[dependencies]`.

## Sections changed
- `xtask/src/deps.rs` module doc (SPEC §3.1 rule list): `analysis ← core` joins the allowed directions.
- `crates/gmclip-core/src/host.rs`: the `AnalysisHost` doc comment.
