//! Media Foundation decode and encode (M5, Windows) behind the [`VideoDecoder`] trait; the shared
//! [`crate::backend::frames::FrameReader`] gives it frame-exact random access over a
//! [`crate::backend::SampleSource`], as on macOS.
//!
//! The FFI lives in `gmclip_gpu::windows`; this module is safe code: the `avcC` record
//! and the samples' length-prefixed NAL units become Annex B (parameter sets before
//! every sync sample, so any restart is self-contained), and each picture the MFT hands
//! back — in presentation order, with the time it went in with — is matched to its
//! sample by PTS.

use std::collections::HashMap;

pub mod encode;
pub use encode::MfH264Encoder;

use gmclip_gpu::GpuError;
use gmclip_gpu::windows::{H264Decoder, VideoCodec};

use crate::decode::{
    DecodedFrame, DecoderCaps, EncodedSample, FrameImage, MediaError, SubmitFlags, VideoDecoder,
};
use crate::index::SampleIdx;

pub(crate) fn media_err(step: &'static str, frame: Option<u64>, e: &GpuError) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: e.os_status(),
    }
}

fn bad(step: &'static str) -> MediaError {
    MediaError {
        step,
        frame: None,
        source_id: None,
        os_status: None,
    }
}

const START: [u8; 4] = [0, 0, 0, 1];

/// The `avcC` record's NAL length size and its SPS and PPS as Annex B (ISO/IEC 14496-15
/// §5.3.3.1). Every length is bounds-checked: file data, never trusted.
pub fn avcc_to_annex_b(avcc: &[u8]) -> Result<(usize, Vec<u8>), MediaError> {
    let short = || bad("avcc_truncated");
    if avcc.len() < 7 || avcc[0] != 1 {
        return Err(bad("avcc_version"));
    }
    let nal_len = usize::from(avcc[4] & 3) + 1;
    let mut out = Vec::new();
    let mut i = 5;
    let sets = |count: usize, i: &mut usize, out: &mut Vec<u8>| -> Result<(), MediaError> {
        for _ in 0..count {
            let n = usize::from(u16::from_be_bytes([
                *avcc.get(*i).ok_or_else(short)?,
                *avcc.get(*i + 1).ok_or_else(short)?,
            ]));
            let nal = avcc.get(*i + 2..*i + 2 + n).ok_or_else(short)?;
            out.extend_from_slice(&START);
            out.extend_from_slice(nal);
            *i += 2 + n;
        }
        Ok(())
    };
    let sps = usize::from(avcc[i] & 0x1f);
    i += 1;
    sets(sps, &mut i, &mut out)?;
    let pps = usize::from(*avcc.get(i).ok_or_else(short)?);
    i += 1;
    sets(pps, &mut i, &mut out)?;
    Ok((nal_len, out))
}

/// One length-prefixed sample as Annex B, appended to `out`.
pub fn sample_to_annex_b(data: &[u8], nal_len: usize, out: &mut Vec<u8>) -> Result<(), MediaError> {
    let mut i = 0;
    while i < data.len() {
        let head = data
            .get(i..i + nal_len)
            .ok_or_else(|| bad("nal_length_truncated"))?;
        let n = head.iter().fold(0usize, |a, b| (a << 8) | usize::from(*b));
        let nal = data
            .get(i + nal_len..i + nal_len + n)
            .ok_or_else(|| bad("nal_truncated"))?;
        out.extend_from_slice(&START);
        out.extend_from_slice(nal);
        i += nal_len + n;
    }
    Ok(())
}

/// Where a decoder runs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Accel {
    /// Microsoft's software decoder on the CPU.
    Software,
    /// The GPU (D3D11), pictures copied to the CPU.
    Hardware,
}

/// What a session decodes: H.264 (length-prefixed NAL units made Annex B, parameter
/// sets before every sync sample) or VP9 (packets as they are).
#[derive(Debug)]
enum Stream {
    H264 { nal_len: usize, params: Vec<u8> },
    Vp9,
}

/// A Media Foundation video session as a [`VideoDecoder`]. Pictures come out in
/// presentation order as CPU NV12 planes (`FrameImage::Cpu`, tightly packed).
#[derive(Debug)]
pub struct MfVideoDecoder {
    dec: H264Decoder,
    caps: DecoderCaps,
    stream: Stream,
    /// PTS → (sample, pre-roll) of every submission still to come out.
    inflight: HashMap<i64, (SampleIdx, bool)>,
    buf: Vec<u8>,
    /// The access units since the last sync sample (Annex B, its PTS and sample).
    /// Microsoft's decoder drops non-sync input after a drain until the next sync
    /// sample, where VideoToolbox keeps its references; so after a drain the chain is
    /// replayed as pre-roll before decoding goes on.
    chain: Vec<(Vec<u8>, i64, SampleIdx)>,
    drained: bool,
}

/// The H.264 session the first Windows code used: software.
pub type MfH264Decoder = MfVideoDecoder;

impl MfVideoDecoder {
    /// A software H.264 session for a track with this `avcC` record and container size.
    pub fn new(avcc: &[u8], width: u32, height: u32) -> Result<Self, MediaError> {
        Self::h264(avcc, width, height, Accel::Software)
    }

    /// An H.264 session for a track with this `avcC` record and container size.
    pub fn h264(avcc: &[u8], width: u32, height: u32, accel: Accel) -> Result<Self, MediaError> {
        let (nal_len, params) = avcc_to_annex_b(avcc)?;
        Self::open(
            VideoCodec::H264,
            Stream::H264 { nal_len, params },
            width,
            height,
            accel,
        )
    }

    /// A VP9 session (on the GPU: Microsoft's VP9 decoder has no software mode).
    pub fn vp9(width: u32, height: u32) -> Result<Self, MediaError> {
        Self::open(VideoCodec::Vp9, Stream::Vp9, width, height, Accel::Hardware)
    }

    fn open(
        codec: VideoCodec,
        stream: Stream,
        width: u32,
        height: u32,
        accel: Accel,
    ) -> Result<Self, MediaError> {
        let dec = H264Decoder::open(codec, width, height, accel == Accel::Hardware)
            .map_err(|e| media_err("mf_session_create", None, &e))?;
        Ok(Self {
            dec,
            caps: DecoderCaps {
                hardware: accel == Accel::Hardware,
                max_sessions: 3,
            },
            stream,
            inflight: HashMap::new(),
            buf: Vec::new(),
            chain: Vec::new(),
            drained: false,
        })
    }

    /// After a drain: a fresh start, then the chain again as pre-roll (its pictures are
    /// decoded for reference and never handed out).
    fn replay(&mut self) -> Result<(), MediaError> {
        self.dec
            .flush()
            .map_err(|e| media_err("mf_flush", None, &e))?;
        self.drained = false;
        for (data, pts, sample) in &self.chain {
            self.inflight.insert(*pts, (*sample, true));
            self.dec.skip_output(*pts);
            self.dec
                .decode(data, *pts)
                .map_err(|e| media_err("mf_replay", Some(u64::from(sample.0)), &e))?;
        }
        Ok(())
    }
}

impl VideoDecoder for MfVideoDecoder {
    fn submit(&mut self, s: &EncodedSample<'_>, flags: SubmitFlags) -> Result<(), MediaError> {
        let frame = Some(u64::from(s.sample.0));
        self.buf.clear();
        match &self.stream {
            Stream::H264 { nal_len, params } => {
                if s.sync {
                    self.buf.extend_from_slice(params);
                }
                sample_to_annex_b(s.data, *nal_len, &mut self.buf)
                    .map_err(|e| MediaError { frame, ..e })?;
            }
            Stream::Vp9 => self.buf.extend_from_slice(s.data),
        }
        if s.sync {
            self.chain.clear();
            // a new chain needs nothing from before the drain
            if self.drained {
                self.dec
                    .flush()
                    .map_err(|e| media_err("mf_flush", frame, &e))?;
                self.drained = false;
            }
        } else if self.drained {
            self.replay()?;
        }
        self.chain.push((self.buf.clone(), s.pts, s.sample));
        self.inflight.insert(s.pts, (s.sample, flags.no_output));
        // pre-roll pictures are never read: the decoder skips their copy to the CPU
        if flags.no_output {
            self.dec.skip_output(s.pts);
        }
        self.dec
            .decode(&self.buf, s.pts)
            .map_err(|e| media_err("mf_decode_submit", frame, &e))
    }

    fn poll(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        while let Some(f) = self.dec.take() {
            let Some((sample, preroll)) = self.inflight.remove(&f.time) else {
                continue; // a picture no submission is waiting for
            };
            if preroll {
                continue;
            }
            return Ok(Some(DecodedFrame {
                sample,
                pts: f.time,
                image: FrameImage::Cpu {
                    y: f.y,
                    uv: f.uv,
                    stride: [
                        f.width * u32::from(f.bits.div_ceil(8)),
                        f.width * u32::from(f.bits.div_ceil(8)),
                    ],
                    bits: f.bits,
                },
            }));
        }
        Ok(None)
    }

    fn flush(&mut self) -> Result<(), MediaError> {
        self.drained = true;
        self.dec
            .drain()
            .map_err(|e| media_err("mf_drain", None, &e))
    }

    fn caps(&self) -> &DecoderCaps {
        &self.caps
    }

    fn needs_input_to_output(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_and_samples_become_annex_b() {
        // version 1, profile 100, compat 0, level 40, 4-byte lengths; 1 SPS, 1 PPS
        let avcc = [
            1, 100, 0, 40, 0xff, 0xe1, 0, 3, 0x67, 0xaa, 0xbb, 1, 0, 2, 0x68, 0xcc,
        ];
        let (n, ps) = avcc_to_annex_b(&avcc).unwrap();
        assert_eq!(n, 4);
        assert_eq!(ps, [0, 0, 0, 1, 0x67, 0xaa, 0xbb, 0, 0, 0, 1, 0x68, 0xcc]);
        let mut out = Vec::new();
        sample_to_annex_b(&[0, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 1, 0x06], 4, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0x01, 0, 0, 0, 1, 0x06]);
        // truncation anywhere is an error, never a panic
        for k in 0..avcc.len() {
            let _ = avcc_to_annex_b(&avcc[..k]);
        }
        assert!(sample_to_annex_b(&[0, 0, 0, 9, 1], 4, &mut out).is_err());
        assert!(sample_to_annex_b(&[0, 0], 4, &mut out).is_err());
    }
}
