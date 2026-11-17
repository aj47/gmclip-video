//! Byte builders for synthetic MP4 and Matroska files (unit tests, bomb fixtures and
//! fuzz seeds). Plain construction, no validation: tests build broken files on purpose.
#![allow(dead_code)]

pub fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

pub fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version, 0, 0, 0];
    b.extend_from_slice(body);
    bx(kind, &b)
}

fn be32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// A track for [`mp4`].
#[derive(Clone)]
pub struct Trak {
    pub video: bool,
    pub timescale: u32,
    pub sizes: Vec<u32>,
    pub deltas: Vec<u32>,
    pub cts: Option<Vec<i32>>,
    pub sync: Option<Vec<u32>>,
    pub edits: Option<Vec<(u32, i32)>>,
    pub per_chunk: u32,
    /// Write `stsz` with one constant size (all `sizes` equal).
    pub constant: bool,
    /// A version-1 `elst` (64-bit fields), instead of `edits`.
    pub edits64: Option<Vec<(u64, i64)>>,
    /// Samples per chunk, chunk by chunk (overrides `per_chunk`; `stsc` is its runs).
    pub chunk_pattern: Option<Vec<u32>>,
    /// Write `co64` instead of `stco`.
    pub co64: bool,
    /// Write `stz2` with this field size (4, 8 or 16) instead of `stsz`.
    pub stz2: Option<u8>,
}

impl Trak {
    /// Samples per chunk, chunk by chunk.
    pub fn chunks(&self) -> Vec<u32> {
        match &self.chunk_pattern {
            Some(p) => p.clone(),
            None => {
                let per = self.per_chunk.max(1);
                let n = self.sizes.len() as u32;
                (0..n.div_ceil(per)).map(|k| per.min(n - k * per)).collect()
            }
        }
    }
}

impl Trak {
    pub fn video(n: usize) -> Self {
        Trak {
            video: true,
            timescale: 15360,
            sizes: (0..n).map(|i| 100 + i as u32).collect(),
            deltas: vec![512; n],
            cts: None,
            sync: Some(vec![1]),
            edits: None,
            per_chunk: 1,
            constant: false,
            edits64: None,
            chunk_pattern: None,
            co64: false,
            stz2: None,
        }
    }
}

fn avc1() -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 16]);
    e.extend_from_slice(&64u16.to_be_bytes());
    e.extend_from_slice(&48u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 50]);
    e.extend(bx(b"avcC", &[1, 0x64, 0, 0x28, 0xFF, 0xE0, 0]));
    bx(b"avc1", &e)
}

fn sowt() -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 8]);
    e.extend_from_slice(&2u16.to_be_bytes()); // channels
    e.extend_from_slice(&16u16.to_be_bytes()); // bits
    e.extend_from_slice(&[0u8; 4]);
    e.extend_from_slice(&(48000u32 << 16).to_be_bytes());
    bx(b"sowt", &e)
}

fn trak(id: u32, t: &Trak, offsets: &[u32], movie_ts: u32) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 72]);
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&t.timescale.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 8]);
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(if t.video { b"vide" } else { b"soun" });
    hdlr.extend_from_slice(&[0u8; 13]);
    let entry = if t.video { avc1() } else { sowt() };
    let mut stsd = be32(&[1]);
    stsd.extend(entry);
    let mut stts = be32(&[t.deltas.len() as u32]);
    for d in &t.deltas {
        stts.extend(be32(&[1, *d]));
    }
    let n = t.sizes.len() as u32;
    let mut stsz = if t.constant {
        be32(&[t.sizes.first().copied().unwrap_or(0), n])
    } else {
        be32(&[0, n])
    };
    if !t.constant {
        stsz.extend(be32(&t.sizes));
    }
    let chunks = offsets.len() as u32;
    // stsc: runs of equal samples-per-chunk (the last chunk may be short, as stsc
    // allows: only the first n samples are placed).
    let pattern = t.chunks();
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (i, &k) in pattern.iter().enumerate() {
        if t.chunk_pattern.is_none() && i + 1 == pattern.len() && !runs.is_empty() {
            break;
        }
        if runs.last().is_none_or(|r| r.1 != k) {
            runs.push((i as u32 + 1, k));
        }
    }
    let mut stsc = be32(&[runs.len() as u32]);
    for (f, k) in &runs {
        stsc.extend(be32(&[*f, *k, 1]));
    }
    let mut stco = be32(&[chunks]);
    if t.co64 {
        for o in offsets {
            stco.extend_from_slice(&(u64::from(*o)).to_be_bytes());
        }
    } else {
        stco.extend(be32(offsets));
    }
    let mut stbl = full(b"stsd", 0, &stsd);
    stbl.extend(full(b"stts", 0, &stts));
    if let Some(c) = &t.cts {
        let mut b = be32(&[c.len() as u32]);
        for v in c {
            b.extend(be32(&[1, *v as u32]));
        }
        stbl.extend(full(b"ctts", 0, &b));
    }
    if let Some(s) = &t.sync {
        let mut b = be32(&[s.len() as u32]);
        b.extend(be32(s));
        stbl.extend(full(b"stss", 0, &b));
    }
    stbl.extend(full(b"stsc", 0, &stsc));
    match t.stz2 {
        None => stbl.extend(full(b"stsz", 0, &stsz)),
        Some(field) => {
            let mut b = vec![0, 0, 0, field];
            b.extend(be32(&[n]));
            match field {
                4 => {
                    for pair in t.sizes.chunks(2) {
                        let hi = pair[0] as u8;
                        let lo = pair.get(1).copied().unwrap_or(0) as u8;
                        b.push((hi << 4) | (lo & 0xF));
                    }
                }
                8 => b.extend(t.sizes.iter().map(|&x| x as u8)),
                _ => {
                    for x in &t.sizes {
                        b.extend_from_slice(&(*x as u16).to_be_bytes());
                    }
                }
            }
            stbl.extend(full(b"stz2", 0, &b));
        }
    }
    stbl.extend(full(if t.co64 { b"co64" } else { b"stco" }, 0, &stco));
    let minf = bx(b"stbl", &stbl);
    let mdia = [
        full(b"mdhd", 0, &mdhd),
        full(b"hdlr", 0, &hdlr),
        bx(b"minf", &minf),
    ]
    .concat();
    let mut body = full(b"tkhd", 0, &tkhd);
    if let Some(e) = &t.edits {
        let mut b = be32(&[e.len() as u32]);
        for (d, m) in e {
            b.extend(be32(&[*d, *m as u32, 0x0001_0000]));
        }
        body.extend(bx(b"edts", &full(b"elst", 0, &b)));
    }
    if let Some(e) = &t.edits64 {
        let mut b = be32(&[e.len() as u32]);
        for (d, m) in e {
            b.extend_from_slice(&d.to_be_bytes());
            b.extend_from_slice(&m.to_be_bytes());
            b.extend(be32(&[0x0001_0000]));
        }
        body.extend(bx(b"edts", &full(b"elst", 1, &b)));
    }
    let _ = movie_ts;
    body.extend(bx(b"mdia", &mdia));
    bx(b"trak", &body)
}

/// ftyp + mdat (zero bytes for every sample) + moov.
pub fn mp4(tracks: &[Trak]) -> Vec<u8> {
    let mut out = bx(b"ftyp", b"isom\0\0\0\0isom");
    let mut mdat_body = Vec::new();
    let mut offs: Vec<Vec<u32>> = Vec::new();
    let mdat_start = out.len() as u32 + 8;
    for t in tracks {
        let mut o = Vec::new();
        let mut i = 0usize;
        for k in t.chunks() {
            o.push(mdat_start + mdat_body.len() as u32);
            for s in t.sizes.iter().skip(i).take(k as usize) {
                mdat_body.extend(std::iter::repeat_n(0u8, *s as usize));
            }
            i += k as usize;
        }
        offs.push(o);
    }
    out.extend(bx(b"mdat", &mdat_body));
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 84]);
    let mut moov = full(b"mvhd", 0, &mvhd);
    for (i, t) in tracks.iter().enumerate() {
        moov.extend(trak(i as u32 + 1, t, &offs[i], 1000));
    }
    out.extend(bx(b"moov", &moov));
    out
}

// ---------- Matroska ----------

/// EBML element with an 8-byte size.
pub fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = id
        .to_be_bytes()
        .iter()
        .copied()
        .skip_while(|&b| b == 0)
        .collect();
    v.push(0x01);
    v.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    v.extend_from_slice(body);
    v
}

pub fn uint_el(id: u32, v: u64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

pub fn str_el(id: u32, s: &str) -> Vec<u8> {
    el(id, s.as_bytes())
}

/// SimpleBlock body: track 1, relative timecode, flags, then `payload`.
pub fn simple_block(rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x81];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(payload);
    el(0xA3, &b)
}

/// EBML header + Segment(Info, Tracks(one VP9 track), clusters, optional Cues).
pub fn mkv(clusters: &[Vec<u8>], cues: bool) -> Vec<u8> {
    let header = el(
        0x1A45_DFA3,
        &[str_el(0x4282, "webm"), uint_el(0x4285, 2)].concat(),
    );
    let info = el(0x1549_A966, &uint_el(0x2A_D7B1, 1_000_000));
    let entry = el(
        0xAE,
        &[
            uint_el(0xD7, 1),
            uint_el(0x83, 1),
            str_el(0x86, "V_VP9"),
            el(0xE0, &[uint_el(0xB0, 64), uint_el(0xBA, 48)].concat()),
        ]
        .concat(),
    );
    let tracks = el(0x1654_AE6B, &entry);
    let mut seg = [info, tracks].concat();
    let first_cluster = seg.len() as u64;
    for c in clusters {
        seg.extend_from_slice(c);
    }
    if cues {
        let pos = el(
            0xB7,
            &[uint_el(0xF7, 1), uint_el(0xF1, first_cluster)].concat(),
        );
        seg.extend(el(
            0x1C53_BB6B,
            &el(0xBB, &[uint_el(0xB3, 0), pos].concat()),
        ));
    }
    [header, el(0x1853_8067, &seg)].concat()
}

pub fn cluster(ts: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut b = uint_el(0xE7, ts);
    for x in blocks {
        b.extend_from_slice(x);
    }
    el(0x1F43_B675, &b)
}
