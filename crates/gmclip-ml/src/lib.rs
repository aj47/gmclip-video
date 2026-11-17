//! Local ML (SPEC §9): the ASR trait, Parakeet TDT on ONNX Runtime via parakeet-rs (M1,
//! K4's fallback taken up front), Silero VAD on the same ORT, the word-edge arbiter, the
//! model register/cache, and the R-ML-2 edge harness.
//!
//! - One ORT per process, loaded by absolute path after a hash check ([`runtime`]).
//! - Models load only from the pinned cache ([`models`], R-ML-3, R-LOAD-1/2).
//! - ASR input is the voice track at 16 kHz mono ([`gmclip_analysis::pcm::voice_16k`]).
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_model::Flicks;

pub mod arbiter;
pub mod edgeset;
pub mod embed;
pub mod holes;
pub mod models;
pub mod parakeet;
pub mod runtime;
pub mod sha256;
pub mod stitch;
pub mod tdt;
pub mod vad;
pub mod wer;

/// Identifies an ASR model (the model register row id, SPEC §15.3).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct AsrModelId(pub String);

/// A recognised word (words, never tokens), on the source clock.
#[derive(Clone, PartialEq, Debug)]
pub struct AsrWord {
    pub text: String,
    pub start: Flicks,
    pub end: Flicks,
    /// Parakeet's greedy TDT decode reports no confidence.
    pub confidence: Option<f32>,
}

/// Which execution provider ran which nodes (R-ML-1).
#[derive(Clone, Debug, Default)]
pub struct EpReport {
    pub lines: Vec<String>,
}

/// One finalised chunk of a long transcription: these words will not change.
#[derive(Clone, Debug)]
pub struct AsrChunk<'a> {
    pub index: usize,
    pub of: usize,
    pub words: &'a [AsrWord],
    /// Everything before this time (source clock) is final.
    pub final_until: Flicks,
}

/// Cooperative cancellation.
#[derive(Debug, Default)]
pub struct CancelToken(AtomicBool);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// ML errors. Messages are shape-only (no paths, no user text).
#[derive(Clone, Eq, PartialEq, Debug, thiserror::Error)]
pub enum MlError {
    #[error("cancelled")]
    Cancelled,
    #[error("model not available: {0}")]
    ModelMissing(String),
    #[error("backend failed at {0}")]
    Backend(&'static str),
    #[error("cannot load: {0}")]
    Load(&'static str),
    /// A hash or archive check refused the input (R-LOAD-2).
    #[error("{0}")]
    Refused(&'static str),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("invalid: {0}")]
    Invalid(&'static str),
}

/// A speech recogniser over 16 kHz mono PCM.
pub trait Asr: Send + Sync {
    fn id(&self) -> AsrModelId;

    /// The `asr_revision` its words belong to (R-MODEL-4): model hash, decoder,
    /// runtime, chunking and stitching versions. Same input + same revision = same words.
    fn revision(&self) -> String;

    /// Transcribes a long buffer in overlapping windows, calling `on_chunk` as each
    /// window's words become final. `offset` is the source time of `pcm[0]`.
    fn transcribe_streaming(
        &self,
        pcm_16k_mono: &[f32],
        offset: Flicks,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(AsrChunk<'_>),
    ) -> Result<Vec<AsrWord>, MlError>;

    /// [`Asr::transcribe_streaming`] without the callback.
    fn transcribe(
        &self,
        pcm_16k_mono: &[f32],
        offset: Flicks,
        cancel: &CancelToken,
    ) -> Result<Vec<AsrWord>, MlError> {
        self.transcribe_streaming(pcm_16k_mono, offset, cancel, &mut |_| {})
    }

    /// One short window decoded as a whole (no chunking) at a given input gain: the
    /// arbiter's probe. Callers take the gain once for a whole region with
    /// [`Asr::input_gain`], so probes cut from it are all heard at the same level.
    fn transcribe_window(
        &self,
        pcm_16k_mono: &[f32],
        offset: Flicks,
        gain: f32,
    ) -> Result<Vec<AsrWord>, MlError>;

    /// The input gain this backend would apply to `pcm` (1.0 = as recorded).
    fn input_gain(&self, _pcm_16k_mono: &[f32]) -> f32 {
        1.0
    }

    fn execution_report(&self) -> EpReport;
}
