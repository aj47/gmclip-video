//! Token → word stitching and long-audio chunking (SPEC §9), pure and unit-tested.
//!
//! - **Stitching**: a SentencePiece token that begins with `▁` starts a word; every other
//!   token continues the current one (sub-words, and punctuation, which Parakeet emits
//!   without `▁`, so "Hi," stays one word as in the reference transcripts). parakeet-rs
//!   hands tokens over with `▁` already turned into a leading space. Parakeet v3's
//!   vocabulary has no `▁<digit>` piece: a number that is its own word comes as a bare
//!   `▁` token then digits, and that bare `▁` starts the word. parakeet-rs also inserts
//!   a space before digits that follow letters; that space is its guess, not the model's,
//!   so it is ignored ("Qwen3", "MP4" stay one word).
//! - **Times** are encoder frames: Parakeet's encoder subsamples 8× at a 10 ms hop, so
//!   one frame is 80 ms = 1280 samples at 16 kHz. A word spans from its first token's
//!   frame to its last token's frame + TDT duration.
//! - **Chunking**: 30 s windows with 2 s overlap (the last one full length, ending at the
//!   buffer's end). Words are taken from the window whose centre is nearer: the
//!   switch-over between windows *i* and *i*+1 is the midpoint of their centres. Because
//!   the two decodes place a word at the switch-over a frame or two apart, the join is
//!   made on the **words**, not the time: the overlap's words of both windows are
//!   aligned (longest common subsequence of normalised words within ±1 s of the
//!   switch-over) and the seam is the aligned pair nearest the switch-over; window *i*
//!   contributes up to and including it, window *i*+1 everything after. No word is lost
//!   or doubled at a join unless the two decodes disagree on every word near it, when
//!   the midpoint rule is the fallback.

use gmclip_analysis::fillers::normalise;
use gmclip_analysis::span::Span;

/// Samples per encoder frame at 16 kHz (80 ms).
pub const FRAME_SAMPLES: usize = 1280;
/// Bump when the stitching or merging rules change: it is part of `asr_revision`.
/// 3 (2026-10-16): the seam aligns only words within reach of the switch-over on both
/// sides.
pub const STITCH_VERSION: u32 = 3;

/// A decoded token: text with `▁` already mapped to a leading space, frame times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start_frame: u32,
    pub end_frame: u32,
}

/// A word in samples of the decoded window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawWord {
    pub text: String,
    pub span: Span,
}

/// Seconds (as parakeet-rs reports them, multiples of 0.08 s) to a frame index.
pub fn seconds_to_frame(s: f32) -> u32 {
    (f64::from(s) / 0.08).round().max(0.0) as u32
}

/// Stitches tokens into words.
pub fn tokens_to_words(tokens: &[Token]) -> Vec<RawWord> {
    let mut out: Vec<RawWord> = Vec::new();
    // A bare `▁` token (Parakeet emits one before a digit run: "▁", "5", "0", "8", "0")
    // carries no text but still starts the next word.
    let mut pending_start = false;
    for t in tokens {
        let piece = t.text.trim_start_matches([' ', '▁']);
        let digits = !piece.is_empty() && piece.bytes().all(|b| b.is_ascii_digit());
        let marked = t.text.starts_with(' ') || t.text.starts_with('▁');
        // a space parakeet-rs put before digits is not a word marker (see above)
        let starts_word = pending_start || (marked && !digits);
        if piece.is_empty() {
            pending_start = starts_word;
            continue;
        }
        pending_start = false;
        let span = Span::new(
            t.start_frame as usize * FRAME_SAMPLES,
            t.end_frame.max(t.start_frame) as usize * FRAME_SAMPLES,
        );
        match out.last_mut() {
            Some(w) if !starts_word => {
                w.text.push_str(piece);
                w.span.end = w.span.end.max(span.end);
            }
            _ => out.push(RawWord {
                text: piece.to_owned(),
                span,
            }),
        }
    }
    out.retain(|w| !w.text.trim().is_empty());
    out
}

/// The windows covering `n` samples: `[k·hop, k·hop + window)`, and a last one
/// `[n − window, n)` (the whole buffer when `n ≤ window`).
pub fn plan_windows(n: usize, window: usize, overlap: usize) -> Vec<Span> {
    assert!(window > overlap, "window must exceed overlap");
    let hop = window - overlap;
    let mut out = Vec::new();
    let mut s = 0;
    if n == 0 {
        return out;
    }
    loop {
        if s + window >= n {
            // The last window ends at `n` and is full length (it overlaps its
            // neighbour by more than `overlap`), so the switch-over point between the
            // two centres stays inside both windows. A short last window would put it
            // outside the shorter one and lose the words in between.
            out.push(Span::new(n.saturating_sub(window), n));
            break;
        }
        out.push(Span::new(s, s + window));
        s += hop;
    }
    out
}

/// Where window `i`'s kept words begin and end: midpoints between window centres
/// (`(0, usize::MAX)` at the ends). [`plan_windows`] makes every window full length, so
/// each midpoint lies inside its overlap, at least `overlap/2` from both window edges.
pub fn keep_range(windows: &[Span], i: usize) -> (usize, usize) {
    let centre = |w: &Span| (w.start + w.end) / 2;
    let lo = if i == 0 {
        0
    } else {
        (centre(&windows[i - 1]) + centre(&windows[i])) / 2
    };
    let hi = if i + 1 >= windows.len() {
        usize::MAX
    } else {
        (centre(&windows[i]) + centre(&windows[i + 1])) / 2
    };
    (lo, hi)
}

/// Where windows join: keep `a[..na]` (the earlier window) and `b[nb..]` (the later),
/// both with absolute times, around the switch-over sample `hi`. See the module docs.
pub fn seam(a: &[RawWord], b: &[RawWord], hi: usize, reach: usize) -> (usize, usize) {
    let lo_t = hi.saturating_sub(reach);
    let hi_t = hi.saturating_add(reach);
    // both sides within ±reach of the switch-over: the last window overlaps its
    // neighbour by far more than `overlap`, and an alignment over all of that pairs one
    // saying of a word with another (a repeated "pursuers" 7 s apart doubled 7 s of words)
    let near = |w: &RawWord| (lo_t..hi_t).contains(&w.span.mid());
    let ai: Vec<usize> = (0..a.len()).filter(|&i| near(&a[i])).collect();
    let bi: Vec<usize> = (0..b.len()).filter(|&i| near(&b[i])).collect();
    let an: Vec<String> = ai.iter().map(|&i| normalise(&a[i].text)).collect();
    let bn: Vec<String> = bi.iter().map(|&i| normalise(&b[i].text)).collect();
    // LCS table over the overlap's words
    let (n, m) = (an.len(), bn.len());
    let mut d = vec![0u16; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[at(i, j)] = if !an[i].is_empty() && an[i] == bn[j] {
                d[at(i + 1, j + 1)] + 1
            } else {
                d[at(i + 1, j)].max(d[at(i, j + 1)])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if !an[i].is_empty() && an[i] == bn[j] && d[at(i, j)] == d[at(i + 1, j + 1)] + 1 {
            pairs.push((ai[i], bi[j]));
            i += 1;
            j += 1;
        } else if d[at(i + 1, j)] >= d[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    // the aligned pair nearest the switch-over (by the two placements' mean)
    let best = pairs.into_iter().min_by_key(|&(x, y)| {
        let t = (a[x].span.mid() + b[y].span.mid()) / 2;
        (t.abs_diff(hi), x)
    });
    match best {
        Some((x, y)) => (x + 1, y + 1),
        None => {
            let na = a.iter().take_while(|w| w.span.mid() < hi).count();
            let nb = b.iter().take_while(|w| w.span.mid() < hi).count();
            (na, nb)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(t: &str, a: u32, b: u32) -> Token {
        Token {
            text: t.to_owned(),
            start_frame: a,
            end_frame: b,
        }
    }

    fn w(t: &str, a: usize, b: usize) -> RawWord {
        RawWord {
            text: t.into(),
            span: Span::new(a, b),
        }
    }

    #[test]
    fn stitches_on_word_markers() {
        let toks = [
            tok(" H", 0, 1),
            tok("i", 1, 2),
            tok(",", 2, 3),
            tok(" I", 4, 5),
            tok("'m", 5, 6),
            tok("▁a", 6, 7),
            tok(" ban", 7, 8),
            tok("ana", 8, 10),
            tok(".", 10, 10),
            tok(" at", 12, 13),
            tok(" ", 13, 13), // the model's own word marker before a number
            tok("6", 13, 14),
            tok("0", 14, 15),
            tok(" Qwen", 15, 15),
            tok(" 3", 15, 15), // parakeet-rs's guessed space: not a word start
            tok(" NVIDIA", 15, 16),
            tok(" ", 16, 16), // a bare ▁ before a digit run
            tok("5", 16, 17),
            tok("0", 17, 18),
            tok("8", 18, 19),
        ];
        let words = tokens_to_words(&toks);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Hi,", "I'm", "a", "banana.", "at", "60", "Qwen3", "NVIDIA", "508"
            ]
        );
        assert_eq!(
            words[8].span,
            Span::new(16 * FRAME_SAMPLES, 19 * FRAME_SAMPLES)
        );
        assert_eq!(words[0].span, Span::new(0, 3 * FRAME_SAMPLES));
        assert_eq!(
            words[3].span,
            Span::new(7 * FRAME_SAMPLES, 10 * FRAME_SAMPLES)
        );
        // a leading continuation token starts the first word
        assert_eq!(tokens_to_words(&[tok("lo", 0, 1)])[0].text, "lo");
        assert_eq!(seconds_to_frame(2.48), 31);
        assert_eq!(seconds_to_frame(0.0), 0);
    }

    #[test]
    fn windows_overlap_by_two_seconds() {
        let sr = 16_000;
        let ws = plan_windows(70 * sr, 30 * sr, 2 * sr);
        assert_eq!(
            ws,
            vec![
                Span::new(0, 30 * sr),
                Span::new(28 * sr, 58 * sr),
                Span::new(40 * sr, 70 * sr)
            ]
        );
        assert_eq!(plan_windows(10, 30, 2), vec![Span::new(0, 10)]);
        assert_eq!(plan_windows(30, 30, 2), vec![Span::new(0, 30)]);
        assert!(plan_windows(0, 30, 2).is_empty());
        // switch-over at the midpoint of the centres: 29 s between windows 0 and 1
        assert_eq!(keep_range(&ws, 0), (0, 29 * sr));
        assert_eq!(keep_range(&ws, 1).0, 29 * sr);
        // windows 1 (centre 43) and 2 (centre 55): 49 s, inside both
        assert_eq!(keep_range(&ws, 1).1, 49 * sr);
        assert_eq!(keep_range(&ws, 2).1, usize::MAX);
        // the switch-over always sits ≥ overlap/2 inside both windows
        for n in [31usize, 45, 57, 58, 59, 86, 108, 1000] {
            let ws = plan_windows(n * 10, 300, 20);
            for i in 0..ws.len().saturating_sub(1) {
                let b = keep_range(&ws, i).1;
                assert!(
                    b >= ws[i + 1].start + 10 && b + 10 <= ws[i].end,
                    "n {n} i {i}: {ws:?}"
                );
            }
            assert_eq!(ws.last().unwrap().end, n * 10);
        }
    }

    #[test]
    fn seams_join_on_words_not_times() {
        // the earlier window places "three" just after the switch-over (29), the later
        // just before: the midpoint rule would lose it; the seam keeps it once
        let a = vec![
            w("one", 26, 27),
            w("two", 27, 28),
            w("three", 28, 31),
            w("fo", 31, 32),
        ];
        let b = vec![
            w("two", 27, 28),
            w("three", 27, 29),
            w("four", 29, 32),
            w("five", 32, 33),
        ];
        let (na, nb) = seam(&a, &b, 29, 3);
        let mut out: Vec<&str> = a[..na].iter().map(|w| w.text.as_str()).collect();
        out.extend(b[nb..].iter().map(|w| w.text.as_str()));
        assert_eq!(out, ["one", "two", "three", "four", "five"]);
        // a two-word straddle ("a lot") is not doubled
        let a = vec![w("x", 20, 21), w("a", 28, 29), w("lot", 29, 30)];
        let b = vec![w("a", 28, 29), w("lot", 29, 31), w("y", 31, 32)];
        let (na, nb) = seam(&a, &b, 29, 3);
        let mut out: Vec<&str> = a[..na].iter().map(|w| w.text.as_str()).collect();
        out.extend(b[nb..].iter().map(|w| w.text.as_str()));
        assert_eq!(out, ["x", "a", "lot", "y"]);
        // a real repeat ("that that") keeps both
        let a = vec![w("that", 27, 28), w("that", 28, 29), w("is", 29, 30)];
        let b = vec![
            w("that", 27, 28),
            w("that", 28, 29),
            w("is", 29, 30),
            w("it", 30, 31),
        ];
        let (na, nb) = seam(&a, &b, 29, 3);
        let mut out: Vec<&str> = a[..na].iter().map(|w| w.text.as_str()).collect();
        out.extend(b[nb..].iter().map(|w| w.text.as_str()));
        assert_eq!(out, ["that", "that", "is", "it"]);
        // the last window overlaps far more than 2 s: the earlier window's words run on
        // past the switch-over and the later one's start well before it, so "the
        // pursuers" said twice, 7 s apart, must not be taken for one saying (that doubled
        // every word between them, as heard on h34)
        let a = vec![
            w("him.", 455, 465),
            w("it's", 465, 470),
            w("here.", 484, 489),
            w("the", 489, 491),
            w("pursuers", 494, 501),
            w("are", 501, 503),
        ];
        let b = vec![
            w("the", 415, 416),
            w("roof", 416, 420),
            w("pursuers", 425, 432),
            w("behind", 452, 456),
            w("him.", 456, 462),
            w("it's", 462, 468),
            w("here.", 485, 489),
            w("the", 489, 491),
            w("pursuers", 494, 501),
            w("are", 501, 503),
        ];
        let (na, nb) = seam(&a, &b, 462, 10);
        let mut out: Vec<&str> = a[..na].iter().map(|w| w.text.as_str()).collect();
        out.extend(b[nb..].iter().map(|w| w.text.as_str()));
        assert_eq!(
            out.iter().filter(|w| **w == "pursuers").count(),
            1,
            "{out:?}"
        );
        assert_eq!(out.iter().filter(|w| **w == "him.").count(), 1, "{out:?}");
        // nothing in common: the midpoint rule
        let a = vec![w("p", 27, 28), w("q", 29, 31)];
        let b = vec![w("r", 27, 28), w("s", 30, 31)];
        assert_eq!(seam(&a, &b, 29, 3), (1, 1));
        assert_eq!(seam(&[], &b, 29, 3), (0, 1));
    }
}
