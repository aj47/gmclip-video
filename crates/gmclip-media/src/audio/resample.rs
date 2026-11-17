//! Sample-rate conversion to the mix's 48 kHz at decode (decision 0051).
//!
//! A windowed sinc (Blackman, [`HALF`] taps a side at the source rate, widened by the
//! ratio when it goes down, cut off at 0.95 × the lower Nyquist), one tap set per phase,
//! each normalised to unit gain. Output sample `n` sits at source position
//! `n · rate / 48000`; it is the dot product of the source samples around that position
//! with the phase's taps, in f64. Every output sample is therefore a fixed function of
//! its index and the source, so a read in chunks equals a read whole (playback ≡ export)
//! and a seek changes nothing. Source samples before the first are silence.
//!
//! Phases are exact for every rate with at most [`MAX_PHASES`] (`48000 / gcd`: 160 for
//! 44.1 kHz, 3 for 32 or 16 kHz, 1 for 96 kHz); an odd rate beyond that rounds the
//! position to the nearest of [`MAX_PHASES`] (under 1/8000 of a sample).

/// The mix rate.
pub const OUT_RATE: u32 = 48_000;
/// Taps on each side of the position, at the source rate when it goes up.
pub const HALF: usize = 32;
/// Most phases kept.
pub const MAX_PHASES: u64 = 4096;

/// Fills source frames `[a, a + buf.len() / channels)` into `buf`.
pub type SourceRead<'a> = dyn FnMut(u64, &mut [f32]) -> Result<(), String> + 'a;

/// A converter from one source rate to 48 kHz.
#[derive(Clone, Debug)]
pub struct Resampler {
    /// Source samples per output sample is `p / q`.
    p: u64,
    q: u64,
    /// Phases in the table (`q`, or [`MAX_PHASES`]).
    phases: u64,
    /// Taps a side.
    half: usize,
    /// `phases` rows of `2 · half` taps; tap `k` of a row weighs source sample
    /// `i − half + 1 + k` for a position `i + phase / phases`.
    table: Vec<f64>,
    buf: Vec<f32>,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Resampler {
    /// A converter from `rate` Hz (1 Hz to 768 kHz).
    #[must_use]
    pub fn new(rate: u32) -> Self {
        let rate = u64::from(rate.clamp(1, 768_000));
        let g = gcd(rate, u64::from(OUT_RATE));
        let (p, q) = (rate / g, u64::from(OUT_RATE) / g);
        let phases = q.min(MAX_PHASES);
        // the cutoff, as a fraction of the source's Nyquist
        let fc = 0.95 * (f64::from(OUT_RATE) / rate as f64).min(1.0);
        let half = ((HALF as f64 / fc).ceil() as usize).max(HALF);
        let width = half as f64;
        let mut table = Vec::with_capacity(phases as usize * 2 * half);
        for j in 0..phases {
            let frac = j as f64 / phases as f64;
            let row: Vec<f64> = (0..2 * half)
                .map(|k| {
                    let t = k as f64 - (half as f64 - 1.0) - frac;
                    let x = std::f64::consts::PI * fc * t;
                    let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                    let u = (t / width).clamp(-1.0, 1.0);
                    let w = 0.42
                        + 0.5 * (std::f64::consts::PI * u).cos()
                        + 0.08 * (2.0 * std::f64::consts::PI * u).cos();
                    sinc * w
                })
                .collect();
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|h| h / sum));
        }
        Self {
            p,
            q,
            phases,
            half,
            table,
            buf: Vec::new(),
        }
    }

    /// Source position of output sample `n`: `(i, phase)`.
    fn at(&self, n: u64) -> (u64, usize) {
        let pos = u128::from(n) * u128::from(self.p);
        let (q, ph) = (u128::from(self.q), u128::from(self.phases));
        let i = (pos / q) as u64;
        let r = pos % q;
        if ph == q {
            return (i, r as usize);
        }
        let j = (r * ph + q / 2) / q;
        if j == ph { (i + 1, 0) } else { (i, j as usize) }
    }

    /// Fills `out` (interleaved, `ch` channels) with 48 kHz samples `[first, first + n)`.
    /// `read(a, buf)` fills source frames `[a, a + buf.len() / ch)` (silence past the
    /// end is the reader's).
    pub fn read(
        &mut self,
        ch: usize,
        first: u64,
        out: &mut [f32],
        read: &mut SourceRead<'_>,
    ) -> Result<(), String> {
        let ch = ch.max(1);
        let n = (out.len() / ch) as u64;
        if n == 0 {
            return Ok(());
        }
        let h = self.half as u64;
        let (i_first, _) = self.at(first);
        let (i_last, _) = self.at(first + n - 1);
        // source frames [lo, hi): every tap of every output sample
        let lo = i_first as i128 - (h as i128 - 1);
        let hi = i128::from(i_last) + h as i128 + 1;
        let len = usize::try_from(hi - lo).map_err(|_| "resample span".to_owned())?;
        self.buf.clear();
        self.buf.resize(len * ch, 0.0);
        // before the source's first sample is silence
        let skip = usize::try_from((-lo).max(0)).unwrap_or(len).min(len);
        if skip < len {
            read(
                u64::try_from(lo.max(0)).unwrap_or(0),
                &mut self.buf[skip * ch..],
            )?;
        }
        let taps = 2 * self.half;
        for (m, frame) in out.chunks_exact_mut(ch).enumerate() {
            let (i, j) = self.at(first + m as u64);
            let base = (i128::from(i) - (h as i128 - 1) - lo) as usize;
            let row = &self.table[j * taps..(j + 1) * taps];
            for (c, o) in frame.iter_mut().enumerate() {
                let mut acc = 0.0f64;
                for (k, w) in row.iter().enumerate() {
                    acc += w * f64::from(self.buf[(base + k) * ch + c]);
                }
                *o = acc as f32;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn reader(src: Vec<f32>) -> impl FnMut(u64, &mut [f32]) -> Result<(), String> {
        move |a, buf| {
            for (k, o) in buf.iter_mut().enumerate() {
                *o = src.get(a as usize + k).copied().unwrap_or(0.0);
            }
            Ok(())
        }
    }

    /// A 1 kHz tone at 44.1, 32, 22.05 and 96 kHz and an odd 44 056 Hz comes out as the
    /// same tone at 48 kHz, within −60 dB away from the edges.
    #[test]
    fn a_tone_keeps_its_pitch_and_level() {
        for rate in [44_100u32, 32_000, 22_050, 96_000, 44_056] {
            let src = sine(rate, 1000.0, rate as usize);
            let mut r = Resampler::new(rate);
            let mut out = vec![0.0f32; 48_000];
            r.read(1, 0, &mut out, &mut reader(src)).unwrap();
            let want = sine(48_000, 1000.0, 48_000);
            let err = (2000..46_000)
                .map(|i| (out[i] - want[i]).abs())
                .fold(0f32, f32::max);
            assert!(err < 5e-4, "{rate} Hz: max error {err}");
        }
    }

    /// Going down, a tone above the new Nyquist is filtered out instead of folding back.
    #[test]
    fn going_down_does_not_alias() {
        let src = sine(96_000, 30_000.0, 96_000);
        let mut out = vec![0.0f32; 48_000];
        Resampler::new(96_000)
            .read(1, 0, &mut out, &mut reader(src))
            .unwrap();
        let peak = out[2000..46_000].iter().fold(0f32, |a, x| a.max(x.abs()));
        assert!(peak < 0.5 * 0.01, "a 30 kHz tone leaks at {peak}");
    }

    /// Reads in uneven chunks, stereo, are the whole read bit for bit.
    #[test]
    fn chunks_equal_the_whole_read() {
        let l = sine(44_100, 440.0, 44_100);
        let src: Vec<f32> = l.iter().flat_map(|x| [*x, -x * 0.5]).collect();
        let read = |a: u64, buf: &mut [f32]| -> Result<(), String> {
            for (k, o) in buf.iter_mut().enumerate() {
                *o = src.get(a as usize * 2 + k).copied().unwrap_or(0.0);
            }
            Ok(())
        };
        let mut whole = vec![0.0f32; 2 * 40_000];
        Resampler::new(44_100)
            .read(2, 0, &mut whole, &mut { read })
            .unwrap();
        let mut r = Resampler::new(44_100);
        let mut got = Vec::new();
        let mut at = 0u64;
        for n in [1usize, 7, 1024, 333, 4800, 17, 20_000, 13_818] {
            let mut buf = vec![0.0f32; 2 * n];
            r.read(2, at, &mut buf, &mut { read }).unwrap();
            got.extend_from_slice(&buf);
            at += n as u64;
        }
        assert_eq!(at, 40_000);
        assert_eq!(got, whole);
    }
}
