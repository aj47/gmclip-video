//! Word agreement between two transcripts: a Levenshtein alignment over normalised
//! words, with the matched pairs kept so their times can be compared (B04's "WER ≤ 5%,
//! median word-time delta ≤ 60 ms" against the reference CoreML Parakeet).
//!
//! Normalisation: lowercase, punctuation dropped (apostrophes kept), hyphenated words
//! split ("five-second" → "five", "second"), empty words dropped. Numbers are compared
//! as written, so "1215" vs "twelve fifteen" counts as an error on both sides.

use gmclip_analysis::fillers::normalise;

/// A timed word for comparison (seconds on a shared clock).
#[derive(Clone, Debug, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// Splits and normalises: one input word may become several.
pub fn normalised(words: &[TimedWord]) -> Vec<TimedWord> {
    let mut out = Vec::new();
    for w in words {
        for part in w.text.split(['-', '–', '—', '/']) {
            let n = normalise(part);
            if !n.is_empty() {
                out.push(TimedWord {
                    text: n,
                    start: w.start,
                    end: w.end,
                });
            }
        }
    }
    out
}

/// The alignment result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agreement {
    pub reference_words: usize,
    pub hypothesis_words: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// `(reference index, hypothesis index)` of words that match exactly.
    pub matches: Vec<(usize, usize)>,
    /// |start delta| of every matched pair, seconds, sorted.
    pub start_deltas: Vec<f64>,
    /// |end delta| of every matched pair, seconds, sorted.
    pub end_deltas: Vec<f64>,
}

impl Agreement {
    pub fn wer(&self) -> f64 {
        if self.reference_words == 0 {
            return 0.0;
        }
        (self.substitutions + self.deletions + self.insertions) as f64 / self.reference_words as f64
    }

    /// Matched words over reference words.
    pub fn agreement(&self) -> f64 {
        if self.reference_words == 0 {
            return 1.0;
        }
        self.matches.len() as f64 / self.reference_words as f64
    }

    pub fn median_start_delta(&self) -> Option<f64> {
        quantile(&self.start_deltas, 0.5)
    }

    pub fn median_end_delta(&self) -> Option<f64> {
        quantile(&self.end_deltas, 0.5)
    }
}

/// Nearest-rank quantile of sorted data.
pub fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// Aligns `hyp` against `reference` (both already [`normalised`]).
pub fn align(reference: &[TimedWord], hyp: &[TimedWord]) -> Agreement {
    let (n, m) = (reference.len(), hyp.len());
    // d[i][j]: edit distance of reference[..i] vs hyp[..j]; O(n·m) memory is fine for
    // the window sizes compared here (a 1 h stream is compared in chunks by the caller).
    let mut d = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 0..=n {
        d[at(i, 0)] = i as u32;
    }
    for j in 0..=m {
        d[at(0, j)] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[at(i - 1, j - 1)] + u32::from(reference[i - 1].text != hyp[j - 1].text);
            d[at(i, j)] = sub.min(d[at(i - 1, j)] + 1).min(d[at(i, j - 1)] + 1);
        }
    }
    let mut a = Agreement {
        reference_words: n,
        hypothesis_words: m,
        ..Agreement::default()
    };
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let same = reference[i - 1].text == hyp[j - 1].text;
            if d[at(i, j)] == d[at(i - 1, j - 1)] + u32::from(!same) {
                if same {
                    a.matches.push((i - 1, j - 1));
                } else {
                    a.substitutions += 1;
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && d[at(i, j)] == d[at(i - 1, j)] + 1 {
            a.deletions += 1;
            i -= 1;
        } else {
            a.insertions += 1;
            j -= 1;
        }
    }
    a.matches.reverse();
    for &(r, h) in &a.matches {
        a.start_deltas
            .push((reference[r].start - hyp[h].start).abs());
        a.end_deltas.push((reference[r].end - hyp[h].end).abs());
    }
    a.start_deltas.sort_by(f64::total_cmp);
    a.end_deltas.sort_by(f64::total_cmp);
    a
}

/// Cue-boundary agreement (SPEC §5.10 R-CAP-1, F17): where two cue lists over the same
/// speech break between cues, compared on the word alignment rather than on times, so one
/// mis-heard word costs only the breaks next to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Boundaries {
    /// Breaks between cues in the reference (a cue start other than the first).
    pub reference: usize,
    pub hypothesis: usize,
    /// Hypothesis breaks on a word matched to a reference word that also starts a cue.
    pub matched: usize,
    /// |start delta| of the two cues at each matched break, seconds, sorted.
    pub deltas: Vec<f64>,
    /// Reference cues (with words).
    pub reference_cues: usize,
    /// Reference cues the hypothesis shows exactly: the same words, matched one to one,
    /// as one cue of its own (the exact-text agreement).
    pub same_cues: usize,
}

impl Boundaries {
    pub fn precision(&self) -> f64 {
        if self.hypothesis == 0 {
            return if self.reference == 0 { 1.0 } else { 0.0 };
        }
        self.matched as f64 / self.hypothesis as f64
    }

    pub fn recall(&self) -> f64 {
        if self.reference == 0 {
            return if self.hypothesis == 0 { 1.0 } else { 0.0 };
        }
        self.matched as f64 / self.reference as f64
    }

    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }

    /// Sums counts (pooled F1 over several cuts).
    pub fn add(&mut self, o: &Boundaries) {
        self.reference += o.reference;
        self.hypothesis += o.hypothesis;
        self.matched += o.matched;
        self.deltas.extend_from_slice(&o.deltas);
        self.deltas.sort_by(f64::total_cmp);
        self.reference_cues += o.reference_cues;
        self.same_cues += o.same_cues;
    }
}

/// [`normalised`] words of each cue, and the index of each cue's first word (cues with
/// no word left are dropped).
fn flatten(cues: &[Vec<TimedWord>]) -> (Vec<TimedWord>, Vec<usize>) {
    let (mut words, mut starts) = (Vec::new(), Vec::new());
    for c in cues {
        let n = normalised(c);
        if !n.is_empty() {
            starts.push(words.len());
            words.extend(n);
        }
    }
    (words, starts)
}

/// Aligns two cue lists' words ([`align`]) and counts their cue breaks ([`Boundaries`]).
pub fn cue_boundaries(
    reference: &[Vec<TimedWord>],
    hyp: &[Vec<TimedWord>],
) -> (Agreement, Boundaries) {
    let (rw, rs) = flatten(reference);
    let (hw, hs) = flatten(hyp);
    let a = align(&rw, &hw);
    let rset: std::collections::BTreeSet<usize> = rs.iter().skip(1).copied().collect();
    let hset: std::collections::BTreeSet<usize> = hs.iter().skip(1).copied().collect();
    let mut deltas: Vec<f64> = a
        .matches
        .iter()
        .filter(|(r, h)| rset.contains(r) && hset.contains(h))
        .map(|&(r, h)| (rw[r].start - hw[h].start).abs())
        .collect();
    deltas.sort_by(f64::total_cmp);
    // cue ranges [start, end) over the flattened words
    let ranges = |starts: &[usize], n: usize| -> Vec<(usize, usize)> {
        starts
            .iter()
            .enumerate()
            .map(|(i, &s)| (s, starts.get(i + 1).copied().unwrap_or(n)))
            .collect()
    };
    let hr: std::collections::BTreeSet<(usize, usize)> =
        ranges(&hs, hw.len()).into_iter().collect();
    let to_h: std::collections::BTreeMap<usize, usize> = a.matches.iter().copied().collect();
    let same_cues = ranges(&rs, rw.len())
        .into_iter()
        .filter(|&(s, e)| {
            let Some(&h0) = to_h.get(&s) else {
                return false;
            };
            (s..e).all(|r| to_h.get(&r) == Some(&(h0 + r - s))) && hr.contains(&(h0, h0 + e - s))
        })
        .count();
    let b = Boundaries {
        reference: rset.len(),
        hypothesis: hset.len(),
        matched: deltas.len(),
        deltas,
        reference_cues: rs.len(),
        same_cues,
    };
    (a, b)
}

/// Character edit distance (the arbiter's "edit distance ≤ 1").
pub fn char_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            cur[j] = (prev[j - 1] + usize::from(a[i - 1] != b[j - 1]))
                .min(prev[j] + 1)
                .min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(s: &str, t0: f64) -> Vec<TimedWord> {
        s.split_whitespace()
            .enumerate()
            .map(|(i, w)| TimedWord {
                text: w.into(),
                start: t0 + i as f64,
                end: t0 + i as f64 + 0.5,
            })
            .collect()
    }

    #[test]
    fn counts_edits_and_deltas() {
        let r = normalised(&ws("Hi, I'm a five-second banana.", 0.0));
        assert_eq!(r.len(), 6);
        let h = normalised(&ws("hi im a five second banana extra", 0.04));
        let a = align(&r, &h);
        assert_eq!(a.substitutions, 1); // i'm / im
        assert_eq!(a.insertions, 1);
        assert_eq!(a.deletions, 0);
        assert_eq!(a.matches.len(), 5);
        assert!((a.wer() - 2.0 / 6.0).abs() < 1e-12);
        assert!((a.median_start_delta().unwrap() - 0.04).abs() < 1e-9);
        let a = align(&r, &[]);
        assert_eq!(a.deletions, 6);
        assert_eq!(a.wer(), 1.0);
    }

    #[test]
    fn cue_boundaries_count_breaks_on_matched_words() {
        let cues = |s: &str| -> Vec<Vec<TimedWord>> { s.split('|').map(|c| ws(c, 0.0)).collect() };
        // same words, one cue split differently: 3 inner breaks each, 2 shared
        let r = cues("We generated that|in two minutes.|Hi,|I'm a banana");
        let h = cues("We generated|that in two minutes.|Hi,|I'm a banana");
        let (a, b) = cue_boundaries(&r, &h);
        assert_eq!(a.wer(), 0.0);
        assert_eq!((b.reference, b.hypothesis, b.matched), (3, 3, 2));
        assert!((b.f1() - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!((b.reference_cues, b.same_cues), (4, 2)); // "Hi," and "I'm a banana"
        assert_eq!(b.deltas, [0.0, 0.0]);
        // identical cue lists: F1 1, and a break on a word only one side heard never matches
        let (_, b) = cue_boundaries(&r, &r);
        assert_eq!(b.f1(), 1.0);
        assert_eq!(b.same_cues, 4);
        let mut pooled = Boundaries::default();
        pooled.add(&b);
        pooled.add(&cue_boundaries(&r, &h).1);
        assert_eq!(
            (pooled.reference, pooled.matched, pooled.same_cues),
            (6, 5, 6)
        );
        let h = cues("We generated that|um in two minutes.|Hi,|I'm a banana");
        let (a, b) = cue_boundaries(&r, &h);
        assert_eq!(a.insertions, 1);
        assert_eq!((b.reference, b.hypothesis, b.matched), (3, 3, 2));
        // a cue with no words after normalisation is no cue; nothing at all is F1 1
        let (_, b) = cue_boundaries(&cues("Hi|—|there"), &cues("Hi|there"));
        assert_eq!((b.reference, b.matched), (1, 1));
        assert_eq!(cue_boundaries(&[], &[]).1.f1(), 1.0);
    }

    #[test]
    fn char_distances() {
        assert_eq!(char_distance("banana", "banana"), 0);
        assert_eq!(char_distance("banana", "bananas"), 1);
        assert_eq!(char_distance("kitten", "sitting"), 3);
        assert_eq!(char_distance("", "ab"), 2);
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 0.5), Some(2.0));
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 0.95), Some(4.0));
    }
}
