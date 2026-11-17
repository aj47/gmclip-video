//! `cut_point{arbiter}` (SPEC §9 Word edges, §4.1.3 R-ANC-2, decision 0006): the cut
//! between two abutting words (gap < 0.3 s), decided by **forced alignment**.
//!
//! 1. Candidates: the 5 lowest local minima of the 10 ms / 5 ms energy in
//!    `[w1.end − 150 ms, w2.start + 150 ms]` plus the `energy_min` point.
//! 2. The model's own tokens for the words around the edge: one TDT greedy decode of a
//!    wide region (±5 s), the words matched to w1 and w2 by time.
//! 3. Each candidate *c* is scored `log P(left words | audio [w1.start − 2 s, c)) +
//!    log P(right words | audio [c, w2.end + 2 s))` with the TDT forward algorithm
//!    ([`crate::tdt::TdtAligner::align_ll`]). A cut inside w1 makes w1's last sounds
//!    missing on the left; a cut inside w2 leaves w2's first sounds on the left (where
//!    no token explains them) and removes them on the right: both lower the score. The
//!    best-scoring candidate wins; its margin over the runner-up is reported.
//! 4. `Unresolvable` ("unresolvable edge", a lint upstream) when the words are not heard
//!    around the edge or no candidate aligns; callers fall back to `energy_min`.
//!
//! Both sides' outer edges are fixed, so every candidate's total covers the same audio
//! and the same tokens; only the cut moves. Known limits: the encoder's 80 ms frames are
//! coarser than the candidates' spacing (totals within [`TIE_NATS`] are a tie, won by the
//! quietest), and NeMo's preprocessor normalises features over each side's audio, so a
//! side's statistics shift slightly with the cut.
//!
//! This replaces SPEC v5's free decoding of short probes, which Parakeet v3 does
//! erratically (empty or truncated decodes from a few dB or 250 ms of change; 14 of 150
//! abutting edges resolved). Alignment never asks the model to *find* the words, only
//! how well the known words fit each side.

use gmclip_analysis::edges::{self, arbiter_candidates, energy_min};
use gmclip_analysis::energy::Energy;
use gmclip_analysis::fillers::normalise;
use gmclip_analysis::pcm::VOICE_RATE;
use gmclip_analysis::span::{Span, ms};

use crate::parakeet::ParakeetAsr;
use crate::tdt::{TdtAligner, words_of};
use crate::wer::char_distance;
use crate::{CancelToken, MlError};

/// How the arbiter cuts its audio.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ArbiterParams {
    /// Audio before w1 (left side) and after w2 (right side) that is scored.
    pub context_ms: u32,
    /// Extra audio either side of that for the decode that finds the tokens (Parakeet
    /// drops words near a window's edge).
    pub decode_pad_ms: u32,
    /// Energy minima tried.
    pub candidates: usize,
}

impl Default for ArbiterParams {
    fn default() -> Self {
        Self {
            context_ms: 2_000,
            decode_pad_ms: 3_000,
            candidates: 5,
        }
    }
}

/// A word of the pair, in samples of the 16 kHz voice buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PairWord<'a> {
    pub text: &'a str,
    pub span: Span,
}

/// One candidate and its alignment scores (natural log).
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub at: usize,
    pub rms_db: f32,
    pub ll_left: f64,
    pub ll_right: f64,
}

impl Scored {
    pub fn total(&self) -> f64 {
        self.ll_left + self.ll_right
    }
}

/// The arbiter's answer: `{value, method: arbiter, window, rms_db}` plus its working.
#[derive(Clone, Debug, PartialEq)]
pub struct ArbiterEdge {
    pub value: usize,
    pub window: Span,
    pub rms_db: f32,
    /// Best total minus the best total outside the tie (∞ when every candidate ties).
    pub margin: f64,
    pub scored: Vec<Scored>,
    /// The words the two sides were aligned to.
    pub left_words: Vec<String>,
    pub right_words: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ArbiterError {
    /// No candidate could be scored (`Invalid{reason:"unresolvable edge"}`).
    #[error("unresolvable edge: {reason}")]
    Unresolvable {
        window: Span,
        reason: &'static str,
        scored: Vec<Scored>,
    },
    #[error("{0}")]
    Ml(#[from] MlError),
}

/// Whether two words match: normalised, character edit distance ≤ 1. Words of up to 3
/// characters must match exactly: at distance 1 "a" = "I" and "of" = "on".
pub fn same_word(a: &str, b: &str) -> bool {
    let (a, b) = (normalise(a), normalise(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.chars().count().min(b.chars().count()) <= 3 {
        return a == b;
    }
    char_distance(&a, &b) <= 1
}

/// Whether the gap between two words makes them "abutting" (< 0.3 s): the case
/// [`arbitrate`] is for; wider gaps use `energy_min`.
pub fn is_abutting(w1_end: usize, w2_start: usize) -> bool {
    w2_start.saturating_sub(w1_end) < ms(edges::ABUTTING_GAP_MS, VOICE_RATE)
}

/// Alignment totals closer than this to the best count as a tie.
pub const TIE_NATS: f64 = 0.5;

/// Picks the best of `cands` (sample, rms dB) by `score(c) → (ll_left, ll_right)`;
/// totals within [`TIE_NATS`] of the best are a tie, won by the quietest. The margin is
/// the best total minus the best total outside the tie (∞ if none). Candidates whose
/// score is not finite are kept in the working but cannot win.
pub fn choose(
    window: Span,
    cands: &[(usize, f32)],
    mut score: impl FnMut(usize) -> Result<(f64, f64), MlError>,
    cancel: &CancelToken,
) -> Result<(Scored, f64, Vec<Scored>), ArbiterError> {
    let mut scored = Vec::with_capacity(cands.len());
    for &(c, db) in cands {
        if cancel.is_cancelled() {
            return Err(MlError::Cancelled.into());
        }
        let (ll_left, ll_right) = score(c)?;
        scored.push(Scored {
            at: c,
            rms_db: db,
            ll_left,
            ll_right,
        });
    }
    let mut ranked: Vec<&Scored> = scored.iter().filter(|s| s.total().is_finite()).collect();
    // best total first; ties to the quieter, then the earlier
    ranked.sort_by(|a, b| {
        b.total()
            .total_cmp(&a.total())
            .then(a.rms_db.total_cmp(&b.rms_db))
            .then(a.at.cmp(&b.at))
    });
    let Some(top) = ranked.first().map(|s| s.total()) else {
        return Err(ArbiterError::Unresolvable {
            window,
            reason: "no candidate aligns",
            scored,
        });
    };
    // Totals within TIE_NATS of the best are a tie (candidates 5–40 ms apart share an
    // 80 ms encoder frame): the quietest of them wins, then the earliest.
    let (tied, rest): (Vec<&Scored>, Vec<&Scored>) =
        ranked.iter().partition(|s| s.total() >= top - TIE_NATS);
    let best = tied
        .iter()
        .min_by(|a, b| a.rms_db.total_cmp(&b.rms_db).then(a.at.cmp(&b.at)))
        .map(|s| (*s).clone())
        .ok_or(MlError::Invalid("no candidate"))?;
    let margin = rest.first().map_or(f64::INFINITY, |s| top - s.total());
    Ok((best, margin, scored))
}

/// Decides the cut between `w1` and `w2` on `pcm` (16 kHz mono voice track; `fine` its
/// [`Energy::fine`]). The audio is heard at one input gain measured over all of `pcm`.
pub fn arbitrate(
    tdt: &TdtAligner,
    pcm: &[f32],
    fine: &Energy,
    w1: PairWord<'_>,
    w2: PairWord<'_>,
    params: &ArbiterParams,
    cancel: &CancelToken,
) -> Result<ArbiterEdge, ArbiterError> {
    if fine.rate != VOICE_RATE {
        return Err(MlError::Invalid("arbiter energy must be at 16 kHz").into());
    }
    if w1.span.start > w2.span.end || w2.span.end > pcm.len() || w1.span.is_empty() {
        return Err(MlError::Invalid("words outside the buffer").into());
    }
    let gain = ParakeetAsr::speech_gain(pcm);
    let gained = |a: usize, b: usize| -> Vec<f32> { pcm[a..b].iter().map(|v| v * gain).collect() };
    let (window, mut cands) =
        arbiter_candidates(fine, w1.span.end, w2.span.start, params.candidates);
    if let Some(m) = energy_min(fine, w1.span.end, w2.span.start)
        && !cands
            .iter()
            .any(|c| c.0.abs_diff(m.value) < ms(5, VOICE_RATE))
    {
        cands.push((m.value, m.rms_db));
    }
    let unresolvable = |reason| ArbiterError::Unresolvable {
        window,
        reason,
        scored: Vec::new(),
    };

    // the model's own tokens for the words around the edge
    let ctx = ms(params.context_ms, VOICE_RATE);
    let pad = ms(params.decode_pad_ms, VOICE_RATE);
    let dec_from = w1.span.start.saturating_sub(ctx + pad);
    let dec_to = (w2.span.end + ctx + pad).min(pcm.len());
    if dec_to - dec_from > 30 * VOICE_RATE as usize {
        return Err(unresolvable("edge region longer than 30 s"));
    }
    let enc = tdt.encode(&gained(dec_from, dec_to))?;
    let hits = tdt.greedy(&enc)?;
    let words: Vec<(String, Vec<u32>, usize, usize)> = words_of(|id| tdt.piece(id), &hits)
        .into_iter()
        .map(|(t, ids, a, b)| (t, ids, a + dec_from, b + dec_from))
        .collect();
    let overlap = |w: &(String, Vec<u32>, usize, usize), s: Span| {
        w.3.min(s.end).saturating_sub(w.2.max(s.start))
    };
    // w1: the decoded word overlapping it most (a same-spelled one preferred), never a
    // punctuation-only token; w2 must be the next word and must be w2 (by time or text),
    // so a merge ("alright" for "all right"), a split or a dropped word is refused
    // rather than scored as an edge that is not there.
    let real = |i: usize| !normalise(&words[i].0).is_empty();
    let i1 = (0..words.len())
        .filter(|&i| real(i) && overlap(&words[i], w1.span) > 0)
        .max_by_key(|&i| (same_word(&words[i].0, w1.text), overlap(&words[i], w1.span)))
        .ok_or(unresolvable("word 1 not heard"))?;
    if overlap(&words[i1], w2.span) > overlap(&words[i1], w1.span) {
        return Err(unresolvable("word 1 not heard"));
    }
    let i2 = (i1 + 1..words.len())
        .find(|&j| real(j))
        .filter(|&j| overlap(&words[j], w2.span) > 0 || same_word(&words[j].0, w2.text))
        .ok_or(unresolvable("word 2 not heard"))?;
    let left_from = w1.span.start.saturating_sub(ctx).max(dec_from);
    let right_to = (w2.span.end + ctx).min(dec_to);
    let left: Vec<usize> = (0..=i1)
        .filter(|&j| j == i1 || words[j].2 >= left_from)
        .collect();
    let right: Vec<usize> = (i2..words.len())
        .filter(|&j| j == i2 || words[j].3 <= right_to)
        .collect();
    let left_tokens: Vec<u32> = left
        .iter()
        .flat_map(|&j| words[j].1.iter().copied())
        .collect();
    let right_tokens: Vec<u32> = right
        .iter()
        .flat_map(|&j| words[j].1.iter().copied())
        .collect();

    // (only bites near the buffer's ends) both sides need at least 0.2 s of audio
    let margin_samples = ms(200, VOICE_RATE);
    cands.retain(|&(c, _)| c >= left_from + margin_samples && c + margin_samples <= right_to);
    let frames = (right_to - left_from) / crate::tdt::FRAME_SAMPLES + 1;
    let width = tdt.blank() as usize + 1 + crate::tdt::DURATIONS.len();
    if frames * (left_tokens.len().max(right_tokens.len()) + 1) * width > crate::tdt::MAX_GRID {
        return Err(unresolvable("edge region too long to align"));
    }
    let (best, margin, scored) = choose(
        window,
        &cands,
        |c| {
            let l = tdt.align_ll(&tdt.encode(&gained(left_from, c))?, &left_tokens)?;
            let r = tdt.align_ll(&tdt.encode(&gained(c, right_to))?, &right_tokens)?;
            Ok((l, r))
        },
        cancel,
    )?;
    Ok(ArbiterEdge {
        value: best.at,
        window,
        rms_db: best.rms_db,
        margin,
        scored,
        left_words: left.iter().map(|&j| words[j].0.clone()).collect(),
        right_words: right.iter().map(|&j| words[j].0.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_total_wins_and_the_margin_is_reported() {
        let w = Span::new(0, 100);
        let cands = [(10, -70.0), (20, -60.0), (30, -50.0)];
        // the deepest minimum cuts word 1 (left score low); the second is the boundary
        let (best, margin, scored) = choose(
            w,
            &cands,
            |c| {
                Ok(match c {
                    10 => (-30.0, -5.0),
                    20 => (-6.0, -5.5),
                    _ => (-6.0, -20.0),
                })
            },
            &CancelToken::default(),
        )
        .unwrap();
        assert_eq!(best.at, 20);
        assert!((margin - 14.5).abs() < 1e-9);
        assert_eq!(scored.len(), 3);
        // ties (within 0.5 nats) go to the quieter candidate
        let (best, margin, _) = choose(
            w,
            &cands,
            |c| Ok(if c == 30 { (-1.0, -1.0) } else { (-1.2, -1.0) }),
            &CancelToken::default(),
        )
        .unwrap();
        assert_eq!(best.at, 10);
        assert_eq!(margin, f64::INFINITY);
    }

    #[test]
    fn nothing_aligning_is_unresolvable() {
        let w = Span::new(0, 100);
        let r = choose(
            w,
            &[(10, -70.0)],
            |_| Ok((f64::NEG_INFINITY, -1.0)),
            &CancelToken::default(),
        );
        assert!(matches!(r, Err(ArbiterError::Unresolvable { .. })));
        let r = choose(w, &[], |_| Ok((0.0, 0.0)), &CancelToken::default());
        assert!(matches!(r, Err(ArbiterError::Unresolvable { .. })));
        let c = CancelToken::default();
        c.cancel();
        assert!(matches!(
            choose(w, &[(1, 0.0)], |_| Ok((0.0, 0.0)), &c),
            Err(ArbiterError::Ml(MlError::Cancelled))
        ));
    }

    #[test]
    fn word_matching() {
        assert!(same_word("Banana.", "banana"));
        assert!(same_word("bananas", "banana"));
        assert!(!same_word("a", "I"));
        assert!(!same_word("of", "on"));
        assert!(same_word("It,", "it"));
        assert!(!same_word("band", "banana"));
        assert!(!same_word(",", ""));
        let at = |m| ms(m, VOICE_RATE);
        assert!(is_abutting(at(500), at(790)));
        assert!(!is_abutting(at(500), at(800)));
    }
}
