//! Recording sessions (decision 0045, SPEC §5.18).
//!
//! A session is one folder:
//!
//! - `session.json` ([`Session`]): what was recorded, on which displays, with which
//!   devices and permissions, and how long;
//! - `screen-<display>.mov` (or `screen-window-<id>.mov`) per recorded screen, the
//!   cursor hidden; `face.mov`, the camera with the mic as its second track;
//! - `events.ndjson`: one [`RawEvent`] per line, µs from the session's `t0`, global
//!   points (origin top left of the main display);
//! - `cursors/<sha256>.png`: each cursor image once;
//! - `accessibility.json` ([`SwitchedApp`]), only while recording: the accessibility
//!   switches the focus probe turned on in other apps, so a recording a crash ended can
//!   turn them off when it is opened (a stop or cancel turns them off and removes it).
//!
//! Every file and event is on host time, and every file's time 0 is `t0`, so the files
//! line up without matching anything. [`tracks`] turns the raw log into one
//! [`InputTrack`] per screen file, in that file's pixels, for the import.
//!
//! [`Recording`] (macOS) runs the capture: [`Recording::prepare`] starts every stream
//! warm (during the countdown), [`Recording::go`] starts the files, [`Recording::stop`]
//! closes them and writes `session.json`.

mod convert;
#[cfg(target_os = "macos")]
mod record;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use convert::{ScreenTrack, track_for, tracks};
/// The capture layer (permissions, devices), for the CLI and the app, which do not
/// depend on gmclip-gpu themselves.
#[cfg(target_os = "macos")]
pub use gmclip_gpu::apple::capture as cap;
#[cfg(target_os = "macos")]
pub use record::{Hotkey, RecordOptions, Recording, restore_switches_left, sources};

/// The [`Session`] schema id.
pub const SESSION_SCHEMA: &str = "gmclip.rec/1";
pub const SESSION_FILE: &str = "session.json";
pub const EVENTS_FILE: &str = "events.ndjson";
pub const CURSORS_DIR: &str = "cursors";
pub const SWITCHES_FILE: &str = "accessibility.json";

/// An accessibility switch (`AXManualAccessibility`, `AXEnhancedUserInterface`) the
/// recorder turned on in an app, to turn off again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchedApp {
    pub pid: i32,
    /// Its bundle id: a pid that is another app by then is left alone.
    pub app: String,
    pub attr: String,
}

/// Writes `accessibility.json` (an empty list removes it).
pub fn write_switches(dir: &Path, list: &[SwitchedApp]) -> Result<(), RecError> {
    let p = dir.join(SWITCHES_FILE);
    if list.is_empty() {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(RecError::io(p.display(), e)),
            _ => Ok(()),
        };
    }
    let tmp = dir.join(".accessibility.json.tmp");
    let b = serde_json::to_vec(list).map_err(|e| RecError::Capture(e.to_string()))?;
    std::fs::write(&tmp, b).map_err(|e| RecError::io(tmp.display(), e))?;
    std::fs::rename(&tmp, &p).map_err(|e| RecError::io(p.display(), e))
}

/// Reads `accessibility.json` (none, or unreadable: nothing).
pub fn read_switches(dir: &Path) -> Vec<SwitchedApp> {
    std::fs::read(dir.join(SWITCHES_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Why a recording could not start, run or be read.
#[derive(Debug, thiserror::Error)]
pub enum RecError {
    #[error("{0}")]
    Capture(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0} is not a recording: {1}")]
    NotASession(String, String),
    #[error("nothing to record: {0}")]
    Nothing(String),
}

impl RecError {
    pub(crate) fn io(what: impl std::fmt::Display, e: std::io::Error) -> Self {
        RecError::Io(what.to_string(), e)
    }
}

/// A display's frame in global points and its pixels per point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayGeom {
    pub id: u32,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
}

/// One screen file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenFile {
    pub file: String,
    pub display: DisplayGeom,
    /// A window recording: the window's id, title and app, and its frame at the start
    /// (later frames are `window` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowRef>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frames: u64,
    pub dropped: u64,
}

impl ScreenFile {
    /// A note when more than 2 % of the frames were left out (the hardware encoder fell
    /// behind: 60 fps, a big display, or a live stream sharing the encoder).
    pub fn dropped_note(&self) -> Option<String> {
        let all = self.frames + self.dropped;
        if self.dropped * 50 <= all {
            return None;
        }
        let pct = (self.dropped * 100 + all / 2) / all;
        let hint = if self.fps > 30 { "; try 30 fps" } else { "" };
        Some(format!(
            "{}: {} of {all} frames dropped ({pct} %), the encoder fell behind{hint}",
            self.file, self.dropped
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRef {
    pub id: u32,
    pub title: String,
    pub app: String,
    pub rect: [f64; 4],
}

/// The face file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceFile {
    pub file: String,
    pub camera: String,
    /// The mic recorded as the file's second track (absent: no mic).
    pub mic: Option<String>,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub dropped: u64,
}

/// `session.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub schema: String,
    pub name: String,
    pub app_version: String,
    pub screens: Vec<ScreenFile>,
    pub face: Option<FaceFile>,
    pub events: String,
    /// Each permission's state when recording started (`granted`, `denied`, …).
    pub permissions: std::collections::BTreeMap<String, String>,
    /// What was asked for but not recorded, and why ("camera: permission denied").
    #[serde(default)]
    pub missing: Vec<String>,
    /// Length, µs from `t0` to the stop.
    pub duration_us: i64,
    /// False while recording (or after a crash): the files may end early.
    pub complete: bool,
}

impl Session {
    pub fn read(dir: &Path) -> Result<Self, RecError> {
        let p = dir.join(SESSION_FILE);
        let b = std::fs::read(&p).map_err(|e| RecError::io(p.display(), e))?;
        let s: Session = serde_json::from_slice(&b)
            .map_err(|e| RecError::NotASession(dir.display().to_string(), e.to_string()))?;
        if s.schema != SESSION_SCHEMA {
            return Err(RecError::NotASession(
                dir.display().to_string(),
                format!("schema {}", s.schema),
            ));
        }
        Ok(s)
    }

    pub(crate) fn write(&self, dir: &Path) -> Result<(), RecError> {
        let p = dir.join(SESSION_FILE);
        let tmp = dir.join(".session.json.tmp");
        let b = serde_json::to_vec_pretty(self).map_err(|e| RecError::Capture(e.to_string()))?;
        std::fs::write(&tmp, b).map_err(|e| RecError::io(tmp.display(), e))?;
        std::fs::rename(&tmp, &p).map_err(|e| RecError::io(p.display(), e))
    }

    /// The files of the session, screens first, as absolute paths.
    pub fn files(&self, dir: &Path) -> Vec<PathBuf> {
        self.screens
            .iter()
            .map(|s| dir.join(&s.file))
            .chain(self.face.iter().map(|f| dir.join(&f.file)))
            .collect()
    }
}

/// Whether `dir` holds a recording session.
pub fn is_session(dir: &Path) -> bool {
    dir.join(SESSION_FILE).is_file()
}

/// One line of `events.ndjson`. `t` is µs from `t0`; points are global.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum RawEvent {
    /// The cursor when screen `s` (index into `screens`) wrote a frame.
    Cursor {
        t: i64,
        s: u16,
        x: f64,
        y: f64,
    },
    Move {
        t: i64,
        x: f64,
        y: f64,
        drag: bool,
    },
    Button {
        t: i64,
        x: f64,
        y: f64,
        b: u8,
        down: bool,
        n: u8,
    },
    Scroll {
        t: i64,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// The cursor image changed (`cursors/<sha256>.png`; hotspot in its pixels).
    Shape {
        t: i64,
        sha256: String,
        w: u32,
        h: u32,
        hot_x: f64,
        hot_y: f64,
        pt_w: f64,
        pt_h: f64,
    },
    Focus {
        t: i64,
        rect: Option<[f64; 4]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caret: Option<[f64; 4]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<[f64; 4]>,
        role: String,
        #[serde(default)]
        subrole: String,
        app: String,
    },
    /// Key-downs since the last `keys` (counts only).
    Keys {
        t: i64,
        n: u32,
    },
    App {
        t: i64,
        app: String,
        name: String,
    },
    /// A recorded window's frame changed.
    Window {
        t: i64,
        s: u16,
        rect: [f64; 4],
    },
    Marker {
        t: i64,
    },
}

impl RawEvent {
    pub fn t(&self) -> i64 {
        match self {
            RawEvent::Cursor { t, .. }
            | RawEvent::Move { t, .. }
            | RawEvent::Button { t, .. }
            | RawEvent::Scroll { t, .. }
            | RawEvent::Shape { t, .. }
            | RawEvent::Focus { t, .. }
            | RawEvent::Keys { t, .. }
            | RawEvent::App { t, .. }
            | RawEvent::Window { t, .. }
            | RawEvent::Marker { t } => *t,
        }
    }
}

/// Reads `events.ndjson`, in time order. A torn last line (a crash mid-write) is
/// skipped; any other bad line is an error.
pub fn read_events(dir: &Path) -> Result<Vec<RawEvent>, RecError> {
    let p = dir.join(EVENTS_FILE);
    let text = std::fs::read_to_string(&p).map_err(|e| RecError::io(p.display(), e))?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        match serde_json::from_str::<RawEvent>(l) {
            Ok(e) => out.push(e),
            Err(_) if i + 1 == lines.len() => {}
            Err(e) => {
                return Err(RecError::NotASession(
                    dir.display().to_string(),
                    format!("{EVENTS_FILE} line {}: {e}", i + 1),
                ));
            }
        }
    }
    out.sort_by_key(RawEvent::t);
    Ok(out)
}

/// A session folder name from a Unix time: `Recording 2026-10-19 20-45-12` (UTC; the
/// app passes the person's local time instead).
pub fn default_name(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "Recording {y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_kept_until_an_empty_list_removes_them() {
        let dir = std::env::temp_dir().join(format!("gmclip-rec-sw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_switches(&dir).is_empty());
        let list = vec![SwitchedApp {
            pid: 42,
            app: "com.google.Chrome".into(),
            attr: "AXEnhancedUserInterface".into(),
        }];
        write_switches(&dir, &list).unwrap();
        assert_eq!(read_switches(&dir), list);
        write_switches(&dir, &[]).unwrap();
        assert!(!dir.join(SWITCHES_FILE).exists());
        write_switches(&dir, &[]).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_take_that_dropped_frames_says_so() {
        let mut f = ScreenFile {
            file: "screen-2.mov".into(),
            display: DisplayGeom {
                id: 2,
                name: "PHL 276E8V".into(),
                x: 0.0,
                y: 0.0,
                w: 2560.0,
                h: 1440.0,
                scale: 2.0,
            },
            window: None,
            width: 4096,
            height: 2304,
            fps: 60,
            frames: 2955,
            dropped: 585,
        };
        assert_eq!(
            f.dropped_note().as_deref(),
            Some(
                "screen-2.mov: 585 of 3540 frames dropped (17 %), the encoder fell behind; try 30 fps"
            )
        );
        f.dropped = 10;
        assert_eq!(f.dropped_note(), None);
        f.frames = 0;
        f.dropped = 0;
        assert_eq!(f.dropped_note(), None);
    }

    #[test]
    fn names_from_unix_time() {
        assert_eq!(default_name(0), "Recording 1970-01-01 00-00-00");
        assert_eq!(default_name(1_792_445_112), "Recording 2026-10-19 21-25-12");
    }

    #[test]
    fn events_round_trip_and_a_torn_last_line_is_skipped() {
        let dir = std::env::temp_dir().join(format!("gmclip-rec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let evs = [
            RawEvent::Move {
                t: 20,
                x: 1.0,
                y: 2.0,
                drag: false,
            },
            RawEvent::Keys { t: 10, n: 3 },
        ];
        let mut text: String = evs
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        text.push_str("{\"k\":\"move\",\"t\":3");
        std::fs::write(dir.join(EVENTS_FILE), text).unwrap();
        let got = read_events(&dir).unwrap();
        assert_eq!(got, vec![evs[1].clone(), evs[0].clone()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
