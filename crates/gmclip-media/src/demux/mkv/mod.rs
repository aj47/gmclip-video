//! Matroska/WebM index (B-02, SPEC §6.1): our own EBML reader. SeekHead, Info, Tracks
//! and Cues are read into memory and parsed as trees; every Cluster is walked for the
//! presentation index (SimpleBlock and BlockGroup, all four lacing modes) with small
//! reads at element and block headers, so frame data is skipped, not read. A file without Cues is `Unsupported` at M0 (cue-less bisection is M2).
//!
//! A damaged element inside the segment triggers a resync: the damaged cluster's frames
//! are dropped and a forward scan from the damage looks for the next top-level ID
//! (Cluster, Cues, Tags, …) that starts a plausible element. Resync work is capped at [`super::RESYNC_FACTOR`] × the file size and at
//! [`MAX_RESYNCS`] resyncs; past either the file is `Invalid` (R-MEDIA-SEC-2).

pub mod ebml;

use super::io::{ByteSource, Cursor, read_vec, vec_with_capacity};
use super::{Codec, Container, DemuxError, Demuxed, RESYNC_FACTOR, Result, Track, TrackKind};
use ebml::{Element, UNKNOWN};

/// Most resyncs one file may need before it is called hostile.
pub const MAX_RESYNCS: u32 = 64;
/// Read size for a resync scan.
const WINDOW: usize = 64 * 1024;
/// Read size when walking clusters: element and block headers are a few bytes, so a
/// small read keeps the walk from pulling frame data in (see the B-02 lab test).
const WALK_READ: usize = 1024;
/// Largest EBML header element.
const MAX_EBML_HEADER: u64 = 4096;

mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX: u32 = 0x55B1;
    pub const CHROMA_SITING_HORZ: u32 = 0x55B7;
    pub const CHROMA_SITING_VERT: u32 = 0x55B8;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const BIT_DEPTH: u32 = 0x6264;
    pub const CONTENT_ENCODINGS: u32 = 0x6D80;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const REFERENCE_BLOCK: u32 = 0xFB;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
    pub const CUE_RELATIVE_POSITION: u32 = 0xF0;
    pub const TAGS: u32 = 0x1254_C367;
    pub const CHAPTERS: u32 = 0x1043_A770;
    pub const ATTACHMENTS: u32 = 0x1941_A469;
    /// Elements that can only appear at the top of a Segment: one of these inside an
    /// unknown-size Cluster ends it.
    pub const LEVEL1: &[u32] = &[
        SEEK_HEAD,
        INFO,
        TRACKS,
        CLUSTER,
        CUES,
        TAGS,
        CHAPTERS,
        ATTACHMENTS,
    ];
    /// Master elements parsed as trees.
    pub const MASTERS: &[u32] = &[
        TRACK_ENTRY,
        VIDEO,
        AUDIO,
        CONTENT_ENCODINGS,
        0x6240,
        0x5034,
        0x5035,
        0x55B0,
        0x55D0,
        0x7670,
        CUE_POINT,
        CUE_TRACK_POSITIONS,
        0xDB,
        0x4DBB,
        EBML,
        SEGMENT,
        CLUSTER,
        BLOCK_GROUP,
        TRACKS,
        CUES,
        INFO,
        SEEK_HEAD,
        TAGS,
        0x7373,
        0x67C8,
    ];
}

/// One Cues entry: a cluster where decoding of `track` can start at `time`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CuePoint {
    /// Segment ticks (TimestampScale units, the track's `timescale`).
    pub time: u64,
    pub track: u64,
    /// Absolute file offset of the Cluster.
    pub cluster: u64,
    /// Offset of the block inside the cluster's data, when given.
    pub relative: Option<u64>,
}

/// The last cue for `track` at or before `t` (cues sorted by track then time, as
/// [`index`] returns them). Binary search: microseconds on a 2-hour file (B-02 asks
/// ≤ 5 ms).
pub fn cue_for(cues: &[CuePoint], track: u64, t: u64) -> Option<&CuePoint> {
    let lo = cues.partition_point(|c| c.track < track);
    let hi = cues.partition_point(|c| c.track <= track);
    let slice = cues.get(lo..hi)?;
    let k = slice.partition_point(|c| c.time <= t);
    slice.get(k.checked_sub(1)?)
}

/// A read window over the source.
struct Window<'s, S: ?Sized> {
    src: &'s mut S,
    start: u64,
    buf: Vec<u8>,
}

impl<S: ByteSource + ?Sized> Window<'_, S> {
    fn len(&self) -> u64 {
        self.src.len()
    }
    /// Up to `n` bytes at `pos` (fewer only at the end of the file).
    fn get(&mut self, pos: u64, n: usize) -> Result<&[u8]> {
        let len = self.src.len();
        if pos >= len {
            return Err(DemuxError::invalid("read past end of file", pos));
        }
        let avail = usize::try_from((len - pos).min(n as u64)).unwrap_or(n);
        let have = pos >= self.start && pos + avail as u64 <= self.start + self.buf.len() as u64;
        if !have {
            let want =
                usize::try_from((len - pos).min(WALK_READ.max(n) as u64)).unwrap_or(WALK_READ);
            self.buf.clear();
            self.buf
                .try_reserve(want)
                .map_err(|_| DemuxError::budget("read window", pos))?;
            self.buf.resize(want, 0);
            self.src.read_at(pos, &mut self.buf)?;
            self.start = pos;
        }
        let from = usize::try_from(pos - self.start).unwrap_or(0);
        self.buf
            .get(from..from + avail)
            .ok_or_else(|| DemuxError::invalid("window", pos))
    }
    /// Element header at `pos`: `(id, size, header_len)`.
    fn header(&mut self, pos: u64) -> Result<(u32, u64, u64)> {
        let b = self.get(pos, 12)?;
        let mut c = Cursor::new(b, pos);
        let id = ebml::read_id(&mut c)?;
        let size = ebml::read_size(&mut c)?;
        Ok((id, size, c.offset() - pos))
    }
}

struct TrackState {
    number: u64,
    track: Track,
    default_duration: Option<u64>,
    /// CodecDelay in ns: subtracted from every block timestamp (Matroska §5.1.4.1.13).
    codec_delay: u64,
    block_durations: Vec<Option<u64>>,
}

/// Index a Matroska/WebM file.
pub fn index<S: ByteSource + ?Sized>(src: &mut S) -> Result<Demuxed> {
    let len = src.len();
    let mut w = Window {
        src,
        start: 0,
        buf: Vec::new(),
    };
    // EBML header
    let (hid, hsize, hlen) = w.header(0)?;
    if hid != id::EBML || hsize == UNKNOWN || hsize > MAX_EBML_HEADER {
        return Err(DemuxError::invalid("not an EBML file", 0));
    }
    let head = read_vec(w.src, hlen, hsize)?;
    let mut budget = 4096;
    let els = ebml::parse(&head, hlen, &[], 1, &mut budget)?;
    let doc = els
        .iter()
        .find(|e| e.id == id::DOC_TYPE)
        .map(Element::string)
        .unwrap_or_else(|| "matroska".into());
    if doc != "matroska" && doc != "webm" {
        return Err(DemuxError::unsupported(format!("EBML DocType '{doc}'")));
    }
    if let Some(v) = els.iter().find(|e| e.id == id::DOC_TYPE_READ_VERSION)
        && v.uint()? > 4
    {
        return Err(DemuxError::unsupported("Matroska DocTypeReadVersion > 4"));
    }
    // Segment (skip anything between the header and it, e.g. Void)
    let mut pos = hlen + hsize;
    let (seg_data, seg_end) = loop {
        let (eid, size, hl) = w.header(pos)?;
        if eid == id::SEGMENT {
            let data = pos + hl;
            let end = if size == UNKNOWN {
                len
            } else {
                data.saturating_add(size).min(len)
            };
            break (data, end);
        }
        if size == UNKNOWN {
            return Err(DemuxError::invalid(
                "unknown-size element before Segment",
                pos,
            ));
        }
        pos = pos
            .checked_add(hl + size)
            .filter(|&p| p < len)
            .ok_or_else(|| DemuxError::invalid("no Segment", pos))?;
    };

    let mut scale: u64 = 1_000_000;
    let mut tracks: Vec<TrackState> = Vec::new();
    let mut tracks_seen = false;
    let mut cues: Vec<CuePoint> = Vec::new();
    let mut cues_seen = false;
    let mut frame_budget = super::entry_budget(len);
    let mut resyncs = 0u32;
    let mut scanned = 0u64;
    let mut pos = seg_data;
    while pos < seg_end {
        if seg_end - pos < 2 {
            break;
        }
        let parsed = w.header(pos);
        let (eid, size, hl) = match parsed {
            Ok(h) => h,
            Err(e) => {
                pos = resync(&mut w, pos, seg_end, &mut resyncs, &mut scanned, e)?;
                continue;
            }
        };
        let data = pos + hl;
        let end = if size == UNKNOWN {
            if eid != id::CLUSTER {
                return Err(DemuxError::invalid(
                    format!("unknown-size element {eid:#x} in Segment"),
                    pos,
                ));
            }
            None
        } else {
            Some(
                data.checked_add(size)
                    .ok_or_else(|| DemuxError::invalid("element size overflows", pos))?,
            )
        };
        match eid {
            id::INFO | id::TRACKS | id::CUES | id::SEEK_HEAD => {
                let end = end.filter(|&e| e <= seg_end).ok_or_else(|| {
                    DemuxError::invalid(format!("element {eid:#x} runs past the Segment"), pos)
                })?;
                let body = read_vec(w.src, data, end - data)?;
                let mut budget = ((body.len() as u64) / 2 + 1).min(super::MAX_ELEMENTS);
                let els = ebml::parse(&body, data, id::MASTERS, 2, &mut budget)?;
                match eid {
                    id::INFO => {
                        if let Some(s) = els.iter().find(|e| e.id == id::TIMESTAMP_SCALE) {
                            scale = s.uint()?;
                        }
                    }
                    id::SEEK_HEAD => {
                        // A SeekHead that indexes the file but lists no Cues: cue-less.
                        let lists_cues = els.iter().filter(|e| e.id == id::SEEK).any(|e| {
                            e.child(id::SEEK_ID)
                                .is_some_and(|i| i.data == id::CUES.to_be_bytes())
                        });
                        let lists_any = els.iter().any(|e| e.id == id::SEEK);
                        let chained = els.iter().filter(|e| e.id == id::SEEK).any(|e| {
                            e.child(id::SEEK_ID)
                                .is_some_and(|i| i.data == id::SEEK_HEAD.to_be_bytes())
                        });
                        if lists_any && !lists_cues && !chained && !cues_seen {
                            return Err(DemuxError::unsupported("no cues (M2)"));
                        }
                    }
                    id::TRACKS => {
                        if tracks_seen {
                            return Err(DemuxError::invalid("two Tracks elements", pos));
                        }
                        tracks_seen = true;
                        for e in els.iter().filter(|e| e.id == id::TRACK_ENTRY) {
                            tracks.push(track_entry(e, scale)?);
                        }
                    }
                    _ => {
                        cues_seen = true;
                        parse_cues(&els, seg_data, &mut cues)?;
                    }
                }
                pos = end;
            }
            id::CLUSTER => {
                if !tracks_seen {
                    return Err(DemuxError::invalid("Cluster before Tracks", pos));
                }
                let cend = end.map_or(seg_end, |e| e.min(seg_end));
                let marks: Vec<usize> = tracks.iter().map(TrackState::len).collect();
                match walk_cluster(
                    &mut w,
                    data,
                    cend,
                    end.is_none(),
                    &mut tracks,
                    &mut frame_budget,
                ) {
                    Ok(next) => pos = next,
                    Err(e) if is_damage(&e) => {
                        // Drop the damaged cluster's frames and scan on from the damage.
                        for (t, &m) in tracks.iter_mut().zip(&marks) {
                            t.truncate(m);
                        }
                        let at = match &e {
                            DemuxError::Invalid { offset, .. } => (*offset).max(pos),
                            _ => pos,
                        };
                        pos = resync(&mut w, at, seg_end, &mut resyncs, &mut scanned, e)?
                    }
                    Err(e) => return Err(e),
                }
            }
            _ => {
                // `end` is None only for a Cluster (checked above).
                pos = end.ok_or_else(|| DemuxError::invalid("unknown-size element", pos))?;
            }
        }
    }
    // A cue must point at a cluster inside the segment; others are dropped.
    cues.retain(|c| c.cluster >= seg_data && c.cluster < seg_end);
    if !cues_seen || cues.is_empty() {
        return Err(DemuxError::unsupported("no cues (M2)"));
    }
    if scale == 0 || 1_000_000_000 % scale != 0 {
        return Err(DemuxError::unsupported(format!(
            "TimestampScale {scale} does not divide one second"
        )));
    }
    cues.sort_by_key(|c| (c.track, c.time, c.cluster));
    let mut out = Vec::new();
    for t in tracks {
        out.push(finish_track(t, scale)?);
    }
    Ok(Demuxed {
        container: Container::Matroska,
        tracks: out,
        cues,
    })
}

/// A structural error a resync can recover from (not a budget breach or an I/O error).
fn is_damage(e: &DemuxError) -> bool {
    matches!(e, DemuxError::Invalid { reason, .. } if !reason.starts_with("budget"))
}

/// Scan forward from `from + 1` for the next top-level element ID (a Cluster, or the
/// Cues/Tags/... that follow the last one).
fn resync<S: ByteSource + ?Sized>(
    w: &mut Window<'_, S>,
    from: u64,
    end: u64,
    resyncs: &mut u32,
    scanned: &mut u64,
    cause: DemuxError,
) -> Result<u64> {
    *resyncs += 1;
    if *resyncs > MAX_RESYNCS {
        return Err(DemuxError::budget(
            format!("more than {MAX_RESYNCS} resyncs (last: {cause})"),
            from,
        ));
    }
    let limit = w.len().saturating_mul(RESYNC_FACTOR);
    let mut p = from + 1;
    while p + 4 <= end {
        // Load each stretch once and test every candidate in it; `scanned` counts the
        // bytes loaded, so false matches cannot multiply the work.
        let chunk = w.get(p, WINDOW)?;
        let n = chunk.len();
        let hits: Vec<usize> = chunk
            .windows(4)
            .enumerate()
            .filter(|(_, x)| {
                <[u8; 4]>::try_from(*x).is_ok_and(|b| id::LEVEL1.contains(&u32::from_be_bytes(b)))
            })
            .map(|(k, _)| k)
            .collect();
        *scanned += n as u64;
        if *scanned > limit {
            return Err(DemuxError::budget(
                "cluster resync scanned more than 4x the file",
                p,
            ));
        }
        for k in hits {
            let at = p + k as u64;
            if at + 4 <= end && plausible(w, at, end) {
                return Ok(at);
            }
        }
        if n < 4 {
            break;
        }
        p += (n - 3) as u64;
    }
    Ok(end)
}

/// Does a top-level ID found by a resync scan start a real element? Its size must fit
/// the segment, and a Cluster must open with its Timestamp.
fn plausible<S: ByteSource + ?Sized>(w: &mut Window<'_, S>, at: u64, end: u64) -> bool {
    let Ok((eid, size, hl)) = w.header(at) else {
        return false;
    };
    if size != UNKNOWN
        && at
            .checked_add(hl)
            .and_then(|d| d.checked_add(size))
            .is_none_or(|e| e > end)
    {
        return false;
    }
    if size == UNKNOWN && eid != id::CLUSTER {
        return false;
    }
    if eid == id::CLUSTER {
        return matches!(w.header(at + hl), Ok((id::TIMESTAMP, s, _)) if s <= 8);
    }
    true
}

fn track_entry(e: &Element<'_>, _scale: u64) -> Result<TrackState> {
    let number = e.uint_of(id::TRACK_NUMBER, 0)?;
    if number == 0 {
        return Err(DemuxError::invalid(
            "TrackEntry without TrackNumber",
            e.offset,
        ));
    }
    if e.child(id::CONTENT_ENCODINGS).is_some() {
        return Err(DemuxError::unsupported(
            "Matroska ContentEncodings (compressed or encrypted track)",
        ));
    }
    let kind = match e.uint_of(id::TRACK_TYPE, 0)? {
        1 => TrackKind::Video,
        2 => TrackKind::Audio,
        _ => TrackKind::Other,
    };
    let codec_id = e
        .child(id::CODEC_ID)
        .map(Element::string)
        .unwrap_or_default();
    let private = e
        .child(id::CODEC_PRIVATE)
        .map(|p| p.data.to_vec())
        .unwrap_or_default();
    let audio = e.child(id::AUDIO);
    let bits = audio.map_or(Ok(0), |a| a.uint_of(id::BIT_DEPTH, 0))?;
    let bits16 = u16::try_from(bits).unwrap_or(0);
    let codec = match codec_id.as_str() {
        "V_VP9" => Codec::Vp9 { vpcc: None },
        "V_MPEG4/ISO/AVC" => Codec::H264 { avcc: private },
        "V_MPEGH/ISO/HEVC" => Codec::Hevc { hvcc: private },
        "V_AV1" => Codec::Av1 { av1c: private },
        "A_OPUS" => Codec::Opus { config: private },
        "A_PCM/INT/LIT" => Codec::Pcm {
            bits: bits16,
            float: false,
            little_endian: true,
        },
        "A_PCM/INT/BIG" => Codec::Pcm {
            bits: bits16,
            float: false,
            little_endian: false,
        },
        "A_PCM/FLOAT/IEEE" => Codec::Pcm {
            bits: bits16,
            float: true,
            little_endian: true,
        },
        a if a == "A_AAC" || a.starts_with("A_AAC/") => Codec::Aac { asc: private },
        other => Codec::Other {
            name: other.to_owned(),
        },
    };
    let colour = match e.child(id::VIDEO).and_then(|v| v.child(id::COLOUR)) {
        Some(c) => {
            let code = |i: u32| -> Result<Option<u8>> {
                Ok(c.child(i)
                    .map(Element::uint)
                    .transpose()?
                    .and_then(|v| u8::try_from(v).ok()))
            };
            let siting = (code(id::CHROMA_SITING_HORZ)?, code(id::CHROMA_SITING_VERT)?);
            crate::codec::ColourTags {
                primaries: code(id::PRIMARIES)?,
                transfer: code(id::TRANSFER)?,
                matrix: code(id::MATRIX)?,
                full_range: match code(id::RANGE)? {
                    Some(1) => Some(false),
                    Some(2) => Some(true),
                    _ => None,
                },
                chroma_left: match siting {
                    (Some(1), Some(2)) => Some(true),
                    (Some(h), Some(v)) if h != 0 && v != 0 => Some(false),
                    _ => None,
                },
            }
        }
        None => Default::default(),
    };
    let (width, height) = match e.child(id::VIDEO) {
        Some(v) => (
            u32::try_from(v.uint_of(id::PIXEL_WIDTH, 0)?).unwrap_or(0),
            u32::try_from(v.uint_of(id::PIXEL_HEIGHT, 0)?).unwrap_or(0),
        ),
        None => (0, 0),
    };
    let (sample_rate, channels) = match audio {
        Some(a) => {
            let f = a
                .child(id::SAMPLING_FREQUENCY)
                .map_or(Ok(8000.0), Element::float)?;
            let rate = if f.is_finite() && f > 0.0 && f < 1.0e7 {
                f as u32
            } else {
                0
            };
            (
                rate,
                u16::try_from(a.uint_of(id::CHANNELS, 1)?).unwrap_or(0),
            )
        }
        None => (0, 0),
    };
    let default_duration = e
        .child(id::DEFAULT_DURATION)
        .map(Element::uint)
        .transpose()?;
    let track = Track {
        id: number,
        kind,
        codec,
        timescale: 0,
        width,
        height,
        sample_rate,
        channels,
        colour,
        samples: Default::default(),
        durations: Vec::new(),
        pts_unknown: Vec::new(),
        block_pos: Vec::new(),
        edits: Vec::new(),
        movie_timescale: 0,
    };
    let codec_delay = e.uint_of(id::CODEC_DELAY, 0)?;
    Ok(TrackState {
        number,
        track,
        default_duration,
        codec_delay,
        block_durations: Vec::new(),
    })
}

fn parse_cues(els: &[Element<'_>], seg_data: u64, out: &mut Vec<CuePoint>) -> Result<()> {
    for cp in els.iter().filter(|e| e.id == id::CUE_POINT) {
        let time = cp.uint_of(id::CUE_TIME, 0)?;
        for tp in cp.children_of(id::CUE_TRACK_POSITIONS) {
            let rel = tp.uint_of(id::CUE_CLUSTER_POSITION, 0)?;
            let cluster = seg_data
                .checked_add(rel)
                .ok_or_else(|| DemuxError::invalid("CueClusterPosition overflows", tp.offset))?;
            out.try_reserve(1)
                .map_err(|_| DemuxError::budget("cues", tp.offset))?;
            out.push(CuePoint {
                time,
                track: tp.uint_of(id::CUE_TRACK, 0)?,
                cluster,
                relative: tp
                    .child(id::CUE_RELATIVE_POSITION)
                    .map(Element::uint)
                    .transpose()?,
            });
        }
    }
    Ok(())
}

/// One frame found in a block.
struct Frame {
    pos: u64,
    size: u32,
}

/// An unsigned element value of at most 8 bytes at `body`.
fn small_uint<S: ByteSource + ?Sized>(w: &mut Window<'_, S>, body: u64, size: u64) -> Result<u64> {
    let n = usize::try_from(size)
        .ok()
        .filter(|&n| n <= 8)
        .ok_or_else(|| DemuxError::invalid(format!("{size}-byte integer element"), body))?;
    if n == 0 {
        return Ok(0);
    }
    let b = w.get(body, n)?;
    let b = b
        .get(..n)
        .ok_or_else(|| DemuxError::invalid("integer element runs past the file", body))?;
    ebml::uint(b, body)
}

/// Walk one cluster's children from `data` to `end`; returns where the next Segment
/// child starts.
fn walk_cluster<S: ByteSource + ?Sized>(
    w: &mut Window<'_, S>,
    data: u64,
    end: u64,
    unknown_size: bool,
    tracks: &mut [TrackState],
    frame_budget: &mut u64,
) -> Result<u64> {
    let mut cluster_ts: i64 = 0;
    let mut p = data;
    while p < end {
        if end - p < 2 {
            return Ok(end);
        }
        let (eid, size, hl) = w.header(p)?;
        if unknown_size && id::LEVEL1.contains(&eid) {
            return Ok(p);
        }
        if size == UNKNOWN {
            return Err(DemuxError::invalid(
                format!("unknown-size element {eid:#x} in a Cluster"),
                p,
            ));
        }
        let body = p + hl;
        let next = body
            .checked_add(size)
            .filter(|&n| n <= end)
            .ok_or_else(|| {
                DemuxError::invalid(
                    format!("element {eid:#x} ({size} bytes) runs past its Cluster"),
                    p,
                )
            })?;
        match eid {
            id::TIMESTAMP => {
                cluster_ts = i64::try_from(small_uint(w, body, size)?)
                    .map_err(|_| DemuxError::invalid("Cluster Timestamp", body))?;
            }
            id::SIMPLE_BLOCK => block(w, body, size, cluster_ts, None, tracks, frame_budget)?,
            id::BLOCK_GROUP => {
                let mut q = body;
                let mut blk = None;
                let mut referenced = false;
                let mut duration = None;
                while q < next {
                    let (gid, gsize, ghl) = w.header(q)?;
                    let gbody = q + ghl;
                    let gnext = gbody
                        .checked_add(gsize)
                        .filter(|&n| n <= next && gsize != UNKNOWN)
                        .ok_or_else(|| {
                            DemuxError::invalid("BlockGroup child runs past its group", q)
                        })?;
                    match gid {
                        id::BLOCK => blk = Some((gbody, gsize)),
                        id::REFERENCE_BLOCK => referenced = true,
                        id::BLOCK_DURATION => duration = Some(small_uint(w, gbody, gsize)?),
                        _ => {}
                    }
                    q = gnext;
                }
                if let Some((b, s)) = blk {
                    block(
                        w,
                        b,
                        s,
                        cluster_ts,
                        Some((!referenced, duration)),
                        tracks,
                        frame_budget,
                    )?;
                }
            }
            _ => {}
        }
        p = next;
    }
    Ok(end)
}

/// Most frames one lace may hold.
const MAX_LACE: usize = 256;

/// Parse a SimpleBlock or Block at `data` (`size` bytes). `group` is `(key, duration)`
/// for a Block inside a BlockGroup. Reads the block header (and lace header) only.
fn block<S: ByteSource + ?Sized>(
    w: &mut Window<'_, S>,
    data: u64,
    size: u64,
    cluster_ts: i64,
    group: Option<(bool, Option<u64>)>,
    tracks: &mut [TrackState],
    frame_budget: &mut u64,
) -> Result<()> {
    let body_end = data
        .checked_add(size)
        .ok_or_else(|| DemuxError::invalid("block size", data))?;
    let head = w.get(data, usize::try_from(size.min(12)).unwrap_or(12))?;
    let mut c = Cursor::new(head, data);
    let number = ebml::read_size(&mut c)?;
    let rel = i64::from(i16::from_be_bytes([c.u8()?, c.u8()?]));
    let flags = c.u8()?;
    let Some(t) = tracks.iter_mut().find(|t| t.number == number) else {
        return Ok(()); // a block for a track not in Tracks: ignored, as players do
    };
    let key = match group {
        Some((k, _)) => k,
        None => flags & 0x80 != 0,
    };
    let lacing = (flags >> 1) & 3;
    let mut frames: Vec<Frame> = Vec::new();
    if lacing == 0 {
        let pos = c.offset();
        let sz = u32::try_from(body_end.saturating_sub(pos))
            .map_err(|_| DemuxError::invalid("frame size", pos))?;
        frames.push(Frame { pos, size: sz });
    } else {
        // The lace header: at most 8 bytes per size, or size/255 + 1 per Xiph size.
        let after = c.offset();
        let want = size.min(64 * 1024);
        let head = w.get(data, usize::try_from(want).unwrap_or(64 * 1024))?;
        let mut c = Cursor::new(head, data);
        c.skip(usize::try_from(after - data).unwrap_or(0))?;
        let count = usize::from(c.u8()?) + 1;
        let mut sizes: Vec<u64> = Vec::with_capacity(count.min(MAX_LACE));
        if count > 1 {
            match lacing {
                1 => {
                    for _ in 0..count - 1 {
                        let mut s = 0u64;
                        loop {
                            let b = c.u8()?;
                            s += u64::from(b);
                            if b != 255 {
                                break;
                            }
                        }
                        sizes.push(s);
                    }
                }
                3 => {
                    let at = c.offset();
                    let mut prev = ebml::read_size(&mut c)?;
                    if prev == UNKNOWN {
                        return Err(DemuxError::invalid("lace size of all ones", at));
                    }
                    sizes.push(prev);
                    for _ in 1..count - 1 {
                        let at = c.offset();
                        let first = c.u8()?;
                        let n = ebml::vint_len(first)
                            .ok_or_else(|| DemuxError::invalid("bad lace size", at))?;
                        let mut raw = u64::from(first) & (0xFFu64 >> n);
                        for _ in 1..n {
                            raw = (raw << 8) | u64::from(c.u8()?);
                        }
                        let bias = (1i64 << (7 * n as u32 - 1)) - 1;
                        let delta = (raw as i64).checked_sub(bias);
                        prev = delta
                            .and_then(|d| i64::try_from(prev).ok()?.checked_add(d))
                            .and_then(|v| u64::try_from(v).ok())
                            .ok_or_else(|| DemuxError::invalid("lace size out of range", at))?;
                        sizes.push(prev);
                    }
                }
                _ => {
                    let rest = body_end.saturating_sub(c.offset());
                    if !rest.is_multiple_of(count as u64) {
                        return Err(DemuxError::invalid(
                            "fixed lacing does not divide the block",
                            data,
                        ));
                    }
                    sizes.extend(std::iter::repeat_n(rest / count as u64, count - 1));
                }
            }
        }
        let mut pos = c.offset();
        let used = sizes
            .iter()
            .try_fold(0u64, |a, &s| a.checked_add(s))
            .ok_or_else(|| DemuxError::invalid("lace sizes overflow", data))?;
        let last = body_end
            .checked_sub(pos)
            .and_then(|r| r.checked_sub(used))
            .ok_or_else(|| DemuxError::invalid("lace sizes exceed the block", data))?;
        sizes.push(last);
        for s in sizes {
            if s == 0 {
                return Err(DemuxError::invalid("empty frame in a lace", pos));
            }
            let sz = u32::try_from(s).map_err(|_| DemuxError::invalid("frame size", pos))?;
            frames.push(Frame { pos, size: sz });
            pos += s;
        }
    }
    let ts = cluster_ts
        .checked_add(rel)
        .ok_or_else(|| DemuxError::invalid("block timestamp overflows", data))?;
    // BlockDuration covers the whole lace.
    let per_frame = group
        .and_then(|g| g.1)
        .map(|d| d / frames.len().max(1) as u64);
    let s = &mut t.track.samples;
    for (k, f) in frames.into_iter().enumerate() {
        *frame_budget = frame_budget
            .checked_sub(1)
            .ok_or_else(|| DemuxError::budget("more frames than the file size allows", data))?;
        if s.dts.len() as u64 >= super::MAX_SAMPLES {
            return Err(DemuxError::budget(
                "more than 2^26 frames in one track",
                data,
            ));
        }
        s.dts.push(ts);
        s.cts_off.push(0);
        s.offset.push(f.pos);
        s.size.push(f.size);
        s.sync.push(key);
        t.track.pts_unknown.push(k > 0);
        t.track.block_pos.push(data);
        t.block_durations.push(per_frame);
    }
    Ok(())
}

impl TrackState {
    fn len(&self) -> usize {
        self.track.samples.dts.len()
    }
    /// Drop every frame from `n` on (a damaged cluster's partial frames).
    fn truncate(&mut self, n: usize) {
        let s = &mut self.track.samples;
        s.dts.truncate(n);
        s.cts_off.truncate(n);
        s.offset.truncate(n);
        s.size.truncate(n);
        s.sync.truncate(n);
        self.track.pts_unknown.truncate(n);
        self.track.block_pos.truncate(n);
        self.block_durations.truncate(n);
    }
}

fn finish_track(mut t: TrackState, scale: u64) -> Result<Track> {
    let timescale = u32::try_from(1_000_000_000 / scale)
        .map_err(|_| DemuxError::unsupported("TimestampScale"))?;
    let s = std::mem::take(&mut t.track.samples);
    let n = s.dts.len();
    let to_ticks = |ns: u64| {
        ns.checked_add(scale / 2)
            .map(|v| v / scale)
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(|| DemuxError::invalid("duration in ns overflows", 0))
    };
    let default_ticks = t.default_duration.map(to_ticks).transpose()?;
    // Block timestamps are presentation times.
    let mut pts = s.dts;
    // CodecDelay, rounded to the nearest tick, moves the whole track earlier.
    let delay = to_ticks(t.codec_delay)?;
    if delay != 0 {
        for d in &mut pts {
            *d = d
                .checked_sub(delay)
                .ok_or_else(|| DemuxError::invalid("timestamp underflow", 0))?;
        }
    }
    // Laced frames after the first get timestamps from DefaultDuration when the track
    // has one; otherwise their presentation time stays unknown.
    if let Some(d) = default_ticks {
        let mut k = 0i64;
        for (i, cur) in pts.iter_mut().enumerate() {
            match t.track.pts_unknown.get_mut(i) {
                Some(u) if *u => {
                    k += 1;
                    *cur = cur.saturating_add(k.saturating_mul(d));
                    *u = false;
                }
                _ => k = 0,
            }
        }
    }
    // Presentation order, for durations and (with B-frames) decode times.
    let mut order: Vec<usize> = vec_with_capacity(n, 0)?;
    order.extend(0..n);
    order.sort_by_key(|&i| (pts.get(i).copied().unwrap_or(0), i));
    let mut durations: Vec<u32> = vec_with_capacity(n, 0)?;
    durations.resize(n, 0);
    for (k, &i) in order.iter().enumerate() {
        let explicit = t
            .block_durations
            .get(i)
            .copied()
            .flatten()
            .map(|d| d as i64)
            .or(default_ticks);
        let gap = order
            .get(k + 1)
            .and_then(|&j| pts.get(j)?.checked_sub(*pts.get(i)?))
            .filter(|&g| g > 0);
        if let Some(d) = durations.get_mut(i) {
            *d = u32::try_from(explicit.or(gap).unwrap_or(0).max(0)).unwrap_or(u32::MAX);
        }
    }
    // Matroska stores no decode times. When presentation order differs from decode
    // order (B-frames), decode time i is the i-th smallest presentation time, delayed
    // just enough that every dts is at or before its pts; otherwise dts = pts.
    let monotonic = pts.windows(2).all(|w| matches!(w, [a, b] if a <= b));
    let (dts, cts_off) = if monotonic {
        (pts, s.cts_off)
    } else {
        let sorted: Vec<i64> = order.iter().filter_map(|&i| pts.get(i).copied()).collect();
        // The smallest delay that keeps every dts at or before its pts.
        let lag = sorted
            .iter()
            .zip(&pts)
            .map(|(s, p)| s.saturating_sub(*p))
            .max()
            .unwrap_or(0)
            .max(0);
        let mut dts = vec_with_capacity(n, 0)?;
        let mut cts = vec_with_capacity(n, 0)?;
        for (i, &p) in pts.iter().enumerate() {
            let d = sorted.get(i).copied().unwrap_or(p).saturating_sub(lag);
            dts.push(d);
            cts.push(
                p.checked_sub(d)
                    .and_then(|c| i32::try_from(c).ok())
                    .ok_or_else(|| DemuxError::invalid("composition offset", 0))?,
            );
        }
        (dts, cts)
    };
    let samples = super::finish_index(
        timescale,
        t.track.kind,
        dts,
        cts_off,
        s.offset,
        s.size,
        s.sync,
        &durations,
    )?;
    t.track.samples = samples;
    t.track.timescale = timescale;
    t.track.durations = durations;
    if t.track.pts_unknown.iter().all(|&u| !u) {
        t.track.pts_unknown.clear();
    }
    Ok(t.track)
}
