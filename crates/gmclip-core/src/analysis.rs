//! The analysis view (SPEC §3.2 `Cx` "read-only analysis view", §4.4 `analysis`): the
//! transcripts the store holds, decoded once and shared read-only by handlers (anchors,
//! sweeps, the ASR switch), lint and the agent surface.
//!
//! Transcripts are the `words` / `lines` analysis blobs keyed by `(owner, revision)`,
//! the owner being a source (or sync group) id, written by gmclip's ASR jobs (parakeet
//! revisions). Each is JSON (`gmclip.analysis.words/0`, `gmclip.analysis.lines/0`), times in flicks.

use std::collections::BTreeMap;
use std::sync::Arc;

use gmclip_analysis::blob::{Blob, Param};
use gmclip_analysis::edges;
use gmclip_analysis::energy::Energy;
use gmclip_analysis::span::Span;
use gmclip_model::anchor::WordRef;
use gmclip_model::entities::Project;
use gmclip_model::{Doc, Flicks};
use serde_json::Value;

pub use crate::transcript::{Line, Transcript, Word};

const SEC: i64 = 705_600_000;

/// Decodes a `words` body: `(revision, words)`.
pub fn decode_words(body: &[u8]) -> Option<(String, Vec<Word>)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let rev = v.get("revision")?.as_str()?.to_owned();
    let words: Vec<Word> = serde_json::from_value(v.get("words")?.clone()).ok()?;
    Some((rev, words))
}

/// Decodes a `lines` body.
pub fn decode_lines(body: &[u8]) -> Option<Vec<Line>> {
    let v: Value = serde_json::from_slice(body).ok()?;
    serde_json::from_value(v.get("lines")?.clone()).ok()
}

/// Lines from words (for ASR revisions, which carry no line file): a line ends at a
/// sentence mark, at a pause of ≥ 0.6 s, or after 14 words.
pub fn make_lines(words: &[Word]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for i in 0..words.len() {
        let w = &words[i];
        let ends_sentence = w.text.ends_with(['.', '?', '!']);
        let pause = words
            .get(i + 1)
            .is_some_and(|n| n.start - w.end >= SEC * 6 / 10);
        let last = i + 1 == words.len();
        if ends_sentence || pause || last || i + 1 - start >= 14 {
            let ws = &words[start..=i];
            lines.push(Line {
                start: ws.first().map_or(0, |x| x.start),
                end: ws.last().map_or(0, |x| x.end),
                text: ws
                    .iter()
                    .map(|x| x.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                words: [start as u32, i as u32 + 1],
            });
            start = i + 1;
        }
    }
    lines
}

/// Normalised token form for matching (lowercase letters and digits).
pub fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// An owner's voice levels (its `energy_10ms` blob) and its quiet grid's threshold (its
/// `silences_40ms` blob): what its words' edges are measured on (decision 0048).
#[derive(Debug)]
struct Voice {
    energy: Energy,
    quiet_db: f32,
}

/// Per word of one transcript, where it is heard (see [`AnalysisView::audible`]).
type Audible = Arc<Vec<Option<[i64; 2]>>>;

/// The transcripts the store holds, decoded.
#[derive(Debug, Default, Clone)]
pub struct AnalysisView {
    transcripts: BTreeMap<(String, String), Arc<Transcript>>,
    /// Words heard from cuts' own audio, by stretch signature (`planner::heard`).
    heard: BTreeMap<String, Arc<crate::planner::heard::HeardStretch>>,
    /// Each owner's voice levels, once its analysis is stored.
    voice: BTreeMap<String, Arc<Voice>>,
    /// Where each transcript's words are heard on its owner's voice, word by word.
    audible: BTreeMap<(String, String), Audible>,
}

impl AnalysisView {
    /// Adds (or replaces) a transcript for `owner`.
    pub fn insert(&mut self, owner: &str, t: Transcript) {
        self.transcripts
            .insert((owner.to_owned(), t.revision.clone()), Arc::new(t));
        self.measure(owner);
    }

    /// Adds an owner's voice levels from its stored `energy_10ms` and `silences_40ms`
    /// bodies and measures its transcripts' words on them; `false` when either is not one.
    pub fn insert_voice(&mut self, owner: &str, energy: &[u8], silences: &[u8]) -> bool {
        let Some(energy) = Blob::decode(energy)
            .ok()
            .and_then(|b| Energy::from_blob(&b).ok())
        else {
            return false;
        };
        let Some(Param::Float(q)) = Blob::decode(silences)
            .ok()
            .and_then(|b| b.params.get("threshold_db").cloned())
        else {
            return false;
        };
        self.voice.insert(
            owner.to_owned(),
            Arc::new(Voice {
                energy,
                quiet_db: q as f32,
            }),
        );
        self.measure(owner);
        true
    }

    /// Where the words of an owner's transcript `revision` are heard (decision 0048):
    /// per word, `[start, end]` in flicks measured on the voice (an 18 dB drop from the
    /// word's peak, held 60 ms), `[start, start]` when its ASR span holds no speech at all,
    /// `None` where nothing was measured. ASR times often run a word on into the silence
    /// after it, by up to seconds.
    pub fn audible(&self, owner: &str, revision: &str) -> Option<&[Option<[i64; 2]>]> {
        self.audible
            .get(&(owner.to_owned(), revision.to_owned()))
            .map(|v| v.as_slice())
    }

    /// Where a cut keeps (or, struck, takes out) the words `first..=last` of an owner's
    /// transcript `revision`, in flicks (decision 0049): from 40 ms before the first word's
    /// voice (a soft onset within 250 ms of it kept) to 120 ms after the last word's,
    /// never into the voice of the word before or after. Each side is `None` when its word
    /// was not measured or is the room: the caller keeps the ASR's time there.
    pub fn voice_span(
        &self,
        owner: &str,
        revision: &str,
        first: usize,
        last: usize,
    ) -> (Option<i64>, Option<i64>) {
        let (Some(v), Some(h)) = (self.voice.get(owner), self.audible(owner, revision)) else {
            return (None, None);
        };
        let e = &v.energy;
        let rate = i128::from(e.rate);
        let to_sample = |f: i64| {
            usize::try_from(i128::from(f.max(0)) * rate / i128::from(SEC)).unwrap_or(usize::MAX)
        };
        let to_flicks = |s: usize| i64::try_from(s as i128 * i128::from(SEC) / rate).unwrap_or(0);
        // a word heard, not the room
        let heard = |i: Option<usize>| {
            i.and_then(|i| h.get(i).copied().flatten())
                .filter(|[s, e]| e > s)
        };
        let start = heard(Some(first)).map(|[s, _]| {
            let k = to_flicks(edges::kept_start(e, to_sample(s), v.quiet_db));
            // the word before keeps its voice and, where there is room, its tail
            match heard(first.checked_sub(1)) {
                Some([_, pe]) => k.max((pe + i64::from(edges::TAIL_MS) * SEC / 1000).min(s)),
                None => k,
            }
        });
        let end = heard(Some(last)).map(|[_, x]| {
            let k = to_flicks(edges::kept_end(e, to_sample(x)));
            match heard(last.checked_add(1)) {
                Some([ns, _]) => {
                    k.min(to_flicks(edges::kept_start(e, to_sample(ns), v.quiet_db)).max(x))
                }
                None => k,
            }
        });
        (start, end)
    }

    /// Measures `owner`'s transcripts on its voice, when both are held.
    fn measure(&mut self, owner: &str) {
        let Some(v) = self.voice.get(owner).cloned() else {
            return;
        };
        let e = &v.energy;
        let rate = i128::from(e.rate);
        let to_sample = |f: i64| {
            usize::try_from(i128::from(f.max(0)) * rate / i128::from(SEC)).unwrap_or(usize::MAX)
        };
        let to_flicks = |s: usize| i64::try_from(s as i128 * i128::from(SEC) / rate).unwrap_or(0);
        let mine: Vec<((String, String), Arc<Transcript>)> = self
            .transcripts
            .iter()
            .filter(|((o, _), _)| o == owner)
            .map(|(k, t)| (k.clone(), Arc::clone(t)))
            .collect();
        for (key, t) in mine {
            let spans: Vec<Span> = t
                .words
                .iter()
                .map(|w| Span::new(to_sample(w.start), to_sample(w.end.max(w.start))))
                .collect();
            let heard = edges::audible_edges(e, &spans, Span::new(0, usize::MAX))
                .into_iter()
                .zip(&t.words)
                .map(|(x, w)| match x {
                    // its loudest moment is the room: the ASR timed it into a silence
                    Some((a, _)) if a.peak_db < v.quiet_db => Some([w.start, w.start]),
                    Some((a, b)) => Some([
                        to_flicks(a.sample.min(b.sample)),
                        to_flicks(b.sample.max(a.sample)),
                    ]),
                    None => None,
                })
                .collect();
            self.audible.insert(key, Arc::new(heard));
        }
    }

    /// Builds a transcript from stored bodies (lines made from words when absent).
    pub fn insert_bodies(&mut self, owner: &str, words: &[u8], lines: Option<&[u8]>) -> bool {
        let Some((revision, words)) = decode_words(words) else {
            return false;
        };
        let lines = lines
            .and_then(decode_lines)
            .unwrap_or_else(|| make_lines(&words));
        self.insert(
            owner,
            Transcript {
                revision,
                words,
                lines,
            },
        );
        true
    }

    /// Adds a stretch heard from a cut's own audio (a `heard_words` body); `false` when
    /// the body is not one.
    pub fn insert_heard(&mut self, body: &[u8]) -> bool {
        match crate::planner::heard::HeardStretch::decode(body) {
            Some(h) => {
                self.heard.insert(h.sig.clone(), Arc::new(h));
                true
            }
            None => false,
        }
    }

    /// The words heard over a stretch, by its signature.
    pub fn heard(&self, sig: &str) -> Option<&Arc<crate::planner::heard::HeardStretch>> {
        self.heard.get(sig)
    }

    /// How many heard stretches are held (a plan cache keys on it: new words, new plan).
    pub fn heard_len(&self) -> usize {
        self.heard.len()
    }

    pub fn transcript(&self, owner: &str, revision: &str) -> Option<&Arc<Transcript>> {
        self.transcripts
            .get(&(owner.to_owned(), revision.to_owned()))
    }

    /// Every `(owner, revision)` held.
    pub fn keys(&self) -> Vec<(String, String)> {
        self.transcripts.keys().cloned().collect()
    }

    /// The active transcript of an owner (`Project.active_asr`), else its only one.
    pub fn active(&self, doc: &Doc, owner: &str) -> Option<&Arc<Transcript>> {
        let rev = doc
            .iter::<Project>()
            .next()
            .and_then(|p| p.active_asr.get(owner).cloned());
        match rev {
            Some(r) => self.transcript(owner, &r),
            None => {
                let mut it = self.transcripts.iter().filter(|((o, _), _)| o == owner);
                let first = it.next().map(|(_, t)| t);
                if it.next().is_some() { None } else { first }
            }
        }
    }

    /// The word a reference names, with its owner: `(owner, word)`. Every video of a
    /// project is transcribed by the same ASR build, so a `WordId` alone can name a word
    /// in each of them: of those, the one heard at the reference's own time.
    pub fn word(&self, w: &WordRef) -> Option<(&str, &Word)> {
        let id = &w.word_id;
        self.transcripts
            .iter()
            .filter(|((_, r), _)| *r == id.asr_revision)
            .filter_map(|((o, _), t)| t.words.get(id.ordinal as usize).map(|x| (o.as_str(), x)))
            .min_by_key(|(o, x)| ((x.start - w.t_src_start.0).abs(), *o))
    }

    /// Is there any transcript of revision `rev`?
    pub fn has_revision(&self, rev: &str) -> bool {
        self.transcripts.keys().any(|(_, r)| r == rev)
    }

    /// The nearest word edge to `t` in an owner's active transcript within `tol`
    /// (R-ANC-1 `nearest_word`): `(edge time, ordinal, is_start)`.
    pub fn nearest_edge(
        &self,
        doc: &Doc,
        owner: &str,
        t: Flicks,
        tol: Flicks,
    ) -> Option<(Flicks, u32, bool)> {
        let tr = self.active(doc, owner)?;
        let i = tr.words.partition_point(|w| w.end < t.0);
        let mut best: Option<(i64, u32, bool)> = None;
        for k in i.saturating_sub(1)..(i + 2).min(tr.words.len()) {
            let w = &tr.words[k];
            for (edge, is_start) in [(w.start, true), (w.end, false)] {
                let d = (edge - t.0).abs();
                if d <= tol.0 && best.is_none_or(|b| d < (b.0 - t.0).abs()) {
                    best = Some((edge, k as u32, is_start));
                }
            }
        }
        best.map(|(e, k, s)| (Flicks(e), k, s))
    }
}

/// R-MODEL-4 re-anchoring: maps the words of `from` onto `to` by monotone alignment
/// (Needleman–Wunsch on normalised tokens, |Δt| ≤ 0.5 s, similarity ≥ 0.8), keeping
/// only unique one-to-one matches. Returns `old ordinal → new ordinal`.
pub fn realign(from: &Transcript, to: &Transcript) -> BTreeMap<u32, u32> {
    let a: Vec<String> = from.words.iter().map(|w| norm(&w.text)).collect();
    let b: Vec<String> = to.words.iter().map(|w| norm(&w.text)).collect();
    let mut out = BTreeMap::new();
    if a.is_empty() || b.is_empty() {
        return out;
    }
    // banded DP over time windows keeps this linear in practice: align chunks of words
    // whose times overlap, so a 3-hour transcript never builds an n×m table
    let mut j0 = 0usize;
    let mut i = 0usize;
    while i < a.len() {
        let i1 = (i + 400).min(a.len());
        let t0 = from.words[i].start - SEC / 2;
        let t1 = from.words[i1 - 1].end + SEC / 2;
        while j0 < b.len() && to.words[j0].end < t0 {
            j0 += 1;
        }
        let mut j1 = j0;
        while j1 < b.len() && to.words[j1].start <= t1 {
            j1 += 1;
        }
        for (x, y) in align(&a[i..i1], &b[j0..j1], &from.words[i..i1], &to.words[j0..j1]) {
            out.insert((i + x) as u32, (j0 + y) as u32);
        }
        i = i1;
    }
    out
}

fn similarity(x: &str, y: &str) -> f64 {
    if x == y {
        return 1.0;
    }
    let (n, m) = (x.chars().count(), y.chars().count());
    if n == 0 || m == 0 {
        return 0.0;
    }
    let xs: Vec<char> = x.chars().collect();
    let ys: Vec<char> = y.chars().collect();
    let mut prev: Vec<usize> = (0..=m).collect();
    for i in 1..=n {
        let mut cur = vec![i; m + 1];
        for j in 1..=m {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(xs[i - 1] != ys[j - 1]));
        }
        prev = cur;
    }
    1.0 - prev[m] as f64 / n.max(m) as f64
}

/// Needleman–Wunsch: match = similarity ≥ 0.8 and |Δt| ≤ 0.5 s; gaps cost 1.
fn align(a: &[String], b: &[String], wa: &[Word], wb: &[Word]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let ok = |i: usize, j: usize| {
        similarity(&a[i], &b[j]) >= 0.8 && (wa[i].start - wb[j].start).abs() <= SEC / 2
    };
    // score: maximise matches
    let mut s = vec![vec![0i32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            let diag = s[i - 1][j - 1] + i32::from(ok(i - 1, j - 1));
            s[i][j] = diag.max(s[i - 1][j]).max(s[i][j - 1]);
        }
    }
    let (mut i, mut j) = (n, m);
    let mut pairs = Vec::new();
    while i > 0 && j > 0 {
        if ok(i - 1, j - 1) && s[i][j] == s[i - 1][j - 1] + 1 {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if s[i - 1][j] >= s[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, s: f64, e: f64) -> Word {
        Word {
            text: t.into(),
            start: (s * SEC as f64) as i64,
            end: (e * SEC as f64) as i64,
            confidence: None,
        }
    }

    /// Two videos transcribed by the same ASR build share a revision, so word 1 names a
    /// word in each: the reference's own time says which (the second video's topic must
    /// not take the first video's times).
    #[test]
    fn a_word_id_shared_by_two_videos_resolves_to_the_one_at_its_time() {
        let t = |a: f64, b: f64| Transcript {
            revision: "parakeet".into(),
            words: vec![w("so", a, a + 0.2), w("yeah", b, b + 0.2)],
            lines: vec![],
        };
        let mut v = AnalysisView::default();
        v.insert("video-a", t(1.0, 2.0));
        v.insert("video-b", t(5.0, 9.0));
        let r = |at: f64| WordRef {
            word_id: gmclip_model::anchor::WordId {
                asr_revision: "parakeet".into(),
                ordinal: 1,
            },
            t_src_start: Flicks((at * SEC as f64) as i64),
            t_src_end: Flicks(((at + 0.2) * SEC as f64) as i64),
            normalised_text: "yeah".into(),
            prev2: vec![],
            next2: vec![],
        };
        assert_eq!(
            v.word(&r(9.0)).map(|(o, x)| (o, x.start)),
            Some(("video-b", 9 * SEC))
        );
        assert_eq!(
            v.word(&r(2.0)).map(|(o, x)| (o, x.start)),
            Some(("video-a", 2 * SEC))
        );
    }

    #[test]
    fn lines_break_at_sentences_pauses_and_length() {
        let ws = vec![
            w("hello", 0.0, 0.3),
            w("there.", 0.35, 0.6),
            w("next", 0.7, 0.9),
            w("one", 2.0, 2.2),
        ];
        let l = make_lines(&ws);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].words, [0, 2]);
        assert_eq!(l[1].text, "next");
    }

    #[test]
    fn realign_maps_unique_matches_monotonically() {
        let from = Transcript {
            revision: "a".into(),
            words: vec![
                w("my", 1.0, 1.1),
                w("pc", 1.2, 1.4),
                w("made", 1.5, 1.7),
                w("this", 1.8, 2.0),
            ],
            lines: vec![],
        };
        let to = Transcript {
            revision: "b".into(),
            words: vec![
                w("My", 1.02, 1.1),
                w("PC,", 1.21, 1.4),
                w("maid", 1.52, 1.7),
                w("this", 1.79, 2.0),
                w("extra", 5.0, 5.2),
            ],
            lines: vec![],
        };
        let m = realign(&from, &to);
        assert_eq!(m.get(&0), Some(&0));
        assert_eq!(m.get(&1), Some(&1));
        assert_eq!(m.get(&2), None, "made ≠ maid (similarity 0.75)");
        assert_eq!(m.get(&3), Some(&3));
    }
}

/// Chunked ASR, shared by the app and the holder so a transcription handed from one to
/// the other mid-way (R-ARCH-7) resumes at a window edge and ends with the transcript
/// an uninterrupted run makes.
///
/// Windows of [`chunked::CHUNK_S`] (the first [`chunked::FIRST_S`], so words show within
/// seconds), each heard with [`chunked::PAD_S`] of context on both sides; a window keeps
/// the words that *start* inside it, so a word across a window edge is kept once, from
/// the window that heard all of it. The revision names the chunking (`<model rev>+chunked`).
pub mod chunked {
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde::{Deserialize, Serialize};

    use super::{Transcript, Word, make_lines};
    use crate::host::{AnalysisHost, AudioSource, HostError};
    use gmclip_model::Flicks;

    pub const FIRST_S: i64 = 20;
    pub const CHUNK_S: i64 = 90;
    pub const PAD_S: i64 = 3;
    const SEC: i64 = super::SEC;

    /// Where a transcription stands: the windows heard so far.
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    pub struct AsrPartial {
        pub revision: String,
        /// Source flicks heard (the next window starts here).
        pub done: i64,
        pub words: Vec<Word>,
    }

    impl AsrPartial {
        pub fn transcript(self) -> Transcript {
            let lines = make_lines(&self.words);
            Transcript {
                revision: self.revision,
                words: self.words,
                lines,
            }
        }
    }

    // -----------------------------------------------------------------------------
    // Finished windows, stored (A2.02): a transcription resumes from its last finished
    // window after a cancel, a quit or a kill -9
    // -----------------------------------------------------------------------------

    /// The analysis kind each finished window is stored under (immutable blobs, one per
    /// window; the analysis view ignores them).
    pub const WINDOW_KIND: &str = "asr_window";

    /// What names a run's windows: the heard bytes and track, and a generation (a run
    /// that starts over — `force` after a finished one — is the next generation).
    pub fn window_prefix(audio: &AudioSource) -> String {
        let track = audio.track.map_or("-".to_owned(), |t| t.to_string());
        format!("{}/t{track}", audio.fingerprint)
    }

    fn window_rev(prefix: &str, generation: u32, start: i64) -> String {
        format!("{prefix}/g{generation}/{start:016}")
    }

    /// One stored window.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct StoredWindow {
        revision: String,
        start: i64,
        end: i64,
        words: Vec<Word>,
    }

    /// The newest generation of stored windows under `prefix` and the transcription they
    /// add up to (its contiguous windows from the start). `get(revision)` reads a blob.
    pub fn stored(
        keys: &[(String, String, String)],
        owner: &str,
        prefix: &str,
        get: impl Fn(&str) -> Option<Vec<u8>>,
    ) -> Option<(u32, AsrPartial)> {
        let mine: Vec<(u32, &str)> = keys
            .iter()
            .filter(|(o, k, _)| o == owner && k == WINDOW_KIND)
            .filter_map(|(_, _, r)| {
                let rest = r.strip_prefix(prefix)?.strip_prefix("/g")?;
                let (g, _) = rest.split_once('/')?;
                Some((g.parse().ok()?, r.as_str()))
            })
            .collect();
        let generation = mine.iter().map(|x| x.0).max()?;
        let mut windows: Vec<StoredWindow> = mine
            .iter()
            .filter(|x| x.0 == generation)
            .filter_map(|(_, r)| serde_json::from_slice(&get(r)?).ok())
            .collect();
        windows.sort_by_key(|w| w.start);
        let mut p = AsrPartial::default();
        for w in windows {
            if w.start != p.done || w.end <= w.start {
                break; // a gap: resume from the last contiguous window
            }
            if p.revision.is_empty() {
                p.revision = w.revision;
            }
            p.words.extend(w.words);
            p.done = w.end;
        }
        Some((generation, p))
    }

    /// Where [`run_stored`] began: the generation and the source flicks it resumed at.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Resumed {
        pub generation: u32,
        pub at: i64,
    }

    /// [`run`], resuming from the windows a run of the same audio stored (unless it
    /// finished and `force` asks for a new hearing), and storing each finished window
    /// with `put(revision, body)` before `heard` sees it. A source of unknown length is
    /// heard whole and stores nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn run_stored(
        host: &dyn AnalysisHost,
        audio: &AudioSource,
        total: i64,
        keys: &[(String, String, String)],
        owner: &str,
        get: impl Fn(&str) -> Option<Vec<u8>>,
        mut put: impl FnMut(&str, Vec<u8>),
        force: bool,
        cancel: &AtomicBool,
        mut heard: impl FnMut(&AsrPartial),
    ) -> Result<(AsrPartial, Resumed), HostError> {
        let prefix = window_prefix(audio);
        let (generation, from) = match stored(keys, owner, &prefix, get) {
            Some((g, p)) if force && total > 0 && p.done >= total => (g + 1, AsrPartial::default()),
            Some((g, p)) => (g, p),
            None => (1, AsrPartial::default()),
        };
        let resumed = Resumed {
            generation,
            at: from.done,
        };
        let mut at = from.done;
        let p = run(host, audio, total, from, cancel, |p| {
            if total > 0 && p.done > at {
                let w = StoredWindow {
                    revision: p.revision.clone(),
                    start: at,
                    end: p.done,
                    words: p.words.iter().filter(|w| w.start >= at).cloned().collect(),
                };
                if let Ok(body) = serde_json::to_vec(&w) {
                    put(&window_rev(&prefix, generation, at), body);
                }
                at = p.done;
            }
            heard(p);
        })?;
        Ok((p, resumed))
    }

    /// The window after `done`: `(end, heard_from, heard_to)`.
    pub fn window(done: i64, total: i64) -> (i64, i64, i64) {
        let len = if done == 0 { FIRST_S } else { CHUNK_S } * SEC;
        let end = (done + len).min(total);
        (
            (end),
            (done - PAD_S * SEC).max(0),
            (end + PAD_S * SEC).min(total),
        )
    }

    /// Hears the source from `from.done` to its end, calling `heard` after each window.
    /// A source of unknown length (no probe) is heard whole, under the model's revision.
    pub fn run(
        host: &dyn AnalysisHost,
        audio: &AudioSource,
        total: i64,
        from: AsrPartial,
        cancel: &AtomicBool,
        mut heard: impl FnMut(&AsrPartial),
    ) -> Result<AsrPartial, HostError> {
        let mut p = from;
        if total <= 0 {
            let t = host.transcribe(audio, None, cancel)?;
            p.revision = t.revision;
            p.words = t
                .words
                .iter()
                .map(|w| Word {
                    text: w.text.clone(),
                    start: w.start.0,
                    end: w.end.0,
                    confidence: w.confidence.map(f64::from),
                })
                .collect();
            heard(&p);
            return Ok(p);
        }
        while p.done < total {
            if cancel.load(Ordering::Relaxed) {
                return Err(HostError::Failed("cancelled".into()));
            }
            let (end, a, b) = window(p.done, total);
            let t = host.transcribe(audio, Some((Flicks(a), Flicks(b))), cancel)?;
            if p.revision.is_empty() {
                p.revision = format!("{}+chunked", t.revision.trim_end_matches("/span"));
            }
            let last = end >= total;
            let at = p.done;
            p.words.extend(
                t.words
                    .iter()
                    .filter(|w| w.start.0 >= at && (w.start.0 < end || last))
                    .map(|w| Word {
                        text: w.text.clone(),
                        start: w.start.0,
                        end: w.end.0,
                        confidence: w.confidence.map(f64::from),
                    }),
            );
            p.words.sort_by_key(|w| w.start);
            p.done = end;
            heard(&p);
        }
        if p.revision.is_empty() {
            return Err(HostError::Unsupported(
                "the video has no audio to transcribe",
            ));
        }
        Ok(p)
    }
}
