//! Follow the cursor (SPEC §5.7 "auto punch-in"; AJ 2026-10-15: "only needed some
//! times, but a nice feature to have"): where the mouse cursor is on a screen recording,
//! from frames sampled at 2 fps, and a zoomed crop path that follows it with eased moves.
//!
//! **Finding the cursor** ([`track`]): frame differencing says where to look, a cursor
//! template says whether it is there. Between consecutive samples the pixels whose luma
//! changed by ≥ [`DIFF`] inside the screen region (minus excluded areas: the camera
//! inset, chat) form blobs; around each blob no bigger than a few cursors, an arrow
//! template (tip top-left, [`SIZES`] heights) is slid over the later frame. A placement
//! scores by how many of its body pixels are dark and its rim pixels bright (the macOS
//! arrow; the inverse for a white arrow); the best placement scoring ≥ [`MIN_SCORE`] that
//! did not already score in the earlier frame (so it is where the cursor went, not where
//! it was) is the cursor. A sample with no change near the last position keeps it; a
//! sample where most of the screen changed (a scroll, a window switch) is unknown.
//!
//! **The crop path** ([`crop_path`]): a crop [`FollowRules::max_zoom`] × smaller than the
//! layer's own, inside it, centred on the cursor. It holds while the cursor stays inside
//! its middle [`FollowRules::dead`] share, moves when the cursor leaves it (at most once a
//! second), and zooms back out after the cursor has been gone [`FollowRules::hold`]
//! samples. The host turns each key into an eased visual split, so the preview and the
//! export draw the same moves through the shared compose.
//!
//! Pure and deterministic: the host supplies the luma frames.

use crate::activity::{LumaFrame, NRect};

/// A luma step that counts as a change (8-bit codes).
pub const DIFF: u8 = 40;
/// Cursor heights tried, as shares of the frame height (a 1440p recording's arrow is
/// ≈ 1 % of the height in a 1080p render of it, 2 % at 1×, 3 % on a Retina capture).
pub const SIZES: [f64; 8] = [0.010, 0.012, 0.014, 0.017, 0.021, 0.027, 0.034, 0.043];
/// The least template score of a cursor.
pub const MIN_SCORE: f64 = 0.8;
/// The most the same placement may score in the earlier frame (it is where it went).
pub const WAS_THERE: f64 = 0.65;
/// More than this share of the region changing is a scroll or a window switch.
pub const BUSY: f64 = 0.08;
/// More than this share of the coarse cells around a placement changing is moving
/// picture (a camera, a playing video), not a cursor: a cursor's move changes a few cells.
pub const BUSY_NEAR: f64 = 0.06;
/// Luma at or below this is the arrow's dark, at or above [`BRIGHT`] its light.
const DARK: u8 = 70;
const BRIGHT: u8 = 190;
/// A rim this much lighter (darker) than the body counts too (a small, softened cursor).
const RIM_STEP: i32 = 60;

/// The arrow cursor's outline in its box (x right, y down; tip at the top left): the
/// shape macOS and Windows both draw, close enough at a few pixels high.
const ARROW: [(f64, f64); 7] = [
    (0.0, 0.0),
    (0.0, 0.84),
    (0.22, 0.66),
    (0.36, 1.0),
    (0.52, 0.94),
    (0.38, 0.62),
    (0.66, 0.62),
];

fn in_arrow(x: f64, y: f64) -> bool {
    // even-odd ray casting
    let mut inside = false;
    let n = ARROW.len();
    for i in 0..n {
        let (xi, yi) = ARROW[i];
        let (xj, yj) = ARROW[(i + n - 1) % n];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
    }
    inside
}

/// The arrow at one height: body and rim pixel offsets from the tip.
#[derive(Clone, Debug)]
struct Template {
    h: u32,
    w: u32,
    body: Vec<(u32, u32)>,
    /// Rim offsets are relative to the tip shifted by (1, 1) (they can sit left of it).
    rim: Vec<(u32, u32)>,
    /// A few body pixels checked first (most placements fail on them).
    probe: Vec<(u32, u32)>,
    /// A few rim pixels (offsets like `rim`) checked next.
    rim_probe: Vec<(u32, u32)>,
    /// Box pixels two or more away from the arrow (offsets from the tip): what is around
    /// it. A filled glyph fills them; a cursor does not.
    out: Vec<(u32, u32)>,
    /// The arrow's width in each of its top rows (its diagonal: a box has none).
    row_w: Vec<u32>,
}

impl Template {
    fn new(h: u32) -> Template {
        let w = ((f64::from(h) * 0.66).ceil() as u32).max(2);
        let s = f64::from(h);
        let at = |x: i64, y: i64| in_arrow((x as f64 + 0.5) / s, (y as f64 + 0.5) / s);
        let mut body = Vec::new();
        let mut rim = Vec::new();
        for y in -1..=i64::from(h) {
            for x in -1..=i64::from(w) {
                if at(x, y) {
                    if x >= 0 && y >= 0 {
                        body.push((x as u32, y as u32));
                    }
                } else if (-1..=1).any(|dx| (-1..=1).any(|dy| at(x + dx, y + dy))) {
                    rim.push(((x + 1) as u32, (y + 1) as u32));
                }
            }
        }
        // the body's core (its edge pixels are where a thin outline lands, either side)
        let has = |x: i64, y: i64| x >= 0 && y >= 0 && at(x, y);
        let core: Vec<(u32, u32)> = body
            .iter()
            .copied()
            .filter(|&(x, y)| {
                let (x, y) = (i64::from(x), i64::from(y));
                has(x - 1, y) && has(x + 1, y) && has(x, y - 1) && has(x, y + 1)
            })
            .collect();
        let body = if core.len() >= 6 { core } else { body };
        let step = (body.len() / 12).max(1);
        let probe = body.iter().step_by(step).copied().collect();
        let rstep = (rim.len() / 12).max(1);
        let row_w: Vec<u32> = (0..(i64::from(h) * 3 / 5))
            .map(|y| (0..i64::from(w)).filter(|&x| at(x, y)).count() as u32)
            .collect();
        let mut out = Vec::new();
        for y in 0..i64::from(h) {
            for x in 0..i64::from(w) {
                if !(-2..=2).any(|dx| (-2..=2).any(|dy| has(x + dx, y + dy))) {
                    out.push((x as u32, y as u32));
                }
            }
        }
        let rim_probe = rim.iter().step_by(rstep).copied().collect();
        Template {
            h,
            w,
            body,
            rim,
            probe,
            rim_probe,
            out,
            row_w,
        }
    }

    /// Whether the placement covers changed pixels (`mask`): where the cursor went did
    /// change, under its body or its rim.
    fn on_change(&self, mask: &[bool], w: u32, x: u32, y: u32) -> bool {
        let n = self
            .probe
            .iter()
            .filter(|&&(dx, dy)| mask[((y + dy) * w + x + dx) as usize])
            .count()
            + self
                .rim_probe
                .iter()
                .filter(|&&(dx, dy)| mask[((y + dy - 1) * w + x + dx - 1) as usize])
                .count();
        n * 5 >= self.probe.len() + self.rim_probe.len()
    }

    /// Score of the tip at (x, y) in `f`: dark body and a light rim, or the inverse.
    /// `(body share + rim share) / 2`, each counted with slack (a softened, scaled cursor
    /// is not the template's exact shape): a rim pixel passes when it or a 4-neighbour is
    /// light (≥ [`BRIGHT`], or [`RIM_STEP`] above the body's mean).
    fn score(&self, f: &LumaFrame, x: u32, y: u32) -> f64 {
        let w = f.width;
        if x < 2 || y < 2 || x + self.w + 3 >= f.width || y + self.h + 3 >= f.height {
            return 0.0;
        }
        let px = |dx: u32, dy: u32| f.data[((y + dy) * w + x + dx) as usize];
        // rim offsets sit one up and left of the tip; ±1 more for the slack
        let rv = |dx: u32, dy: u32, ox: i32, oy: i32| {
            let xx = (x + dx) as i32 - 1 + ox;
            let yy = (y + dy) as i32 - 1 + oy;
            f.data[(yy as u32 * w + xx as u32) as usize]
        };
        let mut best = 0.0f64;
        for dark_body in [true, false] {
            let body_ok = |v: u8| if dark_body { v <= DARK } else { v >= BRIGHT };
            let hits = self
                .probe
                .iter()
                .filter(|&&(dx, dy)| body_ok(px(dx, dy)))
                .count();
            if hits * 10 < self.probe.len() * 6 {
                continue;
            }
            let mean = |pts: &[(u32, u32)]| {
                pts.iter()
                    .map(|&(dx, dy)| i32::from(px(dx, dy)))
                    .sum::<i32>()
                    / pts.len().max(1) as i32
            };
            let rim_hit = |m: i32, dx: u32, dy: u32| {
                [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|&(ox, oy)| {
                        let v = rv(dx, dy, ox, oy);
                        if dark_body {
                            v >= BRIGHT || i32::from(v) >= m + RIM_STEP
                        } else {
                            v <= DARK || i32::from(v) <= m - RIM_STEP
                        }
                    })
            };
            let pm = mean(&self.probe);
            let rp = self
                .rim_probe
                .iter()
                .filter(|&&(dx, dy)| rim_hit(pm, dx, dy))
                .count();
            if rp * 2 < self.rim_probe.len() {
                continue;
            }
            let bm = mean(&self.body);
            let b = self
                .body
                .iter()
                .filter(|&&(dx, dy)| body_ok(px(dx, dy)))
                .count();
            let r = self
                .rim
                .iter()
                .filter(|&&(dx, dy)| rim_hit(bm, dx, dy))
                .count();
            let bf = b as f64 / self.body.len().max(1) as f64;
            let rf = r as f64 / self.rim.len().max(1) as f64;
            // both: a plain light area is an inverted arrow's body but has no dark rim
            // the shape is the body's (light around it) or the rim's (dark around it, the
            // rim lighter than the surroundings); a filled glyph is neither
            if self.out.len() >= 4 {
                let around = self
                    .out
                    .iter()
                    .filter(|&&(dx, dy)| body_ok(px(dx, dy)))
                    .count();
                let om = mean(&self.out);
                let rm = self
                    .rim
                    .iter()
                    .map(|&(dx, dy)| i32::from(rv(dx, dy, 0, 0)))
                    .sum::<i32>()
                    / self.rim.len().max(1) as i32;
                let rim_stands_out = if dark_body {
                    rm - om >= RIM_STEP / 2
                } else {
                    om - rm >= RIM_STEP / 2
                };
                if around * 10 > self.out.len() * 3 && !rim_stands_out {
                    continue;
                }
            }
            // the diagonal: in the top rows the body widens with the row
            let mut err = 0u32;
            let mut runs = Vec::with_capacity(self.row_w.len());
            for (k, &e) in self.row_w.iter().enumerate() {
                let k = k as u32;
                let start = u32::from(!body_ok(px(0, k)));
                let run = (start..self.w + 2)
                    .take_while(|&dx| body_ok(px(dx, k)))
                    .count() as u32;
                err += run.abs_diff(e);
                runs.push(run);
            }
            let rows = self.row_w.len().max(1) as f64;
            if f64::from(err) / rows > (0.18 * f64::from(self.w)).max(1.5) {
                continue;
            }
            // … and keeps widening, about a pixel a row (a box's rows are all one width):
            // the least-squares slope of the rows the body is in
            let pts: Vec<(f64, f64)> = runs
                .iter()
                .enumerate()
                .filter(|(_, r)| **r > 0)
                .map(|(k, r)| (k as f64, f64::from(*r)))
                .collect();
            let n = pts.len() as f64;
            if n >= 3.0 {
                let (mx, my) = (
                    pts.iter().map(|p| p.0).sum::<f64>() / n,
                    pts.iter().map(|p| p.1).sum::<f64>() / n,
                );
                let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
                let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
                if sxx > 0.0 && sxy / sxx < 0.5 {
                    continue;
                }
            }
            if bf >= 0.7 && rf >= 0.6 {
                best = best.max((bf + rf) / 2.0);
            }
        }
        best
    }
}

/// A blob's box (changed pixels, 4-connected).
#[derive(Copy, Clone, Debug)]
struct Bbox {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

fn inside(r: &NRect, x: f64, y: f64) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// What one pair of consecutive samples says, before the walk that carries the last
/// position (computed for all pairs in parallel; the walk is sequential).
#[derive(Clone, Debug, Default)]
struct Pair {
    /// Comparable frames with some region to look in.
    usable: bool,
    /// Most of the region changed.
    busy: bool,
    /// Changed pixels on a coarse grid ([`GRID`] px cells), to tell whether the place
    /// the cursor was at changed.
    grid: Vec<bool>,
    grid_w: u32,
    /// The best new cursor placement: `(score, centre x, centre y)` in frame px.
    best: Option<(f64, f64, f64)>,
}

/// The coarse change grid's cell size (px).
const GRID: u32 = 8;

fn pair(
    a: &LumaFrame,
    b: &LumaFrame,
    region: &NRect,
    exclude: &[NRect],
    templates: &[Template],
    max_h: u32,
) -> Pair {
    if a.width != b.width || a.height != b.height || a.data.len() != b.data.len() {
        return Pair::default();
    }
    let (w, h) = (b.width, b.height);
    let (fw, fh) = (f64::from(w), f64::from(h));
    // changed pixels in the region, outside the excluded areas
    let mut mask = vec![false; (w * h) as usize];
    let grid_w = w.div_ceil(GRID);
    let mut grid = vec![false; (grid_w * h.div_ceil(GRID)) as usize];
    let (mut changed, mut area) = (0u64, 0u64);
    for y in 0..h {
        let ny = (f64::from(y) + 0.5) / fh;
        for x in 0..w {
            let nx = (f64::from(x) + 0.5) / fw;
            if !inside(region, nx, ny) || exclude.iter().any(|r| inside(r, nx, ny)) {
                continue;
            }
            area += 1;
            let k = (y * w + x) as usize;
            if a.data[k].abs_diff(b.data[k]) >= DIFF {
                mask[k] = true;
                grid[((y / GRID) * grid_w + x / GRID) as usize] = true;
                changed += 1;
            }
        }
    }
    let mut p = Pair {
        usable: area > 0,
        busy: changed as f64 > BUSY * area as f64,
        grid,
        grid_w,
        best: None,
    };
    if !p.usable || p.busy || changed == 0 {
        return p;
    }
    // where to look: each small blob, grown by a cursor's size
    for bb in blobs(&mask, w, h, 3 * max_h) {
        for t in templates {
            let x0 = bb.x0.saturating_sub(t.w + 1).max(2);
            let y0 = bb.y0.saturating_sub(t.h + 1).max(2);
            let x1 = (bb.x1 + 1).min(w.saturating_sub(t.w + 4));
            let y1 = (bb.y1 + 1).min(h.saturating_sub(t.h + 4));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if !t.on_change(&mask, w, x, y) {
                        continue;
                    }
                    if busy_near(&p.grid, grid_w, x, y, 2 * max_h) {
                        continue;
                    }
                    let s = t.score(b, x, y);
                    if s < MIN_SCORE || p.best.is_some_and(|(bs, _, _)| s <= bs) {
                        continue;
                    }
                    // the tip's placement must be new (not where it was)
                    if t.score(a, x, y) >= WAS_THERE {
                        continue;
                    }
                    let cx = f64::from(x) + f64::from(t.w) * 0.4;
                    let cy = f64::from(y) + f64::from(t.h) * 0.5;
                    p.best = Some((s, cx, cy));
                }
            }
        }
    }
    p
}

/// Whether more than [`BUSY_NEAR`] of the grid cells within `r` px of (x, y) changed.
fn busy_near(grid: &[bool], grid_w: u32, x: u32, y: u32, r: u32) -> bool {
    let rows = grid.len() as u32 / grid_w.max(1);
    let (gx0, gx1) = (x.saturating_sub(r) / GRID, ((x + r) / GRID).min(grid_w - 1));
    let (gy0, gy1) = (
        y.saturating_sub(r) / GRID,
        ((y + r) / GRID).min(rows.saturating_sub(1)),
    );
    let (mut on, mut n) = (0u32, 0u32);
    for gy in gy0..=gy1 {
        for gx in gx0..=gx1 {
            n += 1;
            on += u32::from(grid[(gy * grid_w + gx) as usize]);
        }
    }
    f64::from(on) > BUSY_NEAR * f64::from(n.max(1))
}

/// The cursor's centre in each sample (normalised to the frame), `None` where it is not
/// known. `frames` are consecutive samples of one picture (same size). The pairs are
/// searched on all cores; the result does not depend on how many.
pub fn track(frames: &[LumaFrame], region: &NRect, exclude: &[NRect]) -> Vec<Option<(f64, f64)>> {
    let mut out = vec![None; frames.len()];
    let Some(f0) = frames.first() else {
        return out;
    };
    let fh0 = f64::from(f0.height);
    let templates: Vec<Template> = SIZES
        .iter()
        .map(|k| Template::new(((k * fh0).round() as u32).max(6)))
        .collect();
    let max_h = templates.last().map_or(8, |t| t.h);
    let n = frames.len().saturating_sub(1);
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 8)
        .min(n.max(1));
    let mut pairs: Vec<Pair> = vec![Pair::default(); n];
    std::thread::scope(|sc| {
        let handles: Vec<_> = (0..workers)
            .map(|k| {
                let (templates, frames) = (&templates, frames);
                sc.spawn(move || {
                    (k..n)
                        .step_by(workers)
                        .map(|i| {
                            (
                                i,
                                pair(
                                    &frames[i],
                                    &frames[i + 1],
                                    region,
                                    exclude,
                                    templates,
                                    max_h,
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for hd in handles {
            for (i, p) in hd.join().unwrap_or_default() {
                pairs[i] = p;
            }
        }
    });
    let (fw, fh) = (f64::from(f0.width), fh0);
    let mut last: Option<(f64, f64)> = None;
    for (k, p) in pairs.iter().enumerate() {
        let i = k + 1;
        if !p.usable {
            last = None;
            continue;
        }
        if p.busy {
            // a scroll or a window switch: where the cursor is now is not known
            last = None;
            continue;
        }
        // the cursor did not move when nothing changed where it was (typing elsewhere,
        // a playing video) — its old place changes as soon as it leaves
        if let Some((lx, ly)) = last {
            let r = i64::from(max_h);
            let (cx, cy) = ((lx * fw) as i64, (ly * fh) as i64);
            let g = i64::from(GRID);
            let rows = p.grid.len() as i64 / i64::from(p.grid_w.max(1));
            let moved = ((cy - r).max(0) / g..=((cy + r) / g).min(rows - 1)).any(|gy| {
                ((cx - r).max(0) / g..=((cx + r) / g).min(i64::from(p.grid_w) - 1))
                    .any(|gx| p.grid[(gy * i64::from(p.grid_w) + gx) as usize])
            });
            if !moved {
                out[i] = last;
                continue;
            }
        }
        match p.best {
            Some((_, x, y)) => {
                let q = ((x / fw).clamp(0.0, 1.0), (y / fh).clamp(0.0, 1.0));
                out[i] = Some(q);
                last = Some(q);
            }
            // nothing cursor-like moved: it is where it was
            None => out[i] = last,
        }
    }
    out
}

/// Boxes of the 4-connected blobs of `mask`; blobs larger than `max_side` either way are
/// dropped (a cursor is never that big).
fn blobs(mask: &[bool], w: u32, h: u32, max_side: u32) -> Vec<Bbox> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    let mut stack: Vec<(u32, u32)> = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let (sx, sy) = ((start as u32) % w, (start as u32) / w);
        let mut b = Bbox {
            x0: sx,
            y0: sy,
            x1: sx,
            y1: sy,
        };
        seen[start] = true;
        stack.push((sx, sy));
        while let Some((x, y)) = stack.pop() {
            b.x0 = b.x0.min(x);
            b.y0 = b.y0.min(y);
            b.x1 = b.x1.max(x);
            b.y1 = b.y1.max(y);
            let mut push = |xx: u32, yy: u32| {
                let k = (yy * w + xx) as usize;
                if mask[k] && !seen[k] {
                    seen[k] = true;
                    stack.push((xx, yy));
                }
            };
            if x > 0 {
                push(x - 1, y);
            }
            if x + 1 < w {
                push(x + 1, y);
            }
            if y > 0 {
                push(x, y - 1);
            }
            if y + 1 < h {
                push(x, y + 1);
            }
        }
        if b.x1 - b.x0 < max_side && b.y1 - b.y0 < max_side {
            out.push(b);
        }
    }
    out
}

/// What the crop path keeps to.
#[derive(Clone, Debug, PartialEq)]
pub struct FollowRules {
    /// The layer's own crop (normalised source rect): the zoomed crop stays inside it,
    /// and zooming out returns to it.
    pub base: NRect,
    /// How many times smaller than `base` the zoomed crop is (1.2–4).
    pub max_zoom: f64,
    /// The middle share of the crop the cursor may move in before the crop follows.
    pub dead: f64,
    /// Samples without the cursor before zooming back out.
    pub hold: usize,
    /// Samples between two moves at least.
    pub gap: usize,
}

impl FollowRules {
    pub fn new(base: NRect, max_zoom: f64) -> Self {
        FollowRules {
            base,
            max_zoom: max_zoom.clamp(1.2, 4.0),
            dead: 0.6,
            hold: 6,
            gap: 2,
        }
    }
}

/// One move of the crop: from sample `sample` on, `crop` (`None`: back to the layer's
/// own crop).
#[derive(Clone, Debug, PartialEq)]
pub struct CropKey {
    pub sample: usize,
    pub crop: Option<NRect>,
}

/// The zoomed crop path following `points` (from [`track`], normalised source coords).
pub fn crop_path(points: &[Option<(f64, f64)>], r: &FollowRules) -> Vec<CropKey> {
    let b = r.base;
    let (zw, zh) = (b.w / r.max_zoom, b.h / r.max_zoom);
    let place = |cx: f64, cy: f64| NRect {
        x: (cx - zw / 2.0).clamp(b.x, b.x + b.w - zw),
        y: (cy - zh / 2.0).clamp(b.y, b.y + b.h - zh),
        w: zw,
        h: zh,
    };
    let mut keys: Vec<CropKey> = Vec::new();
    let mut cur: Option<NRect> = None;
    let mut last_seen: Option<usize> = None;
    let mut prev: Option<(f64, f64)> = None;
    for (i, p) in points.iter().enumerate() {
        let since_key = keys.last().map_or(usize::MAX, |k| i - k.sample);
        match p {
            &Some((x, y)) if inside(&b, x, y) => {
                // the mean of this and the last position (a steadier aim)
                let (cx, cy) = match prev {
                    Some((px, py))
                        if last_seen == Some(i.wrapping_sub(1))
                            && (x - px).abs() < zw / 2.0
                            && (y - py).abs() < zh / 2.0 =>
                    {
                        ((x + px) / 2.0, (y + py) / 2.0)
                    }
                    _ => (x, y),
                };
                last_seen = Some(i);
                prev = Some((x, y));
                let outside_dead = cur.is_none_or(|c| {
                    let (mx, my) = (c.w * (1.0 - r.dead) / 2.0, c.h * (1.0 - r.dead) / 2.0);
                    x < c.x + mx || x > c.x + c.w - mx || y < c.y + my || y > c.y + c.h - my
                });
                if outside_dead && (cur.is_none() || since_key >= r.gap) {
                    let next = place(cx, cy);
                    let moved = cur.is_none_or(|c| {
                        (c.x - next.x).abs() > 0.02 * b.w || (c.y - next.y).abs() > 0.02 * b.h
                    });
                    if moved {
                        keys.push(CropKey {
                            sample: i,
                            crop: Some(next),
                        });
                        cur = Some(next);
                    }
                }
            }
            _ => {
                if cur.is_some() && last_seen.is_none_or(|s| i - s >= r.hold) {
                    keys.push(CropKey {
                        sample: i,
                        crop: None,
                    });
                    cur = None;
                    prev = None;
                }
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1280;
    const H: u32 = 720;

    /// A desktop: light grey, rows of dark "text", a few darker panels.
    fn desk(seed: u32) -> Vec<u8> {
        let mut d = vec![228u8; (W * H) as usize];
        let mut s = seed.wrapping_mul(2_654_435_761).max(1);
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        for row in 0..40 {
            let y = 40 + row * 16;
            let mut x = 60;
            while x < W - 80 {
                let cw = 4 + rnd() % 6;
                if rnd() % 5 != 0 {
                    for yy in y..y + 9 {
                        for xx in x..x + cw {
                            d[(yy * W + xx) as usize] = 40;
                        }
                    }
                }
                x += cw + 2;
            }
        }
        d
    }

    /// Draws the arrow cursor (black, a white rim) of `size` px with its tip at (tx, ty).
    fn cursor(d: &mut [u8], tx: u32, ty: u32, size: u32) {
        let s = f64::from(size);
        for yy in 0..size + 3 {
            for xx in 0..size + 3 {
                let (u, v) = ((f64::from(xx) - 1.0) / s, (f64::from(yy) - 1.0) / s);
                let rim = (-1..=1).any(|dx| {
                    (-1..=1).any(|dy| {
                        in_arrow(u + f64::from(dx) / s * 1.5, v + f64::from(dy) / s * 1.5)
                    })
                });
                let (x, y) = (tx + xx, ty + yy);
                if x >= W || y >= H {
                    continue;
                }
                if in_arrow(u, v) {
                    d[(y * W + x) as usize] = 8;
                } else if rim {
                    d[(y * W + x) as usize] = 252;
                }
            }
        }
    }

    fn frame(d: Vec<u8>) -> LumaFrame {
        LumaFrame {
            width: W,
            height: H,
            data: d,
        }
    }

    const FULL: NRect = NRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    #[test]
    fn the_template_is_an_arrow() {
        assert!(in_arrow(0.05, 0.3));
        assert!(!in_arrow(0.6, 0.2));
        assert!(!in_arrow(0.05, 0.95));
        assert!(in_arrow(0.4, 0.85));
    }

    #[test]
    fn the_cursor_is_found_as_it_moves_and_typing_is_not_it() {
        // a path with long and short moves and a few still samples; each sample the
        // user also "types": a character appears on a line far from the cursor
        let path: [(u32, u32); 14] = [
            (300, 200),
            (420, 230),
            (420, 230),
            (700, 380),
            (712, 384),
            (900, 500),
            (900, 500),
            (900, 500),
            (500, 600),
            (140, 120),
            (160, 140),
            (1100, 640),
            (1000, 300),
            (640, 360),
        ];
        let base = desk(7);
        let mut frames = Vec::new();
        for (i, &(x, y)) in path.iter().enumerate() {
            let mut d = base.clone();
            // typed characters accumulate on the bottom line
            for k in 0..=i as u32 {
                for yy in 690..700 {
                    for xx in 100 + k * 9..106 + k * 9 {
                        d[(yy * W + xx) as usize] = 30;
                    }
                }
            }
            cursor(&mut d, x, y, 20);
            frames.push(frame(d));
        }
        let t = track(&frames, &FULL, &[]);
        let mut hit = 0;
        for (i, p) in t.iter().enumerate().skip(1) {
            let (x, y) = path[i];
            // the arrow's box centre (tip + about half its size)
            let (cx, cy) = (
                (f64::from(x) + 8.0) / f64::from(W),
                (f64::from(y) + 11.0) / f64::from(H),
            );
            match p {
                Some((px, py)) => {
                    let err = ((px - cx) * f64::from(W)).hypot((py - cy) * f64::from(H));
                    assert!(err < 20.0, "sample {i}: off by {err:.1} px ({px}, {py})");
                    hit += 1;
                }
                None => panic!("sample {i}: cursor not found"),
            }
        }
        assert_eq!(hit, path.len() - 1);
    }

    #[test]
    fn a_white_arrow_on_a_dark_screen_is_found_too() {
        // a dark theme: the arrow is white with a black rim
        let dark: Vec<u8> = desk(3).iter().map(|v| 255 - v).collect();
        let mut fr = Vec::new();
        for &(x, y) in &[(200u32, 200u32), (800, 420), (300, 600)] {
            let mut d = dark.clone();
            cursor(&mut d, x, y, 24);
            for v in d.iter_mut() {
                if *v == 8 {
                    *v = 250;
                } else if *v == 252 {
                    *v = 5;
                }
            }
            fr.push(frame(d));
        }
        let t = track(&fr, &FULL, &[]);
        let (x, y) = t[2].expect("found");
        assert!(
            (x * f64::from(W) - 308.0).abs() < 12.0 && (y * f64::from(H) - 613.0).abs() < 12.0,
            "{x} {y}"
        );
    }

    #[test]
    fn a_scroll_is_unknown_and_an_excluded_area_is_ignored() {
        let a = desk(1);
        let mut b = desk(2); // everything changed
        cursor(&mut b, 600, 300, 20);
        let t = track(&[frame(a.clone()), frame(b)], &FULL, &[]);
        assert_eq!(t, vec![None, None]);
        // a moving "cursor" inside the excluded camera box is not followed
        let (mut c0, mut c1) = (a.clone(), a);
        cursor(&mut c0, 1100, 600, 20);
        cursor(&mut c1, 1180, 650, 20);
        let cam = NRect {
            x: 0.8,
            y: 0.75,
            w: 0.2,
            h: 0.25,
        };
        let t = track(&[frame(c0), frame(c1)], &FULL, &[cam]);
        assert_eq!(t[1], None);
    }

    #[test]
    fn the_crop_follows_with_a_dead_zone_and_zooms_out_when_the_cursor_is_gone() {
        let base = NRect {
            x: 0.1,
            y: 0.1,
            w: 0.8,
            h: 0.8,
        };
        let r = FollowRules::new(base, 2.0);
        let mut pts = vec![
            None,
            Some((0.3, 0.3)),
            Some((0.32, 0.31)),
            Some((0.33, 0.3)),
        ];
        // a jump far right: the crop moves (one key), clamped inside the base
        pts.extend([Some((0.88, 0.85)), Some((0.87, 0.86))]);
        // gone for a while: zoom out
        pts.extend([None; 7]);
        let keys = crop_path(&pts, &r);
        assert_eq!(keys.len(), 3, "{keys:?}");
        assert_eq!(keys[0].sample, 1);
        let k0 = keys[0].crop.unwrap();
        assert!((k0.w - 0.4).abs() < 1e-9 && (k0.h - 0.4).abs() < 1e-9);
        assert!(
            (k0.x - 0.1).abs() < 1e-9,
            "clamped to the base's left: {k0:?}"
        );
        assert_eq!(keys[1].sample, 4);
        let k1 = keys[1].crop.unwrap();
        assert!((k1.x + k1.w - 0.9).abs() < 1e-9 && (k1.y + k1.h - 0.9).abs() < 1e-9);
        assert_eq!(
            keys[2],
            CropKey {
                sample: 11,
                crop: None
            }
        );
        // small moves inside the dead zone add no keys
        let still: Vec<_> = (0..10)
            .map(|k| Some((0.5 + 0.01 * f64::from(k % 3), 0.5)))
            .collect();
        assert_eq!(crop_path(&still, &r).len(), 1);
    }
}

/// Lab: `CURSOR_LAB=<raw grey frames> CURSOR_LAB_W=1920 CURSOR_LAB_H=1080 cargo test -p
/// gmclip-core --lib cursor::lab -- --ignored --nocapture` prints where the cursor was
/// found in each sample (frames from any decoder at 2 fps).
#[cfg(test)]
mod lab {
    #[test]
    #[ignore = "needs CURSOR_LAB frames"]
    fn track_a_recording() {
        let Some(path) = std::env::var_os("CURSOR_LAB") else {
            return;
        };
        let w: u32 = std::env::var("CURSOR_LAB_W").unwrap().parse().unwrap();
        let h: u32 = std::env::var("CURSOR_LAB_H").unwrap().parse().unwrap();
        let raw = std::fs::read(path).unwrap();
        let frames: Vec<super::LumaFrame> = raw
            .chunks_exact((w * h) as usize)
            .map(|c| super::LumaFrame {
                width: w,
                height: h,
                data: c.to_vec(),
            })
            .collect();
        let full = super::NRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        if let Ok(at) = std::env::var("CURSOR_LAB_AT") {
            // "sample,x0,y0,x1,y1": the best placement per size in that window, and the
            // pixels around it
            let v: Vec<u32> = at.split(',').map(|x| x.parse().unwrap()).collect();
            let f = &frames[v[0] as usize];
            for k in super::SIZES {
                let t = super::Template::new(((k * f64::from(h)).round() as u32).max(6));
                let mut best = (0.0, 0, 0);
                for y in v[2]..v[4] {
                    for x in v[1]..v[3] {
                        let s = t.score(f, x, y);
                        if s > best.0 {
                            best = (s, x, y);
                        }
                    }
                }
                println!("h {:3}: {:.2} at {},{}", t.h, best.0, best.1, best.2);
            }
            for y in v[2]..v[4] {
                let row: String = (v[1]..v[3])
                    .map(|x| {
                        let p = f.data[(y * w + x) as usize];
                        if p < 40 {
                            ' '
                        } else if p < 80 {
                            '.'
                        } else if p < 130 {
                            ':'
                        } else if p < 190 {
                            '+'
                        } else {
                            '#'
                        }
                    })
                    .collect();
                println!("{row}");
            }
            return;
        }
        let t0 = std::time::Instant::now();
        let t = super::track(&frames, &full, &[]);
        let ms = t0.elapsed().as_millis();
        for (i, p) in t.iter().enumerate() {
            if let Some((x, y)) = p {
                println!(
                    "{:5.1} s  {:6.0} {:6.0}",
                    i as f64 / 2.0,
                    x * f64::from(w),
                    y * f64::from(h)
                );
            }
        }
        let found = t.iter().filter(|p| p.is_some()).count();
        println!("found {found}/{} in {ms} ms", t.len());
    }
}
