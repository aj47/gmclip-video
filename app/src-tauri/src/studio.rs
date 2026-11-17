//! The look of a cut beyond its layout (U.09 / A2.09 rest, SPEC §5.9, §5.10, §8): the
//! caption style gallery and the profile's style library.
//!
//! - **Gallery**: every caption preset the project can use — the built-ins
//!   (`gmclip_core::library`), the project's saved styles and the active profile's own
//!   ("my styles") — each drawn on the cut's current frame: the frame is rendered once
//!   through the export's compose without captions, then each preset's caption at that
//!   frame (its settled look) is rasterised over it and scaled down. Nothing is committed.
//! - **Apply**: the preset's face is added to the project when it is missing (`font_add`,
//!   from the app's font catalogue), then `caption_preset_apply` — one undo step for the
//!   style, grouping and look.
//! - **Save as my style**: the framing's current caption style, grouping and look become a
//!   project preset (`caption_preset_save`) and an entry of the profile's style library.
//!   The library lives in the app data, one per active profile (the profile folder is
//!   content gmclip never writes, SPEC §10.5), like the layout presets.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_model::entities::{CaptionPreset, CaptionPresets, Cut, Font, Framing, Styles};
use gmclip_model::{Doc, Id, Ulid};
use gmclip_plan::types::TextRole;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The app-shell commands this module serves.
pub const COMMANDS: &[&str] = &[
    "caption_gallery",
    "caption_style_apply",
    "caption_style_save_mine",
    "caption_style_forget_mine",
    "caption_style_match",
    "captions_file",
    "image_overlay_add",
    "image_thumb",
    "callout_add",
    "overlays_view",
    "brand_kit_view",
    "brand_kit_save",
    "brand_kit_use_mine",
    "brand_logo_set",
    "lower_third_add_at",
    "end_card_add_app",
    "overlay_template_save_app",
    "overlay_template_insert_at",
    "style_items_view",
    "style_item_save_mine",
    "style_item_forget_mine",
    "style_item_apply",
    "plate_save",
    "plates_view",
];

/// Thumbnails are the canvas divided by this (180 × 320 for a 9:16 1080p canvas).
pub const THUMB_DIV: u32 = 6;

fn s<'a>(a: &'a Value, k: &str) -> AppResult<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
}

/// Serves one of [`COMMANDS`].
pub fn dispatch(core: &AppCore, project: &str, name: &str, a: &Value) -> AppResult<Value> {
    match name {
        "caption_gallery" => gallery(
            core,
            project,
            s(a, "cut")?,
            a.get("frame").and_then(Value::as_u64).unwrap_or(0),
        ),
        "caption_style_apply" => apply(core, project, s(a, "cut")?, s(a, "name")?),
        "caption_style_save_mine" => save_mine(core, project, s(a, "cut")?, s(a, "name")?),
        "caption_style_forget_mine" => forget_mine(core, s(a, "name")?),
        "caption_style_match" => match_short(core, project, s(a, "path")?),
        "captions_file" => captions_file(core, project, s(a, "cut")?, s(a, "format")?),
        "image_overlay_add" => image_overlay_add(
            core,
            project,
            s(a, "cut")?,
            a.get("frame").and_then(Value::as_u64).unwrap_or(0),
            a.get("name").and_then(Value::as_str).unwrap_or("image"),
            a.get("data").and_then(Value::as_str),
            a.get("path").and_then(Value::as_str),
        ),
        "image_thumb" => image_thumb(core, project, s(a, "asset")?),
        "callout_add" => callout_add(
            core,
            project,
            s(a, "cut")?,
            a.get("frame").and_then(Value::as_u64).unwrap_or(0),
            s(a, "kind")?,
        ),
        "overlays_view" => overlays_view(core, project, s(a, "cut")?),
        "brand_kit_view" => brand_view(core, project),
        "brand_kit_save" => brand_save(core, project, a),
        "brand_kit_use_mine" => brand_use_mine(core, project),
        "brand_logo_set" => brand_logo_set(
            core,
            project,
            a.get("data").and_then(Value::as_str),
            a.get("path").and_then(Value::as_str),
            a.get("name").and_then(Value::as_str).unwrap_or("logo"),
        ),
        "lower_third_add_at" => {
            let cut = s(a, "cut")?;
            let start = anchor_at(core, project, cut, a.get("frame").and_then(Value::as_u64).unwrap_or(0))?;
            out(core.call(project, "lower_third_add", json!({"cut": cut, "start": start}))?)
        }
        "end_card_add_app" => out(core.call(
            project,
            "end_card_add",
            json!({"cut": s(a, "cut")?, "loop_layout": a.get("loop_layout").and_then(Value::as_bool).unwrap_or(false)}),
        )?),
        "overlay_template_save_app" => template_save(core, project, s(a, "overlay")?, s(a, "name")?),
        "style_items_view" => items_view(core, project),
        "plate_save" => plate_save(
            core,
            project,
            s(a, "name")?,
            a.get("stops")
                .cloned()
                .ok_or_else(|| AppError::invalid("`stops` is required"))?,
        ),
        "plates_view" => plates_view(core, project, s(a, "cut")?),
        "style_item_save_mine" => item_save_mine(
            core,
            project,
            s(a, "name")?,
            a.get("from")
                .cloned()
                .ok_or_else(|| AppError::invalid("`from` is required"))?,
        ),
        "style_item_forget_mine" => item_forget_mine(core, s(a, "kind")?, s(a, "name")?),
        "style_item_apply" => item_apply(
            core,
            project,
            s(a, "kind")?,
            s(a, "name")?,
            a.get("mine").and_then(Value::as_bool).unwrap_or(false),
            a.get("to")
                .cloned()
                .ok_or_else(|| AppError::invalid("`to` is required"))?,
        ),
        "overlay_template_insert_at" => {
            let cut = s(a, "cut")?;
            let start = anchor_at(core, project, cut, a.get("frame").and_then(Value::as_u64).unwrap_or(0))?;
            let mut args = json!({"cut": cut, "name": s(a, "name")?, "start": start});
            if let Some(m) = a.get("measurement").and_then(Value::as_str) {
                args["measurement"] = json!(m);
            }
            out(core.call(project, "overlay_template_insert", args)?)
        }
        _ => Err(AppError::invalid("unknown command")),
    }
}

// ---- the profile's style library ------------------------------------------------------

/// One profile's library.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub caption_presets: BTreeMap<String, CaptionPreset>,
    /// The brand kit (U.09 brand kit): profile content, never authority (R-PROF-4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand_kit: Option<Value>,
    /// The rest of the style library (A2.09, SPEC §8): text styles, pill presets, band
    /// sets, layouts and stream layouts, keyed `<kind>:<name>` like the project's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, gmclip_model::entities::LibraryItem>,
}

type Store = BTreeMap<String, Library>;

fn store_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("style-library.json")
}

fn profile_key(core: &AppCore) -> String {
    gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data)
        .profile
        .map_or_else(
            || "(no profile)".to_owned(),
            |p| p.to_string_lossy().into_owned(),
        )
}

fn load_store(core: &AppCore) -> Store {
    std::fs::read(store_path(core))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_store(core: &AppCore, s: &Store) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&store_path(core), &bytes)
        .map_err(|e| AppError::internal(format!("saving the style library: {}", e.kind())))
}

/// The active profile's library.
pub fn library(core: &AppCore) -> Library {
    load_store(core)
        .remove(&profile_key(core))
        .unwrap_or_default()
}

/// Changes the active profile's library.
pub fn update_library(core: &AppCore, f: impl FnOnce(&mut Library)) -> AppResult<()> {
    let mut st = load_store(core);
    f(st.entry(profile_key(core)).or_default());
    save_store(core, &st)
}

/// A core call's output.
fn out(v: Value) -> AppResult<Value> {
    Ok(v.get("output").cloned().unwrap_or(v))
}

// ---- fonts ------------------------------------------------------------------------------

fn doc_of(core: &AppCore, project: &str) -> AppResult<std::sync::Arc<gmclip_core::Snapshot>> {
    Ok(core.project(project)?.handle.snapshot())
}

/// The catalogue face named `fontname`, as a model font.
fn catalogue_font(fontname: &str) -> Option<Font> {
    let f = crate::fonts::by_name(fontname)?;
    Some(Font {
        id: Id::new(Ulid(0)),
        family: f.family.clone(),
        style: f.style.clone(),
        sha256: Some(f.sha256.clone()),
        em_per_size: f.em_per_size,
    })
}

/// Adds the face `fontname` to the project from the app's catalogue when it is missing.
pub fn ensure_font(core: &AppCore, project: &str, fontname: &str) -> AppResult<()> {
    let snap = doc_of(core, project)?;
    if gmclip_core::library::font_of(&snap.doc, fontname).is_some() {
        return Ok(());
    }
    let f = catalogue_font(fontname).ok_or_else(|| {
        AppError::new(
            "Unsupported",
            format!("{fontname} is not available (not in the app, not in your font folders)"),
        )
    })?;
    core.call(
        project,
        "font_add",
        json!({"family": f.family, "style": f.style, "sha256": f.sha256, "em_per_size": f.em_per_size}),
    )?;
    Ok(())
}

// ---- presets ----------------------------------------------------------------------------

/// A gallery row: where the preset comes from and the preset itself (when it resolves).
struct Row {
    name: String,
    label: String,
    origin: &'static str,
    font: String,
    preset: Option<CaptionPreset>,
    current: bool,
}

fn framing_of(core: &AppCore, project: &str, cut: &str) -> AppResult<Id<Framing>> {
    let f = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
    ulid_id::<Framing>(&f)
}

fn rows(core: &AppCore, doc: &Doc, framing: Id<Framing>) -> Vec<Row> {
    let is_current = |n: &str| gmclip_core::library::is_current(doc, framing, n);
    let mut out = Vec::new();
    for b in gmclip_core::library::builtins() {
        let em = gmclip_core::library::font_of(doc, b.font)
            .map(|f| f.em_per_size)
            .or_else(|| crate::fonts::by_name(b.font).map(|f| f.em_per_size));
        out.push(Row {
            name: b.name.into(),
            label: b.label.into(),
            origin: "builtin",
            font: b.font.into(),
            preset: em.map(|em| b.preset(em)),
            current: is_current(b.name),
        });
    }
    let project: BTreeMap<String, CaptionPreset> = doc
        .iter::<CaptionPresets>()
        .next()
        .map(|l| l.entries.clone())
        .unwrap_or_default();
    for (n, p) in &project {
        out.push(Row {
            name: n.clone(),
            label: n.clone(),
            origin: "project",
            font: p.style.font.clone(),
            preset: Some(p.clone()),
            current: is_current(n),
        });
    }
    for (n, p) in library(core).caption_presets {
        if project.contains_key(&n) {
            continue;
        }
        out.push(Row {
            label: n.clone(),
            origin: "mine",
            font: p.style.font.clone(),
            current: is_current(&n),
            name: n,
            preset: Some(p),
        });
    }
    out
}

/// Applies a preset (built-in, the project's, or one of mine) to the cut's framing.
pub fn apply(core: &AppCore, project: &str, cut: &str, name: &str) -> AppResult<Value> {
    let framing = framing_of(core, project, cut)?;
    let snap = doc_of(core, project)?;
    let row = rows(core, &snap.doc, framing)
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| AppError::invalid("no caption style of that name"))?;
    ensure_font(core, project, &row.font)?;
    let mut args = json!({"framing": framing.0.to_string(), "name": name});
    if row.origin == "mine" {
        args["preset"] = serde_json::to_value(&row.preset).unwrap_or(Value::Null);
    }
    core.call(project, "caption_preset_apply", args)
}

/// The framing's current caption look as a preset.
fn current_preset(doc: &Doc, framing: Id<Framing>) -> AppResult<CaptionPreset> {
    let f = doc
        .get(framing)
        .ok_or_else(|| AppError::internal("framing"))?;
    let sref = f
        .caption_style_ref
        .clone()
        .unwrap_or_else(|| "Speech".into());
    let style = doc
        .iter::<Styles>()
        .next()
        .and_then(|s| s.entries.get(&sref).cloned())
        .ok_or_else(|| {
            AppError::new(
                "Unsupported",
                "turn captions on (or pick a style) before saving it",
            )
        })?;
    Ok(CaptionPreset {
        style,
        grouping: f.caption_grouping,
        look: f.caption_look.clone().unwrap_or_default(),
    })
}

/// Saves the framing's caption look as `name` in the project and in my style library.
pub fn save_mine(core: &AppCore, project: &str, cut: &str, name: &str) -> AppResult<Value> {
    let name = name.trim();
    let framing = framing_of(core, project, cut)?;
    let snap = doc_of(core, project)?;
    let preset = current_preset(&snap.doc, framing)?;
    core.call(
        project,
        "caption_preset_save",
        json!({"name": name, "preset": preset}),
    )?;
    update_library(core, |l| {
        l.caption_presets.insert(name.to_owned(), preset);
    })?;
    Ok(json!({"saved": name}))
}

/// Removes `name` from my style library (the project keeps its copy).
pub fn forget_mine(core: &AppCore, name: &str) -> AppResult<Value> {
    update_library(core, |l| {
        l.caption_presets.remove(name);
    })?;
    Ok(json!({"forgot": name}))
}

// ---- the rest of the style library (A2.09) -------------------------------------------------

/// The project's library items and mine (the active profile's, app-wide), each
/// `{kind, name, item}`.
pub fn items_view(core: &AppCore, project: &str) -> AppResult<Value> {
    let here = out(core.call(project, "style_library_list", json!({}))?)?;
    let mine: Vec<Value> = library(core)
        .items
        .into_iter()
        .filter_map(|(k, item)| {
            let (kind, name) = k.split_once(':')?;
            Some(json!({"kind": kind, "name": name, "item": item}))
        })
        .collect();
    Ok(json!({"project": here["items"], "mine": mine}))
}

/// Saves an item of the project (`from`: a text style, framing, layout or stream layout)
/// in the project's library and in mine.
pub fn item_save_mine(core: &AppCore, project: &str, name: &str, from: Value) -> AppResult<Value> {
    let name = name.trim();
    let saved = out(core.call(
        project,
        "style_library_save",
        json!({"name": name, "from": from}),
    )?)?;
    let key = saved["key"]
        .as_str()
        .ok_or_else(|| AppError::internal("no library key"))?
        .to_owned();
    let (kind, _) = key
        .split_once(':')
        .ok_or_else(|| AppError::internal("a library key without a kind"))?;
    let list = out(core.call(project, "style_library_list", json!({"kind": kind}))?)?;
    let item = list["items"]
        .as_array()
        .and_then(|v| v.iter().find(|r| r["name"] == name))
        .map(|r| r["item"].clone())
        .ok_or_else(|| AppError::internal("the saved item is not listed"))?;
    let item: gmclip_model::entities::LibraryItem =
        serde_json::from_value(item).map_err(|e| AppError::internal(e.to_string()))?;
    update_library(core, |l| {
        l.items.insert(key.clone(), item);
    })?;
    Ok(json!({"saved": key}))
}

/// Saves a background plate (decision 0029: `#RRGGBB` stops, top to bottom) as `name` in
/// the project's style library and in mine.
pub fn plate_save(core: &AppCore, project: &str, name: &str, stops: Value) -> AppResult<Value> {
    let name = name.trim();
    let item = json!({"plate": {"stops": stops}});
    let saved = out(core.call(
        project,
        "style_library_save",
        json!({"name": name, "item": item}),
    )?)?;
    let key = saved["key"]
        .as_str()
        .ok_or_else(|| AppError::internal("no library key"))?
        .to_owned();
    let item: gmclip_model::entities::LibraryItem =
        serde_json::from_value(item).map_err(|e| AppError::internal(e.to_string()))?;
    update_library(core, |l| {
        l.items.insert(key.clone(), item);
    })?;
    Ok(json!({"saved": key}))
}

/// The plates a cut can take (decision 0029): the built-in ones and the project's
/// (`project`), mine (`mine`, app-wide), and the one its framing shows (`current`).
pub fn plates_view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let here = out(core.call(project, "style_library_list", json!({"kind": "plate"}))?)?;
    let mine: Vec<Value> = library(core)
        .items
        .into_iter()
        .filter_map(|(k, item)| {
            let name = k.strip_prefix("plate:")?;
            Some(json!({"kind": "plate", "name": name, "item": item}))
        })
        .collect();
    let snap = doc_of(core, project)?;
    let current = framing_of(core, project, cut)
        .ok()
        .and_then(|f| snap.doc.get(f))
        .and_then(|f| match &f.background {
            gmclip_model::entities::Background::Plate { name } => Some(name.clone()),
            _ => None,
        });
    Ok(json!({"project": here["items"], "mine": mine, "current": current}))
}

/// Removes `kind:name` from mine (projects keep their copies).
pub fn item_forget_mine(core: &AppCore, kind: &str, name: &str) -> AppResult<Value> {
    let key = format!("{kind}:{name}");
    update_library(core, |l| {
        l.items.remove(&key);
    })?;
    Ok(json!({"forgot": key}))
}

/// Applies a library item — the project's, or with `mine` the app-wide one passed as
/// the command's `item` (nothing is stored in the project) — one undo step.
pub fn item_apply(
    core: &AppCore,
    project: &str,
    kind: &str,
    name: &str,
    mine: bool,
    to: Value,
) -> AppResult<Value> {
    let mut args = json!({"kind": kind, "name": name, "to": to});
    if mine {
        let item = library(core)
            .items
            .remove(&format!("{kind}:{name}"))
            .ok_or_else(|| AppError::invalid("not in my style library"))?;
        args["item"] = serde_json::to_value(item).map_err(|e| AppError::internal(e.to_string()))?;
    }
    out(core.call(project, "style_library_apply", args)?)
}

// ---- match a short's captions ------------------------------------------------------------

/// Frames read from a dropped short: one every quarter second over its first 30 s, at
/// most 540 px wide.
const MATCH_EVERY_S: f64 = 0.25;
const MATCH_MAX_S: f64 = 30.0;
const MATCH_W: u32 = 540;

fn hex6(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// `caption_style_match {path}`: measures the burned-in captions of a short (an
/// approximation: `gmclip_core::capmatch`) and keeps the closest preset as a project
/// style named "≈ <file>" — in the gallery for AJ to apply, tweak and save as his own.
/// Nothing on the cut changes.
pub fn match_short(core: &AppCore, project: &str, path: &str) -> AppResult<Value> {
    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !crate::video::VIDEO_EXTS.contains(&ext.as_str()) {
        return Err(AppError::invalid(
            "a short is an MP4, MOV, M4V, MKV or WebM video",
        ));
    }
    if !p.is_file() {
        return Err(AppError::not_found("that file is gone"));
    }
    let t0 = std::time::Instant::now();
    let frames = read_frames(p)?;
    let rgb: Vec<gmclip_core::capmatch::RgbFrame> = frames
        .into_iter()
        .map(|(width, height, rgb)| gmclip_core::capmatch::RgbFrame { width, height, rgb })
        .collect();
    let m = gmclip_core::capmatch::analyse(&rgb, 1.0 / MATCH_EVERY_S).ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "no burned-in captions found in its first 30 s (captions it shows as a separate track are not read)",
        )
    })?;
    // the heavy face when the strokes are heavy and it is installed, else the bundled one
    let font = if m.weight == "heavy"
        && crate::fonts::by_name(gmclip_core::library::STORY_FONT).is_some()
    {
        gmclip_core::library::STORY_FONT
    } else {
        gmclip_core::library::FREE_FONT
    };
    let em = crate::fonts::by_name(font).map_or(1.0, |f| f.em_per_size);
    ensure_font(core, project, font)?;
    let preset = m.preset(font, em);
    let stem: String = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "short".into())
        .chars()
        .filter(|c| !c.is_control())
        .take(54)
        .collect();
    let name = format!("≈ {stem}");
    core.call(
        project,
        "caption_preset_save",
        json!({"name": name, "preset": preset}),
    )?;
    Ok(json!({
        "name": name, "frames": m.frames, "with_text": m.with_text, "y": m.y,
        "size": m.size, "weight": m.weight, "fill": hex6(m.fill),
        "keyword": m.keyword.map(hex6), "outline": m.outline, "pill": m.pill.map(hex6),
        "words_per_cue": m.words_per_cue, "cue_s": m.cue_s,
        "grouping": serde_json::to_value(preset.grouping).unwrap_or(Value::Null),
        "font": font, "notes": m.notes, "ms": t0.elapsed().as_millis() as u64,
    }))
}

#[cfg(any(target_os = "macos", windows))]
fn read_frames(p: &std::path::Path) -> AppResult<Vec<(u32, u32, Vec<u8>)>> {
    gmclip_render::export::proxy::file_frames(p, MATCH_EVERY_S, MATCH_MAX_S, MATCH_W)
        .map(|(_, f)| f)
        .map_err(|e| AppError::new("Unsupported", format!("reading the video: {e}")))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn read_frames(_: &std::path::Path) -> AppResult<Vec<(u32, u32, Vec<u8>)>> {
    Err(AppError::new(
        "Unsupported",
        "reading a short is macOS-only for now",
    ))
}

// ---- images and callouts ------------------------------------------------------------------

/// RFC 4648 base64 (the UI sends pasted and dropped files this way), padding optional.
fn b64_decode(s: &str) -> AppResult<Vec<u8>> {
    let v = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let b: Vec<u8> = s
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(b.len() * 3 / 4);
    for ch in b.chunks(4) {
        let mut n = 0u32;
        for (i, c) in ch.iter().enumerate() {
            n |= v(*c).ok_or_else(|| AppError::invalid("the image data is not base64"))?
                << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if ch.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if ch.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// The cut-time anchor of output frame `frame`: the part showing it, with an offset.
pub fn anchor_at(core: &AppCore, project: &str, cut: &str, frame: u64) -> AppResult<Value> {
    let snap = doc_of(core, project)?;
    let cid = ulid_id::<Cut>(cut)?;
    let t = frame as i64 * crate::planner::fpf(&core.planned(project, cut)?.plan);
    let sp = gmclip_model::clock::cut_layout(&snap.doc, cid)
        .into_iter()
        .find(|s| t >= s.start.0 && t < s.start.0 + s.dur.0)
        .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
    let ms = (t - sp.start.0) * 1000 / crate::planner::FLICKS;
    Ok(json!({"part_start": {"part": sp.part.0.to_string(), "offset_ms": ms}}))
}

/// Decodes a pasted image (`data`, base64) or a file dropped on the window (`path`) once
/// (R-MEDIA-SEC-3), stores it as PNG in the app data by its hash, registers the asset and
/// puts it on the cut at `frame` for 4 s.
pub fn image_overlay_add(
    core: &AppCore,
    project: &str,
    cut: &str,
    frame: u64,
    name: &str,
    data: Option<&str>,
    path: Option<&str>,
) -> AppResult<Value> {
    let bytes = match (data, path) {
        (Some(d), _) => {
            if d.len() > (gmclip_media::image::MAX_BYTES / 3 + 1) * 4 {
                return Err(AppError::invalid("the image is over 64 MB"));
            }
            b64_decode(d)?
        }
        (None, Some(p)) => {
            let p = std::path::Path::new(p);
            let len = std::fs::metadata(p)
                .map_err(|_| AppError::not_found("that file is gone"))?
                .len();
            if len > gmclip_media::image::MAX_BYTES as u64 {
                return Err(AppError::invalid("the image is over 64 MB"));
            }
            std::fs::read(p).map_err(|_| AppError::not_found("that file is gone"))?
        }
        (None, None) => return Err(AppError::invalid("`data` or `path` is required")),
    };
    let name = path
        .and_then(|p| std::path::Path::new(p).file_name())
        .map_or_else(|| name.to_owned(), |n| n.to_string_lossy().into_owned());
    let (asset, w, h) = store_image(core, project, &name, &bytes)?;
    // 40 % of the canvas width, centred, near the top (clear of IG's top 220 px)
    let width = 0.4;
    let canvas = core.planned(project, cut)?.plan.output.canvas;
    let aspect = canvas.w.get() as f64 / canvas.h.get() as f64;
    let hfrac = width * aspect * f64::from(h) / f64::from(w);
    let start = anchor_at(core, project, cut, frame)?;
    let out = core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "image", "start": start, "end": {"dur_ms": 4000},
               "place": {"x": (1.0 - width) / 2.0, "y": (0.2f64).min(1.0 - hfrac), "w": width},
               "image": {"asset": asset, "w": w, "h": h}}),
    )?;
    Ok(json!({"overlay": out["output"]["id"], "asset": asset, "w": w, "h": h}))
}

/// Decodes, re-encodes and stores an image; registers it as an asset: `(asset, w, h)`.
pub fn store_image(
    core: &AppCore,
    project: &str,
    name: &str,
    bytes: &[u8],
) -> AppResult<(String, u32, u32)> {
    store_image_with(core, project, name, bytes, false)
}

/// [`store_image`] with the picture's size on the asset (a still backdrop needs it,
/// decision 0028).
pub fn store_image_probed(
    core: &AppCore,
    project: &str,
    name: &str,
    bytes: &[u8],
) -> AppResult<(String, u32, u32)> {
    store_image_with(core, project, name, bytes, true)
}

fn store_image_with(
    core: &AppCore,
    project: &str,
    name: &str,
    bytes: &[u8],
    probed: bool,
) -> AppResult<(String, u32, u32)> {
    let (sha, size, w, h) = store_image_file(core, bytes)?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    let mut a = json!({"name": if name.trim().is_empty() { "image".to_owned() } else { name }, "sha256": sha, "size": size, "media_type": "image/png"});
    if probed {
        a["probe"] = json!({"w": w, "h": h});
    }
    let out = core.call(project, "asset_add", a)?;
    let id = out["output"]["id"].as_str().unwrap_or_default().to_owned();
    Ok((id, w, h))
}

/// An image decoded (untrusted), re-encoded as PNG and stored in the assets folder by its
/// sha256, without registering it: `(sha256, size, w, h)` for a command that registers it
/// in its own step (the long-form end card).
pub fn store_image_file(core: &AppCore, bytes: &[u8]) -> AppResult<(String, u64, u32, u32)> {
    let img = gmclip_media::image::decode_untrusted(bytes)
        .map_err(|e| AppError::new("Unsupported", e.to_string()))?;
    let png =
        gmclip_media::image::encode_png(&img).map_err(|e| AppError::internal(e.to_string()))?;
    let sha = gmclip_core::profile::style::sha256_hex(&png);
    let path = core
        .fonts()
        .asset_file(&sha)
        .ok_or_else(|| AppError::internal("no assets folder"))?;
    if !path.is_file() {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| {
                AppError::internal(format!("making the assets folder: {}", e.kind()))
            })?;
        }
        crate::core::write_atomic(&path, &png)
            .map_err(|e| AppError::internal(format!("storing the image: {}", e.kind())))?;
    }
    Ok((sha, png.len() as u64, img.width, img.height))
}

/// A stored image as a small RGB thumbnail (the webview never gets the original bytes).
pub fn image_thumb(core: &AppCore, project: &str, asset: &str) -> AppResult<Value> {
    let snap = doc_of(core, project)?;
    let a = snap
        .doc
        .get(ulid_id::<gmclip_model::entities::Asset>(asset)?)
        .ok_or_else(|| AppError::not_found("no such asset"))?;
    let path = core
        .fonts()
        .asset_file(&a.sha256)
        .ok_or_else(|| AppError::not_found("no such asset"))?;
    let bytes =
        std::fs::read(&path).map_err(|_| AppError::not_found("the image file is missing"))?;
    let img = gmclip_media::image::decode_untrusted(&bytes)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let k = (f64::from(img.width.max(img.height)) / 96.0).max(1.0);
    let (w, h) = (
        ((f64::from(img.width) / k).round() as u32).max(1),
        ((f64::from(img.height) / k).round() as u32).max(1),
    );
    let pm = gmclip_media::image::fit(&img, w, h);
    // over the panel grey, as RGB
    let rgb: Vec<u8> = pm
        .chunks_exact(4)
        .flat_map(|p| {
            let a = 255 - u32::from(p[3]);
            [0, 1, 2].map(|k| (u32::from(p[k]) + 128 * a / 255) as u8)
        })
        .collect();
    Ok(json!({"w": w, "h": h, "rgb": crate::importsheet::b64(&rgb)}))
}

/// A callout at `frame` for 3 s: a red box on the middle of the screen panel, or a yellow
/// arrow pointing up-left into it. Move and resize it from the inspector (`overlay_set`).
pub fn callout_add(
    core: &AppCore,
    project: &str,
    cut: &str,
    frame: u64,
    kind: &str,
) -> AppResult<Value> {
    let start = anchor_at(core, project, cut, frame)?;
    let shape = match kind {
        "box" => json!({"kind": "box", "stroke": "#FF3B30FF", "width": 7.4, "radius": 12.0}),
        "arrow" => json!({"kind": "arrow", "stroke": "#FFD400FF", "width": 11.0, "radius": 0.0,
                          "from": [0.78, 0.5], "to": [0.58, 0.36]}),
        _ => return Err(AppError::invalid("a callout is a box or an arrow")),
    };
    let out = core.call(
        project,
        "overlay_add",
        json!({"cut": cut, "kind": "shape", "start": start, "end": {"dur_ms": 3000},
               "place": {"x": 0.2, "y": 0.3, "w": 0.6}, "rect": {"x": 0.2, "y": 0.3, "w": 0.6, "h": 0.15},
               "shape": shape, "z": 25}),
    )?;
    Ok(json!({"overlay": out["output"]["id"]}))
}

/// The cut's image and shape overlays (the inspector's list): id, kind, times, geometry.
pub fn overlays_view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    use gmclip_model::entities::{Overlay, OverlayGeom, OverlayKind};
    let snap = doc_of(core, project)?;
    let cid = ulid_id::<Cut>(cut)?;
    let planned = core.planned(project, cut)?;
    let plan = &planned.plan;
    let mut rows = Vec::new();
    let (mut ni, mut ns, mut nv) = (0usize, 0usize, 0usize);
    let mut ovs: Vec<&Overlay> = snap
        .doc
        .children::<Overlay>(cid.entity())
        .filter_map(|id| snap.doc.get(id))
        .collect();
    ovs.sort_by_key(|o| o.id);
    for o in ovs {
        // the planner numbers events in overlay order; find this one's frames
        let span = if o.kind == OverlayKind::Video {
            nv += 1;
            plan.clip_events
                .iter()
                .flatten()
                .find(|e| e.id.as_str() == format!("clip-{nv}"))
                .map(|e| e.out.0)
        } else if !matches!(o.kind, OverlayKind::Image | OverlayKind::Shape) {
            None
        } else if o.kind == OverlayKind::Image {
            ni += 1;
            plan.image_events
                .iter()
                .flatten()
                .find(|e| e.id.as_str() == format!("image-{ni}"))
                .map(|e| e.out.0)
        } else {
            ns += 1;
            plan.shape_events
                .iter()
                .flatten()
                .find(|e| e.id.as_str() == format!("shape-{ns}"))
                .map(|e| e.out.0)
        };
        let geom = match o.geom {
            OverlayGeom::Place(p) => json!({"x": p.x, "y": p.y, "w": p.w}),
            OverlayGeom::Box(b) => json!({"x": b.x, "y": b.y, "w": b.w, "h": b.h}),
        };
        rows.push(json!({
            "id": o.id.0.to_string(),
            "kind": match o.kind {
                OverlayKind::Image => "image",
                OverlayKind::Shape => "shape",
                OverlayKind::Video => "video",
                _ => "text",
            },
            "tracked": o.track.as_ref().map(|t| t.keys.len()),
            "video": o.video.as_ref().map(|v| json!({"loop": v.r#loop,
                "asset": match &v.media { gmclip_model::entities::AssetOrSpan::Asset(a) => Some(a.0.to_string()), _ => None }})),
            "role": o.role,
            "band": o.band,
            "label": o.text.as_ref().map(|t| t.runs.iter().map(|r| r.text.as_str()).collect::<String>().replace('\n', " ")),
            "shape": o.shape.as_ref().map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
            "asset": o.image.map(|i| i.asset.0.to_string()),
            "opacity": o.opacity,
            "geom": geom,
            "out": span,
            // decision 0042: where it sits among the picture's layers, and a cut-out
            "under": o.under,
            "matte": o.video.as_ref().is_some_and(|v| matches!(v.matte, Some(gmclip_model::entities::Matte::Roto { .. }))),
        }));
    }
    let fr = framing_of(core, project, cut)
        .ok()
        .and_then(|f| snap.doc.get(f));
    // a backdrop says what it shows (decision 0028)
    let backdrop = fr.and_then(|f| match &f.background {
        gmclip_model::entities::Background::Backdrop { asset, .. } => {
            snap.doc.get(*asset).map(|a| {
                json!({"name": a.name, "video": a.media_type.starts_with("video/"),
                   "w": a.probe.map(|p| p.w), "h": a.probe.map(|p| p.h)})
            })
        }
        _ => None,
    });
    // cover layers the cut shows (decision 0033: each can be pinned to motion), in the
    // order they first appear, with the frames they span
    let mut covers: Vec<Value> = Vec::new();
    for (k, ip) in planned.index.parts.iter().enumerate() {
        let Some(q) = plan.parts.get(k) else { continue };
        for (pl_id, model) in &ip.layers {
            if !q
                .layers
                .iter()
                .any(|l| l.id.as_str() == pl_id && l.cover.is_some())
            {
                continue;
            }
            if let Some(row) = covers.iter_mut().find(|c| c["id"] == model.as_str()) {
                row["out"][1] = json!(q.out.end());
                continue;
            }
            let Some(l) = ulid_id::<gmclip_model::entities::Layer>(model)
                .ok()
                .and_then(|id| snap.doc.get(id))
            else {
                continue;
            };
            covers.push(json!({
                "id": model,
                "role": serde_json::to_value(l.role).unwrap_or(Value::Null),
                "style": l.cover.map(|c| match c {
                    gmclip_model::entities::CoverStyle::Blur { .. } => "blur",
                    gmclip_model::entities::CoverStyle::Pixelate { .. } => "pixelate",
                    gmclip_model::entities::CoverStyle::Solid { .. } => "solid",
                }),
                "tracked": l.track.as_ref().map(|t| t.keys.len()),
                "out": [q.out.start(), q.out.end()],
            }));
        }
    }
    // the picture's layers by z (the first part's role for each): where an image or
    // video overlay can sit (decision 0042)
    let mut picture_layers: Vec<(i64, String)> = Vec::new();
    for l in plan.parts.iter().flat_map(|p| &p.layers) {
        if l.cover.is_none() && !picture_layers.iter().any(|(z, _)| *z == l.z) {
            picture_layers.push((l.z, l.role.as_str().to_owned()));
        }
    }
    picture_layers.sort();
    Ok(json!({
        "overlays": rows,
        "picture_layers": picture_layers.iter().map(|(z, role)| json!({"z": z, "role": role})).collect::<Vec<_>>(),
        "covers": covers,
        "backdrop": backdrop,
        "background": fr.map(|f| serde_json::to_value(&f.background).unwrap_or(Value::Null)),
        "caption_look": fr.map(|f| serde_json::to_value(f.caption_look.clone().unwrap_or_default()).unwrap_or(Value::Null)),
        "caption_grouping": fr.map(|f| serde_json::to_value(f.caption_grouping).unwrap_or(Value::Null)),
    }))
}

// ---- the brand kit ---------------------------------------------------------------------

/// The faces the app can draw (the brand kit's face picker).
fn faces() -> Vec<String> {
    crate::fonts::catalogue()
        .iter()
        .map(|f| f.fontname.clone())
        .collect()
}

/// The active profile's own `brand_kit` (profile.json content, read only), if any.
fn profile_kit(core: &AppCore) -> Option<Value> {
    let dir = gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data).profile?;
    gmclip_core::profile::load(&dir)
        .ok()?
        .content
        .get("brand_kit")
        .cloned()
}

/// The project's kit as JSON (asset ids as they are), or null.
fn doc_kit(doc: &Doc) -> Value {
    doc.iter::<gmclip_model::entities::BrandKit>()
        .next()
        .map(|k| serde_json::to_value(k).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

/// What the brand kit panel shows.
pub fn brand_view(core: &AppCore, project: &str) -> AppResult<Value> {
    let snap = doc_of(core, project)?;
    let presets: Vec<Value> = gmclip_core::library::builtins()
        .into_iter()
        .map(|b| json!({"name": b.name, "label": b.label}))
        .chain(
            snap.doc
                .iter::<CaptionPresets>()
                .next()
                .map(|l| {
                    l.entries
                        .keys()
                        .map(|n| json!({"name": n, "label": n}))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
        .collect();
    Ok(json!({
        "kit": doc_kit(&snap.doc),
        "mine": library(core).brand_kit,
        "profile": profile_kit(core),
        "faces": faces(),
        "presets": presets,
    }))
}

/// A kit as the profile's library keeps it: asset ids become the stored file's sha256
/// (portable across projects; the PNG is in the app data by that hash).
fn portable(doc: &Doc, kit: &Value) -> Value {
    let sha_of = |id: &Value| -> Value {
        id.as_str()
            .and_then(|i| ulid_id::<gmclip_model::entities::Asset>(i).ok())
            .and_then(|i| doc.get(i))
            .map_or(Value::Null, |a| json!(a.sha256))
    };
    let mut k = kit.clone();
    if let Some(o) = k.as_object_mut() {
        o.remove("id");
    }
    if let Some(l) = k.get_mut("logo").filter(|l| l.is_object()) {
        l["asset"] = sha_of(&l["asset"].clone());
    }
    if let Some(ts) = k.get_mut("templates").and_then(Value::as_object_mut) {
        for t in ts.values_mut() {
            if let Some(im) = t.get_mut("image").filter(|i| i.is_object()) {
                im["asset"] = sha_of(&im["asset"].clone());
            }
        }
    }
    k
}

/// Saves the kit's fields to the project (`brand_kit_set`, adding the face first) and
/// keeps the whole kit in my style library.
pub fn brand_save(core: &AppCore, project: &str, a: &Value) -> AppResult<Value> {
    let mut args = serde_json::Map::new();
    for k in [
        "name",
        "handle",
        "colours",
        "caption_preset",
        "clear_caption_preset",
    ] {
        if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
            args.insert(k.to_owned(), v.clone());
        }
    }
    for k in ["heading_font", "body_font"] {
        if let Some(f) = a.get(k).and_then(Value::as_str).filter(|f| !f.is_empty()) {
            ensure_font(core, project, f)?;
            args.insert(k.to_owned(), json!(f));
        }
    }
    if let Some(p) = a.get("caption_preset").and_then(Value::as_str) {
        // a built-in preset's face is added too, so new framings can use it
        if let Some(b) = gmclip_core::library::builtins()
            .into_iter()
            .find(|b| b.name == p)
        {
            ensure_font(core, project, b.font)?;
        }
    }
    core.call(project, "brand_kit_set", Value::Object(args))?;
    remember_kit(core, project)?;
    brand_view(core, project)
}

fn remember_kit(core: &AppCore, project: &str) -> AppResult<()> {
    let snap = doc_of(core, project)?;
    let kit = portable(&snap.doc, &doc_kit(&snap.doc));
    update_library(core, |l| l.brand_kit = Some(kit))
}

/// Registers a stored image by its sha256 in this project: the asset id, if the file is
/// in the app data.
fn asset_by_sha(core: &AppCore, project: &str, sha: &str) -> Option<String> {
    let path = core.fonts().asset_file(sha)?;
    let size = std::fs::metadata(&path).ok()?.len();
    let out = core
        .call(
            project,
            "asset_add",
            json!({"name": "logo.png", "sha256": sha, "size": size, "media_type": "image/png"}),
        )
        .ok()?;
    out["output"]["id"].as_str().map(str::to_owned)
}

/// Puts my brand kit (the profile's library) into this project: fields, faces, logo and
/// templates (a template whose image is not on this Mac is skipped).
pub fn brand_use_mine(core: &AppCore, project: &str) -> AppResult<Value> {
    let k = library(core)
        .brand_kit
        .or_else(|| profile_kit(core))
        .ok_or_else(|| AppError::new("Unsupported", "no brand kit saved in your profile yet"))?;
    let mut args = json!({});
    for f in ["name", "handle", "colours", "caption_preset"] {
        if let Some(v) = k.get(f).filter(|v| !v.is_null()) {
            args[f] = v.clone();
        }
    }
    for f in ["heading_font", "body_font"] {
        if let Some(v) = k.get(f).and_then(Value::as_str) {
            ensure_font(core, project, v)?;
            args[f] = json!(v);
        }
    }
    if let Some(p) = k.get("caption_preset").and_then(Value::as_str)
        && let Some(b) = gmclip_core::library::builtins()
            .into_iter()
            .find(|b| b.name == p)
    {
        ensure_font(core, project, b.font)?;
    }
    if let Some(l) = k.get("logo").filter(|l| l.is_object())
        && let Some(id) = l["asset"]
            .as_str()
            .and_then(|sha| asset_by_sha(core, project, sha))
    {
        args["logo"] = json!({"asset": id, "w": l["w"], "h": l["h"]});
    }
    core.call(project, "brand_kit_set", args)?;
    let mut skipped = Vec::new();
    if let Some(ts) = k.get("templates").and_then(Value::as_object) {
        for (name, t) in ts {
            let mut t = t.clone();
            if let Some(im) = t.get_mut("image").filter(|i| i.is_object()) {
                match im["asset"]
                    .as_str()
                    .and_then(|sha| asset_by_sha(core, project, sha))
                {
                    Some(id) => im["asset"] = json!(id),
                    None => {
                        skipped.push(name.clone());
                        continue;
                    }
                }
            }
            if let Some(f) = t
                .pointer("/text/style/font")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                ensure_font(core, project, &f)?;
            }
            core.call(
                project,
                "overlay_template_save",
                json!({"name": name, "template": t}),
            )?;
        }
    }
    let mut v = brand_view(core, project)?;
    v["skipped"] = json!(skipped);
    Ok(v)
}

/// A project with no brand kit takes mine (the profile's library) before its first
/// framing, so new cuts start in the brand (SPEC §8 "brand kit as defaults"). Silent:
/// a project keeps working when the kit cannot be applied.
pub fn seed_kit(core: &AppCore, project: &str) {
    let has = doc_of(core, project).is_ok_and(|s| {
        s.doc
            .iter::<gmclip_model::entities::BrandKit>()
            .next()
            .is_some()
    });
    if !has && library(core).brand_kit.is_some() {
        let _ = brand_use_mine(core, project);
    }
}

/// A pasted or dropped logo becomes the kit's logo.
pub fn brand_logo_set(
    core: &AppCore,
    project: &str,
    data: Option<&str>,
    path: Option<&str>,
    name: &str,
) -> AppResult<Value> {
    let bytes = match (data, path) {
        (Some(d), _) => b64_decode(d)?,
        (None, Some(p)) => {
            std::fs::read(p).map_err(|_| AppError::not_found("that file is gone"))?
        }
        _ => return Err(AppError::invalid("`data` or `path` is required")),
    };
    let (asset, w, h) = store_image(core, project, name, &bytes)?;
    core.call(
        project,
        "brand_kit_set",
        json!({"logo": {"asset": asset, "w": w, "h": h}}),
    )?;
    remember_kit(core, project)?;
    brand_view(core, project)
}

/// Saves an overlay of the cut as a named template, in the project and in my kit.
pub fn template_save(core: &AppCore, project: &str, overlay: &str, name: &str) -> AppResult<Value> {
    core.call(
        project,
        "overlay_template_save",
        json!({"name": name.trim(), "overlay": overlay}),
    )?;
    remember_kit(core, project)?;
    brand_view(core, project)
}

// ---- subtitle files ---------------------------------------------------------------------

/// Writes the cut's captions as `<exports>/<project>/<cut title>.<srt|vtt|ass>` (the core's
/// `captions_export`, for the framing the cut shows).
pub fn captions_file(core: &AppCore, project: &str, cut: &str, format: &str) -> AppResult<Value> {
    let framing = framing_of(core, project, cut)?;
    let out = core.call(
        project,
        "captions_export",
        json!({"cut": cut, "framing": framing.0.to_string(), "format": format}),
    )?["output"]
        .take();
    let text = out["text"].as_str().unwrap_or_default();
    let snap = doc_of(core, project)?;
    let title = snap
        .doc
        .get(ulid_id::<Cut>(cut)?)
        .map(|c| c.title.clone())
        .unwrap_or_default();
    let dir = core.export_dir(project)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::internal(format!("making the export folder: {}", e.kind())))?;
    let ext = out["format"].as_str().unwrap_or("srt");
    let path = dir.join(format!("{}.{ext}", crate::core::safe_name(&title)));
    crate::core::write_atomic(&path, text.as_bytes())
        .map_err(|e| AppError::internal(format!("writing the captions file: {}", e.kind())))?;
    Ok(json!({"path": path, "cues": out["cues"], "format": ext}))
}

// ---- the gallery ----------------------------------------------------------------------

/// 8-bit RGB `w × h`, its bytes base64 (the UI paints them into a canvas).
fn thumb_json(img: &gmclip_render::export::sheet::RgbImage) -> Value {
    json!({"w": img.width, "h": img.height, "rgb": crate::importsheet::b64(&img.px)})
}

/// The cut's frame `f` without captions, at canvas size.
fn base_frame(plan: &gmclip_plan::Plan, f: u64) -> gmclip_render::export::sheet::RgbImage {
    let mut bare = plan.clone();
    if let Some(evs) = bare.text_events.as_mut() {
        evs.retain(|e| e.role != TextRole::Caption);
    }
    let (w, h) = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    #[cfg(any(target_os = "macos", windows))]
    {
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        if let Ok(mut v) = gmclip_render::export::render_frames(&bare, &mut inputs, &[f])
            && let Some((_, img)) = v.pop()
        {
            return img;
        }
    }
    let bg = gmclip_render::compositor::draw::background(plan)
        .map(|c| c.map(|v| (v * 255.0).round() as u8))
        .unwrap_or([0, 0, 0]);
    let _ = f;
    gmclip_render::export::sheet::RgbImage::new(w, h, bg)
}

/// `base` with `plan`'s caption at (or nearest after) frame `f`, in its settled look.
fn with_caption(
    base: &gmclip_render::export::sheet::RgbImage,
    plan: &gmclip_plan::Plan,
    f: u64,
) -> Result<gmclip_render::export::sheet::RgbImage, String> {
    let caps: Vec<&gmclip_plan::types::TextEvent> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .collect();
    let pick = caps
        .iter()
        .find(|e| e.out.start() <= f && f < e.out.end())
        .or_else(|| caps.iter().find(|e| e.out.start() > f))
        .or_else(|| caps.last())
        .ok_or("no captions on this cut yet")?;
    let mut mini = plan.clone();
    let mut ev = (*pick).clone();
    ev.anim = None;
    ev.out = gmclip_plan::scalars::Range([f, f + 1]);
    mini.text_events = Some(vec![ev]);
    let mut tl =
        gmclip_render::compositor::text_layer::TextLayer::new(&mini).map_err(|e| e.to_string())?;
    let tiles = tl.active(&mini, f).map_err(|e| e.to_string())?;
    let (w, h) = (base.width, base.height);
    let mut rgba: Vec<u8> = base
        .px
        .chunks_exact(3)
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    for t in &tiles {
        t.composite_over(&mut rgba, w, h);
    }
    let mut out = gmclip_render::export::sheet::RgbImage::new(w, h, [0, 0, 0]);
    for (d, s) in out.px.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
        d.copy_from_slice(&s[..3]);
    }
    Ok(out)
}

/// The gallery: every preset drawn on the cut's frame `frame`.
pub fn gallery(core: &AppCore, project: &str, cut: &str, frame: u64) -> AppResult<Value> {
    let framing = framing_of(core, project, cut)?;
    let cid = ulid_id::<Cut>(cut)?;
    let snap = doc_of(core, project)?;
    let doc = &snap.doc;
    let planned = core.planned(project, cut)?;
    let plan = &planned.plan;
    let f = frame.min(plan.output.frames.saturating_sub(1));
    let base = base_frame(plan, f);
    let mut out = Vec::new();
    for r in rows(core, doc, framing) {
        let mut row = json!({
            "name": r.name, "label": r.label, "origin": r.origin, "font": r.font,
            "current": r.current, "thumb": Value::Null,
        });
        if let Some(p) = &r.preset {
            row["grouping"] = serde_json::to_value(p.grouping).unwrap_or(Value::Null);
            row["look"] = serde_json::to_value(&p.look).unwrap_or(Value::Null);
            let fonts: Vec<Font> = catalogue_font(&p.style.font).into_iter().collect();
            let drawn = gmclip_core::library::preview_doc(doc, framing, &r.name, p, &fonts)
                .map_err(|e| e.to_string())
                .and_then(|scratch| {
                    gmclip_core::planner::plan_cut_traced(
                        &scratch,
                        cid,
                        framing,
                        core.fonts(),
                        Some(&snap.analysis),
                    )
                    .map(|(pl, _)| pl)
                    .map_err(|e| e.0)
                })
                .and_then(|pl| with_caption(&base, &pl, f));
            match drawn {
                Ok(img) => row["thumb"] = thumb_json(&img.downscale(THUMB_DIV)),
                Err(e) => row["why"] = json!(e.chars().take(160).collect::<String>()),
            }
        } else {
            row["why"] = json!(format!("{} is not available on this Mac", r.font));
        }
        out.push(row);
    }
    Ok(json!({"frame": f, "styles": out}))
}
