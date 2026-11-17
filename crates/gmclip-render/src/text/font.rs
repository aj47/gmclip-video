//! Font files and the per-font em rule (SPEC Conventions, RP rule 6).
//!
//! A libass `Fontsize` at PlayResY = canvas height is not an em: libass sizes a face so that
//! `usWinAscent + usWinDescent` spans `size` pixels (hhea ascent − descent when OS/2 is absent
//! or zero). So `em_px = size · em_per_size` with
//! `em_per_size = unitsPerEm / (usWinAscent + usWinDescent)`, and the line advance is exactly
//! `size` (hhea `lineGap` ignored). Neometric Heavy: 2048 / (2048 + 410) = 0.8332.

use std::fmt;
use std::sync::Arc;

use skrifa::raw::TableProvider;

/// A font file could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontError(pub String);

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "font: {}", self.0)
    }
}

impl std::error::Error for FontError {}

/// The vertical metrics libass sizes by, in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VMetrics {
    /// `head.unitsPerEm`.
    pub units_per_em: u16,
    /// Ascent above the baseline (OS/2 `usWinAscent`, else hhea `ascender`).
    pub ascent: i32,
    /// Descent below the baseline, positive (OS/2 `usWinDescent`, else −hhea `descender`).
    pub descent: i32,
}

impl VMetrics {
    /// `unitsPerEm / (ascent + descent)`: em pixels per libass size unit.
    pub fn em_per_size(&self) -> f64 {
        f64::from(self.units_per_em) / f64::from(self.ascent + self.descent)
    }

    /// Ascent in pixels at a libass `size`.
    pub fn ascent_px(&self, size: f64) -> f64 {
        size * f64::from(self.ascent) / f64::from(self.ascent + self.descent)
    }

    /// Descent in pixels at a libass `size`.
    pub fn descent_px(&self, size: f64) -> f64 {
        size * f64::from(self.descent) / f64::from(self.ascent + self.descent)
    }
}

/// One face from a font file, held as shared bytes (export fonts are blobs, SPEC §7).
#[derive(Clone)]
pub struct FontFace {
    data: Arc<Vec<u8>>,
    index: u32,
    metrics: VMetrics,
}

impl fmt::Debug for FontFace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontFace")
            .field("bytes", &self.data.len())
            .field("index", &self.index)
            .field("metrics", &self.metrics)
            .finish()
    }
}

impl FontFace {
    /// Reads face `index` of a font file.
    pub fn new(data: Vec<u8>, index: u32) -> Result<Self, FontError> {
        let metrics = {
            let f =
                skrifa::FontRef::from_index(&data, index).map_err(|e| FontError(format!("{e}")))?;
            let head = f.head().map_err(|e| FontError(format!("head: {e}")))?;
            let upem = head.units_per_em();
            if upem == 0 {
                return Err(FontError("unitsPerEm is 0".into()));
            }
            let win = f
                .os2()
                .ok()
                .map(|o| (i32::from(o.us_win_ascent()), i32::from(o.us_win_descent())));
            let (ascent, descent) = match win {
                Some((a, d)) if a + d > 0 => (a, d),
                _ => {
                    let h = f.hhea().map_err(|e| FontError(format!("hhea: {e}")))?;
                    (
                        i32::from(h.ascender().to_i16()),
                        -i32::from(h.descender().to_i16()),
                    )
                }
            };
            if ascent + descent <= 0 {
                return Err(FontError("ascent + descent ≤ 0".into()));
            }
            VMetrics {
                units_per_em: upem,
                ascent,
                descent,
            }
        };
        Ok(Self {
            data: Arc::new(data),
            index,
            metrics,
        })
    }

    /// Reads face 0 of a file on disk.
    pub fn open(path: &std::path::Path) -> Result<Self, FontError> {
        let data =
            std::fs::read(path).map_err(|e| FontError(format!("{}: {e}", path.display())))?;
        Self::new(data, 0)
    }

    /// The face's bytes.
    pub fn data(&self) -> &Arc<Vec<u8>> {
        &self.data
    }

    /// The face index within the file.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The libass sizing metrics.
    pub fn metrics(&self) -> VMetrics {
        self.metrics
    }

    /// `em_per_size` for RenderPlan `fonts[]`.
    pub fn em_per_size(&self) -> f64 {
        self.metrics.em_per_size()
    }

    /// A skrifa view of the face.
    pub fn skrifa(&self) -> skrifa::FontRef<'_> {
        skrifa::FontRef::from_index(&self.data, self.index).expect("validated in new()")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neometric_em_rule() {
        let m = VMetrics {
            units_per_em: 2048,
            ascent: 2048,
            descent: 410,
        };
        assert!(
            (m.em_per_size() - 0.8332).abs() < 5e-5,
            "{}",
            m.em_per_size()
        );
        assert!((m.ascent_px(86.0) + m.descent_px(86.0) - 86.0).abs() < 1e-12);
    }
}
