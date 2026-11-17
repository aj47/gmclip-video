//! Clocks (SPEC §4.1.2), skeleton: `src` (source or sync group), `local` (part/clip
//! start), `cut` (parts end to end, the editing clock) and `out(framing)` (with
//! enabled clips, quantised). Only this module converts between them.
//!
//! What is here: [`ModelTime`], the cut layout (part start/duration on the `cut`
//! clock), `cut ↔ src ↔ local` for parts, and the analysis-free subset of anchor
//! resolution that `effective()` needs. Quantisation (the tie rule) lives in
//! [`crate::time`] (Lane A, A01); the `out` clock (clips, framing fps) and sync-group
//! member mapping (`model::sync::map`) are A01/A15 work and return `None` here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anchor::{Anchor, CutSide};
use crate::doc::Doc;
use crate::entities::{Clip, Cut, Framing, Part, Source, SyncGroup};
use crate::ids::Id;
use crate::time::{FLICKS_PER_SECOND, Flicks};

/// Flicks per millisecond (705 600 000 / 1000, exact).
pub const FLICKS_PER_MS: i64 = FLICKS_PER_SECOND / 1000;

/// A time on one of the four clocks (the model form of the wire `WireTime`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "clock", rename_all = "snake_case")]
pub enum ModelTime {
    Src {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<Id<Source>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<Id<SyncGroup>>,
        t: Flicks,
    },
    Cut {
        cut: Id<Cut>,
        t: Flicks,
    },
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        part: Option<Id<Part>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<Id<Clip>>,
        t: Flicks,
    },
    Out {
        cut: Id<Cut>,
        framing: Id<Framing>,
        t: Flicks,
    },
}

pub fn ms_to_flicks(ms: i64) -> Flicks {
    Flicks(ms.saturating_mul(FLICKS_PER_MS))
}

/// Exact ms for a flick count, if it is a whole number of ms.
pub fn flicks_to_exact_ms(f: Flicks) -> Option<i64> {
    (f.0 % FLICKS_PER_MS == 0).then_some(f.0 / FLICKS_PER_MS)
}

/// A part's speed as an exact ratio `num/den` (an f64 is a dyadic rational, so this is
/// exact). `None` for a speed that is not finite, not positive, or outside
/// `[2⁻³², 2³²]`; such parts play at 1.0.
pub fn speed_ratio(speed: f64) -> Option<(i128, i128)> {
    if !(speed.is_finite() && speed > 0.0)
        || !(f64::powi(2.0, -32)..=f64::powi(2.0, 32)).contains(&speed)
    {
        return None;
    }
    let bits = speed.to_bits();
    let exp = i32::try_from((bits >> 52) & 0x7ff).ok()?;
    let frac = i128::from(bits & ((1u64 << 52) - 1));
    // normal numbers only (the range above excludes subnormals)
    let mant = frac | (1i128 << 52);
    let e = exp - 1075; // speed = mant · 2^e
    Some(if e >= 0 {
        (mant << e, 1)
    } else {
        (mant, 1i128 << (-e))
    })
}

/// `round(v · num / den)` with SPEC §4.1.1's tie rule (nearest, ties to the earlier),
/// exactly, in i128. `den > 0`. `None` on overflow.
pub fn mul_div_ties_earlier(v: i64, num: i128, den: i128) -> Option<i64> {
    let n = i128::from(v).checked_mul(num)?;
    // ⌈(2n − den) / 2den⌉
    let a = n.checked_mul(2)?.checked_sub(den)?;
    let d2 = den.checked_mul(2)?;
    i64::try_from(-((-a).div_euclid(d2))).ok()
}

fn at_speed(p: &Part) -> Option<(i128, i128)> {
    if p.speed == 1.0 {
        None
    } else {
        speed_ratio(p.speed)
    }
}

/// A part's speed ramp (decision 0033) clipped to its own source range, in flicks from
/// `src_in` on both clocks: the part plays at `a` for `[0, l0)` of its local time (source
/// `[0, r0)`), eases to `b` along `curve` over `[l0, l0 + dl)` (source `[r0, r1)`), then
/// plays at `b`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct SpeedProfile {
    pub a: f64,
    pub b: f64,
    pub r0: f64,
    pub r1: f64,
    pub l0: f64,
    pub dl: f64,
    pub curve: crate::entities::Curve,
}

impl SpeedProfile {
    /// Source flicks from `src_in` at local time `t` (flicks from the part's start).
    pub fn src_at(&self, t: f64) -> f64 {
        if t <= self.l0 {
            self.a * t
        } else if t < self.l0 + self.dl {
            let x = t - self.l0;
            self.r0 + self.a * x + (self.b - self.a) * self.dl * self.curve.integral(x / self.dl)
        } else {
            self.r1 + self.b * (t - self.l0 - self.dl)
        }
    }

    /// Local time (flicks from the part's start) at source offset `d`: the inverse of
    /// [`SpeedProfile::src_at`] (bisection inside the ramp, where it is a cubic).
    pub fn local_at(&self, d: f64) -> f64 {
        if d <= self.r0 {
            d / self.a
        } else if d < self.r1 {
            let (mut lo, mut hi) = (self.l0, self.l0 + self.dl);
            for _ in 0..80 {
                let mid = 0.5 * (lo + hi);
                if self.src_at(mid) < d {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            0.5 * (lo + hi)
        } else {
            self.l0 + self.dl + (d - self.r1) / self.b
        }
    }

    /// The speed at local time `t`.
    pub fn speed_at(&self, t: f64) -> f64 {
        if t <= self.l0 {
            self.a
        } else if t < self.l0 + self.dl {
            self.a + (self.b - self.a) * self.curve.apply((t - self.l0) / self.dl)
        } else {
            self.b
        }
    }
}

/// A part's ramp as a [`SpeedProfile`], when it has one that changes the speed inside
/// its source range (decision 0033). A ramp wholly before `src_in` leaves the part at
/// `to` throughout, one wholly after it at `speed`; [`effective_speed`] says which.
pub fn speed_profile(p: &Part) -> Option<SpeedProfile> {
    let r = p.ramp?;
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if !ok(p.speed) || !ok(r.to) || r.to == p.speed {
        return None;
    }
    let len = p.src_out.0.saturating_sub(p.src_in.0) as f64;
    let r0 = ((r.src_start.0 - p.src_in.0) as f64).clamp(0.0, len);
    let r1 = ((r.src_end.0 - p.src_in.0) as f64).clamp(0.0, len);
    if r1 <= r0 {
        return None;
    }
    let (a, b) = (p.speed, r.to);
    // the ramp's average speed: a + (b − a)·∫curve
    let avg = a + (b - a) * r.curve.integral(1.0);
    Some(SpeedProfile {
        a,
        b,
        r0,
        r1,
        l0: r0 / a,
        dl: (r1 - r0) / avg,
        curve: r.curve,
    })
}

/// The constant speed a part plays at when it has no ramp inside its source range: its
/// `speed`, or the ramp's `to` when the whole ramp lies before `src_in` (decision 0033).
pub fn effective_speed(p: &Part) -> f64 {
    match p.ramp {
        Some(r) if speed_profile(p).is_none() && r.src_end <= p.src_in && r.to > 0.0 => r.to,
        _ => p.speed,
    }
}

/// How a split at source time `at` divides a part's ramp (decision 0033): the
/// `(speed, ramp)` of the side before `at` and of the side after it. A split before the
/// ramp leaves it to the second side, one after it to the first (the second then plays
/// at `to`); one inside it gives each side its piece, meeting at the speed the ramp has
/// there (to the hundredth), with the same curve.
pub fn split_ramp(
    p: &Part,
    at: Flicks,
) -> (
    (f64, Option<crate::entities::SpeedRamp>),
    (f64, Option<crate::entities::SpeedRamp>),
) {
    let Some(r) = p.ramp else {
        return ((p.speed, None), (p.speed, None));
    };
    if at <= r.src_start {
        return ((p.speed, None), (p.speed, Some(r)));
    }
    if at >= r.src_end {
        return ((p.speed, Some(r)), (r.to, None));
    }
    // the ramp's own profile, unclipped
    let whole = Part {
        src_in: r.src_start,
        src_out: r.src_end,
        ..p.clone()
    };
    let mid = speed_profile(&whole).map_or(p.speed, |sp| {
        let t = sp.local_at((at.0 - r.src_start.0) as f64);
        (sp.speed_at(t) * 100.0).round() / 100.0
    });
    let piece = |from: f64, to: f64, a: Flicks, b: Flicks| {
        (to != from).then_some(crate::entities::SpeedRamp {
            to,
            src_start: a,
            src_end: b,
            curve: r.curve,
        })
    };
    (
        (p.speed, piece(p.speed, mid, r.src_start, at)),
        (mid, piece(mid, r.to, at, r.src_end)),
    )
}

fn constant_ratio(p: &Part) -> Option<(i128, i128)> {
    let s = effective_speed(p);
    if s == 1.0 { None } else { speed_ratio(s) }
}

/// A part's length on the `cut` clock: `(src_out − src_in) / speed`, quantised by the
/// one tie rule; with a ramp, its profile's local length, rounded (decision 0033).
pub fn part_duration(p: &Part) -> Flicks {
    let d = p.src_out.0.saturating_sub(p.src_in.0);
    if let Some(sp) = speed_profile(p) {
        return Flicks(sp.local_at(d as f64).round() as i64);
    }
    if p.ramp.is_some() {
        return match constant_ratio(p) {
            Some((num, den)) => Flicks(mul_div_ties_earlier(d, den, num).unwrap_or(d)),
            None => Flicks(d),
        };
    }
    match at_speed(p) {
        Some((num, den)) => Flicks(mul_div_ties_earlier(d, den, num).unwrap_or(d)),
        None => Flicks(d),
    }
}

/// A source offset `d` (flicks from `src_in`) on the part's local clock: `d / speed` by
/// the one tie rule, or through its ramp (decision 0033).
pub fn src_to_local(p: &Part, d: Flicks) -> Flicks {
    if let Some(sp) = speed_profile(p) {
        return Flicks(sp.local_at(d.0 as f64).round() as i64);
    }
    let r = if p.ramp.is_some() {
        constant_ratio(p)
    } else {
        at_speed(p)
    };
    match r {
        Some((num, den)) => Flicks(mul_div_ties_earlier(d.0, den, num).unwrap_or(d.0)),
        None => d,
    }
}

/// One part's place on the `cut` clock.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct PartSpan {
    pub part: Id<Part>,
    pub start: Flicks,
    pub dur: Flicks,
}

/// The parts of a cut end to end, in `OrderKey` order.
pub fn cut_layout(doc: &Doc, cut: Id<Cut>) -> Vec<PartSpan> {
    let mut t = 0i64;
    doc.children::<Part>(cut.entity())
        .filter_map(|id| {
            let p = doc.get(id)?;
            let dur = part_duration(p);
            let span = PartSpan {
                part: id,
                start: Flicks(t),
                dur,
            };
            t = t.saturating_add(dur.0);
            Some(span)
        })
        .collect()
}

/// `cut` → (part, `src`). Parts are half-open `[start, start + dur)`; the cut's end
/// maps to the last part's `src_out`.
pub fn cut_to_src(doc: &Doc, cut: Id<Cut>, t: Flicks) -> Option<(Id<Part>, Flicks)> {
    let layout = cut_layout(doc, cut);
    let last = *layout.last()?;
    let span = layout
        .iter()
        .find(|s| t.0 >= s.start.0 && t.0 < s.start.0 + s.dur.0)
        .copied()
        .or_else(|| (t.0 == last.start.0 + last.dur.0).then_some(last))?;
    let p = doc.get(span.part)?;
    Some((span.part, local_to_src(p, Flicks(t.0 - span.start.0))))
}

/// `local` (from the part's start, on the cut clock) → `src`.
pub fn local_to_src(p: &Part, t_local: Flicks) -> Flicks {
    if let Some(sp) = speed_profile(p) {
        let l = sp.src_at(t_local.0 as f64).round() as i64;
        return Flicks(p.src_in.0.saturating_add(l));
    }
    let r = if p.ramp.is_some() {
        constant_ratio(p)
    } else {
        at_speed(p)
    };
    let l = match r {
        Some((num, den)) => mul_div_ties_earlier(t_local.0, num, den).unwrap_or(t_local.0),
        None => t_local.0,
    };
    Flicks(p.src_in.0.saturating_add(l))
}

/// `src` → `cut` for a time inside `part`'s source range.
pub fn src_to_cut(doc: &Doc, part: Id<Part>, t_src: Flicks) -> Option<Flicks> {
    let p = doc.get(part)?;
    if t_src < p.src_in || t_src > p.src_out {
        return None;
    }
    let span = cut_layout(doc, p.cut)
        .into_iter()
        .find(|s| s.part == part)?;
    let local = src_to_local(p, Flicks(t_src.0 - p.src_in.0)).0;
    Some(Flicks(span.start.0 + local))
}

/// A [`ModelTime`] as a source time, where that needs no analysis data.
pub fn time_to_src(doc: &Doc, t: &ModelTime) -> Option<Flicks> {
    match t {
        ModelTime::Src { t, .. } => Some(*t),
        ModelTime::Cut { cut, t } => cut_to_src(doc, *cut, *t).map(|(_, s)| s),
        ModelTime::Local {
            part: Some(p), t, ..
        } => doc.get(*p).map(|p| local_to_src(p, *t)),
        ModelTime::Local { .. } | ModelTime::Out { .. } => None,
    }
}

/// Resolves an anchor to a source time without analysis data: word fingerprints,
/// part and cut edges, raw times. Lines, measurements, visual events and frames need
/// the analysis view (gmclip-core's resolver, A15) and return `None`.
pub fn anchor_src(doc: &Doc, a: &Anchor) -> Option<Flicks> {
    match a {
        Anchor::WordStart(w) | Anchor::AudibleStart(w) => Some(w.t_src_start),
        Anchor::WordEnd(w) | Anchor::AudibleEnd(w) => Some(w.t_src_end),
        Anchor::CutPoint(c) => Some(match c.side {
            CutSide::Before => c.word.t_src_start,
            CutSide::After => c.word.t_src_end,
        }),
        Anchor::PartStart(e) => {
            let p = doc.get(e.part)?;
            Some(Flicks(
                p.src_in.0 + ms_to_flicks(e.offset_ms.unwrap_or(0)).0,
            ))
        }
        Anchor::PartEnd(e) => {
            let p = doc.get(e.part)?;
            Some(Flicks(
                p.src_out.0 + ms_to_flicks(e.offset_ms.unwrap_or(0)).0,
            ))
        }
        Anchor::CutStart(e) => {
            cut_to_src(doc, e.cut, ms_to_flicks(e.offset_ms.unwrap_or(0))).map(|(_, s)| s)
        }
        Anchor::CutEnd(e) => {
            let total: i64 = cut_layout(doc, e.cut).iter().map(|s| s.dur.0).sum();
            cut_to_src(
                doc,
                e.cut,
                Flicks(total + ms_to_flicks(e.offset_ms.unwrap_or(0)).0),
            )
            .map(|(_, s)| s)
        }
        Anchor::Raw(r) => time_to_src(doc, &r.time),
        Anchor::LineStart(_)
        | Anchor::LineEnd(_)
        | Anchor::Measured(_)
        | Anchor::VisualEvent(_)
        | Anchor::Frame(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::order::OrderKey;

    #[test]
    fn cut_clock_lays_parts_end_to_end() {
        let mut d = Doc::new();
        let c = cut(1);
        d.insert_for_test(c.clone());
        let ks = OrderKey::sequence(None, 2).unwrap();
        d.insert_for_test(part(2, c.id, ks[0].clone(), 1000, 1500));
        d.insert_for_test(part(3, c.id, ks[1].clone(), 100, 300));
        let l = cut_layout(&d, c.id);
        assert_eq!(l[1].start, Flicks(500));
        assert_eq!(cut_to_src(&d, c.id, Flicks(499)).unwrap().1, Flicks(1499));
        assert_eq!(cut_to_src(&d, c.id, Flicks(500)).unwrap().1, Flicks(100));
        assert_eq!(cut_to_src(&d, c.id, Flicks(700)).unwrap().1, Flicks(300));
        assert_eq!(cut_to_src(&d, c.id, Flicks(701)), None);
        assert_eq!(
            src_to_cut(&d, Id::new(crate::Ulid(3)), Flicks(150)),
            Some(Flicks(550))
        );
        assert_eq!(flicks_to_exact_ms(ms_to_flicks(-40)), Some(-40));
        assert_eq!(flicks_to_exact_ms(Flicks(1)), None);
    }

    #[test]
    fn speed_scaling_is_exact_with_ties_to_the_earlier() {
        assert_eq!(speed_ratio(1.5), Some((3 << 51, 1 << 52)));
        assert_eq!(speed_ratio(0.0), None);
        assert_eq!(speed_ratio(f64::NAN), None);
        // 3 / 2 = 1.5 → 1 (tie, earlier); 5 / 2 = 2.5 → 2; −3 / 2 = −1.5 → −2
        assert_eq!(mul_div_ties_earlier(3, 1, 2), Some(1));
        assert_eq!(mul_div_ties_earlier(5, 1, 2), Some(2));
        assert_eq!(mul_div_ties_earlier(-3, 1, 2), Some(-2));
        assert_eq!(mul_div_ties_earlier(7, 1, 2), Some(3));
        assert_eq!(mul_div_ties_earlier(8, 1, 3), Some(3));
        // a part of 3 flicks at speed 2 lasts 1.5 → 1 flick (f64::round would give 2)
        let c = cut(1);
        let mut p = part(2, c.id, OrderKey::first(), 0, 3);
        p.speed = 2.0;
        assert_eq!(part_duration(&p), Flicks(1));
        p.speed = 0.5;
        assert_eq!(part_duration(&p), Flicks(6));
        assert_eq!(local_to_src(&p, Flicks(3)), Flicks(1));
    }

    /// Decision 0033: a ramp from 1× to 3× over source [100, 300) of a 500-flick part —
    /// 100 at 1×, 200 at an average 2× (100 local), 200 at 3× (66.7 local); the maps invert
    /// each other, and a ramp outside the part leaves it at one speed.
    #[test]
    fn a_ramp_maps_local_and_source_through_its_integral() {
        use crate::entities::{Curve, SpeedRamp};
        let mut d = Doc::new();
        let c = cut(1);
        d.insert_for_test(c.clone());
        let ks = OrderKey::sequence(None, 2).unwrap();
        let mut p = part(2, c.id, ks[0].clone(), 1000, 1500);
        p.ramp = Some(SpeedRamp {
            to: 3.0,
            src_start: Flicks(1100),
            src_end: Flicks(1300),
            curve: Curve::Linear,
        });
        assert_eq!(part_duration(&p), Flicks(267));
        assert_eq!(local_to_src(&p, Flicks(50)), Flicks(1050));
        // 50 local into the ramp: 50 + 2·50²/(2·100) = 75 source
        assert_eq!(local_to_src(&p, Flicks(150)), Flicks(1175));
        assert_eq!(local_to_src(&p, Flicks(200)), Flicks(1300));
        assert_eq!(local_to_src(&p, Flicks(250)), Flicks(1450));
        for t in [0, 37, 100, 133, 199, 230, 266] {
            let s = local_to_src(&p, Flicks(t));
            assert!(
                (src_to_local(&p, Flicks(s.0 - 1000)).0 - t).abs() <= 1,
                "{t}"
            );
        }
        let prof = speed_profile(&p).unwrap();
        assert_eq!(prof.speed_at(150.0), 2.0);
        // laid on the cut clock through the ramp
        d.insert_for_test(p.clone());
        d.insert_for_test(part(3, c.id, ks[1].clone(), 100, 300));
        assert_eq!(cut_layout(&d, c.id)[1].start, Flicks(267));
        assert_eq!(src_to_cut(&d, p.id, Flicks(1175)), Some(Flicks(150)));
        // wholly before the part: it plays at `to`; wholly after: at `speed`
        let mut q = p.clone();
        q.ramp = Some(SpeedRamp {
            to: 2.0,
            src_start: Flicks(0),
            src_end: Flicks(900),
            curve: Curve::EaseInOut,
        });
        assert_eq!(part_duration(&q), Flicks(250));
        q.ramp = Some(SpeedRamp {
            to: 2.0,
            src_start: Flicks(1600),
            src_end: Flicks(1900),
            curve: Curve::EaseIn,
        });
        assert_eq!(part_duration(&q), Flicks(500));
        // a split inside the ramp gives each side its piece, meeting at the speed there
        // (100 source into it is 61.8 local, at 1 + 2 · 0.618 = 2.24×)
        let ((sa, ra), (sb, rb)) = split_ramp(&p, Flicks(1200));
        assert_eq!((sa, sb), (1.0, 2.24));
        assert_eq!(ra.map(|r| (r.to, r.src_end)), Some((2.24, Flicks(1200))));
        assert_eq!(rb.map(|r| (r.to, r.src_start)), Some((3.0, Flicks(1200))));
        // before it, after it
        assert_eq!(split_ramp(&p, Flicks(1050)).0, (1.0, None));
        assert_eq!(split_ramp(&p, Flicks(1400)).1, (3.0, None));
        // every curve's integral reaches the ramp's average speed and is continuous at ½
        assert_eq!(Curve::EaseIn.integral(1.0), 1.0 / 3.0);
        assert!((Curve::EaseOut.integral(1.0) - 2.0 / 3.0).abs() < 1e-15);
        assert!((Curve::EaseInOut.integral(0.5) - 1.0 / 12.0).abs() < 1e-15);
        assert!((Curve::EaseInOut.integral(0.5 + 1e-12) - 1.0 / 12.0).abs() < 1e-11);
    }
}
