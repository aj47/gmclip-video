//! Container demuxers (SPEC §6.1): our own MP4/MOV box walker (B-01) and EBML/Matroska
//! reader (B-02). Both build a sample index without reading coded sample data.
//!
//! Every value read from a file is untrusted (SPEC §16). Parser budgets (R-MEDIA-SEC-2,
//! SR §3.5.7) hold throughout this module:
//! - `unwrap`/`expect` and slice indexing are denied by clippy below; reads go through
//!   [`io::Cursor`], which checks every length against the bytes that remain.
//! - Sample tables are capped at [`MAX_SAMPLES`] entries per track, box nesting at
//!   [`MAX_BOX_DEPTH`], EBML nesting at [`MAX_EBML_DEPTH`]; cluster resync scans at most
//!   [`RESYNC_FACTOR`] × the file size.
//! - Edit-list arithmetic is checked `i128`.
//! - Allocations sized from file data use `try_reserve` ([`io::vec_with_capacity`]).
//!
//! A budget breach is `Invalid` with a `budget:` reason, never a panic or an abort.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

pub mod io;
pub mod mkv;
pub mod mp4;
pub mod wav;

use crate::index::SampleIndex;

/// Most samples one track may have (2^26, R-MEDIA-SEC-2).
pub const MAX_SAMPLES: u64 = 1 << 26;
/// Deepest MP4 box nesting.
pub const MAX_BOX_DEPTH: u32 = 32;
/// Deepest EBML element nesting.
pub const MAX_EBML_DEPTH: u32 = 16;
/// Cluster resync may scan at most this many times the file size.
pub const RESYNC_FACTOR: u64 = 4;
/// Largest metadata box (`moov`, a `Tracks` or `Cues` element) read into memory at once.
pub const MAX_METADATA_BYTES: u64 = 256 << 20;
/// Most boxes one `moov` may hold (each is a tree node in memory).
pub const MAX_NODES: u64 = 1 << 17;
/// Most EBML elements one metadata element may hold.
pub const MAX_ELEMENTS: u64 = 1 << 20;

/// Index entries (samples, packets, frames) all tracks of a file may create together:
/// one per 8 bytes of file plus 64 Ki, so memory stays proportional to the input
/// (FZ1/FZ2 RSS ≤ 256 MB) while real files, at hundreds of bytes per sample, never
/// come near it.
pub fn entry_budget(file_len: u64) -> u64 {
    (file_len / 8).saturating_add(1 << 16).min(MAX_SAMPLES * 4)
}

/// Why a file could not be indexed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DemuxError {
    /// The file breaks its format or a parser budget. `offset` is the byte where the
    /// problem was found.
    #[error("invalid file at byte {offset}: {reason}")]
    Invalid { reason: String, offset: u64 },
    /// Valid, but not something this gate handles (named so the caller can say why).
    #[error("unsupported: {reason}")]
    Unsupported { reason: String },
    /// The OS refused a read.
    #[error("read failed at byte {offset}: {message}")]
    Io { offset: u64, message: String },
}

impl DemuxError {
    pub fn invalid(reason: impl Into<String>, offset: u64) -> Self {
        Self::Invalid {
            reason: reason.into(),
            offset,
        }
    }
    pub fn budget(what: impl std::fmt::Display, offset: u64) -> Self {
        Self::Invalid {
            reason: format!("budget: {what}"),
            offset,
        }
    }
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported {
            reason: reason.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, DemuxError>;

/// Which container a file is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Container {
    /// ISO BMFF: MP4, MOV, M4A.
    Mp4,
    /// Matroska or WebM.
    Matroska,
    /// RIFF/WAVE: one PCM track (decision 0051).
    Wav,
}

/// What a track carries.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Other,
}

/// The codec of a track and its out-of-band configuration, as stored in the container.
/// Nothing here is parsed beyond the container's own structure; `codec::` reads the
/// allowed header fields (SPEC §15.0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    /// `avc1`/`avc3` with its `avcC` record, or Matroska `V_MPEG4/ISO/AVC` CodecPrivate.
    H264 { avcc: Vec<u8> },
    /// `hvc1`/`hev1` with `hvcC` (indexed; decode is M2).
    Hevc { hvcc: Vec<u8> },
    /// `vp09` with `vpcC` (full box payload), or Matroska `V_VP9` (no record).
    Vp9 { vpcc: Option<Vec<u8>> },
    /// `av01`/`V_AV1` with `av1C` (indexed; decode is M4).
    Av1 { av1c: Vec<u8> },
    /// AAC: the AudioSpecificConfig from `esds`, or Matroska `A_AAC*` CodecPrivate.
    Aac { asc: Vec<u8> },
    /// Linear PCM (`ipcm`, `in24`, `in32`, `sowt`, `twos`, `lpcm`, `fl32`, `fl64`, `A_PCM/*`).
    Pcm {
        bits: u16,
        float: bool,
        little_endian: bool,
    },
    /// Opus (`Opus`/`A_OPUS`, M1).
    Opus { config: Vec<u8> },
    /// Anything else, by its four-character code or Matroska CodecID.
    Other { name: String },
}

/// One entry of an edit list (MP4 `elst`), in the units the file stores.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Movie timescale ticks.
    pub segment_duration: u64,
    /// Media timescale ticks; −1 = empty edit.
    pub media_time: i64,
    /// 16.16 fixed point.
    pub rate: i32,
}

/// A demuxed track: what it is plus its sample index.
#[derive(Clone, Debug)]
pub struct Track {
    /// MP4 `track_ID` or Matroska `TrackNumber`.
    pub id: u64,
    pub kind: TrackKind,
    pub codec: Codec,
    /// Ticks per second of `samples` timestamps.
    pub timescale: u32,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Colour tags the container carries (MP4 `colr`, Matroska `Colour`).
    pub colour: crate::codec::ColourTags,
    /// Samples in decode order. For uncompressed PCM, one entry is a packet of up to
    /// 1024 frames inside one chunk.
    pub samples: SampleIndex,
    /// Per-sample durations in `timescale` ticks (MP4 `stts`; Matroska block duration
    /// or the next block's timestamp gap, 0 when unknown).
    pub durations: Vec<u32>,
    /// Matroska only: sample presentation time unknown (a laced frame after the first
    /// with no DefaultDuration).
    pub pts_unknown: Vec<bool>,
    /// Matroska only: where each frame's (Simple)Block payload starts, which is the
    /// position the reference demuxer reports (`samples.offset` is the frame's own
    /// bytes). Empty for MP4.
    pub block_pos: Vec<u64>,
    /// MP4 edit list (empty when the track has none; Matroska never has one).
    pub edits: Vec<Edit>,
    /// The movie timescale `edits` durations are in.
    pub movie_timescale: u32,
}

/// One packet as the presentation sees it (edit lists applied), comparable to an
/// ffprobe packet dump (`tools/make_goldens.py`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub size: u32,
    pub pos: u64,
    pub key: bool,
    /// Decoded for reference only, never shown or heard (outside the edit).
    pub discard: bool,
}

impl Packet {
    /// The golden dump line: `<pts> <dts> <size> <pos> <K|_><D|_>`.
    pub fn golden_line(&self) -> String {
        let t = |v: Option<i64>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
        format!(
            "{} {} {} {} {}{}",
            t(self.pts),
            t(self.dts),
            self.size,
            self.pos,
            if self.key { 'K' } else { '_' },
            if self.discard { 'D' } else { '_' }
        )
    }
}

/// Which packet list to produce.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Sample tables as stored (MP4 `-ignore_editlist`).
    Raw,
    /// Edit lists applied: pre-roll flagged `discard`, timestamps on the presentation
    /// timeline.
    Edited,
}

impl Track {
    /// Where an audio track's sound starts on the presentation timeline, from its edit
    /// list: `(skip, lead)`, the decoded samples to drop (the encoder's priming, less any
    /// empty edit in front) and the samples of silence before the first one kept (an
    /// empty edit longer than the priming). A recording's files start with an empty edit
    /// for the time between the writer's start and the first sample (decision 0045).
    /// `None` for an edit list the readers do not follow: no media segment, more than
    /// one, a rate other than 1, or a media timescale other than the sample rate.
    pub fn audio_start(&self) -> Option<(u64, u64)> {
        if self.edits.is_empty() {
            return Some((0, 0));
        }
        let mut empty: i128 = 0;
        let mut media: Option<&Edit> = None;
        for e in &self.edits {
            match (e.media_time, media) {
                // empty edits after the media segment end it (ignored, as the demuxer does)
                (-1, None) => {
                    empty = empty.checked_add(
                        mp4::edit::rescale(
                            e.segment_duration,
                            self.movie_timescale,
                            self.timescale,
                        )
                        .ok()?,
                    )?;
                }
                (-1, Some(_)) => {}
                (_, None) => media = Some(e),
                (_, Some(_)) => return None,
            }
        }
        let e = media?;
        if e.media_time < 0 || e.rate != 0x0001_0000 || self.timescale != self.sample_rate {
            return None;
        }
        let m = i128::from(e.media_time);
        Some((
            u64::try_from((m - empty).max(0)).ok()?,
            u64::try_from((empty - m).max(0)).ok()?,
        ))
    }

    /// Packets in decode order, raw or with the edit list applied.
    pub fn packets(&self, view: View) -> Result<Vec<Packet>> {
        let s = &self.samples;
        let raw = |i: usize| -> Option<Packet> {
            let dts = *s.dts.get(i)?;
            let off = i64::from(*s.cts_off.get(i)?);
            let unknown = self.pts_unknown.get(i).copied().unwrap_or(false);
            Some(Packet {
                pts: if unknown { None } else { dts.checked_add(off) },
                dts: if unknown { None } else { Some(dts) },
                size: *s.size.get(i)?,
                pos: match self.block_pos.get(i) {
                    Some(&p) => p,
                    None => *s.offset.get(i)?,
                },
                key: *s.sync.get(i)?,
                discard: false,
            })
        };
        if view == View::Raw || self.edits.is_empty() {
            return (0..s.len())
                .map(|i| raw(i).ok_or_else(|| DemuxError::invalid("ragged index", 0)))
                .collect();
        }
        mp4::edit::apply(self, &raw)
    }
}

/// A demuxed file.
#[derive(Clone, Debug)]
pub struct Demuxed {
    pub container: Container,
    pub tracks: Vec<Track>,
    /// Matroska cue points (empty for MP4).
    pub cues: Vec<mkv::CuePoint>,
}

/// Index a file, choosing the container from its first bytes.
pub fn index<S: io::ByteSource>(src: &mut S) -> Result<Demuxed> {
    let mut head = [0u8; 12];
    let n = usize::try_from(src.len().min(12)).unwrap_or(0);
    let head = head.get_mut(..n).unwrap_or_default();
    src.read_at(0, head)?;
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        mkv::index(src)
    } else if wav::is_wav(head) {
        wav::index(src)
    } else {
        mp4::index(src)
    }
}

/// Build a [`SampleIndex`] from decode-order columns: presentation order, reorder depth
/// and constant frame rate are derived here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_index(
    timescale: u32,
    kind: TrackKind,
    dts: Vec<i64>,
    cts_off: Vec<i32>,
    offset: Vec<u64>,
    size: Vec<u32>,
    sync: Vec<bool>,
    durations: &[u32],
) -> Result<SampleIndex> {
    let n = dts.len();
    if [cts_off.len(), offset.len(), size.len(), sync.len()]
        .iter()
        .any(|&l| l != n)
    {
        return Err(DemuxError::invalid(
            "sample table columns differ in length",
            0,
        ));
    }
    let n32 = u32::try_from(n).map_err(|_| DemuxError::budget("sample count", 0))?;
    let mut pts_order: Vec<u32> = io::vec_with_capacity(n, 0)?;
    pts_order.extend(0..n32);
    let pts = |i: u32| -> i64 {
        let i = i as usize;
        dts.get(i)
            .copied()
            .unwrap_or(0)
            .saturating_add(i64::from(cts_off.get(i).copied().unwrap_or(0)))
    };
    pts_order.sort_by_key(|&i| (pts(i), i));
    let mut depth = 0usize;
    for (p, &d) in pts_order.iter().enumerate() {
        depth = depth.max((d as usize).saturating_sub(p));
    }
    if depth > 64 {
        return Err(DemuxError::unsupported(format!(
            "reorder depth {depth} (> 64)"
        )));
    }
    let cfr = if kind == TrackKind::Video && n >= 2 {
        let body = durations.get(..n - 1).unwrap_or_default();
        match body.first() {
            Some(&d) if d > 0 && body.iter().all(|&x| x == d) => {
                let g = gcd(u64::from(timescale), u64::from(d));
                let (num, den) = (u64::from(timescale) / g.max(1), u64::from(d) / g.max(1));
                u32::try_from(den)
                    .ok()
                    .map(|den| gmclip_model::Rational::new(num as i64, den))
            }
            _ => None,
        }
    } else {
        None
    };
    Ok(SampleIndex {
        timescale,
        dts,
        cts_off,
        offset,
        size,
        sync,
        pts_order,
        reorder_depth: u8::try_from(depth).unwrap_or(u8::MAX),
        cfr,
        complete: true,
    })
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod audio_start_tests {
    use super::*;

    fn track(edits: &[(u64, i64)], timescale: u32) -> Track {
        Track {
            id: 2,
            kind: TrackKind::Audio,
            codec: Codec::Aac {
                asc: vec![0x11, 0x88],
            },
            timescale,
            width: 0,
            height: 0,
            sample_rate: 48_000,
            channels: 1,
            colour: crate::codec::ColourTags::default(),
            samples: SampleIndex::default(),
            durations: Vec::new(),
            pts_unknown: Vec::new(),
            block_pos: Vec::new(),
            edits: edits
                .iter()
                .map(|&(segment_duration, media_time)| Edit {
                    segment_duration,
                    media_time,
                    rate: 0x0001_0000,
                })
                .collect(),
            movie_timescale: 48_000,
        }
    }

    /// The priming alone; a recording's empty edit in front of it (AJ's face.mov: 709
    /// samples, then the 2112 priming) drops less; one longer than the priming is silence
    /// first; trailing empty edits are ignored; what the readers cannot follow is `None`.
    #[test]
    fn an_empty_edit_in_front_shifts_the_start() {
        assert_eq!(track(&[], 48_000).audio_start(), Some((0, 0)));
        assert_eq!(
            track(&[(96_000, 2112)], 48_000).audio_start(),
            Some((2112, 0))
        );
        assert_eq!(
            track(&[(709, -1), (180_522, 2112)], 48_000).audio_start(),
            Some((1403, 0))
        );
        assert_eq!(
            track(&[(3000, -1), (180_522, 2112)], 48_000).audio_start(),
            Some((0, 888))
        );
        assert_eq!(
            track(&[(100, -1), (200, -1), (180_522, 2112), (500, -1)], 48_000).audio_start(),
            Some((1812, 0))
        );
        assert_eq!(track(&[(709, -1)], 48_000).audio_start(), None);
        assert_eq!(track(&[(10, 0), (10, 20)], 48_000).audio_start(), None);
        assert_eq!(track(&[(96_000, 2112)], 44_100).audio_start(), None);
    }
}
