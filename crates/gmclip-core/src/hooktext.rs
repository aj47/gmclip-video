//! Hook text with measured numbers (decision 0024; SPEC §10.3 item 7, R-AI-4): on-screen
//! hook lines of 5–8 words, each built on a specific number the clip measurably shows,
//! and each naming where that number comes from.
//!
//! Three sources, nothing else (a number is never invented, rounded past its display
//! rule or moved to another claim):
//! - **measured**: an interval measurement on the cut's media (a run clock's, or one
//!   marked on the preview); the line shows it as whole seconds under 100 s, else `m:ss`,
//!   both floored, so it agrees with its measurement by the §5.13 rule;
//! - **count**: how many measured runs the cut's media has (two or more), with the
//!   fastest;
//! - **spoken**: a number said in the cut (digits or number words), quoted as said: the
//!   line is a 5–8 word window of the transcript around it (fillers dropped, number words
//!   written as digits). A spoken duration that no measured run agrees with (within 15 %)
//!   is flagged and ranked last (AGENTS: drop spoken lines the screen contradicts).
//!
//! Pure and deterministic: no model. An agent may reword, but its numbers still need
//! this provenance (R-AI-4).

use std::collections::BTreeSet;

use gmclip_model::Doc;
use gmclip_model::Id;
use gmclip_model::anchor::Anchor;
use gmclip_model::clock::{ModelTime, cut_layout, src_to_cut};
use gmclip_model::entities::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analysis::AnalysisView;

const F: i64 = 705_600_000;
/// Suggestions returned by default.
pub const DEFAULT_LIMIT: usize = 6;

/// A word of the cut's transcript on the cut clock.
#[derive(Clone, Debug, PartialEq)]
pub struct CutWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

fn media_key(m: MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// The words the cut plays, in cut order: each part's words of its media's active
/// transcript whose middle falls inside the part.
pub fn cut_words(doc: &Doc, view: &AnalysisView, cut: Id<Cut>) -> Vec<CutWord> {
    let mut out = Vec::new();
    for sp in cut_layout(doc, cut) {
        let Some(p) = doc.get(sp.part) else { continue };
        let Some(t) = view.active(doc, &media_key(p.media)) else {
            continue;
        };
        let i = t.words.partition_point(|w| w.end <= p.src_in.0);
        for w in &t.words[i..] {
            let mid = (w.start + w.end) / 2;
            if mid >= p.src_out.0 {
                break;
            }
            if mid < p.src_in.0 {
                continue;
            }
            let at = |x: i64| {
                src_to_cut(
                    doc,
                    sp.part,
                    gmclip_model::Flicks(x.clamp(p.src_in.0, p.src_out.0)),
                )
                .map_or(0, |f| f.0 * 1000 / F)
            };
            out.push(CutWord {
                text: w.text.clone(),
                start_ms: at(w.start),
                end_ms: at(w.end),
            });
        }
    }
    out
}

/// Where a suggestion's number comes from.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NumberSource {
    Measured,
    Count,
    Spoken,
}

/// One hook line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HookSuggestion {
    /// The line, `*marked*` where the number is (5–8 words).
    pub text: String,
    /// The number as the line shows it.
    pub number: String,
    pub kind: NumberSource,
    /// Where the number comes from, in words (spoken quotes are untrusted transcript
    /// text).
    pub source: String,
    /// The measurement it shows, if measured.
    pub measurement: Option<String>,
    /// Where on the cut it is said, ms (spoken).
    pub at_ms: Option<i64>,
    /// What to record on the hook overlay (R-AI-4), one entry per number in the line.
    pub provenance: Vec<NumberProvenance>,
    /// Why to be careful with it (a spoken duration the measured runs disagree with).
    pub warning: Option<String>,
}

// ---------------------------------------------------------------------------------------
// measured and counted
// ---------------------------------------------------------------------------------------

/// A duration as a hook shows it: whole seconds under 100 s ("67 seconds"), else `m:ss`
/// ("1:47"), both floored; tenths under a second.
pub fn duration_text(secs: f64) -> String {
    if secs < 1.0 {
        format!("{:.1} seconds", (secs * 10.0).floor() / 10.0)
    } else if secs < 100.0 {
        let s = secs.floor() as i64;
        if s == 1 {
            "1 second".into()
        } else {
            format!("{s} seconds")
        }
    } else {
        let s = secs.floor() as i64;
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn number_of(shown: &str) -> String {
    shown.split(' ').next().unwrap_or(shown).to_owned()
}

/// The sources (and sync groups) a cut plays.
fn cut_media(doc: &Doc, cut: Id<Cut>) -> (BTreeSet<Id<Source>>, BTreeSet<Id<SyncGroup>>) {
    let mut sources = BTreeSet::new();
    let mut groups = BTreeSet::new();
    for p in doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
    {
        match p.media {
            MediaRef::Source(s) => {
                sources.insert(s);
            }
            MediaRef::Group(g) => {
                groups.insert(g);
                for m in doc
                    .children::<SyncMember>(g.entity())
                    .filter_map(|m| doc.get(m))
                {
                    sources.insert(m.source);
                }
            }
        }
    }
    (sources, groups)
}

/// The cut's measured runs: interval measurements in seconds that its overlays cite, or
/// whose ends are on a video the cut plays. Oldest first.
pub fn measured_runs(doc: &Doc, cut: Id<Cut>) -> Vec<(Id<Measurement>, f64, String)> {
    let (sources, groups) = cut_media(doc, cut);
    let mut cited: BTreeSet<Id<Measurement>> = BTreeSet::new();
    for o in doc
        .children::<Overlay>(cut.entity())
        .filter_map(|o| doc.get(o))
    {
        if let Some(c) = &o.counter {
            for s in [Some(&c.start), c.stop.as_ref()].into_iter().flatten() {
                if let CounterStart::Measurement(m) = s {
                    cited.insert(*m);
                }
            }
        }
        cited.extend(o.number_provenance.iter().filter_map(|p| p.measurement));
    }
    let on_cut = |e: &MeasureEnd| match e {
        MeasureEnd::Frame(f) => sources.contains(&f.source),
        MeasureEnd::Anchor(Anchor::Raw(r)) => match r.time {
            ModelTime::Src { source, group, .. } => {
                source.is_some_and(|s| sources.contains(&s))
                    || group.is_some_and(|g| groups.contains(&g))
            }
            _ => false,
        },
        _ => false,
    };
    doc.iter::<Measurement>()
        .filter(|m| m.kind == MeasurementKind::Interval)
        .filter(|m| cited.contains(&m.id) || on_cut(&m.from))
        .filter_map(|m| {
            let secs = match m.unit.as_str() {
                "s" => m.value,
                "ms" => m.value / 1000.0,
                _ => return None,
            };
            (secs.is_finite() && secs > 0.0).then(|| (m.id, secs, m.method.clone()))
        })
        .collect()
}

fn measured(runs: &[(Id<Measurement>, f64, String)]) -> Vec<HookSuggestion> {
    let mut out = Vec::new();
    for (id, secs, method) in runs {
        let d = duration_text(*secs);
        let n = number_of(&d);
        let source = if method.trim().is_empty() {
            format!("a measured run: {secs:.1} s")
        } else {
            format!("a measured run: {secs:.1} s ({})", method.trim())
        };
        let prov = vec![NumberProvenance {
            text: n.clone(),
            measurement: Some(*id),
            note: Some(format!("measured run, {secs:.3} s")),
            spoken: None,
        }];
        for text in [
            format!("Watch it finish in *{d}*"),
            format!("Start to finish: *{d}*, measured"),
        ] {
            out.push(HookSuggestion {
                text,
                number: n.clone(),
                kind: NumberSource::Measured,
                source: source.clone(),
                measurement: Some(id.0.to_string()),
                at_ms: None,
                provenance: prov.clone(),
                warning: None,
            });
        }
    }
    if runs.len() >= 2 {
        let (id, secs, _) = runs
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .expect("two runs");
        let d = duration_text(*secs);
        let k = runs.len();
        out.push(HookSuggestion {
            text: format!("{k} runs, the fastest in *{d}*"),
            number: k.to_string(),
            kind: NumberSource::Count,
            source: format!("{k} measured runs; the fastest {secs:.1} s"),
            measurement: Some(id.0.to_string()),
            at_ms: None,
            provenance: vec![
                NumberProvenance {
                    text: k.to_string(),
                    measurement: None,
                    note: Some(format!(
                        "count of the measured runs on this cut's video ({k})"
                    )),
                    spoken: None,
                },
                NumberProvenance {
                    text: number_of(&d),
                    measurement: Some(*id),
                    note: Some(format!("the fastest measured run, {secs:.3} s")),
                    spoken: None,
                },
            ],
            warning: None,
        });
    }
    out
}

// ---------------------------------------------------------------------------------------
// spoken
// ---------------------------------------------------------------------------------------

/// What a spoken number counts.
#[derive(Clone, Debug, PartialEq)]
pub enum Unit {
    /// Seconds per unit.
    Duration(f64),
    Percent,
    Money,
    Times,
    /// A count of the word after it.
    Count(String),
    /// A bare number (a model number, a year).
    Bare,
}

/// A number said in the cut: its value, unit and the words it spans.
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    pub value: f64,
    pub unit: Unit,
    /// Word indices `[first, last]`, the unit included.
    pub first: usize,
    pub last: usize,
    /// The number as digits ("2", "5,080", "$20", "50%").
    pub digits: String,
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "for", "with", "it", "is", "that",
    "this", "so", "but", "like", "um", "uh", "i", "you", "we", "they", "he", "she", "my", "your",
    "our", "more", "less", "than", "then", "was", "were", "be", "are", "of", "by", "from", "as",
    "if", "just", "really", "very", "about",
];
const FILLERS: &[&str] = &["um", "uh", "umm", "uhh", "erm", "hmm"];
const LEAD: &[&str] = &[
    "and", "so", "but", "like", "um", "uh", "yeah", "okay", "ok", "oh", "well", "or",
];

fn bare(w: &str) -> String {
    w.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '%' | '$' | '-')))
        .to_lowercase()
}

fn word_number(w: &str) -> Option<u64> {
    if let Some(i) = ONES.iter().position(|x| *x == w) {
        return Some(i as u64);
    }
    if let Some(i) = TENS.iter().position(|x| *x == w) {
        return Some(20 + 10 * i as u64);
    }
    // "twenty-five"
    let (a, b) = w.split_once('-')?;
    let t = TENS.iter().position(|x| *x == a)?;
    let o = ONES
        .iter()
        .position(|x| *x == b)
        .filter(|o| (1..10).contains(o))?;
    Some(20 + 10 * t as u64 + o as u64)
}

/// The number one word of a transcript says: "seven", "twenty-five", "7", "5,080".
pub(crate) fn said_number(w: &str) -> Option<f64> {
    let b = bare(w);
    word_number(&b)
        .map(|n| n as f64)
        .or_else(|| digit_number(&b).map(|(v, _, _)| v))
}

fn duration_unit(w: &str) -> Option<f64> {
    Some(match w {
        "second" | "seconds" | "sec" | "secs" => 1.0,
        "minute" | "minutes" | "min" | "mins" => 60.0,
        "hour" | "hours" | "hr" | "hrs" => 3600.0,
        "day" | "days" => 86_400.0,
        "week" | "weeks" => 604_800.0,
        _ => return None,
    })
}

fn unit_after(words: &[CutWord], j: usize) -> Option<(Unit, usize)> {
    let w = bare(&words.get(j)?.text);
    if let Some(s) = duration_unit(&w) {
        return Some((Unit::Duration(s), j));
    }
    match w.as_str() {
        "percent" | "%" => Some((Unit::Percent, j)),
        "dollars" | "dollar" | "bucks" => Some((Unit::Money, j)),
        "times" | "x" => Some((Unit::Times, j)),
        _ if w.chars().all(char::is_alphabetic)
            && !STOP.contains(&w.as_str())
            && word_number(&w).is_none() =>
        {
            Some((Unit::Count(w), j))
        }
        _ => None,
    }
}

/// A number written in digits: `67`, `5,080`, `2.5`, `$20`, `50%`, `10x`, `16GB`.
fn digit_number(w: &str) -> Option<(f64, Option<Unit>, String)> {
    let money = w.starts_with('$');
    let t = w.trim_start_matches('$');
    let end = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, ',' | '.')))
        .map_or(t.len(), |(i, _)| i);
    let (num, suffix) = t.split_at(end);
    let num = num.trim_end_matches(['.', ',']);
    if num.is_empty() || !num.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let v: f64 = num.replace(',', "").parse().ok()?;
    let unit = match suffix {
        "%" => Some(Unit::Percent),
        "x" => Some(Unit::Times),
        "" => money.then_some(Unit::Money),
        s if s.starts_with('-') => duration_unit(&s[1..]).map(Unit::Duration),
        s => duration_unit(s).map(Unit::Duration),
    };
    let digits = format!("{}{num}{}", if money { "$" } else { "" }, suffix);
    Some((v, unit, digits))
}

/// Every number said in `words`.
pub fn spoken_numbers(words: &[CutWord]) -> Vec<Spoken> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = bare(&words[i].text);
        // digits
        if let Some((v, unit, digits)) = digit_number(&w) {
            let (unit, last) = match unit {
                Some(u) => (u, i),
                None => unit_after(words, i + 1).unwrap_or((Unit::Bare, i)),
            };
            if unit == Unit::Bare {
                i += 1;
                continue;
            }
            out.push(Spoken {
                value: v,
                unit,
                first: i,
                last,
                digits,
            });
            i = last + 1;
            continue;
        }
        // number words, maybe a compound ("twenty five"), maybe hyphenated to their unit
        // ("five-second")
        let (head, hyphen_unit) = match w.split_once('-') {
            Some((a, b)) if word_number(&w).is_none() => (a.to_owned(), duration_unit(b)),
            _ => (w.clone(), None),
        };
        let Some(mut v) = word_number(&head) else {
            i += 1;
            continue;
        };
        let mut j = i;
        if hyphen_unit.is_none()
            && (20..100).contains(&v)
            && v % 10 == 0
            && let Some(o) = words
                .get(i + 1)
                .and_then(|x| word_number(&bare(&x.text)))
                .filter(|o| (1..10).contains(o))
        {
            v += o;
            j = i + 1;
        }
        if let Some(n) = words.get(j + 1).map(|x| bare(&x.text))
            && (n == "hundred" || n == "thousand")
        {
            v *= if n == "hundred" { 100 } else { 1000 };
            j += 1;
        }
        let (unit, last) = match hyphen_unit {
            Some(s) => (Unit::Duration(s), i),
            None => match unit_after(words, j + 1) {
                Some(x) => x,
                None => (Unit::Bare, j),
            },
        };
        // "one" and "zero" are pronouns and idioms far more often than counts: only with
        // a unit that makes them a quantity
        let quantity = matches!(
            unit,
            Unit::Duration(_) | Unit::Percent | Unit::Money | Unit::Times
        );
        if (v <= 1 && !quantity) || matches!(unit, Unit::Bare) {
            i = j + 1;
            continue;
        }
        out.push(Spoken {
            value: v as f64,
            unit,
            first: i,
            last,
            digits: v.to_string(),
        });
        i = last + 1;
    }
    out
}

fn sentence_end(w: &str) -> bool {
    w.trim_end_matches(['"', '\'', ')'])
        .ends_with(['.', '?', '!'])
}

/// The line for a spoken number: the sentence around it (fillers dropped, leading "and
/// so" trimmed), at most 8 words with the number in it, at least 5; number words as
/// digits and the number and its unit marked.
fn spoken_line(words: &[CutWord], s: &Spoken) -> Option<(String, String)> {
    let n = words.len();
    let mut a = s.first;
    while a > 0 && !sentence_end(&words[a - 1].text) {
        a -= 1;
    }
    let mut b = s.last;
    while b + 1 < n && !sentence_end(&words[b].text) {
        b += 1;
    }
    let keep =
        |k: usize| k >= s.first && k <= s.last || !FILLERS.contains(&bare(&words[k].text).as_str());
    let mut idx: Vec<usize> = (a..=b).filter(|&k| keep(k)).collect();
    while idx
        .first()
        .is_some_and(|&k| k < s.first && LEAD.contains(&bare(&words[k].text).as_str()))
    {
        idx.remove(0);
    }
    // at most 8: keep up to 5 words before the number, then what follows
    while idx.len() > 8 {
        let before = idx.iter().filter(|&&k| k < s.first).count();
        if before > 5 || idx.last().is_some_and(|&k| k <= s.last) {
            idx.remove(0);
        } else {
            idx.pop();
        }
    }
    // a short sentence borrows the words around it
    let (mut lo, mut hi) = (idx.first().copied()?, idx.last().copied()?);
    while idx.len() < 5 {
        if hi + 1 < n {
            hi += 1;
            if keep(hi) {
                idx.push(hi);
            }
        } else if lo > 0 {
            lo -= 1;
            if keep(lo) {
                idx.insert(0, lo);
            }
        } else {
            return None;
        }
    }
    let quote = idx
        .iter()
        .map(|&k| words[k].text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: Vec<String> = Vec::new();
    for (pos, &k) in idx.iter().enumerate() {
        let raw = words[k].text.trim();
        let mut t = if k == s.first {
            // the number as digits, keeping a hyphenated unit ("5-second")
            let tail = bare(raw)
                .split_once('-')
                .filter(|_| word_number(&bare(raw)).is_none())
                .map(|(_, u)| format!("-{u}"))
                .unwrap_or_default();
            let digit_form = digit_number(&bare(raw)).is_some();
            if digit_form {
                raw.to_owned()
            } else {
                let core = raw.trim_end_matches([',', '.', '!', '?', ';', ':']);
                format!("{}{tail}{}", s.digits, &raw[core.len()..])
            }
        } else if k > s.first && k <= s.last && word_number(&bare(raw)).is_some() {
            // the rest of a compound number ("twenty *five*") is in the digits already
            continue;
        } else if k > s.first && k <= s.last && matches!(bare(raw).as_str(), "hundred" | "thousand")
        {
            continue;
        } else {
            raw.to_owned()
        };
        if k == s.first {
            t = format!("*{t}");
        }
        if k == s.last {
            let trimmed = t
                .trim_end_matches([',', '.', '!', '?', ';', ':'])
                .to_owned();
            let punct = t[trimmed.len()..].to_owned();
            t = format!("{trimmed}*{punct}");
        }
        if pos == 0 {
            let mut c = t.chars();
            if let Some(f) = c.next() {
                t = f.to_uppercase().chain(c).collect();
            }
        }
        out.push(t);
    }
    let mut line = out.join(" ");
    // the line ends on a sentence's end, or without a dangling comma
    let last_k = *idx.last()?;
    if !sentence_end(&words[last_k].text) {
        while line.ends_with([',', ';', ':']) {
            line.pop();
        }
    }
    // "*" may land after the first char's capital: "*67" stays "*67"
    let wc = line.split_whitespace().count();
    (5..=8).contains(&wc).then_some((line, quote))
}

fn rank(u: &Unit) -> u8 {
    match u {
        Unit::Duration(_) => 0,
        Unit::Money | Unit::Percent | Unit::Times => 1,
        Unit::Count(_) => 2,
        Unit::Bare => 3,
    }
}

fn clock_text(ms: i64) -> String {
    let s = ms as f64 / 1000.0;
    format!("{}:{:04.1}", (s / 60.0).floor() as i64, s % 60.0)
}

fn spoken(words: &[CutWord], runs: &[(Id<Measurement>, f64, String)]) -> Vec<(u8, HookSuggestion)> {
    let mut out = Vec::new();
    for s in spoken_numbers(words) {
        let Some((text, quote)) = spoken_line(words, &s) else {
            continue;
        };
        let at = words[s.first].start_ms;
        let warning = match s.unit {
            Unit::Duration(per) if !runs.is_empty() => {
                let said = s.value * per;
                let agrees = runs.iter().any(|(_, m, _)| (m - said).abs() <= 0.15 * m);
                let near = runs
                    .iter()
                    .find(|(_, m, _)| said > m / 4.0 && said < m * 4.0);
                match (agrees, near) {
                    (false, Some((_, m, _))) => Some(format!(
                        "the measured run says {m:.1} s: the screen may contradict this line"
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        let note = format!("said at {} in the cut: “{quote}”", clock_text(at));
        // one provenance entry per number the line shows (all from this quote)
        let provenance: Vec<NumberProvenance> = crate::lint::numbers(&text)
            .into_iter()
            .map(|(tok, _)| NumberProvenance {
                text: tok,
                measurement: None,
                note: Some(note.clone()),
                spoken: None,
            })
            .collect();
        let r = if warning.is_some() { 9 } else { rank(&s.unit) };
        out.push((
            r,
            HookSuggestion {
                text,
                number: s.digits.clone(),
                kind: NumberSource::Spoken,
                source: format!("said at {}: “{quote}”", clock_text(at)),
                measurement: None,
                at_ms: Some(at),
                provenance,
                warning,
            },
        ));
    }
    out.sort_by_key(|(r, h)| (*r, h.at_ms));
    out
}

/// The hook lines for a cut, best first: measured runs, then the count of runs, then
/// spoken numbers (durations, then money/percent/multiples, then counts; a contradicted
/// one last). Every line agrees with its provenance by the §5.13 display rule.
pub fn suggest(doc: &Doc, view: &AnalysisView, cut: Id<Cut>, limit: usize) -> Vec<HookSuggestion> {
    let runs = measured_runs(doc, cut);
    let words = cut_words(doc, view, cut);
    let mut out = measured(&runs);
    // one line per spoken number first, then the second template of each run
    let (first, second): (Vec<_>, Vec<_>) = out
        .drain(..)
        .enumerate()
        .partition(|(i, h)| h.kind == NumberSource::Count || i % 2 == 0);
    out = first.into_iter().map(|x| x.1).collect();
    out.extend(spoken(&words, &runs).into_iter().map(|x| x.1));
    out.extend(second.into_iter().map(|x| x.1));
    let mut seen = BTreeSet::new();
    out.retain(|h| seen.insert(h.text.to_lowercase()) && agrees_with_provenance(doc, h));
    out.truncate(limit);
    out
}

/// Every number in the line has its provenance, and a measured one agrees with its
/// measurement (the lint's rule).
fn agrees_with_provenance(doc: &Doc, h: &HookSuggestion) -> bool {
    crate::lint::numbers(&h.text)
        .into_iter()
        .all(|(tok, shown)| {
            let Some(p) = h.provenance.iter().find(|p| p.text == tok) else {
                return false;
            };
            match (p.measurement.and_then(|m| doc.get(m)), shown) {
                (Some(m), Some(s)) => {
                    let v = if m.unit == "ms" {
                        m.value / 1000.0
                    } else {
                        m.value
                    };
                    crate::lint::agrees(s, v)
                }
                (Some(_), None) => false,
                (None, _) => true,
            }
        })
}

/// The numbers a cut can back: every number said in it (digits or number words, with
/// or without a unit) and its measured runs. What a title or description may cite.
#[derive(Clone, Debug, Default)]
pub struct Grounds {
    said: Vec<f64>,
    runs: Vec<f64>,
}

impl Grounds {
    pub fn of(doc: &Doc, view: &AnalysisView, cut: Id<Cut>) -> Grounds {
        let words = cut_words(doc, view, cut);
        let mut said: Vec<f64> = spoken_numbers(&words).iter().map(|s| s.value).collect();
        for w in &words {
            let b = bare(&w.text);
            if let Some((v, _, _)) = digit_number(&b) {
                said.push(v);
            }
            let head = b.split_once('-').map_or(b.as_str(), |(a, _)| a);
            if let Some(v) = word_number(&b).or_else(|| word_number(head)) {
                said.push(v as f64);
            }
        }
        let runs = measured_runs(doc, cut).iter().map(|r| r.1).collect();
        Grounds { said, runs }
    }

    /// The first number in `text` the cut does not back, if any: a number said in it
    /// (as said), a measured run (by the §5.13 display rule) or how many runs there are.
    pub fn unbacked(&self, text: &str) -> Option<String> {
        crate::lint::numbers(text)
            .into_iter()
            .find_map(|(tok, shown)| {
                let Some(s) = shown else {
                    return Some(tok);
                };
                let said = self.said.iter().any(|v| (v - s.value).abs() < 1e-9);
                let run = self.runs.iter().any(|r| crate::lint::agrees(s, *r));
                let count = self.runs.len() >= 2 && (s.value - self.runs.len() as f64).abs() < 1e-9;
                (!(said || run || count)).then_some(tok)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(s: &str) -> Vec<CutWord> {
        s.split(' ')
            .enumerate()
            .map(|(i, t)| CutWord {
                text: t.into(),
                start_ms: i as i64 * 400,
                end_ms: i as i64 * 400 + 300,
            })
            .collect()
    }

    #[test]
    fn durations_show_floored_like_the_lint_reads_them() {
        assert_eq!(duration_text(67.43), "67 seconds");
        assert_eq!(duration_text(1.2), "1 second");
        assert_eq!(duration_text(0.46), "0.4 seconds");
        assert_eq!(duration_text(107.9), "1:47");
        assert_eq!(duration_text(3725.0), "62:05");
    }

    #[test]
    fn numbers_said_in_words_and_digits() {
        let w = ws("we were able to generate that five-second video in less than two minutes.");
        let n = spoken_numbers(&w);
        assert_eq!(n.len(), 2, "{n:?}");
        assert_eq!((n[0].value, n[0].unit.clone()), (5.0, Unit::Duration(1.0)));
        assert_eq!((n[1].value, n[1].unit.clone()), (2.0, Unit::Duration(60.0)));
        let w = ws("it costs $20 and runs 10x faster on twenty five GPUs, one of them");
        let n = spoken_numbers(&w);
        let got: Vec<(f64, Unit)> = n.iter().map(|s| (s.value, s.unit.clone())).collect();
        assert_eq!(
            got,
            vec![
                (20.0, Unit::Money),
                (10.0, Unit::Times),
                (25.0, Unit::Count("gpus".into()))
            ]
        );
        // a bare year, "one of": not hooks
        assert!(spoken_numbers(&ws("back in 2019 one of us")).is_empty());
        let n = spoken_numbers(&ws("about 50% of the 5,080 frames"));
        assert_eq!(n[0].unit, Unit::Percent);
        assert_eq!((n[1].value, n[1].digits.as_str()), (5080.0, "5,080"));
    }

    #[test]
    fn a_spoken_line_is_the_sentence_around_the_number() {
        let w = ws(
            "Okay. So um we generated that five-second video in less than two minutes. Then more.",
        );
        let n = spoken_numbers(&w);
        let (line, quote) = spoken_line(&w, &n[1]).unwrap();
        assert_eq!(line, "That five-second video in less than *2 minutes*.");
        assert!(quote.ends_with("two minutes."), "{quote}");
        let (line, _) = spoken_line(&w, &n[0]).unwrap();
        assert_eq!(line, "We generated that *5-second* video in less than");
        let c = line.split_whitespace().count();
        assert!((5..=8).contains(&c), "{line}");
    }
}
