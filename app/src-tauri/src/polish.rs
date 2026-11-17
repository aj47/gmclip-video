//! The Polish tab (Ideas 2026-10-16, lane K2; decision 0025): five suggestions for a
//! short, each labelled with what it measured, each applied with one click as one undo
//! step, none applied on its own (except stressed words when the caption style asks).
//!
//! - **Stressed words** (`gmclip_analysis::emphasis`): caption words the speaker says
//!   louder or slower than his running baseline in this cut — level from the stored level
//!   frames over each word's own span (plus the part's gain: the cut's own audio for the
//!   parts that play their source as recorded), pace from the word timings. Proposed as
//!   keywords; with the caption style's automatic emphasis on, new ones are starred when
//!   the tab opens (once per word: a word you unstar stays unstarred).
//! - **Punch-in**: at the strongest stressed words, never two within 3 s, a small eased
//!   zoom of the screen layer held for the phrase (`punch_ins_set`); a per-cut switch.
//! - **Callouts**: a number, a product or an on-screen thing named in the words — a label
//!   (the pill style) or an arrow into the screen panel, placed in the safe zones away from
//!   the captions and the face (`overlay_add`); never automatic.
//! - **Loop**: the per-cut option, whether the last frame shows the opening's layout, and
//!   the two fixes (`cut_loop_set`, `loop_fix`).
//! - **Hold screen** (`holds_find`, decodes frames): where the screen region jumps to
//!   something else (a change far above this cut's own steps) while a sentence is still
//!   being said; applied with the `h` machinery (`hold_toggle` → `hold_screen_here`) on the
//!   last frame before the jump.

use std::collections::{BTreeMap, BTreeSet};

use gmclip_analysis::emphasis::{self, EmWord, Stress};
use gmclip_core::activity::{LumaFrame, NRect};
use gmclip_core::commands::polish::punch_pairs;
use gmclip_core::polish::{
    self as pol, CalloutKind, PUNCH_GAP_MS, Rect, TWord, mid_sentence, phrase_end, pick_rare,
    screen_jumps,
};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id};
use serde_json::{Value, json};

use crate::core::{AppCore, OpenProject};
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// Every command this module answers (each is also in `ipc::APP_COMMANDS`).
pub const COMMANDS: &[&str] = &["polish_view", "polish_apply", "holds_find"];

/// A callout stays this long (clamped to the cut's end).
const CALLOUT_MS: i64 = 2_500;
/// At most this many callout suggestions.
const MAX_CALLOUTS: usize = 12;
/// Frames a second the hold search samples, and the most samples per plan part.
const HOLD_SAMPLES_PER_S: u64 = 4;
const HOLD_MAX_SAMPLES: u64 = 480;
const HOLD_LUMA_W: u32 = 320;

pub fn ipc(core: &AppCore, command: &str, project: &str, a: &Value) -> AppResult<Value> {
    let s = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
    };
    match command {
        "polish_view" => view(core, project, s("cut")?),
        "polish_apply" => apply(core, project, s("cut")?, s("kind")?, s("key")?),
        "holds_find" => holds_find(core, project, s("cut")?),
        _ => Err(AppError::invalid("unknown command")),
    }
}

/// One caption word of the cut.
#[derive(Clone, Debug)]
struct CW {
    ord: u32,
    text: String,
    part: Id<Part>,
    src0: i64,
    out0_ms: i64,
    out1_ms: i64,
    frame: i64,
    level: Option<f64>,
    keyword: bool,
}

/// What every section reads.
struct Cx {
    doc: Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    fpf: i64,
    canvas: (f64, f64),
    rev: Option<String>,
    words: Vec<CW>,
    sound: bool,
    end_ms: i64,
}

fn owner(m: &MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

fn ms(f: i64) -> i64 {
    f * 1000 / FLICKS
}

fn context(
    core: &AppCore,
    project: &str,
    cut: &str,
) -> AppResult<(Cx, std::sync::Arc<OpenProject>)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let framing = core
        .active_framing(project, cut)
        .and_then(|f| ulid_id::<Framing>(&f).ok())
        .ok_or_else(|| AppError::invalid("the cut has no framing"))?;
    let fr = doc
        .get(framing)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let fpf = gmclip_core::planner::framing_fpf(fr.fps).unwrap_or(FLICKS / 30);
    let canvas = (f64::from(fr.canvas.w), f64::from(fr.canvas.h));
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    let end_ms = layout.last().map_or(0, |l| ms(l.start.0 + l.dur.0));
    let mut words = Vec::new();
    let mut rev = None;
    let mut sound = false;
    if let Some((o, r, t, _)) = crate::words::transcript_of(core, &p, &doc, cid) {
        let cd = doc
            .children::<CaptionDoc>(cid.entity())
            .find_map(|id| doc.get(id))
            .cloned();
        let keys: BTreeSet<u32> = cd
            .as_ref()
            .map(|c| {
                c.keywords
                    .values()
                    .filter(|w| w.asr_revision == r)
                    .map(|w| w.ordinal)
                    .collect()
            })
            .unwrap_or_default();
        let level = crate::reactions::energy(&p, &o);
        sound = level.is_some();
        for span in &layout {
            let Some(part) = doc.get(span.part) else {
                continue;
            };
            if owner(&part.media) != o {
                continue;
            }
            let speed = if part.speed > 0.0 { part.speed } else { 1.0 };
            for (i, w) in t.words.iter().enumerate() {
                let inside = (w.end.min(part.src_out.0) - w.start.max(part.src_in.0)).max(0);
                if 2 * inside < (w.end - w.start).max(1) {
                    continue;
                }
                let id = gmclip_model::anchor::WordId {
                    asr_revision: r.clone(),
                    ordinal: i as u32,
                };
                if cd
                    .as_ref()
                    .is_some_and(|c| matches!(c.edits.get(&id), Some(CaptionEdit::Drop)))
                {
                    continue;
                }
                let s0 = w.start.max(part.src_in.0);
                let s1 = w.end.min(part.src_out.0);
                let out0 = span.start.0 + ((s0 - part.src_in.0) as f64 / speed) as i64;
                let out1 = span.start.0 + ((s1 - part.src_in.0) as f64 / speed) as i64;
                words.push(CW {
                    ord: i as u32,
                    text: w.text.clone(),
                    part: part.id,
                    src0: s0,
                    out0_ms: ms(out0),
                    out1_ms: ms(out1),
                    frame: out0 / fpf,
                    level: level.as_ref().and_then(|(db, hop)| {
                        emphasis::word_level(db, *hop, 0, ms(w.start), ms(w.end))
                            .map(|l| l + part.gain_db)
                    }),
                    keyword: keys.contains(&(i as u32)),
                });
            }
        }
        rev = Some(r);
    }
    Ok((
        Cx {
            doc,
            cut: cid,
            framing,
            fpf,
            canvas,
            rev,
            words,
            sound,
            end_ms,
        },
        p,
    ))
}

fn stressed(cx: &Cx) -> Vec<Stress> {
    let em: Vec<EmWord> = cx
        .words
        .iter()
        .map(|w| EmWord {
            text: w.text.clone(),
            start_ms: w.out0_ms,
            end_ms: w.out1_ms,
            level_db: w.level,
        })
        .collect();
    emphasis::stressed(&em)
}

fn tc(ms: i64) -> String {
    let s = ms as f64 / 1000.0;
    format!("{}:{:04.1}", (s / 60.0).floor() as i64, s % 60.0)
}

fn word_ref(cx: &Cx, w: &CW) -> Value {
    json!({"asr_revision": cx.rev.clone().unwrap_or_default(), "ordinal": w.ord})
}

fn twords(cx: &Cx) -> Vec<TWord> {
    cx.words
        .iter()
        .map(|w| TWord {
            text: w.text.clone(),
            start_ms: w.out0_ms,
            end_ms: w.out1_ms,
        })
        .collect()
}

/// The words this project's tab already proposed under automatic emphasis, per cut
/// (`<home>/emphasis-seen.json`): a word unstarred after that is never starred again.
fn seen_path(p: &OpenProject) -> std::path::PathBuf {
    p.home.join("emphasis-seen.json")
}

fn seen(p: &OpenProject) -> BTreeMap<String, BTreeSet<String>> {
    std::fs::read(seen_path(p))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn remember(p: &OpenProject, cut: &str, keys: &[String]) {
    let mut all = seen(p);
    all.entry(cut.to_owned())
        .or_default()
        .extend(keys.iter().cloned());
    if let Ok(b) = serde_json::to_vec(&all) {
        let tmp = seen_path(p).with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, seen_path(p));
        }
    }
}

fn auto_on(cx: &Cx) -> bool {
    cx.doc
        .get(cx.framing)
        .and_then(|f| f.caption_look.as_ref())
        .is_some_and(|l| l.auto_emphasis)
}

fn emphasis_section(cx: &Cx, st: &[Stress]) -> Value {
    let n = cx.words.len();
    let rows: Vec<Value> = st
        .iter()
        .map(|s| {
            let w = &cx.words[s.index];
            json!({"key": w.ord.to_string(), "text": w.text, "frame": w.frame, "at": tc(w.out0_ms),
                   "label": s.label, "z_level": s.z_level, "z_slow": s.z_slow,
                   "db_over": s.db_over, "pace_ratio": s.pace_ratio, "keyword": w.keyword})
        })
        .collect();
    let state = if cx.rev.is_none() {
        "no_transcript"
    } else if n < 4 {
        "few_words"
    } else if !cx.sound {
        "no_sound"
    } else {
        "ok"
    };
    json!({
        "state": state, "words": n, "budget": (n / emphasis::WORDS_PER_STRESS).max(usize::from(n >= 4)),
        "auto": auto_on(cx), "proposals": rows,
        "measured": "level and pace of each word against the speaker's running baseline in this cut (±20 words)",
    })
}

/// The screen layer of a part at `t` (or the largest visible layer that is not the camera).
fn screen_layer(
    doc: &Doc,
    part: Id<Part>,
    framing: Id<Framing>,
    t: i64,
) -> Option<gmclip_model::effective::ResolvedLayer> {
    let st = gmclip_model::effective::effective_settled(doc, part, framing, Flicks(t))?;
    st.layers
        .iter()
        .find(|l| l.values.visible && l.values.role == LayerRole::Region(RegionRole::Screen))
        .or_else(|| {
            st.layers
                .iter()
                .filter(|l| {
                    l.values.visible && !matches!(l.values.role, LayerRole::Region(RegionRole::Cam))
                })
                .max_by(|a, b| a.values.place.w.total_cmp(&b.values.place.w))
        })
        .cloned()
}

/// Punch-in candidates: `(word index, part, layer, at, until, label)` or why not.
fn punch_section(cx: &Cx, st: &[Stress]) -> (Value, Vec<Value>) {
    let doc = &cx.doc;
    let tw = twords(cx);
    let parts: Vec<Id<Part>> = gmclip_model::clock::cut_layout(doc, cx.cut)
        .iter()
        .map(|s| s.part)
        .collect();
    // the punch-ins already on the cut
    let mut existing: Vec<(Id<VisualSplit>, Id<Part>, i64)> = Vec::new();
    for p in &parts {
        for (i, _) in punch_pairs(doc, *p, cx.framing) {
            if let Some(s) = doc.get(i) {
                existing.push((i, *p, s.at_src.0));
            }
        }
    }
    let mut cands: Vec<Value> = Vec::new();
    let mut times: Vec<(i64, f64)> = Vec::new();
    for s in st {
        let w = &cx.words[s.index];
        let Some(part) = doc.get(w.part) else {
            continue;
        };
        let speed = if part.speed > 0.0 { part.speed } else { 1.0 };
        let end_out = phrase_end(&tw, s.index);
        let at = w.src0;
        let until = (at + ((end_out - w.out0_ms) as f64 * speed * FLICKS as f64 / 1000.0) as i64)
            .min(part.src_out.0 - 3 * cx.fpf);
        let mut why = None;
        if until - at < 3 * FLICKS / 10 {
            why = Some("the part ends right after the word");
        }
        let Some(layer) = screen_layer(doc, part.id, cx.framing, at) else {
            continue;
        };
        if why.is_none()
            && gmclip_model::effective::splits_of(doc, part.id, cx.framing)
                .iter()
                .any(|x| x.tag.is_none() && at <= x.at_src.0 && x.at_src.0 <= until)
        {
            why = Some("a crop change falls inside the phrase");
        }
        let added = existing
            .iter()
            .find(|(_, p, t)| *p == part.id && (t - at).abs() <= FLICKS / 2)
            .map(|(i, _, _)| i.0.to_string());
        let label = format!(
            "{} — zoom 8 % at \"{}\" ({}), held {:.1} s to the end of the phrase",
            s.label,
            w.text.trim_matches(|c: char| !c.is_alphanumeric()),
            tc(w.out0_ms),
            (until - at) as f64 / FLICKS as f64 / speed
        );
        cands.push(json!({
            "key": format!("w{}", w.ord), "text": w.text, "frame": w.frame, "at": tc(w.out0_ms),
            "label": label, "added": added, "why_not": why,
            "punch": {"part": part.id, "layer": layer.id, "at": at, "until": until},
        }));
        times.push((w.out0_ms, if why.is_some() { -1.0 } else { s.score }));
    }
    // the rare few: never two within 3 s (added ones count first)
    let mut keep: Vec<usize> = Vec::new();
    let added_t: Vec<i64> = cands
        .iter()
        .zip(&times)
        .filter(|(c, _)| !c["added"].is_null())
        .map(|(_, t)| t.0)
        .collect();
    for i in pick_rare(&times, PUNCH_GAP_MS) {
        let free = times[i].1 >= 0.0
            && (!cands[i]["added"].is_null()
                || added_t
                    .iter()
                    .all(|t| (t - times[i].0).abs() >= PUNCH_GAP_MS));
        if free {
            keep.push(i);
        }
    }
    let picked: Vec<Value> = keep.iter().map(|i| cands[*i].clone()).collect();
    let section = json!({
        "on": !existing.is_empty(), "count": existing.len(),
        "existing": existing.iter().map(|(i, _, _)| i.0.to_string()).collect::<Vec<_>>(),
        "suggestions": picked,
        "rule": "at the strongest stressed words, never two within 3 s; 8 % in over 0.22 s, back over 0.32 s",
    });
    (section, picked)
}

/// A canvas rectangle of a layer as placed (place + the crop's aspect on the source).
fn layer_rect(
    doc: &Doc,
    part: &Part,
    l: &gmclip_model::effective::LayerValues,
    canvas: (f64, f64),
) -> Rect {
    let (sw, sh) = media_dims(doc, &part.media).unwrap_or((1920.0, 1080.0));
    let w = l.place.w * canvas.0;
    let aspect = (l.crop.w * sw) / (l.crop.h * sh).max(1e-9);
    Rect {
        x: l.place.x * canvas.0,
        y: l.place.y * canvas.1,
        w,
        h: w / aspect.max(1e-9),
    }
}

fn media_dims(doc: &Doc, media: &MediaRef) -> Option<(f64, f64)> {
    let sid = match media {
        MediaRef::Source(s) => *s,
        MediaRef::Group(g) => {
            let grp = doc.get(*g)?;
            grp.picture
                .and_then(|m| doc.get(m))
                .or_else(|| {
                    doc.children::<SyncMember>(g.entity())
                        .filter_map(|m| doc.get(m))
                        .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
                })?
                .source
        }
    };
    let t = doc
        .get(sid)?
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)?;
    Some((f64::from(t.width?), f64::from(t.height?)))
}

fn callout_section(core: &AppCore, project: &str, cx: &Cx) -> Vec<Value> {
    let doc = &cx.doc;
    let mut terms: Vec<String> = doc
        .iter::<Terms>()
        .flat_map(|t| t.entries.values().map(|e| e.written.clone()))
        .collect();
    terms.extend(
        doc.children::<CaptionDoc>(cx.cut.entity())
            .filter_map(|c| doc.get(c))
            .flat_map(|c| c.keyword_terms.clone()),
    );
    let texts: Vec<&str> = cx.words.iter().map(|w| w.text.as_str()).collect();
    let hits = pol::callout_hits(&texts, &terms);
    let pill = crate::short::ensure_presets(core, project)
        .and_then(|()| crate::short::style_of(core, project, "Pill"))
        .ok();
    let planned = core.planned(project, &cx.cut.0.to_string()).ok();
    let hook_until: Option<i64> = planned.as_ref().and_then(|pl| {
        pl.plan
            .text_events
            .iter()
            .flatten()
            .filter(|e| e.id.as_str().starts_with("hook"))
            .map(|e| e.out.0[1] as i64)
            .max()
    });
    let layout = gmclip_model::clock::cut_layout(doc, cx.cut);
    let (cw, ch) = cx.canvas;
    let overlays: Vec<&Overlay> = doc
        .children::<Overlay>(cx.cut.entity())
        .filter_map(|o| doc.get(o))
        .collect();
    let mut out = Vec::new();
    for h in hits.into_iter().take(MAX_CALLOUTS) {
        let w = &cx.words[h.start];
        let Some(part) = doc.get(w.part) else {
            continue;
        };
        let Some(span) = layout.iter().find(|s| s.part == part.id) else {
            continue;
        };
        let anchor = json!({"part_start": {"part": part.id.0.to_string(),
                                           "offset_ms": w.out0_ms - ms(span.start.0)}});
        let dur = CALLOUT_MS.min(cx.end_ms - w.out0_ms).max(500);
        // what to keep clear: the captions, the face, the hook while it shows
        let st = gmclip_model::effective::effective(doc, part.id, cx.framing, Flicks(w.src0));
        let mut avoid: Vec<Rect> = Vec::new();
        let mut panel: Option<Rect> = None;
        if let Some(st) = &st {
            for l in st.layers.iter().filter(|l| l.values.visible) {
                let r = layer_rect(doc, part, &l.values, cx.canvas);
                match l.values.role {
                    LayerRole::Region(RegionRole::Cam) => avoid.push(r),
                    LayerRole::Region(RegionRole::Screen) => panel = Some(r),
                    _ => {}
                }
            }
        }
        if let Some(cp) =
            gmclip_model::effective::effective_caption_place(doc, part.id, cx.framing, None)
        {
            let y = cp.y * ch;
            avoid.push(Rect {
                x: 0.0,
                y: y - 110.0 * ch / 1920.0,
                w: cw,
                h: 220.0 * ch / 1920.0,
            });
        }
        if hook_until.is_some_and(|z| w.frame < z) {
            avoid.push(Rect {
                x: 0.0,
                y: 220.0 * ch / 1920.0,
                w: cw,
                h: 200.0 * ch / 1920.0,
            });
        }
        let what = match h.kind {
            CalloutKind::Number => "a number",
            CalloutKind::Product => "a product",
            CalloutKind::Thing => "something on the screen",
        };
        let measured = format!("{} — said at {} (\"{}\")", h.why, tc(w.out0_ms), h.text);
        let added = overlays
            .iter()
            .any(|o| serde_json::to_value(&o.start).ok().as_ref() == Some(&anchor));
        let (kind, args) = match h.kind {
            CalloutKind::Number | CalloutKind::Product => {
                let Some(style) = pill.clone() else { continue };
                let size = (
                    (28.0 * h.text.chars().count() as f64 + 40.0) * cw / 1080.0,
                    92.0 * ch / 1920.0,
                );
                let Some(r) = pol::place_label(cx.canvas, panel, &avoid, size) else {
                    continue;
                };
                (
                    "label",
                    json!({"cut": cx.cut, "kind": "text", "role": "note", "start": anchor,
                           "end": {"dur_ms": dur}, "z": 25,
                           "place": {"x": r.x / cw, "y": r.y / ch, "w": (r.w / cw).min(1.0)},
                           "text": {"runs": [{"text": h.text}], "style": style}}),
                )
            }
            CalloutKind::Thing => {
                let target = panel.unwrap_or(Rect {
                    x: cw * 0.2,
                    y: ch * 0.2,
                    w: cw * 0.6,
                    h: ch * 0.3,
                });
                let Some((from, to)) = pol::arrow_into(cx.canvas, target, &avoid) else {
                    continue;
                };
                (
                    "arrow",
                    json!({"cut": cx.cut, "kind": "shape", "start": anchor, "end": {"dur_ms": dur}, "z": 25,
                           "place": {"x": from.0.min(to.0) / cw, "y": from.1.min(to.1) / ch, "w": ((from.0 - to.0).abs() / cw).max(0.01)},
                           "shape": {"kind": "arrow", "stroke": "#FFD400FF", "width": 11.0, "radius": 0.0,
                                     "from": [from.0 / cw, from.1 / ch], "to": [to.0 / cw, to.1 / ch]}}),
                )
            }
        };
        out.push(json!({
            "key": format!("c{}", w.ord), "kind": kind, "what": what, "text": h.text,
            "frame": w.frame, "at": tc(w.out0_ms), "label": measured, "added": added,
            "seconds": dur as f64 / 1000.0, "apply": args,
        }));
    }
    out
}

fn loop_section(cx: &Cx) -> Value {
    let on = cx.doc.get(cx.cut).and_then(|c| c.loop_check);
    let Some(st) = pol::loop_state(&cx.doc, cx.cut, cx.framing, cx.fpf) else {
        return json!({"on": on, "state": "empty"});
    };
    let mut fixes = Vec::new();
    if !st.matches {
        if st.first != st.last {
            fixes.push(json!({"key": "layout", "label": if st.last_has_splits {
                "Use the opening's layout on the last part (its zooms go)" } else {
                "Use the opening's layout on the last part" }}));
        }
        fixes.push(json!({"key": "tail", "label": "Add a 0.6 s tail in the opening's layout"}));
    }
    json!({"on": on, "state": "ok", "matches": st.matches, "differences": st.differences,
           "fixes": fixes,
           "measured": "the visible layers' crop and place at the opening's first frame and the cut's last frame"})
}

/// The Polish tab: stressed words, punch-ins, callouts and the loop (holds: `holds_find`).
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let (mut cx, p) = context(core, project, cut)?;
    let mut st = stressed(&cx);
    // the caption style asks for stressed words on its own: star the new ones (once each)
    let mut auto_applied = 0;
    if auto_on(&cx) && cx.rev.is_some() {
        let done = seen(&p).remove(cut).unwrap_or_default();
        let new: Vec<&CW> = st
            .iter()
            .map(|s| &cx.words[s.index])
            .filter(|w| {
                !w.keyword
                    && !done.contains(&format!("{}#{}", cx.rev.clone().unwrap_or_default(), w.ord))
            })
            .collect();
        if !new.is_empty() {
            let words: Vec<Value> = new.iter().map(|w| word_ref(&cx, w)).collect();
            let keys: Vec<String> = new
                .iter()
                .map(|w| format!("{}#{}", cx.rev.clone().unwrap_or_default(), w.ord))
                .collect();
            core.call(
                project,
                "caption_keywords_mark",
                json!({"cut": cut, "words": words, "on": true}),
            )?;
            remember(&p, cut, &keys);
            auto_applied = keys.len();
            let (c2, _) = context(core, project, cut)?;
            cx = c2;
            st = stressed(&cx);
        }
    }
    let (punch, _) = punch_section(&cx, &st);
    Ok(json!({
        "framing": cx.framing, "fps_fpf": cx.fpf,
        "emphasis": emphasis_section(&cx, &st), "auto_applied": auto_applied,
        "punch": punch,
        "callouts": callout_section(core, project, &cx),
        "loop": loop_section(&cx),
    }))
}

/// One click on the Polish tab, worked out again here (the UI sends only what it picked).
pub fn apply(core: &AppCore, project: &str, cut: &str, kind: &str, key: &str) -> AppResult<Value> {
    let (cx, p) = context(core, project, cut)?;
    let st = stressed(&cx);
    let gone = || AppError::new("Conflict", "the cut changed: look at the suggestions again");
    let r = match kind {
        "emphasis" => {
            let rows: Vec<&CW> = st.iter().map(|s| &cx.words[s.index]).collect();
            let pick: Vec<&CW> = match key {
                "all" | "auto_on" => rows.iter().copied().filter(|w| !w.keyword).collect(),
                "auto_off" => Vec::new(),
                k => vec![
                    *rows
                        .iter()
                        .find(|w| w.ord.to_string() == k)
                        .ok_or_else(gone)?,
                ],
            };
            let words: Vec<Value> = pick.iter().map(|w| word_ref(&cx, w)).collect();
            let mut args = json!({"cut": cut, "words": words, "on": true});
            if key == "auto_on" || key == "auto_off" {
                args["auto"] = json!({"framing": cx.framing, "on": key == "auto_on"});
                // every word proposed now counts as seen: an unstarred one stays unstarred
                let r = cx.rev.clone().unwrap_or_default();
                let keys: Vec<String> = rows.iter().map(|w| format!("{r}#{}", w.ord)).collect();
                remember(&p, cut, &keys);
            }
            core.call(project, "caption_keywords_mark", args)?
        }
        "punch" => {
            let (_, picked) = punch_section(&cx, &st);
            let args = match key {
                "all" => json!({"framing": cx.framing, "replace": true,
                                "punches": picked.iter().filter(|c| c["why_not"].is_null()).map(|c| c["punch"].clone()).collect::<Vec<_>>()}),
                "off" => json!({"framing": cx.framing, "replace": true, "punches": []}),
                k if k.starts_with("rm:") => json!({"framing": cx.framing, "remove": [&k[3..]]}),
                k => {
                    let c = picked
                        .iter()
                        .find(|c| c["key"] == k && c["why_not"].is_null() && c["added"].is_null())
                        .ok_or_else(gone)?;
                    json!({"framing": cx.framing, "punches": [c["punch"].clone()]})
                }
            };
            core.call(project, "punch_ins_set", args)?
        }
        "callout" => {
            let rows = callout_section(core, project, &cx);
            let c = rows.iter().find(|c| c["key"] == key).ok_or_else(gone)?;
            core.call(project, "overlay_add", c["apply"].clone())?
        }
        "loop" => match key {
            "on" | "off" => core.call(
                project,
                "cut_loop_set",
                json!({"cut": cut, "on": key == "on"}),
            )?,
            "layout" | "tail" => core.call(
                project,
                "loop_fix",
                json!({"cut": cut, "framing": cx.framing, "how": key}),
            )?,
            _ => return Err(AppError::invalid("a loop key is on, off, layout or tail")),
        },
        _ => return Err(AppError::invalid("unknown polish kind")),
    };
    Ok(
        json!({"kind": kind, "key": key, "output": r["output"].clone(), "committed": r["committed"].clone()}),
    )
}

/// Grey frames of a plan source at presentation indexes (the host's decoder; tests pass
/// their own).
pub type FrameReader<'a> =
    &'a dyn Fn(&gmclip_plan::Plan, &str, &[u64]) -> Result<Vec<LumaFrame>, String>;

/// `holds_find{cut}`: where the screen jumps to something else mid-sentence.
pub fn holds_find(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    use gmclip_core::MediaRender;
    let read = |plan: &gmclip_plan::Plan, src: &str, frames: &[u64]| {
        crate::roto::Frames
            .luma_frames(plan, src, frames, HOLD_LUMA_W)
            .map_err(|e| format!("{e:?}").chars().take(120).collect())
    };
    holds_find_with(core, project, cut, &read)
}

/// [`holds_find`] with a frame reader.
pub fn holds_find_with(
    core: &AppCore,
    project: &str,
    cut: &str,
    read: FrameReader<'_>,
) -> AppResult<Value> {
    let (cx, _) = context(core, project, cut)?;
    let planned = core.planned(project, cut)?;
    let plan = &planned.plan;
    let fps = plan.output.fps;
    let step = (fps.rounded() / HOLD_SAMPLES_PER_S).max(1);
    let owners = gmclip_core::planner::plan_part_owners(&cx.doc, cx.cut, cx.framing);
    // the cut's held spans (no suggestion inside one)
    let held: Vec<(i64, i64)> = cx
        .doc
        .children::<ContentHold>(cx.cut.entity())
        .filter_map(|h| cx.doc.get(h))
        .filter_map(|h| {
            let a = gmclip_model::clock::anchor_src(&cx.doc, &h.from)?;
            let b = gmclip_model::clock::anchor_src(&cx.doc, &h.to)?;
            Some((a.0, b.0))
        })
        .collect();
    // every step of every screen-showing plan part: (plan part, sample out frame, next)
    struct Step {
        k: usize,
        a: u64,
        b: u64,
        frac: f64,
    }
    let mut steps: Vec<Step> = Vec::new();
    let mut regions: BTreeMap<usize, (String, NRect, Vec<NRect>)> = BTreeMap::new();
    let mut samples = 0usize;
    let mut skipped = Vec::new();
    for (k, pp) in plan.parts.iter().enumerate() {
        let Some(pid) = owners.get(k).copied() else {
            continue;
        };
        let Some(part) = cx.doc.get(pid) else {
            continue;
        };
        let Some(pl) = pp.layers.iter().find(|l| l.role.as_str() == "screen") else {
            continue;
        };
        let src_id = pl
            .picture_source
            .as_ref()
            .unwrap_or(&pp.picture.source)
            .as_str()
            .to_owned();
        let Some((sw, sh)) = media_dims(&cx.doc, &part.media) else {
            continue;
        };
        let nr = |x: f64, y: f64, w: f64, h: f64| NRect {
            x: x / sw,
            y: y / sh,
            w: w / sw,
            h: h / sh,
        };
        let region = nr(pl.crop.x, pl.crop.y, pl.crop.w, pl.crop.h);
        let exclude: Vec<NRect> = pp
            .layers
            .iter()
            .filter(|l| l.role.as_str() != "screen" && l.role.as_str() != "detail")
            .filter(|l| {
                l.picture_source
                    .as_ref()
                    .is_none_or(|s| s.as_str() == src_id)
            })
            .map(|l| nr(l.crop.x, l.crop.y, l.crop.w, l.crop.h))
            .filter(|r| r.w * r.h < 0.5)
            .collect();
        let n = pp.out.end() - pp.out.start();
        let st = step.max(n.div_ceil(HOLD_MAX_SAMPLES));
        let idx: Vec<u64> = (0..n).step_by(st as usize).collect();
        if idx.len() < 2 {
            continue;
        }
        let frames: Vec<u64> = idx
            .iter()
            .map(|i| gmclip_core::gmclip_plan::retime::nominal_frame(pp, *i, fps))
            .collect();
        let luma = match read(plan, &src_id, &frames) {
            Ok(l) => l,
            Err(e) => {
                skipped.push(json!({"part": pid, "why": e}));
                continue;
            }
        };
        samples += luma.len();
        for i in 1..luma.len() {
            steps.push(Step {
                k,
                a: pp.out.start() + idx[i - 1],
                b: pp.out.start() + idx[i],
                frac: pol::change_fraction(&luma[i - 1], &luma[i], &region, &exclude),
            });
        }
        regions.insert(k, (src_id, region, exclude));
    }
    let fracs: Vec<f64> = steps.iter().map(|s| s.frac).collect();
    let tw = twords(&cx);
    let mut out = Vec::new();
    for j in screen_jumps(&fracs) {
        let s = &steps[j.step];
        let pp = &plan.parts[s.k];
        let (src_id, region, exclude) = &regions[&s.k];
        // the first frame after sample a that shows the change
        let mut change = s.b;
        if s.b > s.a + 1 {
            let ks: Vec<u64> = (s.a..=s.b).collect();
            let frames: Vec<u64> = ks
                .iter()
                .map(|f| {
                    gmclip_core::gmclip_plan::retime::nominal_frame(pp, f - pp.out.start(), fps)
                })
                .collect();
            if let Ok(l) = read(plan, src_id, &frames) {
                let first = (1..l.len()).find(|i| {
                    pol::change_fraction(&l[0], &l[*i], region, exclude) >= pol::JUMP_FRAC
                });
                if let Some(i) = first {
                    change = ks[i];
                }
            }
        }
        let fpf = cx.fpf;
        let t_ms = ms(change as i64 * fpf);
        let at_frame = change.saturating_sub(1);
        let Some((pid, src)) =
            gmclip_model::clock::cut_to_src(&cx.doc, cx.cut, Flicks(at_frame as i64 * fpf))
        else {
            continue;
        };
        if held.iter().any(|(a, b)| *a <= src.0 && src.0 < *b) {
            continue;
        }
        let Some(last) = mid_sentence(&tw, t_ms) else {
            continue;
        };
        let _ = pid;
        let until = &cx.words[last];
        out.push(json!({
            "key": format!("h{at_frame}"), "frame": at_frame, "change_frame": change,
            "at": tc(t_ms), "until": tc(until.out1_ms), "until_word": until.text,
            "fraction": j.fraction, "z": j.z,
            "label": format!("the screen changes {:.0} % at {} ({:.1}σ over this cut's usual step) while a sentence goes on to \"{}\" at {}: hold on the frame before",
                j.fraction * 100.0, tc(t_ms), j.z, until.text.trim_matches(|c: char| !c.is_alphanumeric()), tc(until.out1_ms)),
        }));
    }
    Ok(
        json!({"samples": samples, "steps": fracs.len(), "holds": out, "skipped": skipped,
              "measured": "the share of the screen region that changed between frames 0.25 s apart, against this cut's own steps; a jump explained by a sentence boundary is left alone"}),
    )
}
