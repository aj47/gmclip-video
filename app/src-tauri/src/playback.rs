//! Playback audio and the playback clock (SPEC §6.3, §6.4; M2).
//!
//! - **The mix the export makes.** A producer thread mixes the plan's audio in chunks
//!   with `AudioPlan::mix_range` (bit-identical to the export's mix, read through the
//!   export's own `PlanPcm`): each source's track named by the plan (`audio_track`, the
//!   mic — decision 0009), the sources under it and the music over the cut (decision
//!   0051), part gains, de-click
//!   fades at joins exactly where the export puts them, envelopes; static loudness as a
//!   gain measured on the whole mix (computed once per plan in the background; the
//!   export's true-peak limiter is not run, so samples are clamped to ±1 instead).
//! - **Output** is the default device through `gmclip_gpu::{apple,windows}::audio_out`; chunks
//!   reach the real-time callback through a bounded channel ~170 ms deep. Joins need no
//!   extra preroll: the producer runs ahead of the device, and each source keeps its
//!   own decoder window (`AacRandomReader`).
//! - **The callback never allocates, frees or locks** ([`Render`]). Chunk buffers go
//!   back to the producer through a second channel (the producer reuses them), the clock
//!   is a sequence lock of atomics the callback alone writes, and both channels are only
//!   ever `try_`-used on both ends, so neither has a waiter to wake (std's channels lock
//!   only to wake a blocked peer). `tests/audio_rt.rs` counts the callback thread's
//!   allocations through a whole session of plays, switches, grains and stops: zero.
//! - **The clock follows the audio device.** Each callback records which plan sample
//!   reaches the speakers at which host time (the device's latency included); video asks
//!   for the plan sample "now" and shows that frame, dropping or holding frames, never
//!   stalling the audio. When the output device changes the viewer reopens the engine on
//!   the new one (its latency is the one the clock adds) and measures A/V again.
//! - **Rates.** ×0.5–×2 play resampled (linear, pitch follows speed); outside that range
//!   the audio is muted and the clock still runs at the rate.
//! - **Ranges.** A play can stop at an exact sample (P plays one word) and can loop
//!   (review mode).
//! - **Scrub grains** (H.02). While paused, each scrub step or seek asks for a grain:
//!   [`GRAIN`] samples of the mix at the playhead under a Tukey window ([`GRAIN_FADE`] in
//!   and out), so every grain starts and ends at silence. Grains of one scrub play back
//!   to back and are never cut off by the next: the newest step waits until the grain
//!   playing has half of itself left, and steps in between are skipped (latest wins).
//! - **No clicks between plays.** A play that replaces a sounding one (a grain, a seek)
//!   starts with a [`DECLICK`] fade-in, and the chunk it cuts off fades out underneath.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::Duration;

use std::sync::{Arc, Mutex};

#[cfg(any(target_os = "macos", windows))]
use gmclip_media::audio::graph::{AudioPlan, LoudnessMode};
use gmclip_plan::Plan;
#[cfg(any(target_os = "macos", windows))]
use gmclip_render::export::PlanPcm;

/// The default output device: CoreAudio on macOS, WASAPI on Windows (same API).
#[cfg(target_os = "macos")]
use gmclip_gpu::apple::audio_out as audio_dev;
#[cfg(windows)]
use gmclip_gpu::windows::audio_out as audio_dev;

/// Output frames per chunk (21 ms).
const CHUNK: usize = 1024;
/// Chunks queued ahead of the device (~170 ms).
const DEPTH: usize = 8;
/// Retargets the callback looks back over (more than can happen in one queue's time).
const RETARGETS: usize = 16;
/// A retarget mixes again the queued chunks from this many frames past the one being
/// heard (time for the first one to be mixed before the device needs it).
const REMIX_AHEAD: usize = 2 * CHUNK;
/// Chunk buffers handed back to the producer. More than can ever be out at once (the
/// queue, the playing chunk, a fading one, the producer's own), so the callback's
/// hand-back never finds it full.
const SPARE: usize = 32;
pub const RATE: f64 = 48_000.0;
/// A scrub grain: 80 ms of the mix.
pub const GRAIN: usize = 3_840;
/// A grain fades in and out over 5 ms (a raised cosine).
pub const GRAIN_FADE: usize = 240;
/// A play that replaces a sounding one fades in over this many frames (1.3 ms), and the
/// chunk it cuts off fades out under it.
pub const DECLICK: usize = 64;

struct Chunk {
    epoch: u64,
    /// The play's plan generation it was mixed in (each retarget is one more).
    generation: u64,
    /// Plan sample of the chunk's first frame, and plan samples per frame (the rate).
    pos: f64,
    step: f64,
    data: Vec<f32>,
    /// The play (or the grain) ends after this chunk.
    last: bool,
}

/// Where the device is: `pos` (plan samples) is heard at host time `at_ns`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Clock {
    epoch: u64,
    pos: f64,
    at_ns: u64,
    step: f64,
    valid: bool,
    /// The play has ended (range end, end of the cut, the end of a grain).
    ended: bool,
}

/// The clock as a sequence lock: the callback is its only writer and never waits;
/// readers retry while a write is in progress. Every field is an atomic, so a torn read
/// is detected (by the sequence) rather than undefined.
#[derive(Default)]
struct ClockCell {
    seq: AtomicU64,
    epoch: AtomicU64,
    pos: AtomicU64,
    at_ns: AtomicU64,
    step: AtomicU64,
    flags: AtomicU64,
}

impl ClockCell {
    /// One writer only (the callback).
    fn write(&self, c: &Clock) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.epoch.store(c.epoch, Ordering::Relaxed);
        self.pos.store(c.pos.to_bits(), Ordering::Relaxed);
        self.at_ns.store(c.at_ns, Ordering::Relaxed);
        self.step.store(c.step.to_bits(), Ordering::Relaxed);
        self.flags.store(
            u64::from(c.valid) | (u64::from(c.ended) << 1),
            Ordering::Relaxed,
        );
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    fn read(&self) -> Clock {
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let flags = self.flags.load(Ordering::Relaxed);
            let c = Clock {
                epoch: self.epoch.load(Ordering::Relaxed),
                pos: f64::from_bits(self.pos.load(Ordering::Relaxed)),
                at_ns: self.at_ns.load(Ordering::Relaxed),
                step: f64::from_bits(self.step.load(Ordering::Relaxed)),
                valid: flags & 1 != 0,
                ended: flags & 2 != 0,
            };
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == s1 {
                return c;
            }
        }
    }
}

struct Shared {
    epoch: AtomicU64,
    clock: ClockCell,
    underruns: AtomicU64,
    muted_rate: AtomicBool,
    /// Frames queued for the device (the producer adds, the callback takes): grains wait
    /// on it so a scrub stays ≤ one grain behind the pointer.
    queued: AtomicUsize,
    /// The play's plan generation (the producer alone writes it): a retarget (an edit
    /// while playing) is one more. Chunks of an older one from `cutoffs[g % RETARGETS]`
    /// on (plan samples, `f64` bits) were mixed again in generation `g`: the callback
    /// skips them.
    generation: AtomicU64,
    cutoffs: [AtomicU64; RETARGETS],
    /// Of `queued`, the frames of skipped chunks the callback has not reached yet.
    stale: AtomicUsize,
}

enum Cmd {
    Play {
        plan: Arc<Plan>,
        from: f64,
        to: Option<u64>,
        rate: f64,
        looping: bool,
        epoch: u64,
    },
    /// A scrub grain at plan sample `at` (the scrub's epoch: grains of one scrub queue
    /// behind each other).
    Grain {
        plan: Arc<Plan>,
        at: u64,
        epoch: u64,
    },
    /// The play goes on in an edited plan (decision 0053): chunks not mixed yet come from
    /// `plan`, `shift` samples on from where the old plan was.
    Retarget {
        plan: Arc<Plan>,
        shift: i64,
        epoch: u64,
    },
    Stop,
}

/// What the producer mixes for a plan: output samples `[from, from + out.len() / 2)`,
/// interleaved stereo, at the plan's 48 kHz.
pub trait Mix: Send {
    /// Output samples in the plan.
    fn total(&self) -> u64;
    fn mix(&mut self, from: u64, out: &mut [f32]);
    /// The static loudness gain to apply (1 until measured).
    fn gain(&self) -> f32 {
        1.0
    }
}

/// Makes the mix of a plan (the real one decodes its sources; tests pass a synthetic one).
pub type MakeMix = Box<dyn FnMut(&Arc<Plan>) -> Option<Box<dyn Mix>> + Send>;

/// The mix [`Audio::open`] plays for `plan` (also for a headless engine: playback =
/// export, decision 0053).
#[cfg(any(target_os = "macos", windows))]
pub fn plan_mix(plan: &Arc<Plan>) -> Result<Box<dyn Mix>, String> {
    Mixer::new(Arc::clone(plan)).map(|m| Box::new(m) as Box<dyn Mix>)
}

/// The playback engine (one per app, owned by the render thread).
pub struct Audio {
    #[cfg(any(target_os = "macos", windows))]
    out: Option<audio_dev::AudioOut>,
    shared: Arc<Shared>,
    cmd: mpsc::Sender<Cmd>,
    /// The output device this engine was opened on.
    device: u32,
    play_from: f64,
    /// The epoch grains of the current scrub share (None when not scrubbing).
    scrub_epoch: Option<u64>,
    /// Why the last plan has no sound (its audio would not build), shown on the viewer
    /// instead of a silent preview.
    no_sound: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Audio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Audio").finish_non_exhaustive()
    }
}

/// Host time now (ns), the clock the device reports on.
pub fn now_ns() -> u64 {
    #[cfg(any(target_os = "macos", windows))]
    {
        audio_dev::host_now_ns()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
    }
}

impl Audio {
    /// Opens the default output device. `None` when there is none (video then runs on
    /// the wall clock).
    pub fn open() -> Option<Audio> {
        #[cfg(any(target_os = "macos", windows))]
        {
            let no_sound = Arc::new(Mutex::new(None));
            let why = Arc::clone(&no_sound);
            let (mut a, mut render) = Audio::engine(Box::new(move |plan: &Arc<Plan>| {
                let m = plan_mix(plan);
                let e = m.as_ref().err().cloned();
                if let Some(e) = &e {
                    crate::applog::event("audio.no_sound", None, e);
                }
                if let Ok(mut w) = why.lock() {
                    *w = e;
                }
                m.ok()
            }))?;
            a.no_sound = no_sound;
            // tests run the whole path without making a sound
            render.silent = std::env::var_os("GMCLIP_PLAYBACK_SILENT").is_some();
            let out = audio_dev::AudioOut::open(Box::new(move |o: &mut [f32], at: u64| {
                render.fill(o, at)
            }))
            .ok()?;
            a.out = Some(out);
            a.device = audio_dev::default_output_device();
            Some(a)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        None
    }

    /// The engine without a device: the caller runs the real-time side by calling
    /// [`Render::fill`] (tests: the callback's allocations, grains, de-click).
    pub fn headless(make: MakeMix) -> Option<(Audio, Render)> {
        Audio::engine(make)
    }

    fn engine(make: MakeMix) -> Option<(Audio, Render)> {
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(0),
            clock: ClockCell::default(),
            underruns: AtomicU64::new(0),
            muted_rate: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            cutoffs: Default::default(),
            stale: AtomicUsize::new(0),
        });
        // room behind a retarget's skipped chunks for the ones mixed again (the producer
        // keeps DEPTH chunks of sound ahead)
        let (tx, rx) = mpsc::sync_channel::<Chunk>(2 * DEPTH);
        let (spare_tx, spare_rx) = mpsc::sync_channel::<Chunk>(SPARE);
        let (ctx, crx) = mpsc::channel::<Cmd>();
        let s3 = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("gmclip-audio-mix".into())
            .spawn(move || {
                let mut make = make;
                producer(&crx, &tx, &spare_rx, &s3, &mut make);
            })
            .ok()?;
        let render = Render {
            rx,
            spare: spare_tx,
            sh: Arc::clone(&shared),
            cur: None,
            fading: None,
            ended_at: None,
            mid: false,
            next_ns: 0,
            step: 1.0,
            silent: false,
        };
        Some((
            Audio {
                #[cfg(any(target_os = "macos", windows))]
                out: None,
                device: 0,
                shared,
                cmd: ctx,
                play_from: 0.0,
                scrub_epoch: None,
                no_sound: Arc::default(),
            },
            render,
        ))
    }

    /// Whether the system's default output device is no longer the one this engine
    /// opened (headphones plugged, AirPods connected): the caller reopens.
    pub fn device_changed(&self) -> bool {
        #[cfg(any(target_os = "macos", windows))]
        {
            if self.out.is_none() {
                return false;
            }
            let now = audio_dev::default_output_device();
            now != 0 && now != self.device
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        false
    }

    /// The device's presentation latency (ms).
    pub fn latency_ms(&self) -> f64 {
        #[cfg(any(target_os = "macos", windows))]
        {
            self.out
                .as_ref()
                .map_or(0.0, |o| o.latency_ns() as f64 / 1e6)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        0.0
    }

    /// Reads the device's latency again (a Bluetooth device settles after it connects);
    /// true when it moved by more than 0.5 ms, so the clock now adds the new one.
    pub fn refresh_latency(&mut self) -> bool {
        #[cfg(any(target_os = "macos", windows))]
        if let Some(o) = self.out.as_mut() {
            let before = o.latency_ns();
            let now = o.refresh_latency();
            return before.abs_diff(now) > 500_000;
        }
        false
    }

    fn next_epoch(&mut self) -> u64 {
        self.scrub_epoch = None;
        self.shared.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn start_out(&mut self) {
        #[cfg(any(target_os = "macos", windows))]
        if let Some(o) = self.out.as_mut() {
            let _ = o.start();
        }
    }

    /// Goes on playing in an edited `plan` without a break (decision 0053): the queued
    /// chunks up to [`REMIX_AHEAD`] past the one being filled play as they are (~40 ms);
    /// the device skips the rest, which are mixed again from `plan`, `shift` samples on
    /// from where the old plan was (an edit before the playhead moves it). The clock keeps the old plan's numbering for the rest of this
    /// play: the caller adds `shift` to [`Self::position`]. No effect on a range, a loop
    /// or a scrub (their caller plays them again).
    pub fn retarget(&mut self, plan: Arc<Plan>, shift: i64) {
        let epoch = self.shared.epoch.load(Ordering::SeqCst);
        let _ = self.cmd.send(Cmd::Retarget { plan, shift, epoch });
    }

    /// Plays `plan` from plan sample `from` (to `to`, exclusive) at `rate`.
    pub fn play(&mut self, plan: Arc<Plan>, from: f64, to: Option<u64>, rate: f64, looping: bool) {
        let epoch = self.next_epoch();
        self.play_from = from;
        let _ = self.cmd.send(Cmd::Play {
            plan,
            from,
            to,
            rate,
            looping,
            epoch,
        });
        self.start_out();
    }

    /// A scrub grain at plan sample `at`: [`GRAIN`] samples under a Tukey window, after
    /// the grain playing now (a newer step replaces one still waiting).
    pub fn grain(&mut self, plan: Arc<Plan>, at: u64) {
        let now = self.shared.epoch.load(Ordering::SeqCst);
        let epoch = match self.scrub_epoch {
            Some(e) if e == now => e,
            _ => {
                let e = self.next_epoch();
                self.scrub_epoch = Some(e);
                e
            }
        };
        let _ = self.cmd.send(Cmd::Grain { plan, at, epoch });
        self.start_out();
    }

    pub fn stop(&mut self) {
        self.next_epoch();
        let _ = self.cmd.send(Cmd::Stop);
        #[cfg(any(target_os = "macos", windows))]
        if let Some(o) = self.out.as_mut() {
            o.stop();
        }
    }

    /// The plan sample being heard now; `None` until the device has played the first
    /// chunk of this play. `ended` once the play's range is over.
    pub fn position(&self) -> Option<(f64, bool)> {
        let epoch = self.shared.epoch.load(Ordering::SeqCst);
        let c = self.shared.clock.read();
        if c.epoch != epoch || !c.valid {
            return None;
        }
        if c.ended {
            return Some((c.pos, true));
        }
        let now = now_ns();
        let dt = (now as f64 - c.at_ns as f64) / 1e9;
        // before the first sample is heard the playhead waits at it
        Some((
            (c.pos + dt.max(0.0) * RATE * c.step).max(c.pos.min(self.play_from)),
            false,
        ))
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    pub fn muted(&self) -> bool {
        self.shared.muted_rate.load(Ordering::Relaxed)
    }

    /// Why the plan last played has no sound, if it has none.
    pub fn no_sound(&self) -> Option<String> {
        self.no_sound.lock().ok().and_then(|w| w.clone())
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for Render {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Render").finish_non_exhaustive()
    }
}

/// The real-time side: copies queued chunks into the device buffer and records the
/// clock. Never allocates, frees or locks (see the module docs).
pub struct Render {
    rx: Receiver<Chunk>,
    spare: SyncSender<Chunk>,
    sh: Arc<Shared>,
    /// The chunk playing and the frames of it already played.
    cur: Option<(Chunk, usize)>,
    /// A chunk a newer play cut off, fading out under it: chunk, next frame, frames done.
    fading: Option<(Chunk, usize, usize)>,
    /// The epoch whose play ended, and the plan sample it ended on.
    ended_at: Option<(u64, f64)>,
    /// The last buffer ended mid-play (what follows in the queue continues what was heard).
    mid: bool,
    /// When the next buffer is due to be heard if the device runs on (host ns).
    next_ns: u64,
    /// The rate of the last chunk played.
    step: f64,
    silent: bool,
}

impl Render {
    /// A chunk buffer back to the producer. The spare queue holds more than can ever be
    /// out, so it is never full; were it, the buffer is leaked rather than freed here.
    fn give_back(&self, c: Chunk) {
        match self.spare.try_send(c) {
            Ok(()) => {}
            Err(TrySendError::Full(c)) => std::mem::forget(c),
            // the engine is closing: nothing is playing any more
            Err(TrySendError::Disconnected(c)) => std::mem::forget(c),
        }
    }

    /// Whether a later retarget mixed `c`'s samples again.
    fn superseded(&self, c: &Chunk) -> bool {
        let g = self.sh.generation.load(Ordering::Acquire);
        (c.generation + 1..=g).rev().take(RETARGETS).any(|r| {
            c.pos
                >= f64::from_bits(self.sh.cutoffs[(r as usize) % RETARGETS].load(Ordering::Relaxed))
        })
    }

    /// A chunk of an older play: the first one after a cut mid-play fades out (it holds
    /// what would have been heard next); the rest go back.
    fn retire(&mut self, c: Chunk, at: usize) {
        if self.mid && self.fading.is_none() && at * 2 < c.data.len() {
            self.mid = false;
            self.fading = Some((c, at, 0));
        } else {
            self.give_back(c);
        }
    }

    /// Fills one device buffer (interleaved stereo); `at_ns` is the host time its first
    /// frame is heard.
    pub fn fill(&mut self, out: &mut [f32], at_ns: u64) {
        let epoch = self.sh.epoch.load(Ordering::Acquire);
        let frames = out.len() / 2;
        // the unit was stopped in between (a pause): what was queued then is not a
        // continuation of anything heard now
        if self.next_ns.abs_diff(at_ns) > 20_000_000 {
            self.mid = false;
        }
        self.next_ns = at_ns + (frames as f64 / RATE * 1e9) as u64;
        if self.cur.as_ref().is_some_and(|(c, _)| c.epoch != epoch)
            && let Some((c, o)) = self.cur.take()
        {
            self.retire(c, o);
        }
        let mut first: Option<(f64, f64)> = None;
        let mut i = 0;
        let mut ended = false;
        while i < frames {
            if self
                .cur
                .as_ref()
                .is_none_or(|(c, o)| *o * 2 >= c.data.len())
            {
                if let Some((c, _)) = self.cur.take() {
                    let last = c.last;
                    if last {
                        ended = true;
                        self.ended_at = Some((epoch, c.pos + (c.data.len() / 2) as f64 * c.step));
                    }
                    self.give_back(c);
                    if last {
                        break;
                    }
                }
                // the next chunk of this play (older ones are retired)
                while let Ok(c) = self.rx.try_recv() {
                    self.sh
                        .queued
                        .fetch_sub(c.data.len() / 2, Ordering::Relaxed);
                    // mixed again in an edited plan (a retarget): the new one is behind it
                    if c.epoch == epoch && self.superseded(&c) {
                        let n = c.data.len() / 2;
                        let _ =
                            self.sh
                                .stale
                                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                                    Some(x.saturating_sub(n))
                                });
                        self.give_back(c);
                        continue;
                    }
                    if c.epoch == epoch {
                        self.cur = Some((c, 0));
                        break;
                    }
                    self.retire(c, 0);
                }
                if self.cur.is_none() {
                    break;
                }
            }
            let Some((c, o)) = self.cur.as_mut() else {
                break;
            };
            if first.is_none() {
                first = Some((c.pos + *o as f64 * c.step, c.step));
            }
            let n = (c.data.len() / 2 - *o).min(frames - i);
            out[i * 2..(i + n) * 2].copy_from_slice(&c.data[*o * 2..(*o + n) * 2]);
            *o += n;
            i += n;
        }
        out[i * 2..].fill(0.0);
        // what a newer play cut off fades out under it
        if let Some((c, o, done)) = self.fading.as_mut() {
            let n = (DECLICK - *done)
                .min(c.data.len() / 2 - (*o).min(c.data.len() / 2))
                .min(frames);
            for k in 0..n {
                let g = 1.0 - (*done + k + 1) as f32 / DECLICK as f32;
                out[k * 2] += c.data[(*o + k) * 2] * g;
                out[k * 2 + 1] += c.data[(*o + k) * 2 + 1] * g;
            }
            *o += n;
            *done += n;
            if (*done >= DECLICK || *o * 2 >= c.data.len())
                && let Some((c, _, _)) = self.fading.take()
            {
                self.give_back(c);
            }
        }
        self.mid = i == frames && first.is_some() && !ended;
        if self.silent {
            out.fill(0.0);
        }
        if i < frames && !ended && self.ended_at.is_none_or(|(g, _)| g != epoch) && first.is_some()
        {
            self.sh.underruns.fetch_add(1, Ordering::Relaxed);
        }
        if let Some((pos, step)) = first {
            self.step = step;
            self.sh.clock.write(&Clock {
                epoch,
                pos,
                at_ns,
                step,
                valid: true,
                ended: false,
            });
        } else if let Some((g, pos)) = self.ended_at
            && g == epoch
        {
            self.sh.clock.write(&Clock {
                epoch,
                pos,
                at_ns,
                step: self.step,
                valid: true,
                ended: true,
            });
        }
    }
}

/// The window of a grain `len` frames long at frame `k`: a raised cosine in and out
/// over [`GRAIN_FADE`] (shorter grains: half their length).
pub fn grain_window(k: usize, len: usize) -> f32 {
    let fade = GRAIN_FADE.min(len / 2).max(1);
    let edge = k.min(len.saturating_sub(1) - k.min(len.saturating_sub(1)));
    if edge >= fade {
        return 1.0;
    }
    let x = (edge as f64 + 0.5) / fade as f64;
    (0.5 - 0.5 * (std::f64::consts::PI * x).cos()) as f32
}

/// A buffer for the producer: a used one back from the callback, else a new one.
fn buffer(spare: &Receiver<Chunk>, frames: usize) -> Vec<f32> {
    let mut v = spare.try_recv().map(|c| c.data).unwrap_or_default();
    v.clear();
    v.resize(frames * 2, 0.0);
    v
}

/// Queues `chunk` for the device, staying responsive: gives up when the play it belongs
/// to is no longer current. False when the engine is gone.
fn send(tx: &SyncSender<Chunk>, sh: &Shared, chunk: Chunk) -> bool {
    let epoch = chunk.epoch;
    let frames = chunk.data.len() / 2;
    let mut c = Some(chunk);
    while let Some(ch) = c.take() {
        // counted before it can be taken, so the callback's count never goes below zero
        sh.queued.fetch_add(frames, Ordering::Relaxed);
        match tx.try_send(ch) {
            Ok(()) => {}
            Err(TrySendError::Full(ch)) => {
                sh.queued.fetch_sub(frames, Ordering::Relaxed);
                if sh.epoch.load(Ordering::SeqCst) != epoch {
                    break;
                }
                std::thread::sleep(Duration::from_millis(4));
                c = Some(ch);
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    true
}

/// The current play, or the current scrub.
enum Job {
    Play {
        epoch: u64,
        pos: f64,
        to: Option<u64>,
        rate: f64,
        looping: bool,
        gain: f32,
        /// The play's first chunk (it fades in).
        first: bool,
        /// Plan samples from `pos`'s numbering to the mixed plan's (edits during the play).
        off: i64,
    },
    Scrub {
        epoch: u64,
        /// The newest grain asked for and not made yet.
        next: Option<u64>,
    },
}

/// The producer: mixes chunks for the current play (or grains for the current scrub)
/// and queues them for the device.
fn producer(
    cmds: &Receiver<Cmd>,
    tx: &SyncSender<Chunk>,
    spare: &Receiver<Chunk>,
    sh: &Shared,
    make: &mut MakeMix,
) {
    let mut mixer: Option<(Arc<Plan>, Box<dyn Mix>)> = None;
    let mut job: Option<Job> = None;
    // the current play's chunks sent (plan sample, frames): what a retarget can mix again
    let mut sent: std::collections::VecDeque<(f64, usize)> = std::collections::VecDeque::new();
    let mut tmp: Vec<f32> = Vec::new();
    let mut ensure = |mixer: &mut Option<(Arc<Plan>, Box<dyn Mix>)>, plan: Arc<Plan>| {
        if mixer.as_ref().is_none_or(|(p, _)| !Arc::ptr_eq(p, &plan)) {
            *mixer = make(&plan).map(|m| (plan, m));
        }
    };
    loop {
        // commands (wait when idle)
        let idle = match &job {
            None | Some(Job::Scrub { next: None, .. }) => true,
            Some(_) => false,
        };
        let next = if idle {
            match cmds.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        } else {
            cmds.try_recv().ok()
        };
        if let Some(c) = next {
            match c {
                Cmd::Stop => job = None,
                Cmd::Play {
                    plan,
                    from,
                    to,
                    rate,
                    looping,
                    epoch,
                } => {
                    ensure(&mut mixer, plan);
                    sent.clear();
                    sh.stale.store(0, Ordering::Relaxed);
                    let gain = mixer.as_ref().map_or(1.0, |(_, m)| m.gain());
                    sh.muted_rate
                        .store(!(0.499..=2.001).contains(&rate), Ordering::Relaxed);
                    job = Some(Job::Play {
                        epoch,
                        pos: from,
                        to,
                        rate,
                        looping,
                        gain,
                        first: true,
                        off: 0,
                    });
                }
                Cmd::Retarget { plan, shift, epoch } => {
                    if let Some(Job::Play {
                        epoch: e,
                        pos,
                        to: None,
                        rate,
                        looping: false,
                        first,
                        off,
                        ..
                    }) = job.as_mut()
                        && *e == epoch
                    {
                        ensure(&mut mixer, plan);
                        // the queued chunks from REMIX_AHEAD past what is being filled now
                        // are mixed again in the edited plan, right behind themselves
                        let c = sh.clock.read();
                        let h = if c.valid && c.epoch == epoch {
                            c.pos
                        } else {
                            f64::NEG_INFINITY
                        };
                        if !*first
                            && let Some(i) = sent
                                .iter()
                                .position(|&(s, _)| s >= h + REMIX_AHEAD as f64 * *rate)
                        {
                            let cutoff = sent[i].0;
                            let stale: usize = sent.drain(i..).map(|(_, n)| n).sum();
                            let g = sh.generation.load(Ordering::Relaxed) + 1;
                            sh.cutoffs[(g as usize) % RETARGETS]
                                .store(cutoff.to_bits(), Ordering::Relaxed);
                            sh.stale.fetch_add(stale, Ordering::Relaxed);
                            sh.generation.store(g, Ordering::Release);
                            *pos = cutoff;
                        }
                        *off += shift;
                    }
                }
                Cmd::Grain { plan, at, epoch } => {
                    ensure(&mut mixer, plan);
                    sh.muted_rate.store(false, Ordering::Relaxed);
                    match job.as_mut() {
                        Some(Job::Scrub { epoch: e, next }) if *e == epoch => *next = Some(at),
                        _ => {
                            job = Some(Job::Scrub {
                                epoch,
                                next: Some(at),
                            });
                        }
                    }
                }
            }
            continue;
        }
        let epoch = match &job {
            Some(Job::Play { epoch, .. } | Job::Scrub { epoch, .. }) => *epoch,
            None => continue,
        };
        if sh.epoch.load(Ordering::SeqCst) != epoch {
            job = None;
            continue;
        }
        let Some((_, m)) = mixer.as_mut() else {
            job = None;
            continue;
        };
        match job.as_mut() {
            Some(Job::Scrub { next, .. }) => {
                // after the grain playing has half of itself left (latest step wins)
                if sh.queued.load(Ordering::Relaxed) > GRAIN / 2 {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let Some(at) = next.take() else { continue };
                let total = m.total();
                let len = (total.saturating_sub(at) as usize).min(GRAIN);
                if len == 0 {
                    continue;
                }
                tmp.clear();
                tmp.resize(len * 2, 0.0);
                m.mix(at, &mut tmp);
                let gain = m.gain();
                let mut k = 0;
                while k < len {
                    let n = (len - k).min(CHUNK);
                    let mut data = buffer(spare, n);
                    for j in 0..n {
                        let w = grain_window(k + j, len) * gain;
                        data[j * 2] = (tmp[(k + j) * 2] * w).clamp(-1.0, 1.0);
                        data[j * 2 + 1] = (tmp[(k + j) * 2 + 1] * w).clamp(-1.0, 1.0);
                    }
                    let chunk = Chunk {
                        epoch,
                        generation: sh.generation.load(Ordering::Relaxed),
                        pos: (at + k as u64) as f64,
                        step: 1.0,
                        data,
                        last: k + n >= len,
                    };
                    if !send(tx, sh, chunk) {
                        return;
                    }
                    k += n;
                }
            }
            Some(Job::Play {
                pos,
                to,
                rate,
                looping,
                gain,
                first,
                off,
                ..
            }) => {
                // DEPTH chunks of sound ahead (skipped ones do not count)
                let ahead = sh
                    .queued
                    .load(Ordering::Relaxed)
                    .saturating_sub(sh.stale.load(Ordering::Relaxed));
                if ahead >= DEPTH * CHUNK {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let (p, rt, g) = (*pos, *rate, *gain);
                // the mixed plan's numbering (`pos` keeps the play's first plan's)
                let o = *off as f64;
                let total = m.total();
                let end = to.unwrap_or(total).min(total) as f64 - o;
                // frames this chunk covers before the end
                let left = ((end - p) / rt).ceil().max(0.0) as usize;
                let n = left.min(CHUNK);
                let mut data = buffer(spare, CHUNK);
                let muted = !(0.499..=2.001).contains(&rt);
                if n > 0 && !muted {
                    let q = (p + o).max(0.0);
                    if (rt - 1.0).abs() < 1e-9 && q.fract() == 0.0 {
                        m.mix(q as u64, &mut data[..n * 2]);
                    } else {
                        // linear resampling of the plan's samples [a, b]
                        let a = q.floor() as u64;
                        let b = (q + n as f64 * rt).floor() as u64 + 2;
                        tmp.clear();
                        tmp.resize(((b - a) as usize) * 2, 0.0);
                        m.mix(a, &mut tmp);
                        for k in 0..n {
                            let x = q + k as f64 * rt - a as f64;
                            let i = x.floor() as usize;
                            let f = (x - x.floor()) as f32;
                            for ch in 0..2 {
                                let s0 = tmp.get(i * 2 + ch).copied().unwrap_or(0.0);
                                let s1 = tmp.get((i + 1) * 2 + ch).copied().unwrap_or(s0);
                                data[k * 2 + ch] = s0 + (s1 - s0) * f;
                            }
                        }
                    }
                    if g != 1.0 {
                        for s in &mut data[..n * 2] {
                            *s = (*s * g).clamp(-1.0, 1.0);
                        }
                    }
                    // a play may replace a sounding one: it fades in (de-click)
                    if *first {
                        for k in 0..n.min(DECLICK) {
                            let w = (k + 1) as f32 / DECLICK as f32;
                            data[k * 2] *= w;
                            data[k * 2 + 1] *= w;
                        }
                    }
                    // and it fades out over its last frames when it stops (a range, the
                    // cut's end) or wraps (a loop): it still ends on its exact sample
                    let tail = (end - p) / rt;
                    if tail < (CHUNK + DECLICK) as f64 {
                        for k in 0..n {
                            let left = tail - k as f64;
                            if left < DECLICK as f64 {
                                let w = (left / DECLICK as f64).clamp(0.0, 1.0) as f32;
                                data[k * 2] *= w;
                                data[k * 2 + 1] *= w;
                            }
                        }
                    }
                }
                *first = false;
                let reached = n < CHUNK || p + n as f64 * rt >= end;
                let wrap = reached && *looping && to.is_none();
                data.truncate(n * 2);
                let chunk = Chunk {
                    epoch,
                    generation: sh.generation.load(Ordering::Relaxed),
                    pos: p,
                    step: rt,
                    data,
                    last: reached && !wrap,
                };
                if !send(tx, sh, chunk) {
                    return;
                }
                sent.push_back((p, n));
                if sent.len() > 2 * DEPTH {
                    sent.pop_front();
                }
                if reached {
                    if wrap {
                        *pos = 0.0;
                        *first = true;
                    } else {
                        job = None;
                    }
                } else {
                    *pos = p + n as f64 * rt;
                }
            }
            None => {}
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
struct Mixer {
    plan: Arc<Plan>,
    ap: AudioPlan,
    /// The export's own sample source: every source the mix hears (the parts' voice, the
    /// sources under it, the overlays), a source this build cannot play read as silence.
    pcm: PlanPcm,
    gain: Arc<Mutex<Option<f32>>>,
}

#[cfg(any(target_os = "macos", windows))]
impl Mix for Mixer {
    fn total(&self) -> u64 {
        self.ap.total
    }

    fn mix(&mut self, from: u64, out: &mut [f32]) {
        if self.ap.mix_range(&mut self.pcm, from, out).is_err() {
            out.fill(0.0);
        }
    }

    fn gain(&self) -> f32 {
        self.gain.lock().ok().and_then(|g| *g).unwrap_or(1.0)
    }
}

#[cfg(any(target_os = "macos", windows))]
impl Mixer {
    fn new(plan: Arc<Plan>) -> Result<Mixer, String> {
        let m = Mixer::new_plain(plan)?;
        if let Ok(mut x) = m.gain.lock() {
            *x = None;
        }
        match m.ap.loudness {
            // the static gain the export applies, measured on the whole mix (background;
            // up to 10 minutes of cut, longer plays as recorded)
            LoudnessMode::Static {
                integrated_lufs, ..
            } if m.ap.total <= 48_000 * 600 => {
                let (p2, g2) = (Arc::clone(&m.plan), Arc::clone(&m.gain));
                let _ = std::thread::Builder::new()
                    .name("gmclip-audio-loudness".into())
                    .spawn(move || {
                        let g = Mixer::whole_gain(&p2, integrated_lufs).unwrap_or(1.0);
                        if let Ok(mut x) = g2.lock() {
                            *x = Some(g);
                        }
                    });
            }
            _ => {
                if let Ok(mut x) = m.gain.lock() {
                    *x = Some(1.0);
                }
            }
        }
        Ok(m)
    }

    fn whole_gain(plan: &Arc<Plan>, target: f64) -> Option<f32> {
        let mut m = Mixer::new_plain(Arc::clone(plan)).ok()?;
        let mix = m.ap.mix(&mut m.pcm).ok()?;
        let i = gmclip_media::audio::loudness::integrated(&mix, 2, 48_000);
        i.is_finite()
            .then(|| 10f64.powf((target - i) / 20.0) as f32)
    }

    fn new_plain(plan: Arc<Plan>) -> Result<Mixer, String> {
        let ap = AudioPlan::from_plan(&plan).map_err(|e| e.to_string())?;
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        let pcm = PlanPcm::open_lenient(&plan, &mut inputs);
        Ok(Mixer {
            plan,
            ap,
            pcm,
            gain: Arc::new(Mutex::new(Some(1.0))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone as an AAC file at `rate` (AudioToolbox through afconvert): `secs` long,
    /// `ch` channels.
    #[cfg(target_os = "macos")]
    pub(crate) fn tone_m4a(
        dir: &std::path::Path,
        name: &str,
        hz: f64,
        rate: u32,
        ch: u16,
        secs: f64,
    ) -> String {
        let n = (f64::from(rate) * secs) as usize;
        let pcm: Vec<f32> = (0..n)
            .flat_map(|i| {
                let v =
                    (0.3 * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin()) as f32;
                std::iter::repeat_n(v, usize::from(ch))
            })
            .collect();
        let wav = dir.join(format!("{name}.wav"));
        gmclip_media::audio::wav::write_f32(std::fs::File::create(&wav).unwrap(), rate, ch, &pcm)
            .unwrap();
        let m4a = dir.join(format!("{name}.m4a"));
        let st = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", &format!("aac@{rate}"), "-b", "128000"])
            .arg(&wav)
            .arg(&m4a)
            .status()
            .unwrap();
        assert!(st.success(), "afconvert");
        m4a.to_string_lossy().into_owned()
    }

    /// Decision 0051: what playback hears for a part with two sources (the voice, and a
    /// recording's system audio under it) and music over it is the export's mix, bit for
    /// bit: the playback mixer's chunks against the export's whole mix, both decoded
    /// from real AAC files.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_playback_mix_is_the_export_mix_for_two_sources() {
        let dir = std::env::temp_dir().join(format!("gmclip-pb-two-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let voice = tone_m4a(&dir, "voice", 220.0, 48_000, 1, 4.0);
        let screen = tone_m4a(&dir, "screen", 1000.0, 48_000, 2, 4.0);
        let v = serde_json::json!({ "schema": "gmclip.renderplan/1",
            "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 90,
                        "video": { "codec": "h264" }, "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
            "background": { "kind": "colour", "rgb": "#000000" },
            "sources": [ { "id": "voice", "path": voice }, { "id": "screen", "path": screen } ],
            "parts": [
                { "id": "p1", "out": [0, 40], "picture": { "source": "screen", "first_frame": 0 }, "layers": [],
                  "audio": { "source": "voice", "first_sample": 4800, "rate": 48000, "gain_db": 3,
                             "fade_in": { "samples": 144, "shape": "linear" },
                             "under": [ { "source": "screen", "first_sample": 4800, "gain_db": -12 } ] } },
                { "id": "p2", "out": [40, 90], "picture": { "source": "screen", "first_frame": 40 }, "layers": [],
                  "audio": { "source": "voice", "first_sample": 68_800, "rate": 48000,
                             "fade_out": { "samples": 144, "shape": "linear" },
                             "under": [ { "source": "screen", "first_sample": 68_800, "gain_db": -6 } ] } } ],
            "audio": { "overlays": [ { "id": "m", "source": "screen", "first_sample": 0, "out_samples": [24_000, 120_000],
                                       "gain_db": -18, "fade_in_samples": 4800, "fade_out_samples": 4800,
                                       "duck": { "depth_db": -10, "attack_samples": 4800, "release_samples": 9600 } } ],
                       "speech": [ [30_000, 60_000] ],
                       "loudness": { "mode": "as_recorded" } } });
        let plan = Arc::new(gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap());
        let ap = AudioPlan::from_plan(&plan).unwrap();
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        let export = ap
            .mix(&mut PlanPcm::open(&plan, &mut inputs).unwrap())
            .unwrap();
        let mut m = Mixer::new_plain(Arc::clone(&plan)).unwrap();
        let mut heard = vec![0.0f32; export.len()];
        for (k, c) in heard.chunks_mut(CHUNK * 2).enumerate() {
            m.mix((k * CHUNK) as u64, c);
        }
        let first = heard.iter().zip(&export).position(|(a, b)| a != b);
        assert_eq!(first, None, "playback differs from the export");
        // the screen's sound is in it: without it the mix is another
        let mut bare = (*plan).clone();
        for p in &mut bare.parts {
            p.audio.under = None;
        }
        bare.audio.overlays = None;
        let alone = AudioPlan::from_plan(&bare)
            .unwrap()
            .mix(&mut PlanPcm::open(&bare, &mut inputs).unwrap())
            .unwrap();
        let diff: f64 = alone
            .iter()
            .zip(&export)
            .map(|(a, b)| f64::from(a - b).powi(2))
            .sum::<f64>()
            / export.len() as f64;
        // −12 dB of a 0.3 tone is ≈ 0.075 peak: its power alone is ≈ 2.8e-3
        assert!(
            diff.sqrt() > 0.03,
            "rms of the sound under the voice {}",
            diff.sqrt()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision 0051: a 44.1 kHz AAC voice and a 22.05 kHz 24-bit WAV under it play at
    /// 48 kHz with their pitch kept, and playback's mix is the export's, bit for bit.
    #[cfg(target_os = "macos")]
    #[test]
    fn sound_at_any_rate_and_pcm_plays_as_the_export_mixes_it() {
        let dir = std::env::temp_dir().join(format!("gmclip-pb-rate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let voice = tone_m4a(&dir, "voice", 220.0, 44_100, 1, 3.0);
        // the WAV: 24-bit integer PCM through afconvert
        let _ = tone_m4a(&dir, "bed", 1000.0, 22_050, 2, 3.0);
        let wav = dir.join("bed24.wav");
        let st = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "WAVE", "-d", "LEI24@22050"])
            .arg(dir.join("bed.wav"))
            .arg(&wav)
            .status()
            .unwrap();
        assert!(st.success());
        let v = serde_json::json!({ "schema": "gmclip.renderplan/1",
            "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 60,
                        "video": { "codec": "h264" }, "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
            "background": { "kind": "colour", "rgb": "#000000" },
            "sources": [ { "id": "voice", "path": voice, "expect": { "audio": { "codec": "aac", "rate": 44100, "channels": 1 } } },
                         { "id": "bed", "path": wav.to_string_lossy(), "expect": { "audio": { "codec": "pcm", "rate": 22050, "channels": 2 } } } ],
            "parts": [ { "id": "p1", "out": [0, 60], "picture": { "source": "voice", "first_frame": 0 }, "layers": [],
                         "audio": { "source": "voice", "first_sample": 24_000, "rate": 48000,
                                    "under": [ { "source": "bed", "first_sample": 24_000, "gain_db": -6 } ] } } ],
            "audio": { "loudness": { "mode": "as_recorded" } } });
        let plan = Arc::new(gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap());
        let mut v0 = v.clone();
        v0["schema"] = serde_json::json!("gmclip.renderplan/0");
        v0["parts"][0]["audio"]
            .as_object_mut()
            .unwrap()
            .remove("under");
        let e = gmclip_plan::parse(&serde_json::to_vec(&v0).unwrap()).unwrap_err();
        assert!(e.path().ends_with("/expect/audio/rate"), "{e}");
        let ap = AudioPlan::from_plan(&plan).unwrap();
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        let export = ap
            .mix(&mut PlanPcm::open(&plan, &mut inputs).unwrap())
            .unwrap();
        let mut m = Mixer::new_plain(Arc::clone(&plan)).unwrap();
        let mut heard = vec![0.0f32; export.len()];
        for (k, c) in heard.chunks_mut(CHUNK * 2).enumerate() {
            m.mix((k * CHUNK) as u64, c);
        }
        let first = heard.iter().zip(&export).position(|(a, b)| a != b);
        assert_eq!(first, None, "playback differs from the export");
        // each source alone: its tone's pitch at 48 kHz (sign changes a second, 2 × hz)
        let mut alone = |keep_voice: bool| {
            let mut p = (*plan).clone();
            if keep_voice {
                p.parts[0].audio.under = None;
            } else {
                p.parts[0].audio.gain_db = Some(-30.0);
                p.parts[0].audio.under.as_mut().unwrap()[0].gain_db = 12.0;
            }
            let mix = AudioPlan::from_plan(&p)
                .unwrap()
                .mix(&mut PlanPcm::open(&p, &mut inputs).unwrap())
                .unwrap();
            let left: Vec<f32> = mix.iter().step_by(2).copied().collect();
            let s = &left[4800..52_800];
            let crossings = s
                .windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count();
            let rms =
                (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
            (crossings, rms)
        };
        let (c, rms) = alone(true);
        assert!((438..=442).contains(&c), "220 Hz voice: {c} sign changes");
        assert!((0.19..0.23).contains(&rms), "voice rms {rms}");
        let (c, _) = alone(false);
        assert!((1990..=2010).contains(&c), "1 kHz bed: {c} sign changes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_grain_starts_and_ends_at_silence() {
        let len = GRAIN;
        assert!(grain_window(0, len) < 1e-4);
        assert!(grain_window(len - 1, len) < 1e-4);
        assert_eq!(grain_window(len / 2, len), 1.0);
        // symmetric, and never steeper than the raised cosine allows
        let max_step = (0..len - 1)
            .map(|k| (grain_window(k + 1, len) - grain_window(k, len)).abs())
            .fold(0f32, f32::max);
        assert!(max_step <= std::f32::consts::PI / (2 * GRAIN_FADE) as f32 + 1e-4);
        for k in 0..len {
            assert!((grain_window(k, len) - grain_window(len - 1 - k, len)).abs() < 1e-6);
        }
        // a short grain (near the cut's end) fades over half of itself
        assert!(grain_window(0, 10) < 0.1);
        assert!(grain_window(9, 10) < 0.1);
        assert!(grain_window(4, 10) > 0.9);
    }

    #[test]
    fn the_clock_reads_whole() {
        let c = ClockCell::default();
        let w = Clock {
            epoch: 7,
            pos: 1234.5,
            at_ns: 99,
            step: 2.0,
            valid: true,
            ended: false,
        };
        c.write(&w);
        assert_eq!(c.read(), w);
        let e = Clock { ended: true, ..w };
        c.write(&e);
        assert_eq!(c.read(), e);
    }
}
