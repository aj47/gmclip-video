//! PCM preparation for the voice track: down-mix and a rational polyphase resampler.
//!
//! ASR and VAD take 16 kHz mono (SPEC §9: "ASR always runs on the source's voice
//! track"). Sources arrive at 48 kHz (M0) or 44.1 kHz (M1, B01), so the resampler is a
//! general `L/M` polyphase windowed-sinc filter with **zero group delay**: output
//! sample *k* sits exactly at input time `k·M/L`, so word times measured on the 16 kHz
//! buffer map back to the source clock with no offset.

use crate::AnalysisError;

/// The rate ASR and VAD run at.
pub const VOICE_RATE: u32 = 16_000;

/// Averages interleaved channels to mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Result<Vec<f32>, AnalysisError> {
    if channels == 0 {
        return Err(AnalysisError::Invalid("zero channels"));
    }
    if !interleaved.len().is_multiple_of(channels) {
        return Err(AnalysisError::Invalid(
            "sample count is not a multiple of channels",
        ));
    }
    if channels == 1 {
        return Ok(interleaved.to_vec());
    }
    let inv = 1.0 / channels as f32;
    Ok(interleaved
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() * inv)
        .collect())
}

/// Down-mixes and resamples to [`VOICE_RATE`] mono.
pub fn voice_16k(
    interleaved: &[f32],
    rate: u32,
    channels: usize,
) -> Result<Vec<f32>, AnalysisError> {
    let mono = downmix(interleaved, channels)?;
    if rate == VOICE_RATE {
        return Ok(mono);
    }
    Ok(Resampler::new(rate, VOICE_RATE)?.process(&mono))
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Modified Bessel function of the first kind, order 0 (series; converges fast for
/// the β values a Kaiser window uses).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// A rational `from → to` resampler: upsample by `L`, low-pass, decimate by `M`,
/// evaluated polyphase (only the taps that meet non-zero input samples).
#[derive(Clone, Debug)]
pub struct Resampler {
    up: usize,
    down: usize,
    /// Half length of the prototype filter in input samples (taps per side per phase).
    half: usize,
    /// Prototype filter at the upsampled rate, length `2·half·L + 1`, centred.
    h: Vec<f32>,
}

impl Resampler {
    /// Stop-band attenuation ≈ 80 dB; pass band to 0.90 of the lower Nyquist.
    pub fn new(from: u32, to: u32) -> Result<Self, AnalysisError> {
        if from == 0 || to == 0 {
            return Err(AnalysisError::Invalid("zero sample rate"));
        }
        let g = gcd(u64::from(from), u64::from(to));
        let up = (u64::from(to) / g) as usize;
        let down = (u64::from(from) / g) as usize;
        if up > 4096 || down > 4096 {
            return Err(AnalysisError::Unsupported("resampling ratio too fine"));
        }
        // Design at the upsampled rate F = from·L. Cut-off midway through the
        // transition band [0.90, 1.0]·min(from, to)/2.
        let nyq = f64::from(from.min(to)) / 2.0;
        let pass = 0.90 * nyq;
        let cutoff = 0.5 * (pass + nyq);
        let fs_up = f64::from(from) * up as f64;
        let atten = 80.0_f64;
        let beta = 0.1102 * (atten - 8.7);
        // Kaiser length estimate, in upsampled-rate taps.
        let trans = (nyq - pass) / fs_up; // normalised (cycles/sample)
        let n_est = ((atten - 7.95) / (2.285 * 2.0 * std::f64::consts::PI * trans)).ceil();
        let half = ((n_est / 2.0) / up as f64).ceil().max(4.0) as usize;
        let len = 2 * half * up + 1;
        let centre = (half * up) as f64;
        let fc = cutoff / fs_up; // normalised cut-off
        let i0b = bessel_i0(beta);
        let mut h = Vec::with_capacity(len);
        for n in 0..len {
            let x = n as f64 - centre;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let r = x / centre;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            // ×L: zero-stuffing divides the signal's gain by L.
            h.push((sinc * w * up as f64) as f32);
        }
        Ok(Self { up, down, half, h })
    }

    /// Output length for `n` input samples: every `k` with `k·M/L < n`.
    pub fn output_len(&self, n: usize) -> usize {
        (n * self.up).div_ceil(self.down)
    }

    /// Resamples a whole buffer (zero-padded at both ends).
    pub fn process(&self, x: &[f32]) -> Vec<f32> {
        let (l, m) = (self.up, self.down);
        let c = self.half * l;
        let n_out = self.output_len(x.len());
        let mut out = Vec::with_capacity(n_out);
        let taps = 2 * self.half + 1;
        for k in 0..n_out {
            // y[k] = Σ_n h[n] · xup[k·M + c − n], xup[j·L] = x[j]
            let t = k * m + c;
            let p = t % l;
            let base = t / l; // input index for n = p
            let mut acc = 0.0f32;
            for j in 0..taps {
                let n = p + j * l;
                if n >= self.h.len() {
                    break;
                }
                let Some(xi) = base.checked_sub(j) else {
                    break;
                };
                if let Some(&v) = x.get(xi) {
                    acc += self.h[n] * v;
                }
            }
            out.push(acc);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn downmix_averages() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2).unwrap(), vec![0.5, 0.5]);
        assert!(downmix(&[1.0, 0.0, 0.5], 2).is_err());
        assert!(downmix(&[], 0).is_err());
    }

    #[test]
    fn passband_is_flat_and_in_phase() {
        for from in [48_000u32, 44_100] {
            let r = Resampler::new(from, VOICE_RATE).unwrap();
            for f in [100.0, 1_000.0, 3_000.0, 6_500.0, 7_000.0] {
                let n = from as usize; // 1 s
                let y = r.process(&sine(from, f, n));
                assert_eq!(y.len(), r.output_len(n));
                // compare with the ideal 16 kHz sine away from the edges
                let ideal = sine(VOICE_RATE, f, y.len());
                let mid = &y[2000..y.len() - 2000];
                let ideal_mid = &ideal[2000..ideal.len() - 2000];
                let err: Vec<f32> = mid.iter().zip(ideal_mid).map(|(a, b)| a - b).collect();
                let snr = 20.0 * (rms(ideal_mid) / rms(&err)).log10();
                assert!(snr > 50.0, "{from} Hz → 16 kHz at {f} Hz: SNR {snr:.1} dB");
            }
        }
    }

    #[test]
    fn stopband_is_attenuated() {
        for from in [48_000u32, 44_100] {
            let r = Resampler::new(from, VOICE_RATE).unwrap();
            for f in [8_600.0, 9_000.0, 12_000.0, 20_000.0] {
                let y = r.process(&sine(from, f, from as usize));
                let mid = &y[2000..y.len() - 2000];
                let level = 20.0 * (rms(mid) / std::f64::consts::FRAC_1_SQRT_2).log10();
                assert!(level < -60.0, "{from}: {f} Hz leaks at {level:.1} dB");
            }
        }
    }

    #[test]
    fn impulse_lands_on_its_exact_time() {
        // An impulse at input sample 3·k must peak at output sample k (zero delay).
        let r = Resampler::new(48_000, VOICE_RATE).unwrap();
        let mut x = vec![0.0f32; 4800];
        x[3 * 500] = 1.0;
        let y = r.process(&x);
        let peak = y
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak, 500);
    }

    #[test]
    fn voice_16k_passes_16k_through() {
        let x = vec![0.25f32; 320];
        assert_eq!(voice_16k(&x, 16_000, 1).unwrap(), x);
        assert_eq!(voice_16k(&vec![0.0; 960], 48_000, 2).unwrap().len(), 160);
    }
}
