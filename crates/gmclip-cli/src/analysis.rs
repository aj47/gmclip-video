//! The CLI's analysis host ([`gmclip_core::AnalysisHost`]): lane D's DSP
//! (gmclip-analysis) and ML (gmclip-ml: Parakeet TDT, Silero VAD, the edge arbiter) over
//! audio decoded by gmclip-media's demuxers and AAC decoder.
//!
//! Everything runs on the voice track at 16 kHz mono (`pcm::voice_16k`); loudness runs
//! on the native samples. Long sources decode in 30 s chunks (each resampled with a
//! 50 ms margin on both sides, so the chunk joins are seamless).
//!
//! Models load from the pinned cache only (R-ML-3); a missing model is `Unsupported`
//! with the reason, never a panic, so a machine without models still analyses energy,
//! waveform and silences.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gmclip_analysis::blob::{self, Blob, Column, Param};
use gmclip_analysis::edges;
use gmclip_analysis::energy::{Energy, Hop};
use gmclip_analysis::fillers::{self, WordSpan};
use gmclip_analysis::pcm::{self, VOICE_RATE};
use gmclip_analysis::peaks::Peaks;
use gmclip_analysis::silence::QuietGrid;
use gmclip_analysis::span::{Span, flicks_to_samples, samples_to_flicks};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, SweepSpan, Transcribed,
};
use gmclip_media::MediaError;
use gmclip_media::SampleIndex;
use gmclip_media::backend::SampleSource;
use gmclip_media::demux::io::{ByteSource, FileSource};
use gmclip_media::demux::{self, Codec, TrackKind};
use gmclip_media::index::SampleIdx;
use gmclip_ml::models::{ModelCache, PARAKEET_TDT_V3_INT8, SILERO_VAD_V6_2_1};
use gmclip_ml::parakeet::{ParakeetAsr, ParakeetOptions};
use gmclip_ml::runtime::OrtRuntime;
use gmclip_ml::vad::{SileroVad, VadParams};
use gmclip_ml::{Asr, AsrWord, CancelToken, MlError};
use gmclip_model::Flicks;
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;
/// The longest span decoded at the native rate (loudness) or listened to in one call.
const MAX_NATIVE_S: i64 = 15 * 60;
/// Context decoded around a measured word edge.
const EDGE_CONTEXT: i64 = SEC * 3 / 2;

fn failed(e: impl std::fmt::Display) -> HostError {
    HostError::Failed(e.to_string().chars().take(120).collect())
}

fn ml_err(e: MlError) -> HostError {
    match e {
        MlError::ModelMissing(_) => HostError::Unsupported(
            "the speech model is not installed (gmclip downloads it on first use; dev: tools/fetch_models.sh)",
        ),
        MlError::Cancelled => HostError::Failed("cancelled".into()),
        other => failed(other),
    }
}

// -------------------------------------------------------------------------------------
// Decoding
// -------------------------------------------------------------------------------------

/// One demuxed track's samples, read from its own file handle.
struct TrackSource {
    file: FileSource,
    index: SampleIndex,
}

impl SampleSource for TrackSource {
    fn index(&self) -> &SampleIndex {
        &self.index
    }

    fn read(&mut self, s: SampleIdx, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        let bad = || MediaError {
            step: "sample_read",
            frame: Some(u64::from(s.0)),
            source_id: None,
            os_status: None,
        };
        let i = s.0 as usize;
        let (Some(&off), Some(&size)) = (self.index.offset.get(i), self.index.size.get(i)) else {
            return Err(bad());
        };
        if size > 16 * 1024 * 1024
            || off
                .checked_add(u64::from(size))
                .is_none_or(|end| end > self.file.len())
        {
            return Err(bad());
        }
        buf.resize(size as usize, 0);
        self.file.read_at(off, buf).map_err(|_| bad())
    }
}

/// An opened audio track.
struct Track {
    #[cfg(any(target_os = "macos", windows))]
    reader: gmclip_media::backend::aac::AacRandomReader<TrackSource>,
    rate: u32,
    channels: usize,
    /// Decodable frames (after priming).
    frames: u64,
}

impl Track {
    fn open(a: &AudioSource) -> Result<Track, HostError> {
        let mut f = FileSource::open(&a.path)
            .map_err(|_| HostError::Failed("the source cannot be opened".into()))?;
        let d = demux::index(&mut f)
            .map_err(|_| HostError::Failed("the source cannot be demuxed".into()))?;
        let audio: Vec<&demux::Track> = d
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        let t = match a.track {
            Some(n) => audio
                .iter()
                .find(|t| t.id == u64::from(n))
                .ok_or(HostError::Failed(
                    "the voice track is not in the file".into(),
                ))?,
            None => audio
                .first()
                .ok_or(HostError::Failed("the source has no audio".into()))?,
        };
        let Codec::Aac { asc } = &t.codec else {
            return Err(HostError::Unsupported("analysis reads AAC audio at M1"));
        };
        // the priming to drop, less an empty edit in front (a recording's files), or
        // silence first when that is the longer
        let (skip, lead) = t.audio_start().ok_or(HostError::Unsupported(
            "a rescaled or multi-segment audio edit list",
        ))?;
        if t.sample_rate == 0 || t.channels == 0 {
            return Err(HostError::Failed("an audio track without a rate".into()));
        }
        let frames = (t.samples.offset.len() as u64 * 1024).saturating_sub(skip + 1024) + lead;
        let file = FileSource::open(&a.path)
            .map_err(|_| HostError::Failed("the source cannot be opened".into()))?;
        let src = TrackSource {
            file,
            index: t.samples.clone(),
        };
        Self::reader(src, asc, t.sample_rate, t.channels, (skip, lead), frames)
    }

    #[cfg(any(target_os = "macos", windows))]
    fn reader(
        src: TrackSource,
        asc: &[u8],
        rate: u32,
        ch: u16,
        (skip, lead): (u64, u64),
        frames: u64,
    ) -> Result<Track, HostError> {
        let reader =
            gmclip_media::backend::aac::AacRandomReader::new(src, asc, rate, u32::from(ch), skip)
                .map_err(|_| HostError::Failed("the AAC decoder refused the track".into()))?
                .with_lead(lead);
        Ok(Track {
            reader,
            rate,
            channels: usize::from(ch),
            frames,
        })
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn reader(
        _: TrackSource,
        _: &[u8],
        _: u32,
        _: u16,
        _: (u64, u64),
        _: u64,
    ) -> Result<Track, HostError> {
        Err(HostError::Unsupported(
            "audio decoding needs macOS or Windows",
        ))
    }

    /// Native interleaved samples of frames `[a, b)`.
    fn read(&mut self, a: u64, b: u64) -> Result<Vec<f32>, HostError> {
        let b = b.min(self.frames);
        if b <= a {
            return Ok(Vec::new());
        }
        #[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_mut))]
        let mut out = vec![0f32; ((b - a) as usize) * self.channels];
        #[cfg(any(target_os = "macos", windows))]
        self.reader
            .read(a, &mut out)
            .map_err(|_| HostError::Failed("the audio cannot be decoded".into()))?;
        Ok(out)
    }

    fn frame_of(&self, t: Flicks) -> u64 {
        u64::try_from(flicks_to_samples(t, self.rate))
            .unwrap_or(0)
            .min(self.frames)
    }
}

/// Decoded voice: 16 kHz mono from source time `start`.
struct Voice {
    pcm: Vec<f32>,
    start: Flicks,
    /// The decode reached the track's last frame.
    to_end: bool,
    /// The whole track's length at 16 kHz (samples).
    track_len: usize,
}

/// The 16 kHz voice of one source as its transcription decodes it, window after window
/// (D.02): when the windows have covered the track from its first sample to its last,
/// `analyze` takes it instead of reading the track again.
#[derive(Debug, Default)]
struct VoiceTap {
    pcm: Vec<f32>,
    /// The windows reached the track's end with no gap.
    complete: bool,
    /// A window started past what is kept (a resumed job): this tap cannot complete.
    broken: bool,
}

impl VoiceTap {
    /// Adds what `v` decoded beyond the samples already kept.
    fn add(&mut self, v: &Voice) {
        if self.broken || self.complete {
            return;
        }
        // the window's first sample at 16 kHz: windows start on whole milliseconds
        let num = i128::from(v.start.0) * i128::from(VOICE_RATE);
        let Ok(s0) = usize::try_from(num / i128::from(SEC)) else {
            self.broken = true;
            return;
        };
        if num % i128::from(SEC) != 0 || s0 > self.pcm.len() {
            self.broken = true;
            self.pcm = Vec::new();
            return;
        }
        if self.pcm.capacity() < v.track_len {
            self.pcm.reserve_exact(v.track_len - self.pcm.len());
        }
        let skip = self.pcm.len() - s0;
        if skip < v.pcm.len() {
            self.pcm.extend_from_slice(&v.pcm[skip..]);
        }
        if v.to_end {
            self.complete = true;
        }
    }
}

type TapKey = (PathBuf, Option<u32>, String);

/// Whole-track reads `analyze` made, per file, because no transcription's voice was
/// kept for it.
fn whole_reads() -> &'static Mutex<std::collections::HashMap<PathBuf, u64>> {
    static R: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, u64>>> =
        std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// How many times `analyze` read the whole voice track of `path` itself (D.02: after a
/// whole transcription it reads none).
#[allow(dead_code)] // the app's lab tests read it
pub fn whole_track_reads(path: &Path) -> u64 {
    whole_reads()
        .lock()
        .ok()
        .and_then(|g| g.get(path).copied())
        .unwrap_or(0)
}

fn tap_key(a: &AudioSource) -> TapKey {
    (a.path.clone(), a.track, a.fingerprint.clone())
}

impl Voice {
    fn t(&self, sample: usize) -> Flicks {
        Flicks(self.start.0 + samples_to_flicks(sample as i64, VOICE_RATE).0)
    }

    fn sample(&self, t: Flicks) -> usize {
        usize::try_from(flicks_to_samples(Flicks(t.0 - self.start.0), VOICE_RATE).max(0))
            .unwrap_or(0)
            .min(self.pcm.len())
    }

    fn span(&self, a: Flicks, b: Flicks) -> Span {
        Span::new(self.sample(a), self.sample(b).max(self.sample(a)))
    }
}

/// One audio track of `[from, to)` at 16 kHz mono (the app's import sheet: "Which track
/// is your mic?" previews and speech ratios).
#[allow(dead_code)] // used by the app, which includes this file
pub fn track_16k(
    path: &Path,
    track: Option<u32>,
    from: Flicks,
    to: Flicks,
) -> Result<Vec<f32>, HostError> {
    let a = AudioSource {
        path: path.to_path_buf(),
        track,
        fingerprint: String::new(),
    };
    Ok(voice(&a, Some((from, to)), &AtomicBool::new(false))?.pcm)
}

/// The voice track of `[from, to)` (whole source when `None`) at 16 kHz mono.
fn voice(
    a: &AudioSource,
    span: Option<(Flicks, Flicks)>,
    cancel: &AtomicBool,
) -> Result<Voice, HostError> {
    let mut tr = Track::open(a)?;
    let (fa, fb) = match span {
        Some((x, y)) => (tr.frame_of(Flicks(x.0.max(0))), tr.frame_of(y)),
        None => (0, tr.frames),
    };
    let rate = u64::from(tr.rate);
    // 30 s chunks, a 50 ms margin each side (an exact number of 16 kHz samples when the
    // rate is a multiple of 20 Hz, which every AAC rate is)
    let chunk = rate * 30;
    let pad = rate / 20;
    let mut pcm =
        Vec::with_capacity(((fb - fa) * u64::from(VOICE_RATE) / rate.max(1)) as usize + 16);
    let mut at = fa;
    while at < fb {
        if cancel.load(Ordering::Relaxed) {
            return Err(HostError::Failed("cancelled".into()));
        }
        let end = (at + chunk).min(fb);
        let ra = at.saturating_sub(pad);
        let rb = (end + pad).min(tr.frames);
        let raw = tr.read(ra, rb)?;
        let v = pcm::voice_16k(&raw, tr.rate, tr.channels).map_err(failed)?;
        let drop = ((at - ra) * u64::from(VOICE_RATE) / rate) as usize;
        let want = ((end - fa) * u64::from(VOICE_RATE) / rate) as usize - pcm.len();
        let take = want.min(v.len().saturating_sub(drop));
        pcm.extend_from_slice(&v[drop.min(v.len())..drop.min(v.len()) + take]);
        at = end;
    }
    Ok(Voice {
        pcm,
        start: samples_to_flicks(fa as i64, tr.rate),
        to_end: fb >= tr.frames,
        track_len: (tr.frames * u64::from(VOICE_RATE) / rate.max(1)) as usize,
    })
}

/// Native samples of `[from, to)` (≤ 15 min): `(interleaved, channels, rate)`.
fn native(a: &AudioSource, from: Flicks, to: Flicks) -> Result<(Vec<f32>, usize, u32), HostError> {
    if to.0 - from.0 > MAX_NATIVE_S * SEC {
        return Err(HostError::Failed("at most 15 minutes at a time".into()));
    }
    let mut tr = Track::open(a)?;
    let (fa, fb) = (tr.frame_of(Flicks(from.0.max(0))), tr.frame_of(to));
    let pcm = tr.read(fa, fb)?;
    Ok((pcm, tr.channels, tr.rate))
}

// -------------------------------------------------------------------------------------
// The host
// -------------------------------------------------------------------------------------

/// Analysis and ASR for the CLI and the holder.
pub struct CliAnalysis {
    /// The model cache root (`None` = the OS cache, `~/Library/Caches/gmclip`).
    models: Option<PathBuf>,
    asr: Mutex<Option<Arc<ParakeetAsr>>>,
    aligner: Mutex<Option<Arc<gmclip_ml::tdt::TdtAligner>>>,
    /// Voice kept from transcriptions for the analysis after them (D.02).
    taps: Mutex<std::collections::HashMap<TapKey, VoiceTap>>,
}

impl std::fmt::Debug for CliAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliAnalysis")
            .field("models", &self.models)
            .finish()
    }
}

impl CliAnalysis {
    /// Models from the OS cache.
    pub fn new() -> CliAnalysis {
        CliAnalysis {
            models: None,
            asr: Mutex::new(None),
            aligner: Mutex::new(None),
            taps: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Models from `root` (must be absolute). Its tests decode audio: macOS-only.
    #[cfg(all(test, target_os = "macos"))]
    pub fn with_models(root: PathBuf) -> CliAnalysis {
        CliAnalysis {
            models: Some(root),
            asr: Mutex::new(None),
            aligner: Mutex::new(None),
            taps: Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn cache(&self) -> Result<ModelCache, HostError> {
        match &self.models {
            Some(r) => ModelCache::new(r.clone()),
            None => ModelCache::default_cache(),
        }
        .map_err(ml_err)
    }

    /// The ASR, loaded once.
    pub fn asr(&self) -> Result<Arc<ParakeetAsr>, HostError> {
        self.asr_until(&AtomicBool::new(false))
    }

    /// [`CliAnalysis::asr`] for a job: a cancel stops the model's pin check (seconds on
    /// its 640 MB) and skips the load, so quitting does not wait for them.
    pub fn asr_until(&self, cancel: &AtomicBool) -> Result<Arc<ParakeetAsr>, HostError> {
        let mut g = self.asr.lock().map_err(|_| failed("asr lock"))?;
        if let Some(a) = g.as_ref() {
            return Ok(Arc::clone(a));
        }
        let stopped = || HostError::Failed("cancelled".into());
        let cache = self.cache()?;
        let model =
            match cache.verify_until(&PARAKEET_TDT_V3_INT8, &|| cancel.load(Ordering::Relaxed)) {
                Err(MlError::Cancelled) => return Err(stopped()),
                r => r.map_err(ml_err)?,
            };
        if cancel.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        let rt = OrtRuntime::init(&cache).map_err(ml_err)?;
        let a =
            Arc::new(ParakeetAsr::load(rt, &model, ParakeetOptions::default()).map_err(ml_err)?);
        *g = Some(Arc::clone(&a));
        Ok(a)
    }

    /// The forced aligner behind the arbiter (decision 0006), loaded once.
    fn aligner(&self) -> Result<Arc<gmclip_ml::tdt::TdtAligner>, HostError> {
        let mut g = self.aligner.lock().map_err(|_| failed("aligner lock"))?;
        if let Some(a) = g.as_ref() {
            return Ok(Arc::clone(a));
        }
        let cache = self.cache()?;
        let asr = cache.verify(&PARAKEET_TDT_V3_INT8).map_err(ml_err)?;
        let al = cache
            .verify(&gmclip_ml::models::PARAKEET_ALIGNER)
            .map_err(ml_err)?;
        let rt = OrtRuntime::init(&cache).map_err(ml_err)?;
        let a = Arc::new(gmclip_ml::tdt::TdtAligner::load(rt, &asr, &al, 4).map_err(ml_err)?);
        *g = Some(Arc::clone(&a));
        Ok(a)
    }

    /// Silero's speech ratio of 16 kHz mono (`None` without the VAD model).
    #[allow(dead_code)] // used by the app, which includes this file
    pub fn speech_ratio(&self, pcm: &[f32]) -> Option<f64> {
        let mut vad = self.vad().ok()?;
        vad.detect(pcm, &VadParams::default(), &CancelToken::default())
            .ok()
            .map(|r| r.speech_ratio())
    }

    fn vad(&self) -> Result<SileroVad, HostError> {
        let cache = self.cache()?;
        let model = cache.verify(&SILERO_VAD_V6_2_1).map_err(ml_err)?;
        let rt = OrtRuntime::init(&cache).map_err(ml_err)?;
        SileroVad::load(rt, &model).map_err(ml_err)
    }

    /// Waveform, energy (10/20/40 ms), the quiet grid and VAD of a source's whole voice
    /// track already at 16 kHz, each blob handed to `each` as soon as it is made: the
    /// timeline's columns first, the VAD (seconds per hour, and a model) last.
    pub fn analyze_pcm(
        &self,
        a: &AudioSource,
        pcm: &[f32],
        cancel: &AtomicBool,
        each: &mut dyn FnMut(AnalysisBlob) -> Result<(), HostError>,
    ) -> Result<(), HostError> {
        if pcm.is_empty() {
            return Err(HostError::Failed("the voice track is empty".into()));
        }
        let fp = fp(a);
        let mut push = |kind: &str, alg: (&str, u32), b: Blob| -> Result<(), HostError> {
            each(AnalysisBlob {
                kind: kind.into(),
                revision: blob::revision(alg.0, alg.1, &fp),
                body: b.encode().map_err(failed)?,
            })
        };
        push(
            "peaks_100hz",
            blob::PEAKS_ALGORITHM,
            Peaks::compute(pcm, VOICE_RATE).map_err(failed)?.to_blob(),
        )?;
        let mut e40 = None;
        for hop in Hop::ALL {
            let e = Energy::at_hop(pcm, VOICE_RATE, hop).map_err(failed)?;
            push(
                blob::Kind::energy(hop).as_str(),
                blob::ENERGY_ALGORITHM,
                e.to_blob(hop),
            )?;
            if hop == Hop::Ms40 {
                e40 = Some(e);
            }
        }
        if let Some(e) = e40
            && let Ok(grid) = QuietGrid::for_span(&e, Span::new(0, pcm.len()))
        {
            push("silences_40ms", ("quiet_grid", 1), silences_blob(&grid))?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(HostError::Failed("cancelled".into()));
        }
        // VAD needs its model; without it the rest still stands
        if let Ok(mut vad) = self.vad() {
            let p = VadParams::default();
            let r = vad
                .detect(pcm, &p, &CancelToken::default())
                .map_err(ml_err)?;
            push("vad_speech", ("silero_vad", 621), r.to_blob(&p))?;
        }
        Ok(())
    }

    /// Keeps what a transcription of `a` decoded, when asked to ([`AnalysisHost::keep_voice`]).
    fn tap(&self, a: &AudioSource, v: &Voice) {
        if let Ok(mut g) = self.taps.lock()
            && let Some(t) = g.get_mut(&tap_key(a))
        {
            t.add(v);
        }
    }

    /// The kept voice of `a`, when its transcription covered the whole track (and the
    /// tap is dropped either way).
    fn take_voice(&self, a: &AudioSource) -> Option<Vec<f32>> {
        let t = self.taps.lock().ok()?.remove(&tap_key(a))?;
        t.complete.then_some(t.pcm)
    }

    fn transcribe_voice(&self, v: &Voice, cancel: &AtomicBool) -> Result<Transcribed, HostError> {
        let asr = self.asr_until(cancel)?;
        let token = Arc::new(CancelToken::default());
        // the job's cancel flag reaches the decoder through its own token
        let watch = Arc::clone(&token);
        let flag: &AtomicBool = cancel;
        let done = AtomicBool::new(false);
        let words = std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if flag.load(Ordering::Relaxed) {
                        watch.cancel();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
            let r = asr.transcribe(&v.pcm, v.start, &token);
            done.store(true, Ordering::Relaxed);
            r
        })
        .map_err(ml_err)?;
        Ok(Transcribed {
            revision: asr.revision(),
            words: words.into_iter().map(heard).collect(),
        })
    }
}

// -------------------------------------------------------------------------------------
// Captions heard from a cut's own audio (SPEC §5.10, decision 0022)
// -------------------------------------------------------------------------------------

/// Output samples per 16 kHz voice sample.
#[cfg(any(target_os = "macos", windows))]
const MIX_PER_VOICE: u64 = 48_000 / VOICE_RATE as u64;

impl CliAnalysis {
    /// Transcribes each stretch's window of `plan`'s voice mix (the export's mixer: part
    /// gain, fades, envelopes; no delivery loudness), down-mixed and resampled to 16 kHz in
    /// 30 s chunks with 50 ms margins (seamless joins, as a source's voice is decoded),
    /// heard by the recogniser the sources are. Words count from the window's start.
    #[cfg(any(target_os = "macos", windows))]
    pub fn hear_stretches(
        &self,
        plan: &gmclip_plan::Plan,
        inputs: &mut dyn gmclip_render::export::Inputs,
        stretches: &[gmclip_core::planner::heard::Stretch],
        cancel: &AtomicBool,
    ) -> Result<Vec<gmclip_core::planner::heard::HeardStretch>, HostError> {
        use gmclip_core::planner::heard::{HeardStretch, HeardWord as Word};
        if stretches.is_empty() {
            return Ok(Vec::new());
        }
        // the model first: a machine without it fails before rendering any audio
        self.asr_until(cancel)?;
        let mut mix = gmclip_render::export::VoiceMix::open(plan, inputs).map_err(failed)?;
        let total = mix.total();
        let mut out = Vec::with_capacity(stretches.len());
        for st in stretches {
            let [a, b] = [st.window[0].min(total), st.window[1].min(total)];
            let chunk = 48_000 * 30;
            let pad = 48_000 / 20;
            let mut pcm: Vec<f32> = Vec::with_capacity(((b - a) / MIX_PER_VOICE) as usize + 16);
            let mut at = a;
            while at < b {
                if cancel.load(Ordering::Relaxed) {
                    return Err(HostError::Failed("cancelled".into()));
                }
                let end = (at + chunk).min(b);
                let ra = at.saturating_sub(pad);
                let rb = (end + pad).min(total);
                let mut raw = vec![0f32; ((rb - ra) * 2) as usize];
                mix.read(ra, &mut raw).map_err(failed)?;
                let v = pcm::voice_16k(&raw, 48_000, 2).map_err(failed)?;
                let drop = ((at - ra) / MIX_PER_VOICE) as usize;
                let want = (((end - a) / MIX_PER_VOICE) as usize).saturating_sub(pcm.len());
                let from = drop.min(v.len());
                let take = want.min(v.len() - from);
                pcm.extend_from_slice(&v[from..from + take]);
                at = end;
            }
            let voice = Voice {
                track_len: pcm.len(),
                pcm,
                start: Flicks(0),
                to_end: true,
            };
            let t = self.transcribe_voice(&voice, cancel)?;
            out.push(HeardStretch::new(
                st,
                t.revision,
                t.words
                    .into_iter()
                    .map(|w| Word {
                        text: w.text,
                        start: w.start.0,
                        end: w.end.0,
                        confidence: w.confidence.map(f64::from),
                    })
                    .collect(),
            ));
        }
        Ok(out)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn hear_stretches(
        &self,
        _plan: &gmclip_plan::Plan,
        _inputs: &mut dyn gmclip_render::export::Inputs,
        stretches: &[gmclip_core::planner::heard::Stretch],
        _cancel: &AtomicBool,
    ) -> Result<Vec<gmclip_core::planner::heard::HeardStretch>, HostError> {
        if stretches.is_empty() {
            return Ok(Vec::new());
        }
        Err(HostError::Unsupported(
            "hearing a cut's audio needs the AAC decoder (macOS and Windows)",
        ))
    }
}

/// The planner's [`Hearer`](gmclip_core::planner::heard::Hearer): a plan's sources opened
/// through the demuxers (absolute paths), heard by `host`.
pub struct PlanHearer<'a> {
    pub host: &'a CliAnalysis,
    pub cancel: &'a AtomicBool,
}

fn host_reason(e: HostError) -> String {
    match e {
        HostError::Unsupported(r) => r.to_owned(),
        HostError::Failed(r) => r,
    }
}

impl gmclip_core::planner::heard::Hearer for PlanHearer<'_> {
    fn warm(&self) -> Result<(), String> {
        self.host
            .asr_until(self.cancel)
            .map(|_| ())
            .map_err(host_reason)
    }

    fn hear(
        &self,
        plan: &gmclip_plan::Plan,
        stretches: &[gmclip_core::planner::heard::Stretch],
    ) -> Result<Vec<gmclip_core::planner::heard::HeardStretch>, String> {
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        self.host
            .hear_stretches(plan, &mut inputs, stretches, self.cancel)
            .map_err(host_reason)
    }
}

impl Default for CliAnalysis {
    fn default() -> Self {
        Self::new()
    }
}

fn heard(w: AsrWord) -> HeardWord {
    HeardWord {
        text: w.text,
        start: w.start,
        end: w.end,
        confidence: w.confidence,
    }
}

fn fp(a: &AudioSource) -> String {
    format!(
        "{}#{}",
        a.fingerprint,
        a.track.map_or_else(|| "a0".into(), |t| format!("t{t}"))
    )
}

/// Silences (the quiet grid's regions) as a blob: sample spans at 16 kHz.
fn silences_blob(grid: &QuietGrid) -> Blob {
    let r = grid.regions();
    let mut b = Blob {
        kind: "silences_40ms".into(),
        ..Default::default()
    };
    b.params
        .insert("rate".into(), Param::Int(i64::from(grid.rate)));
    b.params.insert(
        "threshold_db".into(),
        Param::Float(f64::from(grid.threshold_db)),
    );
    b.params.insert(
        "floor_db".into(),
        Param::Float(f64::from(grid.levels.floor_db)),
    );
    b.params.insert(
        "speech_db".into(),
        Param::Float(f64::from(grid.levels.speech_db)),
    );
    b.columns.push((
        "start".into(),
        Column::I64(r.iter().map(|s| s.start as i64).collect()),
    ));
    b.columns.push((
        "end".into(),
        Column::I64(r.iter().map(|s| s.end as i64).collect()),
    ));
    b
}

/// A cut point `at` between two words moved onto the voice (decision 0049): where the gap
/// between them holds it, no sooner than 120 ms after w1 is heard and no later than 40 ms
/// before w2 (its soft onset kept). Returns the point and a note for the method when it
/// moved.
fn on_the_voice(v: &Voice, fine: &Energy, s1: Span, s2: Span, at: usize) -> (usize, String) {
    let peak = |s: Span| edges::peak_frame(fine, s).map(|(i, _)| fine.frame_centre(i));
    let (Some(p1), Some(p2)) = (peak(s1), peak(s2)) else {
        return (at, String::new());
    };
    let (Some(a), Some(b)) = (
        edges::audible_end(fine, s1, p2),
        edges::audible_start(fine, s2, p1),
    ) else {
        return (at, String::new());
    };
    let quiet = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms40)
        .ok()
        .and_then(|e| QuietGrid::for_span(&e, Span::new(0, v.pcm.len())).ok())
        .map(|g| g.threshold_db);
    let lo = edges::kept_end(fine, a.sample);
    let hi = quiet.map_or(b.sample.saturating_sub(edges_ms(edges::LEAD_MS)), |q| {
        edges::kept_start(fine, b.sample, q)
    });
    if lo > hi || (lo..=hi).contains(&at) {
        return (at, String::new());
    }
    (
        at.clamp(lo, hi),
        "; moved to keep 120 ms after w1 is heard and 40 ms before w2".into(),
    )
}

/// Speech-to-silence pauses in `v` restricted to `[s, e)`, never across a word.
fn pause_cuts(
    v: &Voice,
    span: (Flicks, Flicks),
    min: Flicks,
    keep: Flicks,
    words: &[HeardWord],
) -> Result<(Vec<SweepSpan>, Vec<SweepSpan>), HostError> {
    let e40 = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms40).map_err(failed)?;
    let s = v.span(span.0, span.1);
    let Ok(grid) = QuietGrid::for_span(&e40, s) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let min_len = usize::try_from(flicks_to_samples(min, VOICE_RATE)).unwrap_or(usize::MAX);
    // a word is protected over its audible extent (18 dB drop held 60 ms), not the ASR's
    // times, which often run a word on into the silence after it
    let fine = Energy::fine(&v.pcm, VOICE_RATE).map_err(failed)?;
    let inside: Vec<&HeardWord> = words
        .iter()
        .filter(|w| w.end > v.start && w.start < v.t(v.pcm.len()))
        .collect();
    let spans_w: Vec<Span> = inside.iter().map(|w| v.span(w.start, w.end)).collect();
    // a word whose loudest frame is under the pause threshold is not heard in its ASR
    // span at all (timed late, into the silence after it): its peak is the room's noise,
    // no 18 dB drop follows, and the ASR span would keep the whole silence
    let (audible, peaks): (Vec<HeardWord>, Vec<Option<f32>>) =
        edges::audible_edges(&fine, &spans_w, s)
            .into_iter()
            .zip(&inside)
            .filter_map(|(e, w)| match e {
                Some((a, _)) if a.peak_db < grid.threshold_db => None,
                Some((a, b)) => Some((
                    HeardWord {
                        text: w.text.clone(),
                        start: v.t(a.sample.min(b.sample)),
                        end: v.t(b.sample.max(a.sample)),
                        confidence: None,
                    },
                    Some(a.peak_db),
                )),
                None => Some(((*w).clone(), None)),
            })
            .unzip();
    let words = &audible;
    let (mut cuts, mut skipped) = (Vec::new(), Vec::new());
    // the part's edges sit on its voice (decision 0049): 40 ms before its first word is
    // heard (a soft onset within 250 ms kept), 120 ms after its last; the breaths and room
    // outside go, short of any sound as loud as the voice that is no word
    let min_cut = edges_ms(120);
    let (mut head, mut tail) = (s.start, s.end);
    // the words the part plays some of; an edge inside a word keeps that side whole
    let mut played = words
        .iter()
        .zip(&peaks)
        .filter(|(w, _)| w.end > span.0 && w.start < span.1);
    let (first, last) = (played.next(), played.next_back());
    let last = last.or(first);
    if let Some((w, Some(peak))) = first.filter(|(w, _)| w.start >= span.0) {
        let r = fine.frames_in(Span::new(s.start, v.sample(w.start)));
        let loud = r.clone().find(|&i| fine.db[i] >= peak - edges::DROP_DB);
        let k = edges::kept_start(&fine, v.sample(w.start), grid.threshold_db);
        let k = loud.map_or(k, |i| {
            k.min(fine.frame_start(i).saturating_sub(edges_ms(edges::LEAD_MS)))
        });
        if k >= s.start + min_cut {
            cuts.push(SweepSpan {
                from: v.t(s.start),
                to: v.t(k),
                reason: "before the first word is heard".into(),
            });
            head = k;
        }
    }
    if let Some((w, Some(peak))) = last.filter(|(w, _)| w.end <= span.1) {
        let r = fine.frames_in(Span::new(v.sample(w.end), s.end));
        let loud = r.rev().find(|&i| fine.db[i] >= peak - edges::DROP_DB);
        let k = edges::kept_end(&fine, v.sample(w.end));
        let k = loud.map_or(k, |i| {
            k.max(fine.frame_start(i) + fine.window + edges_ms(edges::TAIL_MS))
        });
        if k + min_cut <= s.end && k >= head {
            cuts.push(SweepSpan {
                from: v.t(k),
                to: v.t(s.end),
                reason: "after the last word is heard".into(),
            });
            tail = k;
        }
    }
    // inside, `keep` of each pause stays on each side
    for p in grid.pauses(min_len) {
        let (pa, pb) = (
            v.t(p.start.max(s.start).max(head)),
            v.t(p.end.min(s.end).min(tail)),
        );
        if pb <= pa {
            continue;
        }
        // never a word: the pause minus every word inside it
        let mut gaps = vec![(pa, pb)];
        for w in words.iter().filter(|w| w.start < pb && pa < w.end) {
            gaps = gaps
                .into_iter()
                .flat_map(|(x, y)| {
                    let mut o = Vec::new();
                    if w.start > x {
                        o.push((x, w.start.min(y)));
                    }
                    if w.end < y {
                        o.push((w.end.max(x), y));
                    }
                    o
                })
                .filter(|(x, y)| y > x)
                .collect();
        }
        for (a, b) in gaps {
            if b.0 - a.0 < min.0 {
                continue;
            }
            // a part with no word heard keeps none at its edges
            let at_start = a <= v.t(s.start);
            let at_end = b >= v.t(s.end);
            let ca = if at_start { a } else { Flicks(a.0 + keep.0) };
            let cb = if at_end { b } else { Flicks(b.0 - keep.0) };
            if cb.0 - ca.0 < SEC * 12 / 100 {
                skipped.push(SweepSpan {
                    from: a,
                    to: b,
                    reason: "too short after keeping silence".into(),
                });
                continue;
            }
            cuts.push(SweepSpan {
                from: ca,
                to: cb,
                reason: format!(
                    "pause {:.2} s under {:.0} dB",
                    (b.0 - a.0) as f64 / SEC as f64,
                    grid.threshold_db
                ),
            });
        }
    }
    Ok((cuts, skipped))
}

impl AnalysisHost for CliAnalysis {
    fn name(&self) -> &'static str {
        "gmclip-ml"
    }

    fn analyze(
        &self,
        a: &AudioSource,
        cancel: &AtomicBool,
    ) -> Result<Vec<AnalysisBlob>, HostError> {
        let mut out = Vec::new();
        self.analyze_each(a, cancel, &mut |b| {
            out.push(b);
            Ok(())
        })?;
        Ok(out)
    }

    fn analyze_each(
        &self,
        a: &AudioSource,
        cancel: &AtomicBool,
        each: &mut dyn FnMut(AnalysisBlob) -> Result<(), HostError>,
    ) -> Result<(), HostError> {
        // the voice the transcription just decoded, else a read of the track
        let pcm = match self.take_voice(a) {
            Some(pcm) => pcm,
            None => {
                if let Ok(mut g) = whole_reads().lock() {
                    *g.entry(a.path.clone()).or_default() += 1;
                }
                voice(a, None, cancel)?.pcm
            }
        };
        self.analyze_pcm(a, &pcm, cancel, each)
    }

    fn keep_voice(&self, a: &AudioSource, keep: bool) {
        if let Ok(mut g) = self.taps.lock() {
            if keep {
                g.insert(tap_key(a), VoiceTap::default());
            } else {
                g.remove(&tap_key(a));
            }
        }
    }

    fn transcribe(
        &self,
        a: &AudioSource,
        span: Option<(Flicks, Flicks)>,
        cancel: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        // the model first: a machine without it fails before decoding hours of audio
        self.asr_until(cancel)?;
        let v = voice(a, span, cancel)?;
        self.tap(a, &v);
        let mut t = self.transcribe_voice(&v, cancel)?;
        if span.is_some() {
            t.revision = format!("{}/span", t.revision);
        }
        Ok(t)
    }

    fn listen(&self, a: &AudioSource, from: Flicks, to: Flicks) -> Result<Heard, HostError> {
        if to.0 <= from.0 || to.0 - from.0 > 120 * SEC {
            return Err(HostError::Failed("a span of at most 120 s".into()));
        }
        let never = AtomicBool::new(false);
        let v = voice(a, Some((from, to)), &never)?;
        let transcribed = match self.transcribe_voice(&v, &never) {
            Ok(t) => t,
            Err(HostError::Unsupported(r)) => Transcribed {
                revision: format!("unavailable: {r}"),
                words: Vec::new(),
            },
            Err(e) => return Err(e),
        };
        let loudness = self.loudness(a, from, to).ok();
        let e = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms10).map_err(failed)?;
        let energy_db_50ms =
            e.db.chunks(5)
                .map(|c| c.iter().copied().fold(f32::MIN, f32::max))
                .collect();
        let e40 = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms40).map_err(failed)?;
        let silences = QuietGrid::for_span(&e40, Span::new(0, v.pcm.len()))
            .map(|g| {
                g.regions()
                    .into_iter()
                    .filter(|r| r.len() >= edges_ms(200))
                    .map(|r| (v.t(r.start), v.t(r.end)))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Heard {
            transcribed,
            loudness,
            energy_db_50ms,
            silences,
        })
    }

    fn loudness(
        &self,
        a: &AudioSource,
        from: Flicks,
        to: Flicks,
    ) -> Result<SpanLoudness, HostError> {
        let (pcm, ch, rate) = native(a, from, to)?;
        if pcm.len() < ch * rate as usize / 2 {
            return Err(HostError::Failed("loudness needs at least 0.5 s".into()));
        }
        let i = gmclip_media::audio::loudness::integrated(&pcm, ch, rate);
        let tp = gmclip_media::audio::loudness::true_peak(&pcm, ch);
        Ok(SpanLoudness {
            integrated_lufs: i,
            true_peak_dbtp: tp.is_finite().then_some(tp),
        })
    }

    fn edge(
        &self,
        a: &AudioSource,
        rule: EdgeRule,
        w1: (&str, Flicks, Flicks),
        w2: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        let lo = Flicks((w1.1.0 - EDGE_CONTEXT).max(0));
        let hi = Flicks(w2.map_or(w1.2, |w| w.2).0 + EDGE_CONTEXT);
        let never = AtomicBool::new(false);
        let v = voice(a, Some((lo, hi)), &never)?;
        let fine = Energy::fine(&v.pcm, VOICE_RATE).map_err(failed)?;
        let s1 = v.span(w1.1, w1.2);
        match rule {
            EdgeRule::AudibleStart | EdgeRule::AudibleEnd => {
                let r = if rule == EdgeRule::AudibleStart {
                    edges::audible_start(&fine, s1, 0)
                } else {
                    edges::audible_end(&fine, s1, v.pcm.len())
                }
                .ok_or(HostError::Failed("no sound in the word".into()))?;
                Ok(MeasuredEdge {
                    value: v.t(r.sample),
                    method: format!(
                        "{:?} (18 dB drop held 60 ms, 10 ms RMS at a 5 ms hop)",
                        r.method
                    )
                    .to_lowercase(),
                    window: (v.t(s1.start), v.t(s1.end)),
                    rms_db: Some(f64::from(r.peak_db)),
                })
            }
            EdgeRule::EnergyMin | EdgeRule::Arbiter => {
                let w2 = w2.ok_or(HostError::Failed("a cut point needs the next word".into()))?;
                let s2 = v.span(w2.1, w2.2);
                if rule == EdgeRule::EnergyMin || !gmclip_ml::arbiter::is_abutting(s1.end, s2.start)
                {
                    let m = edges::energy_min(&fine, s1.end, s2.start)
                        .ok_or(HostError::Failed("no energy between the words".into()))?;
                    let (at, kept) = on_the_voice(&v, &fine, s1, s2, m.value);
                    return Ok(MeasuredEdge {
                        value: v.t(at),
                        method: format!(
                            "energy_min (lowest 10 ms RMS in [w1.end − 40 ms, w2.start + 40 ms]){kept}"
                        ),
                        window: (v.t(m.window.start), v.t(m.window.end)),
                        rms_db: Some(f64::from(m.rms_db)),
                    });
                }
                let aligner = self.aligner()?;
                let r = gmclip_ml::arbiter::arbitrate(
                    aligner.as_ref(),
                    &v.pcm,
                    &fine,
                    gmclip_ml::arbiter::PairWord {
                        text: w1.0,
                        span: s1,
                    },
                    gmclip_ml::arbiter::PairWord {
                        text: w2.0,
                        span: s2,
                    },
                    &gmclip_ml::arbiter::ArbiterParams::default(),
                    &CancelToken::default(),
                )
                .map_err(|_| HostError::Failed("unresolvable edge".into()))?;
                let (at, kept) = on_the_voice(&v, &fine, s1, s2, r.value);
                Ok(MeasuredEdge {
                    value: v.t(at),
                    method: format!(
                        "arbiter (forced alignment of \"{}\" | \"{}\" over {} candidate minima; margin {:.2}){kept}",
                        r.left_words.join(" "),
                        r.right_words.join(" "),
                        r.scored.len(),
                        r.margin
                    ),
                    window: (v.t(r.window.start), v.t(r.window.end)),
                    rms_db: Some(f64::from(r.rms_db)),
                })
            }
        }
    }

    fn pauses(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        min: Flicks,
        keep: Flicks,
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        let never = AtomicBool::new(false);
        let mut out = SweepCandidates {
            revision: format!("quiet_grid@1/{}/min{}/keep{}", fp(a), min.0, keep.0),
            ..Default::default()
        };
        for &(s, e) in spans {
            let v = voice(a, Some((s, e)), &never)?;
            if v.pcm.len() < edges_ms(200) {
                continue;
            }
            let (c, k) = pause_cuts(&v, (s, e), min, keep, words)?;
            out.cuts.extend(c);
            out.skipped.extend(k);
        }
        Ok(out)
    }

    fn fillers(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        let never = AtomicBool::new(false);
        let list = fillers::fillers_for("en").unwrap_or(fillers::FILLERS_EN);
        let mut out = SweepCandidates {
            revision: format!("fillers_en@1/{}", fp(a)),
            ..Default::default()
        };
        for &(s, e) in spans {
            let ws: Vec<&HeardWord> = words
                .iter()
                .filter(|w| w.start >= s && w.end <= e)
                .collect();
            if !ws
                .iter()
                .any(|w| list.contains(&fillers::normalise(&w.text).as_str()))
            {
                continue;
            }
            let v = voice(a, Some((s, e)), &never)?;
            if v.pcm.len() < edges_ms(200) {
                continue;
            }
            let e40 = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms40).map_err(failed)?;
            let fine = Energy::fine(&v.pcm, VOICE_RATE).map_err(failed)?;
            let all = Span::new(0, v.pcm.len());
            let Ok(grid) = QuietGrid::for_span(&e40, all) else {
                continue;
            };
            let spans_w: Vec<WordSpan<'_>> = ws
                .iter()
                .map(|w| WordSpan {
                    text: &w.text,
                    span: v.span(w.start, w.end),
                })
                .collect();
            let r = fillers::detect(&spans_w, list, &grid, &fine, all);
            for c in r.cuts {
                let text = ws[c.word..c.word + c.words.max(1)]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                out.cuts.push(SweepSpan {
                    from: v.t(c.cut.start),
                    to: v.t(c.cut.end),
                    reason: format!("filler \"{text}\" ({:?})", c.anchor).to_lowercase(),
                });
            }
            for k in r.skipped {
                if let Some(w) = ws.get(k) {
                    out.skipped.push(SweepSpan {
                        from: w.start,
                        to: w.end,
                        reason: format!("fused filler \"{}\"", w.text),
                    });
                }
            }
        }
        Ok(out)
    }

    fn blob_summary(
        &self,
        kind: &str,
        body: &[u8],
        from: Flicks,
        to: Flicks,
        max_points: usize,
    ) -> Result<Value, HostError> {
        let b = Blob::decode(body).map_err(failed)?;
        let rate = b
            .int("rate")
            .and_then(|r| u32::try_from(r).ok())
            .unwrap_or(VOICE_RATE);
        let sa = usize::try_from(flicks_to_samples(from, rate).max(0)).unwrap_or(0);
        let sb = usize::try_from(flicks_to_samples(to, rate).max(0)).unwrap_or(0);
        let secs = |s: i64| (s as f64 / f64::from(rate) * 1000.0).round() / 1000.0;
        match kind {
            k if k.starts_with("energy_") => {
                let e = Energy::from_blob(&b).map_err(failed)?;
                let r = e.frames_in(Span::new(sa, sb.max(sa)));
                let frames = &e.db[r.clone()];
                let step = frames.len().div_ceil(max_points.max(1)).max(1);
                let pts: Vec<Value> = frames
                    .chunks(step)
                    .enumerate()
                    .map(|(i, c)| {
                        json!([
                            secs(e.frame_start(r.start + i * step) as i64),
                            (c.iter().copied().fold(f32::MIN, f32::max) * 10.0).round() / 10.0
                        ])
                    })
                    .collect();
                Ok(
                    json!({"unit": "dB RMS (max per point)", "hop_ms": e.hop as f64 * 1000.0 / f64::from(e.rate), "points_per_entry": step, "points": pts}),
                )
            }
            "silences_40ms" | "vad_speech" => {
                let (Some(Column::I64(st)), Some(Column::I64(en))) = (b.get("start"), b.get("end"))
                else {
                    return Ok(json!({"regions": [], "note": "no regions column"}));
                };
                let regs: Vec<Value> = st
                    .iter()
                    .zip(en)
                    .filter(|(s, e)| (**e as usize) > sa && (**s as usize) < sb)
                    .take(max_points.max(1))
                    .map(|(s, e)| json!([secs(*s), secs(*e)]))
                    .collect();
                let thr = b.params.get("threshold_db").cloned();
                Ok(
                    json!({"regions_s": regs, "threshold_db": thr.map(|p| match p { Param::Float(f) => json!(f), _ => Value::Null })}),
                )
            }
            _ => Ok(json!({"kind": kind, "note": "no summary for this kind"})),
        }
    }
}

fn edges_ms(ms: u32) -> usize {
    gmclip_analysis::span::ms(ms, VOICE_RATE)
}

// -------------------------------------------------------------------------------------
// verify{joins} (SPEC §5.14) on gmclip's own ASR
// -------------------------------------------------------------------------------------

const JOIN_WINDOW: i64 = SEC * 3 / 2;

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=y.len()).collect();
    for i in 1..=x.len() {
        let mut cur = vec![i; y.len() + 1];
        for j in 1..=y.len() {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(x[i - 1] != y[j - 1]));
        }
        prev = cur;
    }
    prev[y.len()]
}

/// Checks every join of an export against its sources: the words within 1.5 s of the
/// join that the parts keep (≥ 50 % inside) must be heard in the render within ±0.25 s
/// at edit distance ≤ 1, no other non-filler word may appear there, and the level may
/// not step by more than 12 dB across the join.
pub fn verify_joins(
    an: &CliAnalysis,
    plan: &gmclip_core::gmclip_plan::Plan,
    out: &std::path::Path,
) -> Result<Value, HostError> {
    an.asr()?;
    let fpf = plan.output.fps.flicks_per_frame() as i64;
    let never = AtomicBool::new(false);
    let rendered = AudioSource {
        path: out.to_path_buf(),
        track: None,
        fingerprint: "export".into(),
    };
    let fillers_list = fillers::FILLERS_EN;
    let src_of = |id: &_| {
        plan.sources
            .iter()
            .find(|s| &s.id == id)
            .map(|s| AudioSource {
                path: PathBuf::from(&s.path),
                track: None,
                fingerprint: String::new(),
            })
    };
    let mut joins = Vec::new();
    let mut passed = true;
    for (i, pair) in plan.parts.windows(2).enumerate() {
        let (p, q) = (&pair[0], &pair[1]);
        let j_out = Flicks(p.out.0[1] as i64 * fpf);
        let p_len = Flicks((p.out.0[1] - p.out.0[0]) as i64 * fpf);
        let p_start = Flicks(p.audio.first_sample as i64 * SEC / 48_000);
        let p_end = Flicks(p_start.0 + p_len.0);
        let q_start = Flicks(q.audio.first_sample as i64 * SEC / 48_000);
        let (Some(sp), Some(sq)) = (src_of(&p.audio.source), src_of(&q.audio.source)) else {
            return Err(HostError::Failed(
                "a part's source is not in the plan".into(),
            ));
        };
        // the reference: each side heard with 1 s more context than the window
        let vp = voice(
            &sp,
            Some((
                Flicks((p_end.0 - JOIN_WINDOW - SEC).max(p_start.0 - SEC).max(0)),
                p_end,
            )),
            &never,
        )?;
        let vq = voice(
            &sq,
            Some((q_start, Flicks(q_start.0 + JOIN_WINDOW + SEC))),
            &never,
        )?;
        let lp = an.transcribe_voice(&vp, &never)?;
        let rq = an.transcribe_voice(&vq, &never)?;
        // every word either side heard, on the output clock (a render word matching one
        // of these is not unexpected, even when too little of it is kept to be expected)
        let reference: Vec<(String, Flicks)> = lp
            .words
            .iter()
            .map(|w| (norm(&w.text), Flicks(j_out.0 - (p_end.0 - w.start.0))))
            .chain(
                rq.words
                    .iter()
                    .map(|w| (norm(&w.text), Flicks(j_out.0 + (w.start.0 - q_start.0)))),
            )
            .collect();
        let kept = |w: &HeardWord, a: Flicks, b: Flicks| {
            let inside = (w.end.0.min(b.0) - w.start.0.max(a.0)).max(0);
            inside * 2 >= (w.end.0 - w.start.0).max(1)
        };
        let mut expected: Vec<(String, Flicks)> = Vec::new();
        for w in &lp.words {
            if kept(w, Flicks(p_end.0 - JOIN_WINDOW).max(p_start), p_end) {
                expected.push((norm(&w.text), Flicks(j_out.0 - (p_end.0 - w.start.0))));
            }
        }
        for w in &rq.words {
            if kept(w, q_start, Flicks(q_start.0 + JOIN_WINDOW)) {
                expected.push((norm(&w.text), Flicks(j_out.0 + (w.start.0 - q_start.0))));
            }
        }
        let rv = voice(
            &rendered,
            Some((
                Flicks((j_out.0 - JOIN_WINDOW - SEC).max(0)),
                Flicks(j_out.0 + JOIN_WINDOW + SEC),
            )),
            &never,
        )?;
        let heard_w = an.transcribe_voice(&rv, &never)?.words;
        let near: Vec<&HeardWord> = heard_w
            .iter()
            .filter(|w| (w.start.0 - j_out.0).abs() <= JOIN_WINDOW)
            .collect();
        let mut used = vec![false; near.len()];
        let mut missing = Vec::new();
        for (t, at) in &expected {
            let hit = near.iter().enumerate().find(|(k, w)| {
                !used[*k]
                    && (w.start.0 - at.0).abs() <= SEC / 4
                    && edit_distance(&norm(&w.text), t) <= 1
            });
            match hit {
                Some((k, _)) => used[k] = true,
                None => missing.push(json!({"word": t, "at_s": at.0 as f64 / SEC as f64})),
            }
        }
        let unexpected: Vec<Value> = near
            .iter()
            .zip(&used)
            .filter(|(w, u)| !**u && !fillers_list.contains(&fillers::normalise(&w.text).as_str()))
            .filter(|(w, _)| {
                !reference.iter().any(|(t, at)| {
                    (w.start.0 - at.0).abs() <= SEC / 4 && edit_distance(&norm(&w.text), t) <= 1
                })
            })
            .map(|(w, _)| json!({"word": w.text, "at_s": w.start.0 as f64 / SEC as f64}))
            .collect();
        // the level step the join adds: 10 ms RMS just before vs just after it in the
        // render, against the same two edges in the sources (speech has steps of its own)
        let level = |v: &Voice, s: usize| -> Result<f32, HostError> {
            let fine = Energy::fine(&v.pcm, VOICE_RATE).map_err(failed)?;
            Ok(fine
                .db
                .get((s / fine.hop).min(fine.db.len().saturating_sub(1)))
                .copied()
                .unwrap_or(-120.0))
        };
        let js = rv.sample(j_out);
        let rendered_step =
            level(&rv, js + edges_ms(5))? - level(&rv, js.saturating_sub(edges_ms(15)))?;
        let source_step =
            level(&vq, edges_ms(5))? - level(&vp, vp.pcm.len().saturating_sub(edges_ms(15)))?;
        let step = (rendered_step - source_step).abs();
        let ok = missing.is_empty() && unexpected.is_empty() && step <= 12.0;
        passed &= ok;
        joins.push(json!({
            "join": i, "at_s": j_out.0 as f64 / SEC as f64, "ok": ok,
            "expected": expected.len(), "missing": missing, "unexpected": unexpected,
            "energy_step_db": (step * 10.0).round() / 10.0,
            "energy_step_note": "the step the join adds over the sources' own (≤ 12 dB)",
        }));
    }
    Ok(
        json!({"passed": passed, "joins": joins, "method": "gmclip-ml Parakeet on the render vs the parts' source audio (SPEC §5.14)", "asr_revision": an.asr()?.revision()}),
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;

    /// A spoken source: macOS `say` → AAC 48 kHz stereo in MP4 (dev machines only).
    fn speech(dir: &std::path::Path) -> Option<PathBuf> {
        let ffmpeg = "/opt/homebrew/bin/ffmpeg";
        if !std::path::Path::new(ffmpeg).exists() || !std::path::Path::new("/usr/bin/say").exists()
        {
            return None;
        }
        let aiff = dir.join("s.aiff");
        let ok = Command::new("/usr/bin/say")
            .args(["-v", "Samantha", "-o"])
            .arg(&aiff)
            .arg("So this took sixty seven seconds. [[slnc 900]] Then, um, it was done and we were happy.")
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let mp4 = dir.join("s.mp4");
        let ok = Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-i"])
            .arg(&aiff)
            .args(["-ar", "48000", "-ac", "2", "-c:a", "aac", "-b:a", "160k"])
            .arg(&mp4)
            .status()
            .ok()?
            .success();
        ok.then_some(mp4)
    }

    /// `tests/sweeps_lab.rs` includes this file by path; these tests are the binary's own
    /// and run once, in its unit tests (the model one alone takes over a minute).
    fn included_elsewhere() -> bool {
        !module_path!().starts_with("gmclip::")
    }

    fn dir(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("gmclip-an-{}-{name}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_machine_without_models_still_analyses_energy_and_silences() {
        if included_elsewhere() {
            return;
        }
        let d = dir("nomodels");
        let Some(src) = speech(&d) else {
            eprintln!("skipped: no say/ffmpeg");
            return;
        };
        let an = CliAnalysis::with_models(d.join("no-models"));
        let a = AudioSource {
            path: src,
            track: None,
            fingerprint: "fp".into(),
        };
        let never = AtomicBool::new(false);
        let blobs = an.analyze(&a, &never).unwrap();
        let kinds: Vec<&str> = blobs.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "peaks_100hz",
                "energy_10ms",
                "energy_20ms",
                "energy_40ms",
                "silences_40ms"
            ]
        );
        assert!(
            blobs.iter().all(|b| b.revision.ends_with("/fp#a0")),
            "{:?}",
            blobs[0].revision
        );
        // the silence blob reads back with a region where `say` paused
        let sil = blobs.iter().find(|b| b.kind == "silences_40ms").unwrap();
        let v = an
            .blob_summary("silences_40ms", &sil.body, Flicks(0), Flicks(60 * SEC), 100)
            .unwrap();
        let regs = v["regions_s"].as_array().unwrap();
        assert!(
            regs.iter()
                .any(|r| r[1].as_f64().unwrap() - r[0].as_f64().unwrap() >= 0.7),
            "{v}"
        );
        let e = blobs.iter().find(|b| b.kind == "energy_20ms").unwrap();
        let v = an
            .blob_summary("energy_20ms", &e.body, Flicks(0), Flicks(2 * SEC), 50)
            .unwrap();
        assert!(v["points"].as_array().unwrap().len() <= 50);
        // ASR is Unsupported, with the reason
        assert!(matches!(
            an.transcribe(&a, None, &never),
            Err(HostError::Unsupported(_))
        ));
        let l = an.loudness(&a, Flicks(0), Flicks(3 * SEC)).unwrap();
        assert!(
            l.integrated_lufs < -5.0 && l.integrated_lufs > -60.0,
            "{l:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// With the pinned models in the OS cache (read only): ASR, VAD, edges, sweeps,
    /// listen and verify{joins}. Skipped where the models are not installed.
    #[test]
    fn asr_edges_sweeps_and_joins_with_the_real_models() {
        if included_elsewhere() {
            return;
        }
        let d = dir("models");
        let Some(src) = speech(&d) else {
            eprintln!("skipped: no say/ffmpeg");
            return;
        };
        let an = CliAnalysis::new();
        if an.asr().is_err() {
            eprintln!("skipped: no models in the OS cache (tools/fetch_models.sh)");
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
        let a = AudioSource {
            path: src.clone(),
            track: None,
            fingerprint: "fp".into(),
        };
        let never = AtomicBool::new(false);
        let blobs = an.analyze(&a, &never).unwrap();
        assert!(blobs.iter().any(|b| b.kind == "vad_speech"));
        let t = an.transcribe(&a, None, &never).unwrap();
        let text: Vec<String> = t.words.iter().map(|w| norm(&w.text)).collect();
        for want in ["took", "seconds", "done", "happy"] {
            assert!(text.iter().any(|w| w == want), "{want} in {text:?}");
        }
        assert!(!t.revision.is_empty());
        // a cut point between "this" and "took", inside the pair's window
        let k = text.iter().position(|w| w == "this").unwrap();
        let (w1, w2) = (&t.words[k], &t.words[k + 1]);
        for rule in [EdgeRule::EnergyMin, EdgeRule::Arbiter] {
            match an.edge(
                &a,
                rule,
                (&w1.text, w1.start, w1.end),
                Some((&w2.text, w2.start, w2.end)),
            ) {
                Ok(e) => assert!(
                    e.value.0 >= w1.end.0 - SEC / 5 && e.value.0 <= w2.start.0 + SEC / 5,
                    "{rule:?} {e:?}"
                ),
                Err(HostError::Failed(m)) => assert_eq!(m, "unresolvable edge"),
                Err(e) => panic!("{e:?}"),
            }
        }
        let s = an
            .edge(
                &a,
                EdgeRule::AudibleStart,
                (&w1.text, w1.start, w1.end),
                None,
            )
            .unwrap();
        assert!((s.value.0 - w1.start.0).abs() < SEC / 2, "{s:?}");
        // the pause `say` left is a sweep candidate, and no word is inside any cut
        let dur = Flicks(t.words.last().unwrap().end.0 + SEC / 2);
        let p = an
            .pauses(
                &a,
                &[(Flicks(0), dur)],
                Flicks(SEC * 4 / 10),
                Flicks(SEC * 15 / 100),
                &t.words,
            )
            .unwrap();
        assert!(
            !p.cuts.is_empty(),
            "{p:?} words {:?}",
            t.words
                .iter()
                .map(|w| (
                    w.text.clone(),
                    w.start.0 as f64 / SEC as f64,
                    w.end.0 as f64 / SEC as f64
                ))
                .collect::<Vec<_>>()
        );
        for c in &p.cuts {
            // nothing audible is cut (the check is the level: the ASR runs a word on into
            // the silence after it, and hears words in near-silence)
            let v = voice(&a, Some((c.from, c.to)), &never).unwrap();
            let e = Energy::at_hop(&v.pcm, VOICE_RATE, Hop::Ms40).unwrap();
            let loudest = e.db.iter().copied().fold(f32::MIN, f32::max);
            assert!(loudest < -35.0, "a cut over speech: {c:?} at {loudest} dB");
        }
        let fl = an.fillers(&a, &[(Flicks(0), dur)], &t.words).unwrap();
        eprintln!("fillers: {:?}", fl.cuts);
        let h = an.listen(&a, Flicks(0), Flicks(3 * SEC)).unwrap();
        assert!(!h.transcribed.words.is_empty());
        // verify{joins}: a two-part plan whose "render" is the source itself passes
        let split = (w2.start.0 * 48_000 / SEC) as u64; // the second part starts at "took"
        let frames_a = split * 30 / 48_000;
        let first_b = frames_a * 48_000 / 30;
        let total = (dur.0 * 30 / SEC) as u64;
        let plan = json!({
            "schema": "gmclip.renderplan/0",
            "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
                "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                          "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
                "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}, "frames": total},
            "background": {"kind": "colour", "rgb": "#000000"},
            "sources": [{"id": "s", "path": src.to_str().unwrap()}],
            "parts": [
                {"id": "p1", "out": [0, frames_a], "picture": {"source": "s", "first_frame": 0}, "audio": {"source": "s", "first_sample": 0, "rate": 48000}, "layers": []},
                {"id": "p2", "out": [frames_a, total], "picture": {"source": "s", "first_frame": frames_a}, "audio": {"source": "s", "first_sample": first_b, "rate": 48000}, "layers": []}],
            "audio": {"loudness": {"mode": "as_recorded"}},
        });
        let plan = gmclip_core::gmclip_plan::parse(&serde_json::to_vec(&plan).unwrap()).unwrap();
        let r = verify_joins(&an, &plan, &src).unwrap();
        assert_eq!(r["passed"], true, "{r}");
        assert!(r["joins"][0]["expected"].as_u64().unwrap() >= 2, "{r}");
        // … and one that skips 2.4 s of speech at the join fails: the render (the source
        // as recorded) does not hold what the plan says plays after it
        let mut v: Value = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        v["parts"][1]["audio"]["first_sample"] = json!(first_b + 115_200);
        let skipped = gmclip_core::gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
        let r = verify_joins(&an, &skipped, &src).unwrap();
        assert_eq!(r["passed"], false, "{r}");
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// D.02: the voice a chunked transcription decodes, window after window, is the whole
/// track once its windows reach the end; a gap (a resumed job) or a run that stops early
/// gives nothing, and the analysis then reads the track itself.
#[cfg(test)]
mod tap_tests {
    use super::*;
    use gmclip_core::analysis::chunked;

    /// 16 kHz samples of `[from, to)` flicks of a synthetic track, as `voice()` returns
    /// them for a span.
    fn window(track: &[f32], from: i64, to: i64) -> Voice {
        let s = |t: i64| usize::try_from(t * i64::from(VOICE_RATE) / SEC).unwrap();
        let (a, b) = (s(from), s(to).min(track.len()));
        Voice {
            pcm: track[a..b].to_vec(),
            start: Flicks(from),
            to_end: b >= track.len(),
            track_len: track.len(),
        }
    }

    fn track(secs: i64) -> Vec<f32> {
        (0..secs * i64::from(VOICE_RATE))
            .map(|i| ((i % 977) as f32 / 977.0) - 0.5)
            .collect()
    }

    #[test]
    fn a_whole_chunked_transcription_is_the_whole_track() {
        let secs = 250;
        let tr = track(secs);
        let total = secs * SEC;
        let mut tap = VoiceTap::default();
        let mut done = 0;
        while done < total {
            let (end, a, b) = chunked::window(done, total);
            tap.add(&window(&tr, a, b));
            done = end;
        }
        assert!(tap.complete && !tap.broken);
        assert_eq!(tap.pcm.len(), tr.len());
        assert!(
            tap.pcm == tr,
            "the kept voice is the track, sample for sample"
        );
    }

    #[test]
    fn a_resumed_or_unfinished_transcription_keeps_nothing() {
        let secs = 250;
        let tr = track(secs);
        let total = secs * SEC;
        // resumed at its second window: the samples before it were never decoded here
        let mut tap = VoiceTap::default();
        let (first_end, _, _) = chunked::window(0, total);
        let (_, a, b) = chunked::window(first_end, total);
        tap.add(&window(&tr, a, b));
        assert!(tap.broken && !tap.complete && tap.pcm.is_empty());
        // stopped after its first window
        let mut tap = VoiceTap::default();
        let (_, a, b) = chunked::window(0, total);
        tap.add(&window(&tr, a, b));
        assert!(!tap.complete);
        let host = CliAnalysis::new();
        let src = AudioSource {
            path: PathBuf::from("/nonexistent.mp4"),
            track: None,
            fingerprint: "fp".into(),
        };
        host.keep_voice(&src, true);
        host.tap(&src, &window(&tr, a, b));
        assert!(
            host.take_voice(&src).is_none(),
            "incomplete: read the track"
        );
        assert!(host.take_voice(&src).is_none(), "and the tap is gone");
        // a whole one is taken once
        host.keep_voice(&src, true);
        host.tap(&src, &window(&tr, 0, total));
        assert_eq!(host.take_voice(&src).map(|v| v.len()), Some(tr.len()));
        assert!(host.take_voice(&src).is_none());
        // without keep_voice nothing is kept
        host.tap(&src, &window(&tr, 0, total));
        assert!(host.take_voice(&src).is_none());
    }

    /// "for you." then 3 s of room tone, where the ASR timed "you." from 1.05 to 3.9 s
    /// (bb mobile demo kept 2.8 s of that silence): the late word protects none of it.
    #[test]
    fn a_word_timed_into_the_silence_after_it_keeps_none_of_it() {
        let rate = VOICE_RATE as usize;
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2e-4 - 1e-4
        };
        let tone = |i: usize| 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin();
        let pcm: Vec<f32> = (0..5 * rate)
            .map(|i| match i {
                i if i < rate || i >= 4 * rate => tone(i),
                _ => noise(),
            })
            .collect();
        let v = Voice {
            track_len: pcm.len(),
            pcm,
            start: Flicks(0),
            to_end: true,
        };
        let s = |x: f64| Flicks((x * SEC as f64) as i64);
        let word = |text: &str, a: f64, b: f64| HeardWord {
            text: text.into(),
            start: s(a),
            end: s(b),
            confidence: None,
        };
        let words = [
            word("for", 0.5, 1.0),
            word("you.", 1.05, 3.9),
            word("okay.", 4.0, 4.9),
        ];
        let (cuts, _) = pause_cuts(&v, (s(0.0), s(5.0)), s(0.4), s(0.15), &words).unwrap();
        let removed: i64 = cuts.iter().map(|c| c.to.0 - c.from.0).sum();
        assert!(removed > s(2.5).0, "{cuts:?}");
        assert!(
            cuts.iter().all(|c| c.from >= s(1.0) && c.to <= s(4.0)),
            "never a heard word: {cuts:?}"
        );
    }

    /// Decision 0049: a part keeps 40 ms before its first word is heard and 120 ms after its
    /// last (a breath before it goes too), and a pause inside it keeps 100 ms each side.
    #[test]
    fn a_sweep_puts_a_parts_edges_on_its_voice() {
        let rate = VOICE_RATE as usize;
        let mut seed = 7u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2e-4 - 1e-4
        };
        let tone = |i: usize, amp: f32| {
            amp * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin()
        };
        let at = |x: f64| (x * rate as f64) as usize;
        // a breath at 0.3–0.5 s, "hello" 1.0–1.6 s, "there" 2.2–2.5 s, the room to 3 s
        let pcm: Vec<f32> = (0..3 * rate)
            .map(|i| match i {
                i if (at(0.3)..at(0.5)).contains(&i) => tone(i, 0.008),
                i if (at(1.0)..at(1.6)).contains(&i) || (at(2.2)..at(2.5)).contains(&i) => {
                    tone(i, 0.3)
                }
                _ => noise(),
            })
            .collect();
        let v = Voice {
            track_len: pcm.len(),
            pcm,
            start: Flicks(0),
            to_end: true,
        };
        let s = |x: f64| Flicks((x * SEC as f64) as i64);
        let word = |text: &str, a: f64, b: f64| HeardWord {
            text: text.into(),
            start: s(a),
            end: s(b),
            confidence: None,
        };
        let words = [word("hello", 1.05, 1.55), word("there", 2.25, 2.9)];
        let (cuts, _) = pause_cuts(&v, (s(0.0), s(3.0)), s(0.32), s(0.10), &words).unwrap();
        let near = |a: Flicks, b: f64, tol: f64| (a.0 - s(b).0).abs() <= s(tol).0;
        let has = |from: f64, to: f64, tol: f64| {
            cuts.iter()
                .any(|c| near(c.from, from, tol) && near(c.to, to, tol))
        };
        assert!(
            has(0.0, 0.96, 0.01),
            "the head to 40 ms before hello: {cuts:?}"
        );
        assert!(
            has(2.62, 3.0, 0.01),
            "the tail from 120 ms after there: {cuts:?}"
        );
        assert!(
            has(1.7, 2.1, 0.045),
            "the pause less 100 ms each side: {cuts:?}"
        );
        assert_eq!(cuts.len(), 3, "{cuts:?}");
        // a part that starts inside hello keeps the rest of it
        let (cuts, _) = pause_cuts(&v, (s(1.3), s(3.0)), s(0.32), s(0.10), &words).unwrap();
        assert!(cuts.iter().all(|c| c.from >= s(1.6)), "{cuts:?}");
    }

    /// Decision 0049: a cut point between two words keeps 120 ms after the first is heard
    /// and 40 ms before the second, where the gap holds both.
    #[test]
    fn a_cut_point_keeps_the_voice_either_side() {
        let rate = VOICE_RATE as usize;
        let at = |x: f64| (x * rate as f64) as usize;
        let pcm: Vec<f32> = (0..3 * rate)
            .map(|i| {
                if (at(0.5)..at(1.0)).contains(&i) || (at(1.6)..at(2.0)).contains(&i) {
                    0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin()
                } else {
                    1e-4 * ((i * 7919 % 101) as f32 / 50.0 - 1.0)
                }
            })
            .collect();
        let fine = Energy::fine(&pcm, VOICE_RATE).unwrap();
        let v = Voice {
            track_len: pcm.len(),
            pcm,
            start: Flicks(0),
            to_end: true,
        };
        let (s1, s2) = (Span::new(at(0.55), at(1.3)), Span::new(at(1.62), at(1.95)));
        let near = |got: usize, want: f64| got.abs_diff(at(want)) <= at(0.01);
        let (got, why) = on_the_voice(&v, &fine, s1, s2, at(1.02));
        assert!(near(got, 1.12) && !why.is_empty(), "{got}");
        let (got, _) = on_the_voice(&v, &fine, s1, s2, at(1.59));
        assert!(near(got, 1.56), "{got}");
        let (got, why) = on_the_voice(&v, &fine, s1, s2, at(1.3));
        assert_eq!((got, why.is_empty()), (at(1.3), true), "inside: left alone");
    }
}
