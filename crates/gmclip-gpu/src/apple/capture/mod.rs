//! Recording (decision 0045, SPEC §5.18): ScreenCaptureKit for displays and windows,
//! AVFoundation for the camera and the mic, AVAssetWriter for the files, a listen-only
//! CGEventTap for the pointer and keys, and the Accessibility API for the focused field.
//!
//! Every framework is reached through the Objective-C runtime or its C API (as
//! `share.rs` and `vision.rs` do), so no binding crate is added; a missing class is an
//! error, never a panic. Times are host time (`mach_absolute_time` ticks): ScreenCapture
//! Kit, AVCapture and CGEvent all stamp in it, so the files and the input log of one
//! session share a clock without matching anything afterwards.
//!
//! This module only captures. `gmclip-media::rec` turns what it reports into a session
//! folder (`session.json`, `events.ndjson`, the cursor images).

mod face;
mod focus;
mod input;
mod perms;
mod preview;
mod screen;
mod sources;
mod writer;

use std::ffi::{CString, c_void};
use std::sync::OnceLock;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_media::CMTime;

pub use face::{FaceConfig, FaceRecorder};
pub use focus::{
    AxSwitch, CursorShape, FocusProbe, Focused, Frontmost, Switched, cursor_shape,
    restore_switches, window_bounds,
};
pub use input::{InputEvent, InputTap, mouse_location};
pub use perms::{Perm, PermKind, Permissions, permissions, request};
pub use preview::{CameraPreview, PreviewFrame};
pub use screen::{ScreenConfig, ScreenRecorder, ScreenTarget};
pub use sources::{Device, Display, Sources, Window, live_encoders, sources};
pub use writer::{WriterStats, finalize};

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "ScreenCaptureKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// Why recording could not start or finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("the OS is missing {0}")]
    Missing(&'static str),
    #[error("{0} permission is not granted")]
    Permission(&'static str),
    #[error("no such {0}: {1}")]
    NoSuch(&'static str, String),
    #[error("{step} failed: {message}")]
    Os { step: &'static str, message: String },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

pub(crate) fn class(name: &'static str) -> Result<&'static AnyClass, CaptureError> {
    let c = CString::new(name).map_err(|_| CaptureError::Missing(name))?;
    AnyClass::get(&c).ok_or(CaptureError::Missing(name))
}

// ---- host clock ----

#[repr(C)]
#[derive(Default)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebase) -> i32;
}

fn timebase() -> (u64, u64) {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut tb = MachTimebase::default();
        // SAFETY: fills a caller-owned struct; returns KERN_SUCCESS (0) on every Mac.
        let r = unsafe { mach_timebase_info(&mut tb) };
        if r != 0 || tb.denom == 0 {
            (1, 1)
        } else {
            (u64::from(tb.numer), u64::from(tb.denom))
        }
    })
}

/// Host time now, in `mach_absolute_time` ticks.
pub fn host_now() -> u64 {
    // SAFETY: no arguments; reads the monotonic clock.
    unsafe { mach_absolute_time() }
}

/// Host ticks → nanoseconds.
pub fn host_to_ns(ticks: u64) -> u64 {
    let (n, d) = timebase();
    (u128::from(ticks) * u128::from(n) / u128::from(d)) as u64
}

/// Nanoseconds → host ticks.
pub fn ns_to_host(ns: u64) -> u64 {
    let (n, d) = timebase();
    (u128::from(ns) * u128::from(d) / u128::from(n)) as u64
}

// ---- CoreMedia ----

unsafe extern "C" {
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: *mut c_void) -> CMTime;
    fn CMSampleBufferGetSampleAttachmentsArray(sbuf: *mut c_void, create: u8) -> *mut AnyObject;
    fn CMSampleBufferGetImageBuffer(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetFormatDescription(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferDataIsReady(sbuf: *mut c_void) -> u8;
    fn CMClockMakeHostTimeFromSystemUnits(host: u64) -> CMTime;
    fn CMClockConvertHostTimeToSystemUnits(t: CMTime) -> u64;
    fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(desc: *mut c_void) -> *const Asbd;
    fn CVPixelBufferGetWidth(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(pb: *mut c_void) -> usize;
}

/// `AudioStreamBasicDescription`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Asbd {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

/// A sample buffer's presentation time in host ticks (both capture APIs stamp on the
/// host clock).
fn sample_host_time(sbuf: *mut c_void) -> u64 {
    // SAFETY: `sbuf` is a live CMSampleBuffer handed to a capture callback.
    let pts = unsafe { CMSampleBufferGetPresentationTimeStamp(sbuf) };
    // SAFETY: converts a host-clock CMTime; returns 0 for an invalid time.
    unsafe { CMClockConvertHostTimeToSystemUnits(pts) }
}

fn host_cmtime(ticks: u64) -> CMTime {
    // SAFETY: plain value conversion.
    unsafe { CMClockMakeHostTimeFromSystemUnits(ticks) }
}

fn cmtime(value: i64, timescale: i32) -> CMTime {
    // SAFETY: plain value construction.
    unsafe { CMTimeMake(value, timescale) }
}

// ---- Foundation helpers ----

pub(crate) fn ns_string(s: &str) -> Result<Retained<AnyObject>, CaptureError> {
    let c = CString::new(s).map_err(|_| CaptureError::Missing("a NUL-free string"))?;
    let cls = class("NSString")?;
    // SAFETY: a NUL-terminated UTF-8 string; returns an autoreleased NSString or nil.
    let s: Option<Retained<AnyObject>> =
        unsafe { msg_send![cls, stringWithUTF8String: c.as_ptr()] };
    s.ok_or(CaptureError::Missing("NSString"))
}

/// An `NSString` (or nil) as a Rust string.
pub(crate) fn rust_string(s: Option<&AnyObject>) -> Option<String> {
    let s = s?;
    // SAFETY: -UTF8String on an NSString returns a pointer valid for the autorelease
    // scope, or NULL; it is copied out at once.
    let p: *const std::ffi::c_char = unsafe { msg_send![s, UTF8String] };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated (checked above).
    Some(
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned(),
    )
}

pub(crate) fn ns_number_i64(n: i64) -> Result<Retained<AnyObject>, CaptureError> {
    let cls = class("NSNumber")?;
    // SAFETY: +numberWithLongLong: returns an autoreleased NSNumber.
    let v: Option<Retained<AnyObject>> = unsafe { msg_send![cls, numberWithLongLong: n] };
    v.ok_or(CaptureError::Missing("NSNumber"))
}

pub(crate) fn ns_number_f64(n: f64) -> Result<Retained<AnyObject>, CaptureError> {
    let cls = class("NSNumber")?;
    // SAFETY: +numberWithDouble: returns an autoreleased NSNumber.
    let v: Option<Retained<AnyObject>> = unsafe { msg_send![cls, numberWithDouble: n] };
    v.ok_or(CaptureError::Missing("NSNumber"))
}

/// An `NSDictionary` from `(key, value)` pairs.
pub(crate) fn ns_dict(
    pairs: &[(&AnyObject, &AnyObject)],
) -> Result<Retained<AnyObject>, CaptureError> {
    let cls = class("NSDictionary")?;
    let keys: Vec<*const AnyObject> = pairs.iter().map(|(k, _)| *k as *const AnyObject).collect();
    let vals: Vec<*const AnyObject> = pairs.iter().map(|(_, v)| *v as *const AnyObject).collect();
    // SAFETY: two arrays of `count` non-nil objects; the dictionary retains them.
    let d: Option<Retained<AnyObject>> = unsafe {
        msg_send![cls, dictionaryWithObjects: vals.as_ptr(), forKeys: keys.as_ptr(), count: pairs.len()]
    };
    d.ok_or(CaptureError::Missing("NSDictionary"))
}

pub(crate) fn ns_array_empty() -> Result<Retained<AnyObject>, CaptureError> {
    let cls = class("NSArray")?;
    // SAFETY: +array returns an autoreleased empty array.
    let a: Option<Retained<AnyObject>> = unsafe { msg_send![cls, array] };
    a.ok_or(CaptureError::Missing("NSArray"))
}

/// The objects of an `NSArray`.
pub(crate) fn ns_array_items(a: &AnyObject) -> Vec<Retained<AnyObject>> {
    // SAFETY: -count on an NSArray.
    let n: usize = unsafe { msg_send![a, count] };
    (0..n)
        .filter_map(|i| {
            // SAFETY: index < count; returns the (non-nil) element.
            let o: Option<Retained<AnyObject>> = unsafe { msg_send![a, objectAtIndex: i] };
            o
        })
        .collect()
}

/// An `NSError`'s description, for messages.
pub(crate) fn error_text(e: *mut AnyObject) -> String {
    if e.is_null() {
        return "unknown error".into();
    }
    // SAFETY: a live NSError; -localizedDescription returns an NSString.
    let d: Option<Retained<AnyObject>> = unsafe { msg_send![e, localizedDescription] };
    rust_string(d.as_deref()).unwrap_or_else(|| "unknown error".into())
}

/// `+[NSURL fileURLWithPath:]`.
pub(crate) fn file_url(path: &std::path::Path) -> Result<Retained<AnyObject>, CaptureError> {
    let s = path
        .to_str()
        .ok_or(CaptureError::NoSuch("path", path.display().to_string()))?;
    let ns = ns_string(s)?;
    let cls = class("NSURL")?;
    // SAFETY: an NSString path; returns an autoreleased file URL or nil.
    let u: Option<Retained<AnyObject>> = unsafe { msg_send![cls, fileURLWithPath: &*ns] };
    u.ok_or(CaptureError::Missing("NSURL"))
}

// ---- dispatch ----

unsafe extern "C" {
    fn dispatch_queue_create(label: *const std::ffi::c_char, attr: *mut c_void) -> *mut AnyObject;
}

/// A serial dispatch queue (an ObjC object under ARC rules; retained by the returned
/// handle and released on drop).
pub(crate) fn serial_queue(label: &std::ffi::CStr) -> Result<Retained<AnyObject>, CaptureError> {
    // SAFETY: a static label and DISPATCH_QUEUE_SERIAL (NULL); returns a +1 queue.
    let q = unsafe { dispatch_queue_create(label.as_ptr(), std::ptr::null_mut()) };
    // SAFETY: a +1 object from a create function: owned here.
    unsafe { Retained::from_raw(q) }.ok_or(CaptureError::Missing("dispatch_queue_create"))
}

/// Waits for a completion handler that reports into `rx`.
pub(crate) fn wait<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    what: &'static str,
    secs: u64,
) -> Result<T, CaptureError> {
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .map_err(|_| CaptureError::Timeout(what))
}

/// The object a delegate callback was registered for, keyed by the delegate's address
/// (the ObjC classes built here have no Rust state of their own).
pub(crate) struct Registry<T: ?Sized>(std::sync::Mutex<Vec<(usize, std::sync::Arc<T>)>>);

impl<T: ?Sized> Registry<T> {
    pub(crate) const fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }
    pub(crate) fn insert(&self, key: usize, v: std::sync::Arc<T>) {
        if let Ok(mut g) = self.0.lock() {
            g.push((key, v));
        }
    }
    pub(crate) fn get(&self, key: usize) -> Option<std::sync::Arc<T>> {
        self.0
            .lock()
            .ok()?
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    }
    pub(crate) fn remove(&self, key: usize) {
        if let Ok(mut g) = self.0.lock() {
            g.retain(|(k, _)| *k != key);
        }
    }
}
