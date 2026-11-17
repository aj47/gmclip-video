//! VideoToolbox decompression sessions (H-02 H.264, H-03 VP9). The FFI only: sample
//! order, pre-roll policy and reorder live in `gmclip-media::backend::vt`.
//!
//! Decoded pictures arrive through a C callback in **decode order** (temporal
//! processing is never requested), as IOSurface-backed, Metal-compatible
//! `CVPixelBuffer`s, each tagged with the caller's `tag`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Condvar, Mutex, Once};
use std::time::Duration;

use objc2_core_foundation::{CFBoolean, CFData, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreate, kCMBlockBufferAssureMemoryNowFlag,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMVideoCodecType_AV1,
    kCMVideoCodecType_H264, kCMVideoCodecType_HEVC, kCMVideoCodecType_VP9,
};
use objc2_core_video::{
    CVImageBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTIsHardwareDecodeSupported,
    VTRegisterSupplementalVideoDecoderIfAvailable, VTSessionCopyProperty,
    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

use super::cf::{Val, check, dict, up};
use super::pixel_buffer::{PixelBuffer, PlaneFormat};
use crate::GpuError;

/// The codec and its configuration record, as stored in the container.
#[derive(Clone, Debug)]
pub enum CodecConfig {
    /// `avcC` (AVCDecoderConfigurationRecord); samples are length-prefixed NAL units.
    H264 { avcc: Vec<u8> },
    /// `vpcC` (VPCodecConfigurationRecord, version 1, with its 4-byte FullBox header);
    /// one VP9 frame per sample (superframes split by the caller).
    Vp9 { vpcc: Vec<u8> },
}

/// Session parameters.
#[derive(Clone, Debug)]
pub struct VtDecoderConfig {
    pub codec: CodecConfig,
    pub width: u32,
    pub height: u32,
    /// Output pixel format (8-bit `420v` or 10-bit `x420`).
    pub output: PlaneFormat,
    /// Fail rather than fall back to a software decoder.
    pub require_hardware: bool,
    /// Timescale of the `pts`/`dts`/`duration` values passed to [`VtDecoder::decode`].
    pub timescale: i32,
}

/// Per-sample decode flags.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct DecodeFlags {
    /// `kVTDecodeFrame_DoNotOutputFrame`: decode for reference only (pre-roll).
    pub no_output: bool,
    /// `kVTDecodeFrame_EnableAsynchronousDecompression`.
    pub asynchronous: bool,
}

/// One callback from the session.
#[derive(Debug)]
pub struct VtFrame {
    /// The `tag` passed to [`VtDecoder::decode`].
    pub tag: u64,
    /// `noErr` (0) or the decoder's error.
    pub status: i32,
    /// `VTDecodeInfoFlags` bits (asynchronous, frame dropped, …).
    pub info_flags: u32,
    /// The decoded picture; `None` for pre-roll, dropped or failed frames.
    pub image: Option<PixelBuffer>,
    /// Presentation timestamp in the session timescale (as passed in).
    pub pts: i64,
}

struct Shared {
    out: Mutex<VecDeque<VtFrame>>,
    /// Signalled after every callback record is queued.
    ready: Condvar,
}

/// The decompression output callback. Runs on VideoToolbox's thread for asynchronous
/// decodes, or inside `VTDecompressionSessionDecodeFrame` otherwise.
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    info_flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: `refcon` is `Arc::as_ptr` of the `Shared` owned by the `VtDecoder` that
    // created this session; it is released only after the session is invalidated,
    // after which no callback runs.
    let shared = unsafe { &*(refcon as *const Shared) };
    let image = NonNull::new(image).map(|p| {
        // SAFETY: the callback receives the image buffer at +0 (Get rule); retaining it
        // gives us an owned reference that outlives the callback.
        PixelBuffer::from_retained(unsafe { CFRetained::retain(p) })
    });
    let frame = VtFrame {
        tag: frame_refcon as usize as u64,
        status,
        info_flags: info_flags.0,
        image,
        pts: if pts.flags.contains(CMTimeFlags::Valid) {
            pts.value
        } else {
            i64::MIN
        },
    };
    shared
        .out
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(frame);
    shared.ready.notify_all();
}

/// A VideoToolbox decompression session.
pub struct VtDecoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    shared: std::sync::Arc<Shared>,
    timescale: i32,
    hardware: bool,
}

// SAFETY: VTDecompressionSession and CMFormatDescription are CF objects with atomic
// retain/release; VideoToolbox allows a session to be driven from any thread as long as
// calls are not concurrent, which `&mut self` on every entry point guarantees. The
// shared queue is behind a Mutex.
unsafe impl Send for VtDecoder {}

impl std::fmt::Debug for VtDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtDecoder")
            .field("timescale", &self.timescale)
            .field("hardware", &self.hardware)
            .finish_non_exhaustive()
    }
}

/// Registers Apple's supplemental VP9 decoder (once per process). Needed before a VP9
/// format description or `VTIsHardwareDecodeSupported(vp09)` means anything.
pub fn register_vp9() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain registration call with a valid codec type; idempotent.
        unsafe { VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_VP9) };
    });
}

/// Whether the OS has a hardware decoder for H.264 / VP9 (VP9 after [`register_vp9`]).
pub fn hardware_decode_supported(vp9: bool) -> bool {
    let codec = if vp9 {
        register_vp9();
        kCMVideoCodecType_VP9
    } else {
        kCMVideoCodecType_H264
    };
    // SAFETY: plain query with a valid codec type.
    unsafe { VTIsHardwareDecodeSupported(codec) }
}

/// A codec the startup capability probe asks VideoToolbox about (R-PLAT-1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProbeCodec {
    H264,
    Hevc,
    /// Apple's supplemental VP9 decoder (registered first).
    Vp9,
    Av1,
}

/// Whether VideoToolbox has a hardware decoder for `codec` on this Mac (the startup
/// probe; VP9 after [`register_vp9`]). HEVC and AV1 are reported, not read, at M2.
pub fn hardware_decode(codec: ProbeCodec) -> bool {
    let c = match codec {
        ProbeCodec::H264 => kCMVideoCodecType_H264,
        ProbeCodec::Hevc => kCMVideoCodecType_HEVC,
        ProbeCodec::Vp9 => {
            register_vp9();
            kCMVideoCodecType_VP9
        }
        ProbeCodec::Av1 => kCMVideoCodecType_AV1,
    };
    // SAFETY: plain query with a valid codec type.
    unsafe { VTIsHardwareDecodeSupported(c) }
}

pub(crate) fn cm_time(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

/// A video format description carrying the codec configuration record as a sample
/// description extension atom (`avcC` / `vpcC`).
fn format_description(cfg: &VtDecoderConfig) -> Result<CFRetained<CMFormatDescription>, GpuError> {
    let (codec, atom_name, atom) = match &cfg.codec {
        CodecConfig::H264 { avcc } => (kCMVideoCodecType_H264, "avcC", avcc),
        CodecConfig::Vp9 { vpcc } => {
            register_vp9();
            (kCMVideoCodecType_VP9, "vpcC", vpcc)
        }
    };
    let atom_data = CFData::from_bytes(atom);
    let atom_key = CFString::from_str(atom_name);
    let atoms = dict(&[(&atom_key, Val::Obj(&up(&*atom_data)))]);
    // SAFETY: immutable CFString exported by CoreMedia.
    let atoms_key = unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms };
    let ext = dict(&[(atoms_key, Val::Obj(&up(&*atoms)))]);
    let mut out: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `ext` is a valid CFDictionary; `out` receives a +1 retained description.
    let status = unsafe {
        CMVideoFormatDescriptionCreate(
            None,
            codec,
            cfg.width as i32,
            cfg.height as i32,
            Some(ext.as_opaque()),
            NonNull::from(&mut out),
        )
    };
    check(status, "CMVideoFormatDescriptionCreate")?;
    let ptr = NonNull::new(out as *mut CMFormatDescription).ok_or(GpuError::Os {
        step: "CMVideoFormatDescriptionCreate",
        status: -1,
    })?;
    // SAFETY: Create rule, +1 retained.
    Ok(unsafe { CFRetained::from_raw(ptr) })
}

/// Copies `data` into a new CMBlockBuffer and wraps it as a one-sample CMSampleBuffer.
pub(crate) fn sample_buffer(
    data: &[u8],
    format: &CMFormatDescription,
    timing: CMSampleTimingInfo,
) -> Result<CFRetained<CMSampleBuffer>, GpuError> {
    if data.is_empty() {
        return Err(GpuError::Unsupported("empty coded sample".into()));
    }
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: NULL memory block + default allocator makes CoreMedia allocate
    // `data.len()` bytes now (AssureMemoryNow); `block` receives a +1 retained buffer.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            data.len(),
            None,
            std::ptr::null(),
            0,
            data.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    check(status, "CMBlockBufferCreateWithMemoryBlock")?;
    let block = NonNull::new(block).ok_or(GpuError::Os {
        step: "CMBlockBufferCreateWithMemoryBlock",
        status: -1,
    })?;
    // SAFETY: Create rule, +1 retained.
    let block: CFRetained<CMBlockBuffer> = unsafe { CFRetained::from_raw(block) };
    // SAFETY: `data` is a valid slice; the block holds exactly `data.len()` bytes.
    let status = unsafe {
        CMBlockBuffer::replace_data_bytes(NonNull::from(data).cast(), &block, 0, data.len())
    };
    check(status, "CMBlockBufferReplaceDataBytes")?;
    let size = data.len();
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: one sample with one timing entry and one size entry, all valid for the
    // duration of the call; `sample` receives a +1 retained sample buffer.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            1,
            &timing,
            1,
            &size,
            NonNull::from(&mut sample),
        )
    };
    check(status, "CMSampleBufferCreateReady")?;
    let sample = NonNull::new(sample).ok_or(GpuError::Os {
        step: "CMSampleBufferCreateReady",
        status: -1,
    })?;
    // SAFETY: Create rule, +1 retained.
    Ok(unsafe { CFRetained::from_raw(sample) })
}

impl VtDecoder {
    pub fn new(cfg: &VtDecoderConfig) -> Result<Self, GpuError> {
        let format = format_description(cfg)?;
        // SAFETY: immutable CFStrings exported by VideoToolbox / CoreVideo.
        let (hw_enable, hw_require, fmt_key, ios_key, mtl_key) = unsafe {
            (
                kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
                kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let spec = if cfg.require_hardware {
            dict(&[(hw_enable, Val::Bool(true)), (hw_require, Val::Bool(true))])
        } else {
            dict(&[(hw_enable, Val::Bool(true))])
        };
        let dest = dict(&[
            (fmt_key, Val::I64(cfg.output.fourcc() as i64)),
            (ios_key, Val::EmptyDict),
            (mtl_key, Val::Bool(true)),
        ]);
        let shared = std::sync::Arc::new(Shared {
            out: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: std::sync::Arc::as_ptr(&shared) as *mut c_void,
        };
        let mut out: *mut VTDecompressionSession = std::ptr::null_mut();
        // SAFETY: valid format description and dictionaries; `record` points at a
        // callback with the documented signature and at `shared`, which lives until
        // after the session is invalidated in `Drop`; `out` receives +1.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_opaque()),
                Some(dest.as_opaque()),
                &record,
                NonNull::from(&mut out),
            )
        };
        check(status, "VTDecompressionSessionCreate")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "VTDecompressionSessionCreate",
            status: -1,
        })?;
        // SAFETY: Create rule, +1 retained.
        let session: CFRetained<VTDecompressionSession> = unsafe { CFRetained::from_raw(ptr) };
        let hardware = {
            let mut value: *const CFType = std::ptr::null();
            // SAFETY: a session is a VTSession (CFType); `value` receives a +1 retained
            // CFBoolean on success.
            let status = unsafe {
                VTSessionCopyProperty(
                    &session,
                    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                    None,
                    (&mut value as *mut *const CFType).cast(),
                )
            };
            match NonNull::new(value as *mut CFType) {
                Some(p) if status == 0 => {
                    // SAFETY: Copy rule, +1 retained.
                    let v: CFRetained<CFType> = unsafe { CFRetained::from_raw(p) };
                    v.downcast_ref::<CFBoolean>().is_some_and(|b| b.as_bool())
                }
                _ => false,
            }
        };
        Ok(Self {
            session,
            format,
            shared,
            timescale: cfg.timescale,
            hardware,
        })
    }

    /// Whether VideoToolbox picked a hardware decoder for this session.
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Submits one coded sample. Its output (or a pre-roll / failure record) shows up in
    /// [`Self::take`] tagged with `tag`.
    pub fn decode(
        &mut self,
        data: &[u8],
        pts: i64,
        dts: i64,
        duration: Option<i64>,
        tag: u64,
        flags: DecodeFlags,
    ) -> Result<(), GpuError> {
        let timing = CMSampleTimingInfo {
            duration: match duration {
                Some(d) => cm_time(d, self.timescale),
                // SAFETY: immutable constant exported by CoreMedia.
                None => unsafe { objc2_core_media::kCMTimeInvalid },
            },
            presentationTimeStamp: cm_time(pts, self.timescale),
            decodeTimeStamp: cm_time(dts, self.timescale),
        };
        let sample = sample_buffer(data, &self.format, timing)?;
        let mut vt_flags = VTDecodeFrameFlags::empty();
        if flags.no_output {
            vt_flags |= VTDecodeFrameFlags::Frame_DoNotOutputFrame;
        }
        if flags.asynchronous {
            vt_flags |= VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression;
        }
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: valid session and sample buffer; the frame refcon is an opaque integer
        // tag that the callback only converts back; `info` is a valid out-pointer.
        let status = unsafe {
            self.session
                .decode_frame(&sample, vt_flags, tag as usize as *mut c_void, &mut info)
        };
        check(status, "VTDecompressionSessionDecodeFrame")
    }

    /// Emits every delayed frame and waits for asynchronous decodes to finish.
    pub fn flush(&mut self) -> Result<(), GpuError> {
        // SAFETY: valid session; both calls are synchronous with respect to the caller.
        let status = unsafe { self.session.finish_delayed_frames() };
        check(status, "VTDecompressionSessionFinishDelayedFrames")?;
        // SAFETY: as above.
        let status = unsafe { self.session.wait_for_asynchronous_frames() };
        check(status, "VTDecompressionSessionWaitForAsynchronousFrames")
    }

    /// The next callback record, in the order the decoder produced them.
    pub fn take(&mut self) -> Option<VtFrame> {
        self.shared
            .out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Blocks until a callback record is waiting in [`Self::take`] or `timeout` passes;
    /// whether one is. Unlike [`Self::flush`] it leaves the rest of the frames in flight
    /// decoding, so the caller can wait for one picture without draining the session.
    pub fn wait_output(&mut self, timeout: Duration) -> bool {
        let q = self.shared.out.lock().unwrap_or_else(|e| e.into_inner());
        let (q, _) = self
            .shared
            .ready
            .wait_timeout_while(q, timeout, |q| q.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        !q.is_empty()
    }

    /// Number of callback records waiting in [`Self::take`].
    pub fn pending(&self) -> usize {
        self.shared
            .out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        // SAFETY: valid session. Waiting then invalidating guarantees no callback runs
        // after this point, so freeing `shared` afterwards (field drop) is sound.
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
        }
    }
}
