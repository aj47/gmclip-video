//! Our own MP4 writer (B-04; SPEC §6.5 step 4, R-EXP-5).
//!
//! Layout: `ftyp` → reserved region (`moov` + `free` padding) → `mdat` (largesize). The
//! reservation is sized from the **planned sample counts** with every table at its worst
//! case, so the real `moov` always fits; it is built and patched in place once the last
//! sample is written. Rules (SPEC §6.5): no `edts` ever; the first presentation time of
//! every track is 0 (the writer refuses anything else); `ctts` iff some PTS ≠ DTS, version 1
//! iff an offset is negative, with `cslg` beside it; `stss` iff some sample is not sync;
//! video timescale = fps × 512 for integer rates (30 → 15360), else 90000; audio at its
//! sample rate. Parameter sets are copied verbatim (`avcC`, `esds`), never rewritten.
//!
//! Video tracks must start on a sync sample. The reservation costs ≤ 52 bytes per
//! *planned* sample (worst-case tables), so callers pass exact counts where they have
//! them (an export knows its frame and packet counts). A writer dropped without a
//! successful [`Mp4Writer::finish`] leaves an unusable file (its `mdat` size is unpatched):
//! write to a temporary path and rename on success, as every export does.
//!
//! [`plan`] runs the same layout on sample metadata alone (FZ4's target, SR §3.2). The
//! offset choice is: `co64` iff the planned bound puts the payload past 2^32, or a real
//! chunk offset passes 2^32 (then a planned `stco` is upgraded; the reservation is always
//! sized for `co64`).
//! [`inspect`] is our own box-tree reader used by the conformance tests (B-05).
#![deny(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]

mod bytes;
pub mod inspect;
mod moov;
mod mp4w;
pub mod remux;
pub mod trim;

pub use mp4w::{Mp4Writer, NullSink, PlannedFile, PlannedSample, Sample, plan};

/// Movie timescale of `mvhd`/`tkhd` durations (ffmpeg's and AVFoundation's default).
pub const MOVIE_TIMESCALE: u32 = 1000;

/// Video timescale for a frame rate (SPEC §6.5 step 4): `num × 512` for integer rates
/// (30 → 15360, matching the reference), the numerator for an NTSC rate (30000/1001 → 30000, so a
/// frame is exactly 1001 ticks; decision 0020), else 90000.
#[must_use]
pub fn video_timescale(fps_num: u32, fps_den: u32) -> u32 {
    let g = gcd(fps_num, fps_den).max(1);
    if fps_den / g == 1
        && let Some(t) = (fps_num / g).checked_mul(512)
        && t > 0
    {
        return t;
    }
    if fps_den / g == 1001 && fps_num / g > 0 {
        return fps_num / g;
    }
    90_000
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Errors from planning or writing an MP4.
#[derive(Debug, thiserror::Error)]
pub enum MuxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid track {track}: {reason}")]
    InvalidTrack { track: usize, reason: String },
    #[error("track {track}: more samples than planned ({planned})")]
    OverPlan { track: usize, planned: u32 },
    #[error("track {track} sample {sample}: {reason}")]
    InvalidSample {
        track: usize,
        sample: u32,
        reason: String,
    },
    #[error("track {track}: first presentation time is {first_pts}, must be 0 (no edit lists)")]
    FirstPtsNotZero { track: usize, first_pts: i64 },
    #[error("track {track} has no samples")]
    EmptyTrack { track: usize },
    #[error("a box or the moov reservation exceeds 32 bits")]
    BoxTooLarge,
    #[error("file offset overflow")]
    Overflow,
    #[error("internal: {0}")]
    Internal(&'static str),
}

/// Colour description written as `colr`/`nclx` (ISO/IEC 23001-8 code points).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorTags {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

impl ColorTags {
    /// BT.709 limited range (the export default, R-EXP-6 "bt709 tags").
    pub const BT709_LIMITED: Self = Self {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };
}

/// What a track carries.
#[derive(Clone, Debug)]
pub enum Codec {
    /// H.264 in `avc1` with the encoder's `avcC` record copied verbatim.
    H264 {
        avcc: Vec<u8>,
        width: u16,
        height: u16,
        color: Option<ColorTags>,
    },
    /// AAC in `mp4a`/`esds` with the encoder's AudioSpecificConfig copied verbatim.
    Aac {
        asc: Vec<u8>,
        channels: u16,
        sample_rate: u32,
    },
}

/// One track's configuration.
#[derive(Clone, Debug)]
pub struct TrackConfig {
    pub codec: Codec,
    /// Media timescale (ticks per second).
    pub timescale: u32,
    /// An `elst` for this track. Exports never set it (SPEC §6.5, R-EXP-6: no `edts`);
    /// only `remux` does, to keep an input's AAC encoder delay exact in every player.
    pub edit: Option<TrackEdit>,
}

/// One normal-rate edit: present `presented` ticks of media starting at `media_time`
/// (both in the track timescale).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackEdit {
    pub media_time: i64,
    pub presented: u64,
    /// The `elst` segment duration in the movie timescale, when it must be exactly an
    /// input's (a remux); otherwise `presented` rounded to the nearest movie tick.
    pub segment: Option<u64>,
}

impl TrackConfig {
    /// An H.264 track at `fps_num/fps_den`, timescale per [`video_timescale`].
    #[must_use]
    pub fn h264(
        avcc: Vec<u8>,
        width: u16,
        height: u16,
        fps_num: u32,
        fps_den: u32,
        color: Option<ColorTags>,
    ) -> Self {
        Self {
            codec: Codec::H264 {
                avcc,
                width,
                height,
                color,
            },
            timescale: video_timescale(fps_num, fps_den),
            edit: None,
        }
    }

    /// An AAC track; the timescale is the sample rate.
    #[must_use]
    pub fn aac(asc: Vec<u8>, channels: u16, sample_rate: u32) -> Self {
        Self {
            codec: Codec::Aac {
                asc,
                channels,
                sample_rate,
            },
            timescale: sample_rate,
            edit: None,
        }
    }

    pub(crate) fn is_video(&self) -> bool {
        matches!(self.codec, Codec::H264 { .. })
    }

    pub(crate) fn validate(&self, track: usize) -> Result<(), MuxError> {
        let bad = |reason: &str| {
            Err(MuxError::InvalidTrack {
                track,
                reason: reason.to_owned(),
            })
        };
        if self.timescale == 0 {
            return bad("timescale is 0");
        }
        match &self.codec {
            Codec::H264 {
                avcc,
                width,
                height,
                ..
            } => {
                // avcC: configurationVersion 1, profile, compat, level, then lengthSize.
                if avcc.len() < 7 || avcc.first() != Some(&1) {
                    return bad("avcC must start with configurationVersion 1 and hold ≥ 7 bytes");
                }
                if *width == 0 || *height == 0 {
                    return bad("width and height must be non-zero");
                }
                if avcc.len() > 1 << 20 {
                    return bad("avcC larger than 1 MiB");
                }
            }
            Codec::Aac {
                asc,
                channels,
                sample_rate,
            } => {
                if asc.is_empty() || asc.len() > 64 {
                    return bad("AudioSpecificConfig must be 1..=64 bytes");
                }
                // A version-0 AudioSampleEntry holds the rate as 16.16.
                if *channels == 0 || *sample_rate == 0 || *sample_rate > 65_535 {
                    return bad("channels must be non-zero and the sample rate in 1..=65535");
                }
                if self.timescale != *sample_rate {
                    return bad("audio timescale must equal the sample rate");
                }
            }
        }
        Ok(())
    }
}

/// How chunk offsets are written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OffsetBoxes {
    /// 32-bit `stco`.
    Stco,
    /// 64-bit `co64`.
    Co64,
}

/// The plan the reservation is sized from (implementer Part C: "plan(counts)").
#[derive(Clone, Debug)]
pub struct MuxPlan {
    /// Upper bound on samples per track, in track order.
    pub sample_counts: Vec<u32>,
    /// Upper bound on the summed payload bytes of all samples, when known (e.g. from the
    /// preset's peak bitrate × duration). Decides `stco` vs `co64` up front; `None`
    /// chooses `co64`.
    pub max_payload_bytes: Option<u64>,
    /// Gapless info written as an iTunSMPB tag (`moov/udta/meta/ilst`), which ffmpeg and
    /// AVFoundation both honour. Without it AVFoundation assumes 2112 samples of AAC
    /// priming in a file with no edit list and plays the audio 44 ms early (measured,
    /// B-05). Exports write priming 0 (the encoder was fed from cut sample D, so decoded
    /// position q is cut sample q) and the cut's exact length.
    pub gapless: Option<Gapless>,
}

/// iTunSMPB fields: samples to skip at the start, padding at the end, and the length
/// that remains. `priming + length + padding` = the decoded AAC total (packets × 1024).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gapless {
    pub priming: u32,
    pub padding: u32,
    pub length: u64,
}

impl Gapless {
    /// The iTunSMPB value string, as AudioToolbox writes it.
    #[must_use]
    pub fn itunsmpb(&self) -> String {
        format!(
            " 00000000 {:08X} {:08X} {:016X}{}",
            self.priming,
            self.padding,
            self.length,
            " 00000000".repeat(8)
        )
    }
}

/// `ctts` as written for one track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ctts {
    None,
    V0,
    V1,
}

/// What the writer wrote for one track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackReport {
    pub samples: u32,
    pub chunks: u32,
    /// Media duration in the track timescale (sum of sample durations).
    pub duration: u64,
    pub timescale: u32,
    pub ctts: Ctts,
    pub cslg: bool,
    pub stss: bool,
    pub min_cts_offset: i64,
    pub max_cts_offset: i64,
    pub payload_bytes: u64,
}

/// What the writer wrote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MuxReport {
    /// The choice made from the plan.
    pub planned_offsets: OffsetBoxes,
    /// The boxes written (differs from the plan only if the payload outgrew the bound).
    pub offsets: OffsetBoxes,
    pub reserved: u64,
    pub moov_size: u64,
    pub mdat_offset: u64,
    pub mdat_size: u64,
    pub tracks: Vec<TrackReport>,
}

#[cfg(test)]
pub(crate) mod tests;
