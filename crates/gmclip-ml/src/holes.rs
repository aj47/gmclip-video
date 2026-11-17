//! Holes: speech the decoder heard no words in, and the words a second decode of just
//! that stretch adds back.
//!
//! Parakeet's greedy TDT decode can go blank for the rest of a window: on AJ's
//! 2026-10-19 voice-over (`2026-10-19 12-00-36.mp4`), the import's first window
//! (0–23 s) decoded "…every other harness directly." and nothing more, smearing
//! "directly." over 4.6–12.6 s and dropping a 6 s sentence (12.7–18.6 s) that every
//! decode starting after 8 s hears whole. So a window's sound that no word covers is
//! decoded again on its own, and the words that land in the hole are kept.
//!
//! - Sound: the window's 40 ms quiet grid at its own §5.5 threshold
//!   ([`silence::QuietGrid::for_span`]); sound runs are what it does not call quiet.
//! - Covered: a word's start to its end, at most [`WORD_MAX_S`] long (the decoder
//!   stretches a word's end over the silence or the speech after it), padded by
//!   [`COVER_PAD_S`] each side.
//! - A hole: uncovered sound, runs bridged across quiet gaps up to [`BRIDGE_S`], with at
//!   least [`MIN_SOUND_S`] of sound. It is decoded from [`LEAD_S`] before to [`LEAD_S`]
//!   after.

use gmclip_analysis::energy::{Energy, Hop};
use gmclip_analysis::pcm::VOICE_RATE;
use gmclip_analysis::silence::QuietGrid;
use gmclip_analysis::span::Span;

use crate::stitch::RawWord;

/// The longest a word is taken to cover (s).
pub const WORD_MAX_S: f64 = 1.2;
/// Coverage padding each side of a word (s).
pub const COVER_PAD_S: f64 = 0.2;
/// Uncovered sound runs this close (s) are one hole.
pub const BRIDGE_S: f64 = 0.6;
/// The least sound (s) a hole holds.
pub const MIN_SOUND_S: f64 = 0.8;
/// Context decoded either side of a hole (s).
pub const LEAD_S: f64 = 0.3;

fn samples(s: f64) -> usize {
    (s * f64::from(VOICE_RATE)).round() as usize
}

/// What `words` cover, merged and sorted.
fn covered(words: &[RawWord], len: usize) -> Vec<Span> {
    let (max, pad) = (samples(WORD_MAX_S), samples(COVER_PAD_S));
    let mut c: Vec<Span> = words
        .iter()
        .map(|w| {
            let end = w.span.end.min(w.span.start + max);
            Span::new(w.span.start.saturating_sub(pad), (end + pad).min(len))
        })
        .collect();
    c.sort_by_key(|s| s.start);
    let mut out: Vec<Span> = Vec::new();
    for s in c {
        match out.last_mut() {
            Some(l) if s.start <= l.end => l.end = l.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

/// `run` minus `cover` (both sorted).
fn subtract(run: Span, cover: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut at = run.start;
    for c in cover
        .iter()
        .filter(|c| c.end > run.start && c.start < run.end)
    {
        if c.start > at {
            out.push(Span::new(at, c.start));
        }
        at = at.max(c.end);
    }
    if at < run.end {
        out.push(Span::new(at, run.end));
    }
    out
}

/// The holes in `pcm` (16 kHz) that `words` (window samples) leave, in order.
pub fn find(pcm: &[f32], words: &[RawWord]) -> Vec<Span> {
    let Ok(e) = Energy::at_hop(pcm, VOICE_RATE, Hop::Ms40) else {
        return Vec::new();
    };
    let whole = Span::new(0, pcm.len());
    let Ok(grid) = QuietGrid::for_span(&e, whole) else {
        return Vec::new();
    };
    let cover = covered(words, pcm.len());
    let uncovered: Vec<Span> = grid
        .sound_runs(whole)
        .into_iter()
        .flat_map(|r| subtract(r, &cover))
        .collect();
    // (hole, sound inside it)
    let mut runs: Vec<(Span, usize)> = Vec::new();
    for u in uncovered {
        match runs.last_mut() {
            Some((h, n)) if u.start - h.end <= samples(BRIDGE_S) => {
                h.end = u.end;
                *n += u.len();
            }
            _ => runs.push((u, u.len())),
        }
    }
    runs.into_iter()
        .filter(|(_, n)| *n >= samples(MIN_SOUND_S))
        .map(|(h, _)| h)
        .collect()
}

/// The span decoded for `hole` in a buffer of `len` samples.
pub fn decode_span(hole: Span, len: usize) -> Span {
    let lead = samples(LEAD_S);
    Span::new(hole.start.saturating_sub(lead), (hole.end + lead).min(len))
}

/// Adds the `heard` words (window samples) that start inside `hole` and overlap no word
/// of `words`' coverage; a word whose end runs into an added word is cut at its start.
/// Returns how many were added.
pub fn fill(words: &mut Vec<RawWord>, hole: Span, heard: Vec<RawWord>, len: usize) -> usize {
    let lead = samples(LEAD_S);
    let cover = covered(words, len);
    let added: Vec<RawWord> = heard
        .into_iter()
        .filter(|w| w.span.start + lead >= hole.start && w.span.start < hole.end)
        .filter(|w| {
            let mid = Span::new(
                w.span.start,
                w.span.end.min(w.span.start + samples(WORD_MAX_S)),
            )
            .mid();
            !cover.iter().any(|c| c.start <= mid && mid < c.end)
        })
        .collect();
    let n = added.len();
    for a in &added {
        for w in words.iter_mut() {
            if w.span.start < a.span.start && w.span.end > a.span.start {
                w.span.end = a.span.start;
            }
        }
    }
    words.extend(added);
    words.sort_by_key(|w| (w.span.start, w.span.end));
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: usize = VOICE_RATE as usize;

    /// Silence with a 200 Hz tone over each `(from_s, to_s)`.
    fn tone(len_s: f64, bursts: &[(f64, f64)]) -> Vec<f32> {
        let mut v = vec![0.0001f32; samples(len_s)];
        for &(a, b) in bursts {
            for (i, x) in v.iter_mut().enumerate().take(samples(b)).skip(samples(a)) {
                *x = 0.3 * (i as f32 * 200.0 * std::f32::consts::TAU / SR as f32).sin();
            }
        }
        v
    }

    fn word(text: &str, a: f64, b: f64) -> RawWord {
        RawWord {
            text: text.into(),
            span: Span::new(samples(a), samples(b)),
        }
    }

    fn secs(s: Span) -> (f64, f64) {
        let r = |n: usize| (n as f64 / SR as f64 * 10.0).round() / 10.0;
        (r(s.start), r(s.end))
    }

    #[test]
    fn speech_with_no_words_is_a_hole() {
        // speech 0.5–4.6 s heard; 12.7–18.6 s (two phrases) heard as nothing but the
        // previous word's smeared end
        let pcm = tone(23.0, &[(0.5, 4.6), (4.6, 5.2), (12.7, 15.0), (15.3, 18.6)]);
        let words = vec![word("every", 3.6, 4.0), word("directly.", 4.6, 12.6)];
        let holes = find(&pcm, &words);
        assert_eq!(
            holes.len(),
            2,
            "{:?}",
            holes.iter().map(|h| secs(*h)).collect::<Vec<_>>()
        );
        assert_eq!(secs(holes[1]), (12.7, 18.6));
    }

    #[test]
    fn covered_speech_and_short_sounds_are_not_holes() {
        let pcm = tone(10.0, &[(1.0, 3.0), (6.0, 6.4)]);
        let words = vec![word("a", 1.0, 1.8), word("b", 1.9, 3.0)];
        assert!(find(&pcm, &words).is_empty());
        assert!(find(&tone(10.0, &[]), &[]).is_empty(), "silence");
    }

    #[test]
    fn a_hole_keeps_only_the_new_words_inside_it() {
        let mut words = vec![word("directly.", 4.6, 12.6), word("It's", 23.6, 24.0)];
        let hole = Span::new(samples(12.7), samples(18.6));
        let heard = vec![
            word("directly.", 12.0, 12.4), // before the hole: the context the decode had
            word("I", 12.7, 13.0),
            word("stopped", 13.0, 13.4),
            word("of.", 18.0, 18.5),
            word("It's", 23.6, 24.0), // already there
        ];
        assert_eq!(fill(&mut words, hole, heard, samples(30.0)), 3);
        let t: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(t, ["directly.", "I", "stopped", "of.", "It's"]);
        assert_eq!(
            secs(words[0].span),
            (4.6, 12.6),
            "ends before the first added word"
        );
    }

    #[test]
    fn a_smeared_word_is_cut_where_an_added_word_starts() {
        let mut words = vec![word("of.", 18.0, 23.6)];
        let hole = Span::new(samples(19.5), samples(22.0));
        assert_eq!(
            fill(
                &mut words,
                hole,
                vec![word("so", 19.6, 20.0)],
                samples(30.0)
            ),
            1
        );
        assert_eq!(secs(words[0].span), (18.0, 19.6));
    }
}
