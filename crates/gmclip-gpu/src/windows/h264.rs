//! Video decode through Media Foundation's decoder MFTs (M5; the Windows counterpart of
//! `apple::vt_decode`). The FFI only: sample bookkeeping, pre-roll and the reorder policy
//! live in `gmclip-media::backend::mf`.
//!
//! H.264 (Annex B access units, parameter sets in band) through Microsoft's H.264
//! decoder, in software or on the GPU; VP9 (whole packets, superframes included) through
//! the system's VP9 decoder, which runs on the GPU only. NV12 out, cropped to the display
//! aperture (1080p streams are coded 1088 rows high). The MFT reorders: frames come out
//! in presentation order, each carrying the sample time it went in with. On the GPU the
//! decoder runs on [`D3dDevice`] and each picture is copied to the CPU (a zero-copy DX12
//! import is next).

use std::collections::{HashSet, VecDeque};
use std::mem::ManuallyDrop;
use std::sync::Arc;

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MFCreateMediaType, MFMediaType_Video, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoFormat_P010,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIBuffer, MF_SA_D3D11_AWARE, MFT_CATEGORY_VIDEO_DECODER,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_VP90,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::core::Interface;

use super::d3d::D3dDevice;
use super::mf::{append_bytes, empty_sample, ensure, os, sample_of};
use crate::GpuError;

/// The codecs Media Foundation decodes here.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VideoCodec {
    H264,
    Vp9,
}

/// One decoded picture: tightly packed NV12 (8-bit) or P010 (10-bit: 16-bit
/// little-endian samples, the 10 bits in the high bits) at the display size.
#[derive(Debug)]
pub struct Nv12Frame {
    /// The sample time the access unit was submitted with.
    pub time: i64,
    pub width: u32,
    pub height: u32,
    /// 8 (NV12) or 10 (P010).
    pub bits: u8,
    /// `width × height` luma samples.
    pub y: Vec<u8>,
    /// `width × height / 2` interleaved CbCr samples.
    pub uv: Vec<u8>,
}

impl Nv12Frame {
    /// A picture asked for with [`H264Decoder::skip_output`]: its planes are empty.
    pub fn skipped(&self) -> bool {
        self.y.is_empty()
    }
}

/// The negotiated output layout.
#[derive(Copy, Clone, Debug, Default)]
struct Layout {
    /// Coded size (the buffer's), stride, and the display area inside it.
    coded_h: u32,
    stride: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// Output sample bytes, for an MFT that does not allocate its own.
    cap: u32,
    provides: bool,
    /// Bytes per sample: 1 (NV12) or 2 (P010).
    bps: usize,
}

/// A video decoder session (H.264 or VP9; the name is its first codec's).
pub struct H264Decoder {
    mft: IMFTransform,
    /// The GPU the decoder runs on, when it does.
    hw: Option<Arc<D3dDevice>>,
    layout: Option<Layout>,
    out: VecDeque<Nv12Frame>,
    bytes: Vec<u8>,
    /// Sample times whose pictures nobody reads (a seek's pre-roll): no copy.
    skip: HashSet<i64>,
}

// SAFETY: the H.264 decoder MFT is free-threaded (MTA); every call takes `&mut self`, and
// `ensure` puts any thread that uses it into the MTA first.
unsafe impl Send for H264Decoder {}

impl std::fmt::Debug for H264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Decoder")
            .field("layout", &self.layout)
            .field("queued", &self.out.len())
            .finish_non_exhaustive()
    }
}

impl H264Decoder {
    /// A software H.264 session for a stream of `width × height` (the container's; the
    /// stream's own SPS wins once decoding starts).
    pub fn new(width: u32, height: u32) -> Result<Self, GpuError> {
        Self::open(VideoCodec::H264, width, height, false)
    }

    /// A session for `codec`, on the GPU (`hardware`) or the CPU. VP9 is GPU only.
    pub fn open(
        codec: VideoCodec,
        width: u32,
        height: u32,
        hardware: bool,
    ) -> Result<Self, GpuError> {
        ensure()?;
        let hw = if hardware {
            Some(D3dDevice::shared()?)
        } else {
            None
        };
        let (mft, subtype): (IMFTransform, _) = match codec {
            VideoCodec::H264 => (
                // SAFETY: plain object creation; MF and COM are up on this thread.
                unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| os("CoCreateInstance(H.264 decoder)", &e))?,
                MFVideoFormat_H264,
            ),
            VideoCodec::Vp9 => {
                if hw.is_none() {
                    return Err(GpuError::Unsupported(
                        "VP9 decodes on the GPU only (Microsoft's VP9 decoder)".into(),
                    ));
                }
                (find_decoder(MFVideoFormat_VP90, "VP9")?, MFVideoFormat_VP90)
            }
        };
        // SAFETY: MF and COM are up on this thread; every attribute key and value is a
        // live constant or local; the interfaces are released by their wrappers; the
        // device manager outlives the MFT (`hw` is kept in `Self`).
        unsafe {
            if let Ok(a) = mft.GetAttributes() {
                // output as soon as the stream allows (still in presentation order)
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
                if hw.is_some() && a.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                    return Err(GpuError::Unsupported(
                        "this decoder cannot run on a D3D11 device".into(),
                    ));
                }
            }
            if let Some(d) = &hw {
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, d.manager().as_raw() as usize)
                    .map_err(|e| os("IMFTransform::ProcessMessage(SET_D3D_MANAGER)", &e))?;
            }
            let input: IMFMediaType =
                MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &subtype))
                .and_then(|()| {
                    input.SetUINT64(
                        &MF_MT_FRAME_SIZE,
                        (u64::from(width) << 32) | u64::from(height),
                    )
                })
                .map_err(|e| os("video input type", &e))?;
            mft.SetInputType(0, &input, 0)
                .map_err(|e| os("IMFTransform::SetInputType(video)", &e))?;
            let mut me = Self {
                mft,
                hw,
                layout: None,
                out: VecDeque::new(),
                bytes: Vec::new(),
                skip: HashSet::new(),
            };
            me.negotiate()?;
            me.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| me.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                .map_err(|e| os("IMFTransform::ProcessMessage(start)", &e))?;
            Ok(me)
        }
    }

    /// Picks the NV12 output the MFT offers and records its layout (again after a
    /// stream change: the SPS's size, cropping and stride).
    fn negotiate(&mut self) -> Result<(), GpuError> {
        // SAFETY: a live MFT; the types are released by their wrappers; the aperture
        // blob is read into a local of its exact size.
        unsafe {
            // the stream's own depth: the first of NV12 / P010 the decoder offers (it lists
            // its native format first; after a stream change, the stream's)
            let mut chosen: Option<(IMFMediaType, usize)> = None;
            for i in 0.. {
                let Ok(t) = self.mft.GetOutputAvailableType(0, i) else {
                    break;
                };
                match t.GetGUID(&MF_MT_SUBTYPE) {
                    Ok(g) if g == MFVideoFormat_NV12 => chosen = Some((t, 1)),
                    Ok(g) if g == MFVideoFormat_P010 => chosen = Some((t, 2)),
                    _ => continue,
                }
                break;
            }
            let (t, bps) = chosen.ok_or_else(|| {
                GpuError::Unsupported("the decoder offers neither NV12 nor P010".into())
            })?;
            self.mft
                .SetOutputType(0, &t, 0)
                .map_err(|e| os("IMFTransform::SetOutputType(NV12/P010)", &e))?;
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (cw, ch) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
            let stride = t
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|s| (s as i32).unsigned_abs())
                .unwrap_or(cw * bps as u32);
            let mut area = MFVideoArea::default();
            let blob = std::slice::from_raw_parts_mut(
                (&raw mut area).cast::<u8>(),
                std::mem::size_of::<MFVideoArea>(),
            );
            let (x, y, w, h) = if t
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None)
                .is_ok()
                && area.Area.cx > 0
                && area.Area.cy > 0
            {
                (
                    area.OffsetX.value.max(0) as u32,
                    area.OffsetY.value.max(0) as u32,
                    area.Area.cx as u32,
                    area.Area.cy as u32,
                )
            } else {
                (0, 0, cw, ch)
            };
            let info = self
                .mft
                .GetOutputStreamInfo(0)
                .map_err(|e| os("IMFTransform::GetOutputStreamInfo", &e))?;
            self.layout = Some(Layout {
                coded_h: ch,
                stride: stride.max(cw * bps as u32),
                x,
                y,
                w,
                h,
                cap: info.cbSize.max(stride.max(cw * bps as u32) * ch * 3 / 2),
                provides: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                bps,
            });
        }
        Ok(())
    }

    /// Submits one Annex B access unit with sample time `time`; pictures it completes
    /// are queued for [`H264Decoder::take`].
    pub fn decode(&mut self, annex_b: &[u8], time: i64) -> Result<(), GpuError> {
        ensure()?;
        let input = sample_of(annex_b, time)?;
        loop {
            // SAFETY: a live, configured MFT and sample.
            match unsafe { self.mft.ProcessInput(0, &input, 0) } {
                Ok(()) => break,
                // full: take what is ready, then try again
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    if !self.collect()? {
                        return Err(os("IMFTransform::ProcessInput(H.264, stuck)", &e));
                    }
                }
                Err(e) => return Err(os("IMFTransform::ProcessInput(H.264)", &e)),
            }
        }
        self.collect()?;
        Ok(())
    }

    /// Every picture the decoder still holds (end of a range); the session then needs a
    /// [`H264Decoder::flush`] before new input from elsewhere in the stream.
    pub fn drain(&mut self) -> Result<(), GpuError> {
        ensure()?;
        // SAFETY: a live MFT; a drain is answered by the outputs collected next.
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| os("IMFTransform::ProcessMessage(drain)", &e))?;
        self.collect()?;
        Ok(())
    }

    /// Forgets every picture in flight (a seek).
    pub fn flush(&mut self) -> Result<(), GpuError> {
        ensure()?;
        self.out.clear();
        self.skip.clear();
        // SAFETY: a live MFT.
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }
            .map_err(|e| os("IMFTransform::ProcessMessage(flush)", &e))
    }

    /// The picture of the access unit submitted with `time` is not read: it comes out of
    /// [`H264Decoder::take`] with empty planes ([`Nv12Frame::skipped`]), without the
    /// GPU-to-CPU copy (a seek's pre-roll decodes up to a GOP of pictures nobody shows).
    pub fn skip_output(&mut self, time: i64) {
        self.skip.insert(time);
    }

    /// The next decoded picture, in presentation order.
    pub fn take(&mut self) -> Option<Nv12Frame> {
        self.out.pop_front()
    }

    /// Pulls every ready picture; `true` if any came out.
    fn collect(&mut self) -> Result<bool, GpuError> {
        let mut any = false;
        loop {
            let layout = self.layout.unwrap_or_default();
            let sample = if layout.provides {
                None
            } else {
                Some(empty_sample(layout.cap)?)
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
                        self.push(&s, layout)?;
                        any = true;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.negotiate()?,
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(any),
                Err(e) => return Err(os("IMFTransform::ProcessOutput(H.264)", &e)),
            }
        }
    }

    /// Crops one output sample to the display area as packed NV12.
    fn push(&mut self, s: &IMFSample, l: Layout) -> Result<(), GpuError> {
        // SAFETY: a live sample.
        let time = unsafe { s.GetSampleTime() }.unwrap_or(0);
        if self.skip.remove(&time) {
            self.out.push_back(Nv12Frame {
                time,
                width: l.w,
                height: l.h,
                bits: if l.bps == 2 { 10 } else { 8 },
                y: Vec::new(),
                uv: Vec::new(),
            });
            return Ok(());
        }
        if let Some(d) = &self.hw {
            // SAFETY: a live sample whose buffer is a DXGI surface (the MFT runs on our
            // device); the texture is released by its wrapper after the copy.
            let (tex, index) = unsafe {
                let b = s
                    .GetBufferByIndex(0)
                    .map_err(|e| os("IMFSample::GetBufferByIndex", &e))?;
                let dx: IMFDXGIBuffer = b.cast().map_err(|e| os("IMFDXGIBuffer", &e))?;
                let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
                dx.GetResource(&ID3D11Texture2D::IID, &mut raw)
                    .map_err(|e| os("IMFDXGIBuffer::GetResource", &e))?;
                let tex = ID3D11Texture2D::from_raw(raw);
                let index = dx
                    .GetSubresourceIndex()
                    .map_err(|e| os("IMFDXGIBuffer::GetSubresourceIndex", &e))?;
                (tex, index)
            };
            let (y, uv) = d.read_nv12(&tex, index, (l.x, l.y, l.w, l.h))?;
            self.out.push_back(Nv12Frame {
                time,
                width: l.w,
                height: l.h,
                bits: if l.bps == 2 { 10 } else { 8 },
                y,
                uv,
            });
            return Ok(());
        }
        self.bytes.clear();
        append_bytes(s, &mut self.bytes)?;
        let (stride, ch) = (l.stride as usize, l.coded_h as usize);
        let need = stride * ch * 3 / 2;
        if self.bytes.len() < need || l.w == 0 {
            return Err(GpuError::Unsupported(format!(
                "an NV12 picture of {} bytes, {need} expected",
                self.bytes.len()
            )));
        }
        let (x, y, w, h) = (l.x as usize, l.y as usize, l.w as usize, l.h as usize);
        let b = l.bps;
        let mut py = Vec::with_capacity(w * h * b);
        for r in y..y + h {
            py.extend_from_slice(&self.bytes[r * stride + x * b..r * stride + (x + w) * b]);
        }
        let uv0 = stride * ch;
        let mut puv = Vec::with_capacity(w * h / 2 * b);
        for r in y / 2..(y + h) / 2 {
            let o = uv0 + r * stride + (x & !1) * b;
            puv.extend_from_slice(&self.bytes[o..o + w * b]);
        }
        self.out.push_back(Nv12Frame {
            time,
            width: l.w,
            height: l.h,
            bits: if b == 2 { 10 } else { 8 },
            y: py,
            uv: puv,
        });
        Ok(())
    }
}

/// The first decoder Media Foundation lists for `subtype` to NV12, hardware ones
/// included (sorted by merit).
fn find_decoder(subtype: windows::core::GUID, name: &str) -> Result<IMFTransform, GpuError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: the type infos are locals; MF fills `list` with `n` activation objects
    // (CoTaskMemAlloc'd), each moved out once below, then the array freed once.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut n,
        )
        .map_err(|e| os("MFTEnumEx", &e))?;
        let items: Vec<Option<IMFActivate>> = (0..n as usize)
            .map(|i| std::ptr::read(list.add(i)))
            .collect();
        if !list.is_null() {
            CoTaskMemFree(Some(list as *const _));
        }
        for a in items.into_iter().flatten() {
            if let Ok(t) = a.ActivateObject::<IMFTransform>() {
                return Ok(t);
            }
        }
    }
    Err(GpuError::Unsupported(format!(
        "no {name} decoder on this PC (Windows' VP9 Video Extensions)"
    )))
}

/// Whether Media Foundation lists a hardware decoder for `subtype` (the startup probe's
/// HEVC and AV1 lines; gmclip does not read those yet).
pub fn hardware_decoder_listed(subtype: windows::core::GUID) -> bool {
    if ensure().is_err() {
        return false;
    }
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: the type info is a local; MF fills `list` with `n` activation objects
    // (CoTaskMemAlloc'd), each released below, then the array freed once.
    unsafe {
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            None,
            &mut list,
            &mut n,
        )
        .is_err()
        {
            return false;
        }
        for i in 0..n as usize {
            drop(std::ptr::read(list.add(i)));
        }
        if !list.is_null() {
            CoTaskMemFree(Some(list as *const _));
        }
    }
    n > 0
}

/// [`hardware_decoder_listed`] by name: `"hevc"` or `"av1"` (anything else: `false`).
pub fn hardware_decoder_listed_for(codec: &str) -> bool {
    use windows::Win32::Media::MediaFoundation::{MFVideoFormat_AV1, MFVideoFormat_HEVC};
    match codec {
        "hevc" => hardware_decoder_listed(MFVideoFormat_HEVC),
        "av1" => hardware_decoder_listed(MFVideoFormat_AV1),
        _ => false,
    }
}
