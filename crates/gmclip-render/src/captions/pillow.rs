//! The reference's fit measure, reproduced (SPEC §5.9 "fit: shrink"; RP §2.4 "Fit").
//!
//! the reference sizes hooks, captions and pills with `story_reel.fit_size`: the widest line's Pillow
//! `ImageFont.truetype(font, size).getlength(line)` × `LIBASS_PER_PILLOW` (0.838), shrunk to
//! `int(size · max_px / w)` when it exceeds `max_px`. AJ's Pillow (11.3.0, FreeType 2.13.3,
//! libraqm 0.10.1) lays text out with raqm, i.e. HarfBuzz over an `hb_ft` font:
//!
//! - `FT_Request_Size(NOMINAL, size·64)` gives `x_scale = size·64·65536 / unitsPerEm`, and
//!   hb-ft sets the HarfBuzz scale to `size·64` (26.6 pixels per em).
//! - Advances: FreeType unhinted advance (raqm leaves hb-ft's `FT_LOAD_NO_HINTING`) scaled to
//!   16.16, then `(v + 512) >> 10` to 26.6 — that is `⌊adv·size·64/upem + ½⌋`, the same
//!   rounding as HarfBuzz's own `em_scale` of the hmtx advance.
//! - GPOS kerning and other positioning in HarfBuzz's `em_scale` (`(v·mult + 32768) >> 16`).
//! - `getlength` = Σ `x_advance` / 64.
//!
//! So shaping with harfrust (HarfBuzz's Rust port) at scale `size·64` and summing
//! `x_advance` reproduces `getlength` exactly; `tools/s4_pillow_widths.py` writes the
//! Pillow table every golden line is checked against (`captions/pillow-widths.tsv`).

use std::sync::Arc;

use harfrust::{FontRef, ShapeOptions, ShaperData, UnicodeBuffer};

use crate::text::font::{FontError, FontFace};

/// `LIBASS_PER_PILLOW` in `story_reel.py`: libass draws this OTF 0.838× Pillow's width.
pub const LIBASS_PER_PILLOW: f64 = 0.838;

/// Pillow `getlength`, reproduced for one face.
#[derive(Clone)]
pub struct PillowMeasure {
    data: Arc<Vec<u8>>,
    index: u32,
    shaper: Arc<ShaperData>,
}

impl std::fmt::Debug for PillowMeasure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PillowMeasure")
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl PillowMeasure {
    /// A measure over a face's bytes.
    pub fn new(face: &FontFace) -> Result<Self, FontError> {
        let data = face.data().clone();
        let font =
            FontRef::from_index(&data, face.index()).map_err(|e| FontError(format!("{e}")))?;
        let shaper = Arc::new(ShaperData::new(&font));
        Ok(Self {
            data,
            index: face.index(),
            shaper,
        })
    }

    /// Pillow `ImageFont.truetype(font, size).getlength(text)` in pixels, with raqm layout.
    pub fn getlength(&self, text: &str, size: u32) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let font = FontRef::from_index(&self.data, self.index).expect("validated in new()");
        let shaper = self.shaper.shaper(&font).build();
        let mut buf = UnicodeBuffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        let scale = i32::try_from(u64::from(size) * 64).unwrap_or(i32::MAX);
        let out = shaper.shape(buf, ShapeOptions::new().scale(Some(scale)));
        let sum: i64 = out
            .glyph_positions()
            .iter()
            .map(|p| i64::from(p.x_advance))
            .sum();
        sum as f64 / 64.0
    }

    /// The reference's `fit_size(text, size, max_px)`: markup (`{…}`) stripped, lines split at the
    /// literal `\N`, widest line in libass px = `getlength × 0.838`; `size` if that fits
    /// `max_px`, else `int(size · max_px / w)`.
    pub fn fit_size(&self, text: &str, size: u32, max_px: u32) -> u32 {
        let w = self.width(text, size);
        if w <= f64::from(max_px) {
            size
        } else {
            // Python: int(size * max_px / w) — an int product, then true division.
            ((u64::from(size) * u64::from(max_px)) as f64 / w).trunc() as u32
        }
    }

    /// The width `fit_size` compares: widest line × [`LIBASS_PER_PILLOW`].
    pub fn width(&self, text: &str, size: u32) -> f64 {
        let plain = plain(text);
        let w = plain
            .split("\\N")
            .map(|l| self.getlength(l, size))
            .fold(f64::NEG_INFINITY, f64::max);
        w * LIBASS_PER_PILLOW
    }
}

pub use gmclip_plan::story_cues::plain;

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn plain_matches_python_regex() {
        assert_eq!(plain("we {Y}saved{W} him."), "we saved him.");
        assert_eq!(
            plain("{\\an7\\pos(48,262)\\fs50}BeriGame  {\\c&H00D4FF&}a"),
            "BeriGame  a"
        );
        assert_eq!(plain("open { brace"), "open { brace");
        assert_eq!(plain("a{b}c{d"), "ac{d");
    }
}
