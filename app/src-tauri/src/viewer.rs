//! The render thread (SPEC §3.11): one per app. It owns the GPU preview session for the
//! cut on screen, the playhead and the live gizmo state, renders NV12 frames at the
//! displayed size for the frame socket, and turns pointer records into gizmo drags.
//!
//! - Playback follows the audio device (§6.3, `playback`): the frame shown is the one
//!   whose samples are being heard; a frame that is late is dropped, never waited for,
//!   and counted in the next frame's header. Without an output device the plan's frame
//!   clock runs from a wall-clock anchor instead.
//! - Gizmos (D4): every visible layer's box, and the selected layer's outline with corner
//!   handles, are drawn by the compositor into the same frame, so a handle can never swim
//!   against the picture. A drag previews live from records (no commit); release commits
//!   one `place_set` through the core (§7: "one command on release").

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use gmclip_plan::Plan;
use gmclip_plan::types::Rect;
use serde::Serialize;

use crate::core::AppCore;
use crate::frame::{FLAG_FULL, FLAG_PLAYING, FrameSlot, OutFrame, Record, RecordKind, keys};

/// What the viewer shows and does, for `gm_call viewer_state` and the `viewer` scope.
#[derive(Clone, Debug, Default, Serialize, PartialEq, schemars::JsonSchema)]
pub struct ViewerState {
    pub project: Option<String>,
    pub cut: Option<String>,
    pub frame: u64,
    pub frames: u64,
    /// Frames per second (29.97… at an NTSC rate).
    pub fps: f64,
    pub playing: bool,
    pub full: bool,
    pub size: (u32, u32),
    /// `(plan part id, plan layer id)` of the selected layer.
    pub selected: Option<(String, String)>,
    pub error: Option<String>,
    pub dropped: u64,
    pub rendered: u64,
    /// Playback speed (J/K/L shuttle): 1.0 normal; negative while J plays backwards
    /// (silent, on the scrub path; the exact frame shows once it stops).
    pub rate: f64,
    /// The framing shown when it is not the cut's own (the crop editor).
    pub framing: Option<String>,
    /// Playback audio: device latency, A/V offset at frame handoff, underruns, muted.
    pub audio: AudioStats,
    /// Scrub audio on (a grain at each step or seek while paused).
    pub scrub: bool,
    /// Where the last frame's pictures came from: `exact` (the master), `proxy` (the
    /// scrub proxy) or `keyframe` (the master's keyframe; the proxy does not cover the
    /// frame yet). Scrub frames settle to `exact` 150 ms after the last scrub step.
    #[schemars(schema_with = "crate::events::scrub_path")]
    pub scrub_path: Option<String>,
    /// The last frame drew a live draft matte (no accurate mask track yet): the
    /// viewer's "draft matte" badge (SPEC §5.8).
    pub draft_matte: bool,
    /// Matte view: matted layers' masks in white on black (R.02).
    pub matte_view: bool,
    /// The layer being dragged, `[x, y, w, h]` canvas px, while a drag is on (the UI
    /// draws its off-canvas ghost: what lies past the frame's edge).
    #[schemars(with = "Option<(f64, f64, f64, f64)>")]
    pub drag: Option<[f64; 4]>,
    /// Snap guides of the drag: `(vertical, at)` in canvas px.
    pub guides: Vec<(bool, f64)>,
    /// A text overlay or the caption seat being dragged (U.12): how far the frame draws
    /// it from where the cut has it, `(dx, dy)` canvas px, until the commit's re-planned
    /// cut arrives (then the frame is the plan's own again).
    pub text_drag: Option<(f64, f64)>,
}

/// What the audio side reports (the A/V offset is the shown frame's centre minus the
/// sample being heard when it was handed to the frame socket).
#[derive(Clone, Debug, Default, Serialize, PartialEq, schemars::JsonSchema)]
pub struct AudioStats {
    pub device: bool,
    pub latency_ms: f64,
    pub av_mean_ms: f64,
    pub av_max_ms: f64,
    pub av_frames: u64,
    pub underruns: u64,
    pub muted: bool,
    /// Why this plan plays no sound (its audio would not build), when it plays none.
    pub no_sound: Option<String>,
    /// Output device switches the engine recovered from (it reopens on the new device;
    /// the A/V numbers above restart there, measured against the new device's latency).
    pub reopens: u64,
}

/// Messages to the render thread.
#[derive(Debug)]
pub enum ViewerMsg {
    Load {
        project: String,
        cut: String,
        /// Another framing of the cut than the one it shows (the crop editor's full
        /// source picture); `keep` keeps the playhead.
        framing: Option<String>,
        keep: bool,
    },
    /// A plan that is not a cut (review mode's moment span); `looping` replays it.
    LoadPlan {
        key: (String, String),
        plan: Arc<Plan>,
        looping: bool,
        /// Paused at this frame instead of playing from 0 (the stream viewer).
        at: Option<u64>,
    },
    Unload,
    Replan {
        project: String,
    },
    Seek(u64),
    /// A scrub step (H.05): shown from the scrub proxy (or the master's keyframe), then
    /// the exact frame once the scrub rests for [`SCRUB_SETTLE`]. While playing, a seek.
    ScrubTo(u64),
    Play,
    Pause,
    Toggle,
    Step(i64),
    Full(bool),
    /// Matte view on or off (R.02): the cut-outs' masks alone.
    MatteView(bool),
    /// S2a: full quality during playback as well (a measurement mode, not a preference).
    Stress(bool),
    /// Playback speed, 0.25–8 (the shuttle); −8…−0.25 plays backwards (J), silently on
    /// the scrub path, until anything else is asked of the viewer or frame 0 is reached.
    Rate(f64),
    /// Scrub audio: a short grain of sound at each step or seek while paused.
    Scrub(bool),
    /// The output device changed: reopen the engine on it now instead of at the next
    /// once-a-second check (a play continues; A/V is measured again on the new device).
    AudioDeviceChanged,
    /// Plays output samples `[from, to)` and stops there (sweep previews), or replays it
    /// until something else is asked of the viewer (`looping`: ⌥L, U.08).
    PlayRange {
        from: u64,
        to: u64,
        looping: bool,
    },
    /// Plays source milliseconds `[from, to)` of the cut (P: exactly one word).
    PlaySrc {
        from_ms: i64,
        to_ms: i64,
    },
    Select(Option<(String, String)>),
    /// The layers grouped on the timeline (model `(part, layer)`s): a drag of one of them
    /// commits the same place on all.
    Group(Vec<(String, String)>),
    Records(Vec<Record>),
    /// A text overlay or the caption seat dragged on the paused frame (U.12): the frame
    /// draws those text events moved (the shared compositor, never logged). Replaces the
    /// previous move of the same drag.
    TextDrag(gmclip_render::export::preview::TextShift),
    /// The text drag ended: `committed` keeps the move drawn until the commit's re-planned
    /// cut arrives (so the text does not jump back for a frame); otherwise (Esc, a failed
    /// commit) the frame goes back at once.
    TextDragEnd {
        committed: bool,
    },
    /// A part edge dragged on the timeline (decision 0053): the cut as the trim would
    /// leave it ([`crate::core::AppCore::trim_peek`]), shown paused at the moved edge's
    /// frame in place of the cut. Replaces the previous step of the same drag.
    TrimPeek {
        plan: Arc<Plan>,
        frame: u64,
    },
    /// The edge drag ended: the cut again, at the frame shown before the drag.
    TrimPeekEnd,
}

/// The handle IPC uses.
#[derive(Clone)]
pub struct Viewer {
    tx: Sender<ViewerMsg>,
    pub state: Arc<Mutex<ViewerState>>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer").finish_non_exhaustive()
    }
}

impl Viewer {
    /// A viewer with no render thread: what it is sent goes nowhere (no window).
    pub fn detached() -> Viewer {
        let (tx, _) = mpsc::channel::<ViewerMsg>();
        Viewer {
            tx,
            state: Arc::default(),
        }
    }

    pub fn send(&self, m: ViewerMsg) {
        let _ = self.tx.send(m);
    }
    pub fn state(&self) -> ViewerState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
struct Drag {
    part: String,
    layer: String,
    /// Corner being dragged (0 tl, 1 tr, 2 bl, 3 br) or move.
    corner: Option<u8>,
    start: (f64, f64),
    orig: Rect,
    now: Rect,
    aspect: f64,
    /// The core gesture holding the layer (R-GEST-1/R-CC-2), ended on release or Esc.
    gesture: Option<u64>,
}

/// How long a committed text drag's move stays drawn waiting for its re-planned cut.
const TEXT_PENDING_MAX: Duration = Duration::from_secs(3);

/// `GMCLIP_DEBUG_IPC`: render-thread events on stderr (shape only).
fn debug(msg: &str) {
    if std::env::var_os("GMCLIP_DEBUG_IPC").is_some() {
        eprintln!("gmclip viewer: {msg}");
    }
}

const ACCENT: [f32; 4] = [0.0, 0.62, 1.0, 1.0];
/// Snap guides (magenta, as most editors draw them).
const GUIDE: [f32; 4] = [1.0, 0.18, 0.67, 1.0];

/// Canvas snapping for a layer drag (SPEC §5.7 "canvas pane with snapping/guides"): a
/// moved box's left / centre / right and top / middle / bottom snap to the frame's edges
/// and centre lines and to the other layers' edges and centres within `tol` canvas px;
/// a resized box (corner `k`, aspect kept) snaps its moving vertical edge. Returns the
/// snapped box and the guides it snapped to, `(vertical, at)`.
pub(crate) fn snap_rect(
    r: Rect,
    corner: Option<u8>,
    aspect: f64,
    canvas: (f64, f64),
    others: &[Rect],
    tol: f64,
) -> (Rect, Vec<(bool, f64)>) {
    let mut xs = vec![0.0, canvas.0 / 2.0, canvas.0];
    let mut ys = vec![0.0, canvas.1 / 2.0, canvas.1];
    for o in others {
        xs.extend([o.x, o.x + o.w / 2.0, o.x + o.w]);
        ys.extend([o.y, o.y + o.h / 2.0, o.y + o.h]);
    }
    // the smallest move bringing one of `pts` onto one of `targets`, within tol
    let best = |pts: &[f64], targets: &[f64]| -> Option<(f64, f64)> {
        let mut b: Option<(f64, f64)> = None;
        for p in pts {
            for t in targets {
                let d = t - p;
                if d.abs() <= tol && b.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    b = Some((d, *t));
                }
            }
        }
        b
    };
    let mut out = r;
    let mut guides = Vec::new();
    match corner {
        None => {
            if let Some((d, t)) = best(&[r.x, r.x + r.w / 2.0, r.x + r.w], &xs) {
                out.x += d;
                guides.push((true, t));
            }
            if let Some((d, t)) = best(&[r.y, r.y + r.h / 2.0, r.y + r.h], &ys) {
                out.y += d;
                guides.push((false, t));
            }
        }
        Some(k) => {
            let right = k == 1 || k == 3;
            let bottom = k == 2 || k == 3;
            let edge = if right { r.x + r.w } else { r.x };
            if let Some((d, t)) = best(&[edge], &xs) {
                let w = if right { r.w + d } else { r.w - d };
                if w >= 8.0 {
                    let h = w * aspect;
                    out = Rect {
                        x: if right { r.x } else { r.x + r.w - w },
                        y: if bottom { r.y } else { r.y + r.h - h },
                        w,
                        h,
                    };
                    guides.push((true, t));
                }
            }
        }
    }
    (out, guides)
}

/// A scrub that rests this long shows the exact frame.
pub const SCRUB_SETTLE: Duration = Duration::from_millis(150);

/// J's backwards play: the frame `back` frames (whole ones due so far) before `from`,
/// never before frame 0.
pub fn reverse_target(from: u64, back: f64) -> u64 {
    if !back.is_finite() || back <= 0.0 {
        return from;
    }
    from.saturating_sub(back.min(u64::MAX as f64) as u64)
}

fn premul(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0] * a, c[1] * a, c[2] * a, a]
}

/// Where output frame `f` of `old` is in `new`, an edit of it (decision 0053): the frame
/// that shows the same source frame, in the part nearest `f` that shows it at 1×; else
/// `f` (its part is gone, or plays at another speed).
pub fn same_frame(old: &Plan, new: &Plan, f: u64) -> u64 {
    let last = new.output.frames.saturating_sub(1);
    let plain = |p: &gmclip_plan::types::Part| p.speed.as_ref().is_none_or(|s| s.num == s.den);
    let Some(p) = old
        .parts
        .iter()
        .find(|p| p.out.start() <= f && f < p.out.end() && plain(p))
    else {
        return f.min(last);
    };
    let sf = p.picture.first_frame + (f - p.out.start());
    new.parts
        .iter()
        .filter(|q| q.picture.source == p.picture.source && plain(q))
        .filter(|q| q.picture.first_frame <= sf && sf < q.picture.first_frame + q.out.len())
        .map(|q| q.out.start() + (sf - q.picture.first_frame))
        .min_by_key(|&g| g.abs_diff(f))
        .unwrap_or(f)
        .min(last)
}

/// Spawns the render thread; frames go to `frames`, records arrive on `records`.
pub fn spawn(
    core: &Arc<AppCore>,
    frames: Arc<FrameSlot>,
    records: Receiver<Vec<Record>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
) -> Viewer {
    let (tx, rx) = mpsc::channel::<ViewerMsg>();
    let state = Arc::new(Mutex::new(ViewerState::default()));
    // Records are forwarded into the one message queue.
    {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("gmclip-records".into())
            .spawn(move || {
                while let Ok(r) = records.recv() {
                    if tx.send(ViewerMsg::Records(r)).is_err() {
                        return;
                    }
                }
            });
    }
    // Commits on the viewer's project re-plan it.
    {
        let tx = tx.clone();
        core.add_commit_hook(Box::new(move |p| {
            let _ = tx.send(ViewerMsg::Replan {
                project: p.to_owned(),
            });
        }));
    }
    let weak = Arc::downgrade(core);
    let st = state.clone();
    let _ = std::thread::Builder::new()
        .name("gmclip-render".into())
        .spawn(move || {
            let mut r = RenderThread::new(weak, st, frames, connected);
            r.run(&rx);
        });
    Viewer { tx, state }
}

struct RenderThread {
    core: Weak<AppCore>,
    state: Arc<Mutex<ViewerState>>,
    frames: Arc<FrameSlot>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// The client was connected at the last loop turn (a new client gets a frame).
    was_connected: bool,
    /// What the `viewer` scope last saw (a rate-limited change is flushed later).
    published: ViewerState,
    #[cfg(any(target_os = "macos", windows))]
    session: Option<gmclip_render::export::preview::PreviewSession>,
    /// Windows: the 1 ms timer while playing (frame waits are otherwise rounded to the
    /// 15.6 ms tick, which drops frames at 60 fps).
    #[cfg(windows)]
    fine_timer: Option<gmclip_platform::windows::FineTimer>,
    inputs: gmclip_render::export::inputs::DemuxInputs,
    plan: Option<Arc<Plan>>,
    /// The document rev the plan was made at (gizmo commits name it).
    plan_rev: u64,
    key: Option<(String, String)>,
    frame: u64,
    playing: bool,
    anchor: (Instant, u64),
    viewport: (u32, u32),
    full: bool,
    selected: Option<(String, String)>,
    group: Vec<(String, String)>,
    drag: Option<Drag>,
    /// The drag's snap guides (canvas px), drawn as gizmo lines.
    guides: Vec<(bool, f64)>,
    /// A committed drag shows until the re-planned cut arrives.
    pending: Option<Drag>,
    /// A live text drag (U.12), and when it was committed (cleared by the next plan, or
    /// after [`TEXT_PENDING_MAX`] when no plan came: a commit that changed nothing).
    text_drag: Option<gmclip_render::export::preview::TextShift>,
    text_pending: Option<Instant>,
    echo_t: f64,
    /// S2a: keep full size during playback too (measurement only).
    stress: bool,
    /// Replay from the start at the end (review mode).
    looping: bool,
    /// Playback speed (the shuttle).
    rate: f64,
    /// Playing backwards (J): `(when, from frame, speed)`; the frame shown is the anchor
    /// frame less the frames due since then.
    reverse: Option<(Instant, u64, f64)>,
    /// A framing other than the cut's own (the crop editor).
    framing: Option<String>,
    dropped: u64,
    rendered: u64,
    seq: u32,
    dirty: bool,
    last_state_pub: Instant,
    /// Playback audio and the clock (None without an output device).
    audio: Option<crate::playback::Audio>,
    /// The current play stops at this output sample (a range play).
    range_end: Option<u64>,
    /// Scrub audio (grains while paused).
    scrub: bool,
    /// The current play starts at this output sample (instead of the frame's first).
    range_from: Option<u64>,
    /// A looped range (⌥L): `[from, to)` output samples, replayed at its end.
    loop_range: Option<(u64, u64)>,
    /// When the current play started (the audio clock may take a moment).
    play_started: Instant,
    av_sum: f64,
    av_max: f64,
    av_n: u64,
    /// Output device switches recovered from.
    reopens: u64,
    /// Last check of the default output device.
    device_checked: Instant,
    /// Scrubbing: the exact frame is due at this instant (H.05).
    scrub_settle: Option<Instant>,
    /// Where the last frame's pictures came from.
    scrub_path: Option<&'static str>,
    /// The last frame drew a draft matte; matte view is on.
    draft_matte: bool,
    matte_view: bool,
    /// When the proxy folders were last handed to the preview session.
    proxy_dirs_at: Option<Instant>,
    /// A trim drag's monitor is shown: the cut's own plan (kept current by commits
    /// meanwhile) and the frame to go back to.
    peek: Option<(Option<Arc<Plan>>, u64)>,
    /// Samples from the audio clock's numbering to the plan's: edits during a play move
    /// the playhead without restarting the sound (decision 0053).
    play_shift: i64,
}

impl RenderThread {
    /// Counts dropped frames; while playing, each drop is reported to the background-job
    /// gate (D.01: ASR pauses on a drop in the last 2 s).
    fn drop_frames(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        self.dropped += n;
        if self.playing
            && let Some(core) = self.core.upgrade()
        {
            core.qos.frame_dropped(Instant::now());
        }
    }

    fn new(
        core: Weak<AppCore>,
        state: Arc<Mutex<ViewerState>>,
        frames: Arc<FrameSlot>,
        connected: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            core,
            state,
            frames,
            connected,
            was_connected: false,
            published: ViewerState::default(),
            #[cfg(any(target_os = "macos", windows))]
            session: None,
            #[cfg(windows)]
            fine_timer: None,
            inputs: gmclip_render::export::inputs::DemuxInputs::new(None),
            plan: None,
            plan_rev: 0,
            key: None,
            frame: 0,
            playing: false,
            anchor: (Instant::now(), 0),
            viewport: (540, 960),
            full: false,
            selected: None,
            group: Vec::new(),
            drag: None,
            guides: Vec::new(),
            pending: None,
            text_drag: None,
            text_pending: None,
            echo_t: 0.0,
            stress: false,
            looping: false,
            rate: 1.0,
            reverse: None,
            framing: None,
            dropped: 0,
            rendered: 0,
            seq: 0,
            dirty: false,
            last_state_pub: Instant::now(),
            audio: crate::playback::Audio::open(),
            range_end: None,
            scrub: true,
            range_from: None,
            loop_range: None,
            play_started: Instant::now(),
            av_sum: 0.0,
            av_max: 0.0,
            av_n: 0,
            reopens: 0,
            device_checked: Instant::now(),
            scrub_settle: None,
            scrub_path: None,
            draft_matte: false,
            matte_view: false,
            proxy_dirs_at: None,
            peek: None,
            play_shift: 0,
        }
    }

    /// A switch of the output device (AirPods, headphones) reopens the engine on the new
    /// device, so its latency is the one the clock adds; a play continues where it was.
    /// On the same device, a change of its reported latency (Bluetooth settling) reaches
    /// the clock at once. Either way the A/V offset is measured again from there.
    fn check_device(&mut self) {
        if self.device_checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.device_checked = Instant::now();
        let changed = match self.audio.as_ref() {
            Some(a) => a.device_changed(),
            // no device yet (or it went away): try again
            None => cfg!(target_os = "macos"),
        };
        if changed {
            self.reopen_audio();
        } else if self
            .audio
            .as_mut()
            .is_some_and(crate::playback::Audio::refresh_latency)
        {
            debug("output latency changed: A/V measured again");
            self.reset_av();
            self.publish(true);
        }
    }

    /// Reopens the playback engine on the system's output device now; a play continues
    /// where it was, its clock on the new device's latency.
    fn reopen_audio(&mut self) {
        let was = self.playing;
        let had = self.audio.is_some();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.audio = None;
        self.audio = crate::playback::Audio::open();
        if self.audio.is_none() {
            return;
        }
        if had {
            self.reopens += 1;
        }
        self.reset_av();
        if was {
            // the frame on screen is where the old device was
            self.anchor = (Instant::now(), self.frame);
            self.range_from = None;
            self.start_audio();
        }
        debug("output device changed: playback reopened");
        self.publish(true);
    }

    /// The A/V offset is measured afresh (a new device, a new latency).
    fn reset_av(&mut self) {
        self.av_sum = 0.0;
        self.av_max = 0.0;
        self.av_n = 0;
    }

    /// The output rate `(num, den)` (30 before a plan).
    fn rate_nd(&self) -> (u64, u64) {
        self.plan.as_ref().map_or((30, 1), |p| p.output.fps.rate())
    }

    /// The output sample at frame `f`: `S(f)` (rule 4; NTSC rounds down, decision 0020).
    fn sample_at(&self, f: u64) -> u64 {
        let (n, d) = self.rate_nd();
        (u128::from(f) * 48_000 * u128::from(d) / u128::from(n.max(1))) as u64
    }

    /// The frame whose samples hold output sample `s`: the last `f` with `S(f) ≤ s`.
    fn frame_at(&self, s: u64) -> u64 {
        let (n, d) = self.rate_nd();
        let q = 48_000 * u128::from(d);
        ((u128::from(s) + 1) * u128::from(n))
            .div_ceil(q)
            .saturating_sub(1) as u64
    }

    fn audio_stats(&self) -> AudioStats {
        AudioStats {
            device: self.audio.is_some(),
            latency_ms: self.audio.as_ref().map_or(0.0, |a| a.latency_ms()),
            av_mean_ms: if self.av_n > 0 {
                self.av_sum / self.av_n as f64
            } else {
                0.0
            },
            av_max_ms: self.av_max,
            av_frames: self.av_n,
            underruns: self.audio.as_ref().map_or(0, |a| a.underruns()),
            muted: self.audio.as_ref().is_some_and(|a| a.muted()),
            no_sound: self.audio.as_ref().and_then(|a| a.no_sound()),
            reopens: self.reopens,
        }
    }

    /// (Re)starts the audio at the playhead (or the range start).
    fn start_audio(&mut self) {
        let at = self.sample_at(self.frame);
        let (Some(a), Some(plan)) = (self.audio.as_mut(), self.plan.clone()) else {
            return;
        };
        let from = self.range_from.take().unwrap_or(at);
        a.play(plan, from as f64, self.range_end, self.rate, self.looping);
        self.play_shift = 0;
        self.play_started = Instant::now();
    }

    /// Source ms → output sample, through the plan's parts (the first part holding it).
    fn src_to_out(&self, ms: i64) -> Option<u64> {
        let plan = self.plan.as_ref()?;
        let s = u64::try_from(ms.max(0)).ok()? * 48;
        plan.parts.iter().find_map(|p| {
            let first = p.audio.first_sample;
            let len = gmclip_plan::validate::part_samples(p.out, plan.output.fps)?;
            (s >= first && s < first + len).then(|| self.sample_at(p.out.start()) + (s - first))
        })
    }

    /// Frames per second (29.97… at an NTSC rate).
    fn fps(&self) -> f64 {
        self.plan.as_ref().map_or(30.0, |p| p.output.fps.as_f64())
    }

    fn frames_total(&self) -> u64 {
        self.plan.as_ref().map_or(0, |p| p.output.frames)
    }

    fn publish(&mut self, force: bool) {
        let s = ViewerState {
            project: self.key.as_ref().map(|k| k.0.clone()),
            cut: self.key.as_ref().map(|k| k.1.clone()),
            frame: self.frame,
            frames: self.frames_total(),
            fps: self.fps(),
            playing: self.playing || self.reverse.is_some(),
            full: self.full,
            #[cfg(any(target_os = "macos", windows))]
            size: self.session.as_ref().map_or((0, 0), |s| s.size()),
            #[cfg(not(any(target_os = "macos", windows)))]
            size: (0, 0),
            selected: self.selected.clone(),
            error: self.state.lock().ok().and_then(|s| s.error.clone()),
            dropped: self.dropped,
            rendered: self.rendered,
            rate: self.rate,
            framing: self.framing.clone(),
            audio: self.audio_stats(),
            scrub: self.scrub,
            scrub_path: self.scrub_path.map(str::to_owned),
            draft_matte: self.draft_matte,
            matte_view: self.matte_view,
            drag: self
                .drag
                .as_ref()
                .map(|d| [d.now.x, d.now.y, d.now.w, d.now.h]),
            guides: self.guides.clone(),
            text_drag: self.text_drag.as_ref().map(|t| (t.dx, t.dy)),
        };
        if let Ok(mut g) = self.state.lock() {
            *g = s.clone();
        }
        if s != self.published
            && (force || self.last_state_pub.elapsed() >= Duration::from_millis(66))
            && let Some(core) = self.core.upgrade()
        {
            core.bus.viewer(crate::events::ViewerEvent::State {
                state: Box::new(s.clone()),
            });
            self.published = s;
            self.last_state_pub = Instant::now();
        }
    }

    fn set_error(&mut self, e: Option<String>) {
        if let Some(m) = &e {
            debug(&format!("error: {m}"));
        }
        if let Ok(mut g) = self.state.lock() {
            g.error = e.clone();
        }
        if let Some(core) = self.core.upgrade() {
            // with the cut it is about: an error from replanning a cut no longer on screen
            // (a long-form just removed) is not the screen's to show
            let cut = self.key.as_ref().map(|k| k.1.clone());
            core.bus
                .viewer(crate::events::ViewerEvent::Error { error: e, cut });
        }
    }

    fn load(&mut self, project: &str, cut: &str, framing: Option<String>, keep: bool) {
        debug("load");
        let Some(core) = self.core.upgrade() else {
            return;
        };
        match view_plan(&core, project, cut, framing.as_deref()) {
            Ok(pl) => {
                self.key = Some((project.to_owned(), cut.to_owned()));
                self.framing = framing;
                self.peek = None;
                self.drag = None;
                self.pending = None;
                self.text_drag = None;
                self.text_pending = None;
                self.selected = None;
                self.plan_rev = pl.rev;
                self.install(pl.plan);
                if keep {
                    self.frame = self.frame.min(self.frames_total().saturating_sub(1));
                } else {
                    self.frame = 0;
                }
                self.playing = false;
                self.set_error(None);
            }
            Err(e) => self.set_error(Some(e.message)),
        }
        self.dirty = true;
        self.publish(true);
    }

    fn replan(&mut self, project: &str) {
        let Some((p, c)) = self.key.clone() else {
            return;
        };
        // A plan that is not a cut (review mode, a library search hit) is not re-planned
        // by commits.
        if p != project
            || c.starts_with("moment:")
            || c.starts_with("stream:")
            || c.starts_with("span:")
        {
            return;
        }
        let Some(core) = self.core.upgrade() else {
            return;
        };
        match view_plan(&core, &p, &c, self.framing.as_deref()) {
            Ok(pl) => {
                self.plan_rev = pl.rev;
                // under a trim monitor, the cut it goes back to
                match self.peek.as_mut() {
                    Some((cut, _)) => *cut = Some(pl.plan),
                    None => {
                        let old = self.plan.clone();
                        self.install(Arc::clone(&pl.plan));
                        if let Some(old) = old.filter(|o| self.playing && !Arc::ptr_eq(o, &pl.plan))
                        {
                            self.go_on_playing(&old, pl.plan);
                        }
                    }
                }
                self.pending = None;
                self.end_text_pending();
                self.set_error(None);
            }
            Err(e) => {
                self.pending = None;
                self.end_text_pending();
                self.set_error(Some(e.message));
            }
        }
        self.frame = self.frame.min(self.frames_total().saturating_sub(1));
        self.dirty = true;
        self.publish(true);
    }

    /// An edit committed while playing (decision 0053): the frame on screen stays on
    /// screen (it moves with its part when the edit moved the part) and the sound goes on
    /// without a break in the edited plan from ~40 ms ahead ([`crate::playback::Audio::retarget`]),
    /// so a join further on plays the new arrangement.
    fn go_on_playing(&mut self, old: &Plan, plan: Arc<Plan>) {
        let to = same_frame(old, &plan, self.frame);
        let shift = self.sample_at(to) as i64 - self.sample_at(self.frame) as i64;
        self.frame = to;
        self.anchor = (Instant::now(), to);
        let plain = self.range_end.is_none() && !self.looping && self.reverse.is_none();
        match self.audio.as_mut() {
            Some(a) if plain => {
                a.retarget(plan, shift);
                self.play_shift += shift;
            }
            // a range or a loop plays again from here
            Some(_) => self.start_audio(),
            None => {}
        }
    }

    /// The committed text drag's move ends with the plan that holds it.
    fn end_text_pending(&mut self) {
        if self.text_pending.is_some() {
            self.text_drag = None;
            self.text_pending = None;
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn install(&mut self, plan: Arc<Plan>) {
        use gmclip_render::export::preview::PreviewSession;
        let size = self.target_size_for(&plan);
        let r = match self.session.as_mut() {
            Some(s) => s
                .set_plan(plan.clone(), &mut self.inputs)
                .and_then(|()| s.resize(size)),
            None => PreviewSession::open(plan.clone(), &mut self.inputs, size).map(|mut s| {
                // roto: the accurate mask tracks every export shares, else live drafts
                s.set_matte_store(gmclip_render::matte::store::shared());
                s.set_matte_view(self.matte_view);
                self.session = Some(s);
            }),
        };
        if let Err(e) = r {
            self.session = None;
            self.set_error(Some(e.to_string()));
        }
        self.plan = Some(plan);
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn install(&mut self, plan: Arc<Plan>) {
        self.plan = Some(plan);
        self.set_error(Some("preview is macOS-only until M5".into()));
    }

    /// Full size only when paused at full quality (SPEC §6.3), or under S2a's stress mode.
    fn target_size_for(&self, plan: &Plan) -> (u32, u32) {
        if self.full && (!self.playing || self.stress) {
            (
                plan.output.canvas.w.get() as u32,
                plan.output.canvas.h.get() as u32,
            )
        } else {
            self.viewport
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn resize(&mut self) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let size = self.target_size_for(&plan);
        if let Some(s) = self.session.as_mut()
            && let Err(e) = s.resize(size)
        {
            let msg = e.to_string();
            self.set_error(Some(msg));
        }
        self.dirty = true;
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn resize(&mut self) {}

    /// Canvas-pixel layer boxes on the current frame (with the live drag applied).
    fn boxes(&self) -> Vec<(String, String, Rect, f64)> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
        let Ok(draw) = gmclip_render::compositor::frame_draw(plan, self.frame) else {
            return Vec::new();
        };
        draw.layers
            .iter()
            // the backdrop and clip events are not the part's layers (decision 0028)
            .filter(|l| !l.is_extra())
            .map(|l| {
                let mut place = l.place;
                for d in [&self.drag, &self.pending].into_iter().flatten() {
                    if d.part == draw.part_id && d.layer == l.layer_id {
                        place = d.now;
                    }
                }
                (
                    draw.part_id.clone(),
                    l.layer_id.clone(),
                    place,
                    l.crop.h / l.crop.w,
                )
            })
            .collect()
    }

    fn canvas(&self) -> (f64, f64) {
        self.plan.as_ref().map_or((1080.0, 1920.0), |p| {
            (
                p.output.canvas.w.get() as f64,
                p.output.canvas.h.get() as f64,
            )
        })
    }

    fn on_records(&mut self, recs: Vec<Record>) {
        let (cw, ch) = self.canvas();
        for r in recs {
            // Only pointer records are echoed (the input-to-photon measure).
            if matches!(r.kind, RecordKind::Down | RecordKind::Move | RecordKind::Up) {
                self.echo_t = self.echo_t.max(r.t);
            }
            let (x, y) = (f64::from(r.x) * cw, f64::from(r.y) * ch);
            match r.kind {
                RecordKind::Viewport => {
                    let v = (r.x.round().max(2.0) as u32, r.y.round().max(2.0) as u32);
                    if v != self.viewport {
                        self.viewport = v;
                        self.resize();
                    }
                }
                RecordKind::Down if !self.playing => {
                    let size = self.display_size();
                    // 14 device pixels on screen, in canvas pixels.
                    let handle = 14.0 * self.screen_scale() * cw / f64::from(size.0.max(1));
                    let boxes = self.boxes();
                    let mut hit = None;
                    // The selected layer's handles first, then the topmost box.
                    if let Some((sp, sl)) = &self.selected
                        && let Some(b) = boxes.iter().find(|b| &b.0 == sp && &b.1 == sl)
                    {
                        let corners = [
                            (b.2.x, b.2.y),
                            (b.2.x + b.2.w, b.2.y),
                            (b.2.x, b.2.y + b.2.h),
                            (b.2.x + b.2.w, b.2.y + b.2.h),
                        ];
                        for (k, c) in corners.iter().enumerate() {
                            if (x - c.0).abs() <= handle && (y - c.1).abs() <= handle {
                                hit = Some((b.clone(), Some(k as u8)));
                            }
                        }
                    }
                    // then the selected layer's body, even under another (picked on the
                    // timeline's layer rows or the inspector: it drags where it is)
                    if hit.is_none()
                        && let Some((sp, sl)) = &self.selected
                    {
                        hit = boxes
                            .iter()
                            .find(|b| {
                                &b.0 == sp
                                    && &b.1 == sl
                                    && x >= b.2.x
                                    && x < b.2.x + b.2.w
                                    && y >= b.2.y
                                    && y < b.2.y + b.2.h
                            })
                            .map(|b| (b.clone(), None));
                    }
                    if hit.is_none() {
                        hit = boxes
                            .iter()
                            .rev()
                            .find(|b| {
                                x >= b.2.x && x < b.2.x + b.2.w && y >= b.2.y && y < b.2.y + b.2.h
                            })
                            .map(|b| (b.clone(), None));
                    }
                    match hit {
                        Some(((part, layer, rect, aspect), corner)) => {
                            self.selected = Some((part.clone(), layer.clone()));
                            let gesture = self.gesture_begin(&part, &layer);
                            self.drag = Some(Drag {
                                part,
                                layer,
                                corner,
                                start: (x, y),
                                orig: rect,
                                now: rect,
                                aspect,
                                gesture,
                            });
                        }
                        None => self.selected = None,
                    }
                    self.dirty = true;
                    self.publish(true);
                }
                RecordKind::Move => {
                    let canvas = (cw, ch);
                    let tol =
                        8.0 * self.screen_scale() * cw / f64::from(self.display_size().0.max(1));
                    let others: Vec<Rect> = match &self.drag {
                        Some(d) => self
                            .boxes()
                            .into_iter()
                            .filter(|b| !(b.0 == d.part && b.1 == d.layer))
                            .map(|b| b.2)
                            .collect(),
                        None => Vec::new(),
                    };
                    if let Some(d) = self.drag.as_mut() {
                        let (dx, dy) = (x - d.start.0, y - d.start.1);
                        let o = d.orig;
                        d.now = match d.corner {
                            None => Rect {
                                x: o.x + dx,
                                y: o.y + dy,
                                ..o
                            },
                            Some(k) => {
                                // Keep the opposite corner; width follows x, height the aspect.
                                let right = k == 1 || k == 3;
                                let bottom = k == 2 || k == 3;
                                let w = if right { o.w + dx } else { o.w - dx }.max(8.0);
                                let h = w * d.aspect;
                                let nx = if right { o.x } else { o.x + o.w - w };
                                let ny = if bottom { o.y } else { o.y + o.h - h };
                                Rect { x: nx, y: ny, w, h }
                            }
                        };
                        // canvas snapping (⌥ off): edges and centres to the frame's and
                        // the other layers'
                        self.guides = Vec::new();
                        if r.mods & 2 == 0 {
                            let (snapped, guides) =
                                snap_rect(d.now, d.corner, d.aspect, canvas, &others, tol);
                            d.now = snapped;
                            self.guides = guides;
                        }
                        self.dirty = true;
                    }
                    if self.drag.is_some() {
                        self.publish(false);
                    }
                }
                RecordKind::Up => {
                    self.guides.clear();
                    if let Some(d) = self.drag.take() {
                        let moved = (d.now.x - d.orig.x).abs()
                            + (d.now.y - d.orig.y).abs()
                            + (d.now.w - d.orig.w).abs();
                        if moved >= 0.5 && self.commit(&d) {
                            self.gesture_end(d.gesture);
                            self.pending = Some(d);
                        } else {
                            self.gesture_end(d.gesture);
                        }
                        self.dirty = true;
                        self.publish(true);
                    }
                }
                RecordKind::Cancel => {
                    // Esc or a lost pointer: the layer goes back; nothing is written
                    self.guides.clear();
                    if let Some(d) = self.drag.take() {
                        self.gesture_end(d.gesture);
                    }
                    self.dirty = true;
                    self.publish(true);
                }
                RecordKind::Wheel if !self.playing && r.dy != 0.0 => {
                    let step = if r.dy > 0.0 { 1 } else { -1 };
                    self.step(step);
                }
                RecordKind::Key => match r.code {
                    keys::PLAY_PAUSE => self.toggle(),
                    keys::PAUSE => self.pause(),
                    keys::STEP_BACK => self.step(-1),
                    keys::STEP_FORWARD => self.step(1),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    /// R-GEST-1: pointer down on a layer holds its model layer and part in the core, so
    /// an agent's write to them meanwhile gets `Conflict{by: human}` (R-CC-2).
    fn gesture_begin(&self, part: &str, layer: &str) -> Option<u64> {
        let core = self.core.upgrade()?;
        let (p, c) = self.key.clone()?;
        let pl = core.planned(&p, &c).ok()?;
        let ix = pl.index.parts.iter().find(|x| x.plan_id == part)?;
        let mut es = Vec::new();
        if let Ok(id) = crate::planner::ulid_id::<gmclip_model::entities::Part>(&ix.model) {
            es.push(id.entity());
        }
        if let Some((_, m)) = ix.layers.iter().find(|(a, _)| a == layer)
            && let Ok(id) = crate::planner::ulid_id::<gmclip_model::entities::Layer>(m)
        {
            es.push(id.entity());
        }
        core.project(&p).ok()?.handle.gesture_begin(es).ok()
    }

    fn gesture_end(&self, g: Option<u64>) {
        if let (Some(g), Some(core), Some((p, _))) = (g, self.core.upgrade(), self.key.clone())
            && let Ok(pr) = core.project(&p)
        {
            pr.handle.gesture_end(g);
        }
    }

    /// Commits a drag as one `place_set` (R-GEST-1's commit: the human's one
    /// transaction; the previews before it stayed on the render thread, never logged).
    fn commit(&self, d: &Drag) -> bool {
        let (Some(core), Some((p, c))) = (self.core.upgrade(), self.key.clone()) else {
            return false;
        };
        match core.place_px_group(
            &p,
            &c,
            &d.part,
            &d.layer,
            d.now.x,
            d.now.y,
            d.now.w,
            Some(self.plan_rev),
            &self.group,
        ) {
            Ok(_) => true,
            Err(e) => {
                core.bus.viewer(crate::events::ViewerEvent::Error {
                    error: Some(e.message),
                    cut: None,
                });
                false
            }
        }
    }

    /// Frame pixels per device pixel of the viewer box (> 1 at full quality).
    fn screen_scale(&self) -> f64 {
        (f64::from(self.display_size().0) / f64::from(self.viewport.0.max(1))).max(1.0)
    }

    fn display_size(&self) -> (u32, u32) {
        #[cfg(any(target_os = "macos", windows))]
        {
            self.session.as_ref().map_or(self.viewport, |s| s.size())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            self.viewport
        }
    }

    fn step(&mut self, n: i64) {
        if self.playing {
            self.pause();
        }
        let last = self.frames_total().saturating_sub(1) as i64;
        self.frame = (self.frame as i64 + n).clamp(0, last.max(0)) as u64;
        self.grain();
        self.dirty = true;
        self.publish(true);
    }

    /// Scrub audio: a grain (80 ms, faded in and out) at the playhead, without moving it
    /// (paused only); grains of one scrub play back to back, the newest step winning.
    fn grain(&mut self) {
        if self.playing || !self.scrub {
            return;
        }
        let total = self.sample_at(self.frames_total());
        let from = self.sample_at(self.frame);
        let (Some(a), Some(plan)) = (self.audio.as_mut(), self.plan.clone()) else {
            return;
        };
        if from < total {
            a.grain(plan, from);
        }
    }

    fn toggle(&mut self) {
        if self.playing {
            self.pause()
        } else {
            self.play()
        }
    }

    /// At a looped range's end: play it again from its start.
    fn replay_loop(&mut self) {
        if let Some((from, to)) = self.loop_range {
            self.frame = self.frame_at(from);
            self.range_from = Some(from);
            self.range_end = Some(to);
            self.play();
        }
    }

    fn play(&mut self) {
        if self.plan.is_none() {
            return;
        }
        if self.frame + 1 >= self.frames_total() {
            self.frame = 0;
        }
        self.playing = true;
        self.drag = None;
        if self.full {
            self.resize();
        }
        self.anchor = (Instant::now(), self.frame);
        self.start_audio();
        self.dirty = true;
        self.publish(true);
    }

    /// J: plays backwards at `speed` from the playhead (a play forward stops first).
    fn start_reverse(&mut self, speed: f64) {
        if self.plan.is_none() {
            return;
        }
        if self.playing {
            self.pause();
        }
        let speed = if speed.is_finite() {
            speed.clamp(0.25, 8.0)
        } else {
            1.0
        };
        self.reverse = Some((Instant::now(), self.frame, speed));
        self.rate = -speed;
        self.publish(true);
    }

    /// Ends a backwards play where it is (the exact frame settles in).
    fn stop_reverse(&mut self) {
        if self.reverse.take().is_some() {
            self.rate = 1.0;
            self.scrub_settle = Some(Instant::now());
            self.dirty = true;
            self.publish(true);
        }
    }

    fn pause(&mut self) {
        self.playing = false;
        self.range_end = None;
        self.range_from = None;
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        if self.full {
            self.resize();
        }
        self.dirty = true;
        self.publish(true);
    }

    #[cfg(any(target_os = "macos", windows))]
    fn gizmos(&self) -> Vec<gmclip_render::export::preview::GizmoQuad> {
        use gmclip_render::export::preview::GizmoQuad;
        if self.playing {
            return Vec::new();
        }
        let (cw, ch) = self.canvas();
        let size = self.display_size();
        let (s, sy) = (f64::from(size.0) / cw, f64::from(size.1) / ch);
        // Sizes in device pixels on screen, whatever the frame's size.
        let k = self.screen_scale() as f32;
        let mut q = Vec::new();
        let mut outline = |r: Rect, t: f32, c: [f32; 4]| {
            let (x, y, w, h) = (
                (r.x * s) as f32,
                (r.y * sy) as f32,
                (r.w * s) as f32,
                (r.h * sy) as f32,
            );
            q.push(GizmoQuad {
                x,
                y,
                w,
                h: t,
                rgba: c,
            });
            q.push(GizmoQuad {
                x,
                y: y + h - t,
                w,
                h: t,
                rgba: c,
            });
            q.push(GizmoQuad {
                x,
                y,
                w: t,
                h,
                rgba: c,
            });
            q.push(GizmoQuad {
                x: x + w - t,
                y,
                w: t,
                h,
                rgba: c,
            });
        };
        let boxes = self.boxes();
        // R-COMP-1: at full quality only a selected layer's gizmo is drawn, so a paused
        // full-quality frame with nothing selected is the export's frame.
        for b in boxes.iter().filter(|_| !self.full) {
            let sel = self
                .selected
                .as_ref()
                .is_some_and(|(p, l)| &b.0 == p && &b.1 == l);
            if !sel {
                outline(b.2, k, premul([1.0, 1.0, 1.0, 1.0], 0.35));
            }
        }
        if let Some(b) = boxes.iter().find(|b| {
            self.selected
                .as_ref()
                .is_some_and(|(p, l)| &b.0 == p && &b.1 == l)
        }) {
            outline(b.2, 2.0 * k, ACCENT);
            let hs = 10.0 * k;
            for (cx, cy) in [
                (b.2.x, b.2.y),
                (b.2.x + b.2.w, b.2.y),
                (b.2.x, b.2.y + b.2.h),
                (b.2.x + b.2.w, b.2.y + b.2.h),
            ] {
                let (px, py) = ((cx * s) as f32, (cy * sy) as f32);
                q.push(GizmoQuad {
                    x: px - hs / 2.0,
                    y: py - hs / 2.0,
                    w: hs,
                    h: hs,
                    rgba: [1.0, 1.0, 1.0, 1.0],
                });
                q.push(GizmoQuad {
                    x: px - hs / 2.0 + 2.0 * k,
                    y: py - hs / 2.0 + 2.0 * k,
                    w: hs - 4.0 * k,
                    h: hs - 4.0 * k,
                    rgba: ACCENT,
                });
            }
        }
        // snap guides of a drag: thin lines across the frame
        for (vertical, at) in &self.guides {
            let t = k;
            q.push(if *vertical {
                GizmoQuad {
                    x: (at * s) as f32 - t / 2.0,
                    y: 0.0,
                    w: t,
                    h: size.1 as f32,
                    rgba: GUIDE,
                }
            } else {
                GizmoQuad {
                    x: 0.0,
                    y: (at * sy) as f32 - t / 2.0,
                    w: size.0 as f32,
                    h: t,
                    rgba: GUIDE,
                }
            });
        }
        q
    }

    #[cfg(any(target_os = "macos", windows))]
    fn render(&mut self) {
        use gmclip_render::export::preview::LayerOverride;
        if !self.connected.load(Ordering::Relaxed) {
            debug("render skipped: no frame socket client");
            return;
        }
        let gizmos = self.gizmos();
        let overrides: Vec<LayerOverride> = [&self.drag, &self.pending]
            .into_iter()
            .flatten()
            .map(|d| LayerOverride {
                part_id: d.part.clone(),
                layer_id: d.layer.clone(),
                place: Some(d.now),
                crop: None,
            })
            .collect();
        let (s0, s1) = (self.sample_at(self.frame), self.sample_at(self.frame + 1));
        let Some(s) = self.session.as_mut() else {
            debug("render skipped: no preview session");
            return;
        };
        // a live text drag (U.12) draws in the frame itself, through the same compose
        s.set_text_shifts(self.text_drag.iter().cloned().collect());
        let t0 = Instant::now();
        let heard = self
            .playing
            .then(|| {
                self.audio
                    .as_ref()
                    .and_then(|a| a.position())
                    .map(|(p, e)| (p + self.play_shift as f64, e))
            })
            .flatten();
        let scrubbing = self.scrub_settle.is_some() && !self.playing;
        if scrubbing
            && self
                .proxy_dirs_at
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            && let Some(core) = self.core.upgrade()
        {
            s.set_proxy_dirs(core.proxies.dirs());
            self.proxy_dirs_at = Some(Instant::now());
        }
        let r = if scrubbing {
            s.render_scrub(self.frame, &overrides, &gizmos)
                .map(|(f, p)| (f, p.as_str()))
        } else {
            s.render(self.frame, &overrides, &gizmos)
                .map(|f| (f, "exact"))
        };
        self.draft_matte = s.drew_draft_matte();
        match r {
            Ok((f, path)) => {
                self.scrub_path = Some(path);
                self.seq = self.seq.wrapping_add(1);
                let out = OutFrame {
                    seq: self.seq,
                    out_frame: self.frame as u32,
                    width: f.width as u16,
                    height: f.height as u16,
                    echo_t: self.echo_t,
                    render_us: t0.elapsed().as_micros().min(u128::from(u32::MAX)) as u32,
                    flags: (if self.playing { FLAG_PLAYING } else { 0 })
                        | (if self.full { FLAG_FULL } else { 0 }),
                    // cumulative, mod 2^16: the UI takes differences
                    dropped: (self.dropped & 0xffff) as u16,
                    y: f.y,
                    uv: f.uv,
                };
                // Late frames drop: a newer frame replaces one the socket has not sent.
                if self.frames.put(out) {
                    self.drop_frames(1);
                }
                // A/V: this frame's centre against the sample heard as it is handed over
                if let Some((pos, false)) = heard {
                    let off = (s0 as f64 + (s1 - s0) as f64 / 2.0 - pos) / 48.0;
                    self.av_sum += off.abs();
                    self.av_max = self.av_max.max(off.abs());
                    self.av_n += 1;
                }
                self.rendered += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_error(Some(msg));
                self.playing = false;
            }
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn render(&mut self) {}

    fn run(&mut self, rx: &Receiver<ViewerMsg>) {
        loop {
            #[cfg(windows)]
            if self.playing != self.fine_timer.is_some() {
                self.fine_timer = self
                    .playing
                    .then(gmclip_platform::windows::FineTimer::begin);
            }
            let heard = if self.playing {
                self.audio
                    .as_ref()
                    .and_then(|a| a.position())
                    .map(|(p, e)| (p + self.play_shift as f64, e))
            } else {
                None
            };
            let timeout = if let Some((pos, _)) = heard {
                // until the next frame's first sample is heard
                let next = self.sample_at(self.frame + 1) as f64;
                Duration::from_secs_f64(((next - pos) / (48_000.0 * self.rate)).clamp(0.001, 0.05))
            } else if self.playing && self.audio.is_some() {
                Duration::from_millis(2)
            } else if let Some((_, _, speed)) = self.reverse {
                Duration::from_secs_f64((1.0 / (self.fps().max(1.0) * speed)).clamp(0.004, 0.05))
            } else if self.playing {
                let fps = self.fps().max(1.0) * self.rate;
                let next = self.anchor.0
                    + Duration::from_secs_f64(
                        (self.frame + 1).saturating_sub(self.anchor.1) as f64 / fps,
                    );
                next.saturating_duration_since(Instant::now())
            } else if let Some(due) = self.scrub_settle {
                due.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1))
            } else {
                Duration::from_millis(250)
            };
            let first = match rx.recv_timeout(timeout) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let mut msgs: Vec<ViewerMsg> = first.into_iter().collect();
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
            }
            for m in msgs {
                // anything else asked of the viewer ends a loop
                if !matches!(
                    m,
                    ViewerMsg::PlayRange { .. }
                        | ViewerMsg::Records(_)
                        | ViewerMsg::Select(_)
                        | ViewerMsg::Group(_)
                        | ViewerMsg::Replan { .. }
                        | ViewerMsg::Full(_)
                        | ViewerMsg::Stress(_)
                        | ViewerMsg::Scrub(_)
                        | ViewerMsg::AudioDeviceChanged
                        | ViewerMsg::Rate(_)
                        | ViewerMsg::TextDrag(_)
                        | ViewerMsg::TextDragEnd { .. }
                ) {
                    self.loop_range = None;
                }
                // a backwards play ends on anything but a look change or a new speed;
                // Space (toggle) only stops it
                if self.reverse.is_some()
                    && !matches!(
                        m,
                        ViewerMsg::Records(_)
                            | ViewerMsg::Select(_)
                            | ViewerMsg::Group(_)
                            | ViewerMsg::Replan { .. }
                            | ViewerMsg::Full(_)
                            | ViewerMsg::Stress(_)
                            | ViewerMsg::Scrub(_)
                            | ViewerMsg::AudioDeviceChanged
                            | ViewerMsg::MatteView(_)
                            | ViewerMsg::Rate(_)
                    )
                {
                    let toggle = matches!(m, ViewerMsg::Toggle);
                    self.stop_reverse();
                    if toggle {
                        continue;
                    }
                }
                match m {
                    ViewerMsg::Load {
                        project,
                        cut,
                        framing,
                        keep,
                    } => {
                        self.looping = false;
                        self.load(&project, &cut, framing, keep);
                    }
                    ViewerMsg::LoadPlan {
                        key,
                        plan,
                        looping,
                        at,
                    } => {
                        self.key = Some(key);
                        self.peek = None;
                        self.drag = None;
                        self.pending = None;
                        self.text_drag = None;
                        self.text_pending = None;
                        self.selected = None;
                        self.looping = looping;
                        self.plan_rev = 0;
                        self.install(plan);
                        self.frame = 0;
                        self.set_error(None);
                        match at {
                            Some(f) => {
                                if self.playing {
                                    self.pause();
                                }
                                self.frame = f.min(self.frames_total().saturating_sub(1));
                                self.dirty = true;
                                self.publish(true);
                            }
                            None => self.play(),
                        }
                    }
                    ViewerMsg::Unload => {
                        self.key = None;
                        self.plan = None;
                        self.peek = None;
                        self.drag = None;
                        self.pending = None;
                        self.text_drag = None;
                        self.text_pending = None;
                        self.selected = None;
                        #[cfg(any(target_os = "macos", windows))]
                        {
                            self.session = None;
                        }
                        if self.playing {
                            self.pause();
                        }
                        self.publish(true);
                    }
                    ViewerMsg::Replan { project } => self.replan(&project),
                    ViewerMsg::TextDrag(t) if !self.playing && self.plan.is_some() => {
                        self.text_drag = Some(t);
                        self.text_pending = None;
                        self.dirty = true;
                        self.publish(false);
                    }
                    // playing (the gizmos are off) or nothing loaded: nothing to move
                    ViewerMsg::TextDrag(_) => {}
                    ViewerMsg::TextDragEnd { committed } => {
                        if committed && self.text_drag.is_some() {
                            self.text_pending = Some(Instant::now());
                        } else {
                            self.text_drag = None;
                            self.text_pending = None;
                            self.dirty = true;
                        }
                        self.publish(true);
                    }
                    ViewerMsg::ScrubTo(f) if !self.playing => {
                        self.frame = f.min(self.frames_total().saturating_sub(1));
                        self.scrub_settle = Some(Instant::now() + SCRUB_SETTLE);
                        self.grain();
                        self.dirty = true;
                        self.publish(true);
                    }
                    ViewerMsg::Seek(f) | ViewerMsg::ScrubTo(f) => {
                        self.scrub_settle = None;
                        self.frame = f.min(self.frames_total().saturating_sub(1));
                        if self.playing {
                            self.anchor = (Instant::now(), self.frame);
                            self.range_end = None;
                            self.start_audio();
                        } else {
                            self.grain();
                        }
                        self.dirty = true;
                        self.publish(true);
                    }
                    ViewerMsg::Play => self.play(),
                    ViewerMsg::Pause => self.pause(),
                    ViewerMsg::Toggle => self.toggle(),
                    ViewerMsg::Step(n) => self.step(n),
                    ViewerMsg::Full(f) => {
                        self.full = f;
                        self.resize();
                        self.publish(true);
                    }
                    ViewerMsg::Scrub(on) => {
                        self.scrub = on;
                        self.publish(true);
                    }
                    ViewerMsg::AudioDeviceChanged => self.reopen_audio(),
                    ViewerMsg::MatteView(on) => {
                        self.matte_view = on;
                        #[cfg(any(target_os = "macos", windows))]
                        if let Some(s) = self.session.as_mut() {
                            s.set_matte_view(on);
                        }
                        self.dirty = true;
                        self.publish(true);
                    }
                    ViewerMsg::Rate(r) if r < 0.0 => self.start_reverse(-r),
                    ViewerMsg::Rate(r) => {
                        self.stop_reverse();
                        self.rate = if r.is_finite() {
                            r.clamp(0.25, 8.0)
                        } else {
                            1.0
                        };
                        self.anchor = (Instant::now(), self.frame);
                        if self.playing {
                            self.start_audio();
                        }
                        self.publish(true);
                    }
                    ViewerMsg::PlayRange { from, to, looping } => {
                        let total = self.sample_at(self.frames_total());
                        self.loop_range = looping.then_some((from, to.min(total)));
                        if self.plan.is_some() && from < to && from < total {
                            self.frame = self.frame_at(from);
                            self.range_from = Some(from);
                            self.range_end = Some(to.min(total));
                            self.looping = false;
                            self.rate = 1.0;
                            self.playing = false;
                            self.play();
                        }
                    }
                    ViewerMsg::PlaySrc { from_ms, to_ms } => {
                        if let (Some(a), Some(b)) = (
                            self.src_to_out(from_ms),
                            self.src_to_out((to_ms - 1).max(from_ms)),
                        ) {
                            let total = self.sample_at(self.frames_total());
                            let to = (b + 48).min(total);
                            if a < to {
                                self.frame = self.frame_at(a);
                                self.range_from = Some(a);
                                self.range_end = Some(to);
                                self.looping = false;
                                self.rate = 1.0;
                                self.playing = false;
                                self.play();
                            }
                        }
                    }
                    ViewerMsg::Stress(on) => {
                        self.stress = on;
                        self.resize();
                    }
                    ViewerMsg::Group(g) => self.group = g,
                    ViewerMsg::Select(s) => {
                        self.selected = s;
                        self.dirty = true;
                        self.publish(true);
                    }
                    ViewerMsg::Records(r) => self.on_records(r),
                    ViewerMsg::TrimPeek { plan, frame } if self.plan.is_some() => {
                        if self.playing {
                            self.pause();
                        }
                        if self.peek.is_none() {
                            self.peek = Some((self.plan.clone(), self.frame));
                        }
                        self.scrub_settle = None;
                        self.install(plan);
                        self.frame = frame.min(self.frames_total().saturating_sub(1));
                        self.dirty = true;
                        self.publish(true);
                    }
                    // nothing loaded: nothing to trim
                    ViewerMsg::TrimPeek { .. } => {}
                    ViewerMsg::TrimPeekEnd => {
                        if let Some((cut, back)) = self.peek.take() {
                            if let Some(plan) = cut {
                                self.install(plan);
                            }
                            self.frame = back.min(self.frames_total().saturating_sub(1));
                            self.dirty = true;
                            self.publish(true);
                        }
                    }
                }
            }
            // the device's clock, while it runs (a play that has not reached the speakers
            // yet holds its first frame for up to 300 ms, then falls back to the wall clock)
            let heard = if self.playing {
                self.audio
                    .as_ref()
                    .and_then(|a| a.position())
                    .map(|(p, e)| (p + self.play_shift as f64, e))
            } else {
                None
            };
            let last = self.frames_total().saturating_sub(1);
            if let Some((pos, ended)) = heard {
                let target = self.frame_at(pos.max(0.0).floor() as u64).min(last);
                if target != self.frame {
                    if target > self.frame {
                        self.drop_frames(target - self.frame - 1);
                    }
                    self.frame = target;
                    self.dirty = true;
                }
                if ended {
                    if let Some(to) = self.range_end {
                        self.frame = self.frame_at(to.saturating_sub(1)).min(last);
                    }
                    self.dirty = true;
                    self.pause();
                    self.replay_loop();
                }
            } else if self.playing
                && (self.audio.is_none()
                    || self.play_started.elapsed() > Duration::from_millis(300))
            {
                let fps = self.fps().max(1.0) * self.rate;
                let elapsed = self.anchor.0.elapsed().as_secs_f64();
                let target = (self.anchor.1 + (elapsed * fps).floor() as u64).min(last);
                if target > self.frame {
                    self.drop_frames(target - self.frame - 1);
                    self.frame = target;
                    self.dirty = true;
                }
            }
            if let Some((t0, f0, speed)) = self.reverse {
                let back = (t0.elapsed().as_secs_f64() * self.fps().max(1.0) * speed).floor();
                let target = reverse_target(f0, back).min(last);
                if target != self.frame {
                    self.frame = target;
                    self.scrub_settle = Some(Instant::now() + SCRUB_SETTLE);
                    self.dirty = true;
                }
                if target == 0 {
                    self.stop_reverse();
                }
            }
            if self.playing
                && heard.is_none()
                && let Some(to) = self.range_end
                && self.sample_at(self.frame + 1) >= to
            {
                self.pause();
                self.replay_loop();
            }
            if self.playing && heard.is_none() && self.frame + 1 >= self.frames_total() {
                if self.looping {
                    self.frame = 0;
                    self.anchor = (Instant::now(), 0);
                    self.dirty = true;
                } else {
                    self.frame = self.frames_total().saturating_sub(1);
                    self.pause();
                }
            }
            self.check_device();
            if self
                .text_pending
                .is_some_and(|t| t.elapsed() >= TEXT_PENDING_MAX)
            {
                self.end_text_pending();
                self.dirty = true;
                self.publish(true);
            }
            // A client that just connected gets the current frame.
            let c = self.connected.load(Ordering::Relaxed);
            if c && !self.was_connected {
                self.dirty = true;
            }
            self.was_connected = c;
            // a scrub that rested (or a play that started) shows the exact frame
            if let Some(due) = self.scrub_settle
                && (self.playing || Instant::now() >= due)
            {
                self.scrub_settle = None;
                self.dirty = true;
            }
            if self.dirty {
                self.dirty = false;
                self.render();
            }
            // Rate-limited; a change held back now goes out on a later turn.
            self.publish(false);
        }
    }
}

#[cfg(test)]
mod snap_tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_moved_box_snaps_to_the_frame_centre_and_other_layers() {
        let canvas = (1080.0, 1920.0);
        // centre 3 px off the frame's vertical centre line: snapped, one guide
        let (o, g) = snap_rect(r(243.0, 700.0, 600.0, 400.0), None, 0.0, canvas, &[], 8.0);
        assert_eq!((o.x, o.y), (240.0, 700.0));
        assert_eq!(g, vec![(true, 540.0)]);
        // top 5 px below another layer's bottom: snapped to it
        let cam = r(160.0, 1120.0, 760.0, 520.0);
        let (o, g) = snap_rect(
            r(100.0, 1645.0, 300.0, 100.0),
            None,
            0.0,
            canvas,
            &[cam],
            8.0,
        );
        assert_eq!(o.y, 1640.0);
        assert!(g.contains(&(false, 1640.0)));
        // out of reach: unchanged, no guides
        let far = r(300.0, 300.0, 200.0, 200.0);
        assert_eq!(snap_rect(far, None, 0.0, canvas, &[], 8.0), (far, vec![]));
        // past the frame's left edge by 4 px: snapped back to 0
        assert_eq!(
            snap_rect(r(-4.0, 10.0, 50.0, 50.0), None, 0.0, canvas, &[], 8.0)
                .0
                .x,
            0.0
        );
    }

    #[test]
    fn a_resized_box_snaps_its_moving_edge_and_keeps_its_aspect() {
        // bottom-right corner, right edge 1076 → 1080; height follows the aspect
        let (o, g) = snap_rect(
            r(80.0, 250.0, 996.0, 498.0),
            Some(3),
            0.5,
            (1080.0, 1920.0),
            &[],
            8.0,
        );
        assert_eq!((o.x, o.y, o.w, o.h), (80.0, 250.0, 1000.0, 500.0));
        assert_eq!(g, vec![(true, 1080.0)]);
        // top-left corner: the left edge 5 → 0; the bottom-right corner stays put
        let (o, _) = snap_rect(
            r(5.0, 105.0, 400.0, 200.0),
            Some(0),
            0.5,
            (1080.0, 1920.0),
            &[],
            8.0,
        );
        assert_eq!((o.x, o.w, o.h), (0.0, 405.0, 202.5));
        assert_eq!(o.y + o.h, 305.0);
    }
}

#[cfg(test)]
mod reverse_tests {
    use super::reverse_target;

    #[test]
    fn backwards_play_counts_whole_frames_and_stops_at_zero() {
        assert_eq!(reverse_target(300, 0.0), 300);
        assert_eq!(reverse_target(300, 29.9), 271);
        assert_eq!(reverse_target(300, 60.0), 240);
        assert_eq!(reverse_target(10, 45.0), 0);
        assert_eq!(reverse_target(10, f64::NAN), 10);
        assert_eq!(reverse_target(10, -3.0), 10);
        assert_eq!(reverse_target(u64::MAX, f64::INFINITY), u64::MAX);
    }
}

/// The plan the viewer shows: the cut in `framing` (its own when none), or the crop
/// editor's source picture of a part (`source:<part>`, or `source:<part>:<layer>` for a
/// layer of another source, [`crate::framing::source_view_plan`]).
fn view_plan(
    core: &crate::core::AppCore,
    project: &str,
    cut: &str,
    framing: Option<&str>,
) -> crate::error::AppResult<crate::core::Planned> {
    match framing.and_then(|f| f.strip_prefix("source:")) {
        Some(k) => {
            let (part, layer) = k.split_once(':').map_or((k, None), |(p, l)| (p, Some(l)));
            crate::framing::source_view_plan(core, project, cut, part, layer)
        }
        None => core.planned_in(project, cut, framing),
    }
}
