//! `ftyp` and `moov` construction from finished sample tables.

use super::bytes::{BoxBuf, descriptor_len};
use super::{Codec, ColorTags, Ctts, Gapless, MOVIE_TIMESCALE, MuxError, TrackConfig};

/// `ftyp`: fixed size, written before anything else.
pub(crate) fn ftyp() -> Result<Vec<u8>, MuxError> {
    let mut b = BoxBuf::new();
    let o = b.begin(b"ftyp");
    b.bytes(b"isom");
    b.u32(0x200);
    // iso4: version-1 `ctts` (signed composition offsets); avc1: H.264 sample entries.
    for brand in [b"isom", b"iso2", b"iso4", b"avc1", b"mp41"] {
        b.bytes(brand);
    }
    b.end(o)?;
    Ok(b.into_inner())
}

/// Bitrate facts for `btrt` and `esds`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Rates {
    pub buffer_size_db: u32,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
}

/// One track's finished tables.
#[derive(Debug)]
pub(crate) struct TrackTables<'a> {
    pub id: u32,
    pub cfg: &'a TrackConfig,
    pub sizes: &'a [u32],
    pub durations: &'a [u32],
    /// Composition offset per sample (PTS − file DTS), in track ticks.
    pub cts_offsets: &'a [i64],
    pub sync: &'a [bool],
    pub chunk_offsets: &'a [u64],
    pub chunk_counts: &'a [u32],
    pub media_duration: u64,
    pub rates: Rates,
}

impl TrackTables<'_> {
    pub(crate) fn ctts_kind(&self) -> Ctts {
        if self.cts_offsets.iter().all(|&o| o == 0) {
            Ctts::None
        } else if self.cts_offsets.iter().any(|&o| o < 0) {
            Ctts::V1
        } else {
            Ctts::V0
        }
    }

    pub(crate) fn needs_stss(&self) -> bool {
        self.sync.iter().any(|&s| !s)
    }
}

/// `moov` for finished tracks. `force_v1` writes 64-bit header durations (used to size
/// the worst case); otherwise version 1 is used only when a duration needs it.
pub(crate) fn moov(
    tracks: &[TrackTables<'_>],
    gapless: Option<&Gapless>,
    co64: bool,
    force_v1: bool,
) -> Result<Vec<u8>, MuxError> {
    let mut b = BoxBuf::new();
    let o = b.begin(b"moov");
    let mut movie_dur = 0u64;
    for t in tracks {
        movie_dur = movie_dur.max(to_movie(
            t.cfg.edit.map_or(t.media_duration, |e| e.presented),
            t.cfg.timescale,
        )?);
    }
    let next_id = u32::try_from(tracks.len())
        .map_err(|_| MuxError::Overflow)?
        .checked_add(1)
        .ok_or(MuxError::Overflow)?;
    mvhd(&mut b, movie_dur, next_id, force_v1)?;
    for t in tracks {
        trak(&mut b, t, co64, force_v1)?;
    }
    if let Some(g) = gapless {
        itunsmpb(&mut b, g)?;
    }
    b.end(o)?;
    Ok(b.into_inner())
}

/// `udta/meta/ilst/----` with mean `com.apple.iTunes`, name `iTunSMPB`, as
/// AudioToolbox's own m4a files carry it.
fn itunsmpb(b: &mut BoxBuf, g: &Gapless) -> Result<(), MuxError> {
    let udta = b.begin(b"udta");
    let meta = b.begin_full(b"meta", 0, 0);
    let hdlr = b.begin_full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(b"mdir");
    b.bytes(b"appl");
    b.zeros(10); // reserved + empty name, byte for byte as AudioToolbox writes it
    b.end(hdlr)?;
    let ilst = b.begin(b"ilst");
    let item = b.begin(b"----");
    let mean = b.begin_full(b"mean", 0, 0);
    b.bytes(b"com.apple.iTunes");
    b.end(mean)?;
    let name = b.begin_full(b"name", 0, 0);
    b.bytes(b"iTunSMPB");
    b.end(name)?;
    let data = b.begin(b"data");
    b.u32(1); // UTF-8
    b.u32(0); // locale
    b.bytes(g.itunsmpb().as_bytes());
    b.end(data)?;
    b.end(item)?;
    b.end(ilst)?;
    b.end(meta)?;
    b.end(udta)
}

/// Media ticks → movie ticks, rounded to nearest (an edit's length).
fn to_movie_round(d: u64, timescale: u32) -> Result<u64, MuxError> {
    let ts = u128::from(timescale.max(1));
    u64::try_from((u128::from(d) * u128::from(MOVIE_TIMESCALE) + ts / 2) / ts)
        .map_err(|_| MuxError::Overflow)
}

/// Media ticks → movie ticks, rounded up (ffmpeg's rule), so the movie never ends early.
fn to_movie(d: u64, timescale: u32) -> Result<u64, MuxError> {
    let n = u128::from(d) * u128::from(MOVIE_TIMESCALE);
    let ts = u128::from(timescale);
    if ts == 0 {
        return Err(MuxError::Internal("timescale 0"));
    }
    u64::try_from(n.div_ceil(ts)).map_err(|_| MuxError::Overflow)
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn mvhd(b: &mut BoxBuf, duration: u64, next_id: u32, force_v1: bool) -> Result<(), MuxError> {
    let v1 = force_v1 || duration > u64::from(u32::MAX);
    let o = b.begin_full(b"mvhd", u8::from(v1), 0);
    if v1 {
        b.u64(0);
        b.u64(0);
        b.u32(MOVIE_TIMESCALE);
        b.u64(duration);
    } else {
        b.u32(0);
        b.u32(0);
        b.u32(MOVIE_TIMESCALE);
        b.u32(u32::try_from(duration).map_err(|_| MuxError::Overflow)?);
    }
    b.u32(0x0001_0000); // rate 1.0
    b.u16(0x0100); // volume 1.0
    b.zeros(10);
    for m in MATRIX {
        b.u32(m);
    }
    b.zeros(24); // pre_defined
    b.u32(next_id);
    b.end(o)
}

fn trak(b: &mut BoxBuf, t: &TrackTables<'_>, co64: bool, force_v1: bool) -> Result<(), MuxError> {
    let o = b.begin(b"trak");
    tkhd(b, t, force_v1)?;
    // No `edts` in exports (SPEC §6.5 step 4, R-EXP-6); only a remux asks for one.
    if let Some(e) = t.cfg.edit {
        let seg = match e.segment {
            Some(s) => s,
            None => to_movie_round(e.presented, t.cfg.timescale)?,
        };
        let v1 = force_v1 || seg > u64::from(u32::MAX) || i32::try_from(e.media_time).is_err();
        let edts = b.begin(b"edts");
        let elst = b.begin_full(b"elst", u8::from(v1), 0);
        b.u32(1);
        if v1 {
            b.u64(seg);
            b.i64(e.media_time);
        } else {
            b.u32(u32::try_from(seg).map_err(|_| MuxError::Overflow)?);
            b.i32(i32::try_from(e.media_time).map_err(|_| MuxError::Overflow)?);
        }
        b.u32(0x0001_0000);
        b.end(elst)?;
        b.end(edts)?;
    }
    let mdia = b.begin(b"mdia");
    mdhd(b, t.media_duration, t.cfg.timescale, force_v1)?;
    hdlr(b, t.cfg.is_video())?;
    let minf = b.begin(b"minf");
    if t.cfg.is_video() {
        let v = b.begin_full(b"vmhd", 0, 1);
        b.zeros(8); // graphicsmode + opcolor
        b.end(v)?;
    } else {
        let s = b.begin_full(b"smhd", 0, 0);
        b.zeros(4); // balance + reserved
        b.end(s)?;
    }
    let dinf = b.begin(b"dinf");
    let dref = b.begin_full(b"dref", 0, 0);
    b.u32(1);
    let url = b.begin_full(b"url ", 0, 1); // self-contained
    b.end(url)?;
    b.end(dref)?;
    b.end(dinf)?;
    stbl(b, t, co64)?;
    b.end(minf)?;
    b.end(mdia)?;
    b.end(o)
}

fn tkhd(b: &mut BoxBuf, t: &TrackTables<'_>, force_v1: bool) -> Result<(), MuxError> {
    let dur = to_movie(
        t.cfg.edit.map_or(t.media_duration, |e| e.presented),
        t.cfg.timescale,
    )?;
    let v1 = force_v1 || dur > u64::from(u32::MAX);
    let o = b.begin_full(b"tkhd", u8::from(v1), 0x3); // enabled | in movie
    if v1 {
        b.u64(0);
        b.u64(0);
        b.u32(t.id);
        b.u32(0);
        b.u64(dur);
    } else {
        b.u32(0);
        b.u32(0);
        b.u32(t.id);
        b.u32(0);
        b.u32(u32::try_from(dur).map_err(|_| MuxError::Overflow)?);
    }
    b.zeros(8);
    b.u16(0); // layer
    b.u16(u16::from(!t.cfg.is_video())); // alternate_group: 0 video, 1 audio (as ffmpeg)
    b.u16(if t.cfg.is_video() { 0 } else { 0x0100 });
    b.u16(0);
    for m in MATRIX {
        b.u32(m);
    }
    let (w, h) = match &t.cfg.codec {
        Codec::H264 { width, height, .. } => (u32::from(*width) << 16, u32::from(*height) << 16),
        Codec::Aac { .. } => (0, 0),
    };
    b.u32(w);
    b.u32(h);
    b.end(o)
}

fn mdhd(b: &mut BoxBuf, duration: u64, timescale: u32, force_v1: bool) -> Result<(), MuxError> {
    let v1 = force_v1 || duration > u64::from(u32::MAX);
    let o = b.begin_full(b"mdhd", u8::from(v1), 0);
    if v1 {
        b.u64(0);
        b.u64(0);
        b.u32(timescale);
        b.u64(duration);
    } else {
        b.u32(0);
        b.u32(0);
        b.u32(timescale);
        b.u32(u32::try_from(duration).map_err(|_| MuxError::Overflow)?);
    }
    b.u16(0x55c4); // 'und'
    b.u16(0);
    b.end(o)
}

fn hdlr(b: &mut BoxBuf, video: bool) -> Result<(), MuxError> {
    let o = b.begin_full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(if video { b"vide" } else { b"soun" });
    b.zeros(12);
    b.bytes(if video {
        b"VideoHandler\0"
    } else {
        b"SoundHandler\0"
    });
    b.end(o)
}

fn stbl(b: &mut BoxBuf, t: &TrackTables<'_>, co64: bool) -> Result<(), MuxError> {
    let o = b.begin(b"stbl");
    stsd(b, t)?;
    stts(b, t.durations)?;
    if t.needs_stss() {
        let s = b.begin_full(b"stss", 0, 0);
        let n = t.sync.iter().filter(|&&s| s).count();
        b.u32(u32::try_from(n).map_err(|_| MuxError::Overflow)?);
        for (i, _) in t.sync.iter().enumerate().filter(|(_, s)| **s) {
            b.u32(u32::try_from(i + 1).map_err(|_| MuxError::Overflow)?);
        }
        b.end(s)?;
    }
    match t.ctts_kind() {
        Ctts::None => {}
        kind => {
            ctts(b, t.cts_offsets, kind == Ctts::V1)?;
            cslg(b, t)?;
        }
    }
    stsc(b, t.chunk_counts)?;
    stsz(b, t.sizes)?;
    if co64 {
        let c = b.begin_full(b"co64", 0, 0);
        b.u32(u32::try_from(t.chunk_offsets.len()).map_err(|_| MuxError::Overflow)?);
        for &off in t.chunk_offsets {
            b.u64(off);
        }
        b.end(c)?;
    } else {
        let c = b.begin_full(b"stco", 0, 0);
        b.u32(u32::try_from(t.chunk_offsets.len()).map_err(|_| MuxError::Overflow)?);
        for &off in t.chunk_offsets {
            b.u32(u32::try_from(off).map_err(|_| MuxError::Overflow)?);
        }
        b.end(c)?;
    }
    if !t.cfg.is_video() {
        roll_group(
            b,
            u32::try_from(t.sizes.len()).map_err(|_| MuxError::Overflow)?,
        )?;
    }
    b.end(o)
}

/// AAC's pre-roll as a `roll` sample group (ISO/IEC 14496-12 §10.1): every access unit
/// needs the one before it (roll distance −1), as ffmpeg writes it. AVFoundation reads it:
/// without it, it assumes its own 2112-sample priming on top of the track's timing.
fn roll_group(b: &mut BoxBuf, samples: u32) -> Result<(), MuxError> {
    let g = b.begin_full(b"sgpd", 1, 0);
    b.bytes(b"roll");
    b.u32(2); // default_length
    b.u32(1); // entry_count
    b.u16(0xffff); // roll_distance −1
    b.end(g)?;
    let m = b.begin_full(b"sbgp", 0, 0);
    b.bytes(b"roll");
    b.u32(1); // entry_count
    b.u32(samples);
    b.u32(1); // group_description_index
    b.end(m)
}

fn stsd(b: &mut BoxBuf, t: &TrackTables<'_>) -> Result<(), MuxError> {
    let o = b.begin_full(b"stsd", 0, 0);
    b.u32(1);
    match &t.cfg.codec {
        Codec::H264 {
            avcc,
            width,
            height,
            color,
        } => avc1(b, avcc, *width, *height, *color, t.rates)?,
        Codec::Aac {
            asc,
            channels,
            sample_rate,
        } => mp4a(b, t.id, asc, *channels, *sample_rate, t.rates)?,
    }
    b.end(o)
}

fn sample_entry_header(b: &mut BoxBuf) {
    b.zeros(6);
    b.u16(1); // data_reference_index
}

fn avc1(
    b: &mut BoxBuf,
    avcc: &[u8],
    width: u16,
    height: u16,
    color: Option<ColorTags>,
    rates: Rates,
) -> Result<(), MuxError> {
    let o = b.begin(b"avc1");
    sample_entry_header(b);
    b.zeros(16); // pre_defined, reserved, pre_defined[3]
    b.u16(width);
    b.u16(height);
    b.u32(0x0048_0000); // 72 dpi
    b.u32(0x0048_0000);
    b.u32(0);
    b.u16(1); // frame_count
    b.zeros(32); // compressorname (empty Pascal string)
    b.u16(0x0018); // depth
    b.u16(0xffff); // pre_defined = -1
    let c = b.begin(b"avcC");
    b.bytes(avcc);
    b.end(c)?;
    if let Some(col) = color {
        let c = b.begin(b"colr");
        b.bytes(b"nclx");
        b.u16(col.primaries);
        b.u16(col.transfer);
        b.u16(col.matrix);
        b.u8(if col.full_range { 0x80 } else { 0 });
        b.end(c)?;
    }
    let p = b.begin(b"pasp");
    b.u32(1);
    b.u32(1);
    b.end(p)?;
    let r = b.begin(b"btrt");
    b.u32(rates.buffer_size_db);
    b.u32(rates.max_bitrate);
    b.u32(rates.avg_bitrate);
    b.end(r)?;
    b.end(o)
}

fn mp4a(
    b: &mut BoxBuf,
    track_id: u32,
    asc: &[u8],
    channels: u16,
    sample_rate: u32,
    rates: Rates,
) -> Result<(), MuxError> {
    let o = b.begin(b"mp4a");
    sample_entry_header(b);
    b.zeros(8); // version 0 + reserved
    b.u16(channels);
    b.u16(16); // samplesize
    b.u16(0);
    b.u16(0);
    // 16.16 fixed point; rates ≥ 65536 do not fit and are written as 0 (the esds is authoritative).
    b.u32(if sample_rate < 65_536 {
        sample_rate << 16
    } else {
        0
    });
    let e = b.begin_full(b"esds", 0, 0);
    let dsi = asc.len();
    let dcd = 13 + 5 + dsi;
    let es = 3 + 5 + dcd + 5 + 1;
    b.u8(0x03);
    descriptor_len(b, es)?;
    b.u16(u16::try_from(track_id).map_err(|_| MuxError::Overflow)?);
    b.u8(0); // flags
    b.u8(0x04);
    descriptor_len(b, dcd)?;
    b.u8(0x40); // MPEG-4 Audio
    b.u8(0x15); // AudioStream << 2 | reserved 1
    b.u24(rates.buffer_size_db.min(0x00ff_ffff));
    b.u32(rates.max_bitrate);
    b.u32(rates.avg_bitrate);
    b.u8(0x05);
    descriptor_len(b, dsi)?;
    b.bytes(asc);
    b.u8(0x06);
    descriptor_len(b, 1)?;
    b.u8(0x02); // SLConfig predefined: MP4
    b.end(e)?;
    b.end(o)
}

fn stts(b: &mut BoxBuf, durations: &[u32]) -> Result<(), MuxError> {
    let runs = runs(durations.iter().copied());
    let o = b.begin_full(b"stts", 0, 0);
    b.u32(u32::try_from(runs.len()).map_err(|_| MuxError::Overflow)?);
    for (count, delta) in runs {
        b.u32(count);
        b.u32(delta);
    }
    b.end(o)
}

fn ctts(b: &mut BoxBuf, offsets: &[i64], v1: bool) -> Result<(), MuxError> {
    let runs = runs(offsets.iter().copied());
    let o = b.begin_full(b"ctts", u8::from(v1), 0);
    b.u32(u32::try_from(runs.len()).map_err(|_| MuxError::Overflow)?);
    for (count, off) in runs {
        b.u32(count);
        if v1 {
            b.i32(i32::try_from(off).map_err(|_| MuxError::Overflow)?);
        } else {
            b.u32(u32::try_from(off).map_err(|_| MuxError::Overflow)?);
        }
    }
    b.end(o)
}

/// `cslg` (ISO/IEC 14496-12 §8.6.1.4), version 0 when every field fits 32 bits.
fn cslg(b: &mut BoxBuf, t: &TrackTables<'_>) -> Result<(), MuxError> {
    let least = t.cts_offsets.iter().copied().min().unwrap_or(0);
    let greatest = t.cts_offsets.iter().copied().max().unwrap_or(0);
    let shift = least.min(0).checked_neg().ok_or(MuxError::Overflow)?;
    // ISO/IEC 14496-12 §8.6.1.4: start = smallest CTS; end = CTS + duration of the sample
    // with the largest CTS. CTS = file DTS + offset.
    let mut dts = 0i64;
    let mut start = i64::MAX;
    let mut last: Option<(i64, u32)> = None;
    for (d, o) in t.durations.iter().zip(t.cts_offsets) {
        let cts = dts.checked_add(*o).ok_or(MuxError::Overflow)?;
        start = start.min(cts);
        if last.is_none_or(|(c, _)| cts >= c) {
            last = Some((cts, *d));
        }
        dts = dts.checked_add(i64::from(*d)).ok_or(MuxError::Overflow)?;
    }
    let (start, end) = match last {
        Some((c, d)) => (
            start,
            c.checked_add(i64::from(d)).ok_or(MuxError::Overflow)?,
        ),
        None => (0, 0),
    };
    let fields = [shift, least, greatest, start, end];
    let fits = fields.iter().all(|v| i32::try_from(*v).is_ok());
    let o = b.begin_full(b"cslg", u8::from(!fits), 0);
    for v in fields {
        if fits {
            b.i32(i32::try_from(v).map_err(|_| MuxError::Overflow)?);
        } else {
            b.i64(v);
        }
    }
    b.end(o)
}

fn stsc(b: &mut BoxBuf, chunk_counts: &[u32]) -> Result<(), MuxError> {
    let o = b.begin_full(b"stsc", 0, 0);
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for (i, &n) in chunk_counts.iter().enumerate() {
        if entries.last().map(|e| e.1) != Some(n) {
            entries.push((u32::try_from(i + 1).map_err(|_| MuxError::Overflow)?, n));
        }
    }
    b.u32(u32::try_from(entries.len()).map_err(|_| MuxError::Overflow)?);
    for (first, n) in entries {
        b.u32(first);
        b.u32(n);
        b.u32(1);
    }
    b.end(o)
}

fn stsz(b: &mut BoxBuf, sizes: &[u32]) -> Result<(), MuxError> {
    let o = b.begin_full(b"stsz", 0, 0);
    let count = u32::try_from(sizes.len()).map_err(|_| MuxError::Overflow)?;
    match sizes.first() {
        // sample_size 0 means "a table follows", so an all-zero track keeps its table.
        Some(&first) if first != 0 && sizes.iter().all(|&s| s == first) => {
            b.u32(first);
            b.u32(count);
        }
        _ => {
            b.u32(0);
            b.u32(count);
            for &s in sizes {
                b.u32(s);
            }
        }
    }
    b.end(o)
}

/// Run-length encoding as (count, value).
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v && *n < u32::MAX => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// Bytes a track's tables add beyond an empty-table `moov`, at worst, for `n` samples
/// (every run length 1, every sample its own chunk, a full `stsz`, `co64`, `ctts`,
/// `cslg` v1 and an `stss` listing every sample).
pub(crate) fn worst_table_bytes(n: u64) -> Option<u64> {
    let stts = n.checked_mul(8)?;
    let ctts = n.checked_mul(8)?.checked_add(16)?;
    let cslg = 12 + 5 * 8;
    let stss = n.checked_mul(4)?.checked_add(16)?;
    let stsc = n.checked_mul(12)?;
    let stsz = n.checked_mul(4)?;
    let co64 = n.checked_mul(8)?;
    stts.checked_add(ctts)?
        .checked_add(cslg)?
        .checked_add(stss)?
        .checked_add(stsc)?
        .checked_add(stsz)?
        .checked_add(co64)
}
