//! Decoder trait shells (implementer Part C; SPEC §6.2).

use crate::index::SampleIdx;

/// Media errors carry `{step, frame?, source?, os_status?}` (R-CMD-6).
#[derive(Clone, Eq, PartialEq, Debug, thiserror::Error)]
#[error(
    "media error at {step}{}{}",
    frame.map(|f| format!(", frame {f}")).unwrap_or_default(),
    os_status.map(|s| format!(" (OSStatus {s})")).unwrap_or_default()
)]
pub struct MediaError {
    pub step: &'static str,
    pub frame: Option<u64>,
    pub source_id: Option<String>,
    pub os_status: Option<i64>,
}

/// One coded sample handed to a decoder.
#[derive(Copy, Clone, Debug)]
pub struct EncodedSample<'a> {
    pub sample: SampleIdx,
    pub dts: i64,
    pub pts: i64,
    pub sync: bool,
    pub data: &'a [u8],
}

/// Submit flags.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct SubmitFlags {
    /// Pre-roll: decode for reference only, emit no frame.
    pub no_output: bool,
}

/// What a decoder session can do.
#[derive(Clone, Debug, Default)]
pub struct DecoderCaps {
    pub hardware: bool,
    pub max_sessions: u32,
}

/// Decoded picture. GPU planes arrive with gmclip-gpu (H-01); CPU planes are the
/// fallback and the CI path.
#[derive(Debug)]
pub enum FrameImage {
    Cpu {
        y: Vec<u8>,
        uv: Vec<u8>,
        /// Row length of each plane, in bytes.
        stride: [u32; 2],
        /// Bits per sample: 8 (NV12, a byte a sample) or 10 (P010: 16-bit little-endian
        /// samples, the 10 bits in the high bits, as VideoToolbox's P010).
        bits: u8,
    },
    /// An IOSurface-backed `CVPixelBuffer` from VideoToolbox (H-02); imported as two
    /// plane textures by `gmclip_gpu::apple::MetalImporter`. Shared (`Arc`): layers
    /// reading the same frame share one decoded picture (SPEC §6.2 Memory), and VP9
    /// `show_existing_frame` repeats one (H-03).
    #[cfg(target_os = "macos")]
    Apple(std::sync::Arc<gmclip_gpu::apple::PixelBuffer>),
}

/// A decoded frame, in any order; the reorder stage sorts by PTS.
#[derive(Debug)]
pub struct DecodedFrame {
    pub sample: SampleIdx,
    pub pts: i64,
    pub image: FrameImage,
}

/// An OS decoder session.
pub trait VideoDecoder: Send {
    fn submit(&mut self, s: &EncodedSample<'_>, flags: SubmitFlags) -> Result<(), MediaError>;
    fn poll(&mut self) -> Result<Option<DecodedFrame>, MediaError>;
    /// Drains delayed frames.
    fn flush(&mut self) -> Result<(), MediaError>;
    fn caps(&self) -> &DecoderCaps;
    /// A sequential reader should keep its look-ahead topped up (one more sample per
    /// request) instead of letting it drain and calling [`Self::flush`]: on Media
    /// Foundation the decoder hands a picture out only as more input arrives and a flush
    /// is a drain, then a replay from the keyframe; on an asynchronous VideoToolbox
    /// session a flush waits for every picture in flight, so a drained look-ahead puts
    /// the decode back in series with the caller's work. `false` for a synchronous
    /// decoder (VideoToolbox VP9: each submit blocks until decoded).
    fn needs_input_to_output(&self) -> bool {
        false
    }
    /// Blocks until [`Self::poll`] has something to hand out, leaving the other samples
    /// in flight decoding (a flush waits for all of them). `false` when the decoder
    /// cannot wait for a single picture or nothing came: the caller flushes instead.
    fn wait_output(&mut self) -> Result<bool, MediaError> {
        Ok(false)
    }
}
