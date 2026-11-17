//! Reaction moments (Ideas 2026-10-16, K.06): where the stream laughs or reacts big.
//!
//! Two signals, each measured against **the source's own statistics** (never a fixed dB):
//!
//! - **Sound**: the level in ~100 ms frames, smoothed over 0.5 s, as a robust z-score
//!   against the whole source (median and MAD), and the **onset** — how far the level
//!   rose within 0.3 s — z-scored the same way. A spike is a local peak of the level at
//!   [`SPIKE_Z`] or more, or a sharp rise ([`ONSET_Z`]) into a level above the usual
//!   (speech rises sharply after every pause, back to its ordinary level), at most one
//!   per [`GAP_MS`].
//! - **Words**: transcript cues within ±[`NEAR_MS`] of the peak — laughter ("haha",
//!   "lol", "lmao") and reactions ("wow", "no way", "oh my god", "let's go", "insane").
//!
//! A spike with a cue is a candidate; a spike without one only when it is very large
//! ([`ALONE_Z`]: laughter is often not transcribed at all). Each candidate points at what
//! caused it: the [`LEAD_MS`] before the peak to [`TAIL_MS`] after it.
//!
//! Precision is **not measured**: no labelled laughter exists for the lab streams yet.

/// One transcript word, in milliseconds of the source.
#[derive(Clone, Debug, PartialEq)]
pub struct CueWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// What made a candidate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionKind {
    /// A sound spike with laughter in the words.
    Laugh,
    /// A sound spike with a reaction word or phrase.
    Reaction,
    /// A very large sound spike with no cue in the words.
    Loud,
}

/// One candidate.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Reaction {
    /// The spike's peak (source ms).
    pub peak_ms: i64,
    /// The span it points at: [`LEAD_MS`] before the peak to [`TAIL_MS`] after.
    pub start_ms: i64,
    pub end_ms: i64,
    /// The spike's size: z of the level and of the onset against the source's own.
    pub z_level: f64,
    pub z_onset: f64,
    pub kind: ReactionKind,
    /// The cue words heard near the peak (transcript text, untrusted).
    pub cues: Vec<String>,
    /// 2–5 for the moment card: from the spike's size, a cue adds one.
    pub score: u8,
}

/// Frame of the level series (ms).
pub const FRAME_MS: i64 = 100;
/// A spike: a z of this or more (the level's or the onset's).
pub const SPIKE_Z: f64 = 2.5;
/// A sharp rise (z of the onset) makes a spike when the level it rises to is at least
/// [`ONSET_LEVEL_Z`] above the source's usual.
pub const ONSET_Z: f64 = 4.0;
pub const ONSET_LEVEL_Z: f64 = 1.5;
/// A spike with no cue in the words counts only at this size.
pub const ALONE_Z: f64 = 4.5;
/// Cue words this close to the peak count.
pub const NEAR_MS: i64 = 3_000;
/// One candidate per this much source.
pub const GAP_MS: i64 = 20_000;
/// The candidate's span: what led to the reaction, and the reaction itself.
pub const LEAD_MS: i64 = 15_000;
pub const TAIL_MS: i64 = 3_000;
/// The most candidates one source gives.
pub const MAX: usize = 40;

const LAUGH: &[&str] = &[
    "haha", "hahaha", "hahahaha", "lol", "lmao", "hehe", "lmfao", "rofl",
];
const REACT: &[&str] = &[
    "wow",
    "whoa",
    "woah",
    "omg",
    "holy",
    "insane",
    "crazy",
    "unbelievable",
    "incredible",
    "nuts",
    "wild",
    "yes",
    "yeah",
    "damn",
    "dang",
    "oops",
    "ooh",
    "wooo",
    "woo",
    "yay",
];
const PHRASES: &[&[&str]] = &[
    &["no", "way"],
    &["oh", "my", "god"],
    &["oh", "my", "gosh"],
    &["let's", "go"],
    &["lets", "go"],
    &["what", "the"],
    &["are", "you", "kidding"],
    &["i", "can't", "believe"],
];

fn tok(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_laugh(t: &str) -> bool {
    LAUGH.contains(&t)
        || (t.len() >= 4
            && (t.starts_with("ha") || t.starts_with("he"))
            && t.chars().all(|c| matches!(c, 'h' | 'a' | 'e')))
}

/// The cue words of a transcript: `(index, is laughter)`. A single reaction word counts
/// only when it is said with an exclamation mark or stands alone in its sentence ("wow."),
/// so "yes, the GPU" is not a reaction; laughter always counts; phrases always count.
pub fn cues(words: &[CueWord]) -> Vec<(usize, bool)> {
    let toks: Vec<String> = words.iter().map(|w| tok(&w.text)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let t = toks[i].as_str();
        if is_laugh(t) {
            out.push((i, true));
            i += 1;
            continue;
        }
        if let Some(p) = PHRASES
            .iter()
            .find(|p| toks.len() - i >= p.len() && p.iter().zip(&toks[i..]).all(|(a, b)| a == b))
        {
            out.extend((i..i + p.len()).map(|k| (k, false)));
            i += p.len();
            continue;
        }
        if REACT.contains(&t) {
            let text = words[i].text.trim();
            let alone = text.ends_with('!')
                || ((i == 0 || words[i - 1].text.ends_with(['.', '?', '!']))
                    && text.ends_with(['.', '?', '!']));
            if alone {
                out.push((i, false));
            }
        }
        i += 1;
    }
    out
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Robust z-scores against the series' own median and MAD (scale floored so a perfectly
/// flat source never divides by zero).
fn robust_z(x: &[f64]) -> Vec<f64> {
    let mut a = x.to_vec();
    let med = median(&mut a);
    let mut dev: Vec<f64> = x.iter().map(|v| (v - med).abs()).collect();
    let mad = median(&mut dev);
    let scale = (1.4826 * mad).max(1e-3);
    x.iter().map(|v| (v - med) / scale).collect()
}

/// Level frames (dBFS, `hop_ms` apart) pooled by mean power into frames of about
/// [`FRAME_MS`]: `(levels, frame length in ms)` (a whole number of hops).
pub fn pooled(db: &[f32], hop_ms: u32) -> (Vec<f64>, i64) {
    let hop = i64::from(hop_ms.max(1));
    let k = ((FRAME_MS + hop / 2) / hop).max(1) as usize;
    let v = db
        .chunks(k)
        .map(|c| {
            let p = c
                .iter()
                .map(|d| 10f64.powf(f64::from(*d) / 10.0))
                .sum::<f64>()
                / c.len() as f64;
            10.0 * p.max(1e-12).log10()
        })
        .collect();
    (v, hop * k as i64)
}

/// The candidates of a source: its level frames (`db`, `hop_ms` apart, from 0) and its
/// transcript words. Strongest first is how they are chosen; they come back in time order.
pub fn find(db: &[f32], hop_ms: u32, words: &[CueWord]) -> Vec<Reaction> {
    let (lv, frame) = pooled(db, hop_ms);
    let n = lv.len();
    if n < 10 {
        return Vec::new();
    }
    let frames = |ms: i64| {
        usize::try_from((ms + frame / 2) / frame)
            .unwrap_or(1)
            .max(1)
    };
    // 0.5 s smoothing (mean power), then the rise within 0.3 s
    let r = frames(250);
    let sm: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(r), (i + r).min(n - 1));
            let p =
                lv[a..=b].iter().map(|d| 10f64.powf(d / 10.0)).sum::<f64>() / (b - a + 1) as f64;
            10.0 * p.max(1e-12).log10()
        })
        .collect();
    let back = frames(300);
    let rise: Vec<f64> = (0..n)
        .map(|i| {
            let lo = sm[i.saturating_sub(back)..=i]
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            sm[i] - lo
        })
        .collect();
    let zl = robust_z(&sm);
    let zo = robust_z(&rise);
    // a sharp rise counts only into a level above the source's usual: speech rises sharply
    // after every pause, back to its ordinary level
    let score: Vec<f64> = (0..n)
        .map(|i| {
            if zo[i] >= ONSET_Z && zl[i] >= ONSET_LEVEL_Z {
                zl[i].max(SPIKE_Z)
            } else {
                zl[i]
            }
        })
        .collect();
    let cue = cues(words);
    let mut peaks: Vec<usize> = (0..n)
        .filter(|&i| score[i] >= SPIKE_Z)
        .filter(|&i| {
            (i == 0 || score[i] >= score[i - 1]) && (i + 1 == n || score[i] > score[i + 1])
        })
        .collect();
    peaks.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let mut out: Vec<Reaction> = Vec::new();
    for p in peaks {
        let peak_ms = p as i64 * frame + frame / 2;
        if out.iter().any(|r| (r.peak_ms - peak_ms).abs() < GAP_MS) {
            continue;
        }
        let near: Vec<&(usize, bool)> = cue
            .iter()
            .filter(|(i, _)| {
                let w = &words[*i];
                w.end_ms >= peak_ms - NEAR_MS && w.start_ms <= peak_ms + NEAR_MS
            })
            .collect();
        let laugh = near.iter().any(|c| c.1);
        let kind = if laugh {
            ReactionKind::Laugh
        } else if !near.is_empty() {
            ReactionKind::Reaction
        } else if score[p] >= ALONE_Z {
            ReactionKind::Loud
        } else {
            continue;
        };
        let size = score[p];
        let base: u8 = if size >= 6.0 {
            4
        } else if size >= 4.0 {
            3
        } else {
            2
        };
        let score_card = (base + u8::from(kind != ReactionKind::Loud)).min(5);
        out.push(Reaction {
            peak_ms,
            start_ms: (peak_ms - LEAD_MS).max(0),
            end_ms: peak_ms + TAIL_MS,
            z_level: (zl[p] * 100.0).round() / 100.0,
            z_onset: (zo[p] * 100.0).round() / 100.0,
            kind,
            cues: near.iter().map(|(i, _)| words[*i].text.clone()).collect(),
            score: score_card,
        });
        if out.len() >= MAX {
            break;
        }
    }
    out.sort_by_key(|r| r.peak_ms);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of 10 ms level frames: speech-like syllables around `base` dB (each 250 ms
    /// somewhere in −8…+4 dB, a little jitter, a quiet pause every 3 s), with `bursts` of
    /// `(at_s, len_s, +dB)`.
    fn level(secs: usize, base: f32, bursts: &[(f64, f64, f32)]) -> Vec<f32> {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        let syll: Vec<f32> = (0..secs * 4).map(|_| next() * 12.0 - 8.0).collect();
        (0..secs * 100)
            .map(|i| {
                let t = i as f64 / 100.0;
                let jitter = ((i * 7919) % 13) as f32 * 0.2 - 1.2;
                let pause = if (t % 3.0) > 2.6 { -18.0 } else { 0.0 };
                let burst: f32 = bursts
                    .iter()
                    .filter(|(a, l, _)| t >= *a && t < a + l)
                    .map(|b| b.2)
                    .sum();
                base + syll[i / 25] + jitter + pause + burst
            })
            .collect()
    }

    fn w(text: &str, s: f64) -> CueWord {
        CueWord {
            text: text.into(),
            start_ms: (s * 1000.0) as i64,
            end_ms: (s * 1000.0) as i64 + 300,
        }
    }

    #[test]
    fn a_laugh_is_a_spike_with_laughter_in_the_words() {
        let db = level(
            300,
            -30.0,
            &[(60.0, 1.5, 16.0), (150.0, 1.0, 6.0), (240.0, 1.2, 30.0)],
        );
        let words = vec![
            w("so", 59.0),
            w("Haha", 60.4),
            w("hahaha.", 60.9),
            // a big spike at 150 s with no cue and a moderate size: not a candidate
            w("the", 150.2),
            // a cue with no sound at 200 s: not a candidate
            w("Wow!", 200.0),
            w("okay", 240.1),
        ];
        let r = find(&db, 10, &words);
        let at: Vec<(i64, ReactionKind)> = r.iter().map(|x| (x.peak_ms / 1000, x.kind)).collect();
        assert_eq!(
            at,
            [(60, ReactionKind::Laugh), (240, ReactionKind::Loud)],
            "{r:#?}"
        );
        let laugh = &r[0];
        assert_eq!(laugh.cues, ["Haha", "hahaha."]);
        assert_eq!(laugh.start_ms, laugh.peak_ms - LEAD_MS);
        assert!(laugh.score >= 3);
    }

    #[test]
    fn thresholds_follow_the_source_not_a_fixed_level() {
        // the same performance recorded 25 dB quieter finds the same moments
        let bursts = [(60.0, 1.5, 16.0), (120.0, 1.0, 14.0)];
        let words = vec![
            w("no", 60.2),
            w("way", 60.5),
            w("Let's", 120.1),
            w("go!", 120.4),
        ];
        let loud = find(&level(200, -20.0, &bursts), 10, &words);
        let quiet = find(&level(200, -45.0, &bursts), 10, &words);
        let peaks = |r: &[Reaction]| r.iter().map(|x| x.peak_ms).collect::<Vec<_>>();
        assert_eq!(peaks(&loud), peaks(&quiet));
        assert_eq!(loud.len(), 2);
        assert!(loud.iter().all(|r| r.kind == ReactionKind::Reaction));
        // and the 40 ms analysis hop finds them too
        let pooled40: Vec<f32> = level(200, -20.0, &bursts)
            .chunks(4)
            .map(|c| {
                let p = c
                    .iter()
                    .map(|d| 10f64.powf(f64::from(*d) / 10.0))
                    .sum::<f64>()
                    / 4.0;
                (10.0 * p.log10()) as f32
            })
            .collect();
        let at40 = peaks(&find(&pooled40, 40, &words));
        assert_eq!(at40.len(), 2);
        assert!(
            at40.iter()
                .zip(peaks(&loud))
                .all(|(a, b)| (a - b).abs() <= 200),
            "{at40:?}"
        );
    }

    #[test]
    fn cue_words_need_to_be_reactions() {
        let ws = [
            w("yes,", 0.0),
            w("the", 0.3),
            w("GPU", 0.6),
            w("Yes!", 1.0),
            w("wow.", 2.0),
            w("hahah", 3.0),
        ];
        let c: Vec<usize> = cues(&ws).iter().map(|c| c.0).collect();
        assert_eq!(c, [3, 4, 5]);
        assert!(find(&[], 10, &ws).is_empty());
    }
}
