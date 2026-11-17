//! Zoom sections (`/1` `zooms[]`, decision 0046): applied to a frame's draw list in the
//! one compose, before the recorded cursor is placed, so preview and export draw the same
//! zoom (R-COMP-1) and the cursor follows the zoomed crop.
//!
//! On output frame `f` of a zoom `[a, b)` with `k = f − a`, for each named layer of the
//! frame's part, its own crop `C` (source px, after any eased split):
//! - progress `p = ease_in_out(min(k / ease_in, (b − 1 − f) / ease_out))`, each term 1
//!   when its ease is 0 (a zoom that starts where another on the same layer ends does not
//!   ease in from `C`: it eases from that zoom's last crop, and that one does not ease out);
//! - the target `T` (source px): a `point` at `C.xy + point · C.wh`; a `cursor` the
//!   recorded cursor's position, a `focus` the caret's centre, else the focused field's,
//!   else the cursor — each through a critically damped spring ([`SPRING_TAU_S`]) simulated
//!   from the zoom's first frame one output frame at a time (the closed form for a target
//!   held over the step), so a frame's value is the same seeked or played; where the track
//!   says nothing the last target is held, and before any the crop's centre is used;
//! - the full crop `Z`: `C.wh / depth`, centred on `T`, kept inside `C`;
//! - the drawn crop: `C + (Z − C) · p` per component.
//!
//! `camera_scale` shrinks each named camera layer's place toward its nearest canvas
//! corner to `1 − (1 − camera_scale) · p` of its size, keeping that corner's edges.
//!
//! `motion_blur` (off unless the plan says `true`): on a frame where the drawn crop moved
//! since the frame before (a corner of the crop moves over [`BLUR_MIN_PX`] canvas px), the
//! layer is drawn as the mean of [`BLUR_SAMPLES`] sub-samples across the last half frame
//! (a 180° shutter, as the recorded cursor's): sample `j` is the crop at `f − j/8`, the
//! straight line between the frame's crop and the one before. [`ZoomLayer::apply`] notes
//! them and [`ZoomLayer::expand_blur`] turns the layer into that many copies of itself in
//! the draw list, the `k`-th (from 1) at `1/k` of its opacity, so an opaque layer drawn
//! over anything ends as exactly the mean of the copies (the running mean). The copies
//! keep the layer's place and frame (the shadow only under the first); being plain draw
//! list entries, every compositor (the GPU's and the CPU reference) draws them the same,
//! so preview and export stay one. A layer drawn translucent (opacity < 1) is approximated:
//! the background shows through a little less than the opacity says, and a rounded
//! frame's anti-aliased edge likewise.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_model::rec::InputTrack;
use gmclip_plan::Plan;
use gmclip_plan::retime;
use gmclip_plan::types::{Rect, Zoom, ZoomTarget};

use super::draw::FrameDraw;

/// The spring's time constant, s.
pub const SPRING_TAU_S: f64 = 0.25;

/// Motion blur's sub-samples across half a frame, and the canvas px a crop corner must
/// move in a frame for them to be drawn (the recorded cursor's, decision 0046).
pub use super::cursor::{BLUR_MIN_PX, BLUR_SAMPLES};

/// The model's ease-in-out (cubic smoothstep, `Curve::EaseInOut`).
pub fn ease_in_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `a + (b − a) · t` per component.
pub fn lerp_rect(a: Rect, b: Rect, t: f64) -> Rect {
    Rect {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        w: a.w + (b.w - a.w) * t,
        h: a.h + (b.h - a.h) * t,
    }
}

/// The crop `depth`× tighter than `c`, centred on `(tx, ty)` and kept inside `c`.
pub fn full_crop(c: Rect, depth: f64, (tx, ty): (f64, f64)) -> Rect {
    let d = depth.max(1.0);
    let (w, h) = (c.w / d, c.h / d);
    Rect {
        x: (tx - w / 2.0).clamp(c.x, c.x + c.w - w),
        y: (ty - h / 2.0).clamp(c.y, c.y + c.h - h),
        w,
        h,
    }
}

/// One step of a critically damped spring (`ω = 1/τ`) toward `r`, held over `dt`:
/// `x(t) = r + (c1 + c2·t)·e^(−ωt)`, `c1 = x0 − r`, `c2 = v0 + ω·c1`.
pub fn spring_step(x: f64, v: f64, r: f64, dt: f64, tau: f64) -> (f64, f64) {
    let w = 1.0 / tau.max(1e-6);
    let c1 = x - r;
    let c2 = v + w * c1;
    let e = (-w * dt).exp();
    (r + (c1 + c2 * dt) * e, (c2 - w * (c1 + c2 * dt)) * e)
}

/// The progress of `z` on frame `f` before easing: `(in, out)`.
fn ramps(z: &Zoom, f: u64) -> (f64, f64) {
    let k = f.saturating_sub(z.out.start());
    let r = z.out.end().saturating_sub(1).saturating_sub(f);
    let ramp = |n: u64, e: u64| {
        if e == 0 {
            1.0
        } else {
            (n as f64 / e as f64).min(1.0)
        }
    };
    (ramp(k, z.ease_in), ramp(r, z.ease_out))
}

/// The eased progress of a lone zoom on frame `f` (no neighbour on its layers).
pub fn progress(z: &Zoom, f: u64) -> f64 {
    let (i, o) = ramps(z, f);
    ease_in_out(i.min(o))
}

/// Whether two zooms name a layer id in common.
fn share(a: &Zoom, b: &Zoom) -> bool {
    a.layers
        .iter()
        .any(|x| b.layers.iter().any(|y| x.layer == y.layer))
}

/// The plan's zooms, their input tracks loaded, and each cursor/focus zoom's springed
/// target per frame (normalised to the track's picture) cached from its first frame.
#[derive(Default)]
pub struct ZoomLayer {
    /// Input tracks by cursor id (`None`: unreadable).
    tracks: HashMap<String, Option<Arc<InputTrack>>>,
    /// Parsed tracks by sha256, kept across plans.
    by_sha: HashMap<String, Option<Arc<InputTrack>>>,
    /// Springed targets by zoom id: frame `out[0] + i` → `(x, y, vx, vy, known, last target x, y)`.
    springs: HashMap<String, Vec<[f64; 7]>>,
    notes: Vec<String>,
    /// The last [`Self::apply`]'s motion-blurred layers: `(index in the draw list, the
    /// sub-sample crops, the frame's own first)`.
    blur: Vec<(usize, Vec<Rect>)>,
}

impl std::fmt::Debug for ZoomLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoomLayer")
            .field("tracks", &self.tracks.len())
            .field("notes", &self.notes)
            .finish()
    }
}

impl ZoomLayer {
    pub fn new(plan: &Plan) -> Self {
        let mut z = Self::default();
        z.set_plan(plan);
        z
    }

    /// Re-reads a new plan's tracks (parsed ones kept by sha256); the spring caches go.
    pub fn set_plan(&mut self, plan: &Plan) {
        self.tracks.clear();
        self.springs.clear();
        self.notes.clear();
        if plan.zooms.as_ref().is_none_or(|z| z.is_empty()) {
            return;
        }
        for c in plan.cursors.iter().flatten() {
            let sha = c.sha256.as_str().to_owned();
            let t = match self.by_sha.get(&sha) {
                Some(t) => t.clone(),
                None => {
                    let t = std::fs::read(&c.path)
                        .map_err(|e| e.kind().to_string())
                        .and_then(|b| {
                            serde_json::from_slice::<InputTrack>(&b).map_err(|e| e.to_string())
                        })
                        .and_then(|t| t.check().map(|_| t).map_err(|e| e.to_string()));
                    let t = match t {
                        Ok(t) => Some(Arc::new(t)),
                        Err(e) => {
                            self.notes.push(format!(
                                "zoom target {}: the input track: {e}; zooms on the centre",
                                c.id.as_str()
                            ));
                            None
                        }
                    };
                    self.by_sha.insert(sha, t.clone());
                    t
                }
            };
            self.tracks.insert(c.id.as_str().to_owned(), t);
        }
    }

    /// Uses `track` for cursor `id` (tests, and hosts holding the track already).
    pub fn set_track(&mut self, id: &str, track: Arc<InputTrack>) {
        self.tracks.insert(id.to_owned(), Some(track));
        self.springs.clear();
    }

    /// What could not be loaded.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The raw target of `z` on frame `f`, normalised to the track's picture.
    fn raw_target(&self, plan: &Plan, z: &Zoom, f: u64) -> Option<(f64, f64)> {
        let (id, focus) = match &z.target {
            ZoomTarget::Cursor(id) => (id, false),
            ZoomTarget::Focus(id) => (id, true),
            ZoomTarget::Point(_) => return None,
        };
        let track = self.tracks.get(id.as_str())?.as_ref()?;
        let part = plan
            .parts
            .iter()
            .find(|p| p.out.start() <= f && f < p.out.end())?;
        let (num, den) = retime::source_time(part, f - part.out.start(), plan.output.fps);
        let t = i64::try_from(num * 1_000_000 / den.max(1)).unwrap_or(i64::MAX);
        let (w, h) = (f64::from(track.w.max(1)), f64::from(track.h.max(1)));
        let centre = |r: [f32; 4]| {
            (
                (f64::from(r[0]) + f64::from(r[2]) / 2.0) / w,
                (f64::from(r[1]) + f64::from(r[3]) / 2.0) / h,
            )
        };
        if focus && let Some(fc) = track.focus_at(t) {
            if let Some(c) = fc.caret {
                return Some(centre(c));
            }
            if let Some(r) = fc.rect {
                return Some(centre(r));
            }
        }
        track
            .cursor_at(t)
            .map(|c| (f64::from(c.x) / w, f64::from(c.y) / h))
    }

    /// The springed target of `z` on frame `f` (normalised), simulated from `out[0]`.
    pub fn target(&mut self, plan: &Plan, z: &Zoom, f: u64) -> Option<(f64, f64)> {
        if !(z.out.start() <= f && f < z.out.end()) {
            return None;
        }
        let i = (f - z.out.start()) as usize;
        let have = self.springs.get(z.id.as_str()).map_or(0, Vec::len);
        if have <= i {
            let (n, d) = plan.output.fps.rate();
            let dt = d as f64 / n as f64;
            let mut v = self.springs.remove(z.id.as_str()).unwrap_or_default();
            for j in v.len()..=i {
                let raw = self.raw_target(plan, z, z.out.start() + j as u64);
                let next = match (v.last().copied(), raw) {
                    // the first known target: the spring starts at rest on it
                    (None, Some((x, y))) => [x, y, 0.0, 0.0, 1.0, x, y],
                    (None, None) => [0.0; 7],
                    (Some(s), Some((x, y))) if s[4] == 0.0 => [x, y, 0.0, 0.0, 1.0, x, y],
                    (Some(s), None) if s[4] == 0.0 => s,
                    // where the track says nothing, it goes on toward the last target
                    (Some(s), r) => {
                        let (rx, ry) = r.unwrap_or((s[5], s[6]));
                        let (x, vx) = spring_step(s[0], s[2], rx, dt, SPRING_TAU_S);
                        let (y, vy) = spring_step(s[1], s[3], ry, dt, SPRING_TAU_S);
                        [x, y, vx, vy, 1.0, rx, ry]
                    }
                };
                v.push(next);
            }
            self.springs.insert(z.id.as_str().to_owned(), v);
        }
        let s = self.springs.get(z.id.as_str())?.get(i)?;
        (s[4] != 0.0).then_some((s[0], s[1]))
    }

    /// The drawn crop of `z` on frame `f` for a layer whose own crop is `c`, and the
    /// zoom's eased progress there (`dims`: the layer's source size).
    fn crop_on(
        &mut self,
        plan: &Plan,
        zooms: &[Zoom],
        z: &Zoom,
        f: u64,
        c: Rect,
        dims: Option<(f64, f64)>,
    ) -> (Rect, f64) {
        let prev = zooms
            .iter()
            .find(|o| o.out.end() == z.out.start() && share(o, z));
        let next = zooms
            .iter()
            .find(|o| o.out.start() == z.out.end() && share(o, z));
        let (ri, ro) = ramps(z, f);
        let ri = if prev.is_some() { 1.0 } else { ri };
        let ro = if next.is_some() { 1.0 } else { ro };
        let p = ease_in_out(ri.min(ro));
        let t = self.target(plan, z, f);
        // a chained zoom eases from the one before's last crop
        let from = match prev {
            Some(o) if z.ease_in > 0 && f - z.out.start() < z.ease_in => {
                let k = f - z.out.start();
                let tp = self.target(plan, o, o.out.end() - 1);
                Some((o, tp, ease_in_out(k as f64 / z.ease_in as f64)))
            }
            _ => None,
        };
        let at = |tgt: Option<(f64, f64)>, z: &Zoom| match (&z.target, tgt) {
            (ZoomTarget::Point([x, y]), _) => (c.x + x * c.w, c.y + y * c.h),
            (_, Some((x, y))) => match dims {
                Some((w, h)) => (x * w, y * h),
                None => (c.x + c.w / 2.0, c.y + c.h / 2.0),
            },
            _ => (c.x + c.w / 2.0, c.y + c.h / 2.0),
        };
        let full = full_crop(c, z.depth, at(t, z));
        let crop = match from {
            Some((o, tp, q)) => {
                let start = full_crop(c, o.depth, at(tp, o));
                lerp_rect(start, full, q)
            }
            None => lerp_rect(c, full, p),
        };
        (crop, p)
    }

    /// Applies `plan`'s zooms to `draw` (crops in source px, places in plan canvas px),
    /// and notes the layers a zoom's motion blur draws as sub-samples
    /// ([`Self::expand_blur`]).
    pub fn apply(&mut self, plan: &Plan, draw: &mut FrameDraw) {
        self.blur.clear();
        let Some(zooms) = plan.zooms.as_deref() else {
            return;
        };
        let f = draw.out_frame;
        let (cw, ch) = (
            plan.output.canvas.w.get() as f64,
            plan.output.canvas.h.get() as f64,
        );
        for z in zooms {
            if !(z.out.start() <= f && f < z.out.end()) {
                continue;
            }
            let blur = z.motion_blur == Some(true);
            for (i, l) in draw.layers.iter_mut().enumerate() {
                if l.is_extra() {
                    continue;
                }
                let named = |refs: &[gmclip_plan::types::LayerRef]| {
                    refs.iter()
                        .any(|r| r.part.as_str() == draw.part_id && r.layer.as_str() == l.layer_id)
                };
                let (crop, p) = {
                    let dims = source_dims(plan, &l.source);
                    let c = l.crop;
                    let (crop, p) = self.crop_on(plan, zooms, z, f, c, dims);
                    if named(&z.layers) {
                        // the crop a frame before, for the shutter (the zoom's own; before
                        // its first frame it starts from where it is)
                        if blur && f > z.out.start() {
                            let (before, _) = self.crop_on(plan, zooms, z, f - 1, c, dims);
                            if moved(crop, before, l.place) {
                                let n = BLUR_SAMPLES - 1;
                                let subs = (0..BLUR_SAMPLES)
                                    .map(|j| lerp_rect(crop, before, j as f64 / (2 * n) as f64))
                                    .collect();
                                self.blur.retain(|b| b.0 != i);
                                self.blur.push((i, subs));
                            }
                        }
                        (Some(crop), p)
                    } else {
                        (None, p)
                    }
                };
                if let Some(crop) = crop {
                    l.crop = crop;
                }
                if let (Some(s), Some(cams)) = (z.camera_scale, z.camera_layers.as_deref())
                    && named(cams)
                {
                    l.place = shrink_to_corner(l.place, 1.0 - (1.0 - s) * p, (cw, ch));
                }
            }
        }
        self.blur.sort_by_key(|b| b.0);
    }

    /// Whether the last [`Self::apply`] left layers to draw motion-blurred.
    pub fn blurs(&self) -> bool {
        !self.blur.is_empty()
    }

    /// Draws the last [`Self::apply`]'s motion-blurred layers as their sub-samples: each
    /// becomes [`BLUR_SAMPLES`] copies in place (the `k`-th at `1/k` of its opacity; the
    /// shadow under the first only), and `draw.tiles_at` follows. Returns where each layer
    /// index (and the end, `len`) now is, for tiles placed by index elsewhere
    /// ([`remap_tiles`]). Without blur the draw is untouched and the map is the identity.
    pub fn expand_blur(&mut self, draw: &mut FrameDraw) -> Vec<usize> {
        let n = draw.layers.len();
        if self.blur.is_empty() {
            return (0..=n).collect();
        }
        let blur = std::mem::take(&mut self.blur);
        let old = std::mem::take(&mut draw.layers);
        let mut map = Vec::with_capacity(n + 1);
        for (i, l) in old.into_iter().enumerate() {
            map.push(draw.layers.len());
            match blur.iter().find(|b| b.0 == i) {
                Some((_, subs)) => {
                    for (k, crop) in subs.iter().enumerate() {
                        let mut c = l.clone();
                        c.crop = *crop;
                        c.opacity = l.opacity / (k + 1) as f32;
                        if k > 0
                            && let Some(sh) = c.shape.as_mut()
                        {
                            sh.shadow = None;
                        }
                        draw.layers.push(c);
                    }
                }
                None => draw.layers.push(l),
            }
        }
        map.push(draw.layers.len());
        remap_tiles(&mut draw.tiles_at, &map);
        map
    }
}

/// Tiles placed before layer `i` (`(i, tile)`), moved to where [`ZoomLayer::expand_blur`]
/// put that layer.
pub fn remap_tiles<T>(tiles: &mut [(usize, T)], map: &[usize]) {
    for t in tiles {
        if let Some(&m) = map.get(t.0) {
            t.0 = m;
        }
    }
}

/// Whether a crop corner moves over [`BLUR_MIN_PX`] canvas px between `before` and `now`
/// (as drawn into `place`).
fn moved(now: Rect, before: Rect, place: Rect) -> bool {
    let to_canvas = |c: Rect, (sx, sy): (f64, f64)| {
        (
            place.x + (sx - c.x) * place.w / c.w.max(1e-9),
            place.y + (sy - c.y) * place.h / c.h.max(1e-9),
        )
    };
    [(now.x, now.y), (now.x + now.w, now.y + now.h)]
        .iter()
        .any(|&s| {
            let (a, b) = (to_canvas(now, s), to_canvas(before, s));
            (a.0 - b.0).hypot(a.1 - b.1) > BLUR_MIN_PX
        })
}

/// A source's picture size from the plan (`expect.video`).
fn source_dims(plan: &Plan, source: &str) -> Option<(f64, f64)> {
    let v = plan
        .sources
        .iter()
        .find(|s| s.id.as_str() == source)?
        .expect
        .as_ref()?
        .video
        .as_ref()?;
    Some((v.w? as f64, v.h? as f64))
}

/// `place` scaled by `s`, keeping the edges at its nearest canvas corner.
pub fn shrink_to_corner(place: Rect, s: f64, (cw, ch): (f64, f64)) -> Rect {
    let (w, h) = (place.w * s, place.h * s);
    let left = place.x + place.w / 2.0 <= cw / 2.0;
    let top = place.y + place.h / 2.0 <= ch / 2.0;
    Rect {
        x: if left { place.x } else { place.x + place.w - w },
        y: if top { place.y } else { place.y + place.h - h },
        w,
        h,
    }
}
