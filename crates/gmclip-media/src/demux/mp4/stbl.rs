//! Sample-table expansion (`stsz`/`stz2`, `stts`, `ctts`, `stss`, `stsc`, `stco`/`co64`)
//! into per-sample decode timestamps, composition offsets, file offsets, sizes, sync
//! flags and durations.
//!
//! Uncompressed PCM whose `stts` counts single frames is grouped the way it is stored:
//! one packet per run of at most 1024 frames inside a chunk, so a 50 s 48 kHz window is
//! 2 345 packets rather than 2.4 M entries.

use super::super::io::{Cursor, vec_with_capacity};
use super::super::{DemuxError, MAX_SAMPLES, Result};
use super::boxes::Node;

/// The expanded table, decode order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Samples {
    pub dts: Vec<i64>,
    pub cts: Vec<i32>,
    pub offset: Vec<u64>,
    pub size: Vec<u32>,
    pub sync: Vec<bool>,
    pub dur: Vec<u32>,
}

impl Samples {
    pub fn len(&self) -> usize {
        self.dts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.dts.is_empty()
    }
}

/// Most frames one PCM packet groups.
pub const PCM_PACKET_FRAMES: u32 = 1024;

fn table<'a>(stbl: &'a Node<'a>, kind: &[u8; 4]) -> Option<&'a Node<'a>> {
    stbl.child(kind)
}

/// `n` fixed-size entries must fit in what remains of the box.
fn entries(c: &Cursor<'_>, n: u32, each: usize, what: &str) -> Result<usize> {
    let n = n as usize;
    if n.checked_mul(each).is_none_or(|b| b > c.remaining()) {
        return Err(DemuxError::invalid(
            format!(
                "{what} claims {n} entries, only {} bytes remain",
                c.remaining()
            ),
            c.offset(),
        ));
    }
    Ok(n)
}

enum Sizes {
    Constant(u32),
    Each(Vec<u32>),
}

fn sizes(stbl: &Node<'_>) -> Result<(Sizes, u64)> {
    if let Some(stsz) = table(stbl, b"stsz") {
        let mut c = stsz.cursor();
        c.skip(4)?;
        let constant = c.u32()?;
        let count = c.u32()?;
        if constant != 0 {
            return Ok((Sizes::Constant(constant), u64::from(count)));
        }
        if u64::from(count) > MAX_SAMPLES {
            return Err(DemuxError::budget(
                format!("stsz has {count} entries (cap 2^26)"),
                stsz.offset,
            ));
        }
        let n = entries(&c, count, 4, "stsz")?;
        let mut v = vec_with_capacity(n, stsz.offset)?;
        for _ in 0..n {
            v.push(c.u32()?);
        }
        return Ok((Sizes::Each(v), u64::from(count)));
    }
    if let Some(stz2) = table(stbl, b"stz2") {
        let mut c = stz2.cursor();
        c.skip(4 + 3)?;
        let field = c.u8()?;
        let count = c.u32()?;
        if u64::from(count) > MAX_SAMPLES {
            return Err(DemuxError::budget(
                format!("stz2 has {count} entries (cap 2^26)"),
                stz2.offset,
            ));
        }
        let bits = match field {
            4 | 8 | 16 => usize::from(field),
            f => {
                return Err(DemuxError::invalid(
                    format!("stz2 field size {f}"),
                    stz2.offset,
                ));
            }
        };
        let bytes = (count as usize).checked_mul(bits).map(|b| b.div_ceil(8));
        if bytes.is_none_or(|b| b > c.remaining()) {
            return Err(DemuxError::invalid(
                "stz2 entries run past the box",
                stz2.offset,
            ));
        }
        let n = count as usize;
        let mut v = vec_with_capacity(n, stz2.offset)?;
        let mut i = 0;
        while i < n {
            match bits {
                4 => {
                    let b = c.u8()?;
                    v.push(u32::from(b >> 4));
                    if i + 1 < n {
                        v.push(u32::from(b & 0xF));
                    }
                    i += 2;
                }
                8 => {
                    v.push(u32::from(c.u8()?));
                    i += 1;
                }
                _ => {
                    v.push(u32::from(c.u16()?));
                    i += 1;
                }
            }
        }
        return Ok((Sizes::Each(v), u64::from(count)));
    }
    Err(DemuxError::invalid("stbl has no stsz/stz2", stbl.offset))
}

/// `(count, value)` runs of a two-field table.
fn runs(node: &Node<'_>, what: &str) -> Result<Vec<(u32, u32)>> {
    let mut c = node.cursor();
    c.skip(4)?;
    let count = c.u32()?;
    let n = entries(&c, count, 8, what)?;
    let mut v = vec_with_capacity(n, node.offset)?;
    for _ in 0..n {
        v.push((c.u32()?, c.u32()?));
    }
    Ok(v)
}

fn chunk_offsets(stbl: &Node<'_>) -> Result<Vec<u64>> {
    if let Some(stco) = table(stbl, b"stco") {
        let mut c = stco.cursor();
        c.skip(4)?;
        let count = c.u32()?;
        let n = entries(&c, count, 4, "stco")?;
        let mut v = vec_with_capacity(n, stco.offset)?;
        for _ in 0..n {
            v.push(u64::from(c.u32()?));
        }
        return Ok(v);
    }
    if let Some(co64) = table(stbl, b"co64") {
        let mut c = co64.cursor();
        c.skip(4)?;
        let count = c.u32()?;
        let n = entries(&c, count, 8, "co64")?;
        let mut v = vec_with_capacity(n, co64.offset)?;
        for _ in 0..n {
            v.push(c.u64()?);
        }
        return Ok(v);
    }
    Err(DemuxError::invalid("stbl has no stco/co64", stbl.offset))
}

struct Stsc {
    first_chunk: u32,
    per_chunk: u32,
    description: u32,
}

fn stsc(stbl: &Node<'_>) -> Result<Vec<Stsc>> {
    let node =
        table(stbl, b"stsc").ok_or_else(|| DemuxError::invalid("stbl has no stsc", stbl.offset))?;
    let mut c = node.cursor();
    c.skip(4)?;
    let count = c.u32()?;
    let n = entries(&c, count, 12, "stsc")?;
    let mut v: Vec<Stsc> = vec_with_capacity(n, node.offset)?;
    for _ in 0..n {
        let e = Stsc {
            first_chunk: c.u32()?,
            per_chunk: c.u32()?,
            description: c.u32()?,
        };
        let ordered = v.last().is_none_or(|p| e.first_chunk > p.first_chunk);
        if e.first_chunk == 0 || !ordered {
            return Err(DemuxError::invalid(
                "stsc first_chunk not increasing from 1",
                node.offset,
            ));
        }
        if e.description != 1 {
            return Err(DemuxError::unsupported(
                "more than one sample description in a track",
            ));
        }
        v.push(e);
    }
    Ok(v)
}

/// Walk chunks, calling `f(chunk_offset, samples_in_chunk)` until `want` samples are
/// placed. Errors if the chunks hold fewer.
fn for_chunks(stbl: &Node<'_>, want: u64, mut f: impl FnMut(u64, u32) -> Result<()>) -> Result<()> {
    let offsets = chunk_offsets(stbl)?;
    let map = stsc(stbl)?;
    let mut placed = 0u64;
    let mut m = 0usize;
    for (ci, &off) in offsets.iter().enumerate() {
        if placed >= want {
            break;
        }
        let chunk_no =
            u32::try_from(ci + 1).map_err(|_| DemuxError::invalid("chunk count", stbl.offset))?;
        while map.get(m + 1).is_some_and(|n| n.first_chunk <= chunk_no) {
            m += 1;
        }
        let Some(entry) = map.get(m) else {
            return Err(DemuxError::invalid("stsc is empty", stbl.offset));
        };
        if entry.first_chunk > chunk_no {
            return Err(DemuxError::invalid(
                "stsc does not start at chunk 1",
                stbl.offset,
            ));
        }
        let n = u64::from(entry.per_chunk).min(want - placed);
        let n32 =
            u32::try_from(n).map_err(|_| DemuxError::invalid("chunk too large", stbl.offset))?;
        f(off, n32)?;
        placed += n;
    }
    if placed < want {
        return Err(DemuxError::invalid(
            format!("chunks hold {placed} samples, stsz has {want}"),
            stbl.offset,
        ));
    }
    Ok(())
}

/// Expand a track's tables. `pcm_frame_bytes` is set for uncompressed audio. `budget`
/// is the number of entries all tracks together may still create (bounded by the file
/// size), decremented here.
pub fn expand(stbl: &Node<'_>, pcm_frame_bytes: Option<u32>, budget: &mut u64) -> Result<Samples> {
    let (sizes, count) = sizes(stbl)?;
    let stts_node =
        table(stbl, b"stts").ok_or_else(|| DemuxError::invalid("stbl has no stts", stbl.offset))?;
    let stts = runs(stts_node, "stts")?;
    let stts_total: u64 = stts.iter().map(|&(n, _)| u64::from(n)).sum();
    if stts_total != count {
        return Err(DemuxError::invalid(
            format!("stts covers {stts_total} samples, stsz has {count}"),
            stts_node.offset,
        ));
    }
    if let (Some(frame), Sizes::Constant(_)) = (pcm_frame_bytes, &sizes)
        && stts.iter().all(|&(n, d)| d == 1 || n == 0)
    {
        return pcm_packets(stbl, count, frame, budget);
    }
    if count > MAX_SAMPLES {
        return Err(DemuxError::budget(
            format!("{count} samples in one track (cap 2^26)"),
            stbl.offset,
        ));
    }
    *budget = budget
        .checked_sub(count)
        .ok_or_else(|| DemuxError::budget("more samples than the file has bytes", stbl.offset))?;
    let n = usize::try_from(count).map_err(|_| DemuxError::budget("sample count", stbl.offset))?;
    let mut s = Samples {
        dts: vec_with_capacity(n, stbl.offset)?,
        cts: vec_with_capacity(n, stbl.offset)?,
        offset: vec_with_capacity(n, stbl.offset)?,
        size: vec_with_capacity(n, stbl.offset)?,
        sync: vec_with_capacity(n, stbl.offset)?,
        dur: vec_with_capacity(n, stbl.offset)?,
    };
    // offsets and sizes
    let size_of = |i: usize| -> u32 {
        match &sizes {
            Sizes::Constant(c) => *c,
            Sizes::Each(v) => v.get(i).copied().unwrap_or(0),
        }
    };
    for_chunks(stbl, count, |off, k| {
        let mut pos = off;
        for _ in 0..k {
            let size = size_of(s.offset.len());
            s.offset.push(pos);
            s.size.push(size);
            pos = pos
                .checked_add(u64::from(size))
                .ok_or_else(|| DemuxError::invalid("offset overflow", off))?;
        }
        Ok(())
    })?;
    // decode times and durations
    let mut t: i64 = 0;
    for &(k, d) in &stts {
        for _ in 0..k {
            s.dts.push(t);
            s.dur.push(d);
            t = t
                .checked_add(i64::from(d))
                .ok_or_else(|| DemuxError::invalid("dts overflow", stts_node.offset))?;
        }
    }
    // composition offsets (read signed whatever the version, as players do)
    if let Some(ctts) = table(stbl, b"ctts") {
        for (k, v) in runs(ctts, "ctts")? {
            for _ in 0..k {
                if s.cts.len() == n {
                    break;
                }
                s.cts.push(v as i32);
            }
        }
    }
    s.cts.resize(n, 0);
    // A negative composition offset moves every dts earlier by its magnitude, so
    // dts <= pts holds; the offsets grow by the same amount and pts are unchanged.
    let min_cts = s.cts.iter().copied().min().unwrap_or(0);
    if min_cts < 0 {
        for d in &mut s.dts {
            *d = d
                .checked_add(i64::from(min_cts))
                .ok_or_else(|| DemuxError::invalid("dts underflow", stbl.offset))?;
        }
        for c in &mut s.cts {
            *c = c
                .checked_sub(min_cts)
                .ok_or_else(|| DemuxError::invalid("composition offset overflows", stbl.offset))?;
        }
    }
    // sync samples
    match table(stbl, b"stss") {
        None => s.sync.resize(n, true),
        Some(stss) => {
            s.sync.resize(n, false);
            let mut c = stss.cursor();
            c.skip(4)?;
            let k = c.u32()?;
            let k = entries(&c, k, 4, "stss")?;
            for _ in 0..k {
                let i = c.u32()?;
                match i.checked_sub(1).and_then(|i| s.sync.get_mut(i as usize)) {
                    Some(flag) => *flag = true,
                    None => {
                        return Err(DemuxError::invalid(
                            format!("stss names sample {i} of {n}"),
                            stss.offset,
                        ));
                    }
                }
            }
        }
    }
    Ok(s)
}

fn pcm_packets(
    stbl: &Node<'_>,
    frames: u64,
    frame_bytes: u32,
    budget: &mut u64,
) -> Result<Samples> {
    let max_packets = frames
        .div_ceil(u64::from(PCM_PACKET_FRAMES))
        .saturating_add(1 << 20);
    // Reserve for the packets the frame count implies, bounded by the byte budget.
    let want = usize::try_from(
        frames
            .div_ceil(u64::from(PCM_PACKET_FRAMES))
            .min(*budget)
            .min(MAX_SAMPLES),
    )
    .map_err(|_| DemuxError::budget("PCM packets", stbl.offset))?;
    let mut s = Samples {
        dts: vec_with_capacity(want, stbl.offset)?,
        cts: vec_with_capacity(want, stbl.offset)?,
        offset: vec_with_capacity(want, stbl.offset)?,
        size: vec_with_capacity(want, stbl.offset)?,
        sync: vec_with_capacity(want, stbl.offset)?,
        dur: vec_with_capacity(want, stbl.offset)?,
    };
    let mut t: i64 = 0;
    let mut made = 0u64;
    for_chunks(stbl, frames, |off, mut k| {
        let mut pos = off;
        while k > 0 {
            let n = k.min(PCM_PACKET_FRAMES);
            made += 1;
            if made > MAX_SAMPLES || made > max_packets {
                return Err(DemuxError::budget("too many PCM packets", off));
            }
            *budget = budget
                .checked_sub(1)
                .ok_or_else(|| DemuxError::budget("more packets than bytes", off))?;
            let size = n
                .checked_mul(frame_bytes)
                .ok_or_else(|| DemuxError::invalid("PCM packet size", off))?;
            s.dts.push(t);
            s.cts.push(0);
            s.offset.push(pos);
            s.size.push(size);
            s.sync.push(true);
            s.dur.push(n);
            t += i64::from(n);
            pos = pos
                .checked_add(u64::from(size))
                .ok_or_else(|| DemuxError::invalid("offset overflow", off))?;
            k -= n;
        }
        Ok(())
    })?;
    Ok(s)
}
