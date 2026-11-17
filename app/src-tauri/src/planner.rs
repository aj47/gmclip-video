//! The interim planner for imported cuts: model parts and layers → RenderPlan v0.
//!
//! **Stopgap until A18/A33** (the model planner and the reference-plans-into-the-model, lane A).
//! An imported cut keeps its converted RenderPlan as the *base*; the model holds what a
//! human edits (parts: trim, split, reorder, delete; part-override layers: crop, place,
//! visibility, z, opacity). This module re-tiles the base plan from the model:
//!
//! - each model part (in `OrderKey` order) is traced back to its base part (directly, or
//!   through `lineage.split_of`) and shows source frames `[frame(src_in), frame(src_out))`
//!   by SPEC §4.1.1's one tie rule; its audio keeps the base part's picture→audio offset
//!   (two-source parts stay in sync);
//! - layers come from `effective()` for the part; a layer whose crop and place still equal
//!   the seeded values keeps the base plan's exact pixels, so an untouched import plans
//!   to the identical RenderPlan (and `render_sig`);
//! - text events and gain envelopes are carried on the output clock: every base frame a
//!   new part still shows keeps its events, cut pieces are dropped, contiguous pieces
//!   merge back under the original id;
//! - fades: dropped at joins that became continuous (RP rule 7), the reference's 144-sample linear
//!   click guard added at joins that became discontinuous.
//!
//! Nothing here is model math in the UI (R-CMD-2): the UI only reads the result.

use std::collections::BTreeMap;

use gmclip_model::clock::{cut_layout, mul_div_ties_earlier};
use gmclip_model::effective::effective;
use gmclip_model::entities::{Cut, Framing, Part};
use gmclip_model::{Doc, Flicks, Id, Ulid};
use gmclip_plan::Plan;
use gmclip_plan::scalars::{Id as PlanId, Range};
use gmclip_plan::types::{Fade, FadeShape, Rect};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Flicks per second (SPEC §4.1.1).
pub const FLICKS: i64 = 705_600_000;

/// A plan source as the model knows it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceMap {
    pub model: String,
    pub width: u32,
    pub height: u32,
}

/// A seeded layer: where it came from and the normalised values it was seeded with.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LayerSeed {
    pub part: usize,
    pub layer: usize,
    pub crop: [f64; 4],
    pub place: [f64; 3],
}

/// How an imported cut's model entities map onto its base plan (`<home>/imports/`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportMap {
    pub version: u32,
    pub cut: String,
    pub framing: String,
    /// plan source id → model source.
    pub sources: BTreeMap<String, SourceMap>,
    /// model part id → base part index.
    pub parts: BTreeMap<String, usize>,
    /// model layer id → seed.
    pub layers: BTreeMap<String, LayerSeed>,
}

/// An imported cut: its base plan and the map.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportedCut {
    pub base: Plan,
    pub map: ImportMap,
}

/// Which model entity each plan id came from (viewer gizmos commit through this).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanIndex {
    /// Per plan part: (plan part id, model part id, base part index, plan layer ids →
    /// model layer ids).
    pub parts: Vec<PartIndex>,
    /// Built caption cues: (plan event id, the words it shows) — model cuts only.
    pub cues: Vec<(String, Vec<gmclip_model::anchor::WordId>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PartIndex {
    pub plan_id: String,
    pub model: String,
    pub base: usize,
    pub layers: Vec<(String, String)>,
}

impl PlanIndex {
    pub fn model_layer(&self, plan_part: &str, plan_layer: &str) -> Option<(&str, &str)> {
        let p = self.parts.iter().find(|p| p.plan_id == plan_part)?;
        let l = p.layers.iter().find(|(pl, _)| pl == plan_layer)?;
        Some((p.model.as_str(), l.1.as_str()))
    }
}

pub fn ulid_id<E>(s: &str) -> AppResult<Id<E>> {
    Ulid::parse(s)
        .map(Id::new)
        .map_err(|_| AppError::invalid("not an id"))
}

/// `(fps num, fps den)` of the plan (den 1001 at an NTSC rate).
pub fn fps(plan: &Plan) -> (i64, i64) {
    let (n, d) = plan.output.fps.rate();
    (n as i64, d as i64)
}

/// Flicks per output frame (exact for every RenderPlan rate).
pub fn fpf(plan: &Plan) -> i64 {
    plan.output.fps.flicks_per_frame() as i64
}

/// Output frame index for a source time: nearest frame, ties to the earlier (§4.1.1).
pub fn frame_of(t: Flicks, fps: (i64, i64)) -> Option<i64> {
    mul_div_ties_earlier(
        t.0,
        i128::from(fps.0),
        i128::from(FLICKS) * i128::from(fps.1),
    )
}

/// The exact source time of frame `k`.
pub fn time_of(k: i64, fps: (i64, i64)) -> Flicks {
    Flicks((i128::from(k) * i128::from(FLICKS) * i128::from(fps.1) / i128::from(fps.0)) as i64)
}

/// 48 kHz samples per output frame (an integer for every integer rate; rounded down at
/// an NTSC one, whose frames carry that or one more).
pub fn samples_per_frame(plan: &Plan) -> u64 {
    let (n, d) = plan.output.fps.rate();
    48_000 * d / n
}

/// Follows `split_of` back to a part the map knows.
fn base_of(doc: &Doc, map: &ImportMap, p: &Part) -> Option<usize> {
    let mut cur = p.clone();
    for _ in 0..10_000 {
        if let Some(&i) = map.parts.get(&cur.id.0.to_string()) {
            return Some(i);
        }
        let prev = cur.lineage.split_of?;
        cur = doc.get(prev)?.clone();
    }
    None
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9
}

/// One new part's view of its base part, for carrying output-clock items.
struct Seg {
    /// Base part's `out` range.
    bs: u64,
    be: u64,
    /// New part shows base output frames `[bs + d, bs + d + n)` …
    d: i64,
    n: u64,
    /// … at new output frames `[o, o + n)`.
    o: u64,
}

/// Maps a base range `[a, b)` (in `unit`s per frame: 1 for frames, spf for samples) into
/// the new timeline's pieces, merged where contiguous.
fn carry(segs: &[Seg], a: u64, b: u64, unit: u64) -> Vec<(u64, u64)> {
    let mut pieces: Vec<(u64, u64)> = Vec::new();
    for s in segs {
        let shown_lo = (s.bs as i64 + s.d).max(s.bs as i64);
        let shown_hi = (s.bs as i64 + s.d + s.n as i64).min(s.be as i64);
        if shown_hi <= shown_lo {
            continue;
        }
        let lo = (a as i64).max(shown_lo * unit as i64);
        let hi = (b as i64).min(shown_hi * unit as i64);
        if hi <= lo {
            continue;
        }
        let shift = (s.o as i64 - s.bs as i64 - s.d) * unit as i64;
        pieces.push(((lo + shift) as u64, (hi + shift) as u64));
    }
    pieces.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for p in pieces {
        match out.last_mut() {
            Some(l) if l.1 == p.0 => l.1 = p.1,
            _ => out.push(p),
        }
    }
    out
}

fn piece_id(base: &PlanId, k: usize) -> PlanId {
    if k == 0 {
        return base.clone();
    }
    let mut s: String = base.as_str().chars().take(90).collect();
    s.push_str(&format!(".{}", k + 1));
    PlanId::new(s).unwrap_or_else(|_| base.clone())
}

/// Plans `imp`'s cut from the model. Returns the plan, which plan id is which model
/// entity, and whether anything differs from the base.
pub fn plan_cut(doc: &Doc, imp: &ImportedCut) -> AppResult<(Plan, PlanIndex, bool)> {
    let base = &imp.base;
    let map = &imp.map;
    let cut: Id<Cut> = ulid_id(&map.cut)?;
    let framing: Id<Framing> = ulid_id(&map.framing)?;
    if doc.get(cut).is_none() {
        return Err(AppError::not_found("this cut no longer exists"));
    }
    let fps = fps(base);
    let spf = samples_per_frame(base);
    let (cw, ch) = (
        base.output.canvas.w.get() as f64,
        base.output.canvas.h.get() as f64,
    );
    let mut plan = base.clone();
    plan.parts.clear();
    let mut index = PlanIndex::default();
    let mut segs: Vec<Seg> = Vec::new();
    let mut used: BTreeMap<usize, usize> = BTreeMap::new();
    let mut changed = false;
    let mut acc = 0u64;
    let layout = cut_layout(doc, cut);
    if layout.len() != base.parts.len() {
        changed = true;
    }
    for (pos, span) in layout.iter().enumerate() {
        let Some(p) = doc.get(span.part) else {
            changed = true;
            continue;
        };
        let Some(bi) = base_of(doc, map, p) else {
            return Err(AppError::new(
                "Unsupported",
                "a part that was not imported (added by hand) cannot be planned until the model planner lands (A18)",
            ));
        };
        let bp = base
            .parts
            .get(bi)
            .ok_or_else(|| AppError::internal("import map names a missing base part"))?;
        let first = frame_of(p.src_in, fps).ok_or_else(|| AppError::invalid("src_in"))?;
        let end = frame_of(p.src_out, fps).ok_or_else(|| AppError::invalid("src_out"))?;
        if end <= first || first < 0 {
            // A part with no whole frame shows nothing; the plan differs from the base.
            changed = true;
            continue;
        }
        let n = (end - first) as u64;
        let d = first - bp.picture.first_frame as i64;
        let audio_first = bp.audio.first_sample as i64 + d * spf as i64;
        if audio_first < 0 {
            return Err(AppError::invalid(
                "a trim reaches before the start of the audio source",
            ));
        }
        let k = used.entry(bi).or_insert(0);
        let plan_id = piece_id(&bp.id, *k);
        *k += 1;
        if pos != bi || d != 0 || n != bp.out.len() || plan_id != bp.id {
            changed = true;
        }
        let mut np = bp.clone();
        np.id = plan_id;
        np.out = Range([acc, acc + n]);
        np.picture.first_frame = first as u64;
        np.picture.expect_pts = None;
        np.audio.first_sample = audio_first as u64;
        if np.native.is_some() && (d != 0 || n != bp.out.len()) {
            np.native = None;
        }
        // Layers from the model's effective stack (part overrides), each traced to its
        // seed: directly, or (a split's b-side copies) through the seeded layer of the
        // same base part with the same order key.
        if !gmclip_model::effective::splits_of(doc, span.part, framing).is_empty() {
            return Err(AppError::new(
                "Unsupported",
                "visual splits plan with the model planner (A18), not the import planner",
            ));
        }
        let mut lmap = Vec::new();
        if let Some(stack) = effective(doc, span.part, framing, p.src_in) {
            let mut layers = Vec::with_capacity(stack.layers.len());
            let mut indices = Vec::with_capacity(stack.layers.len());
            for rl in &stack.layers {
                let lid = rl.id.0.to_string();
                let seed = map.layers.get(&lid).or_else(|| {
                    map.layers.iter().find_map(|(sid, sd)| {
                        let sl = doc.get(ulid_id::<gmclip_model::entities::Layer>(sid).ok()?)?;
                        (sd.part == bi && sl.order == rl.order).then_some(sd)
                    })
                });
                let Some(seed) = seed.filter(|s| s.part == bi) else {
                    return Err(AppError::new(
                        "Unsupported",
                        "a layer added by hand plans with the model planner (A18), not the import planner",
                    ));
                };
                let bl_idx = seed.layer;
                indices.push(bl_idx);
                let Some(bl) = bp.layers.get(bl_idx) else {
                    return Err(AppError::internal("import map names a missing base layer"));
                };
                let seed = Some(seed);
                let mut l = bl.clone();
                let v = &rl.values;
                let crop_same = seed.is_some_and(|s| {
                    close(s.crop[0], v.crop.x)
                        && close(s.crop[1], v.crop.y)
                        && close(s.crop[2], v.crop.w)
                        && close(s.crop[3], v.crop.h)
                });
                let place_same = seed.is_some_and(|s| {
                    close(s.place[0], v.place.x)
                        && close(s.place[1], v.place.y)
                        && close(s.place[2], v.place.w)
                });
                if !(crop_same && place_same) {
                    changed = true;
                    let src = l
                        .picture_source
                        .as_ref()
                        .unwrap_or(&np.picture.source)
                        .as_str()
                        .to_owned();
                    let dims = map
                        .sources
                        .get(&src)
                        .ok_or_else(|| AppError::internal("import map has no source dims"))?;
                    let (sw, sh) = (f64::from(dims.width), f64::from(dims.height));
                    if !crop_same {
                        l.crop = Rect {
                            x: v.crop.x * sw,
                            y: v.crop.y * sh,
                            w: v.crop.w * sw,
                            h: v.crop.h * sh,
                        };
                    }
                    let pw = v.place.w * cw;
                    l.place = Rect {
                        x: v.place.x * cw,
                        y: v.place.y * ch,
                        w: pw,
                        h: pw * l.crop.h / l.crop.w,
                    };
                }
                let vis = v.visible;
                if vis != bl.is_visible() {
                    changed = true;
                    l.visible = Some(vis);
                }
                if !close(v.opacity, bl.opacity()) {
                    changed = true;
                    l.opacity = Some(v.opacity);
                }
                if i64::from(v.z) != bl.z {
                    changed = true;
                    l.z = i64::from(v.z);
                }
                lmap.push((l.id.as_str().to_owned(), lid));
                layers.push(l);
            }
            // A removed, added or reordered layer changes the plan.
            if indices.iter().copied().ne(0..bp.layers.len()) {
                changed = true;
            }
            np.layers = layers;
        } else if !bp.layers.is_empty() {
            changed = true;
            np.layers.clear();
        }
        index.parts.push(PartIndex {
            plan_id: np.id.as_str().to_owned(),
            model: span.part.0.to_string(),
            base: bi,
            layers: lmap,
        });
        segs.push(Seg {
            bs: bp.out.start(),
            be: bp.out.end(),
            d,
            n,
            o: acc,
        });
        acc += n;
        plan.parts.push(np);
    }
    if plan.parts.is_empty() {
        return Err(AppError::invalid("the cut has no parts to show"));
    }
    if !changed {
        return Ok((base.clone(), index, false));
    }
    plan.output.frames = acc;
    // Fades at joins (RP rule 7).
    let linear = || {
        Some(Some(Fade {
            samples: 144,
            shape: FadeShape::Linear,
        }))
    };
    for j in 0..plan.parts.len() {
        let next = plan
            .parts
            .get(j + 1)
            .map(|q| (q.audio.source.clone(), q.audio.first_sample));
        let p = &mut plan.parts[j];
        let len = p.out.len() * spf;
        match next {
            Some((src, first)) if src == p.audio.source && first == p.audio.first_sample + len => {
                p.audio.fade_out = None;
                if let Some(q) = plan.parts.get_mut(j + 1) {
                    q.audio.fade_in = None;
                }
            }
            Some(_) => {
                if p.audio.fade_out().is_none() {
                    p.audio.fade_out = linear();
                }
                if let Some(q) = plan.parts.get_mut(j + 1)
                    && q.audio.fade_in().is_none()
                {
                    q.audio.fade_in = linear();
                }
            }
            None => {}
        }
    }
    // Text events on the output clock.
    if let Some(evs) = &base.text_events {
        let mut out = Vec::new();
        for e in evs {
            for (k, (a, b)) in carry(&segs, e.out.start(), e.out.end(), 1)
                .into_iter()
                .enumerate()
            {
                let mut ne = e.clone();
                ne.id = piece_id(&e.id, k);
                ne.out = Range([a, b]);
                out.push(ne);
            }
        }
        plan.text_events = Some(out);
    }
    // Gain envelopes on the output sample clock.
    if let Some(envs) = &base.audio.envelopes {
        let mut out = Vec::new();
        for e in envs {
            for (k, (a, b)) in carry(&segs, e.out_samples.start(), e.out_samples.end(), spf)
                .into_iter()
                .enumerate()
            {
                let mut ne = e.clone();
                ne.id = piece_id(&e.id, k);
                ne.out_samples = Range([a, b]);
                // RP: ramp_in + ramp_out ≤ length; scale both when a trim shortened it.
                let len = (b - a).max(2);
                let (ri, ro) = (ne.ramp_in_samples.max(1), ne.ramp_out_samples.max(1));
                if ri + ro > len {
                    let rin = (len * ri / (ri + ro)).clamp(1, len - 1);
                    ne.ramp_in_samples = rin;
                    ne.ramp_out_samples = len - rin;
                }
                out.push(ne);
            }
        }
        plan.audio.envelopes = Some(out);
    }
    // The reference render no longer matches.
    plan.checks = None;
    if let Some(m) = plan.meta.as_mut() {
        m.import_report
            .get_or_insert_with(Vec::new)
            .push("edited in gmclip (interim planner, until A18)".into());
    }
    let errs = gmclip_plan::check_plan(&plan);
    if let Some(e) = errs.first() {
        return Err(AppError::new(
            "Invalid",
            format!(
                "the edited cut does not plan: {} ({})",
                e.reason(),
                e.path()
            ),
        ));
    }
    Ok((plan, index, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_time_round_trip_at_30() {
        let f = (30, 1);
        for k in [0i64, 1, 29, 79_612, 150_000] {
            assert_eq!(frame_of(time_of(k, f), f), Some(k));
        }
        // A time half a frame past k quantises to k (ties to the earlier).
        let half = Flicks(time_of(10, f).0 + FLICKS / 60);
        assert_eq!(frame_of(half, f), Some(10));
    }

    #[test]
    fn carry_merges_contiguous_pieces_and_drops_cut_frames() {
        // Base: part A [0,100), part B [100,200). New: A trimmed by 10 at the head, B whole.
        let segs = vec![
            Seg {
                bs: 0,
                be: 100,
                d: 10,
                n: 90,
                o: 0,
            },
            Seg {
                bs: 100,
                be: 200,
                d: 0,
                n: 100,
                o: 90,
            },
        ];
        // An event over [5, 150): its first 5 frames were cut.
        assert_eq!(carry(&segs, 5, 150, 1), vec![(0, 140)]);
        // An event entirely inside the cut head disappears.
        assert!(carry(&segs, 0, 10, 1).is_empty());
        // Samples: unit 1600.
        assert_eq!(carry(&segs, 0, 200 * 1600, 1600), vec![(0, 190 * 1600)]);
    }
}
