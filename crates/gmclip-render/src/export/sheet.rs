//! Contact sheets for `gmclip still` (H-05b, the K3 early warning): rendered frames
//! beside the reference's frames at the same output index, with Instagram's overlay zones drawn
//! on ours, written as PNG. Pure CPU; no image crate (an uncompressed-deflate PNG
//! writer is ~60 lines and needs no supply-chain audit).
// Per-channel loops over 3-byte pixels read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::io::Write;
use std::path::Path;

/// 8-bit RGB, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u8>,
}

impl RgbImage {
    pub fn new(width: u32, height: u32, fill: [u8; 3]) -> Self {
        Self {
            width,
            height,
            px: fill.repeat(width as usize * height as usize),
        }
    }

    fn put(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let o = (y as usize * self.width as usize + x as usize) * 3;
            self.px[o..o + 3].copy_from_slice(&c);
        }
    }

    fn blend(&mut self, x: i64, y: i64, c: [u8; 3], a: f32) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let o = (y as usize * self.width as usize + x as usize) * 3;
            for k in 0..3 {
                let v = f32::from(self.px[o + k]) * (1.0 - a) + f32::from(c[k]) * a;
                self.px[o + k] = v.round() as u8;
            }
        }
    }

    /// Box-filter downscale by an integer factor.
    pub fn downscale(&self, f: u32) -> Self {
        let f = f.max(1);
        let (w, h) = (self.width / f, self.height / f);
        let mut out = Self::new(w, h, [0, 0, 0]);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 3];
                for j in 0..f {
                    for i in 0..f {
                        let o = (((y * f + j) * self.width + x * f + i) * 3) as usize;
                        for k in 0..3 {
                            acc[k] += u32::from(self.px[o + k]);
                        }
                    }
                }
                let o = ((y * w + x) * 3) as usize;
                for k in 0..3 {
                    out.px[o + k] = (acc[k] / (f * f)) as u8;
                }
            }
        }
        out
    }

    fn paste(&mut self, src: &RgbImage, x0: u32, y0: u32) {
        for y in 0..src.height {
            for x in 0..src.width {
                let o = ((y * src.width + x) * 3) as usize;
                self.put(
                    i64::from(x0 + x),
                    i64::from(y0 + y),
                    [src.px[o], src.px[o + 1], src.px[o + 2]],
                );
            }
        }
    }
}

/// BT.709 limited-range NV12 → RGB (for sheets; display only).
pub fn nv12_to_rgb(y: &[u8], uv: &[u8], width: u32, height: u32) -> RgbImage {
    let (w, h) = (width as usize, height as usize);
    let cw = w.div_ceil(2);
    let mut out = RgbImage::new(width, height, [0, 0, 0]);
    for j in 0..h {
        for i in 0..w {
            let c = ((j / 2) * cw + i / 2) * 2;
            let rgb = crate::compositor::ycbcr_to_rgb(
                f32::from(y[j * w + i]),
                f32::from(uv[c]),
                f32::from(uv[c + 1]),
            );
            let o = (j * w + i) * 3;
            for k in 0..3 {
                out.px[o + k] = (rgb[k].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    out
}

/// Instagram Reels overlay zones on a 1080×1920 frame (AGENTS.md rule 9): the top
/// 220 px, the bottom 360 px, and the right 120 px between y 1000 and 1560.
pub const IG_ZONES: [[u32; 4]; 3] = [
    [0, 0, 1080, 220],
    [0, 1560, 1080, 360],
    [960, 1000, 120, 560],
];

/// 3×5 digit glyphs, one bit per pixel, rows top to bottom.
const DIGITS: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

fn number(img: &mut RgbImage, mut x: i64, y: i64, n: u64, scale: i64, c: [u8; 3]) {
    for ch in n.to_string().bytes() {
        let g = DIGITS[usize::from(ch - b'0')];
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            img.put(x + col * scale + dx, y + row as i64 * scale + dy, c);
                        }
                    }
                }
            }
        }
        x += 4 * scale;
    }
}

/// One cell: output frame `frame`, our render, and the reference's frame if there is one.
#[derive(Clone, Debug)]
pub struct SheetCell {
    pub frame: u64,
    pub ours: RgbImage,
    pub reference: Option<RgbImage>,
}

/// Lays cells out `cols` per row: each cell is ours (IG zones tinted red) beside the
/// reference, at 1/`scale` size, with the output frame number above.
pub fn contact_sheet(cells: &[SheetCell], cols: usize, scale: u32) -> RgbImage {
    let (fw, fh) = cells.first().map_or((1080 / scale, 1920 / scale), |c| {
        (c.ours.width / scale, c.ours.height / scale)
    });
    let (gap, label) = (8u32, 24u32);
    let cell_w = 2 * fw + gap;
    let cell_h = fh + label;
    let cols = cols.max(1);
    let rows = cells.len().div_ceil(cols) as u32;
    let mut sheet = RgbImage::new(
        cols as u32 * (cell_w + 2 * gap) + gap,
        rows * (cell_h + gap) + gap,
        [24, 24, 28],
    );
    for (k, c) in cells.iter().enumerate() {
        let (cx, cy) = ((k % cols) as u32, (k / cols) as u32);
        let x0 = gap + cx * (cell_w + 2 * gap);
        let y0 = gap + cy * (cell_h + gap);
        number(
            &mut sheet,
            i64::from(x0),
            i64::from(y0 + 4),
            c.frame,
            3,
            [230, 230, 230],
        );
        let tint = |img: &mut RgbImage| {
            for z in IG_ZONES {
                for y in z[1] / scale..(z[1] + z[3]) / scale {
                    for x in z[0] / scale..(z[0] + z[2]) / scale {
                        img.blend(i64::from(x), i64::from(y), [220, 40, 40], 0.25);
                    }
                }
            }
        };
        // Zones on both sides: with text off on ours, the reference's side is where captions,
        // hooks and pills can run into them.
        let mut ours = c.ours.downscale(scale);
        tint(&mut ours);
        sheet.paste(&ours, x0, y0 + label);
        if let Some(r) = &c.reference {
            // Clipped to the cell, whatever the reference's size.
            let src = r.downscale(scale);
            let mut cell = RgbImage::new(fw.min(src.width), fh.min(src.height), [0, 0, 0]);
            for y in 0..cell.height {
                for x in 0..cell.width {
                    let o = ((y * src.width + x) * 3) as usize;
                    cell.put(
                        i64::from(x),
                        i64::from(y),
                        [src.px[o], src.px[o + 1], src.px[o + 2]],
                    );
                }
            }
            tint(&mut cell);
            sheet.paste(&cell, x0 + fw + gap, y0 + label);
        }
    }
    sheet
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Encodes `img` as a PNG (RGB8, no filter, stored deflate blocks).
pub fn png_bytes(img: &RgbImage) -> Vec<u8> {
    assert!(img.width > 0 && img.height > 0, "an empty image has no PNG");
    let mut raw = Vec::with_capacity((img.width as usize * 3 + 1) * img.height as usize);
    for row in img.px.chunks(img.width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let n = b.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut bsum) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65_521;
        bsum = (bsum + a) % 65_521;
    }
    z.extend_from_slice(&((bsum << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

pub fn write_png(img: &RgbImage, path: &Path) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&png_bytes(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_png_structure() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        let img = RgbImage::new(3, 2, [10, 20, 30]);
        let p = png_bytes(&img);
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
    }

    #[test]
    fn sheet_size_and_zone_tint() {
        let cell = SheetCell {
            frame: 42,
            ours: RgbImage::new(1080, 1920, [0, 0, 0]),
            reference: Some(RgbImage::new(1080, 1920, [255, 255, 255])),
        };
        let s = contact_sheet(&[cell.clone(), cell], 2, 4);
        assert_eq!(s.width, 2 * (2 * 270 + 8 + 16) + 8);
        // Top-left of "ours" (inside the top IG zone) is tinted red.
        let (x, y) = (8u32 + 2, 8u32 + 24 + 2);
        let o = ((y * s.width + x) * 3) as usize;
        assert!(s.px[o] > 40 && s.px[o + 1] < 20, "{:?}", &s.px[o..o + 3]);
    }
}
