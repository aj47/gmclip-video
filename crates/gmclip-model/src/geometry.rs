//! Geometry (SPEC §4.2). Crops are normalised to source display space; no model
//! field stores resolution-specific pixels (R-GEO-1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A crop in normalised source display space, each component in `[0, 1]`.
#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    pub const FULL: NormRect = NormRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Component-wise linear interpolation (`t = 0` → `self`, `t = 1` → `to`).
    pub fn lerp(self, to: NormRect, t: f64) -> NormRect {
        NormRect {
            x: lerp(self.x, to.x, t),
            y: lerp(self.y, to.y, t),
            w: lerp(self.w, to.w, t),
            h: lerp(self.h, to.h, t),
        }
    }

    /// Every component finite, `w, h > 0`, inside `[0, 1]` (with a 1e-9 slack).
    pub fn is_valid(&self) -> bool {
        let e = 1e-9;
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
            && self.w > 0.0
            && self.h > 0.0
            && self.x >= -e
            && self.y >= -e
            && self.x + self.w <= 1.0 + e
            && self.y + self.h <= 1.0 + e
    }
}

/// A placement on the canvas, x/y as fractions of canvas width/height and `w` as a
/// fraction of canvas width; height is derived from the crop's aspect (SPEC §4.2).
#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub w: f64,
}

impl Place {
    pub fn lerp(self, to: Place, t: f64) -> Place {
        Place {
            x: lerp(self.x, to.x, t),
            y: lerp(self.y, to.y, t),
            w: lerp(self.w, to.w, t),
        }
    }

    /// Derived height as a fraction of canvas height:
    /// `w·(Wc/Hc)·(crop.h·Hs)/(crop.w·Ws)` in canvas widths, divided by `Hc/Wc`.
    pub fn derived_h(&self, crop: &NormRect, canvas: (u32, u32), source: (u32, u32)) -> f64 {
        let (wc, hc) = (f64::from(canvas.0), f64::from(canvas.1));
        let (ws, hs) = (f64::from(source.0), f64::from(source.1));
        let h_px = self.w * wc * (crop.h * hs) / (crop.w * ws);
        h_px / hc
    }
}

/// A box on the canvas (fractions of canvas width/height).
#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CanvasBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// `a + (b − a)·t`.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
