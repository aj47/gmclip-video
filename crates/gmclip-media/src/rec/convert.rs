//! A session's raw log → one [`InputTrack`] per screen file, in that file's pixels.
//!
//! A display recording maps a global point `g` to `(g − origin) × pixels/points`. A
//! window recording maps it through the window's frame at that time (its `window`
//! events), since the capture follows the window. Times stay µs from `t0`, which is the
//! file's own time 0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gmclip_model::rec::{
    App, Click, CursorSample, Focus, InputTrack, Keys, SHAPE_UNKNOWN, Scroll, ShapeInfo,
};

use super::{CURSORS_DIR, RawEvent, RecError, Session, read_events};

/// One screen file's input track and the cursor images it names, by shape index.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenTrack {
    pub file: PathBuf,
    pub track: InputTrack,
    pub shape_files: Vec<PathBuf>,
}

/// Every screen file's track.
pub fn tracks(dir: &Path) -> Result<Vec<ScreenTrack>, RecError> {
    let session = Session::read(dir)?;
    let events = read_events(dir)?;
    Ok((0..session.screens.len())
        .map(|i| track_for(dir, &session, i, &events))
        .collect())
}

/// Where screen `i`'s picture sits in global points at time `t`: origin and pixels
/// per point on each axis.
struct Mapper {
    origin: (f64, f64),
    k: (f64, f64),
    window: Vec<(i64, [f64; 4])>,
    size: (f64, f64),
}

impl Mapper {
    fn at(&self, t: i64) -> ((f64, f64), (f64, f64)) {
        if self.window.is_empty() {
            return (self.origin, self.k);
        }
        let i = self.window.partition_point(|(wt, _)| *wt <= t);
        let r = self.window[i.saturating_sub(1)].1;
        (
            (r[0], r[1]),
            (self.size.0 / r[2].max(1.0), self.size.1 / r[3].max(1.0)),
        )
    }

    fn point(&self, t: i64, x: f64, y: f64) -> (f32, f32) {
        let (o, k) = self.at(t);
        (((x - o.0) * k.0) as f32, ((y - o.1) * k.1) as f32)
    }

    /// A rect in picture pixels, or None when it does not touch the picture.
    fn rect(&self, t: i64, r: Option<[f64; 4]>) -> Option<[f32; 4]> {
        let r = r?;
        let (x, y) = self.point(t, r[0], r[1]);
        let (_, k) = self.at(t);
        let (w, h) = ((r[2] * k.0) as f32, (r[3] * k.1) as f32);
        let on = x + w > 0.0 && y + h > 0.0 && x < self.size.0 as f32 && y < self.size.1 as f32;
        on.then_some([x, y, w, h])
    }
}

/// Screen `i`'s track from the session's raw events (in time order).
pub fn track_for(dir: &Path, session: &Session, i: usize, events: &[RawEvent]) -> ScreenTrack {
    let s = &session.screens[i];
    let size = (f64::from(s.width), f64::from(s.height));
    let mut m = Mapper {
        origin: (s.display.x, s.display.y),
        k: (size.0 / s.display.w.max(1.0), size.1 / s.display.h.max(1.0)),
        window: Vec::new(),
        size,
    };
    if let Some(w) = &s.window {
        m.window.push((i64::MIN, w.rect));
        for e in events {
            if let RawEvent::Window { t, s: si, rect } = e
                && usize::from(*si) == i
            {
                m.window.push((*t, *rect));
            }
        }
    }
    let scale = (size.0
        / s.window
            .as_ref()
            .map_or(s.display.w, |w| w.rect[2])
            .max(1.0)) as f32;
    let mut track = InputTrack::new(s.width, s.height, scale);
    let mut shape_files = Vec::new();
    let mut shape_index: HashMap<String, u16> = HashMap::new();
    let mut pending: Option<u16> = None;
    let mut last: Option<(f32, f32)> = None;
    let push = |track: &mut InputTrack, pending: &mut Option<u16>, c: CursorSample| {
        let shape = pending.take().unwrap_or(SHAPE_UNKNOWN);
        track.cursor.push((c.0, c.1, c.2, shape));
    };
    for e in events {
        match e {
            RawEvent::Cursor { t, s: si, x, y } if usize::from(*si) == i => {
                let (px, py) = m.point(*t, *x, *y);
                last = Some((px, py));
                push(&mut track, &mut pending, (*t, px, py, 0));
            }
            RawEvent::Move { t, x, y, .. } => {
                let (px, py) = m.point(*t, *x, *y);
                last = Some((px, py));
                push(&mut track, &mut pending, (*t, px, py, 0));
            }
            RawEvent::Button {
                t,
                x,
                y,
                b,
                down,
                n,
            } => {
                let (px, py) = m.point(*t, *x, *y);
                last = Some((px, py));
                push(&mut track, &mut pending, (*t, px, py, 0));
                track.clicks.push(Click {
                    t: *t,
                    x: px,
                    y: py,
                    button: *b,
                    down: *down,
                    count: *n,
                });
            }
            RawEvent::Scroll { t, x, y, dx, dy } => {
                let (px, py) = m.point(*t, *x, *y);
                let (_, k) = m.at(*t);
                track.scrolls.push(Scroll {
                    t: *t,
                    x: px,
                    y: py,
                    dx: (*dx * k.0) as f32,
                    dy: (*dy * k.1) as f32,
                });
            }
            RawEvent::Shape {
                t,
                sha256,
                w,
                h,
                hot_x,
                hot_y,
                pt_w,
                pt_h,
            } => {
                let idx = *shape_index.entry(sha256.clone()).or_insert_with(|| {
                    track.shapes.push(ShapeInfo {
                        sha256: sha256.clone(),
                        w: *w,
                        h: *h,
                        hot_x: *hot_x as f32,
                        hot_y: *hot_y as f32,
                        pt_w: *pt_w as f32,
                        pt_h: *pt_h as f32,
                    });
                    shape_files.push(dir.join(CURSORS_DIR).join(format!("{sha256}.png")));
                    (track.shapes.len() - 1) as u16
                });
                // The new shape holds from now: a sample here, where the cursor was.
                match last {
                    Some((px, py)) => track.cursor.push((*t, px, py, idx)),
                    None => pending = Some(idx),
                }
            }
            RawEvent::Focus {
                t,
                rect,
                caret,
                window,
                role,
                app,
                ..
            } => track.focus.push(Focus {
                t: *t,
                rect: m.rect(*t, *rect),
                caret: m.rect(*t, *caret),
                window: m.rect(*t, *window),
                role: role.clone(),
                app: app.clone(),
            }),
            RawEvent::Keys { t, n } => track.keys.push(Keys { t: *t, n: *n }),
            RawEvent::App { t, app, name } => track.apps.push(App {
                t: *t,
                app: app.clone(),
                name: name.clone(),
            }),
            RawEvent::Marker { t } => track.markers.push(*t),
            RawEvent::Cursor { .. } | RawEvent::Window { .. } => {}
        }
    }
    // Events at one time from different threads: keep the order stable by time.
    track.cursor.sort_by_key(|c| c.0);
    ScreenTrack {
        file: dir.join(&s.file),
        track,
        shape_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rec::{DisplayGeom, ScreenFile, WindowRef};

    fn session(window: Option<WindowRef>) -> Session {
        Session {
            schema: crate::rec::SESSION_SCHEMA.into(),
            name: "t".into(),
            app_version: "0".into(),
            screens: vec![ScreenFile {
                file: "screen-2.mov".into(),
                display: DisplayGeom {
                    id: 2,
                    name: "Side".into(),
                    x: 1512.0,
                    y: 0.0,
                    w: 1920.0,
                    h: 1080.0,
                    scale: 2.0,
                },
                window,
                width: 3840,
                height: 2160,
                fps: 60,
                frames: 0,
                dropped: 0,
            }],
            face: None,
            events: "events.ndjson".into(),
            permissions: Default::default(),
            missing: vec![],
            duration_us: 0,
            complete: true,
        }
    }

    #[test]
    fn a_second_display_maps_global_points_to_its_pixels() {
        let s = session(None);
        let ev = vec![
            RawEvent::Shape {
                t: 0,
                sha256: "abc".into(),
                w: 64,
                h: 64,
                hot_x: 8.0,
                hot_y: 8.0,
                pt_w: 32.0,
                pt_h: 32.0,
            },
            RawEvent::Cursor {
                t: 10,
                s: 0,
                x: 1612.0,
                y: 50.0,
            },
            RawEvent::Button {
                t: 20,
                x: 1612.0,
                y: 50.0,
                b: 0,
                down: true,
                n: 1,
            },
            RawEvent::Move {
                t: 30,
                x: 100.0,
                y: 50.0,
                drag: false,
            },
            RawEvent::Focus {
                t: 40,
                rect: Some([1600.0, 40.0, 100.0, 20.0]),
                caret: None,
                window: None,
                role: "AXTextField".into(),
                subrole: String::new(),
                app: "x".into(),
            },
        ];
        let t = track_for(Path::new("/r"), &s, 0, &ev).track;
        assert!(t.check().is_ok(), "{:?}", t.check());
        assert_eq!(t.scale, 2.0);
        assert_eq!(t.cursor[0], (10, 200.0, 100.0, 0));
        assert_eq!(t.clicks[0].x, 200.0);
        // On the main display, left of this one: off the picture.
        assert!(t.cursor_at(30).is_none());
        assert_eq!(t.focus[0].rect, Some([176.0, 80.0, 200.0, 40.0]));
    }

    #[test]
    fn a_window_recording_follows_the_window() {
        let s = session(Some(WindowRef {
            id: 7,
            title: "w".into(),
            app: "a".into(),
            rect: [100.0, 100.0, 1920.0, 1080.0],
        }));
        let ev = vec![
            RawEvent::Cursor {
                t: 10,
                s: 0,
                x: 200.0,
                y: 100.0,
            },
            RawEvent::Window {
                t: 15,
                s: 0,
                rect: [300.0, 100.0, 1920.0, 1080.0],
            },
            RawEvent::Cursor {
                t: 20,
                s: 0,
                x: 400.0,
                y: 100.0,
            },
        ];
        let t = track_for(Path::new("/r"), &s, 0, &ev).track;
        assert_eq!((t.cursor[0].1, t.cursor[1].1), (200.0, 200.0));
    }
}
