//! Time (SPEC §4.1). Flicks are the model's one time unit: 705 600 000 per second.
//! Conversions go through [`rescale`], which works in i128 so no intermediate overflows.

use serde::{Deserialize, Serialize};

/// Flicks per second.
pub const FLICKS_PER_SECOND: i64 = 705_600_000;

/// A time or duration in flicks (1/705 600 000 s).
#[derive(
    Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Flicks(pub i64);

impl Flicks {
    pub const ZERO: Flicks = Flicks(0);

    /// Duration of one tick of the flick clock, as a rational number of seconds.
    pub const UNIT: Rational = Rational {
        num: 1,
        den: FLICKS_PER_SECOND as u32,
    };
}

/// A rational number `num / den`. As a tick size, `Rational { num: 1, den: 90_000 }`
/// is one 90 kHz tick; as a frame rate, `{ num: 30_000, den: 1_001 }` is 29.97 fps.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: i64, den: u32) -> Self {
        Self { num, den }
    }

    /// One tick of a `timescale`-Hz clock (a sample timestamp's native unit).
    pub const fn tick(timescale: u32) -> Self {
        Self {
            num: 1,
            den: timescale,
        }
    }
}

/// How [`rescale`] rounds a result that is not an integer.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Rounding {
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceil,
    /// Nearest, ties to the earlier (smaller) value: SPEC §4.1.1's one tie rule.
    NearestTiesEarlier,
}

/// Converts `v` units of `from` into units of `to`: `round(v · from / to)`.
///
/// `from` and `to` are unit sizes in seconds (see [`Rational::tick`]). Returns `None`
/// when `to` is zero or negative, `from` is negative, or the result does not fit in i64
/// (or, only for exotic units whose reduced `from.num·to.den` exceeds 2^64 — no timescale
/// or frame rate has one — when `v` times it overflows i128). It never panics or wraps.
pub fn checked_rescale(v: i64, from: Rational, to: Rational, r: Rounding) -> Option<i64> {
    if to.num <= 0 || to.den == 0 || from.den == 0 || from.num < 0 {
        return None;
    }
    // v · (from.num / from.den) / (to.num / to.den) = v · num / den, with
    // num = from.num · to.den and den = from.den · to.num (each < 2^95, so exact in i128),
    // reduced by their gcd before v multiplies in.
    let mut num = i128::from(from.num) * i128::from(to.den);
    let mut den = i128::from(from.den) * i128::from(to.num);
    let g = gcd(num, den);
    num /= g;
    den /= g;
    let n = i128::from(v).checked_mul(num)?;
    // q = ⌊n/den⌋ and 0 ≤ rem < den; no negation, so no i128::MIN edge.
    let (q, rem) = (n.div_euclid(den), n.rem_euclid(den));
    let q = match r {
        Rounding::Floor => q,
        Rounding::Ceil => q + i128::from(rem != 0),
        // nearest, ties to the earlier: up only when the remainder is past half
        // (2·rem < 2^96, no overflow).
        Rounding::NearestTiesEarlier => q + i128::from(2 * rem > den),
    };
    i64::try_from(q).ok()
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    // both ≥ 0 here and b > 0, so the result is ≥ 1
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// [`checked_rescale`] for callers that have already validated their units.
///
/// # Panics
/// On an invalid unit or an overflowing result (see [`checked_rescale`]).
pub fn rescale(v: i64, from: Rational, to: Rational, r: Rounding) -> i64 {
    checked_rescale(v, from, to, r).expect("rescale: invalid unit or result out of range")
}

/// Quantises a time on the `cut`/`out` clocks to an output frame index at `fps`:
/// `k = ⌈t·F − ½⌉` (nearest frame, ties to the earlier), SPEC §4.1.1 conversion (2).
pub fn frame_at(t: Flicks, fps: Rational) -> Option<i64> {
    // one frame lasts den/num seconds
    let frame = Rational {
        num: i64::from(fps.den),
        den: u32::try_from(fps.num).ok()?,
    };
    checked_rescale(t.0, Flicks::UNIT, frame, Rounding::NearestTiesEarlier)
}

#[cfg(test)]
#[path = "time_props.rs"]
mod props;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_to_flicks_and_back() {
        let one_s = rescale(
            90_000,
            Rational::tick(90_000),
            Flicks::UNIT,
            Rounding::Floor,
        );
        assert_eq!(one_s, FLICKS_PER_SECOND);
        let back = rescale(one_s, Flicks::UNIT, Rational::tick(48_000), Rounding::Floor);
        assert_eq!(back, 48_000);
    }

    #[test]
    fn rounding_modes() {
        // 1 tick of a 3 Hz clock in units of 1/2 s = 2/3
        let (f, t) = (Rational::tick(3), Rational::tick(2));
        assert_eq!(rescale(1, f, t, Rounding::Floor), 0);
        assert_eq!(rescale(1, f, t, Rounding::Ceil), 1);
        assert_eq!(rescale(1, f, t, Rounding::NearestTiesEarlier), 1);
        assert_eq!(rescale(-1, f, t, Rounding::Floor), -1);
        assert_eq!(rescale(-1, f, t, Rounding::Ceil), 0);
    }

    #[test]
    fn ties_go_to_the_earlier_frame() {
        let fps = Rational::new(30, 1);
        let half_frame = Flicks(FLICKS_PER_SECOND / 60);
        assert_eq!(frame_at(half_frame, fps), Some(0));
        assert_eq!(frame_at(Flicks(half_frame.0 + 1), fps), Some(1));
        assert_eq!(frame_at(Flicks(3 * half_frame.0), fps), Some(1));
        assert_eq!(frame_at(Flicks(-half_frame.0), fps), Some(-1));
    }

    #[test]
    fn thirty_days_at_every_timescale_does_not_overflow() {
        let thirty_days = 30 * 24 * 3600_i64;
        for ts in [
            1,
            600,
            1000,
            10_000,
            12_288,
            15_360,
            30_000,
            90_000,
            1_000_000,
            1_000_000_000,
        ] {
            let f = rescale(
                thirty_days * i64::from(ts),
                Rational::tick(ts),
                Flicks::UNIT,
                Rounding::Floor,
            );
            assert_eq!(f, thirty_days * FLICKS_PER_SECOND);
        }
    }

    #[test]
    fn invalid_units_are_none() {
        assert_eq!(
            checked_rescale(1, Flicks::UNIT, Rational::new(0, 1), Rounding::Floor),
            None
        );
        assert_eq!(
            checked_rescale(i64::MAX, Rational::new(2, 1), Flicks::UNIT, Rounding::Floor),
            None
        );
    }
}
