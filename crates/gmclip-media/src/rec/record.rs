//! Running a recording on macOS: the capture streams from `gmclip_gpu::apple::capture`,
//! a writer thread for `events.ndjson`, and a 15 Hz poll for the focused field, the
//! cursor's image, the frontmost app and a recorded window's frame.
//!
//! Hotkeys are read from the event tap (so they work whichever app is in front): ⌃⌥S
//! stops, ⌃⌥M drops a marker. Key-downs are counted per poll, never logged by key, and
//! not at all while a secure text field has focus.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gmclip_gpu::apple::capture::{
    self as cap, FaceConfig, FaceRecorder, FocusProbe, Frontmost, InputEvent, InputTap, PermKind,
    ScreenConfig, ScreenRecorder, ScreenTarget, Sources,
};
use sha2::{Digest, Sha256};

use super::{
    CURSORS_DIR, DisplayGeom, EVENTS_FILE, FaceFile, RawEvent, RecError, SESSION_SCHEMA,
    ScreenFile, Session, SwitchedApp, WindowRef, default_name, read_switches, write_switches,
};

/// Keeps the probe's switches on disk while recording, so a crash can be undone.
fn save_switches(dir: &Path, list: &[cap::Switched]) {
    let list: Vec<SwitchedApp> = list
        .iter()
        .map(|s| SwitchedApp {
            pid: s.pid,
            app: s.bundle.clone(),
            attr: s.attr.as_str().into(),
        })
        .collect();
    let _ = write_switches(dir, &list);
}

/// Turns off the accessibility switches a recording that never stopped (a crash) left
/// on in other apps, and forgets them.
pub fn restore_switches_left(dir: &Path) {
    let list: Vec<cap::Switched> = read_switches(dir)
        .into_iter()
        .filter_map(|s| {
            Some(cap::Switched {
                pid: s.pid,
                bundle: s.app,
                attr: cap::AxSwitch::parse(&s.attr)?,
            })
        })
        .collect();
    cap::restore_switches(&list);
    let _ = write_switches(dir, &[]);
}

/// What can be recorded now (displays and windows need Screen Recording).
pub fn sources() -> Sources {
    cap::sources()
}

/// What to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordOptions {
    /// The folder the session folder is made in.
    pub parent: PathBuf,
    /// The session folder's name (default: `Recording <UTC date time>`).
    pub name: Option<String>,
    /// Displays to record, one file each (empty with no `window`: the main display).
    pub displays: Vec<u32>,
    /// Record one window instead of displays.
    pub window: Option<u32>,
    pub face: bool,
    pub camera: Option<String>,
    pub mic: bool,
    pub mic_id: Option<String>,
    pub system_audio: bool,
    pub fps: u32,
    pub hevc: bool,
    /// Streaming-safe: 30 fps at most and pictures within 2560×1440, so a live stream
    /// on the same hardware encoder keeps going. None: on while OBS or another live
    /// encoder is running (decision 0055).
    pub streaming_safe: Option<bool>,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            parent: PathBuf::from("."),
            name: None,
            displays: Vec::new(),
            window: None,
            face: true,
            camera: None,
            mic: true,
            mic_id: None,
            system_audio: true,
            // 60 is opt-in: the hardware encoder keeps up with 5K + 4K at 30, not 60, on
            // an M1 Pro; the cursor is redrawn from the log at the output rate anyway.
            fps: 30,
            // H.264: what the editor decodes today (HEVC is M2).
            hevc: false,
            streaming_safe: None,
        }
    }
}

/// The most pixels a streaming-safe picture has (2560×1440): with a stream's encoder
/// (OBS at 2560×1440 30 fps) about twice that stream's load on the hardware encoder.
const STREAMING_SAFE_PIXELS: u32 = 2560 * 1440;

/// The frame rate, the picture cap and the note saying so, for `fps` asked, the
/// streaming-safe choice and the live encoders running. An M1 Pro's one hardware
/// encoder, filled by a 4096×2304 60 fps recording, gave OBS's stream no frames for
/// 46 s until every ingest server hung up (decision 0055).
fn streaming_safe(
    fps: u32,
    asked: Option<bool>,
    live: &[String],
) -> (u32, Option<u32>, Option<String>) {
    if !asked.unwrap_or(!live.is_empty()) {
        return (fps, None, None);
    }
    let fps = fps.min(30);
    let why = if live.is_empty() {
        "streaming-safe is on".to_string()
    } else {
        format!("{} is running", live.join(", "))
    };
    let note = format!(
        "full size: {why}, so the screen records at {fps} fps within 2560×1440 (streaming-safe)"
    );
    (fps, Some(STREAMING_SAFE_PIXELS), Some(note))
}

/// A hotkey pressed during the recording.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hotkey {
    Stop,
    Marker,
}

/// ⌃ and ⌥ (`kCGEventFlagMaskControl | kCGEventFlagMaskAlternate`), and the masks of
/// every modifier, to require exactly those two.
const CTRL_ALT: u64 = 0x0004_0000 | 0x0008_0000;
const MODIFIERS: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
/// ANSI key codes.
const KEY_S: u16 = 1;
const KEY_M: u16 = 46;

struct Shared {
    /// Host time of `t0`; 0 before [`Recording::go`].
    t0: AtomicU64,
    events: Mutex<Option<Sender<RawEvent>>>,
    keys: AtomicU32,
    secure: AtomicBool,
    markers: AtomicI64,
    hotkeys: Mutex<Option<Sender<Hotkey>>>,
}

impl Shared {
    /// µs from `t0`, or None before it.
    fn us(&self, host: u64) -> Option<i64> {
        let t0 = self.t0.load(Ordering::Acquire);
        if t0 == 0 || host < t0 {
            return None;
        }
        Some((cap::host_to_ns(host - t0) / 1000) as i64)
    }
    fn now_us(&self) -> Option<i64> {
        self.us(cap::host_now())
    }
    fn send(&self, e: RawEvent) {
        if let Ok(g) = self.events.lock()
            && let Some(tx) = g.as_ref()
        {
            let _ = tx.send(e);
        }
    }
    fn marker(&self) {
        if let Some(t) = self.now_us() {
            self.markers.fetch_add(1, Ordering::Relaxed);
            self.send(RawEvent::Marker { t });
        }
    }
}

struct Screen {
    rec: ScreenRecorder,
    file: String,
    display: DisplayGeom,
    window: Option<WindowRef>,
}

/// Removes a session folder a failed start made.
struct Made(Option<PathBuf>);

impl Drop for Made {
    fn drop(&mut self) {
        if let Some(d) = self.0.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// A recording in progress.
pub struct Recording {
    dir: PathBuf,
    name: String,
    shared: Arc<Shared>,
    screens: Vec<Screen>,
    face: Option<FaceRecorder>,
    face_file: Option<String>,
    tap: Option<InputTap>,
    poll: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    writer: Option<JoinHandle<Result<(), String>>>,
    hotkeys: Option<Receiver<Hotkey>>,
    permissions: BTreeMap<String, String>,
    missing: Vec<String>,
    started: Option<Instant>,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("dir", &self.dir)
            .field("screens", &self.screens.len())
            .finish_non_exhaustive()
    }
}

fn cap_err(e: cap::CaptureError) -> RecError {
    RecError::Capture(e.to_string())
}

fn geom(d: &cap::Display) -> DisplayGeom {
    DisplayGeom {
        id: d.id,
        name: d.name.clone(),
        x: d.x,
        y: d.y,
        w: d.w,
        h: d.h,
        scale: d.scale,
    }
}

fn round4(r: [f64; 4]) -> [f64; 4] {
    r.map(|v| (v * 2.0).round() / 2.0)
}

/// The event log's writer thread: one JSON line per event, flushed every second.
fn spawn_writer(path: PathBuf, rx: Receiver<RawEvent>) -> JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("gmclip-rec-events".into())
        .spawn(move || {
            let f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
            let mut w = std::io::BufWriter::new(f);
            let mut flushed = Instant::now();
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(e) => {
                        let line = serde_json::to_string(&e).map_err(|e| e.to_string())?;
                        w.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                        w.write_all(b"\n").map_err(|e| e.to_string())?;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if flushed.elapsed() >= Duration::from_secs(1) {
                    w.flush().map_err(|e| e.to_string())?;
                    flushed = Instant::now();
                }
            }
            w.flush().map_err(|e| e.to_string())
        })
        .expect("spawn the event writer")
}

/// Stores a cursor image once under `cursors/` (decoded with limits and stored again as
/// plain RGBA PNG) and returns its sha256.
fn store_shape(dir: &Path, png: &[u8], seen: &mut HashSet<String>) -> Option<(String, u32, u32)> {
    let img = crate::image::decode_untrusted(png).ok()?;
    let clean = crate::image::encode_png(&img).ok()?;
    let sha: String = Sha256::digest(&clean)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if seen.insert(sha.clone()) {
        let p = dir.join(CURSORS_DIR).join(format!("{sha}.png"));
        std::fs::write(p, &clean).ok()?;
    }
    Some((sha, img.width, img.height))
}

/// The 15 Hz poll: focus, cursor image, frontmost app, a recorded window's frame, and
/// the key count since the last poll.
fn spawn_poll(
    dir: PathBuf,
    shared: Arc<Shared>,
    mut probe: Option<FocusProbe>,
    windows: Vec<(u16, u32)>,
    quit: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("gmclip-rec-poll".into())
        .spawn(move || {
            let mut last_focus: Option<RawEvent> = None;
            let mut last_shape: Option<u64> = None;
            let mut last_app = String::new();
            let mut last_win: Vec<Option<[f64; 4]>> = vec![None; windows.len()];
            let mut seen = HashSet::new();
            let mut switched = 0;
            while !quit.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(66));
                let Some(t) = shared.now_us() else { continue };
                if let Some(p) = probe.as_mut() {
                    let f = p.focused();
                    if p.switched().len() != switched {
                        switched = p.switched().len();
                        save_switches(&dir, p.switched());
                    }
                    shared
                        .secure
                        .store(f.as_ref().is_some_and(|f| f.secure), Ordering::Release);
                    let e = match f {
                        Some(f) => RawEvent::Focus {
                            t,
                            rect: f.rect.map(round4),
                            caret: f.caret.map(round4),
                            window: f.window.map(round4),
                            role: f.role,
                            subrole: f.subrole,
                            app: f.app,
                        },
                        None => RawEvent::Focus {
                            t,
                            rect: None,
                            caret: None,
                            window: None,
                            role: String::new(),
                            subrole: String::new(),
                            app: String::new(),
                        },
                    };
                    let same = match (&e, &last_focus) {
                        (
                            RawEvent::Focus {
                                t: _,
                                rect,
                                caret,
                                window,
                                role,
                                subrole,
                                app,
                            },
                            Some(RawEvent::Focus {
                                rect: r2,
                                caret: c2,
                                window: w2,
                                role: ro2,
                                subrole: s2,
                                app: a2,
                                ..
                            }),
                        ) => {
                            rect == r2
                                && caret == c2
                                && window == w2
                                && role == ro2
                                && subrole == s2
                                && app == a2
                        }
                        _ => false,
                    };
                    if !same {
                        shared.send(e.clone());
                        last_focus = Some(e);
                    }
                }
                if let Some(s) = cap::cursor_shape(last_shape)
                    && Some(s.key) != last_shape
                    && !s.png.is_empty()
                {
                    if let Some((sha, w, h)) = store_shape(&dir, &s.png, &mut seen) {
                        let k = f64::from(w) / f64::from(s.w.max(1));
                        shared.send(RawEvent::Shape {
                            t,
                            sha256: sha,
                            w,
                            h,
                            hot_x: s.hot_x * k,
                            hot_y: s.hot_y * k,
                            pt_w: s.pt_w,
                            pt_h: s.pt_h,
                        });
                    }
                    last_shape = Some(s.key);
                }
                if let Some(a) = Frontmost::now()
                    && a.bundle != last_app
                {
                    last_app = a.bundle.clone();
                    shared.send(RawEvent::App {
                        t,
                        app: a.bundle,
                        name: a.name,
                    });
                }
                for (j, (s, id)) in windows.iter().enumerate() {
                    let r = cap::window_bounds(*id).map(round4);
                    if r.is_some() && r != last_win[j] {
                        last_win[j] = r;
                        if let Some(rect) = r {
                            shared.send(RawEvent::Window { t, s: *s, rect });
                        }
                    }
                }
                let n = shared.keys.swap(0, Ordering::AcqRel);
                if n > 0 {
                    shared.send(RawEvent::Keys { t, n });
                }
            }
            // Stopped, cancelled or dropped: the apps woken go back to how they were.
            if let Some(mut p) = probe {
                p.restore();
            }
            if switched > 0 {
                let _ = write_switches(&dir, &[]);
            }
        })
        .expect("spawn the recording poll")
}

fn on_input(shared: &Shared, e: InputEvent) {
    match e {
        InputEvent::Key {
            down: true,
            code,
            flags,
            ..
        } if flags & MODIFIERS == CTRL_ALT && (code == KEY_S || code == KEY_M) => {
            let k = if code == KEY_S {
                Hotkey::Stop
            } else {
                Hotkey::Marker
            };
            if k == Hotkey::Marker {
                shared.marker();
            }
            if let Ok(g) = shared.hotkeys.lock()
                && let Some(tx) = g.as_ref()
            {
                let _ = tx.send(k);
            }
        }
        InputEvent::Key { t, down: true, .. } => {
            if shared.us(t).is_some() && !shared.secure.load(Ordering::Acquire) {
                shared.keys.fetch_add(1, Ordering::AcqRel);
            }
        }
        InputEvent::Key { .. } | InputEvent::Flags { .. } => {}
        InputEvent::Move { t, x, y, drag } => {
            if let Some(t) = shared.us(t) {
                shared.send(RawEvent::Move { t, x, y, drag });
            }
        }
        InputEvent::Button {
            t,
            x,
            y,
            button,
            down,
            count,
        } => {
            if let Some(t) = shared.us(t) {
                shared.send(RawEvent::Button {
                    t,
                    x,
                    y,
                    b: button,
                    down,
                    n: count,
                });
            }
        }
        InputEvent::Scroll { t, x, y, dx, dy } => {
            if let Some(t) = shared.us(t) {
                shared.send(RawEvent::Scroll { t, x, y, dx, dy });
            }
        }
    }
}

impl Recording {
    /// Makes the session folder and starts every stream warm; nothing is written until
    /// [`Self::go`]. A refused permission drops what needs it and is listed in
    /// `missing` (Screen Recording is the one that stops the recording).
    pub fn prepare(opts: &RecordOptions) -> Result<Self, RecError> {
        let perms = cap::permissions();
        let permissions: BTreeMap<String, String> = PermKind::ALL
            .iter()
            .map(|k| (k.as_str().to_string(), perms.get(*k).as_str().to_string()))
            .collect();
        if !perms.screen.granted() {
            return Err(RecError::Capture(
                "Screen Recording permission is not granted".into(),
            ));
        }
        let mut missing = Vec::new();
        let (fps, max_pixels, note) =
            streaming_safe(opts.fps, opts.streaming_safe, &cap::live_encoders());
        missing.extend(note);
        let name = opts.name.clone().unwrap_or_else(|| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            default_name(secs)
        });
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(RecError::Nothing(format!("{name:?} is not a folder name")));
        }
        let dir = opts.parent.join(&name);
        if dir.exists() {
            return Err(RecError::Nothing(format!(
                "{} already exists",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir.join(CURSORS_DIR))
            .map_err(|e| RecError::io(dir.display(), e))?;
        // A start that fails takes its folder with it.
        let mut made = Made(Some(dir.clone()));

        let (tx, rx) = channel::<RawEvent>();
        let (htx, hrx) = channel::<Hotkey>();
        let shared = Arc::new(Shared {
            t0: AtomicU64::new(0),
            events: Mutex::new(Some(tx)),
            keys: AtomicU32::new(0),
            secure: AtomicBool::new(false),
            markers: AtomicI64::new(0),
            hotkeys: Mutex::new(Some(htx)),
        });

        let srcs = cap::sources();
        let targets: Vec<ScreenTarget> = match opts.window {
            Some(w) => vec![ScreenTarget::Window(w)],
            None if opts.displays.is_empty() => {
                let main = srcs
                    .displays
                    .iter()
                    .find(|d| d.main)
                    .or(srcs.displays.first());
                vec![ScreenTarget::Display(
                    main.ok_or_else(|| {
                        RecError::Nothing("no display is available (asleep or locked?)".into())
                    })?
                    .id,
                )]
            }
            None => opts
                .displays
                .iter()
                .map(|d| ScreenTarget::Display(*d))
                .collect(),
        };
        let mut screens = Vec::new();
        let mut windows = Vec::new();
        for (i, target) in targets.iter().enumerate() {
            let file = match target {
                ScreenTarget::Display(id) => format!("screen-{id}.mov"),
                ScreenTarget::Window(id) => format!("screen-window-{id}.mov"),
            };
            let cfg = ScreenConfig {
                target: *target,
                path: dir.join(&file),
                fps,
                // One screen file carries the system sound; the others are picture only.
                audio: opts.system_audio && i == 0,
                hevc: opts.hevc,
                max_pixels,
            };
            let s = shared.clone();
            let idx = i as u16;
            let on_frame = Box::new(move |host: u64| {
                if let Some(t) = s.us(host) {
                    let (x, y) = cap::mouse_location();
                    s.send(RawEvent::Cursor { t, s: idx, x, y });
                }
            });
            let rec = ScreenRecorder::start(&cfg, on_frame).map_err(cap_err)?;
            let info = rec.info().clone();
            let window = info.window.as_ref().map(|w| WindowRef {
                id: w.id,
                title: w.title.clone(),
                app: w.app.clone(),
                rect: [w.x, w.y, w.w, w.h],
            });
            if let ScreenTarget::Window(id) = target {
                windows.push((idx, *id));
            }
            screens.push(Screen {
                rec,
                file,
                display: geom(&info.display),
                window,
            });
        }

        let (face, face_file) = if opts.face {
            if !perms.camera.granted() {
                missing.push("camera: permission not granted".into());
                (None, None)
            } else {
                if opts.mic && !perms.microphone.granted() {
                    missing.push("microphone: permission not granted".into());
                }
                let cfg = FaceConfig {
                    path: dir.join("face.mov"),
                    camera: opts.camera.clone(),
                    mic: opts.mic_id.clone(),
                    with_mic: opts.mic,
                    hevc: false,
                };
                match FaceRecorder::start(&cfg) {
                    Ok(f) => (Some(f), Some("face.mov".to_string())),
                    Err(e) => {
                        missing.push(format!("camera: {e}"));
                        (None, None)
                    }
                }
            }
        } else {
            (None, None)
        };

        let s = shared.clone();
        let tap = match InputTap::start(Box::new(move |e| on_input(&s, e))) {
            Ok(t) => {
                if !t.keys {
                    missing.push("keys and hotkeys: Input Monitoring not granted".into());
                }
                Some(t)
            }
            Err(e) => {
                missing.push(format!("pointer events: {e}"));
                None
            }
        };
        let probe = FocusProbe::new();
        if probe.is_none() {
            missing.push("focused field: Accessibility not granted".into());
        }
        let quit = Arc::new(AtomicBool::new(false));
        let poll = spawn_poll(dir.clone(), shared.clone(), probe, windows, quit.clone());
        let writer = spawn_writer(dir.join(EVENTS_FILE), rx);

        made.0 = None;
        Ok(Self {
            dir,
            name,
            shared,
            screens,
            face,
            face_file,
            tap,
            poll: Some((quit, poll)),
            writer: Some(writer),
            hotkeys: Some(hrx),
            permissions,
            missing,
            started: None,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What was asked for but will not be recorded.
    pub fn missing(&self) -> &[String] {
        &self.missing
    }

    /// The hotkeys pressed (⌃⌥S stop, ⌃⌥M marker); taken once.
    pub fn hotkeys(&mut self) -> Option<Receiver<Hotkey>> {
        self.hotkeys.take()
    }

    /// Starts every file now; returns `t0` (host ticks).
    pub fn go(&mut self) -> Result<u64, RecError> {
        // Confirm the recovery manifest can be saved before any capture begins.
        self.session(Vec::new(), None, 0, false).write(&self.dir)?;
        let t0 = cap::host_now();
        self.shared.t0.store(t0, Ordering::Release);
        for s in &self.screens {
            s.rec.go(t0).map_err(cap_err)?;
        }
        if let Some(f) = &self.face {
            f.go(t0);
        }
        self.started = Some(Instant::now());
        Ok(t0)
    }

    /// Time since [`Self::go`].
    pub fn elapsed(&self) -> Duration {
        self.started.map_or(Duration::ZERO, |s| s.elapsed())
    }

    pub fn marker(&self) {
        self.shared.marker();
    }

    pub fn markers(&self) -> i64 {
        self.shared.markers.load(Ordering::Relaxed)
    }

    /// Encoder overload observed so far. A busy callback is retried at the next poll.
    pub fn dropped(&self) -> u64 {
        self.screens
            .iter()
            .filter_map(|s| s.rec.dropped())
            .chain(self.face.iter().filter_map(|f| f.dropped()))
            .fold(0, u64::saturating_add)
    }

    /// Why the OS stopped a screen stream early, if it did.
    pub fn trouble(&self) -> Option<String> {
        self.screens.iter().find_map(|s| s.rec.stopped())
    }

    fn session(
        &self,
        screens: Vec<ScreenFile>,
        face: Option<FaceFile>,
        duration_us: i64,
        complete: bool,
    ) -> Session {
        let screens = if screens.is_empty() {
            self.screens
                .iter()
                .map(|s| {
                    let info = s.rec.info();
                    ScreenFile {
                        file: s.file.clone(),
                        display: s.display.clone(),
                        window: s.window.clone(),
                        width: info.width,
                        height: info.height,
                        fps: s.rec.fps(),
                        frames: 0,
                        dropped: 0,
                    }
                })
                .collect()
        } else {
            screens
        };
        Session {
            schema: SESSION_SCHEMA.into(),
            name: self.name.clone(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            screens,
            face,
            events: EVENTS_FILE.into(),
            permissions: self.permissions.clone(),
            missing: self.missing.clone(),
            duration_us,
            complete,
        }
    }

    /// Stops everything, closes the files and writes `session.json`.
    pub fn stop(mut self) -> Result<(PathBuf, Session), RecError> {
        let duration_us = self.shared.now_us().unwrap_or(0);
        if let Some(t) = self.tap.take() {
            t.stop();
        }
        if let Some((quit, h)) = self.poll.take() {
            quit.store(true, Ordering::Release);
            let _ = h.join();
        }
        let mut errors = Vec::new();
        let mut files = Vec::new();
        for s in std::mem::take(&mut self.screens) {
            let info = s.rec.info().clone();
            let fps = s.rec.fps();
            match s.rec.finish() {
                Ok(st) => files.push(ScreenFile {
                    file: s.file,
                    display: s.display,
                    window: s.window,
                    width: info.width,
                    height: info.height,
                    fps,
                    frames: st.video_frames,
                    dropped: st.dropped,
                }),
                Err(e) => errors.push(format!("{}: {e}", s.file)),
            }
        }
        let face = match (self.face.take(), self.face_file.take()) {
            (Some(f), Some(file)) => {
                let (camera, mic) = f.devices();
                let (camera, mic) = (camera.to_string(), mic.map(str::to_string));
                match f.finish() {
                    Ok(st) => Some(FaceFile {
                        file,
                        camera,
                        mic,
                        width: st.width,
                        height: st.height,
                        frames: st.video_frames,
                        dropped: st.dropped,
                    }),
                    Err(e) => {
                        self.missing.push(format!("camera: {e}"));
                        None
                    }
                }
            }
            _ => None,
        };
        // Closing the sender ends the writer thread once the queue is written.
        if let Ok(mut g) = self.shared.events.lock() {
            g.take();
        }
        if let Some(w) = self.writer.take() {
            match w.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(format!("{EVENTS_FILE}: {e}")),
                Err(_) => errors.push(format!("{EVENTS_FILE}: the writer stopped")),
            }
        }
        if files.is_empty() {
            return Err(RecError::Capture(errors.join("; ")));
        }
        for f in files
            .iter()
            .map(|s| s.file.as_str())
            .chain(face.iter().map(|f| f.file.as_str()))
        {
            if let Err(e) = cap::finalize(&self.dir.join(f)) {
                errors.push(format!("{f}: {e} (left fragmented)"));
            }
        }
        self.missing.extend(errors);
        let session = self.session(files, face, duration_us, true);
        session.write(&self.dir)?;
        Ok((self.dir.clone(), session))
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // Stopped without `stop` (an error path): end the threads; the files close as
        // their recorders drop, and session.json stays marked incomplete.
        if let Some(t) = self.tap.take() {
            t.stop();
        }
        if let Some((quit, h)) = self.poll.take() {
            quit.store(true, Ordering::Release);
            let _ = h.join();
        }
        if let Ok(mut g) = self.shared.events.lock() {
            g.take();
        }
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{STREAMING_SAFE_PIXELS, streaming_safe};

    #[test]
    fn obs_running_records_at_30_within_1440p_and_says_so() {
        let obs = vec!["OBS Studio".to_string()];
        let (fps, cap, note) = streaming_safe(60, None, &obs);
        assert_eq!((fps, cap), (30, Some(STREAMING_SAFE_PIXELS)));
        assert!(note.unwrap().contains("OBS Studio is running"));
        assert_eq!(streaming_safe(60, None, &[]), (60, None, None));
        assert_eq!(streaming_safe(60, Some(false), &obs), (60, None, None));
        let (fps, cap, note) = streaming_safe(24, Some(true), &[]);
        assert_eq!((fps, cap), (24, Some(STREAMING_SAFE_PIXELS)));
        assert!(note.unwrap().contains("streaming-safe is on"));
    }
}
