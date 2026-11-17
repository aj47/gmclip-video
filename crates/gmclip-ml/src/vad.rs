//! Silero VAD v6.2.1 (MIT) on the process's ONNX Runtime (SPEC §9).
//!
//! The model is streamed 512 samples at a time at 16 kHz, each chunk prefixed with the
//! previous chunk's last 64 samples, its LSTM state carried across chunks: the same
//! framing as Silero's own `OnnxWrapper`. Speech probabilities become regions with
//! Silero's `get_speech_timestamps` defaults: on at ≥ 0.5, off below 0.35 held 100 ms,
//! regions shorter than 250 ms dropped, 30 ms of padding each side, overlaps merged.

use gmclip_analysis::blob::{Blob, Column, Kind, Param};
use gmclip_analysis::pcm::VOICE_RATE;
use gmclip_analysis::span::{Span, ms};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::models::{SILERO_VAD_V6_2_1, VerifiedModel};
use crate::runtime::OrtRuntime;
use crate::{CancelToken, MlError};

pub const CHUNK: usize = 512;
pub const CONTEXT: usize = 64;

/// Region rules (Silero's defaults).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct VadParams {
    pub threshold: f32,
    pub neg_threshold: f32,
    pub min_speech_ms: u32,
    pub min_silence_ms: u32,
    pub pad_ms: u32,
}

impl Default for VadParams {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            neg_threshold: 0.35,
            min_speech_ms: 250,
            min_silence_ms: 100,
            pad_ms: 30,
        }
    }
}

/// Speech regions of one buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct VadResult {
    /// One probability per 512-sample chunk.
    pub probs: Vec<f32>,
    pub regions: Vec<Span>,
    pub samples: usize,
}

impl VadResult {
    /// Fraction of the buffer inside speech regions.
    pub fn speech_ratio(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.regions.iter().map(Span::len).sum::<usize>() as f64 / self.samples as f64
    }

    /// The `vad_speech` analysis blob.
    pub fn to_blob(&self, p: &VadParams) -> Blob {
        Blob::new(Kind::Vad)
            .param("rate", Param::Int(i64::from(VOICE_RATE)))
            .param("chunk", Param::Int(CHUNK as i64))
            .param("model", Param::Text(SILERO_VAD_V6_2_1.id.into()))
            .param("threshold", Param::Float(f64::from(p.threshold)))
            .param("neg_threshold", Param::Float(f64::from(p.neg_threshold)))
            .param("min_speech_ms", Param::Int(i64::from(p.min_speech_ms)))
            .param("min_silence_ms", Param::Int(i64::from(p.min_silence_ms)))
            .param("pad_ms", Param::Int(i64::from(p.pad_ms)))
            .column(
                "start",
                Column::I64(self.regions.iter().map(|s| s.start as i64).collect()),
            )
            .column(
                "end",
                Column::I64(self.regions.iter().map(|s| s.end as i64).collect()),
            )
            .column("prob", Column::F32(self.probs.clone()))
    }
}

/// The VAD model session.
pub struct SileroVad {
    session: Session,
}

impl std::fmt::Debug for SileroVad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SileroVad")
    }
}

impl SileroVad {
    pub fn load(rt: &OrtRuntime, model: &VerifiedModel) -> Result<Self, MlError> {
        if model.entry.id != SILERO_VAD_V6_2_1.id {
            return Err(MlError::Invalid("not the Silero VAD model"));
        }
        let _ort = crate::runtime::inside(true);
        let session = Session::builder(rt.environment())
            .and_then(|b| {
                b.with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(ort::Error::<()>::from)
            })
            .and_then(|b| b.with_intra_threads(1).map_err(ort::Error::<()>::from))
            .and_then(|mut b| b.commit_from_file(model.file("silero_vad.onnx")))
            .map_err(|_| MlError::Backend("loading Silero VAD"))?;
        Ok(Self { session })
    }

    /// Speech probability per 512-sample chunk of 16 kHz mono PCM (the last chunk
    /// zero-padded).
    pub fn probabilities(
        &mut self,
        pcm: &[f32],
        cancel: &CancelToken,
    ) -> Result<Vec<f32>, MlError> {
        let mut state = vec![0f32; 2 * 128];
        let mut context = [0f32; CONTEXT];
        let mut out = Vec::with_capacity(pcm.len().div_ceil(CHUNK));
        let mut input = vec![0f32; CONTEXT + CHUNK];
        for (k, chunk) in pcm.chunks(CHUNK).enumerate() {
            if k % 1024 == 0 && cancel.is_cancelled() {
                return Err(MlError::Cancelled);
            }
            // per chunk: the exit waits for one chunk, never the whole track
            let _ort = crate::runtime::inside(false);
            input[..CONTEXT].copy_from_slice(&context);
            input[CONTEXT..CONTEXT + chunk.len()].copy_from_slice(chunk);
            input[CONTEXT + chunk.len()..].fill(0.0);
            context.copy_from_slice(&input[CHUNK..]);
            let x = Tensor::from_array(([1usize, CONTEXT + CHUNK], input.clone()))
                .map_err(|_| MlError::Backend("VAD input"))?;
            let st = Tensor::from_array(([2usize, 1, 128], state.clone()))
                .map_err(|_| MlError::Backend("VAD state"))?;
            let sr = Tensor::from_array(((), vec![i64::from(VOICE_RATE)]))
                .map_err(|_| MlError::Backend("VAD rate"))?;
            let outputs = self
                .session
                .run(ort::inputs!["input" => x, "state" => st, "sr" => sr])
                .map_err(|_| MlError::Backend("VAD run"))?;
            let (_, p) = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|_| MlError::Backend("VAD output"))?;
            out.push(*p.first().ok_or(MlError::Backend("VAD output"))?);
            let (_, s) = outputs["stateN"]
                .try_extract_tensor::<f32>()
                .map_err(|_| MlError::Backend("VAD state output"))?;
            if s.len() != state.len() {
                return Err(MlError::Backend("VAD state shape"));
            }
            state.copy_from_slice(s);
        }
        Ok(out)
    }

    pub fn detect(
        &mut self,
        pcm: &[f32],
        params: &VadParams,
        cancel: &CancelToken,
    ) -> Result<VadResult, MlError> {
        let probs = self.probabilities(pcm, cancel)?;
        let regions = regions(&probs, pcm.len(), params);
        Ok(VadResult {
            probs,
            regions,
            samples: pcm.len(),
        })
    }
}

/// Probabilities → padded, merged speech regions (samples at 16 kHz).
pub fn regions(probs: &[f32], n: usize, p: &VadParams) -> Vec<Span> {
    let rate = VOICE_RATE;
    let min_speech = ms(p.min_speech_ms, rate);
    let min_silence = ms(p.min_silence_ms, rate);
    let pad = ms(p.pad_ms, rate);
    let mut raw: Vec<Span> = Vec::new();
    let mut triggered = false;
    let mut start = 0;
    let mut temp_end: Option<usize> = None;
    for (i, &pr) in probs.iter().enumerate() {
        let at = i * CHUNK;
        if pr >= p.threshold {
            temp_end = None;
            if !triggered {
                triggered = true;
                start = at;
            }
            continue;
        }
        if triggered && pr < p.neg_threshold {
            let te = *temp_end.get_or_insert(at);
            if at - te >= min_silence {
                if te - start > min_speech {
                    raw.push(Span::new(start, te));
                }
                triggered = false;
                temp_end = None;
            }
        }
    }
    if triggered && n.saturating_sub(start) > min_speech {
        raw.push(Span::new(start, n));
    }
    let mut out: Vec<Span> = Vec::new();
    for s in raw {
        let s = Span::new(s.start.saturating_sub(pad), (s.end + pad).min(n));
        match out.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_follow_silero_rules() {
        let p = VadParams::default();
        let c = |n: usize| n * CHUNK;
        // 20 chunks of speech, 2 chunks of dip (64 ms < 100 ms: held), 20 more, silence
        let mut probs = vec![0.0f32; 5];
        probs.extend(vec![0.9; 20]);
        probs.extend(vec![0.1; 2]);
        probs.extend(vec![0.9; 20]);
        probs.extend(vec![0.0; 30]);
        let r = regions(&probs, c(probs.len()), &p);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].start, c(5) - 480);
        assert_eq!(r[0].end, c(47) + 480);
        // a 5-chunk blip (160 ms) is below min_speech
        let mut probs = vec![0.0f32; 10];
        probs.extend(vec![0.9; 5]);
        probs.extend(vec![0.0; 10]);
        assert!(regions(&probs, c(25), &p).is_empty());
        // hysteresis: 0.4 neither starts nor ends speech
        let mut probs = vec![0.9f32; 10];
        probs.extend(vec![0.4; 10]);
        probs.extend(vec![0.0; 10]);
        let r = regions(&probs, c(30), &p);
        assert_eq!(r, vec![Span::new(0, c(20) + 480)]);
        // speech running to the end is closed at n
        let mut probs = vec![0.0f32];
        probs.extend(vec![0.9; 12]);
        let r = regions(&probs, c(13) - 100, &p);
        assert_eq!(r, vec![Span::new(c(1) - 480, c(13) - 100)]);
        let v = VadResult {
            probs: vec![],
            regions: vec![Span::new(0, 50)],
            samples: 100,
        };
        assert_eq!(v.speech_ratio(), 0.5);
    }
}
