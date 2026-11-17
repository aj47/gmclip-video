//! VideoToolbox decode (H-02 H.264, H-03 VP9) behind the [`VideoDecoder`] trait; the
//! shared [`FrameReader`] (`backend::frames`) gives it frame-exact random access over a
//! [`SampleSource`].
//!
//! The FFI lives in `gmclip_gpu::apple::vt_decode`; this module is safe code.

use std::collections::HashMap;

pub mod any;
pub mod audio;
pub mod encode;
pub mod vp9;
pub use any::VtAnyDecoder;
pub use audio::{AacRandomReader, AacTrackDecoder, AacTrackEncoder, asc_from_esds};
pub use encode::VtH264Encoder;
pub use vp9::VtVp9Decoder;

use gmclip_gpu::GpuError;
use gmclip_gpu::apple::{DecodeFlags, VtDecoder, VtDecoderConfig};

use crate::decode::{
    DecodedFrame, DecoderCaps, EncodedSample, FrameImage, MediaError, SubmitFlags, VideoDecoder,
};
use crate::index::SampleIdx;

pub use gmclip_gpu::apple::{CodecConfig, PlaneFormat};

/// Maps a gmclip-gpu error to a media error at `step`.
pub(crate) fn media_err(step: &'static str, frame: Option<u64>, e: GpuError) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: e.os_status(),
    }
}

/// A VideoToolbox session as a [`VideoDecoder`]. Frames come out in decode order.
///
/// Every submission gets its own sequence number as the VideoToolbox frame refcon, so
/// a sample submitted twice (pre-roll in one request, output in another) can never be
/// mistaken for the other submission. A pre-roll frame that fails is remembered and
/// reported by the next [`VideoDecoder::poll`]: a broken reference must fail the
/// request rather than silently corrupt the target.
#[derive(Debug)]
pub struct VtVideoDecoder {
    vt: VtDecoder,
    caps: DecoderCaps,
    /// Submission sequence → (sample, PTS from the index, pre-roll).
    inflight: HashMap<u64, (SampleIdx, i64, bool)>,
    next_seq: u64,
    preroll_error: Option<MediaError>,
    asynchronous: bool,
}

impl VtVideoDecoder {
    pub fn new(cfg: &VtDecoderConfig, asynchronous: bool) -> Result<Self, MediaError> {
        let vt = VtDecoder::new(cfg).map_err(|e| media_err("vt_session_create", None, e))?;
        let caps = DecoderCaps {
            hardware: vt.is_hardware(),
            max_sessions: 3,
        };
        Ok(Self {
            vt,
            caps,
            inflight: HashMap::new(),
            next_seq: 0,
            preroll_error: None,
            asynchronous,
        })
    }
}

impl VideoDecoder for VtVideoDecoder {
    fn submit(&mut self, s: &EncodedSample<'_>, flags: SubmitFlags) -> Result<(), MediaError> {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.inflight
            .insert(seq, (s.sample, s.pts, flags.no_output));
        let r = self.vt.decode(
            s.data,
            s.pts,
            s.dts,
            None,
            seq,
            DecodeFlags {
                no_output: flags.no_output,
                asynchronous: self.asynchronous,
            },
        );
        if r.is_err() {
            self.inflight.remove(&seq);
        }
        r.map_err(|e| media_err("vt_decode_submit", Some(u64::from(s.sample.0)), e))
    }

    fn poll(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        while let Some(f) = self.vt.take() {
            let Some((sample, pts, preroll)) = self.inflight.remove(&f.tag) else {
                continue; // not ours (cannot happen: every tag is a live submission)
            };
            let frame = Some(u64::from(sample.0));
            let dropped = f.info_flags & 0x2 != 0; // kVTDecodeInfo_FrameDropped
            if preroll {
                if f.status != 0 && self.preroll_error.is_none() {
                    self.preroll_error = Some(MediaError {
                        step: "vt_decode_preroll",
                        frame,
                        source_id: None,
                        os_status: Some(i64::from(f.status)),
                    });
                }
                continue;
            }
            if let Some(e) = self.preroll_error.take() {
                return Err(e);
            }
            if f.status != 0 {
                return Err(MediaError {
                    step: "vt_decode",
                    frame,
                    source_id: None,
                    os_status: Some(i64::from(f.status)),
                });
            }
            let (Some(pb), false) = (f.image, dropped) else {
                return Err(MediaError {
                    step: "vt_decode_dropped",
                    frame,
                    source_id: None,
                    os_status: None,
                });
            };
            // The PTS is the index's, carried through the submission record.
            return Ok(Some(DecodedFrame {
                sample,
                pts,
                image: FrameImage::Apple(std::sync::Arc::new(pb)),
            }));
        }
        match self.preroll_error.take() {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    fn flush(&mut self) -> Result<(), MediaError> {
        self.vt
            .flush()
            .map_err(|e| media_err("vt_decode_flush", None, e))
    }

    fn caps(&self) -> &DecoderCaps {
        &self.caps
    }

    /// An asynchronous session decodes while the caller works on the last picture, as
    /// long as samples stay in flight; its flush waits for all of them.
    fn needs_input_to_output(&self) -> bool {
        self.asynchronous
    }

    fn wait_output(&mut self) -> Result<bool, MediaError> {
        if !self.asynchronous || self.inflight.is_empty() {
            return Ok(false);
        }
        // bounded: a picture that never comes falls back to the caller's flush
        Ok(self.vt.wait_output(std::time::Duration::from_secs(1)))
    }
}

pub use crate::backend::frames::FrameReader;
