//! The five permissions recording needs (decision 0045): Screen Recording, Camera,
//! Microphone, Accessibility (the focused field) and Input Monitoring (keys and the
//! pointer from the event tap). Settings → Recording asks for them in this order when
//! it is turned on; each one's state is shown, and a refused one turns off only what
//! needs it.

use std::sync::mpsc;

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyObject, Bool};

use super::{CaptureError, class};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const AnyObject) -> bool;
    static kAXTrustedCheckOptionPrompt: *const AnyObject;
    static AVMediaTypeVideo: *const AnyObject;
    static AVMediaTypeAudio: *const AnyObject;
}

/// One permission's state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Perm {
    Granted,
    Denied,
    /// Never asked (camera and mic only: the OS cannot say for the other three until it
    /// has asked, so they read `Denied` before then).
    NotAsked,
}

impl Perm {
    pub fn as_str(self) -> &'static str {
        match self {
            Perm::Granted => "granted",
            Perm::Denied => "denied",
            Perm::NotAsked => "not_asked",
        }
    }
    pub fn granted(self) -> bool {
        self == Perm::Granted
    }
}

/// Which permission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PermKind {
    Screen,
    Camera,
    Microphone,
    Accessibility,
    Input,
}

impl PermKind {
    /// In the order Settings asks for them.
    pub const ALL: [PermKind; 5] = [
        PermKind::Screen,
        PermKind::Camera,
        PermKind::Microphone,
        PermKind::Accessibility,
        PermKind::Input,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            PermKind::Screen => "screen",
            PermKind::Camera => "camera",
            PermKind::Microphone => "microphone",
            PermKind::Accessibility => "accessibility",
            PermKind::Input => "input",
        }
    }
    /// The System Settings pane that grants it (`x-apple.systempreferences:` URL).
    pub fn settings_url(self) -> &'static str {
        match self {
            PermKind::Screen => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            PermKind::Camera => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera"
            }
            PermKind::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            PermKind::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            PermKind::Input => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
        }
    }
}

/// Every permission's state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub screen: Perm,
    pub camera: Perm,
    pub microphone: Perm,
    pub accessibility: Perm,
    pub input: Perm,
}

impl Permissions {
    pub fn get(&self, k: PermKind) -> Perm {
        match k {
            PermKind::Screen => self.screen,
            PermKind::Camera => self.camera,
            PermKind::Microphone => self.microphone,
            PermKind::Accessibility => self.accessibility,
            PermKind::Input => self.input,
        }
    }
}

fn yes(b: bool) -> Perm {
    if b { Perm::Granted } else { Perm::Denied }
}

fn av_status(media: *const AnyObject) -> Perm {
    let Ok(c) = class("AVCaptureDevice") else {
        return Perm::Denied;
    };
    // SAFETY: an AVMediaType constant; returns AVAuthorizationStatus (NSInteger).
    let s: isize = unsafe { msg_send![c, authorizationStatusForMediaType: media] };
    match s {
        0 => Perm::NotAsked,
        3 => Perm::Granted,
        _ => Perm::Denied,
    }
}

/// Every permission's state, without asking for any.
pub fn permissions() -> Permissions {
    // SAFETY: the preflight and trust checks take no arguments and never prompt; the
    // AVMediaType statics are AVFoundation constants.
    unsafe {
        Permissions {
            screen: yes(CGPreflightScreenCaptureAccess()),
            camera: av_status(AVMediaTypeVideo),
            microphone: av_status(AVMediaTypeAudio),
            accessibility: yes(AXIsProcessTrusted()),
            input: yes(CGPreflightListenEventAccess()),
        }
    }
}

fn av_request(media: *const AnyObject) -> Result<Perm, CaptureError> {
    match av_status(media) {
        Perm::NotAsked => {}
        p => return Ok(p),
    }
    let c = class("AVCaptureDevice")?;
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: an AVMediaType constant and a block the framework copies and calls once
    // on an arbitrary queue.
    let () = unsafe { msg_send![c, requestAccessForMediaType: media, completionHandler: &*block] };
    // The person may take a while to answer the sheet.
    let granted = super::wait(&rx, "the permission sheet", 600)?;
    Ok(yes(granted))
}

/// Asks for one permission, showing the OS prompt when it has not been answered, and
/// returns its state. Screen Recording, Accessibility and Input Monitoring answer at
/// once (the OS opens System Settings; the grant takes effect after a relaunch for
/// Screen Recording and Input Monitoring); camera and mic wait for the sheet.
pub fn request(kind: PermKind) -> Result<Perm, CaptureError> {
    match kind {
        // SAFETY: no arguments; may show the OS prompt once.
        PermKind::Screen => Ok(yes(unsafe { CGRequestScreenCaptureAccess() })),
        // SAFETY: no arguments; may show the OS prompt once.
        PermKind::Input => Ok(yes(unsafe { CGRequestListenEventAccess() })),
        PermKind::Accessibility => {
            let t = super::ns_number_i64(1)?;
            // SAFETY: an HIServices constant (a CFString, toll-free an NSString).
            let key = unsafe { &*kAXTrustedCheckOptionPrompt };
            let d = super::ns_dict(&[(key, &*t)])?;
            // SAFETY: a CFDictionary (toll-free NSDictionary) of options.
            Ok(yes(unsafe { AXIsProcessTrustedWithOptions(&*d) }))
        }
        // SAFETY: AVFoundation constants.
        PermKind::Camera => av_request(unsafe { AVMediaTypeVideo }),
        // SAFETY: AVFoundation constants.
        PermKind::Microphone => av_request(unsafe { AVMediaTypeAudio }),
    }
}
