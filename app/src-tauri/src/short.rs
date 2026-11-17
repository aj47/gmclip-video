//! The core of a Short (backlog U.09; AGENTS.md "Reel / Short style"): captions on with
//! the story preset, a text hook, section pills and a measured run clock. Everything is
//! registry commands (font_add, text_style_set, caption_set, framing_set, caption_edit,
//! overlay_add/overlay_set/overlay_delete, measurement_add), so agents can do the same.
//!
//! - **Story preset**: Neometric Heavy (the face `assets/fonts/faces.json` names), styles
//!   `Speech` 86 (white, outline 12, shadow 7), `Hook` 68 (TikTok's look: black on a
//!   white rounded box per line) and `Pill` 50 (boxed, dark box, padding 16), libass
//!   sizes on a 1080 short side; captions grouped `story_v5`
//!   (1–3 words, built by the planner from the cut's own words), seated at y 1065 of 1920
//!   in 9:16 and 84 % down in 16:9.
//! - **Corrections**: "XD.dev → exe.dev" re-spells every matching word of the cut in
//!   captions (one `cut_edit` of `caption_edit`s); audio is untouched.
//! - **Text hook**: from frame 0 until the end of a chosen word (or a few seconds), in
//!   its band (y ≈ 220–400 of 1920).
//! - **Section pill**: a boxed label top-left at (48, 262), over a part.
//! - **Run clock**: mark the run's start and end on the preview (a measurement from the
//!   source frames); the clock pill counts source time through jump cuts ("GENERATING
//!   0:23" → "DONE IN 1:07").

use gmclip_model::Flicks;
use gmclip_model::anchor::{WordId, WordRef};
use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{
    CaptionDoc, CaptionSource, Cut, Font, Framing, Measurement, Overlay, OverlayEnd, Part, Styles,
};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

const SHORT: f64 = 1080.0;
const FONT: &str = "Neometric Heavy";

fn u(px: f64) -> f64 {
    px * 1000.0 / SHORT
}

fn out(v: &Value) -> Value {
    v["output"].clone()
}

/// The story face (the bundle or the user's fonts): (family, style, sha256, em_per_size).
fn story_font() -> AppResult<(String, String, String, f64)> {
    let f = crate::fonts::by_name(FONT).ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "Neometric Heavy is not available (not in the app, not in your font folders)",
        )
    })?;
    Ok((
        f.family.clone(),
        f.style.clone(),
        f.sha256.clone(),
        f.em_per_size,
    ))
}

fn plain(size: f64, em: f64) -> Value {
    json!({
        "font": FONT, "size": size * em * 1000.0 / SHORT, "fill": "#FFFFFFFF",
        "outline": {"w": u(12.0), "colour": "#000000FF"},
        "shadow": {"dx": u(7.0), "dy": u(7.0), "blur": 0.0, "colour": "#000000FF"},
        "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0,
        "letter_spacing": 0.0, "case": "as_is", "pill": null,
    })
}

/// The text hook's look: TikTok's default text style, black on a white box per line with
/// rounded corners (house plate: opaque white, radius and padding at 1080 px), no outline.
fn hook(em: f64) -> Value {
    json!({
        "font": FONT, "size": 68.0 * em * 1000.0 / SHORT, "fill": "#000000FF",
        "outline": null, "shadow": null,
        "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0,
        "letter_spacing": 0.0, "case": "as_is",
        "pill": {"on": true, "colour": "#FFFFFF", "opacity": 1.0,
                 "radius": u(16.0), "pad_x": u(26.0), "pad_y": u(12.0), "per_line": true},
    })
}

fn pill(em: f64) -> Value {
    json!({
        "font": FONT, "size": 50.0 * em * 1000.0 / SHORT, "fill": "#FFFFFFFF",
        "outline": null, "shadow": null,
        "align": "left", "max_width": null, "fit": "none", "line_height": 1.0,
        "letter_spacing": u(1.0), "case": "as_is",
        "pill": {"on": true, "colour": "#101010", "opacity": (255.0 - 48.0) / 255.0,
                 "radius": 0.0, "pad_x": u(16.0), "pad_y": u(16.0)},
    })
}

/// Makes sure the project has the reference face and the Speech / Hook / Pill styles.
pub fn ensure_presets(core: &AppCore, project: &str) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let styles = doc.iter::<Styles>().next();
    let have = |n: &str| styles.is_some_and(|s| s.entries.contains_key(n));
    let has_font = doc
        .iter::<Font>()
        .any(|f| gmclip_core::planner::style::font_name(&f.family, &f.style) == FONT);
    // a Hook still on the old default (white, outlined) moves to the boxed look; one a
    // person restyled stays as it is
    let (family, style, sha, em) = story_font()?;
    let old_hook =
        serde_json::from_value::<gmclip_model::entities::TextStyle>(plain(88.0, em)).ok();
    let stale_hook = styles
        .and_then(|s| s.entries.get("Hook"))
        .is_some_and(|h| old_hook.as_ref() == Some(h));
    if has_font && have("Speech") && have("Hook") && have("Pill") && !stale_hook {
        return Ok(());
    }
    if !has_font {
        core.call(
            project,
            "font_add",
            json!({"family": family, "style": style, "sha256": sha, "em_per_size": em}),
        )?;
    }
    for (name, st) in [
        ("Speech", plain(86.0, em)),
        ("Hook", hook(em)),
        ("Pill", pill(em)),
    ] {
        if !have(name) || (name == "Hook" && stale_hook) {
            core.call(
                project,
                "text_style_set",
                json!({"name": name, "style": st}),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn style_of(core: &AppCore, project: &str, name: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    doc.iter::<Styles>()
        .next()
        .and_then(|s| s.entries.get(name).cloned())
        .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
        .ok_or_else(|| AppError::internal("the story styles are missing"))
}

fn portrait(core: &AppCore, project: &str, cut: &str) -> bool {
    core.active_framing(project, cut)
        .and_then(|f| {
            let p = core.project(project).ok()?;
            let doc = p.handle.snapshot().doc.clone();
            let fr = doc.get(ulid_id::<Framing>(&f).ok()?)?;
            Some(fr.canvas.h > fr.canvas.w)
        })
        .unwrap_or(true)
}

/// Captions on (Story) or off.
pub fn captions_set(core: &AppCore, project: &str, cut: &str, on: bool) -> AppResult<Value> {
    if !on {
        return core.call(
            project,
            "caption_set",
            json!({"cut": cut, "enabled": false}),
        );
    }
    ensure_presets(core, project)?;
    let framing = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
    let y = if portrait(core, project, cut) {
        1065.0 / 1920.0
    } else {
        0.84
    };
    // a seat the framing already has (a profile's pinned seat, a caption height) stays
    let seated = core.project(project).ok().is_some_and(|p| {
        let doc = &p.handle.snapshot().doc;
        ulid_id::<Framing>(&framing)
            .ok()
            .and_then(|f| doc.get(f))
            .is_some_and(|f| f.caption_place.is_some())
    });
    let mut op = json!({"op": "framing_set", "framing": framing, "caption_grouping": "story_v5"});
    if !seated {
        op["caption_place"] = json!({"x": 0.5, "y": y, "w": 0.9});
    }
    core.call(project, "layout_edit", json!({"cut": cut, "ops": [op]}))?;
    core.call(
        project,
        "caption_set",
        json!({"cut": cut, "enabled": true, "source": "heard", "drop_fillers": true}),
    )
}

/// The terms the gate checks: the one term list (`crate::termlist`) — the project's, the
/// profile's `profile.json` terms and the terms saved for the profile.
fn gate_terms(core: &AppCore, doc: &gmclip_model::Doc) -> Vec<gmclip_model::entities::Term> {
    crate::termlist::term_list(core, doc)
}

/// The project's caption corrections (heard → shown) on the gate's words, as the captions
/// apply them: a word or run the corrections re-spell is checked as it will be shown.
fn corrected(
    doc: &gmclip_model::Doc,
    words: Vec<gmclip_core::terms::GateWord>,
) -> Vec<gmclip_core::terms::GateWord> {
    let Some(c) = doc
        .iter::<gmclip_model::entities::CaptionCorrections>()
        .next()
    else {
        return words;
    };
    let as_terms: Vec<gmclip_model::entities::Term> = c
        .entries
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| gmclip_model::entities::Term {
            written: v.clone(),
            heard_as: vec![k.clone()],
        })
        .collect();
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let fixes = gmclip_core::terms::corrections(&texts, &as_terms);
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    for f in fixes {
        out.extend(words[i..f.at].iter().cloned());
        out.push(gmclip_core::terms::GateWord {
            ordinal: words[f.at].ordinal,
            text: f.text.clone(),
        });
        i = f.at + f.len;
    }
    out.extend(words[i..].iter().cloned());
    out
}

/// The term gate over the words this cut's captions show (D.04 / A.09): names the
/// speech model got wrong, with the right spelling. It reports and never applies.
pub fn term_gate(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let terms = gate_terms(core, &doc);
    let pl = core.planned(project, cut)?;
    let cd = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id))
        .cloned();
    let t = crate::words::transcript_of(core, &p, &doc, cid).map(|x| x.2);
    let mut words = Vec::new();
    let mut rev = String::new();
    let mut at: std::collections::BTreeMap<u32, u64> = Default::default();
    for (ev, ws) in &pl.index.cues {
        let start = pl
            .plan
            .text_events
            .as_ref()
            .and_then(|e| e.iter().find(|e| e.id.as_str() == ev))
            .map_or(0, |e| e.out.start());
        for w in ws {
            rev = w.asr_revision.clone();
            let text = match cd.as_ref().and_then(|c| c.edits.get(w)) {
                Some(gmclip_model::entities::CaptionEdit::Text(x)) => x.clone(),
                Some(gmclip_model::entities::CaptionEdit::Drop) => continue,
                None => t
                    .as_ref()
                    .and_then(|t| t.words.get(w.ordinal as usize))
                    .map(|x| x.text.clone())
                    .unwrap_or_default(),
            };
            at.insert(w.ordinal, start);
            words.push(gmclip_core::terms::GateWord {
                ordinal: w.ordinal,
                text,
            });
        }
    }
    let words = corrected(&doc, words);
    let found = gmclip_core::terms::gate(&words, &terms);
    Ok(json!({
        "rev": rev,
        "terms": terms.len(),
        "findings": found.iter().map(|f| json!({
            "ordinals": f.ordinals, "shown": f.shown, "written": f.written, "kind": f.kind,
            "frame": f.ordinals.first().and_then(|o| at.get(o)),
        })).collect::<Vec<_>>(),
    }))
}

/// The captions' height on the frame: their centre at `y_px` canvas pixels (reel plans
/// set `caption_y`, e.g. 1110 under a taller panel); the whole cut, the framing shown.
pub fn caption_y_set(core: &AppCore, project: &str, cut: &str, y_px: f64) -> AppResult<Value> {
    let framing = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
    let pl = core.planned(project, cut)?;
    let ch = pl.plan.output.canvas.h.get() as f64;
    if !y_px.is_finite() || y_px <= 0.0 || y_px >= ch {
        return Err(AppError::invalid("a height inside the frame, in pixels"));
    }
    core.call(
        project,
        "layout_edit",
        json!({"cut": cut, "ops": [{"op": "framing_set", "framing": framing,
               "caption_place": {"x": 0.5, "y": y_px / ch, "w": 0.9}}]}),
    )
}

/// The caption panel: on/off, the corrections (re-spellings), the overlays.
pub fn short_view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let cd = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id))
        .cloned();
    let pl = core.planned(project, cut).ok();
    let fpf = pl
        .as_ref()
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let words = crate::words::transcript_of(core, &p, &doc, cid).map(|x| x.2);
    // Corrections, grouped by heard → shown.
    let mut fixes: std::collections::BTreeMap<(String, String), usize> = Default::default();
    if let (Some(cd), Some(t)) = (&cd, &words) {
        for (id, e) in &cd.edits {
            if let gmclip_model::entities::CaptionEdit::Text(to) = e
                && let Some(w) = t.words.get(id.ordinal as usize)
            {
                *fixes.entry((w.text.clone(), to.clone())).or_default() += 1;
            }
        }
    }
    let overlays: Vec<Value> = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|o| {
            let text: String = o
                .text
                .as_ref()
                .map(|t| t.runs.iter().map(|r| r.text.as_str()).collect())
                .unwrap_or_default();
            let at = |a: &gmclip_model::anchor::Anchor| {
                gmclip_core::anchors::resolve(&doc, Some(cid), a, "/")
                    .ok()
                    .and_then(|r| gmclip_core::anchors::to_cut(&doc, cid, r.src, None))
                    .map(|t| t.0 / fpf)
            };
            let start = at(&o.start);
            let end = match &o.end {
                OverlayEnd::At(a) => at(a),
                OverlayEnd::DurMs(ms) => start.map(|s| s + i64::from(*ms) * FLICKS / 1000 / fpf),
            };
            json!({"id": o.id.0.to_string(), "role": o.role, "text": text,
                   "counter": o.counter.as_ref().map(|c| json!({"running": c.running_label, "done": c.done_label})),
                   "start": start, "end": end})
        })
        .collect();
    let hook_end = overlays
        .iter()
        .find(|x| x["role"] == "text_hook")
        .and_then(|x| x["end"].as_i64());
    let measurements: Vec<Value> = doc
        .iter::<Measurement>()
        .filter(|m| m.unit == "s")
        .map(|m| json!({"id": m.id.0.to_string(), "value": m.value, "method": m.method}))
        .collect();
    // The built cues and their words, for the split / join tool.
    let cues: Vec<Value> = pl
        .as_ref()
        .map(|pl| {
            pl.index
                .cues
                .iter()
                .filter_map(|(ev, ws)| {
                    let e = pl
                        .plan
                        .text_events
                        .as_ref()?
                        .iter()
                        .find(|e| e.id.as_str() == ev)?;
                    let text: String = e.runs.iter().map(|r| r.text.as_str()).collect();
                    let words: Vec<Value> = ws
                        .iter()
                        .enumerate()
                        .map(|(i, w)| {
                            let star = e.runs.get(i).is_some_and(|r| r.fill.is_some());
                            let shown = cd
                                .as_ref()
                                .and_then(|c| match c.edits.get(w) {
                                    Some(gmclip_model::entities::CaptionEdit::Text(t)) => {
                                        Some(t.clone())
                                    }
                                    _ => None,
                                })
                                .or_else(|| {
                                    words
                                        .as_ref()
                                        .and_then(|t| t.words.get(w.ordinal as usize))
                                        .map(|x| x.text.clone())
                                })
                                .unwrap_or_default();
                            let brk = cd.as_ref().and_then(|c| c.cue_breaks.get(w)).copied();
                            json!({"rev": w.asr_revision, "ordinal": w.ordinal, "text": shown, "break": brk, "star": star})
                        })
                        .collect();
                    Some(json!({"id": ev, "text": text.trim(), "start": e.out.0[0], "end": e.out.0[1], "words": words}))
                })
                .collect()
        })
        .unwrap_or_default();
    let hand_breaks = cd.as_ref().map_or(0, |c| c.cue_breaks.len());
    Ok(json!({
        "cues": cues,
        "hand_breaks": hand_breaks,
        "keyword_terms": cd.as_ref().map(|c| c.keyword_terms.clone()).unwrap_or_default(),
        "caption_y": pl.as_ref().and_then(|pl| {
            pl.plan.text_events.as_ref()?.iter()
                .find(|e| e.role == gmclip_plan::types::TextRole::Caption)
                .map(|e| e.pos.y)
        }),
        "keep": doc.iter::<gmclip_model::entities::KeepPairs>().next()
            .map(|k| k.entries.values().map(|w| w.join(" ")).collect::<Vec<_>>())
            .unwrap_or_default(),
        "captions": cd.as_ref().is_some_and(|c| c.enabled && gmclip_core::planner::builds_from_words(c.source)),
        "imported_captions": cd.as_ref().is_some_and(|c| c.enabled && c.source == CaptionSource::Imported),
        "fixes": fixes.into_iter().map(|((from, to), n)| json!({"from": from, "to": to, "n": n})).collect::<Vec<_>>(),
        "overlays": overlays,
        "measurements": measurements,
        "transcript": words.is_some(),
        "hook_layout": hook_end.and_then(|end| {
            let cv = core.cut_view(project, cut).ok()?;
            let (at, take) = crate::takelayout::hook_layout_breaks(&cv, end)?;
            Some(json!({"changes_at": at, "take": take, "hook_end": end}))
        }),
    }))
}

fn norm(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '\'')
        .to_lowercase()
}

/// Re-spells every word of the cut's transcript that reads `from` (case and edge
/// punctuation ignored) as `to` in captions; an empty `to` clears those re-spellings.
pub fn caption_fix(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: &str,
    to: &str,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let (_, rev, t, _) = crate::words::transcript_of(core, &p, &doc, cid)
        .ok_or_else(|| AppError::new("Unsupported", "no transcript for this cut yet"))?;
    let want = norm(from);
    if want.is_empty() || to.chars().count() > 80 {
        return Err(AppError::invalid(
            "a word to correct, and at most 80 characters",
        ));
    }
    let edit = if to.trim().is_empty() {
        Value::Null
    } else {
        json!({"text": to.trim()})
    };
    let ops: Vec<Value> = t
        .words
        .iter()
        .enumerate()
        .filter(|(_, w)| norm(&w.text) == want)
        .map(|(i, _)| {
            json!({"op": "caption_edit", "word": {"asr_revision": rev, "ordinal": i}, "edit": edit})
        })
        .collect();
    if ops.is_empty() {
        return Err(AppError::not_found("no word in the transcript reads that"));
    }
    let n = ops.len();
    for chunk in ops.chunks(200) {
        core.call(project, "cut_edit", json!({"cut": cut, "ops": chunk}))?;
    }
    Ok(json!({"words": n}))
}

/// The keywords field: a comma list of terms (the reference's `keywords`) starred across the cut.
pub fn keywords_set(core: &AppCore, project: &str, cut: &str, list: &str) -> AppResult<Value> {
    let terms: Vec<&str> = list
        .split([',', '\n'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    core.call(
        project,
        "caption_keywords_set",
        json!({"cut": cut, "terms": terms}),
    )
}

/// The Short tab's split / join tool: `split` starts a cue at a word, `join` keeps it
/// with the word before, `None` clears the hand-made boundary (`caption_cue_break`).
pub fn cue_break(
    core: &AppCore,
    project: &str,
    cut: &str,
    rev: &str,
    ordinal: u64,
    brk: Option<&str>,
) -> AppResult<Value> {
    let b = match brk {
        None => Value::Null,
        Some(b @ ("split" | "join")) => json!(b),
        Some(_) => return Err(AppError::invalid("break: split, join or none")),
    };
    core.call(
        project,
        "caption_cue_break",
        json!({"cut": cut, "word": {"asr_revision": rev, "ordinal": ordinal}, "break": b}),
    )
}

/// Find & replace in captions: each match (consecutive transcript ordinals) shows `to`
/// instead — its first word re-spelled, the rest dropped from captions; an empty `to`
/// clears those re-spellings. Audio is untouched. One transaction per 200 edits.
pub fn caption_replace(
    core: &AppCore,
    project: &str,
    cut: &str,
    rev: &str,
    hits: &[Vec<u64>],
    to: &str,
) -> AppResult<Value> {
    if to.chars().count() > 200 || hits.is_empty() || hits.len() > 10_000 {
        return Err(AppError::invalid(
            "1–10 000 matches and at most 200 characters",
        ));
    }
    let to = to.trim();
    let mut ops = Vec::new();
    for h in hits {
        if h.is_empty() || h.len() > 12 || h.windows(2).any(|w| w[1] != w[0] + 1) {
            return Err(AppError::invalid("a match is 1–12 consecutive words"));
        }
        for (k, o) in h.iter().enumerate() {
            let edit = match (to.is_empty(), k) {
                (true, _) => Value::Null,
                (false, 0) => json!({"text": to}),
                (false, _) => json!("drop"),
            };
            ops.push(json!({"op": "caption_edit", "word": {"asr_revision": rev, "ordinal": o}, "edit": edit}));
        }
    }
    for chunk in ops.chunks(200) {
        core.call(project, "cut_edit", json!({"cut": cut, "ops": chunk}))?;
    }
    Ok(json!({"matches": hits.len()}))
}

pub(crate) fn word_ref(
    core: &AppCore,
    project: &str,
    cut: &str,
    ordinal: u64,
) -> AppResult<WordRef> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let (_, rev, t, _) = crate::words::transcript_of(core, &p, &doc, cid)
        .ok_or_else(|| AppError::new("Unsupported", "no transcript for this cut yet"))?;
    let i = ordinal as usize;
    let w = t
        .words
        .get(i)
        .ok_or_else(|| AppError::invalid("no such word"))?;
    let n = |s: &str| {
        s.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    Ok(WordRef {
        word_id: WordId {
            asr_revision: rev,
            ordinal: ordinal as u32,
        },
        t_src_start: Flicks(w.start),
        t_src_end: Flicks(w.end),
        normalised_text: n(&w.text),
        prev2: (i.saturating_sub(2)..i)
            .filter_map(|j| t.words.get(j))
            .map(|w| n(&w.text))
            .collect(),
        next2: (i + 1..i + 3)
            .filter_map(|j| t.words.get(j))
            .map(|w| n(&w.text))
            .collect(),
    })
}

/// A hook line wrapped for its band: lines of at most ~22 characters (reference keeps hook
/// lines about that long so they fit 1000 px), split at the spaces nearest the middle.
/// `*words*` in a hook or pill draw in the keyword yellow (reference: "in *67 seconds.*",
/// "MiniMax H3 *running on my PC*"); the rest stays white.
pub fn marked_runs(text: &str) -> Value {
    let mut runs = Vec::new();
    for (i, part) in text.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }
        if i % 2 == 1 {
            runs.push(json!({"text": part, "colour": "#FFD400FF"}));
        } else {
            runs.push(json!({"text": part}));
        }
    }
    json!(runs)
}

pub fn wrap_hook(text: &str) -> String {
    // the fewest lines of about MAX characters (the boxed hook's size 68), filled evenly:
    // a word goes to the next line when that leaves this one closer to the even length
    const MAX: usize = 28;
    let t = text.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    let total = t.chars().count();
    if total <= MAX || words.len() < 2 {
        return t.to_owned();
    }
    let n = total.div_ceil(MAX);
    let target = total as f64 / n as f64;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in words {
        let len = cur.chars().count();
        let with = len + 1 + w.chars().count();
        if !cur.is_empty() && lines.len() + 1 < n && with as f64 - target > target - len as f64 {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    lines.push(cur);
    lines.join("\n")
}

/// When the cut's hook ends after `frame`, its end anchor (a pill or clock waits for the
/// hook to clear the band they share).
fn after_hook(core: &AppCore, project: &str, cut: &str, frame: i64) -> Option<Value> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut).ok()?;
    let fpf = crate::planner::fpf(&core.planned(project, cut).ok()?.plan);
    let hook = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .find(|o| o.role.as_deref() == Some("text_hook"))?;
    let (anchor, end) = match &hook.end {
        OverlayEnd::At(a) => {
            let r = gmclip_core::anchors::resolve(&doc, Some(cid), a, "/").ok()?;
            let t = gmclip_core::anchors::to_cut(&doc, cid, r.src, None)?;
            (serde_json::to_value(a).ok()?, t.0 / fpf)
        }
        OverlayEnd::DurMs(ms) => {
            let k = (i64::from(*ms) * FLICKS / 1000 + fpf - 1) / fpf;
            (
                json!({"raw": {"time": {"clock": "cut", "cut": cut, "t": k * fpf}, "reason": "after the hook"}}),
                k,
            )
        }
    };
    (end > frame).then_some(anchor)
}

fn delete_role(core: &AppCore, project: &str, cut: &str, role: &str) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let ops: Vec<Value> = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .filter(|o| o.role.as_deref() == Some(role))
        .map(|o| json!({"op": "overlay_delete", "overlay": o.id.0.to_string()}))
        .collect();
    if !ops.is_empty() {
        core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))?;
    }
    Ok(())
}

/// The text hook: from frame 0 until the end of word `until` (an ordinal of the cut's
/// transcript), else for `dur_ms`. Replaces the cut's hook; empty text removes it.
pub fn hook_set(
    core: &AppCore,
    project: &str,
    cut: &str,
    text: &str,
    until: Option<u64>,
    dur_ms: Option<u32>,
) -> AppResult<Value> {
    hook_set_with(core, project, cut, text, until, dur_ms, None)
}

/// [`hook_set`] recording where the line's numbers come from: `provenance` is a
/// suggested line's `number_provenance` (`hook_text_suggest`, R-AI-4).
pub fn hook_set_with(
    core: &AppCore,
    project: &str,
    cut: &str,
    text: &str,
    until: Option<u64>,
    dur_ms: Option<u32>,
    provenance: Option<Value>,
) -> AppResult<Value> {
    if text.chars().count() > 120 {
        return Err(AppError::invalid(
            "a hook is a short line (at most 120 characters)",
        ));
    }
    delete_role(core, project, cut, "text_hook")?;
    if text.trim().is_empty() {
        return Ok(json!({"removed": true}));
    }
    ensure_presets(core, project)?;
    let end = match until {
        Some(o) => json!({"at": {"word_end": word_ref(core, project, cut, o)?}}),
        None => json!({"dur_ms": dur_ms.unwrap_or(5000).clamp(500, 60_000)}),
    };
    let y = if portrait(core, project, cut) {
        310.0 / 1920.0
    } else {
        0.16
    };
    core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "text", "role": "text_hook", "band": "hook",
               "start": {"cut_start": {"cut": cut}}, "end": end,
               "place": {"x": 0.5, "y": y, "w": 0.9},
               "text": {"runs": marked_runs(&wrap_hook(text)), "style": style_of(core, project, "Hook")?},
               // where its numbers come from (a suggested line's, R-AI-4); a typed hook's
               // are the human's own
               "number_provenance": provenance.filter(Value::is_array).unwrap_or_else(|| json!([]))}),
    )
}

fn pill_place(core: &AppCore, project: &str, cut: &str) -> Value {
    if portrait(core, project, cut) {
        json!({"x": 48.0 / 1080.0, "y": 262.0 / 1920.0, "w": 0.8})
    } else {
        json!({"x": 48.0 / 1920.0, "y": 0.06, "w": 0.6})
    }
}

/// A section pill over a part (start to end of that part).
pub fn pill_add(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    text: &str,
) -> AppResult<Value> {
    pill_add_span(core, project, cut, part, None, text)
}

/// A section pill from the start of `part` to the end of `to_part` (a later part; the
/// same one when `None`): the reference's context pill spans several takes.
pub fn pill_add_span(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    to_part: Option<&str>,
    text: &str,
) -> AppResult<Value> {
    let last = to_part.unwrap_or(part).to_owned();
    if text.trim().is_empty() || text.chars().count() > 80 {
        return Err(AppError::invalid(
            "a pill is a short label (1–80 characters)",
        ));
    }
    ensure_presets(core, project)?;
    let start_frame = {
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = ulid_id::<Cut>(cut)?;
        let pid = ulid_id::<Part>(part)?;
        let fpf = crate::planner::fpf(&core.planned(project, cut)?.plan);
        cut_layout(&doc, cid)
            .iter()
            .find(|s| s.part == pid)
            .map(|s| s.start.0 / fpf)
            .ok_or_else(|| AppError::not_found("that part is not in the cut"))?
    };
    let start = after_hook(core, project, cut, start_frame)
        .unwrap_or(json!({"part_start": {"part": part}}));
    core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "text", "role": "section_pill", "band": "pill",
               "start": start, "end": {"at": {"part_end": {"part": last}}},
               "place": pill_place(core, project, cut),
               "text": {"runs": marked_runs(text.trim()), "style": style_of(core, project, "Pill")?}}),
    )
}

/// A media time as a frame of the picture it shows: the source itself, or a sync group's
/// picture member at its own time (evidence frames for a measurement).
fn clock_frame(
    doc: &gmclip_model::Doc,
    media: gmclip_model::entities::MediaRef,
    src: Flicks,
) -> AppResult<(String, i64, f64)> {
    // a sync group: its picture member, at that member's own time (group − offset)
    let (s, src) = match media {
        gmclip_model::entities::MediaRef::Source(s) => (s, src),
        gmclip_model::entities::MediaRef::Group(g) => {
            let m = doc
                .get::<gmclip_model::entities::SyncGroup>(g)
                .and_then(|g| g.picture)
                .and_then(|m| doc.get::<gmclip_model::entities::SyncMember>(m))
                .ok_or_else(|| AppError::invalid("the stream has no picture member"))?;
            let off = doc
                .children::<gmclip_model::entities::SyncSegment>(m.id.entity())
                .filter_map(|x| doc.get(x))
                .find(|x| x.src_range.start <= src && src < x.src_range.end)
                .map_or(0, |x| x.offset.0);
            (m.source, Flicks(src.0 - off))
        }
    };
    let source = doc.get(s).ok_or_else(|| AppError::internal("source"))?;
    let rate = source
        .probe
        .tracks
        .iter()
        .find_map(|t| t.fps)
        .map_or(30.0, |r| r.num as f64 / f64::from(r.den.max(1)));
    let idx = (src.0 as f64 / FLICKS as f64 * rate).round() as i64;
    Ok((s.0.to_string(), idx, src.0 as f64 / FLICKS as f64))
}

/// Records a run (a measurement) from two output frames the person marked: its start
/// (the app's state change) and the first frame the result shows.
pub fn run_measure(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = crate::planner::fpf(&core.planned(project, cut)?.plan);
    let at = |f: u64| -> AppResult<(gmclip_model::entities::MediaRef, Flicks)> {
        let (pid, src) = gmclip_model::clock::cut_to_src(&doc, cid, Flicks(f as i64 * fpf))
            .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
        let part: &Part = doc.get(pid).ok_or_else(|| AppError::internal("part"))?;
        Ok((part.media, src))
    };
    let (ma, a) = at(from)?;
    let (mb, b) = at(to)?;
    if ma != mb {
        return Err(AppError::invalid(
            "mark the start before the end, in the same video",
        ));
    }
    measure_on_clock(
        core,
        project,
        &doc,
        ma,
        a,
        b,
        "start and end frames marked on the preview",
    )
}

/// A run measured on the media's own clock: a source's frames, or for a sync group raw
/// times on the group's clock (the clock its parts and the run clock count in), with the
/// picture member's frames kept as evidence.
fn measure_on_clock(
    core: &AppCore,
    project: &str,
    doc: &gmclip_model::Doc,
    media: gmclip_model::entities::MediaRef,
    a: Flicks,
    b: Flicks,
    method: &str,
) -> AppResult<Value> {
    if b <= a {
        return Err(AppError::invalid("mark the start before the end"));
    }
    let (s1, f1, _) = clock_frame(doc, media, a)?;
    let (s2, f2, _) = clock_frame(doc, media, b)?;
    let secs = (((b.0 - a.0) as f64 / FLICKS as f64) * 1000.0).round() / 1000.0;
    let end = |t: Flicks, sx: &str, fx: i64| match media {
        gmclip_model::entities::MediaRef::Group(g) => {
            json!({"anchor": {"raw": {"time": {"clock": "src", "group": g.0.to_string(), "t": t.0}, "reason": method}}})
        }
        gmclip_model::entities::MediaRef::Source(_) => {
            json!({"frame": {"source": sx, "frame_index": fx}})
        }
    };
    let m = core.call(
        project,
        "measurement_add",
        json!({"kind": "interval", "from": end(a, &s1, f1), "to": end(b, &s2, f2),
               "value": secs, "unit": "s",
               "evidence_frames": [{"source": s1, "frame_index": f1}, {"source": s2, "frame_index": f2}],
               "method": method, "author": "local:human"}),
    )?;
    Ok(json!({"measurement": out(&m)["id"], "seconds": secs}))
}

/// Records a run from two typed source times (seconds on the stream's clock), for runs
/// whose start or end the cut does not show (h79: the run started 0.03 s before its take
/// and finished between takes).
pub fn run_measure_src(
    core: &AppCore,
    project: &str,
    cut: &str,
    from_s: f64,
    to_s: f64,
) -> AppResult<Value> {
    if !(from_s.is_finite() && to_s.is_finite()) || to_s <= from_s || from_s < 0.0 {
        return Err(AppError::invalid("a start before the end, in seconds"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let media = gmclip_model::clock::cut_layout(&doc, cid)
        .first()
        .and_then(|sp| doc.get::<Part>(sp.part))
        .map(|pt| pt.media)
        .ok_or_else(|| AppError::invalid("the cut has no parts"))?;
    let fl = |s: f64| Flicks((s * FLICKS as f64).round() as i64);
    measure_on_clock(
        core,
        project,
        &doc,
        media,
        fl(from_s),
        fl(to_s),
        "start and end times typed from the stream",
    )
}

/// The stream around `at_s` (the cut's media clock, as the transcript and the typed stream
/// times count it): 10 s before to 30 s after, full frame, to browse with the playhead
/// and mark a run the cut does not show. Returns the plan and the stream time of its
/// frame 0, so frame `f` is `t0_s + f / fps`.
pub fn source_view(
    core: &AppCore,
    project: &str,
    cut: &str,
    at_s: f64,
) -> AppResult<(gmclip_plan::Plan, Value)> {
    if !at_s.is_finite() || at_s < 0.0 {
        return Err(AppError::invalid("a time in the stream, in seconds"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let media = gmclip_model::clock::cut_layout(&doc, cid)
        .first()
        .and_then(|sp| doc.get::<Part>(sp.part))
        .map(|pt| pt.media)
        .ok_or_else(|| AppError::invalid("the cut has no parts"))?;
    let (lo, hi) = crate::cutnew::media_range(&doc, media);
    let at = ((at_s * FLICKS as f64).round() as i64).clamp(lo, hi.saturating_sub(1));
    let t0 = (at - 10 * FLICKS).max(lo);
    let t1 = (at + 30 * FLICKS).min(hi);
    if t1 <= t0 {
        return Err(AppError::invalid("that time is not in the stream"));
    }
    let (plan, z) =
        crate::review::stream_plan(&doc, media, "run measure", (Flicks(t0), Flicks(t1)))?;
    let fps = plan.output.fps.as_f64();
    let frames = plan.output.frames;
    let frame = ((at - z.0) / crate::planner::fpf(&plan)).clamp(0, frames as i64 - 1);
    Ok((
        plan,
        json!({"t0_s": z.0 as f64 / FLICKS as f64, "fps": fps, "frame": frame, "frames": frames}),
    ))
}

/// A run clock pill for a measurement: from the run's start to the cut's end, counting
/// source time ("GENERATING 0:23" → "DONE IN 1:07"). Replaces the cut's run clock.
pub fn clock_add(
    core: &AppCore,
    project: &str,
    cut: &str,
    measurement: &str,
    running: &str,
    done: &str,
) -> AppResult<Value> {
    clock_add_span(core, project, cut, measurement, running, done, None, None)
}

/// [`clock_add`] with where it shows: from the start of `from_part` (else the run's start
/// when the cut shows it, else the first take after the run started) to the end of
/// `until_part` (else the cut's end). The reference's h79: the clock from "start the clock" (take
/// 5) and "DONE IN 1:07" until take 10 ends.
#[allow(clippy::too_many_arguments)]
pub fn clock_add_span(
    core: &AppCore,
    project: &str,
    cut: &str,
    measurement: &str,
    running: &str,
    done: &str,
    from_part: Option<&str>,
    until_part: Option<&str>,
) -> AppResult<Value> {
    ulid_id::<Measurement>(measurement)?;
    if running.chars().count() > 40 || done.chars().count() > 40 {
        return Err(AppError::invalid("labels of at most 40 characters"));
    }
    ensure_presets(core, project)?;
    delete_role(core, project, cut, "run_clock")?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let m: &Measurement = doc
        .get(ulid_id::<Measurement>(measurement)?)
        .ok_or_else(|| AppError::not_found("no such measurement"))?;
    let secs = m.value;
    let run_start = {
        let r = gmclip_core::anchors::resolve(
            &doc,
            ulid_id::<Cut>(cut).ok(),
            &gmclip_model::anchor::Anchor::Measured(m.id),
            "/",
        )
        .map_err(|_| AppError::invalid("the measurement does not resolve"))?;
        let fpf = crate::planner::fpf(&core.planned(project, cut)?.plan);
        let cid = ulid_id::<Cut>(cut)?;
        r.cut_t
            .or_else(|| gmclip_core::anchors::to_cut(&doc, cid, r.src, None))
            .map(|t| t.0 / fpf)
            .ok_or(r.src)
    };
    let start = match (from_part, run_start) {
        (Some(p0), _) => json!({"part_start": {"part": p0}}),
        (None, Ok(f)) => {
            after_hook(core, project, cut, f).unwrap_or(json!({"measured": measurement}))
        }
        // the cut does not show the run's start: the first take after it began
        (None, Err(src)) => {
            let cid = ulid_id::<Cut>(cut)?;
            let next = gmclip_model::clock::cut_layout(&doc, cid)
                .iter()
                .filter_map(|sp| doc.get::<gmclip_model::entities::Part>(sp.part))
                .find(|pt| pt.src_in >= src)
                .map(|pt| pt.id.0.to_string())
                .ok_or_else(|| {
                    AppError::invalid("no take of the cut comes after the run's start")
                })?;
            json!({"part_start": {"part": next}})
        }
    };
    let run_start = run_start.unwrap_or(0);
    let end = match until_part {
        Some(p1) => json!({"at": {"part_end": {"part": p1}}}),
        None => json!({"at": {"cut_end": {"cut": cut}}}),
    };
    // The context pill hands the band to the clock: pills still showing when the clock
    // starts end there (AGENTS.md: a context label first, then the measured clock).
    {
        let cid = ulid_id::<Cut>(cut)?;
        let fpf = crate::planner::fpf(&core.planned(project, cut)?.plan);
        let start_a: gmclip_model::anchor::Anchor =
            serde_json::from_value(start.clone()).map_err(|_| AppError::internal("anchor"))?;
        let frame_of = |a: &gmclip_model::anchor::Anchor| {
            gmclip_core::anchors::resolve(&doc, Some(cid), a, "/")
                .ok()
                .and_then(|r| gmclip_core::anchors::to_cut(&doc, cid, r.src, None))
                .map(|t| t.0 / fpf)
        };
        let k = frame_of(&start_a).unwrap_or(run_start);
        let ops: Vec<Value> = doc
            .children::<Overlay>(cid.entity())
            .filter_map(|id| doc.get(id))
            .filter(|o| o.role.as_deref() == Some("section_pill"))
            .filter(|o| {
                let a = frame_of(&o.start).unwrap_or(i64::MAX);
                let z = match &o.end {
                    OverlayEnd::At(e) => frame_of(e).unwrap_or(i64::MIN),
                    OverlayEnd::DurMs(ms) => a + i64::from(*ms) * FLICKS / 1000 / fpf,
                };
                a < k && k < z
            })
            .map(|o| json!({"op": "overlay_set", "overlay": o.id.0.to_string(), "end": {"at": start}}))
            .collect();
        if !ops.is_empty() {
            core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))?;
        }
    }
    core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "counter", "role": "run_clock", "band": "pill",
               "start": start, "end": end,
               "place": pill_place(core, project, cut),
               "text": {"runs": [], "style": style_of(core, project, "Pill")?},
               "counter": {"start": {"measurement": measurement}, "stop": null, "clock": "src",
                           "format": "", "running_label": running.trim(), "done_label": done.trim()},
               "number_provenance": [{"text": format!("{}:{:02}", (secs as i64) / 60, (secs as i64) % 60),
                                      "measurement": measurement, "note": "marked start and end frames"}]}),
    )
}

/// Deletes an overlay (hook, pill, clock).
pub fn overlay_delete(core: &AppCore, project: &str, cut: &str, overlay: &str) -> AppResult<Value> {
    ulid_id::<Overlay>(overlay)?;
    core.call(
        project,
        "cut_edit",
        json!({"cut": cut, "ops": [{"op": "overlay_delete", "overlay": overlay}]}),
    )
}

/// Output frame of a part's start (for the UI's pill list).
pub fn part_at(core: &AppCore, project: &str, cut: &str, frame: u64) -> AppResult<String> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let pl = core.planned(project, cut)?;
    let t = frame as i64 * crate::planner::fpf(&pl.plan);
    cut_layout(&doc, cid)
        .iter()
        .find(|s| t >= s.start.0 && t < s.start.0 + s.dur.0)
        .map(|s| s.part.0.to_string())
        .ok_or_else(|| AppError::invalid("that frame is not in the cut"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn hook_lines() {
        assert_eq!(
            super::wrap_hook("My PC made this in 67 seconds."),
            "My PC made this\nin 67 seconds."
        );
        assert_eq!(super::wrap_hook("Short hook"), "Short hook");
        assert_eq!(
            super::wrap_hook("North Korea stole billions in crypto. Prices barely moved."),
            "North Korea stole\nbillions in crypto.\nPrices barely moved."
        );
    }
}
