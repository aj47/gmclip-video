//! Commands on the analysis (SPEC §9, §5.5, R-MODEL-4):
//! - `asr_activate`: switch a source's active transcript, re-anchoring every word
//!   reference from the old revision to the new one (one undoable transaction);
//! - `measurement_add` (holder-internal): record a measurement the holder took;
//! - `sweep_apply` (holder-internal): remove a sweep's spans as one transaction;
//! - `sweep_pauses`, `sweep_fillers`, `source_analyze`: answered by the holder, which
//!   runs the analysis host (I/O) and then calls the internal commands.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, WordId, WordRef};
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use super::words::remove_span;
use crate::analysis::{Transcript, norm, realign};
use crate::command::{Command, Cx};

fn holder_only() -> CmdError {
    CmdError::Unsupported {
        platform: "actor".into(),
        reason: "answered by the holder".into(),
    }
}

// ---------------------------------------------------------------------------------------
// asr_activate (R-MODEL-4)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AsrActivateArgs {
    /// The source (or sync group) id whose transcript changes.
    pub owner: String,
    /// The ASR revision to make active (one the project holds).
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AsrActivated {
    pub owner: String,
    pub from: Option<String>,
    pub to: String,
    /// Word references moved to the new revision.
    pub remapped: usize,
    /// `entity field` of references with no unique match (kept on the old revision).
    pub unresolved: Vec<String>,
    /// Word time overrides dropped (they belonged to the old words).
    pub overrides_dropped: usize,
}

#[derive(Debug)]
pub struct AsrActivate;

/// Maps word references from one transcript onto another.
struct Remap<'a> {
    old_rev: &'a str,
    to: &'a Transcript,
    map: BTreeMap<u32, u32>,
    remapped: usize,
    unresolved: Vec<String>,
}

impl Remap<'_> {
    fn word_id(&mut self, w: &WordId, at: &str) -> Option<WordId> {
        if w.asr_revision != self.old_rev {
            return None;
        }
        match self.map.get(&w.ordinal) {
            Some(n) => {
                self.remapped += 1;
                Some(WordId {
                    asr_revision: self.to.revision.clone(),
                    ordinal: *n,
                })
            }
            None => {
                self.unresolved.push(at.to_owned());
                None
            }
        }
    }

    fn word_ref(&mut self, r: &WordRef, at: &str) -> Option<WordRef> {
        let id = self.word_id(&r.word_id, at)?;
        let k = id.ordinal as usize;
        let w = self.to.words.get(k)?;
        let around = |range: std::ops::Range<usize>| -> Vec<String> {
            range
                .filter_map(|i| self.to.words.get(i))
                .map(|x| norm(&x.text))
                .collect()
        };
        Some(WordRef {
            t_src_start: Flicks(w.start),
            t_src_end: Flicks(w.end),
            normalised_text: norm(&w.text),
            prev2: around(k.saturating_sub(2)..k),
            next2: around(k + 1..k + 3),
            word_id: id,
        })
    }

    /// `Some(new)` when the anchor named a word of the old revision that maps.
    fn anchor(&mut self, a: &Anchor, at: &str) -> Option<Anchor> {
        Some(match a {
            Anchor::WordStart(r) => Anchor::WordStart(self.word_ref(r, at)?),
            Anchor::WordEnd(r) => Anchor::WordEnd(self.word_ref(r, at)?),
            Anchor::AudibleStart(r) => Anchor::AudibleStart(self.word_ref(r, at)?),
            Anchor::AudibleEnd(r) => Anchor::AudibleEnd(self.word_ref(r, at)?),
            Anchor::CutPoint(c) => {
                let mut c = c.clone();
                c.word = self.word_ref(&c.word, at)?;
                Anchor::CutPoint(c)
            }
            _ => return None,
        })
    }
}

#[command(
    name = "asr_activate",
    scope = "edit",
    undo = "undoable",
    doc = "Make a transcript revision the active one for a source, moving every word reference (moments, overlays, gain, holds, captions) onto its words; unmatched references are listed.",
    example = r#"{"owner": "01J00000000000000000000009", "revision": "parakeet-tdt-0.6b-v3@abc/w30o2/norm1/stitch2"}"#
)]
impl Command for AsrActivate {
    type Args = AsrActivateArgs;
    type Output = AsrActivated;
    fn run(a: AsrActivateArgs, cx: &mut Cx<'_>) -> Result<AsrActivated, CmdError> {
        let to = cx
            .analysis
            .transcript(&a.owner, &a.revision)
            .cloned()
            .ok_or_else(|| CmdError::invalid("/revision", "no such transcript for this source"))?;
        // a project made without a Project entity (fixtures, early files) gets one here
        let existing = cx.tx.peek().iter::<Project>().next().map(|p| p.id);
        let project = match existing {
            Some(p) => p,
            None => {
                let id: Id<Project> = cx.tx.mint();
                cx.tx.create(Project {
                    id,
                    meta: ProjectMeta {
                        name: String::new(),
                        created_ms: cx.services.now_ms,
                        app_version: env!("CARGO_PKG_VERSION").into(),
                    },
                    active_asr: BTreeMap::new(),
                    profile_ref: None,
                })?;
                id
            }
        };
        let from = cx.tx.get(project)?.active_asr.get(&a.owner).cloned();
        let mut out = AsrActivated {
            owner: a.owner.clone(),
            from: from.clone(),
            to: a.revision.clone(),
            remapped: 0,
            unresolved: vec![],
            overrides_dropped: 0,
        };
        if from.as_deref() == Some(a.revision.as_str()) {
            return Ok(out);
        }
        cx.tx.update(project, |p| {
            p.active_asr.insert(a.owner.clone(), a.revision.clone());
        })?;
        let Some(old) = from
            .as_deref()
            .and_then(|r| cx.analysis.transcript(&a.owner, r))
            .cloned()
        else {
            return Ok(out); // the first transcript: nothing to move
        };
        let mut rm = Remap {
            old_rev: &old.revision,
            to: &to,
            map: realign(&old, &to),
            remapped: 0,
            unresolved: vec![],
        };
        rewrite_anchors(cx, &mut rm)?;
        // caption docs: keys and values naming words
        for cd in cx
            .tx
            .peek()
            .iter::<CaptionDoc>()
            .map(|c| c.id)
            .collect::<Vec<_>>()
        {
            let c = cx.tx.get(cd)?.clone();
            let at = format!("{}", cd.entity());
            let mut edits = BTreeMap::new();
            for (k, v) in &c.edits {
                let nk = rm
                    .word_id(k, &format!("{at} edits"))
                    .unwrap_or_else(|| k.clone());
                edits.insert(nk, v.clone());
            }
            let mut keywords = BTreeMap::new();
            for (k, v) in &c.keywords {
                keywords.insert(
                    k.clone(),
                    rm.word_id(v, &format!("{at} keywords"))
                        .unwrap_or_else(|| v.clone()),
                );
            }
            let mut ranges = BTreeMap::new();
            for (k, r) in &c.no_caption_ranges {
                let from_a = rm
                    .anchor(&r.from, &format!("{at} no_caption"))
                    .unwrap_or_else(|| r.from.clone());
                let to_a = rm
                    .anchor(&r.to, &format!("{at} no_caption"))
                    .unwrap_or_else(|| r.to.clone());
                ranges.insert(
                    k.clone(),
                    NoCaptionRange {
                        from: from_a,
                        to: to_a,
                    },
                );
            }
            let dropped = c
                .word_time_overrides
                .keys()
                .filter(|k| k.asr_revision == old.revision)
                .count();
            out.overrides_dropped += dropped;
            if edits != c.edits
                || keywords != c.keywords
                || ranges != c.no_caption_ranges
                || dropped > 0
            {
                cx.tx.update(cd, |c| {
                    c.edits = edits;
                    c.keywords = keywords;
                    c.no_caption_ranges = ranges;
                    c.word_time_overrides
                        .retain(|k, _| k.asr_revision != old.revision);
                })?;
            }
        }
        out.remapped = rm.remapped;
        out.unresolved = rm.unresolved;
        Ok(out)
    }
}

/// Rewrites the word anchors of every anchor-bearing entity.
fn rewrite_anchors(cx: &mut Cx<'_>, rm: &mut Remap<'_>) -> Result<(), CmdError> {
    macro_rules! each {
        ($kind:ty, |$e:ident, $at:ident| $body:block) => {
            for id in cx.tx.peek().iter::<$kind>().map(|x| x.id).collect::<Vec<_>>() {
                let mut $e = cx.tx.get(id)?.clone();
                let $at = id.entity().to_string();
                let before = $e.clone();
                $body
                if $e != before {
                    cx.tx.update(id, |x| *x = $e)?;
                }
            }
        };
    }
    each!(Moment, |m, at| {
        if let Some(x) = rm.anchor(&m.span.start, &format!("{at} span.start")) {
            m.span.start = x;
        }
        if let Some(x) = rm.anchor(&m.span.end, &format!("{at} span.end")) {
            m.span.end = x;
        }
        if let Some(p) = m.payoff.as_mut() {
            if let Some(x) = rm.anchor(&p.start, &format!("{at} payoff.start")) {
                p.start = x;
            }
            if let Some(x) = rm.anchor(&p.end, &format!("{at} payoff.end")) {
                p.end = x;
            }
        }
    });
    each!(Overlay, |o, at| {
        if let Some(x) = rm.anchor(&o.start, &format!("{at} start")) {
            o.start = x;
        }
        if let OverlayEnd::At(e) = &o.end
            && let Some(x) = rm.anchor(e, &format!("{at} end"))
        {
            o.end = OverlayEnd::At(x);
        }
    });
    each!(GainSegment, |g, at| {
        if let Some(x) = rm.anchor(&g.start, &format!("{at} start")) {
            g.start = x;
        }
        if let Some(x) = rm.anchor(&g.end, &format!("{at} end")) {
            g.end = x;
        }
    });
    each!(ContentHold, |h, at| {
        if let Some(x) = rm.anchor(&h.from, &format!("{at} from")) {
            h.from = x;
        }
        if let Some(x) = rm.anchor(&h.to, &format!("{at} to")) {
            h.to = x;
        }
    });
    each!(HideRoles, |h, at| {
        if let Some(x) = rm.anchor(&h.from, &format!("{at} from")) {
            h.from = x;
        }
        if let Some(x) = rm.anchor(&h.to, &format!("{at} to")) {
            h.to = x;
        }
    });
    each!(StorySuggestion, |s, at| {
        if let Some(x) = rm.anchor(&s.span.start, &format!("{at} span.start")) {
            s.span.start = x;
        }
        if let Some(x) = rm.anchor(&s.span.end, &format!("{at} span.end")) {
            s.span.end = x;
        }
    });
    each!(Topic, |t, at| {
        if let Some(x) = rm.anchor(&t.span.start, &format!("{at} span.start")) {
            t.span.start = x;
        }
        if let Some(x) = rm.anchor(&t.span.end, &format!("{at} span.end")) {
            t.span.end = x;
        }
    });
    each!(FluffProposal, |f, at| {
        if let Some(x) = rm.anchor(&f.span.start, &format!("{at} span.start")) {
            f.span.start = x;
        }
        if let Some(x) = rm.anchor(&f.span.end, &format!("{at} span.end")) {
            f.span.end = x;
        }
    });
    each!(Measurement, |m, at| {
        if let MeasureEnd::Anchor(a) = &m.from
            && let Some(x) = rm.anchor(a, &format!("{at} from"))
        {
            m.from = MeasureEnd::Anchor(x);
        }
        if let Some(MeasureEnd::Anchor(a)) = &m.to
            && let Some(x) = rm.anchor(a, &format!("{at} to"))
        {
            m.to = Some(MeasureEnd::Anchor(x));
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------
// asr_run (A2.02): a job answered by the holder
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AsrRunArgs {
    /// A source, or a sync group (heard once, through its audio member).
    pub source: String,
    /// Hear it again from the start even when a finished transcript exists.
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AsrStarted {
    pub job: String,
    pub state: String,
}

#[derive(Debug)]
pub struct AsrRun;

#[command(
    name = "asr_run",
    scope = "edit",
    exec = "job",
    undo = "undoable",
    since = "M2",
    doc = "Transcribe a source's voice track as a job, window by window; each finished window is kept, so after a cancel or a quit running it again resumes where it stopped. The first transcript becomes the active one. job_wait reports progress and the result.",
    example = r#"{"source": "01J00000000000000000000009"}"#,
    example = r#"{"source": "01J00000000000000000000009", "force": true}"#
)]
impl Command for AsrRun {
    type Args = AsrRunArgs;
    type Output = AsrStarted;
    fn run(_: AsrRunArgs, _: &mut Cx<'_>) -> Result<AsrStarted, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamProcessArgs {
    /// The stream (a source; a member of a sync group is processed as its group).
    pub source: String,
    /// Only these steps of the recipe, in this order (default: all of it).
    #[serde(default)]
    pub steps: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct StreamProcess;

#[command(
    name = "stream_process",
    scope = "edit",
    exec = "job",
    undo = "undoable",
    since = "M2",
    doc = "Process this stream: the recipe as one job, step by step: transcribe (once per sync group, resumable), analyse, find moments and the full sweep (agent passes: they wait for your click), the density gate; delivery and the visual scout come later. job_wait shows each step.",
    example = r#"{"source": "01J00000000000000000000009"}"#,
    example = r#"{"source": "01J00000000000000000000009", "steps": ["transcribe", "density"]}"#
)]
impl Command for StreamProcess {
    type Args = StreamProcessArgs;
    type Output = AsrStarted;
    fn run(_: StreamProcessArgs, _: &mut Cx<'_>) -> Result<AsrStarted, CmdError> {
        Err(holder_only())
    }
}

// ---------------------------------------------------------------------------------------
// measurement_add (holder-internal)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementAddArgs {
    pub kind: MeasurementKind,
    pub from: MeasureEnd,
    #[serde(default)]
    pub to: Option<MeasureEnd>,
    pub value: f64,
    pub unit: String,
    #[serde(default)]
    pub evidence_frames: Vec<gmclip_model::anchor::FrameRef>,
    pub method: String,
    pub author: String,
}

#[derive(Debug)]
pub struct MeasurementAdd;

#[command(
    name = "measurement_add",
    scope = "read",
    undo = "undoable",
    doc = "Record a measurement the holder took (internal: agents measure with the measure tool).",
    example = r#"{"kind": "loudness", "from": {"anchor": {"cut_start": {"cut": "01J00000000000000000000001"}}}, "value": -16.2, "unit": "LUFS", "method": "bs1770", "author": "local:claude-code"}"#
)]
impl Command for MeasurementAdd {
    type Args = MeasurementAddArgs;
    type Output = super::Created;
    fn run(a: MeasurementAddArgs, cx: &mut Cx<'_>) -> Result<super::Created, CmdError> {
        if !a.value.is_finite()
            || a.unit.len() > 16
            || a.method.len() > 200
            || a.author.len() > 120
            || a.evidence_frames.len() > 16
        {
            return Err(CmdError::invalid(
                "/",
                "a finite value; short unit, method and author; ≤ 16 frames",
            ));
        }
        let id: Id<Measurement> = cx.tx.mint();
        cx.tx.create(Measurement {
            id,
            kind: a.kind,
            from: a.from,
            to: a.to,
            value: a.value,
            unit: a.unit,
            evidence_frames: a.evidence_frames,
            method: a.method,
            author: a.author,
        })?;
        Ok(super::created(id))
    }
}

// ---------------------------------------------------------------------------------------
// Sweeps (SPEC §5.5)
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SweepKind {
    Pauses,
    Fillers,
    /// Repeated or abandoned-then-restarted attempts (SPEC §5.5, decision 0040): the
    /// earlier attempt goes.
    Retakes,
}

impl SweepKind {
    pub fn name(self) -> &'static str {
        match self {
            SweepKind::Pauses => "pauses",
            SweepKind::Fillers => "fillers",
            SweepKind::Retakes => "retakes",
        }
    }

    pub fn parse(s: &str) -> Option<SweepKind> {
        match s {
            "pauses" => Some(SweepKind::Pauses),
            "fillers" => Some(SweepKind::Fillers),
            "retakes" => Some(SweepKind::Retakes),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepSpanArg {
    /// The media the span is on (the parts' `media`).
    pub media: MediaRef,
    /// Media-clock flicks.
    pub from: Flicks,
    pub to: Flicks,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepApplyArgs {
    pub cut: Id<Cut>,
    /// A `sweep_preview`'s id: applies exactly the spans it listed, or `Conflict` when
    /// the cut's parts changed since, `StaleAnalysis` when the analysis did (answered by
    /// the holder).
    #[serde(default)]
    pub preview_id: Option<String>,
    /// With `preview_id`: only these of its spans (their indices in the preview).
    #[serde(default)]
    pub only: Option<Vec<usize>>,
    /// Without `preview_id` (holder-internal): the kind, spans and analysis revision.
    #[serde(default)]
    pub kind: Option<SweepKind>,
    #[serde(default)]
    pub spans: Vec<SweepSpanArg>,
    /// The analysis revision the spans come from (recorded with the transaction).
    #[serde(default)]
    pub revision: Option<String>,
    /// The cut's parts as the preview saw them ([`cut_sig`]): `Conflict` when they
    /// changed.
    #[serde(default)]
    pub cut_sig: Option<String>,
}

/// A signature of the parts a cut plays (order, media, source in/out): what a sweep
/// preview was computed on. Equal signatures mean the preview's spans still apply
/// exactly.
pub fn cut_sig(doc: &gmclip_model::Doc, cut: Id<Cut>) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let played: Vec<&Part> = gmclip_model::clock::cut_layout(doc, cut)
        .into_iter()
        .filter_map(|sp| doc.get(sp.part))
        .collect();
    for p in &played {
        p.id.0.to_string().hash(&mut h);
        serde_json::to_string(&p.media)
            .unwrap_or_default()
            .hash(&mut h);
        p.src_in.0.hash(&mut h);
        p.src_out.0.hash(&mut h);
    }
    format!("{}:{:016x}", played.len(), h.finish())
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SweepApplied {
    pub kind: SweepKind,
    pub removed_spans: usize,
    pub parts_changed: usize,
    pub parts_removed: usize,
    /// Fragments under 0.12 s left between removals, dropped (SPEC §5.3).
    pub fragments_dropped: usize,
    pub revision: String,
}

#[derive(Debug)]
pub struct SweepApply;

#[command(
    name = "sweep_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Apply a sweep preview (sweep_preview's preview_id) as one transaction: exactly the spans it listed, Conflict if the cut changed since, StaleAnalysis if the analysis did.",
    example = r#"{"cut": "01J00000000000000000000001", "preview_id": "sp3"}"#,
    example = r#"{"cut": "01J00000000000000000000001", "preview_id": "sp3", "only": [0, 2]}"#,
    example = r#"{"cut": "01J00000000000000000000001", "kind": "pauses", "spans": [{"media": {"source": "01J00000000000000000000009"}, "from": 70560000000, "to": 70913000000}], "revision": "quiet_grid@1/fp"}"#
)]
impl Command for SweepApply {
    type Args = SweepApplyArgs;
    type Output = SweepApplied;
    fn run(a: SweepApplyArgs, cx: &mut Cx<'_>) -> Result<SweepApplied, CmdError> {
        if a.preview_id.is_some() {
            return Err(holder_only());
        }
        let (Some(kind), Some(revision)) = (a.kind, a.revision.clone()) else {
            return Err(CmdError::invalid(
                "/",
                "preview_id, or kind, spans and revision",
            ));
        };
        if a.spans.is_empty() || a.spans.len() > 5_000 {
            return Err(CmdError::invalid("/spans", "1..=5000 spans"));
        }
        super::bounded(&revision, "/revision", 300)?;
        cx.tx.get(a.cut)?;
        if let Some(want) = &a.cut_sig {
            // read the parts (recorded), so a later change conflicts at commit too
            for p in cx.tx.children::<Part>(a.cut.entity()) {
                cx.tx.get(p)?;
            }
            let now = cut_sig(cx.tx.peek(), a.cut);
            if *want != now {
                let e = a.cut.entity();
                let doc = cx.tx.peek();
                return Err(CmdError::Conflict(Box::new(gmclip_proto::Conflict {
                    entities: vec![gmclip_proto::ConflictEntity {
                        entity: e.to_string(),
                        current_rev: doc.index_rev(e, <Part as gmclip_model::Entity>::KIND),
                        by: doc
                            .last_author(e)
                            .map_or_else(|| "system".into(), |a| a.label()),
                    }],
                    fields: None,
                    current: serde_json::json!({"cut_sig": now}),
                    diff: serde_json::json!({"reason": "the cut's parts changed since the sweep preview; preview again"}),
                    options: Some(vec![gmclip_proto::ConflictOption::Cancel]),
                })));
            }
        }
        let (mut changed, mut removed, mut dropped) = (0, 0, 0);
        // from the end backwards, so earlier spans see the parts as the sweep found them
        let mut spans: Vec<&SweepSpanArg> = a.spans.iter().collect();
        spans.sort_by_key(|x| std::cmp::Reverse(x.from));
        for s in spans {
            if s.to <= s.from {
                return Err(CmdError::invalid(
                    "/spans",
                    "each span ends after it starts",
                ));
            }
            let home = a.cut;
            // Part edges live on the output frame grid (the planner refuses others):
            // each span shrinks inward to whole frames of the cut's framing, so a cut
            // never reaches into the words around a pause.
            let fpf = cx
                .tx
                .peek()
                .children::<gmclip_model::entities::Framing>(home.entity())
                .find_map(|f| cx.tx.peek().get(f))
                .and_then(|f| crate::planner::framing_fpf(f.fps));
            let (from, to) = match fpf {
                Some(f) => (
                    gmclip_model::Flicks((s.from.0 + f - 1).div_euclid(f) * f),
                    gmclip_model::Flicks(s.to.0.div_euclid(f) * f),
                ),
                None => (s.from, s.to),
            };
            if to <= from {
                continue;
            }
            // a fragment under 0.12 s left beside the span is dropped (SPEC §5.3) — but
            // never one that holds a word: the span gives way to keep 0.12 s there
            let Some((from, to)) = keep_word_fragments(cx, home, s.media, from, to, fpf)? else {
                continue;
            };
            let (c, r, d) = remove_span(cx, home, from, to, Some(s.media))?;
            changed += c;
            removed += r;
            dropped += d;
        }
        // what the sweep left between its spans that says nothing and is too short to be
        // a beat (under 0.5 s) goes too (decision 0040: a pause sweep after a retakes
        // sweep left dozens of 0.13 s parts of silence)
        let swept: Vec<MediaRef> = a.spans.iter().map(|x| x.media).collect();
        for (media, from, to) in wordless_slivers(cx, a.cut, &swept) {
            let (c, r, _) = remove_span(cx, a.cut, from, to, Some(media))?;
            changed += c;
            removed += r;
            dropped += r;
        }
        Ok(SweepApplied {
            kind,
            removed_spans: a.spans.len(),
            parts_changed: changed,
            parts_removed: removed,
            fragments_dropped: dropped,
            revision,
        })
    }
}

/// Parts of the swept media under 0.5 s in which the media's active transcript has no
/// word (`crate::script::plays_in`: 0.12 s of a word, or half of a shorter one).
fn wordless_slivers(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    media: &[MediaRef],
) -> Vec<(MediaRef, Flicks, Flicks)> {
    const SLIVER: i64 = gmclip_model::time::FLICKS_PER_SECOND / 2;
    let doc = cx.tx.peek();
    let mut out = Vec::new();
    for pid in doc.children::<Part>(cut.entity()) {
        let Some(p) = doc.get(pid) else { continue };
        if !media.contains(&p.media) || p.src_out.0 - p.src_in.0 >= SLIVER {
            continue;
        }
        let owner = match p.media {
            MediaRef::Source(x) => x.0.to_string(),
            MediaRef::Group(x) => x.0.to_string(),
        };
        let Some(t) = cx.analysis.active(doc, &owner) else {
            continue;
        };
        let says = t.words.iter().any(|w| {
            crate::script::plays_in(w.start, w.end, &[(p.src_in.0, p.src_out.0)]).is_some()
        });
        if !says {
            out.push((p.media, p.src_in, p.src_out));
        }
    }
    out
}

/// Shrinks a sweep span so the fragment it would leave beside it in a part (shorter
/// than a part may be, so dropped) keeps any word of `media`'s active transcript:
/// that fragment grows to the minimum part length instead. `None` when nothing is left
/// to cut.
fn keep_word_fragments(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    media: MediaRef,
    mut from: Flicks,
    mut to: Flicks,
    fpf: Option<i64>,
) -> Result<Option<(Flicks, Flicks)>, CmdError> {
    use super::part::MIN_PART;
    let owner = match media {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    };
    let Some(t) = cx.analysis.active(cx.tx.peek(), &owner).cloned() else {
        return Ok(Some((from, to)));
    };
    let has_word = |a: i64, b: i64| {
        let i = t.words.partition_point(|w| w.end <= a);
        t.words.get(i).is_some_and(|w| w.start < b)
    };
    let ceil = |x: i64| fpf.map_or(x, |f| (x + f - 1).div_euclid(f) * f);
    let floor = |x: i64| fpf.map_or(x, |f| x.div_euclid(f) * f);
    for pid in cx.tx.children::<Part>(cut.entity()) {
        let p = cx.tx.get(pid)?.clone();
        if p.media != media || p.src_out <= from || p.src_in >= to {
            continue;
        }
        let head = from.0 - p.src_in.0;
        if head > 0 && head < MIN_PART.0 && has_word(p.src_in.0, from.0) {
            from = Flicks(ceil(p.src_in.0 + MIN_PART.0));
        }
        let tail = p.src_out.0 - to.0;
        if tail > 0 && tail < MIN_PART.0 && has_word(to.0, p.src_out.0) {
            to = Flicks(floor(p.src_out.0 - MIN_PART.0));
        }
    }
    Ok((to > from).then_some((from, to)))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepPausesArgs {
    pub cut: String,
    /// Pauses at least this long go (0.15–5 s, default 0.32).
    #[serde(default)]
    pub min_s: Option<f64>,
    /// Silence kept on each side of a pause inside a part (default 0.10 s); a part's edges keep 40 ms before its first word is heard and 120 ms after its last.
    #[serde(default)]
    pub keep_s: Option<f64>,
    /// Preview only (default true): the list, nothing changed.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepFillersArgs {
    pub cut: String,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// `sweep_retakes` takes what `sweep_fillers` takes.
pub type SweepRetakesArgs = SweepFillersArgs;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SweepPreview {
    pub kind: SweepKind,
    pub cuts: Vec<serde_json::Value>,
    pub skipped: Vec<serde_json::Value>,
    pub applied: Option<SweepApplied>,
}

#[derive(Debug)]
pub struct SweepPauses;

#[command(
    name = "sweep_pauses",
    scope = "edit",
    undo = "undoable",
    doc = "Tighten a cut: remove pauses of at least min_s seconds (keeping keep_s of silence), never a word; previews by default (dry_run), applies as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "min_s": 0.4, "dry_run": true}"#
)]
impl Command for SweepPauses {
    type Args = SweepPausesArgs;
    type Output = SweepPreview;
    fn run(_: SweepPausesArgs, _: &mut Cx<'_>) -> Result<SweepPreview, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug)]
pub struct SweepFillers;

#[command(
    name = "sweep_fillers",
    scope = "edit",
    undo = "undoable",
    doc = "Remove filler words (um, uh, erm…) from a cut, anchored on the quiet grid, fused ones skipped; previews by default (dry_run), applies as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "dry_run": true}"#
)]
impl Command for SweepFillers {
    type Args = SweepFillersArgs;
    type Output = SweepPreview;
    fn run(_: SweepFillersArgs, _: &mut Cx<'_>) -> Result<SweepPreview, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug)]
pub struct SweepRetakes;

#[command(
    name = "sweep_retakes",
    scope = "edit",
    undo = "undoable",
    doc = "Remove retakes from a cut (SPEC §5.5): an attempt said again word for word, nearly the same, or broken off and begun again within 20 s loses its earlier attempt, never the one kept; across parts and videos, on the words the cut plays now. Previews by default (dry_run) listing each earlier and later attempt; applies as one transaction. Read cut_get{projection: words} after.",
    example = r#"{"cut": "01J00000000000000000000001", "dry_run": true}"#
)]
impl Command for SweepRetakes {
    type Args = SweepRetakesArgs;
    type Output = SweepPreview;
    fn run(_: SweepRetakesArgs, _: &mut Cx<'_>) -> Result<SweepPreview, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepPreviewArgs {
    pub cut: String,
    pub kind: SweepKind,
    /// Pauses: at least this long go (0.15–5 s, default 0.32).
    #[serde(default)]
    pub min_s: Option<f64>,
    /// Pauses: silence kept on each side (default 0.10 s).
    #[serde(default)]
    pub keep_s: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SweepPreviewed {
    /// Pass to `sweep_apply{preview_id}` to apply exactly these spans.
    pub preview_id: String,
    pub kind: SweepKind,
    pub cuts: Vec<serde_json::Value>,
    pub skipped: Vec<serde_json::Value>,
    pub revision: String,
}

#[derive(Debug)]
pub struct SweepPreviewCmd;

#[command(
    name = "sweep_preview",
    scope = "edit",
    undo = "read_only",
    since = "M2",
    doc = "Preview a sweep of a cut (pauses of at least min_s, or fillers): the spans it would remove, the skipped ones, and a preview_id that sweep_apply applies exactly, as one transaction.",
    example = r#"{"cut": "01J00000000000000000000001", "kind": "pauses", "min_s": 0.4}"#,
    example = r#"{"cut": "01J00000000000000000000001", "kind": "fillers"}"#
)]
impl Command for SweepPreviewCmd {
    type Args = SweepPreviewArgs;
    type Output = SweepPreviewed;
    fn run(_: SweepPreviewArgs, _: &mut Cx<'_>) -> Result<SweepPreviewed, CmdError> {
        Err(holder_only())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAnalyzeArgs {
    pub source: String,
    /// Run ASR again even when a transcript exists.
    #[serde(default)]
    pub transcribe: Option<bool>,
}

#[derive(Debug)]
pub struct SourceAnalyze;

#[command(
    name = "source_analyze",
    scope = "edit",
    undo = "read_only",
    doc = "Analyse a source in the background: waveform, energy, quiet grid, voice activity and a transcript of the voice track (a job; wait with job_wait).",
    example = r#"{"source": "01J00000000000000000000009"}"#
)]
impl Command for SourceAnalyze {
    type Args = SourceAnalyzeArgs;
    type Output = Done;
    fn run(_: SourceAnalyzeArgs, _: &mut Cx<'_>) -> Result<Done, CmdError> {
        Err(holder_only())
    }
}
