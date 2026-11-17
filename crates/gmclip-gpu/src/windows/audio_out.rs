//! Playback output on Windows (SPEC §6.3, M5; the twin of `apple::audio_out`): the
//! default render endpoint through shared-mode, event-driven WASAPI, 48 kHz interleaved
//! stereo f32 (the audio engine converts to the device's own mix format,
//! `AUTOCONVERTPCM`). A dedicated thread owns the COM objects; on each buffer event it
//! hands the free part of WASAPI's own buffer to the Rust closure together with the
//! host time (QPC nanoseconds) at which its first sample is heard. That is the clock
//! video follows, and it is measured, not estimated: `IAudioClock::GetPosition` says
//! which sample the device is playing at which QPC instant, so the buffer's first sample
//! (the `written`-th) is heard at that instant plus `(written − played) / rate`. Drivers
//! often report a stream latency of 0, which the position already includes. Without a
//! position the estimate is now + queued frames + the reported latency.
//!
//! The closure must not block or allocate (it runs on the audio thread, raised to the
//! "Pro Audio" MMCSS class); the caller's side feeds it through a channel. The thread
//! itself takes no lock and allocates nothing per buffer: start, stop and quit are
//! atomics plus the buffer event.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioClock, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{
    SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForSingleObject,
};
use windows::core::w;

use crate::GpuError;

/// The output rate the closure fills.
pub const RATE: u32 = 48_000;

/// Fills one buffer: interleaved stereo f32, and the host time (ns) its first sample is
/// heard.
pub type RenderFn = Box<dyn FnMut(&mut [f32], u64) + Send>;

/// WASAPI's buffer: 20 ms (in 100 ns units). The engine fires the event once per period.
const BUFFER_HNS: i64 = 200_000;

fn os(step: &'static str, e: &windows::core::Error) -> GpuError {
    GpuError::Os {
        step,
        status: i64::from(e.code().0),
    }
}

/// Now, in host-time nanoseconds (QueryPerformanceCounter; the clock the callback's
/// times are on).
pub fn host_now_ns() -> u64 {
    let (mut c, mut f) = (0i64, 0i64);
    // SAFETY: plain clock reads into valid out pointers; both cannot fail on XP and later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 {
        return 0;
    }
    (i128::from(c) * 1_000_000_000 / i128::from(f)) as u64
}

/// COM on this thread for the duration of one call (multithreaded apartment; a thread
/// already in another apartment keeps it, which is fine for these objects).
struct Com(bool);

impl Com {
    fn init() -> Self {
        // SAFETY: plain call; balanced by CoUninitialize in Drop when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Com(hr.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The system's default render endpoint right now, as a hash of its endpoint id (0 when
/// there is none). Playback compares it to the one it opened with and reopens on a
/// switch (headphones, a USB interface), so the latency it adds is the new device's.
pub fn default_output_device() -> u32 {
    let _com = Com::init();
    // SAFETY: COM is initialised on this thread; the enumerator and device are released
    // by their wrappers; the id string is freed with CoTaskMemFree as documented.
    unsafe {
        let Ok(en) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return 0;
        };
        let Ok(dev) = en.GetDefaultAudioEndpoint(eRender, eConsole) else {
            return 0;
        };
        let Ok(id) = dev.GetId() else {
            return 0;
        };
        // FNV-1a over the UTF-16 id; never 0 for a real device
        let mut h: u32 = 0x811c_9dc5;
        for u in id.as_wide() {
            for b in u.to_le_bytes() {
                h ^= u32::from(b);
                h = h.wrapping_mul(0x0100_0193);
            }
        }
        CoTaskMemFree(Some(id.0.cast()));
        h.max(1)
    }
}

/// What the audio thread and the handle share.
struct Shared {
    run: AtomicBool,
    quit: AtomicBool,
    latency_ns: AtomicU64,
    /// The buffer event, signalled by WASAPI each period and by `drop` to wake the thread.
    event: HANDLE,
}

// SAFETY: an event HANDLE may be used from any thread.
unsafe impl Send for Shared {}
// SAFETY: as above; the rest are atomics.
unsafe impl Sync for Shared {}

/// One open output stream.
pub struct AudioOut {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    running: bool,
    latency_ns: u64,
}

impl std::fmt::Debug for AudioOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOut")
            .field("running", &self.running)
            .field("latency_ns", &self.latency_ns)
            .finish_non_exhaustive()
    }
}

/// 48 kHz stereo f32, as the closure fills it.
fn format() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// The stream, opened on the audio thread (COM objects stay on the thread that made them).
struct Stream {
    client: IAudioClient,
    render: IAudioRenderClient,
    /// The device position (`None` when the endpoint does not give one).
    clock: Option<(IAudioClock, u64)>,
    frames: u32,
}

impl Stream {
    /// The host time (ns) at which the stream's `written`-th frame is heard, from the
    /// device position; `None` when there is no position.
    fn heard_at(&self, written: u64) -> Option<u64> {
        let (clock, freq) = self.clock.as_ref()?;
        let (mut pos, mut qpc) = (0u64, 0u64);
        // SAFETY: a live clock of a started stream; valid out pointers.
        unsafe { clock.GetPosition(&mut pos, Some(&mut qpc)) }.ok()?;
        // `qpc` is the QPC instant of `pos`, in 100 ns units (host_now_ns's clock)
        let played_ns = i128::from(pos) * 1_000_000_000 / i128::from(*freq);
        let written_ns = i128::from(written) * 1_000_000_000 / i128::from(RATE);
        let at = i128::from(qpc) * 100 + written_ns - played_ns;
        u64::try_from(at).ok()
    }
}

fn open_stream(event: HANDLE) -> Result<(Stream, u64), GpuError> {
    let fmt = format();
    // SAFETY: COM is initialised on this (the audio) thread; every pointer passed is to a
    // live local; the interfaces are released by their wrappers.
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| os("audio_out_enumerator", &e))?;
        let dev = en
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|_| GpuError::Unsupported("no default audio output".into()))?;
        let client: IAudioClient = dev
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| os("audio_out_activate", &e))?;
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_HNS,
                0,
                (&raw const fmt).cast(),
                None,
            )
            .map_err(|e| os("audio_out_init", &e))?;
        client
            .SetEventHandle(event)
            .map_err(|e| os("audio_out_event", &e))?;
        let frames = client
            .GetBufferSize()
            .map_err(|e| os("audio_out_buffer", &e))?;
        let render: IAudioRenderClient = client
            .GetService()
            .map_err(|e| os("audio_out_render", &e))?;
        let latency_ns = latency_of(&client);
        let clock = client
            .GetService::<IAudioClock>()
            .ok()
            .and_then(|c| c.GetFrequency().ok().filter(|f| *f > 0).map(|f| (c, f)));
        Ok((
            Stream {
                client,
                render,
                clock,
                frames,
            },
            latency_ns,
        ))
    }
}

/// The stream's presentation latency (the engine's and the device's), in ns.
fn latency_of(client: &IAudioClient) -> u64 {
    // SAFETY: a live, initialised client.
    let hns = unsafe { client.GetStreamLatency() }.unwrap_or(0);
    u64::try_from(hns).unwrap_or(0) * 100
}

/// The audio thread: waits on the buffer event and fills what WASAPI has free.
fn pump(stream: Stream, shared: &Shared, mut render: RenderFn) {
    // "Pro Audio" scheduling for the buffer loop (MMCSS); best effort
    let mut task = 0u32;
    // SAFETY: a static task name and a valid out pointer.
    let mm = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task) }.ok();
    let mut started = false;
    // frames handed to WASAPI since the stream (re)started: the device position counts
    // from the same start
    let mut written: u64 = 0;
    while !shared.quit.load(Ordering::Acquire) {
        // SAFETY: a valid event handle owned by `shared`, which outlives this loop.
        let woke = unsafe { WaitForSingleObject(shared.event, 100) };
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        let want = shared.run.load(Ordering::Acquire);
        if want != started {
            // SAFETY: a live, initialised client; Start/Stop/Reset are called on its
            // thread. A stop resets the stream: the next start counts from 0 again.
            let r = unsafe {
                if want {
                    stream.client.Start()
                } else {
                    stream.client.Stop().and_then(|()| stream.client.Reset())
                }
            };
            written = 0;
            if r.is_err() {
                // the device went away: playback sees the default device change and
                // reopens; until then this stream stays silent
                started = false;
                continue;
            }
            started = want;
            if !started {
                continue;
            }
        }
        if !started || woke != WAIT_OBJECT_0 {
            continue;
        }
        // SAFETY: a live, started client.
        let Ok(queued) = (unsafe { stream.client.GetCurrentPadding() }) else {
            continue;
        };
        let free = stream.frames.saturating_sub(queued);
        if free == 0 {
            continue;
        }
        // SAFETY: GetBuffer returns `free` frames of the stream format (stereo f32, 8 bytes
        // per frame) valid until ReleaseBuffer; nothing else touches them meanwhile.
        let Ok(p) = (unsafe { stream.render.GetBuffer(free) }) else {
            continue;
        };
        if p.is_null() {
            continue;
        }
        // SAFETY: as above; `free * 2` f32s, suitably aligned (the engine's buffers are).
        let out = unsafe { std::slice::from_raw_parts_mut(p.cast::<f32>(), free as usize * 2) };
        let now = host_now_ns();
        let at = stream.heard_at(written).map_or_else(
            || {
                let ahead_ns = u64::from(queued) * 1_000_000_000 / u64::from(RATE);
                now + ahead_ns + shared.latency_ns.load(Ordering::Relaxed)
            },
            // never in the past: a buffer is heard after it is written
            |at| at.max(now),
        );
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(out, at))).is_err() {
            out.fill(0.0);
        }
        // SAFETY: releases exactly the frames got above.
        let _ = unsafe { stream.render.ReleaseBuffer(free, 0) };
        written += u64::from(free);
    }
    if started {
        // SAFETY: a live client on its own thread.
        let _ = unsafe { stream.client.Stop() };
    }
    if let Some(h) = mm {
        // SAFETY: the handle AvSetMmThreadCharacteristicsW returned on this thread.
        let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
    }
}

impl AudioOut {
    /// Opens the default output device (stopped).
    pub fn open(render: RenderFn) -> Result<Self, GpuError> {
        // SAFETY: an unnamed auto-reset event, owned by `Shared` and closed in Drop.
        let event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|e| os("audio_out_event_new", &e))?;
        let shared = Arc::new(Shared {
            run: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            latency_ns: AtomicU64::new(0),
            event,
        });
        let (tx, rx) = mpsc::sync_channel::<Result<u64, GpuError>>(1);
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("gmclip-audio-out".into())
            .spawn(move || {
                let _com = Com::init();
                match open_stream(s.event) {
                    Ok((stream, latency)) => {
                        s.latency_ns.store(latency, Ordering::Relaxed);
                        let _ = tx.send(Ok(latency));
                        pump(stream, &s, render);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| GpuError::Unsupported(format!("audio thread: {e}")))?;
        let mut me = AudioOut {
            shared,
            thread: Some(thread),
            running: false,
            latency_ns: 0,
        };
        match rx.recv() {
            Ok(Ok(latency)) => {
                me.latency_ns = latency;
                Ok(me)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(GpuError::Unsupported("the audio thread ended".into())),
        }
    }

    /// The stream's presentation latency again (WASAPI reports one fixed value per
    /// stream; a new device means a new stream, which playback opens on a switch).
    pub fn refresh_latency(&mut self) -> u64 {
        self.latency_ns = self.shared.latency_ns.load(Ordering::Relaxed);
        self.latency_ns
    }

    pub fn start(&mut self) -> Result<(), GpuError> {
        if !self.running {
            self.shared.run.store(true, Ordering::Release);
            // SAFETY: a valid event handle; wakes the thread to start the client now.
            let _ = unsafe { SetEvent(self.shared.event) };
            self.running = true;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.running {
            self.shared.run.store(false, Ordering::Release);
            // SAFETY: as in `start`.
            let _ = unsafe { SetEvent(self.shared.event) };
            self.running = false;
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// The device's presentation latency (already added to the callback's times).
    pub fn latency_ns(&self) -> u64 {
        self.latency_ns
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        // SAFETY: a valid event handle; wakes the thread so it sees `quit`.
        let _ = unsafe { SetEvent(self.shared.event) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // SAFETY: the thread has ended; nothing else holds the event any more.
        let _ = unsafe { CloseHandle(self.shared.event) };
    }
}
