//! Roto mattes (SPEC §5.8, decision 0013): mask tracks, their codec and cache, and the
//! per-frame finish (soften, brush corrections) that preview and export share.
//!
//! A **track** is one 8-bit mask per frame of a part span, at the crop's integer source
//! bounds (the pixels the compositor samples), temporally smoothed when it is built.
//! It is keyed by `(source fingerprint, span, crop, model)` ([`track_key`]) and stored
//! compressed ([`codec`], PackBits-style RLE: no new crate) in a [`store::MatteStore`]
//! with a size cap and LRU eviction. `soften` and the brush strokes are applied when a
//! frame is drawn ([`finish`]), so changing them never re-runs segmentation, and preview
//! and export draw the same bytes from the same track (R-COMP-1).
//!
//! Everything here is integer or fixed-order f64 arithmetic on the CPU: deterministic
//! across runs and machines.

pub mod store;

use gmclip_plan::types::{MatteStroke, StrokeMode};

/// An 8-bit single-channel mask, rows of `width` bytes (255 = keep).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn filled(width: u32, height: u32, v: u8) -> Self {
        Self {
            width,
            height,
            data: vec![v; width as usize * height as usize],
        }
    }

    pub fn at(&self, x: u32, y: u32) -> u8 {
        self.data[y as usize * self.width as usize + x as usize]
    }

    /// Bilinear resample to `w × h` (pixel centres aligned, edges clamped), in 16.16
    /// fixed point so every machine gets the same bytes.
    pub fn resize(&self, w: u32, h: u32) -> Mask {
        if (w, h) == (self.width, self.height) {
            return self.clone();
        }
        let (sw, sh) = (self.width.max(1) as i64, self.height.max(1) as i64);
        let mut out = Mask::filled(w, h, 0);
        if self.data.is_empty() || w == 0 || h == 0 {
            return out;
        }
        // source coordinate of destination centre i: (i + ½)·s/d − ½, in 16.16
        let coord = |i: u32, s: i64, d: u32| -> (i64, i64, i64) {
            let num = (2 * i64::from(i) + 1) * s * 65_536 / (2 * i64::from(d)) - 32_768;
            let num = num.clamp(0, (s - 1) * 65_536);
            let i0 = num >> 16;
            let f = num & 0xFFFF;
            (i0, (i0 + 1).min(s - 1), f)
        };
        let xs: Vec<(i64, i64, i64)> = (0..w).map(|x| coord(x, sw, w)).collect();
        for y in 0..h {
            let (y0, y1, fy) = coord(y, sh, h);
            let r0 = &self.data[(y0 * sw) as usize..((y0 + 1) * sw) as usize];
            let r1 = &self.data[(y1 * sw) as usize..((y1 + 1) * sw) as usize];
            for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
                let a =
                    i64::from(r0[x0 as usize]) * (65_536 - fx) + i64::from(r0[x1 as usize]) * fx;
                let b =
                    i64::from(r1[x0 as usize]) * (65_536 - fx) + i64::from(r1[x1 as usize]) * fx;
                let v = (a * (65_536 - fy) + b * fy + (1 << 31)) >> 32;
                out.data[y as usize * w as usize + x] = v.clamp(0, 255) as u8;
            }
        }
        out
    }

    /// Fraction of pixels ≥ 128.
    pub fn coverage(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().filter(|&&v| v >= 128).count() as f64 / self.data.len() as f64
    }
}

/// PackBits-style run-length coding of mask bytes: a control byte `c < 128` is followed
/// by `c + 1` literal bytes; `c ≥ 128` repeats the next byte `c − 125` times (3..=130).
pub mod codec {
    /// Encodes `data`.
    pub fn encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() / 16 + 16);
        let mut lit: Vec<u8> = Vec::with_capacity(128);
        let flush = |lit: &mut Vec<u8>, out: &mut Vec<u8>| {
            for chunk in lit.chunks(128) {
                out.push((chunk.len() - 1) as u8);
                out.extend_from_slice(chunk);
            }
            lit.clear();
        };
        let mut i = 0;
        while i < data.len() {
            let v = data[i];
            let mut run = 1;
            while i + run < data.len() && data[i + run] == v && run < 130 {
                run += 1;
            }
            if run >= 3 {
                flush(&mut lit, &mut out);
                out.push((run + 125) as u8);
                out.push(v);
                i += run;
            } else {
                lit.push(v);
                i += 1;
                if lit.len() == 128 {
                    flush(&mut lit, &mut out);
                }
            }
        }
        flush(&mut lit, &mut out);
        out
    }

    /// Decodes exactly `n` bytes; `None` when `enc` is malformed or the wrong length.
    pub fn decode(enc: &[u8], n: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        while i < enc.len() {
            let c = enc[i];
            i += 1;
            if c < 128 {
                let len = usize::from(c) + 1;
                out.extend_from_slice(enc.get(i..i + len)?);
                i += len;
            } else {
                let v = *enc.get(i)?;
                i += 1;
                out.extend(std::iter::repeat_n(v, usize::from(c) - 125));
            }
            if out.len() > n {
                return None;
            }
        }
        (out.len() == n).then_some(out)
    }
}

/// Temporal smoothing of a track: each frame becomes `(prev + 2·cur + next + 2) / 4`,
/// the span's ends repeating themselves (a single-frame track is unchanged).
pub fn smooth_temporal(frames: &[Mask]) -> Vec<Mask> {
    let n = frames.len();
    (0..n)
        .map(|i| {
            let (p, c, q) = (
                &frames[i.saturating_sub(1)],
                &frames[i],
                &frames[(i + 1).min(n - 1)],
            );
            if p.data.len() != c.data.len() || q.data.len() != c.data.len() {
                return c.clone();
            }
            let data = c
                .data
                .iter()
                .zip(&p.data)
                .zip(&q.data)
                .map(|((&c, &p), &q)| {
                    ((u16::from(p) + 2 * u16::from(c) + u16::from(q) + 2) / 4) as u8
                })
                .collect();
            Mask {
                width: c.width,
                height: c.height,
                data,
            }
        })
        .collect()
}

fn box_pass(
    src: &[u8],
    dst: &mut [u8],
    len: usize,
    stride: usize,
    count: usize,
    step: usize,
    r: usize,
) {
    // `count` lines of `len` samples; sample k of line j at j·step + k·stride
    let n = (2 * r + 1) as u32;
    for j in 0..count {
        let at = |k: isize| -> u32 {
            let k = k.clamp(0, len as isize - 1) as usize;
            u32::from(src[j * step + k * stride])
        };
        let mut sum: u32 = (-(r as isize)..=r as isize).map(at).sum();
        for k in 0..len {
            dst[j * step + k * stride] = ((sum + n / 2) / n) as u8;
            sum += at(k as isize + r as isize + 1);
            sum -= at(k as isize - r as isize);
        }
    }
}

/// Softens a mask's edges: two separable box passes of radius `r` (≈ a Gaussian of
/// σ ≈ r·0.8), clamped at the borders, integer-rounded.
pub fn soften(m: &mut Mask, r: u32) {
    if r == 0 || m.data.is_empty() {
        return;
    }
    let (w, h) = (m.width as usize, m.height as usize);
    let r = r as usize;
    let mut tmp = vec![0u8; m.data.len()];
    for _ in 0..2 {
        box_pass(&m.data, &mut tmp, w, 1, h, w, r);
        box_pass(&tmp, &mut m.data, h, w, w, 1, r);
    }
}

/// Where a mask lies in source pixels: its top-left and the source pixels per mask
/// pixel.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MaskPlacement {
    pub x: f64,
    pub y: f64,
    /// Source pixels per mask pixel, x and y.
    pub sx: f64,
    pub sy: f64,
}

fn seg_dist(px: f64, py: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (a.0 + t * dx, a.1 + t * dy);
    ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
}

/// Paints brush strokes (source-pixel polylines) onto a mask placed at `at`.
pub fn apply_strokes(m: &mut Mask, at: MaskPlacement, strokes: &[MatteStroke]) {
    let (w, h) = (m.width as i64, m.height as i64);
    for s in strokes {
        if !(s.radius.is_finite() && s.radius > 0.0) || s.points.is_empty() {
            continue;
        }
        let r = s.radius;
        let hard = r * (1.0 - s.feather.unwrap_or(0.0).clamp(0.0, 1.0));
        let pts: Vec<(f64, f64)> = s.points.iter().map(|p| (p.x, p.y)).collect();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in &pts {
            x0 = x0.min(x - r);
            y0 = y0.min(y - r);
            x1 = x1.max(x + r);
            y1 = y1.max(y + r);
        }
        // mask pixel range covering the stroke's box
        let i0 = (((x0 - at.x) / at.sx).floor() as i64).clamp(0, w);
        let i1 = (((x1 - at.x) / at.sx).ceil() as i64).clamp(0, w);
        let j0 = (((y0 - at.y) / at.sy).floor() as i64).clamp(0, h);
        let j1 = (((y1 - at.y) / at.sy).ceil() as i64).clamp(0, h);
        for j in j0..j1 {
            let py = at.y + (j as f64 + 0.5) * at.sy;
            for i in i0..i1 {
                let px = at.x + (i as f64 + 0.5) * at.sx;
                let d = if pts.len() == 1 {
                    seg_dist(px, py, pts[0], pts[0])
                } else {
                    pts.windows(2)
                        .map(|ab| seg_dist(px, py, ab[0], ab[1]))
                        .fold(f64::MAX, f64::min)
                };
                let b = if d <= hard {
                    1.0
                } else if d >= r {
                    0.0
                } else {
                    (r - d) / (r - hard)
                };
                if b <= 0.0 {
                    continue;
                }
                let v = (b * 255.0).round() as u8;
                let o = (j * w + i) as usize;
                m.data[o] = match s.mode {
                    StrokeMode::Add => m.data[o].max(v),
                    StrokeMode::Subtract => m.data[o].min(255 - v),
                };
            }
        }
    }
}

/// A track frame made ready to draw: softened by `soften` source pixels (converted to
/// mask pixels), then the strokes painted on top.
pub fn finish(raw: &Mask, at: MaskPlacement, soften_px: f64, strokes: &[MatteStroke]) -> Mask {
    let mut m = raw.clone();
    let r = (soften_px.clamp(0.0, 32.0) / at.sx.max(1e-6)).round() as u32;
    soften(&mut m, r);
    apply_strokes(&mut m, at, strokes);
    m
}

/// A mask track: one coded mask per frame of a span, all `width × height`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub width: u32,
    pub height: u32,
    /// The segmentation that made it (`apple_vision.accurate`, …).
    pub model: String,
    /// [`codec`]-encoded frames.
    pub frames: Vec<Vec<u8>>,
}

const MAGIC: &[u8; 6] = b"GMSK1\n";

impl Track {
    /// Smooths and encodes raw masks (all the same size).
    pub fn from_masks(model: &str, masks: &[Mask]) -> Option<Self> {
        let first = masks.first()?;
        if masks
            .iter()
            .any(|m| (m.width, m.height) != (first.width, first.height))
        {
            return None;
        }
        let smooth = smooth_temporal(masks);
        Some(Self {
            width: first.width,
            height: first.height,
            model: model.to_owned(),
            frames: smooth.iter().map(|m| codec::encode(&m.data)).collect(),
        })
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frame `i` decoded (`None` past the end or when corrupt).
    pub fn frame(&self, i: usize) -> Option<Mask> {
        let n = self.width as usize * self.height as usize;
        let data = codec::decode(self.frames.get(i)?, n)?;
        Some(Mask {
            width: self.width,
            height: self.height,
            data,
        })
    }

    /// Compressed size in bytes.
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(Vec::len).sum()
    }

    /// `GMSK1\n`, w, h, n (u32 LE), model length (u8) + model, n frame lengths (u32 LE),
    /// then the frames.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes() + 64 + 4 * self.frames.len());
        out.extend_from_slice(MAGIC);
        for v in [self.width, self.height, self.frames.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let model = &self.model.as_bytes()[..self.model.len().min(255)];
        out.push(model.len() as u8);
        out.extend_from_slice(model);
        for f in &self.frames {
            out.extend_from_slice(&(f.len() as u32).to_le_bytes());
        }
        for f in &self.frames {
            out.extend_from_slice(f);
        }
        out
    }

    /// Parses [`Track::to_bytes`]; `None` when malformed (sizes are bounded: ≤ 8192 px a
    /// side, ≤ 1 M frames).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let rest = b.strip_prefix(MAGIC)?;
        let u32_at = |s: &[u8], o: usize| -> Option<u32> {
            Some(u32::from_le_bytes(s.get(o..o + 4)?.try_into().ok()?))
        };
        let (w, h, n) = (u32_at(rest, 0)?, u32_at(rest, 4)?, u32_at(rest, 8)?);
        if w == 0 || h == 0 || w > 8192 || h > 8192 || n > 1 << 20 {
            return None;
        }
        let ml = usize::from(*rest.get(12)?);
        let model = std::str::from_utf8(rest.get(13..13 + ml)?).ok()?.to_owned();
        let mut o = 13 + ml;
        let mut lens = Vec::with_capacity(n as usize);
        for _ in 0..n {
            lens.push(u32_at(rest, o)? as usize);
            o += 4;
        }
        let mut frames = Vec::with_capacity(n as usize);
        for l in lens {
            frames.push(rest.get(o..o + l)?.to_vec());
            o += l;
        }
        (o == rest.len()).then_some(Self {
            width: w,
            height: h,
            model,
            frames,
        })
    }
}

/// The source-pixel rectangle `(x, y, w, h)` a layer's mask track covers: the crop's
/// integer bounds, snapped out to even coordinates (4:2:0 chroma), inside `w × h`.
pub fn track_rect(crop: &gmclip_plan::types::Rect, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (w, h) = (w.max(2), h.max(2));
    let x0 = (crop.x.floor().max(0.0) as u32).min(w - 2) & !1;
    let y0 = (crop.y.floor().max(0.0) as u32).min(h - 2) & !1;
    let x1 = ((crop.x + crop.w).ceil().max(0.0) as u32).clamp(x0 + 2, w);
    let y1 = ((crop.y + crop.h).ceil().max(0.0) as u32).clamp(y0 + 2, h);
    let cw = (((x1 - x0) + 1) & !1).min(w - x0);
    let ch = (((y1 - y0) + 1) & !1).min(h - y0);
    (x0, y0, cw, ch)
}

/// The cache key of a track: SHA-256 (hex, 32 chars) of the source identity, the span
/// (first frame, frame count), the integer crop bounds and the model.
pub fn track_key(
    source: &str,
    first: u64,
    frames: u64,
    crop: (u32, u32, u32, u32),
    model: &str,
) -> String {
    let s = format!(
        "gmclip.matte/1\n{source}\n{first}+{frames}\n{},{},{},{}\n{model}",
        crop.0, crop.1, crop.2, crop.3
    );
    let mut h = crate::sha256::hex(s.as_bytes());
    h.truncate(32);
    h
}

/// The source identity a track key uses: the content hash when the plan has one, else
/// path, size and mtime.
pub fn source_identity(src: &gmclip_plan::types::Source) -> String {
    match &src.fingerprint {
        Some(f) => match &f.sha256 {
            Some(sha) => format!("sha256:{}", sha.as_str()),
            None => format!(
                "{}|{}|{}",
                src.path,
                f.size.unwrap_or(0),
                f.mtime_ns.unwrap_or(0)
            ),
        },
        None => src.path.clone(),
    }
}

/// How good a part's matte looks, for finding bad parts fast (SPEC §5.8 matte view).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct MatteQuality {
    pub frames: usize,
    /// Mean fraction of the crop that is person (≥ 128).
    pub coverage: f64,
    /// Mean fraction of pixels the model is unsure about (32..224): soft or noisy edges.
    pub uncertain: f64,
    /// Mean fraction of pixels whose person/background state flips between frames.
    pub flicker: f64,
    /// Frames with almost no person (< 0.5 % coverage): the model lost the subject.
    pub empty_frames: usize,
    /// 0–100: `100 · (1 − 3·uncertain − 8·flicker − empty share − overfill)`, clamped,
    /// where overfill is how far coverage is above 0.92 (the model took the background).
    pub score: f64,
}

/// [`MatteQuality`] of a sequence of finished masks.
pub fn quality(masks: &[Mask]) -> MatteQuality {
    let n = masks.len();
    if n == 0 {
        return MatteQuality::default();
    }
    let mut cov = 0.0;
    let mut unc = 0.0;
    let mut flick = 0.0;
    let mut empty = 0;
    for (i, m) in masks.iter().enumerate() {
        let len = m.data.len().max(1) as f64;
        let c = m.coverage();
        cov += c;
        if c < 0.005 {
            empty += 1;
        }
        unc += m.data.iter().filter(|&&v| (32..224).contains(&v)).count() as f64 / len;
        if i > 0 && masks[i - 1].data.len() == m.data.len() {
            let changed = m
                .data
                .iter()
                .zip(&masks[i - 1].data)
                .filter(|(a, b)| (**a >= 128) != (**b >= 128))
                .count();
            flick += changed as f64 / len;
        }
    }
    let nf = n as f64;
    let (coverage, uncertain) = (cov / nf, unc / nf);
    let flicker = if n > 1 { flick / (nf - 1.0) } else { 0.0 };
    let overfill = ((coverage - 0.92) / 0.08).clamp(0.0, 1.0);
    let score = (100.0 * (1.0 - 3.0 * uncertain - 8.0 * flicker - empty as f64 / nf - overfill))
        .clamp(0.0, 100.0);
    MatteQuality {
        frames: n,
        coverage,
        uncertain,
        flicker,
        empty_frames: empty,
        score: (score * 10.0).round() / 10.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmclip_plan::types::Point;

    fn disc(w: u32, h: u32, cx: f64, cy: f64, r: f64) -> Mask {
        let mut m = Mask::filled(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
                if d < r {
                    m.data[(y * w + x) as usize] = 255;
                }
            }
        }
        m
    }

    #[test]
    fn codec_round_trips_and_compresses() {
        let m = disc(496, 340, 250.0, 170.0, 120.0);
        let e = codec::encode(&m.data);
        assert!(
            e.len() * 50 < m.data.len(),
            "{} of {}",
            e.len(),
            m.data.len()
        );
        assert_eq!(codec::decode(&e, m.data.len()).unwrap(), m.data);
        // literals, short runs and long runs
        let mut v: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 256) as u8).collect();
        v.extend(std::iter::repeat_n(9u8, 500));
        v.extend([1, 1, 2, 2, 2, 3]);
        assert_eq!(codec::decode(&codec::encode(&v), v.len()).unwrap(), v);
        assert_eq!(
            codec::decode(&codec::encode(&[]), 0).unwrap(),
            Vec::<u8>::new()
        );
        assert!(codec::decode(&[200], 5).is_none());
        assert!(codec::decode(&codec::encode(&v), v.len() - 1).is_none());
    }

    #[test]
    fn resize_is_identity_at_size_and_keeps_edges() {
        let m = disc(64, 48, 32.0, 24.0, 16.0);
        assert_eq!(m.resize(64, 48), m);
        let big = m.resize(128, 96);
        assert_eq!(big.at(64, 48), 255);
        assert_eq!(big.at(0, 0), 0);
        let back = big.resize(64, 48);
        let diff = back
            .data
            .iter()
            .zip(&m.data)
            .filter(|(a, b)| a.abs_diff(**b) > 64)
            .count();
        assert!(diff < 40, "{diff}");
        // deterministic
        assert_eq!(m.resize(37, 91), m.resize(37, 91));
    }

    #[test]
    fn smoothing_averages_neighbours() {
        let a = Mask::filled(4, 4, 0);
        let b = Mask::filled(4, 4, 255);
        let s = smooth_temporal(&[a.clone(), b.clone(), a.clone()]);
        assert_eq!(s[1].data[0], 128); // (0 + 510 + 0 + 2) / 4
        assert_eq!(s[0].data[0], 64); // (0 + 0 + 255 + 2) / 4
        assert_eq!(smooth_temporal(std::slice::from_ref(&b))[0], b);
    }

    #[test]
    fn soften_blurs_only_edges() {
        let mut m = disc(64, 64, 32.0, 32.0, 20.0);
        let before = m.clone();
        soften(&mut m, 3);
        assert_eq!(m.at(32, 32), 255);
        assert_eq!(m.at(0, 0), 0);
        let soft = m.data.iter().filter(|&&v| v > 0 && v < 255).count();
        assert!(soft > before.data.iter().filter(|&&v| v > 0 && v < 255).count());
        let mut z = m.clone();
        soften(&mut z, 0);
        assert_eq!(z, m);
    }

    #[test]
    fn strokes_add_and_subtract() {
        let mut m = Mask::filled(100, 100, 0);
        let at = MaskPlacement {
            x: 1000.0,
            y: 500.0,
            sx: 1.0,
            sy: 1.0,
        };
        let add = MatteStroke {
            mode: StrokeMode::Add,
            radius: 10.0,
            feather: Some(0.5),
            points: vec![
                Point {
                    x: 1020.0,
                    y: 550.0,
                },
                Point {
                    x: 1080.0,
                    y: 550.0,
                },
            ],
        };
        apply_strokes(&mut m, at, std::slice::from_ref(&add));
        assert_eq!(m.at(50, 50), 255);
        assert_eq!(m.at(50, 10), 0);
        // feathered edge: between hard (5) and radius (10)
        let edge = m.at(50, 57);
        assert!(edge > 0 && edge < 255, "{edge}");
        let sub = MatteStroke {
            mode: StrokeMode::Subtract,
            radius: 4.0,
            feather: None,
            points: vec![Point {
                x: 1050.5,
                y: 550.5,
            }],
        };
        apply_strokes(&mut m, at, &[sub]);
        assert_eq!(m.at(50, 50), 0);
        assert_eq!(m.at(30, 50), 255);
    }

    #[test]
    fn track_round_trips() {
        let masks: Vec<Mask> = (0..5)
            .map(|i| disc(80, 60, 30.0 + i as f64, 30.0, 15.0))
            .collect();
        let t = Track::from_masks("apple_vision.accurate", &masks).unwrap();
        let b = t.to_bytes();
        let t2 = Track::from_bytes(&b).unwrap();
        assert_eq!(t, t2);
        assert_eq!(t2.frame(2).unwrap(), smooth_temporal(&masks)[2]);
        assert!(t2.frame(5).is_none());
        assert!(Track::from_bytes(&b[..b.len() - 1]).is_none());
        assert!(Track::from_masks("x", &[Mask::filled(2, 2, 0), Mask::filled(3, 2, 0)]).is_none());
    }

    #[test]
    fn track_rect_covers_the_crop_evenly() {
        use gmclip_plan::types::Rect;
        let r = track_rect(
            &Rect {
                x: 2060.4,
                y: 634.0,
                w: 496.0,
                h: 340.2,
            },
            2560,
            1440,
        );
        assert_eq!(r, (2060, 634, 498, 342));
        let r = track_rect(
            &Rect {
                x: 2401.0,
                y: 1301.0,
                w: 500.0,
                h: 500.0,
            },
            2560,
            1440,
        );
        assert_eq!(r, (2400, 1300, 160, 140));
        assert!(r.0.is_multiple_of(2) && r.2.is_multiple_of(2));
    }

    #[test]
    fn keys_change_with_every_input() {
        let k = track_key(
            "sha256:ab",
            10,
            30,
            (0, 0, 100, 100),
            "apple_vision.accurate",
        );
        assert_eq!(k.len(), 32);
        assert_ne!(
            k,
            track_key(
                "sha256:ab",
                11,
                30,
                (0, 0, 100, 100),
                "apple_vision.accurate"
            )
        );
        assert_ne!(
            k,
            track_key(
                "sha256:ab",
                10,
                31,
                (0, 0, 100, 100),
                "apple_vision.accurate"
            )
        );
        assert_ne!(
            k,
            track_key(
                "sha256:ab",
                10,
                30,
                (2, 0, 100, 100),
                "apple_vision.accurate"
            )
        );
        assert_ne!(
            k,
            track_key("sha256:ab", 10, 30, (0, 0, 100, 100), "apple_vision.fast")
        );
        assert_ne!(
            k,
            track_key(
                "sha256:ac",
                10,
                30,
                (0, 0, 100, 100),
                "apple_vision.accurate"
            )
        );
    }

    #[test]
    fn quality_flags_flicker_and_loss() {
        let good: Vec<Mask> = (0..10).map(|_| disc(64, 64, 32.0, 32.0, 20.0)).collect();
        let q = quality(&good);
        assert!(q.score > 90.0, "{q:?}");
        assert_eq!(q.flicker, 0.0);
        let bad: Vec<Mask> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    disc(64, 64, 32.0, 32.0, 20.0)
                } else {
                    Mask::filled(64, 64, 0)
                }
            })
            .collect();
        let q2 = quality(&bad);
        assert_eq!(q2.empty_frames, 5);
        assert!(q2.score < 40.0, "{q2:?}");
        let full = vec![Mask::filled(8, 8, 255); 3];
        assert!(quality(&full).score < 10.0);
    }
}
