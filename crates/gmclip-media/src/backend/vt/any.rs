//! One decoder type for every codec the export path opens (H-10).

use super::{VtVideoDecoder, VtVp9Decoder};
use crate::decode::{
    DecodedFrame, DecoderCaps, EncodedSample, MediaError, SubmitFlags, VideoDecoder,
};

/// H.264 or VP9 on VideoToolbox.
#[derive(Debug)]
pub enum VtAnyDecoder {
    H264(VtVideoDecoder),
    Vp9(VtVp9Decoder),
}

impl VideoDecoder for VtAnyDecoder {
    fn submit(&mut self, s: &EncodedSample<'_>, flags: SubmitFlags) -> Result<(), MediaError> {
        match self {
            Self::H264(d) => d.submit(s, flags),
            Self::Vp9(d) => d.submit(s, flags),
        }
    }

    fn poll(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        match self {
            Self::H264(d) => d.poll(),
            Self::Vp9(d) => d.poll(),
        }
    }

    fn flush(&mut self) -> Result<(), MediaError> {
        match self {
            Self::H264(d) => d.flush(),
            Self::Vp9(d) => d.flush(),
        }
    }

    fn caps(&self) -> &DecoderCaps {
        match self {
            Self::H264(d) => d.caps(),
            Self::Vp9(d) => d.caps(),
        }
    }

    fn needs_input_to_output(&self) -> bool {
        match self {
            Self::H264(d) => d.needs_input_to_output(),
            Self::Vp9(d) => d.needs_input_to_output(),
        }
    }

    fn wait_output(&mut self) -> Result<bool, MediaError> {
        match self {
            Self::H264(d) => d.wait_output(),
            Self::Vp9(d) => d.wait_output(),
        }
    }
}
