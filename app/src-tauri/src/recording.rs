//! Recording in the app (decision 0045, SPEC §5.18): Settings › Recording, the Record
//! sheet, and opening a finished recording as a project.
//!
//! - `recording_status`: the setting, the five permissions, and the recording running
//!   now (its folder, time, markers, what is missing, and whether ⌃⌥S asked it to stop).
//! - `recording_enable {on}`: the setting, asking for nothing.
//! - `recording_ask {kind}`: asks for one permission. Settings asks for them one at a
//!   time, in order (Screen Recording, Accessibility and Input Monitoring answer at once,
//!   so asking for all of them together stacks their prompts).
//! - `recording_sources`: displays, windows, cameras and mics.
//! - `recording_preview {on, camera?}`: the Record sheet's camera preview, in the camera's
//!   own mode (as the face file records it); its pictures are
//!   `gm://camera/frame?after=<seq>`.
//! - `recording_prepare {…}` starts every stream warm (the sheet's countdown runs
//!   meanwhile), `recording_go` starts the files, `recording_marker` drops a marker,
//!   `recording_stop` closes the files and opens the session as a project,
//!   `recording_cancel` drops a prepared or running recording and its folder.
//! - `recording_open {dir}`: opens a session folder made earlier (by `gmclip record`, or
//!   one a crash left).
//!
//! Opening a session: the face file and the first screen file are one recording at
//! offset 0 (`video::open_recording`), and each screen file gets its input track and
//! cursor images as assets plus `source_cursor_set`, so the cursor the recording hid is
//! drawn from the log.

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// Where recordings go unless the sheet says otherwise: `~/Movies/gmclip/Recordings`.
fn default_parent(core: &AppCore) -> PathBuf {
    core.dirs
        .library
        .parent()
        .map_or_else(|| core.dirs.library.clone(), Path::to_path_buf)
        .join("Recordings")
}

fn enabled(core: &AppCore) -> bool {
    crate::uiprefs::get(core)["recording"]
        .as_bool()
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------
// opening a session
// ---------------------------------------------------------------------------------------

/// Stores bytes as an asset of `media_type` (by sha256) and registers it in `project`.
fn store_asset(
    core: &AppCore,
    project: &str,
    name: &str,
    bytes: &[u8],
    media_type: &str,
    size: Option<(u32, u32)>,
) -> AppResult<String> {
    let sha = gmclip_core::profile::style::sha256_hex(bytes);
    let path = core
        .fonts()
        .asset_file_typed(&sha, media_type)
        .ok_or_else(|| AppError::internal("no assets folder"))?;
    if !path.is_file() {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| {
                AppError::internal(format!("making the assets folder: {}", e.kind()))
            })?;
        }
        crate::core::write_atomic(&path, bytes)
            .map_err(|e| AppError::internal(format!("storing {name}: {}", e.kind())))?;
    }
    let out = core.call(project, "asset_add", {
        let mut a =
            json!({"name": name, "sha256": sha, "size": bytes.len(), "media_type": media_type});
        // A picture's size: the planner draws cursor images at it.
        if let Some((w, h)) = size {
            a["probe"] = json!({"w": w, "h": h});
        }
        a
    })?;
    Ok(out["output"]["id"].as_str().unwrap_or_default().to_owned())
}

/// Gives the screen source at `file` its input track and cursor images.
fn attach_cursor(
    core: &AppCore,
    project: &str,
    t: &gmclip_media::rec::ScreenTrack,
) -> AppResult<()> {
    let want = t
        .file
        .canonicalize()
        .unwrap_or_else(|_| t.file.clone())
        .to_string_lossy()
        .into_owned();
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let Some(src) = doc
        .iter::<gmclip_model::entities::Source>()
        .find(|s| s.path == want)
    else {
        return Err(AppError::internal("the screen file is not in the project"));
    };
    let mut shapes = Vec::new();
    for (i, f) in t.shape_files.iter().enumerate() {
        let png = std::fs::read(f).map_err(|_| AppError::not_found("a cursor image is missing"))?;
        // The recorder stored it decoded and re-encoded already; still checked here.
        let img = gmclip_media::image::decode_untrusted(&png)
            .map_err(|e| AppError::invalid(e.to_string()))?;
        shapes.push(store_asset(
            core,
            project,
            &format!("cursor {i}.png"),
            &png,
            "image/png",
            Some((img.width, img.height)),
        )?);
    }
    t.track.check().map_err(AppError::invalid)?;
    let bytes = serde_json::to_vec(&t.track).map_err(|e| AppError::internal(e.to_string()))?;
    let name = format!(
        "{} input",
        t.file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let track = store_asset(
        core,
        project,
        &name,
        &bytes,
        gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE,
        None,
    )?;
    core.call(
        project,
        "source_cursor_set",
        json!({"source": src.id.0.to_string(), "track": track, "shapes": shapes}),
    )?;
    Ok(())
}

/// Stores the markers dropped while recording (µs from `t0`, every file's time 0) on
/// each of the session's files (decision 0052).
fn attach_markers(
    core: &AppCore,
    project: &str,
    files: &[PathBuf],
    markers_us: &[i64],
) -> AppResult<()> {
    if markers_us.is_empty() {
        return Ok(());
    }
    let ms: Vec<i64> = markers_us.iter().map(|t| (t + 500).div_euclid(1000)).collect();
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    for f in files {
        let want = f
            .canonicalize()
            .unwrap_or_else(|_| f.clone())
            .to_string_lossy()
            .into_owned();
        if let Some(src) = doc
            .iter::<gmclip_model::entities::Source>()
            .find(|s| s.path == want)
        {
            core.call(
                project,
                "source_markers_set",
                json!({"source": src.id.0.to_string(), "markers_ms": ms}),
            )?;
        }
    }
    Ok(())
}

/// Opens a session folder as a project.
pub fn open(core: &Arc<AppCore>, dir: &Path) -> AppResult<Value> {
    if !dir.is_absolute() {
        return Err(AppError::invalid("an absolute folder"));
    }
    let session =
        gmclip_media::rec::Session::read(dir).map_err(|e| AppError::invalid(e.to_string()))?;
    // Cursor metadata is optional: a truncated event log must not hide valid video.
    let mut notes = session.missing.clone();
    let tracks = match gmclip_media::rec::tracks(dir) {
        Ok(tracks) => tracks,
        Err(_) => {
            notes.push("The cursor log could not be read; your video files are still available.".into());
            Vec::new()
        }
    };
    let screens: Vec<PathBuf> = session.screens.iter().map(|s| dir.join(&s.file)).collect();
    let face = session.face.as_ref().map(|f| dir.join(&f.file));
    // A crash leaves the files fragmented, the editor reads plain movies; and it leaves
    // the apps the recorder woke for their focused field in accessibility mode.
    #[cfg(target_os = "macos")]
    if !session.complete {
        for f in screens.iter().chain(face.iter()) {
            let _ = gmclip_media::rec::cap::finalize(f);
        }
        gmclip_media::rec::restore_switches_left(dir);
    }
    let mut r = crate::video::open_recording(core, &session.name, face.as_deref(), &screens)?;
    let pid = r["project"].as_str().unwrap_or_default().to_owned();
    let dropped = session.screens.iter().map(|s| s.dropped)
        .chain(session.face.iter().map(|f| f.dropped)).fold(0, u64::saturating_add);
    if dropped > 0 {
        notes.push(format!("Capture fell behind ({dropped} samples dropped). For the next take, use 30 fps or record a smaller area."));
    }
    for t in &tracks {
        if let Err(e) = attach_cursor(core, &pid, t) {
            notes.push(format!("{}: no cursor ({})", t.file.display(), e.message));
        }
    }
    // every file's track holds the session's markers; the face file has none of its own
    if let Some(t) = tracks.first() {
        let files: Vec<PathBuf> = screens.iter().chain(face.iter()).cloned().collect();
        if let Err(e) = attach_markers(core, &pid, &files, &t.track.markers) {
            notes.push(format!("the markers were not added ({})", e.message));
        }
    }
    if session.face.is_some()
        && let Some(cut) = r["cut"].as_str()
        && let Err(e) = crate::screenstudio::camera_bubble(core, &pid, cut)
    {
        notes.push(format!("the camera is not placed ({})", e.message));
    }
    if session.face.is_some() && session.screens.len() > 1 {
        notes.push("the other displays were added as videos of their own".into());
    }
    r["notes"] = json!(notes);
    r["session"] = json!(dir);
    Ok(r)
}

// ---------------------------------------------------------------------------------------
// recording (macOS)
// ---------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicBool, Ordering};

    use gmclip_media::rec::{Hotkey, RecordOptions, Recording, cap};

    use super::*;

    struct Active {
        rec: Recording,
        stop_asked: Arc<AtomicBool>,
        started: bool,
    }

    static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

    fn active() -> AppResult<std::sync::MutexGuard<'static, Option<Active>>> {
        ACTIVE
            .lock()
            .map_err(|_| AppError::internal("the recorder is stuck"))
    }

    fn perms() -> Value {
        let p = cap::permissions();
        let mut o = serde_json::Map::new();
        for k in cap::PermKind::ALL {
            o.insert(
                k.as_str().into(),
                json!({"state": p.get(k).as_str(), "settings": k.settings_url()}),
            );
        }
        Value::Object(o)
    }

    pub fn status(core: &AppCore) -> AppResult<Value> {
        let g = active()?;
        let rec = g.as_ref().map(|a| {
            json!({
                "dir": a.rec.dir(),
                "started": a.started,
                "elapsed_s": a.rec.elapsed().as_secs_f64(),
                "markers": a.rec.markers(),
                "missing": a.rec.missing(),
                "stop_asked": a.stop_asked.load(Ordering::Acquire),
                "trouble": a.rec.trouble(),
                "dropped": a.rec.dropped(),
            })
        });
        Ok(json!({
            "enabled": enabled(core),
            "permissions": perms(),
            "bundled": bundled(),
            "recording": rec,
            "folder": default_parent(core),
        }))
    }

    /// Running inside `gmclip.app`. macOS asks for the camera and mic on behalf of the
    /// app that launched a bare binary (Terminal, bb…), which may never show the sheet.
    fn bundled() -> bool {
        std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
    }

    pub fn enable(core: &AppCore, on: bool) -> AppResult<Value> {
        crate::uiprefs::put(core, &json!({"recording": on}))?;
        status(core)
    }

    /// Asks for one permission. macOS shows its prompt once: one refused (or asked
    /// before, `settings`) opens its System Settings pane instead.
    pub fn ask(core: &AppCore, kind: &str, settings: bool) -> AppResult<Value> {
        let k = cap::PermKind::ALL
            .into_iter()
            .find(|k| k.as_str() == kind)
            .ok_or_else(|| AppError::invalid(format!("no permission {kind}")))?;
        let refused = matches!(k, cap::PermKind::Camera | cap::PermKind::Microphone)
            && cap::permissions().get(k) == cap::Perm::Denied;
        if settings || refused {
            std::process::Command::new("/usr/bin/open")
                .arg(k.settings_url())
                .status()
                .map_err(|e| AppError::internal(format!("opening System Settings: {e}")))?;
        } else {
            cap::request(k).map_err(|e| AppError::invalid(e.to_string()))?;
        }
        status(core)
    }

    static PREVIEW: Mutex<Option<cap::CameraPreview>> = Mutex::new(None);

    /// Starts (or, `on` false, stops) the camera preview. Starting again changes camera.
    pub fn preview(camera: Option<&str>, on: bool) -> AppResult<Value> {
        let mut g = PREVIEW
            .lock()
            .map_err(|_| AppError::internal("the preview is stuck"))?;
        // the old session lets the camera go before a new one opens it
        *g = None;
        if on {
            *g = Some(
                cap::CameraPreview::start(camera, 480)
                    .map_err(|e| AppError::invalid(e.to_string()))?,
            );
        }
        Ok(json!({"on": g.is_some()}))
    }

    /// `gm://camera/frame?after=<seq>`: a 12-byte header `{w u32, h u32, seq u32}` (LE)
    /// then the RGBA picture, when one newer than `seq` has come. (The path is there
    /// because the scheme handler's URI writes an empty one as `/`.)
    pub fn preview_frame(uri: &str) -> Option<Vec<u8>> {
        let after: u64 = uri.strip_prefix("gm://camera/frame?after=")?.parse().ok()?;
        let g = PREVIEW.lock().ok()?;
        let f = g.as_ref()?.latest(after)?;
        let mut b = Vec::with_capacity(12 + f.rgba.len());
        b.extend_from_slice(&f.w.to_le_bytes());
        b.extend_from_slice(&f.h.to_le_bytes());
        b.extend_from_slice(&(f.seq as u32).to_le_bytes());
        b.extend_from_slice(&f.rgba);
        Some(b)
    }

    pub fn sources() -> Value {
        let s = gmclip_media::rec::sources();
        json!({
            "displays": s.displays.iter().map(|d| {
                let (w, h) = d.pixels();
                json!({"id": d.id, "name": d.name, "main": d.main, "width": w, "height": h})
            }).collect::<Vec<_>>(),
            "windows": s.windows.iter().map(|w| json!({"id": w.id, "app": w.app, "title": w.title})).collect::<Vec<_>>(),
            "cameras": s.cameras.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>(),
            "mics": s.mics.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>(),
            // OBS and the like: the recording is streaming-safe while one runs.
            "live": cap::live_encoders(),
        })
    }

    fn opt_str(a: &Value, k: &str) -> Option<String> {
        a.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    pub fn prepare(core: &AppCore, a: &Value) -> AppResult<Value> {
        if !enabled(core) {
            return Err(AppError::invalid("turn on Settings › Recording first"));
        }
        let mut g = active()?;
        if g.is_some() {
            return Err(AppError::invalid("a recording is already running"));
        }
        // the face file opens the camera itself
        let _ = preview(None, false);
        let parent = opt_str(a, "folder").map_or_else(|| default_parent(core), PathBuf::from);
        if !parent.is_absolute() {
            return Err(AppError::invalid("an absolute folder"));
        }
        std::fs::create_dir_all(&parent)
            .map_err(|e| AppError::invalid(format!("making {}: {}", parent.display(), e.kind())))?;
        let displays: Vec<u32> = a
            .get("displays")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_u64)
                    .filter_map(|d| u32::try_from(d).ok())
                    .collect()
            })
            .unwrap_or_default();
        let opts = RecordOptions {
            parent,
            name: opt_str(a, "name"),
            displays,
            window: a
                .get("window")
                .and_then(Value::as_u64)
                .and_then(|w| u32::try_from(w).ok()),
            face: a.get("face").and_then(Value::as_bool).unwrap_or(true),
            camera: opt_str(a, "camera"),
            mic: a.get("mic").and_then(Value::as_bool).unwrap_or(true),
            mic_id: opt_str(a, "mic_id"),
            system_audio: a
                .get("system_audio")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            fps: a
                .get("fps")
                .and_then(Value::as_u64)
                .map_or(30, |f| f.clamp(15, 60) as u32),
            hevc: false,
            streaming_safe: a.get("streaming_safe").and_then(Value::as_bool),
        };
        // Asked again here: one never answered when recording was turned on (or turned on
        // before the app could ask) would otherwise leave the face file out silently.
        if opts.face {
            let _ = cap::request(cap::PermKind::Camera);
            if opts.mic {
                let _ = cap::request(cap::PermKind::Microphone);
            }
        }
        let mut rec = Recording::prepare(&opts).map_err(|e| AppError::invalid(e.to_string()))?;
        let stop_asked = Arc::new(AtomicBool::new(false));
        if let Some(hk) = rec.hotkeys() {
            let flag = stop_asked.clone();
            std::thread::spawn(move || {
                // Ends when the recording (and the hotkey sender) is gone.
                for k in hk {
                    if k == Hotkey::Stop {
                        flag.store(true, Ordering::Release);
                    }
                }
            });
        }
        let out = json!({"dir": rec.dir(), "missing": rec.missing()});
        *g = Some(Active {
            rec,
            stop_asked,
            started: false,
        });
        Ok(out)
    }

    pub fn go() -> AppResult<Value> {
        let mut g = active()?;
        let a = g
            .as_mut()
            .ok_or_else(|| AppError::invalid("no recording is prepared"))?;
        if !a.started {
            a.rec.go().map_err(|e| AppError::internal(e.to_string()))?;
            a.started = true;
        }
        Ok(json!({"dir": a.rec.dir()}))
    }

    pub fn marker() -> AppResult<Value> {
        let g = active()?;
        let a = g
            .as_ref()
            .ok_or_else(|| AppError::invalid("nothing is recording"))?;
        a.rec.marker();
        Ok(json!({"markers": a.rec.markers()}))
    }

    pub fn stop(core: &Arc<AppCore>) -> AppResult<Value> {
        let a = active()?
            .take()
            .ok_or_else(|| AppError::invalid("nothing is recording"))?;
        if !a.started {
            let dir = a.rec.dir().to_path_buf();
            drop(a);
            let _ = std::fs::remove_dir_all(&dir);
            return Ok(json!({"cancelled": true}));
        }
        let (dir, _) = a
            .rec
            .stop()
            .map_err(|e| AppError::internal(e.to_string()))?;
        open(core, &dir)
    }

    pub fn cancel() -> AppResult<Value> {
        let Some(a) = active()?.take() else {
            return Ok(json!({"cancelled": false}));
        };
        let dir = a.rec.dir().to_path_buf();
        let started = a.started;
        if started {
            let _ = a.rec.stop();
        } else {
            drop(a);
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(json!({"cancelled": true}))
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    use super::*;

    fn no() -> AppError {
        AppError::new("Unsupported", "recording is macOS only for now")
    }
    pub fn status(core: &AppCore) -> AppResult<Value> {
        Ok(
            json!({"enabled": false, "unsupported": true, "recording": null, "folder": default_parent(core)}),
        )
    }
    pub fn enable(_: &AppCore, _: bool) -> AppResult<Value> {
        Err(no())
    }
    pub fn preview(_: Option<&str>, _: bool) -> AppResult<Value> {
        Err(no())
    }
    pub fn preview_frame(_: &str) -> Option<Vec<u8>> {
        None
    }
    pub fn ask(_: &AppCore, _: &str, _: bool) -> AppResult<Value> {
        Err(no())
    }
    pub fn sources() -> Value {
        json!({"displays": [], "windows": [], "cameras": [], "mics": [], "live": []})
    }
    pub fn prepare(_: &AppCore, _: &Value) -> AppResult<Value> {
        Err(no())
    }
    pub fn go() -> AppResult<Value> {
        Err(no())
    }
    pub fn marker() -> AppResult<Value> {
        Err(no())
    }
    pub fn stop(_: &Arc<AppCore>) -> AppResult<Value> {
        Err(no())
    }
    pub fn cancel() -> AppResult<Value> {
        Err(no())
    }
}

pub use mac::{
    ask, cancel, enable, go, marker, prepare, preview, preview_frame, sources, status, stop,
};
