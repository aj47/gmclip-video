//! The export pipeline (H-10, SPEC §6.5): RenderPlan v0 → MP4.
//!
//! Per output frame: the part's draw list (compositor `draw`), each layer's source
//! frame decoded frame-exactly ([`gmclip_media::backend::FrameStream`] for live layers,
//! a cache for held / frozen ones), composited on the GPU into the RGBA16F canvas,
//! converted to NV12 (H-07, 3 in flight), encoded by VideoToolbox (H-08) or Media Foundation
//! (M5)
//! and written by lane B's `Mp4Writer` (B-04) with the audio interleaved.
//!
//! Audio: the plan's mix graph (B-11) reads every part's samples through an AAC
//! random-access reader (H-04), the static loudness pass runs (B-12), the encoder is fed
//! from sample D on (priming compensation, `audio::priming`) and AAC-LC 192 kbps is
//! encoded once (H-09); packets are muxed with the gapless tag.
//!
//! The 1-s peak is metered on the encoded stream; over the social cap, the video pass
//! repeats down `RATE_CAP_LADDER`. A render writes `<out>.partial.mp4` and only a
//! finished one is installed (`export_record::install_export`, B-16), with the export
//! record beside it. Text events are drawn over the composite (H-06,
//! `compositor::text_layer`).
//!
//! Sources come in through [`Inputs`]: the demuxers (B-01/B-02) once they land; the
//! lab tests use an ffprobe-driven shim.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gmclip_media::backend::SampleSource;
use gmclip_plan::Plan;

use crate::compositor::ChromaSiting;
use crate::ops::RenderError;

#[cfg(any(target_os = "macos", windows))]
mod pipeline;
/// The plan's voice mix in ranges, for captions heard from the cut's own audio (§5.10).
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::VoiceMix;
/// Moment clips by remux of the scrub proxy (H.10).
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::clip;
/// Roto mask tracks (SPEC §5.8, R.01): what a plan needs, and building them with Apple
/// Vision `.accurate` into a [`crate::matte::store::MatteStore`] (macOS; Windows applies
/// tracks built on a Mac).
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::mattes;
/// The preview session (frame stream, SPEC D5): the export's compositor at the displayed
/// size with gizmos drawn in the same pass.
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::preview;
/// The scrub proxy (H.05, SPEC §6.3): built in the background per source, read while
/// scrubbing; playback and exact frames always read the master.
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::proxy;
/// Motion tracking (decision 0031): a point followed through a source with Apple Vision's
/// object tracker (macOS; Windows says it is not there yet).
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::track;
/// The mix's sample source (export and playback share it) and the sources a plan hears.
#[cfg(any(target_os = "macos", windows))]
pub use pipeline::{PlanPcm, heard_sources};
pub mod inputs;
pub mod sheet;

pub use inputs::DemuxInputs;

/// A source's video track as the export opens it.
pub struct VideoInput {
    pub source: Box<dyn SampleSource + Send>,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub siting: ChromaSiting,
}

impl std::fmt::Debug for VideoInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoInput")
            .field("codec", &self.codec)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// The codec of a video track.
#[derive(Clone, Debug)]
pub enum VideoCodec {
    H264 {
        avcc: Vec<u8>,
    },
    /// VP9: the configuration is synthesised from the first key frame.
    Vp9,
}

/// A source's audio track: AAC-LC, or linear PCM (decision 0051), at its own rate.
pub struct AudioInput {
    pub source: Box<dyn SampleSource + Send>,
    /// AudioSpecificConfig (AAC; empty for PCM).
    pub asc: Vec<u8>,
    /// How the samples are stored when the track is PCM.
    pub pcm: Option<gmclip_media::backend::pcm::PcmFormat>,
    pub sample_rate: u32,
    pub channels: u32,
    /// Leading decoded samples the container's edit list drops (priming).
    pub skip: u64,
    /// Samples of silence before the first one kept (an empty edit longer than the
    /// priming: [`gmclip_media::demux::Track::audio_start`]).
    pub lead: u64,
}

impl std::fmt::Debug for AudioInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioInput")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("skip", &self.skip)
            .field("lead", &self.lead)
            .finish_non_exhaustive()
    }
}

/// Opens plan sources by id (`plan.sources[].id`).
pub trait Inputs {
    fn video(&mut self, plan: &Plan, source: &str) -> Result<VideoInput, RenderError>;
    fn audio(&mut self, plan: &Plan, source: &str) -> Result<AudioInput, RenderError>;
}

/// Export options.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Set to cancel; checked once per output frame.
    pub cancel: Option<Arc<AtomicBool>>,
    /// `superseded/` keeps this many previous renders (B-16: 3).
    pub keep_superseded: Option<usize>,
    /// Record a SHA-256 of every NV12 frame handed to the encoder, and of the mix before
    /// loudness per [`MIX_HASH_BLOCK`] samples (diagnostics; preview = export checks).
    pub hash_encoder_input: bool,
    /// Set to the output frame being composited (a progress bar; lane U).
    pub progress: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// The roto mask-track cache (SPEC §5.8); `None`: tracks built for this export only.
    pub mattes: Option<Arc<crate::matte::store::MatteStore>>,
    /// Proceed with draft mattes instead of waiting for accurate tracks (only after the
    /// user confirmed it; the record lists them in `draft_mattes`).
    pub draft_mattes: bool,
    /// Set to the frames segmented while mask tracks are built (before the video pass).
    pub matte_progress: Option<Arc<std::sync::atomic::AtomicU64>>,
}

impl ExportOptions {
    /// Whether the export was asked to stop.
    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

/// What a finished export did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportReport {
    pub out: PathBuf,
    pub frames: u64,
    pub audio_samples: u64,
    pub seconds: f64,
    /// Fullest 1-s window of the video stream (decode order), bits.
    pub video_peak_bps: u64,
    /// Rung of `RATE_CAP_LADDER` the accepted encode used.
    pub rate_rung: usize,
    /// Video seeks (new decoder chains) across sources.
    pub video_seeks: u64,
    pub notes: Vec<String>,
    /// Per output frame, SHA-256 of the NV12 planes the encoder got (when asked).
    pub encoder_input_hashes: Vec<String>,
    /// The mix before loudness ([`mix_block_hashes`], when asked): what playback plays.
    pub mix_hashes: Vec<String>,
    /// `part/layer` ids exported with draft mattes (also on the export record).
    pub draft_mattes: Vec<String>,
}

/// Output samples per hash of [`mix_block_hashes`] (one AAC frame, one playback chunk).
pub const MIX_HASH_BLOCK: usize = 1024;

/// SHA-256 of each [`MIX_HASH_BLOCK`] samples of an interleaved stereo mix, as
/// little-endian `f32` (the last block may be shorter).
pub fn mix_block_hashes(mix: &[f32]) -> Vec<String> {
    mix.chunks(MIX_HASH_BLOCK * 2)
        .map(|b| {
            let bytes: Vec<u8> = b.iter().flat_map(|x| x.to_le_bytes()).collect();
            crate::sha256::hex(&bytes)
        })
        .collect()
}

/// Renders `plan` to `out` (an `.mp4`).
#[cfg(any(target_os = "macos", windows))]
pub fn export(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    out: &Path,
    opts: &ExportOptions,
) -> Result<ExportReport, RenderError> {
    pipeline::export(plan, inputs, out, opts)
}

/// Renders `plan` to `out` (Linux: M6).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn export(
    _plan: &Plan,
    _inputs: &mut dyn Inputs,
    _out: &Path,
    _opts: &ExportOptions,
) -> Result<ExportReport, RenderError> {
    Err(RenderError::Unsupported {
        step: "render",
        reason: "export runs on macOS and Windows (Linux M6)".into(),
    })
}

/// Output frames `at` as the export renders them, as RGB (H-05b stills and sheets).
#[cfg(any(target_os = "macos", windows))]
pub fn render_frames(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    at: &[u64],
) -> Result<Vec<(u64, sheet::RgbImage)>, RenderError> {
    pipeline::render_frames(plan, inputs, at)
}
