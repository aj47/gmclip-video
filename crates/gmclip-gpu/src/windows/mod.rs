//! Windows media FFI (M5): the OS sessions whose calls need `unsafe` — WASAPI playback
//! output and Media Foundation decode and encode. The twin of [`crate::apple`].

pub mod aac;
pub mod aac_enc;
pub mod audio_out;
pub mod d3d;
pub mod h264;
pub mod h264_enc;
mod mf;
pub mod share;

pub use aac::{AAC_FRAME, AacDecoder};
pub use aac_enc::{AAC_ENCODER_PRIMING, AacEncoder};
pub use d3d::D3dDevice;
pub use h264::{
    H264Decoder, Nv12Frame, VideoCodec, hardware_decoder_listed, hardware_decoder_listed_for,
};
pub use h264_enc::{H264Encoder, H264EncoderConfig, H264Level, H264Packet};
