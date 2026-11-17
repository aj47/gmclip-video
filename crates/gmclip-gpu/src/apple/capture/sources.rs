//! What can be recorded: displays and windows (ScreenCaptureKit's shareable content),
//! cameras and microphones (AVCaptureDevice).

use std::ffi::c_void;
use std::sync::mpsc;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_core_foundation::CGRect;

use super::{CaptureError, class, ns_array_items, rust_string};

unsafe extern "C" {
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut c_void);
    fn CGMainDisplayID() -> u32;
    static AVMediaTypeVideo: *const AnyObject;
    static AVMediaTypeAudio: *const AnyObject;
}

/// A display: its id, name, frame in global points (origin top left of the main
/// display) and backing scale (pixels per point).
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub id: u32,
    pub name: String,
    pub main: bool,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
}

impl Display {
    /// The display's size in pixels.
    pub fn pixels(&self) -> (u32, u32) {
        (
            (self.w * self.scale).round() as u32,
            (self.h * self.scale).round() as u32,
        )
    }
}

/// An on-screen window.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: u32,
    pub title: String,
    pub app: String,
    pub bundle: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A camera or microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sources {
    pub displays: Vec<Display>,
    pub windows: Vec<Window>,
    pub cameras: Vec<Device>,
    pub mics: Vec<Device>,
}

/// `SCShareableContent` (on-screen windows only, desktop windows left out). Needs the
/// Screen Recording permission.
pub(crate) fn shareable() -> Result<Retained<AnyObject>, CaptureError> {
    let c = class("SCShareableContent")?;
    let (tx, rx) = mpsc::channel::<Result<Retained<AnyObject>, String>>();
    let block = RcBlock::new(move |content: *mut AnyObject, err: *mut AnyObject| {
        // SAFETY: the handler's content is nil or a live SCShareableContent.
        let got = unsafe { Retained::retain(content) };
        let _ = tx.send(got.ok_or_else(|| super::error_text(err)));
    });
    // SAFETY: two BOOLs and a block the framework copies and calls once.
    let () = unsafe {
        msg_send![c, getShareableContentExcludingDesktopWindows: Bool::YES, onScreenWindowsOnly: Bool::YES, completionHandler: &*block]
    };
    super::wait(&rx, "the screen list", 10)?.map_err(|message| CaptureError::Os {
        step: "SCShareableContent",
        message,
    })
}

fn display_scale(id: u32) -> f64 {
    // SAFETY: returns a +1 mode (or NULL) released below.
    let mode = unsafe { CGDisplayCopyDisplayMode(id) };
    if mode.is_null() {
        return 1.0;
    }
    // SAFETY: a live display mode.
    let (px, pt) = unsafe {
        (
            CGDisplayModeGetPixelWidth(mode),
            CGDisplayModeGetWidth(mode),
        )
    };
    // SAFETY: releases the copy made above.
    unsafe { CGDisplayModeRelease(mode) };
    if pt == 0 { 1.0 } else { px as f64 / pt as f64 }
}

/// Display names from `NSScreen` (`localizedName`), by display id.
fn screen_names() -> Vec<(u32, String)> {
    let Ok(c) = class("NSScreen") else {
        return Vec::new();
    };
    // SAFETY: +screens returns an NSArray of NSScreen.
    let screens: Option<Retained<AnyObject>> = unsafe { msg_send![c, screens] };
    let Some(screens) = screens else {
        return Vec::new();
    };
    let Ok(key) = super::ns_string("NSScreenNumber") else {
        return Vec::new();
    };
    ns_array_items(&screens)
        .iter()
        .filter_map(|s| {
            // SAFETY: -deviceDescription returns an NSDictionary.
            let d: Option<Retained<AnyObject>> = unsafe { msg_send![&**s, deviceDescription] };
            // SAFETY: -objectForKey: on that dictionary; an NSNumber or nil.
            let n: Option<Retained<AnyObject>> = unsafe { msg_send![&*d?, objectForKey: &*key] };
            // SAFETY: -unsignedIntValue on an NSNumber.
            let id: u32 = unsafe { msg_send![&*n?, unsignedIntValue] };
            // SAFETY: -localizedName (macOS 10.15+) returns an NSString.
            let name: Option<Retained<AnyObject>> = unsafe { msg_send![&**s, localizedName] };
            Some((id, rust_string(name.as_deref())?))
        })
        .collect()
}

pub(crate) fn display_of(d: &AnyObject, names: &[(u32, String)], main: u32) -> Display {
    // SAFETY: SCDisplay's -displayID (CGDirectDisplayID) and -frame (CGRect).
    let (id, frame): (u32, CGRect) = unsafe { (msg_send![d, displayID], msg_send![d, frame]) };
    Display {
        id,
        name: names
            .iter()
            .find(|(i, _)| *i == id)
            .map_or_else(|| format!("Display {id}"), |(_, n)| n.clone()),
        main: id == main,
        x: frame.origin.x,
        y: frame.origin.y,
        w: frame.size.width,
        h: frame.size.height,
        scale: display_scale(id),
    }
}

pub(crate) fn window_of(w: &AnyObject) -> Window {
    // SAFETY: SCWindow's -windowID, -frame, -title and -owningApplication (whose
    // -applicationName and -bundleIdentifier are NSStrings).
    unsafe {
        let id: u32 = msg_send![w, windowID];
        let frame: CGRect = msg_send![w, frame];
        let title: Option<Retained<AnyObject>> = msg_send![w, title];
        let app: Option<Retained<AnyObject>> = msg_send![w, owningApplication];
        let (name, bundle) = match app.as_deref() {
            Some(a) => {
                let n: Option<Retained<AnyObject>> = msg_send![a, applicationName];
                let b: Option<Retained<AnyObject>> = msg_send![a, bundleIdentifier];
                (rust_string(n.as_deref()), rust_string(b.as_deref()))
            }
            None => (None, None),
        };
        Window {
            id,
            title: rust_string(title.as_deref()).unwrap_or_default(),
            app: name.unwrap_or_default(),
            bundle: bundle.unwrap_or_default(),
            x: frame.origin.x,
            y: frame.origin.y,
            w: frame.size.width,
            h: frame.size.height,
        }
    }
}

fn devices(media: *const AnyObject) -> Vec<Device> {
    let Ok(c) = class("AVCaptureDevice") else {
        return Vec::new();
    };
    // SAFETY: +devicesWithMediaType: returns an NSArray of AVCaptureDevice.
    let list: Option<Retained<AnyObject>> = unsafe { msg_send![c, devicesWithMediaType: media] };
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out: Vec<Device> = ns_array_items(&list)
        .iter()
        .filter_map(|d| {
            // SAFETY: -uniqueID and -localizedName are NSStrings.
            let (id, name): (Option<Retained<AnyObject>>, Option<Retained<AnyObject>>) =
                unsafe { (msg_send![&**d, uniqueID], msg_send![&**d, localizedName]) };
            Some(Device {
                id: rust_string(id.as_deref())?,
                name: rust_string(name.as_deref()).unwrap_or_default(),
            })
        })
        .collect();
    // The default device first: the one a recording uses when none is picked.
    // SAFETY: +defaultDeviceWithMediaType: returns the default device or nil; -uniqueID
    // is an NSString.
    let default: Option<String> = unsafe {
        let d: Option<Retained<AnyObject>> = msg_send![c, defaultDeviceWithMediaType: media];
        d.and_then(|d| {
            let id: Option<Retained<AnyObject>> = msg_send![&*d, uniqueID];
            rust_string(id.as_deref())
        })
    };
    out.sort_by_key(|d| Some(&d.id) != default.as_ref());
    out
}

/// The capture device with this unique id (or the default one for `media`).
pub(crate) fn device(
    media: *const AnyObject,
    id: Option<&str>,
) -> Result<Retained<AnyObject>, CaptureError> {
    let c = class("AVCaptureDevice")?;
    let d: Option<Retained<AnyObject>> = match id {
        Some(id) => {
            let s = super::ns_string(id)?;
            // SAFETY: +deviceWithUniqueID: returns the device or nil.
            unsafe { msg_send![c, deviceWithUniqueID: &*s] }
        }
        // SAFETY: +defaultDeviceWithMediaType: returns the default device or nil.
        None => unsafe { msg_send![c, defaultDeviceWithMediaType: media] },
    };
    d.ok_or_else(|| CaptureError::NoSuch("capture device", id.unwrap_or("default").into()))
}

/// The camera (`video`) or mic (`!video`) device by id, or the default.
pub(crate) fn av_device(
    video: bool,
    id: Option<&str>,
) -> Result<Retained<AnyObject>, CaptureError> {
    // SAFETY: AVFoundation constants.
    let media = unsafe {
        if video {
            AVMediaTypeVideo
        } else {
            AVMediaTypeAudio
        }
    };
    device(media, id)
}

/// Everything that can be recorded. Displays and windows need Screen Recording (empty
/// without it); cameras and mics are listed either way.
pub fn sources() -> Sources {
    // SAFETY: AVFoundation constants.
    let (v, a) = unsafe { (AVMediaTypeVideo, AVMediaTypeAudio) };
    let mut out = Sources {
        cameras: devices(v),
        mics: devices(a),
        ..Sources::default()
    };
    let Ok(content) = shareable() else {
        return out;
    };
    let names = screen_names();
    // SAFETY: no arguments.
    let main = unsafe { CGMainDisplayID() };
    // SAFETY: SCShareableContent's -displays and -windows are NSArrays.
    let (ds, ws): (Option<Retained<AnyObject>>, Option<Retained<AnyObject>>) = unsafe {
        (
            msg_send![&*content, displays],
            msg_send![&*content, windows],
        )
    };
    if let Some(ds) = ds {
        out.displays = ns_array_items(&ds)
            .iter()
            .map(|d| display_of(d, &names, main))
            .collect();
        out.displays.sort_by_key(|d| (!d.main, d.id));
    }
    if let Some(ws) = ws {
        out.windows = ns_array_items(&ws)
            .iter()
            .filter(|w| {
                // SAFETY: -windowLayer (NSInteger); 0 is the normal window layer.
                let layer: isize = unsafe { msg_send![&***w, windowLayer] };
                layer == 0
            })
            .map(|w| window_of(w))
            .filter(|w| w.w >= 64.0 && w.h >= 64.0 && !w.app.is_empty())
            .collect();
    }
    out
}

/// Apps that encode a live stream (bundle ids). They share the Mac's one hardware video
/// encoder with the recorder, and a recording that fills it starves their stream until
/// the ingest servers hang up (decision 0055).
const LIVE_ENCODERS: &[&str] = &[
    "com.obsproject.obs-studio",
    "com.streamlabs.slobs",
    "com.ecamm.EcammLive",
    "com.telestream.wirecast",
];

fn is_live_encoder(bundle: &str) -> bool {
    LIVE_ENCODERS.contains(&bundle)
}

/// The names of the running apps that may be streaming (OBS and the like).
pub fn live_encoders() -> Vec<String> {
    let Ok(c) = class("NSWorkspace") else {
        return Vec::new();
    };
    // SAFETY: +sharedWorkspace, -runningApplications (an NSArray of
    // NSRunningApplication) and their NSString properties.
    unsafe {
        let ws: Option<Retained<AnyObject>> = msg_send![c, sharedWorkspace];
        let Some(ws) = ws else { return Vec::new() };
        let apps: Option<Retained<AnyObject>> = msg_send![&*ws, runningApplications];
        let Some(apps) = apps else { return Vec::new() };
        let mut out: Vec<String> = ns_array_items(&apps)
            .iter()
            .filter_map(|a| {
                let b: Option<Retained<AnyObject>> = msg_send![&**a, bundleIdentifier];
                let b = rust_string(b.as_deref())?;
                if !is_live_encoder(&b) {
                    return None;
                }
                let n: Option<Retained<AnyObject>> = msg_send![&**a, localizedName];
                Some(rust_string(n.as_deref()).unwrap_or(b))
            })
            .collect();
        out.dedup();
        out
    }
}

/// The SCDisplay or SCWindow object with this id, from fresh shareable content.
pub(crate) fn sc_object(window: bool, id: u32) -> Result<Retained<AnyObject>, CaptureError> {
    let content = shareable()?;
    // SAFETY: SCShareableContent's -displays / -windows are NSArrays.
    let list: Option<Retained<AnyObject>> = unsafe {
        if window {
            msg_send![&*content, windows]
        } else {
            msg_send![&*content, displays]
        }
    };
    let list = list.ok_or(CaptureError::Missing("SCShareableContent"))?;
    ns_array_items(&list)
        .into_iter()
        .find(|o| {
            // SAFETY: -windowID / -displayID on SCWindow / SCDisplay.
            let i: u32 = unsafe {
                if window {
                    msg_send![&**o, windowID]
                } else {
                    msg_send![&**o, displayID]
                }
            };
            i == id
        })
        .ok_or_else(|| {
            CaptureError::NoSuch(if window { "window" } else { "display" }, id.to_string())
        })
}

/// Display names for [`display_of`].
pub(crate) fn names_and_main() -> (Vec<(u32, String)>, u32) {
    // SAFETY: no arguments.
    (screen_names(), unsafe { CGMainDisplayID() })
}

#[cfg(test)]
mod tests {
    use super::is_live_encoder;

    #[test]
    fn obs_is_a_live_encoder_and_a_browser_is_not() {
        assert!(is_live_encoder("com.obsproject.obs-studio"));
        assert!(is_live_encoder("com.ecamm.EcammLive"));
        assert!(!is_live_encoder("com.google.Chrome"));
        assert!(!is_live_encoder(""));
    }
}
