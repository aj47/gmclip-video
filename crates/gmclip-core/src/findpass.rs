//! Find moments and the full sweep, chunked (SPEC §10.3.1–2, §10.6; backlog A.01): the
//! pipeline gmclip runs around whoever reads the transcript.
//!
//! - **Chunks:** 20 stream-minutes each (≤ 500 lines), overlapping by 15 lines so a
//!   moment on a seam is offered to both.
//! - **Find moments:** per chunk, a first read → a sweep for "only what it missed" →
//!   one repair of the misquoted; then every uncarded stretch ≥ 120 s gets a gap read
//!   (and its repair). Per chunk at most `max(12, 2 per minute)`, best first.
//! - **Full sweep:** every chunk again with what is carded there; a second round only
//!   while the video is under the density floor.
//! - **Every item** names its lines (`L12`–`L14`, the numbers `transcript_read` shows)
//!   and quotes them. The quote is anchored: its words must cover ≥ 0.55 of a window of
//!   lines in the step, else it is sent back for repair (or dropped, on a repair, a gap's
//!   repair or a sweep); a quote found more than a line from where it claims is snapped
//!   there. Times come from the line table, never from the reader.
//! - **Seams:** twins within 3 s keep the better score. An item near the end of a chunk
//!   is held until the next chunk is in, so the twin can win before anything is written;
//!   what is written is never rewritten.
//!
//! This module is pure: no model, no store, no clock of its own. The agent surface
//! (`gmclip_mcp::findpass`) serves its steps through `ai_pass` / `pass_step` and writes
//! what it returns with `moments_found_add`.

use std::collections::VecDeque;

use serde::Serialize;
use serde_json::Value;

use crate::commands::moment::is_exact_run;
use crate::density;

/// Stream-minutes per chunk.
pub const CHUNK_MINUTES: f64 = 20.0;
/// A chunk's line ceiling (a very dense transcript).
pub const CHUNK_LINES: usize = 500;
/// Lines two neighbouring chunks share.
pub const CHUNK_OVERLAP: usize = 15;
/// Moments whose starts are closer than this (s) are one moment.
pub const DEDUPE_S: f64 = 3.0;
/// An uncarded stretch this long (s) gets a gap read.
pub const GAP_S: f64 = 120.0;
/// Per chunk at most this many moments a minute …
pub const CAP_PER_MIN: f64 = 2.0;
/// … and never fewer than this.
pub const CAP_MIN: usize = 12;
/// How much of a quote's opening words a window of lines must hold.
pub const MATCH_MIN: f64 = 0.55;
/// Full-sweep rounds (the second only under the density floor).
pub const SWEEP_ROUNDS: u32 = 2;
/// A pass run expires this long after its last step (SPEC §10.6).
pub const EXPIRE_MS: i64 = 30 * 60 * 1000;
/// Items one submit may carry.
pub const MAX_ITEMS: usize = 200;
/// The kinds of moment the brief offers; anything else is recorded as `story`.
pub const TYPES: &[&str] = &[
    "result", "reaction", "story", "tip", "fail", "opinion", "demo",
];
/// The neutral label agent-executed results carry (SPEC §10.6).
pub const LABEL: &str = "Found by your agent — recall not measured";

/// One transcript line, source seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A moment already on the video when the pass starts (a human's, an import, an
/// earlier run's): what a read is told is carded, and what nothing may duplicate.
#[derive(Clone, Debug, PartialEq)]
pub struct Carded {
    pub start: f64,
    pub end: f64,
    pub quote: String,
}

/// Which pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    FindMoments,
    FullSweep,
}

impl Pass {
    pub fn parse(s: &str) -> Option<Pass> {
        match s {
            "find_moments" => Some(Pass::FindMoments),
            "full_sweep" => Some(Pass::FullSweep),
            _ => None,
        }
    }

    pub fn def(self) -> &'static crate::passes::PassDef {
        match self {
            Pass::FindMoments => &crate::passes::FIND_MOMENTS,
            Pass::FullSweep => &crate::passes::FULL_SWEEP,
        }
    }
}

/// What a step asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    First,
    Sweep,
    Repair,
    Gap,
    FullSweep,
}

impl StepKind {
    pub fn name(self) -> &'static str {
        match self {
            StepKind::First => "first",
            StepKind::Sweep => "sweep",
            StepKind::Repair => "repair",
            StepKind::Gap => "gap",
            StepKind::FullSweep => "full_sweep",
        }
    }
}

/// One step: the lines `[lo, hi)` it covers and what it is told.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub kind: StepKind,
    pub lo: usize,
    pub hi: usize,
    /// The chunk it belongs to (`None`: a gap).
    pub chunk: Option<usize>,
    /// Full-sweep round (1-based; 0 for find moments).
    pub round: u32,
    /// What is carded here, one line each (untrusted transcript text inside).
    pub already: Vec<String>,
    /// What a repair step is to fix (the submitted items, as given).
    pub candidates: Vec<Value>,
}

/// A checked moment, placed on the line table.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Found {
    /// 0-based line indexes, inclusive.
    pub first_line: usize,
    pub last_line: usize,
    pub start_s: f64,
    pub end_s: f64,
    pub quote: String,
    pub excerpt: Option<String>,
    pub score: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub why: String,
    pub text_hook: Option<String>,
    pub step: StepKind,
    /// Its quote was found more than a line away from the lines it named.
    pub snapped: bool,
    /// The quote is a contiguous run of the words of its lines (quote verification).
    pub exact: bool,
}

/// An item refused outright, by its index in the submit.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Refused {
    pub index: usize,
    pub reason: String,
}

/// What one submit's items came to.
#[derive(Clone, Debug, Default)]
pub struct Cleaned {
    pub found: Vec<Found>,
    /// Items whose quote is not where they claim (as submitted).
    pub misquoted: Vec<(usize, Value)>,
    pub refused: Vec<Refused>,
    pub snapped: usize,
}

/// `(lo, hi)` line ranges each holding at most [`CHUNK_MINUTES`] of the video (and at
/// most [`CHUNK_LINES`] lines), overlapping by [`CHUNK_OVERLAP`] lines.
pub fn chunk_spans(lines: &[Line]) -> Vec<(usize, usize)> {
    let (mut spans, mut i, n) = (Vec::new(), 0usize, lines.len());
    while i < n {
        let mut j = i + 1;
        while j < n && j - i < CHUNK_LINES && lines[j].end - lines[i].start <= CHUNK_MINUTES * 60.0
        {
            j += 1;
        }
        spans.push((i, j));
        if j >= n {
            break;
        }
        // never fails to advance
        i = (i + 1).max(j.saturating_sub(CHUNK_OVERLAP));
    }
    spans
}

/// Lower-case letter/digit runs.
fn norm(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Which line in `[lo, hi)` a quote starts on, and how much of its opening (14 words)
/// the best three-line window holds (0..1).
pub fn anchor_line(quote: &str, lines: &[Line], lo: usize, hi: usize) -> (Option<usize>, f64) {
    let want: Vec<String> = norm(quote).into_iter().take(14).collect();
    if want.is_empty() {
        return (None, 0.0);
    }
    let wset: std::collections::BTreeSet<&str> = want.iter().map(String::as_str).collect();
    let hi = hi.min(lines.len());
    let (mut best, mut best_i) = (0.0, None);
    for i in lo..hi {
        let window: String = lines[i..(i + 3).min(lines.len())]
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let got = norm(&window);
        if got.is_empty() {
            continue;
        }
        let got: std::collections::BTreeSet<&str> = got.iter().map(String::as_str).collect();
        let score = wset.intersection(&got).count() as f64 / wset.len() as f64;
        if score > best {
            (best, best_i) = (score, Some(i));
        }
    }
    let Some(bi) = best_i else {
        return (None, 0.0);
    };
    // the window can start a line or two early: settle on the line the opening words are on
    let head: std::collections::BTreeSet<&str> = want.iter().take(7).map(String::as_str).collect();
    let (mut tight, mut tight_i) = (0.0, bi);
    for (i, l) in lines.iter().enumerate().take((bi + 3).min(hi)).skip(bi) {
        let got = norm(&l.text);
        let got: std::collections::BTreeSet<&str> = got.iter().map(String::as_str).collect();
        let s = if got.is_empty() {
            0.0
        } else {
            head.intersection(&got).count() as f64 / head.len() as f64
        };
        if s > tight {
            (tight, tight_i) = (s, i);
        }
    }
    (Some(tight_i), best)
}

/// A line reference: `"L12"` (as `transcript_read` shows it) or `12`; 1-based.
fn line_ref(v: &Value) -> Option<usize> {
    let n = match v {
        Value::String(s) => s.trim().strip_prefix(['L', 'l'])?.parse::<usize>().ok()?,
        Value::Number(n) => usize::try_from(n.as_u64()?).ok()?,
        _ => return None,
    };
    n.checked_sub(1)
}

fn text_field(
    o: &serde_json::Map<String, Value>,
    k: &str,
    max: usize,
) -> Result<Option<String>, String> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= max => {
            let t = s.trim();
            Ok((!t.is_empty()).then(|| t.to_owned()))
        }
        Some(Value::String(_)) => Err(format!("{k}: at most {max} characters")),
        Some(_) => Err(format!("{k}: text")),
    }
}

/// Checks a submit's items against the step's lines `[lo, hi)`: refs inside the step,
/// score 2–5, a quote anchored (≥ [`MATCH_MIN`]) and snapped when it is more than a line
/// from where it claims.
pub fn clean(items: &[Value], lines: &[Line], lo: usize, hi: usize, kind: StepKind) -> Cleaned {
    let mut out = Cleaned::default();
    for (index, it) in items.iter().enumerate() {
        let mut refuse = |reason: String| out.refused.push(Refused { index, reason });
        let Some(o) = it.as_object() else {
            refuse("an object {start_line, end_line, quote, score, type, why}".into());
            continue;
        };
        let (Some(s), Some(e)) = (
            o.get("start_line").and_then(line_ref),
            o.get("end_line").and_then(line_ref),
        ) else {
            refuse("start_line and end_line: L-numbers from the step's lines".into());
            continue;
        };
        if s < lo || s >= hi || e < lo || e >= hi {
            refuse(format!(
                "start_line/end_line outside this step's lines (L{}–L{})",
                lo + 1,
                hi
            ));
            continue;
        }
        let score = match o.get("score").and_then(Value::as_u64) {
            Some(n @ 2..=5) => n as u8,
            Some(n) if n > 5 => 5,
            _ => {
                refuse("score: 2–5 (never 1)".into());
                continue;
            }
        };
        let fields = (|| -> Result<_, String> {
            let quote = text_field(o, "quote", 4000)?.ok_or("quote: the words said")?;
            let why = text_field(o, "why", 600)?.unwrap_or_default();
            let kind = text_field(o, "type", 40)?.unwrap_or_default();
            let excerpt = text_field(o, "excerpt", 2000)?;
            let hook = text_field(o, "text_hook", 200)?;
            Ok((quote, why, kind, excerpt, hook))
        })();
        let (quote, why, ty, excerpt, text_hook) = match fields {
            Ok(f) => f,
            Err(r) => {
                refuse(r);
                continue;
            }
        };
        let (found, conf) = anchor_line(&quote, lines, lo, hi);
        if conf < MATCH_MIN {
            out.misquoted.push((index, it.clone()));
            continue;
        }
        let (mut s, mut e) = (s, e.max(s));
        let mut snapped = false;
        if let Some(f) = found
            && f.abs_diff(s) > 1
        {
            e = (f + (e - s)).min(lines.len() - 1);
            s = f;
            snapped = true;
            out.snapped += 1;
        }
        let (a, mut b) = (lines[s].start, lines[e].end);
        if b <= a {
            b = a + 8.0;
        }
        let said: Vec<&str> = lines[s.saturating_sub(1)..(e + 2).min(lines.len())]
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        let exact = is_exact_run(&said.join(" "), &quote);
        // an excerpt that is not a run of the quote is dropped: passes quote exactly
        let excerpt = excerpt.filter(|x| is_exact_run(&quote, x));
        out.found.push(Found {
            first_line: s,
            last_line: e,
            start_s: a,
            end_s: b,
            quote,
            excerpt,
            score,
            kind: if TYPES.contains(&ty.as_str()) {
                ty
            } else {
                "story".into()
            },
            why,
            text_hook,
            step: kind,
            snapped,
            exact,
        });
    }
    out
}

/// Counts over a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    /// Items submitted.
    pub submitted: usize,
    /// Moments written.
    pub written: usize,
    /// … of which from a sweep step, a gap step, a repair.
    pub swept: usize,
    pub gap_found: usize,
    pub repaired: usize,
    pub snapped: usize,
    /// Misquoted and not repaired.
    pub unverifiable: usize,
    /// Refused outright (bad refs, score, fields).
    pub refused: usize,
    /// Over a chunk's cap.
    pub capped: usize,
    /// Twins and re-cards of an existing moment.
    pub duplicates: usize,
    /// Written moments whose quote is an exact run of their lines.
    pub exact: usize,
}

impl Stats {
    /// Share of written moments whose quote verified (`None` before any).
    pub fn quote_verified(&self) -> Option<f64> {
        (self.written > 0).then(|| self.exact as f64 / self.written as f64)
    }
}

/// The run's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Done,
    /// Expired before its last step: what was submitted is kept.
    Partial,
}

/// Why a submit was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepError {
    /// 30 minutes since the last step: the run is recorded partial.
    Expired,
    /// The run already ended.
    Finished,
    TooMany,
}

/// What a submit came to.
#[derive(Clone, Debug, Default)]
pub struct Submitted {
    /// Moments to write now (a twin held at a seam is written with the next chunk).
    pub write: Vec<Found>,
    pub refused: Vec<Refused>,
    /// Indexes sent back for repair (the next step repairs them).
    pub to_repair: Vec<usize>,
    /// Indexes dropped as misquoted (no repair left).
    pub dropped: Vec<usize>,
    /// Indexes that were twins or re-cards.
    pub duplicates: usize,
    pub capped: usize,
}

/// A step's summary, for the run record.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepRecord {
    pub kind: StepKind,
    /// 1-based, inclusive, as the reader saw them.
    pub lines: (usize, usize),
    pub round: u32,
    pub submitted: usize,
}

/// The chunked pipeline for one video.
#[derive(Clone, Debug)]
pub struct Pipeline {
    pub pass: Pass,
    lines: Vec<Line>,
    words: Vec<usize>,
    chunks: Vec<(usize, usize)>,
    carded: Vec<Carded>,
    written: Vec<Found>,
    held: Vec<Found>,
    cur: Option<Step>,
    queue: VecDeque<Step>,
    /// The chunk in progress: what its reads found and what they misquoted.
    acc: Vec<Found>,
    mis: Vec<Value>,
    round: u32,
    round_new: usize,
    pub stats: Stats,
    pub done: Vec<StepRecord>,
    pub state: RunState,
    pub last_ms: i64,
}

impl Pipeline {
    /// A run over `lines` (in time order) on a video already carrying `carded`.
    pub fn new(pass: Pass, lines: Vec<Line>, carded: Vec<Carded>, now_ms: i64) -> Pipeline {
        let chunks = chunk_spans(&lines);
        let words = lines
            .iter()
            .map(|l| l.text.split_whitespace().count())
            .collect();
        let mut p = Pipeline {
            pass,
            lines,
            words,
            chunks,
            carded,
            written: Vec::new(),
            held: Vec::new(),
            cur: None,
            queue: VecDeque::new(),
            acc: Vec::new(),
            mis: Vec::new(),
            round: u32::from(pass == Pass::FullSweep),
            round_new: 0,
            stats: Stats::default(),
            done: Vec::new(),
            state: RunState::Running,
            last_ms: now_ms,
        };
        match pass {
            Pass::FindMoments => p.queue_chunk(0, StepKind::First),
            Pass::FullSweep => {
                for i in 0..p.chunks.len() {
                    p.queue_chunk(i, StepKind::FullSweep);
                }
            }
        }
        p.advance();
        p
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// How many chunks the video has.
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }

    /// The step to answer, while running.
    pub fn step(&self) -> Option<&Step> {
        self.cur
            .as_ref()
            .filter(|_| self.state == RunState::Running)
    }

    /// Steps not yet answered (the current one first), for a partial record.
    pub fn remaining(&self) -> Vec<StepRecord> {
        self.cur
            .iter()
            .chain(self.queue.iter())
            .map(|s| StepRecord {
                kind: s.kind,
                lines: (s.lo + 1, s.hi),
                round: s.round,
                submitted: 0,
            })
            .collect()
    }

    fn queue_chunk(&mut self, i: usize, kind: StepKind) {
        if let Some(&(lo, hi)) = self.chunks.get(i) {
            self.queue.push_back(Step {
                kind,
                lo,
                hi,
                chunk: Some(i),
                round: self.round,
                already: Vec::new(),
                candidates: Vec::new(),
            });
        }
    }

    /// Every moment's span on the video now (carded, written, held).
    pub fn spans(&self) -> Vec<(f64, f64)> {
        self.carded
            .iter()
            .map(|c| (c.start, c.end))
            .chain(
                self.written
                    .iter()
                    .chain(&self.held)
                    .map(|f| (f.start_s, f.end_s)),
            )
            .collect()
    }

    /// The density gate over every moment on the video now.
    pub fn density(&self) -> Option<density::Density> {
        let lines: Vec<density::Line> = self
            .lines
            .iter()
            .zip(&self.words)
            .map(|(l, &w)| density::Line {
                start: l.start,
                end: l.end,
                words: w,
            })
            .collect();
        density::density(&lines, &self.spans())
    }

    /// What is carded in `[lo, hi)`'s time range, one listing line each.
    fn listing(&self, lo: usize, hi: usize) -> Vec<String> {
        let (a, b) = (self.lines[lo].start, self.lines[hi - 1].end);
        let mut v: Vec<(f64, f64, &str)> = self
            .carded
            .iter()
            .map(|c| (c.start, c.end, c.quote.as_str()))
            .chain(
                self.written
                    .iter()
                    .chain(&self.held)
                    .map(|f| (f.start_s, f.end_s, f.quote.as_str())),
            )
            .filter(|(s, _, _)| a <= *s && *s <= b)
            .collect();
        v.sort_by(|x, y| x.0.total_cmp(&y.0));
        v.iter()
            .map(|(s, e, q)| {
                format!(
                    "[{} – {}] \"{}\"",
                    clock(*s),
                    clock(*e),
                    q.chars().take(110).collect::<String>()
                )
            })
            .collect()
    }

    /// Makes the next queued step current (its listing taken now, so it sees every
    /// moment written before it).
    fn advance(&mut self) {
        self.cur = self.queue.pop_front().map(|mut s| {
            if s.kind == StepKind::FullSweep {
                s.already = self.listing(s.lo, s.hi);
            }
            s
        });
        if self.cur.is_none() {
            self.state = RunState::Done;
        }
    }

    /// Marks the run partial if its last step was [`EXPIRE_MS`] ago; returns what it
    /// held at a seam, to be written now (submitted chunks are kept).
    pub fn expire_if_idle(&mut self, now_ms: i64) -> Option<Vec<Found>> {
        if self.state != RunState::Running || now_ms - self.last_ms < EXPIRE_MS {
            return None;
        }
        self.state = RunState::Partial;
        let held = std::mem::take(&mut self.held);
        self.stats.written += held.len();
        self.stats.exact += held.iter().filter(|f| f.exact).count();
        self.written.extend(held.iter().cloned());
        Some(held)
    }

    /// Answers the current step. A run idle past [`EXPIRE_MS`] refuses it; the caller
    /// runs [`Pipeline::expire_if_idle`] first to keep what the run held.
    pub fn submit(&mut self, items: &[Value], now_ms: i64) -> Result<Submitted, StepError> {
        if self.state == RunState::Running && now_ms - self.last_ms >= EXPIRE_MS {
            return Err(StepError::Expired);
        }
        match self.state {
            RunState::Partial => return Err(StepError::Expired),
            RunState::Done => return Err(StepError::Finished),
            RunState::Running => {}
        }
        if items.len() > MAX_ITEMS {
            return Err(StepError::TooMany);
        }
        let Some(step) = self.cur.clone() else {
            return Err(StepError::Finished);
        };
        self.last_ms = now_ms;
        self.stats.submitted += items.len();
        self.done.push(StepRecord {
            kind: step.kind,
            lines: (step.lo + 1, step.hi),
            round: step.round,
            submitted: items.len(),
        });
        let c = clean(items, &self.lines, step.lo, step.hi, step.kind);
        self.stats.refused += c.refused.len();
        self.stats.snapped += c.snapped;
        let mut out = Submitted {
            refused: c.refused,
            ..Submitted::default()
        };
        let mis_idx: Vec<usize> = c.misquoted.iter().map(|(i, _)| *i).collect();
        let mis_vals: Vec<Value> = c.misquoted.into_iter().map(|(_, v)| v).collect();
        match step.kind {
            StepKind::First => {
                self.acc.extend(c.found);
                self.mis.extend(mis_vals);
                out.to_repair = mis_idx;
                if items.len() == out.refused.len() {
                    // nothing read as a moment: no sweep of an empty list
                    self.finish_chunk(step, &mut out);
                } else {
                    // what the first read meant as moments (not what was refused)
                    let listing = items
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !out.refused.iter().any(|r| r.index == *i))
                        .filter_map(|(_, v)| v.as_object())
                        .map(|o| {
                            let r = |k: &str| {
                                o.get(k).map_or_else(String::new, |v| match v {
                                    Value::String(s) => s.chars().take(12).collect(),
                                    v => v.to_string(),
                                })
                            };
                            let q: String = o
                                .get("quote")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .chars()
                                .take(90)
                                .collect();
                            format!("{}–{}: \"{q}\"", r("start_line"), r("end_line"))
                        })
                        .collect();
                    self.queue.push_front(Step {
                        kind: StepKind::Sweep,
                        already: listing,
                        ..step
                    });
                }
            }
            StepKind::Sweep => {
                self.stats.swept += c.found.len();
                self.acc.extend(c.found);
                self.mis.extend(mis_vals);
                out.to_repair = mis_idx;
                self.after_reads(step, &mut out);
            }
            StepKind::Gap => {
                self.acc.extend(c.found);
                self.mis.extend(mis_vals);
                out.to_repair = mis_idx;
                self.after_reads(step, &mut out);
            }
            StepKind::Repair => {
                self.stats.repaired += c.found.len();
                self.stats.unverifiable += step.candidates.len().saturating_sub(c.found.len());
                out.dropped = mis_idx;
                self.acc.extend(c.found);
                self.mis.clear();
                if step.chunk.is_some() {
                    self.finish_chunk(step, &mut out);
                } else {
                    self.finish_gap(&mut out);
                }
            }
            StepKind::FullSweep => {
                self.stats.unverifiable += mis_idx.len();
                out.dropped = mis_idx;
                self.acc.extend(c.found);
                self.finish_sweep_chunk(step, &mut out);
            }
        }
        self.advance();
        Ok(out)
    }

    /// After a chunk's sweep or a gap read: repair what was misquoted, once, else close.
    fn after_reads(&mut self, step: Step, out: &mut Submitted) {
        if !self.mis.is_empty() {
            let candidates = std::mem::take(&mut self.mis);
            self.queue.push_front(Step {
                kind: StepKind::Repair,
                already: Vec::new(),
                candidates,
                ..step
            });
        } else if step.chunk.is_some() {
            self.finish_chunk(step, out);
        } else {
            self.finish_gap(out);
        }
    }

    /// True when `f` repeats a moment already carded or written (start within 3 s).
    fn is_recard(&self, f: &Found) -> bool {
        self.carded
            .iter()
            .map(|c| c.start)
            .chain(self.written.iter().map(|w| w.start_s))
            .any(|s| (s - f.start_s).abs() < DEDUPE_S)
    }

    /// Twins among `v` (start order): the better score stays.
    fn twins(v: Vec<Found>, dups: &mut usize) -> Vec<Found> {
        let mut v = v;
        v.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let mut kept: Vec<Found> = Vec::new();
        for f in v {
            match kept.last_mut() {
                Some(k) if (f.start_s - k.start_s).abs() < DEDUPE_S => {
                    *dups += 1;
                    if f.score > k.score {
                        *k = f;
                    }
                }
                _ => kept.push(f),
            }
        }
        kept
    }

    /// Writes `v` (after twins and re-cards), holding back what sits on the seam with
    /// chunk `next` when there is one.
    fn emit(&mut self, v: Vec<Found>, next: Option<usize>, out: &mut Submitted) {
        let mut dups = 0;
        let v: Vec<Found> = Self::twins(v, &mut dups)
            .into_iter()
            .filter(|f| {
                let r = self.is_recard(f);
                dups += usize::from(r);
                !r
            })
            .collect();
        let seam = next
            .and_then(|i| self.chunks.get(i))
            .map(|&(lo, _)| self.lines[lo].start - DEDUPE_S);
        let (hold, write): (Vec<Found>, Vec<Found>) = v
            .into_iter()
            .partition(|f| seam.is_some_and(|s| f.start_s >= s));
        self.held = hold;
        self.stats.duplicates += dups;
        out.duplicates += dups;
        self.stats.written += write.len();
        self.stats.exact += write.iter().filter(|f| f.exact).count();
        self.round_new += write.len();
        self.written.extend(write.iter().cloned());
        out.write.extend(write);
    }

    /// A find-moments chunk is in: cap it, write it, go on (next chunk, or the gaps).
    fn finish_chunk(&mut self, step: Step, out: &mut Submitted) {
        let i = step.chunk.unwrap_or_default();
        let (lo, hi) = self.chunks[i];
        let mut acc = std::mem::take(&mut self.acc);
        self.mis.clear();
        let win_min = (self.lines[hi - 1].end - self.lines[lo].start) / 60.0;
        let cap = CAP_MIN.max((win_min * CAP_PER_MIN).round() as usize);
        acc.sort_by(|a, b| b.score.cmp(&a.score).then(a.start_s.total_cmp(&b.start_s)));
        if acc.len() > cap {
            let over = acc.len() - cap;
            self.stats.capped += over;
            out.capped += over;
            acc.truncate(cap);
        }
        let mut all = std::mem::take(&mut self.held);
        all.extend(acc);
        let next = (i + 1 < self.chunks.len()).then_some(i + 1);
        self.emit(all, next, out);
        match next {
            Some(n) => self.queue_chunk(n, StepKind::First),
            None => self.queue_gaps(),
        }
    }

    /// A gap read (and its repair) is in: write it.
    fn finish_gap(&mut self, out: &mut Submitted) {
        let acc = std::mem::take(&mut self.acc);
        self.mis.clear();
        let before = out.write.len();
        self.emit(acc, None, out);
        self.stats.gap_found += out.write.len() - before;
    }

    /// Every uncarded stretch ≥ [`GAP_S`] once the chunks are in.
    fn queue_gaps(&mut self) {
        let Some(first) = self.lines.first() else {
            return;
        };
        let mut spans = self.spans();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps = Vec::new();
        let mut prev = first.start;
        for (s, e) in spans {
            if s - prev >= GAP_S {
                gaps.push((prev, s));
            }
            prev = prev.max(e);
        }
        if let Some(last) = self.lines.last()
            && last.end - prev >= GAP_S
        {
            gaps.push((prev, last.end));
        }
        for (a, b) in gaps {
            let idx: Vec<usize> = (0..self.lines.len())
                .filter(|&i| self.lines[i].start >= a && self.lines[i].end <= b)
                .collect();
            if let (Some(&lo), Some(&hi)) = (idx.first(), idx.last()) {
                self.queue.push_back(Step {
                    kind: StepKind::Gap,
                    lo,
                    hi: hi + 1,
                    chunk: None,
                    round: 0,
                    already: Vec::new(),
                    candidates: Vec::new(),
                });
            }
        }
    }

    /// A full-sweep chunk is in: write it; at a round's end, another round only while
    /// the video is under the density floor and the round found something.
    fn finish_sweep_chunk(&mut self, step: Step, out: &mut Submitted) {
        let i = step.chunk.unwrap_or_default();
        let acc = std::mem::take(&mut self.acc);
        let mut all = std::mem::take(&mut self.held);
        all.extend(acc);
        let next = (i + 1 < self.chunks.len()).then_some(i + 1);
        let before = out.write.len();
        self.emit(all, next, out);
        self.stats.swept += out.write.len() - before;
        if next.is_none() && self.queue.is_empty() {
            let thin = self.density().is_some_and(|d| !d.ok);
            if self.round < SWEEP_ROUNDS && self.round_new > 0 && thin {
                self.round += 1;
                self.round_new = 0;
                for c in 0..self.chunks.len() {
                    self.queue_chunk(c, StepKind::FullSweep);
                }
            }
        }
    }
}

/// `h:mm:ss` / `m:ss`.
pub fn clock(s: f64) -> String {
    let t = s.max(0.0).round() as u64;
    let (h, m, sec) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A steady talk: one 8-word line every 5 s for `minutes`, each line unique.
    fn talk(minutes: usize) -> Vec<Line> {
        (0..minutes * 12)
            .map(|i| Line {
                start: i as f64 * 5.0,
                end: i as f64 * 5.0 + 4.0,
                text: format!("line {i} says alpha{i} beta{i} gamma{i} delta{i} and more"),
            })
            .collect()
    }

    fn item(l: &[Line], s: usize, e: usize, score: u8) -> Value {
        let q: Vec<&str> = l[s..=e].iter().map(|x| x.text.as_str()).collect();
        json!({"start_line": format!("L{}", s + 1), "end_line": format!("L{}", e + 1),
               "quote": q.join(" "), "score": score, "type": "result", "why": "a number"})
    }

    #[test]
    fn chunks_are_twenty_minutes_and_share_fifteen_lines() {
        let l = talk(50);
        let c = chunk_spans(&l);
        assert_eq!(
            c[0],
            (0, 240),
            "20 min of 5 s lines (the window's end within 1200 s)"
        );
        assert_eq!(c[1].0, 240 - CHUNK_OVERLAP);
        assert_eq!(c.last().unwrap().1, l.len());
        for w in c.windows(2) {
            assert_eq!(w[0].1 - w[1].0, CHUNK_OVERLAP);
        }
        // a dense transcript is bounded by lines
        let dense: Vec<Line> = (0..1200)
            .map(|i| Line {
                start: i as f64 * 0.5,
                end: i as f64 * 0.5 + 0.4,
                text: "x".into(),
            })
            .collect();
        assert_eq!(chunk_spans(&dense)[0], (0, CHUNK_LINES));
        assert!(chunk_spans(&[]).is_empty());
    }

    #[test]
    fn anchoring_snaps_and_refuses() {
        let l = talk(5);
        // the quote of L31–L32 claimed at L10–L11: snapped to L31
        let mut it = item(&l, 30, 31, 4);
        it["start_line"] = json!("L10");
        it["end_line"] = json!("L11");
        let c = clean(&[it], &l, 0, l.len(), StepKind::First);
        assert_eq!(c.snapped, 1);
        assert_eq!((c.found[0].first_line, c.found[0].last_line), (30, 31));
        assert_eq!((c.found[0].start_s, c.found[0].end_s), (150.0, 159.0));
        assert!(c.found[0].exact && c.found[0].snapped);
        // one line off is left where it claims
        let mut it = item(&l, 30, 31, 4);
        it["start_line"] = json!(30);
        let c = clean(&[it], &l, 0, l.len(), StepKind::First);
        assert_eq!((c.snapped, c.found[0].first_line), (0, 29));
        // words that are not there: misquoted (repair); bad refs, scores: refused
        let bad = [
            json!({"start_line": "L3", "end_line": "L3", "quote": "nothing like this was ever said here", "score": 3}),
            json!({"start_line": "L3", "end_line": "L3", "quote": l[2].text, "score": 1}),
            json!({"start_line": "L999", "end_line": "L999", "quote": l[2].text, "score": 3}),
            json!({"start_line": "three", "end_line": "L3", "quote": l[2].text, "score": 3}),
            json!({"start_line": "L3", "end_line": "L3", "quote": l[2].text, "score": 3, "why": "x".repeat(601)}),
            json!("L3"),
        ];
        let c = clean(&bad, &l, 0, l.len(), StepKind::First);
        assert_eq!(c.misquoted.len(), 1);
        assert_eq!(
            c.refused.iter().map(|r| r.index).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        // outside the step's own lines
        let c = clean(&[item(&l, 2, 2, 3)], &l, 10, 20, StepKind::Gap);
        assert!(c.refused[0].reason.contains("L11–L20"), "{:?}", c.refused);
        // a paraphrase that keeps the words anchors but does not verify
        let mut it = item(&l, 5, 5, 3);
        it["quote"] = json!("line 5 says beta5 alpha5 gamma5 delta5 and more");
        let c = clean(&[it], &l, 0, l.len(), StepKind::First);
        assert!(!c.found[0].exact);
        // unknown types are stories; an excerpt that is not a run of the quote goes
        let mut it = item(&l, 5, 5, 9);
        it["type"] = json!("banger");
        it["excerpt"] = json!("not in it");
        let f = &clean(&[it], &l, 0, l.len(), StepKind::First).found[0];
        assert_eq!(
            (f.kind.as_str(), f.score, f.excerpt.as_ref()),
            ("story", 5, None)
        );
    }

    fn answer(p: &mut Pipeline, items: Vec<Value>) -> Submitted {
        p.submit(&items, 0).unwrap()
    }

    #[test]
    fn find_moments_runs_first_sweep_repair_per_chunk_then_the_gaps() {
        let l = talk(45); // 540 lines: chunks [0,240) [225,465) [450,540)
        let mut p = Pipeline::new(Pass::FindMoments, l.clone(), vec![], 0);
        assert_eq!(p.chunks(), 3);
        let s = p.step().unwrap().clone();
        assert_eq!(
            (s.kind, s.lo, s.hi, s.chunk),
            (StepKind::First, 0, 240, Some(0))
        );
        // first: two good moments, one misquote
        let bad = json!({"start_line": "L50", "end_line": "L50", "quote": "words nobody said at all anywhere", "score": 3});
        let out = answer(
            &mut p,
            vec![item(&l, 10, 12, 4), item(&l, 100, 101, 3), bad.clone()],
        );
        assert!(
            out.write.is_empty(),
            "nothing is written before the chunk closes"
        );
        assert_eq!(out.to_repair, [2]);
        // sweep: told what the first read found; adds one
        let s = p.step().unwrap().clone();
        assert_eq!(s.kind, StepKind::Sweep);
        assert_eq!(s.already.len(), 3);
        assert!(
            s.already[0].starts_with("L11–L13: \"line 10 says"),
            "{:?}",
            s.already
        );
        answer(&mut p, vec![item(&l, 150, 151, 2)]);
        // repair: the misquote comes back as a candidate; the fix is accepted
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.candidates.len()), (StepKind::Repair, 1));
        assert_eq!(s.candidates[0], bad);
        let out = answer(&mut p, vec![item(&l, 50, 50, 3)]);
        assert_eq!(
            out.write.len(),
            4,
            "the chunk closes: 2 + 1 swept + 1 repaired"
        );
        assert_eq!(p.stats.repaired, 1);
        assert_eq!(p.stats.swept, 1);
        // chunk 2: a seam twin of the next chunk is held, then the better twin wins
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.lo), (StepKind::First, 225));
        let out = answer(&mut p, vec![item(&l, 300, 301, 3), item(&l, 455, 456, 2)]);
        assert!(out.write.is_empty());
        let out = answer(&mut p, vec![]); // its sweep finds nothing new
        assert_eq!(
            out.write.len(),
            1,
            "L456 sits on the seam with chunk 3: held"
        );
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.lo), (StepKind::First, 450));
        answer(&mut p, vec![item(&l, 455, 456, 5), item(&l, 520, 521, 3)]);
        let out = answer(&mut p, vec![]);
        assert_eq!(out.write.len(), 2);
        assert_eq!(out.duplicates, 1);
        let twin = out.write.iter().find(|f| f.first_line == 455).unwrap();
        assert_eq!(twin.score, 5, "the better-scored twin");
        // gaps: every uncarded stretch ≥ 120 s, in order
        let gaps: Vec<(usize, usize)> = std::iter::from_fn(|| {
            let s = p.step()?.clone();
            assert_eq!(s.kind, StepKind::Gap);
            answer(&mut p, vec![]);
            Some((s.lo, s.hi))
        })
        .collect();
        // moments at 50–64 s, 250, 500, 750, 1500, 2275, 2600 s; video ends at 2699 s
        assert_eq!(
            gaps,
            [
                (13, 50),
                (51, 100),
                (102, 150),
                (152, 300),
                (302, 455),
                (457, 520)
            ]
        );
        assert_eq!(p.state, RunState::Done);
        assert!(p.step().is_none());
        assert_eq!(p.stats.written, 7);
        assert_eq!(p.stats.quote_verified(), Some(1.0));
        assert_eq!(p.submit(&[], 0).unwrap_err(), StepError::Finished);
        let d = p.density().unwrap();
        assert_eq!(d.moments, 7);
        assert!(!d.ok, "7 moments in 45 min is thin: {d:?}");
    }

    #[test]
    fn a_gap_read_is_written_and_a_recard_is_not() {
        let l = talk(10); // one chunk
        let carded = vec![Carded {
            start: 300.0,
            end: 320.0,
            quote: l[60].text.clone(),
        }];
        let mut p = Pipeline::new(Pass::FindMoments, l.clone(), carded, 0);
        // a re-card of the existing moment (60 → 300 s) and a new one
        answer(&mut p, vec![item(&l, 60, 61, 5), item(&l, 10, 11, 3)]);
        let out = answer(&mut p, vec![]);
        assert_eq!(out.write.len(), 1);
        assert_eq!(out.duplicates, 1);
        // gaps between 55 s and 300 s, and after 320 s
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.lo, s.hi), (StepKind::Gap, 12, 60));
        let out = answer(&mut p, vec![item(&l, 40, 41, 3)]);
        assert_eq!(out.write.len(), 1);
        assert_eq!(p.stats.gap_found, 1);
        let s = p.step().unwrap().clone();
        assert_eq!((s.lo, s.hi), (64, 120));
        // a misquote in a gap gets its repair; a repair's misquote is dropped
        let out = answer(
            &mut p,
            vec![
                json!({"start_line": "L70", "end_line": "L70", "quote": "zzz qqq never", "score": 3}),
            ],
        );
        assert_eq!(out.to_repair, [0]);
        assert_eq!(p.step().unwrap().kind, StepKind::Repair);
        let out = answer(
            &mut p,
            vec![
                json!({"start_line": "L70", "end_line": "L70", "quote": "still zzz qqq never", "score": 3}),
            ],
        );
        assert_eq!(out.dropped, [0]);
        assert_eq!(p.stats.unverifiable, 1);
        assert_eq!(p.state, RunState::Done);
    }

    #[test]
    fn the_chunk_cap_keeps_the_best() {
        let l = talk(5); // 60 lines, 5 min: cap 12
        let mut p = Pipeline::new(Pass::FindMoments, l.clone(), vec![], 0);
        let items: Vec<Value> = (0..20)
            .map(|i| item(&l, i * 3, i * 3, if i < 5 { 5 } else { 2 }))
            .collect();
        answer(&mut p, items);
        let out = answer(&mut p, vec![]);
        assert_eq!(out.write.len(), CAP_MIN);
        assert_eq!(out.capped, 8);
        assert_eq!(out.write.iter().filter(|f| f.score == 5).count(), 5);
    }

    #[test]
    fn an_idle_run_expires_partial_and_keeps_what_was_submitted() {
        let l = talk(45);
        let mut p = Pipeline::new(Pass::FindMoments, l.clone(), vec![], 0);
        answer(&mut p, vec![item(&l, 10, 10, 3), item(&l, 230, 231, 4)]);
        let out = answer(&mut p, vec![]);
        assert_eq!(out.write.len(), 1, "L231 is held at the seam");
        assert!(p.expire_if_idle(EXPIRE_MS - 1).is_none());
        let held = p.expire_if_idle(EXPIRE_MS + 1).unwrap();
        assert_eq!(held.len(), 1, "the held moment is kept");
        assert_eq!(p.state, RunState::Partial);
        assert!(p.step().is_none());
        assert_eq!(
            p.submit(&[], EXPIRE_MS + 2).unwrap_err(),
            StepError::Expired
        );
        let left = p.remaining();
        assert_eq!(left[0].kind, StepKind::First);
        assert_eq!(left[0].lines, (226, 465), "the unsubmitted chunk is listed");
        assert_eq!(p.stats.written, 2);
        // a run that keeps stepping does not expire
        let mut q = Pipeline::new(Pass::FindMoments, l, vec![], 0);
        q.submit(&[], EXPIRE_MS - 10).unwrap();
        assert!(q.expire_if_idle(2 * EXPIRE_MS - 20).is_none());
    }

    #[test]
    fn the_full_sweep_lists_the_carded_and_runs_a_second_round_only_when_thin() {
        let l = talk(30); // 360 lines: chunks [0,240) [225,360)
        let carded = vec![Carded {
            start: 100.0,
            end: 130.0,
            quote: "an earlier moment".into(),
        }];
        let mut p = Pipeline::new(Pass::FullSweep, l.clone(), carded.clone(), 0);
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.round, s.lo), (StepKind::FullSweep, 1, 0));
        assert_eq!(s.already, ["[1:40 – 2:10] \"an earlier moment\""]);
        // a re-card is dropped, a misquote dropped (no repair in a sweep)
        let out = answer(
            &mut p,
            vec![
                item(&l, 20, 21, 3),
                item(&l, 30, 30, 3),
                json!({"start_line": "L5", "end_line": "L5", "quote": "never said zzz", "score": 2}),
            ],
        );
        assert_eq!(out.write.len(), 1);
        assert_eq!((out.duplicates, out.dropped.clone()), (1, vec![2]));
        // the next chunk's listing sees what was just written
        let s = p.step().unwrap().clone();
        assert_eq!(s.lo, 225);
        answer(&mut p, vec![item(&l, 300, 301, 4)]);
        // 3 moments in 30 min: still thin and the round found something → round 2
        let s = p.step().unwrap().clone();
        assert_eq!((s.kind, s.round), (StepKind::FullSweep, 2));
        assert_eq!(s.already.len(), 2);
        answer(&mut p, vec![]);
        answer(&mut p, vec![]);
        assert_eq!(p.state, RunState::Done, "two rounds at most");
        // a sweep that reaches the floor stops after one round
        let mut q = Pipeline::new(Pass::FullSweep, l.clone(), carded, 0);
        let many: Vec<Value> = (0..14)
            .map(|i| item(&l, i * 16 + 2, i * 16 + 2, 3))
            .collect();
        answer(&mut q, many);
        answer(
            &mut q,
            (0..4)
                .map(|i| item(&l, 250 + i * 25, 250 + i * 25, 3))
                .collect(),
        );
        assert!(q.density().unwrap().ok);
        assert_eq!(q.state, RunState::Done);
    }

    #[test]
    fn steps_have_their_instructions() {
        for k in [
            StepKind::First,
            StepKind::Sweep,
            StepKind::Repair,
            StepKind::Gap,
        ] {
            assert!(Pass::FindMoments.def().step(k.name()).is_some(), "{k:?}");
        }
        assert!(
            Pass::FullSweep
                .def()
                .step(StepKind::FullSweep.name())
                .is_some()
        );
        assert_eq!(Pass::parse("full_sweep"), Some(Pass::FullSweep));
        assert_eq!(clock(3725.4), "1:02:05");
        assert_eq!(clock(65.0), "1:05");
    }
}
