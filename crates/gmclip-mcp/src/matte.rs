//! The holder's media-reading look commands (R.02, R.03): `matte_quality` (each part's
//! cut-out scored from its mask track) and `crop_follow_action` (the activity crop,
//! SPEC §5.7: preview by default, applied as one `crop_follow_apply` transaction).

use gmclip_core::activity::{self, CropRules, LumaFrame, NRect};
use gmclip_core::planner::plan_part_owners;
use gmclip_core::{CallRequest, CmdError};
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::Base;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::holder::{Conn, Holder, OpenProject};

/// Samples per second the activity crop reads.
const SAMPLES_PER_S: u64 = 2;
/// The most frames one part's activity is read from (two minutes at two a second).
const MAX_SAMPLES: u64 = 240;
/// Proxy width the frames are read at.
const LUMA_W: u32 = 640;
/// A split moves to the nearest word gap within this many seconds.
const GAP_SNAP_S: f64 = 1.5;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityArgs {
    cut: String,
    #[serde(default)]
    framing: Option<String>,
    #[serde(default)]
    build: Option<bool>,
}

fn framing_id(doc: &Doc, cut: Id<Cut>, f: Option<&str>) -> Result<Id<Framing>, CmdError> {
    match f {
        Some(f) => {
            let id = gmclip_model::Ulid::parse(f.split_once(':').map_or(f, |(_, r)| r))
                .map(Id::<Framing>::new)
                .map_err(|_| CmdError::invalid("/framing", "not a framing id"))?;
            if doc.get(id).is_none_or(|x| x.cut != cut) {
                return Err(CmdError::invalid("/framing", "not a framing of this cut"));
            }
            Ok(id)
        }
        None => doc
            .children::<Framing>(cut.entity())
            .next()
            .ok_or_else(|| CmdError::invalid("/framing", "the cut has no framing")),
    }
}

/// `matte_quality{cut, framing?, build?}`.
pub fn matte_quality(
    holder: &Holder,
    p: &OpenProject,
    args: Value,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: QualityArgs = serde_json::from_value(args)
        .map_err(|_| CmdError::invalid("/args", "{cut, framing?, build?}"))?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let framing = framing_id(doc, cut, a.framing.as_deref())?;
    // with the transcripts: a cut with heard captions plans only with them
    let plan = holder
        .planner
        .plan_with(doc, cut, framing, &snap.analysis)?;
    let owners = plan_part_owners(doc, cut, framing);
    let mut v = holder
        .media
        .matte_quality(&plan, a.build.unwrap_or(false))?;
    // name the model parts behind the plan's p01, p02, …
    if let Some(rows) = v.get_mut("parts").and_then(Value::as_array_mut) {
        for r in rows {
            let k = r
                .get("plan_part")
                .and_then(Value::as_str)
                .and_then(|s| plan.parts.iter().position(|pp| pp.id.as_str() == s));
            if let Some(id) = k.and_then(|k| owners.get(k)) {
                r["part"] = json!(id);
            }
        }
    }
    v["as_of"] = json!(doc.rev());
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowArgs {
    cut: String,
    #[serde(default)]
    framing: Option<String>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    dry_run: Option<bool>,
}

fn ulid_of<E>(s: &str, path: &str) -> Result<Id<E>, CmdError> {
    gmclip_model::Ulid::parse(s.split_once(':').map_or(s, |(_, r)| r))
        .map(Id::<E>::new)
        .map_err(|_| CmdError::invalid(path, "not an id"))
}

/// Word gaps (source time, the part's media clock) inside `[from, to)`.
fn word_gaps(snap: &gmclip_core::Snapshot, part: &Part) -> Vec<i64> {
    let doc = &snap.doc;
    let owner = match part.media {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    };
    let Some(t) = snap.analysis.active(doc, &owner) else {
        return Vec::new();
    };
    let mut w: Vec<(i64, i64)> = t
        .words
        .iter()
        .filter(|w| w.end > part.src_in.0 && w.start < part.src_out.0)
        .map(|w| (w.start, w.end))
        .collect();
    w.sort();
    w.windows(2)
        .map(|p| (p[0].1 + p[1].0) / 2)
        .filter(|g| *g > part.src_in.0 && *g < part.src_out.0)
        .collect()
}

/// One part's activity crop(s).
#[derive(Clone, Debug)]
pub struct PartFollow {
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    /// `(from, crop, share of the change held, scale)`; `from` is `None` for the first.
    pub crops: Vec<(Option<Flicks>, NormRect, f64, f64)>,
    pub moved: f64,
    pub change: u64,
    pub lint: Option<&'static str>,
}

/// What `crop_follow_action` found: per part, the crop(s) and the split.
#[derive(Clone, Debug)]
pub struct Follow {
    pub framing: Id<Framing>,
    pub parts: Vec<PartFollow>,
    pub skipped: Vec<Value>,
}

impl Follow {
    /// The parts as the reply lists them.
    pub fn listed(&self) -> Vec<Value> {
        self.parts
            .iter()
            .map(|d| {
                json!({
                    "part": d.part, "layer": d.layer, "moved": (d.moved * 1000.0).round() / 1000.0,
                    "change": d.change, "lint": d.lint,
                    "crops": d.crops.iter().map(|(at, c, held, scale)| json!({
                        "at": at, "crop": c, "holds": (held * 1000.0).round() / 1000.0,
                        "scale": (scale * 1000.0).round() / 1000.0,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect()
    }

    /// The `crop_follow_apply` arguments that set it.
    pub fn apply_args(&self) -> Value {
        let crops: Vec<Value> = self
            .parts
            .iter()
            .flat_map(|d| {
                d.crops.iter().map(
                    |(at, c, _, _)| json!({"part": d.part, "layer": d.layer, "at": at, "crop": c}),
                )
            })
            .collect();
        json!({"framing": self.framing, "crops": crops})
    }

    /// Visual splits it adds.
    pub fn splits(&self) -> usize {
        self.parts
            .iter()
            .map(|d| d.crops.len().saturating_sub(1))
            .sum()
    }
}

/// The activity crop of `cut`'s parts (every part with a screen layer, or `only_part`),
/// from frames `media` reads at 2 fps; shared by the agent surface and the app.
pub fn follow(
    planner: &dyn gmclip_core::Planner,
    media: &dyn gmclip_core::MediaRender,
    snap: &gmclip_core::Snapshot,
    cut: Id<Cut>,
    framing: Option<&str>,
    only_part: Option<Id<Part>>,
    only_layer: Option<Id<Layer>>,
) -> Result<Follow, CmdError> {
    let doc = &snap.doc;
    let framing = framing_id(doc, cut, framing)?;
    // with the transcripts: a cut with heard captions plans only with them
    let plan = planner.plan_with(doc, cut, framing, &snap.analysis)?;
    let owners = plan_part_owners(doc, cut, framing);
    let fpf = plan.output.fps.flicks_per_frame() as i64;
    let step = (plan.output.fps.rounded() / SAMPLES_PER_S).max(1);
    let mut done: Vec<PartFollow> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for pid in doc.children::<Part>(cut.entity()) {
        if only_part.is_some_and(|o| o != pid) {
            continue;
        }
        let Some(part) = doc.get(pid) else { continue };
        let plan_k: Vec<usize> = owners
            .iter()
            .enumerate()
            .filter(|(_, o)| **o == pid)
            .map(|(k, _)| k)
            .collect();
        if plan_k.len() != 1 {
            skipped.push(json!({"part": pid, "why": "already has visual splits in this framing"}));
            continue;
        }
        let Some(pp) = plan.parts.get(plan_k[0]) else {
            skipped.push(json!({"part": pid, "why": "not in the plan"}));
            continue;
        };
        // the screen layer, in the model and in the plan
        let Some(stack) = gmclip_model::effective::effective(doc, pid, framing, part.src_in) else {
            continue;
        };
        let Some(ml) = stack.layers.iter().find(|l| match only_layer {
            Some(id) => l.id == id,
            None => l.values.role == LayerRole::Region(RegionRole::Screen),
        }) else {
            skipped.push(json!({"part": pid, "why": "no screen layer"}));
            continue;
        };
        let Some(pl) = pp.layers.iter().find(|l| l.role.as_str() == "screen") else {
            skipped.push(json!({"part": pid, "why": "the screen layer is hidden here"}));
            continue;
        };
        let src_id = pl.picture_source.as_ref().unwrap_or(&pp.picture.source);
        let Some(psrc) = plan.sources.iter().find(|s| &s.id == src_id) else {
            continue;
        };
        // source size from the plan crop (px) and the model crop (normalised)
        let (sw, sh) = (
            (pl.crop.w / ml.values.crop.w.max(1e-9)).round(),
            (pl.crop.h / ml.values.crop.h.max(1e-9)).round(),
        );
        let region = doc
            .iter::<Source>()
            .find(|s| s.path == psrc.path)
            .and_then(|s| {
                doc.children::<Region>(s.id.entity())
                    .filter_map(|r| doc.get(r))
                    .find(|r| r.role == RegionRole::Screen)
                    .map(|r| r.rect)
            })
            .unwrap_or(NormRect::FULL);
        let n = pp.out.end() - pp.out.start();
        // a long part (a whole stream) is sampled more sparsely: at most MAX_SAMPLES
        // frames, so the answer comes in seconds, not after decoding thousands
        let step = step.max(n.div_ceil(MAX_SAMPLES));
        let frames: Vec<u64> = (0..n)
            .step_by(step as usize)
            .map(|i| gmclip_core::gmclip_plan::retime::nominal_frame(pp, i, plan.output.fps))
            .collect();
        if frames.len() < 2 {
            skipped.push(json!({"part": pid, "why": "shorter than a second"}));
            continue;
        }
        let luma: Vec<LumaFrame> = media
            .luma_frames(&plan, src_id.as_str(), &frames, LUMA_W)
            .map_err(CmdError::from)?;
        let gaps = word_gaps(snap, part);
        let sample_of = |t: i64| (t - part.src_in.0) as f64 / (fpf * step as i64) as f64;
        // change behind other layers (the camera inset) and in other regions (chat,
        // alerts, private) is not the screen's
        let nr = |r: &NormRect| NRect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        };
        let mut exclude: Vec<NRect> = stack
            .layers
            .iter()
            .filter(|l| l.id != ml.id && l.values.role != LayerRole::Detail)
            .map(|l| nr(&l.values.crop))
            .filter(|r| r.w * r.h < 0.5)
            .collect();
        if let Some(src) = doc.iter::<Source>().find(|s| s.path == psrc.path) {
            exclude.extend(
                doc.children::<Region>(src.id.entity())
                    .filter_map(|r| doc.get(r))
                    .filter(|r| r.role != RegionRole::Screen)
                    .map(|r| nr(&r.rect)),
            );
        }
        let rules = CropRules {
            region: nr(&region),
            exclude,
            aspect: pl.crop.w / pl.crop.h,
            source: (sw as u32, sh as u32),
            panel_w: pl.place.w,
        };
        let gap_samples: Vec<f64> = gaps.iter().map(|g| sample_of(*g)).collect();
        let Some(act) = activity::analyse(&luma, &rules, &gap_samples) else {
            skipped.push(json!({"part": pid, "why": "no frames"}));
            continue;
        };
        if act.crops.is_empty() {
            skipped.push(json!({"part": pid, "why": "the screen is still here: crop kept"}));
            continue;
        }
        let mut crops = Vec::new();
        for c in &act.crops {
            let at = (c.from_sample > 0).then(|| {
                // the move, on the frame grid, at the nearest word gap within 1.5 s
                let t = part.src_in.0 + c.from_sample as i64 * step as i64 * fpf;
                let snap_t = gaps
                    .iter()
                    .copied()
                    .filter(|g| {
                        ((g - t) as f64).abs()
                            <= GAP_SNAP_S * gmclip_model::time::FLICKS_PER_SECOND as f64
                    })
                    .min_by_key(|g| (g - t).abs())
                    .unwrap_or(t);
                let k = ((snap_t - part.src_in.0) as f64 / fpf as f64).round() as i64;
                Flicks(part.src_in.0 + k.clamp(1, (n as i64) - 1) * fpf)
            });
            let crop = NormRect {
                x: c.crop.x.clamp(0.0, 1.0),
                y: c.crop.y.clamp(0.0, 1.0),
                w: c.crop.w.min(1.0 - c.crop.x.clamp(0.0, 1.0)),
                h: c.crop.h.min(1.0 - c.crop.y.clamp(0.0, 1.0)),
            };
            crops.push((at, crop, c.held, c.scale));
        }
        done.push(PartFollow {
            part: pid,
            layer: ml.id,
            crops,
            moved: act.moved,
            change: act.change,
            lint: act
                .text_too_small
                .then_some("screen text likely too small (effective scale < 0.6)"),
        });
    }
    Ok(Follow {
        framing,
        parts: done,
        skipped,
    })
}

/// `crop_follow_action{cut, framing?, part?, layer?, dry_run?}`.
#[allow(clippy::too_many_arguments)]
pub fn crop_follow_action(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: Value,
    base: Option<Base>,
    request_id: Option<String>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: FollowArgs = serde_json::from_value(args)
        .map_err(|_| CmdError::invalid("/args", "{cut, framing?, part?, layer?, dry_run?}"))?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let only_part: Option<Id<Part>> = a.part.as_deref().map(|s| ulid_of(s, "/part")).transpose()?;
    let only_layer: Option<Id<Layer>> = a
        .layer
        .as_deref()
        .map(|s| ulid_of(s, "/layer"))
        .transpose()?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let f = follow(
        holder.planner.as_ref(),
        holder.media.as_ref(),
        &snap,
        cut,
        a.framing.as_deref(),
        only_part,
        only_layer,
    )?;
    let dry = a.dry_run.unwrap_or(true);
    let splits = f.splits();
    let mut v = json!({
        "dry_run": dry, "parts": f.listed(), "skipped": f.skipped, "as_of": doc.rev(),
    });
    if dry || f.parts.is_empty() {
        v["display"] = json!(format!(
            "follow the action: {} part(s), {splits} split(s), {} skipped{} · as_of {}",
            f.parts.len(),
            v["skipped"].as_array().map_or(0, Vec::len),
            if dry {
                " (preview: run again with dry_run:false and base to apply)"
            } else {
                ""
            },
            doc.rev()
        ));
        return Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES));
    }
    let ok = p.handle.call(CallRequest {
        command: "crop_follow_apply".into(),
        args: f.apply_args(),
        base,
        request_id,
        caller: conn.caller(),
    })?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    v["committed"] = json!(ok.committed);
    v["as_of"] = json!(ok.as_of);
    v["display"] = json!(format!(
        "follow the action: {} part(s) re-cropped, {splits} split(s); committed rev {:?} · as_of {}",
        f.parts.len(),
        ok.committed,
        ok.as_of
    ));
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}
