//! Text compositing (H-06): the plan's text events rasterised by lane C's cue raster
//! (B-09, premultiplied RGBA8 tiles at integer canvas anchors), cached per event while
//! it is on screen, drawn above every layer ordered by `layer` then array order (RP
//! rule 5 / §2.6: v0 draws text above all layers), premultiplied over.
//!
//! `/1` image and shape events (decision 0014) are tiles too: an image resampled to its
//! place by exact area weights ([`gmclip_media::image::fit`]), a callout rasterised by
//! [`super::shapes`]. Everything is drawn by `layer`, then images, shapes, text, then array
//! order.
//!
//! An animated event (`/1` `anim`, decision 0014) is rasterised per frame at its
//! animated size ([`gmclip_plan::anim`]) and its opacity multiplies the tile; a settled
//! frame is the plain event's tile, so a held cue still rasterises once.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_plan::Plan;
use gmclip_plan::anim::{self, AnimState};

use super::cpu::RgbaImage;
use super::draw::ComposeError;
use crate::text::font::FontFace;
use crate::text::raster::{TextRaster, Tile};
use crate::text::{TextEvent, TextStyle};
use std::hash::{Hash, Hasher};

/// The tiles above every layer, and those drawn among them: `(before layer i, tile)`.
pub type TileSplit = (Vec<Arc<Tile>>, Vec<(usize, Arc<Tile>)>);

/// The plan's text, rasterised on demand.
///
/// Tiles are cached by content — the event's text, runs, position, alignment, size and
/// its style, hashed — so a cached tile is never handed out for an edited event, and
/// any plan may be passed to [`Self::active`]. Only the tiles drawn on the last frame are
/// kept (an event that comes back is rasterised again).
pub struct TextLayer {
    raster: TextRaster,
    tiles: HashMap<u64, Arc<Tile>>,
    /// A still backdrop's tile (decision 0028), with the key it was built for.
    under: Option<(u64, Arc<Tile>)>,
    /// The plan's images, decoded once (by `images[].id`).
    images: HashMap<String, Arc<gmclip_media::image::Rgba8>>,
}

impl std::fmt::Debug for TextLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextLayer")
            .field("cached", &self.tiles.len())
            .finish()
    }
}

/// The cache key of an event drawn in a style (everything the raster reads).
fn tile_key(ev: &TextEvent, style: &TextStyle) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Both serialise deterministically (struct fields in order, no maps inside).
    fn json<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_default()
    }
    json(&ev.runs).hash(&mut h);
    json(&ev.pos).hash(&mut h);
    ev.align.hash(&mut h);
    ev.size.to_bits().hash(&mut h);
    json(style).hash(&mut h);
    h.finish()
}

impl TextLayer {
    /// Loads the plan's fonts (only the files the plan names: SPEC §7 export fonts),
    /// and none when the plan has no text events.
    pub fn new(plan: &Plan) -> Result<Self, ComposeError> {
        let mut raster = TextRaster::new();
        let (loads_fonts, loads_images) = Self::loads(plan);
        if loads_fonts {
            for f in plan.fonts.iter().flatten() {
                let face = FontFace::open(std::path::Path::new(&f.path))
                    .map_err(|e| ComposeError::Text(format!("font {}: {e}", f.id.as_str())))?;
                raster
                    .add_font(f.id.as_str(), face)
                    .map_err(|e| ComposeError::Text(format!("font {}: {e}", f.id.as_str())))?;
            }
        }
        let mut images = HashMap::new();
        if loads_images {
            for m in plan.images.iter().flatten() {
                let bad = |e: String| ComposeError::Text(format!("image {}: {e}", m.id.as_str()));
                let bytes = std::fs::read(&m.path).map_err(|e| bad(e.kind().to_string()))?;
                let img = gmclip_media::image::decode_untrusted(&bytes)
                    .map_err(|e| bad(e.to_string()))?;
                if u64::from(img.width) != m.w || u64::from(img.height) != m.h {
                    return Err(bad("the file is not the size the plan says".into()));
                }
                images.insert(m.id.as_str().to_owned(), Arc::new(img));
            }
        }
        Ok(Self {
            raster,
            tiles: HashMap::new(),
            under: None,
            images,
        })
    }

    /// Whether [`Self::new`] reads the plan's fonts and its images: fonts only when it has
    /// text events, images only when it has image events or a still backdrop.
    fn loads(plan: &Plan) -> (bool, bool) {
        let still_backdrop = matches!(
            &plan.background,
            gmclip_plan::types::Background::Backdrop { image: Some(_), .. }
        );
        (
            plan.text_events.as_ref().is_some_and(|t| !t.is_empty()),
            still_backdrop || plan.image_events.as_ref().is_some_and(|e| !e.is_empty()),
        )
    }

    /// Whether a layer built for `old` must be rebuilt for `plan`: the fonts or images
    /// changed, or the plan now needs fonts or images the old one did not load (a plan
    /// may list a font with no text event yet, so the same `fonts` is not enough).
    pub fn stale(old: &Plan, plan: &Plan) -> bool {
        plan.fonts != old.fonts
            || plan.images != old.images
            || Self::loads(plan) != Self::loads(old)
    }

    /// A still backdrop's tile (decision 0028): the image's `crop` (rounded to whole
    /// pixels) resampled to its `place` by exact area weights, drawn under every layer.
    /// Built once and kept while the plan's backdrop is the same.
    pub fn under(&mut self, plan: &Plan) -> Result<Option<Arc<Tile>>, ComposeError> {
        if let gmclip_plan::types::Background::Plate { stops } = &plan.background {
            let (w, h) = (
                plan.output.canvas.w.get() as u32,
                plan.output.canvas.h.get() as u32,
            );
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            "plate".hash(&mut hh);
            for s in stops {
                s.as_str().hash(&mut hh);
            }
            (w, h).hash(&mut hh);
            let key = hh.finish();
            if let Some((k, t)) = &self.under
                && *k == key
            {
                return Ok(Some(Arc::clone(t)));
            }
            let t = Arc::new(plate_tile(stops, w, h)?);
            self.under = Some((key, Arc::clone(&t)));
            return Ok(Some(t));
        }
        let gmclip_plan::types::Background::Backdrop {
            crop,
            place,
            image: Some(id),
            ..
        } = &plan.background
        else {
            self.under = None;
            return Ok(None);
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.as_str().hash(&mut h);
        for v in [
            crop.x, crop.y, crop.w, crop.h, place.x, place.y, place.w, place.h,
        ] {
            v.to_bits().hash(&mut h);
        }
        let key = h.finish();
        if let Some((k, t)) = &self.under
            && *k == key
        {
            return Ok(Some(Arc::clone(t)));
        }
        let img = self.images.get(id.as_str()).ok_or_else(|| {
            ComposeError::Text(format!("backdrop image {}: not loaded", id.as_str()))
        })?;
        let (iw, ih) = (f64::from(img.width), f64::from(img.height));
        let x0 = crop.x.round().clamp(0.0, iw - 1.0) as u32;
        let y0 = crop.y.round().clamp(0.0, ih - 1.0) as u32;
        let x1 = (crop.x + crop.w).round().clamp(f64::from(x0) + 1.0, iw) as u32;
        let y1 = (crop.y + crop.h).round().clamp(f64::from(y0) + 1.0, ih) as u32;
        let (cw, chh) = (x1 - x0, y1 - y0);
        let mut px = Vec::with_capacity((cw * chh * 4) as usize);
        for y in y0..y1 {
            let row = (y * img.width + x0) as usize * 4;
            px.extend_from_slice(&img.px[row..row + cw as usize * 4]);
        }
        let cropped = gmclip_media::image::Rgba8 {
            width: cw,
            height: chh,
            px,
        };
        let w = place.w.round().clamp(1.0, 8192.0) as u32;
        let hh = place.h.round().clamp(1.0, 8192.0) as u32;
        let t = Arc::new(Tile {
            x: place.x.round() as i32,
            y: place.y.round() as i32,
            width: w,
            height: hh,
            rgba: gmclip_media::image::fit(&cropped, w, hh),
        });
        self.under = Some((key, Arc::clone(&t)));
        Ok(Some(t))
    }

    /// The tiles on output frame `f` (`out` is half-open), bottom to top: `layer` (absent
    /// = 0), then images, shapes, text, then array order.
    pub fn active(&mut self, plan: &Plan, f: u64) -> Result<Vec<Arc<Tile>>, ComposeError> {
        self.active_moved(plan, f, &|_| None)
    }

    /// [`Self::active`] with some text events drawn elsewhere: `moved` gives an event's
    /// offset `(dx, dy)` in canvas px, or `None` to leave it (a preview's live text drag,
    /// U.12; the event is rasterised at its moved position exactly as a plan placing it
    /// there would be). Export never moves anything.
    pub fn active_moved(
        &mut self,
        plan: &Plan,
        f: u64,
        moved: &dyn Fn(&TextEvent) -> Option<(f64, f64)>,
    ) -> Result<Vec<Arc<Tile>>, ComposeError> {
        Ok(self.active_split(plan, f, moved, &[])?.0)
    }

    /// [`Self::active_moved`] split in one pass (one tile cache for both): the tiles above
    /// every layer, and the image events drawn among them (decision 0042) at
    /// [`super::draw::FrameDraw::tiles_at`]'s indices, in its order.
    pub fn active_split(
        &mut self,
        plan: &Plan,
        f: u64,
        moved: &dyn Fn(&TextEvent) -> Option<(f64, f64)>,
        tiles_at: &[(usize, String)],
    ) -> Result<TileSplit, ComposeError> {
        let on = |r: &gmclip_plan::scalars::Range| r.start() <= f && f < r.end();
        // (layer, kind, index): 0 image, 1 shape, 2 text; an image with `z` is drawn among
        // the layers when the draw list places it (else not at all)
        let mut order: Vec<(i64, u8, usize)> = Vec::new();
        for (i, e) in plan.image_events.iter().flatten().enumerate() {
            if on(&e.out) && (e.z.is_none() || tiles_at.iter().any(|(_, id)| id == e.id.as_str())) {
                order.push((e.layer.unwrap_or(0), 0, i));
            }
        }
        for (i, e) in plan.shape_events.iter().flatten().enumerate() {
            if on(&e.out) {
                order.push((e.layer.unwrap_or(0), 1, i));
            }
        }
        for (i, e) in plan.text_events.iter().flatten().enumerate() {
            if on(&e.out) {
                order.push((e.layer.unwrap_or(0), 2, i));
            }
        }
        order.sort_unstable();
        let styles = plan.text_styles.as_ref();
        // animation steps in whole frames (an NTSC rate counts as its integer neighbour)
        let fps = plan.output.fps.rounded();
        let mut out = Vec::with_capacity(order.len());
        let mut among: Vec<(String, Arc<Tile>)> = Vec::new();
        let mut keep = HashMap::with_capacity(order.len());
        // overlays pinned to a moving point (decision 0031): (kind, event) → its track
        let tracks: HashMap<(u8, &str), &gmclip_plan::types::EventTrack> = plan
            .tracks
            .iter()
            .flatten()
            .map(|t| {
                let k = match t.kind {
                    gmclip_plan::types::TrackedKind::Image => 0,
                    gmclip_plan::types::TrackedKind::Shape => 1,
                    gmclip_plan::types::TrackedKind::Text => 2,
                };
                ((k, t.event.as_str()), t)
            })
            .collect();
        for (_, kind, i) in order {
            let (id, start) = match kind {
                0 => {
                    let e = &plan.image_events.as_ref().expect("listed")[i];
                    (e.id.as_str(), e.out.start())
                }
                1 => {
                    let e = &plan.shape_events.as_ref().expect("listed")[i];
                    (e.id.as_str(), e.out.start())
                }
                _ => {
                    let e = &plan.text_events.as_ref().expect("listed")[i];
                    (e.id.as_str(), e.out.start())
                }
            };
            let shift = tracks
                .get(&(kind, id))
                .map_or((0, 0), |t| t.offset(start, f));
            let tile = match kind {
                0 => {
                    let e = &plan.image_events.as_ref().expect("listed")[i];
                    let key = image_key(e);
                    match self.tiles.get(&key).or_else(|| keep.get(&key)) {
                        Some(t) => (key, Arc::clone(t)),
                        None => (key, Arc::new(self.image_tile(e)?)),
                    }
                }
                1 => {
                    let e = &plan.shape_events.as_ref().expect("listed")[i];
                    let key = shape_key(e);
                    match self.tiles.get(&key).or_else(|| keep.get(&key)) {
                        Some(t) => (key, Arc::clone(t)),
                        None => (
                            key,
                            Arc::new(super::shapes::render(&e.shape).map_err(|x| {
                                ComposeError::Text(format!("shape {}: {x}", e.id.as_str()))
                            })?),
                        ),
                    }
                }
                _ => {
                    let ev = &plan.text_events.as_ref().expect("listed")[i];
                    let style = styles.and_then(|s| s.get(&ev.style)).ok_or_else(|| {
                        ComposeError::Text(format!("text event {}: unknown style", ev.id.as_str()))
                    })?;
                    match moved(ev) {
                        Some((dx, dy)) => {
                            let mut e = ev.clone();
                            e.pos.x += dx;
                            e.pos.y += dy;
                            self.text_tile(&e, style, f, fps, &keep)?
                        }
                        None => self.text_tile(ev, style, f, fps, &keep)?,
                    }
                }
            };
            keep.insert(tile.0, Arc::clone(&tile.1));
            let tile = if shift == (0, 0) {
                tile.1
            } else {
                // the same pixels, moved: kept by content and offset
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (tile.0, "track", shift).hash(&mut h);
                let key = h.finish();
                let t = match self.tiles.get(&key).or_else(|| keep.get(&key)) {
                    Some(t) => Arc::clone(t),
                    None => Arc::new(Tile {
                        x: tile.1.x + shift.0,
                        y: tile.1.y + shift.1,
                        width: tile.1.width,
                        height: tile.1.height,
                        rgba: tile.1.rgba.clone(),
                    }),
                };
                keep.insert(key, Arc::clone(&t));
                t
            };
            let is_among =
                kind == 0 && plan.image_events.as_ref().is_some_and(|v| v[i].z.is_some());
            if is_among {
                among.push((id.to_owned(), tile));
            } else {
                out.push(tile);
            }
        }
        self.tiles = keep;
        let among = tiles_at
            .iter()
            .filter_map(|(k, id)| {
                among
                    .iter()
                    .find(|(i, _)| i == id)
                    .map(|(_, t)| (*k, Arc::clone(t)))
            })
            .collect();
        Ok((out, among))
    }

    fn text_tile(
        &mut self,
        ev: &TextEvent,
        style: &TextStyle,
        f: u64,
        fps: u64,
        keep: &HashMap<u64, Arc<Tile>>,
    ) -> Result<(u64, Arc<Tile>), ComposeError> {
        let st = ev.anim.as_ref().map_or(AnimState::REST, |a| {
            anim::state(a, f - ev.out.start(), ev.out.len(), fps)
        });
        let animated = (!st.is_rest()).then(|| anim::apply(ev, style, st));
        let (ev, style) = match &animated {
            Some((e, s)) => (e, s),
            None => (ev, style),
        };
        let mut key = tile_key(ev, style);
        if st.alpha != 1.0 {
            key ^= st.alpha.to_bits().rotate_left(17);
        }
        if let Some(t) = self.tiles.get(&key).or_else(|| keep.get(&key)) {
            return Ok((key, Arc::clone(t)));
        }
        let mut t = self
            .raster
            .render(ev, style)
            .map_err(|e| ComposeError::Text(format!("text event {}: {e}", ev.id.as_str())))?;
        fade(&mut t, st.alpha);
        Ok((key, Arc::new(t)))
    }

    /// An image event's tile: the picture resampled to its place (whole canvas px; the
    /// place's corner rounded to the nearest pixel), times its opacity.
    fn image_tile(&self, e: &gmclip_plan::types::ImageEvent) -> Result<Tile, ComposeError> {
        let img = self.images.get(e.image.as_str()).ok_or_else(|| {
            ComposeError::Text(format!("image event {}: image not loaded", e.id.as_str()))
        })?;
        let w = e.place.w.round().clamp(1.0, 8192.0) as u32;
        let h = e.place.h.round().clamp(1.0, 8192.0) as u32;
        let mut t = Tile {
            x: e.place.x.round() as i32,
            y: e.place.y.round() as i32,
            width: w,
            height: h,
            rgba: gmclip_media::image::fit(img, w, h),
        };
        fade(&mut t, e.opacity.unwrap_or(1.0));
        Ok(t)
    }
}

/// A plate's gradient (decision 0029) as a `w × h` tile: row `y` (centre `y + ½`) sits at
/// `t = (y + ½) / h · (n − 1)` along the `n` stops, spread evenly from the top to the
/// bottom, and is the straight mix of the two stops around it in 8-bit R'G'B' codes,
/// rounded half up. Opaque, the same bytes everywhere.
pub fn plate_tile(
    stops: &[gmclip_plan::scalars::Rgb],
    w: u32,
    h: u32,
) -> Result<Tile, ComposeError> {
    if stops.len() < 2 || w == 0 || h == 0 {
        return Err(ComposeError::Invalid(
            "a plate needs 2 stops and a canvas".into(),
        ));
    }
    let rgb: Vec<[f64; 3]> = stops
        .iter()
        .map(|s| {
            let h = s.as_str().trim_start_matches('#');
            let c = |i: usize| f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0));
            [c(0), c(2), c(4)]
        })
        .collect();
    let n = rgb.len();
    let mut row = Vec::with_capacity(w as usize * 4);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let t = (f64::from(y) + 0.5) / f64::from(h) * (n - 1) as f64;
        let i = (t.floor() as usize).min(n - 2);
        let f = t - i as f64;
        let px: [u8; 4] = [0, 1, 2, 3].map(|k| {
            if k == 3 {
                255
            } else {
                (rgb[i][k] + (rgb[i + 1][k] - rgb[i][k]) * f + 0.5).floor() as u8
            }
        });
        row.clear();
        for _ in 0..w {
            row.extend_from_slice(&px);
        }
        rgba.extend_from_slice(&row);
    }
    Ok(Tile {
        x: 0,
        y: 0,
        width: w,
        height: h,
        rgba,
    })
}

fn image_key(e: &gmclip_plan::types::ImageEvent) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "image".hash(&mut h);
    e.image.as_str().hash(&mut h);
    for v in [
        e.place.x,
        e.place.y,
        e.place.w,
        e.place.h,
        e.opacity.unwrap_or(1.0),
    ] {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

fn shape_key(e: &gmclip_plan::types::ShapeEvent) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "shape".hash(&mut h);
    serde_json::to_string(&e.shape)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

/// Multiplies a premultiplied tile by `alpha` (8-bit, rounded half up).
pub fn fade(t: &mut Tile, alpha: f64) {
    if alpha >= 1.0 {
        return;
    }
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    for v in &mut t.rgba {
        *v = ((u32::from(*v) * a + 127) / 255) as u8;
    }
}

/// Premultiplied over of tiles onto the CPU canvas (values are 8-bit codes / 255).
pub fn composite_cpu(canvas: &mut RgbaImage, tiles: &[Arc<Tile>]) {
    let (cw, ch) = (canvas.width as i64, canvas.height as i64);
    for t in tiles {
        for ty in 0..i64::from(t.height) {
            let y = i64::from(t.y) + ty;
            if y < 0 || y >= ch {
                continue;
            }
            for tx in 0..i64::from(t.width) {
                let x = i64::from(t.x) + tx;
                if x < 0 || x >= cw {
                    continue;
                }
                let s = ((ty * i64::from(t.width) + tx) * 4) as usize;
                let a = f32::from(t.rgba[s + 3]) / 255.0;
                if a == 0.0 {
                    continue;
                }
                let d = &mut canvas.px[(y * cw + x) as usize];
                for (k, dk) in d.iter_mut().take(3).enumerate() {
                    *dk = f32::from(t.rgba[s + k]) / 255.0 + *dk * (1.0 - a);
                }
                d[3] = a + d[3] * (1.0 - a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The h79 example plan with its font swapped for the bundled OFL face (CI has no
    /// Neometric).
    fn plan() -> Plan {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let text = std::fs::read_to_string(format!(
            "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
        ))
        .unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["fonts"][0]["path"] = format!("{root}/assets/fonts/figtree/Figtree-Black.ttf").into();
        gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
    }

    fn ids(plan: &Plan, tiles: &[Arc<Tile>], layer: &mut TextLayer, f: u64) -> Vec<String> {
        // Map tiles back to events by re-rasterising each active event alone.
        let evs = plan.text_events.as_ref().unwrap();
        let mut out = Vec::new();
        for t in tiles {
            let ev = evs
                .iter()
                .filter(|e| e.out.start() <= f && f < e.out.end())
                .find(|e| {
                    let s = &plan.text_styles.as_ref().unwrap()[&e.style];
                    layer.raster.render(e, s).unwrap() == **t
                })
                .unwrap();
            out.push(ev.id.as_str().to_owned());
        }
        out
    }

    #[test]
    fn active_set_is_half_open_ordered_and_random_access() {
        let plan = plan();
        let mut tl = TextLayer::new(&plan).unwrap();
        // c01 is [55, 67): on at 66, off at 67 (c02 then); hook (layer 2) above c01 (1).
        let t = tl.active(&plan, 66).unwrap();
        assert_eq!(ids(&plan, &t, &mut tl, 66), ["c01", "hook"]);
        let t = tl.active(&plan, 67).unwrap();
        assert_eq!(ids(&plan, &t, &mut tl, 67), ["c02", "hook"]);
        // The hook's tile is reused while it stays on.
        let a = tl.active(&plan, 100).unwrap();
        let b = tl.active(&plan, 101).unwrap();
        assert!(Arc::ptr_eq(a.last().unwrap(), b.last().unwrap()));
        // Backwards, and a frame with only the pill (layer 3) and a gap in captions.
        let t = tl.active(&plan, 0).unwrap();
        assert_eq!(ids(&plan, &t, &mut tl, 0), ["hook"]);
        let t = tl.active(&plan, 348).unwrap();
        assert_eq!(ids(&plan, &t, &mut tl, 348), ["pill-1"]);
        assert!(tl.active(&plan, 397).unwrap().is_empty());
    }

    /// U.12: a live text move draws the event exactly as a plan that placed it there
    /// would (the preview after release is that plan), and moves nothing else.
    #[test]
    fn a_moved_event_is_the_tile_of_the_plan_that_moves_it() {
        let plan = plan();
        let mut tl = TextLayer::new(&plan).unwrap();
        let shift = crate::export::preview::TextShift {
            role: gmclip_plan::types::TextRole::TextHook,
            at: plan
                .text_events
                .as_ref()
                .unwrap()
                .iter()
                .find(|e| e.id.as_str() == "hook")
                .map(|e| (e.pos.x, e.pos.y)),
            dx: 12.0,
            dy: 140.0,
        };
        let before = tl.active(&plan, 60).unwrap();
        let moved = tl.active_moved(&plan, 60, &|ev| shift.offset(ev)).unwrap();
        let mut placed = plan.clone();
        for e in placed.text_events.as_mut().unwrap() {
            if e.id.as_str() == "hook" {
                e.pos.x += 12.0;
                e.pos.y += 140.0;
            }
        }
        let want = TextLayer::new(&placed)
            .unwrap()
            .active(&placed, 60)
            .unwrap();
        assert_eq!(moved.len(), 2);
        assert_eq!(*moved[0], *before[0], "the caption stays where it was");
        assert_eq!(
            *moved[1], *want[1],
            "the hook is drawn where the plan would put it"
        );
        assert_eq!(
            (moved[1].x, moved[1].y),
            (before[1].x + 12, before[1].y + 140)
        );
        // the caption seat: every caption, nothing else
        let seat = crate::export::preview::TextShift {
            role: gmclip_plan::types::TextRole::Caption,
            at: None,
            dx: 0.0,
            dy: -200.0,
        };
        let s = tl.active_moved(&plan, 60, &|ev| seat.offset(ev)).unwrap();
        assert_eq!(s[0].y, before[0].y - 200);
        assert_eq!(*s[1], *before[1]);
    }

    #[test]
    fn an_edited_event_gets_a_new_tile() {
        let mut plan = plan();
        let mut tl = TextLayer::new(&plan).unwrap();
        let before = tl.active(&plan, 60).unwrap();
        let evs = plan.text_events.as_mut().unwrap();
        let c01 = evs.iter_mut().find(|e| e.id.as_str() == "c01").unwrap();
        c01.runs[0].text = "edited".into();
        let after = tl.active(&plan, 60).unwrap();
        assert_eq!(before.len(), after.len());
        assert_ne!(
            *before[0], *after[0],
            "a stale tile came back for an edited event"
        );
        assert!(
            Arc::ptr_eq(&before[1], &after[1]),
            "the unchanged hook is reused"
        );
        // A plan with no text events needs no fonts and draws nothing.
        let mut bare = plan.clone();
        bare.text_events = None;
        bare.fonts.as_mut().unwrap()[0].path = "/nonexistent/font.otf".into();
        let mut tl = TextLayer::new(&bare).unwrap();
        assert!(tl.active(&bare, 60).unwrap().is_empty());
    }

    /// A plan that lists its font before it has a text event (a Short whose styles are set
    /// before its hook is placed) loads no font; when the hook arrives with the same
    /// `fonts`, the layer is stale and must be rebuilt, or the hook's font is "not
    /// registered" at frame 0.
    #[test]
    fn a_plan_that_starts_drawing_text_rebuilds_the_layer() {
        let plan = plan();
        let mut bare = plan.clone();
        bare.text_events = None;
        assert_eq!(bare.fonts, plan.fonts);
        let mut tl = TextLayer::new(&bare).unwrap();
        assert!(tl.active(&plan, 0).is_err(), "the bare layer has no font");
        assert!(TextLayer::stale(&bare, &plan));
        assert!(!TextLayer::stale(&plan, &plan));
        let mut tl = TextLayer::new(&plan).unwrap();
        assert!(!tl.active(&plan, 0).unwrap().is_empty());
    }

    #[test]
    fn composite_cpu_clips_and_blends_premultiplied() {
        let mut canvas = RgbaImage {
            width: 4,
            height: 3,
            px: vec![[0.2, 0.4, 0.6, 1.0]; 12],
        };
        // 3×2 tile at (-1, 2): only its right two columns of the first row land.
        let mut rgba = Vec::new();
        for _ in 0..6 {
            rgba.extend_from_slice(&[128, 0, 0, 128]); // premultiplied half-red
        }
        let tile = Arc::new(Tile {
            x: -1,
            y: 2,
            width: 3,
            height: 2,
            rgba,
        });
        composite_cpu(&mut canvas, &[tile]);
        let a = 128.0 / 255.0;
        for (i, p) in canvas.px.iter().enumerate() {
            let (x, y) = (i % 4, i / 4);
            if y == 2 && x < 2 {
                assert!(
                    (p[0] - (a + 0.2 * (1.0 - a))).abs() < 1e-6,
                    "{x},{y}: {p:?}"
                );
                assert!((p[1] - 0.4 * (1.0 - a)).abs() < 1e-6);
                assert!((p[3] - 1.0).abs() < 1e-6);
            } else {
                assert_eq!(*p, [0.2, 0.4, 0.6, 1.0], "{x},{y} touched");
            }
        }
    }
}
