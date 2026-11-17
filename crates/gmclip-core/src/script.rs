//! What a cut says (decision 0040): the words each part plays, in cut order, from the
//! media's active transcript, with what an editor listens for — silence inside and
//! around speech, parts that begin or end mid-sentence or mid-word, parts with no
//! speech, and retakes (SPEC §5.5: repeated or abandoned-then-restarted attempts; the
//! earlier attempt is the one to strike).
//!
//! One reading serves the agent surface (`cut_get{projection: words}`), lint (so a
//! check after an edit hears what the edit left) and the retakes sweep, so an agent,
//! a human's sweeps panel and the linter agree on what is said and what repeats.
//!
//! Times: words are on their media's clock (source flicks); `at` is the cut's output
//! clock, mapped linearly through each part's duration (a speed ramp bends it a
//! little; nothing here cuts on it).

use std::collections::BTreeSet;

use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{Cut, MediaRef, Part};
use gmclip_model::time::FLICKS_PER_SECOND;
use gmclip_model::{Doc, Id};
use serde::Serialize;

use crate::analysis::AnalysisView;

const SEC: i64 = FLICKS_PER_SECOND;
/// Silence at least this long inside or around a part's speech is dead air.
pub const GAP_S: f64 = 1.0;
/// A source word this close after another continues its sentence (no full stop).
const SAME_SENTENCE_S: f64 = 1.0;
/// ASR word times run on through the silence beside a word ("BB." 6.64–10.64 s,
/// "Continue" starting long before it is said, in AJ's recording), so:
/// - a part plays a word when it holds the largest share of it, at least this much
///   (or half the word, if shorter) …
const PLAYS_S: f64 = 0.12;
/// - a word is spoken for at most this long from its start (the rest is silence) …
const SPOKEN_MAX_S: f64 = 0.8;
/// - and an edge cuts a word only in a word timed no longer than this (a longer one is
///   mostly silence), at least this far inside it on both sides.
const CUT_WORD_MAX_S: f64 = 0.6;
const MID_WORD_S: f64 = 0.08;
/// Retakes are looked for this far back on the cut's clock (SPEC §5.5) …
const RETAKE_WINDOW_S: i64 = 20;
/// … and at most this many words back.
const RETAKE_MAX_WORDS: usize = 40;
/// An abandoned attempt is at most this many words.
const ABANDONED_MAX_WORDS: usize = 12;

/// One word a part plays.
#[derive(Clone, Debug, Serialize)]
pub struct ScriptWord {
    /// Its index in the transcript (the `word_id.ordinal` strikes name).
    pub ordinal: u32,
    pub text: String,
    pub src_start: i64,
    pub src_end: i64,
    /// Where the speech of it ends: measured on the voice when it is analysed (decision
    /// 0048), else its end, at most [`SPOKEN_MAX_S`] after its start.
    pub spoken_end: i64,
    /// Where it plays on the cut's clock.
    pub at: i64,
}

/// A silence inside a part's speech.
#[derive(Clone, Debug, Serialize)]
pub struct Gap {
    /// Cut-clock start, and its length (flicks).
    pub at: i64,
    pub len: i64,
}

/// One part as it sounds.
#[derive(Clone, Debug, Serialize)]
pub struct PartScript {
    pub index: usize,
    pub part: Id<Part>,
    pub media: MediaRef,
    pub owner: String,
    pub start: i64,
    pub dur: i64,
    pub src_in: i64,
    pub src_out: i64,
    /// The media has a transcript (else nothing here is known about its speech).
    pub transcript: bool,
    /// The video has no audio track: nothing can be said in it (a graphic, a screen
    /// capture without sound).
    pub no_audio: bool,
    pub words: Vec<ScriptWord>,
    /// Silence before the first word and after the last (the whole part when silent).
    pub lead: i64,
    pub trail: i64,
    /// Silences of at least [`GAP_S`] between its words.
    pub gaps: Vec<Gap>,
    /// Its first word continues a sentence the cut does not play the start of.
    pub starts_mid_sentence: bool,
    /// Its last word's sentence goes on in the source and the cut does not follow it.
    pub ends_mid_sentence: bool,
    /// An edge inside a word (the word is cut).
    pub cuts_word_in: Option<String>,
    pub cuts_word_out: Option<String>,
}

impl PartScript {
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Played but says nothing: a transcript with no word inside, or no audio at all.
    pub fn silent(&self) -> bool {
        self.words.is_empty() && (self.transcript || self.no_audio)
    }
}

/// Why an attempt counts as a retake.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetakeKind {
    /// Said again word for word ("I I", "you'll see all you'll see all").
    Repeated,
    /// Said again nearly the same (≥ 0.7 of the words, ≥ 0.85 for a whole sentence).
    Similar,
    /// Broken off and started again ("Alright, here is my first time / Alright, this
    /// is the first screen").
    Restarted,
}

/// One retake: the earlier attempt (to strike) and the attempt that replaces it.
#[derive(Clone, Debug, Serialize)]
pub struct Retake {
    pub kind: RetakeKind,
    pub earlier: String,
    pub later: String,
    /// Cut-clock start of the earlier attempt.
    pub at: i64,
    /// The earlier attempt's source spans, one per part it crosses: from its first
    /// word to where the next kept word starts in that part (the restart's breath
    /// goes with it).
    pub spans: Vec<(MediaRef, i64, i64)>,
    /// The parts it is in (cut indices).
    pub parts: Vec<usize>,
}

/// A cut as it sounds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Script {
    pub parts: Vec<PartScript>,
    pub retakes: Vec<Retake>,
    /// The transcript revisions read, by owner (what a sweep records).
    pub revisions: Vec<(String, String)>,
}

impl Script {
    /// Seconds of the cut with speech between its first and last word per part.
    pub fn silent_s(&self) -> f64 {
        self.parts
            .iter()
            .filter(|p| p.transcript)
            .map(|p| {
                (p.lead + p.trail + p.gaps.iter().map(|g| g.len).sum::<i64>()) as f64 / SEC as f64
            })
            .sum()
    }
}

fn owner_of(m: MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// Which of `ranges` (source in/out of the parts playing one media, in cut order) plays
/// the word `[start, end)`: the one holding the largest share of it, if that share is
/// at least [`PLAYS_S`] or half the word. Shared by the words read, lint, the retakes
/// sweep and the window's word strip, so they agree on what is said.
pub fn plays_in(start: i64, end: i64, ranges: &[(i64, i64)]) -> Option<usize> {
    let dur = (end - start).max(1);
    let need = (dur / 2).min((PLAYS_S * SEC as f64) as i64).max(1);
    ranges
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| (i, (end.min(b) - start.max(a)).max(0)))
        .filter(|&(_, o)| o >= need)
        .max_by_key(|&(i, o)| (o, std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

/// Ends a sentence (its text ends in . ? ! or …, quotes aside).
pub fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end_matches(['"', '\'', ')', ']', '”', '’']);
    t.ends_with(['.', '?', '!', '…'])
}

/// A word for comparing attempts: lower case, letters and digits only.
pub fn token(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// What `cut` says, from the active transcripts in `view`.
pub fn script(doc: &Doc, view: &AnalysisView, cut: Id<Cut>) -> Script {
    let mut out = Script::default();
    let mut revs: BTreeSet<(String, String)> = BTreeSet::new();
    let layout = cut_layout(doc, cut);
    // each media's parts in cut order: which one plays a word is decided among them
    let mut ranges: std::collections::BTreeMap<String, Vec<(usize, i64, i64)>> =
        std::collections::BTreeMap::new();
    for (k, sp) in layout.iter().enumerate() {
        if let Some(p) = doc.get(sp.part) {
            ranges
                .entry(owner_of(p.media))
                .or_default()
                .push((k, p.src_in.0, p.src_out.0));
        }
    }
    for (k, sp) in layout.iter().enumerate() {
        let Some(p) = doc.get(sp.part) else { continue };
        let owner = owner_of(p.media);
        let mine = ranges.get(&owner).cloned().unwrap_or_default();
        let spans: Vec<(i64, i64)> = mine.iter().map(|&(_, a, b)| (a, b)).collect();
        let me = mine.iter().position(|&(kk, _, _)| kk == k);
        let t = view.active(doc, &owner);
        if let Some(t) = t {
            revs.insert((owner.clone(), t.revision.clone()));
        }
        let (a, b) = (p.src_in.0, p.src_out.0);
        let len = (b - a).max(1);
        let to_cut =
            |src: i64| sp.start.0 + ((src - a) as i128 * sp.dur.0 as i128 / len as i128) as i64;
        let mut words = Vec::new();
        let (mut cut_in, mut cut_out) = (None, None);
        if let Some(t) = t {
            let mw = (MID_WORD_S * SEC as f64) as i64;
            let secs = |x: f64| (x * SEC as f64) as i64;
            // where each word is heard, measured on the voice (decision 0048), else the
            // ASR's times with the long ones (run on into silence) not judged; a soft
            // onset measures late, so a word starts at the earlier of the two
            let audible = view.audible(&owner, &t.revision);
            for (i, w) in t.words.iter().enumerate() {
                if w.end <= a || w.start >= b {
                    continue;
                }
                let heard = audible.and_then(|h| h.get(i).copied().flatten());
                let (hs, he, judged) = match heard {
                    Some([s, e]) => (s.min(w.start), e, true),
                    None => (
                        w.start,
                        w.end,
                        (w.end - w.start).max(0) <= secs(CUT_WORD_MAX_S),
                    ),
                };
                if judged && hs + mw < a && a + mw < he {
                    cut_in = Some(w.text.clone());
                }
                if judged && hs + mw < b && b + mw < he {
                    cut_out = Some(w.text.clone());
                }
                if me.is_some() && plays_in(w.start, w.end, &spans) == me {
                    words.push(ScriptWord {
                        ordinal: i as u32,
                        text: w.text.clone(),
                        src_start: w.start,
                        src_end: w.end,
                        spoken_end: match heard {
                            Some([_, e]) => e,
                            None => w.end.min(w.start + secs(SPOKEN_MAX_S)),
                        },
                        at: to_cut(w.start.max(a)),
                    });
                }
            }
        }
        let gap_min = (GAP_S * SEC as f64) as i64;
        let scale = |d: i64| (d as i128 * sp.dur.0 as i128 / len as i128) as i64;
        let (lead, trail, gaps) = match (words.first(), words.last()) {
            (Some(f), Some(l)) => {
                let gaps = words
                    .windows(2)
                    .filter(|w| w[1].src_start - w[0].spoken_end >= gap_min)
                    .map(|w| Gap {
                        at: to_cut(w[0].spoken_end),
                        len: scale(w[1].src_start - w[0].spoken_end),
                    })
                    .collect();
                (
                    scale((f.src_start - a).max(0)),
                    scale((b - l.spoken_end.min(b)).max(0)),
                    gaps,
                )
            }
            _ => (sp.dur.0, 0, vec![]),
        };
        out.parts.push(PartScript {
            index: out.parts.len(),
            part: p.id,
            media: p.media,
            owner,
            start: sp.start.0,
            dur: sp.dur.0,
            src_in: a,
            src_out: b,
            transcript: t.is_some(),
            no_audio: match p.media {
                MediaRef::Source(sid) => doc
                    .get(sid)
                    .is_some_and(|s| s.probe.audio_tracks.is_empty()),
                MediaRef::Group(_) => false,
            },
            words,
            lead,
            trail,
            gaps,
            starts_mid_sentence: false,
            ends_mid_sentence: false,
            cuts_word_in: cut_in,
            cuts_word_out: cut_out,
        });
    }
    sentence_edges(doc, view, &mut out.parts);
    out.retakes = retakes(&out.parts);
    out.revisions = revs.into_iter().collect();
    out
}

/// Marks parts that start or end mid-sentence. A part starts mid-sentence when the
/// source word before its first word goes on the same sentence (no full stop, under
/// a second before it) and what the cut plays just before does not lead into it: a
/// sentence end, another media, or an earlier point of the same media. Likewise at
/// its end. A strike inside a sentence (the cut jumps forward in the same media from
/// one unfinished phrase to the next) is not flagged.
fn sentence_edges(doc: &Doc, view: &AnalysisView, parts: &mut [PartScript]) {
    let near = (SAME_SENTENCE_S * SEC as f64) as i64;
    // the transcript word before / after a played word, if the same sentence
    let prev_same = |owner: &str, ord: u32| -> bool {
        let Some(t) = view.active(doc, owner) else {
            return false;
        };
        let (Some(w), Some(p)) = (
            t.words.get(ord as usize),
            (ord as usize).checked_sub(1).and_then(|i| t.words.get(i)),
        ) else {
            return false;
        };
        !ends_sentence(&p.text) && w.start - p.end < near
    };
    let next_same = |owner: &str, ord: u32| -> bool {
        let Some(t) = view.active(doc, owner) else {
            return false;
        };
        let (Some(w), Some(n)) = (t.words.get(ord as usize), t.words.get(ord as usize + 1)) else {
            return false;
        };
        !ends_sentence(&w.text) && n.start - w.end < near
    };
    // the last / first played word of the parts around `i` (skipping silent ones)
    let before = |parts: &[PartScript], i: usize| {
        parts[..i].iter().rev().find_map(|p| {
            p.words
                .last()
                .map(|w| (p.owner.clone(), w.ordinal, w.text.clone()))
        })
    };
    let after = |parts: &[PartScript], i: usize| {
        parts[i + 1..]
            .iter()
            .find_map(|p| p.words.first().map(|w| (p.owner.clone(), w.ordinal)))
    };
    // two words of one recording more than the retake window apart
    let far = |owner: &str, a: u32, b: u32| -> bool {
        view.active(doc, owner).is_some_and(|t| {
            match (t.words.get(a as usize), t.words.get(b as usize)) {
                (Some(x), Some(y)) => (y.start - x.end).abs() > RETAKE_WINDOW_S * SEC,
                _ => true,
            }
        })
    };
    let mut flags = vec![(false, false); parts.len()];
    for i in 0..parts.len() {
        let p = &parts[i];
        let (Some(f), Some(l)) = (p.words.first(), p.words.last()) else {
            continue;
        };
        if prev_same(&p.owner, f.ordinal) {
            flags[i].0 = match before(parts, i) {
                Some((o, ord, text)) => {
                    o != p.owner
                        || ord >= f.ordinal
                        || ends_sentence(&text)
                        || far(&p.owner, ord, f.ordinal)
                }
                None => true,
            };
        }
        if next_same(&p.owner, l.ordinal) {
            flags[i].1 = match after(parts, i) {
                // the cut goes on later in the same recording: a strike (a retake, a
                // pause), not a dangling sentence, unless it jumps far
                Some((o, ord)) => o != p.owner || ord <= l.ordinal || far(&o, l.ordinal, ord),
                None => true,
            };
        }
    }
    for (p, (s, e)) in parts.iter_mut().zip(flags) {
        p.starts_mid_sentence = s;
        p.ends_mid_sentence = e;
    }
}

/// Longest common subsequence length.
fn lcs(a: &[String], b: &[String]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut prev = 0;
        for (j, y) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if x == y {
                prev + 1
            } else {
                row[j + 1].max(row[j])
            };
            prev = cur;
        }
    }
    row[b.len()]
}

/// Words too common to say two sentences are about the same thing.
const STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "so", "to", "of", "in", "on", "at", "for", "with", "i",
    "im", "ill", "ive", "id", "you", "we", "it", "its", "is", "are", "was", "be", "this", "that",
    "my", "your", "me", "can", "see", "just", "gonna", "let", "lets", "okay", "alright", "yeah",
    "um", "uh", "like", "have", "has", "do", "what", "there", "here", "now", "then",
];

/// Words a phrase does not end on: an attempt broken off on one was abandoned.
const DANGLING: &[&str] = &[
    "gonna", "going", "to", "the", "a", "an", "and", "of", "just", "my", "your", "is", "was",
];

/// Retakes over the cut's words in order (SPEC §5.5). For each word, an earlier attempt
/// within 20 s (and 40 words) that opens the same way and is said again:
/// - **repeated**: word for word ("I I", "you'll see all you'll see all"); a single
///   letter twice is a spelling ("B B"), not a stutter;
/// - **similar**: ≥ 0.7 of its words said again in order, over 4+ words; a whole
///   sentence needs ≥ 0.8, or the same three opening words and half its content words
///   (so "I can see Codex here. I can see Pi there." stays);
/// - **restarted**: an unfinished attempt of ≤ 12 words broken off and begun again —
///   its second word comes again, half its words after a pause, or it is ≤ 4 words
///   broken off on a word no phrase ends on ("gonna", "the", "to" …). "I'm", "I'll",
///   "I've" open like "I".
///
/// Similar and restarted attempts begin where an attempt can (the cut's start, a
/// sentence end, a pause, a part edge or a struck word) unless they share their
/// opening words. Only kept words are compared, so each sign-off of three gives way
/// to the next and the last stays. The longest valid earlier attempt wins, and the
/// restart may sit up to three words later where the attempts match better.
pub fn retakes(parts: &[PartScript]) -> Vec<Retake> {
    struct W<'a> {
        part: usize,
        w: &'a ScriptWord,
        tok: String,
    }
    let ws: Vec<W> = parts
        .iter()
        .enumerate()
        .flat_map(|(pi, p)| {
            p.words.iter().map(move |w| W {
                part: pi,
                w,
                tok: token(&w.text),
            })
        })
        .filter(|w| !w.tok.is_empty())
        .collect();
    let n = ws.len();
    let mut struck = vec![false; n];
    let mut out = Vec::new();
    let window = RETAKE_WINDOW_S * SEC;
    let pause_min = SEC / 2;
    let content = |ix: &[usize]| -> BTreeSet<&str> {
        ix.iter()
            .map(|&k| ws[k].tok.as_str())
            .filter(|t| !STOP.contains(t))
            .collect()
    };
    // attempt `i..j` (its kept words) against what is said from `j`: the kind, and the
    // share of its words said again
    let judge = |i: usize, j: usize, struck: &[bool]| -> Option<(RetakeKind, f64)> {
        let earlier: Vec<usize> = (i..j).filter(|&k| !struck[k]).collect();
        let l = earlier.len();
        if l == 0 {
            return None;
        }
        let later: Vec<usize> = (j..n).filter(|&k| !struck[k]).take(l).collect();
        let et: Vec<String> = earlier.iter().map(|&k| ws[k].tok.clone()).collect();
        let lt: Vec<String> = later.iter().map(|&k| ws[k].tok.clone()).collect();
        let opening = et.iter().zip(&lt).take_while(|(a, b)| a == b).count();
        // the same first word, or another little opener before the same two ("We can
        // see / I can see")
        let first = |t: &str| -> String {
            match t {
                "im" | "ill" | "ive" | "id" => "i".into(),
                _ => t.to_owned(),
            }
        };
        let same_start = first(&et[0]) == first(&lt[0])
            || (l >= 3
                && lt.len() >= 3
                && et[1..3] == lt[1..3]
                && STOP.contains(&et[0].as_str())
                && STOP.contains(&lt[0].as_str()));
        if !same_start {
            return None;
        }
        if et == lt {
            let letter =
                l == 1 && et[0].chars().count() == 1 && !matches!(et[0].as_str(), "i" | "a");
            return (!letter).then_some((RetakeKind::Repeated, 1.0));
        }
        let prev = (0..i).rev().find(|&k| !struck[k]);
        let begins = match prev {
            None => true,
            Some(p) => {
                ends_sentence(&ws[p].w.text)
                    || p + 1 < i
                    || ws[i].part != ws[p].part
                    || ws[i].w.src_start - ws[p].w.src_end >= pause_min
            }
        } || opening >= 3
            || (opening >= 2 && l <= 4);
        if !begins {
            return None;
        }
        let ratio = lcs(&et, &lt) as f64 / l as f64;
        let whole = ends_sentence(&ws[*earlier.last().unwrap_or(&i)].w.text);
        if l >= 4 {
            if ratio >= if whole { 0.8 } else { 0.7 } {
                return Some((RetakeKind::Similar, ratio));
            }
            if whole && opening >= 3 {
                // the later attempt's own sentence, not only as many words
                let mut sent: Vec<usize> = Vec::new();
                for k in (j..n).filter(|&k| !struck[k]) {
                    sent.push(k);
                    if ends_sentence(&ws[k].w.text) || sent.len() >= 2 * l {
                        break;
                    }
                }
                let (a, b) = (content(&earlier), content(&sent));
                let shared = a.intersection(&b).count();
                if !a.is_empty() && shared * 2 >= a.len().max(b.len()) {
                    return Some((RetakeKind::Similar, ratio));
                }
            }
        }
        if !whole && (2..=ABANDONED_MAX_WORDS).contains(&l) && lt.len() >= 2 {
            let shared = et.iter().filter(|t| lt.contains(t)).count();
            let last = earlier[l - 1];
            let pause =
                ws[j].w.src_start - ws[last].w.src_end >= pause_min || ws[j].part != ws[last].part;
            // broken off on a word that cannot end a phrase ("And I'm just gonna | and
            // I actually …")
            let dangling = l <= 4 && DANGLING.contains(&et[l - 1].as_str());
            if et[1] == lt[1] || (pause && shared * 2 >= l) || dangling {
                return Some((RetakeKind::Restarted, ratio));
            }
        }
        None
    };
    for j0 in 1..n {
        if struck[j0] {
            continue;
        }
        let mut best: Option<(usize, RetakeKind, f64)> = None;
        let mut back = 0;
        for i in (0..j0).rev() {
            if ws[j0].w.at - ws[i].w.at > window {
                break;
            }
            if struck[i] {
                continue;
            }
            back += 1;
            if back > RETAKE_MAX_WORDS {
                break;
            }
            if let Some((k, r)) = judge(i, j0, &struck) {
                // the longest valid attempt (scanning back, the last found)
                best = Some((i, k, r));
            }
        }
        let Some((i, mut kind, mut ratio)) = best else {
            continue;
        };
        // the restart may begin a word or three later ("installed and so | and it's
        // picked up"): take the point the attempts match best
        let mut j = j0;
        for j2 in j0 + 1..(j0 + 4).min(n) {
            if struck[j2] || ws[j2].w.at - ws[i].w.at > window {
                break;
            }
            if let Some((k, r)) = judge(i, j2, &struck)
                && r > ratio + 1e-9
            {
                (j, kind, ratio) = (j2, k, r);
            }
        }
        let _ = ratio;
        // the earlier attempt's kept words, and their source spans part by part: from
        // a word to where the next kept word of that part starts (the restart's breath)
        let gone: Vec<usize> = (i..j).filter(|&k| !struck[k]).collect();
        let next_kept = |k: usize| (k + 1..n).find(|&x| !struck[x] && !gone.contains(&x));
        let mut spans: Vec<(MediaRef, i64, i64)> = Vec::new();
        let mut in_parts: Vec<usize> = Vec::new();
        let mut g = 0;
        while g < gone.len() {
            let pi = ws[gone[g]].part;
            let mut e = g;
            while e + 1 < gone.len() && ws[gone[e + 1]].part == pi && gone[e + 1] == gone[e] + 1 {
                e += 1;
            }
            let p = &parts[pi];
            let from = ws[gone[g]].w.src_start.max(p.src_in);
            let to = match next_kept(gone[e]) {
                Some(nx) if ws[nx].part == pi && nx == gone[e] + 1 => ws[nx].w.src_start,
                _ => ws[gone[e]].w.src_end,
            }
            .min(p.src_out);
            if to > from {
                spans.push((p.media, from, to));
            }
            if !in_parts.contains(&pi) {
                in_parts.push(pi);
            }
            g = e + 1;
        }
        for &k in &gone {
            struck[k] = true;
        }
        let said = |ix: &[usize]| {
            ix.iter()
                .map(|&k| ws[k].w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let after: Vec<usize> = (j..n)
            .filter(|&k| !struck[k])
            .take(gone.len().max(4))
            .collect();
        out.push(Retake {
            kind,
            earlier: said(&gone),
            later: said(&after),
            at: ws[i].w.at,
            spans,
            parts: in_parts,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(text: &str, gap_after: &[usize]) -> PartScript {
        let mut t = 0i64;
        let words = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let s = t;
                t += SEC / 3;
                let e = t;
                if gap_after.contains(&i) {
                    t += SEC;
                }
                ScriptWord {
                    ordinal: i as u32,
                    text: w.into(),
                    src_start: s,
                    src_end: e,
                    spoken_end: e,
                    at: s,
                }
            })
            .collect();
        PartScript {
            index: 0,
            part: Id::new(gmclip_model::Ulid::from_parts(0, 0)),
            media: MediaRef::Source(Id::new(gmclip_model::Ulid::from_parts(0, 0))),
            owner: "s".into(),
            start: 0,
            dur: t,
            src_in: 0,
            src_out: t,
            transcript: true,
            no_audio: false,
            words,
            lead: 0,
            trail: 0,
            gaps: vec![],
            starts_mid_sentence: false,
            ends_mid_sentence: false,
            cuts_word_in: None,
            cuts_word_out: None,
        }
    }

    fn kept(p: &PartScript) -> String {
        let r = retakes(std::slice::from_ref(p));
        p.words
            .iter()
            .filter(|w| {
                !r.iter().any(|x| {
                    x.spans
                        .iter()
                        .any(|(_, a, b)| w.src_start >= *a && w.src_start < *b)
                })
            })
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn ajs_git_repositories_takes_collapse_to_the_last() {
        let p = part(
            "Continue to the next screen and you'll see all You'll see all the Git repositories. All the Git repositories on your computer. All the Git repositories on your computer that you've used in the last thirty days.",
            &[],
        );
        assert_eq!(
            kept(&p),
            "Continue to the next screen and You'll see All the Git repositories on your computer that you've used in the last thirty days."
        );
    }

    #[test]
    fn stutters_restarts_and_false_starts_go() {
        assert_eq!(
            kept(&part("I can see code I I can see Codex.", &[])),
            "I can see Codex."
        );
        // broken off after a pause and begun again with the same word
        assert_eq!(
            kept(&part(
                "Alright, here is my first time Alright, this is the first screen after you install BB.",
                &[5]
            )),
            "Alright, this is the first screen after you install BB."
        );
        assert_eq!(
            kept(&part(
                "And there's also a button to install cursor, which I and there's also a button for me to install cursor, which I didn't have installed.",
                &[]
            )),
            "and there's also a button for me to install cursor, which I didn't have installed."
        );
    }

    #[test]
    fn of_three_sign_offs_the_last_stays() {
        let p = part(
            "I'm excited to try the mobile app. And I'll let you guys know what I think as I use and I'll let and And I'll give you guys an update of my experience with BB. And I'll give you guys updates of my experience with BB on the next. And I'll give you and I'll give updates on my experience with BB as I use it in more videos.",
            &[19, 22],
        );
        assert_eq!(
            kept(&p),
            "I'm excited to try the mobile app. and I'll give updates on my experience with BB as I use it in more videos."
        );
    }

    #[test]
    fn phrases_broken_off_on_a_dangling_word_go() {
        assert_eq!(
            kept(&part(
                "And I'm just gonna and I actually have my Android connected here.",
                &[]
            )),
            "and I actually have my Android connected here."
        );
        assert_eq!(
            kept(&part(
                "I'm also gonna I also clicked Android to get the APK file.",
                &[]
            )),
            "I also clicked Android to get the APK file."
        );
        // a single letter twice is a name being spelt
        assert_eq!(
            kept(&part("I can start using B B. Okay.", &[])),
            "I can start using B B. Okay."
        );
    }

    #[test]
    fn sentences_that_only_begin_alike_stay() {
        let text = "I can see Codex here. I can see Pi there. We can go fast.";
        assert_eq!(kept(&part(text, &[])), text);
    }
}
