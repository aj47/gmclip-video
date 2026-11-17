//! Media engine (SPEC §6): demux and index, OS decode with reorder, OS encode, our own
//! MP4 writer and audio. No gmclip crate decodes or encodes coded picture or audio
//! data (SPEC §15.0); `codec/` reads only the fields in `codec/ALLOWED_FIELDS.md`.
//!
//! W0 skeleton: the `VideoDecoder`/`Demuxer` trait shells from implementer Part C and
//! module stubs. GPU frame types (`GpuImage::Planes`) arrive with gmclip-gpu (H-01).
#![forbid(unsafe_code)]

pub mod decode;
pub mod index;
/// Whole-file operations the CLI calls (`probe`, `remux`; A-01).
pub mod ops;

/// Container demuxers (B-01 MP4/MOV, B-02 MKV/WebM).
pub mod demux;

/// Allowed-fields readers (B-03). Read-only; never writes coded data (SPEC §15.0).
pub mod codec;

/// Fuzz harness bodies (B-17), shared by `fuzz/` and the crasher regression tests.
#[doc(hidden)]
pub mod fuzz;

/// OS decode/encode backends, one per platform, behind `cfg`, plus the reorder stage
/// and frame-exact seek they share.
pub mod backend;

/// Our own muxers (B-04 `Mp4Writer`).
pub mod mux;

/// Audio: mix graph, loudness, AAC priming compensation, WAV I/O (B-11, B-12, R-EXP-5).
pub mod audio;

/// Export record, `render_sig` and `superseded/` rotation (B-16).
pub mod export_record;

/// Export verify v0 (B-15; R-EXP-6, R-EXP-2).
pub mod verify;

/// Overlay images: untrusted PNG/JPEG decoded with limits, stored as PNG (R-MEDIA-SEC-3).
pub mod image;

/// Recording sessions: the session folder, its event log and per-screen input tracks
/// (decision 0045).
pub mod rec;

pub use decode::{DecodedFrame, MediaError, VideoDecoder};
pub use index::SampleIndex;
