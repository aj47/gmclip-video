//! AAC-LC decode through Media Foundation's AAC decoder MFT (M5; the twin of
//! `apple::AacDecoder`, same API). The FFI only: priming / edit-list policy lives in
//! `gmclip-media::backend::aac`, shared with macOS.
//!
//! Raw AAC access units in (`MF_MT_AAC_PAYLOAD_TYPE` 0, the AudioSpecificConfig after a
//! `HEAACWAVEINFO` header in `MF_MT_USER_DATA`), 32-bit float interleaved out. Each
//! packet is fed, then output is drained until the MFT asks for more input; a reset is a
//! flush.

use std::mem::ManuallyDrop;

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSAACDecMFT, IMFMediaType, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_MT_USER_DATA, MFAudioFormat_AAC, MFAudioFormat_Float, MFAudioFormat_PCM, MFCreateMediaType,
    MFMediaType_Audio, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use super::mf::{append_bytes, empty_sample, ensure, os, sample_of};
use crate::GpuError;

/// Samples per AAC-LC access unit.
pub const AAC_FRAME: usize = 1024;

/// AAC decoder to interleaved f32.
pub struct AacDecoder {
    mft: IMFTransform,
    channels: usize,
    /// Output bytes per call, for MFTs that do not allocate their own samples.
    out_cap: u32,
    provides: bool,
    pcm: Pcm,
    bytes: Vec<u8>,
}

/// The sample format the decoder hands out.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Pcm {
    F32,
    S16,
}

// SAFETY: the AAC decoder MFT is free-threaded (MTA); every call takes `&mut self`, and
// `ensure` puts any thread that uses it into the MTA first.
unsafe impl Send for AacDecoder {}

impl std::fmt::Debug for AacDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacDecoder")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// `HEAACWAVEINFO` after its `WAVEFORMATEX` (the 12 bytes MF wants before the ASC in
/// `MF_MT_USER_DATA`): raw payload, profile/level unknown (0xFE), no structure type.
fn user_data(asc: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 12];
    v[2] = 0xfe;
    v.extend_from_slice(asc);
    v
}

impl AacDecoder {
    /// `asc` is the AudioSpecificConfig (MP4 `esds` DecoderSpecificInfo).
    pub fn new(asc: &[u8], sample_rate: u32, channels: u32) -> Result<Self, GpuError> {
        ensure()?;
        // SAFETY: MF and COM are up on this thread; every attribute key and value is a
        // live constant or local; the interfaces are released by their wrappers.
        unsafe {
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSAACDecMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| os("CoCreateInstance(AAC decoder)", &e))?;
            let input: IMFMediaType =
                MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate))
                .and_then(|()| input.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0))
                .and_then(|()| input.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0xfe))
                .and_then(|()| input.SetBlob(&MF_MT_USER_DATA, &user_data(asc)))
                .map_err(|e| os("AAC input type", &e))?;
            mft.SetInputType(0, &input, 0)
                .map_err(|e| os("IMFTransform::SetInputType(AAC)", &e))?;
            // the decoder's own offers: float if it has one, else 16-bit PCM (converted)
            let mut chosen: Option<(IMFMediaType, Pcm)> = None;
            for i in 0.. {
                let Ok(t) = mft.GetOutputAvailableType(0, i) else {
                    break;
                };
                let sub = t.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default();
                let bits = t.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).unwrap_or(0);
                let kind = if sub == MFAudioFormat_Float && bits == 32 {
                    Pcm::F32
                } else if sub == MFAudioFormat_PCM && bits == 16 {
                    Pcm::S16
                } else {
                    continue;
                };
                if chosen
                    .as_ref()
                    .is_none_or(|c| c.1 == Pcm::S16 && kind == Pcm::F32)
                {
                    chosen = Some((t, kind));
                }
            }
            let (output, pcm) = chosen.ok_or_else(|| {
                GpuError::Unsupported("the AAC decoder offers no PCM output".into())
            })?;
            mft.SetOutputType(0, &output, 0)
                .map_err(|e| os("IMFTransform::SetOutputType(PCM)", &e))?;
            let info = mft
                .GetOutputStreamInfo(0)
                .map_err(|e| os("IMFTransform::GetOutputStreamInfo", &e))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                .map_err(|e| os("IMFTransform::ProcessMessage(start)", &e))?;
            let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            Ok(Self {
                mft,
                channels: channels as usize,
                // a little over one packet's worth even if the MFT reports less
                out_cap: info
                    .cbSize
                    .max((AAC_FRAME * 2 * channels as usize * 4) as u32),
                provides,
                pcm,
                bytes: Vec::new(),
            })
        }
    }

    /// Decodes one access unit; appends the PCM the decoder hands out (normally 1024
    /// frames) to `out` and returns its frame count.
    ///
    /// Microsoft's decoder answers one packet late (the first packet after a start or a
    /// flush gives 0 frames, each later one the previous packet's), where AudioToolbox
    /// answers at once. The output stream is the same sample for sample; the shared
    /// reader places samples by count, and [`AacDecoder::drain`] gives the last packet's.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, GpuError> {
        ensure()?;
        let input = sample_of(packet, 0)?;
        // SAFETY: a live, configured MFT and sample.
        unsafe { self.mft.ProcessInput(0, &input, 0) }
            .map_err(|e| os("IMFTransform::ProcessInput(AAC)", &e))?;
        self.bytes.clear();
        self.collect()?;
        Ok(self.emit(out))
    }

    /// End of the stream: appends what the decoder still holds (the last packet's
    /// samples) and returns its frame count. The decoder then needs a reset.
    pub fn drain(&mut self, out: &mut Vec<f32>) -> Result<usize, GpuError> {
        ensure()?;
        // SAFETY: a live MFT; a drain is answered by the outputs collected next.
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| os("IMFTransform::ProcessMessage(drain)", &e))?;
        self.bytes.clear();
        self.collect()?;
        Ok(self.emit(out))
    }

    /// Converts `self.bytes` to f32 onto `out`; frames appended.
    fn emit(&mut self, out: &mut Vec<f32>) -> usize {
        let n = match self.pcm {
            Pcm::F32 => {
                let n = self.bytes.len() / 4;
                out.reserve(n);
                out.extend(
                    self.bytes
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
                n
            }
            Pcm::S16 => {
                let n = self.bytes.len() / 2;
                out.reserve(n);
                out.extend(
                    self.bytes
                        .chunks_exact(2)
                        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0),
                );
                n
            }
        };
        n / self.channels
    }

    /// Appends every output the decoder has ready to `self.bytes`.
    fn collect(&mut self) -> Result<(), GpuError> {
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
            // one; the ManuallyDrop fields are dropped exactly once below.
            let r = unsafe { self.mft.ProcessOutput(0, &mut db, &mut status) };
            // SAFETY: taken exactly once, after the call that may have filled them.
            let got = unsafe { ManuallyDrop::take(&mut db[0].pSample) };
            // SAFETY: as above; events are released if the MFT set any.
            drop(unsafe { ManuallyDrop::take(&mut db[0].pEvents) });
            match r {
                Ok(()) => {
                    if let Some(s) = got {
                        append_bytes(&s, &mut self.bytes)?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) => return Err(os("IMFTransform::ProcessOutput(AAC)", &e)),
            }
        }
    }

    /// Forgets decoder state. After a seek, decode at least one packet before the
    /// first one whose output is used and discard its samples (AAC frames overlap).
    pub fn reset(&mut self) -> Result<(), GpuError> {
        ensure()?;
        // SAFETY: a live MFT.
        unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }
            .map_err(|e| os("IMFTransform::ProcessMessage(flush)", &e))
    }
}
