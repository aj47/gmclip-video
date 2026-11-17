//! AudioToolbox AAC-LC encode (H-09) and decode (H-04) through `AudioConverter`.
//! The FFI only: priming / edit-list policy lives in `gmclip-media::backend::vt::audio`.
//!
//! PCM is interleaved f32 at the stream's sample rate. Encoded packets are raw AAC
//! access units (no ADTS), 1024 frames each.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterGetProperty,
    AudioConverterNew, AudioConverterPrimeInfo, AudioConverterRef, AudioConverterReset,
    AudioConverterSetProperty, kAudioConverterCompressionMagicCookie,
    kAudioConverterDecompressionMagicCookie, kAudioConverterEncodeBitRate,
    kAudioConverterPrimeInfo, kAudioConverterPropertyMaximumOutputPacketSize,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};

use super::cf::check;
use crate::GpuError;

/// AAC frames per packet.
pub const AAC_FRAME: usize = 1024;

/// Returned by the input callbacks to stop a fill that has no more input *for now*
/// (not end of stream). Any non-zero value works; this one is recognisable.
const NO_DATA_YET: i32 = 0x6e6f_6474; // 'nodt'

fn pcm_format(sample_rate: u32, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 4 * channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * channels,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn aac_format(sample_rate: u32, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: kAudioFormatMPEG4AAC,
        mFormatFlags: 2, // kMPEG4Object_AAC_LC
        mBytesPerPacket: 0,
        mFramesPerPacket: AAC_FRAME as u32,
        mBytesPerFrame: 0,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 0,
        mReserved: 0,
    }
}

/// An owned `AudioConverterRef`.
struct Converter(AudioConverterRef);

impl Converter {
    fn new(
        src: AudioStreamBasicDescription,
        dst: AudioStreamBasicDescription,
    ) -> Result<Self, GpuError> {
        let mut conv: AudioConverterRef = std::ptr::null_mut();
        let (mut s, mut d) = (src, dst);
        // SAFETY: both descriptions are valid for the call; `conv` receives a new
        // converter we own (disposed in Drop).
        let status = unsafe {
            AudioConverterNew(
                NonNull::from(&mut s),
                NonNull::from(&mut d),
                NonNull::from(&mut conv),
            )
        };
        check(status, "AudioConverterNew")?;
        if conv.is_null() {
            return Err(GpuError::Os {
                step: "AudioConverterNew",
                status: -1,
            });
        }
        Ok(Self(conv))
    }

    fn get<T: Copy>(&self, id: u32, step: &'static str, mut v: T) -> Result<T, GpuError> {
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `v` is a writable `T` of `size` bytes, the documented type of `id`.
        let status = unsafe {
            AudioConverterGetProperty(
                self.0,
                id,
                NonNull::from(&mut size),
                NonNull::from(&mut v).cast(),
            )
        };
        check(status, step)?;
        Ok(v)
    }

    fn get_bytes(&self, id: u32, step: &'static str) -> Result<Vec<u8>, GpuError> {
        let mut buf = vec![0u8; 4096];
        let mut size = buf.len() as u32;
        // SAFETY: `buf` is writable for `size` bytes; the converter writes at most
        // that many and updates `size`.
        let status = unsafe {
            AudioConverterGetProperty(
                self.0,
                id,
                NonNull::from(&mut size),
                NonNull::from(&mut buf[..]).cast(),
            )
        };
        check(status, step)?;
        buf.truncate(size as usize);
        Ok(buf)
    }

    fn set_bytes(&self, id: u32, data: &[u8], step: &'static str) -> Result<(), GpuError> {
        // SAFETY: `data` is readable for `data.len()` bytes for the call.
        let status = unsafe {
            AudioConverterSetProperty(self.0, id, data.len() as u32, NonNull::from(data).cast())
        };
        check(status, step)
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: we own the converter; nothing uses it after this.
        unsafe { AudioConverterDispose(self.0) };
    }
}

// ---------------------------------------------------------------------------------
// Encode

struct EncInput {
    pcm: Vec<f32>,
    /// Frames consumed from `pcm` by the converter.
    pos: usize,
    channels: usize,
    eof: bool,
}

unsafe extern "C-unwind" fn enc_input(
    _conv: AudioConverterRef,
    io_packets: NonNull<u32>,
    io_data: NonNull<AudioBufferList>,
    _desc: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: `user` is the `EncInput` borrowed mutably by `AacEncoder::pull` for the
    // duration of AudioConverterFillComplexBuffer, which is the only caller.
    let input = unsafe { &mut *(user as *mut EncInput) };
    let avail = input.pcm.len() / input.channels - input.pos;
    // SAFETY: valid out-pointers supplied by AudioToolbox for this call.
    let (packets, list) = unsafe { (&mut *io_packets.as_ptr(), &mut *io_data.as_ptr()) };
    if avail == 0 {
        *packets = 0;
        list.mBuffers[0].mDataByteSize = 0;
        list.mBuffers[0].mData = std::ptr::null_mut();
        return if input.eof { 0 } else { NO_DATA_YET };
    }
    let n = avail.min(*packets as usize);
    let start = input.pos * input.channels;
    list.mNumberBuffers = 1;
    list.mBuffers[0].mNumberChannels = input.channels as u32;
    list.mBuffers[0].mDataByteSize = (n * input.channels * 4) as u32;
    // AudioToolbox may read this memory until the input proc is called again — which
    // can be in a later fill call (AudioConverter.h). `pull` upholds that: a fill
    // only returns after a callback that handed out no data (NO_DATA_YET / EOF), so
    // no pointer into `pcm` is outstanding when `push` next reallocates it; on any
    // error the converter is reset, which drops whatever it held.
    list.mBuffers[0].mData = input.pcm[start..].as_mut_ptr().cast();
    *packets = n as u32;
    input.pos += n;
    0
}

/// AAC-LC encoder.
pub struct AacEncoder {
    conv: Converter,
    input: EncInput,
    max_packet: usize,
    priming: u32,
    cookie: Vec<u8>,
}

// SAFETY: the converter has no thread affinity; every call takes `&mut self`.
unsafe impl Send for AacEncoder {}

impl std::fmt::Debug for AacEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacEncoder")
            .field("priming", &self.priming)
            .finish_non_exhaustive()
    }
}

impl AacEncoder {
    pub fn new(sample_rate: u32, channels: u32, bitrate: u32) -> Result<Self, GpuError> {
        let conv = Converter::new(
            pcm_format(sample_rate, channels),
            aac_format(sample_rate, channels),
        )?;
        // Constant bit rate: the SPEC preset is "AAC-LC 192 kbps" and ffmpeg's aac_at
        // defaults to it; the converter's own default (long-term average) gave 167 kbps
        // on the test mix and may change between macOS versions.
        conv.set_bytes(
            objc2_audio_toolbox::kAudioCodecPropertyBitRateControlMode,
            &objc2_audio_toolbox::kAudioCodecBitRateControlMode_Constant.to_ne_bytes(),
            "kAudioCodecPropertyBitRateControlMode",
        )?;
        conv.set_bytes(
            kAudioConverterEncodeBitRate,
            &bitrate.to_ne_bytes(),
            "kAudioConverterEncodeBitRate",
        )?;
        let prime = conv.get(
            kAudioConverterPrimeInfo,
            "kAudioConverterPrimeInfo",
            AudioConverterPrimeInfo {
                leadingFrames: 0,
                trailingFrames: 0,
            },
        )?;
        let max_packet = conv.get(
            kAudioConverterPropertyMaximumOutputPacketSize,
            "kAudioConverterPropertyMaximumOutputPacketSize",
            0u32,
        )? as usize;
        let cookie = conv.get_bytes(
            kAudioConverterCompressionMagicCookie,
            "kAudioConverterCompressionMagicCookie",
        )?;
        let bitrate_now = conv.get(
            kAudioConverterEncodeBitRate,
            "kAudioConverterEncodeBitRate",
            0u32,
        )?;
        if bitrate_now != bitrate {
            return Err(GpuError::Unsupported(format!(
                "AAC bitrate {bitrate} became {bitrate_now}"
            )));
        }
        Ok(Self {
            conv,
            input: EncInput {
                pcm: Vec::new(),
                pos: 0,
                channels: channels as usize,
                eof: false,
            },
            max_packet: max_packet.max(1536 * channels as usize),
            priming: prime.leadingFrames,
            cookie,
        })
    }

    /// The encoder's priming (leading) frames D, read from `kAudioConverterPrimeInfo`.
    pub fn priming(&self) -> u32 {
        self.priming
    }

    /// The compression magic cookie (an MPEG-4 ES descriptor holding the
    /// AudioSpecificConfig).
    pub fn magic_cookie(&self) -> &[u8] {
        &self.cookie
    }

    /// Queues interleaved PCM.
    pub fn push(&mut self, pcm: &[f32]) {
        // Drop what the converter consumed, then append.
        let consumed = self.input.pos * self.input.channels;
        self.input.pcm.drain(..consumed);
        self.input.pos = 0;
        self.input.pcm.extend_from_slice(pcm);
    }

    /// Encodes every packet the queued PCM allows; with `eof`, flushes the tail.
    pub fn pull(&mut self, eof: bool) -> Result<Vec<Vec<u8>>, GpuError> {
        self.input.eof = eof;
        let mut out = Vec::new();
        loop {
            let mut buf = vec![0u8; self.max_packet];
            let mut list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: self.input.channels as u32,
                    mDataByteSize: buf.len() as u32,
                    mData: buf.as_mut_ptr().cast(),
                }],
            };
            let mut packets: u32 = 1;
            let mut desc = AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            };
            // SAFETY: the input callback gets `&mut self.input` (exclusive for the call);
            // `list` points at `buf` (writable `max_packet` bytes); `desc` has room for
            // the one packet requested.
            let status = unsafe {
                AudioConverterFillComplexBuffer(
                    self.conv.0,
                    Some(enc_input),
                    (&mut self.input as *mut EncInput).cast(),
                    NonNull::from(&mut packets),
                    NonNull::from(&mut list),
                    &mut desc,
                )
            };
            if status != 0 && status != NO_DATA_YET {
                // Drop any input pointer the converter may still hold (see enc_input).
                // SAFETY: valid converter we own.
                unsafe { AudioConverterReset(self.conv.0) };
                self.input.pcm.clear();
                self.input.pos = 0;
                return Err(GpuError::Os {
                    step: "AudioConverterFillComplexBuffer(encode)",
                    status: i64::from(status),
                });
            }
            if packets == 0 {
                break;
            }
            let len = list.mBuffers[0].mDataByteSize as usize;
            buf.truncate(len);
            out.push(buf);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------
// Decode

struct DecInput<'a> {
    packet: Option<&'a [u8]>,
    desc: AudioStreamPacketDescription,
}

unsafe extern "C-unwind" fn dec_input(
    _conv: AudioConverterRef,
    io_packets: NonNull<u32>,
    io_data: NonNull<AudioBufferList>,
    out_desc: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: `user` is the `DecInput` borrowed mutably by `AacDecoder::decode` for the
    // duration of the fill call.
    let input = unsafe { &mut *(user as *mut DecInput<'_>) };
    // SAFETY: valid out-pointers supplied by AudioToolbox for this call.
    let (packets, list) = unsafe { (&mut *io_packets.as_ptr(), &mut *io_data.as_ptr()) };
    match input.packet.take() {
        None => {
            *packets = 0;
            NO_DATA_YET
        }
        Some(p) => {
            list.mNumberBuffers = 1;
            list.mBuffers[0].mNumberChannels = 0;
            list.mBuffers[0].mDataByteSize = p.len() as u32;
            // The converter only reads input memory.
            list.mBuffers[0].mData = p.as_ptr() as *mut c_void;
            input.desc = AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: p.len() as u32,
            };
            if !out_desc.is_null() {
                // SAFETY: AudioToolbox asked for a packet description pointer; `desc`
                // lives in `input`, valid until the fill returns.
                unsafe { *out_desc = &mut input.desc };
            }
            *packets = 1;
            0
        }
    }
}

/// AAC decoder to interleaved f32.
pub struct AacDecoder {
    conv: Converter,
    channels: usize,
}

// SAFETY: the converter has no thread affinity; every call takes `&mut self`.
unsafe impl Send for AacDecoder {}

impl std::fmt::Debug for AacDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacDecoder")
            .field("channels", &self.channels)
            .finish()
    }
}

/// Wraps an AudioSpecificConfig in the MPEG-4 ES descriptor AudioToolbox takes as the
/// AAC decompression magic cookie (the `esds` payload after its FullBox header).
pub fn esds_from_asc(asc: &[u8]) -> Vec<u8> {
    // Single-byte descriptor lengths: the ES descriptor must stay under 128 bytes.
    assert!(
        asc.len() <= 100,
        "AudioSpecificConfig too long for a short-form ES descriptor"
    );
    let mut dsi = vec![0x05, asc.len() as u8];
    dsi.extend_from_slice(asc);
    let mut dcd = vec![
        0x04,
        (13 + dsi.len()) as u8,
        0x40,
        0x15,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    dcd.extend_from_slice(&dsi);
    let mut es = vec![0x03, (3 + dcd.len() + 3) as u8, 0, 0, 0];
    es.extend_from_slice(&dcd);
    es.extend_from_slice(&[0x06, 0x01, 0x02]);
    es
}

impl AacDecoder {
    /// `asc` is the AudioSpecificConfig (MP4 `esds` DecoderSpecificInfo).
    pub fn new(asc: &[u8], sample_rate: u32, channels: u32) -> Result<Self, GpuError> {
        let conv = Converter::new(
            aac_format(sample_rate, channels),
            pcm_format(sample_rate, channels),
        )?;
        conv.set_bytes(
            kAudioConverterDecompressionMagicCookie,
            &esds_from_asc(asc),
            "kAudioConverterDecompressionMagicCookie",
        )?;
        Ok(Self {
            conv,
            channels: channels as usize,
        })
    }

    /// Decodes one access unit; appends its PCM (normally 1024 frames) to `out`.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, GpuError> {
        // Room for 4 packets' worth: one AAC-LC packet (1024 frames) never fills it, so
        // the fill always ends on a callback that hands out no data and AudioToolbox
        // holds no pointer to `packet` afterwards.
        let mut pcm = vec![0f32; AAC_FRAME * 4 * self.channels];
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: self.channels as u32,
                mDataByteSize: (pcm.len() * 4) as u32,
                mData: pcm.as_mut_ptr().cast(),
            }],
        };
        let mut frames: u32 = (AAC_FRAME * 4) as u32;
        let mut input = DecInput {
            packet: Some(packet),
            desc: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            },
        };
        // SAFETY: `input` is exclusively borrowed by the callback for the call; `list`
        // points at `pcm`, writable for `frames · channels` f32s.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.conv.0,
                Some(dec_input),
                (&mut input as *mut DecInput<'_>).cast(),
                NonNull::from(&mut frames),
                NonNull::from(&mut list),
                std::ptr::null_mut(),
            )
        };
        if status != 0 && status != NO_DATA_YET {
            // The packet buffer belongs to the caller: make sure nothing holds it.
            // SAFETY: valid converter we own.
            unsafe { AudioConverterReset(self.conv.0) };
            return Err(GpuError::Os {
                step: "AudioConverterFillComplexBuffer(decode)",
                status: i64::from(status),
            });
        }
        let n = frames as usize;
        out.extend_from_slice(&pcm[..n * self.channels]);
        Ok(n)
    }

    /// End of the stream: what the decoder still holds. AudioToolbox answers each packet
    /// at once, so nothing (the Windows decoder holds one packet).
    pub fn drain(&mut self, _out: &mut Vec<f32>) -> Result<usize, GpuError> {
        Ok(0)
    }

    /// Forgets decoder state. After a seek, decode at least one packet before the
    /// first one whose output is used and discard its samples (AAC frames overlap).
    pub fn reset(&mut self) -> Result<(), GpuError> {
        // SAFETY: valid converter we own.
        check(
            unsafe { AudioConverterReset(self.conv.0) },
            "AudioConverterReset",
        )
    }
}
