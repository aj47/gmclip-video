//! Our own ISO BMFF box-tree reader for muxer conformance (B-05). MP4Box is not
//! installed, so the R-EXP-5 box dumps come from here (and ffprobe in `tools/`).
//!
//! It is a test oracle, not the demuxer (B-01 owns that), but it reads untrusted bytes
//! the same way: every length is checked against the bytes that remain, nesting is capped
//! at 32, and nothing panics.

// Fixed-length slices returned by checked `get` are indexed directly; every
// file-derived offset goes through `get`.
#![allow(clippy::indexing_slicing)]

use std::fmt::Write as _;

/// Maximum box nesting (R-MEDIA-SEC-2's cap).
const MAX_DEPTH: usize = 32;

/// A box in the tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoxNode {
    pub fourcc: [u8; 4],
    pub offset: u64,
    pub size: u64,
    pub header: u64,
    pub children: Vec<BoxNode>,
}

impl BoxNode {
    #[must_use]
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.fourcc).into_owned()
    }

    /// The first descendant (depth-first) with this fourcc.
    #[must_use]
    pub fn find(&self, fourcc: &[u8; 4]) -> Option<&BoxNode> {
        self.children.iter().find_map(|c| {
            if &c.fourcc == fourcc {
                Some(c)
            } else {
                c.find(fourcc)
            }
        })
    }

    /// The payload after the box header.
    #[must_use]
    pub fn payload<'a>(&self, file: &'a [u8]) -> &'a [u8] {
        let start = self.offset.saturating_add(self.header);
        let end = self.offset.saturating_add(self.size);
        usize::try_from(start)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(s, e)| file.get(s..e))
            .unwrap_or(&[])
    }
}

/// Inspection errors.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum InspectError {
    #[error("box at {offset}: {reason}")]
    Malformed { offset: u64, reason: &'static str },
    #[error("missing box {0}")]
    Missing(&'static str),
}

fn container_skip(fourcc: &[u8; 4]) -> Option<usize> {
    match fourcc {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"dinf" | b"stbl" | b"edts" | b"udta" | b"mvex"
        | b"moof" | b"traf" => Some(0),
        b"stsd" | b"dref" => Some(8), // full box + entry_count
        b"meta" => Some(4),           // full box (ISO; QuickTime's plain meta is not written here)
        b"ilst" | b"----" => Some(0),
        b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"vp09" => Some(78),
        b"mp4a" => Some(28),
        _ => None,
    }
}

/// Parses the box tree of a whole file (or any byte range of sibling boxes).
pub fn parse(data: &[u8]) -> Result<Vec<BoxNode>, InspectError> {
    parse_range(data, 0, data.len() as u64, 0)
}

fn rd_u32(d: &[u8], at: u64) -> Option<u32> {
    let a = usize::try_from(at).ok()?;
    let b = d.get(a..a.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn rd_u64(d: &[u8], at: u64) -> Option<u64> {
    let a = usize::try_from(at).ok()?;
    let b = d.get(a..a.checked_add(8)?)?;
    let mut x = [0u8; 8];
    x.copy_from_slice(b);
    Some(u64::from_be_bytes(x))
}

fn parse_range(d: &[u8], start: u64, end: u64, depth: usize) -> Result<Vec<BoxNode>, InspectError> {
    if depth > MAX_DEPTH {
        return Err(InspectError::Malformed {
            offset: start,
            reason: "nesting deeper than 32",
        });
    }
    let mut out = Vec::new();
    let mut at = start;
    while at < end {
        let bad = |reason| InspectError::Malformed { offset: at, reason };
        if end - at < 8 {
            return Err(bad("trailing bytes shorter than a box header"));
        }
        let size32 = rd_u32(d, at).ok_or_else(|| bad("truncated header"))?;
        let mut fourcc = [0u8; 4];
        let a = usize::try_from(at + 4).map_err(|_| bad("offset"))?;
        fourcc.copy_from_slice(d.get(a..a + 4).ok_or_else(|| bad("truncated header"))?);
        let (size, header) = match size32 {
            0 => (end - at, 8),
            1 => (
                rd_u64(d, at + 8).ok_or_else(|| bad("truncated largesize"))?,
                16,
            ),
            s => (u64::from(s), 8),
        };
        if size < header || size > end - at {
            return Err(bad("size outside its parent"));
        }
        let children = match container_skip(&fourcc) {
            Some(skip) => {
                let cs = at + header + skip as u64;
                if cs > at + size {
                    return Err(bad("container shorter than its fixed fields"));
                }
                parse_range(d, cs, at + size, depth + 1)?
            }
            None => Vec::new(),
        };
        out.push(BoxNode {
            fourcc,
            offset: at,
            size,
            header,
            children,
        });
        at += size;
    }
    Ok(out)
}

/// A big-endian cursor over a payload; every read is checked.
#[derive(Debug)]
struct Rd<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).and_then(|b| b.first().copied())
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| i32::from_be_bytes(v.to_be_bytes()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| {
            let mut x = [0u8; 8];
            x.copy_from_slice(b);
            u64::from_be_bytes(x)
        })
    }
    fn i64(&mut self) -> Option<i64> {
        self.u64().map(|v| i64::from_be_bytes(v.to_be_bytes()))
    }
    /// version, flags
    fn full(&mut self) -> Option<(u8, u32)> {
        let v = self.u8()?;
        let f = self.take(3)?;
        Some((v, u32::from_be_bytes([0, f[0], f[1], f[2]])))
    }
}

/// One sample as the file describes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleInfo {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub sync: bool,
}

/// `cslg` fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cslg {
    pub version: u8,
    pub shift: i64,
    pub least: i64,
    pub greatest: i64,
    pub start: i64,
    pub end: i64,
}

/// One track as the file describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
    pub id: u32,
    pub handler: [u8; 4],
    pub timescale: u32,
    pub media_duration: u64,
    pub tkhd_duration: u64,
    pub sample_entry: [u8; 4],
    pub has_edts: bool,
    pub ctts_version: Option<u8>,
    pub cslg: Option<Cslg>,
    pub has_stss: bool,
    pub co64: bool,
    pub samples: Vec<SampleInfo>,
    /// `elst` entries (segment duration in the movie timescale, media time in the track
    /// timescale, rate 16.16).
    pub edits: Vec<Edit>,
    /// What the sample entry configures.
    pub entry: EntryInfo,
}

/// One `elst` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    pub segment_duration: u64,
    pub media_time: i64,
    pub rate: i32,
}

/// Sample-entry facts: the codec configuration record (`avcC` payload, or the
/// AudioSpecificConfig inside `esds`), dimensions or channels/rate, and tags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryInfo {
    pub config: Vec<u8>,
    pub width: u16,
    pub height: u16,
    pub channels: u16,
    pub sample_rate: u32,
    /// `colr`/`nclx`: (primaries, transfer, matrix, full range).
    pub colr: Option<(u16, u16, u16, bool)>,
    /// `pasp`: (h spacing, v spacing).
    pub pasp: Option<(u32, u32)>,
}

/// The movie timescale (`mvhd`).
pub fn movie_timescale(file: &[u8]) -> Result<u32, InspectError> {
    let top = parse(file)?;
    let moov = top
        .iter()
        .find(|b| &b.fourcc == b"moov")
        .ok_or(missing("moov"))?;
    let mvhd = moov.find(b"mvhd").ok_or(missing("mvhd"))?;
    let mut r = Rd {
        d: mvhd.payload(file),
        at: 0,
    };
    let (v, _) = r.full().ok_or_else(|| trunc(mvhd))?;
    r.take(if v == 1 { 16 } else { 8 })
        .ok_or_else(|| trunc(mvhd))?;
    r.u32().ok_or_else(|| trunc(mvhd))
}

fn edits(file: &[u8], trak: &BoxNode) -> Result<Vec<Edit>, InspectError> {
    let Some(elst) = trak.find(b"elst") else {
        return Ok(Vec::new());
    };
    let mut r = Rd {
        d: elst.payload(file),
        at: 0,
    };
    let (v, _) = r.full().ok_or_else(|| trunc(elst))?;
    let n = r.u32().ok_or_else(|| trunc(elst))?;
    if n > 1 << 16 {
        return Err(InspectError::Malformed {
            offset: elst.offset,
            reason: "more than 65536 edits",
        });
    }
    let mut out = Vec::new();
    for _ in 0..n {
        let (segment_duration, media_time) = if v == 1 {
            (
                r.u64().ok_or_else(|| trunc(elst))?,
                r.i64().ok_or_else(|| trunc(elst))?,
            )
        } else {
            (
                u64::from(r.u32().ok_or_else(|| trunc(elst))?),
                i64::from(r.i32().ok_or_else(|| trunc(elst))?),
            )
        };
        let rate = r.i32().ok_or_else(|| trunc(elst))?;
        out.push(Edit {
            segment_duration,
            media_time,
            rate,
        });
    }
    Ok(out)
}

/// The AudioSpecificConfig inside an `esds` payload (DecoderSpecificInfo, tag 5).
fn esds_asc(esds: &[u8]) -> Option<Vec<u8>> {
    let mut r = Rd { d: esds, at: 4 };
    while let Some(tag) = r.u8() {
        let mut len = 0usize;
        for _ in 0..4 {
            let b = r.u8()?;
            len = (len << 7) | usize::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
        match tag {
            0x03 => {
                r.take(2)?; // ES_ID
                let flags = r.u8()?;
                if flags & 0x80 != 0 {
                    r.take(2)?;
                }
                if flags & 0x40 != 0 {
                    let n = r.u8()?;
                    r.take(usize::from(n))?;
                }
                if flags & 0x20 != 0 {
                    r.take(2)?;
                }
            }
            0x04 => {
                r.take(13)?;
            }
            0x05 => return r.take(len).map(<[u8]>::to_vec),
            _ => {
                r.take(len)?;
            }
        }
    }
    None
}

fn entry_info(file: &[u8], stsd: &BoxNode) -> EntryInfo {
    let mut e = EntryInfo::default();
    let Some(entry) = stsd.children.first() else {
        return e;
    };
    let p = entry.payload(file);
    match &entry.fourcc {
        b"avc1" | b"avc3" => {
            e.width = p
                .get(24..26)
                .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
            e.height = p
                .get(26..28)
                .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
            e.config = entry
                .find(b"avcC")
                .map(|b| b.payload(file).to_vec())
                .unwrap_or_default();
        }
        b"mp4a" => {
            e.channels = p
                .get(16..18)
                .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
            e.sample_rate = p
                .get(24..26)
                .map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])));
            e.config = entry
                .find(b"esds")
                .and_then(|b| esds_asc(b.payload(file)))
                .unwrap_or_default();
        }
        _ => {}
    }
    if let Some(c) = entry.find(b"colr") {
        let p = c.payload(file);
        if p.get(..4) == Some(b"nclx") && p.len() >= 11 {
            e.colr = Some((
                u16::from_be_bytes([p[4], p[5]]),
                u16::from_be_bytes([p[6], p[7]]),
                u16::from_be_bytes([p[8], p[9]]),
                p[10] & 0x80 != 0,
            ));
        }
    }
    if let Some(c) = entry.find(b"pasp") {
        let p = c.payload(file);
        if p.len() >= 8 {
            e.pasp = Some((
                u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
                u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
            ));
        }
    }
    e
}

fn missing(what: &'static str) -> InspectError {
    InspectError::Missing(what)
}

fn trunc(node: &BoxNode) -> InspectError {
    InspectError::Malformed {
        offset: node.offset,
        reason: "truncated payload",
    }
}

/// Every track with its expanded sample table (decode order; `pts` = DTS + `ctts`).
pub fn tracks(file: &[u8]) -> Result<Vec<TrackInfo>, InspectError> {
    let top = parse(file)?;
    let moov = top
        .iter()
        .find(|b| &b.fourcc == b"moov")
        .ok_or(missing("moov"))?;
    moov.children
        .iter()
        .filter(|b| &b.fourcc == b"trak")
        .map(|t| track(file, t))
        .collect()
}

fn track(file: &[u8], trak: &BoxNode) -> Result<TrackInfo, InspectError> {
    let get = |f: &'static [u8; 4], name: &'static str| trak.find(f).ok_or(missing(name));
    let tkhd = get(b"tkhd", "tkhd")?;
    let mut r = Rd {
        d: tkhd.payload(file),
        at: 0,
    };
    let (v, _) = r.full().ok_or_else(|| trunc(tkhd))?;
    let (id, tkhd_duration) = if v == 1 {
        r.take(16).ok_or_else(|| trunc(tkhd))?;
        let id = r.u32().ok_or_else(|| trunc(tkhd))?;
        r.take(4).ok_or_else(|| trunc(tkhd))?;
        (id, r.u64().ok_or_else(|| trunc(tkhd))?)
    } else {
        r.take(8).ok_or_else(|| trunc(tkhd))?;
        let id = r.u32().ok_or_else(|| trunc(tkhd))?;
        r.take(4).ok_or_else(|| trunc(tkhd))?;
        (id, u64::from(r.u32().ok_or_else(|| trunc(tkhd))?))
    };
    let mdhd = get(b"mdhd", "mdhd")?;
    let mut r = Rd {
        d: mdhd.payload(file),
        at: 0,
    };
    let (v, _) = r.full().ok_or_else(|| trunc(mdhd))?;
    let (timescale, media_duration) = if v == 1 {
        r.take(16).ok_or_else(|| trunc(mdhd))?;
        (
            r.u32().ok_or_else(|| trunc(mdhd))?,
            r.u64().ok_or_else(|| trunc(mdhd))?,
        )
    } else {
        r.take(8).ok_or_else(|| trunc(mdhd))?;
        (
            r.u32().ok_or_else(|| trunc(mdhd))?,
            u64::from(r.u32().ok_or_else(|| trunc(mdhd))?),
        )
    };
    let hdlr = get(b"hdlr", "hdlr")?;
    let mut handler = [0u8; 4];
    handler.copy_from_slice(hdlr.payload(file).get(8..12).ok_or_else(|| trunc(hdlr))?);
    let stsd = get(b"stsd", "stsd")?;
    let sample_entry = stsd
        .children
        .first()
        .map(|c| c.fourcc)
        .ok_or(missing("sample entry"))?;

    // stts
    let stts = get(b"stts", "stts")?;
    let mut r = Rd {
        d: stts.payload(file),
        at: 0,
    };
    r.full().ok_or_else(|| trunc(stts))?;
    let n = r.u32().ok_or_else(|| trunc(stts))?;
    let mut durations: Vec<u32> = Vec::new();
    for _ in 0..n {
        let count = r.u32().ok_or_else(|| trunc(stts))?;
        let delta = r.u32().ok_or_else(|| trunc(stts))?;
        if durations.len() as u64 + u64::from(count) > (1 << 26) {
            return Err(InspectError::Malformed {
                offset: stts.offset,
                reason: "more than 2^26 samples",
            });
        }
        durations.extend(std::iter::repeat_n(delta, count as usize));
    }
    // stsz
    let stsz = get(b"stsz", "stsz")?;
    let mut r = Rd {
        d: stsz.payload(file),
        at: 0,
    };
    r.full().ok_or_else(|| trunc(stsz))?;
    let fixed = r.u32().ok_or_else(|| trunc(stsz))?;
    let count = r.u32().ok_or_else(|| trunc(stsz))?;
    if count as usize != durations.len() {
        return Err(InspectError::Malformed {
            offset: stsz.offset,
            reason: "stsz count ≠ stts total",
        });
    }
    let mut sizes = Vec::with_capacity(durations.len());
    for _ in 0..count {
        sizes.push(if fixed != 0 {
            fixed
        } else {
            r.u32().ok_or_else(|| trunc(stsz))?
        });
    }
    // ctts
    let mut offsets = vec![0i64; sizes.len()];
    let mut ctts_version = None;
    if let Some(ctts) = trak.find(b"ctts") {
        let mut r = Rd {
            d: ctts.payload(file),
            at: 0,
        };
        let (v, _) = r.full().ok_or_else(|| trunc(ctts))?;
        ctts_version = Some(v);
        let n = r.u32().ok_or_else(|| trunc(ctts))?;
        let mut i = 0usize;
        for _ in 0..n {
            let count = r.u32().ok_or_else(|| trunc(ctts))?;
            if i.saturating_add(count as usize) > offsets.len() {
                return Err(InspectError::Malformed {
                    offset: ctts.offset,
                    reason: "ctts longer than stts",
                });
            }
            let off = if v == 1 {
                i64::from(r.i32().ok_or_else(|| trunc(ctts))?)
            } else {
                i64::from(r.u32().ok_or_else(|| trunc(ctts))?)
            };
            for _ in 0..count {
                *offsets.get_mut(i).ok_or(InspectError::Malformed {
                    offset: ctts.offset,
                    reason: "ctts longer than stts",
                })? = off;
                i += 1;
            }
        }
        if i != offsets.len() {
            return Err(InspectError::Malformed {
                offset: ctts.offset,
                reason: "ctts total ≠ sample count",
            });
        }
    }
    let cslg = match trak.find(b"cslg") {
        None => None,
        Some(c) => {
            let mut r = Rd {
                d: c.payload(file),
                at: 0,
            };
            let (v, _) = r.full().ok_or_else(|| trunc(c))?;
            let mut f = [0i64; 5];
            for x in &mut f {
                *x = if v == 1 {
                    r.i64()
                } else {
                    r.i32().map(i64::from)
                }
                .ok_or_else(|| trunc(c))?;
            }
            Some(Cslg {
                version: v,
                shift: f[0],
                least: f[1],
                greatest: f[2],
                start: f[3],
                end: f[4],
            })
        }
    };
    // stss
    let mut sync = vec![true; sizes.len()];
    let has_stss = if let Some(s) = trak.find(b"stss") {
        sync.iter_mut().for_each(|x| *x = false);
        let mut r = Rd {
            d: s.payload(file),
            at: 0,
        };
        r.full().ok_or_else(|| trunc(s))?;
        let n = r.u32().ok_or_else(|| trunc(s))?;
        for _ in 0..n {
            let k = r.u32().ok_or_else(|| trunc(s))? as usize;
            *k.checked_sub(1)
                .and_then(|k| sync.get_mut(k))
                .ok_or(InspectError::Malformed {
                    offset: s.offset,
                    reason: "stss index out of range",
                })? = true;
        }
        true
    } else {
        false
    };
    // chunk offsets
    let (co, co64) = match (trak.find(b"stco"), trak.find(b"co64")) {
        (Some(b), None) => (b, false),
        (None, Some(b)) => (b, true),
        _ => return Err(missing("exactly one of stco/co64")),
    };
    let mut r = Rd {
        d: co.payload(file),
        at: 0,
    };
    r.full().ok_or_else(|| trunc(co))?;
    let n = r.u32().ok_or_else(|| trunc(co))?;
    let mut chunk_offsets = Vec::new();
    for _ in 0..n {
        chunk_offsets.push(
            if co64 {
                r.u64()
            } else {
                r.u32().map(u64::from)
            }
            .ok_or_else(|| trunc(co))?,
        );
    }
    // stsc → samples per chunk
    let stsc = get(b"stsc", "stsc")?;
    let mut r = Rd {
        d: stsc.payload(file),
        at: 0,
    };
    r.full().ok_or_else(|| trunc(stsc))?;
    let n = r.u32().ok_or_else(|| trunc(stsc))?;
    let mut entries = Vec::new();
    for _ in 0..n {
        let first = r.u32().ok_or_else(|| trunc(stsc))?;
        let per = r.u32().ok_or_else(|| trunc(stsc))?;
        r.u32().ok_or_else(|| trunc(stsc))?;
        if entries
            .last()
            .map_or(first != 1, |&(prev, _)| first <= prev)
        {
            return Err(InspectError::Malformed {
                offset: stsc.offset,
                reason: "stsc first_chunk must start at 1 and increase",
            });
        }
        entries.push((first, per));
    }
    let mut samples = Vec::with_capacity(sizes.len());
    let mut k = 0usize;
    let mut dts = 0i64;
    for (ci, &off) in chunk_offsets.iter().enumerate() {
        let chunk_no = u32::try_from(ci + 1).unwrap_or(u32::MAX);
        let per = entries
            .iter()
            .rev()
            .find(|e| e.0 <= chunk_no)
            .map_or(0, |e| e.1);
        let mut at = off;
        for _ in 0..per {
            let (Some(&size), Some(&duration), Some(&o), Some(&s)) =
                (sizes.get(k), durations.get(k), offsets.get(k), sync.get(k))
            else {
                return Err(InspectError::Malformed {
                    offset: stsc.offset,
                    reason: "chunks hold more samples than stsz",
                });
            };
            let next = at
                .checked_add(u64::from(size))
                .filter(|&e| e <= file.len() as u64)
                .ok_or(InspectError::Malformed {
                    offset: co.offset,
                    reason: "sample bytes outside the file",
                })?;
            samples.push(SampleInfo {
                offset: at,
                size,
                dts,
                pts: dts + o,
                duration,
                sync: s,
            });
            at = next;
            dts += i64::from(duration);
            k += 1;
        }
    }
    if k != sizes.len() {
        return Err(InspectError::Malformed {
            offset: stsc.offset,
            reason: "chunks hold fewer samples than stsz",
        });
    }
    Ok(TrackInfo {
        id,
        handler,
        timescale,
        media_duration,
        tkhd_duration,
        sample_entry,
        has_edts: trak.find(b"edts").is_some() || trak.find(b"elst").is_some(),
        ctts_version,
        cslg,
        has_stss,
        co64,
        samples,
        edits: edits(file, trak)?,
        entry: entry_info(file, stsd),
    })
}

/// A text dump of the box tree with the fields R-EXP-5 cares about, one box per line
/// (indent = depth). Offsets are omitted so two layouts compare by structure; sizes of
/// table boxes are kept.
pub fn dump(file: &[u8]) -> Result<String, InspectError> {
    let top = parse(file)?;
    let mut s = String::new();
    for b in &top {
        dump_node(file, b, 0, &mut s);
    }
    Ok(s)
}

fn dump_node(file: &[u8], b: &BoxNode, depth: usize, s: &mut String) {
    let _ = write!(s, "{}{} size={}", "  ".repeat(depth), b.name(), b.size);
    let p = b.payload(file);
    let mut r = Rd { d: p, at: 0 };
    match &b.fourcc {
        b"ftyp" => {
            let major = r.take(4).map(String::from_utf8_lossy).unwrap_or_default();
            let minor = r.u32().unwrap_or(0);
            let _ = write!(s, " major={major} minor={minor} compat=");
            while let Some(c) = r.take(4) {
                let _ = write!(s, "{},", String::from_utf8_lossy(c));
            }
        }
        b"mdat" => {
            let _ = write!(s, " largesize={}", b.header == 16);
        }
        b"mvhd" | b"mdhd" => {
            if let Some((v, _)) = r.full() {
                let skip = if v == 1 { 16 } else { 8 };
                let _ = r.take(skip);
                let ts = r.u32().unwrap_or(0);
                let d = if v == 1 {
                    r.u64().unwrap_or(0)
                } else {
                    u64::from(r.u32().unwrap_or(0))
                };
                let _ = write!(s, " v={v} timescale={ts} duration={d}");
            }
        }
        b"tkhd" => {
            if let Some((v, f)) = r.full() {
                let skip = if v == 1 { 16 } else { 8 };
                let _ = r.take(skip);
                let id = r.u32().unwrap_or(0);
                let _ = r.take(4);
                let d = if v == 1 {
                    r.u64().unwrap_or(0)
                } else {
                    u64::from(r.u32().unwrap_or(0))
                };
                let _ = r.take(52);
                let w = r.u32().unwrap_or(0) >> 16;
                let h = r.u32().unwrap_or(0) >> 16;
                let _ = write!(s, " v={v} flags={f} id={id} duration={d} {w}x{h}");
            }
        }
        b"hdlr" => {
            let _ = r.take(8);
            let _ = write!(
                s,
                " type={}",
                r.take(4).map(String::from_utf8_lossy).unwrap_or_default()
            );
        }
        b"stts" | b"stss" | b"stsc" | b"stco" | b"co64" => {
            if let Some((v, _)) = r.full() {
                let _ = write!(s, " v={v} entries={}", r.u32().unwrap_or(0));
            }
        }
        b"ctts" => {
            if let Some((v, _)) = r.full() {
                let _ = write!(s, " v={v} entries={}", r.u32().unwrap_or(0));
            }
        }
        b"stsz" => {
            if let Some((v, _)) = r.full() {
                let fixed = r.u32().unwrap_or(0);
                let _ = write!(
                    s,
                    " v={v} sample_size={fixed} count={}",
                    r.u32().unwrap_or(0)
                );
            }
        }
        b"cslg" => {
            if let Some((v, _)) = r.full() {
                let f: Vec<i64> = (0..5)
                    .map(|_| {
                        if v == 1 {
                            r.i64().unwrap_or(0)
                        } else {
                            i64::from(r.i32().unwrap_or(0))
                        }
                    })
                    .collect();
                let _ = write!(
                    s,
                    " v={v} shift={} least={} greatest={} start={} end={}",
                    f[0], f[1], f[2], f[3], f[4]
                );
            }
        }
        b"avc1" | b"mp4a" => {
            let _ = r.take(8);
            if &b.fourcc == b"avc1" {
                let _ = r.take(16);
                let _ = write!(s, " {}x{}", r.u16().unwrap_or(0), r.u16().unwrap_or(0));
            } else {
                let _ = r.take(8);
                let ch = r.u16().unwrap_or(0);
                let bits = r.u16().unwrap_or(0);
                let _ = r.take(4);
                let _ = write!(
                    s,
                    " channels={ch} bits={bits} rate={}",
                    r.u32().unwrap_or(0) >> 16
                );
            }
        }
        b"colr" => {
            let kind = r.take(4).map(String::from_utf8_lossy).unwrap_or_default();
            let _ = write!(
                s,
                " {kind} {}/{}/{} full={}",
                r.u16().unwrap_or(0),
                r.u16().unwrap_or(0),
                r.u16().unwrap_or(0),
                r.u8().unwrap_or(0) >> 7
            );
        }
        b"pasp" => {
            let _ = write!(s, " {}:{}", r.u32().unwrap_or(0), r.u32().unwrap_or(0));
        }
        b"avcC" => {
            let _ = write!(s, " bytes={}", p.len());
        }
        _ => {}
    }
    s.push('\n');
    for c in &b.children {
        dump_node(file, c, depth + 1, s);
    }
}

/// The iTunSMPB gapless tag (`moov/udta/meta/ilst/----` named `iTunSMPB`), if present:
/// (priming, padding, length).
pub fn itunsmpb(file: &[u8]) -> Result<Option<super::Gapless>, InspectError> {
    let top = parse(file)?;
    let Some(moov) = top.iter().find(|b| &b.fourcc == b"moov") else {
        return Ok(None);
    };
    let Some(ilst) = moov.find(b"ilst") else {
        return Ok(None);
    };
    for item in ilst.children.iter().filter(|c| &c.fourcc == b"----") {
        let name = item
            .find(b"name")
            .map(|n| n.payload(file).get(4..).unwrap_or(&[]).to_vec());
        if name.as_deref() != Some(b"iTunSMPB".as_slice()) {
            continue;
        }
        let Some(data) = item.find(b"data") else {
            continue;
        };
        let text = String::from_utf8_lossy(data.payload(file).get(8..).unwrap_or(&[])).into_owned();
        let f: Vec<&str> = text.split_whitespace().collect();
        let hex = |i: usize| f.get(i).and_then(|s| u64::from_str_radix(s, 16).ok());
        if let (Some(p), Some(pad), Some(len)) = (hex(1), hex(2), hex(3)) {
            return Ok(Some(super::Gapless {
                priming: u32::try_from(p).unwrap_or(u32::MAX),
                padding: u32::try_from(pad).unwrap_or(u32::MAX),
                length: len,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn esds_parsing_is_checked() {
        // ES (03) → DecoderConfig (04) → DecoderSpecificInfo (05), 4-byte and 1-byte lengths.
        let mut e = vec![
            0, 0, 0, 0, 0x03, 0x80, 0x80, 0x80, 0x19, 0, 1, 0, 0x04, 0x11,
        ];
        e.extend_from_slice(&[
            0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 0x02, 0x11, 0x90, 0x06, 0x01, 0x02,
        ]);
        assert_eq!(esds_asc(&e), Some(vec![0x11, 0x90]));
        // ES_ID flags with the optional fields present.
        let f = vec![
            0, 0, 0, 0, 0x03, 0x10, 0, 1, 0xe0, 0, 2, 3, b'a', b'b', b'c', 0, 4, 0x05, 0x01, 0x12,
        ];
        assert_eq!(esds_asc(&f), Some(vec![0x12]));
        // Truncated anywhere: None, never a panic.
        for n in 0..e.len() {
            let _ = esds_asc(&e[..n]);
        }
        assert_eq!(esds_asc(&e[..e.len() - 6]), None);
        assert_eq!(esds_asc(&[0, 0, 0, 0, 0x05, 0x80, 0x80, 0x80, 0x7f]), None);
    }
}
