//! Sample spans and their exact conversion to flicks.

use gmclip_model::{Flicks, Rational, Rounding, rescale};

/// A half-open range of samples `[start, end)` in one buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub const fn mid(&self) -> usize {
        self.start + self.len() / 2
    }
}

/// `n` samples at `rate` Hz as flicks (exact for every rate dividing 705 600 000,
/// i.e. 8, 16, 22.05, 24, 32, 44.1 and 48 kHz; nearest-ties-earlier otherwise).
pub fn samples_to_flicks(n: i64, rate: u32) -> Flicks {
    Flicks(rescale(
        n,
        Rational::tick(rate),
        Flicks::UNIT,
        Rounding::NearestTiesEarlier,
    ))
}

/// Flicks as a sample index at `rate` Hz, nearest sample (ties to the earlier).
pub fn flicks_to_samples(t: Flicks, rate: u32) -> i64 {
    rescale(
        t.0,
        Flicks::UNIT,
        Rational::tick(rate),
        Rounding::NearestTiesEarlier,
    )
}

/// Milliseconds as a sample count at `rate` (nearest).
pub const fn ms(ms: u32, rate: u32) -> usize {
    ((ms as u64 * rate as u64 + 500) / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_at_voice_rates() {
        assert_eq!(samples_to_flicks(16_000, 16_000), Flicks(705_600_000));
        assert_eq!(samples_to_flicks(1, 16_000), Flicks(44_100));
        assert_eq!(samples_to_flicks(1, 48_000), Flicks(14_700));
        assert_eq!(flicks_to_samples(Flicks(44_100 * 7), 16_000), 7);
        assert_eq!(ms(10, 16_000), 160);
        assert_eq!(ms(5, 16_000), 80);
    }
}
