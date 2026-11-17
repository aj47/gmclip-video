//! A recording's input track (decision 0045, SPEC §5.18): where the cursor was, what it
//! looked like, what was clicked, scrolled and typed into, on one screen file's picture
//! and clock.
//!
//! gmclip's recorder hides the cursor in the screen file and logs it instead; the import
//! turns the session's `events.ndjson` (global points, host time) into one
//! [`InputTrack`] per screen file (that file's pixels, µs from its first frame) and
//! stores it as an asset. The compositor redraws the cursor from it, and cursor follow
//! and zoom to focus read it instead of looking at the pixels. Pure data and pure
//! queries; parsing and storing are the callers'.

use serde::{Deserialize, Serialize};

/// The [`InputTrack`] schema id.
pub const INPUT_TRACK_SCHEMA: &str = "gmclip.input/1";
/// The media type an input track is stored under.
pub const INPUT_TRACK_MEDIA_TYPE: &str = "application/vnd.gmclip.input+json";
/// A cursor sample index meaning "shape unknown" (draw the last known one).
pub const SHAPE_UNKNOWN: u16 = u16::MAX;
/// Two cursor samples further apart than this are not interpolated between.
pub const MAX_GAP_US: i64 = 250_000;

/// `[t_us, x, y, shape]`: the cursor's hotspot in picture pixels at file time `t_us`.
/// `shape` indexes [`InputTrack::shapes`] ([`SHAPE_UNKNOWN`] = as before). A sample off
/// the picture (another display) has x or y outside `[0, w)` / `[0, h)`.
pub type CursorSample = (i64, f32, f32, u16);

/// One recorded input track.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct InputTrack {
    pub schema: String,
    /// The picture the coordinates are in (the screen file's pixels).
    pub w: u32,
    pub h: u32,
    /// Picture pixels per point (the display's backing scale): a cursor image `n` points
    /// wide covers `n × scale` pixels.
    pub scale: f32,
    /// Every screen frame's cursor and every pointer event, in time order.
    pub cursor: Vec<CursorSample>,
    /// Cursor images, by the sha256 of their PNG (stored next to the track as assets).
    #[serde(default)]
    pub shapes: Vec<ShapeInfo>,
    #[serde(default)]
    pub clicks: Vec<Click>,
    #[serde(default)]
    pub scrolls: Vec<Scroll>,
    /// The focused element on each change (`rect: None` = nothing focused, or a field
    /// on another display).
    #[serde(default)]
    pub focus: Vec<Focus>,
    /// Key-downs per burst (counts only, never characters; none in secure fields).
    #[serde(default)]
    pub keys: Vec<Keys>,
    /// The frontmost app on each change.
    #[serde(default)]
    pub apps: Vec<App>,
    /// The marker hotkey, at these times.
    #[serde(default)]
    pub markers: Vec<i64>,
}

/// A cursor image: its PNG's sha256, its pixel size, its hotspot in those pixels, and
/// its size in points (so it can be drawn at the picture's scale).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ShapeInfo {
    pub sha256: String,
    pub w: u32,
    pub h: u32,
    pub hot_x: f32,
    pub hot_y: f32,
    pub pt_w: f32,
    pub pt_h: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Click {
    pub t: i64,
    pub x: f32,
    pub y: f32,
    /// 0 left, 1 right, 2+ other.
    pub button: u8,
    pub down: bool,
    /// 1 single, 2 double, …
    pub count: u8,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Scroll {
    pub t: i64,
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Focus {
    pub t: i64,
    /// The element's frame `[x, y, w, h]` in picture pixels.
    pub rect: Option<[f32; 4]>,
    /// The text caret (or selection) `[x, y, w, h]`, when the element reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret: Option<[f32; 4]>,
    /// Its window's frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[f32; 4]>,
    /// `AXTextField`, `AXTextArea`, `AXButton`, …
    #[serde(default)]
    pub role: String,
    /// The app's bundle id.
    #[serde(default)]
    pub app: String,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Keys {
    pub t: i64,
    pub n: u32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct App {
    pub t: i64,
    pub app: String,
    pub name: String,
}

/// Where the cursor is at one time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CursorAt {
    pub x: f32,
    pub y: f32,
    /// Index into [`InputTrack::shapes`], if any shape is known by then.
    pub shape: Option<usize>,
    /// µs since the cursor last moved (0 while moving).
    pub idle_us: i64,
}

impl InputTrack {
    /// An empty track for a `w × h` picture.
    pub fn new(w: u32, h: u32, scale: f32) -> Self {
        Self {
            schema: INPUT_TRACK_SCHEMA.into(),
            w,
            h,
            scale,
            ..Self::default()
        }
    }

    fn on_picture(&self, x: f32, y: f32) -> bool {
        x >= 0.0 && y >= 0.0 && x < self.w as f32 && y < self.h as f32
    }

    /// Index of the last cursor sample at or before `t`.
    fn sample_before(&self, t: i64) -> Option<usize> {
        let i = self.cursor.partition_point(|s| s.0 <= t);
        i.checked_sub(1)
    }

    /// The shape in force at sample `i` (the last known one at or before it).
    fn shape_at(&self, i: usize) -> Option<usize> {
        self.cursor[..=i]
            .iter()
            .rev()
            .find(|s| s.3 != SHAPE_UNKNOWN)
            .map(|s| s.3 as usize)
            .filter(|&s| s < self.shapes.len())
    }

    /// The cursor at file time `t` (µs): interpolated between the samples around it,
    /// none before the first sample, after a gap longer than [`MAX_GAP_US`], or while
    /// it is off this picture.
    pub fn cursor_at(&self, t: i64) -> Option<CursorAt> {
        let i = self.sample_before(t)?;
        let a = self.cursor[i];
        let (x, y) = match self.cursor.get(i + 1) {
            Some(b) if b.0 - a.0 <= MAX_GAP_US && b.0 > a.0 => {
                let f = (t - a.0) as f32 / (b.0 - a.0) as f32;
                (a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
            }
            Some(_) => (a.1, a.2),
            None if t - a.0 <= MAX_GAP_US => (a.1, a.2),
            None => return None,
        };
        if !self.on_picture(x, y) {
            return None;
        }
        Some(CursorAt {
            x,
            y,
            shape: self.shape_at(i),
            idle_us: t - self.last_move_at(i),
        })
    }

    /// The cursor averaged over `window_us` before and after `t` (a light smoothing
    /// that keeps clicks where they were: samples within 60 ms of a click are not
    /// smoothed). `window_us = 0` is [`Self::cursor_at`].
    pub fn smoothed_at(&self, t: i64, window_us: i64) -> Option<CursorAt> {
        let at = self.cursor_at(t)?;
        if window_us <= 0 || self.clicks.iter().any(|c| (c.t - t).abs() <= 60_000) {
            return Some(at);
        }
        let lo = self.cursor.partition_point(|s| s.0 < t - window_us);
        let hi = self.cursor.partition_point(|s| s.0 <= t + window_us);
        let mut sum = (0.0f64, 0.0f64, 0.0f64);
        for s in &self.cursor[lo..hi] {
            if !self.on_picture(s.1, s.2) {
                continue;
            }
            // Triangle weights: the samples nearest `t` count most.
            let w = 1.0 - ((s.0 - t).abs() as f64 / (window_us as f64 + 1.0));
            sum.0 += f64::from(s.1) * w;
            sum.1 += f64::from(s.2) * w;
            sum.2 += w;
        }
        if sum.2 <= 0.0 {
            return Some(at);
        }
        Some(CursorAt {
            x: (sum.0 / sum.2) as f32,
            y: (sum.1 / sum.2) as f32,
            ..at
        })
    }

    /// File time of the last sample at or before sample `i` that moved.
    fn last_move_at(&self, i: usize) -> i64 {
        let mut j = i;
        while j > 0 {
            let (a, b) = (self.cursor[j - 1], self.cursor[j]);
            if (a.1 - b.1).abs() > 0.5 || (a.2 - b.2).abs() > 0.5 {
                return b.0;
            }
            j -= 1;
        }
        self.cursor.first().map_or(0, |s| s.0)
    }

    /// The focused element at `t`.
    pub fn focus_at(&self, t: i64) -> Option<&Focus> {
        let i = self.focus.partition_point(|f| f.t <= t);
        self.focus[..i].last()
    }

    /// Clicks (button down) in `[from, to)`.
    pub fn clicks_in(&self, from: i64, to: i64) -> impl Iterator<Item = &Click> {
        self.clicks
            .iter()
            .filter(move |c| c.down && c.t >= from && c.t < to)
    }

    /// Key-downs in `[from, to)`.
    pub fn keys_in(&self, from: i64, to: i64) -> u32 {
        self.keys
            .iter()
            .filter(|k| k.t >= from && k.t < to)
            .map(|k| k.n)
            .sum()
    }

    /// The last click at or before `t` and how long ago it was (µs).
    pub fn last_click(&self, t: i64) -> Option<(&Click, i64)> {
        self.clicks
            .iter()
            .rev()
            .find(|c| c.down && c.t <= t)
            .map(|c| (c, t - c.t))
    }

    /// Checks the track is well formed: the schema, a picture, times in order.
    pub fn check(&self) -> Result<(), String> {
        if self.schema != INPUT_TRACK_SCHEMA {
            return Err(format!(
                "schema is {:?}, not {INPUT_TRACK_SCHEMA}",
                self.schema
            ));
        }
        // `scale > 0` written so a NaN scale is refused too
        if self.w == 0 || self.h == 0 || self.scale.is_nan() || self.scale <= 0.0 {
            return Err("the picture has no size".into());
        }
        if self.cursor.windows(2).any(|w| w[1].0 < w[0].0) {
            return Err("cursor samples are not in time order".into());
        }
        if self.focus.windows(2).any(|w| w[1].t < w[0].t) {
            return Err("focus changes are not in time order".into());
        }
        if let Some(s) = self
            .cursor
            .iter()
            .find(|s| s.3 != SHAPE_UNKNOWN && usize::from(s.3) >= self.shapes.len())
        {
            return Err(format!(
                "a cursor sample at {} µs names shape {} of {}",
                s.0,
                s.3,
                self.shapes.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> InputTrack {
        let mut t = InputTrack::new(1920, 1080, 2.0);
        t.shapes.push(ShapeInfo {
            sha256: "a".into(),
            w: 32,
            h: 32,
            hot_x: 4.0,
            hot_y: 4.0,
            pt_w: 16.0,
            pt_h: 16.0,
        });
        t.cursor = vec![
            (0, 100.0, 100.0, 0),
            (16_666, 110.0, 100.0, SHAPE_UNKNOWN),
            (33_333, 110.0, 100.0, SHAPE_UNKNOWN),
            (1_000_000, 500.0, 500.0, SHAPE_UNKNOWN),
            (1_016_666, -10.0, 500.0, SHAPE_UNKNOWN),
        ];
        t
    }

    #[test]
    fn interpolates_between_samples() {
        let t = track();
        let c = t.cursor_at(8_333).unwrap();
        assert!((c.x - 105.0).abs() < 0.1, "{c:?}");
        assert_eq!(c.shape, Some(0));
    }

    #[test]
    fn a_long_gap_holds_the_last_sample() {
        let c = track().cursor_at(500_000).unwrap();
        assert_eq!((c.x, c.y), (110.0, 100.0));
        assert!(c.idle_us > 400_000, "{c:?}");
    }

    #[test]
    fn off_the_picture_and_before_the_first_sample_is_none() {
        let t = track();
        assert!(t.cursor_at(-1).is_none());
        assert!(t.cursor_at(1_016_666).is_none());
        assert!(t.cursor_at(5_000_000).is_none());
    }

    #[test]
    fn check_refuses_out_of_order_and_unknown_shapes() {
        let mut t = track();
        assert!(t.check().is_ok());
        t.cursor.push((5, 1.0, 1.0, 3));
        assert!(t.check().is_err());
    }

    #[test]
    fn focus_and_clicks() {
        let mut t = track();
        t.focus.push(Focus {
            t: 10,
            rect: Some([0.0, 0.0, 10.0, 10.0]),
            caret: None,
            window: None,
            role: "AXTextField".into(),
            app: "x".into(),
        });
        t.clicks.push(Click {
            t: 20,
            x: 1.0,
            y: 1.0,
            button: 0,
            down: true,
            count: 1,
        });
        assert!(t.focus_at(5).is_none());
        assert_eq!(t.focus_at(11).unwrap().role, "AXTextField");
        assert_eq!(t.clicks_in(0, 30).count(), 1);
        assert_eq!(t.last_click(25).unwrap().1, 5);
    }
}
