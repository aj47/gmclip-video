//! RenderPlan v0 → what to draw on one output frame (RP rule 5).

use std::sync::Arc;

use gmclip_plan::Plan;
use gmclip_plan::retime::{self, PtsClock};
use gmclip_plan::types::{Background, LayerCover, LayerMatte, Rect, TimeMap};

use crate::matte::Mask;

/// Errors building a frame's draw list.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ComposeError {
    #[error("output frame {0} is not inside any part")]
    NoPart(u64),
    #[error("unsupported in the M0 compositor: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
    #[error("source frame unavailable: {0}")]
    Source(String),
    #[error("gpu: {0}")]
    Gpu(String),
    /// A font or text event that cannot be drawn (H-06).
    #[error("text: {0}")]
    Text(String),
}

/// One layer to draw: a crop of `source`'s presentation frame `frame` into `place`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerDraw {
    pub layer_id: String,
    pub source: String,
    /// Absolute presentation index in `source`.
    pub frame: u64,
    pub crop: Rect,
    pub place: Rect,
    pub opacity: f32,
    /// `hold` or `freeze_first`: the same source frame on every output frame of the
    /// part (the export path caches it instead of streaming).
    pub still: bool,
    /// A roto matte (decision 0013): its spec and span from the plan, and the finished
    /// mask once the compose step has resolved it.
    pub matte: Option<MatteDraw>,
    /// A cover layer (decision 0021): the crop is drawn into `place` obscured.
    pub cover: Option<CoverDraw>,
    /// The layer's frame (decision 0046): rounded corners, a shadow, a border.
    pub shape: Option<ShapeDraw>,
}

/// A layer's frame on one frame (decision 0046, RP `layers[].frame`): `radius` a fraction
/// of the placed rectangle's short side; the shadow's `blur` and `offset_y` and the
/// border's `width` fractions of the canvas's short side (the canvas drawn into, so a
/// preview at display size draws the same frame scaled).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeDraw {
    pub radius: f32,
    pub shadow: Option<ShadowDraw>,
    pub border: Option<BorderDraw>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShadowDraw {
    pub blur: f32,
    pub offset_y: f32,
    pub opacity: f32,
    /// R'G'B' in [0, 1].
    pub rgb: [f32; 3],
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BorderDraw {
    pub width: f32,
    /// R'G'B' in [0, 1].
    pub rgb: [f32; 3],
}

impl ShapeDraw {
    /// The draw of a plan frame.
    pub fn of(f: &gmclip_plan::types::LayerFrame) -> Result<Self, ComposeError> {
        Ok(Self {
            radius: f.radius.clamp(0.0, 0.5) as f32,
            shadow: f
                .shadow
                .as_ref()
                .map(|s| -> Result<ShadowDraw, ComposeError> {
                    Ok(ShadowDraw {
                        blur: s.blur.max(0.0) as f32,
                        offset_y: s.offset_y as f32,
                        opacity: s.opacity.clamp(0.0, 1.0) as f32,
                        rgb: parse_rgb(s.rgb.as_str())?,
                    })
                })
                .transpose()?,
            border: f
                .border
                .as_ref()
                .map(|b| -> Result<BorderDraw, ComposeError> {
                    Ok(BorderDraw {
                        width: b.width.max(0.0) as f32,
                        rgb: parse_rgb(b.rgb.as_str())?,
                    })
                })
                .transpose()?,
        })
    }

    /// The corner radius in px for a place (clamped to half its short side).
    pub fn radius_px(&self, place: &Rect) -> f32 {
        let short = (place.w.min(place.h)) as f32;
        (self.radius * short).clamp(0.0, short / 2.0)
    }
}

/// The signed distance (px, negative inside) from `(px, py)` to the edge of the rectangle
/// `(x, y, w, h)` with corners of radius `r`. The GPU shaders compute the same in f32.
pub fn rounded_sd(px: f32, py: f32, rect: [f32; 4], r: f32) -> f32 {
    let (hw, hh) = (rect[2] * 0.5, rect[3] * 0.5);
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (px - (rect[0] + hw)).abs() - (hw - r);
    let qy = (py - (rect[1] + hh)).abs() - (hh - r);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// The picture's alpha from its frame at canvas pixel `(x, y)`: `clamp(½ − d, 0, 1)`.
pub fn shape_alpha(x: i32, y: i32, place: &Rect, r: f32) -> f32 {
    let d = rounded_sd(
        x as f32 + 0.5,
        y as f32 + 0.5,
        [
            place.x as f32,
            place.y as f32,
            place.w as f32,
            place.h as f32,
        ],
        r,
    );
    (0.5 - d).clamp(0.0, 1.0)
}

/// A frame's shadow alpha (before its opacity) at distance `d` from the moved shape's
/// edge, softened over `soft` px: smoothstep of `clamp(½ − d / soft)`; a hard edge when
/// `soft < 1`.
pub fn shadow_falloff(d: f32, soft: f32) -> f32 {
    if soft < 1.0 {
        return (0.5 - d).clamp(0.0, 1.0);
    }
    let t = (0.5 - d / soft).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A frame's border alpha (before the layer's opacity) at distance `d`, `width` px wide.
pub fn border_alpha(d: f32, width: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0) - (0.5 - d - width).clamp(0.0, 1.0)
}

/// The layer id of a backdrop video in a frame's draw list (decision 0028).
pub const BACKDROP_LAYER: &str = "@backdrop";

/// The layer id prefix of a clip event in a frame's draw list (decision 0028).
pub const CLIP_LAYER: &str = "@clip:";

impl LayerDraw {
    /// A layer that is not one of the part's own: the backdrop video or a clip event
    /// (decision 0028). Their ids start with `@`, which no plan id may.
    pub fn is_extra(&self) -> bool {
        self.layer_id.starts_with('@')
    }

    /// The decode stream that serves this layer: the source's own for the part's layers;
    /// the backdrop and the clip events read one stream of their own per source, so a part
    /// and a clip of one recording do not seek each other's decoder every frame.
    pub fn stream(&self) -> String {
        if self.layer_id == BACKDROP_LAYER {
            BACKDROP_LAYER.to_owned()
        } else if self.layer_id.starts_with(CLIP_LAYER) {
            format!("{CLIP_LAYER}{}", self.source)
        } else {
            self.source.clone()
        }
    }
}

/// The decode streams a plan reads, `(stream, source)` in first-use order: each picture
/// source of the parts' layers, the backdrop video and each clip event source
/// ([`LayerDraw::stream`]).
pub fn plan_streams(plan: &Plan) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |k: String, s: String| {
        if !out.iter().any(|(a, _)| *a == k) {
            out.push((k, s));
        }
    };
    for p in &plan.parts {
        for l in &p.layers {
            let s = l
                .picture_source
                .as_ref()
                .unwrap_or(&p.picture.source)
                .as_str()
                .to_owned();
            add(s.clone(), s);
        }
    }
    if let Background::Backdrop { video: Some(v), .. } = &plan.background {
        add(BACKDROP_LAYER.to_owned(), v.source.as_str().to_owned());
    }
    for e in plan.clip_events.iter().flatten() {
        let s = e.picture.source.as_str().to_owned();
        add(format!("{CLIP_LAYER}{s}"), s);
    }
    out
}

/// How a cover layer draws (decision 0021): the crop goes through the layer path into a
/// `grid` picture (`place / k`, from the plan's canvas px, so a smaller preview resamples
/// through the same picture), then back up into `place` — a cubic B-spline (blur) or
/// nearest (pixelate) — or the place is filled with one colour (solid; no picture).
#[derive(Clone, Debug, PartialEq)]
pub struct CoverDraw {
    pub kind: CoverKind,
    pub grid: (u32, u32),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CoverKind {
    Blur,
    Pixelate,
    /// Opaque R'G'B' in [0, 1].
    Solid([f32; 3]),
}

impl CoverDraw {
    /// The draw of a plan cover whose place is `place` (plan canvas px).
    pub fn of(c: &LayerCover, place: Rect) -> Result<Self, ComposeError> {
        Ok(Self {
            kind: match c {
                LayerCover::Blur { .. } => CoverKind::Blur,
                LayerCover::Pixelate { .. } => CoverKind::Pixelate,
                LayerCover::Solid { rgb } => CoverKind::Solid(parse_rgb(rgb.as_str())?),
            },
            grid: c.grid(place.w, place.h),
        })
    }

    /// The layer that draws the crop into the grid picture (opaque, whole texels).
    pub fn small_layer(&self, l: &LayerDraw) -> LayerDraw {
        LayerDraw {
            place: Rect {
                x: 0.0,
                y: 0.0,
                w: f64::from(self.grid.0),
                h: f64::from(self.grid.1),
            },
            opacity: 1.0,
            matte: None,
            cover: None,
            shape: None,
            ..l.clone()
        }
    }
}

/// A layer's roto matte on one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct MatteDraw {
    pub spec: LayerMatte,
    /// The track span: first source frame and frame count (1 for held / frozen layers).
    pub span: (u64, u64),
    /// The finished mask (soften and strokes applied) and where it lies in the
    /// picture's pixels: `(x, y, w, h)`. `None` until resolved; a layer with a matte and
    /// no mask is refused by the compositor.
    pub mask: Option<(Arc<Mask>, Rect)>,
    /// The mask is a draft (live `.fast` segmentation), not the export track.
    pub draft: bool,
    /// Matte view (preview only): draw the mask itself, white where the person is kept.
    pub view: bool,
}

/// The `/1` blurred-source background (decision 0014): the part's picture on this frame,
/// cropped to the canvas aspect (centred, cover), resampled down to `canvas / blur` by
/// the layer path (Lanczos-3, widened by the downscale: the low-pass), back up to the
/// canvas with a cubic B-spline, and dimmed.
#[derive(Clone, Debug, PartialEq)]
pub struct BackdropDraw {
    pub source: String,
    pub frame: u64,
    pub blur: u32,
    pub dim: f32,
}

impl BackdropDraw {
    /// The centred crop of a `w × h` picture with the canvas's aspect.
    pub fn cover_crop(w: u32, h: u32, cw: u32, ch: u32) -> Rect {
        let (w, h, cw, ch) = (f64::from(w), f64::from(h), f64::from(cw), f64::from(ch));
        if w * ch > h * cw {
            let cropw = h * cw / ch;
            Rect {
                x: (w - cropw) / 2.0,
                y: 0.0,
                w: cropw,
                h,
            }
        } else {
            let croph = w * ch / cw;
            Rect {
                x: 0.0,
                y: (h - croph) / 2.0,
                w,
                h: croph,
            }
        }
    }

    /// The small picture's size for a `cw × ch` canvas, and its place in it.
    pub fn small(&self, cw: u32, ch: u32) -> ((u32, u32), Rect) {
        let b = f64::from(self.blur.max(1));
        let (pw, ph) = (f64::from(cw) / b, f64::from(ch) / b);
        (
            (pw.ceil().max(1.0) as u32, ph.ceil().max(1.0) as u32),
            Rect {
                x: 0.0,
                y: 0.0,
                w: pw,
                h: ph,
            },
        )
    }
}

/// Cubic B-spline weights for fraction `t` (taps at −1, 0, +1, +2).
pub fn bspline(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        (1.0 - t) * (1.0 - t) * (1.0 - t) / 6.0,
        (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0,
        (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0,
        t3 / 6.0,
    ]
}

/// Everything drawn on one output frame, bottom to top.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameDraw {
    pub out_frame: u64,
    pub part_id: String,
    /// Opaque R'G'B' background in [0, 1].
    pub background: [f32; 3],
    /// The blurred-source fill over the background, under every layer.
    pub backdrop: Option<BackdropDraw>,
    pub layers: Vec<LayerDraw>,
    /// Image events drawn among the layers (decision 0042): `(i, id)` draws the image
    /// event `id` just before `layers[i]` (`i == layers.len()`: after the last), in list
    /// order. Nothing may reorder `layers` once these are set.
    pub tiles_at: Vec<(usize, String)>,
}

fn parse_rgb(hex: &str) -> Result<[f32; 3], ComposeError> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return Err(ComposeError::Invalid(format!("colour {hex}")));
    }
    let c = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .map(|v| f32::from(v) / 255.0)
            .map_err(|_| ComposeError::Invalid(format!("colour {hex}")))
    };
    Ok([c(0)?, c(2)?, c(4)?])
}

/// The background colour of a plan (a plate's is its first stop; its gradient is a tile,
/// [`crate::compositor::text_layer::plate_tile`]).
pub fn background(plan: &Plan) -> Result<[f32; 3], ComposeError> {
    match &plan.background {
        Background::Colour { rgb } => parse_rgb(rgb.as_str()),
        // decision 0029: the plate's first stop under its gradient tile
        Background::Plate { stops } => parse_rgb(
            stops
                .first()
                .ok_or_else(|| ComposeError::Invalid("a plate without stops".into()))?
                .as_str(),
        ),
        // the colour under the blurred picture (where no picture is drawn)
        Background::BlurredSource { rgb, .. } => parse_rgb(rgb.as_str()),
        // the colour where the backdrop does not cover (decision 0028)
        Background::Backdrop { rgb, .. } => parse_rgb(rgb.as_str()),
    }
}

/// Each source's presentation clock, for parts that convert their picture's rate
/// (`picture.fps`, decision 0017). The export and the preview hand the one compose
/// their decoders' clocks; without one, a converting part is read at its nominal rate.
pub trait SourceClocks {
    fn clock(&self, source: &str) -> Option<&PtsClock>;
}

/// No clocks: every converting part at its nominal rate.
#[derive(Clone, Copy, Debug, Default)]
pub struct NominalClocks;

impl SourceClocks for NominalClocks {
    fn clock(&self, _: &str) -> Option<&PtsClock> {
        None
    }
}

/// [`frame_draw_with`] at nominal rates ([`NominalClocks`]).
pub fn frame_draw(plan: &Plan, out_frame: u64) -> Result<FrameDraw, ComposeError> {
    frame_draw_with(plan, out_frame, &NominalClocks)
}

/// The draw list for output frame `out_frame`.
///
/// Source frame per layer (`picture_source` defaults to the part's picture source):
/// - `live` (or absent): `picture.first_frame + (out_frame − part.out[0])`; with
///   `picture.fps` (decision 0017), the source frame presented at or before source time
///   `(first_frame + out_frame − out[0]) / output.fps`, by `clocks` ([`retime`]); a
///   layer of another source counts from its own `first_frame`, by time with its `fps`
///   (decisions 0046, 0047);
/// - `freeze_first`: the live frame of the part's first output frame;
/// - `hold {frame}`: `frame` (absolute; may lie outside the part).
pub fn frame_draw_with(
    plan: &Plan,
    out_frame: u64,
    clocks: &dyn SourceClocks,
) -> Result<FrameDraw, ComposeError> {
    let part = plan
        .parts
        .iter()
        .find(|p| p.out.start() <= out_frame && out_frame < p.out.end())
        .ok_or(ComposeError::NoPart(out_frame))?;
    let offset = out_frame - part.out.start();
    let fps = plan.output.fps;
    let clock = clocks.clock(part.picture.source.as_str());
    let live = retime::picture_frame(part, offset, fps, clock);
    let first = retime::picture_frame(part, 0, fps, clock);
    let mut order: Vec<usize> = (0..part.layers.len()).collect();
    order.sort_by_key(|&i| part.layers[i].z); // stable: array order breaks ties
    let mut layers = Vec::new();
    // the z of each drawn part layer, for the events drawn among them (decision 0042)
    let mut zs: Vec<i64> = Vec::new();
    for i in order {
        let l = &part.layers[i];
        let opacity = l.opacity() as f32;
        if !l.is_visible() || opacity <= 0.0 {
            continue;
        }
        let source = l
            .picture_source
            .as_ref()
            .unwrap_or(&part.picture.source)
            .as_str()
            .to_owned();
        let other = l
            .picture_source
            .as_ref()
            .is_some_and(|s| s != &part.picture.source);
        // decision 0046: another source read live from its `first_frame`, on the part's
        // own output clock (validation keeps it off sped parts); at
        // another rate, by that source's clock (decision 0047)
        let (live, first) = match (other, l.first_frame.is_some()) {
            (true, true) => {
                let c = clocks.clock(&source);
                (
                    retime::layer_frame(l, offset, fps, c).unwrap_or(live),
                    retime::layer_frame(l, 0, fps, c).unwrap_or(first),
                )
            }
            _ => (live, first),
        };
        if other && l.first_frame.is_none() && !matches!(l.time_map, Some(TimeMap::Hold { .. })) {
            // RP rule 5 defines a frame of another source only for `hold` (and, `/1`,
            // with `first_frame`).
            return Err(ComposeError::Unsupported(format!(
                "layer {}: live/freeze_first on a picture_source other than the part's",
                l.id.as_str()
            )));
        }
        let frame = match &l.time_map {
            None | Some(TimeMap::Live) => live,
            Some(TimeMap::FreezeFirst) => first,
            Some(TimeMap::Hold { frame, .. }) => *frame,
        };
        let still = matches!(
            l.time_map,
            Some(TimeMap::FreezeFirst | TimeMap::Hold { .. })
        );
        let matte = l.matte.as_ref().map(|m| MatteDraw {
            spec: m.clone(),
            span: if still {
                (frame, 1)
            } else {
                retime::picture_span(part, fps)
            },
            mask: None,
            draft: false,
            view: false,
        });
        // an eased visual split (`/1`, decision 0016): crop, place and opacity move to
        // the layer's own over the part's first frames
        let (crop, place, opacity) = match &l.ease {
            Some(e) => {
                let (c, p, o) = e.at(offset, l.crop, l.place, f64::from(opacity));
                (c, p, o as f32)
            }
            None => (l.crop, l.place, opacity),
        };
        // a cover pinned to a moving point (`/1`, decision 0033): moved in whole pixels
        let (crop, place) = match &l.track {
            Some(t) => t.moved(out_frame, crop, place),
            None => (crop, place),
        };
        if opacity <= 0.0 {
            continue;
        }
        let cover = l
            .cover
            .as_ref()
            .map(|c| CoverDraw::of(c, place))
            .transpose()?;
        // decision 0046: a layer's frame (never on a cover)
        let shape = match (&l.frame, &cover) {
            (Some(f), None) => Some(ShapeDraw::of(f)?),
            _ => None,
        };
        layers.push(LayerDraw {
            layer_id: l.id.as_str().to_owned(),
            source,
            frame,
            crop,
            place,
            opacity: opacity.min(1.0),
            still,
            matte,
            cover,
            shape,
        });
        zs.push(l.z);
    }
    // decision 0028: the backdrop video under every layer, on the output clock (so it runs
    // on across part joins), and the clip events above them, by `layer` then array order;
    // decision 0042: a clip or image event with `z` is drawn among the part's layers
    // instead, just under the first one whose `z` is at least its own
    let mut stack: Vec<LayerDraw> = Vec::with_capacity(layers.len() + 2);
    if let Background::Backdrop {
        crop,
        place,
        video: Some(v),
        ..
    } = &plan.background
    {
        let source = v.source.as_str().to_owned();
        let frame = retime::clip_frame(v, out_frame, fps, clocks.clock(&source));
        stack.push(LayerDraw {
            layer_id: BACKDROP_LAYER.to_owned(),
            source,
            frame,
            crop: *crop,
            place: *place,
            opacity: 1.0,
            still: false,
            matte: None,
            cover: None,
            shape: None,
        });
    }
    let clip_events = plan.clip_events.as_deref().unwrap_or_default();
    let clip_draw = |e: &gmclip_plan::types::ClipEvent| -> Option<LayerDraw> {
        let opacity = e.opacity.unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        if opacity <= 0.0 {
            return None;
        }
        let source = e.picture.source.as_str().to_owned();
        let frame = retime::clip_frame(
            &e.picture,
            out_frame - e.out.start(),
            fps,
            clocks.clock(&source),
        );
        let matte = e.matte.as_ref().map(|m| MatteDraw {
            spec: m.clone(),
            span: retime::clip_span(e, fps),
            mask: None,
            draft: false,
            view: false,
        });
        Some(LayerDraw {
            layer_id: format!("{CLIP_LAYER}{}", e.id.as_str()),
            source,
            frame,
            crop: e.crop,
            place: e.place,
            opacity,
            still: false,
            matte,
            cover: None,
            shape: None,
        })
    };
    let on = |out: gmclip_plan::scalars::Range| out.start() <= out_frame && out_frame < out.end();
    // the events among the layers: (before part layer k, z, layer, clip 0 / image 1, index)
    let at = |z: i64| zs.iter().position(|&lz| lz >= z).unwrap_or(zs.len());
    let mut among: Vec<(usize, i64, i64, u8, usize)> = Vec::new();
    for (i, e) in clip_events.iter().enumerate() {
        if let Some(z) = e.z
            && on(e.out)
        {
            among.push((at(z), z, e.layer.unwrap_or(0), 0, i));
        }
    }
    let image_events = plan.image_events.as_deref().unwrap_or_default();
    for (i, e) in image_events.iter().enumerate() {
        if let Some(z) = e.z
            && on(e.out)
        {
            among.push((at(z), z, e.layer.unwrap_or(0), 1, i));
        }
    }
    among.sort_unstable();
    let mut tiles_at = Vec::new();
    let mut next = among.iter().peekable();
    let n = layers.len();
    for (k, l) in layers
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None))
        .enumerate()
    {
        while let Some(&&(pos, _, _, kind, i)) = next.peek()
            && pos <= k
        {
            next.next();
            if kind == 0 {
                stack.extend(clip_draw(&clip_events[i]));
            } else {
                tiles_at.push((stack.len(), image_events[i].id.as_str().to_owned()));
            }
        }
        if k < n {
            stack.extend(l);
        }
    }
    let mut clips: Vec<(i64, usize)> = clip_events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.z.is_none() && on(e.out))
        .map(|(i, e)| (e.layer.unwrap_or(0), i))
        .collect();
    clips.sort_unstable();
    for (_, i) in clips {
        stack.extend(clip_draw(&clip_events[i]));
    }
    let layers = stack;
    let backdrop = match &plan.background {
        Background::BlurredSource { blur, dim, .. } => Some(BackdropDraw {
            source: part.picture.source.as_str().to_owned(),
            frame: live,
            blur: (*blur).clamp(1, 64) as u32,
            dim: dim.clamp(0.0, 1.0) as f32,
        }),
        _ => None,
    };
    Ok(FrameDraw {
        out_frame,
        part_id: part.id.as_str().to_owned(),
        background: background(plan)?,
        backdrop,
        layers,
        tiles_at,
    })
}
