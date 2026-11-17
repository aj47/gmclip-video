//! Loudness (B-12; SPEC §5.6, RenderPlan rule 8): ITU-R BS.1770-4 integrated loudness
//! with gating, momentary/short-term loudness (EBU Tech 3341), 4× oversampled true peak
//! (BS.1770-4 Annex 2's 48-tap interpolator), and the `static` mode: measure, apply one
//! gain, run a lookahead true-peak limiter, re-measure. Never dynamic.
//!
//! Own implementation (no crate): K-weighting coefficients are derived from BS.1770's
//! analogue prototypes for any rate (they reproduce the standard's 48 kHz table), and
//! the interpolator is the standard's table verbatim.

/// ITU-R BS.1770-4 Annex 2 (and -5): 48-tap, 4-phase FIR interpolator, per phase.
const TP_PHASES: [[f64; 12]; 4] = [
    [
        0.001_708_984_375,
        0.010_986_328_125,
        -0.019_653_320_312_5,
        0.033_203_125,
        -0.059_448_242_187_5,
        0.137_329_101_562_5,
        0.972_167_968_75,
        -0.102_294_921_875,
        0.047_607_421_875,
        -0.026_611_328_125,
        0.014_892_578_125,
        -0.008_300_781_25,
    ],
    [
        -0.029_174_804_687_5,
        0.029_296_875,
        -0.051_757_812_5,
        0.089_111_328_125,
        -0.166_503_906_25,
        0.465_087_890_625,
        0.779_785_156_25,
        -0.200_317_382_812_5,
        0.101_562_5,
        -0.058_227_539_062_5,
        0.033_081_054_687_5,
        -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5,
        0.033_081_054_687_5,
        -0.058_227_539_062_5,
        0.101_562_5,
        -0.200_317_382_812_5,
        0.779_785_156_25,
        0.465_087_890_625,
        -0.166_503_906_25,
        0.089_111_328_125,
        -0.051_757_812_5,
        0.029_296_875,
        -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_25,
        0.014_892_578_125,
        -0.026_611_328_125,
        0.047_607_421_875,
        -0.102_294_921_875,
        0.972_167_968_75,
        0.137_329_101_562_5,
        -0.059_448_242_187_5,
        0.033_203_125,
        -0.019_653_320_312_5,
        0.010_986_328_125,
        0.001_708_984_375,
    ],
];

/// Absolute gate (LKFS) and relative gate offset (LU), BS.1770-4 §2.
const ABS_GATE: f64 = -70.0;
const REL_GATE: f64 = -10.0;

/// A biquad in transposed direct form II, f64.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting stages for `rate` (BS.1770-4 Table 1 / 2 at 48 kHz).
fn k_weighting(rate: u32) -> [Biquad; 2] {
    let fs = f64::from(rate);
    // Stage 1: high shelf (head effects).
    let (f0, g, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    // Stage 2: RLB high pass.
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, hp]
}

/// Channel weights by count: BS.1770's G_i (L, R, C = 1.0; surrounds 1.41; LFE excluded).
/// Stereo → [1, 1]; 5.0 (L R C Ls Rs) → [1, 1, 1, 1.41, 1.41]; 5.1 (L R C LFE Ls Rs).
#[must_use]
pub fn channel_weights(channels: usize) -> Vec<f64> {
    match channels {
        4 => vec![1.0, 1.0, 1.41, 1.41], // quad: L R Ls Rs
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

/// Prefix sums of the K-weighted, channel-weighted energy Σ G_i · y_i² per frame:
/// `p[n]` is the sum over frames `[0, n)`, so any window's energy is a difference.
fn energy_prefix(pcm: &[f32], channels: usize, rate: u32) -> Vec<f64> {
    let ch = channels.max(1);
    let weights = channel_weights(ch);
    let mut filters: Vec<[Biquad; 2]> = (0..ch).map(|_| k_weighting(rate)).collect();
    let mut p = Vec::with_capacity(pcm.len() / ch + 1);
    let mut acc = 0f64;
    p.push(0.0);
    for frame in pcm.chunks_exact(ch) {
        for ((x, f), w) in frame.iter().zip(filters.iter_mut()).zip(&weights) {
            let s1 = f[0].run(f64::from(*x));
            let y = f[1].run(s1);
            acc += w * y * y;
        }
        p.push(acc);
    }
    p
}

fn lufs(mean_energy: f64) -> f64 {
    -0.691 + 10.0 * mean_energy.log10()
}

/// Mean-square energies of every complete window of `len` frames, starting every `hop`.
fn windows(p: &[f64], len: usize, hop: usize) -> Vec<f64> {
    let frames = p.len().saturating_sub(1);
    if frames < len || len == 0 {
        return Vec::new();
    }
    (0..=(frames - len) / hop.max(1))
        .map(|j| {
            let a = j * hop.max(1);
            (p.get(a + len).copied().unwrap_or(0.0) - p.get(a).copied().unwrap_or(0.0)).max(0.0)
                / len as f64
        })
        .collect()
}

/// Integrated loudness (LUFS) of an interleaved buffer, BS.1770-4: 400 ms blocks every
/// 100 ms, absolute gate −70 LKFS, relative gate −10 LU. `-inf` when every block is
/// gated out (silence, or shorter than 400 ms).
#[must_use]
pub fn integrated(pcm: &[f32], channels: usize, rate: u32) -> f64 {
    let p = energy_prefix(pcm, channels, rate);
    let step = (rate / 10) as usize;
    gated(&windows(&p, 4 * step, step))
}

fn gated(blocks: &[f64]) -> f64 {
    let abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&z| lufs(z) > ABS_GATE)
        .collect();
    if abs.is_empty() {
        return f64::NEG_INFINITY;
    }
    let rel = lufs(abs.iter().sum::<f64>() / abs.len() as f64) + REL_GATE;
    let kept: Vec<f64> = abs.into_iter().filter(|&z| lufs(z) > rel).collect();
    if kept.is_empty() {
        return f64::NEG_INFINITY;
    }
    lufs(kept.iter().sum::<f64>() / kept.len() as f64)
}

/// Momentary (400 ms) loudness every 100 ms, LUFS.
#[must_use]
pub fn momentary(pcm: &[f32], channels: usize, rate: u32) -> Vec<f64> {
    let step = (rate / 10) as usize;
    windows(&energy_prefix(pcm, channels, rate), 4 * step, step)
        .into_iter()
        .map(lufs)
        .collect()
}

/// Short-term (3 s) loudness every 100 ms, LUFS.
#[must_use]
pub fn short_term(pcm: &[f32], channels: usize, rate: u32) -> Vec<f64> {
    let step = (rate / 10) as usize;
    windows(&energy_prefix(pcm, channels, rate), 30 * step, step)
        .into_iter()
        .map(lufs)
        .collect()
}

/// Maximum momentary loudness over every window position (1-sample hop), as a
/// file-based meter reports it (EBU Tech 3341 case 13).
#[must_use]
pub fn max_momentary(pcm: &[f32], channels: usize, rate: u32) -> f64 {
    let len = (rate as usize) * 4 / 10;
    windows(&energy_prefix(pcm, channels, rate), len, 1)
        .into_iter()
        .fold(f64::NEG_INFINITY, |m, z| m.max(lufs(z)))
}

/// Maximum short-term loudness over every window position (1-sample hop).
#[must_use]
pub fn max_short_term(pcm: &[f32], channels: usize, rate: u32) -> f64 {
    let len = (rate as usize) * 3;
    windows(&energy_prefix(pcm, channels, rate), len, 1)
        .into_iter()
        .fold(f64::NEG_INFINITY, |m, z| m.max(lufs(z)))
}

/// Per-frame true-peak estimate: for each frame `n`, the largest |value| over the four
/// interpolated points the BS.1770 filter places around it, and the sample itself, over
/// all channels (linear). Output frame `n` reflects input frames up to `n + 6`.
fn true_peak_curve(pcm: &[f32], channels: usize) -> Vec<f32> {
    let ch = channels.max(1);
    let frames = pcm.len() / ch;
    let mut out = vec![0f32; frames];
    // One channel at a time, zero-padded by the filter's reach on both sides.
    let mut x = vec![0f64; frames + 12];
    for c in 0..ch {
        for (n, v) in x.iter_mut().skip(6).take(frames).enumerate() {
            *v = f64::from(pcm.get(n * ch + c).copied().unwrap_or(0.0));
        }
        for (n, o) in out.iter_mut().enumerate() {
            // x[n + 6] is frame n; taps run back from frame n + 6 (the interpolator's
            // points between frames n and n + 1 for this 12-tap, delay-5.5 filter).
            let win = &x[n + 1..n + 13];
            let mut peak = f64::from(*o).max(win[5].abs());
            for ph in &TP_PHASES {
                let mut y = 0.0;
                for (h, v) in ph.iter().zip(win.iter().rev()) {
                    y += h * v;
                }
                peak = peak.max(y.abs());
            }
            *o = peak as f32;
        }
    }
    out
}

/// Sliding minimum over `[n − r, n + r]` (monotone deque, O(n)).
fn sliding_min(v: &[f64], r: usize) -> Vec<f64> {
    let mut out = vec![1.0; v.len()];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next = 0usize;
    for (n, o) in out.iter_mut().enumerate() {
        let hi = (n + r + 1).min(v.len());
        while next < hi {
            while dq.back().is_some_and(|&b| v[b] >= v[next]) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        while dq.front().is_some_and(|&f| f + r < n) {
            dq.pop_front();
        }
        *o = dq.front().map_or(1.0, |&f| v[f]);
    }
    out
}

/// True peak (dBTP) of an interleaved buffer: 4× oversampled per BS.1770-4 Annex 2.
#[must_use]
pub fn true_peak(pcm: &[f32], channels: usize) -> f64 {
    let p = true_peak_curve(pcm, channels)
        .into_iter()
        .fold(0f32, f32::max);
    20.0 * f64::from(p).log10()
}

/// Integrated loudness and true peak together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
}

#[must_use]
pub fn measure(pcm: &[f32], channels: usize, rate: u32) -> Measurement {
    Measurement {
        integrated_lufs: integrated(pcm, channels, rate),
        true_peak_dbtp: true_peak(pcm, channels),
    }
}

/// What `static` normalisation did (both measurements go into the export record).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticReport {
    pub before: Measurement,
    /// The one static gain actually applied (dB).
    pub gain_db: f64,
    pub limiter: LimiterStats,
    pub after: Measurement,
    /// False when the target could not be reached within ±0.2 LU (the ceiling caps the
    /// loudness, or the limiter would have to go deeper than [`MAX_LIMIT_DB`]). The
    /// export reports it; it never silently continues.
    pub reached: bool,
}

/// How hard the true-peak limiter worked.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LimiterStats {
    /// Frames turned down by more than 0.1 dB, 1 dB and 3 dB.
    pub frames_over_0_1_db: usize,
    pub frames_over_1_db: usize,
    pub frames_over_3_db: usize,
    /// Deepest reduction (dB, ≤ 0).
    pub deepest_db: f64,
}

/// Limiter lookahead and release.
const LOOKAHEAD_MS: u32 = 2;
const RELEASE_MS: f64 = 80.0;
/// The deepest gain reduction the gain search will accept on a single frame.
pub const MAX_LIMIT_DB: f64 = 12.0;

/// `static` loudness (RP rule 8): one gain, then a 4× oversampled lookahead true-peak
/// limiter at `ceiling_dbtp`, then re-measure. The gain is *solved* so the limited result
/// lands on the target (the limiter only lowers loudness): a bracketed secant/bisection
/// search on `I(limit(x · g)) − target`, which is monotone in `g`. It is still one static
/// gain. Clips with no gated loudness (silence, < 400 ms) get no gain but are still
/// limited to the ceiling.
pub fn normalize_static(
    pcm: &mut [f32],
    channels: usize,
    rate: u32,
    target_lufs: f64,
    ceiling_dbtp: f64,
) -> StaticReport {
    let before = measure(pcm, channels, rate);
    if !before.integrated_lufs.is_finite() {
        let limiter = limit(pcm, channels, rate, ceiling_dbtp);
        let after = measure(pcm, channels, rate);
        return StaticReport {
            before,
            gain_db: 0.0,
            limiter,
            after,
            reached: false,
        };
    }
    let original = pcm.to_vec();
    let run = |g: f64, out: &mut [f32]| -> (f64, LimiterStats) {
        out.copy_from_slice(&original);
        apply_gain(out, g);
        let st = limit(out, channels, rate, ceiling_dbtp);
        (integrated(out, channels, rate), st)
    };
    // Bracket: lo has I ≤ target (the unlimited gain cannot overshoot); hi grows until
    // I ≥ target or the limiter would go deeper than MAX_LIMIT_DB.
    let g0 = target_lufs - before.integrated_lufs;
    let (mut lo, mut lo_i) = (g0, f64::NAN);
    let mut hi: Option<(f64, f64)> = None;
    let mut best = (g0, f64::INFINITY);
    let mut g = g0;
    for _ in 0..16 {
        let (i, st) = run(g, pcm);
        let miss = target_lufs - i;
        if miss.abs() < best.1.abs() || !best.1.is_finite() {
            best = (g, miss);
        }
        if miss.abs() < 0.02 {
            break;
        }
        if miss > 0.0 {
            (lo, lo_i) = (g, i);
            if st.deepest_db < -MAX_LIMIT_DB {
                break; // more gain only digs the limiter deeper
            }
        } else {
            hi = Some((g, i));
        }
        g = match hi {
            // Secant inside the bracket, falling back to bisection.
            Some((h, hi_i)) if lo_i.is_finite() && hi_i > lo_i => {
                let s = lo + (target_lufs - lo_i) * (h - lo) / (hi_i - lo_i);
                if s > lo && s < h { s } else { (lo + h) / 2.0 }
            }
            Some((h, _)) => (lo + h) / 2.0,
            // No upper bound yet: step by the miss, at least 0.5 dB, at most 6 dB.
            None => lo + miss.clamp(0.5, 6.0),
        };
    }
    let (i, limiter) = run(best.0, pcm);
    let after = Measurement {
        integrated_lufs: i,
        true_peak_dbtp: true_peak(pcm, channels),
    };
    StaticReport {
        before,
        gain_db: best.0,
        limiter,
        after,
        reached: (i - target_lufs).abs() <= 0.2,
    }
}

fn apply_gain(pcm: &mut [f32], db: f64) {
    let g = 10f64.powf(db / 20.0);
    pcm.iter_mut().for_each(|s| *s = (f64::from(*s) * g) as f32);
}

/// Lookahead true-peak limiter, offline: required gain per frame from the true-peak
/// curve, a min over ±lookahead (so gain is down before a peak), a one-pole release, then
/// a moving average no wider than the lookahead (which keeps the gain at or below the
/// requirement at every frame). Re-checked on the oversampled result and tightened; a
/// last uniform trim (at most the residual overshoot, a few hundredths of a dB) makes the
/// ceiling hold exactly.
fn limit(pcm: &mut [f32], channels: usize, rate: u32, ceiling_dbtp: f64) -> LimiterStats {
    let ch = channels.max(1);
    let frames = pcm.len() / ch;
    let la = (rate * LOOKAHEAD_MS / 1000).max(1) as usize;
    let rel = (-1.0 / (RELEASE_MS / 1000.0 * f64::from(rate))).exp();
    let target = 10f64.powf(ceiling_dbtp / 20.0);
    let original = pcm.to_vec();
    let curve = true_peak_curve(&original, ch);
    if curve.iter().all(|&p| f64::from(p) <= target) {
        return LimiterStats::default();
    }
    let mut ceiling = target;
    let mut smooth = vec![1f64; frames];
    for _ in 0..8 {
        let need: Vec<f64> = curve
            .iter()
            .map(|&p| {
                if f64::from(p) > ceiling {
                    ceiling / f64::from(p)
                } else {
                    1.0
                }
            })
            .collect();
        // Min over [n − la, n + la].
        let mut g = sliding_min(&need, la);
        // Release: recover towards 1 no faster than the time constant.
        for n in 1..frames {
            let up = 1.0 - (1.0 - g[n - 1]) * rel;
            g[n] = g[n].min(up);
        }
        // Smooth with a centred moving average of half-width la/2 (≤ la, so ≤ need).
        let h = la / 2;
        let mut prefix = vec![0f64; frames + 1];
        for n in 0..frames {
            prefix[n + 1] = prefix[n] + g[n];
        }
        for (n, sm) in smooth.iter_mut().enumerate() {
            let (a, b) = (n.saturating_sub(h), (n + h + 1).min(frames));
            *sm = (prefix[b] - prefix[a]) / (b - a) as f64;
        }
        apply_curve(pcm, &original, &smooth, ch);
        let tp = f64::from(true_peak_curve(pcm, ch).into_iter().fold(0f32, f32::max));
        if tp <= target {
            break;
        }
        // Gain modulation widened a peak: tighten by the overshoot and redo.
        ceiling *= target / tp * 0.999;
    }
    // Guarantee: whatever overshoot is left comes off uniformly.
    let tp = f64::from(true_peak_curve(pcm, ch).into_iter().fold(0f32, f32::max));
    if tp > target {
        let k = target / tp * 0.9999;
        smooth.iter_mut().for_each(|g| *g *= k);
        apply_curve(pcm, &original, &smooth, ch);
    }
    let mut st = LimiterStats::default();
    for &gn in &smooth {
        let db = 20.0 * gn.log10();
        st.frames_over_0_1_db += usize::from(db < -0.1);
        st.frames_over_1_db += usize::from(db < -1.0);
        st.frames_over_3_db += usize::from(db < -3.0);
        st.deepest_db = st.deepest_db.min(db);
    }
    st
}

fn apply_curve(pcm: &mut [f32], original: &[f32], gains: &[f64], ch: usize) {
    for ((frame, src), gn) in pcm
        .chunks_exact_mut(ch)
        .zip(original.chunks_exact(ch))
        .zip(gains)
    {
        for (s, o) in frame.iter_mut().zip(src) {
            *s = (f64::from(*o) * gn) as f32;
        }
    }
}

/// What delivery-aware normalisation did (decision 0004).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeliveryReport {
    /// The normalisation behind the delivered file (the second one if re-encoded).
    pub mix: StaticReport,
    /// Limiter ceiling used for the delivered file (dBTP).
    pub ceiling_dbtp: f64,
    /// True peak of the first encode, decoded (dBTP).
    pub first_delivered_tp_dbtp: f64,
    /// True peak of the delivered file, decoded (dBTP).
    pub delivered_tp_dbtp: f64,
    /// Integrated loudness of the delivered file, decoded (LUFS).
    pub delivered_integrated_lufs: f64,
    pub reencoded: bool,
    /// Whether the delivered file is at or under the requested ceiling.
    pub within_ceiling: bool,
}

/// `static` loudness with the true-peak ceiling applied to the **delivered** file
/// (decision 0004): normalise, encode and decode with `codec` (the export's own AAC
/// round trip, returning the decoded PCM aligned to the mix), measure true peak; if it is
/// over `ceiling_dbtp`, lower the limiter ceiling by the overshoot and encode once more.
/// Returns the PCM that was encoded last (the caller keeps that encode) and the report.
pub fn normalize_for_delivery<E>(
    mix: &[f32],
    channels: usize,
    rate: u32,
    target_lufs: f64,
    ceiling_dbtp: f64,
    mut codec: impl FnMut(&[f32]) -> Result<Vec<f32>, E>,
) -> Result<(Vec<f32>, DeliveryReport), E> {
    let mut pcm = mix.to_vec();
    let first = normalize_static(&mut pcm, channels, rate, target_lufs, ceiling_dbtp);
    let decoded = codec(&pcm)?;
    let tp1 = true_peak(&decoded, channels);
    let mut report = DeliveryReport {
        mix: first,
        ceiling_dbtp,
        first_delivered_tp_dbtp: tp1,
        delivered_tp_dbtp: tp1,
        delivered_integrated_lufs: integrated(&decoded, channels, rate),
        reencoded: false,
        within_ceiling: tp1 <= ceiling_dbtp,
    };
    if tp1 > ceiling_dbtp && tp1.is_finite() {
        let lowered = ceiling_dbtp - (tp1 - ceiling_dbtp);
        pcm.copy_from_slice(mix);
        let second = normalize_static(&mut pcm, channels, rate, target_lufs, lowered);
        let decoded = codec(&pcm)?;
        let tp2 = true_peak(&decoded, channels);
        report = DeliveryReport {
            mix: second,
            ceiling_dbtp: lowered,
            first_delivered_tp_dbtp: tp1,
            delivered_tp_dbtp: tp2,
            delivered_integrated_lufs: integrated(&decoded, channels, rate),
            reencoded: true,
            within_ceiling: tp2 <= ceiling_dbtp,
        };
    }
    Ok((pcm, report))
}

impl From<&StaticReport> for crate::export_record::LoudnessRecord {
    /// Both measurements (integrated and true peak), after and before, for the export
    /// record (B-12 acceptance; SPEC §5.14 `loudness{integrated, true_peak}`).
    fn from(r: &StaticReport) -> Self {
        // Silence measures −inf, which JSON cannot hold: those become null.
        let f = |x: f64| x.is_finite().then_some(x);
        Self {
            integrated: f(r.after.integrated_lufs),
            true_peak: f(r.after.true_peak_dbtp),
            before: Some(crate::export_record::LoudnessBefore {
                integrated: f(r.before.integrated_lufs),
                true_peak: f(r.before.true_peak_dbtp),
            }),
            gain_db: Some(r.gain_db),
            target_reached: Some(r.reached),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    #[test]
    fn delivery_re_encodes_once_at_a_lowered_ceiling() {
        // A mock codec that sharpens transients (unity DC gain, +HF): true peak rises
        // after "encoding", as AAC's does.
        let codec = |x: &[f32]| -> Result<Vec<f32>, ()> {
            let n = x.len() / 2;
            let mut y = vec![0f32; x.len()];
            for c in 0..2 {
                for i in 0..n {
                    let at = |k: i64| {
                        if k < 0 || k as usize >= n {
                            0.0
                        } else {
                            f64::from(x[2 * k as usize + c])
                        }
                    };
                    let i = i as i64;
                    y[2 * i as usize + c] =
                        (1.3 * at(i) - 0.15 * at(i - 1) - 0.15 * at(i + 1)) as f32;
                }
            }
            Ok(y)
        };
        let mut s = sine(440.0, 0.05, 0.0, 6.0, 2);
        for k in (0..s.len()).step_by(2 * 9_600) {
            for j in 0..8 {
                s[k + 2 * j] = if j % 2 == 0 { 0.6 } else { -0.6 };
                s[k + 2 * j + 1] = s[k + 2 * j];
            }
        }
        let (_, r) = normalize_for_delivery(&s, 2, FS, -16.0, -1.5, codec).unwrap();
        assert!(r.first_delivered_tp_dbtp > -1.5, "{r:?}");
        assert!(r.reencoded && r.ceiling_dbtp < -1.5);
        assert!(r.within_ceiling && r.delivered_tp_dbtp <= -1.5, "{r:?}");
        // A codec that adds nothing: one encode, no re-encode.
        let (_, r) =
            normalize_for_delivery(&s, 2, FS, -16.0, -1.5, |x: &[f32]| Ok::<_, ()>(x.to_vec()))
                .unwrap();
        assert!(!r.reencoded && r.within_ceiling);
    }

    #[test]
    fn k_weighting_matches_the_48k_table() {
        let [s, h] = k_weighting(48_000);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(s.b[0], 1.535_124_859_586_97) && close(s.b[1], -2.691_696_189_406_38));
        assert!(close(s.b[2], 1.198_392_810_852_85));
        assert!(close(s.a[0], -1.690_659_293_182_41) && close(s.a[1], 0.732_480_774_215_85));
        assert!(close(h.a[0], -1.990_047_454_833_98) && close(h.a[1], 0.990_072_250_366_21));
    }

    #[test]
    fn interpolator_is_the_standard_table() {
        // Phases 3 and 2 are phases 0 and 1 reversed (BS.1770-5 Annex 2 table).
        for k in 0..2 {
            let mut r = TP_PHASES[k];
            r.reverse();
            assert_eq!(r, TP_PHASES[3 - k]);
        }
    }

    #[test]
    fn interpolator_phases_have_unity_dc_gain() {
        for ph in &TP_PHASES {
            let s: f64 = ph.iter().sum();
            assert!((s - 1.0).abs() < 0.03, "{s}"); // the table's phase 0 sums to 0.973
        }
    }

    fn sine(freq: f64, amp: f64, phase_deg: f64, secs: f64, ch: usize) -> Vec<f32> {
        let n = (secs * f64::from(FS)) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (amp
                    * (std::f64::consts::TAU * freq * i as f64 / f64::from(FS)
                        + phase_deg.to_radians())
                    .sin()) as f32;
                std::iter::repeat_n(v, ch)
            })
            .collect()
    }

    #[test]
    fn silence_is_minus_infinity() {
        assert_eq!(integrated(&vec![0.0; 96_000], 2, FS), f64::NEG_INFINITY);
        let mut s = vec![0.0f32; 96_000];
        let r = normalize_static(&mut s, 2, FS, -16.0, -1.5);
        assert_eq!(r.gain_db, 0.0);
    }

    #[test]
    fn static_normalisation_hits_target_and_ceiling() {
        // A tone with sparse loud clicks: the gain to −16 would push clicks past −1.5.
        let mut s = sine(440.0, 0.05, 0.0, 10.0, 2);
        for k in (0..s.len()).step_by(2 * 24_000) {
            s[k] = 0.9;
            s[k + 1] = -0.9;
        }
        let r = normalize_static(&mut s, 2, FS, -16.0, -1.5);
        assert!((r.after.integrated_lufs + 16.0).abs() <= 0.2, "{r:?}");
        assert!(r.after.true_peak_dbtp <= -1.5, "{r:?}");
        assert!(
            r.reached && r.limiter.frames_over_0_1_db > 0 && r.limiter.deepest_db < 0.0,
            "{r:?}"
        );
    }

    #[test]
    fn short_and_silent_clips_are_still_limited_and_record_round_trips() {
        // 0.29 s at full scale: no gated loudness, but the ceiling still holds.
        let mut s = sine(997.0, 1.0, 0.0, 0.29, 2);
        let r = normalize_static(&mut s, 2, FS, -16.0, -1.5);
        assert!(!r.reached && r.gain_db == 0.0);
        assert!(r.after.true_peak_dbtp <= -1.5, "{r:?}");
        let mut z = vec![0f32; 9600];
        let r = normalize_static(&mut z, 2, FS, -16.0, -1.5);
        let rec = crate::export_record::LoudnessRecord::from(&r);
        assert_eq!(rec.integrated, None);
        let back: crate::export_record::LoudnessRecord =
            serde_json::from_slice(&serde_json::to_vec(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
    }

    #[test]
    fn an_unreachable_target_is_reported_with_the_gain_applied() {
        // Sparse full-scale pulses: −1.5 dBTP caps the loudness well below −12 LUFS.
        let mut s = vec![0f32; 2 * FS as usize * 4];
        for k in (0..s.len() / 2).step_by(12_000) {
            for j in 0..480 {
                let v =
                    (std::f64::consts::TAU * 1000.0 * j as f64 / f64::from(FS)).sin() as f32 * 0.9;
                s[2 * (k + j)] = v;
                s[2 * (k + j) + 1] = v;
            }
        }
        let orig = s.clone();
        let r = normalize_static(&mut s, 2, FS, -6.0, -1.5);
        assert!(!r.reached, "{r:?}");
        assert!(r.limiter.deepest_db >= -MAX_LIMIT_DB - 3.0, "{r:?}");
        assert!(r.after.true_peak_dbtp <= -1.5);
        // The reported gain is the one applied: re-running it reproduces the output.
        let mut again = orig;
        apply_gain(&mut again, r.gain_db);
        limit(&mut again, 2, FS, -1.5);
        assert_eq!(again, s);
    }
}
