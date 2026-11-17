//! Parakeet TDT 0.6B v3 (int8) on ONNX Runtime via parakeet-rs (SPEC §9, M1).
//!
//! parakeet-rs owns the mel front end, the encoder session and the greedy TDT decode;
//! this module owns everything gmclip's model depends on: the windows, the stitching
//! ([`crate::stitch`]), the second decode of speech a window heard no words in
//! ([`crate::holes`]), the times on the source clock, the `asr_revision`, cancellation
//! between windows, and the execution-provider report (R-ML-1) captured from ORT's own
//! session log.

use std::sync::{Arc, Mutex};

use gmclip_analysis::energy::{Energy, Hop};
use gmclip_analysis::pcm::VOICE_RATE;
use gmclip_analysis::silence;
use gmclip_analysis::span::{Span, samples_to_flicks};
use gmclip_model::Flicks;
use ort::logging::LogLevel;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode, Transcriber};

use crate::holes;
use crate::models::{PARAKEET_TDT_V3_INT8, VerifiedModel};
use crate::runtime::OrtRuntime;
use crate::stitch::{self, RawWord, Token};
use crate::{Asr, AsrChunk, AsrModelId, AsrWord, CancelToken, EpReport, MlError};

/// Speech level the input is normalised to before decoding (dBFS).
pub const TARGET_SPEECH_DB: f32 = -20.0;
/// Bumped when hole filling ([`crate::holes`]) changes what a window decodes to.
/// 1 (2026-10-19): speech no word covers is decoded again on its own.
pub const HOLES_VERSION: u32 = 1;

/// The most the input is boosted (dB).
pub const MAX_GAIN_DB: f32 = 30.0;

/// Options. Defaults are SPEC §9's: 30 s windows, 2 s overlap.
#[derive(Clone, Debug)]
pub struct ParakeetOptions {
    pub window_s: u32,
    pub overlap_s: u32,
    /// ORT intra-op threads (default: the machine's available parallelism, at most 8).
    pub intra_threads: usize,
}

impl Default for ParakeetOptions {
    fn default() -> Self {
        Self {
            window_s: 30,
            overlap_s: 2,
            intra_threads: std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4),
        }
    }
}

/// The ASR backend.
pub struct ParakeetAsr {
    model: Mutex<ParakeetTDT>,
    opts: ParakeetOptions,
    revision: String,
    ep_log: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for ParakeetAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParakeetAsr")
            .field("opts", &self.opts)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Whether an ORT log line says where nodes were placed.
fn is_placement_line(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("placed on") || m.contains("node placements") || m.contains("assigned to")
}

impl ParakeetAsr {
    /// Loads the encoder and decoder/joint sessions from a verified model directory.
    pub fn load(
        rt: &OrtRuntime,
        model: &VerifiedModel,
        opts: ParakeetOptions,
    ) -> Result<Self, MlError> {
        if model.entry.id != PARAKEET_TDT_V3_INT8.id {
            return Err(MlError::Invalid("not the Parakeet TDT model"));
        }
        if opts.window_s <= opts.overlap_s || opts.window_s == 0 {
            return Err(MlError::Invalid("window must exceed overlap"));
        }
        let ep_log: Arc<Mutex<Vec<String>>> = Arc::default();
        let loading = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sink = ep_log.clone();
        let still_loading = loading.clone();
        // Session creation logs the placement summary; after that every message is
        // dropped at the first check (the sessions keep their Verbose level for life).
        let logger: ort::logging::LoggerFunction = Arc::new(
            move |_level: LogLevel, _cat: &str, id: &str, _loc: &str, msg: &str| {
                if still_loading.load(std::sync::atomic::Ordering::Relaxed)
                    && is_placement_line(msg)
                    && let Ok(mut v) = sink.lock()
                    && v.len() < 64
                {
                    v.push(format!("[{id}] {msg}"));
                }
            },
        );
        let config = ExecutionConfig::new()
            .with_environment(rt.environment().clone())
            .with_intra_threads(opts.intra_threads)
            .with_inter_threads(1)
            .with_custom_configure(move |b| {
                let b = b
                    .with_logger(logger.clone())
                    .map_err(ort::Error::<()>::from)?;
                b.with_log_level(LogLevel::Verbose)
                    .map_err(ort::Error::<()>::from)
            });
        let joint = config.clone();
        let m = {
            let _ort = crate::runtime::inside(true);
            ParakeetTDT::from_pretrained_with_joint_config(&model.dir, Some(config), Some(joint))
                .map_err(|_| MlError::Backend("loading Parakeet"))?
        };
        loading.store(false, std::sync::atomic::Ordering::Relaxed);
        // every pinned file of the model, not just the encoder
        let mut h = crate::sha256::Sha256::new();
        for f in PARAKEET_TDT_V3_INT8.files {
            h.update(f.name.as_bytes());
            h.update(f.sha256.as_bytes());
        }
        let pins = h.finish_hex();
        let enc = &pins[..12];
        let revision = format!(
            "{}@{enc}+parakeet-rs-0.4.1+{}/w{}o{}/norm1/stitch{}/holes{}",
            PARAKEET_TDT_V3_INT8.id,
            rt.id(),
            opts.window_s,
            opts.overlap_s,
            stitch::STITCH_VERSION,
            HOLES_VERSION
        );
        Ok(Self {
            model: Mutex::new(m),
            opts,
            revision,
            ep_log,
        })
    }

    /// Decodes one window of 16 kHz samples into words (times relative to the window).
    fn decode(&self, pcm: &[f32]) -> Result<Vec<RawWord>, MlError> {
        // parakeet-rs's feature extractor needs at least one STFT frame
        if pcm.len() < 400 {
            return Ok(Vec::new());
        }
        // a panic inside parakeet-rs leaves no state behind worth refusing over
        let mut m = self.model.lock().unwrap_or_else(|p| p.into_inner());
        let _ort = crate::runtime::inside(false);
        let r = m
            .transcribe_samples(pcm.to_vec(), VOICE_RATE, 1, Some(TimestampMode::Tokens))
            .map_err(|_| MlError::Backend("Parakeet decode"))?;
        let tokens: Vec<Token> = r
            .tokens
            .iter()
            .map(|t| Token {
                text: t.text.clone(),
                start_frame: stitch::seconds_to_frame(t.start),
                end_frame: stitch::seconds_to_frame(t.end),
            })
            .collect();
        let mut words = stitch::tokens_to_words(&tokens);
        // a word cannot end past the audio it was decoded from
        for w in &mut words {
            w.span.end = w.span.end.min(pcm.len());
            w.span.start = w.span.start.min(w.span.end);
        }
        Ok(words)
    }

    /// The level normalisation applied before decoding: one gain for a whole buffer
    /// (a whole stream, or the whole region an arbiter probes) that brings its speech
    /// level (95th percentile of 40 ms RMS frames, the §5.5 "speech" level) to
    /// [`TARGET_SPEECH_DB`], boosting by at most [`MAX_GAIN_DB`] and never attenuating.
    /// Parakeet's log-mel front end is not level-invariant near its floor: on AJ's
    /// streams (≈ −30 LUFS) quiet on-screen voices were dropped at the recorded level
    /// and recovered at +6 dB. A buffer with too little sound to measure (under 10 % of
    /// frames within 30 dB of its loudest) is left as recorded, so a mostly silent
    /// buffer does not get its noise floor boosted.
    pub fn speech_gain(pcm: &[f32]) -> f32 {
        let Ok(e) = Energy::at_hop(pcm, VOICE_RATE, Hop::Ms40) else {
            return 1.0;
        };
        let Some(l) = silence::levels(&e, Span::new(0, pcm.len())) else {
            return 1.0;
        };
        let loud = e.db.iter().filter(|&&d| d > l.speech_db - 30.0).count();
        if l.speech_db <= gmclip_analysis::energy::FLOOR_DB + 1.0 || loud * 10 < e.db.len() {
            return 1.0;
        }
        let db = (TARGET_SPEECH_DB - l.speech_db).clamp(0.0, MAX_GAIN_DB);
        10f32.powf(db / 20.0)
    }

    /// Decodes `pcm` scaled by `gain` (scaled per window, never the whole buffer).
    fn decode_gained(&self, pcm: &[f32], gain: f32) -> Result<Vec<RawWord>, MlError> {
        if gain == 1.0 {
            return self.decode(pcm);
        }
        let x: Vec<f32> = pcm.iter().map(|v| v * gain).collect();
        self.decode(&x)
    }

    /// [`Self::decode_gained`], then each hole it leaves ([`holes::find`]) decoded again
    /// on its own and its words added.
    fn decode_filled(&self, pcm: &[f32], gain: f32) -> Result<Vec<RawWord>, MlError> {
        let mut words = self.decode_gained(pcm, gain)?;
        for hole in holes::find(pcm, &words) {
            let s = holes::decode_span(hole, pcm.len());
            let mut heard = self.decode_gained(&pcm[s.start..s.end], gain)?;
            for w in &mut heard {
                w.span = Span::new(w.span.start + s.start, w.span.end + s.start);
            }
            holes::fill(&mut words, hole, heard, pcm.len());
        }
        Ok(words)
    }

    fn to_asr(words: &[RawWord], offset: Flicks) -> Vec<AsrWord> {
        words
            .iter()
            .map(|w| AsrWord {
                text: w.text.clone(),
                start: Flicks(offset.0 + samples_to_flicks(w.span.start as i64, VOICE_RATE).0),
                end: Flicks(offset.0 + samples_to_flicks(w.span.end as i64, VOICE_RATE).0),
                confidence: None,
            })
            .collect()
    }
}

impl Asr for ParakeetAsr {
    fn id(&self) -> AsrModelId {
        AsrModelId(PARAKEET_TDT_V3_INT8.id.to_owned())
    }

    fn revision(&self) -> String {
        self.revision.clone()
    }

    fn transcribe_streaming(
        &self,
        pcm: &[f32],
        offset: Flicks,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(AsrChunk<'_>),
    ) -> Result<Vec<AsrWord>, MlError> {
        let sr = VOICE_RATE as usize;
        let windows = stitch::plan_windows(
            pcm.len(),
            self.opts.window_s as usize * sr,
            self.opts.overlap_s as usize * sr,
        );
        let gain = Self::speech_gain(pcm);
        let hop_reach = sr; // ±1 s around each switch-over
        let mut out: Vec<AsrWord> = Vec::new();
        // the previous window's words (absolute times) and how many of them are spent
        let mut prev: Option<(Vec<RawWord>, usize)> = None;
        let mut final_so_far = offset;
        let mut emit = |i: usize, words: &[RawWord], until: Flicks, out: &mut Vec<AsrWord>| {
            // a word may not start before what an earlier chunk declared final
            let mut fresh: Vec<AsrWord> = Self::to_asr(words, offset)
                .into_iter()
                .filter(|w| w.start >= final_so_far)
                .collect();
            fresh.sort_by_key(|w| (w.start, w.end));
            final_so_far = until.max(final_so_far);
            on_chunk(AsrChunk {
                index: i,
                of: windows.len(),
                words: &fresh,
                final_until: final_so_far,
            });
            out.extend(fresh);
        };
        for (i, w) in windows.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(MlError::Cancelled);
            }
            let mut words = self.decode_filled(&pcm[w.start..w.end], gain)?;
            for x in &mut words {
                x.span = Span::new(x.span.start + w.start, x.span.end + w.start);
            }
            if let Some((p, from)) = prev.take() {
                let (_, hi) = stitch::keep_range(&windows, i - 1);
                let (na, nb) = stitch::seam(&p[from..], &words, hi, hop_reach);
                // final: nothing later starts before the first word the next window adds
                let until = words.get(nb).map_or(w.end, |x| x.span.start);
                let until = Flicks(offset.0 + samples_to_flicks(until as i64, VOICE_RATE).0);
                emit(i - 1, &p[from..from + na], until, &mut out);
                prev = Some((words, nb));
            } else {
                prev = Some((words, 0));
            }
        }
        if let Some((p, from)) = prev {
            let end = Flicks(offset.0 + samples_to_flicks(pcm.len() as i64, VOICE_RATE).0);
            emit(windows.len() - 1, &p[from..], end, &mut out);
        }
        Ok(out)
    }

    fn transcribe_window(
        &self,
        pcm: &[f32],
        offset: Flicks,
        gain: f32,
    ) -> Result<Vec<AsrWord>, MlError> {
        if pcm.len() > self.opts.window_s as usize * VOICE_RATE as usize {
            return Err(MlError::Invalid("window longer than the model window"));
        }
        if !(gain.is_finite() && gain > 0.0) {
            return Err(MlError::Invalid("gain"));
        }
        Ok(Self::to_asr(&self.decode_gained(pcm, gain)?, offset))
    }

    fn input_gain(&self, pcm: &[f32]) -> f32 {
        Self::speech_gain(pcm)
    }

    fn execution_report(&self) -> EpReport {
        let mut lines = vec![format!(
            "requested: CPUExecutionProvider (int8), intra_threads {}",
            self.opts.intra_threads
        )];
        if let Ok(v) = self.ep_log.lock() {
            lines.extend(v.iter().cloned());
        }
        EpReport { lines }
    }
}
