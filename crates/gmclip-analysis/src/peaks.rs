//! Waveform peaks at 100 Hz: per 10 ms bucket, the minimum and maximum sample.

use crate::AnalysisError;

/// Buckets per second.
pub const PEAKS_HZ: u32 = 100;

/// Min/max per bucket. Bucket *i* covers samples `[i·rate/100, (i+1)·rate/100)`,
/// computed with integer bucket edges so a 44.1 kHz buffer does not drift.
#[derive(Clone, Debug, PartialEq)]
pub struct Peaks {
    pub rate: u32,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl Peaks {
    pub fn compute(pcm: &[f32], rate: u32) -> Result<Self, AnalysisError> {
        if rate < PEAKS_HZ {
            return Err(AnalysisError::Invalid("rate below 100 Hz"));
        }
        let rate64 = u64::from(rate);
        let n = (pcm.len() as u64 * u64::from(PEAKS_HZ)).div_ceil(rate64) as usize;
        let mut min = Vec::with_capacity(n);
        let mut max = Vec::with_capacity(n);
        for i in 0..n as u64 {
            let a = (i * rate64 / u64::from(PEAKS_HZ)) as usize;
            let b = (((i + 1) * rate64 / u64::from(PEAKS_HZ)) as usize).min(pcm.len());
            let bucket = &pcm[a.min(b)..b];
            let (lo, hi) = bucket
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| {
                    (lo.min(v), hi.max(v))
                });
            if bucket.is_empty() {
                min.push(0.0);
                max.push(0.0);
            } else {
                min.push(lo);
                max.push(hi);
            }
        }
        Ok(Self { rate, min, max })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_10_ms() {
        let mut x = vec![0.0f32; 44_100];
        x[441 * 3 + 5] = 0.75;
        x[441 * 7] = -0.5;
        let p = Peaks::compute(&x, 44_100).unwrap();
        assert_eq!(p.min.len(), 100);
        assert_eq!(p.max[3], 0.75);
        assert_eq!(p.min[7], -0.5);
        assert_eq!(p.max[4], 0.0);
        // a partial last bucket still counts
        assert_eq!(Peaks::compute(&[0.1; 161], 16_000).unwrap().max.len(), 2);
    }
}
