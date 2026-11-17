//! VideoToolbox H.264 compression sessions (H-08). The FFI only: DTS rebasing and the
//! hand-off to a muxer live in `gmclip-media::backend::vt::encode`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTimeFlags,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ColorPrimaries,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_H264EntropyMode, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTH264EntropyMode_CABAC, kVTProfileLevel_H264_High_4_0,
    kVTProfileLevel_H264_High_4_2, kVTProfileLevel_H264_High_5_1,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use super::cf::{Val, check, dict, up};
use super::pixel_buffer::PixelBuffer;
use super::vt_decode::cm_time;
use crate::GpuError;

/// H.264 level of the session (SPEC §6.5.1: 4.0 at 30 fps, 4.2 at 60).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum H264Level {
    High40,
    High42,
    /// YouTube 16:9 1440p (SPEC §6.5.1).
    High51,
}

/// Encoder settings. Defaults are the Reels/TikTok/Shorts preset (SPEC §6.5.1).
#[derive(Clone, Debug)]
pub struct VtEncoderConfig {
    pub width: u32,
    pub height: u32,
    /// Integer frame rate (RenderPlan v0 rates are integers).
    pub fps: u32,
    /// Timescale of the PTS / durations passed to [`VtEncoder::encode`].
    pub timescale: i32,
    pub level: H264Level,
    /// `AverageBitRate`, bits per second.
    pub average_bitrate: u32,
    /// `DataRateLimits`: at most this many bits in any `max_window_s` window.
    pub max_bitrate: u32,
    pub max_window_s: f64,
    /// Keyframe every `gop` frames (and every `gop / fps` seconds).
    pub gop: u32,
    /// `AllowFrameReordering` (B-frames).
    pub b_frames: bool,
    /// `MinAllowedFrameQP`: a quality ceiling, used only by the rate-cap retry ladder
    /// (`gmclip-media::backend::encode::RATE_CAP_LADDER`) when a first encode broke the
    /// 1-s peak — VideoToolbox's rate control overshoots for about a second after a
    /// cut from easy to hard content (H-08 review, measured 35 Mbps).
    pub min_qp: Option<u8>,
    pub require_hardware: bool,
}

impl VtEncoderConfig {
    /// The Reels preset at 30 fps: High@4.0, CABAC, B-frames, closed GOP of 1 s,
    /// VBR 16 Mbps average with a 24 Mbps cap over 1 s.
    pub fn reels(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            fps: 30,
            timescale: 15360,
            level: H264Level::High40,
            average_bitrate: 16_000_000,
            max_bitrate: 24_000_000,
            max_window_s: 1.0,
            gop: 30,
            b_frames: true,
            min_qp: None,
            require_hardware: false,
        }
    }
}

/// One compressed sample: AVCC (4-byte length-prefixed NAL units), in decode order,
/// with VideoToolbox's own timestamps (not yet rebased).
#[derive(Clone, Debug)]
pub struct VtPacket {
    pub data: Vec<u8>,
    pub pts: i64,
    /// `None` when VideoToolbox gave no decode timestamp (no reordering).
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub keyframe: bool,
    /// The `avcC` record of the format description this sample carries.
    pub avcc: Option<Vec<u8>>,
}

struct Shared {
    out: Mutex<VecDeque<Result<VtPacket, i32>>>,
    /// The session timescale; output timestamps are converted to it.
    timescale: i32,
}

/// Status pushed for a frame VideoToolbox dropped (NULL sample, noErr).
const DROPPED: i32 = i32::MIN;
/// Status pushed when a sample buffer could not be read.
const UNREADABLE: i32 = i32::MIN + 1;
/// Status pushed when the callback panicked.
const PANICKED: i32 = i32::MIN + 2;

fn copy_avcc(fd: &CMFormatDescription) -> Option<Vec<u8>> {
    // SAFETY: immutable CFString exported by CoreMedia.
    let key = unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms };
    // SAFETY: valid format description; a Get-rule function whose result the binding
    // retains.
    let atoms = unsafe { fd.extension(key) }?;
    let atoms: CFRetained<CFDictionary> = atoms.downcast::<CFDictionary>().ok()?;
    let name = CFString::from_str("avcC");
    // SAFETY: CFDictionaryGetValue with a CFString key; the returned value (Get rule) is
    // owned by `atoms`, which outlives this use; we retain it before reading.
    let v = unsafe { atoms.value((&*name as *const CFString).cast()) };
    let v = NonNull::new(v as *mut CFType)?;
    // SAFETY: Get rule: retain to own it.
    let v: CFRetained<CFType> = unsafe { CFRetained::retain(v) };
    v.downcast::<CFData>().ok().map(|d| d.to_vec())
}

/// `t` in `timescale` units (exact when the scales divide; rounded otherwise).
fn rescale(t: objc2_core_media::CMTime, timescale: i32) -> i64 {
    if t.timescale == timescale || t.timescale == 0 {
        return t.value;
    }
    let v = i128::from(t.value) * i128::from(timescale);
    let d = i128::from(t.timescale);
    ((2 * v + d) / (2 * d)) as i64
}

fn packet_from_sample(sb: &CMSampleBuffer, timescale: i32) -> Option<VtPacket> {
    // SAFETY: `sb` is a valid sample buffer for the duration of the callback.
    let block = unsafe { sb.data_buffer() }?;
    // SAFETY: valid block buffer.
    let len = unsafe { block.data_length() };
    let mut data = vec![0u8; len];
    if len > 0 {
        // SAFETY: `data` has exactly `len` writable bytes.
        let status = unsafe { block.copy_data_bytes(0, len, NonNull::from(&mut data[..]).cast()) };
        if status != 0 {
            return None;
        }
    }
    // SAFETY: valid sample buffer; plain getters.
    let (pts, dts, dur) = unsafe {
        (
            sb.presentation_time_stamp(),
            sb.decode_time_stamp(),
            sb.duration(),
        )
    };
    let valid = |t: objc2_core_media::CMTime| t.flags.contains(CMTimeFlags::Valid);
    // Keyframe: no attachments, or attachment 0 without NotSync = true.
    // SAFETY: valid sample buffer; Get rule, the binding retains the returned array.
    let keyframe = match unsafe { sb.sample_attachments_array(false) } {
        None => true,
        Some(arr) => {
            // SAFETY: the sample attachments array holds CFDictionaries.
            let arr: CFRetained<CFArray<CFDictionary>> = unsafe { CFRetained::cast_unchecked(arr) };
            match arr.get(0) {
                None => true,
                Some(d) => {
                    // SAFETY: immutable CFString exported by CoreMedia.
                    let k = unsafe { kCMSampleAttachmentKey_NotSync };
                    // SAFETY: CFDictionaryGetValue with a CFString key, Get rule.
                    let v = unsafe { d.value((k as *const CFString).cast()) };
                    match NonNull::new(v as *mut CFType) {
                        None => true,
                        Some(p) => {
                            // SAFETY: Get rule: retain to own it.
                            let v: CFRetained<CFType> = unsafe { CFRetained::retain(p) };
                            !v.downcast_ref::<CFBoolean>().is_some_and(|b| b.as_bool())
                        }
                    }
                }
            }
        }
    };
    if !valid(pts) {
        return None;
    }
    // The configuration record, on sync samples only (where a change could happen).
    let avcc = if keyframe {
        // SAFETY: valid sample buffer.
        unsafe { sb.format_description() }.and_then(|fd| copy_avcc(&fd))
    } else {
        None
    };
    Some(VtPacket {
        data,
        pts: rescale(pts, timescale),
        dts: valid(dts).then(|| rescale(dts, timescale)),
        duration: valid(dur).then(|| rescale(dur, timescale)),
        keyframe,
        avcc,
    })
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    info: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: `refcon` is `Arc::as_ptr` of the `Shared` the `VtEncoder` owns; the Arc
    // is released only after the session is completed and invalidated.
    let shared = unsafe { &*(refcon as *const Shared) };
    let ts = shared.timescale;
    // Never unwind into VideoToolbox's thread.
    let item = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if status != 0 {
            return Err(status);
        }
        let dropped = info.contains(VTEncodeInfoFlags::FrameDropped);
        match NonNull::new(sample) {
            None => Err(DROPPED),
            Some(_) if dropped => Err(DROPPED),
            // SAFETY: the sample buffer is valid for the duration of the callback (+0).
            Some(p) => packet_from_sample(unsafe { p.as_ref() }, ts).ok_or(UNREADABLE),
        }
    }))
    .unwrap_or(Err(PANICKED));
    shared
        .out
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(item);
}

/// A VideoToolbox H.264 compression session.
pub struct VtEncoder {
    session: CFRetained<VTCompressionSession>,
    shared: std::sync::Arc<Shared>,
    timescale: i32,
}

// SAFETY: CF objects with atomic retain/release; every entry point takes `&mut self`,
// so calls into the session never overlap; the output queue is behind a Mutex.
unsafe impl Send for VtEncoder {}

impl std::fmt::Debug for VtEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtEncoder")
            .field("timescale", &self.timescale)
            .finish_non_exhaustive()
    }
}

impl VtEncoder {
    pub fn new(cfg: &VtEncoderConfig) -> Result<Self, GpuError> {
        // SAFETY: immutable CFStrings exported by VideoToolbox.
        let (hw_enable, hw_require) = unsafe {
            (
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            )
        };
        let spec = if cfg.require_hardware {
            dict(&[(hw_enable, Val::Bool(true)), (hw_require, Val::Bool(true))])
        } else {
            dict(&[(hw_enable, Val::Bool(true))])
        };
        let shared = std::sync::Arc::new(Shared {
            out: Mutex::new(VecDeque::new()),
            timescale: cfg.timescale,
        });
        let mut out: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: valid dictionary; the callback has the documented signature and its
        // refcon (`shared`) outlives the session (see Drop); `out` receives +1.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                cfg.width as i32,
                cfg.height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                None,
                None,
                Some(output_callback),
                std::sync::Arc::as_ptr(&shared) as *mut c_void,
                NonNull::from(&mut out),
            )
        };
        check(status, "VTCompressionSessionCreate")?;
        let ptr = NonNull::new(out).ok_or(GpuError::Os {
            step: "VTCompressionSessionCreate",
            status: -1,
        })?;
        // SAFETY: Create rule, +1 retained.
        let session: CFRetained<VTCompressionSession> = unsafe { CFRetained::from_raw(ptr) };
        let enc = Self {
            session,
            shared,
            timescale: cfg.timescale,
        };
        enc.configure(cfg)?;
        // SAFETY: valid, configured session.
        let status = unsafe { enc.session.prepare_to_encode_frames() };
        check(status, "VTCompressionSessionPrepareToEncodeFrames")?;
        Ok(enc)
    }

    fn set(&self, key: &CFString, value: &CFType, step: &'static str) -> Result<(), GpuError> {
        // SAFETY: a compression session is a VTSession; key and value are valid CF
        // objects of the types the property expects (checked by VT, which returns an
        // error status otherwise).
        check(
            unsafe { VTSessionSetProperty(&self.session, key, Some(value)) },
            step,
        )
    }

    fn configure(&self, cfg: &VtEncoderConfig) -> Result<(), GpuError> {
        // SAFETY: immutable CFStrings exported by VideoToolbox / CoreVideo.
        unsafe {
            let level = match cfg.level {
                H264Level::High40 => kVTProfileLevel_H264_High_4_0,
                H264Level::High42 => kVTProfileLevel_H264_High_4_2,
                H264Level::High51 => kVTProfileLevel_H264_High_5_1,
            };
            self.set(
                kVTCompressionPropertyKey_ProfileLevel,
                &up(level),
                "ProfileLevel",
            )?;
            self.set(
                kVTCompressionPropertyKey_H264EntropyMode,
                &up(kVTH264EntropyMode_CABAC),
                "H264EntropyMode",
            )?;
            self.set(
                kVTCompressionPropertyKey_RealTime,
                &up(CFBoolean::new(false)),
                "RealTime",
            )?;
            self.set(
                kVTCompressionPropertyKey_AllowFrameReordering,
                &up(CFBoolean::new(cfg.b_frames)),
                "AllowFrameReordering",
            )?;
            self.set(
                kVTCompressionPropertyKey_AllowOpenGOP,
                &up(CFBoolean::new(false)),
                "AllowOpenGOP",
            )?;
            self.set(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                // Twice the GOP: the grid comes from ForceKeyFrame on every `gop`-th
                // frame (gmclip-media). At `gop` (or `gop + 1`) VideoToolbox also puts
                // its own IDR one frame early on easy content (measured: 29, 30, 59, 60…).
                &up(&*CFNumber::new_i32(cfg.gop as i32 * 2)),
                "MaxKeyFrameInterval",
            )?;
            self.set(
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                // Likewise twice the GOP duration.
                &up(&*CFNumber::new_f64(
                    2.0 * f64::from(cfg.gop) / f64::from(cfg.fps),
                )),
                "MaxKeyFrameIntervalDuration",
            )?;
            self.set(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &up(&*CFNumber::new_i32(cfg.fps as i32)),
                "ExpectedFrameRate",
            )?;
            self.set(
                kVTCompressionPropertyKey_AverageBitRate,
                &up(&*CFNumber::new_i64(i64::from(cfg.average_bitrate))),
                "AverageBitRate",
            )?;
            // DataRateLimits: [bytes, seconds]. (Measured on M1: not enforced in the
            // second after a cut to hard content; the peak is checked downstream.)
            let bytes =
                CFNumber::new_i64((f64::from(cfg.max_bitrate) / 8.0 * cfg.max_window_s) as i64);
            let secs = CFNumber::new_f64(cfg.max_window_s);
            let limits = CFArray::<CFNumber>::from_objects(&[&*bytes, &*secs]);
            self.set(
                kVTCompressionPropertyKey_DataRateLimits,
                &up(&*limits),
                "DataRateLimits",
            )?;
            if let Some(q) = cfg.min_qp {
                self.set(
                    objc2_video_toolbox::kVTCompressionPropertyKey_MinAllowedFrameQP,
                    &up(&*CFNumber::new_i32(i32::from(q))),
                    "MinAllowedFrameQP",
                )?;
            }
            let bt709 = objc2_core_video::kCVImageBufferColorPrimaries_ITU_R_709_2;
            self.set(
                kVTCompressionPropertyKey_ColorPrimaries,
                &up(bt709),
                "ColorPrimaries",
            )?;
            self.set(
                kVTCompressionPropertyKey_TransferFunction,
                &up(objc2_core_video::kCVImageBufferTransferFunction_ITU_R_709_2),
                "TransferFunction",
            )?;
            self.set(
                kVTCompressionPropertyKey_YCbCrMatrix,
                &up(objc2_core_video::kCVImageBufferYCbCrMatrix_ITU_R_709_2),
                "YCbCrMatrix",
            )?;
        }
        Ok(())
    }

    /// Queues one frame. Its packet (if any) shows up in [`Self::take`] in decode order.
    /// `force_keyframe` makes it an IDR (the caller keeps the GOP grid exact).
    pub fn encode(
        &mut self,
        frame: &PixelBuffer,
        pts: i64,
        duration: i64,
        force_keyframe: bool,
    ) -> Result<(), GpuError> {
        // SAFETY: immutable CFString exported by VideoToolbox.
        let force_key = unsafe { objc2_video_toolbox::kVTEncodeFrameOptionKey_ForceKeyFrame };
        let props = force_keyframe.then(|| dict(&[(force_key, Val::Bool(true))]));
        let mut info = VTEncodeInfoFlags::empty();
        // SAFETY: valid session and pixel buffer; VT retains the buffer while it needs
        // it; `info` is a valid out-pointer.
        let status = unsafe {
            self.session.encode_frame(
                frame.raw(),
                cm_time(pts, self.timescale),
                cm_time(duration, self.timescale),
                props.as_ref().map(|d| d.as_opaque()),
                std::ptr::null_mut(),
                &mut info,
            )
        };
        check(status, "VTCompressionSessionEncodeFrame")?;
        if info.contains(VTEncodeInfoFlags::FrameDropped) {
            return Err(GpuError::Os {
                step: "VTCompressionSessionEncodeFrame(frame dropped)",
                status: -1,
            });
        }
        Ok(())
    }

    /// Emits every pending frame (end of stream).
    pub fn finish(&mut self) -> Result<(), GpuError> {
        // SAFETY: valid session; kCMTimeInvalid completes all frames.
        let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        check(status, "VTCompressionSessionCompleteFrames")
    }

    /// The next packet in decode order, or the encoder's error status.
    pub fn take(&mut self) -> Option<Result<VtPacket, GpuError>> {
        let item = self
            .shared
            .out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()?;
        Some(item.map_err(|status| GpuError::Os {
            step: match status {
                DROPPED => "VTCompressionOutputCallback(frame dropped)",
                UNREADABLE => "VTCompressionOutputCallback(unreadable sample)",
                PANICKED => "VTCompressionOutputCallback(panic)",
                _ => "VTCompressionOutputCallback",
            },
            status: i64::from(status),
        }))
    }
}

/// Whether VideoToolbox will open a hardware encoder for H.264 (`hevc: false`) or HEVC
/// on this Mac: a 1280×720 session that requires hardware is created and invalidated at
/// once (the startup capability probe, R-PLAT-1). Exports fall back to a software H.264
/// encoder when this is false.
pub fn hardware_encode_supported(hevc: bool) -> bool {
    // SAFETY: immutable CFStrings exported by VideoToolbox.
    let (hw_enable, hw_require) = unsafe {
        (
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
            kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
        )
    };
    let spec = dict(&[(hw_enable, Val::Bool(true)), (hw_require, Val::Bool(true))]);
    let codec = if hevc {
        kCMVideoCodecType_HEVC
    } else {
        kCMVideoCodecType_H264
    };
    let mut out: *mut VTCompressionSession = std::ptr::null_mut();
    // SAFETY: valid dictionary; no output callback (no frame is ever encoded); `out`
    // receives +1 on success.
    let status = unsafe {
        VTCompressionSession::create(
            None,
            1280,
            720,
            codec,
            Some(spec.as_opaque()),
            None,
            None,
            None,
            std::ptr::null_mut(),
            NonNull::from(&mut out),
        )
    };
    let Some(ptr) = NonNull::new(out) else {
        return false;
    };
    // SAFETY: Create rule, +1 retained; invalidated before it is released.
    let session: CFRetained<VTCompressionSession> = unsafe { CFRetained::from_raw(ptr) };
    // SAFETY: valid session that never received a frame.
    unsafe { session.invalidate() };
    status == 0
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        // SAFETY: valid session. Completing then invalidating guarantees no callback
        // runs afterwards, so `shared` (dropped after this) is never used again.
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}
