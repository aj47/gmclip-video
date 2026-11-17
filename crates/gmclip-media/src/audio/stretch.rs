//! Time-stretch with the pitch kept (decision 0030): WSOLA, waveform-similarity
//! overlap-add, in our own f32/f64 arithmetic (no crate), so the same input gives the
//! same samples on every machine and the export, the preview's playback and verify mix
//! the same audio.
//!
//! Frames of [`FRAME`] samples (40 ms at 48 kHz) are laid down every [`HOP`] samples of
//! output under a periodic Hann window (which sums to 1 at half overlap). Output frame `j`
//! reads the input near `j · HOP · speed`: within ±[`TOLERANCE`] of it, at the offset whose
//! first [`HOP`] samples best match what naturally follows the previous frame (normalised
//! cross-correlation on a mono mix, searched every [`DECIMATE`] samples, then refined to
//! the sample). The first frame is not faded in, so the part starts at full level.

/// Frame length in samples (40 ms at 48 kHz).
pub const FRAME: usize = 1920;
/// Output hop in samples (half a frame).
pub const HOP: usize = FRAME / 2;
/// How far a frame may move from its nominal input position, in samples (10 ms).
pub const TOLERANCE: usize = 480;
/// The coarse search's step.
pub const DECIMATE: usize = 4;

/// Input samples (per channel) [`stretch`] reads beyond `⌈out_frames · speed⌉`.
pub const MARGIN: usize = FRAME + TOLERANCE;

/// `num / den`.
fn ratio(num: u64, den: u64) -> f64 {
    num as f64 / den.max(1) as f64
}

fn mono(x: &[f32], i: usize) -> f64 {
    (f64::from(x[2 * i]) + f64::from(x[2 * i + 1])) * 0.5
}

/// Normalised cross-correlation of `HOP` samples at `a` and `b` (mono), every `step`.
fn corr(x: &[f32], a: usize, b: usize, step: usize) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    let mut i = 0;
    while i < HOP {
        let (u, v) = (mono(x, a + i), mono(x, b + i));
        ab += u * v;
        aa += u * u;
        bb += v * v;
        i += step;
    }
    ab / (aa * bb + 1e-12).sqrt()
}

/// Stretches interleaved stereo `input` (48 kHz) played at `num/den` × to exactly
/// `out_frames` stereo frames, pitch kept. `input` should hold
/// `⌈out_frames · num / den⌉ + MARGIN` frames (fewer are read as silence).
pub fn stretch(input: &[f32], num: u64, den: u64, out_frames: usize) -> Vec<f32> {
    let speed = ratio(num, den);
    stretch_by(input, out_frames, &|n| n * speed)
}

/// [`stretch`] along a time map (decision 0033, a speed ramp): output sample `n` reads the
/// input near sample `map(n)`, which must not decrease. A constant `map(n) = n · speed` is
/// exactly [`stretch`]. `input` should hold `⌈map(out_frames)⌉ + MARGIN` frames.
pub fn stretch_by(input: &[f32], out_frames: usize, map: &dyn Fn(f64) -> f64) -> Vec<f32> {
    let n_in = input.len() / 2;
    let mut out = vec![0.0f32; out_frames * 2];
    if out_frames == 0 {
        return out;
    }
    if n_in < FRAME + 1 {
        // shorter than one frame: nearest-sample resampling (a tiny part)
        for (k, o) in out.chunks_exact_mut(2).enumerate() {
            let i = (map(k as f64) as usize).min(n_in.saturating_sub(1));
            if n_in > 0 {
                o[0] = input[2 * i];
                o[1] = input[2 * i + 1];
            }
        }
        return out;
    }
    let window: Vec<f64> = (0..FRAME)
        .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / FRAME as f64).cos())
        .collect();
    let last = n_in - FRAME; // the largest frame start
    let mut acc = vec![0.0f64; (out_frames + FRAME) * 2];
    let mut prev: Option<usize> = None;
    let mut j = 0usize;
    while j * HOP < out_frames {
        let nominal = (map(j as f64 * HOP as f64).round() as usize).min(last);
        let pos = match prev {
            None => nominal,
            Some(p) => {
                let target = p + HOP;
                if target + HOP > n_in {
                    nominal
                } else {
                    let lo = nominal.saturating_sub(TOLERANCE);
                    let hi = (nominal + TOLERANCE).min(last);
                    // coarse, then to the sample
                    let mut best = (f64::MIN, nominal);
                    let mut c = lo;
                    while c <= hi {
                        let v = corr(input, c, target, DECIMATE);
                        if v > best.0 {
                            best = (v, c);
                        }
                        c += DECIMATE;
                    }
                    let (a, b) = (
                        best.1.saturating_sub(DECIMATE - 1).max(lo),
                        (best.1 + DECIMATE - 1).min(hi),
                    );
                    let mut fine = (f64::MIN, best.1);
                    for c in a..=b {
                        let v = corr(input, c, target, 1);
                        if v > fine.0 {
                            fine = (v, c);
                        }
                    }
                    fine.1
                }
            }
        };
        let o = j * HOP;
        for n in 0..FRAME {
            // the first frame is not faded in: the part starts at full level
            let w = if j == 0 && n < HOP { 1.0 } else { window[n] };
            acc[2 * (o + n)] += w * f64::from(input[2 * (pos + n)]);
            acc[2 * (o + n) + 1] += w * f64::from(input[2 * (pos + n) + 1]);
        }
        prev = Some(pos);
        j += 1;
    }
    for (o, a) in out.iter_mut().zip(&acc) {
        *o = *a as f32;
    }
    out
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sine(hz: f64, secs: f64) -> Vec<f32> {
        let n = (secs * 48_000.0) as usize;
        (0..n)
            .flat_map(|i| {
                let v =
                    (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    /// Frequency by rising zero crossings over the middle half of the left channel.
    fn freq(x: &[f32]) -> f64 {
        let n = x.len() / 2;
        let (a, b) = (n / 4, 3 * n / 4);
        let mut ups = Vec::new();
        for i in a + 1..b {
            if x[2 * (i - 1)] < 0.0 && x[2 * i] >= 0.0 {
                ups.push(i);
            }
        }
        (ups.len() - 1) as f64 * 48_000.0 / (ups[ups.len() - 1] - ups[0]) as f64
    }

    fn rms(x: &[f32]) -> f64 {
        let n = x.len() / 2;
        let (a, b) = (n / 4, 3 * n / 4);
        ((a..b).map(|i| f64::from(x[2 * i]).powi(2)).sum::<f64>() / (b - a) as f64).sqrt()
    }

    #[test]
    fn faster_and_slower_keep_pitch_level_and_length() {
        let input = sine(440.0, 3.0);
        for (num, den) in [(3u64, 2u64), (2, 1), (1, 2), (4, 1), (1, 4)] {
            let out_frames =
                (48_000.0 * 2.0 * den as f64 / num as f64).min(48_000.0 * 2.5) as usize;
            let y = stretch(&input, num, den, out_frames);
            assert_eq!(y.len(), out_frames * 2, "{num}/{den}");
            let f = freq(&y);
            assert!((f - 440.0).abs() < 4.0, "{num}/{den}: {f:.1} Hz");
            let r = rms(&y) / rms(&input);
            assert!((0.85..1.1).contains(&r), "{num}/{den}: level × {r:.3}");
            // the start is at full level (no fade in), the samples are finite
            assert!(y.iter().all(|v| v.is_finite()));
            assert!((y[2] - input[2]).abs() < 1e-6);
        }
    }

    /// Decision 0033: along a ramp's time map the pitch is kept the whole way and the
    /// output takes exactly the input the map reaches; a constant map is [`stretch`].
    #[test]
    fn a_ramp_keeps_pitch_and_reads_its_integral() {
        let input = sine(440.0, 4.0);
        // 1× to 2× linearly over the first 2 s of output, then 2×: x + x²/4 (seconds)
        let n = 48_000.0f64;
        let map = |s: f64| {
            let x = s / n;
            let u = if x < 2.0 {
                x + x * x / 4.0
            } else {
                3.0 + 2.0 * (x - 2.0)
            };
            u * n
        };
        let out_frames = (n * 2.4) as usize; // reaches 3.8 s of input
        let y = stretch_by(&input, out_frames, &map);
        assert_eq!(y.len(), out_frames * 2);
        for (a, b) in [(0.1, 0.9), (1.1, 1.9), (2.0, 2.4)] {
            let seg = &y[(a * n) as usize * 2..(b * n) as usize * 2];
            let f = freq(seg);
            assert!((f - 440.0).abs() < 5.0, "{a}–{b} s: {f:.1} Hz");
        }
        assert!(y.iter().all(|v| v.is_finite()));
        let speed = 1.5f64;
        assert_eq!(
            stretch(&input, 3, 2, 30_000),
            stretch_by(&input, 30_000, &|s| s * speed)
        );
    }

    #[test]
    fn deterministic_and_short_inputs() {
        let input = sine(220.0, 1.0);
        assert_eq!(stretch(&input, 3, 2, 20_000), stretch(&input, 3, 2, 20_000));
        // shorter than a frame: resampled by the nearest sample
        let tiny = sine(220.0, 0.02);
        let y = stretch(&tiny, 2, 1, 480);
        assert_eq!(y.len(), 960);
        assert_eq!(y[2], tiny[4]);
        assert!(stretch(&[], 2, 1, 10).iter().all(|v| *v == 0.0));
    }
}
