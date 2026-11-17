//! Story commands (A2.08; SPEC §5.2.1, R-PROP-1, R-STORY-1).
//!
//! A story pass (gather, sweep, repair, arrange) writes **suggestions and the arc only**:
//! `story_suggestions_set` never touches a part. "Use the proposed cut" is `story_apply`,
//! one transaction that adds the arc's suggestions as parts in arc order. It never adds
//! a rejected suggestion, and a part the human deletes rejects the suggestion it came
//! from (`part_delete`), so applying again never re-adds it. A gather that proposes the
//! same span again finds it rejected. Apply adds and reorders; it removes nothing.

use std::collections::{BTreeMap, BTreeSet};

use gmclip_macros::command;
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id, OrderKey};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Done, bounded};
use crate::command::{Command, Cx};

/// Two spans within this of each other at both ends are the same span (100 ms).
const SAME_SPAN: i64 = 705_600_000 / 10;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoryBriefSetArgs {
    pub cut: Id<Cut>,
    /// What the story is about, in the human's words (≤ 4 000 characters).
    pub brief: String,
}

#[derive(Debug)]
pub struct StoryBriefSet;

#[command(
    name = "story_brief_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set a cut's story brief (what the story is about; gather reads it).",
    example = r#"{"cut": "01J00000000000000000000001", "brief": "the 67 s local video run, from prompt to result"}"#
)]
impl Command for StoryBriefSet {
    type Args = StoryBriefSetArgs;
    type Output = Done;
    fn run(a: StoryBriefSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        bounded(&a.brief, "/brief", 4_000)?;
        cx.tx.update(a.cut, |c| c.brief = a.brief)?;
        Ok(Done {})
    }
}

/// One suggested span.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestionIn {
    pub span: Span,
    pub role: PartRole,
    /// 0–1.
    #[serde(default)]
    pub relevance: f64,
    /// The words of the span, as heard (≤ 2 000 characters).
    #[serde(default)]
    pub quote: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorySuggestionsSetArgs {
    pub cut: Id<Cut>,
    /// At most 200.
    pub suggestions: Vec<SuggestionIn>,
    /// The proposed order, as indexes into `suggestions`; all of them in order when absent.
    #[serde(default)]
    pub arc: Option<Vec<usize>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SuggestionsSet {
    /// The new suggestions' ids, in the order given.
    pub ids: Vec<String>,
    /// Suggestions that match a span the human rejected: stored as rejected.
    pub rejected: usize,
    /// Suggested (not rejected, not in the cut) suggestions this replaced.
    pub replaced: usize,
}

/// Resolves a span to source times.
fn span_src(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    s: &Span,
    path: &str,
) -> Result<(Flicks, Flicks), CmdError> {
    let _ = cx.tx.children::<Part>(cut.entity());
    let a = crate::anchors::resolve_in(
        cx.tx.peek(),
        Some(cx.analysis),
        Some(cut),
        &s.start,
        &format!("{path}/start"),
    )?;
    let b = crate::anchors::resolve_in(
        cx.tx.peek(),
        Some(cx.analysis),
        Some(cut),
        &s.end,
        &format!("{path}/end"),
    )?;
    Ok((a.src, b.src))
}

#[derive(Debug)]
pub struct StorySuggestionsSet;

#[command(
    name = "story_suggestions_set",
    scope = "edit",
    undo = "undoable",
    doc = "A story pass's output: the cut's suggested spans and the proposed order (arc). Changes no part (R-STORY-1); spans the human rejected stay rejected.",
    example = r#"{"cut": "01J00000000000000000000001", "suggestions": [{"span": {"start": {"raw": {"time": {"clock": "src", "t": 0}, "reason": "gather"}}, "end": {"raw": {"time": {"clock": "src", "t": 3528000000}, "reason": "gather"}}}, "role": "setup", "relevance": 0.8, "quote": "so I typed the prompt"}]}"#
)]
impl Command for StorySuggestionsSet {
    type Args = StorySuggestionsSetArgs;
    type Output = SuggestionsSet;
    fn run(a: StorySuggestionsSetArgs, cx: &mut Cx<'_>) -> Result<SuggestionsSet, CmdError> {
        cx.tx.get(a.cut)?;
        if a.suggestions.len() > 200 {
            return Err(CmdError::invalid("/suggestions", "at most 200"));
        }
        let mut times = Vec::with_capacity(a.suggestions.len());
        for (i, s) in a.suggestions.iter().enumerate() {
            bounded(&s.quote, &format!("/suggestions/{i}/quote"), 2_000)?;
            if !(0.0..=1.0).contains(&s.relevance) {
                return Err(CmdError::invalid(
                    &format!("/suggestions/{i}/relevance"),
                    "0–1",
                ));
            }
            let (x, y) = span_src(cx, a.cut, &s.span, &format!("/suggestions/{i}/span"))?;
            if y.0 - x.0 < super::part::MIN_PART.0 {
                return Err(CmdError::invalid(
                    &format!("/suggestions/{i}/span"),
                    "at least 0.12 s, end after start",
                ));
            }
            times.push((x, y));
        }
        let arc = a.arc.unwrap_or_else(|| (0..a.suggestions.len()).collect());
        let mut seen = BTreeSet::new();
        for &k in &arc {
            if k >= a.suggestions.len() || !seen.insert(k) {
                return Err(CmdError::invalid(
                    "/arc",
                    "distinct indexes into suggestions",
                ));
            }
        }
        // what stays: rejected suggestions and those a part came from
        let parts = cx.tx.children::<Part>(a.cut.entity());
        let mut in_cut = BTreeSet::new();
        for p in parts {
            if let Some(s) = cx.tx.get(p)?.lineage.suggestion {
                in_cut.insert(s);
            }
        }
        let old = cx.tx.children::<StorySuggestion>(a.cut.entity());
        let mut rejected_spans = Vec::new();
        let mut replaced = 0;
        let mut last: Option<OrderKey> = None;
        for o in old {
            let s = cx.tx.get(o)?.clone();
            if s.state == SuggestionState::Rejected {
                if let Ok(t) = span_src(cx, a.cut, &s.span, "/") {
                    rejected_spans.push(t);
                }
                last = Some(s.order.clone());
            } else if in_cut.contains(&o) {
                last = Some(s.order.clone());
            } else {
                cx.tx.delete(o)?;
                replaced += 1;
            }
        }
        let mut ids = Vec::with_capacity(a.suggestions.len());
        let mut rejected = 0;
        for (s, (x, y)) in a.suggestions.into_iter().zip(times) {
            let was_rejected = rejected_spans
                .iter()
                .any(|(rx, ry)| (rx.0 - x.0).abs() <= SAME_SPAN && (ry.0 - y.0).abs() <= SAME_SPAN);
            if was_rejected {
                rejected += 1;
            }
            let order = OrderKey::between(last.as_ref(), None).map_err(|_| CmdError::Internal {
                trace_id: "order".into(),
            })?;
            last = Some(order.clone());
            let id: Id<StorySuggestion> = cx.tx.mint();
            cx.tx.create(StorySuggestion {
                id,
                cut: a.cut,
                order,
                span: s.span,
                role: s.role,
                relevance: s.relevance,
                quote: s.quote,
                state: if was_rejected {
                    SuggestionState::Rejected
                } else {
                    SuggestionState::Suggested
                },
            })?;
            ids.push(id);
        }
        let story_arc: Vec<Id<StorySuggestion>> = arc.iter().map(|&k| ids[k]).collect();
        cx.tx.update(a.cut, |c| c.story_arc = story_arc)?;
        Ok(SuggestionsSet {
            ids: ids.iter().map(|i| i.0.to_string()).collect(),
            rejected,
            replaced,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorySuggestionStateArgs {
    pub suggestion: Id<StorySuggestion>,
    pub state: SuggestionState,
}

#[derive(Debug)]
pub struct StorySuggestionState;

#[command(
    name = "story_suggestion_state",
    scope = "edit",
    undo = "undoable",
    doc = "Reject a suggested span (story_apply never adds it), or bring it back.",
    example = r#"{"suggestion": "01J00000000000000000000009", "state": "rejected"}"#
)]
impl Command for StorySuggestionState {
    type Args = StorySuggestionStateArgs;
    type Output = Done;
    fn run(a: StorySuggestionStateArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        cx.tx.update(a.suggestion, |s| s.state = a.state)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoryApplyArgs {
    pub cut: Id<Cut>,
    /// Only these suggestions (the panel's ＋), appended after the cut's parts; the whole
    /// arc, in arc order after the hook, when absent.
    #[serde(default)]
    pub only: Option<Vec<Id<StorySuggestion>>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Applied {
    /// Parts added from the arc.
    pub added: Vec<String>,
    /// Arc suggestions skipped because the human rejected them (or their span).
    pub skipped_rejected: usize,
    /// Arc suggestions already in the cut (moved into arc order).
    pub kept: usize,
}

#[derive(Debug)]
pub struct StoryApply;

#[command(
    name = "story_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Use the proposed cut: the arc's suggestions become parts, in arc order after the hook, in one undo step. Rejected suggestions are never added; no part is removed (R-PROP-1, R-STORY-1).",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for StoryApply {
    type Args = StoryApplyArgs;
    type Output = Applied;
    fn run(a: StoryApplyArgs, cx: &mut Cx<'_>) -> Result<Applied, CmdError> {
        let c = cx.tx.get(a.cut)?.clone();
        let parts = cx.tx.children::<Part>(a.cut.entity());
        let mut by_sug: BTreeMap<Id<StorySuggestion>, Id<Part>> = BTreeMap::new();
        let mut hooks = Vec::new();
        let mut first_media = None;
        let mut look = BTreeMap::new();
        for (k, &p) in parts.iter().enumerate() {
            let x = cx.tx.get(p)?;
            if k == 0 {
                first_media = Some(x.media);
                look = x.look.clone();
            }
            match x.lineage.suggestion {
                Some(s) => {
                    by_sug.insert(s, p);
                }
                None if x.from == PartFrom::Hook || x.role == PartRole::Hook => hooks.push(p),
                None => {}
            }
        }
        let media = match c.moment {
            Some(m) => cx.tx.get(m)?.media,
            None => first_media.ok_or_else(|| {
                CmdError::invalid("/cut", "the cut has no source yet: add its hook part first")
            })?,
        };
        // spans already in the cut, and spans the human rejected (under any suggestion)
        let mut in_cut: Vec<(Flicks, Flicks, Id<Part>)> = Vec::new();
        for &p in &parts {
            let x = cx.tx.get(p)?;
            in_cut.push((x.src_in, x.src_out, p));
        }
        let mut refused: Vec<(Flicks, Flicks)> = Vec::new();
        for o in cx.tx.children::<StorySuggestion>(a.cut.entity()) {
            let s = cx.tx.get(o)?.clone();
            if s.state == SuggestionState::Rejected
                && let Ok(t) = span_src(cx, a.cut, &s.span, "/")
            {
                refused.push(t);
            }
        }
        let same = |a: (Flicks, Flicks), b: (Flicks, Flicks)| {
            (a.0.0 - b.0.0).abs() <= SAME_SPAN && (a.1.0 - b.1.0).abs() <= SAME_SPAN
        };
        let picking = a.only.is_some();
        let wanted: Vec<Id<StorySuggestion>> = match a.only {
            Some(v) => {
                if v.len() > 200 {
                    return Err(CmdError::invalid("/only", "at most 200"));
                }
                for s in &v {
                    if cx.tx.get(*s)?.cut != a.cut {
                        return Err(CmdError::invalid("/only", "a suggestion of another cut"));
                    }
                }
                v
            }
            None => c.story_arc.clone(),
        };
        // ＋ keeps the cut's order and appends; the arc puts the hook first
        let mut order: Vec<Id<Part>> = if picking {
            parts.clone()
        } else {
            hooks.clone()
        };
        let mut added = Vec::new();
        let (mut skipped, mut kept) = (0, 0);
        for sid in &wanted {
            let s = cx.tx.get(*sid)?.clone();
            if s.state == SuggestionState::Rejected {
                skipped += 1;
                continue;
            }
            if let Some(p) = by_sug.get(sid) {
                if !order.contains(p) {
                    order.push(*p);
                }
                kept += 1;
                continue;
            }
            let (x, y) = span_src(cx, a.cut, &s.span, "/story_arc")?;
            let fpf = super::longform::stream_fpf(cx.tx.peek(), a.cut);
            let (x, y) = (
                Flicks(super::longform::nearest_frame(x.0, fpf)),
                Flicks(super::longform::nearest_frame(y.0, fpf)),
            );
            if refused.iter().any(|r| same(*r, (x, y))) {
                skipped += 1;
                continue;
            }
            if let Some(&(_, _, p)) = in_cut.iter().find(|(i, o, _)| same((*i, *o), (x, y))) {
                if !order.contains(&p) {
                    order.push(p);
                }
                kept += 1;
                continue;
            }
            if y.0 - x.0 < super::part::MIN_PART.0 {
                skipped += 1;
                continue;
            }
            let id: Id<Part> = cx.tx.mint();
            cx.tx.create(Part {
                id,
                cut: a.cut,
                order: OrderKey::first(),
                media,
                src_in: x,
                src_out: y,
                audio_from: None,
                role: s.role,
                from: PartFrom::Suggested,
                lineage: PartLineage {
                    suggestion: Some(*sid),
                    ..PartLineage::default()
                },
                gain_db: 0.0,
                speed: 1.0,
                ramp: None,
                part_group: None,
                under_db: None,
                look: look.clone(),
                caption_place_override: BTreeMap::new(),
            })?;
            order.push(id);
            added.push(id);
        }
        // the rest keep their relative order, after the arc
        for p in parts {
            if !order.contains(&p) {
                order.push(p);
            }
        }
        let mut last: Option<OrderKey> = None;
        for p in order {
            let key = OrderKey::between(last.as_ref(), None).map_err(|_| CmdError::Internal {
                trace_id: "order".into(),
            })?;
            last = Some(key.clone());
            cx.tx.update(p, |x| x.order = key)?;
        }
        Ok(Applied {
            added: added.iter().map(|i| i.0.to_string()).collect(),
            skipped_rejected: skipped,
            kept,
        })
    }
}

/// `part_delete` by the human: the suggestion the part came from is rejected, so the
/// next `story_apply` does not bring it back (R-STORY-1).
pub(crate) fn reject_on_delete(cx: &mut Cx<'_>, part: Id<Part>) -> Result<(), CmdError> {
    if cx.caller.kind != gmclip_proto::CallerKind::Human {
        return Ok(());
    }
    if let Some(s) = cx.tx.get(part)?.lineage.suggestion
        && cx.tx.get(s).is_ok()
    {
        cx.tx.update(s, |x| x.state = SuggestionState::Rejected)?;
    }
    Ok(())
}

/// A.02's measure: how much of a shipped cut a gather found — the share of the shipped
/// parts' source seconds that some suggested span covers (0–1; 1 with no shipped parts).
/// Spans are `[in, out)` source flicks on one source.
pub fn span_recall(suggested: &[(Flicks, Flicks)], shipped: &[(Flicks, Flicks)]) -> f64 {
    let mut s: Vec<(i64, i64)> = suggested
        .iter()
        .filter(|(a, b)| b.0 > a.0)
        .map(|(a, b)| (a.0, b.0))
        .collect();
    s.sort_unstable();
    // merge the suggestions so overlaps count once
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in s {
        match merged.last_mut() {
            Some(m) if a <= m.1 => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let (mut total, mut hit) = (0i128, 0i128);
    for (a, b) in shipped {
        if b.0 <= a.0 {
            continue;
        }
        total += i128::from(b.0 - a.0);
        for (x, y) in &merged {
            let lo = a.0.max(*x);
            let hi = b.0.min(*y);
            if hi > lo {
                hit += i128::from(hi - lo);
            }
        }
    }
    if total == 0 {
        1.0
    } else {
        hit as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(a: i64, b: i64) -> (Flicks, Flicks) {
        (Flicks(a), Flicks(b))
    }

    #[test]
    fn recall_counts_covered_seconds_once() {
        // shipped 0–10 and 20–30; suggested 5–15 and 8–12 (overlapping) and 20–30
        let r = span_recall(&[f(5, 15), f(8, 12), f(20, 30)], &[f(0, 10), f(20, 30)]);
        assert!((r - 0.75).abs() < 1e-9, "{r}");
        assert_eq!(span_recall(&[], &[f(0, 10)]), 0.0);
        assert_eq!(span_recall(&[f(0, 1)], &[]), 1.0);
    }
}
