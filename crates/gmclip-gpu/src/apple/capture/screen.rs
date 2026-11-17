//! One screen file: a ScreenCaptureKit stream of a display or a window at native pixels,
//! **with the cursor left out** (`showsCursor = NO`; gmclip redraws it from the input
//! log, decision 0045), system audio as the file's sound, written by [`Writer`].
//!
//! The stream starts warm (during the countdown) and drops frames until [`go`] gives it
//! the session's `t0`; every complete frame written after that is reported to
//! `on_frame` with its host time, which is when the recorder samples the cursor.
//!
//! [`go`]: ScreenRecorder::go

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::sources::{Display, Window, display_of, names_and_main, sc_object, window_of};
use super::writer::{VideoSettings, Writer, WriterStats};
use super::{
    CMSampleBufferDataIsReady, CMSampleBufferGetImageBuffer,
    CMSampleBufferGetSampleAttachmentsArray, CaptureError, Registry, class, cmtime, error_text,
    host_to_ns, ns_array_empty, ns_array_items, ns_to_host, sample_host_time, serial_queue,
};

unsafe extern "C" {
    static SCStreamFrameInfoStatus: *const AnyObject;
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// `SCStreamOutputType`.
const OUTPUT_SCREEN: isize = 0;
const OUTPUT_AUDIO: isize = 1;
/// `SCFrameStatusComplete`.
const FRAME_COMPLETE: isize = 0;
/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` ('420v'), what the encoder takes.
const PIXEL_420V: u32 = 0x3432_3076;

/// What to record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScreenTarget {
    Display(u32),
    Window(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenConfig {
    pub target: ScreenTarget,
    pub path: PathBuf,
    /// 30 or 60.
    pub fps: u32,
    /// System audio as the file's sound track.
    pub audio: bool,
    /// HEVC (else H.264). gmclip's decoder reads H.264 and VP9 today (HEVC is M2), so
    /// recordings are H.264 unless asked.
    pub hevc: bool,
    /// At most this many pixels a picture (scaled down keeping the aspect), to leave
    /// the hardware encoder room for a live stream (decision 0055).
    pub max_pixels: Option<u32>,
}

/// Called with each written frame's host time.
pub type OnFrame = Box<dyn Fn(u64) + Send + Sync>;

/// A retained sample buffer (the last complete frame).
struct Held(*mut c_void);

// SAFETY: a CMSampleBuffer is immutable once made and may be retained, read and
// released from any thread.
unsafe impl Send for Held {}

impl Drop for Held {
    fn drop(&mut self) {
        // SAFETY: retained in `Held::new`.
        unsafe { CFRelease(self.0) };
    }
}

impl Held {
    fn new(sbuf: *mut c_void) -> Self {
        // SAFETY: a live sample buffer; released on drop.
        unsafe { CFRetain(sbuf) };
        Self(sbuf)
    }
}

/// The file being written and the constant-rate grid it is written on: ScreenCapture
/// Kit sends a picture only when the screen changed (an idle frame has none, and fewer
/// arrive than the rate asked for), so every slot `t0 + k/fps` up to each callback is
/// written with the latest picture, re-stamped. The file is constant frame rate, as
/// the editor's frame-exact pipeline expects, with one cursor sample per frame.
struct State {
    writer: Option<Writer>,
    last: Option<Held>,
    next: u64,
}

struct Ctx {
    writer: Mutex<State>,
    fps: u32,
    on_frame: OnFrame,
    stopped: Mutex<Option<String>>,
    live: AtomicBool,
}

fn ctx_registry() -> &'static Registry<Ctx> {
    static R: Registry<Ctx> = Registry::new();
    &R
}

/// The frame's `SCFrameStatus` (complete, idle, blank, …), from its attachments.
fn frame_status(sbuf: *mut c_void) -> Option<isize> {
    // SAFETY: a live sample buffer; returns its attachments array (not retained) or NULL.
    let arr = unsafe { CMSampleBufferGetSampleAttachmentsArray(sbuf, 0) };
    if arr.is_null() {
        return None;
    }
    // SAFETY: a CFArray of CFDictionary, toll-free NSArray of NSDictionary.
    let items = ns_array_items(unsafe { &*arr });
    let first = items.first()?;
    // SAFETY: a ScreenCaptureKit constant used as the dictionary key.
    let key = unsafe { &*SCStreamFrameInfoStatus };
    // SAFETY: -objectForKey: on an NSDictionary; an NSNumber or nil.
    let n: Option<Retained<AnyObject>> = unsafe { msg_send![&**first, objectForKey: key] };
    // SAFETY: -integerValue on an NSNumber.
    Some(unsafe { msg_send![&*n?, integerValue] })
}

extern "C-unwind" fn did_output(
    this: &AnyObject,
    _: Sel,
    _stream: *mut AnyObject,
    sbuf: *mut c_void,
    kind: isize,
) {
    let Some(ctx) = ctx_registry().get(this as *const AnyObject as usize) else {
        return;
    };
    if sbuf.is_null() || !ctx.live.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: a live sample buffer handed to this callback.
    if unsafe { CMSampleBufferDataIsReady(sbuf) } == 0 {
        return;
    }
    match kind {
        OUTPUT_SCREEN => {
            // SAFETY: as above; a complete frame carries a pixel buffer.
            let picture = frame_status(sbuf) == Some(FRAME_COMPLETE)
                && !unsafe { CMSampleBufferGetImageBuffer(sbuf) }.is_null();
            let t = sample_host_time(sbuf);
            let Ok(mut g) = ctx.writer.lock() else { return };
            let st = &mut *g;
            if picture {
                st.last = Some(Held::new(sbuf));
            }
            let (Some(w), Some(last)) = (st.writer.as_mut(), st.last.as_ref()) else {
                return;
            };
            if !w.started() || t < w.t0() {
                return;
            }
            let fps = u128::from(ctx.fps);
            let slot =
                ((u128::from(host_to_ns(t - w.t0())) * fps + 500_000_000) / 1_000_000_000) as u64;
            // After a stall longer than two seconds, skip ahead rather than flood.
            if slot > st.next + 2 * u64::from(ctx.fps) {
                st.next = slot;
            }
            let dur = ns_to_host(1_000_000_000 / u64::from(ctx.fps));
            while st.next <= slot {
                let pts = w.t0() + ns_to_host((u128::from(st.next) * 1_000_000_000 / fps) as u64);
                if w.append_retimed(last.0, pts, dur) {
                    (ctx.on_frame)(pts);
                }
                st.next += 1;
            }
        }
        OUTPUT_AUDIO => {
            if let Ok(mut g) = ctx.writer.lock()
                && let Some(w) = g.writer.as_mut()
            {
                w.append(false, sbuf);
            }
        }
        _ => {}
    }
}

extern "C-unwind" fn did_stop(
    this: &AnyObject,
    _: Sel,
    _stream: *mut AnyObject,
    err: *mut AnyObject,
) {
    if let Some(ctx) = ctx_registry().get(this as *const AnyObject as usize)
        && let Ok(mut g) = ctx.stopped.lock()
    {
        *g = Some(error_text(err));
    }
}

fn output_class() -> Result<&'static AnyClass, CaptureError> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            let superclass = AnyClass::get(c"NSObject")?;
            let mut b = ClassBuilder::new(c"GmclipScreenOutput", superclass)?;
            for p in [c"SCStreamOutput", c"SCStreamDelegate"] {
                if let Some(p) = AnyProtocol::get(p) {
                    b.add_protocol(p);
                }
            }
            // SAFETY: the signatures match -stream:didOutputSampleBuffer:ofType: and
            // -stream:didStopWithError: (id, SEL, SCStream *, CMSampleBufferRef /
            // NSError *, SCStreamOutputType).
            unsafe {
                b.add_method(
                    sel!(stream:didOutputSampleBuffer:ofType:),
                    did_output as extern "C-unwind" fn(_, _, _, _, _),
                );
                b.add_method(
                    sel!(stream:didStopWithError:),
                    did_stop as extern "C-unwind" fn(_, _, _, _),
                );
            }
            Some(b.register())
        })
        .ok_or(CaptureError::Missing("SCStreamOutput"))
}

/// The file's picture for a capture of `px`: even sides, and for H.264 within what the
/// hardware encoder and the editor's decoder take (4096 a side, 4096×2304 pixels), scaled
/// down keeping the aspect (a 5K display records at 4096×2304). The cursor log maps to
/// the file's pixels, so a scaled picture still lines up. `max_pixels` caps the area
/// further.
fn fit((w, h): (u32, u32), hevc: bool, max_pixels: Option<u32>) -> (u32, u32) {
    let (w, h) = (f64::from(w.max(2)), f64::from(h.max(2)));
    let k = if hevc {
        (8192.0 / w.max(h)).min(1.0)
    } else {
        (4096.0 / w.max(h))
            .min((4096.0 * 2304.0 / (w * h)).sqrt())
            .min(1.0)
    };
    let k = max_pixels.map_or(k, |m| k.min((f64::from(m) / (w * h)).sqrt()));
    let even = |v: f64| ((v * k).floor() as u32).max(2) & !1;
    (even(w), even(h))
}

/// What a screen file shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenInfo {
    /// The display recorded (for a window: the display it started on).
    pub display: Display,
    pub window: Option<Window>,
    /// The file's picture.
    pub width: u32,
    pub height: u32,
}

/// A running screen capture.
pub struct ScreenRecorder {
    stream: Retained<AnyObject>,
    output: Retained<AnyObject>,
    _queue: Retained<AnyObject>,
    ctx: Arc<Ctx>,
    info: ScreenInfo,
    fps: u32,
}

impl std::fmt::Debug for ScreenRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenRecorder")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

// SAFETY: the stream and its output object are only messaged from the owning thread
// (start, go, finish); the capture callbacks reach the shared state through `Ctx`'s
// mutexes.
unsafe impl Send for ScreenRecorder {}

/// The display a window's centre is on (or the main one).
fn display_for_window(w: &Window) -> Result<Display, CaptureError> {
    let content = super::sources::shareable()?;
    let (names, main) = names_and_main();
    // SAFETY: SCShareableContent's -displays is an NSArray of SCDisplay.
    let ds: Option<Retained<AnyObject>> = unsafe { msg_send![&*content, displays] };
    let ds = ds.ok_or(CaptureError::Missing("SCShareableContent displays"))?;
    let all: Vec<Display> = ns_array_items(&ds)
        .iter()
        .map(|d| display_of(d, &names, main))
        .collect();
    let (cx, cy) = (w.x + w.w / 2.0, w.y + w.h / 2.0);
    all.iter()
        .find(|d| cx >= d.x && cx < d.x + d.w && cy >= d.y && cy < d.y + d.h)
        .or_else(|| all.iter().find(|d| d.main))
        .or(all.first())
        .cloned()
        .ok_or(CaptureError::NoSuch("display", "any".into()))
}

impl ScreenRecorder {
    /// Starts the stream (frames are dropped until [`Self::go`]).
    pub fn start(cfg: &ScreenConfig, on_frame: OnFrame) -> Result<Self, CaptureError> {
        if !super::permissions().screen.granted() {
            return Err(CaptureError::Permission("Screen Recording"));
        }
        let (filter, info) = match cfg.target {
            ScreenTarget::Display(id) => {
                let d = sc_object(false, id)?;
                let (names, main) = names_and_main();
                let display = display_of(&d, &names, main);
                let (w, h) = fit(display.pixels(), cfg.hevc, cfg.max_pixels);
                let arr = ns_array_empty()?;
                let c = class("SCContentFilter")?;
                // SAFETY: +alloc then -initWithDisplay:excludingWindows: with a live
                // SCDisplay and an empty array.
                let f: Option<Retained<AnyObject>> = unsafe {
                    let a: Allocated<AnyObject> = msg_send![c, alloc];
                    msg_send![a, initWithDisplay: &*d, excludingWindows: &*arr]
                };
                let f = f.ok_or(CaptureError::Missing("SCContentFilter"))?;
                (
                    f,
                    ScreenInfo {
                        display,
                        window: None,
                        width: w,
                        height: h,
                    },
                )
            }
            ScreenTarget::Window(id) => {
                let wobj = sc_object(true, id)?;
                let window = window_of(&wobj);
                let display = display_for_window(&window)?;
                let (w, h) = fit(
                    (
                        (window.w * display.scale).round() as u32,
                        (window.h * display.scale).round() as u32,
                    ),
                    cfg.hevc,
                    cfg.max_pixels,
                );
                let c = class("SCContentFilter")?;
                // SAFETY: +alloc then -initWithDesktopIndependentWindow: with a live
                // SCWindow.
                let f: Option<Retained<AnyObject>> = unsafe {
                    let a: Allocated<AnyObject> = msg_send![c, alloc];
                    msg_send![a, initWithDesktopIndependentWindow: &*wobj]
                };
                let f = f.ok_or(CaptureError::Missing("SCContentFilter"))?;
                (
                    f,
                    ScreenInfo {
                        display,
                        window: Some(window),
                        width: w,
                        height: h,
                    },
                )
            }
        };
        let fps = cfg.fps.clamp(1, 120);
        let conf_class = class("SCStreamConfiguration")?;
        // SAFETY: +new, then plain property setters on the configuration.
        let conf: Retained<AnyObject> = unsafe {
            let conf: Option<Retained<AnyObject>> = msg_send![conf_class, new];
            let conf = conf.ok_or(CaptureError::Missing("SCStreamConfiguration"))?;
            let () = msg_send![&*conf, setWidth: info.width as usize];
            let () = msg_send![&*conf, setHeight: info.height as usize];
            let () = msg_send![&*conf, setMinimumFrameInterval: cmtime(1, fps as i32)];
            let () = msg_send![&*conf, setShowsCursor: Bool::NO];
            let () = msg_send![&*conf, setQueueDepth: 8isize];
            let () = msg_send![&*conf, setPixelFormat: PIXEL_420V];
            if cfg.audio {
                let () = msg_send![&*conf, setCapturesAudio: Bool::YES];
                let () = msg_send![&*conf, setSampleRate: 48_000isize];
                let () = msg_send![&*conf, setChannelCount: 2isize];
                let () = msg_send![&*conf, setExcludesCurrentProcessAudio: Bool::YES];
            }
            conf
        };

        let mut writer = Writer::new(&cfg.path)?;
        writer.add_video(VideoSettings {
            w: info.width,
            h: info.height,
            fps,
            hevc: cfg.hevc,
        })?;
        if cfg.audio {
            writer.add_audio(48_000.0, 2)?;
        }

        let oc = output_class()?;
        // SAFETY: +new on the NSObject subclass registered above.
        let output: Option<Retained<AnyObject>> = unsafe { msg_send![oc, new] };
        let output = output.ok_or(CaptureError::Missing("GmclipScreenOutput"))?;
        let ctx = Arc::new(Ctx {
            writer: Mutex::new(State {
                writer: Some(writer),
                last: None,
                next: 0,
            }),
            fps,
            on_frame,
            stopped: Mutex::new(None),
            live: AtomicBool::new(true),
        });
        ctx_registry().insert(&*output as *const AnyObject as usize, ctx.clone());

        let sc = class("SCStream")?;
        // SAFETY: +alloc then -initWithFilter:configuration:delegate: with live objects;
        // the stream keeps the delegate weakly, and `output` outlives it (held here).
        let stream: Option<Retained<AnyObject>> = unsafe {
            let a: Allocated<AnyObject> = msg_send![sc, alloc];
            msg_send![a, initWithFilter: &*filter, configuration: &*conf, delegate: &*output]
        };
        let stream = stream.ok_or(CaptureError::Missing("SCStream"))?;
        let queue = serial_queue(c"net.gmclip.capture.screen")?;
        let kinds: &[isize] = if cfg.audio {
            &[OUTPUT_SCREEN, OUTPUT_AUDIO]
        } else {
            &[OUTPUT_SCREEN]
        };
        for &kind in kinds {
            let mut err: *mut AnyObject = std::ptr::null_mut();
            // SAFETY: a live output object, an SCStreamOutputType, a serial queue and
            // an NSError out-pointer.
            let ok: Bool = unsafe {
                msg_send![&*stream, addStreamOutput: &*output, type: kind, sampleHandlerQueue: &*queue, error: &mut err]
            };
            if !ok.as_bool() {
                ctx_registry().remove(&*output as *const AnyObject as usize);
                return Err(CaptureError::Os {
                    step: "SCStream addStreamOutput",
                    message: error_text(err),
                });
            }
        }
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let block = RcBlock::new(move |err: *mut AnyObject| {
            let _ = tx.send((!err.is_null()).then(|| error_text(err)));
        });
        // SAFETY: a block the stream copies and calls once capture has started.
        let () = unsafe { msg_send![&*stream, startCaptureWithCompletionHandler: &*block] };
        if let Some(message) = super::wait(&rx, "the screen capture to start", 15)? {
            ctx_registry().remove(&*output as *const AnyObject as usize);
            return Err(CaptureError::Os {
                step: "SCStream startCapture",
                message,
            });
        }
        Ok(Self {
            stream,
            output,
            _queue: queue,
            ctx,
            info,
            fps,
        })
    }

    pub fn info(&self) -> &ScreenInfo {
        &self.info
    }

    /// Capture samples rejected by the encoder so far. Never waits on a callback.
    pub fn dropped(&self) -> Option<u64> {
        let g = self.ctx.writer.try_lock().ok()?;
        g.writer.as_ref().map(|w| w.dropped())
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Starts the file at host time `t0`.
    pub fn go(&self, t0: u64) -> Result<(), CaptureError> {
        let mut g = self
            .ctx
            .writer
            .lock()
            .map_err(|_| CaptureError::Missing("writer lock"))?;
        match g.writer.as_mut() {
            Some(w) => w.start(t0),
            None => Err(CaptureError::Missing("writer")),
        }
    }

    /// Why the OS stopped the stream early, if it did (a display unplugged, the
    /// permission revoked).
    pub fn stopped(&self) -> Option<String> {
        self.ctx.stopped.lock().ok()?.clone()
    }

    /// Stops the stream and closes the file.
    pub fn finish(self) -> Result<WriterStats, CaptureError> {
        let (tx, rx) = mpsc::channel::<()>();
        let block = RcBlock::new(move |_err: *mut AnyObject| {
            let _ = tx.send(());
        });
        // SAFETY: a block the stream copies and calls once capture has stopped.
        let () = unsafe { msg_send![&*self.stream, stopCaptureWithCompletionHandler: &*block] };
        let stopped = super::wait(&rx, "the screen capture to stop", 15);
        self.ctx.live.store(false, Ordering::Release);
        ctx_registry().remove(&*self.output as *const AnyObject as usize);
        let writer = self.ctx.writer.lock().ok().and_then(|mut g| {
            g.last = None;
            g.writer.take()
        });
        stopped?;
        writer.ok_or(CaptureError::Missing("writer"))?.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn h264_pictures_fit_the_encoder() {
        assert_eq!(fit((5120, 2880), false, None), (4096, 2304));
        assert_eq!(fit((2160, 3840), false, None), (2160, 3840));
        assert_eq!(fit((3024, 1964), false, None), (3024, 1964));
        assert_eq!(fit((5120, 2880), true, None), (5120, 2880));
        assert_eq!(fit((1001, 777), false, None), (1000, 776));
    }

    #[test]
    fn a_pixel_cap_scales_the_picture_down_keeping_its_aspect() {
        let cap = Some(2560 * 1440);
        assert_eq!(fit((5120, 2880), false, cap), (2560, 1440));
        assert_eq!(fit((5120, 2880), true, cap), (2560, 1440));
        assert_eq!(fit((2160, 3840), false, cap), (1440, 2560));
        assert_eq!(fit((1920, 1080), false, cap), (1920, 1080));
    }
}
