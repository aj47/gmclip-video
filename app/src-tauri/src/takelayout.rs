//! Per-take layout by numbers (M2 walkthrough gap 3; AGENTS.md "one steady layout": per
//! section change only the panel's crop or inset).
//!
//! - **Detail inset**: a screen sub-crop as its own layer on one take (the Run button, the
//!   progress bar, the taskbar clock), added as that part's addition in the framing shown
//!   and removed with `layer_delete`.
//! - **Copy to the next N takes**: the take's layout link, every layer's crop and place,
//!   and its insets onto the next N parts in output order (or all of them), as one undo
//!   step while it fits one `layout_edit` (200 ops).
//! - **Presets**: a take's whole stack saved under a name and applied to any take. They
//!   live in the app data, one list per active profile (the profile folder is content gmclip
//!   never writes, SPEC §10.5), and the profile's own stream layouts (`stream-layouts.json`,
//!   read only) are offered as "screen + facecam" presets with their screen and cam boxes.
//!
//! Everything goes through `layout_edit` ops agents already have (`layer_crop`,
//! `layer_place`, `layer_add`, `layer_delete`, `layout_link`), so an agent can do the same
//! by numbers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_model::Id;
use gmclip_model::entities::{Cut, Framing, Layer, LayerOwner, LayerRole, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// One layer of a saved stack: crop normalised to the source, place to the canvas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetLayer {
    pub role: LayerRole,
    pub crop: [f64; 4],
    pub place: [f64; 3],
    pub z: i32,
    /// A part's own addition (an inset) rather than one of the base stack's layers.
    pub addition: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    /// The canvas it was saved on (`[w, h]`); applied only to framings of that shape.
    pub canvas: [u32; 2],
    pub layers: Vec<PresetLayer>,
}

fn framing_of(core: &AppCore, project: &str, cut: &str) -> AppResult<(String, Id<Framing>)> {
    let f = core
        .active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
    let id = ulid_id::<Framing>(&f)?;
    Ok((f, id))
}

/// A part's effective stack in a framing, as preset layers (with the model layer ids).
pub(crate) fn stack_of(
    doc: &gmclip_model::Doc,
    part: Id<Part>,
    framing: Id<Framing>,
) -> AppResult<Vec<(String, PresetLayer)>> {
    let p = doc
        .get(part)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let st = gmclip_model::effective::effective(doc, part, framing, p.src_in)
        .ok_or_else(|| AppError::internal("no stack for that part"))?;
    Ok(st
        .layers
        .iter()
        .map(|l| {
            let (c, pl) = (l.values.crop, l.values.place);
            (
                l.id.0.to_string(),
                PresetLayer {
                    role: l.values.role,
                    crop: [c.x, c.y, c.w, c.h],
                    place: [pl.x, pl.y, pl.w],
                    z: l.values.z,
                    addition: l.addition,
                },
            )
        })
        .collect())
}

fn crop_json(c: &[f64; 4]) -> Value {
    json!({"x": c[0], "y": c[1], "w": c[2], "h": c[3]})
}

fn place_json(p: &[f64; 3]) -> Value {
    json!({"x": p[0], "y": p[1], "w": p[2]})
}

/// The ops that give `target` the stack `want`: base layers matched by role take its crop
/// and place (this part's override), the target's own insets go, and every preset layer
/// left over is added as this part's addition.
pub(crate) fn stack_ops(
    doc: &gmclip_model::Doc,
    target: Id<Part>,
    framing: Id<Framing>,
    want: &[PresetLayer],
) -> AppResult<Vec<Value>> {
    let (q, f) = (target.0.to_string(), framing.0.to_string());
    let have = stack_of(doc, target, framing)?;
    let mut used = vec![false; want.len()];
    let mut ops = Vec::new();
    for (id, l) in &have {
        if l.addition {
            ops.push(json!({"op": "layer_delete", "layer": id}));
            continue;
        }
        if let Some(k) =
            (0..want.len()).find(|&k| !used[k] && !want[k].addition && want[k].role == l.role)
        {
            used[k] = true;
            let w = &want[k];
            ops.push(json!({"op": "layer_crop", "layer": id, "part": q, "framing": f, "crop": crop_json(&w.crop)}));
            ops.push(json!({"op": "layer_place", "layer": id, "part": q, "framing": f, "place": place_json(&w.place)}));
        }
    }
    for (k, w) in want.iter().enumerate() {
        if !used[k] {
            ops.push(json!({"op": "layer_add", "owner": {"part_override": {"part": q, "framing": f}},
                            "role": w.role, "crop": crop_json(&w.crop), "place": place_json(&w.place), "z": w.z}));
        }
    }
    Ok(ops)
}

pub(crate) fn run_ops(core: &AppCore, project: &str, cut: &str, ops: &[Value]) -> AppResult<()> {
    for chunk in ops.chunks(200) {
        core.call(project, "layout_edit", json!({"cut": cut, "ops": chunk}))?;
    }
    Ok(())
}

/// Parts in output order.
fn order(doc: &gmclip_model::Doc, cut: Id<Cut>) -> Vec<Id<Part>> {
    gmclip_model::clock::cut_layout(doc, cut)
        .iter()
        .map(|s| s.part)
        .collect()
}

/// Adds a detail inset to one take: `crop` in source pixels (default: a strip across the
/// middle of the screen layer's crop), `place` `[x, y, w]` in canvas pixels (default: the
/// full-width band under AJ's panel, y 812).
pub fn detail_add(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    crop_px: Option<[f64; 4]>,
    place_px: Option<[f64; 3]>,
) -> AppResult<Value> {
    let (f, fid) = framing_of(core, project, cut)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let pid = ulid_id::<Part>(part)?;
    let pt = doc
        .get(pid)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let fr = doc
        .get(fid)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let (cw, ch) = (f64::from(fr.canvas.w), f64::from(fr.canvas.h));
    let (sw, sh) = crate::framing::picture_dims(&doc, &pt.media);
    let stack = stack_of(&doc, pid, fid)?;
    let crop = match crop_px {
        Some(r) => [r[0] / sw, r[1] / sh, r[2] / sw, r[3] / sh],
        None => {
            let s = stack
                .iter()
                .find(|(_, l)| {
                    l.role == LayerRole::Region(gmclip_model::entities::RegionRole::Screen)
                })
                .map_or([0.0, 0.0, 1.0, 1.0], |(_, l)| l.crop);
            [
                s[0] + s[2] / 4.0,
                s[1] + s[3] * 7.0 / 16.0,
                s[2] / 2.0,
                s[3] / 8.0,
            ]
        }
    };
    if crop.iter().any(|v| !v.is_finite())
        || crop[2] <= 0.0
        || crop[3] <= 0.0
        || crop[0] < 0.0
        || crop[1] < 0.0
        || crop[0] + crop[2] > 1.0001
        || crop[1] + crop[3] > 1.0001
    {
        return Err(AppError::invalid(
            "the inset's crop is a rectangle inside the picture",
        ));
    }
    let place = match place_px {
        Some(p) => [p[0] / cw, p[1] / ch, p[2] / cw],
        None => [30.0 / 1080.0, 812.0 / 1920.0, 1020.0 / 1080.0],
    };
    let z = stack.iter().map(|(_, l)| l.z).max().unwrap_or(0) + 1;
    let r = core.call(
        project,
        "layout_edit",
        json!({"cut": cut, "ops": [{"op": "layer_add", "owner": {"part_override": {"part": part, "framing": f}},
               "role": "detail", "crop": crop_json(&crop), "place": place_json(&place), "z": z}]}),
    )?;
    Ok(json!({"layer": r["results"][0]["id"]}))
}

/// AGENTS.md (h53) as a check: the frame where the picture's layout changes while the
/// text hook is still up (a take after the opening one starting before the hook ends with
/// other layers, crops or places), and that take's number; `None` when the rule holds.
pub fn hook_layout_breaks(cut_view: &Value, hook_end: i64) -> Option<(i64, usize)> {
    let parts = cut_view["parts"].as_array()?;
    let sig = |p: &Value| -> Vec<(String, Vec<i64>, Vec<i64>, bool)> {
        p["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let px = |k: &str| -> Vec<i64> {
                    l[k].as_array()
                        .into_iter()
                        .flatten()
                        .map(|v| v.as_f64().unwrap_or(0.0).round() as i64)
                        .collect()
                };
                (
                    l["role"].as_str().unwrap_or_default().to_owned(),
                    px("crop"),
                    px("place"),
                    l["visible"].as_bool().unwrap_or(true),
                )
            })
            .collect()
    };
    let first = parts.first()?;
    let open = sig(first);
    parts.iter().enumerate().skip(1).find_map(|(i, p)| {
        let at = p["out"][0].as_i64()?;
        (at < hook_end && sig(p) != open).then_some((at, i + 1))
    })
}

/// AGENTS.md (h53): when the text hook outlasts the opening take, the next take keeps the
/// opening's layout until the hook ends. The take the hook ends in is split at the word
/// boundary nearest the hook's end (or at the end itself when no word starts there),
/// and every take before that point gets the opening take's layout.
pub fn keep_opening_until_hook(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let sv = crate::short::short_view(core, project, cut)?;
    let end = sv["overlays"]
        .as_array()
        .and_then(|o| o.iter().find(|x| x["role"] == "text_hook"))
        .and_then(|x| x["end"].as_i64())
        .ok_or_else(|| AppError::invalid("this cut has no text hook yet"))?;
    let parts = |c: &AppCore| -> AppResult<Vec<(String, i64, i64)>> {
        let v = c.cut_view(project, cut)?;
        Ok(v["parts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        (
                            p["id"].as_str().unwrap_or_default().to_owned(),
                            p["out"][0].as_i64().unwrap_or(0),
                            p["out"][1].as_i64().unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    };
    let ps = parts(core)?;
    let Some(first) = ps.first().cloned() else {
        return Err(AppError::invalid("the cut has no parts"));
    };
    if end <= first.2 {
        return Ok(
            json!({"split_at": null, "parts": 0, "note": "the hook clears within the opening take"}),
        );
    }
    // split the take the hook ends in, at the word start nearest the hook's end
    let mut split_at = None;
    if let Some((_, a, b)) = ps.iter().find(|(_, a, b)| *a < end && end < *b) {
        let words: Vec<i64> = crate::transcript::transcript_view(core, project, cut)
            .ok()
            .and_then(|t| t["frame"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_i64)
            .filter(|f| f > a && f < b)
            .collect();
        let at = words
            .iter()
            .copied()
            .min_by_key(|f| (f - end).abs())
            .filter(|f| (f - end).abs() <= 45)
            .unwrap_or(end);
        core.split_at_frame(project, cut, at as u64)?;
        split_at = Some(at);
    }
    let limit = split_at.unwrap_or(end);
    let targets: Vec<Id<Part>> = parts(core)?
        .iter()
        .skip(1)
        .filter(|(_, a, _)| *a < limit)
        .filter_map(|(id, _, _)| ulid_id::<Part>(id).ok())
        .collect();
    let r = copy_to(core, project, cut, &first.0, &targets)?;
    Ok(json!({"split_at": split_at, "parts": r["parts"]}))
}

/// Removes a take's inset (a part addition).
pub fn layer_delete(core: &AppCore, project: &str, cut: &str, layer: &str) -> AppResult<Value> {
    core.call(
        project,
        "layout_edit",
        json!({"cut": cut, "ops": [{"op": "layer_delete", "layer": layer}]}),
    )
}

/// Gives the next `count` takes after `from` in output order (all other takes when
/// `None`) its layout link, crops, places and insets.
pub fn copy_layout(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: &str,
    count: Option<usize>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let pid = ulid_id::<Part>(from)?;
    doc.get(pid)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let all = order(&doc, cid);
    let targets: Vec<Id<Part>> = match count {
        None => all.iter().copied().filter(|q| *q != pid).collect(),
        Some(n) => {
            let at = all.iter().position(|q| *q == pid).unwrap_or(all.len());
            all.iter().skip(at + 1).take(n).copied().collect()
        }
    };
    copy_to(core, project, cut, from, &targets)
}

/// Gives every other part cut from the same source video as `from` (its picture source)
/// its layout link, crops, places and insets: one crop setup for all the takes of one
/// recording (AJ 2026-10-19), one undo step.
pub fn copy_same_source(core: &AppCore, project: &str, cut: &str, from: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let pid = ulid_id::<Part>(from)?;
    let src = doc
        .get(pid)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let video = crate::framing::picture_source(&doc, &src.media);
    let targets: Vec<Id<Part>> = order(&doc, cid)
        .into_iter()
        .filter(|q| *q != pid)
        .filter(|q| {
            doc.get(*q)
                .is_some_and(|x| crate::framing::picture_source(&doc, &x.media) == video)
        })
        .collect();
    copy_to(core, project, cut, from, &targets)
}

/// Gives `targets` the layout link, crops, places and insets of part `from`.
pub fn copy_to(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: &str,
    targets: &[Id<Part>],
) -> AppResult<Value> {
    if targets.is_empty() {
        return Ok(json!({"parts": 0, "ops": 0}));
    }
    let (f, fid) = framing_of(core, project, cut)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let pid = ulid_id::<Part>(from)?;
    let src = doc
        .get(pid)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let src_stack = stack_of(&doc, pid, fid)?;
    // Linked to the source's layout (or both on the framing's defaults), a target's base
    // layers are the source's own: crop and place them for the target, swap its insets
    // for the source's, all in one transaction (one undo) with the links.
    let layout = src.look.get(&fid).and_then(|l| l.layout_ref);
    let mut ops: Vec<Value> = Vec::new();
    for q in targets {
        let qs = q.0.to_string();
        ops.push(match layout {
            Some(l) => {
                json!({"op": "layout_link", "part": qs, "framing": f, "layout": l.0.to_string()})
            }
            None => json!({"op": "layout_unlink", "part": qs, "framing": f}),
        });
        for (id, l) in stack_of(&doc, *q, fid)? {
            if l.addition {
                ops.push(json!({"op": "layer_delete", "layer": id}));
            }
        }
        for (id, l) in &src_stack {
            if l.addition {
                ops.push(json!({"op": "layer_add", "owner": {"part_override": {"part": qs, "framing": f}},
                                "role": l.role, "crop": crop_json(&l.crop), "place": place_json(&l.place), "z": l.z}));
            } else {
                ops.push(json!({"op": "layer_crop", "layer": id, "part": qs, "framing": f, "crop": crop_json(&l.crop)}));
                ops.push(json!({"op": "layer_place", "layer": id, "part": qs, "framing": f, "place": place_json(&l.place)}));
            }
        }
    }
    run_ops(core, project, cut, &ops)?;
    Ok(json!({"parts": targets.len(), "ops": ops.len()}))
}

// ---- presets -------------------------------------------------------------------------

fn store_path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("layout-presets.json")
}

/// The active profile (Owner settings), or `None`: presets are kept per profile.
fn profile(core: &AppCore) -> Option<PathBuf> {
    gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data).profile
}

pub(crate) fn profile_key(core: &AppCore) -> String {
    profile(core).map_or_else(
        || "(no profile)".to_owned(),
        |p| p.to_string_lossy().into_owned(),
    )
}

type Store = BTreeMap<String, Vec<LayoutPreset>>;

fn load_store(core: &AppCore) -> Store {
    std::fs::read(store_path(core))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_store(core: &AppCore, s: &Store) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&store_path(core), &bytes)
        .map_err(|e| AppError::internal(format!("saving the layout presets: {}", e.kind())))
}

/// The active profile's stream layout presets: its `stream-layouts.json` (read only),
/// then the ones drawn and saved as presets in the app (`crate::drawlayout`).
pub(crate) fn profile_layouts(core: &AppCore) -> Vec<gmclip_core::profile::layouts::LayoutPreset> {
    let mut out = profile(core)
        .and_then(|dir| {
            gmclip_core::profile::style::read_confined(&dir.join("stream-layouts.json"), 1 << 20)
                .ok()
        })
        .and_then(|t| gmclip_core::profile::layouts::parse(&t))
        .unwrap_or_default();
    out.extend(crate::drawlayout::saved_presets(core));
    out
}

/// A2.20 (SPEC §5.7.1 step 3): when the picture of `media` has no camera box yet, the
/// active profile's stream layout preset that names it (`profile::layouts::match_source`)
/// becomes its stream layout, so seeding takes the face from it. Says what matched, or
/// which presets named it equally (none applied then).
pub fn match_stream_layout(
    core: &AppCore,
    project: &str,
    media: &gmclip_model::entities::MediaRef,
) -> AppResult<Value> {
    use gmclip_core::profile::layouts::{Match, match_source};
    use gmclip_model::entities::TrackKind;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    if crate::framing::source_boxes(&doc, media).1.is_some() {
        return Ok(json!({"matched": null, "why": "it has its camera box"}));
    }
    let Some(src) = crate::framing::picture_source(&doc, media).and_then(|s| doc.get(s)) else {
        return Ok(json!({"matched": null, "why": "no picture"}));
    };
    let Some(dims) = src
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .and_then(|t| Some([t.width?, t.height?]))
    else {
        return Ok(json!({"matched": null, "why": "the picture's size is not known"}));
    };
    let presets = profile_layouts(core);
    match match_source(&presets, &src.path, dims) {
        Match::One { index, by } => {
            let l = &presets[index].layout;
            let name: String = l.name.chars().take(200).collect();
            core.call(
                project,
                "source_stream_layout_set",
                json!({"source": src.id, "name": name, "canvas": l.canvas_size, "regions": l.regions}),
            )?;
            Ok(json!({"matched": name, "by": by}))
        }
        Match::Ambiguous(ix) => Ok(json!({
            "matched": null,
            "ambiguous": ix.iter().map(|i| presets[*i].layout.name.clone()).collect::<Vec<_>>(),
        })),
        Match::None => Ok(json!({"matched": null})),
    }
}

/// The profile's stream layouts as "screen + facecam" presets on a 1080×1920 canvas: the
/// screen box as the panel (y 250, 1020 wide) and the cam box at AJ's facecam seat.
fn stream_presets(core: &AppCore) -> Vec<LayoutPreset> {
    use gmclip_model::entities::RegionRole;
    profile_layouts(core)
        .into_iter()
        .filter_map(|p| {
            let l = p.layout;
            let rect = |r: RegionRole| {
                l.regions
                    .iter()
                    .find(|x| x.role == r)
                    .map(|x| [x.rect.x, x.rect.y, x.rect.w, x.rect.h])
            };
            let cam = rect(RegionRole::Cam)?;
            let screen = rect(RegionRole::Screen).unwrap_or([0.0, 0.0, 1.0, 1.0]);
            Some(LayoutPreset {
                name: format!("{} (stream layout)", l.name),
                canvas: [1080, 1920],
                layers: vec![
                    PresetLayer {
                        role: LayerRole::Region(RegionRole::Screen),
                        crop: screen,
                        place: [30.0 / 1080.0, 250.0 / 1920.0, 1020.0 / 1080.0],
                        z: 0,
                        addition: false,
                    },
                    PresetLayer {
                        role: LayerRole::Region(RegionRole::Cam),
                        crop: cam,
                        place: [160.0 / 1080.0, 1120.0 / 1920.0, 760.0 / 1080.0],
                        z: 1,
                        addition: false,
                    },
                ],
            })
        })
        .collect()
}

fn all_presets(core: &AppCore) -> (Vec<LayoutPreset>, usize) {
    let mine = load_store(core)
        .remove(&profile_key(core))
        .unwrap_or_default();
    let n = mine.len();
    let mut all = mine;
    all.extend(stream_presets(core));
    (all, n)
}

/// The presets for the active profile and this cut's canvas.
pub fn presets(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let (_, fid) = framing_of(core, project, cut)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let fr = doc
        .get(fid)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let (all, mine) = all_presets(core);
    let list: Vec<Value> = all
        .iter()
        .enumerate()
        .filter(|(_, x)| x.canvas == [fr.canvas.w, fr.canvas.h])
        .map(|(i, x)| {
            json!({"name": x.name, "layers": x.layers.len(),
                   "insets": x.layers.iter().filter(|l| l.addition).count(), "saved": i < mine})
        })
        .collect();
    Ok(json!({"profile": profile(core), "presets": list}))
}

/// Saves a take's whole stack as a preset (replacing one of the same name).
pub fn preset_save(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    name: &str,
) -> AppResult<Value> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::invalid("a preset name, at most 60 characters"));
    }
    let (_, fid) = framing_of(core, project, cut)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let fr = doc
        .get(fid)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let layers: Vec<PresetLayer> = stack_of(&doc, ulid_id::<Part>(part)?, fid)?
        .into_iter()
        .map(|x| x.1)
        .collect();
    let mut s = load_store(core);
    let list = s.entry(profile_key(core)).or_default();
    if list.len() >= 100 && !list.iter().any(|x| x.name == name) {
        return Err(AppError::invalid("at most 100 presets per profile"));
    }
    list.retain(|x| x.name != name);
    list.push(LayoutPreset {
        name: name.to_owned(),
        canvas: [fr.canvas.w, fr.canvas.h],
        layers,
    });
    save_store(core, &s)?;
    Ok(json!({"name": name}))
}

/// Deletes a saved preset (stream layouts from the profile are read only).
pub fn preset_delete(core: &AppCore, name: &str) -> AppResult<Value> {
    let mut s = load_store(core);
    let key = profile_key(core);
    let list = s.entry(key).or_default();
    let n = list.len();
    list.retain(|x| x.name != name);
    if list.len() == n {
        return Err(AppError::not_found("no saved preset of that name"));
    }
    save_store(core, &s)?;
    Ok(json!({"deleted": name}))
}

/// Renames a saved preset (one of the new name is replaced).
pub fn preset_rename(core: &AppCore, name: &str, to: &str) -> AppResult<Value> {
    let to = to.trim();
    if to.is_empty() || to.chars().count() > 60 {
        return Err(AppError::invalid("a preset name, at most 60 characters"));
    }
    let mut s = load_store(core);
    let list = s.entry(profile_key(core)).or_default();
    if !list.iter().any(|x| x.name == name) {
        return Err(AppError::not_found("no saved preset of that name"));
    }
    if to != name {
        list.retain(|x| x.name != to);
        for x in list.iter_mut().filter(|x| x.name == name) {
            to.clone_into(&mut x.name);
        }
        save_store(core, &s)?;
    }
    Ok(json!({"name": to}))
}

/// Applies a preset to takes (crops, places and insets; the layout link is kept).
pub fn preset_apply(
    core: &AppCore,
    project: &str,
    cut: &str,
    name: &str,
    parts: &[String],
) -> AppResult<Value> {
    let (_, fid) = framing_of(core, project, cut)?;
    let preset = all_presets(core)
        .0
        .into_iter()
        .find(|x| x.name == name)
        .ok_or_else(|| AppError::not_found("no preset of that name"))?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let fr = doc
        .get(fid)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    if preset.canvas != [fr.canvas.w, fr.canvas.h] {
        return Err(AppError::invalid(format!(
            "that preset is for a {}×{} canvas",
            preset.canvas[0], preset.canvas[1]
        )));
    }
    let mut ops = Vec::new();
    for q in parts {
        ops.extend(stack_ops(&doc, ulid_id::<Part>(q)?, fid, &preset.layers)?);
    }
    run_ops(core, project, cut, &ops)?;
    Ok(json!({"parts": parts.len(), "ops": ops.len()}))
}

/// Whether a model layer is a part's own addition (an inset the inspector may remove).
pub fn is_addition(doc: &gmclip_model::Doc, layer: &str) -> bool {
    ulid_id::<Layer>(layer)
        .ok()
        .and_then(|l| doc.get(l))
        .is_some_and(|l| matches!(l.owner, LayerOwner::PartOverride { .. }))
}
