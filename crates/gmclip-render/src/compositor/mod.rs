//! Compositor v0 (H-05, SPEC §7 M0 pass list).
//!
//! Per layer: sample the source's NV12 planes → Y'CbCr → R'G'B' (BT.709 limited,
//! "compat" working space) → sub-pixel crop → separable Lanczos-3, widened by the
//! downscale factor → place → premultiplied *over* in RGBA16F, layers in ascending
//! `z` then array order, over a solid background. `visible: false` and opacity 0 skip
//! a layer; `time_map` picks the source frame (live / hold / freeze_first).
//!
//! Two implementations of the same arithmetic:
//! - [`cpu`]: f32, runs everywhere (hosted CI);
//! - [`gpu`]: wgpu render passes (lab on Metal; the export path).
//!
//! Geometry conventions (both paths):
//! - pixel `k` covers `[k, k+1)`, its centre is `k + ½`;
//! - destination pixel `X` samples source position `u = crop.x + (X + ½ − place.x) ·
//!   crop.w / place.w` (likewise in y);
//! - Lanczos-3 weights `L(t) = sinc(t)·sinc(t/3)`, `t = (k + ½ − u) / s`,
//!   `s = max(1, crop.w / place.w)`, normalised; taps clamp to the crop's integer
//!   bounds `[⌊crop.x⌋, ⌈crop.x + crop.w⌉)` (exactly ffmpeg's `crop` then `scale` for
//!   integer crops);
//! - a destination pixel's coverage of the place rect (partial at fractional edges)
//!   times the layer opacity is its alpha.
//! - chroma is 4:2:0, centred vertically; horizontally it is left-cosited (H.264 /
//!   HEVC `chroma_loc_type 0`, the default when untagged) or centred (VP9 / AV1 when
//!   untagged, as swscale assumes) per source ([`ChromaSiting`]); upsampled
//!   bilinearly before the matrix. Sources are limited range (VideoToolbox outputs
//!   `420v`); full-range sources are M2.
//! - the background colour is converted with BT.709 like everything else (reference drew it
//!   with ffmpeg's BT.601 default; for #0c1118 both give Y = 30).
//! - sub-pixel crops: the sample positions follow the fractional crop exactly; taps
//!   clamp to the crop's integer bounds, and chroma taps may read one chroma sample
//!   outside them (ffmpeg crops chroma first; the difference is below one code value).

pub mod cpu;
pub mod cursor;
pub mod draw;
pub mod gpu;
pub mod shapes;
pub mod text_layer;
pub mod zoom;

pub use draw::{
    BACKDROP_LAYER, BackdropDraw, CLIP_LAYER, ComposeError, CoverDraw, CoverKind, FrameDraw,
    LayerDraw, MatteDraw, NominalClocks, SourceClocks, frame_draw, frame_draw_with, plan_streams,
};
pub use draw::{BorderDraw, ShadowDraw, ShapeDraw};

/// Horizontal siting of 4:2:0 chroma samples (vertical siting is centred for both).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ChromaSiting {
    /// Co-sited with even luma columns (H.264/HEVC `chroma_loc_type 0`, the default).
    #[default]
    Left,
    /// Between luma columns (VP9/AV1 untagged; MPEG-1).
    Center,
}

impl ChromaSiting {
    /// Luma-x offset subtracted before halving: chroma position `(x − off) / 2`.
    pub fn x_offset(self) -> f32 {
        match self {
            ChromaSiting::Left => 0.0,
            ChromaSiting::Center => 0.5,
        }
    }
}

/// BT.709 limited range, 8-bit codes → R'G'B' in [0, 1] (unclamped).
pub fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let y = (y - 16.0) / 219.0;
    let cb = (cb - 128.0) / 224.0;
    let cr = (cr - 128.0) / 224.0;
    [
        y + 1.5748 * cr,
        y - 0.187_324_27 * cb - 0.468_124_3 * cr,
        y + 1.8556 * cb,
    ]
}

/// The Lanczos-3 kernel.
pub fn lanczos3(t: f32) -> f32 {
    let t = t.abs();
    if t < 1e-6 {
        return 1.0;
    }
    if t >= 3.0 {
        return 0.0;
    }
    let pt = std::f32::consts::PI * t;
    3.0 * pt.sin() * (pt / 3.0).sin() / (pt * pt)
}

/// Normalised taps `(first source index, weights)` for one output sample at source
/// position `u` with scale `s ≥ 1`, clamped to `[lo, hi)`.
pub fn taps(u: f32, s: f32, lo: i32, hi: i32) -> Vec<(i32, f32)> {
    let r = 3.0 * s;
    let k0 = (u - r - 0.5).ceil() as i32;
    let k1 = (u + r - 0.5).floor() as i32;
    let mut out: Vec<(i32, f32)> = Vec::with_capacity((k1 - k0 + 1).max(0) as usize);
    let mut sum = 0.0;
    for k in k0..=k1 {
        let w = lanczos3((k as f32 + 0.5 - u) / s);
        if w == 0.0 {
            continue;
        }
        sum += w;
        out.push((k.clamp(lo, hi - 1), w));
    }
    if sum != 0.0 {
        for t in &mut out {
            t.1 /= sum;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_reference_points() {
        let w = ycbcr_to_rgb(235.0, 128.0, 128.0);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-6));
        let k = ycbcr_to_rgb(16.0, 128.0, 128.0);
        assert!(k.iter().all(|v| v.abs() < 1e-6));
        // Inverse of the H-07 forward transform for pure red (63, 102, 240).
        let r = ycbcr_to_rgb(62.5594, 102.3348, 240.0);
        assert!(
            (r[0] - 1.0).abs() < 1e-3 && r[1].abs() < 1e-3 && r[2].abs() < 1e-3,
            "{r:?}"
        );
    }

    #[test]
    fn taps_identity_and_normalised() {
        // Scale 1, centred on a pixel: one tap of weight 1 (the others are sinc zeros).
        let t = taps(10.5, 1.0, 0, 100);
        let w: f32 = t.iter().filter(|x| x.0 == 10).map(|x| x.1).sum();
        assert!((w - 1.0).abs() < 1e-5, "{t:?}");
        // Downscale 2: support widened to 6 source pixels each side.
        let t = taps(20.0, 2.0, 0, 100);
        assert_eq!(t.len(), 12);
        let sum: f32 = t.iter().map(|x| x.1).sum();
        assert!((sum - 1.0).abs() < 1e-5);
        // Clamped at the crop edge.
        assert!(taps(0.2, 1.0, 0, 100).iter().all(|x| x.0 >= 0));
    }
}
