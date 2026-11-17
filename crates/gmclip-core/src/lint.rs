//! Lint (SPEC §5.13): warnings, never refusals. The **[M1] agent set**:
//! - `agent_typed_time`: a time the agent typed rather than anchored — a raw time, a
//!   non-zero `offset_ms`, a word fingerprint no transcript has verified yet (M1), or a
//!   source time typed into a hold, a visual split or a layer's time map (R-ANC-1);
//! - `number_without_provenance` (**error**): a number on screen an agent wrote whose
//!   provenance names no measurement (a note alone is the agent's own claim);
//! - `number_disagrees_with_provenance` (**error**): the displayed number differs from
//!   its measurement by more than its display rounding (`m:ss` floored; integers ±1;
//!   decimals ±½ of the last digit);
//! - `agent_grading_unmeasured`: gain an agent set with no `measure{loudness}` cited;
//! - `line_edged_span_unmeasured`: the cut's moment is line-edged (**info** since A2.17:
//!   it fires on every imported golden);
//! - `hook_outside_moment`: part one's source range meets neither the moment's span nor
//!   its payoff (payoff-first is allowed);
//! - `part_too_short`: a part under 0.25 s.
//!
//! Plus structural checks every export needs (`empty_cut` error, `no_framing`).
//!
//! The **[M2] core set** (A2.17), checked per framing on a 1080×1920 reference:
//! - `duration_over_cap`: longer than the framing's cap (100 s for 9:16, 15 min for 16:9)
//!   or `duration_under_band`: under 10 s (no kit band yet);
//! - `overlapping_source`: two parts play more than 0.5 s of the same source (part one,
//!   the payoff opening, may replay a later take);
//! - `term_gate`: a caption word spells a project term wrong ([`crate::terms`]);
//! - `hook_outside_band`: the text hook's centre outside the hook band (y 220–420 px);
//! - `overlay_zone`: captions, the hook or the face inside Instagram's overlay zones
//!   (top 220 px, bottom 360 px, right 120 px between y 1000 and 1560), calibrated on AJ's
//!   approved goldens: his facecam seat runs up to 100 px into the bottom zone;
//! - `framings_fps_differ`: the cut's framings disagree on frame rate;
//! - `private_region_under_text` (**error**): captions or a text overlay drawn over the
//!   place a `private`-class source region lands on the canvas;
//! - `screen_text_small` (R.03, SPEC §5.7; **info**: it fires on AJ's approved h34, h37
//!   and h48): a screen layer shown under 0.6 canvas px per source px (its text is
//!   likely too small to read on a phone).
//! - `loop_last_frame` (decision 0025): a cut set to loop whose last frame does not show the
//!   opening's layout and crop; the fix is `loop_fix`.
//! Export staleness and spans used by an exported sibling need the export records, so the
//! app adds them.
//!
//! Authorship is the entity's last author (`Doc::last_author`): the model keeps no
//! per-field author, so a human touching an agent's overlay (e.g. its opacity) makes it
//! the human's for lint. Known M1 limit; per-field provenance lands with R-HIST-1.
//!
//! R-LINT-2: error-level findings block **agent-started** exports with `NeedsApproval`,
//! never human exports ([`blocks_agent_export`]).

use gmclip_model::anchor::Anchor;
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Author, Doc, EntityId, Flicks, Id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anchors;

const SEC: i64 = 705_600_000;

#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Calibrated down: the rule fires on AJ's approved goldens (A2.17), so it informs
    /// and never warns.
    Info,
    Warning,
    Error,
}

/// One finding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub rule: String,
    pub level: Level,
    /// gmclip's words (never caller text beyond 16 bytes).
    pub message: String,
    pub entity: Option<String>,
    /// The one-click fix, where the spec names one (§5.13): a registry command and its
    /// arguments, worked out here. Never applied by lint itself; a human clicks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

/// A one-click fix for a finding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fix {
    /// The button's words (gmclip's).
    pub label: String,
    /// A registry command.
    pub command: String,
    pub args: serde_json::Value,
}

/// The [M2] cap and band (SPEC §5.13): 9:16 ≤ 100 s, 16:9 ≤ 15 min, ≥ 10 s.
const CAP_VERTICAL_S: f64 = 100.0;
const CAP_HORIZONTAL_S: f64 = 900.0;
const BAND_MIN_S: f64 = 10.0;
/// Instagram's overlay zones on a 1080×1920 canvas, and the hook band.
const ZONE_TOP: f64 = 220.0;
const ZONE_BOTTOM: f64 = 1560.0;
const ZONE_RIGHT_X: f64 = 960.0;
const ZONE_RIGHT_Y: (f64, f64) = (1000.0, 1560.0);
const HOOK_BAND: (f64, f64) = (220.0, 420.0);
/// Calibration on the approved goldens: AJ's facecam seat [160, 1120, 760, 520] reaches
/// y 1640, 80 px into the bottom zone (h53's 1626); 100 px is allowed.
const FACE_BOTTOM_ALLOW: f64 = 100.0;

fn by_agent(doc: &Doc, e: EntityId) -> bool {
    matches!(doc.last_author(e), Some(Author::Run(_)))
}

fn find(rule: &str, level: Level, message: impl Into<String>, entity: Option<EntityId>) -> Finding {
    Finding {
        rule: rule.into(),
        level,
        message: message.into(),
        entity: entity.map(|e| e.to_string()),
        fix: None,
    }
}

/// A number as displayed: its value and the tolerance its display rounding allows.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) value: f64,
    /// `m:ss` / `h:mm:ss`: seconds floored, so the true value lies in `[value, value + 1)`.
    floored: bool,
    tol: f64,
}

fn parse_shown(tok: &str) -> Option<Shown> {
    if !tok
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | ':' | ','))
    {
        return None; // non-ASCII digits: a number, but not one we can check
    }
    let clean = tok.replace(',', "");
    if clean.contains(':') {
        let groups: Vec<&str> = clean.split(':').collect();
        if groups.len() > 3 || groups.iter().any(|g| g.is_empty() || g.contains('.')) {
            return None;
        }
        let mut total = 0f64;
        for (i, g) in groups.iter().enumerate() {
            let v: f64 = g.parse().ok()?;
            if i > 0 && (v >= 60.0 || g.len() != 2) {
                return None;
            }
            total = total * 60.0 + v;
        }
        return Some(Shown {
            value: total,
            floored: true,
            tol: 0.0,
        });
    }
    if clean.matches('.').count() > 1 {
        return None;
    }
    let v: f64 = clean.parse().ok()?;
    let tol = match clean.split_once('.') {
        Some((_, frac)) => 0.5 * 10f64.powi(-(frac.len() as i32)),
        None => 1.0,
    };
    Some(Shown {
        value: v,
        floored: false,
        tol,
    })
}

/// Numbers in a text (any script's digits): each token, and its value when it parses
/// as an integer, a decimal, `m:ss` or `h:mm:ss`. A token that does not parse still
/// needs provenance.
pub(crate) fn numbers(text: &str) -> Vec<(String, Option<Shown>)> {
    let mut out = Vec::new();
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_numeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len()
            && (b[i].is_numeric()
                || (matches!(b[i], '.' | ':' | ',') && i + 1 < b.len() && b[i + 1].is_numeric()))
        {
            i += 1;
        }
        // digits inside a name are not a figure: a run right after a letter or `@`
        // (H100, @user1) or one that runs on into a word (0xSero, 3Blue1Brown); a unit
        // suffix (24GB, 35B, 4x, 90s) stays a number
        let before = start.checked_sub(1).map(|k| b[k]);
        let tail = b[i..].iter().take_while(|c| c.is_alphanumeric()).count();
        if before.is_some_and(|c| c.is_alphabetic() || c == '@') || tail >= 4 {
            continue;
        }
        let tok: String = b[start..i].iter().collect();
        let shown = parse_shown(&tok);
        out.push((tok, shown));
    }
    out
}

/// Does `measured` round to what is displayed?
pub(crate) fn agrees(shown: Shown, measured: f64) -> bool {
    if shown.floored {
        measured >= shown.value && measured < shown.value + 1.0
    } else {
        (measured - shown.value).abs() <= shown.tol + 1e-9
    }
}

/// What an overlay puts on screen as text (its runs, and a counter's labels).
fn overlay_text(o: &Overlay) -> String {
    let mut t: String = o
        .text
        .as_ref()
        .map(|t| t.runs.iter().map(|r| r.text.as_str()).collect())
        .unwrap_or_default();
    if let Some(c) = &o.counter {
        t.push(' ');
        t.push_str(&c.running_label);
        t.push(' ');
        t.push_str(&c.done_label);
    }
    t
}

fn src_range(doc: &Doc, a: &Anchor, b: &Anchor) -> Option<(Flicks, Flicks)> {
    let x = anchors::resolve(doc, None, a, "").ok()?.src;
    let y = anchors::resolve(doc, None, b, "").ok()?.src;
    Some((x, y))
}

/// The overlays a cut shows: its own and its clips'.
fn overlays_of(doc: &Doc, cut: Id<Cut>) -> Vec<Id<Overlay>> {
    let mut v: Vec<Id<Overlay>> = doc.children::<Overlay>(cut.entity()).collect();
    for c in doc.children::<Clip>(cut.entity()) {
        v.extend(doc.children::<Overlay>(c.entity()));
    }
    v
}

/// The layers a cut renders through: its framings', its cut layouts', its parts'
/// overrides.
fn layers_of(doc: &Doc, cut: Id<Cut>, parts: &[&Part]) -> Vec<Id<Layer>> {
    let mut v = Vec::new();
    for f in doc.children::<Framing>(cut.entity()) {
        v.extend(doc.children::<Layer>(f.entity()));
    }
    for l in doc.children::<Layout>(cut.entity()) {
        v.extend(doc.children::<Layer>(l.entity()));
    }
    for p in parts {
        v.extend(doc.children::<Layer>(p.id.entity()));
    }
    v
}

fn numbers_lint(
    doc: &Doc,
    view: Option<&crate::analysis::AnalysisView>,
    id: Id<Overlay>,
    o: &Overlay,
    f: &mut Vec<Finding>,
) {
    let text = overlay_text(o);
    let agent = by_agent(doc, id.entity());
    for (tok, shown) in numbers(&text) {
        // provenance names this exact number
        let prov = o
            .number_provenance
            .iter()
            .find(|p| numbers(&p.text).iter().any(|(t, _)| *t == tok))
            // a run clock's numbers all come from its measurement: a number typed over
            // it by hand is judged against that (`run_clock_make`, A2.07)
            .or_else(|| {
                (o.role.as_deref() == Some("run_clock"))
                    .then(|| o.number_provenance.iter().find(|p| p.measurement.is_some()))
                    .flatten()
            });
        // the speaker says the number (AJ 2026-10-20: "My first 7 bb plugins" over "seven
        // of them here"): the word, found in the transcript at its stored time (the
        // fingerprint without one), must say the number shown
        if let Some(w) = prov.and_then(|p| p.spoken.as_ref()) {
            let said = match view {
                Some(v) => v
                    .word(w)
                    .filter(|(_, x)| (x.start - w.t_src_start.0).abs() <= SEC / 4)
                    .map(|(_, x)| x.text.as_str()),
                None => Some(w.normalised_text.as_str()),
            };
            match said.map(|t| (t, crate::hooktext::said_number(t))) {
                Some((_, Some(n))) if shown.is_none_or(|s| agrees(s, n)) => {}
                Some((t, _)) => f.push(find(
                    "number_disagrees_with_provenance",
                    Level::Error,
                    format!(
                        "{} on screen, but the word it cites says “{}”",
                        clip16(&tok),
                        clip16(t)
                    ),
                    Some(id.entity()),
                )),
                None => f.push(find(
                    "number_without_provenance",
                    Level::Error,
                    format!(
                        "the number {} cites a word the transcript does not have",
                        clip16(&tok)
                    ),
                    Some(id.entity()),
                )),
            }
            continue;
        }
        let measured = prov.and_then(|p| p.measurement).and_then(|m| doc.get(m));
        match (prov, measured) {
            (_, Some(m)) => {
                let value = if m.unit == "ms" {
                    m.value / 1000.0
                } else {
                    m.value
                };
                // a running clock shows the time so far: any value up to the measured total
                // agrees (the reference's clock ticks, each its own overlay; calibrated on h34/h37)
                let running = o.role.as_deref() == Some("run_clock")
                    && shown.is_some_and(|s| s.value >= 0.0 && s.value <= value + s.tol);
                if let Some(s) = shown
                    && !agrees(s, value)
                    && !running
                {
                    f.push(find(
                        "number_disagrees_with_provenance",
                        Level::Error,
                        format!(
                            "{} on screen, but its measurement says {value:.3}",
                            clip16(&tok)
                        ),
                        Some(id.entity()),
                    ));
                }
            }
            (None, None) if agent => f.push(find(
                "number_without_provenance",
                Level::Error,
                format!("the number {} on screen has no provenance", clip16(&tok)),
                Some(id.entity()),
            )),
            (Some(_), None) if agent => f.push(find(
                "number_without_provenance",
                Level::Error,
                format!(
                    "the number {} cites no measurement (a note alone is not provenance)",
                    clip16(&tok)
                ),
                Some(id.entity()),
            )),
            _ => {}
        }
    }
}

fn clock(flicks: i64) -> String {
    let s = (flicks.max(0) / SEC) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// What the cut says (decision 0040, [`crate::script`]): retakes left in, dead air
/// inside or around speech, parts that begin or end mid-sentence or cut a word, and
/// silent parts in a cut that talks, and (decision 0049, `part_filler_elsewhere`) a
/// wordless part under 0.6 s that continues neither neighbour in its video. Warnings:
/// each names the sweep or read that fixes or shows it. Messages carry gmclip's words
/// and times only, never the transcript's text.
fn script_rules(
    doc: &Doc,
    view: &crate::analysis::AnalysisView,
    cut: Id<Cut>,
    f: &mut Vec<Finding>,
) {
    let sc = crate::script::script(doc, view, cut);
    let talks = sc.parts.iter().any(|p| !p.words.is_empty());
    if !talks {
        return;
    }
    for r in &sc.retakes {
        let words = r.earlier.split_whitespace().count();
        let entity = r
            .parts
            .first()
            .and_then(|&i| sc.parts.get(i))
            .map(|p| p.part.entity());
        f.push(find(
            "retake",
            Level::Warning,
            format!(
                "a retake at {}: {words} word(s) said again just after; sweep_retakes strikes the earlier attempt",
                clock(r.at)
            ),
            entity,
        ));
    }
    let gap = (crate::script::GAP_S * SEC as f64) as i64;
    // a part continues another when one starts where the other ends in the same video
    let joins = |a: &crate::script::PartScript, b: &crate::script::PartScript| {
        a.media == b.media && (b.src_in - a.src_out).abs() <= SEC / 100
    };
    for (k, p) in sc.parts.iter().enumerate() {
        let e = Some(p.part.entity());
        let prev = k.checked_sub(1).and_then(|j| sc.parts.get(j));
        let next = sc.parts.get(k + 1);
        // decision 0049: a short wordless part from another moment of the video flashes
        // another pose between two takes (jump, flash, jump)
        if p.silent()
            && p.dur < SEC * 6 / 10
            && !prev.is_some_and(|q| joins(q, p))
            && !next.is_some_and(|q| joins(p, q))
        {
            let mut x = find(
                "part_filler_elsewhere",
                Level::Warning,
                format!(
                    "part {} at {} fills {:.2} s with another moment of the video, which flashes between the takes; play on from the part before instead",
                    p.index,
                    clock(p.start),
                    p.dur as f64 / SEC as f64
                ),
                e,
            );
            x.fix = prev.map(|q| Fix {
                label: "Play on from the part before instead".into(),
                command: "cut_edit".into(),
                args: serde_json::json!({"cut": cut, "ops": [
                    {"op": "part_delete", "part": p.part},
                    {"op": "part_trim", "part": q.part, "src_out": q.src_out + (p.src_out - p.src_in)},
                ]}),
            });
            f.push(x);
        }
        if p.silent() {
            if p.dur >= gap {
                f.push(find(
                    "silent_part",
                    Level::Warning,
                    format!(
                        "part {} at {} says nothing for {:.1} s in a cut that talks (B-roll belongs on a video overlay over speech)",
                        p.index,
                        clock(p.start),
                        p.dur as f64 / SEC as f64
                    ),
                    e,
                ));
            }
            continue;
        }
        if !p.transcript {
            continue;
        }
        let dead: i64 = [p.lead, p.trail].iter().filter(|&&d| d >= gap).sum::<i64>()
            + p.gaps.iter().map(|g| g.len).sum::<i64>();
        if dead > 0 {
            f.push(find(
                "dead_air",
                Level::Warning,
                format!(
                    "part {} at {} has {:.1} s of silence of a second or more around or between its words; sweep_pauses removes it",
                    p.index,
                    clock(p.start),
                    dead as f64 / SEC as f64
                ),
                e,
            ));
        }
        if p.starts_mid_sentence || p.ends_mid_sentence {
            let which = match (p.starts_mid_sentence, p.ends_mid_sentence) {
                (true, true) => "starts and ends",
                (true, false) => "starts",
                _ => "ends",
            };
            f.push(find(
                "mid_sentence",
                Level::Warning,
                format!(
                    "part {} at {} {which} mid-sentence (cut_get projection words shows it); move the edge to the sentence's start or end",
                    p.index,
                    clock(p.start)
                ),
                e,
            ));
        }
        if p.cuts_word_in.is_some() || p.cuts_word_out.is_some() {
            f.push(find(
                "cuts_word",
                Level::Warning,
                format!(
                    "part {} at {} has an edge inside a word; trim to a word edge (strike or restore words instead of typing times)",
                    p.index,
                    clock(p.start)
                ),
                e,
            ));
        }
    }
}

/// Lints one cut (without transcripts: word anchors count as typed).
pub fn lint_cut(doc: &Doc, cut: Id<Cut>) -> Vec<Finding> {
    lint_cut_in(doc, None, cut)
}

/// Lints one cut, judging word anchors against the project's transcripts.
pub fn lint_cut_in(
    doc: &Doc,
    view: Option<&crate::analysis::AnalysisView>,
    cut: Id<Cut>,
) -> Vec<Finding> {
    let mut f = Vec::new();
    let Some(c) = doc.get(cut) else { return f };
    let parts: Vec<&Part> = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .collect();
    if parts.is_empty() {
        f.push(find(
            "empty_cut",
            Level::Error,
            "the cut has no parts",
            Some(cut.entity()),
        ));
    }
    if doc.children::<Framing>(cut.entity()).next().is_none() {
        f.push(find(
            "no_framing",
            Level::Warning,
            "the cut has no framing to export",
            Some(cut.entity()),
        ));
    }
    for p in &parts {
        if p.src_out.0 - p.src_in.0 < SEC / 4 {
            f.push(find(
                "part_too_short",
                Level::Warning,
                "a part under 0.25 s",
                Some(p.id.entity()),
            ));
        }
    }
    // agent-typed times: anchors …
    for (e, field, a) in anchors::anchors_of_cut(doc, cut) {
        if anchors::is_typed_time_in(view, &a) && by_agent(doc, e) {
            f.push(find("agent_typed_time", Level::Warning, format!("{field}: a time the agent typed (raw, offset or an unverified word time), not anchored or measured"), Some(e)));
        }
    }
    // … and source times typed into holds, splits and time maps
    for id in doc.children::<ContentHold>(cut.entity()) {
        if doc
            .get(id)
            .is_some_and(|h| matches!(h.at, HoldAt::AtSrc(_)))
            && by_agent(doc, id.entity())
        {
            f.push(find(
                "agent_typed_time",
                Level::Warning,
                "a content hold at a typed source time (pick a frame with see)",
                Some(id.entity()),
            ));
        }
    }
    for p in &parts {
        for v in doc.children::<VisualSplit>(p.id.entity()) {
            if by_agent(doc, v.entity()) {
                f.push(find(
                    "agent_typed_time",
                    Level::Warning,
                    "a visual split at a typed source time",
                    Some(v.entity()),
                ));
            }
        }
    }
    for l in layers_of(doc, cut, &parts) {
        if doc
            .get(l)
            .is_some_and(|x| matches!(x.time_map, TimeMap::Hold(HoldAt::AtSrc(_))))
            && by_agent(doc, l.entity())
        {
            f.push(find(
                "agent_typed_time",
                Level::Warning,
                "a layer held at a typed source time",
                Some(l.entity()),
            ));
        }
    }
    // numbers on screen
    for id in overlays_of(doc, cut) {
        if let Some(o) = doc.get(id) {
            numbers_lint(doc, view, id, o, &mut f);
        }
    }
    // agent grading without measurement
    for id in doc.children::<GainSegment>(cut.entity()) {
        if doc.get(id).is_some_and(|g| g.measurement_ref.is_none()) && by_agent(doc, id.entity()) {
            f.push(find(
                "agent_grading_unmeasured",
                Level::Warning,
                "agent gain with no measure{loudness} cited (agent grading — review)",
                Some(id.entity()),
            ));
        }
    }
    for p in &parts {
        if p.gain_db != 0.0 && by_agent(doc, p.id.entity()) {
            f.push(find(
                "agent_grading_unmeasured",
                Level::Warning,
                "a part's gain set by the agent with no measurement (agent grading — review)",
                Some(p.id.entity()),
            ));
        }
    }
    m2_rules(doc, view, cut, &parts, &mut f);
    if let Some(v) = view {
        script_rules(doc, v, cut, &mut f);
    }
    loop_rule(doc, cut, c, &mut f);
    // the moment: line-edged spans; part one inside the span or the payoff
    if let Some(m) = c.moment.and_then(|m| doc.get(m)) {
        if m.span.edges == Edges::Line {
            f.push(find(
                "line_edged_span_unmeasured",
                Level::Info,
                "the moment's span is line-edged, not measured to word edges",
                Some(m.id.entity()),
            ));
        }
        if let Some(first) = parts.first() {
            let meets = |r: Option<(Flicks, Flicks)>| {
                r.is_some_and(|(a, b)| first.src_in < b && a < first.src_out)
            };
            let span = src_range(doc, &m.span.start, &m.span.end);
            let payoff = m
                .payoff
                .as_ref()
                .and_then(|p| src_range(doc, &p.start, &p.end));
            if (span.is_some() || payoff.is_some()) && !meets(span) && !meets(payoff) {
                f.push(find(
                    "hook_outside_moment",
                    Level::Warning,
                    "part one is outside both the moment's span and its payoff",
                    Some(first.id.entity()),
                ));
            }
        }
    }
    f
}

/// `loop_last_frame` (decision 0025; SPEC §5.13 [M4] "last layout ≠ first", built for a
/// cut set to loop): the last frame should show the opening's layout and crop. The fix
/// gives the last part the opening's layout, or — when the last part has visual splits
/// of its own, or is the opening — a short tail in it.
fn loop_rule(doc: &Doc, cut: Id<Cut>, c: &Cut, f: &mut Vec<Finding>) {
    if c.loop_check != Some(true) {
        return;
    }
    for fid in doc.children::<Framing>(cut.entity()) {
        let Some(fr) = doc.get(fid) else { continue };
        let fpf = crate::planner::framing_fpf(fr.fps).unwrap_or(SEC / 30);
        let Some(st) = crate::polish::loop_state(doc, cut, fid, fpf) else {
            continue;
        };
        if st.matches {
            continue;
        }
        let tail = st.last_has_splits || st.first == st.last;
        let mut x = find(
            "loop_last_frame",
            Level::Warning,
            format!(
                "the last frame does not loop back to the opening ({})",
                st.differences.join(", ")
            ),
            Some(st.last.entity()),
        );
        x.fix = Some(Fix {
            label: if tail {
                "Add a short tail in the opening's layout"
            } else {
                "Use the opening's layout on the last part"
            }
            .into(),
            command: "loop_fix".into(),
            args: serde_json::json!({"cut": cut, "framing": fid, "how": if tail { "tail" } else { "layout" }}),
        });
        f.push(x);
    }
}

/// The picture size of a part's media (a source, or a sync group's picture member).
fn media_dims(doc: &Doc, media: &MediaRef) -> Option<(f64, f64)> {
    let sid = match media {
        MediaRef::Source(s) => Some(*s),
        MediaRef::Group(g) => doc
            .iter::<SyncMember>()
            .find(|m| m.group == *g && m.role == MemberRole::Picture)
            .map(|m| m.source),
    }?;
    let t = doc
        .get(sid)?
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)?;
    Some((f64::from(t.width?), f64::from(t.height?)))
}

/// A rect on the canvas, in the 1080×1920 reference: `[x0, y0, x1, y1]`.
type Rect = [f64; 4];

fn overlaps(a: Rect, b: Rect) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The [M2] core set (see the module doc).
/// The width of a part's picture source (its probe), if known.
fn picture_width(doc: &Doc, p: &Part) -> Option<u32> {
    let src = match p.media {
        MediaRef::Source(s) => s,
        MediaRef::Group(g) => {
            let grp = doc.get(g)?;
            let m = grp.picture.and_then(|m| doc.get(m)).or_else(|| {
                doc.children::<SyncMember>(g.entity())
                    .filter_map(|m| doc.get(m))
                    .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
            })?;
            m.source
        }
    };
    doc.get(src)?
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .and_then(|t| t.width)
}

fn m2_rules(
    doc: &Doc,
    view: Option<&crate::analysis::AnalysisView>,
    cut: Id<Cut>,
    parts: &[&Part],
    f: &mut Vec<Finding>,
) {
    let framings: Vec<&Framing> = doc
        .children::<Framing>(cut.entity())
        .filter_map(|x| doc.get(x))
        .collect();
    // duration
    let dur_s: f64 = parts
        .iter()
        .map(|p| (p.src_out.0 - p.src_in.0).max(0) as f64 / SEC as f64)
        .sum();
    if !parts.is_empty() {
        let vertical = framings.iter().any(|x| x.canvas.h > x.canvas.w);
        let cap = if vertical {
            CAP_VERTICAL_S
        } else {
            CAP_HORIZONTAL_S
        };
        if !framings.is_empty() && dur_s > cap {
            f.push(find(
                "duration_over_cap",
                Level::Warning,
                format!("{dur_s:.1} s is over the framing's {cap:.0} s cap"),
                Some(cut.entity()),
            ));
        }
        if dur_s < BAND_MIN_S {
            f.push(find(
                "duration_under_band",
                Level::Warning,
                format!("{dur_s:.1} s is under {BAND_MIN_S:.0} s"),
                Some(cut.entity()),
            ));
        }
    }
    // overlapping source (part one, the payoff-first opening, may replay a later take:
    // AJ's loop and payoff openings, calibrated on h88)
    for (i, a) in parts.iter().enumerate().skip(1) {
        for b in &parts[i + 1..] {
            if a.media != b.media {
                continue;
            }
            let ov = a.src_out.0.min(b.src_out.0) - a.src_in.0.max(b.src_in.0);
            if ov > SEC / 2 {
                f.push(find(
                    "overlapping_source",
                    Level::Warning,
                    format!(
                        "two parts play the same {:.1} s of the source",
                        ov as f64 / SEC as f64
                    ),
                    Some(b.id.entity()),
                ));
            }
        }
    }
    // screen text likely too small (SPEC §5.7 activity crop): a visible screen layer
    // shown at under 0.6 canvas px per source px, at each part's start and each cut
    // visual split
    for fr in &framings {
        for p in parts {
            let Some(src_w) = picture_width(doc, p) else {
                continue;
            };
            let mut at = vec![p.src_in];
            at.extend(
                gmclip_model::effective::splits_of(doc, p.id, fr.id)
                    .iter()
                    .filter(|s| s.at_src > p.src_in && s.at_src < p.src_out)
                    .map(|s| s.at_src),
            );
            let small = at.iter().any(|t| {
                gmclip_model::effective::effective(doc, p.id, fr.id, *t).is_some_and(|st| {
                    st.layers.iter().any(|l| {
                        let v = &l.values;
                        v.role == LayerRole::Region(RegionRole::Screen)
                            && v.visible
                            && v.crop.w > 0.0
                            && v.place.w * f64::from(fr.canvas.w) / (v.crop.w * f64::from(src_w))
                                < crate::activity::SMALL_TEXT_SCALE
                    })
                })
            });
            if small {
                // calibrated to info: it fires on AJ's approved h34, h37 and h48 (A2.17)
                f.push(find(
                    "screen_text_small",
                    Level::Info,
                    "screen text likely too small: the screen is shown under 0.6× (crop in, or Follow the action)",
                    Some(p.id.entity()),
                ));
            }
        }
    }
    // frame rates
    if framings.windows(2).any(|w| w[0].fps != w[1].fps) {
        f.push(find(
            "framings_fps_differ",
            Level::Warning,
            "the cut's framings have different frame rates",
            Some(cut.entity()),
        ));
    }
    // the term gate over the words the captions show
    let cd = doc
        .children::<CaptionDoc>(cut.entity())
        .find_map(|x| doc.get(x));
    let terms: Vec<Term> = doc
        .iter::<Terms>()
        .flat_map(|t| t.entries.values().cloned())
        .collect();
    if let (Some(view), Some(cd)) = (view, cd)
        && cd.enabled
        && !terms.is_empty()
    {
        let mut words = Vec::new();
        let mut revs: Vec<String> = Vec::new();
        for p in parts {
            let owner = match p.media {
                MediaRef::Source(s) => s.0.to_string(),
                MediaRef::Group(g) => g.0.to_string(),
            };
            let Some(t) = view.active(doc, &owner) else {
                continue;
            };
            for (i, w) in t.words.iter().enumerate() {
                if w.end <= p.src_in.0 || w.start >= p.src_out.0 {
                    continue;
                }
                let id = gmclip_model::anchor::WordId {
                    asr_revision: t.revision.clone(),
                    ordinal: i as u32,
                };
                let text = match cd.edits.get(&id) {
                    Some(CaptionEdit::Drop) => continue,
                    Some(CaptionEdit::Text(x)) => x.clone(),
                    None => w.text.clone(),
                };
                words.push(crate::terms::GateWord {
                    ordinal: i as u32,
                    text,
                });
                revs.push(t.revision.clone());
            }
        }
        for g in crate::terms::gate(&words, &terms) {
            let mut x = find(
                "term_gate",
                Level::Warning,
                format!(
                    "the captions may spell {} wrong ({})",
                    clip16(&g.written),
                    clip16(&g.shown)
                ),
                Some(cd.id.entity()),
            );
            // §5.12: the suggested correction, applied only by the human's click — the
            // first word shows the term, the rest of the run is dropped from the captions
            if let Some(at) = words
                .iter()
                .position(|w| Some(&w.ordinal) == g.ordinals.first())
            {
                let rev = &revs[at];
                let ops: Vec<serde_json::Value> = g
                    .ordinals
                    .iter()
                    .enumerate()
                    .map(|(k, o)| {
                        serde_json::json!({"op": "caption_edit",
                            "word": {"asr_revision": rev, "ordinal": o},
                            "edit": if k == 0 { serde_json::json!({"text": g.written}) } else { serde_json::json!("drop") }})
                    })
                    .collect();
                x.fix = Some(Fix {
                    label: "Use the project's spelling".into(),
                    command: "cut_edit".into(),
                    args: serde_json::json!({"cut": cut, "ops": ops}),
                });
            }
            f.push(x);
        }
    }
    // geometry, per vertical framing, on the 1080×1920 reference
    for fr in framings.iter().filter(|x| x.canvas.h > x.canvas.w) {
        let (cw, ch) = (f64::from(fr.canvas.w), f64::from(fr.canvas.h));
        let (sx, sy) = (1080.0 / cw.max(1.0), 1920.0 / ch.max(1.0));
        let px = |x: f64| x * cw * sx;
        let py = |y: f64| y * ch * sy;
        // the hook band and the zones for text
        for id in overlays_of(doc, cut) {
            let Some(o) = doc.get(id) else { continue };
            if o.role.as_deref() != Some("text_hook") {
                continue;
            }
            let (visible, geom) = gmclip_model::effective::effective_overlay_place(o, fr.id);
            let OverlayGeom::Place(pl) = geom else {
                continue;
            };
            let y = py(pl.y);
            if visible && !(HOOK_BAND.0..=HOOK_BAND.1).contains(&y) {
                f.push(find(
                    "hook_outside_band",
                    Level::Warning,
                    format!(
                        "the text hook's centre is at y {y:.0}, outside the hook band (220–420)"
                    ),
                    Some(id.entity()),
                ));
            }
        }
        let cap_y = cd
            .filter(|c| c.enabled)
            .and(fr.caption_place)
            .map(|p| py(p.y));
        if let Some(y) = cap_y
            && !(ZONE_TOP..=ZONE_BOTTOM).contains(&y)
        {
            f.push(find(
                "overlay_zone",
                Level::Warning,
                format!("captions at y {y:.0} sit in Instagram's overlay zone"),
                Some(fr.id.entity()),
            ));
        }
        // per part: the face's box, and private regions under text; one "Cover chat and
        // alerts" for the whole cut in this framing
        let mut cover_ops: Vec<serde_json::Value> = Vec::new();
        let mut private_findings: Vec<usize> = Vec::new();
        for p in parts {
            let Some(st) = gmclip_model::effective::effective(doc, p.id, fr.id, p.src_in) else {
                continue;
            };
            let Some((sw, sh)) = media_dims(doc, &p.media) else {
                continue;
            };
            // cover layers (SPEC §5.7) hide what is under them: where they land
            let covers: Vec<Rect> = st
                .layers
                .iter()
                .filter(|l| {
                    l.values.kind == LayerKind::Cover
                        && l.values.cover.is_some()
                        && l.values.visible
                        && l.values.opacity >= 0.99
                        && l.values.crop.w > 0.0
                        && l.values.crop.h > 0.0
                })
                .map(|l| {
                    let (c, pl) = (l.values.crop, l.values.place);
                    let w = px(pl.w);
                    let h = w * (c.h * sh) / (c.w * sw);
                    let (x0, y0) = (px(pl.x), py(pl.y));
                    [x0, y0, x0 + w, y0 + h]
                })
                .collect();
            let shown_layers: Vec<&gmclip_model::effective::ResolvedLayer> = st
                .layers
                .iter()
                .filter(|l| l.values.kind != LayerKind::Cover)
                .collect();
            let boxes: Vec<(LayerRole, NormRectLike, Rect)> = shown_layers
                .iter()
                .filter(|l| l.values.visible && l.values.crop.w > 0.0 && l.values.crop.h > 0.0)
                .map(|l| {
                    let (c, pl) = (l.values.crop, l.values.place);
                    let w = px(pl.w);
                    let h = w * (c.h * sh) / (c.w * sw);
                    let (x0, y0) = (px(pl.x), py(pl.y));
                    (
                        l.values.role,
                        NormRectLike {
                            x: c.x,
                            y: c.y,
                            w: c.w,
                            h: c.h,
                        },
                        [x0, y0, x0 + w, y0 + h],
                    )
                })
                .collect();
            for (role, _, r) in &boxes {
                if *role == LayerRole::Region(RegionRole::Cam) {
                    let in_right =
                        r[2] > ZONE_RIGHT_X && r[1] < ZONE_RIGHT_Y.1 && r[3] > ZONE_RIGHT_Y.0;
                    if r[1] < ZONE_TOP || r[3] > ZONE_BOTTOM + FACE_BOTTOM_ALLOW || in_right {
                        f.push(find(
                            "overlay_zone",
                            Level::Warning,
                            format!(
                                "the face ({:.0}–{:.0} px) runs into Instagram's overlay zone",
                                r[1], r[3]
                            ),
                            Some(p.id.entity()),
                        ));
                    }
                }
            }
            // private-class regions of the part's picture source, where they land
            let regions: Vec<(RegionRole, NormRect)> = match p.media {
                MediaRef::Source(s) => doc.get(s).and_then(|x| x.stream_layout),
                MediaRef::Group(g) => doc
                    .iter::<SyncMember>()
                    .find(|m| m.group == g && m.role == MemberRole::Picture)
                    .and_then(|m| doc.get(m.source))
                    .and_then(|x| x.stream_layout),
            }
            .and_then(|l| doc.get(l))
            .map(|l| {
                l.regions
                    .iter()
                    .filter(|r| r.role.is_private_class())
                    .map(|r| (r.role, r.rect))
                    .collect()
            })
            .unwrap_or_default();
            if regions.is_empty() {
                continue;
            }
            let mut texts: Vec<Rect> = Vec::new();
            if let Some(y) = cap_y {
                texts.push([54.0, y - 60.0, 1026.0, y + 60.0]);
            }
            for id in overlays_of(doc, cut) {
                if let Some(o) = doc.get(id)
                    && o.text.is_some()
                    && let (true, OverlayGeom::Place(pl)) =
                        gmclip_model::effective::effective_overlay_place(o, fr.id)
                {
                    let (x, y, w) = (px(pl.x), py(pl.y), px(pl.w));
                    texts.push([x - w / 2.0, y - 60.0, x + w / 2.0, y + 60.0]);
                }
            }
            // every private region a shown layer puts on the canvas and no cover hides:
            // (layer, region role, the region's part inside the crop, where it lands)
            let mut uncovered: Vec<(usize, RegionRole, NormRect, Rect)> = Vec::new();
            for (k, (_, c, r)) in boxes.iter().enumerate() {
                for (role, g) in &regions {
                    // the region's part inside this layer's crop, mapped to the canvas
                    let (ix0, iy0) = (g.x.max(c.x), g.y.max(c.y));
                    let (ix1, iy1) = ((g.x + g.w).min(c.x + c.w), (g.y + g.h).min(c.y + c.h));
                    if ix1 <= ix0 || iy1 <= iy0 {
                        continue;
                    }
                    let mx = |u: f64| r[0] + (u - c.x) / c.w * (r[2] - r[0]);
                    let my = |v: f64| r[1] + (v - c.y) / c.h * (r[3] - r[1]);
                    let shown = [mx(ix0), my(iy0), mx(ix1), my(iy1)];
                    if covers.iter().any(|cv| contains(*cv, shown)) {
                        continue;
                    }
                    let inside = NormRect {
                        x: ix0,
                        y: iy0,
                        w: ix1 - ix0,
                        h: iy1 - iy0,
                    };
                    uncovered.push((k, *role, inside, shown));
                }
            }
            cover_ops.extend(cover_ops_of(
                p.id,
                fr.id,
                &shown_layers,
                &boxes,
                &uncovered,
                cw,
            ));
            for (_, _, _, shown) in &uncovered {
                if texts.iter().any(|t| overlaps(*t, *shown)) {
                    private_findings.push(f.len());
                    f.push(find(
                        "private_region_under_text",
                        Level::Error,
                        "captions or a text overlay sit over a private region of the screen",
                        Some(p.id.entity()),
                    ));
                }
            }
        }
        if !private_findings.is_empty() && !cover_ops.is_empty() {
            let fix = Fix {
                label: "Cover chat and alerts".into(),
                command: "layout_edit".into(),
                args: serde_json::json!({"cut": cut, "ops": cover_ops}),
            };
            for i in private_findings {
                f[i].fix = Some(fix.clone());
            }
        }
    }
}

/// Whether box `outer` holds box `inner` (canvas px, ½ px of slack).
fn contains(outer: Rect, inner: Rect) -> bool {
    outer[0] <= inner[0] + 0.5
        && outer[1] <= inner[1] + 0.5
        && outer[2] >= inner[2] - 0.5
        && outer[3] >= inner[3] - 0.5
}

/// The blur a "Cover chat and alerts" cover gets: 24 px on a 1080-wide canvas.
const COVER_BLUR_1080: f64 = 24.0;

/// The ops of "Cover chat and alerts" (SPEC §5.7, the one-click fix of
/// `private_region_under_text`, one `layout_edit` for the cut) for one part: for every
/// private region a layer of the part shows uncovered, a blurred cover layer of the part
/// (its own addition in this framing) over exactly where that region lands, just above
/// the layer showing it. Applied only by the human's click.
fn cover_ops_of(
    part: Id<Part>,
    framing: Id<Framing>,
    layers: &[&gmclip_model::effective::ResolvedLayer],
    boxes: &[(LayerRole, NormRectLike, Rect)],
    uncovered: &[(usize, RegionRole, NormRect, Rect)],
    canvas_w: f64,
) -> Vec<serde_json::Value> {
    // `boxes` keeps the shown layers that pass its filter, in order: pair them back up
    let shown: Vec<&&gmclip_model::effective::ResolvedLayer> = layers
        .iter()
        .filter(|l| l.values.visible && l.values.crop.w > 0.0 && l.values.crop.h > 0.0)
        .collect();
    if shown.len() != boxes.len() {
        return Vec::new();
    }
    let radius = (COVER_BLUR_1080 * canvas_w / 1080.0).clamp(1.0, 400.0);
    uncovered
        .iter()
        .map(|(k, role, inside, _)| {
            let v = &shown[*k].values;
            let (c, pl, r) = (v.crop, v.place, boxes[*k].2);
            // the layer's height on the 1080×1920 reference, as a fraction of the canvas
            let h = (r[3] - r[1]) / 1920.0;
            let place = gmclip_model::geometry::Place {
                x: pl.x + (inside.x - c.x) / c.w * pl.w,
                y: pl.y + (inside.y - c.y) / c.h * h,
                w: inside.w / c.w * pl.w,
            };
            serde_json::json!({
                "op": "layer_add",
                "owner": LayerOwner::PartOverride { part, framing },
                "role": LayerRole::Region(*role),
                "kind": LayerKind::Cover,
                "cover": CoverStyle::Blur { radius },
                "crop": inside,
                "place": place,
                "z": v.z.saturating_add(1),
            })
        })
        .collect()
}

/// A crop rect (normalised), carried with its canvas box.
#[derive(Copy, Clone, Debug)]
struct NormRectLike {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// R-LINT-2: an agent-started export with an error-level finding needs approval.
pub fn blocks_agent_export(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.level == Level::Error)
}

pub(crate) fn clip16(s: &str) -> &str {
    let mut end = s.len().min(16);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_rounding_and_tokens() {
        let n = numbers("DONE IN 1:07 on a 5080, 67.4 s, 1:02:03, 5.0.1, ٣");
        let vals: Vec<Option<f64>> = n.iter().map(|x| x.1.map(|s| s.value)).collect();
        assert_eq!(
            vals,
            [
                Some(67.0),
                Some(5080.0),
                Some(67.4),
                Some(3723.0),
                None,
                None
            ]
        );
        let s0 = n[0].1.unwrap();
        assert!(agrees(s0, 67.4) && !agrees(s0, 68.0));
        let d = n[2].1.unwrap();
        assert!(agrees(d, 67.44) && !agrees(d, 67.46));
        let i = numbers("67 seconds")[0].1.unwrap();
        assert!(agrees(i, 67.9) && agrees(i, 66.0) && !agrees(i, 68.5));
        assert_eq!(
            numbers("1:75")[0].1,
            None,
            "ss ≥ 60 does not parse (so it needs provenance)"
        );
    }
}

#[cfg(test)]
mod name_digits {
    use super::numbers;

    #[test]
    fn digits_in_names_are_not_figures() {
        let t = |s: &str| numbers(s).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert!(t("0xSero").is_empty());
        assert!(t("@sero2 on H100").is_empty());
        assert_eq!(t("RTX 3090, 24GB, 35B, 4x"), ["3090", "24", "35", "4"]);
    }
}
