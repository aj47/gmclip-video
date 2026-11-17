//! Allowed-fields readers (B-03, SPEC §15.0). Read-only: these modules read the header
//! fields listed in `codec/ALLOWED_FIELDS.md`, each for the clause it serves, split VP9
//! superframes and synthesise `vpcC`. They never decode or encode coded picture or
//! audio data, and never parse slice or tile data. Every value read is untrusted; the
//! same budget rules as `demux` hold (no unwrap, no indexing, checked lengths).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

pub mod bits;
pub mod h264;
pub mod vp9;

/// Why a header could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("{field}: header ends at bit {bit}")]
    Truncated { field: &'static str, bit: usize },
    #[error("{field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    #[error("unsupported: {what}")]
    Unsupported { what: &'static str },
}

pub type Result<T> = std::result::Result<T, CodecError>;

/// Colour description from the container (Matroska `Colour`, MP4 `colr` nclx/nclc), as
/// ISO/IEC 23091-2 code points. Not codec data: the demuxer fills it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColourTags {
    pub primaries: Option<u8>,
    pub transfer: Option<u8>,
    pub matrix: Option<u8>,
    pub full_range: Option<bool>,
    /// Chroma sited "left" (co-sited horizontally, between lines vertically).
    pub chroma_left: Option<bool>,
}
