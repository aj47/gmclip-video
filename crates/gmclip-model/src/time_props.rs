//! Property tests for [`super::rescale`] and [`super::frame_at`] (A-10; SPEC §4.1.1:
//! "property-tested over timescales {1, 600, 1000, 10000, 12288, 15360, 30000, 90000,
//! 1e6, 1e9} and 30-day durations").
//!
//! Generated cases from a seeded splitmix64 stream rather than a property-test crate
//! (no new dependency, SR R-SUP-1); every run checks the same cases, and a failure prints
//! the inputs. Each property is checked against its *definition* in exact i128
//! arithmetic, not against a second implementation of the same formula.

use super::*;

/// SPEC §4.1.1's timescales.
const TIMESCALES: [u32; 10] = [
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
];

const THIRTY_DAYS_S: i64 = 30 * 24 * 3600;

/// Cases per property per timescale pair.
const CASES: usize = 400;

/// Cases per timescale pair in the main property (100 pairs: 1e6 in all, the M1
/// backlog's A01 count).
const MAIN_CASES: usize = 10_000;

/// splitmix64: a tiny, well-mixed, seedable generator.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[-max, max]`, with the edges and small values over-represented.
    fn signed(&mut self, max: i64) -> i64 {
        let r = self.next();
        let mag = match r % 8 {
            0 => max,
            1 => (self.next() % 4) as i64,
            2 => (self.next() % 1_000) as i64,
            _ => (self.next() % (max as u64 + 1)) as i64,
        };
        if r & (1 << 40) != 0 { -mag } else { mag }
    }
}

/// `v` ticks of a `from`-Hz clock in ticks of a `to`-Hz clock: the exact `v·to/from`.
fn exact(v: i64, from: u32, to: u32) -> (i128, i128) {
    (i128::from(v) * i128::from(to), i128::from(from))
}

fn check_definitions(v: i64, from: u32, to: u32) {
    let (n, d) = exact(v, from, to);
    let (a, b) = (Rational::tick(from), Rational::tick(to));
    let ctx = format!("v={v} from={from} to={to}");
    let fl = i128::from(rescale(v, a, b, Rounding::Floor));
    let ce = i128::from(rescale(v, a, b, Rounding::Ceil));
    let ne = i128::from(rescale(v, a, b, Rounding::NearestTiesEarlier));
    // floor: q·d ≤ n < (q+1)·d
    assert!(fl * d <= n && n < (fl + 1) * d, "floor {ctx}");
    // ceil: (q−1)·d < n ≤ q·d
    assert!((ce - 1) * d < n && n <= ce * d, "ceil {ctx}");
    // nearest, ties to the earlier: −d < 2(n − q·d) ≤ d
    let twice = 2 * (n - ne * d);
    assert!(-d < twice && twice <= d, "nearest {ctx}");
    assert!(ce - fl <= 1 && (fl == ne || ce == ne), "ordering {ctx}");
    // floor(−x) = −ceil(x)
    assert_eq!(
        i128::from(rescale(-v, a, b, Rounding::Floor)),
        -ce,
        "symmetry {ctx}"
    );
}

#[test]
fn rescale_meets_its_definitions_for_every_timescale_pair() {
    let mut g = Gen(0x4131_3021_7469_6d65);
    for &from in &TIMESCALES {
        let max = THIRTY_DAYS_S * i64::from(from);
        for &to in &TIMESCALES {
            for _ in 0..MAIN_CASES {
                check_definitions(g.signed(max), from, to);
            }
            for v in [0, 1, -1, max, -max] {
                check_definitions(v, from, to);
            }
        }
    }
}

#[test]
fn rescale_to_and_from_flicks_meets_its_definitions() {
    let mut g = Gen(7);
    let flicks = Flicks::UNIT;
    for &ts in &TIMESCALES {
        let tick = Rational::tick(ts);
        for _ in 0..CASES {
            let v = g.signed(THIRTY_DAYS_S * i64::from(ts));
            // ticks → flicks: v · 705 600 000 / ts
            let f = i128::from(rescale(v, tick, flicks, Rounding::Floor));
            let n = i128::from(v) * i128::from(FLICKS_PER_SECOND);
            let d = i128::from(ts);
            assert!(f * d <= n && n < (f + 1) * d, "v={v} ts={ts}");
            // flicks → ticks
            let fv = g.signed(THIRTY_DAYS_S * FLICKS_PER_SECOND);
            let t = i128::from(rescale(fv, flicks, tick, Rounding::NearestTiesEarlier));
            let n = i128::from(fv) * d;
            let d2 = i128::from(FLICKS_PER_SECOND);
            let twice = 2 * (n - t * d2);
            assert!(-d2 < twice && twice <= d2, "fv={fv} ts={ts}");
        }
    }
}

/// Exact ties on the flicks → ticks path: `k·F + F/2` flicks at 1 Hz is half way
/// between seconds k and k+1, and must give k; one flick either side moves off the tie.
#[test]
fn flicks_to_ticks_ties_go_to_the_earlier_tick() {
    let mut g = Gen(23);
    let (f, sec) = (FLICKS_PER_SECOND, Rational::tick(1));
    for _ in 0..CASES {
        let k = g.signed(THIRTY_DAYS_S);
        let tie = k * f + f / 2;
        let at = |t| rescale(t, Flicks::UNIT, sec, Rounding::NearestTiesEarlier);
        assert_eq!(at(tie), k, "k={k}");
        assert_eq!(at(tie + 1), k + 1, "k={k}");
        assert_eq!(at(tie - 1), k, "k={k}");
    }
}

/// Ticks → flicks → ticks is the identity whenever a flick is no coarser than a tick
/// (every listed timescale but 1e9), however the ticks fall between flicks.
#[test]
fn ticks_round_trip_through_flicks() {
    let mut g = Gen(11);
    for &ts in TIMESCALES
        .iter()
        .filter(|&&t| i64::from(t) <= FLICKS_PER_SECOND)
    {
        let tick = Rational::tick(ts);
        for _ in 0..CASES {
            let v = g.signed(THIRTY_DAYS_S * i64::from(ts));
            let f = rescale(v, tick, Flicks::UNIT, Rounding::NearestTiesEarlier);
            let back = rescale(f, Flicks::UNIT, tick, Rounding::NearestTiesEarlier);
            assert_eq!(back, v, "ts={ts}");
        }
    }
    // 1e9 is finer than a flick: the round trip lands within half a flick.
    let tick = Rational::tick(1_000_000_000);
    for _ in 0..CASES {
        let v = g.signed(THIRTY_DAYS_S * 1_000_000_000);
        let f = rescale(v, tick, Flicks::UNIT, Rounding::NearestTiesEarlier);
        let back = rescale(f, Flicks::UNIT, tick, Rounding::NearestTiesEarlier);
        // half a flick is 1e9 / 705.6e6 / 2 ≈ 0.71 ns, so at most 1 tick off
        assert!((back - v).abs() <= 1, "v={v} back={back}");
    }
}

#[test]
fn rescale_is_monotonic() {
    let mut g = Gen(13);
    for &from in &TIMESCALES {
        for &to in &TIMESCALES {
            let (a, b) = (Rational::tick(from), Rational::tick(to));
            for _ in 0..CASES / 4 {
                let max = THIRTY_DAYS_S * i64::from(from);
                let (x, y) = (g.signed(max), g.signed(max));
                let (lo, hi) = (x.min(y), x.max(y));
                for r in [
                    Rounding::Floor,
                    Rounding::Ceil,
                    Rounding::NearestTiesEarlier,
                ] {
                    assert!(
                        rescale(lo, a, b, r) <= rescale(hi, a, b, r),
                        "{lo} {hi} {from}->{to}"
                    );
                }
            }
        }
    }
}

/// `checked_rescale` never panics and never wraps, for any i64 (`i64::MIN` included),
/// any rounding and odd units (power-of-two numerators that put a product at
/// exactly i128::MIN): it returns the value its definition gives, or `None` only when
/// that value leaves i64 or `v` times the reduced numerator leaves i128.
#[test]
fn checked_rescale_never_overflows_silently() {
    let mut g = Gen(17);
    let units = [
        Flicks::UNIT,
        Rational::tick(1),
        Rational::tick(1_000_000_000),
        Rational::new(i64::MAX, 1),
        Rational::new(1, u32::MAX),
        Rational::new(1001, 30_000),
        Rational::new(1 << 33, 1),
        Rational::new(1, 1 << 31),
        Rational::new(1 << 33, 1 << 31),
        Rational::new(1 << 62, 1 << 31),
        Rational::new(77_158_673_929, 1),
        Rational::new(2, 119_537_721),
    ];
    for &a in &units {
        for &b in &units {
            for i in 0..CASES {
                let v = match (i, g.next() % 4) {
                    (0, _) => i64::MIN,
                    (1, _) => i64::MAX,
                    (2, _) => -i64::MAX,
                    (_, 0) => i64::MIN,
                    (_, 1) => i64::MAX,
                    _ => g.next() as i64,
                };
                for r in [
                    Rounding::Floor,
                    Rounding::Ceil,
                    Rounding::NearestTiesEarlier,
                ] {
                    let got = checked_rescale(v, a, b, r);
                    // Definition, by remainders, with the fraction reduced like the code
                    // must (else identical units would overflow).
                    let mut num = i128::from(a.num) * i128::from(b.den);
                    let mut den = i128::from(a.den) * i128::from(b.num);
                    let (mut x, mut y) = (num, den);
                    while y != 0 {
                        (x, y) = (y, x % y);
                    }
                    num /= x;
                    den /= x;
                    let Some(n) = i128::from(v).checked_mul(num) else {
                        assert_eq!(got, None, "{v} {a:?} {b:?} {r:?}");
                        continue;
                    };
                    let (q, rem) = (n.div_euclid(den), n.rem_euclid(den));
                    let want = match r {
                        Rounding::Floor => q,
                        Rounding::Ceil if rem == 0 => q,
                        Rounding::Ceil => q + 1,
                        Rounding::NearestTiesEarlier if 2 * rem <= den => q,
                        Rounding::NearestTiesEarlier => q + 1,
                    };
                    assert_eq!(got, i64::try_from(want).ok(), "{v} {a:?} {b:?} {r:?}");
                }
            }
        }
    }
    // The reviewer's cases, by value.
    let ceil = checked_rescale(
        i64::MIN,
        Rational::new(1 << 33, 1 << 31),
        Rational::new(1 << 62, 1 << 31),
        Rounding::Ceil,
    );
    assert_eq!(ceil, Some(-(1 << 34)));
    let u = Rational::new(1 << 62, u32::MAX);
    assert_eq!(
        checked_rescale(1 << 40, u, u, Rounding::Floor),
        Some(1 << 40)
    );
}

/// With real units (any listed timescale, flicks, any frame duration) every i64 input
/// whose result fits gets a result: the i128 intermediates never overflow.
#[test]
fn real_units_never_hit_the_intermediate_limit() {
    let mut units: Vec<Rational> = TIMESCALES.iter().map(|&t| Rational::tick(t)).collect();
    units.push(Flicks::UNIT);
    for (num, den) in [
        (1001_i64, 24_000_u32),
        (1001, 30_000),
        (1001, 60_000),
        (1, 24),
        (1, 60),
    ] {
        units.push(Rational::new(num, den));
    }
    for &a in &units {
        for &b in &units {
            for v in [i64::MAX, i64::MIN + 1, 1, -1, 0] {
                let n = i128::from(v) * i128::from(a.num) * i128::from(b.den);
                let d = i128::from(a.den) * i128::from(b.num);
                let want = i64::try_from(n.div_euclid(d)).ok();
                assert_eq!(
                    checked_rescale(v, a, b, Rounding::Floor),
                    want,
                    "{v} {a:?} {b:?}"
                );
            }
        }
    }
}

/// The tie rule, SPEC §4.1.1 (2): `k = ⌈t·F − ½⌉`. A time exactly between frames k and
/// k+1 is frame k; one flick later it is k+1; a frame's own start is that frame. Every
/// common rate has a whole number of flicks per frame, so these times are exact.
#[test]
fn frame_at_ties_go_to_the_earlier_frame_at_every_rate() {
    let mut g = Gen(19);
    let rates = [
        Rational::new(24, 1),
        Rational::new(25, 1),
        Rational::new(30, 1),
        Rational::new(48, 1),
        Rational::new(50, 1),
        Rational::new(60, 1),
        Rational::new(24_000, 1001),
        Rational::new(30_000, 1001),
        Rational::new(60_000, 1001),
    ];
    for fps in rates {
        let per_frame = i128::from(FLICKS_PER_SECOND) * i128::from(fps.den) / i128::from(fps.num);
        assert_eq!(
            per_frame * i128::from(fps.num),
            i128::from(FLICKS_PER_SECOND) * i128::from(fps.den),
            "{fps:?} has a whole number of flicks per frame"
        );
        assert_eq!(
            per_frame % 2,
            0,
            "{fps:?}: half a frame is a whole number of flicks"
        );
        let per_frame = i64::try_from(per_frame).unwrap();
        let max_k = THIRTY_DAYS_S * fps.num / i64::from(fps.den);
        for _ in 0..CASES {
            let k = g.signed(max_k);
            let start = k * per_frame;
            let mid = start + per_frame / 2;
            assert_eq!(
                frame_at(Flicks(start), fps),
                Some(k),
                "{fps:?} start of {k}"
            );
            assert_eq!(frame_at(Flicks(mid), fps), Some(k), "{fps:?} tie after {k}");
            assert_eq!(
                frame_at(Flicks(mid + 1), fps),
                Some(k + 1),
                "{fps:?} past tie"
            );
            assert_eq!(
                frame_at(Flicks(mid - 1), fps),
                Some(k),
                "{fps:?} before tie"
            );
            assert_eq!(
                frame_at(Flicks(start - per_frame / 2), fps),
                Some(k - 1),
                "{fps:?} tie before {k}"
            );
            // Same answer as the general rescale.
            let any = g.signed(THIRTY_DAYS_S * FLICKS_PER_SECOND);
            let frame = Rational::new(i64::from(fps.den), u32::try_from(fps.num).unwrap());
            assert_eq!(
                frame_at(Flicks(any), fps),
                Some(rescale(
                    any,
                    Flicks::UNIT,
                    frame,
                    Rounding::NearestTiesEarlier
                ))
            );
        }
    }
}
