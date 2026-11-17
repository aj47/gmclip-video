//! H.264 encode through Microsoft's H.264 encoder MFT (M5; the Windows counterpart of
//! `apple::vt_encode`). The FFI only: Annex B → AVCC, the `avcC` record, DTS and the
//! peak meter live in `gmclip-media::backend::mf`.
//!
//! Tightly packed NV12 in, Annex B access units out (parameter sets in band before every
//! IDR), in decode order. High profile, CABAC, peak-constrained VBR, a forced IDR on the
//! caller's GOP grid. Microsoft's software encoder (a synchronous MFT; what exports use),
//! or the GPU vendor's hardware encoder (NVENC, AMF, Quick Sync: an asynchronous MFT on
//! the shared D3D11 device, driven by its NeedInput / HaveOutput / DrainComplete events,
//! system-memory frames in). Measured on the RTX 5080: NVIDIA's MFT ignores the level
//! (it writes 5.0) and takes no B-frame count, so it does not meet the export presets.

use std::collections::{HashMap, VecDeque};
use std::mem::ManuallyDrop;

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264EncoderMFT, CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMaxBitRate,
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQualityVsSpeed,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncH264CABACEnable,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncMPVLevel,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoMinQP, ICodecAPI, IMFMediaType, IMFSample,
    IMFTransform, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX,
    MFCreateMediaType, MFMediaType_Video, MFNominalRange_16_235, MFSampleExtension_CleanPoint,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
    MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR,
    eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_High,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaEventGenerator, METransformDrainComplete, METransformHaveOutput,
    METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_FLAG_NONE,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::Interface;

use super::mf::{append_bytes, empty_sample, ensure, os, sample_of};
use crate::GpuError;

/// H.264 levels the presets use (High profile).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum H264Level {
    High40,
    High42,
    /// YouTube 16:9 1440p (SPEC §6.5.1).
    High51,
}

impl H264Level {
    fn idc(self) -> u32 {
        match self {
            H264Level::High40 => 40,
            H264Level::High42 => 42,
            H264Level::High51 => 51,
        }
    }
}

/// Encoder settings, field for field `apple::VtEncoderConfig`.
#[derive(Clone, Debug)]
pub struct H264EncoderConfig {
    pub width: u32,
    pub height: u32,
    /// Integer frame rate (the rate-control hint; timestamps are exact).
    pub fps: u32,
    /// Timescale of the PTS / durations passed to [`H264Encoder::encode`].
    pub timescale: i32,
    pub level: H264Level,
    /// Mean bit rate, bits per second.
    pub average_bitrate: u32,
    /// At most this many bits per second over `max_window_s` (the VBV buffer).
    pub max_bitrate: u32,
    pub max_window_s: f64,
    /// Keyframe every `gop` frames.
    pub gop: u32,
    /// B-frames (2, as VideoToolbox's pyramid).
    pub b_frames: bool,
    /// A quality ceiling for the rate-cap retry ladder.
    pub min_qp: Option<u8>,
    /// Refuse a software encoder (unused: only the software MFT is wired).
    pub require_hardware: bool,
}

impl H264EncoderConfig {
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

/// One access unit as the encoder emitted it (decode order).
#[derive(Debug)]
pub struct H264Packet {
    /// Annex B.
    pub data: Vec<u8>,
    /// Presentation time, in the config's timescale (the input's, exactly).
    pub pts: i64,
    pub duration: i64,
    pub keyframe: bool,
}

/// An H.264 encoder session.
pub struct H264Encoder {
    mft: IMFTransform,
    codec: ICodecAPI,
    cfg: H264EncoderConfig,
    out_cap: u32,
    provides: bool,
    /// PTS → (PTS, duration) in the caller's timescale, found again from the output's
    /// 100 ns time (rounded both ways, so mapped back through [`Self::ticks`]).
    times: HashMap<i64, (i64, i64)>,
    packets: VecDeque<H264Packet>,
    frame: Vec<u8>,
    /// The first input time, and how much later the MFT stamps its output.
    first_in: Option<i64>,
    shift: Option<i64>,
    /// A hardware (asynchronous) MFT's events, and the inputs it has asked for.
    events: Option<IMFMediaEventGenerator>,
    need: u32,
    /// The encoder's name (its MFT's friendly name, or Microsoft's software encoder).
    name: String,
    /// B-frames between references the session took.
    b_frames: u32,
    /// The D3D11 device a hardware MFT runs on (kept alive as long as it).
    _device: Option<std::sync::Arc<super::d3d::D3dDevice>>,
}

// SAFETY: the H.264 encoder MFT is free-threaded (MTA); every call takes `&mut self`, and
// `ensure` puts any thread that uses it into the MTA first.
unsafe impl Send for H264Encoder {}

impl std::fmt::Debug for H264Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Encoder")
            .field("cfg", &self.cfg)
            .field("queued", &self.packets.len())
            .finish_non_exhaustive()
    }
}

fn set(
    codec: &ICodecAPI,
    api: &windows::core::GUID,
    v: VARIANT,
    what: &'static str,
) -> Result<(), GpuError> {
    // SAFETY: a live ICodecAPI; `api` and `v` outlive the call.
    unsafe { codec.SetValue(api, &v) }.map_err(|e| os(what, &e))
}

fn ratio(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

impl H264Encoder {
    /// A session on Microsoft's software encoder.
    pub fn new(cfg: &H264EncoderConfig) -> Result<Self, GpuError> {
        Self::open(cfg, false)
    }

    /// A session on the GPU's hardware encoder (`hardware`), else Microsoft's software
    /// encoder.
    pub fn open(cfg: &H264EncoderConfig, hardware: bool) -> Result<Self, GpuError> {
        if !cfg.width.is_multiple_of(2)
            || !cfg.height.is_multiple_of(2)
            || cfg.fps == 0
            || cfg.timescale <= 0
        {
            return Err(GpuError::Unsupported(format!(
                "H.264 encode of {}x{} at {} fps (timescale {})",
                cfg.width, cfg.height, cfg.fps, cfg.timescale
            )));
        }
        ensure()?;
        // SAFETY: MF and COM are up on this thread; every attribute key and value is a
        // live constant or local; the interfaces are released by their wrappers.
        unsafe {
            let (mft, name) = if hardware {
                hardware_encoder(super::d3d::D3dDevice::shared()?.luid())?
            } else {
                (
                    CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| os("CoCreateInstance(H.264 encoder)", &e))?,
                    "Microsoft H.264 (software)".to_owned(),
                )
            };
            let mft: IMFTransform = mft;
            // an asynchronous MFT is driven by its events, once unlocked
            let events = match mft.GetAttributes() {
                Ok(a) if a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 => {
                    a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                        .map_err(|e| os("MF_TRANSFORM_ASYNC_UNLOCK", &e))?;
                    Some(
                        mft.cast::<IMFMediaEventGenerator>()
                            .map_err(|e| os("IMFMediaEventGenerator", &e))?,
                    )
                }
                _ => None,
            };
            // a hardware MFT runs on a D3D11 device (it refuses its types without one)
            let device = if events.is_some() {
                let d = super::d3d::D3dDevice::shared()?;
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, d.manager().as_raw() as usize)
                    .map_err(|e| os("IMFTransform::ProcessMessage(SET_D3D_MANAGER)", &e))?;
                Some(d)
            } else {
                None
            };
            let codec: ICodecAPI = mft.cast().map_err(|e| os("ICodecAPI", &e))?;
            // rate control and structure first: the encoder reads them when the types
            // are set
            set(
                &codec,
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
                "CODECAPI_AVEncCommonRateControlMode",
            )
            .or_else(|_| {
                set(
                    &codec,
                    &CODECAPI_AVEncCommonRateControlMode,
                    VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
                    "CODECAPI_AVEncCommonRateControlMode",
                )
            })?;
            set(
                &codec,
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(cfg.average_bitrate),
                "CODECAPI_AVEncCommonMeanBitRate",
            )?;
            // best effort: older encoders lack some of these; the rate cap is checked
            // on the output either way
            let _ = set(
                &codec,
                &CODECAPI_AVEncCommonMaxBitRate,
                VARIANT::from(cfg.max_bitrate),
                "CODECAPI_AVEncCommonMaxBitRate",
            );
            let _ = set(
                &codec,
                &CODECAPI_AVEncCommonBufferSize,
                VARIANT::from((f64::from(cfg.max_bitrate) * cfg.max_window_s) as u32),
                "CODECAPI_AVEncCommonBufferSize",
            );
            set(
                &codec,
                &CODECAPI_AVEncMPVGOPSize,
                VARIANT::from(cfg.gop.max(1)),
                "CODECAPI_AVEncMPVGOPSize",
            )?;
            // two B-frames as VideoToolbox's; a hardware encoder may take fewer (NVENC's
            // MFT refuses the setting): the count it took is the session's
            let mut b = 0u32;
            for want in if cfg.b_frames {
                [2u32, 1, 0]
            } else {
                [0, 0, 0]
            } {
                if set(
                    &codec,
                    &CODECAPI_AVEncMPVDefaultBPictureCount,
                    VARIANT::from(want),
                    "CODECAPI_AVEncMPVDefaultBPictureCount",
                )
                .is_ok()
                {
                    b = want;
                    break;
                }
                if want == 0 && events.is_none() {
                    return Err(GpuError::Unsupported(
                        "the H.264 encoder took no B-frame count".into(),
                    ));
                }
            }
            let _ = set(
                &codec,
                &CODECAPI_AVEncH264CABACEnable,
                VARIANT::from(true),
                "CODECAPI_AVEncH264CABACEnable",
            );
            let _ = set(
                &codec,
                &CODECAPI_AVEncCommonQualityVsSpeed,
                VARIANT::from(70u32),
                "CODECAPI_AVEncCommonQualityVsSpeed",
            );
            if let Some(q) = cfg.min_qp {
                set(
                    &codec,
                    &CODECAPI_AVEncVideoMinQP,
                    VARIANT::from(u32::from(q)),
                    "CODECAPI_AVEncVideoMinQP",
                )?;
            }
            let output: IMFMediaType =
                MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            output
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                .and_then(|()| output.SetUINT32(&MF_MT_AVG_BITRATE, cfg.average_bitrate))
                .and_then(|()| output.SetUINT64(&MF_MT_FRAME_SIZE, ratio(cfg.width, cfg.height)))
                .and_then(|()| output.SetUINT64(&MF_MT_FRAME_RATE, ratio(cfg.fps, 1)))
                .and_then(|()| output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio(1, 1)))
                .and_then(|()| {
                    output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
                .and_then(|()| {
                    output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
                })
                .map_err(|e| os("H.264 output type", &e))?;
            // the level: Microsoft's encoder takes the preset's; NVIDIA's refuses a type
            // carrying one (and a refused type leaves it unable to take another), so a
            // hardware encoder gets it as a codec property (checked on the SPS)
            if events.is_none() {
                output
                    .SetUINT32(&MF_MT_MPEG2_LEVEL, cfg.level.idc())
                    .map_err(|e| os("H.264 output type (level)", &e))?;
            } else {
                // NVIDIA's takes it as a codec property instead
                let _ = set(
                    &codec,
                    &CODECAPI_AVEncMPVLevel,
                    VARIANT::from(cfg.level.idc()),
                    "CODECAPI_AVEncMPVLevel",
                );
            }
            // BT.709 limited range in the VUI, as the mux's `colr` says
            let _ = output.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            let _ = output.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = output.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = output.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            mft.SetOutputType(0, &output, 0)
                .map_err(|e| os("IMFTransform::SetOutputType(H.264)", &e))?;
            let input: IMFMediaType =
                MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12))
                .and_then(|()| input.SetUINT64(&MF_MT_FRAME_SIZE, ratio(cfg.width, cfg.height)))
                .and_then(|()| input.SetUINT64(&MF_MT_FRAME_RATE, ratio(cfg.fps, 1)))
                .and_then(|()| input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio(1, 1)))
                .and_then(|()| {
                    input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
                .map_err(|e| os("NV12 input type", &e))?;
            mft.SetInputType(0, &input, 0)
                .map_err(|e| os("IMFTransform::SetInputType(NV12)", &e))?;
            let info = mft
                .GetOutputStreamInfo(0)
                .map_err(|e| os("IMFTransform::GetOutputStreamInfo", &e))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                .map_err(|e| os("IMFTransform::ProcessMessage(start)", &e))?;
            Ok(Self {
                mft,
                codec,
                cfg: cfg.clone(),
                out_cap: info.cbSize.max(cfg.width * cfg.height * 3 / 2),
                provides: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                times: HashMap::new(),
                packets: VecDeque::new(),
                frame: Vec::new(),
                first_in: None,
                shift: None,
                events,
                need: 0,
                name,
                b_frames: b,
                _device: device,
            })
        }
    }

    /// The encoder's name (for the export record's notes).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// B-frames between references this session uses (`cfg.b_frames` asks for 2).
    pub fn b_frames(&self) -> u32 {
        self.b_frames
    }

    /// Whether this session runs on the GPU's encoder.
    pub fn is_hardware(&self) -> bool {
        self.events.is_some()
    }

    /// Takes one event of an asynchronous MFT (`wait`: block for it); `false` when none
    /// was queued. NeedInput is counted, HaveOutput collected; DrainComplete is reported
    /// through `drained`.
    fn event(&mut self, wait: bool, drained: &mut bool) -> Result<bool, GpuError> {
        let Some(ev) = &self.events else {
            return Ok(false);
        };
        let flags = if wait {
            MF_EVENT_FLAG_NONE
        } else {
            MF_EVENT_FLAG_NO_WAIT
        };
        // SAFETY: a live event generator; the event is released by its wrapper.
        let e = match unsafe { ev.GetEvent(flags) } {
            Ok(e) => e,
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
            Err(e) => return Err(os("IMFMediaEventGenerator::GetEvent", &e)),
        };
        // SAFETY: a live event.
        let kind = unsafe { e.GetType() }.map_err(|e| os("IMFMediaEvent::GetType", &e))?;
        match kind as i32 {
            k if k == METransformNeedInput.0 => self.need += 1,
            k if k == METransformHaveOutput.0 => {
                self.collect_one()?;
            }
            k if k == METransformDrainComplete.0 => *drained = true,
            _ => {}
        }
        Ok(true)
    }

    /// Handles every queued event without waiting.
    fn pump(&mut self) -> Result<(), GpuError> {
        let mut drained = false;
        while self.event(false, &mut drained)? {}
        Ok(())
    }

    /// 100 ns units as the nearest tick of the config's timescale.
    fn ticks(&self, hns: i64) -> i64 {
        let ts = i128::from(self.cfg.timescale);
        ((i128::from(hns) * ts + 5_000_000).div_euclid(10_000_000)) as i64
    }

    /// `ticks` of the config's timescale as 100 ns units (rounded; mapped back exactly
    /// through `times`).
    fn hns(&self, ticks: i64) -> i64 {
        let ts = i128::from(self.cfg.timescale);
        ((i128::from(ticks) * 10_000_000 + ts / 2).div_euclid(ts)) as i64
    }

    /// Encodes one frame of tightly packed NV12 (`y`: width × height, `uv`: width ×
    /// height / 2) at `pts` for `duration` ticks; `force_key` makes it an IDR.
    pub fn encode(
        &mut self,
        y: &[u8],
        uv: &[u8],
        pts: i64,
        duration: i64,
        force_key: bool,
    ) -> Result<(), GpuError> {
        ensure()?;
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        if y.len() != w * h || uv.len() != w * h / 2 {
            return Err(GpuError::Unsupported(format!(
                "NV12 planes of {} + {} bytes for {w}x{h}",
                y.len(),
                uv.len()
            )));
        }
        self.frame.clear();
        self.frame.extend_from_slice(y);
        self.frame.extend_from_slice(uv);
        let t = self.hns(pts);
        let sample = sample_of(&self.frame, t)?;
        let d = self.hns(pts + duration) - t;
        // SAFETY: a live sample; plain attribute call.
        unsafe { sample.SetSampleDuration(d) }
            .map_err(|e| os("IMFSample::SetSampleDuration", &e))?;
        self.times.insert(pts, (pts, duration));
        self.first_in.get_or_insert(t);
        if force_key {
            set(
                &self.codec,
                &CODECAPI_AVEncVideoForceKeyFrame,
                VARIANT::from(1u32),
                "CODECAPI_AVEncVideoForceKeyFrame",
            )?;
        }
        if self.events.is_some() {
            // the hardware MFT asks for each frame
            let mut drained = false;
            while self.need == 0 {
                self.event(true, &mut drained)?;
            }
            self.need -= 1;
            // SAFETY: a live, configured MFT and sample; the MFT asked for input.
            unsafe { self.mft.ProcessInput(0, &sample, 0) }
                .map_err(|e| os("IMFTransform::ProcessInput(NV12, hardware)", &e))?;
            return self.pump();
        }
        loop {
            // SAFETY: a live, configured MFT and sample.
            match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
                Ok(()) => break,
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    if !self.collect()? {
                        return Err(os("IMFTransform::ProcessInput(NV12, stuck)", &e));
                    }
                }
                Err(e) => return Err(os("IMFTransform::ProcessInput(NV12)", &e)),
            }
        }
        self.collect()?;
        Ok(())
    }

    /// Flushes every frame still in the encoder.
    pub fn finish(&mut self) -> Result<(), GpuError> {
        ensure()?;
        // SAFETY: a live MFT; the drain is answered by the outputs collected next.
        unsafe {
            self.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .and_then(|()| self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0))
        }
        .map_err(|e| os("IMFTransform::ProcessMessage(drain)", &e))?;
        if self.events.is_some() {
            let mut drained = false;
            while !drained {
                self.event(true, &mut drained)?;
            }
            return Ok(());
        }
        self.collect()?;
        Ok(())
    }

    /// The next encoded access unit, in decode order.
    pub fn take(&mut self) -> Option<H264Packet> {
        self.packets.pop_front()
    }

    /// One HaveOutput of a hardware MFT: exactly one access unit (or a type change).
    fn collect_one(&mut self) -> Result<(), GpuError> {
        let sample = if self.provides {
            None
        } else {
            Some(empty_sample(self.out_cap)?)
        };
        let mut db = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: one output buffer for stream 0, with a sample when the MFT wants one;
        // the ManuallyDrop fields are taken exactly once below.
        let r = unsafe { self.mft.ProcessOutput(0, &mut db, &mut status) };
        // SAFETY: taken exactly once, after the call that may have filled them.
        let got = unsafe { ManuallyDrop::take(&mut db[0].pSample) };
        // SAFETY: as above; events are released if the MFT set any.
        drop(unsafe { ManuallyDrop::take(&mut db[0].pEvents) });
        match r {
            Ok(()) => {
                if let Some(s) = got {
                    self.push(&s)?;
                }
                Ok(())
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: a live MFT; the offered type is released by its wrapper.
                unsafe {
                    let t = self
                        .mft
                        .GetOutputAvailableType(0, 0)
                        .map_err(|e| os("IMFTransform::GetOutputAvailableType", &e))?;
                    self.mft
                        .SetOutputType(0, &t, 0)
                        .map_err(|e| os("IMFTransform::SetOutputType(H.264)", &e))?;
                }
                Ok(())
            }
            Err(e) => Err(os("IMFTransform::ProcessOutput(H.264, hardware)", &e)),
        }
    }

    /// Pulls every ready access unit; `true` if any came out.
    fn collect(&mut self) -> Result<bool, GpuError> {
        let mut any = false;
        loop {
            let sample = if self.provides {
                None
            } else {
                Some(empty_sample(self.out_cap)?)
            };
            let mut db = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: one output buffer for stream 0, with a sample when the MFT wants
            // one; the ManuallyDrop fields are taken exactly once below.
            let r = unsafe { self.mft.ProcessOutput(0, &mut db, &mut status) };
            // SAFETY: taken exactly once, after the call that may have filled them.
            let got = unsafe { ManuallyDrop::take(&mut db[0].pSample) };
            // SAFETY: as above; events are released if the MFT set any.
            drop(unsafe { ManuallyDrop::take(&mut db[0].pEvents) });
            match r {
                Ok(()) => {
                    if let Some(s) = got {
                        self.push(&s)?;
                        any = true;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // the encoder settled its output type; take it as offered
                    // SAFETY: a live MFT; the offered type is released by its wrapper.
                    unsafe {
                        let t = self
                            .mft
                            .GetOutputAvailableType(0, 0)
                            .map_err(|e| os("IMFTransform::GetOutputAvailableType", &e))?;
                        self.mft
                            .SetOutputType(0, &t, 0)
                            .map_err(|e| os("IMFTransform::SetOutputType(H.264)", &e))?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(any),
                Err(e) => return Err(os("IMFTransform::ProcessOutput(H.264)", &e)),
            }
        }
    }

    fn push(&mut self, s: &IMFSample) -> Result<(), GpuError> {
        // SAFETY: a live sample; plain attribute reads.
        let (t, key) = unsafe {
            (
                s.GetSampleTime()
                    .map_err(|e| os("IMFSample::GetSampleTime", &e))?,
                s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
            )
        };
        // with B-frames the MFT stamps its output a reorder delay late; the first
        // access unit out is the first frame in, which measures it
        let first_in = *self.first_in.get_or_insert(t);
        let shift = *self.shift.get_or_insert(t - first_in);
        let (pts, duration) = self.times.remove(&self.ticks(t - shift)).ok_or_else(|| {
            GpuError::Unsupported(format!("the H.264 encoder returned an unknown time {t}"))
        })?;
        let mut data = Vec::new();
        append_bytes(s, &mut data)?;
        self.packets.push_back(H264Packet {
            data,
            pts,
            duration,
            keyframe: key,
        });
        Ok(())
    }
}

/// The GPU vendor's H.264 encoder MFT (NV12 → H.264, hardware only) on the adapter
/// `luid` (the D3D11 device's: an encoder on another GPU refuses that device), and its
/// friendly name.
fn hardware_encoder(luid: u64) -> Result<(IMFTransform, String), GpuError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: the type infos are locals; MF fills `list` with `n` activation objects
    // (CoTaskMemAlloc'd), each moved out once below, then the array freed once; the
    // friendly name string is freed after it is copied.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut n,
        )
        .map_err(|e| os("MFTEnumEx(H.264 encoders)", &e))?;
        let items: Vec<Option<IMFActivate>> = (0..n as usize)
            .map(|i| std::ptr::read(list.add(i)))
            .collect();
        if !list.is_null() {
            CoTaskMemFree(Some(list as *const _));
        }
        for a in items.into_iter().flatten() {
            if luid != 0 && a.GetUINT64(&MFT_ENUM_ADAPTER_LUID).is_ok_and(|l| l != luid) {
                continue;
            }
            let mut p = windows::core::PWSTR::null();
            let mut len = 0u32;
            let name = if a
                .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len)
                .is_ok()
            {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            } else {
                "hardware H.264 encoder".to_owned()
            };
            if let Ok(t) = a.ActivateObject::<IMFTransform>() {
                return Ok((t, name));
            }
        }
    }
    Err(GpuError::Unsupported(
        "no hardware H.264 encoder on this PC".into(),
    ))
}
