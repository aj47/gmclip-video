//! Word edges on the voice track (SPEC §9 "Word edges", §4.1.3 anchors).
//!
//! ASR word times are a coarse grid (Parakeet's encoder steps are 80 ms, and abutting
//! words are mis-timed by 0.1–0.3 s), so edges are measured on the waveform:
//!
//! - **`audible_start/end`**: from the word's loudest 10 ms frame, the first point
//!   where the level has dropped 18 dB below that peak and stays down for 60 ms
//!   (forward for the end, backward for the start). The scan never crosses a
//!   neighbouring word's peak; when it finds no drop (abutting words) the ASR time is
//!   returned with `method: AsrFallback`.
//! - **`cut_point{energy_min}`**: the quietest 10 ms window (5 ms hop) in
//!   `[w1.end − 40 ms, w2.start + 40 ms]`, ties to the earliest.
//! - The arbiter's candidates: the 3 lowest local minima in
//!   `[w1.end − 150 ms, w2.start + 150 ms]` (the ASR check is gmclip-ml's).
//!
//! All energies here are [`Energy::fine`] frames (10 ms windows, 5 ms hop).

use crate::energy::Energy;
use crate::span::{Span, ms};

/// Level drop that ends (or, backwards, starts) a word.
pub const DROP_DB: f32 = 18.0;
/// How long the drop must hold.
pub const HOLD_MS: u32 = 60;
/// `energy_min` search margin around the gap.
pub const ENERGY_MIN_MARGIN_MS: u32 = 40;
/// Arbiter candidate search margin around the gap.
pub const ARBITER_MARGIN_MS: u32 = 150;
/// Words closer than this are "abutting" and go to the arbiter (§4.1.3 R-ANC-2).
pub const ABUTTING_GAP_MS: u32 = 300;
/// The farthest an audible edge is looked for past the ASR edge.
pub const MAX_SCAN_MS: u32 = 1_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EdgeMethod {
    /// Measured on the waveform (18 dB drop held 60 ms).
    Measured,
    /// No drop found before the neighbouring word: the ASR edge, unchanged.
    AsrFallback,
}

/// One audible edge, in samples of the analysed buffer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AudibleEdge {
    pub sample: usize,
    pub method: EdgeMethod,
    pub peak_db: f32,
}

/// A word's loudest frame inside its ASR span.
pub fn peak_frame(fine: &Energy, word: Span) -> Option<(usize, f32)> {
    let r = fine.frames_in(word);
    let start = r.start;
    fine.db
        .get(r)?
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)))
        .map(|(i, &d)| (start + i, d))
}

/// Consecutive frames whose windows together span the 60 ms hold:
/// `(k − 1)·hop + window ≥ 60 ms` (11 frames at 10 ms / 5 ms).
fn hold_frames(fine: &Energy) -> usize {
    ms(HOLD_MS, fine.rate)
        .saturating_sub(fine.window)
        .div_ceil(fine.hop)
        + 1
}

/// The end of `word`'s sound. The scan stops before `limit` (a sample: the next word's
/// peak, or the end of the analysed span).
pub fn audible_end(fine: &Energy, word: Span, limit: usize) -> Option<AudibleEdge> {
    let (p, peak_db) = peak_frame(fine, word)?;
    let thr = peak_db - DROP_DB;
    let hold = hold_frames(fine);
    let limit = limit.min(word.end + ms(MAX_SCAN_MS, fine.rate));
    let n = fine.db.len();
    let mut i = p + 1;
    while i < n && fine.frame_start(i) < limit {
        if fine.db[i] < thr {
            let held = (i..(i + hold).min(n)).all(|j| fine.db[j] < thr);
            if held {
                return Some(AudibleEdge {
                    sample: fine.frame_start(i),
                    method: EdgeMethod::Measured,
                    peak_db,
                });
            }
        }
        i += 1;
    }
    Some(AudibleEdge {
        sample: word.end,
        method: EdgeMethod::AsrFallback,
        peak_db,
    })
}

/// The start of `word`'s sound. The scan stops at `limit` (the previous word's peak, or
/// the start of the analysed span).
pub fn audible_start(fine: &Energy, word: Span, limit: usize) -> Option<AudibleEdge> {
    let (p, peak_db) = peak_frame(fine, word)?;
    let thr = peak_db - DROP_DB;
    let hold = hold_frames(fine);
    let limit = limit.max(word.start.saturating_sub(ms(MAX_SCAN_MS, fine.rate)));
    let mut i = p;
    while i > 0 {
        i -= 1;
        if fine.frame_start(i) < limit {
            break;
        }
        if fine.db[i] < thr {
            let held = (i.saturating_sub(hold - 1)..=i).all(|j| fine.db[j] < thr);
            if held {
                return Some(AudibleEdge {
                    sample: fine.frame_start(i) + fine.window,
                    method: EdgeMethod::Measured,
                    peak_db,
                });
            }
        }
    }
    Some(AudibleEdge {
        sample: word.start,
        method: EdgeMethod::AsrFallback,
        peak_db,
    })
}

/// `(audible_start, audible_end)` for every word of a transcript, each scan bounded by
/// its neighbours' peaks.
pub fn audible_edges(
    fine: &Energy,
    words: &[Span],
    span: Span,
) -> Vec<Option<(AudibleEdge, AudibleEdge)>> {
    let peaks: Vec<Option<usize>> = words
        .iter()
        .map(|w| peak_frame(fine, *w).map(|(i, _)| fine.frame_centre(i)))
        .collect();
    (0..words.len())
        .map(|k| {
            let prev = k
                .checked_sub(1)
                .and_then(|j| peaks[j])
                .unwrap_or(span.start);
            let next = peaks.get(k + 1).copied().flatten().unwrap_or(span.end);
            let s = audible_start(fine, words[k], prev)?;
            let e = audible_end(fine, words[k], next)?;
            Some((s, e))
        })
        .collect()
}

/// Kept before a part's first voice (decision 0049).
pub const LEAD_MS: u32 = 40;
/// Kept after a part's last voice: a soft ending's decay. Breaths and room past it go.
pub const TAIL_MS: u32 = 120;
/// A quiet first word ("I", "So") keeps its softer onset when that runs into the voice
/// from at most this far before it.
pub const SOFT_ONSET_MS: u32 = 250;

/// Where a part whose voice starts at `voice` (a sample, an `audible_start`) starts
/// (decision 0049): 40 ms before the softer onset straight before the voice, else before
/// the voice. The softer onset is the run of frames above the room (`quiet_db`, the quiet
/// grid's threshold) that leads into the voice without a break; a run longer than 250 ms
/// is a breath or noise, not the word, and is left out.
pub fn kept_start(fine: &Energy, voice: usize, quiet_db: f32) -> usize {
    let reach = ms(SOFT_ONSET_MS, fine.rate);
    let mut onset = voice;
    // frames ending at or before the voice, latest first
    let mut i = voice.saturating_sub(fine.window) / fine.hop.max(1);
    while let Some(&d) = fine.db.get(i) {
        let at = fine.frame_start(i);
        if d <= quiet_db {
            break;
        }
        if voice.saturating_sub(at) > reach {
            // sound further back than a soft onset runs
            onset = voice;
            break;
        }
        onset = at;
        if i == 0 {
            break;
        }
        i -= 1;
    }
    onset.min(voice).saturating_sub(ms(LEAD_MS, fine.rate))
}

/// Where a part whose voice ends at `voice` (an `audible_end`) ends: 120 ms after it.
pub fn kept_end(fine: &Energy, voice: usize) -> usize {
    voice + ms(TAIL_MS, fine.rate)
}

/// How a cut point was found (§4.1.3 `rule`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CutMethod {
    EnergyMin,
    Arbiter,
}

/// A `cut_point` result: `{value, method, window, rms_db}` (§9).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EnergyMin {
    pub value: usize,
    pub method: CutMethod,
    pub window: Span,
    pub rms_db: f32,
}

fn window_frames(fine: &Energy, window: Span) -> std::ops::Range<usize> {
    // frames whose centre lies in the window
    let half = fine.window / 2;
    let first = window.start.saturating_sub(half).div_ceil(fine.hop);
    let last_excl = if window.end > half {
        (window.end - half - 1) / fine.hop + 1
    } else {
        0
    };
    first.min(fine.db.len())..last_excl.min(fine.db.len())
}

fn search_window(fine: &Energy, w1_end: usize, w2_start: usize, margin_ms: u32) -> Span {
    let m = ms(margin_ms, fine.rate);
    let (a, b) = if w1_end <= w2_start {
        (w1_end, w2_start)
    } else {
        (w2_start, w1_end)
    };
    Span::new(a.saturating_sub(m), b + m)
}

/// `cut_point{energy_min}` between a word ending at `w1_end` and one starting at
/// `w2_start`.
pub fn energy_min(fine: &Energy, w1_end: usize, w2_start: usize) -> Option<EnergyMin> {
    let window = search_window(fine, w1_end, w2_start, ENERGY_MIN_MARGIN_MS);
    let r = window_frames(fine, window);
    let start = r.start;
    let (i, &d) = fine
        .db
        .get(r)?
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1).then(a.0.cmp(&b.0)))?;
    Some(EnergyMin {
        value: fine.frame_centre(start + i),
        method: CutMethod::EnergyMin,
        window,
        rms_db: d,
    })
}

/// The arbiter's candidates: the `k` lowest local minima (frame centres) in
/// `[w1.end − 150 ms, w2.start + 150 ms]`, lowest first, ties to the earliest. A
/// plateau counts once, at its first frame. A frame at the window's edge is a minimum
/// only if the frame just outside the window is not lower (a slope running out of the
/// window is not a minimum inside it).
pub fn arbiter_candidates(
    fine: &Energy,
    w1_end: usize,
    w2_start: usize,
    k: usize,
) -> (Span, Vec<(usize, f32)>) {
    let window = search_window(fine, w1_end, w2_start, ARBITER_MARGIN_MS);
    let r = window_frames(fine, window);
    let db = &fine.db;
    let mut mins: Vec<(usize, f32)> = Vec::new();
    let mut i = r.start;
    while i < r.end {
        // the whole plateau this frame belongs to, even where it runs out of the window
        let mut ps = i;
        while ps > 0 && db[ps - 1] == db[i] {
            ps -= 1;
        }
        let mut j = i;
        while j + 1 < db.len() && db[j + 1] == db[i] {
            j += 1;
        }
        let left_ok = ps == 0 || db[ps - 1] > db[i];
        let right_ok = j + 1 >= db.len() || db[j + 1] > db[i];
        if left_ok && right_ok {
            // counted once, at its first frame inside the window
            mins.push((i, db[i]));
        }
        i = j + 1;
    }
    mins.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    mins.truncate(k);
    let out = mins
        .into_iter()
        .map(|(i, d)| (fine.frame_centre(i), d))
        .collect();
    (window, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    /// A tone burst at `db` dBFS RMS over `[a_ms, b_ms)`, on a −70 dBFS bed.
    fn signal(len_ms: u32, bursts: &[(u32, u32, f32)]) -> Vec<f32> {
        let mut x: Vec<f32> = (0..ms(len_ms, RATE))
            .map(|i| 10f32.powf(-70.0 / 20.0) * 2f32.sqrt() * (i as f32 * 0.9).sin())
            .collect();
        for &(a, b, db) in bursts {
            let amp = 10f32.powf(db / 20.0) * 2f32.sqrt();
            for (i, v) in x.iter_mut().enumerate().take(ms(b, RATE)).skip(ms(a, RATE)) {
                *v = amp * (2.0 * std::f32::consts::PI * 180.0 * i as f32 / RATE as f32).sin();
            }
        }
        x
    }

    fn at(ms_: u32) -> usize {
        ms(ms_, RATE)
    }

    #[test]
    fn synthetic_onsets_and_offsets_within_5_ms() {
        // Words at known edges; ASR spans off by up to ±120 ms.
        for (onset, offset) in [(300u32, 700u32), (303, 707), (311, 689), (1000, 1420)] {
            let x = signal(2000, &[(onset, offset, -18.0)]);
            let fine = Energy::fine(&x, RATE).unwrap();
            let asr = Span::new(at(onset + 80), at(offset - 120));
            let s = audible_start(&fine, asr, 0).unwrap();
            let e = audible_end(&fine, asr, x.len()).unwrap();
            assert_eq!(s.method, EdgeMethod::Measured);
            assert_eq!(e.method, EdgeMethod::Measured);
            assert!(
                s.sample.abs_diff(at(onset)) <= at(5),
                "start {} vs {onset} ms",
                s.sample
            );
            assert!(
                e.sample.abs_diff(at(offset)) <= at(5),
                "end {} vs {offset} ms",
                e.sample
            );
        }
    }

    #[test]
    fn parts_keep_the_voice_and_a_soft_onset_straight_before_it() {
        // a quiet onset (−40 dB, under the word's 18 dB drop, over the room) 150 ms before
        // the voice is kept; one 400 ms long is a breath and goes
        let voice_at = |x: &[f32]| {
            let fine = Energy::fine(x, RATE).unwrap();
            let s = audible_start(&fine, Span::new(at(620), at(900)), 0).unwrap();
            assert_eq!(s.method, EdgeMethod::Measured);
            (fine, s.sample)
        };
        let (fine, v) = voice_at(&signal(2000, &[(600, 1000, -18.0)]));
        assert!(v.abs_diff(at(600)) <= at(5), "{v}");
        assert_eq!(kept_start(&fine, v, -48.0), v - at(40));
        assert_eq!(kept_end(&fine, at(1000)), at(1120));
        let (fine, v) = voice_at(&signal(2000, &[(450, 600, -40.0), (600, 1000, -18.0)]));
        let k = kept_start(&fine, v, -48.0);
        assert!(k.abs_diff(at(410)) <= at(10), "soft onset kept: {k}");
        let (fine, v) = voice_at(&signal(2000, &[(200, 600, -40.0), (600, 1000, -18.0)]));
        assert_eq!(
            kept_start(&fine, v, -48.0),
            v - at(40),
            "a breath is not kept"
        );
    }

    #[test]
    fn a_short_dip_is_not_an_end() {
        // 30 ms dip (a stop consonant) inside the word: the end is after it.
        let x = signal(2000, &[(300, 600, -18.0), (630, 900, -20.0)]);
        let fine = Energy::fine(&x, RATE).unwrap();
        let e = audible_end(&fine, Span::new(at(320), at(580)), x.len()).unwrap();
        assert!(e.sample.abs_diff(at(900)) <= at(5), "{}", e.sample);
    }

    #[test]
    fn abutting_words_fall_back_to_asr() {
        // no 18 dB drop between the two words: the scan stops at the next peak
        let x = signal(2000, &[(300, 600, -18.0), (600, 900, -22.0)]);
        let fine = Energy::fine(&x, RATE).unwrap();
        let words = [Span::new(at(320), at(590)), Span::new(at(610), at(880))];
        let edges = audible_edges(&fine, &words, Span::new(0, x.len()));
        let (_, e1) = edges[0].unwrap();
        assert_eq!(e1.method, EdgeMethod::AsrFallback);
        assert_eq!(e1.sample, at(590));
        let (_, e2) = edges[1].unwrap();
        assert_eq!(e2.method, EdgeMethod::Measured);
    }

    #[test]
    fn energy_min_finds_the_gap_and_ties_go_early() {
        let x = signal(2000, &[(300, 600, -18.0), (650, 900, -18.0)]);
        let fine = Energy::fine(&x, RATE).unwrap();
        let m = energy_min(&fine, at(590), at(660)).unwrap();
        assert!(m.value >= at(600) && m.value <= at(650), "{}", m.value);
        assert!(m.rms_db < -60.0);
        assert_eq!(m.window, Span::new(at(550), at(700)));
        // flat bed: the earliest frame wins
        let flat = Energy::compute(&vec![0.0; 16_000], RATE, 160, 80).unwrap();
        let m = energy_min(&flat, at(500), at(520)).unwrap();
        assert_eq!(m.value, at(460));
    }

    #[test]
    fn arbiter_candidates_are_local_minima() {
        // two dips: one deep, one shallow, in the ±150 ms window
        let x = signal(
            2000,
            &[(300, 560, -18.0), (580, 640, -30.0), (660, 900, -18.0)],
        );
        let fine = Energy::fine(&x, RATE).unwrap();
        let (w, c) = arbiter_candidates(&fine, at(600), at(620), 3);
        assert_eq!(w, Span::new(at(450), at(770)));
        assert!(!c.is_empty() && c.len() <= 3);
        // sorted by level
        assert!(c.windows(2).all(|p| p[0].1 <= p[1].1));
        // a silent plateau wider than the window is still one candidate
        let flat = Energy::compute(&vec![0.0; 16_000], RATE, 160, 80).unwrap();
        let (_, c0) = arbiter_candidates(&flat, at(500), at(520), 3);
        assert_eq!(c0.len(), 1);
        // a slope running out of the window is not a minimum inside it
        let mut slope = Energy::compute(&vec![0.0; 16_000], RATE, 160, 80).unwrap();
        for (i, d) in slope.db.iter_mut().enumerate() {
            *d = -(i as f32);
        }
        let (_, c1) = arbiter_candidates(&slope, at(500), at(520), 3);
        assert!(c1.is_empty(), "{c1:?}");
        // the lowest is in one of the two −70 dB gaps
        let lo = c[0].0;
        assert!(
            (at(560)..=at(580)).contains(&lo) || (at(640)..=at(660)).contains(&lo),
            "{lo}"
        );
    }
}
