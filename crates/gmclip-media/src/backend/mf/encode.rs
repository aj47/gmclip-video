//! Media Foundation H.264 encode (M5) feeding a [`VideoSampleSink`], the twin of
//! `vt::encode::VtH264Encoder`: the same `avcC` hand-off, DTS rebase and 1-s peak meter.
//!
//! The MFT emits Annex B in decode order with the parameter sets in band before every
//! IDR. Here they become an `avcC` (taken from the first IDR; a later IDR carrying
//! different ones is an error) and length-prefixed samples without parameter sets or
//! access unit delimiters. The MFT stamps no decode times, so the k-th access unit out
//! gets the k-th frame's input time as its DTS — VideoToolbox's convention — and
//! [`DtsRebaser`] shifts them below the PTS as on macOS.

use std::collections::VecDeque;

use gmclip_gpu::windows::{H264Encoder, H264EncoderConfig};

use super::media_err;
use crate::backend::encode::{
    DtsRebaser, EncodedVideoSample, PeakMeter, RawPacket, VideoSampleSink,
};
use crate::decode::MediaError;

fn bad(step: &'static str, frame: Option<u64>) -> MediaError {
    MediaError {
        step,
        frame,
        source_id: None,
        os_status: None,
    }
}

/// The NAL units of an Annex B buffer (3- or 4-byte start codes), without their start
/// codes or trailing zero bytes.
pub fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |&n| n - 3);
        // the next start code's leading zero (4-byte form) and trailing_zero_8bits
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// An `avcC` record (ISO/IEC 14496-15 §5.3.3.1) for one SPS and one PPS, 4-byte NAL
/// lengths, with the High-profile extension (4:2:0, 8-bit).
pub fn avcc_record(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, MediaError> {
    if sps.len() < 4 || pps.is_empty() || sps.len() > 0xffff || pps.len() > 0xffff {
        return Err(bad("avcc_parameter_sets", None));
    }
    let profile = sps[1];
    let mut v = vec![1, profile, sps[2], sps[3], 0xff, 0xe1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    if matches!(profile, 100 | 110 | 122 | 144) {
        // chroma_format_idc 1, bit depths 8, no SPS extensions
        v.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Ok(v)
}

/// A Media Foundation H.264 session plus the DTS rebase.
#[derive(Debug)]
pub struct MfH264Encoder {
    enc: H264Encoder,
    rebase: DtsRebaser,
    /// The `avcC` handed to the sink.
    avcc: Option<Vec<u8>>,
    frames_in: u64,
    /// Input PTS in submission (presentation) order, not yet given out as a DTS.
    dts: VecDeque<i64>,
    peak: PeakMeter,
    gop: u64,
    /// Samples handed to the sink.
    pub samples_out: u64,
}

impl MfH264Encoder {
    pub fn new(cfg: &H264EncoderConfig) -> Result<Self, MediaError> {
        Ok(Self {
            enc: H264Encoder::new(cfg).map_err(|e| media_err("mf_encode_create", None, &e))?,
            rebase: DtsRebaser::new(
                cfg.b_frames,
                // two B-frames, not a pyramid: one frame of reorder delay (measured)
                if cfg.b_frames {
                    i64::from(cfg.timescale) / i64::from(cfg.fps.max(1))
                } else {
                    0
                },
            ),
            avcc: None,
            frames_in: 0,
            dts: VecDeque::new(),
            peak: PeakMeter::new(i64::from(cfg.timescale)),
            gop: u64::from(cfg.gop.max(1)),
            samples_out: 0,
        })
    }

    /// The fullest 1-s window so far (decode order), in bits.
    pub fn peak_bits_per_second(&self) -> u64 {
        self.peak.peak()
    }

    /// The reorder delay the rebase measured (stream timescale), once known.
    pub fn dts_delay(&self) -> Option<i64> {
        self.rebase.delay()
    }

    /// Encodes one frame of tightly packed NV12. Every `gop`-th frame is forced to an
    /// IDR, so keyframes sit exactly on the GOP grid (closed GOPs).
    pub fn encode(
        &mut self,
        y: &[u8],
        uv: &[u8],
        pts: i64,
        duration: i64,
        sink: &mut dyn VideoSampleSink,
    ) -> Result<(), MediaError> {
        self.enc
            .encode(
                y,
                uv,
                pts,
                duration,
                self.frames_in.is_multiple_of(self.gop),
            )
            .map_err(|e| media_err("mf_encode", Some(self.frames_in), &e))?;
        self.dts.push_back(pts);
        self.frames_in += 1;
        self.drain(sink)
    }

    /// Flushes the encoder and the rebase.
    pub fn finish(&mut self, sink: &mut dyn VideoSampleSink) -> Result<(), MediaError> {
        self.enc
            .finish()
            .map_err(|e| media_err("mf_encode_finish", None, &e))?;
        self.drain(sink)?;
        let mut n = 0u64;
        let peak = &mut self.peak;
        self.rebase.finish(&mut |s: EncodedVideoSample<'_>| {
            n += 1;
            peak.push(s.dts, s.data.len());
            sink.video_sample(&s)
        })?;
        self.samples_out += n;
        Ok(())
    }

    fn drain(&mut self, sink: &mut dyn VideoSampleSink) -> Result<(), MediaError> {
        while let Some(p) = self.enc.take() {
            let mut sps = None;
            let mut pps = None;
            let mut data = Vec::with_capacity(p.data.len());
            for nal in annex_b_nals(&p.data) {
                match nal[0] & 0x1f {
                    7 => sps = Some(nal),
                    8 => pps = Some(nal),
                    9 => {} // access unit delimiter
                    _ => {
                        let n = u32::try_from(nal.len())
                            .map_err(|_| bad("mf_encode_nal_size", Some(self.samples_out)))?;
                        data.extend_from_slice(&n.to_be_bytes());
                        data.extend_from_slice(nal);
                    }
                }
            }
            if let (Some(s), Some(q)) = (sps, pps) {
                let a = avcc_record(s, q)?;
                match &self.avcc {
                    None => {
                        sink.video_config(&a)?;
                        self.avcc = Some(a);
                    }
                    Some(have) if *have != a => {
                        return Err(bad("mf_encode_avcc_changed", Some(self.samples_out)));
                    }
                    Some(_) => {}
                }
            } else if self.avcc.is_none() {
                return Err(bad("mf_encode_no_avcc", None));
            }
            let dts = self
                .dts
                .pop_front()
                .ok_or_else(|| bad("mf_encode_extra_output", Some(self.samples_out)))?;
            let raw = RawPacket {
                data,
                pts: p.pts,
                dts: Some(dts),
                duration: Some(p.duration),
                keyframe: p.keyframe,
            };
            let mut n = 0u64;
            let peak = &mut self.peak;
            self.rebase.push(raw, &mut |s: EncodedVideoSample<'_>| {
                n += 1;
                peak.push(s.dts, s.data.len());
                sink.video_sample(&s)
            })?;
            self.samples_out += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_and_avcc_is_built() {
        let au = [
            0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x67, 100, 0, 40, 0xaa, 0, 0, 1, 0x68, 0xce, 0, 0, 1,
            0x65, 0x88, 0x80, 0,
        ];
        let n = annex_b_nals(&au);
        assert_eq!(
            n,
            [
                &[9, 0xf0][..],
                &[0x67, 100, 0, 40, 0xaa],
                &[0x68, 0xce],
                &[0x65, 0x88, 0x80]
            ]
        );
        let a = avcc_record(n[1], n[2]).unwrap();
        assert_eq!(
            a,
            [
                1, 100, 0, 40, 0xff, 0xe1, 0, 5, 0x67, 100, 0, 40, 0xaa, 1, 0, 2, 0x68, 0xce, 0xfd,
                0xf8, 0xf8, 0
            ]
        );
        // and back: the decoder's parser reads it
        let (len, ps) = super::super::avcc_to_annex_b(&a).unwrap();
        assert_eq!(len, 4);
        assert_eq!(
            ps,
            [0, 0, 0, 1, 0x67, 100, 0, 40, 0xaa, 0, 0, 0, 1, 0x68, 0xce]
        );
        assert!(avcc_record(&[0x67], &[0x68]).is_err());
    }
}
