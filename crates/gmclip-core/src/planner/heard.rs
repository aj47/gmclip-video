//! Which of a cut's caption words are heard again (SPEC §5.10 `heard`, decision 0022).
//!
//! AJ, 2026-10-16: "every cut should have its own audio if it may be different. otherwise
//! if there is no difference we can re use." So captions always come from the cut's own
//! audio, and a part whose audio is provably the source's reuses the source transcript's
//! words for its span instead of transcribing it again.
//!
//! **What is compared.** The *voice mix* the captions are heard from: part gain → part
//! fades → sum → envelopes (RenderPlan rule 7). The delivery loudness stage after it
//! (rule 8: one static gain and a true-peak limiter over the whole program) is neither
//! heard nor compared — it is the same for every part of a cut and is not an edit of any
//! one of them.
//!
//! **Identical** ([`PartAudit::identical`]) means, for one plan part:
//! - its words can come from a transcript of exactly this audio: the caller says the
//!   part's media has an active transcript heard from the member and track this part
//!   plays (`transcribed`);
//! - no part gain (`gain_db` absent or 0);
//! - no speed change (decision 0030);
//! - no envelope with a non-zero gain overlapping its output samples;
//! - no fade other than the planner's de-click at a cut edge (≤ [`DECLICK_MAX`] samples;
//!   the edge words those samples touch are the edge rule's, see
//!   [`super::captions::words`]).
//!
//! RenderPlan v0 has no other way to change a part's audio: one source per part, no mix
//! layers, no rate change (the planner refuses speed), and a sweep or tighten is a cut
//! (new parts, each a plain sample range). Anything the audit does not recognise as
//! unchanged is heard. A model part split into several plan parts (visual splits) is
//! heard as a whole when any of its plan parts is.
//!
//! **Stretches.** Consecutive heard parts are transcribed together as one [`Stretch`],
//! from the rendered voice mix of their output range plus [`LEAD`] before and [`TAIL`]
//! after (context for the recogniser; clamped to the cut). Stretches whose padded windows
//! would touch are merged, with the parts between them heard too. A stretch is keyed by a
//! signature of everything that decides its samples ([`window_sig`]), so an unchanged
//! stretch is never transcribed twice, across edits and across cuts.

use std::collections::BTreeMap;

use gmclip_plan::types as rp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The longest fade still counted as a cut edge's de-click (10 ms at 48 kHz).
pub const DECLICK_MAX: u64 = 480;
/// Context heard before a stretch (1 s).
pub const LEAD: u64 = 48_000;
/// Context heard after a stretch (2 s).
pub const TAIL: u64 = 96_000;
/// The format of a stored stretch transcription (analysis kind [`KIND`]).
pub const FORMAT: &str = "gmclip.analysis.heard/0";
/// The analysis kind heard stretches are stored under (owner [`OWNER`], revision = sig).
pub const KIND: &str = "heard_words";
/// The analysis owner of heard stretches: they are keyed by content, not by cut.
pub const OWNER: &str = "cut-audio";

/// Which parts are heard. `Rule` is the product; the others exist for measurement.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HearPolicy {
    /// Reuse identical parts, hear the rest (AJ, 2026-10-16).
    #[default]
    Rule,
    /// Hear every part (the cut's whole audio).
    All,
    /// Reuse every part's source words (what `stream` captions were).
    None,
}

/// Why a part is heard.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Part gain.
    Gain,
    /// A gain envelope overlaps it.
    Envelope,
    /// A fade longer than a de-click.
    Fade,
    /// No transcript of exactly this audio (none yet, or another member or track).
    NoTranscript,
    /// Another plan part of the same model part is heard.
    SamePart,
    /// Between two heard runs closer than their context (merged into one stretch).
    Between,
    /// The policy hears every part.
    Policy,
    /// Played at another speed (decision 0030).
    Speed,
}

/// One plan part's voice-mix audio.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PartAudit {
    /// The plan part's id.
    pub part: String,
    /// Output samples.
    pub out: [u64; 2],
    /// Signature of the part's audio as the voice mix plays it.
    pub sig: String,
    /// Empty: identical to the source (its words are reused).
    pub changes: Vec<Change>,
}

impl PartAudit {
    pub fn identical(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Output the cut's own audio is transcribed over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Stretch {
    /// [`window_sig`]: the cache key of its words.
    pub sig: String,
    /// Plan parts `[i, j)`.
    pub parts: [usize; 2],
    /// Output samples whose captions come from it.
    pub out: [u64; 2],
    /// Output samples transcribed (`out` plus context).
    pub window: [u64; 2],
}

/// A cut's caption audit: which parts reuse, which stretches are heard, which of those
/// have no words yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Audit {
    pub policy: HearPolicy,
    pub parts: Vec<PartAudit>,
    pub stretches: Vec<Stretch>,
    /// Stretches with no stored words: until they are heard, their parts' captions fall
    /// back to the source words (preview only; an export hears them first).
    pub missing: Vec<Stretch>,
}

impl Audit {
    /// Output samples whose words are reused.
    pub fn reused_samples(&self) -> u64 {
        let total: u64 = self.parts.iter().map(|p| p.out[1] - p.out[0]).sum();
        total.saturating_sub(self.heard_samples())
    }

    /// Output samples heard (stretches' `out`, without context).
    pub fn heard_samples(&self) -> u64 {
        self.stretches.iter().map(|s| s.out[1] - s.out[0]).sum()
    }

    /// Output samples transcribed (stretches' windows, context included).
    pub fn transcribed_samples(&self) -> u64 {
        self.stretches
            .iter()
            .map(|s| s.window[1] - s.window[0])
            .sum()
    }
}

fn hex(d: impl AsRef<[u8]>) -> String {
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn source_key(sources: &[rp::Source], id: &str) -> String {
    match sources.iter().find(|s| s.id.as_str() == id) {
        Some(s) => {
            let fp = s.fingerprint.as_ref();
            format!(
                "{}|{}|{}|{}|t{}",
                s.path,
                fp.and_then(|f| f.sha256.as_ref())
                    .map_or("", gmclip_plan::scalars::Sha256::as_str),
                fp.and_then(|f| f.size).map_or(-1, |n| n as i64),
                fp.and_then(|f| f.mtime_ns).unwrap_or(-1),
                s.audio_track.map_or(0, i64::from),
            )
        }
        None => format!("?{id}"),
    }
}

fn fade_key(f: Option<&rp::Fade>) -> String {
    match f {
        None => "-".into(),
        Some(f) => format!("{}{:?}", f.samples, f.shape),
    }
}

fn samples(f: u64, fps: rp::Fps) -> u64 {
    gmclip_plan::validate::out_samples(f, fps).unwrap_or(u64::MAX)
}

/// The output samples of plan part `p`.
pub fn part_out(p: &rp::Part, fps: rp::Fps) -> [u64; 2] {
    [samples(p.out.start(), fps), samples(p.out.end(), fps)]
}

/// The samples of output `[a, b)` as the voice mix makes them, as text: per part, its
/// source (path, fingerprint, track), the first source sample and count it plays inside
/// `[a, b)`, its offset from `a`, gain and the fades inside; then every envelope piece
/// inside, relative to `a`. Two ranges with the same key are the same samples.
fn mix_key(
    parts: &[rp::Part],
    envelopes: &[rp::Envelope],
    sources: &[rp::Source],
    fps: rp::Fps,
    a: u64,
    b: u64,
) -> String {
    let mut k = String::new();
    for p in parts {
        let [o0, o1] = part_out(p, fps);
        let (x, y) = (o0.max(a), o1.min(b));
        if x >= y {
            continue;
        }
        if let Some(sp) = p.speed {
            // a sped part's samples depend on its whole span and speed (decision 0030)
            k.push_str(&format!(
                "S{}/{}{}@{}+{};",
                sp.num,
                sp.den,
                if sp.stretches() { "s" } else { "m" },
                p.audio.first_sample,
                o1 - o0
            ));
            if let Some(r) = sp.ramp {
                // decision 0033: and on its ramp
                k.push_str(&format!(
                    "R{}/{}@{}+{}{:?};",
                    r.to.num, r.to.den, r.from, r.frames, r.curve
                ));
            }
        }
        k.push_str(&format!(
            "P{}@{}+{}>{}g{}i{}o{};",
            source_key(sources, p.audio.source.as_str()),
            p.audio.first_sample + (x - o0),
            y - x,
            x - a,
            p.audio.gain_db.unwrap_or(0.0),
            if x == o0 {
                fade_key(p.audio.fade_in())
            } else {
                "-".into()
            },
            if y == o1 {
                fade_key(p.audio.fade_out())
            } else {
                "-".into()
            },
        ));
    }
    for e in envelopes {
        let (s0, s1) = (e.out_samples.start(), e.out_samples.end());
        if s1 <= a || s0 >= b {
            continue;
        }
        // the whole envelope shapes the samples inside (its ramps), so it is keyed whole,
        // relative to the range
        k.push_str(&format!(
            "E{}..{}g{}r{},{};",
            s0 as i128 - a as i128,
            s1 as i128 - a as i128,
            e.gain_db,
            e.ramp_in_samples,
            e.ramp_out_samples
        ));
    }
    k
}

/// The cache key of output `[a, b)`'s heard words: SHA-256 of [`mix_key`].
pub fn window_sig(
    parts: &[rp::Part],
    envelopes: &[rp::Envelope],
    sources: &[rp::Source],
    fps: rp::Fps,
    window: [u64; 2],
) -> String {
    let mut h = Sha256::new();
    h.update(FORMAT.as_bytes());
    h.update(b"\n");
    h.update(mix_key(parts, envelopes, sources, fps, window[0], window[1]).as_bytes());
    hex(h.finalize())
}

/// Audits a cut's parts. `transcribed(i)`: plan part `i`'s media has a transcript of
/// exactly the member and track it plays; `owner[i]`: the model part plan part `i`
/// belongs to (plan parts of one model part are heard together). `total`: output
/// samples of the cut.
#[allow(clippy::too_many_arguments)]
pub fn audit(
    parts: &[rp::Part],
    envelopes: &[rp::Envelope],
    sources: &[rp::Source],
    fps: rp::Fps,
    total: u64,
    owner: &[usize],
    transcribed: &dyn Fn(usize) -> bool,
    policy: HearPolicy,
) -> Audit {
    let mut out: Vec<PartAudit> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let o = part_out(p, fps);
            let mut ch = Vec::new();
            match policy {
                HearPolicy::All => ch.push(Change::Policy),
                HearPolicy::None => {}
                HearPolicy::Rule => {
                    if !transcribed(i) {
                        ch.push(Change::NoTranscript);
                    }
                    if p.audio.gain_db.is_some_and(|g| g != 0.0) {
                        ch.push(Change::Gain);
                    }
                    if p.speed.is_some() {
                        ch.push(Change::Speed);
                    }
                    if envelopes.iter().any(|e| {
                        e.gain_db != 0.0
                            && e.out_samples.start() < o[1]
                            && e.out_samples.end() > o[0]
                    }) {
                        ch.push(Change::Envelope);
                    }
                    if [p.audio.fade_in(), p.audio.fade_out()]
                        .into_iter()
                        .flatten()
                        .any(|f| f.samples > DECLICK_MAX)
                    {
                        ch.push(Change::Fade);
                    }
                }
            }
            PartAudit {
                part: p.id.as_str().to_owned(),
                out: o,
                sig: {
                    let mut h = Sha256::new();
                    h.update(mix_key(parts, envelopes, sources, fps, o[0], o[1]).as_bytes());
                    hex(h.finalize())
                },
                changes: ch,
            }
        })
        .collect();
    // a model part is heard whole
    let mut heard_owner: BTreeMap<usize, bool> = BTreeMap::new();
    for (i, a) in out.iter().enumerate() {
        let o = owner.get(i).copied().unwrap_or(i);
        *heard_owner.entry(o).or_default() |= !a.identical();
    }
    for (i, a) in out.iter_mut().enumerate() {
        let o = owner.get(i).copied().unwrap_or(i);
        if a.identical() && heard_owner.get(&o).copied().unwrap_or(false) {
            a.changes.push(Change::SamePart);
        }
    }
    // runs of heard parts, merged when their windows would touch
    let mut runs: Vec<[usize; 2]> = Vec::new();
    let mut i = 0;
    while i < out.len() {
        if out[i].identical() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < out.len() && !out[j].identical() {
            j += 1;
        }
        match runs.last_mut() {
            Some(r) if out[i].out[0] - out[r[1] - 1].out[1] < LEAD + TAIL => r[1] = j,
            _ => runs.push([i, j]),
        }
        i = j;
    }
    for r in &runs {
        for a in &mut out[r[0]..r[1]] {
            if a.identical() {
                a.changes.push(Change::Between);
            }
        }
    }
    let stretches = runs
        .into_iter()
        .map(|[i, j]| {
            let o = [out[i].out[0], out[j - 1].out[1]];
            let w = [o[0].saturating_sub(LEAD), (o[1] + TAIL).min(total)];
            Stretch {
                sig: window_sig(parts, envelopes, sources, fps, w),
                parts: [i, j],
                out: o,
                window: w,
            }
        })
        .collect();
    Audit {
        policy,
        parts: out,
        stretches,
        missing: Vec::new(),
    }
}

/// [`audit`] of a whole RenderPlan, every source taken as transcribed (`gmclip plan hear`:
/// a plan carries no transcripts, so this measures what the rule would hear).
pub fn audit_plan(plan: &rp::Plan, policy: HearPolicy) -> Audit {
    let envs = plan.audio.envelopes.as_deref().unwrap_or(&[]);
    let total = samples(plan.output.frames, plan.output.fps);
    let owner: Vec<usize> = (0..plan.parts.len()).collect();
    audit(
        &plan.parts,
        envs,
        &plan.sources,
        plan.output.fps,
        total,
        &owner,
        &|_| true,
        policy,
    )
}

/// One word heard in a stretch, in flicks from the window's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeardWord {
    pub text: String,
    pub start: i64,
    pub end: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// A stretch's words as stored (`FORMAT`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeardStretch {
    pub format: String,
    pub sig: String,
    /// The recogniser's `asr_revision`.
    pub revision: String,
    /// Output samples transcribed.
    pub window: [u64; 2],
    pub words: Vec<HeardWord>,
}

impl HeardStretch {
    pub fn new(s: &Stretch, revision: String, words: Vec<HeardWord>) -> Self {
        Self {
            format: FORMAT.into(),
            sig: s.sig.clone(),
            revision,
            window: s.window,
            words,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(body: &[u8]) -> Option<Self> {
        serde_json::from_slice::<Self>(body)
            .ok()
            .filter(|h| h.format == FORMAT)
    }
}

/// What the planner needs to hear a cut's stretches (the app's export, `gmclip plan-cut`).
pub trait Hearer {
    /// Loads what hearing needs (the speech model) so [`Hearer::hear`] times hearing only.
    fn warm(&self) -> Result<(), String> {
        Ok(())
    }
    /// Transcribes each stretch's window of `plan`'s voice mix.
    fn hear(&self, plan: &rp::Plan, stretches: &[Stretch]) -> Result<Vec<HeardStretch>, String>;
}
