//! Timeline essentials (backlog U.08): the audio lane (energy waveform and silence
//! ribbons from the source analysis, on the output clock), sweeps (pauses ≥ N s, fillers,
//! retakes — decision 0040)
//! previewed as a list and applied as one `sweep_apply`, the screen hold (`h`), and
//! reordering a part by dragging it.
//!
//! Sweeps reuse the holder's candidate finder (`gmclip_mcp::analysis::sweep_candidates`)
//! on the app's analysis host, so the app and agents cut the same spans.

use gmclip_core::planner::audio_source;
use gmclip_model::clock::cut_layout;
use gmclip_model::entities::{ContentHold, Cut, HoldAt, MediaRef, Part};
use gmclip_model::{Flicks, Id};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

fn fpf(core: &AppCore, project: &str, cut: &str) -> AppResult<i64> {
    Ok(crate::planner::fpf(&core.planned(project, cut)?.plan))
}

/// Where a source span of `media` plays on the output clock (frames), if it does.
fn out_frames(
    doc: &gmclip_model::Doc,
    cut: Id<Cut>,
    media: MediaRef,
    from: i64,
    to: i64,
    fpf: i64,
) -> Option<(i64, i64)> {
    layout(doc, cut).into_iter().find_map(|(p, start, _)| {
        (p.media == media && from >= p.src_in.0 && to <= p.src_out.0).then(|| {
            let a = (start + from - p.src_in.0) / fpf;
            let b = (start + to - p.src_in.0 + fpf - 1) / fpf;
            (a, b)
        })
    })
}

/// The audio lane: per part, energy (dB, max per output frame, ≤ 1500 points) and the
/// quiet regions, in output frames, and the energy onsets near its edges (trims snap to
/// them). The timeline draws from the `gm://data` columns (D.02) and asks this for the
/// onsets alone (`onsets_only`); the whole lane is its fallback where `gm://` cannot be
/// fetched.
pub fn timeline_audio(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    timeline_audio_with(core, project, cut, false)
}

/// [`timeline_audio`]; `onsets_only` leaves out the energy and quiet regions.
pub fn timeline_audio_with(
    core: &AppCore,
    project: &str,
    cut: &str,
    onsets_only: bool,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf(core, project, cut)?;
    let host = core.analysis.host();
    let keys = p.handle.analysis_keys();
    let blob = |owner: &str, kind: &str| -> Option<Vec<u8>> {
        let rev = keys
            .iter()
            .filter(|(o, k, _)| o == owner && k == kind)
            .map(|(_, _, r)| r.clone())
            .max()?;
        p.handle.analysis(owner, kind, &rev)
    };
    let mut parts = Vec::new();
    let mut have = false;
    for (pt, start, dur) in layout(&doc, cid) {
        let pt = &pt;
        let a = start / fpf;
        let n = (dur / fpf).max(1);
        let Some((src, off)) = audio_source(&doc, pt.media, pt.src_in.0) else {
            parts.push(json!({"out": [a, a + n], "db": [], "quiet": []}));
            continue;
        };
        let owner = src.0.to_string();
        let (from, to) = (Flicks(pt.src_in.0 - off), Flicks(pt.src_out.0 - off));
        let mut db: Vec<Value> = Vec::new();
        if onsets_only {
            have |= keys
                .iter()
                .any(|(o, k, _)| *o == owner && k == "energy_10ms");
        } else if let Some(b) = blob(&owner, "energy_10ms")
            && let Ok(v) = host.blob_summary("energy_10ms", &b, from, to, (n as usize).min(1500))
        {
            have = true;
            db = v["points"]
                .as_array()
                .map(|ps| ps.iter().map(|x| x[1].clone()).collect())
                .unwrap_or_default();
        }
        let mut quiet = Vec::new();
        if !onsets_only
            && let Some(b) = blob(&owner, "silences_40ms")
            && let Ok(v) = host.blob_summary("silences_40ms", &b, from, to, 2000)
        {
            for r in v["regions_s"].as_array().into_iter().flatten() {
                let (s, e) = (r[0].as_f64().unwrap_or(0.0), r[1].as_f64().unwrap_or(0.0));
                let fs = ((s * FLICKS as f64) as i64 + off).max(pt.src_in.0);
                let fe = ((e * FLICKS as f64) as i64 + off).min(pt.src_out.0);
                if fe > fs {
                    quiet.push(json!([
                        a + (fs - pt.src_in.0) / fpf,
                        a + (fe - pt.src_in.0) / fpf
                    ]));
                }
            }
        }
        // energy onsets near the part's edges (trims snap to them, SPEC §5.3)
        let mut ons: Vec<i64> = Vec::new();
        if let Some(b) = blob(&owner, "energy_10ms") {
            let w = ONSET_WINDOW_S * FLICKS;
            let mut wins = vec![
                (pt.src_in.0 - w, pt.src_in.0 + w),
                (pt.src_out.0 - w, pt.src_out.0 + w),
            ];
            if wins[1].0 <= wins[0].1 {
                wins = vec![(wins[0].0, wins[1].1)];
            }
            for (wa, wb) in wins {
                let (fa, fb) = (Flicks((wa - off).max(0)), Flicks((wb - off).max(0)));
                if fb.0 <= fa.0 {
                    continue;
                }
                let hops = ((fb.0 - fa.0) / (FLICKS / 100)).max(1) as usize + 1;
                let Ok(v) = host.blob_summary("energy_10ms", &b, fa, fb, hops) else {
                    continue;
                };
                if v["points_per_entry"].as_u64() != Some(1) {
                    continue;
                }
                let pts = v["points"].as_array().cloned().unwrap_or_default();
                let db: Vec<f32> = pts
                    .iter()
                    .map(|x| x[1].as_f64().unwrap_or(-90.0) as f32)
                    .collect();
                for k in onsets(&db) {
                    let t = pts[k][0].as_f64().unwrap_or(0.0);
                    let src = (t * FLICKS as f64).round() as i64 + off;
                    ons.push(a + (src - pt.src_in.0).div_euclid(fpf));
                }
            }
            ons.sort_unstable();
            ons.dedup();
        }
        parts.push(json!({"out": [a, a + n], "db": db, "quiet": quiet, "onsets": ons}));
    }
    Ok(json!({"parts": parts, "analysed": have}))
}

/// A rise of at least this much (dB) …
pub const ONSET_DB: f32 = 12.0;
/// … within this many 10 ms hops (40 ms) is an energy onset (SPEC §5.3).
pub const ONSET_HOPS: usize = 4;
/// After an onset, the next one is at least this many hops later (100 ms).
const ONSET_GAP_HOPS: usize = 10;
/// Onsets are looked for this many seconds either side of a part's edges.
const ONSET_WINDOW_S: i64 = 6;

/// Energy onsets in a 10 ms dB series: the hop where a rise of ≥ [`ONSET_DB`] within
/// [`ONSET_HOPS`] hops starts (a cut there keeps the whole attack), one per rise.
pub fn onsets(db: &[f32]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < db.len() {
        let end = (i + ONSET_HOPS).min(db.len() - 1);
        let (mut peak, mut at) = (db[i], i);
        for (k, v) in db.iter().enumerate().take(end + 1).skip(i + 1) {
            if *v > peak {
                peak = *v;
                at = k;
            }
        }
        if peak - db[i] >= ONSET_DB {
            // the last flat hop before the level starts to climb
            while i + 1 < at && db[i + 1] - db[i] < 1.0 {
                i += 1;
            }
            out.push(i);
            // past the top of this rise, then a short refractory gap
            let mut j = at;
            while j + 1 < db.len() && db[j + 1] > db[j] {
                j += 1;
            }
            i = j.max(i + ONSET_GAP_HOPS);
        } else {
            i += 1;
        }
    }
    out
}

fn preview(
    core: &AppCore,
    project: &str,
    cut: &str,
    kind: &str,
    min_s: f64,
    keep_s: f64,
) -> AppResult<std::sync::Arc<gmclip_mcp::analysis::SweepPreviewRec>> {
    let sweep = gmclip_core::commands::analysis::SweepKind::parse(kind)
        .ok_or_else(|| AppError::invalid("kind is pauses, fillers or retakes"))?;
    if !(0.15..=5.0).contains(&min_s) || !(0.0..=1.0).contains(&keep_s) || keep_s >= min_s {
        return Err(AppError::invalid(
            "pauses of 0.15–5 s, keeping 0–1 s (less than the pause)",
        ));
    }
    let p = core.project(project)?;
    let cid = ulid_id::<Cut>(cut)?;
    let host = core.analysis.host();
    gmclip_mcp::analysis::sweep_preview_make(
        host.as_ref(),
        &p.handle,
        project,
        cid,
        sweep,
        min_s,
        keep_s,
    )
    .map_err(AppError::from)
}

/// The candidates as the sweeps panel lists them (output frames for seeking and the
/// timeline's preview ribbons), kept as a preview its `preview_id` applies (A2.06).
pub fn sweep_preview(
    core: &AppCore,
    project: &str,
    cut: &str,
    kind: &str,
    min_s: f64,
    keep_s: f64,
) -> AppResult<Value> {
    let c = preview(core, project, cut, kind, min_s, keep_s)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf(core, project, cut)?;
    let rows: Vec<Value> = c
        .cuts
        .iter()
        .enumerate()
        .map(|(i, (m, f, t, why))| {
            let out = out_frames(&doc, cid, *m, f.0, t.0, fpf);
            json!({"i": i, "out": out.map(|(a, b)| json!([a, b])), "seconds": ((t.0 - f.0) as f64 / FLICKS as f64 * 1000.0).round() / 1000.0,
                   "reason": why.chars().take(80).collect::<String>()})
        })
        .collect();
    Ok(
        json!({"preview_id": c.id, "kind": kind, "cuts": rows, "skipped": c.skipped.len(), "removes_s": (c.removes_s() * 100.0).round() / 100.0,
              "revision": c.revision, "as_of": c.as_of, "parts_without_audio": c.unsupported}),
    )
}

/// Applies the listed candidates (`take`: their indices) of a preview, exactly, as one
/// `sweep_apply`: `Conflict` when the cut changed since the preview, `StaleAnalysis`
/// when its analysis did.
pub fn sweep_apply(
    core: &AppCore,
    project: &str,
    cut: &str,
    preview_id: &str,
    take: &[usize],
) -> AppResult<Value> {
    if take.is_empty() {
        return Err(AppError::invalid("nothing chosen to cut"));
    }
    let p = core.project(project)?;
    let host = core.analysis.host();
    // the human's, or the agent's whose app command this is (decision 0050)
    let caller = crate::core::acting_caller();
    let base = (caller.kind == gmclip_proto::CallerKind::Agent).then(|| gmclip_proto::Base::AsOf {
        as_of: p.handle.snapshot().doc.rev(),
    });
    if gmclip_mcp::analysis::sweep_preview_get(preview_id)
        .is_some_and(|r| ulid_id::<Cut>(cut).ok() != Some(r.cut))
    {
        return Err(AppError::invalid("that preview is of another cut"));
    }
    let ok = gmclip_mcp::analysis::sweep_preview_apply(
        host.as_ref(),
        &p.handle,
        project,
        preview_id,
        Some(take),
        caller,
        base,
        None,
    )
    .map_err(AppError::from)?;
    Ok(json!({"applied": take.len(), "output": ok.output}))
}

/// `h`: holds the screen on the frame the playhead shows, from there to the end of the
/// sentence being said (`hold_screen_here`, one transaction); on a held part, removes
/// the hold.
pub fn hold_toggle(core: &AppCore, project: &str, cut: &str, frame: u64) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf(core, project, cut)?;
    let t = frame as i64 * fpf;
    let (pid, src) = gmclip_model::clock::cut_to_src(&doc, cid, Flicks(t))
        .ok_or_else(|| AppError::invalid("the playhead is not in the cut"))?;
    let pt: &Part = doc.get(pid).ok_or_else(|| AppError::internal("part"))?;
    // A hold over this time: remove it.
    for h in doc
        .children::<ContentHold>(cid.entity())
        .filter_map(|h| doc.get(h))
    {
        let a = gmclip_model::clock::anchor_src(&doc, &h.from);
        let b = gmclip_model::clock::anchor_src(&doc, &h.to);
        if let (Some(a), Some(b)) = (a, b)
            && a <= src
            && src < b
            && a >= pt.src_in.min(a)
        {
            return core
                .call(
                    project,
                    "cut_edit",
                    json!({"cut": cut, "ops": [{"op": "content_hold_delete", "hold": h.id.0.to_string()}]}),
                )
                .map(|_| json!({"held": false}));
        }
    }
    // §5.3 `h`: core's `hold_screen_here` (A2.07) holds every layer but the camera from
    // this frame to the end of the sentence being said, splitting the part there
    core.call(
        project,
        "hold_screen_here",
        json!({"cut": cut, "at_frame": frame}),
    )
    .map(|r| json!({"held": true, "until": r["output"]["until"]}))
}

/// The hold frame for media time `t` (the picture's own source and frame: a sync group's
/// picture member at its offset), and the clock raw times on this media use.
fn hold_frame(doc: &gmclip_model::Doc, media: MediaRef, t: i64) -> AppResult<(HoldAt, Value)> {
    let src = Flicks(t);
    // the picture's own source and time (a sync group's picture member, at its offset)
    let (s, pic_t, clock) = match media {
        MediaRef::Source(s) => (s, src.0, json!({"source": s.0.to_string()})),
        MediaRef::Group(g) => {
            let grp = doc
                .get::<gmclip_model::entities::SyncGroup>(g)
                .ok_or_else(|| AppError::internal("group"))?;
            let m = grp
                .picture
                .and_then(|m| doc.get::<gmclip_model::entities::SyncMember>(m))
                .ok_or_else(|| AppError::invalid("the group has no picture member"))?;
            let off = doc
                .children::<gmclip_model::entities::SyncSegment>(m.id.entity())
                .filter_map(|x| doc.get(x))
                .find(|x| x.src_range.start <= src && src < x.src_range.end)
                .map_or(0, |x| x.offset.0);
            (m.source, src.0 - off, json!({"group": g.0.to_string()}))
        }
    };
    let rate = doc
        .get(s)
        .and_then(|so| so.probe.tracks.iter().find_map(|t| t.fps))
        .map_or(30.0, |r| r.num as f64 / f64::from(r.den.max(1)));
    let at = HoldAt::Frame(gmclip_model::anchor::FrameRef {
        source: s,
        frame_index: (pic_t as f64 / FLICKS as f64 * rate).round() as i64,
    });
    Ok((at, clock))
}

/// The reference's `hold_at`: the whole take's screen held on the stream frame at `at_s` (the media
/// clock: a typed time, or the stream viewer's), which may lie outside the take.
pub fn hold_part_at(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    at_s: f64,
) -> AppResult<Value> {
    if !at_s.is_finite() || at_s < 0.0 {
        return Err(AppError::invalid("a time in the stream, in seconds"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let pid: Id<Part> = ulid_id(part)?;
    let pt: &Part = doc
        .get(pid)
        .ok_or_else(|| AppError::not_found("that part is not in the cut any more"))?;
    let (at, clock) = hold_frame(&doc, pt.media, (at_s * FLICKS as f64).round() as i64)?;
    let raw = |t: i64, why: &str| {
        let mut time = clock.clone();
        time["clock"] = json!("src");
        time["t"] = json!(t);
        json!({"raw": {"time": time, "reason": why}})
    };
    core.call(
        project,
        "cut_edit",
        json!({"cut": cut, "ops": [{"op": "content_hold_add", "from": raw(pt.src_in.0, "the take's start"),
               "to": raw(pt.src_out.0, "the take's end"), "at": at,
               "roles": [{"region": "screen"}, "detail"]}]}),
    )
    .map(|_| json!({"held": true}))
}

/// Moves a part to position `index` (0-based, in the new order) as one `part_reorder`.
pub fn reorder_to(
    core: &AppCore,
    project: &str,
    cut: &str,
    part: &str,
    index: usize,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let me: Id<Part> = ulid_id(part)?;
    let order: Vec<Id<Part>> = cut_layout(&doc, cid)
        .iter()
        .map(|s| s.part)
        .filter(|x| *x != me)
        .collect();
    let index = index.min(order.len());
    let mut args = json!({"part": part});
    if index > 0 {
        args["after"] = json!(order[index - 1].0.to_string());
    }
    core.call(project, "part_reorder", args)
}

/// Moves several parts (a multi-selection or a group, decision 0037) to position `index`
/// among the parts not moving, back to back in their current order: one `cut_edit`, one
/// undo step.
pub fn parts_move_to(
    core: &AppCore,
    project: &str,
    cut: &str,
    parts: &[String],
    index: usize,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let moving: Vec<Id<Part>> = parts.iter().map(|x| ulid_id(x)).collect::<AppResult<_>>()?;
    let all: Vec<Id<Part>> = cut_layout(&doc, cid).iter().map(|s| s.part).collect();
    let block: Vec<Id<Part>> = all.iter().copied().filter(|x| moving.contains(x)).collect();
    if block.is_empty() {
        return Err(AppError::invalid("none of these parts is in the cut"));
    }
    let rest: Vec<Id<Part>> = all
        .iter()
        .copied()
        .filter(|x| !moving.contains(x))
        .collect();
    let index = index.min(rest.len());
    let mut after = index.checked_sub(1).map(|i| rest[i]);
    let mut ops = Vec::new();
    for pt in block {
        let mut op = json!({"op": "part_reorder", "part": pt.0.to_string()});
        if let Some(a) = after {
            op["after"] = json!(a.0.to_string());
        }
        ops.push(op);
        after = Some(pt);
    }
    core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))
}

/// A cut's parts end to end with their start and length on its clock (flicks), so
/// waveforms, words and onsets follow the parts.
pub(crate) fn layout(
    doc: &gmclip_model::Doc,
    cut: Id<Cut>,
) -> Vec<(gmclip_model::entities::Part, i64, i64)> {
    cut_layout(doc, cut)
        .into_iter()
        .filter_map(|sp| Some((doc.get(sp.part)?.clone(), sp.start.0, sp.dur.0)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onsets_are_sharp_rises_only() {
        // silence, a 30 dB attack over 20 ms, a held level, a slow 20 dB swell over
        // 200 ms (no onset), then a second attack after a dip
        let mut db = vec![-60.0f32; 50];
        db.extend([-45.0, -30.0, -30.0, -31.0, -30.0]);
        db.extend(vec![-30.0; 20]);
        db.extend((0..20).map(|k| -50.0 + k as f32));
        db.extend(vec![-55.0; 10]);
        db.extend([-40.0, -38.0, -36.0, -35.0]);
        let o = onsets(&db);
        assert_eq!(o, vec![49, 104], "{o:?}");
        // 11.9 dB within 40 ms is not one; 12 dB over 50 ms is not one either
        assert!(onsets(&[-50.0, -45.0, -40.0, -38.1, -38.1]).is_empty());
        assert!(onsets(&[-50.0, -48.0, -46.0, -44.0, -42.0, -38.0]).is_empty());
        assert_eq!(onsets(&[-50.0, -48.0, -46.0, -44.0, -38.0]), vec![0]);
        assert!(onsets(&[]).is_empty());
    }
}
