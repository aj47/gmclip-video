//! Read projections (R-MCP-5, A23): small by default, paged, size-capped, with `base`
//! revs, `as_of` and a `display` string on every read, and source-derived text marked
//! untrusted (R-INJ-1).
//!
//! Sizes are counted with the runtime proxy of R-MCP-5: one token per 3.5 UTF-8 bytes.
//! A default projection stays under 4K tokens; no page exceeds 20K. A cut list says what
//! it left out and how to get the rest (`next_cursor`).

use std::collections::BTreeMap;

use gmclip_model::entities::*;
use gmclip_model::{Doc, Id, Ulid};
use gmclip_proto::CmdError;
use serde_json::{Map, Value, json};

use crate::canon::{self, Kind};

/// Flicks per second.
const SEC: f64 = 705_600_000.0;
/// Default projection budget (4K tokens at 3.5 bytes per token).
pub const DEFAULT_BYTES: usize = 4_000 * 7 / 2;
/// Hard page budget (20K tokens).
pub const PAGE_BYTES: usize = 20_000 * 7 / 2;

pub fn tokens(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len() * 2 / 7 + 1)
}

fn secs(f: gmclip_model::Flicks) -> f64 {
    (f.0 as f64 / SEC * 1000.0).round() / 1000.0
}

fn id_of<E>(s: &str, path: &str) -> Result<Id<E>, CmdError> {
    let raw = s.split_once(':').map_or(s, |(_, r)| r);
    Ulid::parse(raw)
        .map(Id::new)
        .map_err(|_| CmdError::invalid(path, "not an id"))
}

/// Parses a cut reference (`<ulid>` or `cut:<ulid>`).
pub fn cut_id(s: &str) -> Result<Id<Cut>, CmdError> {
    id_of(s, "/cut")
}

fn cursor(c: Option<&str>) -> Result<usize, CmdError> {
    c.map_or(Ok(0), |s| {
        s.parse()
            .map_err(|_| CmdError::invalid("/cursor", "a cursor from an earlier page"))
    })
}

/// Adds items until `budget` bytes; returns them, and the next cursor if any are left.
fn page(
    items: impl Iterator<Item = Value>,
    start: usize,
    limit: usize,
    budget: usize,
) -> (Vec<Value>, usize, Option<String>, usize) {
    let all: Vec<Value> = items.collect();
    let total = all.len();
    let mut out = Vec::new();
    let mut used = 0;
    let mut i = start;
    while i < total && out.len() < limit {
        let n = serde_json::to_string(&all[i]).map_or(0, |s| s.len());
        if used + n > budget && !out.is_empty() {
            break;
        }
        used += n;
        out.push(all[i].clone());
        i += 1;
    }
    let next = (i < total).then(|| i.to_string());
    (out, total, next, i)
}

fn truncation(total: usize, start: usize, shown: usize, next: &Option<String>) -> Value {
    match next {
        None => Value::Null,
        Some(c) => json!({
            "omitted": total - start - shown,
            "how_to_page": format!("call again with cursor: \"{c}\""),
        }),
    }
}

fn footer(doc: &Doc) -> String {
    format!("as_of {}", doc.rev())
}

fn base_entry(doc: &Doc, base: &mut Map<String, Value>, e: gmclip_model::EntityId) {
    if let Some(r) = doc.entity_rev(e) {
        base.insert(e.to_string(), json!(r));
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The cut an entity belongs to (walking parents).
pub fn cut_of(doc: &Doc, e: gmclip_model::EntityId) -> Option<gmclip_model::EntityId> {
    let mut cur = e;
    for _ in 0..8 {
        if cur.kind.0 == "cut" {
            return Some(cur);
        }
        cur = doc.parent_of(cur)?;
    }
    None
}

/// Pops items from a paged array until the value fits `max` bytes, keeping
/// `next_cursor` and `truncated` true to what was returned.
fn fit_page(out: &mut Value, key: &str, start: usize, total: usize, max: usize) {
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len());
    let mut changed = false;
    while size(out) > max {
        let Some(a) = out.get_mut(key).and_then(Value::as_array_mut) else {
            break;
        };
        if a.len() <= 1 {
            break;
        }
        a.pop();
        changed = true;
    }
    if changed {
        let shown = out[key].as_array().map_or(0, Vec::len);
        let next = (start + shown < total).then(|| (start + shown).to_string());
        out["truncated"] = truncation(total, start, shown, &next);
        out["next_cursor"] = json!(next);
    }
}

/// `project_overview`; a launched run (`binding`) sees only its own cut and sources.
pub fn overview(
    doc: &Doc,
    project: &crate::holder::OpenProject,
    binding: Option<Id<Cut>>,
    cur: Option<&str>,
) -> Result<Value, CmdError> {
    let start = cursor(cur)?;
    let all_cuts: Vec<&Cut> = doc
        .iter::<Cut>()
        .filter(|c| binding.is_none_or(|b| b == c.id))
        .collect();
    let items = all_cuts.iter().map(|c| {
        let parts: Vec<&Part> = doc
            .children::<Part>(c.id.entity())
            .filter_map(|p| doc.get(p))
            .collect();
        let dur: f64 = parts.iter().map(|p| secs(p.src_out) - secs(p.src_in)).sum();
        let framings: Vec<String> = doc
            .children::<Framing>(c.id.entity())
            .filter_map(|f| doc.get(f))
            .map(|f| canon::clip(&f.name, 64).to_owned())
            .collect();
        json!({
            "id": c.id.0.to_string(),
            "rev": doc.entity_rev(c.id.entity()),
            "title": canon::field(canon::clip(&c.title, 200), Kind::Text),
            "kind": c.kind.name(),
            "parts": parts.len(),
            "duration_s": (dur * 1000.0).round() / 1000.0,
            "framings": framings,
        })
    });
    let (cut_page, total, next, _) = page(items, start, 30, DEFAULT_BYTES / 2);
    let mut base = Map::new();
    for c in &cut_page {
        if let (Some(id), Some(rev)) = (c["id"].as_str(), c["rev"].as_u64()) {
            base.insert(format!("cut:{id}"), json!(rev));
        }
    }
    let used: std::collections::BTreeSet<Id<Source>> = match binding {
        None => doc.iter::<Source>().map(|s| s.id).collect(),
        Some(b) => doc
            .children::<Part>(b.entity())
            .filter_map(|p| doc.get(p))
            .filter_map(|p| match p.media {
                MediaRef::Source(s) => Some(s),
                MediaRef::Group(_) => None,
            })
            .collect(),
    };
    let sources: Vec<Value> = used
        .iter()
        .filter_map(|s| doc.get(*s))
        .take(20)
        .map(|s| json!({"id": s.id.0.to_string(), "file": canon::field(canon::clip(file_name(&s.path), 255), Kind::Name)}))
        .collect();
    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    if binding.is_none() {
        for m in doc.iter::<Moment>() {
            *by_state
                .entry(format!("{:?}", m.state).to_lowercase())
                .or_default() += 1;
        }
    }
    let display = format!(
        "project {} · {} cut(s) · {} source(s){}\n{}\n{}",
        canon::block(&project.info.name, Kind::Name),
        total,
        used.len(),
        if binding.is_some() {
            " · this run's cut only".to_owned()
        } else {
            format!(" · {} moment(s)", doc.count::<Moment>())
        },
        cut_page
            .iter()
            .map(|c| format!(
                "- cut {} {} · {} parts · {} s",
                c["id"].as_str().unwrap_or(""),
                canon::block(c["title"]["text"].as_str().unwrap_or(""), Kind::Text),
                c["parts"],
                c["duration_s"]
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        footer(doc)
    );
    let shown = cut_page.len();
    let mut out = json!({
        "project": {"id": project.info.id, "name": canon::field(&project.info.name, Kind::Name)},
        "as_of": doc.rev(),
        "cuts": cut_page,
        "truncated": truncation(total, start, shown, &next),
        "next_cursor": next,
        "sources": sources,
        "moments": by_state,
        "base": base,
        "display": display,
    });
    fit_page(&mut out, "cuts", start, total, DEFAULT_BYTES);
    Ok(out)
}

fn part_json(doc: &Doc, p: &Part, i: usize) -> Value {
    json!({
        "id": p.id.0.to_string(),
        "rev": doc.entity_rev(p.id.entity()),
        "i": i,
        "media": p.media,
        "src_in": p.src_in.0,
        "src_out": p.src_out.0,
        "src_in_s": secs(p.src_in),
        "dur_s": ((secs(p.src_out) - secs(p.src_in)) * 1000.0).round() / 1000.0,
        "role": p.role,
        "gain_db": p.gain_db,
        "speed": p.speed,
        "ramp": p.ramp,
        "look": p.look.iter().map(|(f, l)| (f.0.to_string(), json!({"layout": l.layout_ref.map(|x| x.0.to_string()), "overrides": l.overrides.len()}))).collect::<Map<_, _>>(),
    })
}

fn layer_json(doc: &Doc, l: &Layer) -> Value {
    json!({
        "id": l.id.0.to_string(),
        "rev": doc.entity_rev(l.id.entity()),
        "role": l.role,
        "crop": l.crop,
        "place": l.place,
        "z": l.z,
        "visible": l.visible,
        "opacity": l.opacity,
    })
}

/// Which slice of a cut `cut_get` returns.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    /// Title, notes, counts, framings and the first parts (default).
    #[default]
    Summary,
    /// Every part, paged.
    Parts,
    /// Framings, layouts and their layers, and each part's stack per framing with where
    /// every layer value comes from (split, part, layout, framing, own), paged.
    Look,
    /// Overlays, paged.
    Overlays,
    /// What the cut says now (decision 0040): each part's words in cut order with
    /// their ids, silence around and between them, parts that start or end
    /// mid-sentence or cut a word, silent parts, and the retakes left in. Read it
    /// after every edit; `transcript_read` shows the source, struck words and all.
    Words,
}

/// `cut_get`.
pub fn cut_get(
    doc: &Doc,
    cut: Id<Cut>,
    proj: Projection,
    cur: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, CmdError> {
    let c = doc.get(cut).ok_or(CmdError::NotFound)?;
    let start = cursor(cur)?;
    let limit = limit.unwrap_or(500).clamp(1, 2_000);
    let mut base = Map::new();
    base_entry(doc, &mut base, cut.entity());
    let parts: Vec<&Part> = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .collect();
    let dur: f64 = parts.iter().map(|p| secs(p.src_out) - secs(p.src_in)).sum();
    let head = json!({
        "id": c.id.0.to_string(),
        "rev": doc.entity_rev(cut.entity()),
        "title": canon::field(&c.title, Kind::Text),
        "kind": c.kind.name(),
        "notes": canon::field(canon::clip(&c.notes, 2_000), Kind::Text),
        "notes_truncated": c.notes.len() > 2_000,
        "parts": parts.len(),
        "duration_s": (dur * 1000.0).round() / 1000.0,
    });
    let mut head = head;
    // a long-form's YouTube chapters from its topics (decision 0024): the lines to paste,
    // and why YouTube will not show them when it will not
    if c.kind == CutKind::Long {
        let ch = gmclip_core::commands::longform::chapters_for(doc, cut);
        head["chapters"] = json!({
            "youtube": canon::field(canon::clip(&ch.youtube, 4_000), Kind::Text),
            "count": ch.youtube_chapters.len(),
            "valid": ch.valid,
            "why_not": ch.why_not,
            "notes": ch.notes.iter().map(|n| canon::field(canon::clip(n, 300), Kind::Text)).collect::<Vec<_>>(),
        });
    }
    let mut out = json!({ "cut": head, "projection": proj, "as_of": doc.rev() });
    let lines: String;
    match proj {
        // `words` reads transcripts ([`cut_words`]); without them it lists the parts
        Projection::Summary | Projection::Parts | Projection::Words => {
            // parts take at most half the page: `base` and `display` grow with them
            let budget = if proj == Projection::Summary {
                DEFAULT_BYTES / 2
            } else {
                PAGE_BYTES / 2
            };
            let lim = if proj == Projection::Summary {
                limit.min(12)
            } else {
                limit
            };
            let items = parts.iter().enumerate().map(|(i, p)| part_json(doc, p, i));
            let (pg, total, next, end) = page(items, start, lim, budget);
            for p in &parts[start.min(parts.len())..end.min(parts.len())] {
                base_entry(doc, &mut base, p.id.entity());
            }
            lines = pg
                .iter()
                .map(|p| {
                    format!(
                        "{:>3}. part {} {:?} {} s +{} s gain {} dB",
                        p["i"],
                        p["id"].as_str().unwrap_or(""),
                        p["role"].as_str().unwrap_or(""),
                        p["src_in_s"],
                        p["dur_s"],
                        p["gain_db"]
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            out["truncated"] = truncation(total, start, pg.len(), &next);
            out["next_cursor"] = json!(next);
            out["parts"] = Value::Array(pg);
            // the recording markers it plays (decision 0052), on the cut's clock; the
            // rest from marker_list
            let markers = gmclip_core::commands::marker::cut_markers(doc, cut);
            if !markers.is_empty() && start == 0 {
                let shown = if proj == Projection::Summary { 20 } else { 100 };
                out["markers"] = json!(
                    markers
                        .iter()
                        .take(shown)
                        .map(
                            |m| json!({"n": m.n, "cut_s": m.cut_ms as f64 / 1000.0, "part": m.part})
                        )
                        .collect::<Vec<_>>()
                );
                out["markers_total"] = json!(markers.len());
            }
            if proj == Projection::Summary {
                let framings: Vec<Value> = doc
                    .children::<Framing>(cut.entity())
                    .filter_map(|f| doc.get(f))
                    .map(|f| json!({"id": f.id.0.to_string(), "name": f.name, "w": f.canvas.w, "h": f.canvas.h}))
                    .collect();
                out["framings"] = Value::Array(framings);
                out["overlays"] = json!(doc.children::<Overlay>(cut.entity()).count());
            }
        }
        Projection::Look => {
            let framings: Vec<Value> = doc
                .children::<Framing>(cut.entity())
                .filter_map(|f| doc.get(f))
                .map(|f| {
                    base_entry(doc, &mut base, f.id.entity());
                    let layers: Vec<Value> = doc.children::<Layer>(f.id.entity()).filter_map(|l| doc.get(l)).map(|l| layer_json(doc, l)).collect();
                    json!({"id": f.id.0.to_string(), "name": f.name, "w": f.canvas.w, "h": f.canvas.h, "layers": layers})
                })
                .collect();
            let layouts: Vec<Value> = doc
                .children::<Layout>(cut.entity())
                .filter_map(|l| doc.get(l))
                .map(|l| {
                    let layers: Vec<Value> = doc
                        .children::<Layer>(l.id.entity())
                        .filter_map(|x| doc.get(x))
                        .map(|x| layer_json(doc, x))
                        .collect();
                    json!({"id": l.id.0.to_string(), "name": l.name, "layers": layers})
                })
                .collect();
            // A2.09 (SPEC §5.7): each part's stack per framing at its first frame — the
            // linked layout (or none: the framing's defaults) and, per layer, where each
            // value comes from (split / part / layout / framing / own); paged
            let fids: Vec<Id<Framing>> = doc.children::<Framing>(cut.entity()).collect();
            let items = parts.iter().map(|p| {
                let per: Map<String, Value> = fids
                    .iter()
                    .map(|f| {
                        let st = gmclip_model::effective::effective(doc, p.id, *f, p.src_in);
                        let layers: Vec<Value> = st
                            .map(|st| st.layers)
                            .unwrap_or_default()
                            .iter()
                            .map(|l| {
                                let from = gmclip_model::effective::value_sources(
                                    doc, p.id, *f, l.id, p.src_in,
                                );
                                json!({"id": l.id.0.to_string(), "role": l.values.role,
                                       "kind": l.values.kind, "addition": l.addition, "from": from})
                            })
                            .collect();
                        let layout = p.look.get(f).and_then(|lk| lk.layout_ref);
                        (
                            f.0.to_string(),
                            json!({"layout": layout.map(|x| x.0.to_string()), "linked": layout.is_some(), "layers": layers}),
                        )
                    })
                    .collect();
                json!({"id": p.id.0.to_string(), "rev": doc.entity_rev(p.id.entity()), "stacks": per})
            });
            let (pg, total, next, _) = page(items, start, limit, PAGE_BYTES / 2);
            lines = format!(
                "{} framing(s), {} layout(s), {} part stack(s) shown",
                framings.len(),
                layouts.len(),
                pg.len()
            );
            out["framings"] = Value::Array(framings);
            out["layouts"] = Value::Array(layouts);
            out["truncated"] = truncation(total, start, pg.len(), &next);
            out["next_cursor"] = json!(next);
            out["parts"] = Value::Array(pg);
        }
        Projection::Overlays => {
            let items = doc.children::<Overlay>(cut.entity()).filter_map(|o| doc.get(o)).map(|o| {
                json!({
                    "id": o.id.0.to_string(),
                    "rev": doc.entity_rev(o.id.entity()),
                    "kind": o.kind,
                    "start": o.start,
                    "end": o.end,
                    "band": o.band,
                    "z": o.z,
                    "opacity": o.opacity,
                    "geom": o.geom,
                    "text": o.text.as_ref().map(|t| canon::field(&t.runs.iter().map(|r| r.text.as_str()).collect::<String>(), Kind::Text)),
                })
            });
            let (pg, total, next, _) = page(items, start, limit, PAGE_BYTES * 9 / 10);
            lines = format!("{} overlay(s)", total);
            out["truncated"] = truncation(total, start, pg.len(), &next);
            out["next_cursor"] = json!(next);
            out["overlays"] = Value::Array(pg);
        }
    }
    out["base"] = Value::Object(base);
    let key = match proj {
        Projection::Overlays => "overlays",
        _ => "parts",
    };
    let total_items = out["cut"]["parts"].as_u64().unwrap_or(0) as usize;
    let total_items = if key == "overlays" {
        doc.children::<Overlay>(cut.entity()).count()
    } else {
        total_items
    };
    out["display"] = json!(format!(
        "cut {} {} · {} parts · {} s\n{}\n{}",
        c.id.0,
        canon::block(&c.title, Kind::Text),
        parts.len(),
        (dur * 10.0).round() / 10.0,
        lines,
        footer(doc)
    ));
    if proj != Projection::Look {
        let max = if proj == Projection::Summary {
            DEFAULT_BYTES
        } else {
            PAGE_BYTES - 512
        };
        fit_page(&mut out, key, start, total_items, max);
    }
    Ok(out)
}

fn clock(f: i64) -> String {
    let s = (f.max(0) as f64 / SEC) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `cut_get{projection: words}` (decision 0040): what the cut says now, part by part,
/// paged; every word's text is source-derived (untrusted).
pub fn cut_words(
    doc: &Doc,
    view: &gmclip_core::analysis::AnalysisView,
    cut: Id<Cut>,
    cur: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, CmdError> {
    let c = doc.get(cut).ok_or(CmdError::NotFound)?;
    let start = cursor(cur)?;
    let limit = limit.unwrap_or(500).clamp(1, 2_000);
    let sc = gmclip_core::script::script(doc, view, cut);
    let f = |x: i64| (x as f64 / SEC * 100.0).round() / 100.0;
    let name = |m: &MediaRef| match m {
        MediaRef::Source(s) => doc
            .get(*s)
            .map_or_else(String::new, |s| file_name(&s.path).to_owned()),
        MediaRef::Group(_) => "sync group".to_owned(),
    };
    let gap = (gmclip_core::script::GAP_S * SEC) as i64;
    let notes_of = |p: &gmclip_core::script::PartScript| -> Vec<String> {
        let mut n = Vec::new();
        if p.no_audio && p.words.is_empty() {
            n.push(format!("no sound: says nothing for {:.1} s", f(p.dur)));
        } else if !p.transcript {
            n.push("no transcript".into());
        } else if p.silent() {
            n.push(format!("says nothing for {:.1} s", f(p.dur)));
        } else {
            if p.lead >= gap {
                n.push(format!("{:.1} s silent before", f(p.lead)));
            }
            for g in &p.gaps {
                n.push(format!("{:.1} s silent at {}", f(g.len), clock(g.at)));
            }
            if p.trail >= gap {
                n.push(format!("{:.1} s silent after", f(p.trail)));
            }
            if p.starts_mid_sentence {
                n.push("starts mid-sentence".into());
            }
            if p.ends_mid_sentence {
                n.push("ends mid-sentence".into());
            }
            if p.cuts_word_in.is_some() || p.cuts_word_out.is_some() {
                n.push("an edge cuts a word".into());
            }
        }
        n
    };
    let items = sc.parts.iter().map(|p| {
        let text = canon::canonical(&p.text(), Kind::Text);
        json!({
            "i": p.index,
            "id": p.part.0.to_string(),
            "rev": doc.entity_rev(p.part.entity()),
            "media": p.media,
            "file": canon::field(canon::clip(&name(&p.media), 255), Kind::Name),
            "at_s": f(p.start), "dur_s": f(p.dur), "src_in_s": f(p.src_in), "src_out_s": f(p.src_out),
            "text": {"text": text, "untrusted": true},
            "words": p.words.iter().map(|w| json!([w.ordinal, canon::canonical(&w.text, Kind::Text), w.src_start, w.src_end])).collect::<Vec<_>>(),
            "notes": notes_of(p),
        })
    });
    let (pg, total, next, end) = page(items, start, limit, PAGE_BYTES * 2 / 3);
    let mut base = Map::new();
    base_entry(doc, &mut base, cut.entity());
    for p in &sc.parts[start.min(sc.parts.len())..end.min(sc.parts.len())] {
        base_entry(doc, &mut base, p.part.entity());
    }
    let lines = pg
        .iter()
        .map(|p| {
            let notes = p["notes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_default();
            let first = p["words"]
                .get(0)
                .and_then(|w| w.get(0))
                .and_then(Value::as_u64);
            let last = p["words"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|w| w.get(0))
                .and_then(Value::as_u64);
            format!(
                "{:>3}. part {} · {} · at {} · {} s{}{}\n     {}",
                p["i"],
                p["id"].as_str().unwrap_or(""),
                canon::block(p["file"]["text"].as_str().unwrap_or(""), Kind::Name),
                clock((p["at_s"].as_f64().unwrap_or(0.0) * SEC) as i64),
                p["dur_s"],
                match (first, last) {
                    (Some(a), Some(b)) => format!(" · words {a}–{b}"),
                    _ => String::new(),
                },
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ⚠ {notes}")
                },
                canon::block(
                    canon::clip(p["text"]["text"].as_str().unwrap_or(""), 1_200),
                    Kind::Text
                ),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let retakes: Vec<Value> = sc
        .retakes
        .iter()
        .map(|r| {
            json!({"kind": r.kind, "at_s": f(r.at), "parts": r.parts,
                   "earlier": canon::field(canon::clip(&r.earlier, 400), Kind::Text),
                   "later": canon::field(canon::clip(&r.later, 400), Kind::Text)})
        })
        .collect();
    let speech = sc.parts.iter().filter(|p| !p.words.is_empty()).count();
    let silent_parts = sc.parts.iter().filter(|p| p.silent()).count();
    let dur: i64 = sc.parts.iter().map(|p| p.dur).sum();
    let summary = format!(
        "{} part(s), {} with speech, {} silent · {:.1} s · {:.1} s of silence around or between words · {} retake(s) left{}",
        sc.parts.len(),
        speech,
        silent_parts,
        f(dur),
        sc.silent_s(),
        sc.retakes.len(),
        if sc.retakes.is_empty() {
            String::new()
        } else {
            format!(
                " (sweep_retakes strikes them): {}",
                sc.retakes
                    .iter()
                    .take(8)
                    .map(|r| format!(
                        "at {} {} then {}",
                        clock(r.at),
                        canon::block(canon::clip(&r.earlier, 80), Kind::Text),
                        canon::block(canon::clip(&r.later, 80), Kind::Text)
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    );
    let mut out = json!({
        "cut": {"id": c.id.0.to_string(), "rev": doc.entity_rev(cut.entity()), "title": canon::field(&c.title, Kind::Text),
                "parts": sc.parts.len(), "duration_s": f(dur), "silent_s": (sc.silent_s() * 10.0).round() / 10.0},
        "projection": "words",
        "parts": pg,
        "retakes": retakes,
        "asr_revisions": sc.revisions.iter().map(|(o, r)| json!({"owner": o, "revision": r})).collect::<Vec<_>>(),
        "truncated": truncation(total, start, end - start, &next),
        "next_cursor": next,
        "base": base,
        "as_of": doc.rev(),
    });
    out["display"] = json!(format!(
        "cut {} {} · what it says now\n{summary}\n{lines}\nStrike words with cut_edit words_strike (consecutive word ids of one transcript); remove silence with sweep_pauses; read this again after each edit.\n{}",
        c.id.0,
        canon::block(&c.title, Kind::Text),
        footer(doc)
    ));
    fit_page(&mut out, "parts", start, total, PAGE_BYTES - 512);
    Ok(out)
}

/// `moments_list`.
pub fn moments(
    doc: &Doc,
    state: Option<MomentState>,
    min_score: Option<u8>,
    cur: Option<&str>,
    limit: Option<usize>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let start = cursor(cur)?;
    // a launched run sees only its own cut's moment
    let only = binding.map(|b| doc.get(b).and_then(|c| c.moment));
    let mut ms: Vec<&Moment> = doc
        .iter::<Moment>()
        .filter(|m| only.is_none_or(|o| o == Some(m.id)))
        .filter(|m| state.is_none_or(|s| m.state == s) && min_score.is_none_or(|s| m.score >= s))
        .collect();
    ms.sort_by(|a, b| b.score.cmp(&a.score).then(a.id.0.cmp(&b.id.0)));
    let items = ms.iter().map(|m| {
        json!({
            "id": m.id.0.to_string(),
            "rev": doc.entity_rev(m.id.entity()),
            "score": m.score,
            "type": m.r#type,
            "state": m.state,
            "media": m.media,
            "span": m.span,
            "quote": canon::field(canon::clip(&m.quote, 600), Kind::Text),
            "text_hook": m.text_hook.as_deref().map(|t| canon::field(canon::clip(t, 200), Kind::Text)),
            "why": canon::field(canon::clip(&m.why, 400), Kind::Text),
        })
    });
    let (pg, total, next, _) = page(
        items,
        start,
        limit.unwrap_or(20).clamp(1, 200),
        DEFAULT_BYTES / 2,
    );
    let shown = pg.len();
    let display = format!(
        "{} moment(s)\n{}\n{}",
        total,
        pg.iter()
            .map(|m| format!(
                "- {} ★{} {}",
                m["id"].as_str().unwrap_or(""),
                m["score"],
                canon::block(
                    canon::clip(m["quote"]["text"].as_str().unwrap_or(""), 80),
                    Kind::Text
                )
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        footer(doc)
    );
    let mut out = json!({
        "moments": pg,
        "truncated": truncation(total, start, shown, &next),
        "next_cursor": next,
        "as_of": doc.rev(),
        "display": display,
    });
    fit_page(&mut out, "moments", start, total, DEFAULT_BYTES);
    Ok(out)
}

/// `lint`: the core rule set (SPEC §5.13 [M1] agent set); never a refusal.
pub fn lint(
    doc: &Doc,
    view: Option<&gmclip_core::analysis::AnalysisView>,
    cut: Id<Cut>,
) -> Result<Value, CmdError> {
    let c = doc.get(cut).ok_or(CmdError::NotFound)?;
    let w = gmclip_core::lint::lint_cut_in(doc, view, cut);
    let errors = w
        .iter()
        .filter(|f| f.level == gmclip_core::lint::Level::Error)
        .count();
    let display = format!(
        "lint {} — {} finding(s), {} error-level (agent exports need approval while any remain)\n{}\n{}",
        canon::block(&c.title, Kind::Text),
        w.len(),
        errors,
        w.iter()
            .map(|x| format!("- {:?} {}: {}", x.level, x.rule, x.message).to_lowercase())
            .collect::<Vec<_>>()
            .join("\n"),
        footer(doc)
    );
    Ok(json!({"findings": w, "errors": errors, "as_of": doc.rev(), "display": display}))
}

/// Which cut a launched run may read.
pub fn check_cut_binding(binding: Option<Id<Cut>>, cut: Id<Cut>) -> Result<(), CmdError> {
    match binding {
        Some(b) if b != cut => Err(CmdError::Unauthorized {
            scope: "cut_binding".into(),
        }),
        _ => Ok(()),
    }
}

/// Shrinks a value until it fits a page: arrays are cut from the end, with a note.
pub fn cap(mut v: Value, max_bytes: usize) -> Value {
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len());
    // room for the note this adds
    let max_bytes = max_bytes.saturating_sub(256);
    if size(&v) <= max_bytes {
        return v;
    }
    let mut cut_items = 0;
    for _ in 0..64 {
        let total = size(&v);
        if total <= max_bytes {
            break;
        }
        let Some(obj) = v.as_object_mut() else { break };
        let biggest = obj
            .iter()
            .filter(|(_, x)| x.as_array().is_some_and(|a| !a.is_empty()))
            .max_by_key(|(_, x)| size(x))
            .map(|(k, _)| k.clone());
        let Some(k) = biggest else { break };
        if let Some(a) = obj.get_mut(&k).and_then(Value::as_array_mut) {
            // shrink in proportion to the overshoot (at least one item)
            let keep = (a.len() * max_bytes / total.max(1)).min(a.len() - 1);
            cut_items += a.len() - keep;
            a.truncate(keep);
        }
    }
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "size_capped".into(),
            json!({"omitted_items": cut_items, "how_to_page": "ask for a narrower projection, a cursor or a smaller limit"}),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_cuts_and_says_so() {
        let v = json!({"items": (0..10_000).map(|i| json!({"i": i, "pad": "x".repeat(20)})).collect::<Vec<_>>(), "display": "d"});
        let c = cap(v, 5_000);
        assert!(serde_json::to_string(&c).unwrap().len() <= 5_200);
        assert!(c["size_capped"]["omitted_items"].as_u64().unwrap() > 0);
    }
}
