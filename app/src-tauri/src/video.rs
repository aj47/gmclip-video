//! Open any video (AJ 2026-10-14: "i should just be able to select any video and it should
//! let me edit that video").
//!
//! "Open video…" (the native panel) or a file dropped on the window: a project in the
//! default library named after the file, the file as a source (probed by gmclip's own
//! demuxers), one cut covering the whole video in a 16:9 framing at the source's size
//! (≤ 1920 wide) with one full-frame layer, then background analysis (ASR first). No
//! name prompt. Opening the same file again reopens its project (Recent videos on Home,
//! `<app data>/recent-videos.json`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_media::demux::{Codec, Track};
use gmclip_model::entities::{AudioTrackProbe, Probe, TrackKind, TrackProbe};
use gmclip_model::{Flicks, Rational};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

/// Extensions "Open video…" offers and a drop accepts (what the demuxers read).
pub const VIDEO_EXTS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm"];

/// One row of Recent videos.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Recent {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub home: PathBuf,
    pub project: String,
    pub cut: String,
    pub duration_s: f64,
    pub opened_ms: i64,
}

const MAX_RECENT: usize = 30;

fn recent_file(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("recent-videos.json")
}

pub fn load_recent(core: &AppCore) -> Vec<Recent> {
    std::fs::read(recent_file(core))
        .ok()
        .filter(|b| b.len() < 4 << 20)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_recent(core: &AppCore, list: &[Recent]) {
    let f = recent_file(core);
    let _ = std::fs::create_dir_all(&core.dirs.app_data);
    let tmp = f.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(list).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, f);
    }
}

fn codec_name(c: &Codec) -> &'static str {
    match c {
        Codec::H264 { .. } => "h264",
        Codec::Hevc { .. } => "hevc",
        Codec::Vp9 { .. } => "vp9",
        Codec::Av1 { .. } => "av1",
        Codec::Aac { .. } => "aac",
        Codec::Pcm { .. } => "pcm",
        _ => "other",
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The frame rate a track's sample durations say (the most common duration; on a coarse
/// clock, the presentation span snapped to a standard rate).
pub(crate) fn track_fps(t: &Track) -> Option<Rational> {
    // The frame count over the span of presentation times, on any clock: Matroska's 1 ms
    // (33 and 34 ms for 30 fps), a restream's 10 kHz fragments (330–340 ticks), a phone's
    // VFR ([`span_rate`]).
    if let Some(r) = span_rate(t) {
        return Some(r);
    }
    // Too few frames for a span: the commonest duration, exactly.
    let mut counts = std::collections::BTreeMap::<u32, usize>::new();
    for d in t.durations.iter().filter(|d| **d > 0) {
        *counts.entry(*d).or_default() += 1;
    }
    let (d, _) = counts.into_iter().max_by_key(|(_, n)| *n)?;
    let (num, den) = (u64::from(t.timescale), u64::from(d));
    if num == 0 {
        return None;
    }
    let g = gcd(num, den).max(1);
    Some(Rational {
        num: i64::try_from(num / g).ok()?,
        den: u32::try_from(den / g).ok()?,
    })
}

/// The rates a recorder writes (and RenderPlan's NTSC forms).
const STANDARD_RATES: [(u64, u64); 9] = [
    (24, 1),
    (25, 1),
    (30, 1),
    (48, 1),
    (50, 1),
    (60, 1),
    (24000, 1001),
    (30000, 1001),
    (60000, 1001),
];

/// The mean rate over the presentation-time span: snapped to the nearest standard rate
/// when that is within 0.5 % and the snap moves the last frame by at most half a frame
/// (so addressing frames at the standard rate cannot drift from the sound), else the
/// measured mean exactly, which the planner reads by timestamps (decision 0017).
fn span_rate(t: &Track) -> Option<Rational> {
    let n = t.samples.dts.len();
    if n <= 2 || t.timescale == 0 {
        return None;
    }
    let pts = |i: usize| {
        t.samples
            .dts
            .get(i)
            .map(|d| d + i64::from(*t.samples.cts_off.get(i).unwrap_or(&0)))
    };
    let hi = (0..n).filter_map(pts).max()?;
    let lo = (0..n).filter_map(pts).min()?;
    let span = u64::try_from(hi - lo).ok().filter(|s| *s > 0)?;
    let ts = u64::from(t.timescale);
    let frames = (n - 1) as u64;
    let fps = ts as f64 * frames as f64 / span as f64;
    let secs = span as f64 / ts as f64;
    let snap = STANDARD_RATES
        .into_iter()
        .map(|(rn, rd)| {
            let r = rn as f64 / rd as f64;
            (rn, rd, (fps - r).abs() / r, (fps - r).abs() * secs)
        })
        .filter(|x| x.2 < 0.005 && x.3 <= 0.5)
        .min_by(|a, b| a.2.total_cmp(&b.2));
    if let Some((rn, rd, _, _)) = snap {
        return Some(Rational {
            num: rn as i64,
            den: rd as u32,
        });
    }
    let (num, den) = (frames * ts, span);
    let g = gcd(num, den).max(1);
    Some(Rational {
        num: i64::try_from(num / g).ok()?,
        den: u32::try_from(den / g).ok()?,
    })
}

fn track_secs(t: &Track) -> f64 {
    let ticks: u64 = t.durations.iter().map(|d| u64::from(*d)).sum();
    if t.timescale == 0 {
        0.0
    } else {
        ticks as f64 / f64::from(t.timescale)
    }
}

/// What the app knows about a file before decoding it.
#[derive(Debug, Clone)]
pub struct Probed {
    pub probe: Probe,
    pub dims: (u32, u32),
    pub fps: Rational,
    pub size: u64,
}

pub fn probe(path: &Path) -> AppResult<Probed> {
    let size = std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .ok_or_else(|| AppError::not_found("that is not a file"))?;
    let ix = crate::inputs::index_file(path).map_err(|_| {
        AppError::invalid(
            "gmclip cannot read that file (MP4, MOV, MKV or WebM with H.264 or HEVC video)",
        )
    })?;
    let v = ix
        .video
        .as_ref()
        .ok_or_else(|| AppError::invalid("that file has no video track"))?;
    let fps = track_fps(v).unwrap_or(Rational { num: 30, den: 1 });
    // The picture's length: a cut past the last video frame cannot render (an audio
    // track a few frames longer is common).
    // Frames, not the sum of sample durations: the last sample's duration often runs
    // past the last frame the decoder can present.
    let n = v.samples.pts_order.len() as f64;
    let secs = (n * f64::from(fps.den) / fps.num.max(1) as f64).min(track_secs(v));
    let probe = Probe {
        duration: Flicks((secs * FLICKS as f64).round() as i64),
        tracks: vec![TrackProbe {
            index: u32::try_from(v.id).unwrap_or(0),
            kind: TrackKind::Video,
            codec: codec_name(&v.codec).into(),
            width: Some(v.width),
            height: Some(v.height),
            fps: Some(fps),
            timescale: v.timescale,
            gop_max_s: None,
        }],
        audio_tracks: ix
            .audio_all
            .iter()
            .map(|a| AudioTrackProbe {
                index: u32::try_from(a.id).unwrap_or(0),
                codec: codec_name(&a.codec).into(),
                rate: a.sample_rate,
                layout: match a.channels {
                    1 => "mono".into(),
                    2 => "stereo".into(),
                    n => format!("{n}ch"),
                },
                bitrate: None,
                speech_ratio: None,
            })
            .collect(),
    };
    Ok(Probed {
        probe,
        dims: (v.width, v.height),
        fps,
        size,
    })
}

/// The output size of a framing for a source: the source's aspect at ≤ `max_w` wide
/// (even numbers, as encoders want).
pub fn canvas_for(dims: (u32, u32), max_w: u32) -> (u32, u32) {
    let (w, h) = (dims.0.max(2), dims.1.max(2));
    let scale = (f64::from(max_w) / f64::from(w)).min(1.0);
    let even = |x: f64| ((x / 2.0).round() as u32 * 2).max(2);
    (even(f64::from(w) * scale), even(f64::from(h) * scale))
}

/// Output fps for a framing: whole-number rates as they are, NTSC-style rates rounded to
/// the nearest whole number (the planner's frame grid is integer fps).
pub fn framing_fps(r: Rational) -> Rational {
    // a rate RenderPlan exports, NTSC exactly (decision 0020); a picture at another
    // converts (decision 0017)
    gmclip_core::planner::output_rate_for(r.num, r.den)
}

/// Flicks per frame of the framing rate a picture at `r` gets.
pub fn framing_fpf(r: Rational) -> i64 {
    gmclip_core::planner::framing_fpf(framing_fps(r)).unwrap_or(FLICKS / 30)
}

/// Opens a video: its project if it was opened before, else a new one.
pub fn open_video(core: &Arc<AppCore>, path: &Path) -> AppResult<Value> {
    open_video_with(core, path, None)
}

/// [`open_video`] with the mic the import sheet chose (`voice`: an audio track index):
/// it is set before transcription starts.
pub fn open_video_with(core: &Arc<AppCore>, path: &Path, voice: Option<u32>) -> AppResult<Value> {
    open_stream(core, path, voice, None)
}

/// A paired picture after probing: its file, probe, and `(from, offset)` flicks per
/// stretch (the first `from` is `i64::MIN`).
type PairedPicture = (PathBuf, Probed, Vec<(i64, i64)>);
/// Offset stretches: `(from group time, offset seconds)`.
type Offsets = Vec<(i64, f64)>;

/// A hi-res picture of the same recording: its file and its offset (seconds; the hi-res
/// time is the proxy's time minus this).
#[derive(Clone, Debug)]
pub struct HiRes {
    pub path: PathBuf,
    pub offset_s: f64,
    /// Piecewise instead: `(from_s, offset_s)` on the voice file's clock, in time order
    /// (the offset changes at `from_s`, e.g. The reference's −503 then −1642 frames at 1000 s).
    pub segments: Vec<(f64, f64)>,
}

/// [`open_video_with`], optionally pairing the file (the voice, the transcript's clock)
/// with a hi-res picture of the same recording ("Are these the same recording?"): the
/// cut's parts are on a sync group taking the picture from the hi-res file and the
/// sound from this one (SPEC §4.3; the reference's two-source reels).
pub fn open_stream(
    core: &Arc<AppCore>,
    path: &Path,
    voice: Option<u32>,
    hires: Option<HiRes>,
) -> AppResult<Value> {
    if !path.is_absolute() {
        return Err(AppError::invalid("an absolute path"));
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !VIDEO_EXTS.contains(&ext.as_str()) {
        return Err(AppError::invalid(format!(
            "gmclip opens {} files",
            VIDEO_EXTS.join(", ")
        )));
    }
    let path = path
        .canonicalize()
        .map_err(|_| AppError::not_found("no file there"))?;
    let pr = probe(&path)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Video".into());
    let mut recent = load_recent(core);
    // Reopen: same file, same size, project still there.
    if let Some(r) = recent
        .iter()
        .find(|r| r.path == path && r.size == pr.size && r.home.is_dir())
        .cloned()
        && let Ok(pj) = core.open_home(&r.home)
        && pj["cuts"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["id"] == r.cut.as_str()))
    {
        let pid = pj["id"].as_str().unwrap_or_default().to_owned();
        if let Some(v) = voice {
            crate::importsheet::set_voice(core, &pid, v)?;
        }
        touch(
            core,
            &mut recent,
            Recent {
                opened_ms: now_ms(),
                ..r.clone()
            },
        );
        core.analysis.ensure(core, &pid);
        core.proxies.ensure(core, &pid);
        // a project opens on its main cut, the stream edit holding every video (0036)
        let cut = main_cut(core, &pid)?.unwrap_or(r.cut);
        return Ok(json!({"project": pid, "cut": cut, "reopened": true, "name": r.name}));
    }
    let pj = core.create_project(&name)?;
    let pid = pj["id"].as_str().unwrap_or_default().to_owned();
    let home = PathBuf::from(pj["home"].as_str().unwrap_or_default());
    let hi = hires.map(|h| paired(h, &path)).transpose()?;
    let cut = seed_cut(core, &pid, &path, &name, &pr, hi, None)?;
    if let Some(v) = voice {
        crate::importsheet::set_voice(core, &pid, v)?;
    }
    touch(
        core,
        &mut recent,
        Recent {
            path: path.clone(),
            name: name.clone(),
            size: pr.size,
            home,
            project: pid.clone(),
            cut: cut.clone(),
            duration_s: pr.probe.duration.0 as f64 / FLICKS as f64,
            opened_ms: now_ms(),
        },
    );
    core.analysis.ensure(core, &pid);
    core.proxies.ensure(core, &pid);
    core.bus.home(crate::events::HomeEvent::Changed);
    Ok(json!({"project": pid, "cut": cut, "reopened": false, "name": name}))
}

/// A hi-res picture of the same recording as `voice`: checked, probed, and its offsets
/// on the picture's frame grid as `(from group time, offset)` flicks per stretch.
fn paired(h: HiRes, voice: &Path) -> AppResult<PairedPicture> {
    if !h.path.is_absolute() || !h.offset_s.is_finite() || h.offset_s.abs() > 86_400.0 {
        return Err(AppError::invalid(
            "the hi-res file's absolute path and an offset in seconds",
        ));
    }
    let hp = h
        .path
        .canonicalize()
        .map_err(|_| AppError::not_found("no hi-res file there"))?;
    if hp == voice {
        return Err(AppError::invalid("the hi-res picture is the same file"));
    }
    let hpr = probe(&hp)?;
    // on the picture's frame grid (the planner refuses an offset between frames)
    let fpf = framing_fpf(hpr.fps);
    let grid = |s: f64| (s * FLICKS as f64 / fpf as f64).round() as i64 * fpf;
    let segs: Vec<(i64, i64)> = if h.segments.is_empty() {
        vec![(i64::MIN, grid(h.offset_s))]
    } else {
        if h.segments.windows(2).any(|w| w[1].0 <= w[0].0)
            || h.segments
                .iter()
                .any(|x| !x.0.is_finite() || !x.1.is_finite() || x.1.abs() > 86_400.0)
        {
            return Err(AppError::invalid("offset changes in time order"));
        }
        h.segments
            .iter()
            .enumerate()
            .map(|(i, (from, off))| (if i == 0 { i64::MIN } else { grid(*from) }, grid(*off)))
            .collect()
    };
    Ok((hp, hpr, segs))
}

fn touch(core: &AppCore, list: &mut Vec<Recent>, r: Recent) {
    list.retain(|x| x.path != r.path);
    list.insert(0, r);
    list.truncate(MAX_RECENT);
    save_recent(core, list);
}

// ---------------------------------------------------------------------------------------
// Several videos in one project (AJ 2026-10-15: "i should be able to load multiple video
// files into one project")
// ---------------------------------------------------------------------------------------

/// One file of a multi-file open or add, with the mic the import sheet chose for it.
#[derive(Clone, Debug)]
pub struct AddFile {
    pub path: PathBuf,
    pub voice: Option<u32>,
}

/// At most this many files at once (a drop is capped at 16 too).
pub const MAX_FILES: usize = 16;

/// Two lengths that agree within 10 % ("Are these the same recording?", SPEC §5.1).
pub fn same_length(a: f64, b: f64) -> bool {
    a > 0.0 && b > 0.0 && (a - b).abs() <= 0.1 * a.max(b)
}

/// The import sheet for several files: each file's sheet, and the pairs whose lengths
/// agree within 10 % (asked "Are these the same recording?").
pub fn inspect_many(core: &AppCore, paths: &[String]) -> AppResult<Value> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(AppError::invalid(format!(
            "1 to {MAX_FILES} videos at once"
        )));
    }
    let mut files = Vec::new();
    let mut failed = Vec::new();
    for p in paths {
        match crate::importsheet::inspect(core, p) {
            Ok(v) => files.push(v),
            Err(e) => failed.push(json!({"path": p, "error": e.message})),
        }
    }
    let mut pairs = Vec::new();
    for i in 0..files.len() {
        for j in i + 1..files.len() {
            let (a, b) = (
                files[i]["duration_s"].as_f64().unwrap_or(0.0),
                files[j]["duration_s"].as_f64().unwrap_or(0.0),
            );
            if same_length(a, b) && files[i]["path"] != files[j]["path"] {
                pairs.push(json!([i, j]));
            }
        }
    }
    Ok(json!({"files": files, "pairs": pairs, "failed": failed}))
}

/// A file checked for opening: canonical, a video gmclip reads, probed.
fn checked(path: &Path) -> AppResult<(PathBuf, Probed, String)> {
    if !path.is_absolute() {
        return Err(AppError::invalid("an absolute path"));
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !VIDEO_EXTS.contains(&ext.as_str()) {
        return Err(AppError::invalid(format!(
            "gmclip opens {} files",
            VIDEO_EXTS.join(", ")
        )));
    }
    let path = path
        .canonicalize()
        .map_err(|_| AppError::not_found("no file there"))?;
    let pr = probe(&path)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Video".into());
    Ok((path, pr, name))
}

fn set_voice_of(core: &AppCore, project: &str, path: &Path, voice: u32) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let want = path.to_string_lossy();
    let src = doc
        .iter::<gmclip_model::entities::Source>()
        .find(|s| s.path == want)
        .ok_or_else(|| AppError::internal("no source"))?;
    core.call(
        project,
        "source_audio_roles_set",
        json!({"source": src.id.0.to_string(), "voice": voice}),
    )?;
    Ok(())
}

/// Opens several videos: one project holding them all (`one_project`, the default the
/// sheet offers), or a project each. `same` pairs (indexes into `files`) are the same
/// recording. `project_name` names the one project (default "<first> + N more"). Returns the
/// project and the cut to show (its main cut, decision 0036).
pub fn open_many(
    core: &Arc<AppCore>,
    files: &[AddFile],
    same: &[(usize, usize)],
    one_project: bool,
    project_name: Option<&str>,
) -> AppResult<Value> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(AppError::invalid(format!(
            "1 to {MAX_FILES} videos at once"
        )));
    }
    if !one_project || files.len() == 1 {
        let mut first: Option<Value> = None;
        for f in files {
            let r = open_video_with(core, &f.path, f.voice)?;
            first.get_or_insert(r);
        }
        return first.ok_or_else(|| AppError::internal("nothing opened"));
    }
    // check every file before making the project
    let (_, _, name) = checked(&files[0].path)?;
    for f in &files[1..] {
        checked(&f.path)?;
    }
    let title = project_name
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{name} + {} more", files.len() - 1));
    let pj = core.create_project(&title.chars().take(120).collect::<String>())?;
    let pid = pj["id"].as_str().unwrap_or_default().to_owned();
    let mut r = add_many(core, &pid, files, same)?;
    r["name"] = json!(title);
    r["reopened"] = json!(false);
    Ok(r)
}

/// Adds videos to an open project, each as a source appended whole to its main cut (a
/// same-recording pair as one sync group, heard once; the first video of a new project
/// makes the main cut), then their scrub proxies and analysis (transcription one source
/// at a time). Files already in the project are skipped and said.
pub fn add_many(
    core: &Arc<AppCore>,
    project: &str,
    files: &[AddFile],
    same: &[(usize, usize)],
) -> AppResult<Value> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(AppError::invalid(format!(
            "1 to {MAX_FILES} videos at once"
        )));
    }
    let p = core.project(project)?;
    let home = p.home.clone();
    let have: std::collections::BTreeSet<String> = p
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Source>()
        .map(|s| s.path.clone())
        .collect();
    let mut ok: Vec<Option<(PathBuf, Probed, String)>> = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for f in files {
        let c = checked(&f.path)?;
        let key = c.0.to_string_lossy().into_owned();
        if have.contains(&key) || !seen.insert(key) {
            skipped.push(json!({"path": c.0, "name": c.2, "why": "already in this project"}));
            ok.push(None);
        } else {
            ok.push(Some(c));
        }
    }
    // each file once in a pair; a pair is two files that were both added
    let mut partner: Vec<Option<usize>> = vec![None; files.len()];
    for &(i, j) in same {
        if i == j || i >= files.len() || j >= files.len() {
            return Err(AppError::invalid("pairs name two of the files"));
        }
        if partner[i].is_some() || partner[j].is_some() {
            return Err(AppError::invalid(
                "a file is the same recording as one other at most",
            ));
        }
        if ok[i].is_some() && ok[j].is_some() {
            partner[i] = Some(j);
            partner[j] = Some(i);
        }
    }
    let mut added = Vec::new();
    let mut notes = Vec::new();
    // Each "same recording" pair is measured first: the voice file is the smaller picture,
    // the other file the picture. A pair whose sound does not line up (two different
    // recordings of about the same length: AJ's streams) stays two videos, rather than one
    // file's picture over the other's voice at a guessed 0 s offset.
    let px = |p: &Probed| u64::from(p.dims.0) * u64::from(p.dims.1);
    let mut measured: Vec<Option<(usize, usize, crate::autosync::Proposal)>> =
        vec![None; files.len()];
    for i in 0..files.len() {
        let Some(j) = partner[i].filter(|j| *j > i) else {
            continue;
        };
        let (Some((ip, ipr, iname)), Some((jp, jpr, jname))) = (ok[i].clone(), ok[j].clone())
        else {
            continue;
        };
        let (voice_i, pic) = if px(&jpr) >= px(&ipr) { (i, j) } else { (j, i) };
        let (vp, pp) = if voice_i == i { (&ip, &jp) } else { (&jp, &ip) };
        match crate::autosync::propose(vp, files[voice_i].voice, pp) {
            Ok(m) if !m.stretches.is_empty() => measured[i] = Some((voice_i, pic, m)),
            r => {
                let why = match r {
                    Err(e) => e.message,
                    Ok(_) => "their sound does not line up".to_owned(),
                };
                notes.push(format!(
                    "“{iname}” and “{jname}” were added as separate videos: not the same recording ({why})"
                ));
                partner[i] = None;
                partner[j] = None;
            }
        }
    }
    let mut recent = load_recent(core);
    for i in 0..files.len() {
        let Some((path, pr, name)) = ok[i].clone() else {
            continue;
        };
        let hires = match partner[i] {
            Some(j) if j < i => continue, // added with its partner
            Some(_) => {
                let (voice_i, pic, m) = measured[i]
                    .take()
                    .ok_or_else(|| AppError::internal("pair"))?;
                let (vp, vpr, vname) = ok[voice_i]
                    .clone()
                    .ok_or_else(|| AppError::internal("pair"))?;
                let (pp, _, _) = ok[pic].clone().ok_or_else(|| AppError::internal("pair"))?;
                let h = HiRes {
                    path: pp.clone(),
                    offset_s: m.stretches[0].offset_s,
                    segments: if m.stretches.len() > 1 {
                        m.stretches.iter().map(|s| (s.from_s, s.offset_s)).collect()
                    } else {
                        Vec::new()
                    },
                };
                let hi = paired(h, &vp)?;
                Some((voice_i, vp, vpr, vname, hi))
            }
            None => None,
        };
        let (vi, vpath, vpr, vname, hi) = match hires {
            Some((vi, vp, vpr, vname, hi)) => (vi, vp, vpr, vname, Some(hi)),
            None => (i, path, pr, name, None),
        };
        let pic_path = hi.as_ref().map(|h| h.0.clone());
        let cut = seed_cut(core, project, &vpath, &vname, &vpr, hi, None)?;
        if let Some(v) = files[vi].voice {
            set_voice_of(core, project, &vpath, v)?;
        }
        for (fp, fsize) in std::iter::once((vpath.clone(), vpr.size)).chain(
            pic_path
                .iter()
                .filter_map(|pp| std::fs::metadata(pp).ok().map(|m| (pp.clone(), m.len()))),
        ) {
            let fname = fp
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            touch(
                core,
                &mut recent,
                Recent {
                    path: fp.clone(),
                    name: fname.clone(),
                    size: fsize,
                    home: home.clone(),
                    project: project.to_owned(),
                    cut: cut.clone(),
                    duration_s: vpr.probe.duration.0 as f64 / FLICKS as f64,
                    opened_ms: now_ms(),
                },
            );
            added.push(
                json!({"path": fp, "name": fname, "stream": cut, "paired": pic_path.is_some()}),
            );
        }
    }
    core.analysis.ensure(core, project);
    core.proxies.ensure(core, project);
    core.bus.home(crate::events::HomeEvent::Changed);
    core.bus
        .project(project, crate::events::ProjectEvent::Sources);
    let cut = main_cut(core, project)?;
    Ok(json!({"project": project, "cut": cut, "added": added, "skipped": skipped, "notes": notes}))
}

/// A recording's system audio under the mic (decision 0051): audible, 12 dB down.
pub const RECORDING_UNDER_DB: f64 = -12.0;

/// Opens a gmclip recording's files as one project (decision 0045): the face file (the
/// mic, the transcript's clock) paired with the first screen file at offset 0 (both
/// start at the session's `t0`, so nothing is measured), and any other screen as a video
/// of its own; the screen file's system audio plays under the mic at
/// [`RECORDING_UNDER_DB`] (decision 0051). Without a face file each screen is its own
/// video. Returns the project, its main cut and the files added.
pub fn open_recording(
    core: &Arc<AppCore>,
    title: &str,
    face: Option<&Path>,
    screens: &[PathBuf],
) -> AppResult<Value> {
    if screens.is_empty() {
        return Err(AppError::invalid("the recording has no screen file"));
    }
    let face = face.map(checked).transpose()?;
    let screens: Vec<(PathBuf, Probed, String)> = screens
        .iter()
        .map(|p| checked(p))
        .collect::<AppResult<_>>()?;
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if title.is_empty() {
        "Recording".to_owned()
    } else {
        title
    };
    let pj = core.create_project(&title.chars().take(120).collect::<String>())?;
    let pid = pj["id"].as_str().unwrap_or_default().to_owned();
    let home = core.project(&pid)?.home.clone();
    let mut recent = load_recent(core);
    let mut added = Vec::new();
    let mut seeded: Vec<(PathBuf, Probed, String, Option<PairedPicture>)> = Vec::new();
    let mut rest = screens.into_iter();
    if let Some((fp, fpr, fname)) = face {
        let (sp, _, _) = rest.next().ok_or_else(|| AppError::internal("screens"))?;
        let hi = paired(
            HiRes {
                path: sp,
                offset_s: 0.0,
                segments: Vec::new(),
            },
            &fp,
        )?;
        seeded.push((fp, fpr, fname, Some(hi)));
    }
    for (p, pr, n) in rest {
        seeded.push((p, pr, n, None));
    }
    // the cut is the recording's, not its first file's ("face", "screen-2")
    if let Some(first) = seeded.first_mut() {
        first.2.clone_from(&title);
    }
    for (vp, vpr, vname, hi) in seeded {
        let pic = hi.as_ref().map(|h| h.0.clone());
        let cut = seed_cut(core, &pid, &vp, &vname, &vpr, hi, Some(RECORDING_UNDER_DB))?;
        for fp in std::iter::once(vp.clone()).chain(pic.clone()) {
            let size = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
            let fname = fp
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            touch(
                core,
                &mut recent,
                Recent {
                    path: fp.clone(),
                    name: fname.clone(),
                    size,
                    home: home.clone(),
                    project: pid.clone(),
                    cut: cut.clone(),
                    duration_s: vpr.probe.duration.0 as f64 / FLICKS as f64,
                    opened_ms: now_ms(),
                },
            );
            added.push(json!({"path": fp, "name": fname, "stream": cut, "paired": pic.is_some()}));
        }
    }
    core.analysis.ensure(core, &pid);
    core.proxies.ensure(core, &pid);
    core.bus.home(crate::events::HomeEvent::Changed);
    core.bus.project(&pid, crate::events::ProjectEvent::Sources);
    let cut = main_cut(core, &pid)?;
    Ok(json!({"project": pid, "cut": cut, "added": added, "name": title, "reopened": false}))
}

/// `source_import` for agents (MCP parity): the holder has checked the file is inside the
/// agent read roots; it is added like a file the human added (probe, on the end of the
/// main cut, proxy, analysis queued).
#[derive(Debug)]
pub struct Importer(pub std::sync::Weak<AppCore>);

impl gmclip_mcp::holder::SourceImporter for Importer {
    fn import(
        &self,
        project: &str,
        path: &Path,
        caller: &gmclip_core::Caller,
    ) -> Result<Value, gmclip_proto::CmdError> {
        let core = self.0.upgrade().ok_or(gmclip_proto::CmdError::Internal {
            trace_id: "app-gone".into(),
        })?;
        // the agent's edits, in its run (decision 0050)
        let r = crate::core::acting_as(caller.clone(), || {
            add_many(
                &core,
                project,
                &[AddFile {
                    path: path.to_owned(),
                    voice: None,
                }],
                &[],
            )
        })
        .map_err(|e| gmclip_proto::CmdError::invalid("/args/path", &e.message))?;
        if let Some(why) = r["skipped"][0]["why"].as_str() {
            return Err(gmclip_proto::CmdError::invalid("/args/path", why));
        }
        let p = core
            .project(project)
            .map_err(|e| gmclip_proto::CmdError::invalid("/project", &e.message))?;
        let doc = p.handle.snapshot().doc.clone();
        let want = r["added"][0]["path"].as_str().unwrap_or_default();
        let s = doc
            .iter::<gmclip_model::entities::Source>()
            .find(|s| s.path == want)
            .ok_or(gmclip_proto::CmdError::NotFound)?;
        Ok(json!({
            "source": s.id.0.to_string(),
            "stream": r["added"][0]["stream"],
            "duration_ms": s.probe.duration.0 * 1000 / FLICKS,
            "display": format!("added a video ({} s) to the end of the project's main cut (its stream edit); transcribing in the background",
                               s.probe.duration.0 / FLICKS),
        }))
    }
}

/// The project's main cut: its stream edit, holding every video (decision 0036).
pub fn main_cut(core: &AppCore, project: &str) -> AppResult<Option<String>> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    Ok(gmclip_core::commands::topic::stream_edit(&doc).map(|c| c.0.to_string()))
}

/// After a video left the project: a same-recording partner it leaves behind becomes a
/// video of its own, appended to the main cut (heard on its own).
pub fn ensure_streams(core: &Arc<AppCore>, project: &str) -> AppResult<()> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    for (_, s) in crate::analysis::owners(&doc) {
        let (media, _) = gmclip_core::commands::longform::media_of_source(&doc, s.id);
        if gmclip_core::commands::topic::stream_edit_for(&doc, media).is_some() {
            continue;
        }
        if let gmclip_model::entities::MediaRef::Source(sid) = media {
            append_video(core, project, json!({"source": sid.0.to_string()}))?;
        }
    }
    core.analysis.ensure(core, project);
    Ok(())
}

/// Appends a video, whole, to the end of the project's main cut; a video of another
/// picture shape than the cut's first is fitted inside the frame (a layout of its own).
fn append_video(core: &AppCore, project: &str, media: Value) -> AppResult<String> {
    let r = core.call(project, "stream_edit_ensure", json!({"media": media}))?;
    let cut = r["output"]["cut"].as_str().unwrap_or_default().to_owned();
    for part in r["output"]["added"].as_array().into_iter().flatten() {
        if let Some(part) = part.as_str() {
            crate::framing::fit_new_part(core, project, &cut, part)?;
        }
    }
    Ok(cut)
}

/// Removes a video (core `source_remove`, refused while cuts play it), then makes sure
/// every video left is on the main cut.
pub fn remove_source(core: &Arc<AppCore>, project: &str, source: &str) -> AppResult<Value> {
    let gone = crate::planner::ulid_id::<gmclip_model::entities::Source>(source)
        .ok()
        .and_then(|sid| core.project(project).ok()?.handle.snapshot().doc.get(sid).map(|s| PathBuf::from(&s.path)));
    let r = core.call(project, "source_remove", json!({"source": source}))?;
    ensure_streams(core, project)?;
    // Home's card stops counting it
    if let Some(gone) = gone {
        let mut recent = load_recent(core);
        let n = recent.len();
        recent.retain(|r| !(r.project == project && r.path == gone));
        if recent.len() != n {
            save_recent(core, &recent);
        }
    }
    core.bus
        .project(project, crate::events::ProjectEvent::Sources);
    core.bus.home(crate::events::HomeEvent::Changed);
    Ok(r["output"].clone())
}

/// Points a video whose file moved at its new place: the new file must be the same
/// length (±2 %) and picture shape: the same size, or a sharper master of the same
/// aspect (a 720p edit copy relinked to its 4K original). Layout is in fractions of the
/// picture, so crops, places and overlays carry over unchanged.
pub fn relink_source(
    core: &Arc<AppCore>,
    project: &str,
    source: &str,
    path: &Path,
) -> AppResult<Value> {
    let (path, pr, _) = checked(path)?;
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let sid = crate::planner::ulid_id::<gmclip_model::entities::Source>(source)?;
    let s = doc
        .get(sid)
        .ok_or_else(|| AppError::not_found("no such video in this project"))?;
    let old = s.probe.duration.0 as f64 / FLICKS as f64;
    let new = pr.probe.duration.0 as f64 / FLICKS as f64;
    let dims = s
        .probe
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .and_then(|t| Some((t.width?, t.height?)));
    // a source never probed (added before its file was found)
    // takes the file as it is
    let unprobed = s.probe.duration.0 <= 0 && dims.is_none();
    let same_shape = |d: (u32, u32)| {
        u64::from(d.0) * u64::from(pr.dims.1) == u64::from(d.1) * u64::from(pr.dims.0)
    };
    if !unprobed && ((old - new).abs() > 0.02 * old.max(1.0) || dims.is_some_and(|d| !same_shape(d)))
    {
        return Err(AppError::invalid(format!(
            "that is a different video ({:.0} s, {}×{}; this one is {:.0} s{})",
            new,
            pr.dims.0,
            pr.dims.1,
            old,
            dims.map_or(String::new(), |d| format!(", {}×{}", d.0, d.1))
        )));
    }
    core.call(
        project,
        "source_relink",
        json!({"source": source, "path": path.to_string_lossy(),
               "fingerprint": gmclip_core::planner::fingerprint(None, Some(pr.size)),
               "probe": pr.probe}),
    )?;
    core.proxies.ensure(core, project);
    core.analysis.ensure(core, project);
    // Home's card for this video follows it to the new file
    let old_path = PathBuf::from(&s.path);
    let mut recent = load_recent(core);
    if let Some(r) = recent.iter_mut().find(|r| r.project == project && r.path == old_path) {
        r.name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.name.clone());
        r.size = pr.size;
        r.path = path.clone();
        save_recent(core, &recent);
    }
    core.bus
        .project(project, crate::events::ProjectEvent::Sources);
    core.bus.home(crate::events::HomeEvent::Changed);
    Ok(json!({"relinked": true, "path": path}))
}

/// The source, cut, a framing in the source's shape (16:9 for a landscape video), one
/// full-frame layer and one whole part. One undo
/// group per command (the cut is new; undoing it piece by piece is fine).
fn seed_cut(
    core: &Arc<AppCore>,
    pid: &str,
    path: &Path,
    name: &str,
    pr: &Probed,
    hires: Option<PairedPicture>,
    under_db: Option<f64>,
) -> AppResult<String> {
    let out = |v: Value| v["output"].clone();
    let id = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let src = out(core.call(
        pid,
        "source_add",
        json!({"path": path.to_string_lossy(),
               "fingerprint": gmclip_core::planner::fingerprint(None, Some(pr.size)),
               "probe": pr.probe}),
    )?);
    let source = id(&src, "id");
    // A hi-res picture of the same recording: its own source, then a sync group (this
    // file is the clock and the sound; the hi-res file the picture, `offset` later).
    let mut cuts_at: Vec<i64> = Vec::new();
    let (media, pic, range) = match &hires {
        Some((hp, hpr, segs)) => {
            let hs = out(core.call(
                pid,
                "source_add",
                json!({"path": hp.to_string_lossy(),
                       "fingerprint": gmclip_core::planner::fingerprint(None, Some(hpr.size)),
                       "probe": hpr.probe}),
            )?);
            let mut pic_member =
                json!({"source": id(&hs, "id"), "role": "picture", "offset": segs[0].1});
            if segs.len() > 1 {
                pic_member["segments"] = json!(segs
                    .iter()
                    .map(|(from, off)| json!({"from": if *from == i64::MIN { 0 } else { *from }, "offset": off}))
                    .collect::<Vec<_>>());
            }
            // a recording's screen file carries the system audio (decision 0051)
            if let Some(db) = under_db {
                pic_member["under_db"] = json!(db);
            }
            let g = out(core.call(
                pid,
                "sync_group_create",
                json!({"members": [
                    {"source": source, "role": "audio", "offset": 0},
                    pic_member]}),
            )?);
            // the group time both files cover (the first offset at the start, the last at
            // the end); parts may not straddle an offset change
            let (first, last) = (segs[0].1, segs[segs.len() - 1].1);
            let a = first.max(0);
            let b = pr.probe.duration.0.min(last + hpr.probe.duration.0);
            cuts_at = segs
                .iter()
                .skip(1)
                .map(|x| x.0)
                .filter(|t| *t > a && *t < b)
                .collect();
            if b <= a {
                return Err(AppError::invalid(
                    "with that offset the two files do not overlap",
                ));
            }
            (json!({"group": id(&g, "group")}), hpr, (a, b))
        }
        None => (json!({"source": source}), pr, (0, pr.probe.duration.0)),
    };
    // a project with its main cut: this video goes on the end of it (decision 0036)
    if main_cut(core, pid)?.is_some() {
        return append_video(core, pid, media);
    }
    let title: String = name.chars().take(200).collect();
    let cut = id(
        &out(core.call(pid, "cut_create", json!({"title": title}))?),
        "cut",
    );
    // Captions off until the model planner builds heard captions (A.09); the transcript
    // pane works without them.
    core.call(pid, "caption_set", json!({"cut": cut, "enabled": false}))?;
    crate::studio::seed_kit(core, pid);
    let fps = framing_fps(pic.fps);
    let fpf = framing_fpf(pic.fps);
    let (w, h) = canvas_for(pic.dims, 1920);
    let framing = id(
        &out(core.call(
            pid,
            "framing_create",
            json!({"cut": cut, "name": if h > w { "9:16" } else { "16:9" }, "w": w, "h": h, "fps": {"num": fps.num, "den": fps.den}}),
        )?),
        "id",
    );
    if h > w {
        // a portrait video is a Short: AJ's kit loudness (static −16 LUFS, −1.5 dBTP)
        core.call(
            pid,
            "framing_set",
            json!({"framing": framing, "loudness": {"static": {"integrated_lufs": -16.0, "true_peak_dbtp": -1.5}}}),
        )?;
    }
    core.call(
        pid,
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"},
               "crop": {"x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0},
               "place": {"x": 0.0, "y": 0.0, "w": 1.0}, "z": 0}),
    )?;
    // The whole video (both files, when paired), on the output frame grid.
    let k0 = (range.0 + fpf - 1).div_euclid(fpf);
    let k1 = range.1.div_euclid(fpf);
    let (src_in, src_out) = (Flicks(k0 * fpf), Flicks(k1.max(k0 + 1) * fpf));
    // one part per stretch between offset changes (whole frames)
    let mut edges = vec![src_in.0];
    for t in &cuts_at {
        edges.push(t.div_euclid(fpf) * fpf);
    }
    edges.push(src_out.0);
    edges.dedup();
    let mut after: Option<String> = None;
    for w in edges.windows(2) {
        if w[1] <= w[0] {
            continue;
        }
        let mut args = json!({"cut": cut, "media": media, "src_in": w[0], "src_out": w[1]});
        if let Some(a) = &after {
            args["after"] = json!(a);
        } else {
            args["role"] = json!("hook");
        }
        let r = core.call(pid, "part_add", args)?;
        after = r["output"]["id"].as_str().map(str::to_owned);
    }
    // The whole video starts the project's stream edit, its main cut (decisions 0011,
    // 0036): later videos are appended to it, topics labelled and fluff struck on it,
    // long-forms and shorts made from it.
    core.call(pid, "stream_edit_ensure", json!({"adopt": cut}))?;
    Ok(cut)
}

/// Recent videos for Home (rows whose file is gone are marked, not dropped).
pub fn recent_json(core: &AppCore) -> Value {
    let rows: Vec<Value> = load_recent(core)
        .into_iter()
        .map(|r| {
            json!({
                "path": r.path, "name": r.name, "project": r.project, "cut": r.cut,
                "duration_s": (r.duration_s * 10.0).round() / 10.0, "opened_ms": r.opened_ms,
                "missing": !r.path.is_file(),
            })
        })
        .collect();
    json!({"videos": rows, "projects": recent_projects(core)})
}

/// Recent projects for Home: the recent videos grouped by project, newest first. Each
/// card names the project (its folder's name), counts its videos, sums their lengths,
/// and shows its first video; opening it opens the project on its main cut (decision
/// 0036).
fn recent_projects(core: &AppCore) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<Recent>)> = Vec::new();
    for r in load_recent(core) {
        match groups.iter_mut().find(|(p, _)| *p == r.project) {
            Some((_, v)) => v.push(r),
            None => groups.push((r.project.clone(), vec![r])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(project, mut v)| {
            // the order the videos were added (an import stamps them in order)
            v.sort_by_key(|r| r.opened_ms);
            let latest = v.iter().max_by_key(|r| r.opened_ms)?.clone();
            let first = v.iter().find(|r| r.path.is_file()).unwrap_or(&v[0]);
            // the project's own name (its folder's `owner.json`), else the folder's
            let name = std::fs::read(latest.home.join("owner.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v["name"].as_str().map(str::to_owned))
                .or_else(|| {
                    latest
                        .home
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| latest.name.clone());
            Some(json!({
                "project": project,
                "name": name,
                "videos": v.len(),
                "duration_s": (v.iter().map(|r| r.duration_s).sum::<f64>() * 10.0).round() / 10.0,
                "opened_ms": latest.opened_ms,
                // what the card shows, and the file that reopens it
                "thumb": first.path,
                "open": v.iter().rev().find(|r| r.path.is_file()).map(|r| r.path.clone()),
                "missing": v.iter().all(|r| !r.path.is_file()),
            }))
        })
        .collect()
}

/// The native "Open video…" panel (main thread only). `None` when cancelled.
#[cfg(target_os = "macos")]
pub fn pick_on_main_thread() -> Option<PathBuf> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setMessage(Some(&NSString::from_str("Choose a video to edit")));
    panel.setPrompt(Some(&NSString::from_str("Open")));
    let exts: Vec<_> = VIDEO_EXTS.iter().map(|e| NSString::from_str(e)).collect();
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&exts)));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URLs().firstObject()?;
    let p = url.path()?;
    Some(PathBuf::from(p.to_string()))
}

/// The system file dialog for one video (W.9). `None` when cancelled.
#[cfg(windows)]
pub fn pick_on_main_thread() -> Option<PathBuf> {
    gmclip_platform::windows::open_file_dialog(
        "Choose a video to edit",
        "Videos",
        VIDEO_EXTS,
        false,
    )
    .ok()?
    .into_iter()
    .next()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn pick_on_main_thread() -> Option<PathBuf> {
    None
}

/// The native panel with several files allowed ("Add video…"). Empty when cancelled.
#[cfg(target_os = "macos")]
pub fn pick_many_on_main_thread() -> Vec<PathBuf> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};
    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(true);
    panel.setMessage(Some(&NSString::from_str("Choose videos to add")));
    panel.setPrompt(Some(&NSString::from_str("Add")));
    let exts: Vec<_> = VIDEO_EXTS.iter().map(|e| NSString::from_str(e)).collect();
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&exts)));
    if panel.runModal() != NSModalResponseOK {
        return Vec::new();
    }
    panel
        .URLs()
        .iter()
        .filter_map(|u| u.path().map(|p| PathBuf::from(p.to_string())))
        .take(MAX_FILES)
        .collect()
}

/// The system file dialog with several videos allowed (W.9). Empty when cancelled.
#[cfg(windows)]
pub fn pick_many_on_main_thread() -> Vec<PathBuf> {
    gmclip_platform::windows::open_file_dialog("Choose videos to add", "Videos", VIDEO_EXTS, true)
        .map(|v| v.into_iter().take(MAX_FILES).collect())
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn pick_many_on_main_thread() -> Vec<PathBuf> {
    Vec::new()
}

/// The hi-res picture member of a cut's sync group and its offsets: `(member, [(from
/// group time, offset seconds)])` in time order (one entry unless piecewise).
pub fn picture_offsets(
    core: &AppCore,
    project: &str,
    cut: &str,
) -> AppResult<Option<(String, Offsets)>> {
    use gmclip_model::entities::{Part, SyncGroup, SyncMember, SyncSegment};
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut)?;
    let group = gmclip_model::clock::cut_layout(&doc, cid)
        .iter()
        .filter_map(|sp| doc.get::<Part>(sp.part))
        .find_map(|pt| match pt.media {
            gmclip_model::entities::MediaRef::Group(g) => Some(g),
            gmclip_model::entities::MediaRef::Source(_) => None,
        });
    let Some(g) = group.and_then(|g| doc.get::<SyncGroup>(g)) else {
        return Ok(None);
    };
    let Some(m) = g.picture.and_then(|m| doc.get::<SyncMember>(m)) else {
        return Ok(None);
    };
    let mut segs: Vec<(i64, f64)> = doc
        .children::<SyncSegment>(m.id.entity())
        .filter_map(|s| doc.get(s))
        .map(|s| (s.src_range.start.0, s.offset.0 as f64 / FLICKS as f64))
        .collect();
    segs.sort_by_key(|x| x.0);
    Ok(Some((m.id.0.to_string(), segs)))
}

/// The first segment's offset (seconds), for callers that want one number.
pub fn picture_offset(
    core: &AppCore,
    project: &str,
    cut: &str,
) -> AppResult<Option<(String, f64)>> {
    Ok(picture_offsets(core, project, cut)?.and_then(|(m, v)| v.first().map(|x| (m, x.1))))
}

/// For the Inspector: each offset stretch with where it starts on the cut (output frame;
/// `null` when the cut does not play that moment).
pub fn picture_offsets_view(core: &AppCore, project: &str, cut: &str) -> Option<Value> {
    let (_, segs) = picture_offsets(core, project, cut).ok()??;
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut).ok()?;
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    let rows: Vec<Value> = segs
        .iter()
        .map(|(from, off)| {
            let f = layout.iter().find_map(|sp| {
                let pt: &gmclip_model::entities::Part = doc.get(sp.part)?;
                let t = gmclip_model::Flicks((*from).max(pt.src_in.0));
                (t <= pt.src_out)
                    .then(|| gmclip_model::clock::src_to_cut(&doc, sp.part, t))
                    .flatten()
            });
            json!({"from_s": (*from).max(0) as f64 / FLICKS as f64, "offset_s": off,
                   "from_frame": f.map(|c| c.0 / fpf)})
        })
        .collect();
    Some(json!(rows))
}

/// Moves the hi-res picture against the sound (`offset_s` = hi-res time is the proxy's
/// time minus this) for the stretch under output frame `frame` (the only one when the
/// offset is not piecewise); `split` starts a new stretch there instead ("the offset
/// changes here"). One undoable step.
pub fn set_picture_offset(
    core: &AppCore,
    project: &str,
    cut: &str,
    offset_s: f64,
    frame: Option<i64>,
    split: bool,
) -> AppResult<Value> {
    if !offset_s.is_finite() || offset_s.abs() > 86_400.0 {
        return Err(AppError::invalid("an offset in seconds"));
    }
    let (member, _) = picture_offset(core, project, cut)?
        .ok_or_else(|| AppError::invalid("this cut has no hi-res picture paired"))?;
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan))
        .max(1);
    // whole frames of the cut's rate
    let off = (offset_s * FLICKS as f64 / fpf as f64).round() as i64 * fpf;
    let mut args = json!({"member": member, "offset": off});
    if let Some(k) = frame {
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut)?;
        let (_, t) = gmclip_model::clock::cut_to_src(&doc, cid, gmclip_model::Flicks(k * fpf))
            .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
        args["at"] = json!(t.0);
        if split {
            // the offset changes on a part edge: split the part there in the same step
            args["split"] = json!(true);
        }
    } else if split {
        return Err(AppError::invalid("where the offset changes"));
    }
    if split {
        // split the part there and start the new offset, in one transaction (one undo)
        let p = core.project(project)?;
        let doc = p.handle.snapshot().doc.clone();
        let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut)?;
        let t = frame.unwrap_or(0) * fpf;
        let (pid, src) = gmclip_model::clock::cut_to_src(&doc, cid, gmclip_model::Flicks(t))
            .ok_or_else(|| AppError::invalid("that frame is not in the cut"))?;
        let pt: &gmclip_model::entities::Part =
            doc.get(pid).ok_or_else(|| AppError::internal("part"))?;
        let mut ops = Vec::new();
        if src.0 > pt.src_in.0 {
            ops.push(json!({"op": "part_split", "part": pid.0.to_string(), "at": src.0}));
        }
        let mut op = args.clone();
        op["op"] = json!("sync_offset_set");
        ops.push(op);
        return core.call(project, "cut_edit", json!({"cut": cut, "ops": ops}));
    }
    core.call(project, "sync_offset_set", args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_and_fps() {
        assert_eq!(canvas_for((2560, 1440), 1920), (1920, 1080));
        assert_eq!(canvas_for((1280, 720), 1920), (1280, 720));
        assert_eq!(canvas_for((1080, 1920), 1920), (1080, 1920));
        // NTSC exactly (decision 0020); 120 converts into 60
        assert_eq!(
            framing_fps(Rational {
                num: 30000,
                den: 1001
            }),
            Rational {
                num: 30000,
                den: 1001
            }
        );
        assert_eq!(
            framing_fps(Rational { num: 120, den: 1 }),
            Rational { num: 60, den: 1 }
        );
        assert_eq!(
            framing_fpf(Rational {
                num: 30000,
                den: 1001
            }),
            23_543_520
        );
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    /// A video track with these presentation times (decode order = presentation order).
    fn track(timescale: u32, pts: &[i64]) -> Track {
        let mut t = Track {
            id: 1,
            kind: gmclip_media::demux::TrackKind::Video,
            codec: Codec::H264 { avcc: Vec::new() },
            timescale,
            width: 64,
            height: 64,
            sample_rate: 0,
            channels: 0,
            colour: Default::default(),
            samples: Default::default(),
            durations: Vec::new(),
            pts_unknown: Vec::new(),
            block_pos: Vec::new(),
            edits: Vec::new(),
            movie_timescale: 1000,
        };
        t.samples.dts = pts.to_vec();
        t.samples.cts_off = vec![0; pts.len()];
        t.durations = pts.windows(2).map(|w| (w[1] - w[0]) as u32).collect();
        t
    }

    fn r(t: &Track) -> (i64, u32) {
        let x = track_fps(t).unwrap();
        (x.num, x.den)
    }

    #[test]
    fn rates_snap_on_any_clock_without_drift() {
        // AJ's restream: 10 kHz clock, 330–340 tick frames averaging 30 fps
        let pts: Vec<i64> = (0..9001).map(|i| (i * 10_000 + 15) / 30).collect();
        assert_eq!(r(&track(10_000, &pts)), (30, 1));
        // Matroska 1 ms at 29.97: 29.97, not 30 (the nearer standard rate)
        let pts: Vec<i64> = (0..3000).map(|i| (i * 1001 + 15) / 30).collect();
        assert_eq!(r(&track(1000, &pts)), (30000, 1001));
        // exact clocks
        let pts: Vec<i64> = (0..100).map(|i| i * 1001).collect();
        assert_eq!(r(&track(24_000, &pts)), (24000, 1001));
        let pts: Vec<i64> = (0..100).map(|i| i * 256).collect();
        assert_eq!(r(&track(15_360, &pts)), (60, 1));
        // a true 30.03 fps over an hour is not 30 (a 4 s drift): kept as measured
        let pts: Vec<i64> = (0..108_108).map(|i| i * 333).collect();
        let (n, d) = r(&track(10_000, &pts));
        assert!(
            (n as f64 / f64::from(d) - 10_000.0 / 333.0).abs() < 1e-9,
            "{n}/{d}"
        );
        // a 120 fps capture is 120 (it converts into a 60 fps cut)
        let pts: Vec<i64> = (0..600).map(|i| i * 128).collect();
        assert_eq!(r(&track(15_360, &pts)), (120, 1));
    }
}
