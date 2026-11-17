//! The activity crop (SPEC §5.7): where a screen changes, from frame differences on the
//! scrub proxy at 2 fps.
//!
//! Per part: a heatmap of changed pixels (|Δluma| ≥ [`DIFF_THRESHOLD`] between
//! consecutive samples, binned into cells) inside the `screen` region; the smallest box
//! of the layer's aspect that holds [`MASS`] of the change, no wider than the region
//! zoomed [`MIN_ZOOM`]× (the "minimum scale 1.6× the full-width scale") and no narrower
//! than [`MAX_UPSCALE`] allows; when the change's centre moves more than [`MOVE_FRAC`] of
//! the frame width across the part, a split where it moves (snapped to a word gap) and
//! a crop per side. Pure and deterministic: the host supplies the luma frames.

use serde::Serialize;

/// A grey picture: rows of `width` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LumaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A luma step that counts as change (8-bit codes; proxy noise stays below it).
pub const DIFF_THRESHOLD: u8 = 12;
/// The share of the change the box must hold.
pub const MASS: f64 = 0.8;
/// The crop is at least this many times the full-width scale (zoomed in ≥ 1.6×).
pub const MIN_ZOOM: f64 = 1.6;
/// The crop is never shown larger than this many canvas px per source px.
pub const MAX_UPSCALE: f64 = 2.0;
/// A move of the change's centre across this share of the frame width splits a part.
pub const MOVE_FRAC: f64 = 0.30;
/// Below this effective scale (canvas px per source px) screen text is likely too small.
pub const SMALL_TEXT_SCALE: f64 = 0.6;
/// Below this share of the region's pixels changing per step, the screen is still and
/// the crop is kept.
pub const STILL_FRAC: f64 = 0.000_02;
/// Excluded areas grow by this share of the frame each side (a face moves past its box).
pub const EXCLUDE_PAD: f64 = 0.03;
/// Heatmap cell size in proxy pixels.
const CELL: u32 = 4;
/// Each side of a split keeps at least this many samples (2 s at 2 fps).
const MIN_SIDE: usize = 4;

/// A rectangle in normalised source coordinates.
#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
pub struct NRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What the crop must fit: the region it may look in, its aspect and the panel size.
#[derive(Clone, Debug, PartialEq)]
pub struct CropRules {
    /// The `screen` region (normalised; the whole frame when there is none).
    pub region: NRect,
    /// Areas whose change never counts: the camera inset, chat, alerts (other layers'
    /// crops and the source's other regions). A moving face would otherwise win.
    pub exclude: Vec<NRect>,
    /// Crop aspect in source pixels (w / h): the layer's panel aspect.
    pub aspect: f64,
    /// The source picture size in pixels.
    pub source: (u32, u32),
    /// The panel's width on the canvas in pixels.
    pub panel_w: f64,
}

/// One crop of a part: from `from_sample` (0 = the part's start) on.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActivityCrop {
    pub from_sample: usize,
    pub crop: NRect,
    /// Share of the change inside the crop.
    pub held: f64,
    /// Canvas px per source px the crop is shown at.
    pub scale: f64,
}

/// A part's activity result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Activity {
    /// Empty when the screen is still (the crop is kept).
    pub crops: Vec<ActivityCrop>,
    /// The change's centre moved by this share of the frame width (the split, if any).
    pub moved: f64,
    /// Total changed cells (0: a still screen; the crop is then centred).
    pub change: u64,
    /// Lint: the crop shows screen text below [`SMALL_TEXT_SCALE`].
    pub text_too_small: bool,
}

/// Changed-cell counts of one step between two samples, `cw × ch` cells over the frame.
fn step_map(a: &LumaFrame, b: &LumaFrame, cw: usize, ch: usize) -> Vec<u32> {
    let mut m = vec![0u32; cw * ch];
    let w = a.width as usize;
    for y in 0..a.height as usize {
        let cy = (y / CELL as usize).min(ch - 1);
        let ra = &a.data[y * w..(y + 1) * w];
        let rb = &b.data[y * w..(y + 1) * w];
        for x in 0..w {
            if ra[x].abs_diff(rb[x]) >= DIFF_THRESHOLD {
                m[cy * cw + (x / CELL as usize).min(cw - 1)] += 1;
            }
        }
    }
    m
}

/// Summed-area table of `m` (`(cw + 1) × (ch + 1)`).
fn integral(m: &[u64], cw: usize, ch: usize) -> Vec<u64> {
    let mut s = vec![0u64; (cw + 1) * (ch + 1)];
    for y in 0..ch {
        let mut row = 0u64;
        for x in 0..cw {
            row += m[y * cw + x];
            s[(y + 1) * (cw + 1) + x + 1] = s[y * (cw + 1) + x + 1] + row;
        }
    }
    s
}

fn box_sum(s: &[u64], cw: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> u64 {
    let w = cw + 1;
    s[y1 * w + x1] + s[y0 * w + x0] - s[y0 * w + x1] - s[y1 * w + x0]
}

/// The smallest box (cells) of the rules' aspect holding [`MASS`] of `heat`, within the
/// region and the zoom limits; ties go to the box holding the most, then top-left.
fn best_box(heat: &[u64], cw: usize, ch: usize, rules: &CropRules) -> (NRect, f64) {
    let total: u64 = heat.iter().sum();
    let (sw, sh) = (f64::from(rules.source.0), f64::from(rules.source.1));
    let r = rules.region;
    // widths in source px: at most the region zoomed MIN_ZOOM×, at least the panel /
    // MAX_UPSCALE, and never taller than the region
    let region_w = r.w * sw;
    let max_w = (region_w / MIN_ZOOM).min(r.h * sh * rules.aspect);
    let min_w = (rules.panel_w / MAX_UPSCALE).min(max_w);
    // cell geometry in normalised units
    let (cx, cy) = (1.0 / cw as f64, 1.0 / ch as f64);
    let rx0 = (r.x / cx).floor() as usize;
    let ry0 = (r.y / cy).floor() as usize;
    let rx1 = (((r.x + r.w) / cx).ceil() as usize).min(cw);
    let ry1 = (((r.y + r.h) / cy).ceil() as usize).min(ch);
    let s = integral(heat, cw, ch);
    let centre = |w_px: f64| -> NRect {
        let (w, h) = (w_px / sw, w_px / rules.aspect / sh);
        NRect {
            x: r.x + (r.w - w) / 2.0,
            y: r.y + (r.h - h) / 2.0,
            w,
            h,
        }
    };
    if total == 0 {
        return (centre(max_w), 0.0);
    }
    let steps = 24;
    let mut best: Option<(NRect, f64)> = None;
    for i in 0..=steps {
        let w_px = min_w + (max_w - min_w) * f64::from(i) / f64::from(steps);
        let (wn, hn) = (w_px / sw, w_px / rules.aspect / sh);
        // the cell box fits inside the exact box, so what it holds the crop holds
        let bw = ((wn / cx).floor() as usize).clamp(1, rx1 - rx0);
        let bh = ((hn / cy).floor() as usize).clamp(1, ry1 - ry0);
        let mut top: Option<(u64, usize, usize)> = None;
        for y0 in ry0..=ry1 - bh {
            for x0 in rx0..=rx1 - bw {
                let v = box_sum(&s, cw, x0, y0, x0 + bw, y0 + bh);
                if top.is_none_or(|t| v > t.0) {
                    top = Some((v, x0, y0));
                }
            }
        }
        let Some((v, x0, y0)) = top else { continue };
        let held = v as f64 / total as f64;
        // place the exact-size box centred on the winning cell box, inside the region
        let (bx, by) = (
            (x0 as f64 + bw as f64 / 2.0) * cx - wn / 2.0,
            (y0 as f64 + bh as f64 / 2.0) * cy - hn / 2.0,
        );
        let rect = NRect {
            x: bx.clamp(r.x, (r.x + r.w - wn).max(r.x)),
            y: by.clamp(r.y, (r.y + r.h - hn).max(r.y)),
            w: wn.min(r.w),
            h: hn.min(r.h),
        };
        if held >= MASS {
            return (rect, held);
        }
        if best.as_ref().is_none_or(|b| held > b.1 + 1e-9) {
            best = Some((rect, held));
        }
    }
    best.unwrap_or_else(|| (centre(max_w), 0.0))
}

/// Mass-weighted centre x (normalised) of a heatmap.
fn centre_x(heat: &[u64], cw: usize) -> Option<f64> {
    let total: u64 = heat.iter().sum();
    if total == 0 {
        return None;
    }
    let sx: f64 = heat
        .iter()
        .enumerate()
        .map(|(i, &v)| v as f64 * ((i % cw) as f64 + 0.5))
        .sum();
    Some(sx / total as f64 / cw as f64)
}

/// The activity crop(s) of one part from its samples (2 fps, consecutive, same size).
/// `word_gaps` are sample positions (fractional) where a split may land: the split
/// snaps to the nearest within 3 samples, else stays where the action moved.
pub fn analyse(frames: &[LumaFrame], rules: &CropRules, word_gaps: &[f64]) -> Option<Activity> {
    let f0 = frames.first()?;
    if frames.iter().any(|f| {
        (f.width, f.height) != (f0.width, f0.height)
            || f.data.len() != (f.width * f.height) as usize
    }) || f0.width < CELL
        || f0.height < CELL
    {
        return None;
    }
    let cw = f0.width.div_ceil(CELL) as usize;
    let ch = f0.height.div_ceil(CELL) as usize;
    // change inside the region only
    let inside = |i: usize| {
        let (x, y) = ((i % cw) as f64 + 0.5, (i / cw) as f64 + 0.5);
        let (nx, ny) = (x / cw as f64, y / ch as f64);
        let within = |r: &NRect, m: f64| {
            nx >= r.x - m && nx <= r.x + r.w + m && ny >= r.y - m && ny <= r.y + r.h + m
        };
        within(&rules.region, 0.0) && !rules.exclude.iter().any(|r| within(r, EXCLUDE_PAD))
    };
    let steps: Vec<Vec<u64>> = frames
        .windows(2)
        .map(|p| {
            step_map(&p[0], &p[1], cw, ch)
                .into_iter()
                .enumerate()
                .map(|(i, v)| if inside(i) { u64::from(v) } else { 0 })
                .collect()
        })
        .collect();
    let sum = |range: std::ops::Range<usize>| -> Vec<u64> {
        let mut m = vec![0u64; cw * ch];
        for s in &steps[range] {
            for (a, b) in m.iter_mut().zip(s) {
                *a += b;
            }
        }
        m
    };
    let all = sum(0..steps.len());
    let change: u64 = all.iter().sum();
    // a still screen (a cursor blink, compression noise): keep the crop as it is
    let px_steps = f64::from(f0.width) * f64::from(f0.height) * steps.len().max(1) as f64;
    if (change as f64) < STILL_FRAC * px_steps * rules.region.w * rules.region.h {
        return Some(Activity {
            crops: Vec::new(),
            moved: 0.0,
            change,
            text_too_small: false,
        });
    }
    // the split where the centre of the change moves most (each side ≥ MIN_SIDE steps)
    let mut split: Option<(usize, f64)> = None;
    if steps.len() >= 2 * MIN_SIDE {
        for k in MIN_SIDE..=steps.len() - MIN_SIDE {
            let (Some(a), Some(b)) = (centre_x(&sum(0..k), cw), centre_x(&sum(k..steps.len()), cw))
            else {
                continue;
            };
            let d = (b - a).abs();
            if split.is_none_or(|s| d > s.1 + 1e-12) {
                split = Some((k, d));
            }
        }
    }
    let moved = split.map_or(0.0, |s| s.1);
    let scale_of = |c: &NRect| rules.panel_w / (c.w * f64::from(rules.source.0));
    let mut crops = Vec::new();
    if let Some((k, d)) = split.filter(|s| s.1 > MOVE_FRAC) {
        // step k is between samples k and k+1; snap to a word gap nearby
        let at = word_gaps
            .iter()
            .copied()
            .filter(|g| (g - (k as f64 + 0.5)).abs() <= 3.0)
            .min_by(|a, b| {
                (a - (k as f64 + 0.5))
                    .abs()
                    .total_cmp(&(b - (k as f64 + 0.5)).abs())
            })
            .map_or(k + 1, |g| (g.round() as usize).clamp(1, frames.len() - 1));
        let (r1, h1) = best_box(&sum(0..at.min(steps.len())), cw, ch, rules);
        let (r2, h2) = best_box(&sum(at.min(steps.len())..steps.len()), cw, ch, rules);
        crops.push(ActivityCrop {
            from_sample: 0,
            scale: scale_of(&r1),
            crop: r1,
            held: h1,
        });
        crops.push(ActivityCrop {
            from_sample: at,
            scale: scale_of(&r2),
            crop: r2,
            held: h2,
        });
        let _ = d;
    } else {
        let (r, held) = best_box(&all, cw, ch, rules);
        crops.push(ActivityCrop {
            from_sample: 0,
            scale: scale_of(&r),
            crop: r,
            held,
        });
    }
    let text_too_small = crops.iter().any(|c| c.scale < SMALL_TEXT_SCALE);
    Some(Activity {
        crops,
        moved,
        change,
        text_too_small,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 320×180 grey frame with a bright square at `(x, y)` (a moving cursor / typing).
    fn frame(spots: &[(u32, u32)], t: u32) -> LumaFrame {
        let (w, h) = (320u32, 180u32);
        let mut data = vec![40u8; (w * h) as usize];
        for &(sx, sy) in spots {
            for y in sy..sy + 10 {
                for x in sx..sx + 10 {
                    // flickers frame to frame: change at the spot
                    data[(y * w + x) as usize] = if t.is_multiple_of(2) { 220 } else { 60 };
                }
            }
        }
        LumaFrame {
            width: w,
            height: h,
            data,
        }
    }

    fn rules() -> CropRules {
        CropRules {
            region: NRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            exclude: vec![],
            aspect: 1080.0 / 760.0,
            source: (2560, 1440),
            panel_w: 1080.0,
        }
    }

    #[test]
    fn crop_holds_the_change_and_obeys_the_zoom() {
        let frames: Vec<LumaFrame> = (0..20).map(|t| frame(&[(250, 30)], t)).collect();
        let a = analyse(&frames, &rules(), &[]).unwrap();
        assert_eq!(a.crops.len(), 1);
        let c = &a.crops[0];
        assert!(c.held >= MASS, "{c:?}");
        // the spot (250..260, 30..40 of 320×180) is inside the crop
        assert!(
            c.crop.x <= 250.0 / 320.0 && c.crop.x + c.crop.w >= 260.0 / 320.0,
            "{c:?}"
        );
        assert!(
            c.crop.y <= 30.0 / 180.0 && c.crop.y + c.crop.h >= 40.0 / 180.0,
            "{c:?}"
        );
        // zoom: at most region/1.6 wide, at most 2× upscale
        let w_px = c.crop.w * 2560.0;
        assert!(
            (1080.0 / MAX_UPSCALE - 1.0..=2560.0 / MIN_ZOOM + 1.0).contains(&w_px),
            "{w_px}"
        );
        // aspect kept
        assert!(((c.crop.w * 2560.0) / (c.crop.h * 1440.0) - 1080.0 / 760.0).abs() < 0.05);
        assert!(!a.text_too_small);
        // deterministic
        assert_eq!(analyse(&frames, &rules(), &[]), Some(a));
    }

    #[test]
    fn a_move_across_the_screen_splits_at_a_word_gap() {
        let frames: Vec<LumaFrame> = (0..24)
            .map(|t| {
                if t < 12 {
                    frame(&[(20, 80)], t)
                } else {
                    frame(&[(280, 80)], t)
                }
            })
            .collect();
        let a = analyse(&frames, &rules(), &[]).unwrap();
        assert!(a.moved > MOVE_FRAC, "{a:?}");
        assert_eq!(a.crops.len(), 2);
        assert!(a.crops[0].crop.x < a.crops[1].crop.x);
        assert!((11..=13).contains(&a.crops[1].from_sample), "{a:?}");
        // a word gap two samples later moves the split there
        let b = analyse(&frames, &rules(), &[13.6]).unwrap();
        assert_eq!(b.crops[1].from_sample, 14);
        // a gap too far away is ignored
        let c = analyse(&frames, &rules(), &[2.0]).unwrap();
        assert_eq!(c.crops[1].from_sample, a.crops[1].from_sample);
    }

    #[test]
    fn small_moves_do_not_split_and_still_screens_centre() {
        let frames: Vec<LumaFrame> = (0..24)
            .map(|t| {
                if t < 12 {
                    frame(&[(140, 80)], t)
                } else {
                    frame(&[(180, 80)], t)
                }
            })
            .collect();
        assert_eq!(analyse(&frames, &rules(), &[]).unwrap().crops.len(), 1);
        let still: Vec<LumaFrame> = (0..10).map(|_| frame(&[], 0)).collect();
        let a = analyse(&still, &rules(), &[]).unwrap();
        assert_eq!(a.change, 0);
        assert!(a.crops.is_empty(), "a still screen keeps its crop");
    }

    #[test]
    fn change_outside_the_region_is_ignored_and_tiny_panels_lint() {
        let mut r = rules();
        r.region = NRect {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let frames: Vec<LumaFrame> = (0..10).map(|t| frame(&[(40, 40), (290, 140)], t)).collect();
        let a = analyse(&frames, &r, &[]).unwrap();
        assert!(a.crops[0].crop.x + a.crops[0].crop.w <= 0.5 + 1e-9);
        assert!(a.crops[0].held >= MASS);
        // a narrow panel showing a wide region: scale under 0.6 → lint
        let mut small = rules();
        small.panel_w = 400.0;
        let frames: Vec<LumaFrame> = (0..10).map(|t| frame(&[(10, 80), (180, 80)], t)).collect();
        let b = analyse(&frames, &small, &[]).unwrap();
        assert!(b.text_too_small, "{b:?}");
        // an excluded inset (the camera) never draws the crop
        let mut ex = rules();
        ex.exclude = vec![NRect {
            x: 0.8,
            y: 0.0,
            w: 0.2,
            h: 0.5,
        }];
        let frames: Vec<LumaFrame> = (0..10).map(|t| frame(&[(40, 120), (290, 20)], t)).collect();
        let e = analyse(&frames, &ex, &[]).unwrap();
        assert!(e.crops[0].held >= MASS && e.crops[0].crop.x < 0.2, "{e:?}");
        assert!(analyse(&[], &rules(), &[]).is_none());
    }
}
