//! The export's macOS pieces: VideoToolbox decode and encode, Metal import of decoded
//! pictures, AudioToolbox AAC, Apple Vision for roto mattes. The pipeline around them
//! (`export::pipeline`) is shared with Windows (`os_windows`).

use gmclip_gpu::apple::{CodecConfig, ImportUsage, MetalImporter, PixelBuffer, VtDecoderConfig};
use gmclip_gpu::{GpuContext, GpuError};
use gmclip_media::MediaError;
use gmclip_media::backend::VideoSampleSink;
use gmclip_media::backend::vt::vp9::{parse_header, split_superframe, vpcc};
use gmclip_media::backend::vt::{VtVideoDecoder, VtVp9Decoder};
use gmclip_media::decode::{DecodedFrame, FrameImage};
use gmclip_media::index::SampleIdx;

pub(crate) use crate::nv12::apple::Nv12Output;
/// Apple Vision's quality levels (public: `mattes::TrackQuality`).
pub use gmclip_gpu::apple::SegQuality;
pub(crate) use gmclip_gpu::apple::{
    AacEncoder, CpuPlanes, H264Level, PlaneTextures, Segmenter, VisionMask,
    VtEncoderConfig as EncoderConfig,
};
pub(crate) use gmclip_media::backend::vt::{
    VtAnyDecoder as AnyDecoder, VtH264Encoder as H264Encoder,
};

/// A finished NV12 output frame (a pool pixel buffer the encoder takes as is).
pub(crate) type OutFrame = PixelBuffer;

use super::{Src, failed, gpu};
use crate::export::VideoCodec;
use crate::ops::RenderError;

/// How to make a decoder for one source.
#[derive(Clone)]
pub(crate) struct DecoderRecipe {
    codec: CodecConfig,
    width: u32,
    height: u32,
    vp9_format: Option<gmclip_gpu::apple::PlaneFormat>,
    timescale: i32,
}

impl DecoderRecipe {
    /// Look-ahead for a [`FrameStream`] on this decoder: none for the synchronous VP9
    /// decoder (each submit blocks, so look-ahead only delays a seek: ~9 ms of a 1440p
    /// VP9 seek, H.05), the stream's default otherwise.
    ///
    /// [`FrameStream`]: gmclip_media::backend::FrameStream
    pub(crate) fn lookahead(&self) -> usize {
        if self.vp9_format.is_some() {
            0
        } else {
            gmclip_media::backend::stream::LOOKAHEAD
        }
    }

    pub(crate) fn make(&self) -> Result<AnyDecoder, MediaError> {
        match (&self.codec, self.vp9_format) {
            (CodecConfig::Vp9 { vpcc }, Some(fmt)) => Ok(AnyDecoder::Vp9(VtVp9Decoder::new(
                vpcc.clone(),
                self.width,
                self.height,
                fmt,
                self.timescale,
            )?)),
            (codec, _) => Ok(AnyDecoder::H264(VtVideoDecoder::new(
                &VtDecoderConfig {
                    codec: codec.clone(),
                    width: self.width,
                    height: self.height,
                    output: gmclip_gpu::apple::PlaneFormat::Nv12Video,
                    require_hardware: false,
                    timescale: self.timescale,
                },
                // Sequential export decode keeps the hardware busy asynchronously.
                true,
            )?)),
        }
    }
}

pub(crate) fn recipe(
    mut src: Src,
    codec: &VideoCodec,
    width: u32,
    height: u32,
) -> Result<(DecoderRecipe, Src), RenderError> {
    let timescale = src.index().timescale as i32;
    match codec {
        VideoCodec::H264 { avcc } => Ok((
            DecoderRecipe {
                codec: CodecConfig::H264 { avcc: avcc.clone() },
                width,
                height,
                vp9_format: None,
                timescale,
            },
            src,
        )),
        VideoCodec::Vp9 => {
            let first = src
                .index()
                .sync
                .iter()
                .position(|&s| s)
                .ok_or_else(|| failed("vp9_config", None, "no key frame"))?;
            let mut buf = Vec::new();
            src.read(SampleIdx(first as u32), &mut buf)?;
            let sub = split_superframe(&buf)
                .into_iter()
                .find_map(|f| parse_header(f).ok().filter(|h| h.key_frame))
                .ok_or_else(|| failed("vp9_config", None, "first sync sample has no key frame"))?;
            let fmt = VtVp9Decoder::output_format(&sub)?;
            Ok((
                DecoderRecipe {
                    codec: CodecConfig::Vp9 {
                        vpcc: vpcc(&sub, None, None, None),
                    },
                    width,
                    height,
                    vp9_format: Some(fmt),
                    timescale,
                },
                src,
            ))
        }
    }
}

/// The 8-bit code scale for a decoded picture's planes; full range is M2.
fn code_scale(pb: &PixelBuffer, frame: u64) -> Result<f32, RenderError> {
    match pb.format() {
        Some(gmclip_gpu::apple::PlaneFormat::Nv12Video) => Ok(255.0),
        // 10 bits MSB-aligned in 16: unorm · 65535 / 256 is the 8-bit-equivalent code.
        Some(gmclip_gpu::apple::PlaneFormat::P010Video) => Ok(65_535.0 / 256.0),
        _ => Err(RenderError::Unsupported {
            step: "compose",
            reason: format!(
                "source picture format at output frame {frame}: full range or unknown (M2)"
            ),
        }),
    }
}

/// Decoded pictures into the compositor's textures: zero-copy Metal views of the
/// IOSurface planes.
pub(crate) struct Importer(MetalImporter);

impl Importer {
    pub(crate) fn new(ctx: &GpuContext) -> Result<Self, RenderError> {
        MetalImporter::new(ctx)
            .map(Self)
            .map_err(gpu("metal_import", None))
    }

    /// The picture's planes and their 8-bit code scale, at output frame `f`.
    pub(crate) fn import(
        &mut self,
        pic: &DecodedFrame,
        f: u64,
    ) -> Result<(PlaneTextures, f32), RenderError> {
        let FrameImage::Apple(pb) = &pic.image else {
            return Err(failed("decode", Some(f), "decoder returned CPU planes"));
        };
        let planes = self
            .0
            .import(pb, ImportUsage::SAMPLE)
            .map_err(gpu("metal_import", Some(f)))?;
        let scale = code_scale(pb, f)?;
        Ok((planes, scale))
    }

    /// Lets go of textures no longer in use.
    pub(crate) fn flush(&mut self) {
        self.0.flush();
    }
}

/// A decoded picture's planes on the CPU; `None` for a picture of another kind (the
/// caller names the failure).
pub(crate) fn cpu_planes(img: &FrameImage) -> Option<Result<CpuPlanes, GpuError>> {
    let FrameImage::Apple(pb) = img else {
        return None;
    };
    Some(pb.read_planes())
}

/// Encodes a decoded picture as is (VideoToolbox scales it to the session's size);
/// `None` for a picture of another kind.
pub(crate) fn encode_picture(
    enc: &mut H264Encoder,
    img: &FrameImage,
    pts: i64,
    duration: i64,
    sink: &mut dyn VideoSampleSink,
) -> Option<Result<(), MediaError>> {
    let FrameImage::Apple(pb) = img else {
        return None;
    };
    Some(enc.encode(pb, pts, duration, sink))
}

/// Segments the crop `rect` of a decoded picture.
pub(crate) fn segment_crop(
    seg: &mut Segmenter,
    img: &FrameImage,
    rect: (u32, u32, u32, u32),
    f: u64,
) -> Result<VisionMask, RenderError> {
    let FrameImage::Apple(pb) = img else {
        return Err(failed("matte", Some(f), "decoder returned CPU planes"));
    };
    let (vm, _) = seg.segment_crop(pb, rect).map_err(gpu("vision", Some(f)))?;
    Ok(vm)
}

/// The AudioSpecificConfig of an encoder's stream.
pub(crate) fn aac_asc(enc: &AacEncoder) -> Option<Vec<u8>> {
    gmclip_media::backend::aac::asc_from_esds(enc.magic_cookie())
}

/// Object tracking (decision 0031): Apple Vision's `VNTrackObjectRequest`.
pub(crate) use gmclip_gpu::apple::Tracker;

/// The tracked box on the next decoded picture.
pub(crate) fn track_next(
    t: &mut Tracker,
    img: &FrameImage,
    f: u64,
) -> Result<([f64; 4], f32), RenderError> {
    let FrameImage::Apple(pb) = img else {
        return Err(failed("track", Some(f), "decoder returned CPU planes"));
    };
    t.next(pb).map_err(gpu("vision_track", Some(f)))
}
