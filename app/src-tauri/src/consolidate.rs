//! Consolidate (L.06): a project folder that travels — the project's database plus only
//! the stretches of its recordings that its cuts (and the topics asked for) use, with 5 s
//! handles, re-wrapped losslessly by our own MP4 writer. It opens on another Mac and
//! renders the same frames.
//!
//! - **Media**: for each recording, the spans the chosen cuts' parts (and their moments,
//!   and the chosen topics) cover, ± the handles, merged; [`gmclip_media::mux::trim`]
//!   copies them on whole closed GOPs (H.264 + 48 kHz AAC MP4: every sample byte-identical).
//!   A file it cannot cut (another codec or container, an open GOP, variable rate, other
//!   audio rates) is copied whole — still lossless, only bigger — and the manifest says
//!   why. Re-encoding with the export encoder is not done here.
//! - **Anchors**: every time stays on its recording's own clock, so nothing a person or an
//!   agent anchored moves. A trimmed recording becomes a sync group (SPEC §4.3) whose
//!   picture and audio members map each kept stretch onto the new file
//!   (`member time = group time − offset`, one segment per stretch); everything that named
//!   the source as media (parts, moments, topics, raw times, transcripts and other
//!   analysis, the active ASR) names the group instead, frame anchors are moved to the new
//!   file's frames, and a recording that was already a sync group keeps its group with
//!   its members' segments rewritten.
//! - **The folder**: `<name>.gmclip/` with `owner.json` (a new project id), `project.sqlite`
//!   (the document as one checkpoint — its edit history stays behind — and the analysis),
//!   `media/` (paths in the database are relative to the folder), the home's
//!   `framing.json` and `chat.json`, and `consolidated.json` (what was kept and why).
//!   [`open_copy`] adopts such a folder on any Mac: the database goes to the app data with
//!   absolute media paths, the folder becomes the project's home.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_model::entities::{
    Cut, CutKind, MediaRef, MemberRole, Moment, Part, Source, SrcRange, SyncGroup, SyncMember,
    SyncMethod, SyncSegment, Topic,
};
use gmclip_model::{
    Author, Doc, EntityId, EntityKind, FieldValue, Flicks, Id, OrderKey, Ulid, cbor,
};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};

/// What to consolidate.
#[derive(Clone, Debug)]
pub struct Options {
    /// The cuts (default: every cut but the stream edit).
    pub cuts: Option<Vec<String>>,
    /// Topics whose spans are kept too.
    pub topics: Vec<String>,
    /// Seconds kept either side of every span.
    pub handles_s: f64,
    /// The folder the project folder is made in.
    pub dest: PathBuf,
    /// The new project's name (default: the project's, "(consolidated)").
    pub name: Option<String>,
}

fn ierr(what: &str, e: impl std::fmt::Display) -> AppError {
    AppError::internal(format!("{what}: {e}"))
}

fn owner(m: &MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// Merges `[a, b)` spans (flicks) after widening them by `h`, clamped at 0.
fn merge(mut v: Vec<(i64, i64)>, h: i64) -> Vec<(i64, i64)> {
    for x in &mut v {
        *x = ((x.0 - h).max(0), x.1 + h);
    }
    v.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for r in v {
        match out.last_mut() {
            Some(l) if r.0 <= l.1 => l.1 = l.1.max(r.1),
            _ => out.push(r),
        }
    }
    out
}

/// Segments of a member, sorted.
fn segments(doc: &Doc, m: Id<SyncMember>) -> Vec<SyncSegment> {
    let mut s: Vec<SyncSegment> = doc
        .children::<SyncSegment>(m.entity())
        .filter_map(|id| doc.get(id).cloned())
        .collect();
    s.sort_by_key(|x| x.src_range.start);
    s
}

/// The `i`-th of a fresh sequence of order keys.
fn order(i: usize) -> OrderKey {
    OrderKey::sequence(None, i + 1)
        .ok()
        .and_then(|v| v.into_iter().last())
        .unwrap_or_else(OrderKey::first)
}

fn new_ulid() -> Ulid {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    Ulid::from_parts(
        ms & ((1 << 48) - 1),
        u128::from_le_bytes(b) & ((1u128 << 80) - 1),
    )
}

/// Where a source's picture or sound went.
#[derive(Clone, Debug, PartialEq)]
enum Moved {
    /// Copied stretches `(src start, src end, offset)` in flicks: new file time = src − offset.
    Pieces(Vec<(i64, i64, i64)>),
    /// Copied whole: the same clock.
    Same,
}

/// One source's new files, as the document rewrite needs them.
#[derive(Clone, Debug, Default)]
struct Mapped {
    video: Option<Moved>,
    /// `(input frame0, output frame0, frames)` per video piece, for frame anchors.
    frames: Vec<(i64, i64, u64)>,
    /// The sound, and the source that now holds it (the original id, or a new one when
    /// the picture keeps the original's).
    audio: Option<(Moved, Id<Source>)>,
}

/// A file written into the folder.
#[derive(Clone, Debug)]
struct OutFile {
    rel: String,
    probe: gmclip_model::entities::Probe,
    sha: String,
}

fn out_file(
    home: &Path,
    rel: &str,
    probe: gmclip_model::entities::Probe,
) -> AppResult<(OutFile, u64)> {
    let (sha, size) =
        gmclip_ml::sha256::file_hex(&home.join(rel)).map_err(|_| AppError::internal("hashing"))?;
    Ok((
        OutFile {
            rel: rel.to_owned(),
            probe,
            sha,
        },
        size,
    ))
}

/// The project as a consolidated folder in `o.dest`.
pub fn consolidate(core: &Arc<AppCore>, project: &str, o: &Options) -> AppResult<Value> {
    let t0 = std::time::Instant::now();
    if !(0.0..=120.0).contains(&o.handles_s) {
        return Err(AppError::invalid("handles are 0–120 seconds"));
    }
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cuts: Vec<&Cut> = match &o.cuts {
        Some(ids) => ids
            .iter()
            .map(|c| {
                ulid_id::<Cut>(c)
                    .ok()
                    .and_then(|id| doc.get(id))
                    .ok_or_else(|| AppError::not_found("no such cut"))
            })
            .collect::<AppResult<_>>()?,
        None => doc
            .iter::<Cut>()
            .filter(|c| c.kind != CutKind::Stream)
            .collect(),
    };
    if cuts.is_empty() {
        return Err(AppError::invalid("there is no cut to consolidate"));
    }
    // 1. the spans each recording needs, on its media clock
    let h = (o.handles_s * FLICKS as f64).round() as i64;
    let mut need: std::collections::HashMap<MediaRef, Vec<(i64, i64)>> =
        std::collections::HashMap::new();
    for c in &cuts {
        for pid in doc.children::<Part>(c.id.entity()) {
            if let Some(pt) = doc.get(pid) {
                need.entry(pt.media)
                    .or_default()
                    .push((pt.src_in.0, pt.src_out.0));
            }
        }
        if let Some(m) = c.moment.and_then(|m| doc.get::<Moment>(m)) {
            let a = gmclip_model::clock::anchor_src(&doc, &m.span.start);
            let b = gmclip_model::clock::anchor_src(&doc, &m.span.end);
            if let (Some(a), Some(b)) = (a, b) {
                need.entry(m.media)
                    .or_default()
                    .push((a.0, b.0.max(a.0 + 1)));
            }
        }
    }
    for t in &o.topics {
        let tp = ulid_id::<Topic>(t)
            .ok()
            .and_then(|id| doc.get(id))
            .ok_or_else(|| AppError::not_found("no such topic"))?;
        let a = gmclip_model::clock::anchor_src(&doc, &tp.span.start);
        let b = gmclip_model::clock::anchor_src(&doc, &tp.span.end);
        if let (Some(a), Some(b)) = (a, b) {
            need.entry(tp.media)
                .or_default()
                .push((a.0, b.0.max(a.0 + 1)));
        }
    }
    // in a fixed order (by media id), each media's spans widened and merged
    let mut need: Vec<(MediaRef, Vec<(i64, i64)>)> =
        need.into_iter().map(|(k, v)| (k, merge(v, h))).collect();
    need.sort_by_key(|(m, _)| owner(m));
    // 2. per source file: the member-time spans and which tracks
    #[derive(Default)]
    struct Want {
        video: Vec<(i64, i64)>,
        audio: Vec<(i64, i64)>,
    }
    let mut want: BTreeMap<Id<Source>, Want> = BTreeMap::new();
    // where a re-encoded copy takes its sound from: `(file, off)`, file time = src − off
    let mut audio_for: BTreeMap<Id<Source>, (PathBuf, i64)> = BTreeMap::new();
    for (media, spans) in &need {
        match media {
            MediaRef::Source(s) => {
                let w = want.entry(*s).or_default();
                w.video.extend(spans);
                w.audio.extend(spans);
                if let Some(src) = doc.get(*s)
                    && !src.probe.audio_tracks.is_empty()
                {
                    audio_for.insert(*s, (PathBuf::from(&src.path), 0));
                }
            }
            MediaRef::Group(g) => {
                if let Some(grp) = doc.get(*g) {
                    let first_off =
                        |m: Id<SyncMember>| segments(&doc, m).first().map_or(0, |x| x.offset.0);
                    let pic = grp.picture.and_then(|m| doc.get(m));
                    let aud = grp.master_audio.and_then(|m| doc.get(m));
                    if let (Some(pm), Some(am)) = (pic, aud)
                        && let Some(asrc) = doc.get(am.source)
                    {
                        audio_for.insert(
                            pm.source,
                            (
                                PathBuf::from(&asrc.path),
                                first_off(am.id) - first_off(pm.id),
                            ),
                        );
                    }
                }
                for mid in doc.children::<SyncMember>(g.entity()) {
                    let Some(m) = doc.get(mid) else { continue };
                    let mut ms = Vec::new();
                    for seg in segments(&doc, mid) {
                        for &(a, b) in spans {
                            let (x, y) = (a.max(seg.src_range.start.0), b.min(seg.src_range.end.0));
                            if y > x {
                                ms.push((x - seg.offset.0, y - seg.offset.0));
                            }
                        }
                    }
                    let w = want.entry(m.source).or_default();
                    if matches!(m.role, MemberRole::Picture | MemberRole::Both) {
                        w.video.extend(&ms);
                    }
                    if matches!(m.role, MemberRole::Audio | MemberRole::Both) {
                        w.audio.extend(&ms);
                    }
                }
            }
        }
    }
    // 3. the folder and the media
    let name = o
        .name
        .clone()
        .unwrap_or_else(|| format!("{} (consolidated)", p.info.name));
    let name: String = name.trim().chars().take(120).collect();
    let mut home = o
        .dest
        .join(format!("{}.gmclip", crate::core::safe_name(&name)));
    let mut k = 2;
    while home.exists() {
        home = o
            .dest
            .join(format!("{} {k}.gmclip", crate::core::safe_name(&name)));
        k += 1;
    }
    let media_dir = home.join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| ierr("making the folder", e.kind()))?;
    let result = (|| -> AppResult<Value> {
        let mut mapped: BTreeMap<Id<Source>, Mapped> = BTreeMap::new();
        let mut files: BTreeMap<Id<Source>, OutFile> = BTreeMap::new();
        let mut new_sources: Vec<Source> = Vec::new();
        let mut report = Vec::new();
        for (sid, w) in &want {
            let src = doc
                .get(*sid)
                .ok_or_else(|| AppError::not_found("a source is gone"))?;
            let input = PathBuf::from(&src.path);
            let stem: String = input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "media".into())
                .chars()
                .filter(|c| !matches!(c, '/' | '\\' | ':'))
                .take(80)
                .collect();
            let tail = &sid.0.to_string()[20..];
            let ext = input
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "mp4".into());
            let fpf = src
                .probe
                .tracks
                .iter()
                .find_map(|t| t.fps)
                .map_or(FLICKS / 30, crate::video::framing_fpf);
            let want_v = !w.video.is_empty();
            let want_a = !w.audio.is_empty() && !src.probe.audio_tracks.is_empty();
            let spans: Vec<(i64, i64)> =
                merge(w.video.iter().chain(&w.audio).copied().collect(), 0);
            let spans_s: Vec<(f64, f64)> = spans
                .iter()
                .map(|&(a, b)| (a as f64 / FLICKS as f64, b as f64 / FLICKS as f64))
                .collect();
            let mut entry = json!({"source": sid.0.to_string(),
                                   "was": input.file_name().map(|f| f.to_string_lossy().into_owned()),
                                   "bytes_before": std::fs::metadata(&input).map_or(0, |m| m.len())});
            let mut mp = Mapped::default();
            let mut after = 0u64;
            let mut whys: Vec<String> = Vec::new();
            let mut whole: Option<String> = None; // the whole original, copied once
            // a re-encoded picture's file and pieces (its sound can serve as the sound)
            let mut reenc: Option<(String, Vec<gmclip_media::mux::trim::Piece>)> = None;
            let mut copy_whole = |home: &Path| -> AppResult<String> {
                if let Some(r) = &whole {
                    return Ok(r.clone());
                }
                let rel = format!("media/{stem}-{tail}-whole.{ext}");
                std::fs::copy(&input, home.join(&rel))
                    .map_err(|e| ierr("copying a recording", e.kind()))?;
                whole = Some(rel.clone());
                Ok(rel)
            };
            // the picture
            if want_v {
                let rel = format!("media/{stem}-{tail}.mp4");
                let out = home.join(&rel);
                let how = match gmclip_media::mux::trim::trim(&input, &out, &spans_s, true, false) {
                    Ok(r) => {
                        mp.video = Some(Moved::Pieces(
                            r.pieces
                                .iter()
                                .filter(|p| p.frames > 0)
                                .map(|p| {
                                    let a = p.in_frame0 * fpf;
                                    (
                                        a,
                                        a + p.frames as i64 * fpf,
                                        (p.in_frame0 - p.out_frame0) * fpf,
                                    )
                                })
                                .collect(),
                        ));
                        mp.frames = r
                            .pieces
                            .iter()
                            .map(|p| (p.in_frame0, p.out_frame0, p.frames))
                            .collect();
                        entry["pieces"] = json!(
                            r.pieces
                                .iter()
                                .map(|p| json!([p.in_start_ms, p.in_end_ms]))
                                .collect::<Vec<_>>()
                        );
                        "trimmed"
                    }
                    Err(why) => {
                        let _ = std::fs::remove_file(&out);
                        whys.push(format!("picture: {why}"));
                        match reencode(src, audio_for.get(sid).cloned(), &out, &spans) {
                            Ok(p) => {
                                reenc = Some((rel.clone(), p.clone()));
                                mp.video = Some(Moved::Pieces(
                                    p.iter()
                                        .map(|x| {
                                            let a = x.in_frame0 * fpf;
                                            (
                                                a,
                                                a + x.frames as i64 * fpf,
                                                (x.in_frame0 - x.out_frame0) * fpf,
                                            )
                                        })
                                        .collect(),
                                ));
                                mp.frames = p
                                    .iter()
                                    .map(|x| (x.in_frame0, x.out_frame0, x.frames))
                                    .collect();
                                entry["pieces"] = json!(
                                    p.iter()
                                        .map(|x| json!([x.in_start_ms, x.in_end_ms]))
                                        .collect::<Vec<_>>()
                                );
                                "reencoded"
                            }
                            Err(why2) => {
                                let _ = std::fs::remove_file(&out);
                                whys.push(format!("the export encoder: {why2}"));
                                mp.video = Some(Moved::Same);
                                "whole"
                            }
                        }
                    }
                };
                let rel = if how == "whole" {
                    copy_whole(&home)?
                } else {
                    rel
                };
                let probe = if how == "whole" {
                    src.probe.clone()
                } else {
                    crate::video::probe(&home.join(&rel))?.probe
                };
                let (f, size) = out_file(&home, &rel, probe)?;
                after += size;
                entry["picture"] = json!({"how": how, "file": rel});
                files.insert(*sid, f);
            }
            // the sound: its own file when the picture has one, so each source is one file
            if want_a {
                let rel = if want_v {
                    format!("media/{stem}-{tail}-sound.m4a")
                } else {
                    format!("media/{stem}-{tail}.m4a")
                };
                let out = home.join(&rel);
                let rate = src.probe.audio_tracks.first().map_or(48_000, |a| a.rate);
                let trimmed = if rate == 48_000 {
                    gmclip_media::mux::trim::trim(&input, &out, &spans_s, false, true)
                        .map_err(|e| e.to_string())
                } else {
                    Err(format!(
                        "{rate} Hz sound (the planner reads 48 kHz positions)"
                    ))
                };
                let (moved, rel, probe, how) = match trimmed {
                    Ok(r) => {
                        let per = FLICKS / i64::from(r.audio_rate.unwrap_or(48_000));
                        let pieces = r
                            .pieces
                            .iter()
                            .filter(|p| p.samples > 0)
                            .map(|p| {
                                let a = p.in_sample0 * per;
                                (
                                    a,
                                    a + p.samples as i64 * per,
                                    (p.in_sample0 - p.out_sample0) * per,
                                )
                            })
                            .collect();
                        let samples: u64 = r.pieces.iter().map(|p| p.samples).sum();
                        let probe = gmclip_model::entities::Probe {
                            duration: Flicks((samples as i64) * per),
                            tracks: Vec::new(),
                            audio_tracks: src.probe.audio_tracks.iter().take(1).cloned().collect(),
                        };
                        (Moved::Pieces(pieces), rel, probe, "trimmed")
                    }
                    Err(why) => {
                        let _ = std::fs::remove_file(&out);
                        whys.push(format!("sound: {why}"));
                        match (&reenc, audio_for.get(sid)) {
                            // the re-encoded picture carries this file's own sound (48 kHz AAC)
                            (Some((vrel, pieces)), Some((path, 0))) if *path == input => {
                                let per = FLICKS / 48_000;
                                let moved = Moved::Pieces(
                                    pieces
                                        .iter()
                                        .map(|p| {
                                            let a = p.in_sample0 * per;
                                            (
                                                a,
                                                a + p.samples as i64 * per,
                                                (p.in_sample0 - p.out_sample0) * per,
                                            )
                                        })
                                        .collect(),
                                );
                                (
                                    moved,
                                    vrel.clone(),
                                    crate::video::probe(&home.join(vrel))?.probe,
                                    "reencoded",
                                )
                            }
                            _ => (Moved::Same, copy_whole(&home)?, src.probe.clone(), "whole"),
                        }
                    }
                };
                // the same whole file as the picture: one source still
                let holder = if !want_v || (mp.video == Some(Moved::Same) && moved == Moved::Same) {
                    *sid
                } else {
                    Id::new(new_ulid())
                };
                if holder == *sid {
                    if !files.contains_key(sid) {
                        let (f, size) = out_file(&home, &rel, probe)?;
                        after += size;
                        files.insert(*sid, f);
                    }
                } else {
                    let (f, size) = out_file(&home, &rel, probe.clone())?;
                    if entry["picture"]["file"].as_str() != Some(rel.as_str()) {
                        after += size;
                    }
                    new_sources.push(Source {
                        id: holder,
                        path: f.rel.clone(),
                        fingerprint: format!("sha256:{}", f.sha),
                        probe,
                        audio_roles: src.audio_roles.clone(),
                        stream_layout: None,
                        relink_hint: None,
                        cursor: None,
                        markers: Vec::new(),
                    });
                }
                entry["sound"] = json!({"how": how, "file": rel});
                mp.audio = Some((moved, holder));
            }
            let hows: Vec<String> = [&entry["picture"]["how"], &entry["sound"]["how"]]
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let hows: Vec<&str> = hows.iter().map(String::as_str).collect();
            entry["how"] = json!(if hows.contains(&"reencoded") {
                "reencoded"
            } else if hows.iter().all(|h| *h == "whole") {
                "whole"
            } else {
                "trimmed"
            });
            entry["lossless"] = json!(!hows.contains(&"reencoded"));
            if !whys.is_empty() {
                entry["why"] = json!(whys.join("; ").chars().take(400).collect::<String>());
            }
            entry["bytes_after"] = json!(after);
            mapped.insert(*sid, mp);
            report.push(entry);
        }
        // 4. the document, with the trimmed recordings behind sync groups
        let chosen: BTreeSet<Ulid> = cuts.iter().map(|c| c.id.0).collect();
        let (newdoc, unresolved, groups) =
            rewrite(&doc, &need, &chosen, &mapped, &files, &new_sources)?;
        let mut analysis = Vec::new();
        for (ow, kind, rev) in p.handle.analysis_keys() {
            let Some(body) = p.handle.analysis(&ow, &kind, &rev) else {
                continue;
            };
            let ow2 = Ulid::parse(&ow)
                .ok()
                .and_then(|u| groups.get(&Id::<Source>::new(u)))
                .map_or(ow.clone(), |g| g.0.to_string());
            analysis.push((ow2, kind, rev, body));
        }
        let info = gmclip_mcp::project::new_home_info(&name);
        gmclip_core::write_fresh_store(&home.join("project.sqlite"), &newdoc, &analysis)
            .map_err(|e| ierr("writing the database", e))?;
        std::fs::write(
            home.join("owner.json"),
            serde_json::to_vec_pretty(&info).map_err(|e| ierr("owner.json", e))?,
        )
        .map_err(|e| ierr("owner.json", e.kind()))?;
        // the home's settings that travel
        if let Ok(b) = std::fs::read(p.home.join("framing.json")) {
            let _ = std::fs::write(home.join("framing.json"), b);
        }
        if let Ok(mut s) = std::fs::read_to_string(p.home.join("chat.json")) {
            for (sid, g) in &groups {
                s = s.replace(&sid.0.to_string(), &g.0.to_string());
            }
            let _ = std::fs::write(home.join("chat.json"), s);
        }
        let manifest = json!({
            "schema": "gmclip.consolidated/0", "from": {"project": p.info.id, "name": p.info.name},
            "project": info.id, "made_ms": crate::core::now_ms(), "handles_s": o.handles_s,
            "cuts": cuts.iter().map(|c| json!({"id": c.id.0.to_string(), "title": c.title})).collect::<Vec<_>>(),
            "topics": o.topics, "sources": report, "unresolved_frame_anchors": unresolved,
            "cuts_not_covered": doc.iter::<Cut>().filter(|c| !cuts.iter().any(|x| x.id == c.id))
                .map(|c| json!({"id": c.id.0.to_string(), "title": c.title, "kind": c.kind})).collect::<Vec<_>>(),
            "history": "the edit history stays with the original project; this one starts at the consolidation",
        });
        std::fs::write(
            home.join("consolidated.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|e| ierr("manifest", e))?,
        )
        .map_err(|e| ierr("manifest", e.kind()))?;
        let before: u64 = report
            .iter()
            .filter_map(|r| r["bytes_before"].as_u64())
            .sum();
        let after: u64 = report
            .iter()
            .filter_map(|r| r["bytes_after"].as_u64())
            .sum();
        Ok(
            json!({"home": home, "project": info.id, "sources": report, "bytes_before": before,
                  "bytes_after": after, "ms": t0.elapsed().as_millis() as u64}),
        )
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&home);
    }
    result
}

/// The export encoder's copy of `spans` (flicks on the source's own clock) of a file
/// [`gmclip_media::mux::trim`] cannot cut (another codec or container, an open GOP…): one
/// H.264 + AAC MP4 with the spans back to back on the frame grid, at most 1920 wide, its
/// sound from `audio` (`(file, off)`: that file's time = the source's − `off`). Lossy, so
/// a render from it is close to the original, not identical. Returns the pieces.
fn reencode(
    src: &Source,
    audio: Option<(PathBuf, i64)>,
    out: &Path,
    spans: &[(i64, i64)],
) -> Result<Vec<gmclip_media::mux::trim::Piece>, String> {
    let (sw, sh, fps) = crate::review::picture_info(src).map_err(|e| e.message)?;
    // the picture's own rate, NTSC exactly (decision 0020), so the copy is frame for frame
    let rate = match crate::review::picture_rate(src) {
        Some(r) => r,
        None => gmclip_plan::types::Fps::new(fps.clamp(1, 60) as u64, 1)
            .map_err(|_| format!("{fps} fps is not an export rate"))?,
    };
    let (rnum, rden) = (rate.num.get() as i64, rate.den.get() as i64);
    let fpf = rate.flicks_per_frame() as i64;
    // S(k): the output sample at frame k (rule 4; NTSC rounds down)
    let s_at = |k: i64| (i128::from(k) * 48_000 * i128::from(rden) / i128::from(rnum)) as i64;
    let (fps_json, schema) = crate::review::rate_json(rate);
    let (cw, ch) = crate::video::canvas_for((sw, sh), 1920);
    let (apath, aoff) = audio.ok_or("no sound to carry")?;
    let mut parts = Vec::new();
    let mut pieces = Vec::new();
    let mut c = 0i64;
    for (i, &(a, b)) in spans.iter().enumerate() {
        let (k0, k1) = (a.div_euclid(fpf).max(0), (b + fpf - 1).div_euclid(fpf));
        let n = (k1 - k0).max(1);
        let first_sample = ((k0 * fpf - aoff) as i128 * 48_000 / i128::from(FLICKS)).max(0);
        parts.push(json!({
            "id": format!("p{i}"), "out": [c, c + n],
            "picture": {"source": "pic", "first_frame": k0},
            "audio": {"source": "aud", "first_sample": first_sample as i64, "rate": 48000},
            "layers": [{"id": "l", "role": "other", "crop": {"x": 0, "y": 0, "w": sw, "h": sh},
                        "place": {"x": 0, "y": 0, "w": cw, "h": ch}, "z": 0}],
        }));
        pieces.push(gmclip_media::mux::trim::Piece {
            in_frame0: k0,
            out_frame0: c,
            frames: n as u64,
            in_sample0: s_at(k0),
            out_sample0: s_at(c),
            samples: (s_at(c + n) - s_at(c)) as u64,
            in_start_ms: k0 * 1000 * rden / rnum,
            in_end_ms: k1 * 1000 * rden / rnum,
        });
        c += n;
    }
    let plan = json!({
        "schema": schema,
        "meta": {"title": "consolidate"},
        "output": {"canvas": {"w": cw, "h": ch}, "fps": fps_json, "frames": c,
                   "video": {"codec": "h264", "rate": {"mode": "vbr", "target_bps": 16_000_000, "max_bps": 24_000_000}},
                   "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "pic", "path": src.path}, {"id": "aud", "path": apath.to_string_lossy()}],
        "parts": parts,
        "audio": {"loudness": {"mode": "as_recorded"}}
    });
    let plan = gmclip_plan::parse(&serde_json::to_vec(&plan).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{} ({})", e.reason(), e.path()))?;
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    gmclip_render::export::export(
        &plan,
        &mut inputs,
        out,
        &gmclip_render::export::ExportOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(pieces)
}

/// A segment covering a whole file's clock (a recording copied whole).
const WHOLE: (i64, i64) = (0, i64::MAX / 4);

/// A member's segments `(group start, group end, offset)` after its file moved: each old
/// segment cut to the pieces kept (group time = member time + old offset), or unchanged
/// when the file was copied whole.
fn recut(old: &[SyncSegment], moved: &Moved) -> Vec<(i64, i64, i64)> {
    match moved {
        Moved::Same => old
            .iter()
            .map(|s| (s.src_range.start.0, s.src_range.end.0, s.offset.0))
            .collect(),
        Moved::Pieces(pieces) => {
            let mut out = Vec::new();
            for seg in old {
                for &(a, b, shift) in pieces {
                    let x = (a + seg.offset.0).max(seg.src_range.start.0);
                    let y = (b + seg.offset.0).min(seg.src_range.end.0);
                    if y > x {
                        out.push((x, y, seg.offset.0 + shift));
                    }
                }
            }
            out
        }
    }
}

fn pieces_of(m: &Moved) -> Vec<(i64, i64, i64)> {
    match m {
        Moved::Pieces(p) => p.clone(),
        Moved::Same => vec![(WHOLE.0, WHOLE.1, 0)],
    }
}

/// The document with moved sources: trimmed sources used directly as media behind new
/// sync groups, existing groups' members re-cut onto the new files, split-off sound files
/// as sources of their own. Returns `(doc, unresolved frame anchors, source → new group)`.
#[allow(clippy::type_complexity)]
fn rewrite(
    doc: &Doc,
    need: &[(MediaRef, Vec<(i64, i64)>)],
    chosen: &BTreeSet<Ulid>,
    mapped: &BTreeMap<Id<Source>, Mapped>,
    files: &BTreeMap<Id<Source>, OutFile>,
    new_sources: &[Source],
) -> AppResult<(Doc, usize, BTreeMap<Id<Source>, Id<SyncGroup>>)> {
    let rev = doc.rev();
    let mut new_rows: Vec<(EntityKind, Ulid, Vec<u8>)> = Vec::new();
    let kind =
        |n: &str| gmclip_model::kinds::kind_by_name(n).ok_or_else(|| AppError::internal("kind"));
    let seg_rows = |rows: &mut Vec<(EntityKind, Ulid, Vec<u8>)>,
                    mid: Id<SyncMember>,
                    segs: &[(i64, i64, i64)]|
     -> AppResult<()> {
        for (j, &(a, b, off)) in segs.iter().enumerate() {
            let sid: Id<SyncSegment> = Id::new(new_ulid());
            rows.push((
                kind("sync_segment")?,
                sid.0,
                cbor::to_vec(&SyncSegment {
                    id: sid,
                    member: mid,
                    order: order(j),
                    src_range: SrcRange {
                        start: Flicks(a),
                        end: Flicks(b),
                    },
                    offset: Flicks(off),
                }),
            ));
        }
        Ok(())
    };
    for s in new_sources {
        new_rows.push((kind("source")?, s.id.0, cbor::to_vec(s)));
    }
    // sources used directly as media whose picture or sound moved: a new group each
    let mut groups: BTreeMap<Id<Source>, Id<SyncGroup>> = BTreeMap::new();
    for (media, _) in need {
        let MediaRef::Source(s) = media else { continue };
        let Some(mp) = mapped.get(s) else { continue };
        let video = mp.video.clone().unwrap_or(Moved::Same);
        let same_audio = mp
            .audio
            .as_ref()
            .is_none_or(|(m, h)| *m == Moved::Same && h == s);
        if video == Moved::Same && same_audio {
            continue; // copied whole: the same clock, the same source
        }
        let g: Id<SyncGroup> = Id::new(new_ulid());
        let pic: Id<SyncMember> = Id::new(new_ulid());
        let aud: Id<SyncMember> = Id::new(new_ulid());
        groups.insert(*s, g);
        let (asrc, apieces) = match &mp.audio {
            Some((m, h)) => (*h, pieces_of(m)),
            None => (*s, pieces_of(&video)),
        };
        new_rows.push((
            kind("sync_group")?,
            g.0,
            cbor::to_vec(&SyncGroup {
                id: g,
                master_audio: Some(aud),
                picture: Some(pic),
            }),
        ));
        for (mid, role, source, pieces, i) in [
            (pic, MemberRole::Picture, *s, pieces_of(&video), 0usize),
            (aud, MemberRole::Audio, asrc, apieces, 1),
        ] {
            new_rows.push((
                kind("sync_member")?,
                mid.0,
                cbor::to_vec(&SyncMember {
                    id: mid,
                    group: g,
                    order: order(i),
                    source,
                    role,
                    rate_ppm: 0.0,
                    method: SyncMethod::Import,
                    confidence: 1.0,
                    under_db: None,
                }),
            ));
            seg_rows(&mut new_rows, mid, &pieces)?;
        }
    }
    // members of existing groups on moved files
    let mut seg_drop: BTreeSet<Ulid> = BTreeSet::new();
    let mut member_set: BTreeMap<Ulid, (MemberRole, Id<Source>)> = BTreeMap::new();
    let mut audio_from: BTreeMap<String, String> = BTreeMap::new();
    for m in doc.iter::<SyncMember>() {
        let Some(mp) = mapped.get(&m.source) else {
            continue;
        };
        let old = segments(doc, m.id);
        let video = mp.video.clone().unwrap_or(Moved::Same);
        let (amoved, holder) = mp.audio.clone().unwrap_or((Moved::Same, m.source));
        let (role, src, segs) = match m.role {
            MemberRole::Picture => (MemberRole::Picture, m.source, recut(&old, &video)),
            MemberRole::Audio => (MemberRole::Audio, holder, recut(&old, &amoved)),
            MemberRole::Both
                if video == Moved::Same && amoved == Moved::Same && holder == m.source =>
            {
                continue;
            }
            MemberRole::Both => {
                // one file became two: this member keeps the picture, a new one the sound
                let a: Id<SyncMember> = Id::new(new_ulid());
                new_rows.push((
                    kind("sync_member")?,
                    a.0,
                    cbor::to_vec(&SyncMember {
                        id: a,
                        role: MemberRole::Audio,
                        source: holder,
                        order: order(99),
                        ..m.clone()
                    }),
                ));
                seg_rows(&mut new_rows, a, &recut(&old, &amoved))?;
                audio_from.insert(m.id.0.to_string(), a.0.to_string());
                (MemberRole::Picture, m.source, recut(&old, &video))
            }
        };
        for x in &old {
            seg_drop.insert(x.id.0);
        }
        member_set.insert(m.id.0, (role, src));
        seg_rows(&mut new_rows, m.id, &segs)?;
    }
    // Cuts left out (the stream edit, by default) keep only what travelled: each of their
    // parts on a trimmed recording is cut to the stretches kept (one part per stretch, in
    // order); a part with nothing kept goes, with what hangs off it. Else the copy would
    // hold cuts that cannot be shown.
    let (clipped, dropped, extra) = clip_left_out(doc, need, chosen, &groups)?;
    // the rest: every entity, rewritten where it names a moved source
    let mut out = Doc::new();
    let mut unresolved = 0usize;
    let srcs: BTreeMap<String, String> = groups
        .iter()
        .map(|(s, g)| (s.0.to_string(), g.0.to_string()))
        .collect();
    let frames: BTreeMap<String, Vec<(i64, i64, u64)>> = mapped
        .iter()
        .filter(|(_, m)| !m.frames.is_empty())
        .map(|(s, m)| (s.0.to_string(), m.frames.clone()))
        .collect();
    let gone = |mut e: EntityId| loop {
        if dropped.contains(&e.id) {
            return true;
        }
        match doc.parent_of(e) {
            Some(p) => e = p,
            None => return false,
        }
    };
    let rows = doc
        .entity_ids()
        .into_iter()
        .map(|e| (e, None))
        .chain(extra.into_iter().map(|(e, b)| (e, Some(b))));
    for (e, fresh) in rows {
        if seg_drop.contains(&e.id) || (fresh.is_none() && gone(e)) {
            continue;
        }
        let body = match (fresh, clipped.get(&e.id)) {
            (Some(b), _) => FieldValue(b),
            (None, Some(b)) => FieldValue(b.clone()),
            (None, None) => match doc.encode(e) {
                Some(b) => b,
                None => continue,
            },
        };
        let rev_e = doc.entity_rev(e).unwrap_or(rev);
        let by = doc.last_author(e).unwrap_or(Author::System);
        let kname = e.kind.0;
        let mut bytes = body.0.clone();
        if kname == "source" {
            let mut s: Source = cbor::from_slice(&bytes).map_err(|e| ierr("source", e))?;
            if let Some(f) = files.get(&s.id) {
                s.path = f.rel.clone();
                s.fingerprint = format!("sha256:{}", f.sha);
                s.probe = f.probe.clone();
                s.relink_hint = None;
            }
            bytes = cbor::to_vec(&s);
        } else if kname == "sync_member" {
            let mut m: SyncMember = cbor::from_slice(&bytes).map_err(|e| ierr("member", e))?;
            if let Some((r, src)) = member_set.get(&m.id.0) {
                m.role = *r;
                m.source = *src;
            }
            bytes = cbor::to_vec(&m);
        } else if kname == "sync_group" {
            let mut g: SyncGroup = cbor::from_slice(&bytes).map_err(|e| ierr("group", e))?;
            if let Some(a) = g
                .master_audio
                .and_then(|m| audio_from.get(&m.0.to_string()))
            {
                g.master_audio = Some(Id::new(Ulid::parse(a).map_err(|_| ierr("id", "member"))?));
            }
            bytes = cbor::to_vec(&g);
        } else if !srcs.is_empty() || !audio_from.is_empty() || !frames.is_empty() {
            let text = String::from_utf8_lossy(&bytes);
            let mentions = srcs
                .keys()
                .chain(audio_from.keys())
                .chain(frames.keys())
                .any(|id| text.contains(id.as_str()));
            if mentions {
                let mut v: Value = cbor::from_slice(&bytes).map_err(|e| ierr("entity", e))?;
                walk(&mut v, &srcs, &audio_from, &frames, &mut unresolved);
                if kname == "project"
                    && let Some(map) = v.get_mut("active_asr").and_then(Value::as_object_mut)
                {
                    // the active ASR per owner
                    for (s, g) in &srcs {
                        if let Some(x) = map.remove(s) {
                            map.insert(g.clone(), x);
                        }
                    }
                }
                bytes = cbor::to_vec(&v);
            }
        }
        out.load_row(e.kind, e.id, rev_e, by, &bytes)
            .map_err(|x| ierr("loading a row", format!("{x:?} ({kname})")))?;
    }
    for (k, id, body) in new_rows {
        out.load_row(k, id, rev, Author::System, &body)
            .map_err(|x| ierr("loading a new row", format!("{x:?}")))?;
    }
    out.set_rev(rev);
    Ok((out, unresolved, groups))
}

/// The left-out cuts' parts on trimmed media, cut to the stretches kept (`need`, the
/// merged spans with handles, all inside what the trimmed files hold): rewritten part rows
/// by id, the parts that keep nothing, and new part rows for the second and later
/// stretches of a part (ordered right after it).
#[allow(clippy::type_complexity)]
fn clip_left_out(
    doc: &Doc,
    need: &[(MediaRef, Vec<(i64, i64)>)],
    chosen: &BTreeSet<Ulid>,
    groups: &BTreeMap<Id<Source>, Id<SyncGroup>>,
) -> AppResult<(
    BTreeMap<Ulid, Vec<u8>>,
    BTreeSet<Ulid>,
    Vec<(EntityId, Vec<u8>)>,
)> {
    let mut clipped = BTreeMap::new();
    let mut dropped = BTreeSet::new();
    let mut extra = Vec::new();
    for c in doc.iter::<Cut>().filter(|c| !chosen.contains(&c.id.0)) {
        let mut parts: Vec<&Part> = doc
            .children::<Part>(c.id.entity())
            .filter_map(|p| doc.get(p))
            .collect();
        parts.sort_by(|a, b| a.order.cmp(&b.order));
        for (i, p) in parts.iter().enumerate() {
            // only media whose file was trimmed (a source behind a new group)
            let MediaRef::Source(s) = p.media else {
                continue;
            };
            if !groups.contains_key(&s) {
                continue;
            }
            let spans = need
                .iter()
                .find(|(m, _)| *m == p.media)
                .map_or(&[][..], |(_, v)| v.as_slice());
            let keep: Vec<(i64, i64)> = spans
                .iter()
                .map(|&(a, b)| (a.max(p.src_in.0), b.min(p.src_out.0)))
                .filter(|(a, b)| b > a)
                .collect();
            if keep == [(p.src_in.0, p.src_out.0)] {
                continue;
            }
            let Some((&first, rest)) = keep.split_first() else {
                dropped.insert(p.id.0);
                continue;
            };
            let at = |(a, b): (i64, i64)| Part {
                src_in: Flicks(a),
                src_out: Flicks(b),
                ..(*p).clone()
            };
            clipped.insert(p.id.0, cbor::to_vec(&at(first)));
            let next = parts.get(i + 1).map(|n| &n.order);
            let mut lo = p.order.clone();
            for &r in rest {
                let order = OrderKey::between(Some(&lo), next)
                    .map_err(|e| ierr("ordering a kept stretch", format!("{e:?}")))?;
                let id: Id<Part> = Id::new(new_ulid());
                let np = Part {
                    id,
                    order: order.clone(),
                    ..at(r)
                };
                extra.push((id.entity(), cbor::to_vec(&np)));
                lo = order;
            }
        }
    }
    Ok((clipped, dropped, extra))
}

/// Rewrites one entity's JSON: media and raw times naming a trimmed source name its group,
/// `audio_from` follows a split member, frame anchors move to the new file's frames.
fn walk(
    v: &mut Value,
    srcs: &BTreeMap<String, String>,
    audio_from: &BTreeMap<String, String>,
    frames: &BTreeMap<String, Vec<(i64, i64, u64)>>,
    unresolved: &mut usize,
) {
    match v {
        Value::Object(o) => {
            // MediaRef::Source(s)
            if o.len() == 1
                && let Some(g) = o
                    .get("source")
                    .and_then(Value::as_str)
                    .and_then(|s| srcs.get(s))
            {
                let g = g.clone();
                o.clear();
                o.insert("group".into(), Value::from(g));
                return;
            }
            // ModelTime::Src { source: s, t }
            if o.get("clock").and_then(Value::as_str) == Some("src")
                && let Some(g) = o
                    .get("source")
                    .and_then(Value::as_str)
                    .and_then(|s| srcs.get(s))
                    .cloned()
            {
                o.remove("source");
                o.insert("group".into(), Value::from(g));
            }
            // FrameRef { source, frame_index }
            if let (Some(s), Some(i)) = (
                o.get("source").and_then(Value::as_str).map(str::to_owned),
                o.get("frame_index").and_then(Value::as_i64),
            ) && let Some(pieces) = frames.get(&s)
            {
                match pieces
                    .iter()
                    .find(|(a, _, n)| i >= *a && i < *a + *n as i64)
                {
                    Some((a, b, _)) => {
                        o.insert("frame_index".into(), Value::from(i - a + b));
                    }
                    None => *unresolved += 1,
                }
            }
            if let Some(a) = o
                .get("audio_from")
                .and_then(Value::as_str)
                .and_then(|m| audio_from.get(m))
                .cloned()
            {
                o.insert("audio_from".into(), Value::from(a));
            }
            for x in o.values_mut() {
                walk(x, srcs, audio_from, frames, unresolved);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk(x, srcs, audio_from, frames, unresolved);
            }
        }
        _ => {}
    }
}

/// Opens a consolidated folder (made here or on another Mac): its database goes to the
/// app data with the media paths made absolute, the folder becomes the project's home.
pub fn open_copy(core: &Arc<AppCore>, home: &Path) -> AppResult<Value> {
    let home = home
        .canonicalize()
        .map_err(|_| AppError::not_found("no folder there"))?;
    let dir = cap_std::fs::Dir::open_ambient_dir(&home, cap_std::ambient_authority())
        .map_err(|_| AppError::not_found("no folder there"))?;
    let info = gmclip_mcp::project::read_home(&dir).map_err(AppError::from)?;
    if core.is_open(&info.id) {
        return Ok(json!({"id": info.id, "opened": true}));
    }
    let db = gmclip_mcp::project::db_path(&core.dirs.app_data, &info.id).map_err(AppError::from)?;
    if !db.exists() {
        let ro = gmclip_core::ReadOnlyStore::open(&home.join("project.sqlite"))
            .map_err(|e| ierr("reading the folder's database", e))?;
        let doc = ro
            .doc()
            .map_err(|e| ierr("reading the folder's database", e))?;
        let mut analysis = Vec::new();
        for (o, k, r) in ro.analysis_keys().map_err(|e| ierr("analysis", e))? {
            if let Some(b) = ro.analysis(&o, &k, &r).map_err(|e| ierr("analysis", e))? {
                analysis.push((o, k, r, b));
            }
        }
        // media paths relative to the folder become absolute here
        let mut out = Doc::new();
        for e in doc.entity_ids() {
            let Some(body) = doc.encode(e) else { continue };
            let mut bytes = body.0.clone();
            if e.kind.0 == "source" {
                let mut s: Source = cbor::from_slice(&bytes).map_err(|x| ierr("source", x))?;
                let p = Path::new(&s.path);
                if p.is_relative() {
                    if p.components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                    {
                        return Err(AppError::invalid("a media path leaves the folder"));
                    }
                    s.path = home.join(p).to_string_lossy().into_owned();
                }
                bytes = cbor::to_vec(&s);
            }
            out.load_row(
                e.kind,
                e.id,
                doc.entity_rev(e).unwrap_or(doc.rev()),
                doc.last_author(e).unwrap_or(Author::System),
                &bytes,
            )
            .map_err(|x| ierr("loading a row", format!("{x:?}")))?;
        }
        out.set_rev(doc.rev());
        gmclip_core::write_fresh_store(&db, &out, &analysis)
            .map_err(|e| ierr("writing the database", e))?;
    }
    gmclip_mcp::project::adopt_home(&core.dirs.app_data, &home, &info.id)
        .map_err(AppError::from)?;
    core.open_home(&home)
}

/// `project_consolidate`, `project_open_copy`.
pub fn ipc(
    core: &Arc<AppCore>,
    command: &str,
    project: Option<&str>,
    a: &Value,
) -> AppResult<Value> {
    match command {
        "project_consolidate" => {
            let pid = project.ok_or_else(|| AppError::invalid("a project is needed"))?;
            let dest = a
                .get("dest")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| core.dirs.exports.join("Consolidated"));
            if !dest.is_absolute() {
                return Err(AppError::invalid("the destination is an absolute folder"));
            }
            std::fs::create_dir_all(&dest).map_err(|e| ierr("the destination", e.kind()))?;
            let strs = |k: &str| -> Vec<String> {
                a.get(k)
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            consolidate(
                core,
                pid,
                &Options {
                    cuts: a
                        .get("cuts")
                        .and_then(Value::as_array)
                        .map(|_| strs("cuts")),
                    topics: strs("topics"),
                    handles_s: a.get("handles_s").and_then(Value::as_f64).unwrap_or(5.0),
                    dest,
                    name: a.get("name").and_then(Value::as_str).map(str::to_owned),
                },
            )
        }
        "project_open_copy" => {
            let path = a
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::invalid("`path` is required"))?;
            open_copy(core, Path::new(path.trim()))
        }
        _ => Err(AppError::invalid("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_widen_merge_and_clamp() {
        assert_eq!(merge(vec![(50, 80), (10, 20)], 10), vec![(0, 30), (40, 90)]);
        assert_eq!(merge(vec![(50, 80), (10, 20)], 20), vec![(0, 100)]);
        assert_eq!(merge(vec![], 5), vec![]);
    }

    #[test]
    fn entities_name_the_group_and_the_new_frames() {
        let s = "01M50000000000000000000000".to_owned();
        let g = "01M5G000000000000000000000".to_owned();
        let srcs = BTreeMap::from([(s.clone(), g.clone())]);
        let af = BTreeMap::from([(
            "01M5M000000000000000000000".to_owned(),
            "01M5N000000000000000000000".to_owned(),
        )]);
        let frames = BTreeMap::from([(s.clone(), vec![(90, 0, 300), (900, 300, 150)])]);
        let mut v = json!({
            "media": {"source": s},
            "span": {"start": {"raw": {"time": {"clock": "src", "source": s, "t": 7}, "reason": "x"}}},
            "evidence": [{"source": s, "frame_index": 100}, {"source": s, "frame_index": 950}, {"source": s, "frame_index": 5}],
            "region": {"id": "r", "source": s, "role": "cam"},
            "audio_from": "01M5M000000000000000000000",
        });
        let mut un = 0;
        walk(&mut v, &srcs, &af, &frames, &mut un);
        assert_eq!(v["media"], json!({"group": g}));
        assert_eq!(
            v["span"]["start"]["raw"]["time"],
            json!({"clock": "src", "group": g, "t": 7})
        );
        assert_eq!(v["evidence"][0]["frame_index"], 10);
        assert_eq!(v["evidence"][1]["frame_index"], 350);
        assert_eq!(
            v["evidence"][2]["frame_index"], 5,
            "outside every piece: kept, counted"
        );
        assert_eq!(un, 1);
        assert_eq!(
            v["region"]["source"],
            s.as_str(),
            "a region stays on its source"
        );
        assert_eq!(v["audio_from"], "01M5N000000000000000000000");
    }
}
