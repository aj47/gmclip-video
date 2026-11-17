//! Stressed words (Ideas 2026-10-16, K2.1): the caption words a speaker leans on, from
//! the cut's own audio and its word timings.
//!
//! Two signals per word, each a robust z-score against **the speaker's running baseline
//! in this cut** (the median and MAD of the words around it, ±[`WINDOW`] words) — never
//! a fixed dB or pace:
//!
//! - **Louder**: the word's level (mean power of its loudest half of level frames, plus
//!   the part's gain), against the levels of the words around it.
//! - **Slower**: milliseconds per syllable, against the words around it **of the same
//!   kind** — a word before a pause or a sentence end is always drawn out (phrase-final
//!   lengthening), so those are compared only with each other.
//!
//! A word is stressed when either z (or the two together) reaches [`STRESS_Z`]. Fillers
//! and weak words ("the", "and", "it's") are never stressed. The result is kept sparse:
//! at most one word in [`WORDS_PER_STRESS`] of the cut, never two within
//! [`MIN_GAP_WORDS`] words, the strongest first.

/// One caption word on the cut's clock, with its measured level.
#[derive(Clone, Debug, PartialEq)]
pub struct EmWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// dBFS of the word's loudest half (see [`word_level`]), part gain included; `None`
    /// when no level frames cover it (it is then judged on pace alone).
    pub level_db: Option<f64>,
}

/// Why a word was picked.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Stress {
    /// Index into the input words.
    pub index: usize,
    /// z of the level and of the pace (positive = louder / slower than usual here).
    pub z_level: f64,
    pub z_slow: f64,
    /// dB over the running median level, and pace as a multiple of the running median.
    pub db_over: f64,
    pub pace_ratio: f64,
    /// `max(z_level, z_slow, 0.75·(z_level + z_slow))`.
    pub score: f64,
    /// What was measured, in gmclip's words ("louder: +4.2 dB over his usual here (2.3σ)").
    pub label: String,
}

/// Words either side whose median and MAD are the running baseline.
pub const WINDOW: usize = 20;
/// A z of this (either signal, or 0.75 × their sum) marks a stressed word.
pub const STRESS_Z: f64 = 2.0;
/// At most one stressed word per this many words of the cut.
pub const WORDS_PER_STRESS: usize = 10;
/// Two stressed words are at least this many words apart.
pub const MIN_GAP_WORDS: usize = 4;
/// A gap after a word of at least this many ms makes it phrase-final.
pub const PAUSE_MS: i64 = 250;

const FILLERS: &[&str] = &[
    "um", "umm", "ummm", "uh", "uhh", "uhhh", "uhm", "erm", "er", "mm", "mhm", "hmm", "ah", "like",
];
const WEAK: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "but",
    "so",
    "to",
    "of",
    "in",
    "on",
    "at",
    "by",
    "for",
    "with",
    "from",
    "as",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "am",
    "it",
    "its",
    "it's",
    "i",
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "you",
    "you're",
    "we",
    "we're",
    "he",
    "she",
    "they",
    "them",
    "his",
    "her",
    "my",
    "your",
    "our",
    "their",
    "me",
    "us",
    "this",
    "that",
    "these",
    "those",
    "there",
    "here",
    "then",
    "than",
    "just",
    "do",
    "did",
    "does",
    "have",
    "has",
    "had",
    "can",
    "could",
    "would",
    "should",
    "will",
    "if",
    "not",
    "no",
    "yes",
    "yeah",
    "okay",
    "ok",
    "oh",
    "well",
    "right",
    "know",
    "kind",
    "sort",
    "gonna",
    "wanna",
    "what",
    "which",
    "who",
    "how",
    "when",
    "where",
    "up",
    "out",
    "about",
    "into",
    "over",
    "some",
    "any",
    "all",
    "get",
    "got",
    "go",
    "let's",
    "let",
    "now",
    "also",
    "very",
    "really",
    "actually",
    "basically",
];

/// The bare, lower-case form of a word (`"GPU,"` → `"gpu"`).
pub fn bare(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’')
        .map(|c| if c == '’' { '\'' } else { c })
        .flat_map(char::to_lowercase)
        .collect()
}

/// A filler ("um", "uh", "like"): never a keyword.
pub fn is_filler(text: &str) -> bool {
    FILLERS.contains(&bare(text).as_str())
}

/// Whether a word may carry stress at all: not a filler, not a weak word, at least two
/// letters (or a digit).
pub fn eligible(text: &str) -> bool {
    let b = bare(text);
    if b.is_empty() || FILLERS.contains(&b.as_str()) || WEAK.contains(&b.as_str()) {
        return false;
    }
    b.chars().any(|c| c.is_ascii_digit()) || b.chars().filter(|c| c.is_alphabetic()).count() >= 2
}

/// Rough syllables of a written word: vowel groups (a final silent `e` dropped), each
/// digit one; at least 1.
pub fn syllables(text: &str) -> usize {
    let b = bare(text);
    let digits = b.chars().filter(char::is_ascii_digit).count();
    let letters: Vec<char> = b.chars().filter(|c| c.is_alphabetic()).collect();
    let mut groups = 0;
    let mut prev = false;
    for c in &letters {
        let v = matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
        if v && !prev {
            groups += 1;
        }
        prev = v;
    }
    if groups > 1 && letters.ends_with(&['e']) && !letters.ends_with(&['l', 'e']) {
        groups -= 1;
    }
    (groups + digits).max(1)
}

/// The level of a word from level frames (dBFS, `hop_ms` apart, frame 0 at `frames_t0_ms`
/// on the same clock as the word): the mean power of the loudest half of the frames it
/// covers, in dB. `None` when no frame covers it.
pub fn word_level(
    db: &[f32],
    hop_ms: u32,
    frames_t0_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Option<f64> {
    let hop = i64::from(hop_ms.max(1));
    let a = (start_ms - frames_t0_ms).div_euclid(hop).max(0);
    let b = ((end_ms - frames_t0_ms + hop - 1).div_euclid(hop)).max(a + 1);
    let a = usize::try_from(a).ok()?;
    let b = usize::try_from(b).ok()?.min(db.len());
    if a >= b {
        return None;
    }
    let mut v: Vec<f64> = db[a..b].iter().map(|d| f64::from(*d)).collect();
    v.sort_by(|x, y| y.total_cmp(x));
    let top = &v[..v.len().div_ceil(2)];
    let p = top.iter().map(|d| 10f64.powf(d / 10.0)).sum::<f64>() / top.len() as f64;
    Some(10.0 * p.max(1e-12).log10())
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// `(median, scale)` of a sample: scale = 1.4826·MAD, floored at half the standard
/// deviation (a sample of near-equal values) and at 1e-6.
fn baseline(v: &[f64]) -> Option<(f64, f64)> {
    if v.len() < 4 {
        return None;
    }
    let med = median(&mut v.to_vec())?;
    let mad = median(&mut v.iter().map(|x| (x - med).abs()).collect::<Vec<_>>())?;
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    Some((med, (1.4826 * mad).max(0.5 * sd).max(1e-6)))
}

/// Whether word `i` ends a phrase: the last word, a sentence end, or a pause after it.
fn phrase_final(words: &[EmWord], i: usize) -> bool {
    let t = words[i].text.trim_end();
    i + 1 == words.len()
        || t.ends_with(['.', '?', '!', ',', ';', ':'])
        || words[i + 1].start_ms - words[i].end_ms >= PAUSE_MS
}

/// The stressed words of a cut (in time order), at most one in [`WORDS_PER_STRESS`].
pub fn stressed(words: &[EmWord]) -> Vec<Stress> {
    let n = words.len();
    if n < 4 {
        return Vec::new();
    }
    let pace: Vec<f64> = words
        .iter()
        .map(|w| (w.end_ms - w.start_ms).max(1) as f64 / syllables(&w.text) as f64)
        .collect();
    let fin: Vec<bool> = (0..n).map(|i| phrase_final(words, i)).collect();
    let mut cands: Vec<Stress> = Vec::new();
    for i in 0..n {
        if !eligible(&words[i].text) {
            continue;
        }
        let lo = i.saturating_sub(WINDOW);
        let hi = (i + WINDOW + 1).min(n);
        let around = |keep: &dyn Fn(usize) -> bool, v: &dyn Fn(usize) -> Option<f64>| {
            (lo..hi)
                .filter(|&j| j != i && keep(j) && !is_filler(&words[j].text))
                .filter_map(v)
                .collect::<Vec<f64>>()
        };
        let lv = around(&|_| true, &|j| words[j].level_db);
        let (z_level, db_over) = match (words[i].level_db, baseline(&lv)) {
            (Some(l), Some((m, s))) => ((l - m) / s, l - m),
            _ => (0.0, 0.0),
        };
        let pv = around(&|j| fin[j] == fin[i], &|j| Some(pace[j]));
        let (z_slow, pace_ratio) = match baseline(&pv) {
            Some((m, s)) => ((pace[i] - m) / s, pace[i] / m.max(1e-6)),
            None => (0.0, 1.0),
        };
        let score = z_level.max(z_slow).max(0.75 * (z_level + z_slow));
        if score < STRESS_Z {
            continue;
        }
        let mut what = Vec::new();
        if z_level >= 1.0 && db_over > 0.0 {
            what.push(format!(
                "louder: +{db_over:.1} dB over the usual here ({z_level:.1}σ)"
            ));
        }
        if z_slow >= 1.0 && pace_ratio > 1.0 {
            what.push(format!(
                "slower: {pace_ratio:.1}× the usual pace{} ({z_slow:.1}σ)",
                if fin[i] { " before a pause" } else { "" }
            ));
        }
        if what.is_empty() {
            what.push(format!(
                "louder and slower together ({:.1}σ, {:.1}σ)",
                z_level, z_slow
            ));
        }
        cands.push(Stress {
            index: i,
            z_level,
            z_slow,
            db_over,
            pace_ratio,
            score,
            label: what.join(" · "),
        });
    }
    let cap = (n / WORDS_PER_STRESS).max(1);
    cands.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
    let mut out: Vec<Stress> = Vec::new();
    for c in cands {
        if out.len() >= cap {
            break;
        }
        if out
            .iter()
            .any(|o| o.index.abs_diff(c.index) < MIN_GAP_WORDS)
        {
            continue;
        }
        out.push(c);
    }
    out.sort_by_key(|s| s.index);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` as words 300 ms long, 50 ms apart (a pause after each sentence), at −30 dB
    /// with ±3 dB and ±15 % of jitter.
    fn talk(text: &str) -> Vec<EmWord> {
        let mut t = 0;
        let mut seed: u64 = 11;
        text.split_whitespace()
            .map(|w| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let jitter = ((seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0;
                let d = (100 + ((seed >> 40) % 40) as i64) * syllables(w) as i64;
                let e = EmWord {
                    text: w.into(),
                    start_ms: t,
                    end_ms: t + d,
                    level_db: Some(-30.0 + 3.0 * jitter),
                };
                t += d + if w.ends_with('.') { 400 } else { 50 };
                e
            })
            .collect()
    }

    const TEXT: &str = "so we were able to generate that video on my own machine and it took \
        a while because the model is big. then I tried the other model and it was faster \
        than the first one which surprised me. the whole thing ran locally on the card and \
        nothing went to the cloud at all. that is the part I care about the most here.";

    #[test]
    fn a_louder_word_and_a_drawn_out_word_are_stressed() {
        let mut w = talk(TEXT);
        let gw = w.iter().position(|x| x.text == "generate").unwrap();
        w[gw].level_db = Some(-18.0);
        let loc = w.iter().position(|x| x.text == "locally").unwrap();
        let d = w[loc].end_ms - w[loc].start_ms;
        // drawn out ×2.2 (the words after it move later)
        for x in w.iter_mut().skip(loc + 1) {
            x.start_ms += d * 6 / 5;
            x.end_ms += d * 6 / 5;
        }
        w[loc].end_ms += d * 6 / 5;
        let s = stressed(&w);
        let picked: Vec<&str> = s.iter().map(|x| w[x.index].text.as_str()).collect();
        assert!(picked.contains(&"generate"), "{picked:?}");
        assert!(picked.contains(&"locally"), "{picked:?}");
        let g = s.iter().find(|x| w[x.index].text == "generate").unwrap();
        assert!(g.label.starts_with("louder: +"), "{}", g.label);
        assert!(g.db_over > 7.0, "{}", g.db_over);
        let l = s.iter().find(|x| w[x.index].text == "locally").unwrap();
        assert!(l.label.contains("slower: 2."), "{}", l.label);
        // sparse: one in ten words at most
        assert!(s.len() <= w.len() / WORDS_PER_STRESS, "{}", s.len());
    }

    #[test]
    fn the_same_speech_quieter_or_louder_marks_the_same_words() {
        let mut w = talk(TEXT);
        let gw = w.iter().position(|x| x.text == "generate").unwrap();
        w[gw].level_db = Some(-18.0);
        let a: Vec<usize> = stressed(&w).iter().map(|s| s.index).collect();
        for x in &mut w {
            x.level_db = x.level_db.map(|l| l - 25.0);
        }
        let b: Vec<usize> = stressed(&w).iter().map(|s| s.index).collect();
        assert_eq!(a, b, "thresholds come from the cut's own levels");
        assert!(a.contains(&gw));
    }

    #[test]
    fn fillers_weak_words_and_flat_speech_are_never_stressed() {
        let mut w = talk(TEXT);
        // a shouted "the" and a long "um"
        let the = w.iter().position(|x| x.text == "the").unwrap();
        w[the].level_db = Some(-15.0);
        w.insert(
            3,
            EmWord {
                text: "um".into(),
                start_ms: w[2].end_ms + 10,
                end_ms: w[2].end_ms + 900,
                level_db: Some(-12.0),
            },
        );
        let s = stressed(&w);
        assert!(
            s.iter()
                .all(|x| !matches!(w[x.index].text.as_str(), "the" | "um")),
            "{s:?}"
        );
        // flat talk: nothing stands out
        let flat: Vec<EmWord> = (0..60)
            .map(|i| EmWord {
                text: "model".into(),
                start_ms: i * 400,
                end_ms: i * 400 + 300,
                level_db: Some(-30.0),
            })
            .collect();
        assert!(stressed(&flat).is_empty());
    }

    #[test]
    fn phrase_final_lengthening_alone_is_not_stress() {
        // every sentence's last word drawn out ×1.8: compared with each other, none stands out
        let mut w = talk(TEXT);
        let mut shift = 0;
        for x in &mut w {
            x.start_ms += shift;
            x.end_ms += shift;
            if x.text.ends_with('.') {
                let d = (x.end_ms - x.start_ms) * 4 / 5;
                x.end_ms += d;
                shift += d;
            }
        }
        // a drawn-out last word is compared with the other last words, never with the rest
        for x in stressed(&w) {
            if w[x.index].text.ends_with('.') {
                assert!(x.z_slow < STRESS_Z, "{} {}", w[x.index].text, x.label);
            }
        }
    }

    #[test]
    fn levels_syllables_and_gaps() {
        assert_eq!(syllables("generate"), 3);
        assert_eq!(syllables("GPU,"), 1);
        assert_eq!(syllables("5080"), 4);
        assert_eq!(syllables("little"), 2);
        let db = [-60.0f32, -30.0, -20.0, -20.0, -30.0, -60.0];
        // frames 1..5 (40 ms hop) cover 40..200 ms: the loudest half is −20, −20
        let l = word_level(&db, 40, 0, 40, 200).unwrap();
        assert!((l + 20.0).abs() < 1e-6, "{l}");
        assert!(word_level(&db, 40, 0, 400, 500).is_none());
        // never two within MIN_GAP_WORDS
        let mut w = talk(TEXT);
        for k in [5, 6, 7] {
            w[k].level_db = Some(-18.0);
        }
        let s = stressed(&w);
        for p in s.windows(2) {
            assert!(p[1].index - p[0].index >= MIN_GAP_WORDS);
        }
    }
}
