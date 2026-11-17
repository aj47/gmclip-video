//! The pointer and keys: a listen-only `CGEventTap` on its own thread and run loop. It
//! never changes or swallows an event. Key events carry their key code only so the
//! recorder can spot its own hotkeys (stop, marker); the recorder logs counts, never
//! which key (decision 0045). Without Input Monitoring the tap is pointer-only.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use objc2::runtime::AnyObject;

use super::{CaptureError, host_now, host_to_ns, ns_to_host};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        callback: TapCallback,
        user: *mut c_void,
    ) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn CGEventGetLocation(event: *mut c_void) -> objc2_core_foundation::CGPoint;
    fn CGEventGetTimestamp(event: *mut c_void) -> u64;
    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
    fn CGEventGetDoubleValueField(event: *mut c_void, field: u32) -> f64;
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventCreate(source: *mut c_void) -> *mut c_void;
}

unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(
        alloc: *mut c_void,
        port: *mut c_void,
        order: isize,
    ) -> *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const AnyObject);
    fn CFRunLoopRunInMode(mode: *const AnyObject, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopStop(rl: *mut c_void);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: *const AnyObject;
    static kCFRunLoopDefaultMode: *const AnyObject;
}

type TapCallback = extern "C-unwind" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void;

// CGEventType values.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const SCROLL: u32 = 22;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;
const TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_INPUT: u32 = 0xFFFF_FFFF;
// CGEventField values.
const CLICK_STATE: u32 = 1;
const BUTTON_NUMBER: u32 = 3;
const KEYCODE: u32 = 9;
const AUTOREPEAT: u32 = 8;
const SCROLL_POINT_DELTA_1: u32 = 96;
const SCROLL_POINT_DELTA_2: u32 = 97;

const POINTER: &[u32] = &[
    LEFT_DOWN,
    LEFT_UP,
    RIGHT_DOWN,
    RIGHT_UP,
    MOVED,
    LEFT_DRAGGED,
    RIGHT_DRAGGED,
    SCROLL,
    OTHER_DOWN,
    OTHER_UP,
    OTHER_DRAGGED,
];
const KEYS: &[u32] = &[KEY_DOWN, KEY_UP, FLAGS_CHANGED];

fn mask(types: &[u32]) -> u64 {
    types.iter().fold(0, |m, t| m | (1u64 << t))
}

/// One input event, at host time `t`, in global points (origin top left of the main
/// display).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum InputEvent {
    Move {
        t: u64,
        x: f64,
        y: f64,
        drag: bool,
    },
    Button {
        t: u64,
        x: f64,
        y: f64,
        button: u8,
        down: bool,
        count: u8,
    },
    Scroll {
        t: u64,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// A key went down (never a repeat) or up. `code` is for hotkeys only.
    Key {
        t: u64,
        down: bool,
        code: u16,
        flags: u64,
    },
    Flags {
        t: u64,
        flags: u64,
    },
}

/// The pointer's position now, in global points.
pub fn mouse_location() -> (f64, f64) {
    // SAFETY: CGEventCreate(NULL) returns a +1 event of the current state (or NULL).
    let e = unsafe { CGEventCreate(std::ptr::null_mut()) };
    if e.is_null() {
        return (0.0, 0.0);
    }
    // SAFETY: a live event, released after reading.
    let p = unsafe { CGEventGetLocation(e) };
    // SAFETY: releases the event created above.
    unsafe { CFRelease(e) };
    (p.x, p.y)
}

/// An event timestamp in host ticks. `CGEventGetTimestamp` is documented as
/// nanoseconds since startup but is host ticks on some systems; whichever is nearer
/// to now is taken.
fn event_host_time(ts: u64) -> u64 {
    let now = host_now();
    let as_ticks = now.abs_diff(ts);
    let as_ns = host_to_ns(now).abs_diff(ts);
    if as_ticks <= as_ns {
        ts
    } else {
        ns_to_host(ts)
    }
}

pub type Sink = Box<dyn Fn(InputEvent) + Send + Sync>;

struct TapCtx {
    sink: Sink,
    port: std::sync::atomic::AtomicUsize,
}

extern "C-unwind" fn on_event(
    _proxy: *mut c_void,
    kind: u32,
    event: *mut c_void,
    user: *mut c_void,
) -> *mut c_void {
    // SAFETY: `user` is the `TapCtx` the tap thread keeps alive until the tap is gone.
    let ctx = unsafe { &*(user as *const TapCtx) };
    if kind == TAP_DISABLED_TIMEOUT || kind == TAP_DISABLED_INPUT {
        let port = ctx.port.load(std::sync::atomic::Ordering::Acquire);
        if port != 0 {
            // SAFETY: the live tap this callback belongs to.
            unsafe { CGEventTapEnable(port as *mut c_void, true) };
        }
        return event;
    }
    if event.is_null() {
        return event;
    }
    // SAFETY: a live event for the duration of the callback.
    let (t, p) = unsafe {
        (
            event_host_time(CGEventGetTimestamp(event)),
            CGEventGetLocation(event),
        )
    };
    // SAFETY: field reads on the live event.
    let int = |f: u32| unsafe { CGEventGetIntegerValueField(event, f) };
    let ev = match kind {
        MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => InputEvent::Move {
            t,
            x: p.x,
            y: p.y,
            drag: kind != MOVED,
        },
        LEFT_DOWN | LEFT_UP | RIGHT_DOWN | RIGHT_UP | OTHER_DOWN | OTHER_UP => InputEvent::Button {
            t,
            x: p.x,
            y: p.y,
            button: match kind {
                LEFT_DOWN | LEFT_UP => 0,
                RIGHT_DOWN | RIGHT_UP => 1,
                _ => int(BUTTON_NUMBER).clamp(2, 255) as u8,
            },
            down: matches!(kind, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN),
            count: int(CLICK_STATE).clamp(0, 255) as u8,
        },
        SCROLL => InputEvent::Scroll {
            t,
            x: p.x,
            y: p.y,
            // SAFETY: field reads on the live event.
            dx: unsafe { CGEventGetDoubleValueField(event, SCROLL_POINT_DELTA_2) },
            // SAFETY: as above.
            dy: unsafe { CGEventGetDoubleValueField(event, SCROLL_POINT_DELTA_1) },
        },
        KEY_DOWN | KEY_UP => {
            if kind == KEY_DOWN && int(AUTOREPEAT) != 0 {
                return event;
            }
            InputEvent::Key {
                t,
                down: kind == KEY_DOWN,
                code: int(KEYCODE).clamp(0, 0xFFFF) as u16,
                // SAFETY: the live event.
                flags: unsafe { CGEventGetFlags(event) },
            }
        }
        FLAGS_CHANGED => InputEvent::Flags {
            t,
            // SAFETY: the live event.
            flags: unsafe { CGEventGetFlags(event) },
        },
        _ => return event,
    };
    (ctx.sink)(ev);
    event
}

/// A running event tap.
pub struct InputTap {
    runloop: usize,
    quit: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Keys are seen (Input Monitoring granted); else pointer only.
    pub keys: bool,
}

impl std::fmt::Debug for InputTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputTap")
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl InputTap {
    /// Starts the tap on its own thread; `sink` is called on that thread.
    pub fn start(sink: Sink) -> Result<Self, CaptureError> {
        let (tx, rx) = mpsc::channel::<Result<(usize, bool), CaptureError>>();
        let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let quit_tap = quit.clone();
        let thread = std::thread::Builder::new()
            .name("gmclip-input-tap".into())
            .spawn(move || {
                let ctx = Arc::new(TapCtx {
                    sink,
                    port: std::sync::atomic::AtomicUsize::new(0),
                });
                let user = Arc::as_ptr(&ctx) as *mut c_void;
                let mut keys = true;
                // SAFETY: a session-level, head-inserted, listen-only tap (1, 0, 1);
                // `user` stays valid while this thread runs (ctx is held below).
                let mut port = unsafe {
                    CGEventTapCreate(1, 0, 1, mask(POINTER) | mask(KEYS), on_event, user)
                };
                if port.is_null() {
                    keys = false;
                    // SAFETY: as above, pointer events only.
                    port = unsafe { CGEventTapCreate(1, 0, 1, mask(POINTER), on_event, user) };
                }
                if port.is_null() {
                    let _ = tx.send(Err(CaptureError::Permission("Input Monitoring")));
                    return;
                }
                ctx.port
                    .store(port as usize, std::sync::atomic::Ordering::Release);
                // SAFETY: a run-loop source for the live tap, added to this thread's
                // run loop in the common modes; the run loop runs until stopped.
                unsafe {
                    let src = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), port, 0);
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
                    CGEventTapEnable(port, true);
                    let _ = tx.send(Ok((rl as usize, keys)));
                    // In slices, so a stop that lands before the loop runs still ends it.
                    while !quit_tap.load(std::sync::atomic::Ordering::Acquire) {
                        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
                    }
                    CGEventTapEnable(port, false);
                    CFRelease(src);
                    CFRelease(port);
                }
                drop(ctx);
            })
            .map_err(|e| CaptureError::Os {
                step: "input tap thread",
                message: e.to_string(),
            })?;
        let (runloop, keys) = super::wait(&rx, "the input tap", 5)??;
        Ok(Self {
            runloop,
            quit,
            thread: Some(thread),
            keys,
        })
    }

    /// Stops the tap and joins its thread.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        if let Some(t) = self.thread.take() {
            self.quit.store(true, std::sync::atomic::Ordering::Release);
            // SAFETY: CFRunLoopStop is thread-safe; the run loop belongs to the tap
            // thread, alive until it is joined here.
            unsafe { CFRunLoopStop(self.runloop as *mut c_void) };
            let _ = t.join();
        }
    }
}

impl Drop for InputTap {
    fn drop(&mut self) {
        self.halt();
    }
}
