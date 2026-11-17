//! The M2 caption groupings (SPEC §5.10), beside the reference's `story_v5` ([`super::captions`]):
//!
//! - `single_word`: one word per cue;
//! - `adaptive_words`: one word per cue, merged with the next while a cue would show for
//!   less than [`FLASH`] (up to 3 words, up to 4 when 3 still would);
//! - `readable_phrase`: a cost search for phrases of at most 30 characters;
//! - `phrase`: the same search for phrases of at most 5 words.
//!
//! Shared rules: sentences break at `.?!` and at pauses over 0.9 s; a keep pair (the reference's
//! `KEEP_BASE` plus the project's) is never split, nor a hand-joined word from its
//! neighbour, and a hand split always opens a cue; a cue does not end on a weak word
//! ("the", "to", …) when the sentence goes on; a cue holds up to 0.5 s past its last word,
//! never into the next; a cue is at least [`MIN_CUE`] long where the next one leaves
//! room (only after merging is exhausted); a sentence's first word is capitalised.

use std::collections::{BTreeMap, BTreeSet};

use gmclip_model::anchor::WordId;
use gmclip_model::entities::{CaptionGrouping, CueBreak};
use gmclip_plan::story_cues;

use super::captions::{CapCue, CapWord};

const SEC: i64 = 705_600_000;
/// `FLASH_MS = 300`: a cue shown for less than this only flashes.
pub const FLASH: i64 = SEC * 3 / 10;
/// The shortest cue left on screen once merging cannot help.
pub const MIN_CUE: i64 = SEC * 15 / 100;
/// How long a cue holds past its last word.
pub const HOLD: i64 = SEC / 2;
/// A pause this long ends a sentence.
pub const PAUSE: i64 = SEC * 9 / 10;
/// `readable_phrase`'s longest cue.
pub const MAX_CHARS: usize = 30;
/// `phrase`'s longest cue.
pub const MAX_WORDS: usize = 5;

fn weak(w: &str) -> bool {
    story_cues::WEAK.contains(&story_cues::norm(w).as_str())
}

/// A unit: words that always share a cue.
struct Unit {
    words: std::ops::Range<usize>,
    start: i64,
    end: i64,
    chars: usize,
    /// A hand split before it.
    split: bool,
}

fn units(
    ws: &[CapWord],
    breaks: &BTreeMap<WordId, CueBreak>,
    keep: &BTreeSet<(String, String)>,
) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    for (i, w) in ws.iter().enumerate() {
        let b = breaks.get(&w.id);
        let joins = match (b, out.last()) {
            (Some(CueBreak::Join), Some(_)) => true,
            (Some(CueBreak::Split), _) | (_, None) => false,
            (_, Some(u)) => {
                let prev = &ws[u.words.end - 1];
                !story_cues::ends_with_any(&prev.text, ".?!")
                    && w.start - prev.end <= PAUSE
                    && keep.contains(&(story_cues::norm(&prev.text), story_cues::norm(&w.text)))
            }
        };
        if joins && let Some(u) = out.last_mut() {
            u.words.end = i + 1;
            u.end = u.end.max(w.end);
            u.chars += 1 + w.text.chars().count();
            continue;
        }
        out.push(Unit {
            words: i..i + 1,
            start: w.start,
            end: w.end,
            chars: w.text.chars().count(),
            split: b == Some(&CueBreak::Split),
        });
    }
    out
}

/// Sentence runs over units: break after `.?!`, at a long pause and at a hand split.
fn runs(ws: &[CapWord], us: &[Unit]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    for i in 0..us.len() {
        if i > from && (us[i].split || us[i].start - us[i - 1].end > PAUSE) {
            out.push(from..i);
            from = i;
        }
        if story_cues::ends_with_any(&ws[us[i].words.end - 1].text, ".?!") {
            out.push(from..i + 1);
            from = i + 1;
        }
    }
    if from < us.len() {
        out.push(from..us.len());
    }
    out
}

/// How long a cue of `us[a..b]` would show: to the next unit's start, or its last word's
/// end plus the hold at the end of the run.
fn shows(us: &[Unit], a: usize, b: usize, run_end: usize) -> i64 {
    if b < run_end {
        us[b].start - us[a].start
    } else {
        us[b - 1].end + HOLD - us[a].start
    }
}

fn nwords(us: &[Unit], a: usize, b: usize) -> usize {
    us[a].words.start.abs_diff(us[b - 1].words.end)
}

/// Cue boundaries (unit ranges) of one run.
fn split_run(
    ws: &[CapWord],
    us: &[Unit],
    r: std::ops::Range<usize>,
    grouping: CaptionGrouping,
) -> Vec<(usize, usize)> {
    let last_word = |b: usize| ws[us[b - 1].words.end - 1].text.as_str();
    match grouping {
        CaptionGrouping::SingleWord => r.map(|i| (i, i + 1)).collect(),
        CaptionGrouping::AdaptiveWords => {
            let mut out = Vec::new();
            let mut a = r.start;
            while a < r.end {
                let mut b = a + 1;
                loop {
                    if b >= r.end {
                        break;
                    }
                    let n = nwords(us, a, b + 1);
                    let flash = shows(us, a, b, r.end) < FLASH;
                    let dangling = weak(last_word(b)) && nwords(us, a, b) < 3;
                    if (flash || dangling) && (n <= 3 || (n == 4 && flash)) {
                        b += 1;
                    } else {
                        break;
                    }
                }
                out.push((a, b));
                a = b;
            }
            out
        }
        _ => {
            // DP: cost 1 per cue (fewer, fuller cues), +3 a weak last word mid-sentence,
            // +2 an inner comma, +12·(0.3 s − shown) under FLASH.
            let fits = |a: usize, b: usize| -> bool {
                if b - a == 1 {
                    return true; // a unit alone always fits
                }
                if grouping == CaptionGrouping::Phrase {
                    nwords(us, a, b) <= MAX_WORDS
                } else {
                    let chars: usize =
                        us[a..b].iter().map(|u| u.chars).sum::<usize>() + (b - a - 1);
                    chars <= MAX_CHARS
                }
            };
            let m = r.end - r.start;
            let mut best = vec![f64::INFINITY; m + 1];
            let mut back = vec![0usize; m + 1];
            best[0] = 0.0;
            for j in 1..=m {
                for i in (0..j).rev() {
                    let (a, b) = (r.start + i, r.start + j);
                    if !fits(a, b) {
                        break;
                    }
                    let mut cost = 1.0;
                    if b < r.end && weak(last_word(b)) {
                        cost += 3.0;
                    }
                    if us[a..b - 1]
                        .iter()
                        .any(|u| ws[u.words.end - 1].text.ends_with(','))
                    {
                        cost += 2.0;
                    }
                    let s = shows(us, a, b, r.end);
                    if s < FLASH {
                        cost += 12.0 * (FLASH - s) as f64 / SEC as f64;
                    }
                    if best[i] + cost < best[j] {
                        best[j] = best[i] + cost;
                        back[j] = i;
                    }
                }
            }
            let mut out = Vec::new();
            let mut j = m;
            while j > 0 {
                out.push((r.start + back[j], r.start + j));
                j = back[j];
            }
            out.reverse();
            out
        }
    }
}

/// Groups words into cues with an M2 grouping (`story_v5` is [`super::captions::group`]).
pub fn group(
    ws: &[CapWord],
    grouping: CaptionGrouping,
    breaks: &BTreeMap<WordId, CueBreak>,
    keep: &BTreeSet<(String, String)>,
    total: i64,
) -> Vec<CapCue> {
    let us = units(ws, breaks, keep);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for r in runs(ws, &us) {
        spans.extend(split_run(ws, &us, r, grouping));
    }
    let mut cues: Vec<CapCue> = Vec::with_capacity(spans.len());
    for (k, &(a, b)) in spans.iter().enumerate() {
        let mut words: Vec<CapWord> = ws[us[a].words.start..us[b - 1].words.end].to_vec();
        let start = us[a].start;
        let next = spans.get(k + 1).map_or(total, |&(n, _)| us[n].start);
        let mut end = next.min(us[b - 1].end + HOLD).min(total);
        if end - start < MIN_CUE {
            end = next.min(start + MIN_CUE).min(total);
        }
        if end <= start {
            continue;
        }
        let opens = cues
            .last()
            .and_then(|c: &CapCue| c.words.last())
            .is_none_or(|w| story_cues::ends_with_any(&w.text, ".?!"));
        if opens && let Some(w0) = words.first_mut() {
            w0.text = story_cues::capitalise(&w0.text);
        }
        cues.push(CapCue {
            start,
            end,
            part: words[0].part,
            words,
        });
    }
    cues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: f64, e: f64) -> CapWord {
        CapWord {
            id: WordId {
                asr_revision: "t".into(),
                ordinal: (s * 100.0) as u32,
            },
            text: text.into(),
            start: (s * SEC as f64) as i64,
            end: (e * SEC as f64) as i64,
            keyword: false,
            part: 0,
        }
    }

    fn texts(c: &[CapCue]) -> Vec<String> {
        c.iter()
            .map(|c| {
                c.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn sample() -> Vec<CapWord> {
        vec![
            w("we", 0.0, 0.15),
            w("generated", 0.15, 0.6),
            w("that", 0.6, 0.75),
            w("in", 0.75, 0.85),
            w("two", 0.85, 1.05),
            w("minutes", 1.05, 1.5),
            w("on", 1.5, 1.6),
            w("my", 1.6, 1.7),
            w("own", 1.7, 1.95),
            w("PC.", 1.95, 2.4),
            w("Wow,", 3.6, 4.0),
            w("that", 4.0, 4.2),
            w("is", 4.2, 4.3),
            w("fast.", 4.3, 4.9),
        ]
    }

    fn run(g: CaptionGrouping) -> Vec<CapCue> {
        group(
            &sample(),
            g,
            &BTreeMap::new(),
            &story_cues::keep_set(&[]),
            6 * SEC,
        )
    }

    fn sane(c: &[CapCue]) {
        assert!(c.windows(2).all(|p| p[0].end <= p[1].start), "overlap");
        assert!(c.iter().all(|c| c.end > c.start));
        // every word captioned once, in order
        let all: Vec<String> = c
            .iter()
            .flat_map(|c| c.words.iter().map(|w| w.text.to_lowercase()))
            .collect();
        let want: Vec<String> = sample().iter().map(|w| w.text.to_lowercase()).collect();
        assert_eq!(all, want);
    }

    #[test]
    fn single_word_keeps_pairs_together() {
        let c = run(CaptionGrouping::SingleWord);
        sane(&c);
        assert_eq!(texts(&c)[0], "We");
        assert!(
            texts(&c).contains(&"two minutes".to_owned()),
            "{:?}",
            texts(&c)
        );
        assert!(c.iter().all(|c| c.words.len() <= 2));
    }

    #[test]
    fn adaptive_words_merge_flashes() {
        let c = run(CaptionGrouping::AdaptiveWords);
        sane(&c);
        let t = texts(&c);
        // "that in" flash on their own (0.15 s, 0.1 s): merged forward, and no cue ends on
        // a weak word while the sentence goes on
        assert!(c.iter().all(|c| c.words.len() <= 4), "{t:?}");
        for (i, cue) in c.iter().enumerate() {
            let last = &cue.words.last().unwrap().text;
            let mid = i + 1 < c.len() && !story_cues::ends_with_any(last, ".?!");
            assert!(!(mid && weak(last) && cue.words.len() < 3), "{t:?}");
        }
        // every cue but a sentence's last shows ≥ FLASH unless 4 words could not help
        for p in c.windows(2) {
            if p[0].words.len() < 4 && p[1].start - p[0].start < FLASH {
                assert!(p[0].words.last().unwrap().text.ends_with('.'), "{t:?}");
            }
        }
    }

    #[test]
    fn readable_phrase_and_phrase_bounds() {
        let c = run(CaptionGrouping::ReadablePhrase);
        sane(&c);
        assert!(
            texts(&c).iter().all(|t| t.chars().count() <= MAX_CHARS),
            "{:?}",
            texts(&c)
        );
        // a comma is a natural break: "Wow," stands alone rather than inside a cue
        assert_eq!(texts(&c)[texts(&c).len() - 2..], ["Wow,", "that is fast."]);
        let p = run(CaptionGrouping::Phrase);
        sane(&p);
        assert!(p.iter().all(|c| c.words.len() <= MAX_WORDS));
        assert!(p.len() < run(CaptionGrouping::SingleWord).len());
        // a hand split always opens a cue
        let ws = sample();
        let mut br = BTreeMap::new();
        br.insert(ws[4].id.clone(), CueBreak::Split);
        let c = group(
            &ws,
            CaptionGrouping::Phrase,
            &br,
            &story_cues::keep_set(&[]),
            6 * SEC,
        );
        assert!(
            c.iter().any(|c| c.words[0].text == "two"),
            "{:?}",
            texts(&c)
        );
    }

    #[test]
    fn cues_hold_and_have_a_floor() {
        let c = run(CaptionGrouping::SingleWord);
        let last = c.last().unwrap();
        assert_eq!(last.end, (4.9 * SEC as f64) as i64 + HOLD);
        for p in c.windows(2) {
            assert!(p[0].end - p[0].start >= MIN_CUE || p[0].end == p[1].start);
        }
    }
}
