//! Output shape and vertical crops (AJ 2026-10-14: "make crops for vertical").
//!
//! - **Output toggle** (9:16 / 16:9 per cut): each shape is a framing of the cut; the one
//!   the cut shows and exports is remembered in the home (`framing.json`, see
//!   [`AppCore::set_active_framing`]). Switching to 9:16 the first time creates a 1080×1920
//!   framing whose default stack is one screen layer, a centred 9:16 crop of the source.
//! - **Presets** are cut layouts (Appendix C §2.1): *Full
//!   screen* (one centred crop) and *Screen + facecam* (a 1080×760 screen panel at
//!   y 250 and a 760-wide facecam at (160, 1120), AGENTS.md's steady layout), with the
//!   source crops scaled to the source's own size. Applying one links the part(s) to it.
//! - **Crops per part**: a crop set on the preview is that part's override in the
//!   framing (`crop_set{part, framing}`); "apply to all parts" links every part to the
//!   selected part's layout and copies its crops and places, as one transaction.

use gmclip_model::entities::{
    Cut, Framing, Layout, MediaRef, MemberRole, Part, Source, SyncMember, TrackKind,
};
use gmclip_model::{Doc, Id};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

pub const PRESETS: &[(&str, &str)] = &[
    ("full", "Full screen"),
    ("screen_cam", "Screen + facecam"),
    ("split", "Game + facecam split"),
    // AJ 2026-10-19: more built-in layouts, for either output shape
    ("cam_corner", "Screen + camera corner"),
    ("halves", "Screen and camera halves"),
    ("cam_full", "Camera only"),
];

/// The camera picture's shape in the corner layout (4:3).
const CORNER_CAM_ASPECT: f64 = 4.0 / 3.0;

/// The aspect (px, w/h) of each layer's crop in the newer built-in layouts, screen first:
/// what fills each layer's place exactly.
fn new_preset_aspects(preset: &str, cw: f64, ch: f64) -> Vec<f64> {
    match preset {
        "cam_corner" => vec![cw / ch, CORNER_CAM_ASPECT],
        "halves" if cw > ch => vec![(cw * 2.0 / 3.0) / ch, (cw / 3.0) / ch],
        "halves" => vec![cw / (ch / 2.0), cw / (ch / 2.0)],
        "cam_full" => vec![cw / ch],
        _ => vec![],
    }
}

/// A default camera crop of aspect `a` (px) where none is drawn: the bottom-left quarter
/// width of the picture, as `screen_cam`'s.
fn default_cam(sw: f64, sh: f64, a: f64) -> [f64; 4] {
    let w = 0.25;
    let h = ((w * sw / a) / sh).min(1.0);
    let w = (h * sh * a / sw).min(w);
    [0.02, (1.0 - h - 0.03).max(0.0), w, h]
}

/// The camera band of the `split` preset: the top 30 % of the frame (SPEC §5.7.1 step 2).
const SPLIT_CAM: f64 = 0.30;
/// AGENTS.md's steady layout on a 1080×1920 canvas: the screen panel's bottom and the
/// facecam's top (px).
const PANEL_BOTTOM: f64 = 1010.0;
const FACE_TOP: f64 = 1120.0;

/// What the active profile's kit seeds into a new 9:16 framing (SPEC §5.7.1 step 2,
/// "template wins"; lane S): the layout preset, the export loudness, the caption preset
/// and, for the split stack, the caption seat in the seam. No profile → `None` (each
/// caller keeps its own default).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KitSeed {
    pub template: Option<String>,
    pub loudness: Option<(f64, f64)>,
    pub caption_preset: Option<String>,
    /// The caption seat the profile pins for 9:16 (canvas px of 1920; AJ's: 1065).
    pub caption_seat: Option<f64>,
}

impl KitSeed {
    /// The active profile's (Owner settings), content only.
    pub fn of(core: &AppCore) -> KitSeed {
        let Some(dir) = gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data).profile
        else {
            return KitSeed::default();
        };
        let Ok(p) = gmclip_core::profile::load_in(&core.dirs.app_data, &dir) else {
            return KitSeed::default();
        };
        let c = &p.content;
        let loudness = c.get("loudness").and_then(|l| {
            (l["mode"] == "static").then_some(())?;
            Some((
                l["integrated_lufs"].as_f64()?,
                l["true_peak_dbtp"].as_f64().unwrap_or(-1.0),
            ))
        });
        KitSeed {
            template: c
                .get("default_template")
                .and_then(Value::as_str)
                .map(str::to_owned),
            loudness: loudness
                .filter(|(i, t)| (-40.0..=0.0).contains(i) && (-9.0..=0.0).contains(t)),
            caption_preset: c
                .get("caption_preset")
                .and_then(Value::as_str)
                .filter(|n| {
                    gmclip_core::library::builtins()
                        .iter()
                        .any(|b| b.name == *n)
                })
                .map(str::to_owned),
            caption_seat: c
                .get("caption_seats")
                .and_then(|x| x.get("9:16"))
                .and_then(|x| x.get(0))
                .and_then(Value::as_f64)
                .filter(|y| (1.0..1920.0).contains(y)),
        }
    }

    /// The caption seat seeding writes into a new 9:16 layout of `preset` (SPEC §5.7.1
    /// step 5), as a normalised place: the seam of a split stack; for screen + face the
    /// middle of the gap between the panel's bottom (y 1010) and the face's top (y 1120)
    /// — unless the profile pins the seat (AJ's 1065, which is also the default with no
    /// profile): a pinned seat is the framing's ([`seed_framing`]), so "Caption height"
    /// still moves it. A single full-frame layer has no seat of its own.
    pub fn seat(&self, preset: &str) -> Option<Value> {
        let y = match preset {
            "split" => SPLIT_CAM,
            "screen_cam" if self.caption_seat.is_none() && self.template.is_some() => {
                (PANEL_BOTTOM + FACE_TOP) / 2.0 / 1920.0
            }
            _ => return None,
        };
        Some(json!({"x": 0.5, "y": y, "w": 0.9}))
    }

    /// The layout preset for a source of this shape: the template's when the source has
    /// what it needs (a camera box for the split stack), else `fallback`.
    pub fn preset(&self, has_cam: bool, fallback: &'static str) -> &'static str {
        match self.template.as_deref() {
            Some("gameplay_facecam_split") if has_cam => "split",
            Some("gameplay_facecam_split") => "full",
            Some("screen_face") => "screen_cam",
            Some("talking_head") => "full",
            _ => fallback,
        }
    }
}

/// After a new 9:16 framing is made: the kit's loudness (else AJ's −16 LUFS, the reference reels'
/// preset), its caption preset, and the caption seat in the seam of a split stack.
pub fn seed_framing(
    core: &AppCore,
    project: &str,
    cut: &str,
    framing: &str,
    seed: &KitSeed,
    preset: &str,
) -> AppResult<()> {
    let (lufs, tp) = seed.loudness.unwrap_or((-16.0, -1.5));
    let mut set = json!({"framing": framing,
        "loudness": {"static": {"integrated_lufs": lufs, "true_peak_dbtp": tp}}});
    if preset == "split" {
        set["caption_place"] = json!({"x": 0.5, "y": SPLIT_CAM, "w": 0.9});
    } else if let Some(y) = seed.caption_seat {
        // the profile's pinned seat (step 5): the framing's, kept by "captions on"
        set["caption_place"] = json!({"x": 0.5, "y": y / 1920.0, "w": 0.9});
    }
    core.call(project, "framing_set", set)?;
    if let Some(name) = &seed.caption_preset {
        // the style gallery's apply (it adds the preset's font first); a preset that
        // cannot be applied leaves the default style, never a failed framing
        core.set_active_framing(project, cut, framing)?;
        let _ = crate::studio::apply(core, project, cut, name);
    }
    Ok(())
}

/// The cut's framings, the one shown, its layouts and which layout each part uses.
pub fn framings(core: &AppCore, project: &str, cut: &str) -> Value {
    let Ok(p) = core.project(project) else {
        return Value::Null;
    };
    let doc = p.handle.snapshot().doc.clone();
    let Ok(cid) = ulid_id::<Cut>(cut) else {
        return Value::Null;
    };
    let active = core.active_framing(project, cut);
    let list: Vec<Value> = doc
        .children::<Framing>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|f| {
            json!({"id": f.id.0.to_string(), "name": f.name, "w": f.canvas.w, "h": f.canvas.h,
                   "aspect": aspect_of(f.canvas.w, f.canvas.h)})
        })
        .collect();
    let layouts: Vec<Value> = doc
        .children::<Layout>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|l| json!({"id": l.id.0.to_string(), "name": l.name}))
        .collect();
    let fid = active.as_deref().and_then(|f| ulid_id::<Framing>(f).ok());
    let part_layouts: serde_json::Map<String, Value> = doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|pt| {
            let name = fid
                .and_then(|f| pt.look.get(&f))
                .and_then(|l| l.layout_ref)
                .and_then(|l| doc.get(l))
                .map(|l| Value::from(l.name.clone()))
                .unwrap_or(Value::Null);
            (pt.id.0.to_string(), name)
        })
        .collect();
    let part_layout_ids: serde_json::Map<String, Value> = doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
        .map(|pt| {
            let id = fid
                .and_then(|f| pt.look.get(&f))
                .and_then(|l| l.layout_ref)
                .filter(|l| doc.get(*l).is_some())
                .map_or(Value::Null, |l| Value::from(l.0.to_string()));
            (pt.id.0.to_string(), id)
        })
        .collect();
    let aspect = list
        .iter()
        .find(|f| Some(f["id"].as_str().unwrap_or_default()) == active.as_deref())
        .map(|f| f["aspect"].clone())
        .unwrap_or(Value::Null);
    json!({"active": active, "aspect": aspect, "list": list, "layouts": layouts,
           "part_layouts": part_layouts, "part_layout_ids": part_layout_ids,
           "presets": PRESETS.iter().map(|(k, n)| json!({"id": k, "name": n})).collect::<Vec<_>>()})
}

pub(crate) fn aspect_of(w: u32, h: u32) -> &'static str {
    if h > w {
        "9:16"
    } else if w > h {
        "16:9"
    } else {
        "1:1"
    }
}

/// The picture size of a part's media (a source, or a sync group's picture member).
pub fn picture_dims(doc: &Doc, media: &MediaRef) -> (f64, f64) {
    let sid: Option<Id<Source>> = match media {
        MediaRef::Source(s) => Some(*s),
        MediaRef::Group(g) => doc
            .iter::<SyncMember>()
            .find(|m| m.group == *g && m.role == MemberRole::Picture)
            .map(|m| m.source),
    };
    sid.and_then(|s| doc.get(s))
        .and_then(|s| s.probe.tracks.iter().find(|t| t.kind == TrackKind::Video))
        .and_then(|t| Some((f64::from(t.width?), f64::from(t.height?))))
        .unwrap_or((1920.0, 1080.0))
}

/// A centred crop of a `sw×sh` source with pixel aspect `a` (w/h), as large as fits.
fn centred(sw: f64, sh: f64, a: f64) -> [f64; 4] {
    let src = sw / sh;
    if src >= a {
        let w = a / src;
        [(1.0 - w) / 2.0, 0.0, w, 1.0]
    } else {
        let h = src / a;
        [0.0, (1.0 - h) / 2.0, 1.0, h]
    }
}

fn rect(r: [f64; 4]) -> Value {
    json!({"x": r[0], "y": r[1], "w": r[2], "h": r[3]})
}

/// The layers of a preset for a `sw×sh` source on a `cw×ch` canvas: (role, crop, place, z).
fn preset_layers(
    preset: &str,
    sw: f64,
    sh: f64,
    cw: f64,
    ch: f64,
) -> Vec<(Value, Value, Value, i32)> {
    let screen = json!({"region": "screen"});
    match preset {
        "split" => {
            // SPEC §5.7.1: the camera across the top 30 %, full width, cover-cropped; the
            // game below it; captions in the seam (`seed_framing`); no key
            let cam_h = SPLIT_CAM * ch;
            vec![
                (
                    screen,
                    rect(centred(sw, sh, cw / (ch - cam_h))),
                    json!({"x": 0.0, "y": SPLIT_CAM, "w": 1.0}),
                    0,
                ),
                (
                    json!({"region": "cam"}),
                    rect(centred(sw, sh, cw / cam_h)),
                    json!({"x": 0.0, "y": 0.0, "w": 1.0}),
                    1,
                ),
            ]
        }
        "screen_cam" => {
            // AGENTS.md: screen panel y≈250–1010 full width; facecam [160,1120,760,520].
            let (py, ph) = (250.0 * ch / 1920.0, 760.0 * ch / 1920.0);
            let panel = centred(sw, sh, cw / ph);
            let cam_a = 760.0 / 520.0;
            let cam_w = 0.25;
            let cam_h = (cam_w * sw / cam_a) / sh;
            let cam = [0.02, (1.0 - cam_h - 0.03).max(0.0), cam_w, cam_h.min(1.0)];
            vec![
                (
                    screen,
                    rect(panel),
                    json!({"x": 0.0, "y": py / ch, "w": 1.0}),
                    0,
                ),
                (
                    json!({"region": "cam"}),
                    rect(cam),
                    json!({"x": 160.0 / 1080.0, "y": 1120.0 / 1920.0, "w": 760.0 / 1080.0}),
                    1,
                ),
            ]
        }
        "cam_corner" => {
            // the screen over the whole frame, the camera in the bottom-right corner
            let a = new_preset_aspects(preset, cw, ch);
            let w = if cw > ch { 0.26 } else { 0.42 };
            let m = 0.03 * cw.min(ch);
            let h = w * cw / CORNER_CAM_ASPECT;
            vec![
                (
                    screen,
                    rect(centred(sw, sh, a[0])),
                    json!({"x": 0.0, "y": 0.0, "w": 1.0}),
                    0,
                ),
                (
                    json!({"region": "cam"}),
                    rect(default_cam(sw, sh, a[1])),
                    json!({"x": 1.0 - w - m / cw, "y": (ch - m - h) / ch, "w": w}),
                    1,
                ),
            ]
        }
        "halves" => {
            // landscape: the screen on the left two thirds, the camera on the right third;
            // portrait: the screen over the top half, the camera under it
            let a = new_preset_aspects(preset, cw, ch);
            let (sp, cp) = if cw > ch {
                (
                    json!({"x": 0.0, "y": 0.0, "w": 2.0 / 3.0}),
                    json!({"x": 2.0 / 3.0, "y": 0.0, "w": 1.0 / 3.0}),
                )
            } else {
                (
                    json!({"x": 0.0, "y": 0.0, "w": 1.0}),
                    json!({"x": 0.0, "y": 0.5, "w": 1.0}),
                )
            };
            vec![
                (screen, rect(centred(sw, sh, a[0])), sp, 0),
                (
                    json!({"region": "cam"}),
                    rect(default_cam(sw, sh, a[1])),
                    cp,
                    1,
                ),
            ]
        }
        "cam_full" => vec![(
            json!({"region": "cam"}),
            rect(centred(sw, sh, cw / ch)),
            json!({"x": 0.0, "y": 0.0, "w": 1.0}),
            0,
        )],
        _ => vec![(
            screen,
            rect(centred(sw, sh, cw / ch)),
            json!({"x": 0.0, "y": 0.0, "w": 1.0}),
            0,
        )],
    }
}

fn id_of(v: &Value, k: &str) -> String {
    v["output"][k].as_str().unwrap_or_default().to_owned()
}

/// Shows the cut in 9:16 or 16:9, creating the framing the first time.
pub fn set_output(core: &AppCore, project: &str, cut: &str, aspect: &str) -> AppResult<Value> {
    let (framing, _) = ensure(core, project, cut, aspect)?;
    core.set_active_framing(project, cut, &framing)?;
    core.bus.project(
        project,
        crate::events::ProjectEvent::Output {
            cut: cut.to_owned(),
        },
    );
    Ok(json!({"framing": framing, "aspect": aspect}))
}

/// The cut's framing of `aspect` (9:16 or 16:9), made from the layout defaults when it has
/// none (the kit's template, its caption seat and loudness for a Short), without changing
/// the framing the cut shows: `(framing id, made now)`.
pub fn ensure(core: &AppCore, project: &str, cut: &str, aspect: &str) -> AppResult<(String, bool)> {
    if aspect != "9:16" && aspect != "16:9" {
        return Err(AppError::invalid("the output is 9:16 or 16:9"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let existing = doc
        .children::<Framing>(cid.entity())
        .filter_map(|id| doc.get(id))
        .find(|f| aspect_of(f.canvas.w, f.canvas.h) == aspect)
        .map(|f| f.id.0.to_string());
    Ok(match existing {
        Some(f) => (f, false),
        None => {
            let first = doc
                .children::<Framing>(cid.entity())
                .find_map(|id| doc.get(id))
                .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))?;
            let fps = json!({"num": first.fps.num, "den": first.fps.den});
            let media = doc
                .children::<Part>(cid.entity())
                .find_map(|id| doc.get(id))
                .map(|pt| pt.media)
                .ok_or_else(|| AppError::new("Unsupported", "the cut has no parts"))?;
            let (sw, sh) = picture_dims(&doc, &media);
            let (w, h) = if aspect == "9:16" {
                (1080, 1920)
            } else {
                (1920, 1080)
            };
            let f = id_of(
                &core.call(
                    project,
                    "framing_create",
                    json!({"cut": cut, "name": aspect, "w": w, "h": h, "fps": fps}),
                )?,
                "id",
            );
            // SPEC §5.7.1 step 2: the profile kit's template chooses the 9:16 stack
            let seed = KitSeed::of(core);
            let kit_preset = if aspect == "9:16" {
                seed.preset(source_boxes(&doc, &media).1.is_some(), "full")
            } else {
                "full"
            };
            let ops: Vec<Value> = preset_layers("full", sw, sh, f64::from(w), f64::from(h))
                .into_iter()
                .map(|(role, crop, place, z)| {
                    json!({"op": "layer_add", "owner": {"framing": f}, "role": role,
                           "crop": crop, "place": place, "z": z})
                })
                .collect();
            core.call(project, "layout_edit", json!({"cut": cut, "ops": ops}))?;
            // parts from a video of another shape get the full frame of their own video
            let now = p.handle.snapshot().doc.clone();
            let first = shape_of(&now, &media);
            let other: Vec<String> = now
                .children::<Part>(cid.entity())
                .filter_map(|id| now.get(id))
                .filter(|pt| shape_of(&now, &pt.media) != first)
                .map(|pt| pt.id.0.to_string())
                .collect();
            if !other.is_empty() {
                apply_preset_in(core, project, cut, &f, &other, "full")?;
            }
            if kit_preset != "full" {
                let seat = if aspect == "9:16" {
                    seed.seat(kit_preset)
                } else {
                    None
                };
                apply_preset_seated(core, project, cut, &f, &[], kit_preset, seat)?;
            }
            // a recording's face is a file of its own, which the presets above never show:
            // its 9:16 starts as the screen over the camera (decision 0052)
            if aspect == "9:16" {
                stack_recordings(core, project, cut, &f)?;
            }
            // a Short is delivered at the kit's loudness (else AJ's static −16 LUFS,
            // −1.5 dBTP; the reference reels' preset); a 16:9 cut stays as recorded
            if aspect == "9:16" {
                seed_framing(core, project, cut, &f, &seed, kit_preset)?;
            }
            // a short's end card to the long-form takes its places in the new shape
            if crate::endcard::has_card(core, project, cut) {
                core.call(project, "end_card_longform_fit", json!({"cut": cut}))?;
            }
            (f, true)
        }
    })
}

/// Links every part of `cut` whose sync group has a screen and a separate camera file (a
/// recording, decision 0045) to the built-in "Screen over camera" in `framing`: one
/// layout per group, since its layers read that group's members.
fn stack_recordings(core: &AppCore, project: &str, cut: &str, framing: &str) -> AppResult<()> {
    use gmclip_core::commands::screenlook::{SCREEN_OVER_CAMERA, screen_and_camera};
    let doc = core.project(project)?.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fid = ulid_id::<Framing>(framing)?;
    let mut groups: Vec<(MediaRef, Vec<String>)> = Vec::new();
    for p in doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
    {
        if !matches!(p.media, MediaRef::Group(_)) {
            continue;
        }
        let (s, c) = screen_and_camera(&doc, p, fid);
        if !(s.is_some_and(|s| s.member.is_some()) && c.is_some_and(|c| c.member.is_some())) {
            continue;
        }
        let id = p.id.0.to_string();
        match groups.iter_mut().find(|(m, _)| *m == p.media) {
            Some((_, ids)) => ids.push(id),
            None => groups.push((p.media, vec![id])),
        }
    }
    for (_, parts) in groups {
        core.call(
            project,
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": SCREEN_OVER_CAMERA, "parts": parts}),
        )?;
    }
    Ok(())
}

/// The export loudness of the cut's framing on screen: `as_recorded`, or a static target
/// (LUFS) with a −1.5 dBTP true-peak ceiling.
pub fn set_loudness(
    core: &AppCore,
    project: &str,
    cut: &str,
    lufs: Option<f64>,
) -> AppResult<Value> {
    let framing = active(core, project, cut)?;
    let mode = match lufs {
        None => json!("as_recorded"),
        Some(l) => json!({"static": {"integrated_lufs": l, "true_peak_dbtp": -1.5}}),
    };
    core.call(
        project,
        "framing_set",
        json!({"framing": framing, "loudness": mode}),
    )
}

fn active(core: &AppCore, project: &str, cut: &str) -> AppResult<String> {
    core.active_framing(project, cut)
        .ok_or_else(|| AppError::new("Unsupported", "this cut has no framing yet"))
}

/// Links parts (all when `parts` is empty) to a preset layout, creating it on first use.
pub fn apply_preset(
    core: &AppCore,
    project: &str,
    cut: &str,
    parts: &[String],
    preset: &str,
) -> AppResult<Value> {
    let framing = active(core, project, cut)?;
    apply_preset_in(core, project, cut, &framing, parts, preset)
}

/// The picture source of a part's media (a source, or a sync group's picture member).
pub(crate) fn picture_source(doc: &Doc, media: &MediaRef) -> Option<Id<Source>> {
    match media {
        MediaRef::Source(s) => Some(*s),
        MediaRef::Group(g) => doc
            .iter::<SyncMember>()
            .find(|m| m.group == *g && m.role == MemberRole::Picture)
            .map(|m| m.source),
    }
}

/// A source's screen and camera boxes, when it has them: its own regions, else its
/// stream layout's (SPEC §5.7.1 seeding uses each source's stream layout).
pub fn source_boxes(doc: &Doc, media: &MediaRef) -> (Option<[f64; 4]>, Option<[f64; 4]>) {
    use gmclip_model::entities::{Region, RegionRole, StreamLayout};
    let Some(sid) = picture_source(doc, media) else {
        return (None, None);
    };
    let own: Vec<(RegionRole, [f64; 4])> = doc
        .children::<Region>(sid.entity())
        .filter_map(|r| doc.get(r))
        .map(|r| (r.role, [r.rect.x, r.rect.y, r.rect.w, r.rect.h]))
        .collect();
    let laid: Vec<(RegionRole, [f64; 4])> = doc
        .get(sid)
        .and_then(|s| s.stream_layout)
        .and_then(|l| doc.get::<StreamLayout>(l))
        .map(|l| {
            l.regions
                .iter()
                .map(|r| (r.role, [r.rect.x, r.rect.y, r.rect.w, r.rect.h]))
                .collect()
        })
        .unwrap_or_default();
    let find = |role: RegionRole| {
        own.iter()
            .chain(&laid)
            .find(|(r, b)| *r == role && b[2] > 0.0 && b[3] > 0.0)
            .map(|(_, b)| *b)
    };
    (find(RegionRole::Screen), find(RegionRole::Cam))
}

/// What a preset's crops depend on for one part: its picture's size and boxes.
#[derive(Clone, Debug, PartialEq)]
struct Shape {
    dims: (f64, f64),
    boxes: (Option<[f64; 4]>, Option<[f64; 4]>),
}

fn shape_of(doc: &Doc, media: &MediaRef) -> Shape {
    Shape {
        dims: picture_dims(doc, media),
        boxes: source_boxes(doc, media),
    }
}

/// A preset's layers for one picture shape: the plain preset, with the camera crop on
/// the source's camera box and the screen crop inside its screen box when it has them.
fn shaped_layers(preset: &str, sh: &Shape, cw: f64, ch: f64) -> Vec<(Value, Value, Value, i32)> {
    let (sw, shh) = sh.dims;
    let mut layers = preset_layers(preset, sw, shh, cw, ch);
    if preset == "split" {
        // a crop of aspect `a` (px) centred inside a normalised box of the source
        let inside = |b: [f64; 4], a: f64| {
            let inner = centred(b[2] * sw, b[3] * shh, a);
            [
                b[0] + inner[0] * b[2],
                b[1] + inner[1] * b[3],
                inner[2] * b[2],
                inner[3] * b[3],
            ]
        };
        let cam_h = SPLIT_CAM * ch;
        if let Some(scr) = sh.boxes.0 {
            layers[0].1 = rect(inside(scr, cw / (ch - cam_h)));
        }
        if let Some(cam) = sh.boxes.1 {
            layers[1].1 = rect(inside(cam, cw / cam_h));
        }
    }
    if matches!(preset, "cam_corner" | "halves" | "cam_full") {
        // each crop at its place's shape, centred inside the drawn screen or camera box
        let inside = |b: [f64; 4], a: f64| {
            let inner = centred(b[2] * sw, b[3] * shh, a);
            [
                b[0] + inner[0] * b[2],
                b[1] + inner[1] * b[3],
                inner[2] * b[2],
                inner[3] * b[3],
            ]
        };
        let a = new_preset_aspects(preset, cw, ch);
        for (k, l) in layers.iter_mut().enumerate() {
            let cam = l.0["region"] == "cam";
            if let Some(b) = if cam { sh.boxes.1 } else { sh.boxes.0 } {
                l.1 = rect(inside(b, a[k]));
            }
        }
    }
    if preset == "screen_cam" {
        if let Some(scr) = sh.boxes.0 {
            // the panel's aspect, centred inside the screen box
            let ph = 760.0 * ch / 1920.0;
            let inner = centred(scr[2] * sw, scr[3] * shh, cw / ph);
            let r = [
                scr[0] + inner[0] * scr[2],
                scr[1] + inner[1] * scr[3],
                inner[2] * scr[2],
                inner[3] * scr[3],
            ];
            layers[0].1 = rect(r);
        }
        if let Some(cam) = sh.boxes.1 {
            layers[1].1 = rect(cam);
        }
    }
    layers
}

/// The name of a source's file, for a layout made for its shape.
fn source_name(doc: &Doc, media: &MediaRef) -> String {
    picture_source(doc, media)
        .and_then(|s| doc.get(s))
        .map(|s| {
            let f = crate::file_part(&s.path);
            f.rsplit_once('.')
                .map_or(f, |(n, _)| n)
                .chars()
                .take(40)
                .collect()
        })
        .unwrap_or_else(|| "video".into())
}

/// [`apply_preset`] in one framing. A cut whose parts come from videos of different
/// shapes (or with their own screen/camera boxes) gets one layout per shape, each part
/// linked to its own video's: a part's look resolves per its own source.
pub fn apply_preset_in(
    core: &AppCore,
    project: &str,
    cut: &str,
    framing: &str,
    parts: &[String],
    preset: &str,
) -> AppResult<Value> {
    apply_preset_seated(core, project, cut, framing, parts, preset, None)
}

/// [`apply_preset_in`], with the caption seat (a normalised place) written into each
/// layout it creates (seeding, SPEC §5.7.1 step 5).
pub fn apply_preset_seated(
    core: &AppCore,
    project: &str,
    cut: &str,
    framing: &str,
    parts: &[String],
    preset: &str,
    seat: Option<Value>,
) -> AppResult<Value> {
    let name = PRESETS
        .iter()
        .find(|(k, _)| *k == preset)
        .map(|(_, n)| *n)
        .ok_or_else(|| AppError::invalid("no such layout preset"))?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fr = doc
        .get(ulid_id::<Framing>(framing)?)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    let (cw, ch) = (f64::from(fr.canvas.w), f64::from(fr.canvas.h));
    // One layout per preset and canvas shape (and picture shape, after the first).
    let lname = format!("{name} {}", aspect_of(fr.canvas.w, fr.canvas.h));
    let all: Vec<&Part> = doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    let first = all
        .first()
        .map(|pt| pt.media)
        .ok_or_else(|| AppError::new("Unsupported", "the cut has no parts"))?;
    let first_shape = shape_of(&doc, &first);
    let targets: Vec<&Part> = if parts.is_empty() {
        all.clone()
    } else {
        all.iter()
            .filter(|pt| parts.contains(&pt.id.0.to_string()))
            .copied()
            .collect()
    };
    if targets.len() != parts.len() && !parts.is_empty() {
        return Err(AppError::not_found("no such part in this cut"));
    }
    // the layout of each shape, made on first use
    let mut made: Vec<(Shape, String)> = Vec::new();
    let mut links: Vec<Value> = Vec::new();
    for pt in &targets {
        let sh = shape_of(&doc, &pt.media);
        let layout = match made.iter().find(|(s, _)| *s == sh) {
            Some((_, l)) => l.clone(),
            None => {
                let wanted = if sh == first_shape {
                    lname.clone()
                } else {
                    format!("{lname} · {}", source_name(&doc, &pt.media))
                };
                let now = p.handle.snapshot().doc.clone();
                let l = match now
                    .children::<Layout>(cid.entity())
                    .filter_map(|id| now.get(id))
                    .find(|l| l.name == wanted)
                {
                    Some(l) => l.id.0.to_string(),
                    None => {
                        let mut args = json!({"cut": cut, "name": wanted});
                        if let Some(seat) = &seat {
                            args["caption_place"] = seat.clone();
                        }
                        let l = id_of(&core.call(project, "layout_create", args)?, "id");
                        let ops: Vec<Value> = shaped_layers(preset, &sh, cw, ch)
                            .into_iter()
                            .map(|(role, crop, place, z)| {
                                json!({"op": "layer_add", "owner": {"layout": l}, "role": role,
                                       "crop": crop, "place": place, "z": z})
                            })
                            .collect();
                        core.call(project, "layout_edit", json!({"cut": cut, "ops": ops}))?;
                        l
                    }
                };
                made.push((sh, l.clone()));
                l
            }
        };
        links.push(json!({"op": "layout_link", "part": pt.id.0.to_string(), "framing": framing, "layout": layout}));
    }
    for chunk in links.chunks(200) {
        core.call(project, "layout_edit", json!({"cut": cut, "ops": chunk}))?;
    }
    let layout = made.first().map(|x| x.1.clone()).unwrap_or_default();
    Ok(json!({"layout": layout, "parts": targets.len(), "layouts": made.len()}))
}

/// A part just added (from any video): in each framing where the cut's parts use a
/// preset layout, it takes that preset for its own video's shape; where none does and
/// its picture is another shape than the first part's, it gets the full-frame preset so
/// the framing's default crop (made for the first video) never cuts it wrong.
pub fn fit_new_part(core: &AppCore, project: &str, cut: &str, part: &str) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let pid = ulid_id::<Part>(part)?;
    let Some(new) = doc.get(pid) else {
        return Ok(());
    };
    let others: Vec<&Part> = doc
        .children::<Part>(cid.entity())
        .filter_map(|id| doc.get(id))
        .filter(|x| x.id != pid)
        .collect();
    let Some(first) = others.first() else {
        return Ok(());
    };
    let differs = shape_of(&doc, &new.media) != shape_of(&doc, &first.media);
    for f in doc.children::<Framing>(cid.entity()) {
        // the preset most of the other parts are linked to in this framing
        let used = others.iter().find_map(|x| {
            let l = x.look.get(&f)?.layout_ref?;
            let name = &doc.get(l)?.name;
            PRESETS
                .iter()
                .find(|(_, n)| name.starts_with(n))
                .map(|(k, _)| *k)
        });
        let preset = match used {
            Some(k) => k,
            None if differs => "full",
            None => continue,
        };
        if new.look.get(&f).is_some_and(|l| l.layout_ref.is_some()) {
            continue;
        }
        apply_preset_in(
            core,
            project,
            cut,
            &f.0.to_string(),
            &[part.to_owned()],
            preset,
        )?;
    }
    Ok(())
}

/// Gives every part the selected part's layout, crops, places and insets.
pub fn apply_to_all(core: &AppCore, project: &str, cut: &str, from: &str) -> AppResult<Value> {
    crate::takelayout::copy_layout(core, project, cut, from, None)
}

/// Sets one part's crop of a layer (normalised source rect) in the framing shown.
pub fn crop_norm(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    layer: &str,
    r: [f64; 4],
) -> AppResult<Value> {
    crop_norm_group(core, project, cut, part, layer, r, &[])
}

/// [`crop_norm`], on every layer of `group` too when this one is in it ([`set_on_group`]).
pub fn crop_norm_group(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    layer: &str,
    r: [f64; 4],
    group: &[(String, String)],
) -> AppResult<Value> {
    let framing = active(core, project, cut)?;
    if r.iter().any(|v| !v.is_finite()) || r[2] <= 0.0 || r[3] <= 0.0 {
        return Err(AppError::invalid(
            "a crop is a rectangle inside the picture",
        ));
    }
    // Inside the picture, keeping its size where it can.
    let w = r[2].min(1.0);
    let h = r[3].min(1.0);
    let x = r[0].clamp(0.0, 1.0 - w);
    let y = r[1].clamp(0.0, 1.0 - h);
    let crop = json!({"x": x, "y": y, "w": w, "h": h});
    if let Some(r) = set_on_group(
        core,
        project,
        cut,
        &framing,
        part,
        layer,
        group,
        "layer_crop",
        "crop",
        &crop,
    ) {
        return r;
    }
    core.call(
        project,
        "crop_set",
        json!({"layer": layer, "part": part, "framing": framing, "crop": crop}),
    )
}

/// The layers grouped on the timeline (AJ 2026-10-20: "group them and change the crop for
/// multiple cameras"), `[{part, layer}]` model ids: absent or malformed is no group.
pub fn group_arg(a: &Value) -> Vec<(String, String)> {
    a.get("group")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|m| {
                    Some((
                        m.get("part")?.as_str()?.to_owned(),
                        m.get("layer")?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A crop or place written on one part's layer, and on every other layer of its group
/// when it is grouped: each member's override in `framing`, one transaction (one ⌘Z).
/// `None` when the layer is not in a group of two or more.
#[allow(clippy::too_many_arguments)]
pub fn set_on_group(
    core: &AppCore,
    project: &str,
    cut: &str,
    framing: &str,
    part: &str,
    layer: &str,
    group: &[(String, String)],
    op: &str,
    key: &str,
    value: &Value,
) -> Option<AppResult<Value>> {
    let grouped = group.len() > 1 && group.iter().any(|(p, l)| p == part && l == layer);
    if !grouped {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let ops: Vec<Value> = group
        .iter()
        .filter(|m| seen.insert((*m).clone()))
        .take(200)
        .map(|(p, l)| {
            let mut o = json!({"op": op, "layer": l, "part": p, "framing": framing});
            o[key] = value.clone();
            o
        })
        .collect();
    Some(core.call(project, "layout_edit", json!({"cut": cut, "ops": ops})))
}

/// The crop editor's picture (AJ 2026-10-19): the cut planned as it is, each part showing
/// only its own source picture whole, at the source's own shape — `part`'s picture is the
/// canvas, the other parts' are fitted into it — with nothing over it (no other layers,
/// overlays, captions or background). Crops are drawn over this, so a portrait recording
/// is shown portrait, not through the 16:9 framing.
///
/// With `layer` (a model layer of `part`), each part shows the picture of that layer, else
/// of its layer of the same role, else its first: a recording's camera layer reads the
/// face file beside the screen (AJ 2026-10-20: "it shows me a preview of the screen, not
/// the camera"), so its crop is drawn over the camera's own picture.
pub fn source_view_plan(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    layer: Option<&str>,
) -> AppResult<crate::core::Planned> {
    use gmclip_plan::scalars::{Even, Rgb};
    use gmclip_plan::types::{Background, Canvas, Rect};
    let pl = core.planned(project, cut)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let dims_of = |model: &str| {
        ulid_id::<Part>(model)
            .ok()
            .and_then(|id| doc.get(id))
            .map(|m| picture_dims(&doc, &m.media))
    };
    let source_dims = |sid: &str| {
        pl.plan
            .sources
            .iter()
            .find(|s| s.id.as_str() == sid)
            .and_then(|s| s.expect.as_ref()?.video.as_ref())
            .and_then(|v| Some((v.w? as f64, v.h? as f64)))
    };
    // the role of the layer asked for, read from `part`'s plan parts
    let role = layer.and_then(|m| {
        pl.index
            .parts
            .iter()
            .zip(pl.plan.parts.iter())
            .find_map(|(ix, q)| {
                let (pl_id, _) = ix.layers.iter().find(|(_, x)| ix.model == part && x == m)?;
                q.layers
                    .iter()
                    .find(|l| l.id.as_str() == pl_id)
                    .map(|l| l.role.as_str().to_owned())
            })
    });
    // per plan part: the layer shown, and its picture's size
    let pick = |k: usize, q: &gmclip_plan::types::Part| {
        let ix = pl.index.parts.get(k);
        let model_of = |l: &gmclip_plan::types::Layer| {
            ix.and_then(|ix| ix.layers.iter().find(|(a, _)| a == l.id.as_str()))
                .map(|x| x.1.as_str())
        };
        let l = layer
            .and_then(|m| q.layers.iter().find(|l| model_of(l) == Some(m)))
            .or_else(|| {
                let r = role.as_deref()?;
                q.layers.iter().find(|l| l.role.as_str() == r)
            })
            .or_else(|| q.layers.iter().find(|l| l.picture_source.is_none()))
            .or_else(|| q.layers.first())?;
        let dims = match &l.picture_source {
            Some(s) => source_dims(s.as_str()),
            None => ix.and_then(|ix| dims_of(&ix.model)),
        };
        Some((l.clone(), dims))
    };
    let focus = pl
        .index
        .parts
        .iter()
        .zip(pl.plan.parts.iter())
        .enumerate()
        .find(|(_, (ix, _))| ix.model == part)
        .and_then(|(k, (_, q))| pick(k, q)?.1);
    let (cw, ch) = focus.or_else(|| dims_of(part)).unwrap_or((1920.0, 1080.0));
    let even = |v: f64| Even::new(((v.round() as u64) & !1).max(2));
    let mut plan = (*pl.plan).clone();
    plan.output.canvas = Canvas {
        w: even(cw).map_err(AppError::internal)?,
        h: even(ch).map_err(AppError::internal)?,
    };
    let (cw, ch) = (
        plan.output.canvas.w.get() as f64,
        plan.output.canvas.h.get() as f64,
    );
    for (k, q) in plan.parts.iter_mut().enumerate() {
        let Some((mut l, dims)) = pick(k, q) else {
            continue;
        };
        let (sw, sh) = dims.unwrap_or((cw, ch));
        // the whole picture, fitted (contained) into the canvas
        let s = (cw / sw).min(ch / sh);
        let (w, h) = (sw * s, sh * s);
        // another source's layer keeps reading that source live (`first_frame`, `fps`)
        if l.picture_source.is_none() {
            l.first_frame = None;
            l.fps = None;
        }
        l.crop = Rect {
            x: 0.0,
            y: 0.0,
            w: sw,
            h: sh,
        };
        l.place = Rect {
            x: (cw - w) / 2.0,
            y: (ch - h) / 2.0,
            w,
            h,
        };
        l.z = 0;
        l.visible = None;
        l.opacity = None;
        l.time_map = None;
        l.matte = None;
        l.ease = None;
        l.cover = None;
        l.track = None;
        l.frame = None;
        q.layers = vec![l];
    }
    plan.text_events = None;
    plan.image_events = None;
    plan.shape_events = None;
    plan.clip_events = None;
    plan.tracks = None;
    plan.zooms = None;
    plan.background = Background::Colour {
        rgb: Rgb::new("#000000".to_owned()).map_err(AppError::internal)?,
    };
    Ok(crate::core::Planned {
        plan: std::sync::Arc::new(plan),
        ..pl
    })
}

/// The 16:9 framing of a cut (the full source picture the crop editor draws on), if any.
pub fn source_view_framing(core: &AppCore, project: &str, cut: &str) -> Option<String> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut).ok()?;
    doc.children::<Framing>(cid.entity())
        .filter_map(|id| doc.get(id))
        .find(|f| f.canvas.w > f.canvas.h)
        .map(|f| f.id.0.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centred_crops() {
        let c = centred(1920.0, 1080.0, 1080.0 / 1920.0);
        assert!((c[2] * 1920.0 / (c[3] * 1080.0) - 0.5625).abs() < 1e-9);
        assert!((c[0] + c[2] / 2.0 - 0.5).abs() < 1e-9);
        let c = centred(1080.0, 1920.0, 1080.0 / 760.0);
        assert_eq!(c[2], 1.0);
    }

    fn num(v: &Value, k: &str) -> f64 {
        v[k].as_f64().unwrap()
    }

    #[test]
    fn the_split_stack_puts_the_camera_box_across_the_top_and_the_game_below() {
        // a 2560×1440 stream with its camera inset bottom-right and the game left of it
        let sh = Shape {
            dims: (2560.0, 1440.0),
            boxes: (Some([0.0, 0.0, 0.8, 1.0]), Some([0.8, 0.55, 0.19, 0.24])),
        };
        let l = shaped_layers("split", &sh, 1080.0, 1920.0);
        assert_eq!(l.len(), 2);
        let (game, cam) = (&l[0], &l[1]);
        assert_eq!(game.0["region"], "screen");
        assert_eq!(cam.0["region"], "cam");
        // camera: full width over the top 30 %, cover-cropped inside its box
        assert_eq!(cam.2, json!({"x": 0.0, "y": 0.0, "w": 1.0}));
        let (cw, chh) = (num(&cam.1, "w") * 2560.0, num(&cam.1, "h") * 1440.0);
        assert!((cw / chh - 1080.0 / 576.0).abs() < 1e-6, "{}", cw / chh);
        assert!(
            num(&cam.1, "x") >= 0.8 - 1e-9 && num(&cam.1, "x") + num(&cam.1, "w") <= 0.99 + 1e-9
        );
        assert!(
            num(&cam.1, "y") >= 0.55 - 1e-9 && num(&cam.1, "y") + num(&cam.1, "h") <= 0.79 + 1e-9
        );
        // game: below the seam, the rest of the frame, inside the screen box
        assert_eq!(game.2, json!({"x": 0.0, "y": SPLIT_CAM, "w": 1.0}));
        let (gw, gh) = (num(&game.1, "w") * 2560.0, num(&game.1, "h") * 1440.0);
        assert!((gw / gh - 1080.0 / 1344.0).abs() < 1e-6);
        assert!(num(&game.1, "x") + num(&game.1, "w") <= 0.8 + 1e-9);
    }

    #[test]
    fn the_kit_template_wins_with_a_fallback_when_the_source_lacks_a_camera() {
        let k = |t: &str| KitSeed {
            template: Some(t.into()),
            ..KitSeed::default()
        };
        assert_eq!(k("gameplay_facecam_split").preset(true, "full"), "split");
        assert_eq!(
            k("gameplay_facecam_split").preset(false, "screen_cam"),
            "full"
        );
        assert_eq!(k("screen_face").preset(false, "full"), "screen_cam");
        assert_eq!(k("talking_head").preset(true, "screen_cam"), "full");
        // no profile: the caller's own default
        assert_eq!(KitSeed::default().preset(true, "screen_cam"), "screen_cam");
        assert_eq!(KitSeed::default().preset(true, "full"), "full");
    }
}
