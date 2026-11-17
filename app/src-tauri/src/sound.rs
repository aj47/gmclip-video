//! Story-reel parity for sound and the camera (backlog #4; AJ's reel plans use them):
//! - **gain over a span** (`program_gain_envelopes`: +10 dB lifted a generated voice in
//!   h79) as core gain segments with 50 ms ramps, between two output frames;
//! - **part gain** (`part_set_gain`), and the level of a recording's system audio under
//!   the voice (`part_set_under`, `sync_under_set`; decision 0051);
//! - **hide the camera over a span** (the source camera dropped out: colour bars or a
//!   frozen frame) as core `hide_roles_add` on the cam role. A hide covers whole parts,
//!   so the parts are split at its edges first, in the same transaction.
//!
//! Spans are anchored on the source clock (raw source/group times), so they follow the
//! words when the cut is re-trimmed.

use gmclip_model::Flicks;
use gmclip_model::clock::{anchor_src, cut_layout, cut_to_src, src_to_cut};
use gmclip_model::entities::{Cut, GainSegment, HideRoles, MediaRef, Part};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// Flicks per output frame (exact at every rate, NTSC included).
fn fpf_of(core: &AppCore, project: &str, cut: &str) -> AppResult<i64> {
    Ok(crate::planner::fpf(&core.planned(project, cut)?.plan))
}

/// A raw source-clock anchor at `t` on `media`'s clock.
pub(crate) fn raw(media: MediaRef, t: Flicks, why: &str) -> Value {
    match media {
        MediaRef::Source(s) => {
            json!({"raw": {"time": {"clock": "src", "source": s.0.to_string(), "t": t.0}, "reason": why}})
        }
        MediaRef::Group(g) => {
            json!({"raw": {"time": {"clock": "src", "group": g.0.to_string(), "t": t.0}, "reason": why}})
        }
    }
}

/// The source point under output frame `f` (the end of the cut maps to its last part's
/// out edge).
pub(crate) fn at_frame(
    doc: &gmclip_model::Doc,
    cid: gmclip_model::Id<Cut>,
    f: i64,
    fpf: i64,
) -> AppResult<(gmclip_model::Id<Part>, Flicks, MediaRef)> {
    let t = Flicks(f * fpf.max(1));
    let (pid, src) = cut_to_src(doc, cid, t)
        .or_else(|| {
            // the very end of the cut
            let last = cut_layout(doc, cid).last().copied()?;
            let p: &Part = doc.get(last.part)?;
            Some((last.part, p.src_out))
        })
        .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
    let p: &Part = doc.get(pid).ok_or_else(|| AppError::internal("part"))?;
    Ok((pid, src, p.media))
}

/// The member of a part's sync group whose own sound can play under the part's audio
/// (decision 0051): the first, other than the group's audio, with an audio track.
fn under_member<'a>(
    doc: &'a gmclip_model::Doc,
    pt: &Part,
) -> Option<&'a gmclip_model::entities::SyncMember> {
    let MediaRef::Group(g) = pt.media else {
        return None;
    };
    let group = doc.get(g)?;
    let audio = pt.audio_from.or(group.master_audio);
    doc.children::<gmclip_model::entities::SyncMember>(g.entity())
        .filter_map(|m| doc.get(m))
        .filter(|m| Some(m.id) != audio)
        .find(|m| {
            doc.get(m.source)
                .is_some_and(|s| !s.probe.audio_tracks.is_empty())
        })
}

/// Gain spans, hides and part gains for the panel (output frames).
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf_of(core, project, cut)?;
    let layout = cut_layout(&doc, cid);
    // a source time → output frame through the first part that plays it
    let to_out = |t: Option<Flicks>| -> Option<i64> {
        let t = t?;
        layout.iter().find_map(|sp| {
            let pt: &Part = doc.get(sp.part)?;
            (pt.src_in <= t && t <= pt.src_out)
                .then(|| src_to_cut(&doc, sp.part, t))
                .flatten()
                .map(|c| (c.0 + fpf / 2) / fpf)
        })
    };
    let gains: Vec<Value> = doc
        .children::<GainSegment>(cid.entity())
        .filter_map(|g| doc.get(g))
        .map(|g| {
            json!({"id": g.id.0.to_string(), "gain_db": g.gain_db,
                   "from": to_out(anchor_src(&doc, &g.start)), "to": to_out(anchor_src(&doc, &g.end))})
        })
        .collect();
    let hides: Vec<Value> = doc
        .children::<HideRoles>(cid.entity())
        .filter_map(|h| doc.get(h))
        .map(|h| {
            json!({"id": h.id.0.to_string(), "roles": h.roles,
                   "from": to_out(anchor_src(&doc, &h.from)), "to": to_out(anchor_src(&doc, &h.to))})
        })
        .collect();
    let quiet: Vec<Value> = doc
        .children::<gmclip_model::entities::CaptionDoc>(cid.entity())
        .find_map(|c| doc.get(c))
        .map(|c| {
            c.no_caption_ranges
                .iter()
                .map(|(k, r)| json!({"id": k, "from": to_out(anchor_src(&doc, &r.from)), "to": to_out(anchor_src(&doc, &r.to))}))
                .collect()
        })
        .unwrap_or_default();
    let parts: Vec<Value> = layout
        .iter()
        .filter_map(|sp| doc.get::<Part>(sp.part))
        .map(|pt| {
            let mut v = json!({"id": pt.id.0.to_string(), "gain_db": pt.gain_db, "speed": pt.speed, "ramp": pt.ramp});
            // decision 0051: the group's other sound (a recording's system audio) and
            // how loud it plays under this part
            if let Some(m) = under_member(&doc, pt) {
                v["under"] = json!({"member": m.id.0.to_string(), "member_db": m.under_db,
                                    "part_db": pt.under_db});
            }
            v
        })
        .collect();
    // decision 0051: music and sound effects over the cut
    let sounds: Vec<Value> = doc
        .children::<gmclip_model::entities::AudioOverlay>(cid.entity())
        .filter_map(|o| doc.get(o))
        .map(|o| {
            let name = match &o.media {
                gmclip_model::entities::AssetOrSpan::Asset(a) => doc
                    .get(*a)
                    .map_or_else(|| "sound".to_owned(), |x| x.name.clone()),
                gmclip_model::entities::AssetOrSpan::Span { .. } => "a stretch of a video's sound".to_owned(),
            };
            json!({"id": o.id.0.to_string(), "name": name, "gain_db": o.gain_db,
                   "duck": o.duck_under_speech.is_some(),
                   "from": to_out(anchor_src(&doc, &o.start)), "to": to_out(anchor_src(&doc, &o.end))})
        })
        .collect();
    Ok(
        json!({"gains": gains, "hides": hides, "parts": parts, "no_captions": quiet, "sounds": sounds}),
    )
}

/// +`gain_db` between output frames `[from, to)`, 50 ms ramps.
pub fn gain_span(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: i64,
    to: i64,
    gain_db: f64,
) -> AppResult<Value> {
    if !(-40.0..=24.0).contains(&gain_db) || to <= from {
        return Err(AppError::invalid("a span and a gain from −40 to +24 dB"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf_of(core, project, cut)?;
    let (_, a, ma) = at_frame(&doc, cid, from, fpf)?;
    let (_, b, mb) = at_frame(&doc, cid, to, fpf)?;
    core.call(
        project,
        "gain_segment_add",
        json!({"cut": cut, "start": raw(ma, a, "gain from here"), "end": raw(mb, b, "gain to here"),
               "gain_db": gain_db, "ramp_in_ms": 50, "ramp_out_ms": 50}),
    )
}

/// The largest music file stored (`asset_add`'s bound for sound).
const MAX_SOUND: u64 = 128 << 20;

/// Lays a music file (M4A or WAV) over the whole cut (decision 0051): stored as an asset,
/// then `audio_overlay_add` from the first part's start to the last part's end at
/// −18 dB, ducked 12 dB under the voice.
pub fn music_add(core: &AppCore, project: &str, cut: &str, path: &str) -> AppResult<Value> {
    let p = std::path::Path::new(path);
    let media_type = match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        _ => return Err(AppError::invalid("an M4A or WAV file")),
    };
    let bytes = std::fs::read(p).map_err(|_| AppError::not_found("that file is gone"))?;
    if bytes.len() as u64 > MAX_SOUND {
        return Err(AppError::invalid("a music file is at most 128 MB"));
    }
    // it must hold a sound gmclip reads
    let mut src = gmclip_media::demux::io::SliceSource(&bytes);
    let d = gmclip_media::demux::index(&mut src)
        .map_err(|_| AppError::invalid("that file is not a sound gmclip reads"))?;
    if !d
        .tracks
        .iter()
        .any(|t| t.kind == gmclip_media::demux::TrackKind::Audio)
    {
        return Err(AppError::invalid("that file has no sound"));
    }
    let sha = gmclip_core::profile::style::sha256_hex(&bytes);
    let dest = core
        .fonts()
        .asset_file_typed(&sha, media_type)
        .ok_or_else(|| AppError::internal("no assets folder"))?;
    if !dest.is_file() {
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| {
                AppError::internal(format!("making the assets folder: {}", e.kind()))
            })?;
        }
        crate::core::write_atomic(&dest, &bytes)
            .map_err(|e| AppError::internal(format!("storing the music: {}", e.kind())))?;
    }
    let name: String = p
        .file_name()
        .map(|n| {
            n.to_string_lossy()
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect()
        })
        .unwrap_or_else(|| "music".to_owned());
    let asset = core.call(
        project,
        "asset_add",
        json!({"name": name, "sha256": sha, "size": bytes.len(), "media_type": media_type}),
    )?;
    let doc = core.project(project)?.handle.snapshot().doc.clone();
    let layout = cut_layout(&doc, ulid_id::<Cut>(cut)?);
    let (Some(first), Some(last)) = (layout.first(), layout.last()) else {
        return Err(AppError::invalid("the cut has no parts"));
    };
    core.call(
        project,
        "audio_overlay_add",
        json!({"cut": cut, "media": {"asset": asset["output"]["id"]},
               "start": {"part_start": {"part": first.part.0.to_string()}},
               "end": {"part_end": {"part": last.part.0.to_string()}},
               "duck": {"depth_db": 12, "attack_ms": 200, "release_ms": 600}}),
    )
}

pub fn gain_delete(core: &AppCore, project: &str, segment: &str) -> AppResult<Value> {
    core.call(project, "gain_segment_delete", json!({"segment": segment}))
}

pub fn part_gain(core: &AppCore, project: &str, part: &str, gain_db: f64) -> AppResult<Value> {
    core.call(
        project,
        "part_set_gain",
        json!({"part": part, "gain_db": gain_db}),
    )
}

/// Hides the camera between output frames `[from, to)`: splits the parts at both edges
/// (a hide covers whole parts) and adds the hide, one transaction.
pub fn hide_cam(core: &AppCore, project: &str, cut: &str, from: i64, to: i64) -> AppResult<Value> {
    if to <= from {
        return Err(AppError::invalid("a span"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf_of(core, project, cut)?;
    let (pa, a, ma) = at_frame(&doc, cid, from, fpf)?;
    let (pb, b, mb) = at_frame(&doc, cid, to, fpf)?;
    let mut ops = Vec::new();
    let split = |pid: gmclip_model::Id<Part>, t: Flicks| -> Option<Value> {
        let pt: &Part = doc.get(pid)?;
        (t.0 - pt.src_in.0 >= fpf && pt.src_out.0 - t.0 >= fpf)
            .then(|| json!({"op": "part_split", "part": pid.0.to_string(), "at": t.0}))
    };
    // the later split first: splitting the earlier part does not move it
    ops.extend(split(pb, b));
    if pa != pb || a != b {
        ops.extend(split(pa, a));
    }
    ops.push(
        json!({"op": "hide_roles_add", "from": raw(ma, a, "camera hidden from here"),
                    "to": raw(mb, b, "to here"), "roles": [{"region": "cam"}]}),
    );
    core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}))
}

pub fn hide_delete(core: &AppCore, project: &str, cut: &str, hide: &str) -> AppResult<Value> {
    core.call(
        project,
        "cut_edit",
        json!({"cut": cut, "ops": [{"op": "hide_roles_delete", "hide": hide}]}),
    )
}

/// No captions between output frames `[from, to)` (reference: none over a full-screen clip
/// whose own text already carries the words, AGENTS.md rule 4).
pub fn no_captions(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: i64,
    to: i64,
) -> AppResult<Value> {
    if to <= from {
        return Err(AppError::invalid("a span"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let fpf = fpf_of(core, project, cut)?;
    let (_, a, ma) = at_frame(&doc, cid, from, fpf)?;
    let (_, b, mb) = at_frame(&doc, cid, to, fpf)?;
    core.call(
        project,
        "caption_set",
        json!({"cut": cut, "no_caption_add": [{"from": raw(ma, a, "no captions from here"), "to": raw(mb, b, "to here")}]}),
    )
}

pub fn no_captions_delete(core: &AppCore, project: &str, cut: &str, id: &str) -> AppResult<Value> {
    core.call(
        project,
        "caption_set",
        json!({"cut": cut, "no_caption_remove": [id]}),
    )
}
