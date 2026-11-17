//! G9's fill-ink box of a text event "rendered alone, no outline or shadow", computed from
//! the font's own glyph metrics (`evals/h79/truth/font-*.json`, made by
//! `evals/h79/make_truth.py` with fontTools) and RenderPlan rule 6:
//!
//! - the em is `size × em_per_size`, with `em_per_size = upem / (winAscent + winDescent)`;
//! - the line advance is exactly `size`; lines stack from the block top;
//! - the baseline of a line sits `winAscent` (scaled) below its line top;
//! - `align` uses the numpad convention around `pos` (5 = block centre, 7 = top-left);
//! - each glyph's ink is its outline bounds; advances add up without kerning, plus
//!   `letter_spacing` between glyphs.
//!
//! This is the layout libass uses for these single-style events (RP §2.1 rule 6). It
//! ignores kerning (±a few px horizontally); the vertical extent, which the band check
//! depends on, is exact to the outline bounds. Characters outside the table count as
//! the em square (conservative).

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Glyph {
    pub adv: f64,
    /// `[xMin, yMin, xMax, yMax]` in font units, `None` for blank glyphs.
    pub ink: Option<[f64; 4]>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FontId {
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FontMetrics {
    pub font: FontId,
    pub units_per_em: f64,
    pub win_ascent: f64,
    pub win_descent: f64,
    pub glyphs: BTreeMap<String, Glyph>,
}

/// An axis-aligned box in canvas pixels `[x0, y0, x1, y1]`.
pub type BBox = [f64; 4];

/// The ink box of `text` (`\n` separates lines) at `size`, anchored at `(x, y)` by
/// numpad `align`, with `letter_spacing` px. `None` when nothing has ink.
pub fn ink_bbox(
    m: &FontMetrics,
    text: &str,
    size: f64,
    align: u8,
    x: f64,
    y: f64,
    letter_spacing: f64,
) -> Option<BBox> {
    let em = size * m.units_per_em / (m.win_ascent + m.win_descent);
    let s = em / m.units_per_em;
    let lines: Vec<&str> = text.split('\n').collect();
    let block_h = size * lines.len() as f64;
    // per line: width (advances) and ink extents relative to the line's pen origin
    let mut widths = Vec::new();
    let mut inks: Vec<Option<BBox>> = Vec::new();
    for line in &lines {
        let mut pen = 0.0f64;
        let mut ink: Option<BBox> = None;
        let n = line.chars().count();
        for (i, c) in line.chars().enumerate() {
            let fallback = Glyph {
                adv: m.units_per_em,
                ink: Some([0.0, -m.win_descent, m.units_per_em, m.win_ascent]),
            };
            let g = m.glyphs.get(&c.to_string()).cloned().unwrap_or(fallback);
            if let Some([x0, y0, x1, y1]) = g.ink {
                // y up in font units → y down in pixels, relative to the baseline
                let b = [pen + x0 * s, -y1 * s, pen + x1 * s, -y0 * s];
                ink = Some(match ink {
                    None => b,
                    Some(a) => [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ],
                });
            }
            pen += g.adv * s;
            if i + 1 < n {
                pen += letter_spacing;
            }
        }
        widths.push(pen);
        inks.push(ink);
    }
    let align = align.clamp(1, 9);
    let (h_al, v_al) = (((align - 1) % 3) as i32, ((align - 1) / 3) as i32); // h: 0 left 1 centre 2 right; v: 0 bottom 1 middle 2 top
    let top = match v_al {
        2 => y,
        1 => y - block_h / 2.0,
        _ => y - block_h,
    };
    let mut out: Option<BBox> = None;
    for (i, (w, ink)) in widths.iter().zip(&inks).enumerate() {
        let Some(ink) = ink else { continue };
        let left = match h_al {
            0 => x,
            1 => x - w / 2.0,
            _ => x - w,
        };
        let baseline = top + size * i as f64 + m.win_ascent * s;
        let b = [
            left + ink[0],
            baseline + ink[1],
            left + ink[2],
            baseline + ink[3],
        ];
        out = Some(match out {
            None => b,
            Some(a) => [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ],
        });
    }
    out
}

/// Whether two boxes overlap (positive area).
pub fn intersects(a: BBox, b: BBox) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> FontMetrics {
        let p = super::super::bundle::h79_dir().join("truth/font-neometric-heavy.json");
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn v4_hook_ink_sits_in_the_band() {
        let m = metrics();
        let b = ink_bbox(
            &m,
            "My PC made this\nin 67 seconds.",
            88.0,
            5,
            540.0,
            305.0,
            0.0,
        )
        .unwrap();
        // layout box is [217, 393]; the ink is inside it and inside [220, 400]
        assert!(b[1] > 220.0 && b[3] < 400.0, "{b:?}");
        assert!(b[1] > 217.0 && b[3] < 393.0, "{b:?}");
        assert!(b[0] > 0.0 && b[2] < 1080.0, "{b:?}");
        // centred
        assert!(((b[0] + b[2]) / 2.0 - 540.0).abs() < 30.0, "{b:?}");
    }

    #[test]
    fn alignment_and_descenders() {
        let m = metrics();
        let a = ink_bbox(&m, "ag", 100.0, 7, 0.0, 0.0, 0.0).unwrap();
        let b = ink_bbox(&m, "aa", 100.0, 7, 0.0, 0.0, 0.0).unwrap();
        assert!(a[3] > b[3], "g descends below a: {a:?} {b:?}");
        let c = ink_bbox(&m, "aa", 100.0, 3, 500.0, 500.0, 0.0).unwrap();
        assert!(c[2] <= 500.0 + 1.0 && c[3] <= 500.0, "{c:?}");
        assert_eq!(ink_bbox(&m, "   ", 100.0, 5, 0.0, 0.0, 0.0), None);
    }
}
