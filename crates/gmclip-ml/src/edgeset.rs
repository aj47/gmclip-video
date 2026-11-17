//! R-ML-2's hand-marked word edges: the file format `tools/mark_edges.py` writes, the
//! refinement under test, and the scorer (median |error| ≤ 40 ms, p95 ≤ 120 ms).
//!
//! An item is one edge of one stream. Its `kind` says what AJ marks:
//! - `abutting`: two words with < 0.3 s between them; the mark is **the latest point that
//!   keeps w1 whole and nothing of w2** (SPEC R-ML-2). Predicted by the arbiter, falling
//!   back to `energy_min` when it cannot resolve the edge (counted).
//! - `end`: a word followed by a pause; the mark is where its sound ends. Predicted by
//!   `audible_end`.
//! - `start`: a word after a pause; the mark is where its sound starts. Predicted by
//!   `audible_start`.
//!
//! The marks are never pre-filled by the arbiter (that would measure it against itself):
//! the page shows the words' text only, not any ASR time.
//!
//! All times in the file are seconds on the stream's own clock (the proxy's timeline).

use serde::{Deserialize, Serialize};

use gmclip_analysis::edges::{EdgeMethod, audible_end, audible_start, energy_min, peak_frame};
use gmclip_analysis::energy::Energy;
use gmclip_analysis::fillers::normalise;
use gmclip_analysis::pcm::VOICE_RATE;
use gmclip_analysis::span::Span;
use gmclip_model::Flicks;

use crate::arbiter::{self, ArbiterParams, PairWord};
use crate::tdt::TdtAligner;
use crate::wer::quantile;
use crate::{Asr, AsrWord, CancelToken, MlError};

pub const FORMAT: &str = "gmclip.edge-marks.v1";
/// R-ML-2's thresholds.
pub const MEDIAN_MAX_MS: f64 = 40.0;
pub const P95_MAX_MS: f64 = 120.0;
pub const MIN_MARKED: usize = 300;
pub const MIN_STREAMS: usize = 5;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Abutting,
    End,
    Start,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkWord {
    pub text: String,
    /// The selection transcript's times (the reference transcript's), used to find the word again;
    /// never shown to the marker.
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkItem {
    pub id: String,
    pub kind: EdgeKind,
    pub w1: MarkWord,
    pub w2: MarkWord,
    /// The audio clip the page plays, stream seconds.
    pub clip_start: f64,
    pub clip_end: f64,
    /// AJ's edge, stream seconds; `None` until marked.
    #[serde(default)]
    pub mark: Option<f64>,
    /// Marked as unusable (crosstalk, music, wrong words).
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkFile {
    pub format: String,
    pub stream: String,
    pub audio_path: String,
    pub audio_sha256: String,
    pub items: Vec<MarkItem>,
}

impl MarkFile {
    pub fn parse(bytes: &[u8]) -> Result<Self, MlError> {
        let f: MarkFile = serde_json::from_slice(bytes)
            .map_err(|_| MlError::Invalid("not an edge-marks file"))?;
        if f.format != FORMAT {
            return Err(MlError::Invalid("edge-marks format"));
        }
        Ok(f)
    }
}

/// How a prediction was made.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Audible,
    AudibleFallback,
    Arbiter,
    /// gmclip's ASR put a gap ≥ 0.3 s between the words: `energy_min`, as production
    /// routes it (R-ANC-2 sends only abutting pairs to the arbiter).
    EnergyMin,
    EnergyMinFallback,
    WordNotFound,
    Failed,
}

/// One predicted edge.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Prediction {
    pub id: String,
    pub kind: EdgeKind,
    pub method: Method,
    /// Stream seconds; `None` when nothing was predicted (counted as a miss).
    pub at: Option<f64>,
    /// What the refinement saw (words heard, arbiter checks).
    pub detail: String,
}

/// gmclip's word matching the marked one: the same word (normalised; short words
/// exactly) nearest in start time, within 1 s.
fn find(words: &[AsrWord], w: &MarkWord) -> Option<usize> {
    let secs = |t: Flicks| t.0 as f64 / 705_600_000.0;
    words
        .iter()
        .enumerate()
        .filter(|(_, x)| arbiter::same_word(&x.text, &w.text))
        .map(|(i, x)| (i, (secs(x.start) - w.start).abs()))
        .filter(|&(_, d)| d < 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

fn is_filler(w: &str) -> bool {
    gmclip_analysis::fillers::FILLERS_EN.contains(&normalise(w).as_str())
}

/// Predicts one item's edge from `pcm`, the 16 kHz voice track whose first sample is at
/// stream time `pcm_start_s` (the item's clip with context either side), using gmclip's
/// own ASR of that audio: never the marks, and the selection transcript's times only to
/// pick out the same two words. The two words must be adjacent in gmclip's ASR (fillers
/// between them allowed). Routing is production's: `end` → `audible_end(w1)`, `start` →
/// `audible_start(w2)`, `abutting` → the forced-alignment arbiter when gmclip's gap is
/// < 0.3 s (`energy_min` when it is not, or when the arbiter cannot resolve the edge),
/// then moved to the latest point before w2's measured onset (R-ML-2: "the latest point that
/// keeps the word whole and nothing of the next").
pub fn predict(
    asr: &dyn Asr,
    tdt: &TdtAligner,
    pcm: &[f32],
    pcm_start_s: f64,
    item: &MarkItem,
    params: &ArbiterParams,
    cancel: &CancelToken,
) -> Result<Prediction, MlError> {
    let mut p = Prediction {
        id: item.id.clone(),
        kind: item.kind,
        method: Method::Failed,
        at: None,
        detail: String::new(),
    };
    let gain = asr.input_gain(pcm);
    let words = match asr.transcribe_window(pcm, Flicks::ZERO, gain) {
        Ok(w) => w,
        Err(MlError::Cancelled) => return Err(MlError::Cancelled),
        Err(_) => return Ok(p),
    };
    let Ok(fine) = Energy::fine(pcm, VOICE_RATE) else {
        return Ok(p);
    };
    let local = |w: &MarkWord| MarkWord {
        text: w.text.clone(),
        start: w.start - pcm_start_s,
        end: w.end - pcm_start_s,
    };
    let next_word = |i1: usize| (i1 + 1..words.len()).find(|&j| !is_filler(&words[j].text));
    let by_text = find(&words, &local(&item.w1)).and_then(|i1| {
        next_word(i1)
            .filter(|&j| arbiter::same_word(&words[j].text, &item.w2.text))
            .map(|j| (i1, j))
    });
    // Where gmclip's ASR heard other words, the edge is still the boundary at that time:
    // the gmclip word overlapping the marked word 1 most, and the word after it.
    let by_time = || {
        let w1 = local(&item.w1);
        let secs = |t: Flicks| t.0 as f64 / 705_600_000.0;
        let overlap =
            |w: &AsrWord| (secs(w.end).min(w1.end) - secs(w.start).max(w1.start)).max(0.0);
        let (i1, o) = words
            .iter()
            .enumerate()
            .map(|(i, w)| (i, overlap(w)))
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        (o > 0.0)
            .then_some(i1)
            .and_then(|i1| next_word(i1).map(|j| (i1, j)))
    };
    let pair = by_text.or_else(|| {
        p.detail = "words matched by time".into();
        by_time()
    });
    let Some((i1, i2)) = pair else {
        p.method = Method::WordNotFound;
        p.detail = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return Ok(p);
    };
    let samples = |t: Flicks| (t.0.max(0) / 44_100) as usize; // 44 100 flicks per sample at 16 kHz
    let span = |i: usize| {
        let a = samples(words[i].start).min(pcm.len());
        Span::new(a, samples(words[i].end).clamp(a + 1, pcm.len().max(a + 1)))
    };
    let (s1, s2) = (span(i1), span(i2));
    let peak = |s: Span| peak_frame(&fine, s).map(|(f, _)| fine.frame_centre(f));
    let (p1, p2) = (peak(s1).unwrap_or(s1.start), peak(s2).unwrap_or(s2.end));
    let measured = |e: Option<gmclip_analysis::edges::AudibleEdge>| match e {
        Some(e) if e.method == EdgeMethod::Measured => (Method::Audible, Some(e.sample)),
        Some(e) => (Method::AudibleFallback, Some(e.sample)),
        None => (Method::Failed, None),
    };
    let (method, at) = match item.kind {
        EdgeKind::End => measured(audible_end(&fine, s1, p2)),
        EdgeKind::Start => measured(audible_start(&fine, s2, p1)),
        EdgeKind::Abutting => {
            let (t1, t2) = (words[i1].text.as_str(), words[i2].text.as_str());
            let (m, c) = if arbiter::is_abutting(s1.end, s2.start) {
                let pw1 = PairWord { text: t1, span: s1 };
                let pw2 = PairWord { text: t2, span: s2 };
                match arbiter::arbitrate(tdt, pcm, &fine, pw1, pw2, params, cancel) {
                    Ok(e) => {
                        p.detail = format!(
                            "margin {:.2} nats; {} | {}",
                            e.margin,
                            e.left_words.join(" "),
                            e.right_words.join(" ")
                        );
                        (Method::Arbiter, Some(e.value))
                    }
                    Err(arbiter::ArbiterError::Unresolvable { reason, .. }) => {
                        p.detail = reason.to_owned();
                        (
                            Method::EnergyMinFallback,
                            energy_min(&fine, s1.end, s2.start).map(|m| m.value),
                        )
                    }
                    Err(arbiter::ArbiterError::Ml(MlError::Cancelled)) => {
                        return Err(MlError::Cancelled);
                    }
                    Err(arbiter::ArbiterError::Ml(_)) => {
                        p.detail = "aligner failed".into();
                        (
                            Method::EnergyMinFallback,
                            energy_min(&fine, s1.end, s2.start).map(|m| m.value),
                        )
                    }
                }
            } else {
                (
                    Method::EnergyMin,
                    energy_min(&fine, s1.end, s2.start).map(|m| m.value),
                )
            };
            // the latest point before w2's measured onset
            let onset = audible_start(&fine, s2, p1)
                .filter(|e| e.method == EdgeMethod::Measured)
                .map(|e| e.sample);
            let c = match (c, onset) {
                (Some(c), Some(o)) if o > c && o <= p2 => Some(o),
                (c, _) => c,
            };
            (m, c)
        }
    };
    p.method = method;
    p.at = at.map(|a| pcm_start_s + a as f64 / f64::from(VOICE_RATE));
    Ok(p)
}

/// R-ML-2's numbers over marked, non-skipped items.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Score {
    pub marked: usize,
    pub skipped: usize,
    pub streams: usize,
    /// The marked set meets R-ML-2's shape (see [`score`]).
    pub set_complete: bool,
    pub predicted: usize,
    /// Items whose words gmclip's ASR did not find: counted as misses at +∞.
    pub not_found: usize,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub pass: bool,
    pub by_kind: Vec<(EdgeKind, usize, f64, f64)>,
    pub by_method: Vec<(Method, usize)>,
}

/// Scores predictions against marks. An unpredicted marked item counts as an infinite
/// error, so a pipeline cannot pass by declining edges.
pub fn score(marks: &[MarkItem], predictions: &[Prediction]) -> Score {
    let mut errs: Vec<(EdgeKind, f64)> = Vec::new();
    let mut s = Score::default();
    let mut methods: std::collections::BTreeMap<String, (Method, usize)> = Default::default();
    let mut streams = std::collections::BTreeSet::new();
    for m in marks {
        if m.skipped {
            s.skipped += 1;
            continue;
        }
        let Some(mark) = m.mark else { continue };
        streams.insert(m.id.split('#').next().unwrap_or_default().to_owned());
        s.marked += 1;
        let p = predictions.iter().find(|p| p.id == m.id);
        if let Some(p) = p {
            methods
                .entry(format!("{:?}", p.method))
                .or_insert((p.method, 0))
                .1 += 1;
        }
        match p.and_then(|p| p.at) {
            Some(at) => {
                s.predicted += 1;
                errs.push((m.kind, 1000.0 * (at - mark).abs()));
            }
            None => {
                s.not_found += 1;
                errs.push((m.kind, f64::INFINITY));
            }
        }
    }
    let stats = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (
            quantile(&v, 0.5).unwrap_or(f64::NAN),
            quantile(&v, 0.95).unwrap_or(f64::NAN),
        )
    };
    (s.median_ms, s.p95_ms) = stats(errs.iter().map(|e| e.1).collect());
    s.streams = streams.len();
    let abutting = errs.iter().filter(|e| e.0 == EdgeKind::Abutting).count();
    // R-ML-2's set: ≥ 300 edges from ≥ 5 streams, half of them abutting; skipping hard
    // edges may not buy a pass (≤ 10 % of the decided items skipped)
    s.set_complete = s.marked >= MIN_MARKED
        && s.streams >= MIN_STREAMS
        && abutting * 2 >= s.marked
        && s.skipped * 10 <= s.marked + s.skipped;
    s.pass = s.set_complete && s.median_ms <= MEDIAN_MAX_MS && s.p95_ms <= P95_MAX_MS;
    for k in [EdgeKind::Abutting, EdgeKind::End, EdgeKind::Start] {
        let v: Vec<f64> = errs.iter().filter(|e| e.0 == k).map(|e| e.1).collect();
        if !v.is_empty() {
            let n = v.len();
            let (med, p95) = stats(v);
            s.by_kind.push((k, n, med, p95));
        }
    }
    s.by_method = methods.into_values().collect();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: EdgeKind, mark: Option<f64>) -> MarkItem {
        let w = MarkWord {
            text: "x".into(),
            start: 0.0,
            end: 0.1,
        };
        MarkItem {
            id: id.into(),
            kind,
            w1: w.clone(),
            w2: w,
            clip_start: 0.0,
            clip_end: 1.0,
            mark,
            skipped: false,
            note: String::new(),
        }
    }

    fn pred(id: &str, at: Option<f64>) -> Prediction {
        Prediction {
            id: id.into(),
            kind: EdgeKind::End,
            method: Method::Audible,
            at,
            detail: String::new(),
        }
    }

    #[test]
    fn scores_median_and_p95_and_counts_misses() {
        let marks: Vec<MarkItem> = (0..20)
            .map(|i| {
                item(
                    &format!("e{i}"),
                    if i % 2 == 0 {
                        EdgeKind::Abutting
                    } else {
                        EdgeKind::End
                    },
                    Some(10.0),
                )
            })
            .chain([item("unmarked", EdgeKind::Start, None)])
            .collect();
        // errors 0, 5, 10, … 95 ms
        let preds: Vec<Prediction> = (0..20)
            .map(|i| pred(&format!("e{i}"), Some(10.0 + i as f64 * 0.005)))
            .collect();
        let s = score(&marks, &preds);
        assert_eq!(s.marked, 20);
        assert!((s.median_ms - 45.0).abs() < 1e-6, "{}", s.median_ms);
        assert!((s.p95_ms - 90.0).abs() < 1e-6, "{}", s.p95_ms);
        assert!(!s.pass);
        assert_eq!(s.by_kind.len(), 2);
        // a missing prediction is an infinite error
        let mut preds2 = preds.clone();
        preds2.truncate(18);
        let s2 = score(&marks, &preds2);
        assert_eq!(s2.not_found, 2);
        assert!(s2.p95_ms.is_infinite());
        // passing case
        let good: Vec<Prediction> = (0..20)
            .map(|i| pred(&format!("e{i}"), Some(10.0 + 0.002 * (i % 3) as f64)))
            .collect();
        let s3 = score(&marks, &good);
        assert!(s3.median_ms <= MEDIAN_MAX_MS && s3.p95_ms <= P95_MAX_MS);
        assert!(!s3.pass, "20 edges from one stream is not R-ML-2's set");
        // the full set: 300 from 5 streams, half abutting
        let big: Vec<MarkItem> = (0..300)
            .map(|i| {
                item(
                    &format!("s{}#{i}", i % 5),
                    if i % 2 == 0 {
                        EdgeKind::Abutting
                    } else {
                        EdgeKind::End
                    },
                    Some(1.0),
                )
            })
            .collect();
        let bp: Vec<Prediction> = (0..300)
            .map(|i| pred(&format!("s{}#{i}", i % 5), Some(1.01)))
            .collect();
        assert!(score(&big, &bp).pass);
        // skipping more than 10 % fails the set
        let mut skip = big.clone();
        for m in skip.iter_mut().take(40) {
            m.skipped = true;
        }
        let ss = score(&skip, &bp);
        assert_eq!(ss.skipped, 40);
        assert!(!ss.pass);
    }

    #[test]
    fn the_file_format_round_trips() {
        let f = MarkFile {
            format: FORMAT.into(),
            stream: "loc_x".into(),
            audio_path: "/a.mp4".into(),
            audio_sha256: "ab".into(),
            items: vec![item("loc_x#0", EdgeKind::Abutting, Some(1.25))],
        };
        let bytes = serde_json::to_vec(&f).unwrap();
        assert_eq!(MarkFile::parse(&bytes).unwrap(), f);
        assert!(MarkFile::parse(b"{\"format\":\"other\"}").is_err());
        // the page's JSON (kind spelled in snake case, mark absent) parses
        let page = br#"{"format":"gmclip.edge-marks.v1","stream":"s","audio_path":"p","audio_sha256":"h",
            "items":[{"id":"s#1","kind":"start","w1":{"text":"a","start":1,"end":2},"w2":{"text":"b","start":3,"end":4},
            "clip_start":0,"clip_end":5}]}"#;
        let f = MarkFile::parse(page).unwrap();
        assert_eq!(f.items[0].kind, EdgeKind::Start);
        assert_eq!(f.items[0].mark, None);
    }
}
