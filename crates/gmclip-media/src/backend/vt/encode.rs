//! VideoToolbox H.264 encode (H-08) feeding a [`VideoSampleSink`] with rebased DTS.

use gmclip_gpu::apple::{PixelBuffer, VtEncoder, VtEncoderConfig, VtPacket};

use super::media_err;
use crate::backend::encode::{
    DtsRebaser, EncodedVideoSample, PeakMeter, RawPacket, VideoSampleSink,
};
use crate::decode::MediaError;

/// A VT compression session plus the DTS rebase.
#[derive(Debug)]
pub struct VtH264Encoder {
    enc: VtEncoder,
    rebase: DtsRebaser,
    /// The `avcC` handed to the sink; a later sync sample carrying a different one
    /// is an error (a mid-stream parameter-set change would otherwise be lost).
    avcc: Option<Vec<u8>>,
    frames_in: u64,
    peak: PeakMeter,
    gop: u64,
    /// Samples handed to the sink.
    pub samples_out: u64,
}

impl VtH264Encoder {
    pub fn new(cfg: &VtEncoderConfig) -> Result<Self, MediaError> {
        Ok(Self {
            enc: VtEncoder::new(cfg).map_err(|e| media_err("vt_encode_create", None, e))?,
            rebase: DtsRebaser::new(
                cfg.b_frames,
                // VT's B=2 pyramid: two frames of reorder delay (measured, H-08).
                if cfg.b_frames {
                    2 * i64::from(cfg.timescale) / i64::from(cfg.fps.max(1))
                } else {
                    0
                },
            ),
            avcc: None,
            frames_in: 0,
            peak: PeakMeter::new(i64::from(cfg.timescale)),
            gop: u64::from(cfg.gop.max(1)),
            samples_out: 0,
        })
    }

    /// The fullest 1-s window so far (decode order), in bits: compare with
    /// `SOCIAL_PEAK_BPS` and retry down `RATE_CAP_LADDER` when over.
    pub fn peak_bits_per_second(&self) -> u64 {
        self.peak.peak()
    }

    /// The reorder delay the rebase measured (stream timescale), once known.
    pub fn dts_delay(&self) -> Option<i64> {
        self.rebase.delay()
    }

    /// Encodes one frame (`frame`'s GPU work must be complete). Every `gop`-th frame
    /// is forced to an IDR, so keyframes sit exactly on the GOP grid (closed GOPs).
    pub fn encode(
        &mut self,
        frame: &PixelBuffer,
        pts: i64,
        duration: i64,
        sink: &mut dyn VideoSampleSink,
    ) -> Result<(), MediaError> {
        self.enc
            .encode(
                frame,
                pts,
                duration,
                self.frames_in.is_multiple_of(self.gop),
            )
            .map_err(|e| media_err("vt_encode", Some(self.frames_in), e))?;
        self.frames_in += 1;
        self.drain(sink)
    }

    /// Flushes the encoder and the rebase.
    pub fn finish(&mut self, sink: &mut dyn VideoSampleSink) -> Result<(), MediaError> {
        self.enc
            .finish()
            .map_err(|e| media_err("vt_encode_finish", None, e))?;
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
            let p: VtPacket = p.map_err(|e| media_err("vt_encode_output", None, e))?;
            match (&self.avcc, &p.avcc) {
                (None, Some(a)) => {
                    sink.video_config(a)?;
                    self.avcc = Some(a.clone());
                }
                (None, None) => {
                    return Err(MediaError {
                        step: "vt_encode_no_avcc",
                        frame: None,
                        source_id: None,
                        os_status: None,
                    });
                }
                (Some(have), Some(a)) if have != a => {
                    return Err(MediaError {
                        step: "vt_encode_avcc_changed",
                        frame: Some(self.samples_out),
                        source_id: None,
                        os_status: None,
                    });
                }
                _ => {}
            }
            let raw = RawPacket {
                data: p.data,
                pts: p.pts,
                dts: p.dts,
                duration: p.duration,
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
