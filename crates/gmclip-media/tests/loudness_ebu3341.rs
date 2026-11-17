//! EBU Tech 3341 (2023) minimum-requirements test signals, synthesised here (B-12).
//! Cases 7 and 8 are authentic programme files (not synthesisable) and 11 and 14 are
//! for live meters (their file-based twins 10 and 13 are run). Tolerances per Table 1:
//! ±0.1 LU for loudness, +0.2/−0.4 dB for true peak.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use gmclip_media::audio::loudness::{
    integrated, max_momentary, max_short_term, momentary, short_term, true_peak,
};

const FS: u32 = 48_000;

fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

/// 1 kHz tone segments `(seconds, dBFS peak or None for silence)`, `ch` identical
/// channels, phase continuous.
fn tones(segments: &[(f64, Option<f64>)], ch: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut n = 0u64;
    for &(secs, level) in segments {
        let len = (secs * f64::from(FS)).round() as u64;
        for _ in 0..len {
            let v = level.map_or(0.0, |l| {
                db(l) * (std::f64::consts::TAU * 1000.0 * n as f64 / f64::from(FS)).sin()
            });
            out.extend(std::iter::repeat_n(v as f32, ch));
            n += 1;
        }
    }
    out
}

fn near(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: {got:.3} vs {want}");
}

#[test]
fn case_1_and_2_steady_tones() {
    for level in [-23.0, -33.0] {
        let s = tones(&[(20.0, Some(level))], 2);
        near(integrated(&s, 2, FS), level, 0.1, "I");
        let m = momentary(&s, 2, FS);
        let st = short_term(&s, 2, FS);
        assert!(m.iter().all(|&x| (x - level).abs() <= 0.1), "M");
        assert!(st.iter().all(|&x| (x - level).abs() <= 0.1), "S");
    }
}

#[test]
fn case_3_4_5_gating() {
    let c3 = tones(
        &[
            (10.0, Some(-36.0)),
            (60.0, Some(-23.0)),
            (10.0, Some(-36.0)),
        ],
        2,
    );
    near(integrated(&c3, 2, FS), -23.0, 0.1, "case 3");
    let c4 = tones(
        &[
            (10.0, Some(-72.0)),
            (10.0, Some(-36.0)),
            (60.0, Some(-23.0)),
            (10.0, Some(-36.0)),
            (10.0, Some(-72.0)),
        ],
        2,
    );
    near(integrated(&c4, 2, FS), -23.0, 0.1, "case 4");
    let c5 = tones(
        &[
            (20.0, Some(-26.0)),
            (20.1, Some(-20.0)),
            (20.0, Some(-26.0)),
        ],
        2,
    );
    near(integrated(&c5, 2, FS), -23.0, 0.1, "case 5");
}

#[test]
fn case_6_five_channels() {
    let n = 20 * FS as usize;
    let levels = [-28.0, -28.0, -24.0, -30.0, -30.0]; // L R C Ls Rs
    let mut s = Vec::with_capacity(n * 5);
    for i in 0..n {
        let x = (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(FS)).sin();
        s.extend(levels.iter().map(|&l| (db(l) * x) as f32));
    }
    near(integrated(&s, 5, FS), -23.0, 0.1, "case 6");
}

#[test]
fn case_9_short_term_constant_after_3s() {
    let seg: Vec<(f64, Option<f64>)> = (0..5)
        .flat_map(|_| [(1.34, Some(-20.0)), (1.66, Some(-30.0))])
        .collect();
    let s = tones(&seg, 2);
    let st = short_term(&s, 2, FS);
    // Window k ends at (k + 30) × 100 ms; every value from 3 s on.
    assert!(!st.is_empty());
    for (k, &x) in st.iter().enumerate() {
        near(
            x,
            -23.0,
            0.1,
            &format!("S at {:.1} s", (k + 30) as f64 / 10.0),
        );
    }
}

#[test]
fn case_10_max_short_term_per_segment() {
    for i in 0..20 {
        let s = tones(
            &[(i as f64 * 0.15, None), (3.0, Some(-23.0)), (1.0, None)],
            2,
        );
        let max = max_short_term(&s, 2, FS);
        near(max, -23.0, 0.1, &format!("segment {i}"));
    }
}

#[test]
fn case_12_momentary_constant_after_1s() {
    let seg: Vec<(f64, Option<f64>)> = (0..25)
        .flat_map(|_| [(0.18, Some(-20.0)), (0.22, Some(-30.0))])
        .collect();
    let s = tones(&seg, 2);
    let m = momentary(&s, 2, FS);
    for (k, &x) in m.iter().enumerate().skip(6) {
        near(
            x,
            -23.0,
            0.1,
            &format!("M at {:.1} s", (k + 4) as f64 / 10.0),
        );
    }
}

#[test]
fn case_13_max_momentary_per_segment() {
    for i in 0..20 {
        let s = tones(
            &[(i as f64 * 0.02, None), (0.4, Some(-23.0)), (1.0, None)],
            2,
        );
        let max = max_momentary(&s, 2, FS);
        near(max, -23.0, 0.1, &format!("segment {i}"));
    }
}

/// A stereo sine at `freq` (Hz), amplitude `amp` (FFS), starting phase `deg`, 1 s, with
/// 10 ms linear fades (Tech 3341 cases 15–19).
fn tp_sine(freq: f64, amp: f64, deg: f64) -> Vec<f32> {
    let n = FS as usize;
    let fade = FS as usize / 100;
    (0..n)
        .flat_map(|i| {
            let env = (i.min(n - 1 - i) as f64 / fade as f64).min(1.0);
            let v = (amp
                * env
                * (std::f64::consts::TAU * freq * i as f64 / f64::from(FS) + deg.to_radians())
                    .sin()) as f32;
            [v, v]
        })
        .collect()
}

fn tp_ok(got: f64, want: f64, what: &str) {
    assert!(
        got <= want + 0.2 && got >= want - 0.4,
        "{what}: {got:.3} dBTP, want {want} +0.2/−0.4"
    );
}

#[test]
fn cases_15_to_19_sines() {
    let fs = f64::from(FS);
    tp_ok(true_peak(&tp_sine(fs / 4.0, 0.5, 0.0), 2), -6.0, "case 15");
    tp_ok(true_peak(&tp_sine(fs / 4.0, 0.5, 45.0), 2), -6.0, "case 16");
    tp_ok(true_peak(&tp_sine(fs / 6.0, 0.5, 60.0), 2), -6.0, "case 17");
    tp_ok(true_peak(&tp_sine(fs / 8.0, 0.5, 67.5), 2), -6.0, "case 18");
    tp_ok(true_peak(&tp_sine(fs / 4.0, 1.41, 45.0), 2), 3.0, "case 19");
}

/// Cases 20–23: at 4·fs, an fs/6 sine (0.5 FFS) holding one period of an fs/4 sine (1.0
/// FFS), phase continuous; anti-alias filtered and decimated to fs at offsets 0–3.
/// Returns the four decimated stereo signals and the filtered signal's 4·fs peak.
fn tp_bursts() -> (Vec<Vec<f32>>, f64) {
    let up = 4 * FS as usize;
    let fs4 = f64::from(4 * FS);
    let n = up / 4; // 0.25 s at 192 kHz
    let start = n / 2;
    let burst = 16; // one period of fs/4 = 12 kHz at 192 kHz
    let mut phase = 0.0f64;
    let mut x = Vec::with_capacity(n);
    for i in 0..n {
        let in_burst = (start..start + burst).contains(&i);
        let (f, a) = if in_burst {
            (f64::from(FS) / 4.0, 1.0)
        } else {
            (f64::from(FS) / 6.0, 0.5)
        };
        let fade = (i.min(n - 1 - i) as f64 / (fs4 * 0.01)).min(1.0);
        x.push(a * fade * phase.sin());
        phase += std::f64::consts::TAU * f / fs4;
    }
    // Windowed-sinc low-pass at 20 kHz (Blackman, 1023 taps) at 192 kHz.
    let taps = 1023usize;
    let fc = 20_000.0 / fs4;
    let h: Vec<f64> = (0..taps)
        .map(|k| {
            let m = k as f64 - (taps - 1) as f64 / 2.0;
            let sinc = if m == 0.0 {
                2.0 * fc
            } else {
                (std::f64::consts::TAU * fc * m).sin() / (std::f64::consts::PI * m)
            };
            let w = 0.42 - 0.5 * (std::f64::consts::TAU * k as f64 / (taps - 1) as f64).cos()
                + 0.08 * (2.0 * std::f64::consts::TAU * k as f64 / (taps - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let half = taps / 2;
    // Only the region around the burst matters for the peak; filter it all the same.
    let y: Vec<f64> = (0..n)
        .map(|i| {
            let mut acc = 0.0;
            for (k, hk) in h.iter().enumerate() {
                let j = i as i64 + half as i64 - k as i64;
                if j >= 0 && (j as usize) < n {
                    acc += hk * x[j as usize];
                }
            }
            acc
        })
        .collect();
    let peak = 20.0 * y.iter().fold(0f64, |m, v| m.max(v.abs())).log10();
    let outs = (0..4)
        .map(|o| {
            y.iter()
                .skip(o)
                .step_by(4)
                .flat_map(|&v| [v as f32, v as f32])
                .collect()
        })
        .collect();
    (outs, peak)
}

#[test]
fn cases_20_to_23_inter_sample_bursts() {
    let (signals, peak192) = tp_bursts();
    for (offset, s) in signals.iter().enumerate() {
        let tp = true_peak(s, 2);
        let sample_peak = 20.0 * f64::from(s.iter().fold(0f32, |m, v| m.max(v.abs()))).log10();
        println!(
            "case {}: TP {tp:.3} dBTP, sample peak {sample_peak:.3}, 192 kHz peak {peak192:.3}",
            20 + offset
        );
        tp_ok(tp, 0.0, &format!("case {}", 20 + offset)); // EBU's expectation
        // And the measurement tracks the signal's real inter-sample peak at every offset,
        // including those whose samples miss it.
        assert!(
            (tp - peak192).abs() <= 0.2,
            "case {}: TP {tp} vs 192 kHz peak {peak192}",
            20 + offset
        );
    }
}
