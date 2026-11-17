//! The export's Windows pieces (M5): Media Foundation decode and encode, decoded CPU
//! planes uploaded into the compositor's textures, the NV12 output read back for the
//! encoder. Same names and shapes as `os_apple`, so the pipeline around them is one.
//!
//! Decode runs on the GPU (D3D11; software H.264 when there is none); VP9 needs the GPU.
//! Not on Windows: Apple Vision segmentation (mask tracks built on a Mac are read and
//! applied; building one, or a live draft matte, is refused).

use std::collections::VecDeque;

use gmclip_gpu::{GpuContext, GpuError};
use gmclip_media::MediaError;
use gmclip_media::backend::VideoSampleSink;
use gmclip_media::backend::mf::{Accel, MfH264Encoder, MfVideoDecoder};
use gmclip_media::decode::{DecodedFrame, FrameImage};

pub(crate) use gmclip_gpu::windows::{AacEncoder, H264EncoderConfig as EncoderConfig, H264Level};
pub(crate) use gmclip_media::backend::mf::MfVideoDecoder as AnyDecoder;

use super::{Src, failed};
use crate::export::VideoCodec;
use crate::nv12::Nv12Converter;
use crate::ops::RenderError;

/// Samples a [`FrameStream`](gmclip_media::backend::FrameStream) submits past a request on
/// Windows (see [`DecoderRecipe::lookahead`]).
const MF_LOOKAHEAD: usize = 4;

/// How to make a decoder for one source: on the GPU (D3D11) when this PC has one, else
/// H.264 in software; VP9 on the GPU only (Microsoft's VP9 decoder).
#[derive(Clone)]
pub(crate) struct DecoderRecipe {
    /// `None` for VP9.
    avcc: Option<Vec<u8>>,
    width: u32,
    height: u32,
}

impl DecoderRecipe {
    /// Look-ahead for a [`FrameStream`] on this decoder: Media Foundation hands a picture
    /// out only as later input arrives (two frames later on the lab pack), so the stream
    /// keeps this many samples submitted past each request
    /// (`VideoDecoder::needs_input_to_output`) and never drains on a sequential read.
    ///
    /// [`FrameStream`]: gmclip_media::backend::FrameStream
    pub(crate) fn lookahead(&self) -> usize {
        MF_LOOKAHEAD
    }

    pub(crate) fn make(&self) -> Result<AnyDecoder, MediaError> {
        match &self.avcc {
            Some(avcc) => MfVideoDecoder::h264(avcc, self.width, self.height, Accel::Hardware)
                .or_else(|_| MfVideoDecoder::h264(avcc, self.width, self.height, Accel::Software)),
            None => MfVideoDecoder::vp9(self.width, self.height),
        }
    }
}

pub(crate) fn recipe(
    src: Src,
    codec: &VideoCodec,
    width: u32,
    height: u32,
) -> Result<(DecoderRecipe, Src), RenderError> {
    let avcc = match codec {
        VideoCodec::H264 { avcc } => Some(avcc.clone()),
        VideoCodec::Vp9 => None,
    };
    Ok((
        DecoderRecipe {
            avcc,
            width,
            height,
        },
        src,
    ))
}

/// A picture's planes as compositor textures (R8 luma, RG8 chroma).
#[derive(Debug)]
pub(crate) struct PlaneTextures {
    pub y: wgpu::Texture,
    pub uv: wgpu::Texture,
    pub width: u32,
    pub height: u32,
}

/// Plane layouts a decoded picture comes in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlaneFormat {
    /// 8-bit 4:2:0, video range.
    Nv12Video,
    /// 10-bit 4:2:0, video range: 16-bit little-endian samples, the bits high.
    P010Video,
}

impl PlaneFormat {
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            PlaneFormat::Nv12Video => 1,
            PlaneFormat::P010Video => 2,
        }
    }
}

/// A picture's planes on the CPU, tightly packed.
#[derive(Clone, Debug)]
pub(crate) struct CpuPlanes {
    pub width: u32,
    pub height: u32,
    pub format: PlaneFormat,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

/// The format of a CPU picture.
fn cpu_format(img: &FrameImage) -> PlaneFormat {
    let FrameImage::Cpu { bits, .. } = img;
    if *bits > 8 {
        PlaneFormat::P010Video
    } else {
        PlaneFormat::Nv12Video
    }
}

/// The size in samples of a CPU NV12 / P010 picture.
fn cpu_size(img: &FrameImage) -> Option<(u32, u32)> {
    let FrameImage::Cpu { y, uv, stride, .. } = img;
    let row = stride[0] as usize;
    let bps = cpu_format(img).bytes_per_sample();
    if row == 0 || !row.is_multiple_of(bps) || !y.len().is_multiple_of(row) {
        return None;
    }
    let (w, h) = (row / bps, y.len() / row);
    (uv.len() == row * h.div_ceil(2) && stride[1] as usize == row).then_some((w as u32, h as u32))
}

/// A P010 plane narrowed to 8 bits (rounded): for the proxy's 8-bit encoder.
fn to_8bit(p: &[u8]) -> Vec<u8> {
    p.chunks_exact(2)
        .map(|s| ((u32::from(u16::from_le_bytes([s[0], s[1]])) + 128) >> 8).min(255) as u8)
        .collect()
}

fn plane(
    dev: &(wgpu::Device, wgpu::Queue),
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
    bpp: u32,
    data: &[u8],
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let t = dev.0.create_texture(&wgpu::TextureDescriptor {
        label: Some("decoded plane"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    dev.1.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &t,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bpp),
            rows_per_image: Some(h),
        },
        size,
    );
    t
}

/// Decoded pictures into the compositor's textures: Media Foundation decodes to CPU
/// NV12 or P010, uploaded here (P010 into 16-bit planes, as macOS imports it).
pub(crate) struct Importer {
    dev: (wgpu::Device, wgpu::Queue),
    norm16: bool,
}

impl Importer {
    pub(crate) fn new(ctx: &GpuContext) -> Result<Self, RenderError> {
        Ok(Self {
            dev: (ctx.device.clone(), ctx.queue.clone()),
            norm16: ctx.norm16,
        })
    }

    /// The picture's planes and their 8-bit code scale, at output frame `f`.
    pub(crate) fn import(
        &mut self,
        pic: &DecodedFrame,
        f: u64,
    ) -> Result<(PlaneTextures, f32), RenderError> {
        let (w, h) = cpu_size(&pic.image)
            .ok_or_else(|| failed("decode", Some(f), "decoded planes of an unexpected size"))?;
        let FrameImage::Cpu { y, uv, .. } = &pic.image;
        let (fy, fuv, b, scale) = match cpu_format(&pic.image) {
            PlaneFormat::Nv12Video => (
                wgpu::TextureFormat::R8Unorm,
                wgpu::TextureFormat::Rg8Unorm,
                1,
                255.0,
            ),
            // 10 bits MSB-aligned in 16: unorm · 65535 / 256 is the 8-bit-equivalent code
            // (as `os_apple`'s code scale)
            PlaneFormat::P010Video if self.norm16 => (
                wgpu::TextureFormat::R16Unorm,
                wgpu::TextureFormat::Rg16Unorm,
                2,
                65_535.0 / 256.0,
            ),
            PlaneFormat::P010Video => {
                return Err(RenderError::Unsupported {
                    step: "compose",
                    reason: "10-bit sources need 16-bit textures, which this GPU lacks".into(),
                });
            }
        };
        let planes = PlaneTextures {
            y: plane(&self.dev, (w, h), fy, b, y),
            uv: plane(&self.dev, (w.div_ceil(2), h.div_ceil(2)), fuv, 2 * b, uv),
            width: w,
            height: h,
        };
        Ok((planes, scale))
    }

    /// Nothing is cached (textures go with their last use).
    pub(crate) fn flush(&mut self) {}
}

/// A decoded picture's planes on the CPU (always `Some`: Media Foundation decodes to
/// CPU planes).
pub(crate) fn cpu_planes(img: &FrameImage) -> Option<Result<CpuPlanes, GpuError>> {
    let FrameImage::Cpu { y, uv, .. } = img;
    Some(match cpu_size(img) {
        Some((width, height)) => Ok(CpuPlanes {
            width,
            height,
            format: cpu_format(img),
            y: y.clone(),
            uv: uv.clone(),
        }),
        None => Err(GpuError::Unsupported(
            "decoded planes of an unexpected size".into(),
        )),
    })
}

/// Box-filtered NV12 resize (the scrub proxy's downscale; VideoToolbox does it inside
/// the encoder on macOS). Each output sample averages the source samples its footprint
/// covers, at least one.
pub(crate) fn scale_nv12(
    y: &[u8],
    uv: &[u8],
    (sw, sh): (u32, u32),
    (dw, dh): (u32, u32),
) -> (Vec<u8>, Vec<u8>) {
    fn plane(src: &[u8], (sw, sh): (usize, usize), (dw, dh): (usize, usize), ch: usize) -> Vec<u8> {
        let mut out = vec![0u8; dw * dh * ch];
        let span = |o: usize, d: usize, s: usize| {
            let a = o * s / d;
            let b = ((o + 1) * s).div_ceil(d).min(s).max(a + 1);
            (a, b)
        };
        for oy in 0..dh {
            let (y0, y1) = span(oy, dh, sh);
            for ox in 0..dw {
                let (x0, x1) = span(ox, dw, sw);
                for c in 0..ch {
                    let mut acc = 0u32;
                    for yy in y0..y1 {
                        let row = &src[(yy * sw) * ch..];
                        for xx in x0..x1 {
                            acc += u32::from(row[xx * ch + c]);
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)) as u32;
                    out[(oy * dw + ox) * ch + c] = ((acc + n / 2) / n) as u8;
                }
            }
        }
        out
    }
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    (
        plane(y, (sw, sh), (dw, dh), 1),
        plane(
            uv,
            (sw.div_ceil(2), sh.div_ceil(2)),
            (dw.div_ceil(2), dh.div_ceil(2)),
            2,
        ),
    )
}

/// Encodes a decoded picture, scaled to the encoder's size when it differs (always
/// `Some`).
pub(crate) fn encode_picture(
    enc: &mut H264Encoder,
    img: &FrameImage,
    pts: i64,
    duration: i64,
    sink: &mut dyn VideoSampleSink,
) -> Option<Result<(), MediaError>> {
    let FrameImage::Cpu { y, uv, .. } = img;
    let Some(size) = cpu_size(img) else {
        return Some(Err(MediaError {
            step: "proxy_planes",
            frame: None,
            source_id: None,
            os_status: None,
        }));
    };
    let narrowed;
    let (y, uv) = if cpu_format(img) == PlaneFormat::P010Video {
        narrowed = (to_8bit(y), to_8bit(uv));
        (&narrowed.0, &narrowed.1)
    } else {
        (y, uv)
    };
    Some(if size == enc.size() {
        enc.0.encode(y, uv, pts, duration, sink)
    } else {
        let (sy, suv) = scale_nv12(y, uv, size, enc.size());
        enc.0.encode(&sy, &suv, pts, duration, sink)
    })
}

/// A finished NV12 output frame, read back from the GPU.
#[derive(Debug)]
pub(crate) struct OutFrame {
    width: u32,
    height: u32,
    y: Vec<u8>,
    uv: Vec<u8>,
}

impl OutFrame {
    pub(crate) fn read_planes(&self) -> Result<CpuPlanes, GpuError> {
        Ok(CpuPlanes {
            width: self.width,
            height: self.height,
            format: PlaneFormat::Nv12Video,
            y: self.y.clone(),
            uv: self.uv.clone(),
        })
    }
}

/// One frame's render targets and readback buffers.
struct Slot {
    y: wgpu::Texture,
    uv: wgpu::Texture,
    y_buf: wgpu::Buffer,
    uv_buf: wgpu::Buffer,
}

fn padded(row: u32) -> u32 {
    row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// Converts R'G'B' canvases into NV12 frames on the CPU, up to `max_in_flight` frames
/// of GPU work queued (the twin of `nv12::apple::Nv12Output`; the readback is the
/// encoder's input until a D3D11 encoder takes textures).
pub(crate) struct Nv12Output {
    device: wgpu::Device,
    queue: wgpu::Queue,
    conv: Nv12Converter,
    size: (u32, u32),
    slots: Vec<Slot>,
    free: Vec<usize>,
    pending: VecDeque<(usize, wgpu::SubmissionIndex)>,
    max_in_flight: usize,
}

impl Nv12Output {
    pub(crate) fn new(
        ctx: &GpuContext,
        width: u32,
        height: u32,
        max_in_flight: usize,
        _downstream_hold: usize,
    ) -> Result<Self, GpuError> {
        let max_in_flight = max_in_flight.clamp(1, 8);
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let target = |w: u32, h: u32, format| {
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("nv12 output plane"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let buffer = |row: u32, rows: u32| {
            ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nv12 readback"),
                size: u64::from(padded(row)) * u64::from(rows),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };
        let slots: Vec<Slot> = (0..=max_in_flight)
            .map(|_| Slot {
                y: target(width, height, wgpu::TextureFormat::R8Unorm),
                uv: target(cw, ch, wgpu::TextureFormat::Rg8Unorm),
                y_buf: buffer(width, height),
                uv_buf: buffer(cw * 2, ch),
            })
            .collect();
        Ok(Self {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            conv: Nv12Converter::new(&ctx.device),
            size: (width, height),
            free: (0..slots.len()).rev().collect(),
            slots,
            pending: VecDeque::new(),
            max_in_flight,
        })
    }

    /// Renders `src` (the canvas: same size as the output, a non-sRGB float format).
    /// Returns the oldest finished frame once `max_in_flight` frames are queued.
    pub(crate) fn render(&mut self, src: &wgpu::TextureView) -> Result<Option<OutFrame>, GpuError> {
        let (w, h) = self.size;
        let t = src.texture();
        if (t.width(), t.height()) != (w, h) || t.format().is_srgb() {
            return Err(GpuError::Unsupported(format!(
                "NV12 source {}x{} {:?}, output {w}x{h}",
                t.width(),
                t.height(),
                t.format()
            )));
        }
        let k = match self.free.pop() {
            Some(k) => k,
            None => {
                return Err(GpuError::PoolExhausted(self.pending.len() as u32));
            }
        };
        let s = &self.slots[k];
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let vy = s.y.create_view(&Default::default());
        let vuv = s.uv.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.conv.encode(&self.device, &mut enc, src, &vy, &vuv);
        for (tex, buf, row, rows, tw) in [
            (&s.y, &s.y_buf, w, h, w),
            (&s.uv, &s.uv_buf, cw * 2, ch, cw),
        ] {
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded(row)),
                        rows_per_image: Some(rows),
                    },
                },
                wgpu::Extent3d {
                    width: tw,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
        }
        let submission = self.queue.submit([enc.finish()]);
        self.pending.push_back((k, submission));
        if self.pending.len() > self.max_in_flight {
            return self.read_oldest().map(Some);
        }
        Ok(None)
    }

    /// Waits for the oldest queued frame and reads it back.
    fn read_oldest(&mut self) -> Result<OutFrame, GpuError> {
        let (k, submission) = self
            .pending
            .pop_front()
            .ok_or_else(|| GpuError::Device("no frame queued".into()))?;
        let (w, h) = self.size;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let s = &self.slots[k];
        for b in [&s.y_buf, &s.uv_buf] {
            b.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|e| GpuError::Device(e.to_string()))?;
        let read = |b: &wgpu::Buffer, row: u32, rows: u32| -> Result<Vec<u8>, GpuError> {
            let v = b
                .slice(..)
                .get_mapped_range()
                .map_err(|e| GpuError::Device(e.to_string()))?;
            let mut out = Vec::with_capacity((row * rows) as usize);
            for r in 0..rows {
                let o = (r * padded(row)) as usize;
                out.extend_from_slice(&v[o..o + row as usize]);
            }
            drop(v);
            b.unmap();
            Ok(out)
        };
        let y = read(&s.y_buf, w, h);
        let uv = read(&s.uv_buf, cw * 2, ch);
        self.free.push(k);
        Ok(OutFrame {
            width: w,
            height: h,
            y: y?,
            uv: uv?,
        })
    }

    /// Waits for everything queued and returns those frames, oldest first.
    pub(crate) fn finish(&mut self) -> Result<Vec<OutFrame>, GpuError> {
        let mut out = Vec::with_capacity(self.pending.len());
        while !self.pending.is_empty() {
            out.push(self.read_oldest()?);
        }
        Ok(out)
    }

    /// One frame, synchronously (stills, tests). Errors if frames are queued.
    pub(crate) fn render_now(&mut self, src: &wgpu::TextureView) -> Result<OutFrame, GpuError> {
        if !self.pending.is_empty() {
            return Err(GpuError::Unsupported(
                "render_now with frames queued".into(),
            ));
        }
        self.render(src)?;
        self.finish()?
            .pop()
            .ok_or_else(|| GpuError::Device("no frame".into()))
    }
}

/// The Media Foundation encoder, taking [`OutFrame`]s, and its picture size.
#[derive(Debug)]
pub(crate) struct H264Encoder(MfH264Encoder, (u32, u32));

impl H264Encoder {
    pub(crate) fn new(cfg: &EncoderConfig) -> Result<Self, MediaError> {
        MfH264Encoder::new(cfg).map(|e| Self(e, (cfg.width, cfg.height)))
    }

    /// The encoder's picture size.
    fn size(&self) -> (u32, u32) {
        self.1
    }

    pub(crate) fn encode(
        &mut self,
        frame: &OutFrame,
        pts: i64,
        duration: i64,
        sink: &mut dyn VideoSampleSink,
    ) -> Result<(), MediaError> {
        self.0.encode(&frame.y, &frame.uv, pts, duration, sink)
    }

    pub(crate) fn finish(&mut self, sink: &mut dyn VideoSampleSink) -> Result<(), MediaError> {
        self.0.finish(sink)
    }

    pub(crate) fn peak_bits_per_second(&self) -> u64 {
        self.0.peak_bits_per_second()
    }
}

/// The AudioSpecificConfig of an encoder's stream.
pub(crate) fn aac_asc(enc: &AacEncoder) -> Option<Vec<u8>> {
    Some(enc.asc().to_vec())
}

/// Roto model quality, named as on macOS so tracks built there are found here.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SegQuality {
    Accurate,
    Balanced,
    Fast,
}

impl SegQuality {
    /// The name stored with a mask track (`apple_vision.accurate`, …).
    pub fn model_name(self) -> &'static str {
        match self {
            SegQuality::Accurate => "apple_vision.accurate",
            SegQuality::Balanced => "apple_vision.balanced",
            SegQuality::Fast => "apple_vision.fast",
        }
    }
}

/// An 8-bit single-channel mask, rows of `width` bytes (255 = person).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VisionMask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Person segmentation: Apple Vision only. Never made on Windows.
#[derive(Debug)]
pub(crate) struct Segmenter(());

fn no_vision() -> GpuError {
    GpuError::Unsupported(
        "person segmentation needs Apple Vision: build mask tracks on a Mac (Windows applies them)"
            .into(),
    )
}

impl Segmenter {
    pub(crate) fn new(_quality: SegQuality) -> Result<Self, GpuError> {
        Err(no_vision())
    }

    pub(crate) fn sequence(_quality: SegQuality) -> Result<Self, GpuError> {
        Err(no_vision())
    }
}

/// Segments the crop `rect` of a decoded picture (unreachable: no [`Segmenter`]).
pub(crate) fn segment_crop(
    _seg: &mut Segmenter,
    _img: &FrameImage,
    _rect: (u32, u32, u32, u32),
    f: u64,
) -> Result<VisionMask, RenderError> {
    Err(failed("vision", Some(f), no_vision()))
}

/// Object tracking (decision 0031): Apple Vision only. Never made on Windows.
#[derive(Debug)]
pub(crate) struct Tracker(());

impl Tracker {
    pub(crate) fn new(_start: [f64; 4]) -> Result<Self, GpuError> {
        Err(GpuError::Unsupported(
            "motion tracking is not on Windows yet (it uses Apple Vision): track on a Mac; Windows plays the tracked overlay".into(),
        ))
    }
}

/// The tracked box on the next decoded picture (unreachable: no [`Tracker`]).
pub(crate) fn track_next(
    _t: &mut Tracker,
    _img: &FrameImage,
    f: u64,
) -> Result<([f64; 4], f32), RenderError> {
    Err(failed(
        "track",
        Some(f),
        "motion tracking is not on Windows yet",
    ))
}
