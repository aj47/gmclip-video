//! MP4/MOV index (B-01, SPEC §6.1): our own box walker over the top level, `moov` read
//! into memory once and parsed as a tree, sample tables expanded, edit lists kept for
//! [`super::Track::packets`]. `mdat` is never read: the walker reads 16-byte headers to
//! skip it.
//!
//! `mp4-atom` (SPEC §6.1 allows it for `moov` subtrees) is not used: the subtrees the
//! index needs are few and fixed, and a dependency would need a cargo-vet audit.

pub mod boxes;
pub mod edit;
pub mod entry;
pub mod stbl;

use super::io::{ByteSource, Cursor, read_vec};
use super::{Codec, Container, DemuxError, Demuxed, Edit, Result, Track, TrackKind};
use boxes::{Node, fourcc};

/// Most top-level boxes a file may have before we call it hostile.
const MAX_TOP_LEVEL: u32 = 1 << 20;

/// A top-level box as found by the walker.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TopBox {
    pub kind: [u8; 4],
    pub offset: u64,
    pub size: u64,
    pub header: u64,
}

/// Walk the top level, reading only box headers.
pub fn top_level<S: ByteSource + ?Sized>(src: &mut S) -> Result<Vec<TopBox>> {
    let len = src.len();
    let mut out = Vec::new();
    let mut at = 0u64;
    let mut count = 0u32;
    while at < len {
        if len - at < 8 {
            break; // trailing padding shorter than a header
        }
        count += 1;
        if count > MAX_TOP_LEVEL {
            return Err(DemuxError::budget("too many top-level boxes", at));
        }
        let mut h = [0u8; 16];
        let want = usize::try_from((len - at).min(16)).unwrap_or(8);
        let h = h.get_mut(..want).unwrap_or_default();
        src.read_at(at, h)?;
        let mut c = Cursor::new(h, at);
        let size32 = c.u32()?;
        let kind = c.fourcc()?;
        let (header, size) = match size32 {
            0 => (8, len - at),
            1 => (16, c.u64()?),
            n => (8, u64::from(n)),
        };
        if size < header {
            return Err(DemuxError::invalid(
                format!("box '{}' size {size} below its header", fourcc(&kind)),
                at,
            ));
        }
        let end = at
            .checked_add(size)
            .ok_or_else(|| DemuxError::invalid("box size overflows", at))?;
        if end > len {
            if &kind == b"mdat" {
                // A truncated recording: keep what is there; samples past the end are
                // the caller's read error, not an index error.
                out.push(TopBox {
                    kind,
                    offset: at,
                    size: len - at,
                    header,
                });
                break;
            }
            return Err(DemuxError::invalid(
                format!(
                    "box '{}' ({size} bytes) runs past the end of the file ({len})",
                    fourcc(&kind)
                ),
                at,
            ));
        }
        out.push(TopBox {
            kind,
            offset: at,
            size,
            header,
        });
        at = end;
    }
    Ok(out)
}

/// Index an MP4/MOV file.
pub fn index<S: ByteSource + ?Sized>(src: &mut S) -> Result<Demuxed> {
    let file_len = src.len();
    let top = top_level(src)?;
    if top.iter().any(|b| &b.kind == b"moof") {
        return Err(DemuxError::unsupported("fragmented MP4 (M2)"));
    }
    let mut moovs = top.iter().filter(|b| &b.kind == b"moov");
    let moov = moovs
        .next()
        .ok_or_else(|| DemuxError::invalid("no moov box", 0))?;
    if moovs.next().is_some() {
        return Err(DemuxError::invalid("two moov boxes", moov.offset));
    }
    let payload = read_vec(src, moov.offset + moov.header, moov.size - moov.header)?;
    let mut box_budget = ((payload.len() as u64) / 8 + 1).min(super::MAX_NODES);
    let children = boxes::parse_children(&payload, moov.offset + moov.header, 1, &mut box_budget)?;
    let moov = Node {
        kind: *b"moov",
        offset: moov.offset,
        payload: &payload,
        payload_offset: moov.offset + moov.header,
        children,
    };
    if moov.child(b"mvex").is_some() {
        return Err(DemuxError::unsupported("fragmented MP4 (M2)"));
    }
    let movie_timescale = match moov.child(b"mvhd") {
        Some(mvhd) => {
            let mut c = mvhd.cursor();
            let v = c.u8()?;
            c.skip(3 + if v == 1 { 16 } else { 8 })?;
            c.u32()?
        }
        None => return Err(DemuxError::invalid("moov has no mvhd", moov.offset)),
    };
    // All tracks together may not have more entries than the file has bytes.
    let mut sample_budget = super::entry_budget(file_len);
    let mut tracks = Vec::new();
    let quicktime = match top.iter().find(|b| &b.kind == b"ftyp") {
        Some(f) => {
            let mut brand = [0u8; 4];
            src.read_at(f.offset + f.header, &mut brand)?;
            &brand == b"qt  "
        }
        // No ftyp: an old QuickTime file.
        None => true,
    };
    for trak in moov.children_of(b"trak") {
        match track(trak, movie_timescale, quicktime, &mut sample_budget) {
            Ok(t) => tracks.push(t),
            // A timecode, text or other non-A/V track that does not parse is kept as an
            // empty `Other` track (stream numbering stays aligned); A/V errors fail.
            Err(e)
                if !is_av(trak)
                    && !matches!(&e, DemuxError::Invalid { reason, .. } if reason.starts_with("budget")) =>
            {
                tracks.push(other_track(trak, movie_timescale))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Demuxed {
        container: Container::Mp4,
        tracks,
        cues: Vec::new(),
    })
}

fn handler(trak: &Node<'_>) -> Option<[u8; 4]> {
    let hdlr = trak.path(&[b"mdia", b"hdlr"])?;
    let mut c = hdlr.cursor();
    c.skip(8).ok()?;
    c.fourcc().ok()
}

fn is_av(trak: &Node<'_>) -> bool {
    matches!(handler(trak).as_ref(), Some(b"vide" | b"soun") | None)
}

fn other_track(trak: &Node<'_>, movie_timescale: u32) -> Track {
    let id = trak
        .child(b"tkhd")
        .and_then(|t| {
            let mut c = t.cursor();
            let v = c.u8().ok()?;
            c.skip(3 + if v == 1 { 16 } else { 8 }).ok()?;
            c.u32().ok()
        })
        .unwrap_or(0);
    Track {
        id: u64::from(id),
        kind: TrackKind::Other,
        codec: Codec::Other {
            name: handler(trak).map(|h| fourcc(&h)).unwrap_or_default(),
        },
        timescale: 1,
        width: 0,
        height: 0,
        sample_rate: 0,
        channels: 0,
        colour: Default::default(),
        samples: Default::default(),
        durations: Vec::new(),
        pts_unknown: Vec::new(),
        block_pos: Vec::new(),
        edits: Vec::new(),
        movie_timescale,
    }
}

fn track(
    trak: &Node<'_>,
    movie_timescale: u32,
    quicktime: bool,
    budget: &mut u64,
) -> Result<Track> {
    let missing = |what: &str| DemuxError::invalid(format!("trak has no {what}"), trak.offset);
    let tkhd = trak.child(b"tkhd").ok_or_else(|| missing("tkhd"))?;
    let mut c = tkhd.cursor();
    let v = c.u8()?;
    c.skip(3 + if v == 1 { 16 } else { 8 })?;
    let id = u64::from(c.u32()?);
    let mdia = trak.child(b"mdia").ok_or_else(|| missing("mdia"))?;
    let mdhd = mdia.child(b"mdhd").ok_or_else(|| missing("mdhd"))?;
    let mut c = mdhd.cursor();
    let v = c.u8()?;
    c.skip(3 + if v == 1 { 16 } else { 8 })?;
    let timescale = c.u32()?;
    if timescale == 0 {
        return Err(DemuxError::invalid("mdhd timescale 0", mdhd.offset));
    }
    let hdlr = mdia.child(b"hdlr").ok_or_else(|| missing("hdlr"))?;
    let mut c = hdlr.cursor();
    c.skip(8)?;
    let kind = match &c.fourcc()? {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        _ => TrackKind::Other,
    };
    let stbl = mdia
        .path(&[b"minf", b"stbl"])
        .ok_or_else(|| missing("minf/stbl"))?;
    let stsd = stbl.child(b"stsd").ok_or_else(|| missing("stsd"))?;
    let (entry, _) = entry::parse_stsd(stsd, kind, quicktime)?;
    let pcm = if kind == TrackKind::Audio {
        entry.pcm_frame_bytes
    } else {
        None
    };
    let s = stbl::expand(stbl, pcm, budget)?;
    let edits = match trak.path(&[b"edts", b"elst"]) {
        Some(elst) => parse_elst(elst)?,
        None => Vec::new(),
    };
    if !edits.is_empty() {
        edit::validate(&edits, movie_timescale, timescale).map_err(|e| match e {
            DemuxError::Invalid { reason, .. } => DemuxError::invalid(reason, trak.offset),
            other => other,
        })?;
    }
    let sample_rate = if kind == TrackKind::Audio && entry.sample_rate == 0 {
        timescale
    } else {
        entry.sample_rate
    };
    let samples = super::finish_index(
        timescale, kind, s.dts, s.cts, s.offset, s.size, s.sync, &s.dur,
    )?;
    Ok(Track {
        id,
        kind,
        codec: entry.codec,
        timescale,
        width: entry.width,
        height: entry.height,
        sample_rate,
        channels: entry.channels,
        colour: entry.colour,
        samples,
        durations: s.dur,
        pts_unknown: Vec::new(),
        block_pos: Vec::new(),
        edits,
        movie_timescale,
    })
}

fn parse_elst(elst: &Node<'_>) -> Result<Vec<Edit>> {
    let mut c = elst.cursor();
    let version = c.u8()?;
    c.skip(3)?;
    let count = c.u32()? as usize;
    let each = if version == 1 { 20 } else { 12 };
    if count.checked_mul(each).is_none_or(|b| b > c.remaining()) {
        return Err(DemuxError::invalid(
            format!("elst claims {count} entries"),
            elst.offset,
        ));
    }
    let mut v = super::io::vec_with_capacity(count, elst.offset)?;
    for _ in 0..count {
        let (segment_duration, media_time) = if version == 1 {
            (c.u64()?, c.i64()?)
        } else {
            (u64::from(c.u32()?), i64::from(c.i32()?))
        };
        let rate = c.i32()?;
        if media_time < -1 {
            return Err(DemuxError::invalid(
                format!("elst media_time {media_time}"),
                elst.offset,
            ));
        }
        v.push(Edit {
            segment_duration,
            media_time,
            rate,
        });
    }
    Ok(v)
}

/// The AVC configuration record of the first H.264 track, if any (B-03 input).
pub fn avcc(d: &Demuxed) -> Option<&[u8]> {
    d.tracks.iter().find_map(|t| match &t.codec {
        Codec::H264 { avcc } => Some(avcc.as_slice()),
        _ => None,
    })
}
