//! Text animation (`/1`, decision 0014; SPEC §5.10 caption animation): what an animated
//! text event looks like on each of its frames. Pure arithmetic on integers and f64, so
//! the export and the preview (one compose, R-COMP-1) and every machine draw the same.
//!
//! Frame counts scale with the output rate: a step of `s` seconds is
//! `max(1, round(s · fps))` frames (2 frames at 30 fps for 1/15 s).
//!
//! | kind | in | out |
//! |---|---|---|
//! | `bounce` | scale 72 % → 118 % over 1/15 s (alpha ⅓ → 1), → 100 % over the next 1/15 s | last 1/15 s: → 76 %, alpha → ⅓ (only when the event outlasts in + out) |
//! | `pop` | 85 % → 108 % → 100 % (1/15 s each), no fade | none |
//! | `fade` | alpha over 0.1 s | alpha over 0.1 s |
//!
//! `pop_runs` (keyword animation): the event pulses to 112 % and back over 0.2 s, and the
//! listed runs' fill goes from the style's fill to their own over the first 0.1 s.
//!
//! Scaling keeps the block's centre: `pos` moves by `(s − 1) · size · lines / 2` away from
//! the edge `align` pins (none for a vertically centred `align`), and every px quantity of
//! the style (outline, shadow, box padding and radius, letter spacing) scales with `size`.

use crate::scalars::Rgba;
use crate::types::{AnimKind, TextAnim, TextEvent, TextStyle};

/// The look of an animated event on one frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AnimState {
    /// Size multiplier.
    pub scale: f64,
    /// Opacity multiplier in [0, 1].
    pub alpha: f64,
    /// How far `pop_runs` have come from the style's fill to their own, in [0, 1].
    pub pop: f64,
}

impl AnimState {
    pub const REST: AnimState = AnimState {
        scale: 1.0,
        alpha: 1.0,
        pop: 1.0,
    };

    /// Draws exactly as the event without animation.
    pub fn is_rest(&self) -> bool {
        *self == Self::REST
    }
}

/// `s` seconds as a frame count at `fps` (at least one frame).
pub fn frames(s: f64, fps: u64) -> u64 {
    ((s * fps as f64).round() as u64).max(1)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The state on frame `k` of an event `len` frames long (`k < len`).
pub fn state(anim: &TextAnim, k: u64, len: u64, fps: u64) -> AnimState {
    let mut st = AnimState::REST;
    let step = frames(1.0 / 15.0, fps);
    let tenth = frames(0.1, fps);
    let j = len.saturating_sub(1).saturating_sub(k); // frames left after this one
    match anim.kind {
        Some(AnimKind::Bounce) => {
            let (a, b, o) = (step, 2 * step, step);
            if k < a {
                st.scale = lerp(0.72, 1.18, k as f64 / a as f64);
                st.alpha = (k + 1) as f64 / (a + 1) as f64;
            } else if k < b {
                st.scale = lerp(1.18, 1.0, (k - a) as f64 / (b - a) as f64);
            }
            if len > b + o && j < o {
                st.scale *= lerp(0.76, 1.0, j as f64 / o as f64);
                st.alpha *= (j + 1) as f64 / (o + 1) as f64;
            }
        }
        Some(AnimKind::Pop) => {
            let (a, b) = (step, 2 * step);
            if k < a {
                st.scale = lerp(0.85, 1.08, k as f64 / a as f64);
            } else if k < b {
                st.scale = lerp(1.08, 1.0, (k - a) as f64 / (b - a) as f64);
            }
        }
        Some(AnimKind::Fade) => {
            if k < tenth {
                st.alpha = (k + 1) as f64 / (tenth + 1) as f64;
            }
            if j < tenth {
                st.alpha *= (j + 1) as f64 / (tenth + 1) as f64;
            }
        }
        None => {}
    }
    if anim.pop_runs.as_ref().is_some_and(|r| !r.is_empty()) {
        let p = tenth;
        if k < 2 * p {
            let t = 1.0 - (k as f64 - p as f64).abs() / p as f64;
            st.scale *= 1.0 + 0.12 * t;
        }
        st.pop = ((k + 1) as f64 / p as f64).min(1.0);
    }
    // a value that is 1 to within float noise is 1 (so a settled frame reuses its tile)
    for v in [&mut st.scale, &mut st.alpha, &mut st.pop] {
        if (*v - 1.0).abs() < 1e-9 {
            *v = 1.0;
        }
    }
    st
}

fn rgba(c: &Rgba) -> [u8; 4] {
    let h = c.as_str().trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(255);
    [b(0), b(2), b(4), b(6)]
}

fn to_rgba(c: [u8; 4]) -> Rgba {
    Rgba::new(format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3])).expect("#RRGGBBAA")
}

/// The event and style to rasterise for `st` (the opacity is applied to the raster).
pub fn apply(ev: &TextEvent, style: &TextStyle, st: AnimState) -> (TextEvent, TextStyle) {
    let mut e = ev.clone();
    let mut s = style.clone();
    e.anim = None;
    if st.scale != 1.0 {
        let k = st.scale;
        let lines = ev
            .runs
            .iter()
            .map(|r| r.text.matches('\n').count())
            .sum::<usize>()
            + 1;
        let shift = (k - 1.0) * ev.size * lines as f64 / 2.0;
        match ev.align.or(style.align).unwrap_or(2) {
            1..=3 => e.pos.y += shift,
            7..=9 => e.pos.y -= shift,
            _ => {}
        }
        e.size = ev.size * k;
        if let Some(o) = s.outline.as_mut() {
            o.width = o.width.map(|w| w * k);
        }
        if let Some(sh) = s.shadow.as_mut() {
            sh.dx = sh.dx.map(|v| v * k);
            sh.dy = sh.dy.map(|v| v * k);
            sh.blur = sh.blur.map(|v| v * k);
        }
        if let Some(b) = s.text_box.as_mut() {
            b.pad_x = b.pad_x.map(|v| v * k);
            b.pad_y = b.pad_y.map(|v| v * k);
            b.radius = b.radius.map(|v| v * k);
        }
        s.letter_spacing = s.letter_spacing.map(|v| v * k);
    }
    if st.pop < 1.0 {
        let from = style.fill.as_ref().map_or([255; 4], rgba);
        for i in ev
            .anim
            .as_ref()
            .and_then(|a| a.pop_runs.as_ref())
            .into_iter()
            .flatten()
        {
            let Some(r) = usize::try_from(*i).ok().and_then(|i| e.runs.get_mut(i)) else {
                continue;
            };
            let to = r.fill.as_ref().map_or(from, rgba);
            let c: [u8; 4] = std::array::from_fn(|n| {
                lerp(f64::from(from[n]), f64::from(to[n]), st.pop).round() as u8
            });
            r.fill = Some(to_rgba(c));
        }
    }
    (e, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(kind: Option<AnimKind>, pop: bool) -> TextAnim {
        TextAnim {
            kind,
            pop_runs: pop.then(|| vec![0]),
        }
    }

    #[test]
    fn bounce_hits_the_spec_values_at_30_fps() {
        let a = anim(Some(AnimKind::Bounce), false);
        let s: Vec<f64> = (0..10).map(|k| state(&a, k, 10, 30).scale).collect();
        assert!((s[0] - 0.72).abs() < 1e-12);
        assert!((s[2] - 1.18).abs() < 1e-12);
        assert_eq!(s[4], 1.0);
        assert_eq!(s[7], 1.0);
        assert!((s[9] - 0.76).abs() < 1e-12, "{s:?}");
        let al: Vec<f64> = (0..10).map(|k| state(&a, k, 10, 30).alpha).collect();
        assert!(al[0] > 0.0 && al[0] < al[1] && al[2] == 1.0 && al[9] < 1.0);
        // a cue too short for in + out has no out
        assert_eq!(state(&a, 4, 5, 30).scale, 1.0);
        // at 60 fps the steps double
        assert!((state(&a, 4, 30, 60).scale - 1.18).abs() < 1e-12);
    }

    #[test]
    fn keyword_pop_pulses_and_colours() {
        let a = anim(None, true);
        let peak = state(&a, 3, 30, 30);
        assert!((peak.scale - 1.12).abs() < 1e-12 && peak.pop == 1.0);
        assert!(state(&a, 0, 30, 30).pop < 1.0);
        assert!(state(&a, 6, 30, 30).is_rest());
        assert!(state(&anim(None, false), 0, 30, 30).is_rest());
    }
}
