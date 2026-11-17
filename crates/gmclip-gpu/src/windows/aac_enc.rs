//! AAC-LC encode through Media Foundation's AAC encoder MFT (M5; the twin of
//! `apple::AacEncoder`, same API). The FFI only: the mix, loudness and priming policy
//! live in `gmclip-render`'s export, shared with macOS.
//!
//! 16-bit PCM in (the only input the MFT takes; the f32 mix is rounded and clipped
//! here), raw AAC access units out (`MF_MT_AAC_PAYLOAD_TYPE` 0), one per output sample,
//! at a constant bit rate of 96, 128, 160 or 192 kbit/s (the MFT's only rates). The
//! AudioSpecificConfig is the output type's `MF_MT_USER_DATA` after its 12-byte
//! `HEAACWAVEINFO` tail.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;

use windows::Win32::Media::MediaFoundation::{
    AACMFTEncoder, IMFTransform, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_MT_USER_DATA, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateMediaType, MFMediaType_Audio,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use super::mf::{append_bytes, empty_sample, ensure, os, sample_of};
use crate::GpuError;

/// The encoder's priming D: decoded sample `q` of its output is input sample `q − D`.
/// Microsoft's encoder trims its own delay, so D is 0 (measured through Media
/// Foundation's AAC decoder, which trims nothing: it matches AudioToolbox sample for
/// sample on the Mac's files, `gmclip-media/tests/mf_audio.rs`; `tests/mf_encode.rs`).
pub const AAC_ENCODER_PRIMING: u32 = 0;

/// Frames of PCM handed to the MFT per input sample.
const CHUNK: usize = 1024 * 8;

/// AAC-LC encoder.
pub struct AacEncoder {
    mft: IMFTransform,
    channels: usize,
    asc: Vec<u8>,
    out_cap: u32,
    provides: bool,
    /// Interleaved f32 not yet handed to the MFT.
    queued: Vec<f32>,
    /// Frames handed to the MFT so far (the next input sample's time).
    frames_in: u64,
    rate: u32,
    packets: VecDeque<Vec<u8>>,
    drained: bool,
}

// SAFETY: the AAC encoder MFT is free-threaded (MTA); every call takes `&mut self`, and
// `ensure` puts any thread that uses it into the MTA first.
unsafe impl Send for AacEncoder {}

impl std::fmt::Debug for AacEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacEncoder")
            .field("channels", &self.channels)
            .field("asc", &self.asc)
            .finish_non_exhaustive()
    }
}

impl AacEncoder {
    /// `bitrate` in bits per second: 96000, 128000, 160000 or 192000.
    pub fn new(sample_rate: u32, channels: u32, bitrate: u32) -> Result<Self, GpuError> {
        if !matches!(bitrate, 96_000 | 128_000 | 160_000 | 192_000) {
            return Err(GpuError::Unsupported(format!(
                "AAC bitrate {bitrate} (Media Foundation encodes 96, 128, 160 or 192 kbit/s)"
            )));
        }
        ensure()?;
        // SAFETY: MF and COM are up on this thread; every attribute key and value is a
        // live constant or local; the interfaces are released by their wrappers; the
        // user-data blob is read into a buffer of the size MF reports.
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| os("CoCreateInstance(AAC encoder)", &e))?;
            let output = MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            output
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .and_then(|()| output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC))
                .and_then(|()| output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))
                .and_then(|()| output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate))
                .and_then(|()| output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))
                .and_then(|()| output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8))
                .and_then(|()| output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0))
                // AAC Profile L2 (0x29): LC, up to 48 kHz stereo
                .and_then(|()| output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29))
                .map_err(|e| os("AAC output type", &e))?;
            mft.SetOutputType(0, &output, 0)
                .map_err(|e| os("IMFTransform::SetOutputType(AAC)", &e))?;
            let input = MFCreateMediaType().map_err(|e| os("MFCreateMediaType", &e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .and_then(|()| input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))
                .and_then(|()| input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * channels))
                .and_then(|()| {
                    input.SetUINT32(
                        &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                        2 * channels * sample_rate,
                    )
                })
                .map_err(|e| os("PCM input type", &e))?;
            mft.SetInputType(0, &input, 0)
                .map_err(|e| os("IMFTransform::SetInputType(PCM)", &e))?;
            let current = mft
                .GetOutputCurrentType(0)
                .map_err(|e| os("IMFTransform::GetOutputCurrentType(AAC)", &e))?;
            let n = current
                .GetBlobSize(&MF_MT_USER_DATA)
                .map_err(|e| os("MF_MT_USER_DATA", &e))?;
            let mut ud = vec![0u8; n as usize];
            current
                .GetBlob(&MF_MT_USER_DATA, &mut ud, None)
                .map_err(|e| os("MF_MT_USER_DATA", &e))?;
            let asc = ud
                .get(12..)
                .filter(|a| a.len() >= 2)
                .ok_or_else(|| GpuError::Unsupported("the AAC encoder gave no ASC".into()))?
                .to_vec();
            let info = mft
                .GetOutputStreamInfo(0)
                .map_err(|e| os("IMFTransform::GetOutputStreamInfo", &e))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                .map_err(|e| os("IMFTransform::ProcessMessage(start)", &e))?;
            Ok(Self {
                mft,
                channels: channels as usize,
                asc,
                out_cap: info.cbSize.max(8192),
                provides: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                queued: Vec::new(),
                frames_in: 0,
                rate: sample_rate,
                packets: VecDeque::new(),
                drained: false,
            })
        }
    }

    /// The encoder's priming (leading) frames D.
    pub fn priming(&self) -> u32 {
        AAC_ENCODER_PRIMING
    }

    /// The AudioSpecificConfig of the stream.
    pub fn asc(&self) -> &[u8] {
        &self.asc
    }

    /// Queues interleaved PCM.
    pub fn push(&mut self, pcm: &[f32]) {
        self.queued.extend_from_slice(pcm);
    }

    /// Encodes every packet the queued PCM allows; with `eof`, flushes the tail.
    pub fn pull(&mut self, eof: bool) -> Result<Vec<Vec<u8>>, GpuError> {
        ensure()?;
        let ch = self.channels.max(1);
        let whole = self.queued.len() / (CHUNK * ch) * CHUNK * ch;
        let take = if eof { self.queued.len() } else { whole };
        let pcm: Vec<f32> = self.queued.drain(..take).collect();
        for chunk in pcm.chunks(CHUNK * ch) {
            self.feed(chunk)?;
        }
        if eof && !self.drained {
            self.drained = true;
            // SAFETY: a live MFT; the drain is answered by the outputs collected next.
            unsafe { self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
                .map_err(|e| os("IMFTransform::ProcessMessage(drain)", &e))?;
            self.collect()?;
        }
        Ok(self.packets.drain(..).collect())
    }

    fn feed(&mut self, pcm: &[f32]) -> Result<(), GpuError> {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for &s in pcm {
            let v = (f64::from(s) * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let frames = (pcm.len() / self.channels.max(1)) as u64;
        let time = (self.frames_in * 10_000_000 / u64::from(self.rate.max(1))) as i64;
        let sample = sample_of(&bytes, time)?;
        self.frames_in += frames;
        loop {
            // SAFETY: a live, configured MFT and sample.
            match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
                Ok(()) => break,
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    if !self.collect()? {
                        return Err(os("IMFTransform::ProcessInput(AAC, stuck)", &e));
                    }
                }
                Err(e) => return Err(os("IMFTransform::ProcessInput(PCM)", &e)),
            }
        }
        self.collect()?;
        Ok(())
    }

    /// Pulls every ready packet; `true` if any came out.
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
                        let mut p = Vec::new();
                        append_bytes(&s, &mut p)?;
                        if !p.is_empty() {
                            self.packets.push_back(p);
                            any = true;
                        }
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(any),
                Err(e) => return Err(os("IMFTransform::ProcessOutput(AAC)", &e)),
            }
        }
    }
}
