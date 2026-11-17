//! `gmclip record` (decision 0045, SPEC §5.18): record screens, the camera and the mic
//! as separate files plus the input log, from a terminal. The app's Record sheet runs
//! the same `gmclip_media::rec::Recording`.
//!
//! `record start` runs until `--duration` passes, Enter is pressed, or ⌃⌥S (from any
//! app); ⌃⌥M drops a marker. `record tracks <session>` prints what the import would
//! read from a session (one input track per screen file).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::Output;
use crate::error::{CliError, Code};

#[derive(Debug, Subcommand)]
pub enum RecordCmd {
    /// Record now.
    Start(StartArgs),
    /// The displays, windows, cameras and mics that can be recorded.
    Sources,
    /// The five permissions recording needs; `--ask` shows each OS prompt not yet
    /// answered.
    Permissions {
        #[arg(long)]
        ask: bool,
    },
    /// What the recorder sees right now: the focused field, the cursor's image and
    /// the frontmost app (a diagnostic for the permissions and the probes).
    Probe,
    /// What a session's input log holds, per screen file.
    Tracks {
        session: PathBuf,
        /// Also write each track next to its screen file (`<file>.input.json`).
        #[arg(long)]
        write: bool,
    },
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// The folder the session folder is made in.
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
    /// The session folder's name.
    #[arg(long)]
    pub name: Option<String>,
    /// A display id to record (repeatable; default: the main display).
    #[arg(long = "display")]
    pub displays: Vec<u32>,
    /// Every display, one file each.
    #[arg(long, conflicts_with_all = ["displays", "window"])]
    pub all_displays: bool,
    /// Record one window (by id, from `record sources`) instead of displays.
    #[arg(long, conflicts_with = "displays")]
    pub window: Option<u32>,
    /// No camera file.
    #[arg(long)]
    pub no_face: bool,
    /// A camera's id (default: the system's default camera).
    #[arg(long)]
    pub camera: Option<String>,
    /// No mic track.
    #[arg(long)]
    pub no_mic: bool,
    /// A mic's id.
    #[arg(long)]
    pub mic: Option<String>,
    /// No system sound in the screen file.
    #[arg(long)]
    pub no_system_audio: bool,
    /// Screen frame rate (the cursor is redrawn from the log at the output rate).
    #[arg(long, default_value_t = 30)]
    pub fps: u32,
    /// HEVC instead of H.264 for the screen (smaller files at full 5K; gmclip opens
    /// HEVC from M2 on).
    #[arg(long)]
    pub hevc: bool,
    /// 30 fps at most and pictures within 2560×1440, so a live stream keeps the hardware
    /// encoder (default: on while OBS or another live encoder is running).
    #[arg(long)]
    pub streaming_safe: Option<bool>,
    /// Stop after this many seconds.
    #[arg(long)]
    pub duration: Option<f64>,
    /// Seconds of countdown (the streams warm up meanwhile).
    #[arg(long, default_value_t = 3)]
    pub countdown: u32,
}

fn fail(step: &str, e: impl std::fmt::Display) -> CliError {
    CliError::new(Code::Media)
        .step(format!("record.{step}"))
        .reason(e.to_string())
}

pub fn run(cmd: RecordCmd) -> Result<Output, CliError> {
    match cmd {
        RecordCmd::Tracks { session, write } => tracks(&session, write),
        #[cfg(target_os = "macos")]
        RecordCmd::Start(a) => mac::start(a),
        #[cfg(target_os = "macos")]
        RecordCmd::Sources => Ok(Output::Summary(mac::sources())),
        #[cfg(target_os = "macos")]
        RecordCmd::Permissions { ask } => mac::permissions(ask),
        #[cfg(target_os = "macos")]
        RecordCmd::Probe => Ok(Output::Summary(mac::probe())),
        #[cfg(not(target_os = "macos"))]
        _ => Err(CliError::new(Code::Unsupported)
            .step("record")
            .reason("recording is macOS only for now")),
    }
}

fn tracks(dir: &std::path::Path, write: bool) -> Result<Output, CliError> {
    let session = gmclip_media::rec::Session::read(dir).map_err(|e| fail("tracks", e))?;
    let tracks = gmclip_media::rec::tracks(dir).map_err(|e| fail("tracks", e))?;
    let mut out = Vec::new();
    for t in &tracks {
        if write {
            let p = PathBuf::from(format!("{}.input.json", t.file.display()));
            let b = serde_json::to_vec(&t.track).map_err(|e| fail("tracks", e))?;
            std::fs::write(&p, b).map_err(|e| fail("tracks", e))?;
        }
        let tr = &t.track;
        out.push(json!({
            "file": t.file,
            "picture": [tr.w, tr.h],
            "scale": tr.scale,
            "cursor_samples": tr.cursor.len(),
            "shapes": tr.shapes.len(),
            "clicks": tr.clicks.iter().filter(|c| c.down).count(),
            "scrolls": tr.scrolls.len(),
            "focus_changes": tr.focus.len(),
            "with_caret": tr.focus.iter().filter(|f| f.caret.is_some()).count(),
            "key_downs": tr.keys.iter().map(|k| k.n).sum::<u32>(),
            "apps": tr.apps.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
            "markers": tr.markers.len(),
            "check": tr.check().err(),
        }));
    }
    Ok(Output::Summary(json!({
        "session": session.name,
        "complete": session.complete,
        "duration_s": session.duration_us as f64 / 1e6,
        "face": session.face,
        "missing": session.missing,
        "screens": out,
    })))
}

#[cfg(target_os = "macos")]
mod mac {
    use std::io::Write as _;
    use std::time::Duration;

    use gmclip_media::rec::{Hotkey, RecordOptions, Recording, cap};

    use super::*;

    pub fn sources() -> Value {
        let s = gmclip_media::rec::sources();
        json!({
            "displays": s.displays.iter().map(|d| {
                let (w, h) = d.pixels();
                json!({"id": d.id, "name": d.name, "main": d.main, "points": [d.x, d.y, d.w, d.h], "pixels": [w, h], "scale": d.scale})
            }).collect::<Vec<_>>(),
            "windows": s.windows.iter().map(|w| json!({"id": w.id, "app": w.app, "title": w.title, "points": [w.x, w.y, w.w, w.h]})).collect::<Vec<_>>(),
            "cameras": s.cameras.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>(),
            "mics": s.mics.iter().map(|d| json!({"id": d.id, "name": d.name})).collect::<Vec<_>>(),
        })
    }

    pub fn probe() -> Value {
        let focus = cap::FocusProbe::new().and_then(|mut p| p.focused());
        let shape = cap::cursor_shape(None);
        let app = cap::Frontmost::now();
        let (x, y) = cap::mouse_location();
        json!({
            "cursor": [x, y],
            "shape": shape.map(|s| json!({"png_bytes": s.png.len(), "pixels": [s.w, s.h], "hot": [s.hot_x, s.hot_y], "points": [s.pt_w, s.pt_h]})),
            "focus": focus.map(|f| json!({"role": f.role, "subrole": f.subrole, "app": f.app, "rect": f.rect, "caret": f.caret, "window": f.window, "secure": f.secure})),
            "frontmost": app.map(|a| json!({"bundle": a.bundle, "name": a.name})),
        })
    }

    pub fn permissions(ask: bool) -> Result<Output, CliError> {
        let mut out = serde_json::Map::new();
        for k in cap::PermKind::ALL {
            let p = if ask {
                cap::request(k).map_err(|e| fail("permissions", e))?
            } else {
                cap::permissions().get(k)
            };
            out.insert(
                k.as_str().into(),
                json!({"state": p.as_str(), "settings": k.settings_url()}),
            );
        }
        Ok(Output::Summary(Value::Object(out)))
    }

    pub fn start(a: StartArgs) -> Result<Output, CliError> {
        let displays = if a.all_displays {
            gmclip_media::rec::sources()
                .displays
                .iter()
                .map(|d| d.id)
                .collect()
        } else {
            a.displays
        };
        let opts = RecordOptions {
            parent: a.out,
            name: a.name,
            displays,
            window: a.window,
            face: !a.no_face,
            camera: a.camera,
            mic: !a.no_mic,
            mic_id: a.mic,
            system_audio: !a.no_system_audio,
            fps: a.fps,
            hevc: a.hevc,
            streaming_safe: a.streaming_safe,
        };
        let mut rec = Recording::prepare(&opts).map_err(|e| fail("start", e))?;
        let hotkeys = rec.hotkeys();
        let mut err = std::io::stderr();
        for m in rec.missing() {
            let _ = writeln!(err, "not recording {m}");
        }
        for n in (1..=a.countdown).rev() {
            let _ = writeln!(err, "{n}…");
            std::thread::sleep(Duration::from_secs(1));
        }
        rec.go().map_err(|e| fail("start", e))?;
        let _ = writeln!(
            err,
            "recording to {} — Enter or ⌃⌥S stops, ⌃⌥M marks",
            rec.dir().display()
        );
        let (enter_tx, enter_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            // Enter stops; a closed stdin (EOF, no terminal) does not.
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                let _ = enter_tx.send(());
            }
        });
        let limit = a.duration.map(Duration::from_secs_f64);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if limit.is_some_and(|l| rec.elapsed() >= l) || enter_rx.try_recv().is_ok() {
                break;
            }
            if hotkeys
                .as_ref()
                .is_some_and(|h| h.try_iter().any(|k| k == Hotkey::Stop))
            {
                break;
            }
            if let Some(t) = rec.trouble() {
                let _ = writeln!(err, "the screen stream stopped: {t}");
                break;
            }
        }
        let (dir, session) = rec.stop().map_err(|e| fail("stop", e))?;
        Ok(Output::Summary(json!({
            "session": dir,
            "duration_s": session.duration_us as f64 / 1e6,
            "screens": session.screens,
            "face": session.face,
            "missing": session.missing,
        })))
    }
}
