//! `gmclip remux` (B-06, the K2 remux probe): re-wrap an H.264 + AAC MP4 with our own
//! writer, so the platforms judge *our container* on coded samples known to be good.
//!
//! Two forms, because an AAC stream encoded with priming P from its first sample cannot
//! be put at t = 0 without an edit list (ffmpeg reads a non-zero iTunSMPB priming as
//! "skip P samples" but keeps the timeline, so the audio lands P samples late; only
//! AVFoundation re-zeroes it — measured, B-06 review):
//!
//! - [`remux`]: every sample byte-identical. The video's edit becomes a PTS shift (first
//!   presented frame at 0, `ctts` v1); the audio keeps the input's edit as an `elst`
//!   (media time = the encoder delay), the one form exact in every player. So this file
//!   has an audio `edts`, which exports never have.
//! - [`remux_with_audio`]: video byte-identical as above; the audio is supplied already
//!   encoded the export way (fed from cut sample D, SPEC §6.5 step 3), muxed with the
//!   iTunSMPB priming-0 tag and **no `edts` at all** — exactly what an export writes.
//!
//! The input is read with [`super::inspect`] (whole file in memory, ≤ 4 GiB) until B-01's
//! demuxer lands. Inputs with more than one video or audio track, leading empty edits,
//! end trims, or an audio timescale other than its sample rate are refused.

use std::path::Path;

use super::inspect::{self, TrackInfo};
use super::{
    ColorTags, Gapless, Mp4Writer, MuxError, MuxPlan, MuxReport, Sample, TrackConfig, TrackEdit,
};
use crate::audio::priming;

/// Largest input `remux` reads into memory.
pub const MAX_INPUT_BYTES: u64 = 4 << 30;

/// What a remux did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemuxReport {
    pub video_samples: usize,
    pub audio_samples: usize,
    /// Ticks the video timeline moved by (the input's video `elst` media time).
    pub video_shift: i64,
    /// The audio edit written ([`remux`]), or none ([`remux_with_audio`]).
    pub audio_edit: Option<TrackEdit>,
    /// The iTunSMPB tag written ([`remux_with_audio`]), or none.
    pub gapless: Option<Gapless>,
    pub mux: MuxReport,
}

#[derive(Debug, thiserror::Error)]
pub enum RemuxError {
    /// The input could not be read.
    #[error("reading the input: {0}")]
    Read(std::io::Error),
    /// The input is not something remux takes.
    #[error("input: {0}")]
    Input(String),
    /// Writing the output failed.
    #[error("writing: {0}")]
    Write(std::io::Error),
    #[error("mux: {0}")]
    Mux(MuxError),
}

impl From<MuxError> for RemuxError {
    fn from(e: MuxError) -> Self {
        match e {
            MuxError::Io(io) => Self::Write(io),
            other => Self::Mux(other),
        }
    }
}

fn bad(s: impl Into<String>) -> RemuxError {
    RemuxError::Input(s.into())
}

/// Where a track's presentation ends: max(PTS + duration) over its samples (for B-frame
/// video this is past the decode-time media duration), else the media duration.
fn composition_end(t: &TrackInfo) -> u64 {
    t.samples
        .iter()
        .map(|s| s.pts.saturating_add(i64::from(s.duration)))
        .max()
        .and_then(|e| u64::try_from(e).ok())
        .unwrap_or(t.media_duration)
}

/// A track's single normal-rate edit as (media start, presented length) in the track
/// timescale; no edit: (0, the whole presentation). The presented length is taken from
/// the samples when the edit's (movie-timescale) length is within one movie tick of it.
pub(crate) fn single_edit(t: &TrackInfo, movie_ts: u32) -> Result<(i64, u64), RemuxError> {
    let end = composition_end(t);
    match t.edits.as_slice() {
        [] => Ok((0, end)),
        [e] if e.media_time >= 0 && e.rate == 0x0001_0000 => {
            let media_time = e.media_time;
            let rest = end
                .checked_sub(media_time.unsigned_abs())
                .ok_or_else(|| bad("edit starts past the media"))?;
            let ts = u128::from(t.timescale.max(1));
            let movie = u128::from(movie_ts.max(1));
            let from_edit = u128::from(e.segment_duration) * ts / movie;
            let tick = ts.div_ceil(movie);
            let diff = i128::try_from(from_edit).unwrap_or(i128::MAX) - i128::from(rest);
            let presented = if diff.unsigned_abs() <= tick {
                rest
            } else {
                u64::try_from(from_edit).map_err(|_| bad("edit length"))?
            };
            if presented > rest {
                return Err(bad("edit runs past the media"));
            }
            Ok((media_time, presented))
        }
        [e, ..] if e.media_time < 0 => Err(bad(
            "a leading empty edit (delayed track start) is not supported",
        )),
        _ => Err(bad("only a single normal-rate edit is supported")),
    }
}

struct Input {
    file: Vec<u8>,
    v: TrackInfo,
    a: TrackInfo,
    movie_ts: u32,
}

fn read_input(input: &Path) -> Result<Input, RemuxError> {
    let len = std::fs::metadata(input).map_err(RemuxError::Read)?.len();
    if len > MAX_INPUT_BYTES {
        return Err(bad(format!(
            "{len} bytes is over the {MAX_INPUT_BYTES}-byte probe limit"
        )));
    }
    let file = std::fs::read(input).map_err(RemuxError::Read)?;
    let tracks = inspect::tracks(&file).map_err(|e| bad(e.to_string()))?;
    let movie_ts = inspect::movie_timescale(&file).map_err(|e| bad(e.to_string()))?;
    let mut video = tracks.iter().filter(|t| &t.handler == b"vide");
    let mut audio = tracks.iter().filter(|t| &t.handler == b"soun");
    let (Some(v), Some(a)) = (video.next(), audio.next()) else {
        return Err(bad("needs one video and one audio track"));
    };
    if video.next().is_some() || audio.next().is_some() || tracks.len() != 2 {
        return Err(bad("more than one video and one audio track"));
    }
    if &v.sample_entry != b"avc1" || &a.sample_entry != b"mp4a" {
        return Err(bad("remux takes avc1 + mp4a"));
    }
    if v.entry.config.is_empty() || a.entry.config.is_empty() {
        return Err(bad("missing avcC or AudioSpecificConfig"));
    }
    if a.timescale != a.entry.sample_rate {
        return Err(bad("audio timescale differs from its sample rate"));
    }
    Ok(Input {
        v: v.clone(),
        a: a.clone(),
        file,
        movie_ts,
    })
}

/// The video track config and its PTS shift (input colour tags kept; untagged stays
/// untagged).
fn video_config(i: &Input) -> Result<(TrackConfig, i64), RemuxError> {
    let (shift, presented) = single_edit(&i.v, i.movie_ts)?;
    if presented + shift.unsigned_abs() != composition_end(&i.v) {
        return Err(bad("a video edit that trims the end is not supported"));
    }
    let colour =
        i.v.entry
            .colr
            .map(|(primaries, transfer, matrix, full_range)| ColorTags {
                primaries,
                transfer,
                matrix,
                full_range,
            });
    let mut cfg = TrackConfig::h264(
        i.v.entry.config.clone(),
        i.v.entry.width,
        i.v.entry.height,
        1,
        1,
        colour,
    );
    cfg.timescale = i.v.timescale; // the input's ticks, so every PTS maps 1:1
    Ok((cfg, shift))
}

fn sample_bytes<'a>(file: &'a [u8], s: &inspect::SampleInfo) -> Result<&'a [u8], RemuxError> {
    let at = usize::try_from(s.offset).map_err(|_| bad("offset"))?;
    file.get(at..at.saturating_add(s.size as usize))
        .ok_or_else(|| bad("sample outside the file"))
}

/// Where the samples come from.
struct Sources<'a> {
    vfile: &'a [u8],
    v: &'a TrackInfo,
    shift: i64,
    afile: &'a [u8],
    /// Audio samples with the durations to write.
    audio: &'a [(inspect::SampleInfo, u32)],
    a_timescale: u32,
}

/// Writes video (shifted by `shift`) and audio samples, interleaved on the decode times
/// actually written, to `out` via a temporary file that is removed on any error.
fn write(
    out: &Path,
    tracks: Vec<TrackConfig>,
    plan: &MuxPlan,
    src: &Sources<'_>,
) -> Result<MuxReport, RemuxError> {
    let tmp = out.with_extension("partial.mp4");
    let result = (|| -> Result<MuxReport, RemuxError> {
        let f = std::fs::File::create(&tmp).map_err(RemuxError::Write)?;
        let mut w = Mp4Writer::create(std::io::BufWriter::new(f), tracks, plan)?;
        let v = src.v;
        let v_dts0 = v.samples.first().map_or(0, |s| s.dts);
        let (mut i, mut j, mut a_dts) = (0usize, 0usize, 0i64);
        while i < v.samples.len() || j < src.audio.len() {
            let take_video = match (v.samples.get(i), src.audio.get(j)) {
                // The writer rebases video DTS to start at 0; interleave on that.
                (Some(vs), Some(_)) => {
                    i128::from(vs.dts - v_dts0) * i128::from(src.a_timescale)
                        <= i128::from(a_dts) * i128::from(v.timescale)
                }
                (Some(_), None) => true,
                _ => false,
            };
            if take_video {
                let Some(s) = v.samples.get(i) else { break };
                let data = sample_bytes(src.vfile, s)?;
                let (dts, pts) = (s.dts - src.shift, s.pts - src.shift);
                w.write_sample(
                    0,
                    &Sample {
                        data,
                        dts,
                        pts,
                        duration: s.duration,
                        sync: s.sync,
                    },
                )?;
                i += 1;
            } else {
                let Some((s, d)) = src.audio.get(j) else {
                    break;
                };
                let data = sample_bytes(src.afile, s)?;
                w.write_sample(
                    1,
                    &Sample {
                        data,
                        dts: a_dts,
                        pts: a_dts,
                        duration: *d,
                        sync: true,
                    },
                )?;
                a_dts += i64::from(*d);
                j += 1;
            }
        }
        let (_, mux) = w.finish()?;
        Ok(mux)
    })();
    match result {
        Ok(mux) => {
            std::fs::rename(&tmp, out).map_err(RemuxError::Write)?;
            Ok(mux)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn count(n: usize, what: &str) -> Result<u32, RemuxError> {
    u32::try_from(n).map_err(|_| bad(format!("too many {what} samples")))
}

/// Re-wraps `input` into `out` sample for sample (the audio keeps its encoder delay as an
/// `elst`; see the module docs).
pub fn remux(input: &Path, out: &Path) -> Result<RemuxReport, RemuxError> {
    let i = read_input(input)?;
    let (vcfg, shift) = video_config(&i)?;
    let (priming, presented) = single_edit(&i.a, i.movie_ts)?;
    let mut acfg = TrackConfig::aac(i.a.entry.config.clone(), i.a.entry.channels, i.a.timescale);
    let segment = i.a.edits.first().map(|e| e.segment_duration);
    acfg.edit = (priming != 0 || presented != composition_end(&i.a)).then_some(TrackEdit {
        media_time: priming,
        presented,
        segment,
    });
    let plan = MuxPlan {
        sample_counts: vec![
            count(i.v.samples.len(), "video")?,
            count(i.a.samples.len(), "audio")?,
        ],
        max_payload_bytes: Some(i.file.len() as u64),
        gapless: None,
    };
    let audio: Vec<_> = i.a.samples.iter().map(|s| (*s, s.duration)).collect();
    let src = Sources {
        vfile: &i.file,
        v: &i.v,
        shift,
        afile: &i.file,
        audio: &audio,
        a_timescale: i.a.timescale,
    };
    let mux = write(out, vec![vcfg, acfg.clone()], &plan, &src)?;
    Ok(RemuxReport {
        video_samples: i.v.samples.len(),
        audio_samples: audio.len(),
        video_shift: shift,
        audio_edit: acfg.edit,
        gapless: None,
        mux,
    })
}

/// Re-wraps `input`'s video sample for sample with the audio of `aac` (an MP4/M4A whose
/// AAC was encoded the export way, fed from cut sample D): its first `ceil(cut/1024)`
/// packets, the last shortened so the track lasts `cut_samples`, the priming-0 iTunSMPB
/// tag and no `edts` (decision 0004).
pub fn remux_with_audio(
    input: &Path,
    aac: &Path,
    cut_samples: u64,
    out: &Path,
) -> Result<RemuxReport, RemuxError> {
    let i = read_input(input)?;
    let (vcfg, shift) = video_config(&i)?;
    let afile = std::fs::read(aac).map_err(RemuxError::Read)?;
    let at = inspect::tracks(&afile).map_err(|e| bad(e.to_string()))?;
    let a = at
        .iter()
        .find(|t| &t.handler == b"soun")
        .ok_or_else(|| bad("no audio track in the AAC file"))?;
    if a.timescale != 48_000 || a.entry.config.is_empty() {
        return Err(bad(
            "the AAC file must be 48 kHz with an AudioSpecificConfig",
        ));
    }
    let durs = priming::packet_durations(cut_samples);
    if a.samples.len() < durs.len() {
        return Err(bad(format!(
            "the AAC file has {} packets, the cut needs {}",
            a.samples.len(),
            durs.len()
        )));
    }
    let audio: Vec<_> = a.samples.iter().zip(&durs).map(|(s, d)| (*s, *d)).collect();
    let gapless = priming::gapless(cut_samples);
    let acfg = TrackConfig::aac(a.entry.config.clone(), a.entry.channels, 48_000);
    let plan = MuxPlan {
        sample_counts: vec![
            count(i.v.samples.len(), "video")?,
            count(audio.len(), "audio")?,
        ],
        max_payload_bytes: Some((i.file.len() + afile.len()) as u64),
        gapless: Some(gapless),
    };
    let src = Sources {
        vfile: &i.file,
        v: &i.v,
        shift,
        afile: &afile,
        audio: &audio,
        a_timescale: 48_000,
    };
    let mux = write(out, vec![vcfg, acfg], &plan, &src)?;
    Ok(RemuxReport {
        video_samples: i.v.samples.len(),
        audio_samples: audio.len(),
        video_shift: shift,
        audio_edit: None,
        gapless: Some(gapless),
        mux,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::mux::inspect::{Edit, EntryInfo};

    fn track(edits: Vec<Edit>, media: u64, ts: u32) -> TrackInfo {
        TrackInfo {
            id: 1,
            handler: *b"soun",
            timescale: ts,
            media_duration: media,
            tkhd_duration: 0,
            sample_entry: *b"mp4a",
            has_edts: !edits.is_empty(),
            ctts_version: None,
            cslg: None,
            has_stss: false,
            co64: false,
            samples: vec![],
            edits,
            entry: EntryInfo::default(),
        }
    }

    #[test]
    fn edits_translate_exactly() {
        // h79's audio: media 2892224, priming 1024, elst 60233 ms → exact 2891200.
        let e = Edit {
            segment_duration: 60_233,
            media_time: 1024,
            rate: 0x0001_0000,
        };
        assert_eq!(
            single_edit(&track(vec![e], 2_892_224, 48_000), 1000).unwrap(),
            (1024, 2_891_200)
        );
        assert_eq!(
            single_edit(&track(vec![], 9600, 48_000), 1000).unwrap(),
            (0, 9600)
        );
        // An end trim far from a tick is kept as the edit says.
        let trim = Edit {
            segment_duration: 100,
            media_time: 0,
            rate: 0x0001_0000,
        };
        assert_eq!(
            single_edit(&track(vec![trim], 48_000, 48_000), 1000).unwrap(),
            (0, 4800)
        );
        // Refused: a leading empty edit, a rate change, two edits, an edit past the media.
        let empty = Edit {
            segment_duration: 10,
            media_time: -1,
            rate: 0x0001_0000,
        };
        let normal = Edit {
            segment_duration: 1000,
            media_time: 0,
            rate: 0x0001_0000,
        };
        assert!(single_edit(&track(vec![empty, normal], 48_000, 48_000), 1000).is_err());
        assert!(
            single_edit(
                &track(
                    vec![Edit {
                        rate: 0x0002_0000,
                        ..normal
                    }],
                    48_000,
                    48_000
                ),
                1000
            )
            .is_err()
        );
        assert!(single_edit(&track(vec![normal, normal], 96_000, 48_000), 1000).is_err());
        assert!(
            single_edit(
                &track(
                    vec![Edit {
                        media_time: 50_000,
                        ..normal
                    }],
                    48_000,
                    48_000
                ),
                1000
            )
            .is_err()
        );
    }
}
