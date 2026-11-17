//! Zoom to focus and clicks (decision 0045 R5, SPEC §5.18): from a recording's input
//! track, a crop path that punches in on what is being typed into and on what was
//! clicked, and zooms back out when nothing happens.
//!
//! **What it aims at**, each sample: while keys are down (within [`ZoomRules::typing_us`])
//! into a text element, its caret ([`ZoomRules::max_zoom`] ×), else the field itself
//! (zoomed only as far as the whole field fits); else, for
//! [`ZoomRules::click_hold_us`] after a click, the click (a click after the last key
//! ends the typing). After
//! [`ZoomRules::idle_us`] without a key or a click the crop goes back to the layer's own.
//! Secure fields log no keys, so they never zoom.
//!
//! **The path** ([`zoom_path`]): a zoomed crop has the layer's own crop's aspect and stays
//! inside it; it holds while the aim stays in its middle [`ZoomRules::dead`] share and the
//! zoom stays within 10 %, and two moves are never closer than [`ZoomRules::gap_us`]. Each
//! key becomes an eased visual split (`cursor_follow_apply`), as a cursor follow's do:
//! the two are the same kind of path on one layer, so writing one replaces the other.
//!
//! Pure and deterministic: the host reads the track and maps samples to source times.

use gmclip_model::entities::{MediaRef, MemberRole, Part, Source, SyncMember, SyncSegment};
use gmclip_model::rec::{Focus, InputTrack};
use gmclip_model::{Doc, Id};

use crate::activity::NRect;

/// The least zoom worth a move: a field that only fits at less is not zoomed into.
pub const MIN_ZOOM: f64 = 1.2;

/// What the zoom path keeps to. Times are µs on the track's (the screen file's) clock.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoomRules {
    /// The layer's own crop (normalised source rect): zoomed crops stay inside it, and
    /// zooming out returns to it.
    pub base: NRect,
    /// How many times smaller than `base` a caret or click crop is (1.2–4).
    pub max_zoom: f64,
    /// Keys within this long before a sample mean typing.
    pub typing_us: i64,
    /// A click is aimed at for this long.
    pub click_hold_us: i64,
    /// This long without a key or a click zooms out.
    pub idle_us: i64,
    /// Two moves are at least this far apart.
    pub gap_us: i64,
    /// The middle share of the crop the aim may move in before the crop follows.
    pub dead: f64,
    /// A field is zoomed so that it fits with this much room around it (× its size).
    pub field_room: f64,
}

impl ZoomRules {
    pub fn new(base: NRect, max_zoom: f64) -> Self {
        ZoomRules {
            base,
            max_zoom: max_zoom.clamp(MIN_ZOOM, 4.0),
            typing_us: 1_000_000,
            click_hold_us: 1_500_000,
            idle_us: 2_500_000,
            gap_us: 1_000_000,
            dead: 0.6,
            field_room: 1.25,
        }
    }
}

/// Why a key was written.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ZoomWhy {
    /// Typing, on the caret.
    Caret,
    /// Typing into a field that reports no caret: the field.
    Field,
    /// A click.
    Click,
    /// Nothing happened for a while: back to the layer's own crop.
    Idle,
}

impl ZoomWhy {
    pub fn as_str(self) -> &'static str {
        match self {
            ZoomWhy::Caret => "caret",
            ZoomWhy::Field => "field",
            ZoomWhy::Click => "click",
            ZoomWhy::Idle => "idle",
        }
    }
}

/// One move of the crop: from sample `sample` on, `crop` (`None`: back to the layer's
/// own crop).
#[derive(Clone, Debug, PartialEq)]
pub struct ZoomKey {
    pub sample: usize,
    pub crop: Option<NRect>,
    pub why: ZoomWhy,
}

/// Whether a focused element takes typing (a caret, or a text role).
fn texty(f: &Focus) -> bool {
    f.caret.is_some()
        || ["Text", "ComboBox", "SearchField"]
            .iter()
            .any(|k| f.role.contains(k))
}

fn inside(r: &NRect, x: f64, y: f64) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

enum Want {
    In {
        x: f64,
        y: f64,
        zoom: f64,
        why: ZoomWhy,
    },
    Out,
    Keep,
}

/// What sample time `t` asks for.
fn want(track: &InputTrack, t: i64, r: &ZoomRules) -> Want {
    let (w, h) = (f64::from(track.w.max(1)), f64::from(track.h.max(1)));
    let last_key = track
        .keys
        .iter()
        .rev()
        .find(|k| k.t <= t && k.n > 0)
        .map(|k| k.t);
    let last_click = track.last_click(t);
    // a click after the last key moves on from the field
    let clicked_since = matches!((last_click, last_key), (Some((c, _)), Some(k)) if c.t > k);
    if !clicked_since
        && last_key.is_some_and(|k| t - k < r.typing_us)
        && let Some(f) = track.focus_at(t).filter(|f| texty(f))
    {
        if let Some([cx, cy, cw, ch]) = f.caret {
            let (x, y) = (
                (f64::from(cx) + f64::from(cw) / 2.0) / w,
                (f64::from(cy) + f64::from(ch) / 2.0) / h,
            );
            if inside(&r.base, x, y) {
                return Want::In {
                    x,
                    y,
                    zoom: r.max_zoom,
                    why: ZoomWhy::Caret,
                };
            }
        } else if let Some([fx, fy, fw, fh]) = f.rect {
            let (x, y) = (
                (f64::from(fx) + f64::from(fw) / 2.0) / w,
                (f64::from(fy) + f64::from(fh) / 2.0) / h,
            );
            let (nw, nh) = (
                f64::from(fw) / w * r.field_room,
                f64::from(fh) / h * r.field_room,
            );
            let fit = (r.base.w / nw.max(1e-6)).min(r.base.h / nh.max(1e-6));
            let zoom = fit.min(r.max_zoom);
            if zoom >= MIN_ZOOM && inside(&r.base, x, y) {
                return Want::In {
                    x,
                    y,
                    zoom,
                    why: ZoomWhy::Field,
                };
            }
        }
    }
    if let Some((c, ago)) = last_click
        && ago < r.click_hold_us
    {
        let (x, y) = (f64::from(c.x) / w, f64::from(c.y) / h);
        if inside(&r.base, x, y) {
            return Want::In {
                x,
                y,
                zoom: r.max_zoom,
                why: ZoomWhy::Click,
            };
        }
    }
    let last = last_key.max(last_click.map(|(c, _)| c.t));
    if last.is_none_or(|l| t - l >= r.idle_us) {
        Want::Out
    } else {
        Want::Keep
    }
}

/// The zoom path over `samples` (track times, µs, increasing).
pub fn zoom_path(track: &InputTrack, samples: &[i64], r: &ZoomRules) -> Vec<ZoomKey> {
    let b = r.base;
    let place = |cx: f64, cy: f64, z: f64| {
        let (zw, zh) = (b.w / z, b.h / z);
        NRect {
            x: (cx - zw / 2.0).clamp(b.x, b.x + b.w - zw),
            y: (cy - zh / 2.0).clamp(b.y, b.y + b.h - zh),
            w: zw,
            h: zh,
        }
    };
    let mut keys: Vec<ZoomKey> = Vec::new();
    let mut cur: Option<NRect> = None;
    let mut last_t: Option<i64> = None;
    for (i, &t) in samples.iter().enumerate() {
        let may = last_t.is_none_or(|l| t - l >= r.gap_us);
        match want(track, t, r) {
            Want::In { x, y, zoom, why } => {
                let next = place(x, y, zoom);
                let change = cur.is_none_or(|c| {
                    let (mx, my) = (c.w * (1.0 - r.dead) / 2.0, c.h * (1.0 - r.dead) / 2.0);
                    let out_of_dead =
                        x < c.x + mx || x > c.x + c.w - mx || y < c.y + my || y > c.y + c.h - my;
                    let rezoom = (c.w / next.w - 1.0).abs() > 0.1;
                    let moved =
                        (c.x - next.x).abs() > 0.02 * b.w || (c.y - next.y).abs() > 0.02 * b.h;
                    (out_of_dead || rezoom) && (moved || rezoom)
                });
                if change && may {
                    keys.push(ZoomKey {
                        sample: i,
                        crop: Some(next),
                        why,
                    });
                    cur = Some(next);
                    last_t = Some(t);
                }
            }
            Want::Out => {
                if cur.is_some() && may {
                    keys.push(ZoomKey {
                        sample: i,
                        crop: None,
                        why: ZoomWhy::Idle,
                    });
                    cur = None;
                    last_t = Some(t);
                }
            }
            Want::Keep => {}
        }
    }
    keys
}

/// The source a part's picture comes from and its sync offset at the part's start
/// (member time = group time − offset; 0 for a part on one source), as the planner
/// picks them. `None` when the group has no picture member or no segment covers it.
pub fn picture_of(doc: &Doc, p: &Part) -> Option<(Id<Source>, i64)> {
    match p.media {
        MediaRef::Source(s) => Some((s, 0)),
        MediaRef::Group(g) => {
            let group = doc.get(g)?;
            let m = group.picture.and_then(|m| doc.get(m)).or_else(|| {
                doc.children::<SyncMember>(g.entity())
                    .filter_map(|m| doc.get(m))
                    .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
            })?;
            let off = doc
                .children::<SyncSegment>(m.id.entity())
                .filter_map(|s| doc.get(s))
                .find(|s| s.src_range.start.0 <= p.src_in.0 && p.src_in.0 < s.src_range.end.0)?
                .offset
                .0;
            Some((m.source, off))
        }
    }
}

/// Flicks to µs (track time).
pub fn flicks_to_us(f: i64) -> i64 {
    (i128::from(f) * 1_000_000 / i128::from(gmclip_model::time::FLICKS_PER_SECOND)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmclip_model::rec::{Click, Keys};

    const FULL: NRect = NRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    fn samples(secs: f64) -> Vec<i64> {
        (0..(secs * 10.0) as i64).map(|i| i * 100_000).collect()
    }

    fn field(t: i64, caret: Option<[f32; 4]>) -> Focus {
        Focus {
            t,
            rect: Some([1000.0, 600.0, 400.0, 40.0]),
            caret,
            window: None,
            role: "AXTextField".into(),
            app: "com.apple.Safari".into(),
        }
    }

    fn typing(track: &mut InputTrack, from_s: f64, to_s: f64) {
        let mut t = (from_s * 1e6) as i64;
        while t < (to_s * 1e6) as i64 {
            track.keys.push(Keys { t, n: 2 });
            t += 100_000;
        }
    }

    #[test]
    fn typing_in_a_field_zooms_on_the_caret_then_out_when_idle() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        tr.focus
            .push(field(500_000, Some([1200.0, 610.0, 2.0, 20.0])));
        typing(&mut tr, 1.0, 3.0);
        let keys = zoom_path(&tr, &samples(8.0), &ZoomRules::new(FULL, 2.0));
        assert_eq!(keys.len(), 2, "{keys:?}");
        let k = &keys[0];
        assert_eq!(k.why, ZoomWhy::Caret);
        assert_eq!(k.sample, 10, "zooms in with the first key");
        let c = k.crop.unwrap();
        assert!(
            (c.w - 0.5).abs() < 1e-9 && (c.h - 0.5).abs() < 1e-9,
            "{c:?}"
        );
        let (cx, cy) = (c.x + c.w / 2.0, c.y + c.h / 2.0);
        assert!(
            (cx - 1201.0 / 1920.0).abs() < 0.01 && (cy - 620.0 / 1080.0).abs() < 0.01,
            "{c:?}"
        );
        // the last key at 2.9 s; out 2.5 s later
        assert_eq!(keys[1].crop, None);
        assert_eq!(keys[1].why, ZoomWhy::Idle);
        assert_eq!(keys[1].sample, 54, "{keys:?}");
    }

    #[test]
    fn a_field_without_a_caret_is_zoomed_only_as_far_as_it_fits() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        tr.focus.push(field(0, None));
        typing(&mut tr, 1.0, 2.0);
        let keys = zoom_path(&tr, &samples(3.0), &ZoomRules::new(FULL, 4.0));
        let k = &keys[0];
        assert_eq!(k.why, ZoomWhy::Field);
        let c = k.crop.unwrap();
        // 400 px × 1.25 room = 500 of 1920: 3.84×
        assert!(c.w * 1920.0 >= 500.0 - 1e-6 && c.w < 0.3, "{c:?}");
    }

    #[test]
    fn keys_outside_a_text_element_do_not_zoom() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        let mut f = field(0, None);
        f.role = "AXButton".into();
        tr.focus.push(f);
        typing(&mut tr, 1.0, 2.0);
        assert!(zoom_path(&tr, &samples(4.0), &ZoomRules::new(FULL, 2.0)).is_empty());
    }

    #[test]
    fn a_click_is_held_then_zoomed_out() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        tr.clicks.push(Click {
            t: 2_000_000,
            x: 300.0,
            y: 200.0,
            button: 0,
            down: true,
            count: 1,
        });
        tr.clicks.push(Click {
            t: 2_080_000,
            x: 300.0,
            y: 200.0,
            button: 0,
            down: false,
            count: 1,
        });
        let keys = zoom_path(&tr, &samples(8.0), &ZoomRules::new(FULL, 2.0));
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert_eq!((keys[0].sample, keys[0].why), (20, ZoomWhy::Click));
        let c = keys[0].crop.unwrap();
        assert_eq!((c.x, c.y), (0.0, 0.0), "clamped inside the base: {c:?}");
        assert_eq!((keys[1].sample, keys[1].crop), (45, None));
    }

    #[test]
    fn a_click_after_typing_moves_on_from_the_caret() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        tr.focus.push(field(0, Some([1200.0, 610.0, 2.0, 20.0])));
        typing(&mut tr, 1.0, 1.5);
        tr.clicks.push(Click {
            t: 2_500_000,
            x: 200.0,
            y: 900.0,
            button: 0,
            down: true,
            count: 1,
        });
        let keys = zoom_path(&tr, &samples(4.0), &ZoomRules::new(FULL, 2.0));
        let whys: Vec<(usize, ZoomWhy)> = keys.iter().map(|k| (k.sample, k.why)).collect();
        assert_eq!(whys, vec![(10, ZoomWhy::Caret), (25, ZoomWhy::Click)]);
    }

    #[test]
    fn moves_are_at_least_a_second_apart_and_inside_the_base() {
        let base = NRect {
            x: 0.1,
            y: 0.1,
            w: 0.6,
            h: 0.6,
        };
        let mut tr = InputTrack::new(1000, 1000, 1.0);
        // clicks every 300 ms across the picture
        for i in 0..20 {
            let x = if i % 2 == 0 { 150.0 } else { 650.0 };
            tr.clicks.push(Click {
                t: 1_000_000 + i * 300_000,
                x,
                y: 400.0,
                button: 0,
                down: true,
                count: 1,
            });
        }
        let s = samples(12.0);
        let keys = zoom_path(&tr, &s, &ZoomRules::new(base, 2.0));
        assert!(keys.len() >= 3, "{keys:?}");
        for w in keys.windows(2) {
            assert!(s[w[1].sample] - s[w[0].sample] >= 1_000_000, "{keys:?}");
        }
        for k in &keys {
            if let Some(c) = k.crop {
                assert!(c.x >= base.x - 1e-9 && c.y >= base.y - 1e-9, "{c:?}");
                assert!(
                    c.x + c.w <= base.x + base.w + 1e-9 && c.y + c.h <= base.y + base.h + 1e-9,
                    "{c:?}"
                );
                assert!((c.w / c.h - base.w / base.h).abs() < 1e-9);
            }
        }
        assert_eq!(keys.last().unwrap().crop, None, "zoomed out at the end");
    }

    #[test]
    fn caret_moves_inside_the_middle_hold_the_crop() {
        let mut tr = InputTrack::new(1920, 1080, 2.0);
        for (i, x) in [900.0f32, 920.0, 940.0, 960.0, 980.0].iter().enumerate() {
            tr.focus.push(field(
                i as i64 * 400_000 + 900_000,
                Some([*x, 540.0, 2.0, 20.0]),
            ));
        }
        typing(&mut tr, 1.0, 3.0);
        let keys = zoom_path(&tr, &samples(3.0), &ZoomRules::new(FULL, 2.0));
        assert_eq!(keys.len(), 1, "{keys:?}");
    }

    #[test]
    fn a_group_part_reads_its_picture_member_at_its_offset() {
        use gmclip_model::entities::*;
        use gmclip_model::fixtures as f;
        use gmclip_model::{Flicks, OrderKey, Ulid};
        let mut d = Doc::new();
        let (face, screen) = (f::source(1), f::source(2));
        let g = SyncGroup {
            id: Id::new(Ulid(10)),
            master_audio: None,
            picture: None,
        };
        let keys = OrderKey::sequence(None, 2).unwrap();
        let member = |n: u128, s: Id<Source>, role, k: &OrderKey| SyncMember {
            id: Id::new(Ulid(n)),
            group: g.id,
            order: k.clone(),
            source: s,
            role,
            rate_ppm: 0.0,
            method: SyncMethod::Import,
            confidence: 1.0,
            under_db: None,
        };
        let (m1, m2) = (
            member(11, face.id, MemberRole::Audio, &keys[0]),
            member(12, screen.id, MemberRole::Picture, &keys[1]),
        );
        let seg = SyncSegment {
            id: Id::new(Ulid(13)),
            member: m2.id,
            order: OrderKey::first(),
            src_range: SrcRange {
                start: Flicks(0),
                end: Flicks(i64::MAX / 2),
            },
            offset: Flicks(705_600_000),
        };
        let cut = f::cut(20);
        let mut p = f::part(
            21,
            cut.id,
            OrderKey::first(),
            5 * 705_600_000,
            6 * 705_600_000,
        );
        p.media = MediaRef::Group(g.id);
        let sid = screen.id;
        for e in [face, screen] {
            d.insert_for_test(e);
        }
        d.insert_for_test(g);
        d.insert_for_test(m1);
        d.insert_for_test(m2);
        d.insert_for_test(seg);
        d.insert_for_test(cut);
        d.insert_for_test(p.clone());
        assert_eq!(picture_of(&d, &p), Some((sid, 705_600_000)));
        // group time 5 s is 4 s into the screen file
        assert_eq!(flicks_to_us(p.src_in.0 - 705_600_000), 4_000_000);
    }
}
