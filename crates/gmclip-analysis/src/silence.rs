//! Silence: the per-span threshold, the 40 ms quiet grid, pauses (SPEC §5.5).
//!
//! The threshold is measured off the span, never picked: across AJ's streams the noise
//! floor spans ~35 dB while speech stays near −19 dBFS, so a fixed number either cuts
//! breaths in a quiet room or finds nothing over a fan.
//!
//! `threshold = clamp(max(floor + 8, speech − 28), −55, −22)` dB, where `floor` and
//! `speech` are percentiles of the span's **short-term level**, the 40 ms RMS frames
//! the quiet grid is built from: `floor` = 5th percentile, `speech` = 95th
//! (nearest rank). The percentiles are this crate's reading of §5.5's "short-term
//! level percentiles"; the reference script used ffmpeg `astats` noise floor / RMS peak.

use crate::AnalysisError;
use crate::energy::{Energy, Hop};
use crate::span::Span;

pub const OVER_FLOOR_DB: f32 = 8.0;
pub const UNDER_SPEECH_DB: f32 = 28.0;
pub const THRESHOLD_RANGE_DB: (f32, f32) = (-55.0, -22.0);
pub const FLOOR_PERCENTILE: u32 = 5;
pub const SPEECH_PERCENTILE: u32 = 95;

/// The two levels the threshold is read from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Levels {
    pub floor_db: f32,
    pub speech_db: f32,
}

/// Nearest-rank percentile `p` (0–100) of `v` (sorted in place), in integer
/// arithmetic. `None` when empty.
pub fn percentile(v: &mut [f32], p: u32) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let rank = (u64::from(p.min(100)) * v.len() as u64).div_ceil(100) as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

/// Floor and speech levels of the 40 ms frames inside `span`.
pub fn levels(grid_energy: &Energy, span: Span) -> Option<Levels> {
    let r = grid_energy.frames_in(span);
    let mut v: Vec<f32> = grid_energy.db.get(r)?.to_vec();
    let floor_db = percentile(&mut v, FLOOR_PERCENTILE)?;
    let speech_db = percentile(&mut v, SPEECH_PERCENTILE)?;
    Some(Levels {
        floor_db,
        speech_db,
    })
}

/// §5.5's rule.
pub fn auto_threshold(l: Levels) -> f32 {
    (l.floor_db + OVER_FLOOR_DB)
        .max(l.speech_db - UNDER_SPEECH_DB)
        .clamp(THRESHOLD_RANGE_DB.0, THRESHOLD_RANGE_DB.1)
}

/// 40 ms cells, each quiet when its RMS level is below the span's threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct QuietGrid {
    pub rate: u32,
    /// Cell length in samples (40 ms).
    pub cell: usize,
    /// Absolute index of the first cell (the span may start mid-buffer).
    pub first_cell: usize,
    pub threshold_db: f32,
    pub levels: Levels,
    pub quiet: Vec<bool>,
}

impl QuietGrid {
    /// The grid over `span`, its threshold measured off `span`. `grid_energy` must be
    /// the 40 ms kind ([`Energy::at_hop`] with [`Hop::Ms40`]).
    pub fn for_span(grid_energy: &Energy, span: Span) -> Result<Self, AnalysisError> {
        let cell = grid_energy.hop;
        if grid_energy.window != cell || cell != crate::span::ms(Hop::Ms40.ms(), grid_energy.rate) {
            return Err(AnalysisError::Invalid("quiet grid needs 40 ms frames"));
        }
        let levels = levels(grid_energy, span).ok_or(AnalysisError::Invalid("empty span"))?;
        let threshold_db = auto_threshold(levels);
        Ok(Self::with_threshold(
            grid_energy,
            span,
            threshold_db,
            levels,
        ))
    }

    /// The grid over `span` at a given threshold.
    pub fn with_threshold(
        grid_energy: &Energy,
        span: Span,
        threshold_db: f32,
        levels: Levels,
    ) -> Self {
        let r = grid_energy.frames_in(span);
        let first_cell = r.start;
        let quiet = grid_energy.db[r]
            .iter()
            .map(|&d| d < threshold_db)
            .collect();
        Self {
            rate: grid_energy.rate,
            cell: grid_energy.hop,
            first_cell,
            threshold_db,
            levels,
            quiet,
        }
    }

    /// Runs of quiet cells as sample spans (every gap ≥ one cell, 40 ms).
    pub fn regions(&self) -> Vec<Span> {
        let mut out = Vec::new();
        let mut open: Option<usize> = None;
        for (i, &q) in self.quiet.iter().enumerate() {
            match (q, open) {
                (true, None) => open = Some(i),
                (false, Some(s)) => {
                    out.push(self.span_of(s, i));
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(s) = open {
            out.push(self.span_of(s, self.quiet.len()));
        }
        out
    }

    fn span_of(&self, a: usize, b: usize) -> Span {
        Span::new(
            (self.first_cell + a) * self.cell,
            (self.first_cell + b) * self.cell,
        )
    }

    /// Quiet regions lasting at least `min_len` samples: the pauses a sweep previews.
    pub fn pauses(&self, min_len: usize) -> Vec<Span> {
        self.regions()
            .into_iter()
            .filter(|s| s.len() >= min_len)
            .collect()
    }

    /// Runs of sound between the quiet regions, clipped to `span`.
    pub fn sound_runs(&self, span: Span) -> Vec<Span> {
        let mut out = Vec::new();
        let mut at = span.start;
        for q in self.regions() {
            if q.start > at {
                out.push(Span::new(at, q.start.min(span.end)));
            }
            at = at.max(q.end);
        }
        if at < span.end {
            out.push(Span::new(at, span.end));
        }
        out.retain(|s| !s.is_empty());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::ms;

    /// Speech-like bursts at −20 dBFS RMS over a −60 dBFS floor.
    fn bursts(rate: u32) -> (Vec<f32>, Vec<Span>) {
        let mut x = vec![0.0f32; rate as usize * 4];
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let floor = 10f32.powf(-60.0 / 20.0) * 3f32.sqrt();
        for v in x.iter_mut() {
            *v = noise() * floor;
        }
        let speech = 10f32.powf(-20.0 / 20.0) * 2f32.sqrt();
        let words = [(0.20, 0.90), (1.00, 1.60), (2.40, 3.20)];
        for &(a, b) in &words {
            let (a, b) = ((a * rate as f32) as usize, (b * rate as f32) as usize);
            for (i, v) in x[a..b].iter_mut().enumerate() {
                *v = speech * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / rate as f32).sin();
            }
        }
        let gaps = vec![
            Span::new(0, ms(200, rate)),
            Span::new(ms(900, rate), ms(1000, rate)),
            Span::new(ms(1600, rate), ms(2400, rate)),
            Span::new(ms(3200, rate), ms(4000, rate)),
        ];
        (x, gaps)
    }

    #[test]
    fn threshold_rule() {
        let l = Levels {
            floor_db: -70.0,
            speech_db: -19.0,
        };
        assert_eq!(auto_threshold(l), -47.0);
        let fan = Levels {
            floor_db: -45.0,
            speech_db: -19.0,
        };
        assert_eq!(auto_threshold(fan), -37.0);
        let loud = Levels {
            floor_db: -20.0,
            speech_db: -5.0,
        };
        assert_eq!(auto_threshold(loud), -22.0);
        let dead = Levels {
            floor_db: -120.0,
            speech_db: -100.0,
        };
        assert_eq!(auto_threshold(dead), -55.0);
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let mut v: Vec<f32> = (1..=20).map(|i| i as f32).collect();
        assert_eq!(percentile(&mut v, 5), Some(1.0));
        assert_eq!(percentile(&mut v, 95), Some(19.0));
        assert_eq!(percentile(&mut v, 100), Some(20.0));
        assert_eq!(percentile(&mut [], 50), None);
        let mut big: Vec<f32> = (0..1_000_000).map(|i| i as f32).collect();
        assert_eq!(percentile(&mut big, 95), Some(949_999.0));
    }

    #[test]
    fn grid_finds_the_gaps() {
        let rate = 16_000;
        let (x, gaps) = bursts(rate);
        let e = Energy::at_hop(&x, rate, Hop::Ms40).unwrap();
        let g = QuietGrid::for_span(&e, Span::new(0, x.len())).unwrap();
        assert!((g.levels.speech_db + 20.0).abs() < 0.5, "{:?}", g.levels);
        assert!((g.levels.floor_db + 60.0).abs() < 1.5, "{:?}", g.levels);
        assert_eq!(g.threshold_db, auto_threshold(g.levels));
        assert!((g.threshold_db + 48.0).abs() < 0.6, "{}", g.threshold_db);
        let r = g.regions();
        assert_eq!(r.len(), gaps.len(), "{r:?}");
        for (got, want) in r.iter().zip(&gaps) {
            // gaps sit on 40 ms cells; edges within one cell
            assert!(
                got.start.abs_diff(want.start) <= ms(40, rate),
                "{got:?} vs {want:?}"
            );
            assert!(
                got.end.abs_diff(want.end) <= ms(40, rate),
                "{got:?} vs {want:?}"
            );
        }
        let p = g.pauses(ms(400, rate));
        assert_eq!(p.len(), 2);
        let runs = g.sound_runs(Span::new(0, x.len()));
        assert_eq!(runs.len(), 3);
    }

    #[test]
    fn grid_needs_40_ms_frames() {
        let e = Energy::at_hop(&[0.0; 16_000], 16_000, Hop::Ms10).unwrap();
        assert!(QuietGrid::for_span(&e, Span::new(0, 16_000)).is_err());
    }
}
