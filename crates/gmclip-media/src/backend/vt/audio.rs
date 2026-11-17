//! AudioToolbox AAC-LC encode (H-09) with the timing rules of SPEC §6.5 step 3: no edit
//! lists in our output, priming compensated by where the mix is fed from. Decode is
//! shared with Windows: [`crate::backend::aac`] (re-exported here).

use gmclip_gpu::apple::{AAC_FRAME, AacEncoder};

use super::media_err;
use crate::backend::encode::AudioSampleSink;
use crate::decode::MediaError;

pub use crate::backend::aac::{AacRandomReader, AacTrackDecoder, asc_from_esds};

/// Encodes the planned mix as AAC-LC with priming compensation and no edit list:
/// the encoder's own priming D (`kAudioConverterPrimeInfo`, 2112 on AudioToolbox) is
/// read at creation, the mix is **fed from sample D onward**, and the first 5 ms of
/// what is fed are faded in. Decoded output sample `j` then lines up with mix sample
/// `j` (lag 0) while the file's audio starts at t = 0; the first D output samples are
/// the encoder's priming (silence ramping into the mix).
#[derive(Debug)]
pub struct AacTrackEncoder {
    enc: AacEncoder,
    asc: Vec<u8>,
    sample_rate: u32,
    channels: usize,
    /// Mix samples still to discard (starts at D).
    to_drop: u64,
    /// Fed samples so far (for the fade).
    fed: u64,
    fade: u64,
    next_pts: i64,
    config_sent: bool,
    /// Mix samples pushed (the track's valid length N; the last packet is padded).
    mix_samples: u64,
    finished: bool,
}

impl AacTrackEncoder {
    pub fn new(sample_rate: u32, channels: u32, bitrate: u32) -> Result<Self, MediaError> {
        let enc = AacEncoder::new(sample_rate, channels, bitrate)
            .map_err(|e| media_err("aac_encoder_create", None, e))?;
        let asc = asc_from_esds(enc.magic_cookie()).ok_or(MediaError {
            step: "aac_encoder_no_asc",
            frame: None,
            source_id: None,
            os_status: None,
        })?;
        let d = u64::from(enc.priming());
        Ok(Self {
            enc,
            asc,
            sample_rate,
            channels: channels as usize,
            to_drop: d,
            fed: 0,
            fade: u64::from(sample_rate) * 5 / 1000,
            next_pts: 0,
            config_sent: false,
            mix_samples: 0,
            finished: false,
        })
    }

    /// The encoder's priming D in samples.
    pub fn priming(&self) -> u32 {
        self.enc.priming()
    }

    pub fn asc(&self) -> &[u8] {
        &self.asc
    }

    /// Mix samples pushed so far: the track's valid length N. Packets cover
    /// `⌈N/1024⌉ · 1024` samples; the writer can record N as the duration.
    pub fn mix_samples(&self) -> u64 {
        self.mix_samples
    }

    /// Feeds the next stretch of the planned mix (interleaved), starting at mix sample 0.
    pub fn push(&mut self, mix: &[f32], sink: &mut dyn AudioSampleSink) -> Result<(), MediaError> {
        if self.finished {
            return Err(MediaError {
                step: "aac_push_after_finish",
                frame: None,
                source_id: None,
                os_status: None,
            });
        }
        let ch = self.channels;
        self.mix_samples += (mix.len() / ch) as u64;
        let frames = (mix.len() / ch) as u64;
        let drop = self.to_drop.min(frames);
        self.to_drop -= drop;
        let mut fed: Vec<f32> = mix[drop as usize * ch..].to_vec();
        if self.fed < self.fade {
            for (k, frame) in fed.chunks_exact_mut(ch).enumerate() {
                let n = self.fed + k as u64;
                if n >= self.fade {
                    break;
                }
                let g = n as f32 / self.fade as f32;
                for s in frame {
                    *s *= g;
                }
            }
        }
        self.fed += (fed.len() / ch) as u64;
        self.enc.push(&fed);
        self.emit(false, sink)
    }

    /// End of the mix: flushes the encoder.
    pub fn finish(&mut self, sink: &mut dyn AudioSampleSink) -> Result<(), MediaError> {
        self.finished = true;
        self.emit(true, sink)
    }

    fn emit(&mut self, eof: bool, sink: &mut dyn AudioSampleSink) -> Result<(), MediaError> {
        let packets = self
            .enc
            .pull(eof)
            .map_err(|e| media_err("aac_encode", None, e))?;
        if !packets.is_empty() && !self.config_sent {
            sink.audio_config(&self.asc, self.sample_rate, self.channels as u32)?;
            self.config_sent = true;
        }
        for p in packets {
            sink.audio_sample(&p, self.next_pts, AAC_FRAME as u32)?;
            self.next_pts += AAC_FRAME as i64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc_from_cookie_layouts() {
        // ffmpeg / AudioToolbox style ES descriptor with an AAC-LC 48 kHz stereo ASC.
        let es = gmclip_gpu::apple::esds_from_asc(&[0x11, 0x90]);
        assert_eq!(asc_from_esds(&es), Some(vec![0x11, 0x90]));
        // Long-form lengths (0x80 0x80 0x80 n).
        let long = [
            0x03, 0x80, 0x80, 0x80, 0x1c, 0, 0, 0, 0x04, 0x80, 0x80, 0x80, 0x14, 0x40, 0x15, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 0x80, 0x80, 0x80, 0x02, 0x12, 0x10,
        ];
        assert_eq!(asc_from_esds(&long), Some(vec![0x12, 0x10]));
        assert_eq!(asc_from_esds(&[0x03, 0x05, 0, 0]), None, "truncated");
        // ES_Descriptor with dependsOn + URL + OCR flags set: optional fields skipped.
        let inner = [
            0x04, 17, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 2, 0x11, 0x90,
        ];
        let mut es = vec![
            0x03,
            (3 + 2 + 3 + 2 + inner.len()) as u8,
            0,
            1,
            0xe0,
            0,
            2,
            2,
            b'a',
            b'b',
            0,
            3,
        ];
        es.extend_from_slice(&inner);
        assert_eq!(asc_from_esds(&es), Some(vec![0x11, 0x90]));
        // Deep nesting of 0x04 descriptors: bounded walk, no stack overflow, None.
        let mut deep = Vec::new();
        for _ in 0..200_000 {
            deep.extend_from_slice(&[0x04, 0x80, 0x80, 0x80, 0x7f]);
        }
        assert_eq!(asc_from_esds(&deep), None);
    }
}
