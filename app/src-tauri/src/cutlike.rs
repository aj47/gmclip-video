//! "New cut like this" (L.05): a finished cut's look on another moment — its framings
//! (canvas, rate, caption grouping and seat, loudness), each framing's layers, the first
//! part's layout and its crops, places and insets, the caption settings and style, the
//! text hook's and pills' styles and places — but none of its parts, words or numbers.
//!
//! The look is a [`CutTemplate`]: content only (R-PROF-4 — sizes, styles, places, never a
//! harness, budget, path, scope or anything that spends), saved by name per active
//! profile in the app data beside the layout presets (the profile folder is content
//! gmclip never writes, SPEC §10.5), and applied in any project. A style whose name the
//! target project already uses for something else is added under the template's name,
//! so another cut's look never changes.
//!
//! Everything is registry commands (`cut_create`, `framing_create`, `framing_set`,
//! `layout_edit`, `caption_set`, `text_style_set`, `font_add`, `overlay_add`), so an agent
//! can do the same by hand.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_model::entities::{
    CaptionDoc, CaptionGrouping, CaptionSource, Cut, Font, Framing, Layer, LayerRole, LoudnessMode,
    Moment, Overlay, OverlayEnd, OverlayGeom, Part, Styles, TextStyle,
};
use gmclip_model::geometry::Place;
use gmclip_model::{Doc, Id, Rational};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};
use crate::takelayout::PresetLayer;

pub const SCHEMA: &str = "gmclip.cut-template/0";

/// One layer of a framing's default stack or of a layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerT {
    pub role: LayerRole,
    pub crop: [f64; 4],
    pub place: [f64; 3],
    pub z: i32,
    pub visible: bool,
    pub opacity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutT {
    pub name: String,
    pub layers: Vec<LayerT>,
}

/// One framing's look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FramingT {
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub fps: Rational,
    pub caption_grouping: CaptionGrouping,
    pub caption_place: Option<Place>,
    pub caption_style: Option<String>,
    pub loudness: Option<LoudnessMode>,
    /// The framing's own default layers.
    pub defaults: Vec<LayerT>,
    /// The layout the first part links to, if any.
    pub layout: Option<LayoutT>,
    /// The first part's effective stack (crops, places, its insets).
    pub part: Vec<PresetLayer>,
    pub part_caption_place: Option<Place>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptionsT {
    pub enabled: bool,
    pub source: CaptionSource,
    pub drop_fillers: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontT {
    pub family: String,
    pub style: String,
    pub sha256: Option<String>,
    pub em_per_size: f64,
}

/// A text hook or pill: where it sits and how it looks (a pill keeps its words, which the
/// person retypes; the hook takes the new moment's).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayT {
    pub role: String,
    pub band: Option<String>,
    pub place: Place,
    pub style: TextStyle,
    pub text: String,
    pub dur_ms: Option<u32>,
    pub z: i32,
}

/// A cut's look (content only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CutTemplate {
    pub schema: String,
    pub name: String,
    pub framings: Vec<FramingT>,
    /// Which framing the new cut shows.
    pub active: usize,
    pub captions: Option<CaptionsT>,
    pub styles: BTreeMap<String, TextStyle>,
    pub fonts: Vec<FontT>,
    pub hook: Option<OverlayT>,
    pub pills: Vec<OverlayT>,
}

fn layer_t(l: &Layer) -> LayerT {
    LayerT {
        role: l.role,
        crop: [l.crop.x, l.crop.y, l.crop.w, l.crop.h],
        place: [l.place.x, l.place.y, l.place.w],
        z: l.z,
        visible: l.visible,
        opacity: l.opacity,
    }
}

fn layers_of(doc: &Doc, owner: gmclip_model::EntityId) -> Vec<LayerT> {
    let mut v: Vec<&Layer> = doc
        .children::<Layer>(owner)
        .filter_map(|id| doc.get(id))
        .collect();
    v.sort_by(|a, b| a.order.cmp(&b.order));
    v.into_iter().map(layer_t).collect()
}

fn text_of(o: &Overlay) -> String {
    o.text
        .as_ref()
        .map(|t| t.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .unwrap_or_default()
}

/// The look of `cut`, as a template named `name`.
pub fn capture(core: &AppCore, project: &str, cut: &str, name: &str) -> AppResult<CutTemplate> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let mut parts: Vec<&Part> = doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    parts.sort_by(|a, b| a.order.cmp(&b.order));
    let first = parts
        .first()
        .ok_or_else(|| AppError::invalid("that cut has no parts to take a look from"))?;
    let frs: Vec<&Framing> = doc
        .children::<Framing>(cid.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    if frs.is_empty() {
        return Err(AppError::invalid("that cut has no framing yet"));
    }
    let active_id = core.active_framing(project, cut);
    let mut framings = Vec::new();
    let mut active = 0;
    for (i, f) in frs.iter().enumerate() {
        if active_id.as_deref() == Some(f.id.0.to_string().as_str()) {
            active = i;
        }
        let layout = first
            .look
            .get(&f.id)
            .and_then(|l| l.layout_ref)
            .and_then(|l| doc.get(l))
            .map(|l| LayoutT {
                name: l.name.clone(),
                layers: layers_of(&doc, l.id.entity()),
            });
        let part = crate::takelayout::stack_of(&doc, first.id, f.id)?
            .into_iter()
            .map(|x| x.1)
            .collect();
        framings.push(FramingT {
            name: f.name.clone(),
            w: f.canvas.w,
            h: f.canvas.h,
            fps: f.fps,
            caption_grouping: f.caption_grouping,
            caption_place: f.caption_place,
            caption_style: f.caption_style_ref.clone(),
            loudness: f.loudness_mode,
            defaults: layers_of(&doc, f.id.entity()),
            layout,
            part,
            part_caption_place: first.caption_place_override.get(&f.id).copied(),
        });
    }
    let captions = doc
        .children::<CaptionDoc>(cid.entity())
        .find_map(|id| doc.get(id))
        .map(|c| CaptionsT {
            enabled: c.enabled,
            source: c.source,
            drop_fillers: c.drop_fillers,
        });
    let styles = doc
        .iter::<Styles>()
        .next()
        .map(|s| s.entries.clone())
        .unwrap_or_default();
    let fonts = doc
        .iter::<Font>()
        .map(|f| FontT {
            family: f.family.clone(),
            style: f.style.clone(),
            sha256: f.sha256.clone(),
            em_per_size: f.em_per_size,
        })
        .collect();
    let overlays: Vec<&Overlay> = doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    let ov = |o: &Overlay| -> Option<OverlayT> {
        let place = match o.geom {
            OverlayGeom::Place(p) => p,
            OverlayGeom::Box(_) => return None,
        };
        Some(OverlayT {
            role: o.role.clone()?,
            band: o.band.clone(),
            place,
            style: o.text.as_ref()?.style.clone(),
            text: text_of(o),
            dur_ms: match o.end {
                OverlayEnd::DurMs(ms) => Some(ms),
                OverlayEnd::At(_) => None,
            },
            z: o.z,
        })
    };
    let hook = overlays
        .iter()
        .find(|o| o.role.as_deref() == Some("text_hook"))
        .and_then(|o| ov(o));
    let pills = overlays
        .iter()
        .filter(|o| o.role.as_deref() == Some("section_pill"))
        .filter_map(|o| ov(o))
        .collect();
    let name = name.trim();
    Ok(CutTemplate {
        schema: SCHEMA.into(),
        name: if name.is_empty() {
            doc.get(cid).map(|c| c.title.clone()).unwrap_or_default()
        } else {
            name.to_owned()
        }
        .chars()
        .take(60)
        .collect(),
        framings,
        active,
        captions,
        styles,
        fonts,
        hook,
        pills,
    })
}

fn out(v: &Value, k: &str) -> String {
    v["output"][k].as_str().unwrap_or_default().to_owned()
}

fn layer_ops(owner: Value, layers: &[LayerT]) -> Vec<Value> {
    layers
        .iter()
        .map(|l| {
            json!({"op": "layer_add", "owner": owner, "role": l.role,
                   "crop": {"x": l.crop[0], "y": l.crop[1], "w": l.crop[2], "h": l.crop[3]},
                   "place": {"x": l.place[0], "y": l.place[1], "w": l.place[2]}, "z": l.z})
        })
        .collect()
}

/// After `layer_add`s, the hidden / translucent layers' settings (by owner, in order).
fn layer_sets(doc: &Doc, owner: gmclip_model::EntityId, want: &[LayerT]) -> Vec<Value> {
    let mut have: Vec<&Layer> = doc
        .children::<Layer>(owner)
        .filter_map(|id| doc.get(id))
        .collect();
    have.sort_by(|a, b| a.order.cmp(&b.order));
    have.iter()
        .zip(want)
        .filter(|(h, w)| h.visible != w.visible || (h.opacity - w.opacity).abs() > 1e-9)
        .map(|(h, w)| json!({"op": "layer_set", "layer": h.id.0.to_string(), "visible": w.visible, "opacity": w.opacity}))
        .collect()
}

fn same_style(a: &TextStyle, b: &TextStyle) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

/// A new cut on `moment` with the template's look; returns the cut, its framings and the
/// styles it had to add under new names.
pub fn apply(core: &AppCore, project: &str, t: &CutTemplate, moment: &str) -> AppResult<Value> {
    if t.schema != SCHEMA {
        return Err(AppError::invalid("not a cut template gmclip reads"));
    }
    let f0 = t
        .framings
        .get(t.active)
        .or(t.framings.first())
        .ok_or_else(|| AppError::invalid("the template has no framing"))?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let mid: Id<Moment> = ulid_id(moment)?;
    let m: &Moment = doc
        .get(mid)
        .ok_or_else(|| AppError::not_found("no such moment"))?;
    let tr = core.transcripts.latest(&p, &crate::review::owner_of(m));
    let (span, _) = crate::review::spans(&doc, m, tr.as_deref())
        .ok_or_else(|| AppError::new("Unsupported", "this moment's times need its transcript"))?;
    // fonts and styles the look needs
    for f in &t.fonts {
        let have = doc
            .iter::<Font>()
            .any(|x| x.family == f.family && x.style == f.style);
        if !have {
            core.call(
                project,
                "font_add",
                json!({"family": f.family, "style": f.style, "sha256": f.sha256, "em_per_size": f.em_per_size}),
            )?;
        }
    }
    let existing: BTreeMap<String, TextStyle> = doc
        .iter::<Styles>()
        .next()
        .map(|s| s.entries.clone())
        .unwrap_or_default();
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    for (name, st) in &t.styles {
        match existing.get(name) {
            Some(e) if same_style(e, st) => {}
            Some(_) => {
                // style names are [A-Za-z0-9_-]: "Speech-VM-look"
                let slug: String = t
                    .name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let alt: String = format!("{name}-{}", slug.trim_matches('-'))
                    .chars()
                    .take(64)
                    .collect();
                if !existing.get(&alt).is_some_and(|e| same_style(e, st)) {
                    core.call(project, "text_style_set", json!({"name": alt, "style": st}))?;
                }
                renamed.insert(name.clone(), alt);
            }
            None => {
                core.call(
                    project,
                    "text_style_set",
                    json!({"name": name, "style": st}),
                )?;
            }
        }
    }
    let title: String = m
        .text_hook
        .clone()
        .filter(|x| !x.trim().is_empty())
        .unwrap_or_else(|| m.excerpt.text.clone())
        .chars()
        .take(120)
        .collect();
    let title = if title.trim().is_empty() {
        "New cut".to_owned()
    } else {
        title
    };
    let cut = out(
        &core.call(
            project,
            "cut_create",
            json!({"title": title, "moment": moment}),
        )?,
        "cut",
    );
    // the one part, on the active framing's frame grid, inside what the media covers
    let fpf = gmclip_core::planner::framing_fpf(f0.fps).unwrap_or(FLICKS / 30);
    let (lo, hi) = crate::cutnew::media_range(&doc, m.media);
    let (a, b) = (span.0.0.max(lo), span.1.0.min(hi));
    if b <= a {
        return Err(AppError::invalid(
            "this moment is outside what the media covers",
        ));
    }
    let k0 = (a + fpf - 1) / fpf;
    let k1 = (b / fpf).max(k0 + 1);
    let part = out(
        &core.call(
            project,
            "part_add",
            json!({"cut": cut, "media": m.media, "src_in": k0 * fpf, "src_out": k1 * fpf, "role": "hook"}),
        )?,
        "id",
    );
    let pid = ulid_id::<Part>(&part)?;
    let mut ids = Vec::new();
    for ft in &t.framings {
        let fid = out(
            &core.call(
                project,
                "framing_create",
                json!({"cut": cut, "name": ft.name, "w": ft.w, "h": ft.h, "fps": ft.fps}),
            )?,
            "id",
        );
        let mut set = json!({"framing": fid, "caption_grouping": ft.caption_grouping});
        if let Some(cp) = ft.caption_place {
            set["caption_place"] = json!(cp);
        }
        if let Some(l) = &ft.loudness {
            set["loudness"] = json!(l);
        }
        if let Some(s) = &ft.caption_style {
            set["caption_style"] = json!(renamed.get(s).unwrap_or(s));
        } else if let Some(alt) = renamed.get("Speech") {
            set["caption_style"] = json!(alt);
        }
        core.call(project, "framing_set", set)?;
        let mut ops = layer_ops(json!({"framing": fid}), &ft.defaults);
        let mut layout = None;
        if let Some(l) = &ft.layout {
            let lid = out(
                &core.call(
                    project,
                    "layout_create",
                    json!({"cut": cut, "name": l.name}),
                )?,
                "id",
            );
            ops.extend(layer_ops(json!({"layout": lid}), &l.layers));
            ops.push(json!({"op": "layout_link", "part": part, "framing": fid, "layout": lid}));
            layout = Some((lid, l.layers.clone()));
        }
        if let Some(cp) = ft.part_caption_place {
            ops.push(json!({"op": "caption_place", "part": part, "framing": fid, "place": cp}));
        }
        crate::takelayout::run_ops(core, project, &cut, &ops)?;
        // visibility / opacity, then the part's own crops, places and insets
        let doc = p.handle.snapshot().doc.clone();
        let fidv = ulid_id::<Framing>(&fid)?;
        let mut more = layer_sets(&doc, fidv.entity(), &ft.defaults);
        if let Some((lid, layers)) = &layout {
            more.extend(layer_sets(
                &doc,
                ulid_id::<gmclip_model::entities::Layout>(lid)?.entity(),
                layers,
            ));
        }
        more.extend(crate::takelayout::stack_ops(&doc, pid, fidv, &ft.part)?);
        crate::takelayout::run_ops(core, project, &cut, &more)?;
        ids.push(fid);
    }
    let active = ids
        .get(t.active)
        .or(ids.first())
        .cloned()
        .unwrap_or_default();
    core.set_active_framing(project, &cut, &active)?;
    if let Some(c) = &t.captions {
        core.call(
            project,
            "caption_set",
            json!({"cut": cut, "enabled": c.enabled, "source": c.source, "drop_fillers": c.drop_fillers}),
        )?;
    }
    // the hook takes the moment's words; the pills keep theirs (the person retypes them)
    let hook_text = m.text_hook.clone().filter(|h| !h.trim().is_empty());
    if let (Some(h), Some(text)) = (&t.hook, &hook_text) {
        core.call(
            project,
            "overlay_add",
            json!({"cut": cut, "kind": "text", "role": "text_hook", "band": h.band,
                   "start": {"cut_start": {"cut": cut}}, "end": {"dur_ms": h.dur_ms.unwrap_or(5000)},
                   "place": h.place, "z": h.z,
                   "text": {"runs": crate::short::marked_runs(&crate::short::wrap_hook(text)), "style": h.style}}),
        )?;
    }
    let mut pills = 0;
    if let Some(pl) = t.pills.first() {
        let start = match (&t.hook, &hook_text) {
            (Some(h), Some(_)) => {
                json!({"raw": {"time": {"clock": "cut", "cut": cut,
                        "t": (i64::from(h.dur_ms.unwrap_or(5000)) * FLICKS / 1000).div_euclid(fpf) * fpf}, "reason": "after the hook"}})
            }
            _ => json!({"part_start": {"part": part}}),
        };
        core.call(
            project,
            "overlay_add",
            json!({"cut": cut, "kind": "text", "role": "section_pill", "band": pl.band,
                   "start": start, "end": {"at": {"part_end": {"part": part}}},
                   "place": pl.place, "z": pl.z,
                   "text": {"runs": crate::short::marked_runs(&pl.text), "style": pl.style}}),
        )?;
        pills = 1;
    }
    Ok(
        json!({"cut": cut, "framings": ids, "active": active, "seconds": ((k1 - k0) * fpf) as f64 / FLICKS as f64,
              "styles_renamed": renamed, "pills": pills, "hook": hook_text.is_some() && t.hook.is_some()}),
    )
}

// ---- saved templates (per profile, app data) -------------------------------------------

fn store_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("cut-templates.json")
}

fn profile_key(core: &AppCore) -> String {
    gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data)
        .profile
        .map_or_else(
            || "(no profile)".to_owned(),
            |p| p.to_string_lossy().into_owned(),
        )
}

type Store = BTreeMap<String, Vec<CutTemplate>>;

fn load_store(core: &AppCore) -> Store {
    std::fs::read(store_path(core))
        .ok()
        .filter(|b| b.len() < 8 << 20)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The active profile's saved templates.
pub fn templates(core: &AppCore) -> Vec<CutTemplate> {
    load_store(core)
        .remove(&profile_key(core))
        .unwrap_or_default()
}

/// Saves a template under its name (replacing one of the same name; ≤ 50 per profile).
pub fn save(core: &AppCore, t: CutTemplate) -> AppResult<()> {
    if t.name.trim().is_empty() {
        return Err(AppError::invalid("a template needs a name"));
    }
    let mut s = load_store(core);
    let list = s.entry(profile_key(core)).or_default();
    if list.len() >= 50 && !list.iter().any(|x| x.name == t.name) {
        return Err(AppError::invalid("at most 50 templates per profile"));
    }
    list.retain(|x| x.name != t.name);
    list.push(t);
    let b = serde_json::to_vec_pretty(&s).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&store_path(core), &b)
        .map_err(|e| AppError::internal(format!("saving the template: {}", e.kind())))
}

/// `cut_like`, `cut_template_save`, `cut_templates`, `cut_template_delete`.
pub fn ipc(core: &AppCore, command: &str, project: Option<&str>, a: &Value) -> AppResult<Value> {
    let s = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
    };
    match command {
        // a new cut on a moment, like `cut` (or like a saved template); saved too when named
        "cut_like" => {
            let pid = project.ok_or_else(|| AppError::invalid("a project is needed"))?;
            let t = match (
                a.get("cut").and_then(Value::as_str),
                a.get("template").and_then(Value::as_str),
            ) {
                (Some(c), _) => capture(
                    core,
                    pid,
                    c,
                    a.get("save_as").and_then(Value::as_str).unwrap_or(""),
                )?,
                (None, Some(name)) => templates(core)
                    .into_iter()
                    .find(|x| x.name == name)
                    .ok_or_else(|| AppError::not_found("no template of that name"))?,
                _ => return Err(AppError::invalid("`cut` or `template` is required")),
            };
            if a.get("save_as")
                .and_then(Value::as_str)
                .is_some_and(|n| !n.trim().is_empty())
            {
                save(core, t.clone())?;
            }
            apply(core, pid, &t, s("moment")?)
        }
        "cut_template_save" => {
            let pid = project.ok_or_else(|| AppError::invalid("a project is needed"))?;
            let t = capture(core, pid, s("cut")?, s("name")?)?;
            let name = t.name.clone();
            save(core, t)?;
            Ok(json!({"name": name}))
        }
        "cut_templates" => Ok(json!({"templates": templates(core).iter().map(|t| json!({
            "name": t.name, "framings": t.framings.iter().map(|f| format!("{}×{}", f.w, f.h)).collect::<Vec<_>>(),
            "hook": t.hook.is_some(), "pills": t.pills.len(), "captions": t.captions.as_ref().is_some_and(|c| c.enabled),
        })).collect::<Vec<_>>()})),
        "cut_template_delete" => {
            let name = s("name")?;
            let mut st = load_store(core);
            let list = st.entry(profile_key(core)).or_default();
            let n = list.len();
            list.retain(|x| x.name != name);
            if list.len() == n {
                return Err(AppError::not_found("no template of that name"));
            }
            let b =
                serde_json::to_vec_pretty(&st).map_err(|e| AppError::internal(e.to_string()))?;
            crate::core::write_atomic(&store_path(core), &b)
                .map_err(|e| AppError::internal(format!("saving: {}", e.kind())))?;
            Ok(json!({"deleted": name}))
        }
        _ => Err(AppError::invalid("unknown command")),
    }
}
