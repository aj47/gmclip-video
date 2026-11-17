//! Analysis (SPEC §9): pure-CPU DSP over a decoded voice track.
//!
//! Every function here takes PCM that someone else decoded (lane H's AAC decoder in
//! production, `tools/` ffmpeg in lab tests) and works in **sample indices** of that
//! buffer, so nothing is rounded until a caller converts to [`gmclip_model::Flicks`]
//! ([`span::Span`] does that conversion exactly).
//!
//! - [`pcm`]: down-mix and the rational polyphase resampler to the 16 kHz voice rate.
//! - [`energy`]: RMS level frames (10/20/40 ms hops; 10 ms windows at a 5 ms hop for edges).
//! - [`peaks`]: waveform min/max at 100 Hz.
//! - [`silence`]: short-term level percentiles, the §5.5 per-span threshold, the 40 ms
//!   quiet grid, pauses.
//! - [`edges`]: `audible_start/end` (18 dB drop held 60 ms) and `cut_point{energy_min}`,
//!   plus the arbiter's candidate minima (§9 Word edges).
//! - [`fillers`]: per-language filler words anchored on the quiet grid (§5.5).
//! - [`emphasis`]: the words a speaker stresses — louder or slower than their running
//!   baseline in the cut (caption keyword proposals).
//! - [`reactions`]: laughter and big reactions — a level spike over the source's own
//!   baseline with reaction words near it (moment candidates).
//! - [`blob`]: the immutable columnar blobs `gmclip-store` keeps per (owner, kind, revision).
#![forbid(unsafe_code)]

pub mod blob;
pub mod edges;
pub mod emphasis;
pub mod energy;
pub mod fillers;
pub mod pcm;
pub mod peaks;
pub mod reactions;
pub mod silence;
pub mod span;

/// Analysis errors. Messages are shape-only (no paths, no user text).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisError {
    #[error("invalid input: {0}")]
    Invalid(&'static str),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("corrupt blob: {0}")]
    Corrupt(&'static str),
}

/// Not built yet: loudness lives in lane B2's BS.1770 meter (B-12), prosody is M3/M4.
pub mod loudness {}
/// Delivery prosody (pYIN, pace): M3/M4 (SPEC §9).
pub mod prosody {}
