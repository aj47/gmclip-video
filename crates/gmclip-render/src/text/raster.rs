//! Cue raster (B-09; SPEC §7 Text, §5.9 Text metrics; RP §2.1 rule 6).
//!
//! One [`TextEvent`] → one premultiplied RGBA8 [`Tile`] at an integer canvas anchor.
//!
//! - **Shaping**: parley 0.12 (HarfBuzz-equivalent shaping via harfrust), one layout per
//!   `\n`-separated line and colour run, with libass's feature set: `kern` off (an ASS script
//!   kerns only with `Kerning: yes`), `liga`/`clig` off whenever letter spacing ≠ 0.
//! - **Layout** reproduces libass 0.15, the renderer the reference burned its captions with (S4): the em
//!   and line metrics come from FreeType's REAL_DIM sizing of the win-metric span (the per-font
//!   em rule, SPEC Conventions); each advance is the *hinted* advance at 256 px scaled by
//!   `size/256` to 1/64 px; spacing is added once per cluster; lines are centred on their summed
//!   advances; glyph origins land on 1/8 px; GPOS offsets are not applied (none in Latin text).
//!   Lines advance by the 1/64-rounded ascent + descent, which is `size` to within 0.0125 px.
//! - **Geometry** is ours, so the bytes do not depend on any platform's libm: unhinted glyph
//!   outlines (skrifa) flattened with `+ − × ÷ √` only; the round-joined outline is the
//!   Minkowski sum of the glyph with a disc of radius `outline.width` (glyph ∪ an outward half-
//!   rectangle per edge ∪ an arc wedge per vertex, arcs by a fixed rotation constant, contours
//!   re-wound by nesting depth so a non-zero fill is the union); the shadow is that union (or
//!   the fill) offset by `(dx, dy)`. Boxes: `geometry: libass_bs3` draws libass's per-(line,
//!   run) boxes padded by ≥ 1 px; otherwise one box per line with exactly `pad`, `radius`
//!   rounded. Points are quantised to 1/256 px before rasterising.
//! - **Raster**: vello_cpu 0.3 at `Level::fallback()` (scalar), single-threaded, u8 pipeline.
//!   Shadow and outline go through A8 masks (§7): pixels ≥ 250 inside a union are set to 255,
//!   because f32 accumulation over overlapping pieces leaves interior pixels at 253–254; a
//!   shadow with `blur` is then blurred by our fixed-point Gaussian. Order: shadow → box →
//!   outline → fills (each run in its colour).
//! - Limits are errors, never clamps: non-finite or out-of-range numbers, more than
//!   [`MAX_CHARS`] characters, a tile over [`MAX_SIDE`] px. No font fallback at M0 (a missing
//!   glyph draws as `.notdef`).
//!
//! The renderer never fits, wraps or shrinks (rule 6): `size` is drawn as given.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use parley::fontique::{Blob, CollectionOptions, FontInfoOverride, SourceCache};
use parley::{
    FontContext, FontFamily, FontFeatures, LayoutContext, PositionedLayoutItem, StyleProperty,
};
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use vello_cpu::color::AlphaColor;
use vello_cpu::color::Srgb;
use vello_cpu::kurbo::{BezPath, Point as KPoint};
use vello_cpu::peniko::Fill;
use vello_cpu::{
    Level, Pixmap, RasterizerSettings, RenderContext, RenderMode, RenderSettings, Resources,
    TargetInit,
};

use super::font::FontFace;
use super::{Colour, ShadowOf, TextEvent, TextStyle};

/// Flattening tolerance, px.
const FLATTEN_TOL: f64 = 0.02;
/// Sub-pixel grid every point is snapped to before rasterising.
const QUANT: f64 = 256.0;
/// `cos(π/64)`, `sin(π/64)`: the arc step (2.8125°); sagitta at r = 12 px is 0.004 px.
const ARC_COS: f64 = 0.998_795_456_205_172_4;
const ARC_SIN: f64 = 0.049_067_674_327_418_015;
/// Largest tile side, px (a 4K canvas side; nothing off-canvas is ever shown).
pub const MAX_SIDE: f64 = 4096.0;
/// Most characters in one event.
pub const MAX_CHARS: usize = 4096;
/// A union pixel at or above this coverage is set to full (see the module doc).
const SOLID: u8 = 250;

/// A text event could not be drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterError(pub String);

impl fmt::Display for RasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "text raster: {}", self.0)
    }
}

impl std::error::Error for RasterError {}

/// A rasterised event: premultiplied RGBA8, row-major, `width × height`, whose top-left pixel
/// sits at canvas pixel `(x, y)`.
#[derive(Clone, PartialEq, Eq)]
pub struct Tile {
    /// Canvas x of the tile's left column.
    pub x: i32,
    /// Canvas y of the tile's top row.
    pub y: i32,
    /// Width in px.
    pub width: u32,
    /// Height in px.
    pub height: u32,
    /// Premultiplied RGBA8.
    pub rgba: Vec<u8>,
}

impl fmt::Debug for Tile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Tile({}x{} at {},{}; fnv {:016x})",
            self.width,
            self.height,
            self.x,
            self.y,
            self.fnv1a()
        )
    }
}

impl Tile {
    /// FNV-1a 64 over the anchor, size and bytes (the cross-machine identity check).
    pub fn fnv1a(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let head = [
            self.x.to_le_bytes(),
            self.y.to_le_bytes(),
            self.width.to_le_bytes(),
            self.height.to_le_bytes(),
        ];
        for b in head.iter().flatten().chain(self.rgba.iter()) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Composites the tile over a straight-alpha RGBA8 canvas of `cw × ch` (src-over; for tests
    /// and the S4 comparison, not the compositor).
    pub fn composite_over(&self, canvas: &mut [u8], cw: u32, ch: u32) {
        if canvas.len() < cw as usize * ch as usize * 4
            || self.rgba.len() < self.width as usize * self.height as usize * 4
        {
            return;
        }
        for ty in 0..self.height {
            let cy = i64::from(self.y) + i64::from(ty);
            if cy < 0 || cy >= i64::from(ch) {
                continue;
            }
            for tx in 0..self.width {
                let cx = i64::from(self.x) + i64::from(tx);
                if cx < 0 || cx >= i64::from(cw) {
                    continue;
                }
                let s = ((ty * self.width + tx) * 4) as usize;
                let d = ((cy as u32 * cw + cx as u32) * 4) as usize;
                let sa = u32::from(self.rgba[s + 3]);
                if sa == 0 {
                    continue;
                }
                let da = u32::from(canvas[d + 3]);
                // out = src + dst·(1 − sa), in premultiplied space, then back to straight.
                let oa = sa * 255 + da * (255 - sa); // ×255
                for c in 0..3 {
                    let sp = u32::from(self.rgba[s + c]) * 255; // ×255
                    let dp = u32::from(canvas[d + c]) * da * (255 - sa) / 255; // ×255
                    canvas[d + c] = ((sp + dp) * 255)
                        .checked_div(oa)
                        .map_or(0, |v| v.min(255) as u8);
                }
                canvas[d + 3] = (oa / 255).min(255) as u8;
            }
        }
    }
}

/// Registered fonts plus the parley contexts; one per thread (cues rasterise in parallel,
/// one context each).
pub struct TextRaster {
    font_cx: FontContext,
    layout_cx: LayoutContext<u16>,
    fonts: BTreeMap<String, (FontFace, String, u64)>,
    /// Apply the `kern` feature. Off by default, as libass shapes an ASS script without
    /// kerning unless its `[Script Info]` says `Kerning: yes` (no the reference script does). Not part of
    /// a RenderPlan: export leaves it off (a tile cache must key on it if anything sets it).
    pub kerning: bool,
}

impl fmt::Debug for TextRaster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextRaster")
            .field("fonts", &self.fonts.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Default for TextRaster {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRaster {
    /// A raster with no fonts. System fonts are never consulted (export fonts are blobs, SPEC §7).
    pub fn new() -> Self {
        let collection = parley::fontique::Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        Self {
            font_cx: FontContext {
                collection,
                source_cache: SourceCache::default(),
            },
            layout_cx: LayoutContext::new(),
            fonts: BTreeMap::new(),
            kerning: false,
        }
    }

    /// Registers a face under its RenderPlan `fonts[].id`.
    pub fn add_font(&mut self, id: &str, face: FontFace) -> Result<(), RasterError> {
        // Each id gets its own family name, so two files of one family (Figtree Regular and
        // Black) never stand in for each other; the shaped run's font is checked per run.
        let family = format!("gmclip:{id}");
        let blob = Blob::new(Arc::new(face.data().as_ref().clone()));
        let blob_id = blob.id();
        let over = FontInfoOverride {
            family_name: Some(&family),
            ..FontInfoOverride::default()
        };
        let families = self.font_cx.collection.register_fonts(blob, Some(over));
        if families.is_empty() {
            return Err(RasterError(format!("font {id}: no face in the file")));
        }
        self.fonts.insert(id.to_owned(), (face, family, blob_id));
        Ok(())
    }

    /// Rasterises one event with its style (sizes final, rule 6).
    pub fn render(&mut self, ev: &TextEvent, style: &TextStyle) -> Result<Tile, RasterError> {
        let geo = self.geometry(ev, style)?;
        geo.rasterise()
    }

    /// Lays out and builds the event's geometry in canvas pixels.
    ///
    /// Layout follows libass 0.15 (the renderer the reference burned its captions with), so positions
    /// match golden renders: glyphs are shaped (parley; `kern` off unless [`Self::kerning`]), then
    /// placed on libass's 26.6 pen. libass shapes at a FreeType size of 256 px (REAL_DIM, the
    /// win-metric span) with *hinted* metrics, so each advance is the whole-pixel-rounded
    /// advance at 256 scaled by `size/256` and rounded to 1/64 px; `Spacing` is added once per
    /// cluster; lines are centred by their summed advances; glyph origins land on 1/8 px.
    pub fn geometry(&mut self, ev: &TextEvent, style: &TextStyle) -> Result<Geometry, RasterError> {
        let st = Resolved::new(style)?;
        let (face, family, blob_id) = self
            .fonts
            .get(style.font.as_str())
            .ok_or_else(|| RasterError(format!("font {:?} not registered", style.font.as_str())))?;
        let (face, family, blob_id) = (face.clone(), family.clone(), *blob_id);
        let size = ev.size;
        if !(size.is_finite() && size > 0.0 && size <= 2048.0) {
            return Err(RasterError(format!("size {size} outside (0, 2048]")));
        }
        if !(ev.pos.x.is_finite()
            && ev.pos.y.is_finite()
            && ev.pos.x.abs() <= 1e5
            && ev.pos.y.abs() <= 1e5)
        {
            return Err(RasterError("pos must be finite and within ±1e5".into()));
        }
        let align = ev.align.or(style.align).unwrap_or(2);
        if !(1..=9).contains(&align) {
            return Err(RasterError(format!("align {align} outside 1–9")));
        }
        let chars: usize = ev.runs.iter().map(|r| r.text.chars().count()).sum();
        if chars > MAX_CHARS {
            return Err(RasterError(format!(
                "{chars} characters (at most {MAX_CHARS})"
            )));
        }
        let m = face.metrics();
        let span = i64::from(m.ascent + m.descent);
        // FreeType at REAL_DIM 256 px: x_scale = FT_DivFix(256·64, asc + desc) (16.16).
        let xs = ft_div_fix(256 * 64, span);
        let k = size / 256.0;
        let em = f64::from(m.units_per_em) * xs as f64 / 65536.0 / 64.0 * k;
        let asc = ft_mul_fix(i64::from(m.ascent), xs) as f64 * k / 64.0;
        let desc = ft_mul_fix(i64::from(m.descent), xs) as f64 * k / 64.0;
        let hsp26 = d6(st.letter_spacing);
        let fills: Vec<Colour> = ev
            .runs
            .iter()
            .map(|r| r.fill.as_ref().map_or(st.fill, Colour::from_plan))
            .collect();
        let font = face.skrifa();
        let hmtx = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        let adv26 = |gid: u32| -> i64 {
            let units = hmtx.advance_width(skrifa::GlyphId::new(gid)).unwrap_or(0.0) as i64;
            let hinted = (ft_mul_fix(units, xs) + 32) & -64; // FT_PIX_ROUND (grid-fitted metrics)
            (hinted as f64 * k).round_ties_even() as i64 // lrint(x_advance · scale_x)
        };

        // Lines of (text, brush) pieces; '\n' breaks lines.
        let mut lines: Vec<Vec<(String, u16)>> = vec![Vec::new()];
        for (i, run) in ev.runs.iter().enumerate() {
            let brush = u16::try_from(i).map_err(|_| RasterError("too many runs".into()))?;
            for (n, piece) in run.text.split('\n').enumerate() {
                if n > 0 {
                    lines.push(Vec::new());
                }
                if !piece.is_empty() {
                    lines
                        .last_mut()
                        .expect("non-empty")
                        .push((piece.to_owned(), brush));
                }
            }
        }

        // Shape; pen positions in 26.6 from the line start.
        let mut shaped: Vec<ShapedLine> = Vec::with_capacity(lines.len());
        for line in &lines {
            let mut glyphs = Vec::new();
            let mut pieces = Vec::new();
            let mut pen: i64 = 0;
            for (text, brush) in line {
                let start = pen;
                let mut last_cluster = pen;
                let mut b = self
                    .layout_cx
                    .ranged_builder(&mut self.font_cx, text, 1.0, false);
                b.push_default(StyleProperty::FontFamily(FontFamily::named(&family)));
                b.push_default(StyleProperty::FontSize(em as f32));
                // libass turns `kern` off unless the script asks, and `liga`/`clig` off
                // whenever Spacing is non-zero (ass_shaper.c, VSFilter compatibility).
                let features = match (self.kerning, hsp26 != 0) {
                    (true, false) => None,
                    (true, true) => Some("\"liga\" 0, \"clig\" 0"),
                    (false, false) => Some("\"kern\" 0"),
                    (false, true) => Some("\"kern\" 0, \"liga\" 0, \"clig\" 0"),
                };
                if let Some(f) = features {
                    b.push_default(StyleProperty::FontFeatures(FontFeatures::from(f)));
                }
                let mut layout = b.build(text);
                layout.break_all_lines(None);
                for pl in layout.lines() {
                    for item in pl.items() {
                        let PositionedLayoutItem::GlyphRun(gr) = item else {
                            continue;
                        };
                        let f = gr.run().font();
                        if f.data.id() != blob_id || f.index != face.index() {
                            return Err(RasterError(format!(
                                "font {}: shaped with face {} of another file or index (collections need a single-face file)",
                                style.font.as_str(),
                                f.index
                            )));
                        }
                        for cluster in gr.run().visual_clusters() {
                            last_cluster = pen;
                            let mut cpen = pen;
                            for g in cluster.glyphs() {
                                glyphs.push(PlacedGlyph {
                                    id: g.id,
                                    pen26: cpen,
                                    brush: *brush,
                                });
                                cpen += adv26(g.id);
                            }
                            pen = cpen + hsp26;
                        }
                    }
                }
                pieces.push((start, last_cluster, pen));
            }
            shaped.push((glyphs, pen, pieces));
        }

        let max_w26 = shaped.iter().map(|(_, w, _)| *w).max().unwrap_or(0);
        let max_w = max_w26 as f64 / 64.0;
        let (asc26, desc26) = (d6(asc), d6(desc));
        let block_h = (asc26 + desc26) as f64 * lines.len() as f64 / 64.0;
        let bx = match align {
            1 | 4 | 7 => 0.0,
            2 | 5 | 8 => max_w / 2.0,
            _ => max_w,
        };
        let by = -asc
            + match align {
                7..=9 => 0.0,
                4..=6 => block_h / 2.0,
                _ => block_h,
            };
        let (device_x, device_y) = (ev.pos.x - bx, ev.pos.y - by);
        let eighth = |v: f64| (v * 8.0).round() / 8.0;
        // Bounds before any outline is built: the pen box, a 1 em overhang, and every effect.
        let reach = em
            + st.outline.map_or(0.0, |o| o.0)
            + st.shadow
                .map_or(0.0, |s| s.dx.abs().max(s.dy.abs()) + 3.0 * s.blur)
            + st.text_box.map_or(0.0, |b| b.pad_x.max(b.pad_y).max(1.0))
            + 2.0;
        let est_w = max_w + 2.0 * reach;
        let est_h = block_h + 2.0 * reach;
        if est_w > MAX_SIDE || est_h > MAX_SIDE {
            return Err(RasterError(format!(
                "text needs about {est_w:.0}×{est_h:.0} px (at most {MAX_SIDE})"
            )));
        }
        let mut geo = Geometry {
            anchor: [ev.pos.x.floor(), ev.pos.y.floor()],
            ..Geometry::default()
        };

        let outlines = font.outline_glyphs();
        let mut runs: BTreeMap<u16, Vec<Contour>> = BTreeMap::new();
        let mut glyph_contours: Vec<Vec<Contour>> = Vec::new();
        for (i, (glyphs, w26, pieces)) in shaped.iter().enumerate() {
            let shift26 = match align {
                1 | 4 | 7 => 0,
                2 | 5 | 8 => d6((max_w26 - w26) as f64 / 64.0 / 2.0),
                _ => max_w26 - w26,
            };
            let baseline = eighth(device_y + (i as i64 * (asc26 + desc26)) as f64 / 64.0);
            let line_x = device_x + shift26 as f64 / 64.0;
            if let Some(b) = st.text_box {
                // libass BS3: a box per glyph, advance (+ spacing) wide and asc + desc high,
                // padded by max(1 px, Outline). Boxes of one colour run merge into one bitmap,
                // and runs composite one after another, so a translucent box is drawn twice
                // where two runs' boxes overlap (the darker band the reference's pills show at a colour
                // change). Hence one box per (line, run), each its own fill.
                if !b.libass {
                    // One box per line, exactly `pad` around the line's ascent + descent and
                    // advance (SPEC §5.9), `radius` rounded.
                    let r = Rect {
                        x0: eighth(line_x) - b.pad_x,
                        y0: baseline - asc - b.pad_y,
                        x1: eighth(line_x) + *w26 as f64 / 64.0 + b.pad_x,
                        y1: baseline + desc + b.pad_y,
                    };
                    geo.boxes.push(rect_contour(r, b.radius));
                    geo.box_colour = Some(b.colour);
                }
                let (px, py) = (b.pad_x.max(1.0), b.pad_y.max(1.0));
                for &(a26, last26, z26) in pieces.iter().filter(|_| b.libass) {
                    // Each glyph's box starts at its (1/8 px) origin; the run's last one ends
                    // its advance (+ spacing) later.
                    let r = Rect {
                        x0: eighth(line_x + a26 as f64 / 64.0) - px,
                        y0: baseline - asc - py,
                        x1: eighth(line_x + last26 as f64 / 64.0)
                            + (z26 - last26) as f64 / 64.0
                            + px,
                        y1: baseline + desc + py,
                    };
                    geo.boxes.push(rect_contour(r, b.radius));
                }
                geo.box_colour = Some(b.colour);
            }
            for g in glyphs {
                let Some(outline) = outlines.get(skrifa::GlyphId::new(g.id)) else {
                    continue;
                };
                let mut pen = FlattenPen::new(eighth(line_x + g.pen26 as f64 / 64.0), baseline);
                outline
                    .draw(
                        DrawSettings::unhinted(Size::new(em as f32), LocationRef::default()),
                        &mut pen,
                    )
                    .map_err(|e| RasterError(format!("glyph {}: {e}", g.id)))?;
                let cs = pen.finish();
                if cs.is_empty() {
                    continue;
                }
                runs.entry(g.brush).or_default().extend(cs.iter().cloned());
                glyph_contours.push(cs);
            }
        }
        for (brush, cs) in runs {
            geo.fills.push((
                fills.get(usize::from(brush)).copied().unwrap_or(st.fill),
                cs,
            ));
        }
        if let Some((width, colour)) = st.outline {
            let mut union = Vec::new();
            for cs in &glyph_contours {
                union.extend(dilate(cs, width));
            }
            geo.outline = Some((colour, union));
        }
        if let Some(s) = st.shadow {
            let src: Vec<Contour> = match (&geo.outline, s.of) {
                (Some((_, u)), ShadowOf::FillAndOutline) => u.clone(),
                _ => geo
                    .fills
                    .iter()
                    .flat_map(|(_, cs)| cs.iter().cloned())
                    .collect(),
            };
            let moved = src
                .into_iter()
                .map(|c| c.into_iter().map(|[x, y]| [x + s.dx, y + s.dy]).collect())
                .collect();
            geo.shadow = Some((s.colour, s.blur, moved));
        }
        Ok(geo)
    }
}

/// FreeType `FT_DivFix` for non-negative inputs: `(a << 16) / b`, rounded.
fn ft_div_fix(a: i64, b: i64) -> i64 {
    ((a << 16) + b / 2) / b.max(1)
}

/// FreeType `FT_MulFix` for non-negative inputs: `a · b / 65536`, rounded.
fn ft_mul_fix(a: i64, b: i64) -> i64 {
    (a * b + 0x8000) >> 16
}

/// libass `double_to_d6`: `lrint(x · 64)`.
fn d6(x: f64) -> i64 {
    (x * 64.0).round_ties_even() as i64
}

/// A style with RenderPlan defaults applied (fill white, outline/shadow colour black, shadow
/// of fill ∪ outline, no blur, square box; libass's own defaults).
#[derive(Clone, Copy, Debug)]
struct Resolved {
    fill: Colour,
    outline: Option<(f64, Colour)>,
    shadow: Option<ResolvedShadow>,
    text_box: Option<ResolvedBox>,
    letter_spacing: f64,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedShadow {
    dx: f64,
    dy: f64,
    blur: f64,
    colour: Colour,
    of: ShadowOf,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedBox {
    colour: Colour,
    pad_x: f64,
    pad_y: f64,
    radius: f64,
    /// `geometry: libass_bs3`: libass's per-(line, run) boxes.
    libass: bool,
}

impl Resolved {
    fn new(s: &TextStyle) -> Result<Self, RasterError> {
        let colour = |c: &Option<gmclip_plan::scalars::Rgba>, d: Colour| {
            c.as_ref().map_or(d, Colour::from_plan)
        };
        let num = |what: &str, v: Option<f64>, lo: f64, hi: f64| -> Result<f64, RasterError> {
            let v = v.unwrap_or(0.0);
            if v.is_finite() && (lo..=hi).contains(&v) {
                Ok(v)
            } else {
                Err(RasterError(format!("{what} {v} outside [{lo}, {hi}]")))
            }
        };
        let outline = match &s.outline {
            Some(o) => Some((
                num("outline.width", o.width, 0.0, 256.0)?,
                colour(&o.colour, Colour::BLACK),
            )),
            None => None,
        };
        let shadow = match &s.shadow {
            Some(sh) => Some(ResolvedShadow {
                dx: num("shadow.dx", sh.dx, -1024.0, 1024.0)?,
                dy: num("shadow.dy", sh.dy, -1024.0, 1024.0)?,
                blur: num("shadow.blur", sh.blur, 0.0, 64.0)?,
                colour: colour(&sh.colour, Colour::BLACK),
                of: sh.of.unwrap_or(ShadowOf::FillAndOutline),
            }),
            None => None,
        };
        let text_box = match &s.text_box {
            Some(b) => Some(ResolvedBox {
                colour: colour(&b.colour, Colour::BLACK),
                pad_x: num("box.pad_x", b.pad_x, 0.0, 1024.0)?,
                pad_y: num("box.pad_y", b.pad_y, 0.0, 1024.0)?,
                radius: num("box.radius", b.radius, 0.0, 1024.0)?,
                libass: b.geometry.is_some(),
            }),
            None => None,
        };
        Ok(Self {
            fill: colour(&s.fill, Colour::WHITE),
            outline: outline.filter(|(w, _)| *w > 0.0),
            shadow,
            text_box,
            letter_spacing: num("letter_spacing", s.letter_spacing, -1024.0, 1024.0)?,
        })
    }
}

/// One shaped line: glyphs, width (26.6) and per-run `(start, last cluster, end)` pens.
type ShapedLine = (Vec<PlacedGlyph>, i64, Vec<(i64, i64, i64)>);

#[derive(Clone, Copy, Debug)]
struct PlacedGlyph {
    id: u32,
    pen26: i64,
    brush: u16,
}

/// A closed polygon in canvas px.
pub type Contour = Vec<[f64; 2]>;

#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

/// A rectangle `[x0, x1) × [y0, y1)` with corners rounded by `radius` (clockwise on
/// screen), as the text box draws it: callouts reuse it.
pub fn rounded_rect(x0: f64, y0: f64, x1: f64, y1: f64, radius: f64) -> Contour {
    rect_contour(Rect { x0, y0, x1, y1 }, radius)
}

/// An event's drawable geometry in canvas px, in draw order.
#[derive(Clone, Debug, Default)]
pub struct Geometry {
    /// Where an empty tile goes (the event's `pos`, floored).
    pub anchor: [f64; 2],
    /// Shadow colour, blur and polygons (already offset).
    pub shadow: Option<(Colour, f64, Vec<Contour>)>,
    /// Box colour (BorderStyle 3).
    pub box_colour: Option<Colour>,
    /// One box polygon per (line, run), each filled on its own.
    pub boxes: Vec<Contour>,
    /// Outline colour and the dilated glyph union.
    pub outline: Option<(Colour, Vec<Contour>)>,
    /// Fill colour and glyph polygons per run colour.
    pub fills: Vec<(Colour, Vec<Contour>)>,
}

impl Geometry {
    fn all_points(&self) -> impl Iterator<Item = &[f64; 2]> {
        let shadow = self
            .shadow
            .iter()
            .flat_map(|(_, _, cs)| cs.iter().flatten());
        let outline = self.outline.iter().flat_map(|(_, cs)| cs.iter().flatten());
        let fills = self.fills.iter().flat_map(|(_, cs)| cs.iter().flatten());
        shadow
            .chain(self.boxes.iter().flatten())
            .chain(outline)
            .chain(fills)
    }

    /// Rasterises into a tile just large enough for every point (1 px margin; the shadow blur
    /// widens it by 3σ).
    pub fn rasterise(&self) -> Result<Tile, RasterError> {
        let (mut x0, mut y0, mut x1, mut y1) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for p in self.all_points() {
            if !(p[0].is_finite() && p[1].is_finite()) {
                return Err(RasterError("non-finite geometry".into()));
            }
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        if !x0.is_finite() {
            // Nothing visible (only spaces): an empty 1×1 tile at the event's anchor.
            return Ok(Tile {
                x: self.anchor[0] as i32,
                y: self.anchor[1] as i32,
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            });
        }
        let blur = self.shadow.as_ref().map_or(0.0, |s| s.1);
        let pad = 1.0 + (3.0 * blur).ceil();
        let (tx, ty) = ((x0 - pad).floor(), (y0 - pad).floor());
        let (w, h) = ((x1 + pad).ceil() - tx, (y1 + pad).ceil() - ty);
        if !(w >= 1.0 && h >= 1.0 && w <= MAX_SIDE && h <= MAX_SIDE)
            || tx.abs() > 1e6
            || ty.abs() > 1e6
        {
            return Err(RasterError(format!(
                "tile {w}x{h} at {tx},{ty} out of range"
            )));
        }
        let (w16, h16) = (w as u16, h as u16);
        let path = |cs: &[Contour]| -> BezPath {
            let mut p = BezPath::new();
            for c in cs {
                let q = |pt: &[f64; 2]| {
                    KPoint::new(
                        ((pt[0] - tx) * QUANT).round() / QUANT,
                        ((pt[1] - ty) * QUANT).round() / QUANT,
                    )
                };
                let mut it = c.iter();
                if let Some(first) = it.next() {
                    p.move_to(q(first));
                    for pt in it {
                        p.line_to(q(pt));
                    }
                    p.close_path();
                }
            }
            p
        };
        let settings = RenderSettings {
            level: Level::fallback(),
            num_threads: 0,
        };
        let mut pixmap = Pixmap::new(w16, h16);
        // Shadow: A8 mask of its shape, solidified, blurred when asked, tinted.
        if let Some((colour, blur, cs)) = &self.shadow {
            let mut m = coverage(&path(cs), w16, h16, settings);
            solidify(&mut m);
            if *blur > 0.0 {
                gaussian(&mut m, usize::from(w16), usize::from(h16), *blur);
            }
            over(&mut pixmap, &m, *colour);
        }
        if let Some(colour) = self.box_colour {
            let ps: Vec<BezPath> = self
                .boxes
                .iter()
                .map(|b| path(std::slice::from_ref(b)))
                .collect();
            draw(&mut pixmap, colour, &ps, settings);
        }
        if let Some((colour, cs)) = &self.outline {
            let mut m = coverage(&path(cs), w16, h16, settings);
            solidify(&mut m);
            over(&mut pixmap, &m, *colour);
        }
        for (colour, cs) in &self.fills {
            draw(&mut pixmap, *colour, &[path(cs)], settings);
        }
        Ok(Tile {
            x: tx as i32,
            y: ty as i32,
            width: u32::from(w16),
            height: u32::from(h16),
            rgba: pixmap.data_as_u8_slice().to_vec(),
        })
    }
}

/// Fills paths in one colour over the pixmap (vello, src-over), each path its own fill.
fn draw(pixmap: &mut Pixmap, colour: Colour, paths: &[BezPath], settings: RenderSettings) {
    let mut ctx = RenderContext::new_with(pixmap.width(), pixmap.height(), settings);
    ctx.set_fill_rule(Fill::NonZero);
    let [r, g, b, a] = colour.0;
    ctx.set_paint(AlphaColor::<Srgb>::from_rgba8(r, g, b, a));
    for p in paths {
        ctx.fill_path(p);
    }
    ctx.flush();
    let rs = RasterizerSettings {
        render_mode: RenderMode::OptimizeSpeed,
        target_init: TargetInit::SrcOver,
        ..RasterizerSettings::default()
    };
    ctx.render_with(pixmap, &mut Resources::new(), rs);
}

/// A path's non-zero coverage as an A8 mask.
fn coverage(p: &BezPath, w: u16, h: u16, settings: RenderSettings) -> Vec<u8> {
    let mut pm = Pixmap::new(w, h);
    let mut ctx = RenderContext::new_with(w, h, settings);
    ctx.set_fill_rule(Fill::NonZero);
    ctx.set_paint(AlphaColor::<Srgb>::from_rgba8(255, 255, 255, 255));
    ctx.fill_path(p);
    ctx.flush();
    ctx.render(&mut pm, &mut Resources::new());
    pm.data_as_u8_slice()
        .chunks_exact(4)
        .map(|px| px[3])
        .collect()
}

/// Interior pixels of a many-piece union can come out at 253–254; make them solid.
fn solidify(m: &mut [u8]) {
    for v in m.iter_mut() {
        if *v >= SOLID {
            *v = 255;
        }
    }
}

/// `colour × mask` src-over a premultiplied pixmap, in integers.
fn over(pixmap: &mut Pixmap, m: &[u8], colour: Colour) {
    let [cr, cg, cb, ca] = colour.0.map(u32::from);
    for (px, &mv) in pixmap.data_as_u8_slice_mut().chunks_exact_mut(4).zip(m) {
        let sa = (u32::from(mv) * ca + 127) / 255;
        if sa == 0 {
            continue;
        }
        let src = [
            (cr * sa + 127) / 255,
            (cg * sa + 127) / 255,
            (cb * sa + 127) / 255,
            sa,
        ];
        for c in 0..4 {
            let d = u32::from(px[c]);
            px[c] = (src[c] + (d * (255 - sa) + 127) / 255).min(255) as u8;
        }
    }
}

/// Our fixed-point separable Gaussian on an A8 mask (σ = `sigma` px, 16.16 weights).
fn gaussian(a: &mut [u8], w: usize, h: usize, sigma: f64) {
    let kernel = gaussian_kernel(sigma);
    let r = kernel.len() / 2;
    let mut tmp = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc: u64 = 0;
            for (k, wt) in kernel.iter().enumerate() {
                if let Some(sx) = (x + k).checked_sub(r).filter(|&s| s < w) {
                    acc += u64::from(a[y * w + sx]) * u64::from(*wt);
                }
            }
            tmp[y * w + x] = ((acc + (1 << 15)) >> 16) as u32;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc: u64 = 0;
            for (k, wt) in kernel.iter().enumerate() {
                if let Some(sy) = (y + k).checked_sub(r).filter(|&s| s < h) {
                    acc += u64::from(tmp[sy * w + x]) * u64::from(*wt);
                }
            }
            a[y * w + x] = (((acc + (1 << 15)) >> 16) as u32).min(255) as u8;
        }
    }
}

/// 16.16 Gaussian weights summing to 65536, from a fixed-point exp approximation (no libm).
fn gaussian_kernel(sigma: f64) -> Vec<u32> {
    let r = (3.0 * sigma).ceil().clamp(1.0, 256.0) as i64;
    // exp(−x) by repeated squaring of (1 − x/1024)^1024: + − × only, deterministic.
    let exp_neg = |x: f64| {
        let mut v = 1.0 - x / 1024.0;
        if v <= 0.0 {
            return 0.0;
        }
        for _ in 0..10 {
            v *= v;
        }
        v
    };
    let raw: Vec<f64> = (-r..=r)
        .map(|i| exp_neg((i * i) as f64 / (2.0 * sigma * sigma)))
        .collect();
    let sum: f64 = raw.iter().sum();
    let mut w: Vec<u32> = raw
        .iter()
        .map(|v| (v / sum * 65536.0).round() as u32)
        .collect();
    let total: u32 = w.iter().sum();
    let mid = w.len() / 2;
    w[mid] = (w[mid] + 65536).saturating_sub(total);
    w
}

/// A box polygon; `radius` rounds the corners with the fixed arc step.
fn rect_contour(r: Rect, radius: f64) -> Contour {
    let rad = radius
        .max(0.0)
        .min((r.x1 - r.x0) / 2.0)
        .min((r.y1 - r.y0) / 2.0);
    if rad <= 0.0 {
        return vec![[r.x0, r.y0], [r.x1, r.y0], [r.x1, r.y1], [r.x0, r.y1]];
    }
    let mut c = Vec::new();
    // Corners clockwise on screen (y down): top-right, bottom-right, bottom-left, top-left.
    let corners = [
        ([r.x1 - rad, r.y0 + rad], [0.0, -1.0]),
        ([r.x1 - rad, r.y1 - rad], [1.0, 0.0]),
        ([r.x0 + rad, r.y1 - rad], [0.0, 1.0]),
        ([r.x0 + rad, r.y0 + rad], [-1.0, 0.0]),
    ];
    for (centre, start) in corners {
        let mut u: [f64; 2] = start;
        for _ in 0..=32 {
            c.push([centre[0] + rad * u[0], centre[1] + rad * u[1]]);
            u = rotate(u, 1.0);
        }
    }
    c
}

/// Rotates a unit vector by ±π/64 (screen orientation: +1 turns x towards +y).
fn rotate(u: [f64; 2], dir: f64) -> [f64; 2] {
    let s = ARC_SIN * dir;
    [u[0] * ARC_COS - u[1] * s, u[0] * s + u[1] * ARC_COS]
}

fn signed_area(c: &[[f64; 2]]) -> f64 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let (p, q) = (c[i], c[(i + 1) % n]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a / 2.0
}

fn oriented(mut c: Contour, sign: f64) -> Option<Contour> {
    let a = signed_area(&c);
    if a == 0.0 || c.len() < 3 {
        return None;
    }
    if a.signum() != sign {
        c.reverse();
    }
    Some(c)
}

/// Glyph ⊕ disc(r): the glyph's contours (re-wound by nesting depth: outers like the largest
/// contour, holes the other way), plus for every edge the half-rectangle on its ink-free side
/// and for every vertex the short-way wedge between its two outward normals, all wound like
/// the outer contours, so a non-zero fill is exactly the union.
fn dilate(glyph: &[Contour], r: f64) -> Vec<Contour> {
    let areas: Vec<f64> = glyph.iter().map(|c| signed_area(c)).collect();
    let outer = areas
        .iter()
        .copied()
        .fold(0.0_f64, |m, a| if a.abs() > m.abs() { a } else { m });
    let sign = if outer < 0.0 { -1.0 } else { 1.0 };
    let mut out: Vec<Contour> = Vec::with_capacity(glyph.len() * 8);
    for (i, c) in glyph.iter().enumerate() {
        let Some(p0) = c.first() else { continue };
        let depth = glyph
            .iter()
            .enumerate()
            .filter(|&(j, o)| j != i && contains(o, *p0))
            .count();
        let want = if depth % 2 == 0 { sign } else { -sign };
        let mut c = c.clone();
        if areas[i] != 0.0 && areas[i].signum() != want {
            c.reverse();
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(c.len());
        for p in &c {
            if pts.last() != Some(p) {
                pts.push(*p);
            }
        }
        while pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        out.push(c);
        let n = pts.len();
        if n < 2 {
            continue;
        }
        // With outers wound `sign` and holes the other way, the ink is on the same side of
        // every edge: for `sign` > 0 (positive area, y down) the left normal (−dy, dx) points
        // into the ink, so (dy, −dx) points away from it; mirrored for `sign` < 0.
        let away = |d: [f64; 2]| {
            if sign > 0.0 {
                [d[1], -d[0]]
            } else {
                [-d[1], d[0]]
            }
        };
        let dirs: Vec<Option<[f64; 2]>> = (0..n)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len = (dx * dx + dy * dy).sqrt();
                (len > 0.0).then(|| [dx / len, dy / len])
            })
            .collect();
        for i in 0..n {
            let Some(d) = dirs[i] else { continue };
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let o = away(d);
            let quad = vec![
                a,
                b,
                [b[0] + r * o[0], b[1] + r * o[1]],
                [a[0] + r * o[0], a[1] + r * o[1]],
            ];
            out.extend(oriented(quad, sign));
        }
        for i in 0..n {
            let (Some(d1), Some(d2)) = (dirs[(i + n - 1) % n], dirs[i]) else {
                continue;
            };
            if let Some(w) = wedge(pts[i], away(d1), away(d2), r) {
                out.extend(oriented(w, sign));
            }
        }
    }
    out
}

/// Even-odd point-in-polygon.
fn contains(c: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let n = c.len();
    for i in 0..n {
        let (a, b) = (c[i], c[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// The disc sector at `v` from unit `a` to unit `b` the short way round (π when opposite).
fn wedge(v: [f64; 2], a: [f64; 2], b: [f64; 2], r: f64) -> Option<Contour> {
    let cross = a[0] * b[1] - a[1] * b[0];
    let dot = a[0] * b[0] + a[1] * b[1];
    if cross == 0.0 && dot > 0.0 {
        return None;
    }
    let dir = if cross >= 0.0 { 1.0 } else { -1.0 };
    let mut c = vec![v, [v[0] + r * a[0], v[1] + r * a[1]]];
    let mut u = a;
    for _ in 0..128 {
        let nx = rotate(u, dir);
        if (nx[0] * b[1] - nx[1] * b[0]) * dir <= 0.0 {
            break;
        }
        c.push([v[0] + r * nx[0], v[1] + r * nx[1]]);
        u = nx;
    }
    c.push([v[0] + r * b[0], v[1] + r * b[1]]);
    Some(c)
}

/// Flattens skrifa's outline (y up, em px) into canvas polygons (y down) at an origin.
struct FlattenPen {
    ox: f64,
    oy: f64,
    cur: Contour,
    out: Vec<Contour>,
    last: [f64; 2],
}

impl FlattenPen {
    fn new(ox: f64, oy: f64) -> Self {
        Self {
            ox,
            oy,
            cur: Vec::new(),
            out: Vec::new(),
            last: [0.0, 0.0],
        }
    }

    fn pt(&self, x: f32, y: f32) -> [f64; 2] {
        [self.ox + f64::from(x), self.oy - f64::from(y)]
    }

    fn push(&mut self, p: [f64; 2]) {
        self.cur.push(p);
        self.last = p;
    }

    fn flush(&mut self) {
        if self.cur.len() >= 3 {
            self.out.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }

    fn finish(mut self) -> Vec<Contour> {
        self.flush();
        self.out
    }
}

fn segments(dev: f64, k: f64, max: f64) -> usize {
    let n = (k * dev / FLATTEN_TOL).sqrt().ceil();
    if n.is_finite() {
        n.clamp(1.0, max) as usize
    } else {
        1
    }
}

impl OutlinePen for FlattenPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        let p = self.pt(x, y);
        self.push(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.push(p);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.last, self.pt(cx0, cy0), self.pt(x, y));
        let dx = p0[0] - 2.0 * p1[0] + p2[0];
        let dy = p0[1] - 2.0 * p1[1] + p2[1];
        let n = segments((dx * dx + dy * dy).sqrt(), 0.125, 64.0);
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let mt = 1.0 - t;
            let (a, b, c) = (mt * mt, 2.0 * mt * t, t * t);
            self.push([
                a * p0[0] + b * p1[0] + c * p2[0],
                a * p0[1] + b * p1[1] + c * p2[1],
            ]);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (
            self.last,
            self.pt(cx0, cy0),
            self.pt(cx1, cy1),
            self.pt(x, y),
        );
        let d = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
            let (dx, dy) = (a[0] - 2.0 * b[0] + c[0], a[1] - 2.0 * b[1] + c[1]);
            (dx * dx + dy * dy).sqrt()
        };
        let dev = d(p0, p1, p2).max(d(p1, p2, p3));
        let n = segments(dev, 0.75, 128.0);
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let mt = 1.0 - t;
            let (a, b, c, e) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            self.push([
                a * p0[0] + b * p1[0] + c * p2[0] + e * p3[0],
                a * p0[1] + b * p1[1] + c * p2[1] + e * p3[1],
            ]);
        }
    }

    fn close(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_constants_close_the_circle() {
        let mut u = [1.0, 0.0];
        for _ in 0..128 {
            u = rotate(u, 1.0);
        }
        assert!((u[0] - 1.0).abs() < 1e-12 && u[1].abs() < 1e-12, "{u:?}");
    }

    #[test]
    fn dilated_square_has_rounded_area() {
        // 10×10 square ⊕ disc(2): area = 100 + 4·10·2 + π·4.
        let sq = vec![vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]];
        let pieces = dilate(&sq, 2.0);
        let geo = Geometry {
            fills: vec![(Colour::WHITE, pieces)],
            ..Geometry::default()
        };
        let tile = geo.rasterise().unwrap();
        let cov: f64 = tile
            .rgba
            .chunks_exact(4)
            .map(|p| f64::from(p[3]) / 255.0)
            .sum();
        let want = 100.0 + 80.0 + std::f64::consts::PI * 4.0;
        assert!((cov - want).abs() < 0.5, "coverage {cov} vs {want}");
    }

    #[test]
    fn gaussian_weights_sum_to_one() {
        for s in [0.5, 1.0, 3.0, 7.5] {
            assert_eq!(gaussian_kernel(s).iter().sum::<u32>(), 65536);
        }
    }

    #[test]
    fn wrongly_wound_outer_still_dilates_fully() {
        // Two separate squares, the second wound the other way (a mirrored component).
        let a = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let mut b = vec![[20.0, 0.0], [26.0, 0.0], [26.0, 6.0], [20.0, 6.0]];
        b.reverse();
        let geo = Geometry {
            fills: vec![(Colour::WHITE, dilate(&[a, b], 3.0))],
            ..Geometry::default()
        };
        let tile = geo.rasterise().unwrap();
        let cov: f64 = tile
            .rgba
            .chunks_exact(4)
            .map(|p| f64::from(p[3]) / 255.0)
            .sum();
        let pi = std::f64::consts::PI;
        let want = (100.0 + 4.0 * 10.0 * 3.0 + pi * 9.0) + (36.0 + 4.0 * 6.0 * 3.0 + pi * 9.0);
        assert!((cov - want).abs() < 1.0, "coverage {cov} vs {want}");
    }

    #[test]
    fn a_hole_stays_open_and_is_ringed() {
        // A 20×20 square with a 10×10 hole: dilating by 2 leaves a 6×6 hole.
        let outer = vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]];
        let mut hole = vec![[5.0, 5.0], [15.0, 5.0], [15.0, 15.0], [5.0, 15.0]];
        hole.reverse();
        let geo = Geometry {
            fills: vec![(Colour::WHITE, dilate(&[outer, hole], 2.0))],
            ..Geometry::default()
        };
        let tile = geo.rasterise().unwrap();
        let cov: f64 = tile
            .rgba
            .chunks_exact(4)
            .map(|p| f64::from(p[3]) / 255.0)
            .sum();
        let pi = std::f64::consts::PI;
        let want = (400.0 + 4.0 * 20.0 * 2.0 + pi * 4.0) - 36.0;
        assert!((cov - want).abs() < 1.0, "coverage {cov} vs {want}");
    }

    #[test]
    fn solidify_and_over() {
        let mut m = vec![249, 250, 254, 255];
        solidify(&mut m);
        assert_eq!(m, vec![249, 255, 255, 255]);
    }
}
