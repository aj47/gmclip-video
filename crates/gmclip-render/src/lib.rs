//! Compositor (SPEC §7), text (§8) and the export planner (§6.5). Core never depends on
//! this crate; the app and CLI adapt it to `gmclip_core::host::Renderer`.
#![forbid(unsafe_code)]

/// Caption cue builders (Lane C: B-08).
pub mod captions;
/// wgpu compositor and WGSL shaders, with a CPU path for CI (H-05).
pub mod compositor;
/// The export pipeline: RenderPlan → MP4 (H-10).
pub mod export;
/// Roto mattes: mask tracks, their cache and the per-frame finish (SPEC §5.8, R.01).
pub mod matte;
/// R'G'B' → NV12 (BT.709 limited, left-cosited chroma) output planes (H-07).
pub mod nv12;
/// Whole-plan operations the CLI calls (`render`, `still`; A-01) and
/// `RenderError`.
pub mod ops;
/// SHA-256 (FIPS 180-4), without a crate.
pub mod sha256;
/// Text events, the ASS importer and the cue raster (Lane C: B-07, B-09; H-06 composites).
pub mod text;
