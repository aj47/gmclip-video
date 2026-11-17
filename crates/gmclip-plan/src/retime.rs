//! Picture rate conversion (`/1`, decision 0017): which source frame an output frame
//! shows when the part's picture source runs at another nominal rate than `output.fps`.
//!
//! A part with `picture.fps` reads its source by time: output frame `k` of the part
//! (0-based) is source time `T = (first_frame + k) / output.fps` on the source's
//! presentation clock (presentation index 0 at 0), and shows the last source frame
//! presented at or before `T` — no blending, so a faster source drops frames and a
//! slower one repeats them. The audio is untouched: it runs on the output clock
//! (rule 4) from the same source time, so picture and sound cannot drift apart.
//!
//! [`PtsClock`] reads a source's own presentation timestamps (a VFR source is never
//! assumed evenly spaced); [`nominal_frame`] is the same pick for an ideal CFR source,
//! for callers that have no index (thumbnails, matte spans).

use crate::types::{Fps, Part};

/// `output.fps` as `(num, den)`.
fn out_rate(out: Fps) -> (u128, u128) {
    let (n, d) = out.rate();
    (u128::from(n), u128::from(d))
}

/// `(num, den)` of a part's speed (1/1 at normal speed; decision 0030).
fn speed_of(p: &Part) -> (u128, u128) {
    p.speed
        .map_or((1, 1), |s| (u128::from(s.num), u128::from(s.den.max(1))))
}

/// How far into its source a ramped part is on output frame `k` (decision 0033), in
/// source frames at the output rate, `(num, den)`; `None` without a ramp.
fn ramp_advance(p: &Part, k: u64) -> Option<(u128, u128)> {
    let sp = p.speed.filter(|s| s.ramp.is_some())?;
    // validation bounds a ramp so this cannot overflow; saturate if it ever did
    let (n, d) = sp.advance(k).unwrap_or((i128::MAX / 4, 1));
    Some((
        u128::try_from(n.max(0)).unwrap_or(0),
        u128::try_from(d.max(1)).unwrap_or(1),
    ))
}

/// Source time of output frame `k` of part `p` as an exact fraction of a second
/// `(num, den)`: `(first_frame + k · speed) / output.fps`; with a speed ramp,
/// `(first_frame + advance(k)) / output.fps` (decision 0033).
pub fn source_time(p: &Part, k: u64, out: Fps) -> (u128, u128) {
    let (n, d) = out_rate(out);
    if let Some((un, ud)) = ramp_advance(p, k) {
        return ((u128::from(p.picture.first_frame) * ud + un) * d, n * ud);
    }
    let (sn, sd) = speed_of(p);
    (
        (u128::from(p.picture.first_frame) * sd + u128::from(k) * sn) * d,
        n * sd,
    )
}

/// The source frame output frame `k` of part `p` shows, for an ideal CFR source at the
/// part's `picture.fps`: `⌊(first_frame + k · speed) · fps / output.fps⌋`; without a
/// rate conversion `⌊first_frame + k · speed⌋` (rule 3; decision 0030); a ramp puts its
/// integral `advance(k)` in place of `k · speed` (decision 0033).
pub fn nominal_frame(p: &Part, k: u64, out: Fps) -> u64 {
    match p.picture.fps {
        Some(r) if r.den > 0 => {
            let (n, d) = source_time(p, k, out);
            let f = n * u128::from(r.num) / (d * u128::from(r.den));
            u64::try_from(f).unwrap_or(u64::MAX)
        }
        _ => {
            if let Some((un, ud)) = ramp_advance(p, k) {
                let f = u128::from(p.picture.first_frame) + un / ud;
                return u64::try_from(f).unwrap_or(u64::MAX);
            }
            let (sn, sd) = speed_of(p);
            let f = u128::from(p.picture.first_frame) + u128::from(k) * sn / sd;
            u64::try_from(f).unwrap_or(u64::MAX)
        }
    }
}

/// The source frames a part's live picture spans, `(first, count)`, by
/// [`nominal_frame`]: what a per-span cache (a matte track) keys on.
pub fn picture_span(p: &Part, out: Fps) -> (u64, u64) {
    let n = p.out.len().max(1);
    let a = nominal_frame(p, 0, out);
    let z = nominal_frame(p, n - 1, out);
    (a, z.saturating_sub(a) + 1)
}

/// The source frames a clip event's picture spans, `(first, count)`, at nominal rates
/// (no clock): what its matte track keys on (decision 0042), as [`picture_span`] for a part.
pub fn clip_span(e: &crate::types::ClipEvent, out: Fps) -> (u64, u64) {
    let last = e.out.len().max(1) - 1;
    let last = match e.picture.frames {
        Some(n) if n > 0 => last.min(n - 1),
        _ => last,
    };
    let a = clip_frame(&e.picture, 0, out, None);
    let z = clip_frame(&e.picture, last, out, None);
    (a, z.saturating_sub(a) + 1)
}

/// A source's presentation clock: each presentation index's PTS minus index 0's, in
/// ticks of `timescale`, in presentation order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtsClock {
    pub timescale: u64,
    pub pts: Vec<i64>,
}

impl PtsClock {
    /// A clock from PTS in presentation order (rebased on the first).
    pub fn new(timescale: u64, pts_in_order: impl IntoIterator<Item = i64>) -> Self {
        let mut pts: Vec<i64> = pts_in_order.into_iter().collect();
        if let Some(&p0) = pts.first() {
            for p in &mut pts {
                *p -= p0;
            }
        }
        Self { timescale, pts }
    }

    /// The last frame presented at or before `num / den` seconds: the largest index
    /// whose PTS is at most that time rounded to the nearest tick (ties up), so a time
    /// on a frame's own (rounded) timestamp shows that frame. 0 before the first frame;
    /// the last frame past the end.
    pub fn frame_at(&self, num: u128, den: u128) -> u64 {
        if self.pts.is_empty() || den == 0 {
            return 0;
        }
        let ts = u128::from(self.timescale.max(1));
        let ticks = (2 * num * ts + den) / (2 * den);
        let ticks = i64::try_from(ticks).unwrap_or(i64::MAX);
        let after = self.pts.partition_point(|&p| p <= ticks);
        after.saturating_sub(1) as u64
    }
}

/// The source frame output frame `k` of part `p` shows: by the source's own clock when
/// one is given and the part converts, else [`nominal_frame`].
pub fn picture_frame(p: &Part, k: u64, out: Fps, clock: Option<&PtsClock>) -> u64 {
    match (p.picture.fps, clock) {
        (Some(_), Some(c)) => {
            let (n, d) = source_time(p, k, out);
            c.frame_at(n, d)
        }
        _ => nominal_frame(p, k, out),
    }
}

/// The source frame a backdrop or clip event shows `k` output frames in (decision 0028):
/// its local frame ([`crate::types::ClipPicture::local`]), then the presentation index at
/// the output rate, or by the source's clock when it converts (as [`picture_frame`]).
pub fn clip_frame(
    p: &crate::types::ClipPicture,
    k: u64,
    out: Fps,
    clock: Option<&PtsClock>,
) -> u64 {
    timed_frame(p.first_frame.saturating_add(p.local(k)), p.fps, out, clock)
}

/// The source frame a live layer of another source (decision 0046) shows `k` output
/// frames into its part: `first_frame + k`, by the source's clock when the layer converts
/// its rate (`layers[].fps`, decision 0047), as [`clip_frame`]. `None` without
/// `first_frame`.
pub fn layer_frame(
    l: &crate::types::Layer,
    k: u64,
    out: Fps,
    clock: Option<&PtsClock>,
) -> Option<u64> {
    let f0 = l.first_frame?;
    Some(timed_frame(f0.saturating_add(k), l.fps, out, clock))
}

/// `at` output frames on a source's presentation clock: the presentation index itself at
/// the output rate; with `fps`, the frame presented at or before `at / output.fps` (by
/// `clock`, else at the nominal rate).
fn timed_frame(
    at: u64,
    fps: Option<crate::types::Rational>,
    out: Fps,
    clock: Option<&PtsClock>,
) -> u64 {
    match fps {
        Some(r) if r.den > 0 => {
            let (n, d) = out_rate(out);
            let (num, den) = (u128::from(at) * d, n);
            match clock {
                Some(c) => c.frame_at(num, den),
                None => u64::try_from(num * u128::from(r.num) / (den * u128::from(r.den)))
                    .unwrap_or(u64::MAX),
            }
        }
        _ => at,
    }
}

#[cfg(test)]
mod speed_tests {
    use super::*;

    fn part(first: u64, speed: Option<(u64, u64)>, fps: Option<(u64, u64)>) -> Part {
        let mut p: Part = serde_json::from_value(serde_json::json!({
            "id": "p1", "out": [0, 100],
            "picture": {"source": "s1", "first_frame": first},
            "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
            "layers": []
        }))
        .unwrap();
        p.speed = speed.map(|(num, den)| crate::types::PartSpeed {
            num,
            den,
            audio: None,
            ramp: None,
        });
        p.picture.fps = fps.map(|(num, den)| crate::types::Rational { num, den });
        p
    }

    /// Decision 0030: a part at 2× shows every other frame, at ½× each frame twice, at
    /// 1.5× by time; a rate conversion composes with it.
    #[test]
    fn speed_picks_frames_by_source_time() {
        let o = Fps::integer(30);
        let pick = |p: &Part, n: u64| (0..n).map(|k| nominal_frame(p, k, o)).collect::<Vec<_>>();
        assert_eq!(pick(&part(10, Some((2, 1)), None), 4), [10, 12, 14, 16]);
        assert_eq!(pick(&part(10, Some((1, 2)), None), 4), [10, 10, 11, 11]);
        assert_eq!(pick(&part(0, Some((3, 2)), None), 5), [0, 1, 3, 4, 6]);
        // 60 fps source at 2× in a 30 fps output: every fourth source frame
        assert_eq!(pick(&part(5, Some((2, 1)), Some((60, 1))), 3), [10, 14, 18]);
        // and by its clock
        let c = PtsClock::new(1000, (0..400).map(|i| i * 1000 / 60));
        let p = part(5, Some((2, 1)), Some((60, 1)));
        assert_eq!(picture_frame(&p, 2, o, Some(&c)), 18);
        assert_eq!(picture_span(&part(10, Some((2, 1)), None), o), (10, 199));
    }

    fn ramped(first: u64, a: (u64, u64), b: (u64, u64), from: u64, frames: u64) -> Part {
        let mut p = part(first, Some(a), None);
        if let Some(s) = p.speed.as_mut() {
            s.ramp = Some(crate::types::SpeedRamp {
                to: crate::types::Rational { num: b.0, den: b.1 },
                from,
                frames,
                curve: crate::types::EaseCurve::Linear,
            });
        }
        p
    }

    /// Decision 0033: a ramp picks frames by the integral of its speed — 1× to 3×
    /// linearly over frames 1–5 advances 0, 1, 2¼, 4, 6¼, 9 (`k + (k − 1)²/4` inside
    /// the ramp), then 3 a frame; the curves and a rate conversion compose with it.
    #[test]
    fn a_ramp_picks_frames_by_the_integral_of_its_speed() {
        let o = Fps::integer(30);
        let pick = |p: &Part, n: u64| (0..n).map(|k| nominal_frame(p, k, o)).collect::<Vec<_>>();
        let up = ramped(100, (1, 1), (3, 1), 1, 4);
        let adv: Vec<(i128, i128)> = (0..8)
            .map(|k| up.speed.unwrap().advance(k).unwrap())
            .collect();
        assert_eq!(
            adv,
            [
                (0, 1),
                (1, 1),
                (9, 4),
                (4, 1),
                (25, 4),
                (9, 1),
                (12, 1),
                (15, 1)
            ]
        );
        assert_eq!(pick(&up, 8), [100, 101, 102, 104, 106, 109, 112, 115]);
        // and back down: 2× to ½× over 3 frames from frame 0, the average speed over the
        // ramp is 1.25, then ½ a frame
        let down = ramped(0, (2, 1), (1, 2), 0, 3);
        assert_eq!(down.speed.unwrap().advance(3), Some((15, 4)));
        assert_eq!(pick(&down, 6), [0, 1, 3, 3, 4, 4]);
        // the f64 sound clock agrees with the exact one on whole frames
        for k in 0..8u64 {
            let (n, d) = up.speed.unwrap().advance(k).unwrap();
            assert!((up.speed.unwrap().advance_f64(k as f64) - n as f64 / d as f64).abs() < 1e-9);
        }
        // every curve reaches the same speed and is continuous at the ramp's ends
        for c in [
            crate::types::EaseCurve::Linear,
            crate::types::EaseCurve::EaseIn,
            crate::types::EaseCurve::EaseOut,
            crate::types::EaseCurve::EaseInOut,
        ] {
            let mut p = ramped(0, (1, 2), (2, 1), 10, 30);
            p.speed.as_mut().unwrap().ramp.as_mut().unwrap().curve = c;
            let sp = p.speed.unwrap();
            let v = |x: f64| sp.advance_f64(x + 1e-3) - sp.advance_f64(x);
            assert!((v(5.0) / 1e-3 - 0.5).abs() < 1e-6, "{c:?} before");
            assert!((v(45.0) / 1e-3 - 2.0).abs() < 1e-6, "{c:?} after");
            // monotone: never a frame back
            let f = pick(&p, 60);
            assert!(f.windows(2).all(|w| w[0] <= w[1]), "{c:?}");
            // the curve's own shape: ease-in is slower than linear early on
            let lin = {
                let mut q = p.clone();
                q.speed.as_mut().unwrap().ramp.as_mut().unwrap().curve =
                    crate::types::EaseCurve::Linear;
                q.speed.unwrap().advance_f64(20.0)
            };
            let here = sp.advance_f64(20.0);
            match c {
                crate::types::EaseCurve::EaseIn | crate::types::EaseCurve::EaseInOut => {
                    assert!(here < lin, "{c:?}")
                }
                crate::types::EaseCurve::EaseOut => assert!(here > lin),
                crate::types::EaseCurve::Linear => assert_eq!(here, lin),
            }
        }
        // a 60 fps source by time: ⌊(first + advance) · 2⌋
        let mut p = ramped(5, (1, 1), (3, 1), 1, 4);
        p.picture.fps = Some(crate::types::Rational { num: 60, den: 1 });
        assert_eq!(nominal_frame(&p, 2, o), 14); // (5 + 9/4) · 2 = 14.5
        let c = PtsClock::new(1000, (0..400).map(|i| i * 1000 / 60));
        assert_eq!(picture_frame(&p, 2, o, Some(&c)), 14);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Rational;

    fn part(first: u64, len: u64, fps: Option<(u64, u64)>) -> Part {
        serde_json::from_value(serde_json::json!({
            "id": "p1", "out": [0, len],
            "picture": {"source": "s1", "first_frame": first},
            "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
            "layers": []
        }))
        .map(|mut p: Part| {
            p.picture.fps = fps.map(|(num, den)| Rational { num, den });
            p
        })
        .unwrap()
    }

    fn out(n: u64) -> Fps {
        serde_json::from_value(serde_json::json!({"num": n, "den": 1})).unwrap()
    }

    #[test]
    fn same_rate_is_one_to_one() {
        let p = part(100, 10, None);
        assert_eq!(
            (0..5)
                .map(|k| nominal_frame(&p, k, out(30)))
                .collect::<Vec<_>>(),
            [100, 101, 102, 103, 104]
        );
    }

    /// Decision 0028: a backdrop or clip picture loops or holds on its own local frames,
    /// then reads its source like a part.
    #[test]
    fn clip_frames_loop_hold_and_convert() {
        let cp = |first: u64, frames: Option<u64>, lp: Option<bool>, fps: Option<(u64, u64)>| {
            crate::types::ClipPicture {
                source: serde_json::from_value(serde_json::json!("s1")).unwrap(),
                first_frame: first,
                fps: fps.map(|(num, den)| Rational { num, den }),
                frames,
                r#loop: lp,
            }
        };
        let o = out(30);
        let straight = cp(100, None, None, None);
        assert_eq!(clip_frame(&straight, 7, o, None), 107);
        let looped = cp(100, Some(10), Some(true), None);
        let v: Vec<u64> = (8..13).map(|k| clip_frame(&looped, k, o, None)).collect();
        assert_eq!(v, [108, 109, 100, 101, 102]);
        let held = cp(100, Some(10), None, None);
        assert_eq!(clip_frame(&held, 50, o, None), 109);
        // a 60 fps clip in a 30 fps output: every other frame, looping on output frames
        let fast = cp(10, Some(4), Some(true), Some((60, 1)));
        let v: Vec<u64> = (0..6).map(|k| clip_frame(&fast, k, o, None)).collect();
        assert_eq!(v, [20, 22, 24, 26, 20, 22]);
        // …and by its own clock when one is given
        let c = PtsClock::new(1000, (0..100).map(|i| i * 1000 / 60));
        assert_eq!(clip_frame(&fast, 1, o, Some(&c)), 22);
    }

    #[test]
    fn sixty_into_thirty_takes_every_other() {
        // first_frame 10 at 30 fps = 1/3 s = source frame 20 at 60
        let p = part(10, 4, Some((60, 1)));
        let v: Vec<u64> = (0..4).map(|k| nominal_frame(&p, k, out(30))).collect();
        assert_eq!(v, [20, 22, 24, 26]);
    }

    #[test]
    fn ntsc_into_thirty_repeats_one_in_a_thousand() {
        let p = part(0, 1002, Some((30000, 1001)));
        let v: Vec<u64> = (0..1002).map(|k| nominal_frame(&p, k, out(30))).collect();
        // ⌊k·1000/1001⌋: k = 1000 → 999, k = 1001 → 1000 … the one repeat is at k = 1
        assert_eq!(&v[..3], &[0, 0, 1]);
        assert_eq!(v[1001], 1000);
        let repeats = v.windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(repeats, 1);
    }

    #[test]
    fn film_into_thirty_follows_pulldown_by_time() {
        let p = part(0, 5, Some((24000, 1001)));
        let v: Vec<u64> = (0..5).map(|k| nominal_frame(&p, k, out(30))).collect();
        assert_eq!(v, [0, 0, 1, 2, 3]);
    }

    #[test]
    fn the_clock_reads_timestamps_not_spacing() {
        // VFR: frames at 0, 33, 100, 133 ms (one long frame)
        let c = PtsClock::new(1000, [5000, 5033, 5100, 5133]);
        let p = part(0, 5, Some((30, 1)));
        let v: Vec<u64> = (0..5)
            .map(|k| picture_frame(&p, k, out(30), Some(&c)))
            .collect();
        // 0, 33.3, 66.7, 100, 133.3 ms
        assert_eq!(v, [0, 1, 1, 2, 3]);
    }

    #[test]
    fn a_time_on_a_rounded_timestamp_shows_that_frame() {
        // 29.97 in a ms container: frame 3 is stored at round(100.1) = 100 ms, so its
        // exact time (100.1 ms) and anything within half a tick before it show frame 3
        let c = PtsClock::new(1000, [0, 33, 67, 100, 133]);
        assert_eq!(c.frame_at(1001, 10_000), 3); // 100.1 ms → tick 100
        assert_eq!(c.frame_at(995, 10_000), 3); // 99.5 ms → tick 100 (ties up)
        assert_eq!(c.frame_at(994, 10_000), 2); // 99.4 ms → tick 99
    }

    #[test]
    fn span_covers_every_pick() {
        let p = part(7, 30, Some((60000, 1001)));
        let (a, n) = picture_span(&p, out(30));
        for k in 0..30 {
            let f = nominal_frame(&p, k, out(30));
            assert!(f >= a && f < a + n);
        }
    }
}
