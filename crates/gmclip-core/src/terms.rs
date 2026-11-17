//! The term gate (SPEC §5.10, D.04): caption words that spell a known name wrong.
//!
//! After a shipped caption read "XD.dev" for the spoken "exe.dev": every project or profile term is a canonical spelling (`written`) and the
//! variants the speech model has produced (`heard_as`). The gate walks the words a cut's
//! captions will show and reports:
//!
//! - **variant**: a word or a run of up to four consecutive words that, ignoring case,
//!   spaces and punctuation, is the term or one of its variants, but is not shown as the
//!   canonical spelling;
//! - **near**: a run that is not a listed variant but is one or two edits from the term
//!   (Damerau-Levenshtein on the letters and digits; one edit, two for a single word of
//!   9 or more), never a common English word or a contraction, never across a sentence
//!   end, and never shorter than 4 characters.
//!
//! It only reports, with the suggested spelling. Nothing is applied: the human (or an
//! agent, through `caption_edit`) decides.
//!
//! The same list drives the **correction pass** ([`correct_words`]): when a recording is
//! transcribed, every run that is exactly a term or a listed variant is spelt as the
//! term before the words are stored. It changes words, never the model, and never a near
//! miss (those stay the gate's to report).

use std::collections::BTreeMap;

use gmclip_model::entities::Term;

/// One caption word: its transcript ordinal and the text the captions show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateWord {
    pub ordinal: u32,
    pub text: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A listed variant (or the term in the wrong case or punctuation).
    Variant,
    /// Not listed, but within an edit or two of the term.
    Near,
}

/// One finding: the words (ordinals), what they show, and the suggested spelling.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub ordinals: Vec<u32>,
    pub shown: String,
    pub written: String,
    pub kind: FindingKind,
}

/// The longest run of words joined before the compare.
pub const MAX_RUN: usize = 4;

/// Letters and digits, lower-cased (spaces and punctuation ignored).
pub fn key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Optimal-string-alignment distance (adjacent transpositions count one), capped.
pub fn distance(a: &str, b: &str, cap: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > cap {
        return cap + 1;
    }
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, v) in d[0].iter_mut().enumerate() {
        *v = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

/// English words too common to ever be a misspelt name (the near check skips them).
const COMMON: &[&str] = &[
    "about",
    "after",
    "again",
    "against",
    "agent",
    "agents",
    "all",
    "already",
    "also",
    "always",
    "another",
    "any",
    "anything",
    "around",
    "back",
    "because",
    "been",
    "before",
    "being",
    "better",
    "between",
    "both",
    "build",
    "building",
    "but",
    "called",
    "came",
    "can",
    "cannot",
    "code",
    "codes",
    "coded",
    "come",
    "comes",
    "coming",
    "could",
    "data",
    "day",
    "did",
    "different",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "even",
    "every",
    "everything",
    "first",
    "for",
    "from",
    "game",
    "games",
    "get",
    "gets",
    "getting",
    "give",
    "going",
    "gonna",
    "good",
    "got",
    "great",
    "had",
    "has",
    "have",
    "having",
    "here",
    "hermit",
    "how",
    "into",
    "its",
    "just",
    "know",
    "last",
    "later",
    "let",
    "like",
    "line",
    "little",
    "local",
    "look",
    "looks",
    "looking",
    "made",
    "make",
    "makes",
    "making",
    "many",
    "maybe",
    "mean",
    "means",
    "might",
    "model",
    "models",
    "more",
    "most",
    "much",
    "must",
    "need",
    "never",
    "new",
    "next",
    "nice",
    "nine",
    "now",
    "off",
    "okay",
    "old",
    "once",
    "one",
    "only",
    "open",
    "other",
    "our",
    "out",
    "over",
    "own",
    "people",
    "pretty",
    "put",
    "quite",
    "really",
    "right",
    "router",
    "routers",
    "run",
    "running",
    "said",
    "same",
    "say",
    "says",
    "scale",
    "see",
    "seems",
    "set",
    "should",
    "show",
    "since",
    "some",
    "something",
    "still",
    "such",
    "sure",
    "take",
    "tale",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "though",
    "through",
    "time",
    "today",
    "too",
    "try",
    "trying",
    "tube",
    "two",
    "under",
    "until",
    "use",
    "used",
    "using",
    "very",
    "video",
    "want",
    "wanted",
    "was",
    "way",
    "well",
    "went",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "why",
    "will",
    "with",
    "work",
    "working",
    "works",
    "would",
    "yeah",
    "yes",
    "yet",
    "you",
    "your",
    "codex",
    "quick",
    "quite",
    "quiet",
    "clock",
    "cloud",
    "clouds",
    "claude",
    "open",
    "opens",
    "token",
    "tokens",
    "hub",
    "tick",
    "tock",
    "tail",
    "tails",
];

fn common(k: &str) -> bool {
    COMMON.contains(&k)
}

/// The term gate over a cut's caption words.
pub fn gate(words: &[GateWord], terms: &[Term]) -> Vec<Finding> {
    // key → (written, is the canonical key); variants and the canonical spelling
    let mut exact: BTreeMap<String, &str> = BTreeMap::new();
    for t in terms {
        let w = key(&t.written);
        if w.is_empty() {
            continue;
        }
        exact.insert(w, &t.written);
        for h in &t.heard_as {
            let k = key(h);
            if !k.is_empty() {
                exact.entry(k).or_insert(&t.written);
            }
        }
    }
    let canon: Vec<(String, &str)> = terms
        .iter()
        .map(|t| (key(&t.written), t.written.as_str()))
        .filter(|(k, _)| k.chars().count() >= 4)
        .collect();
    let keys: Vec<String> = words.iter().map(|w| key(&w.text)).collect();
    let mut out: Vec<Finding> = Vec::new();
    let mut taken = vec![false; words.len()];
    // exact matches first (longest runs first, so "nine router" wins over "router"),
    // then near misses among the words left
    for near in [false, true] {
        for n in (1..=MAX_RUN).rev() {
            for i in 0..words.len().saturating_sub(n - 1) {
                if taken[i..i + n].iter().any(|t| *t) || keys[i..i + n].iter().any(String::is_empty)
                {
                    continue;
                }
                let joined: String = keys[i..i + n].concat();
                let hit = if !near {
                    exact.get(&joined).map(|w| (*w, FindingKind::Variant))
                } else {
                    let len = joined.chars().count();
                    let all_common = keys[i..i + n].iter().all(|k| common(k));
                    // not across a sentence end, not a contraction ("here's")
                    let broken = words[i..i + n - 1]
                        .iter()
                        .any(|w| w.text.ends_with(['.', '?', '!', ',', ';', ':']))
                        || words[i..i + n].iter().any(|w| w.text.contains(['\'', '’']));
                    if len < 4 || all_common || broken {
                        None
                    } else {
                        canon.iter().find_map(|(k, w)| {
                            // a run of words gets one edit; a single word two from 9 letters
                            let cap = if n == 1 && k.chars().count() >= 9 {
                                2
                            } else {
                                1
                            };
                            (k.chars().count() >= 5
                                && joined.chars().next() == k.chars().next()
                                && distance(&joined, k, cap) <= cap)
                                .then_some((*w, FindingKind::Near))
                        })
                    }
                };
                let Some((written, kind)) = hit else { continue };
                for t in &mut taken[i..i + n] {
                    *t = true;
                }
                let shown = words[i..i + n]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                // shown exactly as the term (trailing punctuation aside): nothing to say
                if shown.trim_end_matches(['.', ',', '?', '!', ':', ';']) == written {
                    continue;
                }
                out.push(Finding {
                    ordinals: words[i..i + n].iter().map(|w| w.ordinal).collect(),
                    shown,
                    written: written.to_owned(),
                    kind,
                });
            }
        }
    }
    out.sort_by_key(|f| f.ordinals[0]);
    out
}

// ---------------------------------------------------------------------------------------
// The correction pass (applied when transcribing, never a model change)
// ---------------------------------------------------------------------------------------

/// One replacement the correction pass makes: words `[at, at + len)` become one word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub at: usize,
    pub len: usize,
    /// The term's spelling, with the last word's trailing punctuation.
    pub text: String,
}

const TAIL: [char; 6] = ['.', ',', '?', '!', ':', ';'];

/// What the correction pass changes in a run of words: every run of up to [`MAX_RUN`]
/// words that, ignoring case, spaces and punctuation, is a term or one of its listed
/// variants becomes the term's spelling (longest runs first). It never guesses: a near
/// miss is the gate's to report, a run never crosses a sentence end, and a run already
/// spelt as the term is left alone.
pub fn corrections(texts: &[&str], terms: &[Term]) -> Vec<Correction> {
    let mut exact: BTreeMap<String, &str> = BTreeMap::new();
    for t in terms {
        let w = key(&t.written);
        if w.is_empty() {
            continue;
        }
        exact.insert(w, &t.written);
        for h in &t.heard_as {
            let k = key(h);
            if !k.is_empty() {
                exact.entry(k).or_insert(&t.written);
            }
        }
    }
    if exact.is_empty() {
        return Vec::new();
    }
    let keys: Vec<String> = texts.iter().map(|w| key(w)).collect();
    let mut taken = vec![false; texts.len()];
    let mut out = Vec::new();
    for n in (1..=MAX_RUN).rev() {
        for i in 0..texts.len().saturating_sub(n - 1) {
            if taken[i..i + n].iter().any(|t| *t) || keys[i..i + n].iter().any(String::is_empty) {
                continue;
            }
            if texts[i..i + n - 1]
                .iter()
                .any(|w| w.ends_with(['.', '?', '!']))
            {
                continue;
            }
            let Some(written) = exact.get(&keys[i..i + n].concat()) else {
                continue;
            };
            for t in &mut taken[i..i + n] {
                *t = true;
            }
            let last = texts[i + n - 1];
            let tail = &last[last.trim_end_matches(TAIL).len()..];
            let text = format!("{written}{tail}");
            let shown = texts[i..i + n].join(" ");
            if shown == text {
                continue;
            }
            out.push(Correction {
                at: i,
                len: n,
                text,
            });
        }
    }
    out.sort_by_key(|c| c.at);
    out
}

/// How many times `heard` is said in `texts`: runs of up to [`MAX_RUN`] words equal to it
/// ignoring case, spaces and punctuation, never across a sentence end.
pub fn said(texts: &[&str], heard: &str) -> usize {
    let want = key(heard);
    if want.is_empty() {
        return 0;
    }
    let keys: Vec<String> = texts.iter().map(|w| key(w)).collect();
    let mut n = 0;
    let mut i = 0;
    while i < texts.len() {
        let hit = (1..=MAX_RUN.min(texts.len() - i)).find(|&len| {
            !keys[i].is_empty()
                && !texts[i..i + len - 1]
                    .iter()
                    .any(|w| w.ends_with(['.', '?', '!']))
                && keys[i..i + len].concat() == want
        });
        match hit {
            Some(len) => {
                n += 1;
                i += len;
            }
            None => i += 1,
        }
    }
    n
}

/// Applies [`corrections`] to words with times: a corrected run becomes one word from
/// its first word's start to its last word's end, with the least sure confidence.
/// Returns how many corrections were made.
pub fn correct_words(words: &mut Vec<crate::analysis::Word>, terms: &[Term]) -> usize {
    let fix = corrections(
        &words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
        terms,
    );
    for c in fix.iter().rev() {
        let run: Vec<crate::analysis::Word> = words.drain(c.at..c.at + c.len).collect();
        let confidence = run.iter().filter_map(|w| w.confidence).reduce(f64::min);
        words.insert(
            c.at,
            crate::analysis::Word {
                text: c.text.clone(),
                start: run.first().map_or(0, |w| w.start),
                end: run.last().map_or(0, |w| w.end),
                confidence,
            },
        );
    }
    fix.len()
}

/// The project's terms (`term_set`, `term_fix`).
pub fn project_terms(doc: &gmclip_model::Doc) -> Vec<Term> {
    doc.iter::<gmclip_model::entities::Terms>()
        .flat_map(|t| t.entries.values().cloned())
        .collect()
}

/// [`correct_words`] on a transcript, its lines made again from the corrected words.
pub fn correct_transcript(t: &mut crate::analysis::Transcript, terms: &[Term]) -> usize {
    let n = correct_words(&mut t.words, terms);
    if n > 0 {
        t.lines = crate::analysis::make_lines(&t.words);
    }
    n
}

/// [`correct_words`] on a stretch heard from a cut's own audio.
pub fn correct_heard(words: &mut Vec<crate::planner::heard::HeardWord>, terms: &[Term]) -> usize {
    let fix = corrections(
        &words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
        terms,
    );
    for c in fix.iter().rev() {
        let run: Vec<crate::planner::heard::HeardWord> = words.drain(c.at..c.at + c.len).collect();
        let confidence = run.iter().filter_map(|w| w.confidence).reduce(f64::min);
        words.insert(
            c.at,
            crate::planner::heard::HeardWord {
                text: c.text.clone(),
                start: run.first().map_or(0, |w| w.start),
                end: run.last().map_or(0, |w| w.end),
                confidence,
            },
        );
    }
    fix.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(s: &str) -> Vec<GateWord> {
        s.split_whitespace()
            .enumerate()
            .map(|(i, t)| GateWord {
                ordinal: i as u32,
                text: t.into(),
            })
            .collect()
    }

    fn terms() -> Vec<Term> {
        vec![
            Term {
                written: "exe.dev".into(),
                heard_as: vec!["xd.dev".into(), "x e dev".into(), "exceed dev".into()],
            },
            Term {
                written: "9Router".into(),
                heard_as: vec!["nine router".into(), "9 router".into()],
            },
            Term {
                written: "Cerebras".into(),
                heard_as: vec!["cerebrus".into()],
            },
            Term {
                written: "Tailscale".into(),
                heard_as: vec!["tail scale".into()],
            },
        ]
    }

    #[test]
    fn the_xd_dev_incident_and_runs() {
        let f = gate(
            &ws("so I deployed it on XD.dev and nine router picked Cerebras."),
            &terms(),
        );
        let got: Vec<(&str, &str, FindingKind)> = f
            .iter()
            .map(|x| (x.shown.as_str(), x.written.as_str(), x.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("XD.dev", "exe.dev", FindingKind::Variant),
                ("nine router", "9Router", FindingKind::Variant)
            ]
        );
        assert_eq!(f[1].ordinals, [7, 8]);
    }

    #[test]
    fn near_misses_and_no_common_words() {
        let f = gate(
            &ws("we used Cerebrass and Tailscal over the router"),
            &terms(),
        );
        let got: Vec<(&str, FindingKind)> = f.iter().map(|x| (x.shown.as_str(), x.kind)).collect();
        assert_eq!(
            got,
            [
                ("Cerebrass", FindingKind::Near),
                ("Tailscal", FindingKind::Near)
            ]
        );
        // a correctly spelt term, and plain words near nothing, say nothing
        assert!(gate(&ws("Tailscale is fine, the tail is scaled"), &terms()).is_empty());
    }

    fn heard(s: &str) -> Vec<crate::analysis::Word> {
        s.split_whitespace()
            .enumerate()
            .map(|(i, t)| crate::analysis::Word {
                text: t.into(),
                start: i as i64 * 100,
                end: i as i64 * 100 + 80,
                confidence: Some(0.5 + i as f64 / 100.0),
            })
            .collect()
    }

    fn texts(w: &[crate::analysis::Word]) -> Vec<&str> {
        w.iter().map(|x| x.text.as_str()).collect()
    }

    #[test]
    fn the_correction_pass_spells_listed_variants_and_merges_runs() {
        let mut w = heard("so I deployed it on XD.dev, then nine router picked cerebrus.");
        let n = correct_words(&mut w, &terms());
        assert_eq!(n, 3);
        assert_eq!(
            texts(&w),
            [
                "so",
                "I",
                "deployed",
                "it",
                "on",
                "exe.dev,",
                "then",
                "9Router",
                "picked",
                "Cerebras."
            ]
        );
        // a merged run spans its words and keeps the least sure confidence
        let r = &w[7];
        assert_eq!((r.start, r.end), (700, 880));
        assert_eq!(r.confidence, Some(0.5 + 7.0 / 100.0));
        // the canonical spelling in another case is the term; spelt right, nothing moves
        let mut w = heard("TAILSCALE and Tailscale and exe.dev");
        assert_eq!(correct_words(&mut w, &terms()), 1);
        assert_eq!(
            texts(&w),
            ["Tailscale", "and", "Tailscale", "and", "exe.dev"]
        );
    }

    #[test]
    fn the_correction_pass_never_guesses() {
        // near misses are the gate's to report, never corrected; nor across a sentence end
        let mut w = heard("we used Cerebrass on tail. Scale it");
        assert_eq!(correct_words(&mut w, &terms()), 0);
        assert_eq!(
            texts(&w),
            ["we", "used", "Cerebrass", "on", "tail.", "Scale", "it"]
        );
        // no terms, no words: nothing
        let mut w = heard("XD.dev");
        assert_eq!(correct_words(&mut w, &[]), 0);
        assert_eq!(correct_words(&mut Vec::new(), &terms()), 0);
    }

    #[test]
    fn said_counts_a_heard_form_in_one_or_several_words() {
        let t = ["on", "XD.dev,", "then", "XD", "dev", "and", "xd.", "Dev"];
        assert_eq!(said(&t, "XD.dev"), 2);
        assert_eq!(said(&t, ""), 0);
    }

    #[test]
    fn distance_counts_transpositions_once() {
        assert_eq!(distance("cerebras", "cerebars", 2), 1);
        assert_eq!(distance("abc", "abcdef", 1), 2);
    }
}
