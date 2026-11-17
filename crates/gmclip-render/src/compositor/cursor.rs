//! The recorded cursor (`/1` `cursors[]`, decision 0045, SPEC §5.18): gmclip's recorder
//! hides the cursor in the screen file and logs it, so the compositor draws it from the
//! input track, through the layer that shows the source, as tiles just above that layer.
//!
//! On output frame `f`, for each drawn layer naming a cursor: the source time of the
//! part's frame ([`retime::source_time`]; a held or frozen layer's own frame), the track's
//! (smoothed) hotspot there, mapped through the layer's crop → place; the shape drawn
//! with its hotspot on that point, `pt_w × track.scale × zoom × cursor.scale` wide
//! (`zoom` = place px per crop px, so a 2× crop draws it 2× as the baked cursor would
//! be), clipped to the place, times the layer's opacity, fading out over
//! [`IDLE_FADE_US`] once idle past `hide_idle_ms`. With `clicks`, a ring grows from each
//! click under it. Preview and export draw it through the one compose (R-COMP-1).
//!
//! The cursor's feel (decision 0046) shapes the path before it is drawn: shake removal
//! holds slow small moves still ([`SHAKE_FAST_PX`]), `smoothness` runs a critically
//! damped spring over the held path (time constant [`SPRING_TAU_US`] × smoothness),
//! `loop_ms` glides the last ms of each part back to where the part's first frame had
//! it, `click` draws a ring or a ripple, `click_shrink` dips the cursor to
//! [`CLICK_SHRINK_TO`] for [`CLICK_SHRINK_US`], and `motion_blur` (off by default)
//! spreads a moving cursor over [`BLUR_SAMPLES`] sub-samples across a 180° shutter. Every
//! position is a pure function of the source time: the spring is simulated from
//! [`SPRING_WARMUP_TAUS`] time constants before it at fixed [`SPRING_STEP_US`] steps on a
//! grid anchored at that time, and the shake hold is replayed from the last sample where
//! history cannot matter, so a preview that seeks and an export that plays straight
//! through draw the same pixels (R-COMP-1).
//!
//! A track or image that is missing, unreadable or malformed draws no cursor and leaves
//! a note ([`CursorLayer::notes`]); it never fails a frame.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_model::rec::{InputTrack, MAX_GAP_US};
use gmclip_plan::Plan;
use gmclip_plan::retime;
use gmclip_plan::types::{CursorClick, CursorTrack, Part, Rect, TimeMap};

use super::draw::FrameDraw;
use crate::text::raster::Tile;

/// The fade once the cursor has been idle past `hide_idle_ms`.
pub const IDLE_FADE_US: i64 = 300_000;
/// How long a click's ring grows and fades.
pub const CLICK_RING_US: i64 = 400_000;
/// The ring's final radius in source px (× zoom × cursor scale).
pub const CLICK_RING_PX: f64 = 24.0;
/// A ripple's filled circle, at its start (it fades with the ring).
pub const RIPPLE_FILL_ALPHA: f64 = 0.25;
/// The spring's time constant at `smoothness` 1, µs.
pub const SPRING_TAU_US: f64 = 250_000.0;
/// The spring's fixed simulation step, µs.
pub const SPRING_STEP_US: i64 = 4_000;
/// The spring starts this many time constants before the drawn time (what remains of
/// its start is (1 + 12)·e⁻¹² ≈ 8·10⁻⁵ of the distance).
pub const SPRING_WARMUP_TAUS: f64 = 12.0;
/// Shake removal: the cursor counts as moving (never held) when it went further than
/// [`SHAKE_FAST_PX`] track px over the last [`SHAKE_WINDOW_US`].
pub const SHAKE_WINDOW_US: i64 = 100_000;
pub const SHAKE_FAST_PX: f32 = 40.0;
/// How far back a hold is replayed at most, in samples.
const SHAKE_LOOKBACK: usize = 4096;
/// A click shrinks the cursor to this size and back over this long.
pub const CLICK_SHRINK_TO: f64 = 0.8;
pub const CLICK_SHRINK_US: i64 = 120_000;
/// Motion blur's sub-samples across half a frame (a 180° shutter), and the canvas px a
/// cursor must move in a frame for them to be drawn.
pub const BLUR_SAMPLES: usize = 5;
pub const BLUR_MIN_PX: f64 = 2.0;

/// One plan cursor, loaded.
struct Loaded {
    spec: CursorTrack,
    track: Arc<InputTrack>,
    /// Shape images by the track's shape index (`None`: not loaded).
    shapes: Vec<Option<Arc<gmclip_media::image::Rgba8>>>,
}

/// The plan's cursors: tracks parsed once (by sha256) and shapes resized once per size.
#[derive(Default)]
pub struct CursorLayer {
    cursors: HashMap<String, Loaded>,
    /// Parsed tracks by sha256, kept across plans (`None`: unreadable).
    tracks: HashMap<String, Option<Arc<InputTrack>>>,
    /// Resized premultiplied shapes by (image sha256, w, h).
    sized: HashMap<(String, u32, u32), Arc<Vec<u8>>>,
    notes: Vec<String>,
}

impl std::fmt::Debug for CursorLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorLayer")
            .field("cursors", &self.cursors.len())
            .field("notes", &self.notes)
            .finish()
    }
}

impl CursorLayer {
    /// Loads `plan`'s cursors (none without `cursors[]`).
    pub fn new(plan: &Plan) -> Self {
        let mut c = Self::default();
        c.set_plan(plan);
        c
    }

    /// Re-reads the cursors of a new plan (tracks already parsed are kept by sha256).
    pub fn set_plan(&mut self, plan: &Plan) {
        self.cursors.clear();
        self.notes.clear();
        for spec in plan.cursors.iter().flatten() {
            let sha = spec.sha256.as_str().to_owned();
            let track = match self.tracks.get(&sha) {
                Some(t) => t.clone(),
                None => {
                    let t = match load_track(&spec.path) {
                        Ok(t) => Some(Arc::new(t)),
                        Err(e) => {
                            self.notes.push(format!(
                                "cursor {}: {e}; drawn without it",
                                spec.id.as_str()
                            ));
                            None
                        }
                    };
                    self.tracks.insert(sha, t.clone());
                    t
                }
            };
            let Some(track) = track else { continue };
            let mut shapes = Vec::with_capacity(spec.shapes.len());
            for id in &spec.shapes {
                let img = plan
                    .images
                    .iter()
                    .flatten()
                    .find(|m| &m.id == id)
                    .ok_or_else(|| "not in the plan".to_owned())
                    .and_then(|m| {
                        let bytes = std::fs::read(&m.path).map_err(|e| e.kind().to_string())?;
                        let img = gmclip_media::image::decode_untrusted(&bytes)
                            .map_err(|e| e.to_string())?;
                        if u64::from(img.width) != m.w || u64::from(img.height) != m.h {
                            return Err("the file is not the size the plan says".into());
                        }
                        Ok(img)
                    });
                match img {
                    Ok(i) => shapes.push(Some(Arc::new(i))),
                    Err(e) => {
                        self.notes
                            .push(format!("cursor image {}: {e}", id.as_str()));
                        shapes.push(None);
                    }
                }
            }
            self.cursors.insert(
                spec.id.as_str().to_owned(),
                Loaded {
                    spec: spec.clone(),
                    track,
                    shapes,
                },
            );
        }
    }

    /// What could not be loaded (the cursor is then not drawn).
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The cursor tiles of `draw` (output frame `draw.out_frame` of `plan`), as tiles
    /// drawn among the layers: `(i, t)` draws `t` just before `draw.layers[i]`, so each
    /// cursor sits right above its own layer. Layer crops are in source px and places in
    /// plan canvas px (before any scaling to a preview's size).
    pub fn tiles(&mut self, plan: &Plan, draw: &FrameDraw) -> Vec<(usize, Arc<Tile>)> {
        if self.cursors.is_empty() {
            return Vec::new();
        }
        let Some(part) = plan.parts.iter().find(|p| p.id.as_str() == draw.part_id) else {
            return Vec::new();
        };
        let fps = plan.output.fps;
        let k = draw.out_frame.saturating_sub(part.out.start());
        let mut out = Vec::new();
        for (i, l) in draw.layers.iter().enumerate() {
            if l.is_extra() {
                continue;
            }
            let Some(pl) = part.layers.iter().find(|x| x.id.as_str() == l.layer_id) else {
                continue;
            };
            let Some(c) = pl
                .cursor
                .as_ref()
                .and_then(|id| self.cursors.get(id.as_str()))
            else {
                continue;
            };
            // the source time the layer's picture shows on the part's frame `k`, µs
            let src_us = |k: u64| layer_time_us(part, pl.time_map.as_ref(), k, fps);
            let t_us = src_us(k);
            let frame_us = 1_000_000.0 * fps.rate().1 as f64 / fps.rate().0.max(1) as f64;
            let when = When {
                t_us,
                prev_us: if k > 0 {
                    src_us(k - 1)
                } else {
                    t_us - frame_us.round() as i64
                },
                first_us: src_us(0),
                k: k as f64,
                // the loop's length in output frames, and the part's last frame
                loop_frames: c.spec.loop_ms.unwrap_or(0) as f64 * 1000.0 / frame_us,
                last: part.out.len().saturating_sub(1) as f64,
            };
            // the track's pixels → the source's (they match unless the source says otherwise)
            let src = plan.sources.iter().find(|s| s.id.as_str() == l.source);
            let dims = src
                .and_then(|s| s.expect.as_ref())
                .and_then(|e| e.video.as_ref())
                .and_then(|v| Some((v.w? as f64, v.h? as f64)));
            let (tw, th) = (f64::from(c.track.w), f64::from(c.track.h));
            let (ux, uy) = dims.map_or((1.0, 1.0), |(w, h)| (w / tw, h / th));
            let tiles = cursor_tiles(
                c,
                &mut self.sized,
                &when,
                (ux, uy),
                l.crop,
                l.place,
                l.opacity,
            );
            out.extend(tiles.into_iter().map(|t| (i + 1, t)));
        }
        out
    }
}

/// The source time (µs) a layer's picture shows on its part's output frame `k`.
fn layer_time_us(part: &Part, map: Option<&TimeMap>, k: u64, fps: gmclip_plan::types::Fps) -> i64 {
    let (num, den) = match map {
        Some(TimeMap::FreezeFirst) => retime::source_time(part, 0, fps),
        Some(TimeMap::Hold { frame, .. }) => {
            let (n, d) = match part.picture.fps {
                Some(r) if r.num > 0 => (r.num, r.den),
                _ => fps.rate(),
            };
            (u128::from(*frame) * u128::from(d), u128::from(n))
        }
        None | Some(TimeMap::Live) => retime::source_time(part, k, fps),
    };
    i64::try_from(num * 1_000_000 / den.max(1)).unwrap_or(i64::MAX)
}

/// The times one drawn cursor reads: its source time, the previous output frame's, the
/// part's first frame's, its frame in the part, and the loop.
struct When {
    t_us: i64,
    prev_us: i64,
    first_us: i64,
    k: f64,
    loop_frames: f64,
    last: f64,
}

fn load_track(path: &str) -> Result<InputTrack, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("the input track: {}", e.kind()))?;
    let t: InputTrack =
        serde_json::from_slice(&bytes).map_err(|e| format!("the input track: {e}"))?;
    t.check().map_err(|e| format!("the input track: {e}"))?;
    Ok(t)
}

/// One layer's cursor tiles (a click ring or ripple, then the shape) at `w.t_us`. `unit`
/// is source px per track px.
fn cursor_tiles(
    c: &Loaded,
    sized: &mut HashMap<(String, u32, u32), Arc<Vec<u8>>>,
    w: &When,
    unit: (f64, f64),
    crop: Rect,
    place: Rect,
    opacity: f32,
) -> Vec<Arc<Tile>> {
    let mut out = Vec::new();
    if crop.w <= 0.0 || crop.h <= 0.0 || opacity <= 0.0 {
        return out;
    }
    let t_us = w.t_us;
    let (zx, zy) = (place.w / crop.w, place.h / crop.h);
    let to_canvas = |x: f32, y: f32| {
        (
            place.x + (f64::from(x) * unit.0 - crop.x) * zx,
            place.y + (f64::from(y) * unit.1 - crop.y) * zy,
        )
    };
    let clip = (
        place.x.round() as i64,
        place.y.round() as i64,
        (place.x + place.w).round() as i64,
        (place.y + place.h).round() as i64,
    );
    let spec = &c.spec;
    let style = spec.click_style();
    if style != CursorClick::None
        && let Some((click, age)) = c.track.last_click(t_us)
        && age < CLICK_RING_US
    {
        let f = age as f64 / CLICK_RING_US as f64;
        let (x, y) = to_canvas(click.x, click.y);
        let full = CLICK_RING_PX * spec.scale * unit.0 * zx;
        let width = (2.0 * spec.scale * unit.0 * zx).max(1.0);
        let a = (1.0 - f) * f64::from(opacity);
        let mark = match style {
            // a ripple eases out as it spreads
            CursorClick::Ripple => ring(x, y, full * (1.0 - (1.0 - f).powi(2)), width, a, true),
            _ => ring(x, y, full * f, width, a, false),
        };
        if let Some(t) = mark.and_then(|t| clipped(t, clip)) {
            out.push(Arc::new(t));
        }
    }
    let Some(at) = c.track.cursor_at(t_us) else {
        return out;
    };
    let feel = Feel::of(spec);
    let pos = |t: i64, k: f64| feel.at(&c.track, t, k, w);
    let Some(here) = pos(t_us, w.k) else {
        return out;
    };
    let mut alpha = f64::from(opacity.min(1.0));
    if let Some(h) = spec.hide_idle_ms.filter(|&h| h > 0) {
        let over = at.idle_us - h as i64 * 1000;
        if over > 0 {
            alpha *= 1.0 - (over as f64 / IDLE_FADE_US as f64).min(1.0);
        }
    }
    if alpha <= 0.0 {
        return out;
    }
    let Some(si) = at.shape else { return out };
    let (Some(info), Some(Some(img))) = (c.track.shapes.get(si), c.shapes.get(si)) else {
        return out;
    };
    // the drawn size: its points at the picture's scale, zoomed as the picture is, dipped
    // on a click
    let mut k = f64::from(c.track.scale) * spec.scale;
    if spec.click_shrink == Some(true)
        && let Some((_, age)) = c.track.last_click(t_us)
        && age < CLICK_SHRINK_US
    {
        let f = age as f64 / CLICK_SHRINK_US as f64;
        k *= 1.0 - (1.0 - CLICK_SHRINK_TO) * (std::f64::consts::PI * f).sin();
    }
    let w_px = (f64::from(info.pt_w) * k * unit.0 * zx)
        .round()
        .clamp(1.0, 2048.0) as u32;
    let h_px = (f64::from(info.pt_h) * k * unit.1 * zy)
        .round()
        .clamp(1.0, 2048.0) as u32;
    let hx = f64::from(info.hot_x) * f64::from(w_px) / f64::from(info.w.max(1));
    let hy = f64::from(info.hot_y) * f64::from(h_px) / f64::from(info.h.max(1));
    let px = sized
        .entry((info.sha256.clone(), w_px, h_px))
        .or_insert_with(|| Arc::new(gmclip_media::image::fit(img, w_px, h_px)))
        .clone();
    let corner = |(x, y): (f32, f32)| {
        let (x, y) = to_canvas(x, y);
        ((x - hx).round() as i32, (y - hy).round() as i32)
    };
    let (x0, y0) = corner(here);
    // motion blur: where the cursor was over the last half frame, when it moved
    let mut spots = vec![(x0, y0)];
    if spec.motion_blur == Some(true)
        && let Some(prev) = pos(w.prev_us, w.k - 1.0)
    {
        let (a, b) = (to_canvas(here.0, here.1), to_canvas(prev.0, prev.1));
        if (a.0 - b.0).hypot(a.1 - b.1) > BLUR_MIN_PX {
            let n = BLUR_SAMPLES - 1;
            for i in 1..=n {
                let f = i as f64 / (2 * n) as f64;
                let t = t_us - ((t_us - w.prev_us) as f64 * f).round() as i64;
                if let Some(p) = pos(t, w.k - f) {
                    spots.push(corner(p));
                }
            }
        }
    }
    let mut t = if spots.len() > 1 {
        smear(&px, w_px, h_px, &spots)
    } else {
        Tile {
            x: x0,
            y: y0,
            width: w_px,
            height: h_px,
            rgba: px.as_ref().clone(),
        }
    };
    super::text_layer::fade(&mut t, alpha);
    if let Some(t) = clipped(t, clip) {
        out.push(Arc::new(t));
    }
    out
}

/// The shape drawn at each of `spots` (top-left corners), each at 1/n, summed into one
/// tile (premultiplied, so where every copy overlaps it is the shape itself).
fn smear(px: &[u8], w: u32, h: u32, spots: &[(i32, i32)]) -> Tile {
    let x0 = spots.iter().map(|s| s.0).min().unwrap_or(0);
    let y0 = spots.iter().map(|s| s.1).min().unwrap_or(0);
    let x1 = spots.iter().map(|s| s.0).max().unwrap_or(0) + w as i32;
    let y1 = spots.iter().map(|s| s.1).max().unwrap_or(0) + h as i32;
    let (tw, th) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut sum = vec![0u32; tw * th * 4];
    for &(sx, sy) in spots {
        let (ox, oy) = ((sx - x0) as usize, (sy - y0) as usize);
        for j in 0..h as usize {
            let src = j * w as usize * 4;
            let dst = ((oy + j) * tw + ox) * 4;
            for (d, s) in sum[dst..dst + w as usize * 4]
                .iter_mut()
                .zip(&px[src..src + w as usize * 4])
            {
                *d += u32::from(*s);
            }
        }
    }
    let n = spots.len() as u32;
    Tile {
        x: x0,
        y: y0,
        width: tw as u32,
        height: th as u32,
        rgba: sum.iter().map(|v| ((v + n / 2) / n) as u8).collect(),
    }
}

/// The cursor's feel (decision 0046): how the drawn path is made from the recorded one.
#[derive(Clone, Copy, Debug)]
struct Feel {
    /// The older averaging window (`smooth_ms`), used when there is no spring.
    window_us: i64,
    /// The spring's time constant, µs (0: no spring).
    tau_us: f64,
    /// Shake removal, track px (0: off).
    shake: f32,
}

impl Feel {
    fn of(spec: &CursorTrack) -> Self {
        let sm = spec.smoothness.filter(|s| s.is_finite()).unwrap_or(0.0);
        Self {
            window_us: spec.smooth_ms.unwrap_or(0) as i64 * 1000,
            tau_us: SPRING_TAU_US * sm.clamp(0.0, 1.0),
            shake: spec
                .shake_px
                .filter(|s| s.is_finite())
                .unwrap_or(0.0)
                .clamp(0.0, 20.0) as f32,
        }
    }

    /// Where the cursor is drawn at source time `t` on part frame `k` (fractional for
    /// motion blur), in track px: the shaped path, then the loop's glide.
    fn at(&self, track: &InputTrack, t: i64, k: f64, w: &When) -> Option<(f32, f32)> {
        let p = self.path(track, t)?;
        if w.loop_frames <= 0.0 {
            return Some(p);
        }
        let e = ((k - (w.last - w.loop_frames)) / w.loop_frames).clamp(0.0, 1.0);
        if e <= 0.0 {
            return Some(p);
        }
        let Some(q) = self.path(track, w.first_us) else {
            return Some(p);
        };
        let e = (e * e * (3.0 - 2.0 * e)) as f32;
        Some((p.0 + (q.0 - p.0) * e, p.1 + (q.1 - p.1) * e))
    }

    /// The shaped path at `t` (none where the track has no cursor).
    fn path(&self, track: &InputTrack, t: i64) -> Option<(f32, f32)> {
        if self.tau_us > 0.0 {
            return spring(track, self, t);
        }
        if self.shake > 0.0 {
            return Held::new(track, self.shake, t, t).at(t);
        }
        track.smoothed_at(t, self.window_us).map(|a| (a.x, a.y))
    }
}

/// A critically damped spring following the (held) path, simulated over fixed steps on a
/// grid ending at `t` from [`SPRING_WARMUP_TAUS`] time constants before, starting at
/// rest on the path: a pure function of `t`. Where the track has no cursor the spring
/// starts over from the next sample.
fn spring(track: &InputTrack, feel: &Feel, t: i64) -> Option<(f32, f32)> {
    track.cursor_at(t)?;
    let n = (SPRING_WARMUP_TAUS * feel.tau_us / SPRING_STEP_US as f64).ceil() as i64;
    let start = t - n * SPRING_STEP_US;
    let held = Held::new(track, feel.shake, start, t);
    // ω per step; the exact step of x'' = ω²(g − x) − 2ωx' with g held over it
    let wd = SPRING_STEP_US as f64 / feel.tau_us;
    let e = (-wd).exp();
    let mut st: Option<([f64; 2], [f64; 2])> = None;
    for i in 0..=n {
        let u = start + i * SPRING_STEP_US;
        let Some(g) = held.at(u) else {
            st = None;
            continue;
        };
        let g = [f64::from(g.0), f64::from(g.1)];
        match &mut st {
            None => st = Some((g, [0.0; 2])),
            Some((x, v)) => {
                for a in 0..2 {
                    let y = x[a] - g[a];
                    let c = v[a] + wd * y;
                    x[a] = g[a] + (y + c) * e;
                    v[a] = (v[a] - wd * c) * e;
                }
            }
        }
    }
    st.map(|(x, _)| (x[0] as f32, x[1] as f32))
}

/// The shake-held path over a span of samples: a slow cursor stays where it was until it
/// moves [`Feel::shake`] px away from there. Replayed from the last sample at or before
/// the span where the hold starts over whatever came before (a gap, off the picture, or
/// moving fast), so it is the same however it is reached.
struct Held<'a> {
    track: &'a InputTrack,
    /// The held point of samples `first..first + pts.len()`.
    first: usize,
    pts: Vec<(f32, f32)>,
}

impl<'a> Held<'a> {
    fn new(track: &'a InputTrack, shake: f32, from: i64, to: i64) -> Self {
        let cur = &track.cursor;
        let mut held = Self {
            track,
            first: 0,
            pts: Vec::new(),
        };
        if shake <= 0.0 || cur.is_empty() {
            return held;
        }
        let lo = cur.partition_point(|s| s.0 <= from).saturating_sub(1);
        let hi = (cur.partition_point(|s| s.0 <= to) + 1).min(cur.len());
        let floor = lo.saturating_sub(SHAKE_LOOKBACK);
        let mut s = lo;
        while s > floor && !Self::starts_over(track, s) {
            s -= 1;
        }
        held.first = s;
        let mut h = (cur[s].1, cur[s].2);
        held.pts.push(h);
        for (j, c) in cur.iter().enumerate().take(hi.max(s + 1)).skip(s + 1) {
            let p = (c.1, c.2);
            if Self::starts_over(track, j) || (p.0 - h.0).hypot(p.1 - h.1) >= shake {
                h = p;
            }
            held.pts.push(h);
        }
        held
    }

    /// Whether sample `j` holds nothing from before it.
    fn starts_over(track: &InputTrack, j: usize) -> bool {
        let (t, x, y, _) = track.cursor[j];
        let off = x < 0.0 || y < 0.0 || x >= track.w as f32 || y >= track.h as f32;
        if j == 0 || off || t - track.cursor[j - 1].0 > MAX_GAP_US {
            return true;
        }
        match track.cursor_at(t - SHAKE_WINDOW_US) {
            None => true,
            Some(a) => (a.x - x).hypot(a.y - y) >= SHAKE_FAST_PX,
        }
    }

    /// The held path at `u`: between two samples, from one held point to the next, as
    /// [`InputTrack::cursor_at`] goes between the samples themselves.
    fn at(&self, u: i64) -> Option<(f32, f32)> {
        let raw = self.track.cursor_at(u)?;
        if self.pts.is_empty() {
            return Some((raw.x, raw.y));
        }
        let cur = &self.track.cursor;
        let i = cur.partition_point(|s| s.0 <= u).checked_sub(1)?;
        let Some(a) = i.checked_sub(self.first).and_then(|o| self.pts.get(o)) else {
            return Some((raw.x, raw.y));
        };
        match (cur.get(i + 1), self.pts.get(i + 1 - self.first)) {
            (Some(nb), Some(b)) if nb.0 - cur[i].0 <= MAX_GAP_US && nb.0 > cur[i].0 => {
                let f = (u - cur[i].0) as f32 / (nb.0 - cur[i].0) as f32;
                Some((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f))
            }
            _ => Some(*a),
        }
    }
}

/// `t` cut to the rect `(x0, y0, x1, y1)`; `None` when nothing of it is inside.
fn clipped(t: Tile, (x0, y0, x1, y1): (i64, i64, i64, i64)) -> Option<Tile> {
    let (tx0, ty0) = (i64::from(t.x), i64::from(t.y));
    let (tx1, ty1) = (tx0 + i64::from(t.width), ty0 + i64::from(t.height));
    let (a, b) = (tx0.max(x0), ty0.max(y0));
    let (c, d) = (tx1.min(x1), ty1.min(y1));
    if c <= a || d <= b {
        return None;
    }
    if (a, b, c, d) == (tx0, ty0, tx1, ty1) {
        return Some(t);
    }
    let (w, h) = ((c - a) as usize, (d - b) as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in b..d {
        let row = ((y - ty0) as usize * t.width as usize + (a - tx0) as usize) * 4;
        rgba.extend_from_slice(&t.rgba[row..row + w * 4]);
    }
    Some(Tile {
        x: a as i32,
        y: b as i32,
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

/// A white ring of radius `r` and `width` centred on `(cx, cy)` at `alpha` (coverage by
/// distance from the circle, one pixel of antialiasing), premultiplied; with `fill`, over
/// a disc of the same radius at [`RIPPLE_FILL_ALPHA`] (a ripple).
fn ring(cx: f64, cy: f64, r: f64, width: f64, alpha: f64, fill: bool) -> Option<Tile> {
    if alpha <= 0.0 || r <= 0.0 {
        return None;
    }
    let outer = r + width / 2.0 + 1.0;
    let x0 = (cx - outer).floor() as i32;
    let y0 = (cy - outer).floor() as i32;
    let n = (2.0 * outer).ceil() as u32 + 1;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    for j in 0..n {
        for i in 0..n {
            let dx = f64::from(x0) + f64::from(i) + 0.5 - cx;
            let dy = f64::from(y0) + f64::from(j) + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let d = (dist - r).abs();
            let mut cov = (width / 2.0 + 0.5 - d).clamp(0.0, 1.0) * alpha * 0.9;
            if fill {
                let disc = (r + 0.5 - dist).clamp(0.0, 1.0) * alpha * RIPPLE_FILL_ALPHA;
                cov += disc * (1.0 - cov);
            }
            let v = (cov * 255.0).round() as u8;
            let s = ((j * n + i) * 4) as usize;
            rgba[s..s + 4].copy_from_slice(&[v, v, v, v]);
        }
    }
    Some(Tile {
        x: x0,
        y: y0,
        width: n,
        height: n,
        rgba,
    })
}

/// `cursor` tiles merged into `among` (both `(before layer i, tile)` in drawing order):
/// a cursor goes before every other tile at its index, so it sits right on its layer.
pub fn merge_among(
    among: Vec<(usize, Arc<Tile>)>,
    cursor: Vec<(usize, Arc<Tile>)>,
) -> Vec<(usize, Arc<Tile>)> {
    if cursor.is_empty() {
        return among;
    }
    let mut out = Vec::with_capacity(among.len() + cursor.len());
    let mut a = among.into_iter().peekable();
    for c in cursor {
        while let Some(x) = a.next_if(|x| x.0 < c.0) {
            out.push(x);
        }
        out.push(c);
    }
    out.extend(a);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_keeps_the_inside() {
        let t = Tile {
            x: -1,
            y: 0,
            width: 3,
            height: 2,
            rgba: (0..24).collect(),
        };
        let c = clipped(t.clone(), (0, 1, 10, 10)).unwrap();
        assert_eq!((c.x, c.y, c.width, c.height), (0, 1, 2, 1));
        assert_eq!(c.rgba, (16..24).collect::<Vec<u8>>());
        assert!(clipped(t, (5, 5, 10, 10)).is_none());
    }

    #[test]
    fn a_cursor_goes_first_at_its_index() {
        let t = |x| {
            Arc::new(Tile {
                x,
                y: 0,
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            })
        };
        let m = merge_among(vec![(1, t(10)), (2, t(20))], vec![(1, t(1)), (3, t(3))]);
        let xs: Vec<(usize, i32)> = m.iter().map(|(i, t)| (*i, t.x)).collect();
        assert_eq!(xs, [(1, 1), (1, 10), (2, 20), (3, 3)]);
    }
}
