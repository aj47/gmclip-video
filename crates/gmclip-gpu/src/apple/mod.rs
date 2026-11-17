//! Apple OS media and GPU interop: `CVPixelBuffer`s, the Metal import, VideoToolbox
//! and AudioToolbox sessions. Every `unsafe` FFI call gmclip makes on macOS lives
//! here (SPEC §3.1); `gmclip-media::backend::vt` builds the safe pipeline on top.

pub mod capture;
pub(crate) mod cf;
pub mod exit;
pub mod footprint;
pub mod metal_import;
pub mod pixel_buffer;

pub use footprint::phys_footprint;
pub use metal_import::{ImportUsage, MetalImporter, PlaneTextures, live_keepalives};
pub use pixel_buffer::{CpuPlanes, PixelBuffer, PixelBufferPool, PlaneFormat};
pub mod vt_decode;
pub use vt_decode::{CodecConfig, DecodeFlags, VtDecoder, VtDecoderConfig, VtFrame};
pub mod vt_encode;
pub use vt_encode::{H264Level, VtEncoder, VtEncoderConfig, VtPacket};
pub mod audio;
pub mod audio_out;
pub use audio::{AAC_FRAME, AacDecoder, AacEncoder, esds_from_asc};
pub mod vision;
pub use vision::{SegQuality, Segmenter, VisionMask};
pub mod track;
pub use track::{NormBox, Tracker};
pub mod qos;
pub mod share;
