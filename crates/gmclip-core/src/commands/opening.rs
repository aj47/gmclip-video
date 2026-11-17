//! Three openings, one short (decision 0024): up to three alternate openings of a cut,
//! each a sibling **version** that swaps the cut's opening for its own spans and shares
//! everything after it.
//!
//! - **The opening** of a cut is its leading hook parts (role `hook`, or split from the
//!   hook: `from: hook`), at least the first part. The cut itself is the **main
//!   version**; each [`Opening`] is an alternate. Nothing is copied: a version is the
//!   cut with its opening swapped ([`version_doc`], computed on demand and never
//!   committed), so an edit after the opening is in every version at once.
//! - `opening_add` takes a moment's span (another hook take), a story suggestion's span,
//!   or a span picked by hand; `opening_choose` (the human only) makes an alternate the
//!   main version in one transaction: its spans become the cut's opening (the first
//!   opening part keeps its id, crops and layout), and the old opening is kept as an
//!   alternate (`was_main`), so choosing back is the same gesture.
//! - An alternate may carry its own text hook (`hook_text`): its version shows it; a hook
//!   whose end named a word of the old opening ends with the new opening instead.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::anchor::{Anchor, PartEdge};
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::staging::Staging;
use gmclip_model::{Author, Doc, Flicks, Id, ModelError, Ulid};
use gmclip_proto::{CallerKind, CmdError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Created, Done, bounded, created};
use crate::analysis::AnalysisView;
use crate::command::{Command, Cx, model_err};

const F: i64 = 705_600_000;

/// Alternate openings a cut keeps (AJ: "up to three").
pub const MAX_OPENINGS: usize = 3;
/// An opening is at least a part long and at most this long (2 minutes).
const MAX_OPENING: i64 = 120 * F;
/// The yellow `*marked*` words of a hook take (the reference's keyword colour).
pub const HOOK_ACCENT: &str = "#FFD400FF";

/// The cut's opening parts: its leading parts that are the hook (role `hook`, or a piece
/// split off it), at least the first part; empty for a cut with no parts.
pub fn opening_parts(doc: &Doc, cut: Id<Cut>) -> Vec<Id<Part>> {
    let is_hook = |p: &Part| p.role == PartRole::Hook || p.from == PartFrom::Hook;
    let mut parts = doc
        .children::<Part>(cut.entity())
        .filter_map(|id| doc.get(id));
    let Some(first) = parts.next() else {
        return Vec::new();
    };
    let mut out = vec![first.id];
    // the first part opens the cut whatever it is; the parts after it only while they
    // are the hook (a swept hook is `hook`, `hook-b`, … all `from: hook`)
    if is_hook(first) {
        out.extend(parts.take_while(|p| is_hook(p)).map(|p| p.id));
    }
    out
}

/// The spans of the cut's own opening.
pub fn main_spans(doc: &Doc, cut: Id<Cut>) -> Vec<OpeningSpan> {
    opening_parts(doc, cut)
        .into_iter()
        .filter_map(|id| doc.get(id))
        .map(|p| OpeningSpan {
            media: p.media,
            src_in: p.src_in,
            src_out: p.src_out,
        })
        .collect()
}

fn media_key(m: MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// A version's identity: its opening spans (media and source range), stable across
/// renames and choosing; what an export records so retention can be told per opening.
pub fn spans_sig(spans: &[OpeningSpan]) -> String {
    spans
        .iter()
        .map(|s| format!("{}@{}-{}", media_key(s.media), s.src_in.0, s.src_out.0))
        .collect::<Vec<_>>()
        .join(";")
}

fn spans_ms(spans: &[OpeningSpan]) -> i64 {
    spans
        .iter()
        .map(|s| (s.src_out.0 - s.src_in.0) * 1000 / F)
        .sum()
}

/// A hook line's runs: wrapped at ~22 characters a line (the reference's hook width), `*words*` in
/// the keyword yellow.
pub fn hook_runs(text: &str) -> Vec<TextRun> {
    fn split(t: &str, max: usize) -> Vec<String> {
        if t.chars().count() <= max || !t.contains(' ') {
            return vec![t.to_owned()];
        }
        let mid = t.chars().count() / 2;
        let at = t
            .char_indices()
            .filter(|(_, c)| *c == ' ')
            .min_by_key(|(i, _)| t[..*i].chars().count().abs_diff(mid))
            .map_or(0, |(i, _)| i);
        let (a, b) = (t[..at].trim(), t[at..].trim());
        let mut v = split(a, max);
        v.extend(split(b, max));
        v
    }
    let wrapped = split(text.trim(), 22).join("\n");
    wrapped
        .split('*')
        .enumerate()
        .filter(|(_, p)| !p.is_empty())
        .map(|(i, p)| TextRun {
            text: p.to_owned(),
            colour: (i % 2 == 1).then(|| HOOK_ACCENT.to_owned()),
            keyword: None,
        })
        .collect()
}

/// The text the cut's hook shows now (`*marked*` words restored), if it has one.
pub fn hook_text_of(doc: &Doc, cut: Id<Cut>) -> Option<String> {
    let o = doc
        .children::<Overlay>(cut.entity())
        .filter_map(|id| doc.get(id))
        .find(|o| o.role.as_deref() == Some("text_hook"))?;
    let t = o.text.as_ref()?;
    let s: String = t
        .runs
        .iter()
        .map(|r| {
            if r.colour.is_some() {
                format!("*{}*", r.text)
            } else {
                r.text.clone()
            }
        })
        .collect();
    Some(s.replace('\n', " "))
}

/// Swaps the cut's opening for `spans` inside `s`: the first opening part keeps its id
/// (and its look while the picture is the same size), further spans become hook parts
/// after it, opening parts left over are deleted. With `hook_text` the cut's text hook
/// shows it; a hook whose end no longer falls inside the cut ends with the opening.
pub fn swap_opening(
    s: &mut Staging<'_>,
    cut: Id<Cut>,
    spans: &[OpeningSpan],
    hook_text: Option<&str>,
) -> Result<(), ModelError> {
    if spans.is_empty() {
        return Err(ModelError::Invalid("an opening has at least one span"));
    }
    let old = opening_parts(s.doc(), cut);
    let Some(&first) = old.first() else {
        return Err(ModelError::Invalid("the cut has no parts"));
    };
    let model = s.get(first).ok_or(ModelError::NotFound)?.clone();
    let mut prev: Option<Id<Part>> = None;
    let mut kept = Vec::new();
    for (i, sp) in spans.iter().enumerate() {
        let same_picture = |doc: &Doc, m: MediaRef| {
            m == model.media
                || super::part::picture_size(doc, m) == super::part::picture_size(doc, model.media)
        };
        let look = if same_picture(s.doc(), sp.media) {
            model.look.clone()
        } else {
            BTreeMap::new()
        };
        match old.get(i) {
            Some(&pid) => {
                let media_changed = s.get(pid).is_some_and(|p| p.media != sp.media);
                let keep_look = same_picture(s.doc(), sp.media);
                s.update(pid, |p| {
                    p.media = sp.media;
                    p.src_in = sp.src_in;
                    p.src_out = sp.src_out;
                    if media_changed {
                        p.audio_from = None;
                    }
                    if !keep_look {
                        p.look = BTreeMap::new();
                    }
                })?;
                prev = Some(pid);
                kept.push(pid);
            }
            None => {
                let order = edit::key_after(s, cut, prev, None)?;
                let id: Id<Part> = s.mint();
                s.create(Part {
                    id,
                    cut,
                    order,
                    media: sp.media,
                    src_in: sp.src_in,
                    src_out: sp.src_out,
                    audio_from: None,
                    role: PartRole::Hook,
                    from: PartFrom::Hook,
                    lineage: PartLineage::default(),
                    gain_db: model.gain_db,
                    speed: 1.0,
                    ramp: None,
                    part_group: None,
                    under_db: None,
                    look,
                    caption_place_override: model.caption_place_override.clone(),
                })?;
                prev = Some(id);
                kept.push(id);
            }
        }
    }
    for &pid in old.iter().skip(spans.len()) {
        edit::delete_part(s, pid)?;
    }
    // the text hook: this version's words, and an end inside the cut
    let last = *kept.last().ok_or(ModelError::NotFound)?;
    let hooks: Vec<Id<Overlay>> = s
        .doc()
        .children::<Overlay>(cut.entity())
        .filter(|id| {
            s.get(*id)
                .is_some_and(|o| o.role.as_deref() == Some("text_hook"))
        })
        .collect();
    for h in hooks {
        let end_inside = match &s.get(h).ok_or(ModelError::NotFound)?.end {
            OverlayEnd::At(a) => crate::anchors::resolve(s.doc(), Some(cut), a, "/")
                .ok()
                .is_some_and(|r| r.cut_t.is_some()),
            OverlayEnd::DurMs(_) => true,
        };
        let runs = hook_text.map(hook_runs);
        s.update(h, |o| {
            if !end_inside {
                o.end = OverlayEnd::At(Anchor::PartEnd(PartEdge {
                    part: last,
                    offset_ms: None,
                }));
            }
            if let (Some(r), Some(t)) = (&runs, o.text.as_mut()) {
                t.runs = r.clone();
                // a number typed for another take is not this one's
                o.number_provenance.clear();
            }
        })?;
    }
    Ok(())
}

/// The document with alternate `opening` swapped in, nothing committed: the version a
/// preview plays, an export renders and retention is measured on.
pub fn version_doc(doc: &Doc, opening: Id<Opening>) -> Result<Doc, CmdError> {
    let o = doc.get(opening).ok_or(CmdError::NotFound)?.clone();
    // ids for the parts this version adds, derived from the opening's (deterministic,
    // never stored)
    let mut n: u128 = 0;
    let base = opening.0.0;
    let mut mint = move || {
        n += 1;
        Ulid(base ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835) >> 48))
    };
    let mut s = Staging::new(doc.clone(), Author::System, &mut mint);
    swap_opening(&mut s, o.cut, &o.spans, o.hook_text.as_deref()).map_err(model_err)?;
    Ok(s.into_work())
}

/// The words a span says (the media's active transcript), for showing a version.
fn quote(doc: &Doc, view: &AnalysisView, spans: &[OpeningSpan]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for sp in spans {
        let Some(t) = view.active(doc, &media_key(sp.media)) else {
            continue;
        };
        let i = t.words.partition_point(|w| w.end <= sp.src_in.0);
        for w in &t.words[i..] {
            if w.start >= sp.src_out.0 {
                break;
            }
            out.push(w.text.as_str());
        }
    }
    let s = out.join(" ");
    if s.chars().count() > 400 {
        let mut c: String = s.chars().take(400).collect();
        c.push('…');
        c
    } else {
        s
    }
}

fn check_spans(doc: &Doc, spans: &[OpeningSpan]) -> Result<(), CmdError> {
    if spans.is_empty() || spans.len() > 8 {
        return Err(CmdError::invalid("/spans", "one to eight spans"));
    }
    for sp in spans {
        match sp.media {
            MediaRef::Source(x) => doc.get(x).map(|_| ()),
            MediaRef::Group(g) => doc.get(g).map(|_| ()),
        }
        .ok_or(CmdError::NotFound)?;
        if sp.src_in.0 < 0 || sp.src_out.0 - sp.src_in.0 < super::part::MIN_PART.0 {
            return Err(CmdError::invalid(
                "/spans",
                "each span is at least 0.12 s and starts at ≥ 0",
            ));
        }
    }
    let total: i64 = spans.iter().map(|s| s.src_out.0 - s.src_in.0).sum();
    if total > MAX_OPENING {
        return Err(CmdError::invalid(
            "/spans",
            "an opening runs at most 2 minutes",
        ));
    }
    Ok(())
}

fn check_hook_text(t: &Option<String>) -> Result<(), CmdError> {
    if let Some(t) = t {
        bounded(t, "/hook_text", 120)?;
        if t.chars().any(char::is_control) {
            return Err(CmdError::invalid("/hook_text", "one line of text"));
        }
    }
    Ok(())
}

fn clip_label(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 48 {
        let mut c: String = s.chars().take(47).collect();
        c.push('…');
        c
    } else {
        s
    }
}

// ---------------------------------------------------------------------------------------
// opening_add
// ---------------------------------------------------------------------------------------

/// Where an alternate opening is taken from.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OpeningSource {
    /// A moment's span: another take of the hook.
    Moment(Id<Moment>),
    /// A story suggestion's span.
    Suggestion(Id<StorySuggestion>),
    /// A span picked by hand: word anchors from the transcript (or typed times), on
    /// `media` (default: the media of the word, else of the cut's first part).
    Span(Box<PickedSpan>),
}

/// A span picked by hand (`OpeningSource::Span`).
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PickedSpan {
    pub start: Anchor,
    pub end: Anchor,
    #[serde(default)]
    pub media: Option<MediaRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpeningAddArgs {
    pub cut: Id<Cut>,
    pub from: OpeningSource,
    /// A short name for the version (default: from the moment or the words).
    #[serde(default)]
    pub label: Option<String>,
    /// The text hook this version shows (default: the moment's text hook, if any).
    #[serde(default)]
    pub hook_text: Option<String>,
}

#[derive(Debug)]
pub struct OpeningAdd;

#[command(
    name = "opening_add",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Add an alternate opening to a short (at most three): a moment's span (another hook take), a story suggestion's span or a span picked by hand. It is a sibling version of the cut that swaps the cut's opening (its leading hook parts) and shares everything after it; nothing is copied.",
    example = r#"{"cut": "01J00000000000000000000001", "from": {"moment": "01J00000000000000000000009"}}"#,
    example = r#"{"cut": "01J00000000000000000000001", "from": {"span": {"start": {"raw": {"time": {"clock": "src", "t": 7056000000}, "reason": "picked"}}, "end": {"raw": {"time": {"clock": "src", "t": 9878400000}, "reason": "picked"}}}}, "label": "the reaction", "hook_text": "It ran in *67 seconds*"}"#
)]
impl Command for OpeningAdd {
    type Args = OpeningAddArgs;
    type Output = Created;
    fn run(a: OpeningAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let c = cx.tx.get(a.cut)?.clone();
        if c.kind != CutKind::Short {
            return Err(CmdError::invalid(
                "/cut",
                "alternate openings are for shorts",
            ));
        }
        let parts = cx.tx.children::<Part>(a.cut.entity());
        if parts.is_empty() {
            return Err(CmdError::invalid("/cut", "the cut has no parts yet"));
        }
        if let Some(l) = &a.label {
            bounded(l, "/label", 80)?;
        }
        check_hook_text(&a.hook_text)?;
        let existing = cx.tx.children::<Opening>(a.cut.entity());
        if existing.len() >= MAX_OPENINGS {
            return Err(CmdError::invalid(
                "/cut",
                "a short keeps at most three alternate openings: remove one first",
            ));
        }
        let first_media = cx.tx.get(parts[0])?.media;
        let resolve = |cx: &mut Cx<'_>, x: &Anchor, path: &str| {
            crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), None, x, path)
                .map(|r| r.src)
        };
        let (span, from, from_ref, label, hook) = match &a.from {
            OpeningSource::Moment(m) => {
                let m = cx.tx.get(*m)?.clone();
                let x = resolve(cx, &m.span.start, "/from/moment")?;
                let y = resolve(cx, &m.span.end, "/from/moment")?;
                let label = if m.excerpt.text.trim().is_empty() {
                    clip_label(&m.quote)
                } else {
                    clip_label(&m.excerpt.text)
                };
                (
                    OpeningSpan {
                        media: m.media,
                        src_in: x,
                        src_out: y,
                    },
                    OpeningFrom::Moment,
                    Some(m.id.0.to_string()),
                    label,
                    m.text_hook.clone(),
                )
            }
            OpeningSource::Suggestion(sid) => {
                let sg = cx.tx.get(*sid)?.clone();
                if sg.cut != a.cut {
                    return Err(CmdError::invalid(
                        "/from/suggestion",
                        "a suggestion of another cut",
                    ));
                }
                let media = match c.moment {
                    Some(m) => cx.tx.get(m)?.media,
                    None => first_media,
                };
                let x = resolve(cx, &sg.span.start, "/from/suggestion")?;
                let y = resolve(cx, &sg.span.end, "/from/suggestion")?;
                (
                    OpeningSpan {
                        media,
                        src_in: x,
                        src_out: y,
                    },
                    OpeningFrom::Suggestion,
                    Some(sid.0.to_string()),
                    clip_label(&sg.quote),
                    None,
                )
            }
            OpeningSource::Span(p) => {
                let (start, end, media) = (&p.start, &p.end, p.media);
                let word_media = |cx: &Cx<'_>, x: &Anchor| -> Option<MediaRef> {
                    let w = match x {
                        Anchor::WordStart(w)
                        | Anchor::WordEnd(w)
                        | Anchor::AudibleStart(w)
                        | Anchor::AudibleEnd(w) => w,
                        _ => return None,
                    };
                    let (owner, _) = cx.analysis.word(w)?;
                    let u = Ulid::parse(owner).ok()?;
                    let doc = cx.tx.peek();
                    if doc.get(Id::<Source>::new(u)).is_some() {
                        Some(MediaRef::Source(Id::new(u)))
                    } else if doc.get(Id::<SyncGroup>::new(u)).is_some() {
                        Some(MediaRef::Group(Id::new(u)))
                    } else {
                        None
                    }
                };
                let m = media
                    .or_else(|| word_media(cx, start))
                    .unwrap_or(first_media);
                let x = resolve(cx, start, "/from/span/start")?;
                let y = resolve(cx, end, "/from/span/end")?;
                let sp = OpeningSpan {
                    media: m,
                    src_in: x,
                    src_out: y,
                };
                let words = quote(cx.tx.peek(), cx.analysis, std::slice::from_ref(&sp));
                (sp, OpeningFrom::Manual, None, clip_label(&words), None)
            }
        };
        if span.src_out <= span.src_in {
            return Err(CmdError::invalid("/from", "the span ends before it starts"));
        }
        // part edges sit on the cut's frame grid (nearest frame, ties to the earlier,
        // SPEC §4.1.1): word edges are not on it
        let fpf = super::longform::stream_fpf(cx.tx.peek(), a.cut);
        let span = OpeningSpan {
            src_in: Flicks(super::longform::nearest_frame(span.src_in.0, fpf)),
            src_out: Flicks(super::longform::nearest_frame(span.src_out.0, fpf)),
            ..span
        };
        let spans = vec![span];
        check_spans(cx.tx.peek(), &spans)?;
        let sig = spans_sig(&spans);
        let doc = cx.tx.peek();
        if spans_sig(&main_spans(doc, a.cut)) == sig
            || existing
                .iter()
                .filter_map(|o| doc.get(*o))
                .any(|o| spans_sig(&o.spans) == sig)
        {
            return Err(CmdError::invalid(
                "/from",
                "this opening is already a version of the cut",
            ));
        }
        let hook_text = a.hook_text.or(hook).filter(|t| !t.trim().is_empty());
        let label = a
            .label
            .filter(|l| !l.trim().is_empty())
            .unwrap_or(if label.is_empty() {
                "Alternate opening".to_owned()
            } else {
                label
            });
        let id: Id<Opening> = cx.tx.mint();
        cx.tx.create(Opening {
            id,
            cut: a.cut,
            label,
            spans,
            hook_text,
            from,
            from_ref,
            origin: if cx.caller.kind == CallerKind::Human {
                EditOrigin::Human
            } else {
                EditOrigin::Agent
            },
            at_ms: cx.services.now_ms,
        })?;
        Ok(created(id))
    }
}

// ---------------------------------------------------------------------------------------
// opening_set / opening_delete
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpeningSetArgs {
    pub opening: Id<Opening>,
    #[serde(default)]
    pub label: Option<String>,
    /// The version's own text hook; empty clears it (the version shows the cut's).
    #[serde(default)]
    pub hook_text: Option<String>,
}

#[derive(Debug)]
pub struct OpeningSet;

#[command(
    name = "opening_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Rename an alternate opening or set the text hook its version shows (empty: the cut's own).",
    example = r#"{"opening": "01J00000000000000000000009", "hook_text": "Done in *1:07*"}"#
)]
impl Command for OpeningSet {
    type Args = OpeningSetArgs;
    type Output = Done;
    fn run(a: OpeningSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.get(a.opening)?;
        if let Some(l) = &a.label {
            bounded(l, "/label", 80)?;
            if l.trim().is_empty() {
                return Err(CmdError::invalid("/label", "a name"));
            }
        }
        check_hook_text(&a.hook_text)?;
        cx.tx.update(a.opening, |o| {
            if let Some(l) = a.label {
                o.label = l;
            }
            if let Some(h) = a.hook_text {
                o.hook_text = (!h.trim().is_empty()).then_some(h);
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpeningOnlyArgs {
    pub opening: Id<Opening>,
}

#[derive(Debug)]
pub struct OpeningDelete;

#[command(
    name = "opening_delete",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Remove an alternate opening (the cut and its other versions are untouched).",
    example = r#"{"opening": "01J00000000000000000000009"}"#
)]
impl Command for OpeningDelete {
    type Args = OpeningOnlyArgs;
    type Output = Done;
    fn run(a: OpeningOnlyArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.delete(a.opening)?;
        Ok(Done {})
    }
}

// ---------------------------------------------------------------------------------------
// opening_choose
// ---------------------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct Chosen {
    /// The cut's previous opening, now an alternate (`was_main`).
    pub was_main: String,
    /// The cut's opening parts now.
    pub parts: Vec<String>,
}

#[derive(Debug)]
pub struct OpeningChoose;

#[command(
    name = "opening_choose",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Make an alternate opening the main version, in one undoable transaction: its spans become the cut's opening (the first opening part keeps its id and look) and the cut's old opening is kept as an alternate. The human only.",
    example = r#"{"opening": "01J00000000000000000000009"}"#
)]
impl Command for OpeningChoose {
    type Args = OpeningOnlyArgs;
    type Output = Chosen;
    fn run(a: OpeningOnlyArgs, cx: &mut Cx<'_>) -> Result<Chosen, CmdError> {
        if cx.caller.kind != CallerKind::Human {
            return Err(CmdError::Unauthorized {
                scope: "human_only".into(),
            });
        }
        let o = cx.tx.get(a.opening)?.clone();
        let parts = cx.tx.children::<Part>(o.cut.entity());
        for p in &parts {
            super::record_part_children(cx, *p);
        }
        let _ = cx.tx.children::<Overlay>(o.cut.entity());
        let doc = cx.tx.peek();
        let old = main_spans(doc, o.cut);
        let old_hook = hook_text_of(doc, o.cut);
        let old_label = format!(
            "Before: {}",
            clip_label(&quote(doc, cx.analysis, &old))
                .chars()
                .take(40)
                .collect::<String>()
        );
        cx.tx.delete(a.opening)?;
        swap_opening(cx.tx.staging(), o.cut, &o.spans, o.hook_text.as_deref())
            .map_err(model_err)?;
        let id: Id<Opening> = cx.tx.mint();
        cx.tx.create(Opening {
            id,
            cut: o.cut,
            label: if old_label.trim() == "Before:" {
                "Previous opening".into()
            } else {
                old_label
            },
            spans: old,
            hook_text: old_hook,
            from: OpeningFrom::WasMain,
            from_ref: None,
            origin: EditOrigin::Human,
            at_ms: cx.services.now_ms,
        })?;
        Ok(Chosen {
            was_main: id.0.to_string(),
            parts: opening_parts(cx.tx.peek(), o.cut)
                .iter()
                .map(|p| p.0.to_string())
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------------------
// openings_list
// ---------------------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpanView {
    pub media: MediaRef,
    pub src_in: Flicks,
    pub src_out: Flicks,
    /// Source seconds (3 dp).
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct VersionView {
    /// The alternate's id; absent for the main version (the cut itself).
    pub opening: Option<String>,
    pub label: String,
    pub main: bool,
    pub from: Option<OpeningFrom>,
    pub from_ref: Option<String>,
    pub origin: Option<EditOrigin>,
    /// The text hook this version shows (the cut's when it has none of its own).
    pub hook_text: Option<String>,
    /// The version carries its own text hook.
    pub own_hook: bool,
    pub spans: Vec<SpanView>,
    /// The opening's length and the whole version's, ms.
    pub opening_ms: i64,
    pub total_ms: i64,
    /// What the opening says (untrusted transcript text).
    pub quote: String,
    /// `spans_sig`: what an export of this version records.
    pub sig: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Openings {
    /// The shared rest of the cut, ms.
    pub body_ms: i64,
    /// The main version first, then the alternates (oldest first).
    pub versions: Vec<VersionView>,
    pub max_alternates: usize,
}

/// The versions of a cut: the main one and its alternates.
pub fn openings(doc: &Doc, view: &AnalysisView, cut: Id<Cut>) -> Openings {
    let total: i64 = gmclip_model::clock::cut_layout(doc, cut)
        .iter()
        .map(|s| s.dur.0)
        .sum::<i64>()
        * 1000
        / F;
    let main = main_spans(doc, cut);
    let main_ms = spans_ms(&main);
    let body_ms = (total - main_ms).max(0);
    let cut_hook = hook_text_of(doc, cut);
    let span_view = |s: &OpeningSpan| SpanView {
        media: s.media,
        src_in: s.src_in,
        src_out: s.src_out,
        start_s: (s.src_in.0 as f64 / F as f64 * 1000.0).round() / 1000.0,
        end_s: (s.src_out.0 as f64 / F as f64 * 1000.0).round() / 1000.0,
    };
    let mut versions = vec![VersionView {
        opening: None,
        label: "Main".into(),
        main: true,
        from: None,
        from_ref: None,
        origin: None,
        hook_text: cut_hook.clone(),
        own_hook: cut_hook.is_some(),
        spans: main.iter().map(span_view).collect(),
        opening_ms: main_ms,
        total_ms: total,
        quote: quote(doc, view, &main),
        sig: spans_sig(&main),
    }];
    let mut alts: Vec<&Opening> = doc
        .children::<Opening>(cut.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    alts.sort_by_key(|o| (o.at_ms, o.id.0));
    for o in alts {
        let ms = spans_ms(&o.spans);
        versions.push(VersionView {
            opening: Some(o.id.0.to_string()),
            label: o.label.clone(),
            main: false,
            from: Some(o.from),
            from_ref: o.from_ref.clone(),
            origin: Some(o.origin),
            hook_text: o.hook_text.clone().or_else(|| cut_hook.clone()),
            own_hook: o.hook_text.is_some(),
            spans: o.spans.iter().map(span_view).collect(),
            opening_ms: ms,
            total_ms: body_ms + ms,
            quote: quote(doc, view, &o.spans),
            sig: spans_sig(&o.spans),
        });
    }
    Openings {
        body_ms,
        versions,
        max_alternates: MAX_OPENINGS,
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutArgs {
    pub cut: Id<Cut>,
}

#[derive(Debug)]
pub struct OpeningsList;

#[command(
    name = "openings_list",
    scope = "read",
    undo = "read_only",
    since = "M2",
    doc = "List a short's versions: the main one (the cut) and its alternate openings, each with its spans, length, the words it opens on and the text hook it shows.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for OpeningsList {
    type Args = CutArgs;
    type Output = Openings;
    fn run(a: CutArgs, cx: &mut Cx<'_>) -> Result<Openings, CmdError> {
        cx.tx.get(a.cut)?;
        let _ = cx.tx.children::<Part>(a.cut.entity());
        let _ = cx.tx.children::<Opening>(a.cut.entity());
        Ok(openings(cx.tx.peek(), cx.analysis, a.cut))
    }
}
