//! RMS level frames.
//!
//! Frame *i* covers samples `[i·hop, i·hop + window)`; its level is
//! `20·log10(rms)` dBFS, floored at [`FLOOR_DB`] (digital silence). The stored
//! analysis kinds use `window == hop` at 10, 20 and 40 ms; word edges use 10 ms
//! windows at a 5 ms hop (SPEC §9).

use crate::AnalysisError;
use crate::span::Span;

/// Level of digital silence.
pub const FLOOR_DB: f32 = -120.0;

/// The analysis hops SPEC §9 stores.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Hop {
    Ms10,
    Ms20,
    Ms40,
}

impl Hop {
    pub const ALL: [Hop; 3] = [Hop::Ms10, Hop::Ms20, Hop::Ms40];

    pub const fn ms(self) -> u32 {
        match self {
            Hop::Ms10 => 10,
            Hop::Ms20 => 20,
            Hop::Ms40 => 40,
        }
    }
}

/// `m` ms in samples, refused unless exact (22.05 kHz would drift 8 s an hour at a
/// rounded 10 ms hop). Every rate gmclip analyses (16, 48, 44.1 kHz) is exact.
fn exact_ms(m: u32, rate: u32) -> Result<usize, AnalysisError> {
    let n = u64::from(m) * u64::from(rate);
    if n % 1000 != 0 {
        return Err(AnalysisError::Unsupported(
            "rate is not a whole number of samples per ms step",
        ));
    }
    Ok((n / 1000) as usize)
}

/// Level frames of one buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct Energy {
    pub rate: u32,
    pub window: usize,
    pub hop: usize,
    /// One level per frame, dBFS.
    pub db: Vec<f32>,
}

/// dBFS of a mean square.
pub fn ms_to_db(mean_square: f64) -> f32 {
    if mean_square <= 1e-12 {
        FLOOR_DB
    } else {
        ((10.0 * mean_square.log10()) as f32).max(FLOOR_DB)
    }
}

impl Energy {
    /// Frames over the whole buffer; the last frame may be partial (its mean is
    /// taken over the samples it has). A buffer shorter than one window gives one frame.
    pub fn compute(
        pcm: &[f32],
        rate: u32,
        window: usize,
        hop: usize,
    ) -> Result<Self, AnalysisError> {
        if window == 0 || hop == 0 || rate == 0 {
            return Err(AnalysisError::Invalid("zero window, hop or rate"));
        }
        if hop > window {
            return Err(AnalysisError::Invalid("hop longer than window"));
        }
        // Prefix sums of squares in f64: O(1) per frame whatever the overlap.
        let mut prefix = Vec::with_capacity(pcm.len() + 1);
        prefix.push(0.0f64);
        let mut acc = 0.0f64;
        for &v in pcm {
            acc += f64::from(v) * f64::from(v);
            prefix.push(acc);
        }
        let n = if pcm.is_empty() {
            0
        } else {
            pcm.len().saturating_sub(window).div_ceil(hop) + 1
        };
        let mut db = Vec::with_capacity(n);
        for i in 0..n {
            let a = i * hop;
            let b = (a + window).min(pcm.len());
            let sum = prefix[b] - prefix[a];
            db.push(ms_to_db(sum / (b - a) as f64));
        }
        Ok(Self {
            rate,
            window,
            hop,
            db,
        })
    }

    /// One of the stored kinds: `window == hop`.
    pub fn at_hop(pcm: &[f32], rate: u32, hop: Hop) -> Result<Self, AnalysisError> {
        let n = exact_ms(hop.ms(), rate)?;
        Self::compute(pcm, rate, n, n)
    }

    /// 10 ms windows at a 5 ms hop: the edge-finding resolution.
    pub fn fine(pcm: &[f32], rate: u32) -> Result<Self, AnalysisError> {
        Self::compute(pcm, rate, exact_ms(10, rate)?, exact_ms(5, rate)?)
    }

    pub fn len(&self) -> usize {
        self.db.len()
    }

    pub fn is_empty(&self) -> bool {
        self.db.is_empty()
    }

    /// First sample of frame `i`.
    pub fn frame_start(&self, i: usize) -> usize {
        i * self.hop
    }

    /// Centre sample of frame `i`.
    pub fn frame_centre(&self, i: usize) -> usize {
        i * self.hop + self.window / 2
    }

    /// Frames whose window lies inside `span` (at least one, if the span is non-empty
    /// and inside the buffer): `i` with `i·hop ≥ start` and `i·hop + window ≤ end`,
    /// widened to the frame containing `start` when the span is shorter than a window.
    pub fn frames_in(&self, span: Span) -> std::ops::Range<usize> {
        let first = span.start.div_ceil(self.hop);
        let last_excl = if span.end >= self.window {
            (span.end - self.window) / self.hop + 1
        } else {
            0
        };
        let first = first.min(self.db.len());
        let last_excl = last_excl.min(self.db.len());
        if last_excl > first {
            first..last_excl
        } else {
            let i = (span.start / self.hop).min(self.db.len().saturating_sub(1));
            if self.db.is_empty() { 0..0 } else { i..i + 1 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_sine_is_minus_3_db() {
        let rate = 16_000;
        let x: Vec<f32> = (0..rate)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate as f32).sin())
            .collect();
        for hop in Hop::ALL {
            let e = Energy::at_hop(&x, rate, hop).unwrap();
            assert_eq!(e.len(), (1000 / hop.ms()) as usize);
            for &d in &e.db {
                assert!((d + 3.0103).abs() < 0.01, "{hop:?}: {d}");
            }
        }
    }

    #[test]
    fn silence_is_the_floor_and_fine_frames_overlap() {
        let x = vec![0.0f32; 1600];
        let e = Energy::fine(&x, 16_000).unwrap();
        assert_eq!(e.window, 160);
        assert_eq!(e.hop, 80);
        assert_eq!(e.len(), 19);
        assert!(e.db.iter().all(|&d| d == FLOOR_DB));
        assert_eq!(e.frame_centre(2), 240);
    }

    #[test]
    fn frames_in_spans() {
        let e = Energy::compute(&vec![0.1; 1000], 1000, 10, 5).unwrap();
        assert_eq!(e.frames_in(Span::new(0, 100)), 0..19);
        assert_eq!(e.frames_in(Span::new(12, 30)), 3..5);
        assert_eq!(e.frames_in(Span::new(12, 14)), 2..3);
        assert!(Energy::compute(&[0.1], 1000, 0, 5).is_err());
        assert!(Energy::compute(&[0.1; 6], 1000, 5, 10).is_err());
        assert!(Energy::fine(&[0.1; 100], 22_050).is_err());
        assert!(Energy::at_hop(&[0.1; 100], 44_100, Hop::Ms10).is_ok());
        assert!(Energy::fine(&[0.1; 100], 48_000).is_ok());
    }
}
