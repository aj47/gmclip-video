//! Anchor resolution on the server (R-ANC-1/2, A15 [M1 subset]).
//!
//! Every anchor an agent sends is resolved here and answered with its value **and the
//! method** that produced it, plus the time's provenance (`raw` and non-zero agent-typed
//! `offset_ms` are `raw` and linted; frames are `grounded`; measurements `measured`).
//!
//! At M1, before gmclip's own ASR and analysis (A16, lane D):
//! - word anchors resolve from the word fingerprint (`t_src_start/end`);
//! - `{cut_point}` resolves to the word edge on its side; the energy-minimum and arbiter
//!   rules (R-ANC-2) run when analysis lands, and the method says so;
//! - `{raw}` cannot snap to `nearest_word` yet (no transcript in the project);
//! - `{frame}` needs the source's frame rate (its probe); lines and visual events need a
//!   transcript or analysis and are refused with that reason.

use gmclip_model::anchor::{Anchor, CutSide, TimeProvenance};
use gmclip_model::clock;
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analysis::AnalysisView;

const SEC: f64 = 705_600_000.0;

/// A resolved anchor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resolved {
    /// Source time, seconds (3 dp).
    pub src_s: f64,
    /// Cut-clock time, seconds, when the anchor lies inside one of the cut's parts.
    pub cut_s: Option<f64>,
    /// How it was resolved (`word_fingerprint`, `part_edge`, `cut_edge`, `raw`, `frame`,
    /// `measured`, `cut_point_word_edge`).
    pub method: String,
    pub provenance: TimeProvenance,
    /// Exact source time in flicks.
    #[serde(skip)]
    pub src: Flicks,
    #[serde(skip)]
    pub cut_t: Option<Flicks>,
}

fn secs(f: Flicks) -> f64 {
    (f.0 as f64 / SEC * 1000.0).round() / 1000.0
}

fn unresolvable(path: &str, why: &'static str) -> CmdError {
    CmdError::invalid(path, why)
}

/// A source frame's presentation time, from the source's probed frame rate (CFR).
fn frame_time(doc: &Doc, f: &gmclip_model::anchor::FrameRef) -> Option<Flicks> {
    let s = doc.get(f.source)?;
    let fps = s.probe.tracks.iter().find_map(|t| t.fps)?;
    if fps.num <= 0 || f.frame_index < 0 {
        return None;
    }
    // t = index · den / num seconds, in flicks, nearest
    let fl = i128::from(f.frame_index) * i128::from(fps.den) * 705_600_000;
    i64::try_from((fl + i128::from(fps.num) / 2) / i128::from(fps.num))
        .ok()
        .map(Flicks)
}

/// Where `t_src` falls on the cut clock: the first part, in order, that plays it — of
/// `source` when the time is known to belong to one.
pub fn to_cut(
    doc: &Doc,
    cut: Id<Cut>,
    t_src: Flicks,
    source: Option<Id<Source>>,
) -> Option<Flicks> {
    to_cut_in(doc, cut, t_src, source, None)
}

/// [`to_cut`] on the clock of `group` when it is named (a raw time stored on a sync
/// group's clock): only that group's parts play it.
pub fn to_cut_in(
    doc: &Doc,
    cut: Id<Cut>,
    t_src: Flicks,
    source: Option<Id<Source>>,
    group: Option<Id<gmclip_model::entities::SyncGroup>>,
) -> Option<Flicks> {
    clock::cut_layout(doc, cut).into_iter().find_map(|span| {
        let p = doc.get(span.part)?;
        let t = match (source, p.media) {
            (None, MediaRef::Group(g)) if group.is_some_and(|x| x != g) => return None,
            (None, MediaRef::Source(_)) if group.is_some() => return None,
            (None, _) => t_src,
            (Some(s), MediaRef::Source(x)) if x == s => t_src,
            // a member of the part's sync group: its time on the group's clock (member
            // time = group time − offset, per segment)
            (Some(s), MediaRef::Group(g)) => member_to_group(doc, g, s, t_src)?,
            _ => return None,
        };
        clock::src_to_cut(doc, span.part, t)
    })
}

/// A sync-group member's time on the group's clock (`None` when `s` is not a member or
/// no segment holds it).
pub(crate) fn member_to_group(
    doc: &Doc,
    g: Id<gmclip_model::entities::SyncGroup>,
    s: Id<gmclip_model::entities::Source>,
    t: Flicks,
) -> Option<Flicks> {
    use gmclip_model::entities::{SyncMember, SyncSegment};
    let m = doc
        .children::<SyncMember>(g.entity())
        .filter_map(|m| doc.get(m))
        .find(|m| m.source == s)?;
    doc.children::<SyncSegment>(m.id.entity())
        .filter_map(|x| doc.get(x))
        .find_map(|x| {
            let gt = Flicks(t.0 + x.offset.0);
            (x.src_range.start <= gt && gt < x.src_range.end).then_some(gt)
        })
}

/// Offsets are bounded (±24 h): beyond that a time is an error, not an overflow.
const MAX_OFFSET_MS: i64 = 24 * 3600 * 1000;

fn offset(path: &str, o: Option<i64>) -> Result<i64, CmdError> {
    let o = o.unwrap_or(0);
    if o.abs() > MAX_OFFSET_MS {
        return Err(CmdError::invalid(path, "offset_ms within ±24 h"));
    }
    Ok(clock::ms_to_flicks(o).0)
}

/// Resolves `a` (for `cut`, when given), reporting at `path` what cannot be resolved.
pub fn resolve(
    doc: &Doc,
    cut: Option<Id<Cut>>,
    a: &Anchor,
    path: &str,
) -> Result<Resolved, CmdError> {
    resolve_depth(doc, None, cut, a, path, 0)
}

/// [`resolve`] with the project's transcripts: word anchors resolve against the real
/// words (R-ANC-1) once the project has any.
pub fn resolve_in(
    doc: &Doc,
    view: Option<&AnalysisView>,
    cut: Option<Id<Cut>>,
    a: &Anchor,
    path: &str,
) -> Result<Resolved, CmdError> {
    resolve_depth(doc, view, cut, a, path, 0)
}

/// Words exist to resolve against.
fn has_words(view: Option<&AnalysisView>) -> bool {
    view.is_some_and(|v| !v.keys().is_empty())
}

/// The stored word a reference names, checked against its fingerprint.
fn stored_word<'v>(
    view: &'v AnalysisView,
    w: &gmclip_model::anchor::WordRef,
    path: &str,
) -> Result<(&'v str, &'v crate::analysis::Word), CmdError> {
    let (owner, word) = view.word(w).ok_or_else(|| {
        unresolvable(
            path,
            "no such word in the project's transcripts (read it with transcript_read)",
        )
    })?;
    if (word.start - w.t_src_start.0).abs() > SEC_FLICKS / 4 {
        return Err(unresolvable(
            path,
            "the word's times do not match its id (re-read the transcript)",
        ));
    }
    Ok((owner, word))
}

const SEC_FLICKS: i64 = 705_600_000;

fn owner_source(owner: &str) -> Option<Id<Source>> {
    gmclip_model::Ulid::parse(owner).ok().map(Id::new)
}

fn resolve_depth(
    doc: &Doc,
    view: Option<&AnalysisView>,
    cut: Option<Id<Cut>>,
    a: &Anchor,
    path: &str,
    depth: u8,
) -> Result<Resolved, CmdError> {
    let offset_raw = |o: Option<i64>| {
        if o.unwrap_or(0) != 0 {
            TimeProvenance::Raw
        } else {
            TimeProvenance::Anchored
        }
    };
    let mut source: Option<Id<Source>> = None;
    let mut group: Option<Id<gmclip_model::entities::SyncGroup>> = None;
    let (method, provenance, src, direct_cut): (
        &str,
        TimeProvenance,
        Option<Flicks>,
        Option<Flicks>,
    ) = match a {
        // With transcripts: the stored word's times (its fingerprint must agree)
        Anchor::WordStart(w)
        | Anchor::WordEnd(w)
        | Anchor::AudibleStart(w)
        | Anchor::AudibleEnd(w)
            if has_words(view) =>
        {
            let (owner, word) = stored_word(view.unwrap_or_else(|| unreachable!()), w, path)?;
            source = owner_source(owner);
            let (t, method) = match a {
                Anchor::WordStart(_) => (word.start, "word"),
                Anchor::WordEnd(_) => (word.end, "word"),
                Anchor::AudibleStart(_) => {
                    (word.start, "word (audible edge: measure{edge} refines it)")
                }
                _ => (word.end, "word (audible edge: measure{edge} refines it)"),
            };
            (method, TimeProvenance::Anchored, Some(Flicks(t)), None)
        }
        // An edge measured for this very cut point (energy-min or the arbiter, R-ANC-2)
        Anchor::CutPoint(_) if measured_cut_point(doc, a).is_some() => {
            let (m, t) = measured_cut_point(doc, a).unwrap_or_else(|| unreachable!());
            ("measured cut point", TimeProvenance::Measured, Some(t), m)
        }
        Anchor::CutPoint(c) if has_words(view) => {
            let (owner, word) = stored_word(view.unwrap_or_else(|| unreachable!()), &c.word, path)?;
            source = owner_source(owner);
            let t = match c.side {
                CutSide::Before => word.start,
                CutSide::After => word.end,
            };
            (
                "cut_point_word_edge (measure{edge} on this cut point gives the energy-min/arbiter edge)",
                TimeProvenance::Anchored,
                Some(Flicks(t)),
                None,
            )
        }
        // No transcript in the project: the fingerprint time is the caller's, `raw`
        Anchor::WordStart(_)
        | Anchor::WordEnd(_)
        | Anchor::AudibleStart(_)
        | Anchor::AudibleEnd(_) => (
            "word_fingerprint (unverified: no transcript yet)",
            TimeProvenance::Raw,
            clock::anchor_src(doc, a),
            None,
        ),
        Anchor::CutPoint(c) => (
            match c.side {
                CutSide::Before | CutSide::After => {
                    "cut_point_word_edge (unverified; the energy-min/arbiter rule waits for analysis)"
                }
            },
            TimeProvenance::Raw,
            clock::anchor_src(doc, a),
            None,
        ),
        Anchor::PartStart(e) | Anchor::PartEnd(e) => {
            let p = doc.get(e.part).ok_or(CmdError::NotFound)?;
            if cut.is_some_and(|c| c != p.cut) {
                return Err(unresolvable(path, "a part of another cut"));
            }
            let off = offset(path, e.offset_ms)?;
            let span = clock::cut_layout(doc, p.cut)
                .into_iter()
                .find(|s| s.part == e.part);
            let cut_t = span.and_then(|s| {
                let base = if matches!(a, Anchor::PartStart(_)) {
                    Some(s.start.0)
                } else {
                    s.start.0.checked_add(s.dur.0)
                };
                base?.checked_add(off).map(Flicks)
            });
            let edge = if matches!(a, Anchor::PartStart(_)) {
                p.src_in.0
            } else {
                p.src_out.0
            };
            if let MediaRef::Source(s) = p.media {
                source = Some(s);
            }
            (
                "part_edge",
                offset_raw(e.offset_ms),
                edge.checked_add(off).map(Flicks),
                cut_t,
            )
        }
        Anchor::CutStart(e) | Anchor::CutEnd(e) => {
            if cut.is_some_and(|c| c != e.cut) {
                return Err(unresolvable(path, "another cut's edge"));
            }
            doc.get(e.cut).ok_or(CmdError::NotFound)?;
            let off = offset(path, e.offset_ms)?;
            let total: i64 = clock::cut_layout(doc, e.cut).iter().map(|s| s.dur.0).sum();
            let cut_t = if matches!(a, Anchor::CutStart(_)) {
                Some(off)
            } else {
                total.checked_add(off)
            };
            let src = cut_t.and_then(|t| clock::cut_to_src(doc, e.cut, Flicks(t)).map(|(_, s)| s));
            ("cut_edge", offset_raw(e.offset_ms), src, cut_t.map(Flicks))
        }
        Anchor::Raw(r) => {
            if r.reason.trim().is_empty() || r.reason.chars().count() > 200 {
                return Err(unresolvable(
                    path,
                    "a raw time needs a reason (≤ 200 characters)",
                ));
            }
            let direct = match &r.time {
                gmclip_model::clock::ModelTime::Cut { cut: c, t }
                    if cut.is_none_or(|x| x == *c) =>
                {
                    Some(*t)
                }
                // a time on one video's (or sync group's) clock is played only by that
                // video's parts, not by another video that has the same time
                gmclip_model::clock::ModelTime::Src {
                    source: s,
                    group: g,
                    ..
                } => {
                    source = *s;
                    group = *g;
                    None
                }
                _ => None,
            };
            (
                "raw (no transcript yet for nearest_word)",
                TimeProvenance::Raw,
                clock::anchor_src(doc, a),
                direct,
            )
        }
        Anchor::Frame(f) => {
            let t = frame_time(doc, f)
                .ok_or_else(|| unresolvable(path, "the source has no probed frame rate yet"))?;
            source = Some(f.source);
            ("frame", TimeProvenance::Grounded, Some(t), None)
        }
        Anchor::Measured(m) => {
            let m = doc.get(*m).ok_or(CmdError::NotFound)?;
            if depth > 2 {
                return Err(unresolvable(path, "measurement chain too deep"));
            }
            let inner = match &m.from {
                // an edge measurement's value is the time itself
                MeasureEnd::Anchor(_) if m.kind == MeasurementKind::Edge => {
                    let t = Flicks((m.value * SEC_FLICKS as f64).round() as i64);
                    Resolved {
                        src_s: secs(t),
                        cut_s: None,
                        method: String::new(),
                        provenance: TimeProvenance::Measured,
                        src: t,
                        cut_t: None,
                    }
                }
                MeasureEnd::Anchor(x) => resolve_depth(doc, view, cut, x, path, depth + 1)?,
                MeasureEnd::Frame(f) => {
                    let t = frame_time(doc, f).ok_or_else(|| {
                        unresolvable(path, "the measurement's source has no frame rate")
                    })?;
                    source = Some(f.source);
                    Resolved {
                        src_s: secs(t),
                        cut_s: None,
                        method: String::new(),
                        provenance: TimeProvenance::Measured,
                        src: t,
                        cut_t: None,
                    }
                }
                MeasureEnd::Event(_) => {
                    return Err(unresolvable(path, "visual events need analysis (A15/A16)"));
                }
            };
            (
                "measured",
                TimeProvenance::Measured,
                Some(inner.src),
                inner.cut_t,
            )
        }
        Anchor::LineStart(_) | Anchor::LineEnd(_) => {
            return Err(unresolvable(
                path,
                "line anchors need a transcript (gmclip ASR, A16)",
            ));
        }
        Anchor::VisualEvent(_) => {
            return Err(unresolvable(path, "visual events need analysis (A15/A16)"));
        }
    };
    let src = src.ok_or_else(|| unresolvable(path, "does not resolve in this cut"))?;
    let cut_t = direct_cut.or_else(|| cut.and_then(|c| to_cut_in(doc, c, src, source, group)));
    Ok(Resolved {
        src_s: secs(src),
        cut_s: cut_t.map(secs),
        method: method.to_owned(),
        provenance,
        src,
        cut_t,
    })
}

/// Resolves a `[from, to)` span: both ends must land inside the cut, `from` first.
pub fn span(
    doc: &Doc,
    cut: Id<Cut>,
    from: &Anchor,
    to: &Anchor,
    path: &str,
) -> Result<(Resolved, Resolved), CmdError> {
    span_in(doc, None, cut, from, to, path)
}

/// [`span`] with the project's transcripts.
pub fn span_in(
    doc: &Doc,
    view: Option<&AnalysisView>,
    cut: Id<Cut>,
    from: &Anchor,
    to: &Anchor,
    path: &str,
) -> Result<(Resolved, Resolved), CmdError> {
    let a = resolve_in(doc, view, Some(cut), from, &format!("{path}/from"))?;
    let b = resolve_in(doc, view, Some(cut), to, &format!("{path}/to"))?;
    let (Some(x), Some(y)) = (a.cut_t, b.cut_t) else {
        return Err(CmdError::invalid(
            path,
            "an end of the span is outside the cut",
        ));
    };
    if x >= y {
        return Err(CmdError::invalid(path, "the span ends before it starts"));
    }
    Ok((a, b))
}

/// Every anchor-bearing entity of a cut, for lint and `cut_get`: `(entity, field, anchor)`.
pub fn anchors_of_cut(
    doc: &Doc,
    cut: Id<Cut>,
) -> Vec<(gmclip_model::EntityId, &'static str, Anchor)> {
    let mut v = Vec::new();
    let e = cut.entity();
    for id in doc.children::<GainSegment>(e) {
        if let Some(g) = doc.get(id) {
            v.push((id.entity(), "start", g.start.clone()));
            v.push((id.entity(), "end", g.end.clone()));
        }
    }
    for id in doc.children::<ContentHold>(e) {
        if let Some(h) = doc.get(id) {
            v.push((id.entity(), "from", h.from.clone()));
            v.push((id.entity(), "to", h.to.clone()));
        }
    }
    for id in doc.children::<HideRoles>(e) {
        if let Some(h) = doc.get(id) {
            v.push((id.entity(), "from", h.from.clone()));
            v.push((id.entity(), "to", h.to.clone()));
        }
    }
    let clip_overlays: Vec<Id<Overlay>> = doc
        .children::<Clip>(e)
        .flat_map(|c| doc.children::<Overlay>(c.entity()).collect::<Vec<_>>())
        .collect();
    for id in doc.children::<Overlay>(e).chain(clip_overlays) {
        if let Some(o) = doc.get(id) {
            v.push((id.entity(), "start", o.start.clone()));
            if let OverlayEnd::At(x) = &o.end {
                v.push((id.entity(), "end", x.clone()));
            }
        }
    }
    for id in doc.children::<CaptionDoc>(e) {
        if let Some(c) = doc.get(id) {
            for r in c.no_caption_ranges.values() {
                v.push((id.entity(), "no_caption_ranges", r.from.clone()));
                v.push((id.entity(), "no_caption_ranges", r.to.clone()));
            }
        }
    }
    v
}

/// Is this anchor agent-typed time (R-ANC-1): `raw`, a non-zero `offset_ms`, or (at M1,
/// with no transcript to check it) a word's fingerprint time?
pub fn is_typed_time(a: &Anchor) -> bool {
    is_typed_time_in(None, a)
}

/// [`is_typed_time`] knowing whether the project has transcripts: with words, a word
/// anchor is verified against them (not typed).
pub fn is_typed_time_in(view: Option<&AnalysisView>, a: &Anchor) -> bool {
    let words = has_words(view);
    match a {
        Anchor::Raw(_) => true,
        Anchor::WordStart(_)
        | Anchor::WordEnd(_)
        | Anchor::AudibleStart(_)
        | Anchor::AudibleEnd(_)
        | Anchor::CutPoint(_) => !words,
        Anchor::PartStart(e) | Anchor::PartEnd(e) => e.offset_ms.unwrap_or(0) != 0,
        Anchor::CutStart(e) | Anchor::CutEnd(e) => e.offset_ms.unwrap_or(0) != 0,
        _ => false,
    }
}

/// The edge measured for exactly this cut-point anchor (`measure{edge}` with the
/// energy-min or arbiter rule): `(cut time, source time)`.
fn measured_cut_point(doc: &Doc, a: &Anchor) -> Option<(Option<Flicks>, Flicks)> {
    doc.iter::<Measurement>()
        .filter(|m| {
            m.kind == MeasurementKind::Edge && matches!(&m.from, MeasureEnd::Anchor(x) if x == a)
        })
        .last()
        .map(|m| (None, Flicks((m.value * SEC_FLICKS as f64).round() as i64)))
}
