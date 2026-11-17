//! The CPU compositor: the same arithmetic as the GPU path in f32 (hosted CI, goldens,
//! and the reference the GPU path is checked against).

use std::sync::Arc;

use super::draw::{
    BackdropDraw, ComposeError, CoverDraw, CoverKind, FrameDraw, LayerDraw, border_alpha, bspline,
    rounded_sd, shadow_falloff, shape_alpha,
};
use super::{ChromaSiting, taps, ycbcr_to_rgb};

/// An 8-bit NV12 picture, tightly packed (luma rows of `width`, CbCr rows of
/// `2·⌈width/2⌉`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12Image {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
    pub siting: ChromaSiting,
}

impl Nv12Image {
    pub fn chroma_size(&self) -> (usize, usize) {
        (
            self.width.div_ceil(2) as usize,
            self.height.div_ceil(2) as usize,
        )
    }

    /// R'G'B' at luma position `(x, y)` (clamped), chroma upsampled bilinearly from
    /// 4:2:0 sited per [`ChromaSiting`] horizontally and centred vertically.
    pub fn rgb_at(&self, x: i32, y: i32) -> [f32; 3] {
        let (w, h) = (self.width as i32, self.height as i32);
        let (x, y) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
        let luma = f32::from(self.y[(y * w + x) as usize]);
        let (cw, ch) = self.chroma_size();
        let (cw, ch) = (cw as i32, ch as i32);
        let cxf = (x as f32 - self.siting.x_offset()) / 2.0;
        let cyf = (y as f32 - 0.5) / 2.0;
        let (j0, i0) = (cxf.floor() as i32, cyf.floor() as i32);
        let (fx, fy) = (cxf - j0 as f32, cyf - i0 as f32);
        let at = |j: i32, i: i32| {
            let j = j.clamp(0, cw - 1);
            let i = i.clamp(0, ch - 1);
            let o = ((i * cw + j) * 2) as usize;
            (f32::from(self.uv[o]), f32::from(self.uv[o + 1]))
        };
        let (a, b, c, d) = (
            at(j0, i0),
            at(j0 + 1, i0),
            at(j0, i0 + 1),
            at(j0 + 1, i0 + 1),
        );
        let lerp = |p: f32, q: f32, t: f32| p + (q - p) * t;
        let cb = lerp(lerp(a.0, b.0, fx), lerp(c.0, d.0, fx), fy);
        let cr = lerp(lerp(a.1, b.1, fx), lerp(c.1, d.1, fx), fy);
        ycbcr_to_rgb(luma, cb, cr)
    }
}

/// Premultiplied RGBA, f32 (the RGBA16F intermediate's CPU twin).
#[derive(Clone, Debug, PartialEq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub px: Vec<[f32; 4]>,
}

impl RgbaImage {
    /// Straight R'G'B' of an opaque canvas (what the NV12 output reads).
    pub fn rgb(&self) -> Vec<[f32; 3]> {
        self.px.iter().map(|p| [p[0], p[1], p[2]]).collect()
    }
}

/// Supplies decoded source frames by (source id, presentation index).
pub trait FrameProvider {
    fn frame(&mut self, source: &str, frame: u64) -> Result<Arc<Nv12Image>, ComposeError>;
}

/// Integer pixel bounds `[lo, hi)` covering `[a, a + len)`, clamped to `[0, max)`.
pub(crate) fn bounds(a: f64, len: f64, max: u32) -> (i32, i32) {
    let lo = a.floor().max(0.0) as i32;
    let hi = ((a + len).ceil() as i32).min(max as i32);
    (lo, hi.max(lo))
}

/// Coverage of pixel `[k, k+1)` by `[a, a + len)`.
pub(crate) fn coverage(k: i32, a: f64, len: f64) -> f32 {
    let lo = (k as f64).max(a);
    let hi = ((k + 1) as f64).min(a + len);
    (hi - lo).clamp(0.0, 1.0) as f32
}

/// The matte value of picture pixel `(k, row)` for a mask laid over `r` (picture
/// pixels): nearest mask texel, 0 outside the mask (decision 0013). The GPU shader does
/// the same in f32.
pub(crate) fn mask_at(
    mask: &crate::matte::Mask,
    r: &gmclip_plan::types::Rect,
    k: i32,
    row: i32,
) -> f32 {
    if r.w <= 0.0 || r.h <= 0.0 || mask.width == 0 || mask.height == 0 {
        return 0.0;
    }
    let mx = ((k as f32 + 0.5 - r.x as f32) * mask.width as f32 / r.w as f32).floor();
    let my = ((row as f32 + 0.5 - r.y as f32) * mask.height as f32 / r.h as f32).floor();
    if mx < 0.0 || my < 0.0 || mx >= mask.width as f32 || my >= mask.height as f32 {
        return 0.0;
    }
    f32::from(mask.at(mx as u32, my as u32)) / 255.0
}

fn draw_layer(canvas: &mut RgbaImage, l: &LayerDraw, img: &Nv12Image) -> Result<(), ComposeError> {
    if let Some(m) = &l.matte {
        let Some((mask, rect)) = &m.mask else {
            return Err(ComposeError::Unsupported(format!(
                "layer {}: a roto matte with no mask",
                l.layer_id
            )));
        };
        return draw_layer_masked(canvas, l, img, mask, rect);
    }
    let (c, p) = (l.crop, l.place);
    if c.w <= 0.0 || c.h <= 0.0 || p.w <= 0.0 || p.h <= 0.0 {
        return Err(ComposeError::Invalid(format!(
            "layer {}: empty crop or place",
            l.layer_id
        )));
    }
    let (cx0, cx1) = bounds(c.x, c.w, img.width);
    let (cy0, cy1) = bounds(c.y, c.h, img.height);
    if cx1 <= cx0 || cy1 <= cy0 {
        return Err(ComposeError::Invalid(format!(
            "layer {}: crop outside the source",
            l.layer_id
        )));
    }
    let (dx0, dx1) = bounds(p.x, p.w, canvas.width);
    let (dy0, dy1) = bounds(p.y, p.h, canvas.height);
    if dx1 <= dx0 || dy1 <= dy0 {
        return Ok(());
    }
    let (sx, sy) = ((c.w / p.w) as f32, (c.h / p.h) as f32);
    // decision 0046: a framed layer's alpha is its rounded shape's
    let radius = l.shape.as_ref().map(|f| f.radius_px(&p));
    // Source region in R'G'B'.
    let rw = (cx1 - cx0) as usize;
    let mut src = vec![[0f32; 3]; rw * (cy1 - cy0) as usize];
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            src[(y - cy0) as usize * rw + (x - cx0) as usize] = img.rgb_at(x, y);
        }
    }
    // Horizontal pass: every source row of the crop, every destination column.
    let dw = (dx1 - dx0) as usize;
    let htaps: Vec<Vec<(i32, f32)>> = (dx0..dx1)
        .map(|x| {
            let u = (c.x + (f64::from(x) + 0.5 - p.x) * c.w / p.w) as f32;
            taps(u, sx.max(1.0), cx0, cx1)
        })
        .collect();
    let rows = (cy1 - cy0) as usize;
    let mut mid = vec![[0f32; 3]; dw * rows];
    for r in 0..rows {
        let row = &src[r * rw..(r + 1) * rw];
        for (i, tx) in htaps.iter().enumerate() {
            let mut acc = [0f32; 3];
            for &(k, w) in tx {
                let s = row[(k - cx0) as usize];
                acc[0] += w * s[0];
                acc[1] += w * s[1];
                acc[2] += w * s[2];
            }
            mid[r * dw + i] = acc;
        }
    }
    // Vertical pass into the canvas, premultiplied over.
    let cw = canvas.width as usize;
    for y in dy0..dy1 {
        let v = (c.y + (f64::from(y) + 0.5 - p.y) * c.h / p.h) as f32;
        let ty = taps(v, sy.max(1.0), cy0, cy1);
        let cov_y = coverage(y, p.y, p.h);
        for (i, x) in (dx0..dx1).enumerate() {
            let mut acc = [0f32; 3];
            for &(k, w) in &ty {
                let s = mid[(k - cy0) as usize * dw + i];
                acc[0] += w * s[0];
                acc[1] += w * s[1];
                acc[2] += w * s[2];
            }
            let a = match radius {
                Some(r) => shape_alpha(x, y, &p, r),
                None => cov_y * coverage(x, p.x, p.w),
            } * l.opacity;
            let d = &mut canvas.px[y as usize * cw + x as usize];
            for k in 0..3 {
                d[k] = acc[k] * a + d[k] * (1.0 - a);
            }
            d[3] = a + d[3] * (1.0 - a);
        }
    }
    Ok(())
}

/// [`draw_layer`] with a matte: the source region is premultiplied by the mask, both
/// passes carry alpha, and the layer's coverage × opacity scales the result.
fn draw_layer_masked(
    canvas: &mut RgbaImage,
    l: &LayerDraw,
    img: &Nv12Image,
    mask: &crate::matte::Mask,
    mrect: &gmclip_plan::types::Rect,
) -> Result<(), ComposeError> {
    let (c, p) = (l.crop, l.place);
    if c.w <= 0.0 || c.h <= 0.0 || p.w <= 0.0 || p.h <= 0.0 {
        return Err(ComposeError::Invalid(format!(
            "layer {}: empty crop or place",
            l.layer_id
        )));
    }
    let (cx0, cx1) = bounds(c.x, c.w, img.width);
    let (cy0, cy1) = bounds(c.y, c.h, img.height);
    if cx1 <= cx0 || cy1 <= cy0 {
        return Err(ComposeError::Invalid(format!(
            "layer {}: crop outside the source",
            l.layer_id
        )));
    }
    let (dx0, dx1) = bounds(p.x, p.w, canvas.width);
    let (dy0, dy1) = bounds(p.y, p.h, canvas.height);
    if dx1 <= dx0 || dy1 <= dy0 {
        return Ok(());
    }
    let (sx, sy) = ((c.w / p.w) as f32, (c.h / p.h) as f32);
    let radius = l.shape.as_ref().map(|f| f.radius_px(&p));
    let rw = (cx1 - cx0) as usize;
    let mut src = vec![[0f32; 4]; rw * (cy1 - cy0) as usize];
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            let m = mask_at(mask, mrect, x, y);
            let view = l.matte.as_ref().is_some_and(|m| m.view);
            let rgb = if view { [1.0; 3] } else { img.rgb_at(x, y) };
            src[(y - cy0) as usize * rw + (x - cx0) as usize] =
                [rgb[0] * m, rgb[1] * m, rgb[2] * m, m];
        }
    }
    let dw = (dx1 - dx0) as usize;
    let htaps: Vec<Vec<(i32, f32)>> = (dx0..dx1)
        .map(|x| {
            let u = (c.x + (f64::from(x) + 0.5 - p.x) * c.w / p.w) as f32;
            taps(u, sx.max(1.0), cx0, cx1)
        })
        .collect();
    let rows = (cy1 - cy0) as usize;
    let mut mid = vec![[0f32; 4]; dw * rows];
    for r in 0..rows {
        let row = &src[r * rw..(r + 1) * rw];
        for (i, tx) in htaps.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(k, w) in tx {
                let s = row[(k - cx0) as usize];
                for q in 0..4 {
                    acc[q] += w * s[q];
                }
            }
            mid[r * dw + i] = acc;
        }
    }
    let cw = canvas.width as usize;
    for y in dy0..dy1 {
        let v = (c.y + (f64::from(y) + 0.5 - p.y) * c.h / p.h) as f32;
        let ty = taps(v, sy.max(1.0), cy0, cy1);
        let cov_y = coverage(y, p.y, p.h);
        for (i, x) in (dx0..dx1).enumerate() {
            let mut acc = [0f32; 4];
            for &(k, w) in &ty {
                let s = mid[(k - cy0) as usize * dw + i];
                for q in 0..4 {
                    acc[q] += w * s[q];
                }
            }
            let cov = match radius {
                Some(r) => shape_alpha(x, y, &p, r),
                None => cov_y * coverage(x, p.x, p.w),
            } * l.opacity;
            let a = acc[3].clamp(0.0, 1.0) * cov;
            let d = &mut canvas.px[y as usize * cw + x as usize];
            for k in 0..3 {
                d[k] = acc[k] * cov + d[k] * (1.0 - a);
            }
            d[3] = a + d[3] * (1.0 - a);
        }
    }
    Ok(())
}

/// Composites one output frame on the CPU.
pub fn compose(
    draw: &FrameDraw,
    width: u32,
    height: u32,
    frames: &mut dyn FrameProvider,
) -> Result<RgbaImage, ComposeError> {
    compose_under(draw, width, height, frames, None)
}

/// [`compose`] with a still backdrop's tile (decision 0028) over the background colour,
/// under every layer.
pub fn compose_under(
    draw: &FrameDraw,
    width: u32,
    height: u32,
    frames: &mut dyn FrameProvider,
    under: Option<&std::sync::Arc<crate::text::raster::Tile>>,
) -> Result<RgbaImage, ComposeError> {
    compose_with(draw, width, height, frames, under, &[])
}

/// [`compose_under`] with image tiles drawn among the layers (decision 0042): `(i, t)`
/// draws `t` just before `draw.layers[i]` (after the last when `i` is past it).
pub fn compose_with(
    draw: &FrameDraw,
    width: u32,
    height: u32,
    frames: &mut dyn FrameProvider,
    under: Option<&std::sync::Arc<crate::text::raster::Tile>>,
    among: &[(usize, std::sync::Arc<crate::text::raster::Tile>)],
) -> Result<RgbaImage, ComposeError> {
    let bg = draw.background;
    let mut canvas = RgbaImage {
        width,
        height,
        px: vec![[bg[0], bg[1], bg[2], 1.0]; width as usize * height as usize],
    };
    if let Some(t) = under {
        super::text_layer::composite_cpu(&mut canvas, std::slice::from_ref(t));
    }
    if let Some(b) = &draw.backdrop {
        let img = frames.frame(&b.source, b.frame)?;
        draw_backdrop(&mut canvas, b, &img)?;
    }
    let tiles_before = |canvas: &mut RgbaImage, i: usize| {
        for (_, t) in among.iter().filter(|(k, _)| *k == i) {
            super::text_layer::composite_cpu(canvas, std::slice::from_ref(t));
        }
    };
    for (i, l) in draw.layers.iter().enumerate() {
        tiles_before(&mut canvas, i);
        let img = frames.frame(&l.source, l.frame)?;
        match &l.cover {
            Some(c) => draw_cover(&mut canvas, l, c, &img)?,
            None => {
                // decision 0046: the frame's shadow under the picture, its border over it
                if let Some(f) = &l.shape {
                    draw_frame(&mut canvas, l, f, FramePart::Shadow);
                }
                draw_layer(&mut canvas, l, &img)?;
                if let Some(f) = &l.shape {
                    draw_frame(&mut canvas, l, f, FramePart::Border);
                }
            }
        }
    }
    for (_, t) in among.iter().filter(|(k, _)| *k >= draw.layers.len()) {
        super::text_layer::composite_cpu(&mut canvas, std::slice::from_ref(t));
    }
    Ok(canvas)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum FramePart {
    Shadow,
    Border,
}

/// The pixel bounds and uniforms of a frame's shadow or border on a `cw × ch` canvas
/// (decision 0046): `(rect, r, soft, alpha, rgb, scissor)` where `rect` is the shape
/// (moved for a shadow), `soft` the shadow's blur or the border's width in px, and
/// `scissor` `[x0, x1, y0, y1)`; `None` when nothing of it is on the canvas or visible.
#[allow(clippy::type_complexity)]
pub(crate) fn frame_geometry(
    l: &LayerDraw,
    f: &super::draw::ShapeDraw,
    part: FramePart,
    cw: u32,
    ch: u32,
) -> Option<([f32; 4], f32, f32, f32, [f32; 3], [i32; 4])> {
    let p = l.place;
    if p.w <= 0.0 || p.h <= 0.0 {
        return None;
    }
    let short = cw.min(ch) as f32;
    let r = f.radius_px(&p);
    let rect = [p.x as f32, p.y as f32, p.w as f32, p.h as f32];
    let (rect, soft, alpha, rgb, grow) = match part {
        FramePart::Shadow => {
            let s = f.shadow?;
            let soft = s.blur * short;
            let dy = s.offset_y * short;
            (
                [rect[0], rect[1] + dy, rect[2], rect[3]],
                soft,
                s.opacity * l.opacity,
                s.rgb,
                soft * 0.5 + 1.0,
            )
        }
        FramePart::Border => {
            let b = f.border?;
            (rect, b.width * short, l.opacity, b.rgb, 1.0)
        }
    };
    if alpha <= 0.0 || (soft <= 0.0 && part == FramePart::Border) {
        return None;
    }
    let (x0, x1) = bounds(
        f64::from(rect[0] - grow),
        f64::from(rect[2] + 2.0 * grow),
        cw,
    );
    let (y0, y1) = bounds(
        f64::from(rect[1] - grow),
        f64::from(rect[3] + 2.0 * grow),
        ch,
    );
    (x1 > x0 && y1 > y0).then_some((rect, r, soft, alpha, rgb, [x0, x1, y0, y1]))
}

/// A frame's shadow (under the layer) or border (over it), premultiplied over (decision
/// 0046). The GPU's frame pass is the same in f32.
fn draw_frame(canvas: &mut RgbaImage, l: &LayerDraw, f: &super::draw::ShapeDraw, part: FramePart) {
    let Some((rect, r, soft, alpha, rgb, [x0, x1, y0, y1])) =
        frame_geometry(l, f, part, canvas.width, canvas.height)
    else {
        return;
    };
    let cw = canvas.width as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let d = rounded_sd(x as f32 + 0.5, y as f32 + 0.5, rect, r);
            let a = match part {
                FramePart::Shadow => shadow_falloff(d, soft),
                FramePart::Border => border_alpha(d, soft),
            } * alpha;
            if a <= 0.0 {
                continue;
            }
            let px = &mut canvas.px[y as usize * cw + x as usize];
            for k in 0..3 {
                px[k] = rgb[k] * a + px[k] * (1.0 - a);
            }
            px[3] = a + px[3] * (1.0 - a);
        }
    }
}

/// A cover layer (decision 0021): the crop through the layer path into the `grid`
/// picture, then back up into `place` — a cubic B-spline of the grid (blur) or its
/// nearest texel (pixelate) — or one colour (solid). Canvas pixel `X` reads grid
/// position `u = (X + ½ − place.x) · grid.w / place.w`; coverage of the place times the
/// opacity is the alpha, premultiplied over. The GPU's cover pass is the same in f32.
fn draw_cover(
    canvas: &mut RgbaImage,
    l: &LayerDraw,
    c: &CoverDraw,
    img: &Nv12Image,
) -> Result<(), ComposeError> {
    let p = l.place;
    if p.w <= 0.0 || p.h <= 0.0 {
        return Err(ComposeError::Invalid(format!(
            "layer {}: empty crop or place",
            l.layer_id
        )));
    }
    let (n, m) = (c.grid.0.max(1), c.grid.1.max(1));
    let small = match c.kind {
        CoverKind::Solid(_) => None,
        _ => {
            let mut small = RgbaImage {
                width: n,
                height: m,
                px: vec![[0.0, 0.0, 0.0, 1.0]; (n * m) as usize],
            };
            draw_layer(&mut small, &c.small_layer(l), img)?;
            Some(small)
        }
    };
    let (dx0, dx1) = bounds(p.x, p.w, canvas.width);
    let (dy0, dy1) = bounds(p.y, p.h, canvas.height);
    let (su, sv) = (n as f32 / p.w as f32, m as f32 / p.h as f32);
    let (px, py) = (p.x as f32, p.y as f32);
    let cw = canvas.width as usize;
    for y in dy0..dy1 {
        let v = (y as f32 + 0.5 - py) * sv;
        let cov_y = coverage(y, p.y, p.h);
        for x in dx0..dx1 {
            let u = (x as f32 + 0.5 - px) * su;
            let rgb = match (&c.kind, &small) {
                (CoverKind::Solid(rgb), _) => *rgb,
                (CoverKind::Pixelate, Some(s)) => {
                    let i = (u.floor() as i32).clamp(0, n as i32 - 1) as u32;
                    let j = (v.floor() as i32).clamp(0, m as i32 - 1) as u32;
                    let t = s.px[(j * n + i) as usize];
                    [t[0], t[1], t[2]]
                }
                (_, Some(s)) => {
                    let (uu, vv) = (u - 0.5, v - 0.5);
                    let (i, j) = (uu.floor() as i32, vv.floor() as i32);
                    let (wx, wy) = (bspline(uu - uu.floor()), bspline(vv - vv.floor()));
                    let mut acc = [0f32; 3];
                    for (dj, wyy) in wy.iter().enumerate() {
                        let yy = (j - 1 + dj as i32).clamp(0, m as i32 - 1) as u32;
                        for (di, wxx) in wx.iter().enumerate() {
                            let xx = (i - 1 + di as i32).clamp(0, n as i32 - 1) as u32;
                            let t = s.px[(yy * n + xx) as usize];
                            let w = wyy * wxx;
                            acc[0] += w * t[0];
                            acc[1] += w * t[1];
                            acc[2] += w * t[2];
                        }
                    }
                    acc
                }
                (_, None) => [0.0; 3],
            };
            let a = cov_y * coverage(x, p.x, p.w) * l.opacity;
            let d = &mut canvas.px[y as usize * cw + x as usize];
            for k in 0..3 {
                d[k] = rgb[k] * a + d[k] * (1.0 - a);
            }
            d[3] = a + d[3] * (1.0 - a);
        }
    }
    Ok(())
}

/// The blurred-source fill ([`BackdropDraw`]): down by the layer path, up by a B-spline.
fn draw_backdrop(
    canvas: &mut RgbaImage,
    b: &BackdropDraw,
    img: &Nv12Image,
) -> Result<(), ComposeError> {
    let (cw, ch) = (canvas.width, canvas.height);
    let ((sw, sh), place) = b.small(cw, ch);
    let mut small = RgbaImage {
        width: sw,
        height: sh,
        px: vec![[0.0, 0.0, 0.0, 1.0]; (sw * sh) as usize],
    };
    let layer = LayerDraw {
        layer_id: "backdrop".into(),
        source: b.source.clone(),
        frame: b.frame,
        crop: BackdropDraw::cover_crop(img.width, img.height, cw, ch),
        place,
        opacity: 1.0,
        still: false,
        matte: None,
        cover: None,
        shape: None,
    };
    draw_layer(&mut small, &layer, img)?;
    let k = 1.0 - b.dim;
    let scale = b.blur.max(1) as f32;
    for y in 0..ch {
        let v = (y as f32 + 0.5) / scale - 0.5;
        let (j, ty) = (v.floor() as i32, v - v.floor());
        let wy = bspline(ty);
        for x in 0..cw {
            let u = (x as f32 + 0.5) / scale - 0.5;
            let (i, tx) = (u.floor() as i32, u - u.floor());
            let wx = bspline(tx);
            let mut acc = [0f32; 3];
            for (dj, wyy) in wy.iter().enumerate() {
                let yy = (j - 1 + dj as i32).clamp(0, sh as i32 - 1) as u32;
                for (di, wxx) in wx.iter().enumerate() {
                    let xx = (i - 1 + di as i32).clamp(0, sw as i32 - 1) as u32;
                    let s = small.px[(yy * sw + xx) as usize];
                    let w = wyy * wxx;
                    acc[0] += w * s[0];
                    acc[1] += w * s[1];
                    acc[2] += w * s[2];
                }
            }
            canvas.px[(y * cw + x) as usize] = [acc[0] * k, acc[1] * k, acc[2] * k, 1.0];
        }
    }
    Ok(())
}
