//! What is focused and what the cursor looks like (decision 0045): the Accessibility
//! API's focused element (its frame, role, app, window, and the text caret when it
//! reports one), the frontmost app, a window's bounds, and the system cursor's image.
//!
//! Chromium and Electron apps build their accessibility tree only when asked: the probe
//! sets `AXManualAccessibility` on each app the first time it sees it (and
//! `AXEnhancedUserInterface` on Chrome-family browsers, which read that instead). An app
//! left that way stays slower and, in Chrome's case, acts for a screen reader, so the
//! probe notes each switch it turned on that was not on before and turns it off again
//! when it is dropped (the recording stopped or cancelled). A recorder keeps the same
//! list on disk for a crash to undo ([`restore_switches`]). Secure text fields report no
//! caret, and the recorder counts no keys in them.

use std::collections::HashSet;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::{CaptureError, class, ns_array_items, ns_string, rust_string};

unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateSystemWide() -> *mut c_void;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        el: *mut c_void,
        attr: *const AnyObject,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        el: *mut c_void,
        attr: *const AnyObject,
        param: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        el: *mut c_void,
        attr: *const AnyObject,
        value: *const c_void,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(el: *mut c_void, seconds: f32) -> i32;
    fn AXUIElementGetPid(el: *mut c_void, pid: *mut i32) -> i32;
    fn AXValueGetValue(value: *mut c_void, kind: u32, out: *mut c_void) -> bool;
    fn CFRelease(cf: *const c_void);
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn AXValueGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
    fn CGWindowListCopyWindowInfo(option: u32, window: u32) -> *mut AnyObject;
    fn CGRectMakeWithDictionaryRepresentation(dict: *const AnyObject, rect: *mut CGRect) -> bool;
}

// AXValueType.
const AX_POINT: u32 = 1;
const AX_SIZE: u32 = 2;
const AX_RECT: u32 = 3;
const AX_RANGE: u32 = 4;
/// `kCGWindowListOptionIncludingWindow`.
const INCLUDING_WINDOW: u32 = 1 << 3;
/// `NSBitmapImageFileTypePNG`.
const PNG: usize = 4;

const CHROME_FAMILY: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",
    "org.chromium.Chromium",
    "com.vivaldi.Vivaldi",
];

/// A CF object owned here (released on drop).
struct Cf(*mut c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a +1 CF object owned by this wrapper.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// The focused element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Focused {
    /// `[x, y, w, h]` in global points.
    pub rect: Option<[f64; 4]>,
    pub caret: Option<[f64; 4]>,
    pub window: Option<[f64; 4]>,
    pub role: String,
    pub subrole: String,
    pub app: String,
    pub pid: i32,
    /// A secure text field (passwords).
    pub secure: bool,
}

/// The frontmost app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmost {
    pub bundle: String,
    pub name: String,
    pub pid: i32,
}

/// Reads the focused element; needs Accessibility. Dropping it turns off the switches
/// it turned on.
pub struct FocusProbe {
    system: Cf,
    names: Names,
    ax: Option<SystemAx>,
    switches: Switches,
}

impl std::fmt::Debug for FocusProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FocusProbe")
            .field("apps_seen", &self.switches.seen.len())
            .field("switched", &self.switches.on.len())
            .finish_non_exhaustive()
    }
}

impl Drop for FocusProbe {
    fn drop(&mut self) {
        self.restore();
    }
}

/// An app-wide attribute that makes Chromium and Electron build their tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AxSwitch {
    /// `AXManualAccessibility` (Electron, Chromium).
    Manual,
    /// `AXEnhancedUserInterface` (Chrome-family browsers; VoiceOver sets it too).
    Enhanced,
}

impl AxSwitch {
    pub fn as_str(self) -> &'static str {
        match self {
            AxSwitch::Manual => "AXManualAccessibility",
            AxSwitch::Enhanced => "AXEnhancedUserInterface",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [AxSwitch::Manual, AxSwitch::Enhanced]
            .into_iter()
            .find(|a| a.as_str() == s)
    }
}

/// A switch the probe turned on in an app (it was off, or the app did not say).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switched {
    pub pid: i32,
    /// The app's bundle id then: a pid that is another app now is left alone.
    pub bundle: String,
    pub attr: AxSwitch,
}

/// What the switches read and write: the Accessibility API, or a stand-in in tests.
trait AppAx {
    /// The attribute's value, if the app says.
    fn get(&mut self, pid: i32, attr: AxSwitch) -> Option<bool>;
    /// Sets it. Chromium and Electron answer an error for a set that took effect, so
    /// the answer is not read.
    fn set(&mut self, pid: i32, attr: AxSwitch, on: bool);
    /// The bundle id of the app running as `pid` now ("" when none is).
    fn bundle(&mut self, pid: i32) -> String;
    /// VoiceOver is running: it wants these switches on, so none is turned off.
    fn voice_over(&mut self) -> bool;
}

/// Which apps were woken, and what was turned on in them.
#[derive(Debug, Default)]
struct Switches {
    seen: HashSet<i32>,
    on: Vec<Switched>,
}

impl Switches {
    /// Turns on what `bundle` (as `pid`) needs the first time it is seen, noting each
    /// switch that was not on already.
    fn wake(&mut self, ax: &mut impl AppAx, pid: i32, bundle: &str) {
        if !self.seen.insert(pid) {
            return;
        }
        let mut attrs = vec![AxSwitch::Manual];
        if CHROME_FAMILY.contains(&bundle) {
            attrs.push(AxSwitch::Enhanced);
        }
        for attr in attrs {
            // On already (VoiceOver, another tool): not ours to turn off later.
            if ax.get(pid, attr) == Some(true) {
                continue;
            }
            ax.set(pid, attr, true);
            self.on.push(Switched {
                pid,
                bundle: bundle.to_string(),
                attr,
            });
        }
    }

    /// Turns off everything [`Self::wake`] turned on, and forgets it.
    fn restore(&mut self, ax: &mut impl AppAx) {
        restore_with(ax, &std::mem::take(&mut self.on));
    }
}

/// Turns each switch off in its app, if that app is still running as that pid.
fn restore_with(ax: &mut impl AppAx, list: &[Switched]) {
    if list.is_empty() || ax.voice_over() {
        return;
    }
    for s in list {
        if !s.bundle.is_empty() && ax.bundle(s.pid) == s.bundle {
            ax.set(s.pid, s.attr, false);
        }
    }
}

/// The Accessibility API's application elements.
struct SystemAx {
    manual: Retained<AnyObject>,
    enhanced: Retained<AnyObject>,
    /// How long an app has to answer, in seconds.
    timeout: f32,
}

impl SystemAx {
    fn new(timeout: f32) -> Option<Self> {
        Some(Self {
            manual: ns_string(AxSwitch::Manual.as_str()).ok()?,
            enhanced: ns_string(AxSwitch::Enhanced.as_str()).ok()?,
            timeout,
        })
    }

    fn name(&self, attr: AxSwitch) -> &AnyObject {
        match attr {
            AxSwitch::Manual => &self.manual,
            AxSwitch::Enhanced => &self.enhanced,
        }
    }

    fn app(&self, pid: i32) -> Option<Cf> {
        // SAFETY: returns a +1 application element owned by `Cf`.
        let app = Cf(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        // SAFETY: a live element.
        unsafe { AXUIElementSetMessagingTimeout(app.0, self.timeout) };
        Some(app)
    }
}

impl AppAx for SystemAx {
    fn get(&mut self, pid: i32, attr: AxSwitch) -> Option<bool> {
        let app = self.app(pid)?;
        let v = copy_attr(app.0, self.name(attr))?;
        // SAFETY: a live CF object; a CFBoolean or a CFNumber is an NSNumber (toll-free
        // bridged), which answers -boolValue.
        unsafe {
            let id = CFGetTypeID(v.0);
            if id != CFBooleanGetTypeID() && id != CFNumberGetTypeID() {
                return None;
            }
            let on: bool = msg_send![&*(v.0 as *const AnyObject), boolValue];
            Some(on)
        }
    }

    fn set(&mut self, pid: i32, attr: AxSwitch, on: bool) {
        let Some(app) = self.app(pid) else { return };
        // SAFETY: a live element, an attribute name and a CF boolean constant. An app
        // that does not know the attribute returns an error, which is fine.
        unsafe {
            let v = if on { kCFBooleanTrue } else { kCFBooleanFalse };
            AXUIElementSetAttributeValue(app.0, self.name(attr), v);
        }
    }

    fn bundle(&mut self, pid: i32) -> String {
        bundle_of(pid)
    }

    fn voice_over(&mut self) -> bool {
        let Ok(c) = class("NSWorkspace") else {
            return false;
        };
        // SAFETY: +sharedWorkspace and -isVoiceOverEnabled (a BOOL, macOS 10.13 on).
        unsafe {
            let ws: Option<Retained<AnyObject>> = msg_send![c, sharedWorkspace];
            match ws {
                Some(ws) => msg_send![&*ws, isVoiceOverEnabled],
                None => false,
            }
        }
    }
}

/// Turns off the switches a recorder that never stopped (a crash) left on, each in its
/// app if that app is still running as that pid. Without Accessibility, nothing.
pub fn restore_switches(list: &[Switched]) {
    // SAFETY: no arguments.
    if !unsafe { AXIsProcessTrusted() } {
        return;
    }
    if let Some(mut ax) = SystemAx::new(0.25) {
        restore_with(&mut ax, list);
    }
}

// SAFETY: the system-wide AXUIElement may be messaged from any thread; the probe is
// used by one thread at a time (it is moved into the recorder's poll thread).
unsafe impl Send for FocusProbe {}

struct Names {
    focused: Retained<AnyObject>,
    role: Retained<AnyObject>,
    subrole: Retained<AnyObject>,
    position: Retained<AnyObject>,
    size: Retained<AnyObject>,
    window: Retained<AnyObject>,
    selected_range: Retained<AnyObject>,
    bounds_for_range: Retained<AnyObject>,
}

impl Names {
    fn new() -> Result<Self, CaptureError> {
        Ok(Self {
            focused: ns_string("AXFocusedUIElement")?,
            role: ns_string("AXRole")?,
            subrole: ns_string("AXSubrole")?,
            position: ns_string("AXPosition")?,
            size: ns_string("AXSize")?,
            window: ns_string("AXWindow")?,
            selected_range: ns_string("AXSelectedTextRange")?,
            bounds_for_range: ns_string("AXBoundsForRange")?,
        })
    }
}

fn copy_attr(el: *mut c_void, attr: &AnyObject) -> Option<Cf> {
    let mut v: *mut c_void = std::ptr::null_mut();
    // SAFETY: a live AXUIElement and an attribute-name CFString (toll-free NSString);
    // on success `v` is a +1 CF object.
    let err = unsafe { AXUIElementCopyAttributeValue(el, attr, &mut v) };
    (err == 0 && !v.is_null()).then_some(Cf(v))
}

fn ax_value<T: Default>(v: &Cf, kind: u32) -> Option<T> {
    // SAFETY: a live CF object; only AXValues are read.
    if unsafe { CFGetTypeID(v.0) != AXValueGetTypeID() } {
        return None;
    }
    let mut out = T::default();
    // SAFETY: `out` has the layout of the AXValueType asked for (CGPoint, CGSize,
    // CGRect, CFRange), and AXValueGetValue checks the stored type.
    let ok = unsafe { AXValueGetValue(v.0, kind, (&mut out as *mut T).cast()) };
    ok.then_some(out)
}

fn string_attr(el: *mut c_void, attr: &AnyObject) -> String {
    copy_attr(el, attr)
        // SAFETY: a CFString (toll-free NSString) attribute value.
        .and_then(|v| rust_string(Some(unsafe { &*(v.0 as *const AnyObject) })))
        .unwrap_or_default()
}

fn frame(el: *mut c_void, n: &Names) -> Option<[f64; 4]> {
    let p: CGPoint = ax_value(&copy_attr(el, &n.position)?, AX_POINT)?;
    let s: CGSize = ax_value(&copy_attr(el, &n.size)?, AX_SIZE)?;
    Some([p.x, p.y, s.width, s.height])
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CfRange {
    location: isize,
    length: isize,
}

fn caret(el: *mut c_void, n: &Names) -> Option<[f64; 4]> {
    let range = copy_attr(el, &n.selected_range)?;
    let _: CfRange = ax_value(&range, AX_RANGE)?;
    let mut v: *mut c_void = std::ptr::null_mut();
    // SAFETY: a live element, the parameterized attribute's name and the AXValue
    // range as its parameter; `v` is +1 on success.
    let err = unsafe {
        AXUIElementCopyParameterizedAttributeValue(el, &*n.bounds_for_range, range.0, &mut v)
    };
    if err != 0 || v.is_null() {
        return None;
    }
    let r: CGRect = ax_value(&Cf(v), AX_RECT)?;
    (r.size.height > 0.0).then(|| [r.origin.x, r.origin.y, r.size.width.max(1.0), r.size.height])
}

impl FocusProbe {
    /// None without the Accessibility permission.
    pub fn new() -> Option<Self> {
        // SAFETY: no arguments.
        if !unsafe { AXIsProcessTrusted() } {
            return None;
        }
        // SAFETY: returns a +1 element owned by `Cf`.
        let system = Cf(unsafe { AXUIElementCreateSystemWide() });
        if system.0.is_null() {
            return None;
        }
        // An app that does not answer within 50 ms is skipped for this poll.
        // SAFETY: a live element.
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.05) };
        Some(Self {
            system,
            names: Names::new().ok()?,
            ax: SystemAx::new(0.05),
            switches: Switches::default(),
        })
    }

    fn wake(&mut self, pid: i32, bundle: &str) {
        if let Some(ax) = self.ax.as_mut() {
            self.switches.wake(ax, pid, bundle);
        }
    }

    /// The switches turned on so far and not turned off yet.
    pub fn switched(&self) -> &[Switched] {
        &self.switches.on
    }

    /// Turns off the switches turned on so far (dropping the probe does it too). Apps
    /// get longer to answer than a poll gives them.
    pub fn restore(&mut self) {
        if self.switches.on.is_empty() {
            return;
        }
        if let Some(mut ax) = SystemAx::new(0.25) {
            self.switches.restore(&mut ax);
        }
    }

    /// The focused element now (None when nothing is, or the app did not answer).
    pub fn focused(&mut self) -> Option<Focused> {
        let el = copy_attr(self.system.0, &self.names.focused)?;
        let mut pid = 0i32;
        // SAFETY: a live element and an out-pointer.
        unsafe { AXUIElementGetPid(el.0, &mut pid) };
        let app = bundle_of(pid);
        self.wake(pid, &app);
        let n = &self.names;
        let role = string_attr(el.0, &n.role);
        let subrole = string_attr(el.0, &n.subrole);
        let secure = subrole == "AXSecureTextField";
        let window = copy_attr(el.0, &n.window).and_then(|w| frame(w.0, n));
        Some(Focused {
            rect: frame(el.0, n),
            caret: if secure { None } else { caret(el.0, n) },
            window,
            role,
            subrole,
            app,
            pid,
            secure,
        })
    }
}

/// An app's bundle id, by pid.
fn bundle_of(pid: i32) -> String {
    let Ok(c) = class("NSRunningApplication") else {
        return String::new();
    };
    // SAFETY: returns the running app or nil; -bundleIdentifier is an NSString or nil.
    unsafe {
        let a: Option<Retained<AnyObject>> =
            msg_send![c, runningApplicationWithProcessIdentifier: pid];
        let Some(a) = a else { return String::new() };
        let b: Option<Retained<AnyObject>> = msg_send![&*a, bundleIdentifier];
        rust_string(b.as_deref()).unwrap_or_default()
    }
}

impl Frontmost {
    /// The frontmost app now.
    pub fn now() -> Option<Self> {
        let c = class("NSWorkspace").ok()?;
        // SAFETY: +sharedWorkspace, -frontmostApplication and its NSString properties.
        unsafe {
            let ws: Option<Retained<AnyObject>> = msg_send![c, sharedWorkspace];
            let a: Option<Retained<AnyObject>> = msg_send![&*ws?, frontmostApplication];
            let a = a?;
            let b: Option<Retained<AnyObject>> = msg_send![&*a, bundleIdentifier];
            let n: Option<Retained<AnyObject>> = msg_send![&*a, localizedName];
            let pid: i32 = msg_send![&*a, processIdentifier];
            Some(Self {
                bundle: rust_string(b.as_deref()).unwrap_or_default(),
                name: rust_string(n.as_deref()).unwrap_or_default(),
                pid,
            })
        }
    }
}

/// A window's bounds `[x, y, w, h]` in global points, if it is still there.
pub fn window_bounds(id: u32) -> Option<[f64; 4]> {
    // SAFETY: returns a +1 CFArray (toll-free NSArray) of window dictionaries, or NULL.
    let list = unsafe { CGWindowListCopyWindowInfo(INCLUDING_WINDOW, id) };
    if list.is_null() {
        return None;
    }
    let owned = Cf(list.cast());
    let key = ns_string("kCGWindowBounds").ok()?;
    // SAFETY: the array is live while `owned` is.
    let items = ns_array_items(unsafe { &*(owned.0 as *const AnyObject) });
    let first = items.first()?;
    // SAFETY: -objectForKey: on a window dictionary; a bounds dictionary or nil.
    let d: Option<Retained<AnyObject>> = unsafe { msg_send![&**first, objectForKey: &*key] };
    let mut r = CGRect::default();
    // SAFETY: a CFDictionary from CGWindowListCopyWindowInfo and an out-rect.
    let ok = unsafe { CGRectMakeWithDictionaryRepresentation(&*d?, &mut r) };
    ok.then_some([r.origin.x, r.origin.y, r.size.width, r.size.height])
}

/// The system cursor's image.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorShape {
    /// Changes when the image changes (a hash of its bytes).
    pub key: u64,
    /// The largest representation, as PNG.
    pub png: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// The hotspot in the PNG's pixels.
    pub hot_x: f64,
    pub hot_y: f64,
    /// The image's size in points.
    pub pt_w: f64,
    pub pt_h: f64,
}

fn ns_data(d: &AnyObject) -> Vec<u8> {
    // SAFETY: -bytes / -length on an NSData; the bytes are copied out at once.
    unsafe {
        let len: usize = msg_send![d, length];
        let p: *const c_void = msg_send![d, bytes];
        if p.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p.cast::<u8>(), len).to_vec()
        }
    }
}

/// The system cursor now (of whatever app is under it). `last` is the previous key:
/// when the image is unchanged only the key is returned (`png` empty), so a poll does
/// not encode a PNG.
pub fn cursor_shape(last: Option<u64>) -> Option<CursorShape> {
    let c = class("NSCursor").ok()?;
    // SAFETY: +currentSystemCursor returns the cursor or nil; -image is an NSImage,
    // -hotSpot an NSPoint, -size an NSSize, -TIFFRepresentation an NSData.
    let (image, hot, size, tiff) = unsafe {
        let cur: Option<Retained<AnyObject>> = msg_send![c, currentSystemCursor];
        let cur = cur?;
        let image: Option<Retained<AnyObject>> = msg_send![&*cur, image];
        let image = image?;
        let hot: CGPoint = msg_send![&*cur, hotSpot];
        let size: CGSize = msg_send![&*image, size];
        let tiff: Option<Retained<AnyObject>> = msg_send![&*image, TIFFRepresentation];
        (image, hot, size, tiff?)
    };
    drop(image);
    let bytes = ns_data(&tiff);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    (hot.x.to_bits(), hot.y.to_bits()).hash(&mut h);
    let key = h.finish();
    let mut shape = CursorShape {
        key,
        png: Vec::new(),
        w: 0,
        h: 0,
        hot_x: hot.x,
        hot_y: hot.y,
        pt_w: size.width,
        pt_h: size.height,
    };
    if last == Some(key) {
        return Some(shape);
    }
    let rc = class("NSBitmapImageRep").ok()?;
    // SAFETY: +imageRepsWithData: returns an NSArray of NSBitmapImageRep (or nil).
    let reps: Option<Retained<AnyObject>> = unsafe { msg_send![rc, imageRepsWithData: &*tiff] };
    let reps = ns_array_items(&*reps?);
    let best = reps.iter().max_by_key(|r| {
        // SAFETY: -pixelsWide (NSInteger) on an image rep.
        let w: isize = unsafe { msg_send![&***r, pixelsWide] };
        w
    })?;
    // SAFETY: -pixelsWide/-pixelsHigh on the rep; -representationUsingType:properties:
    // with PNG and an empty dictionary returns NSData or nil.
    let (w, hh, png) = unsafe {
        let w: isize = msg_send![&**best, pixelsWide];
        let hh: isize = msg_send![&**best, pixelsHigh];
        let props = super::ns_dict(&[]).ok()?;
        let png: Option<Retained<AnyObject>> =
            msg_send![&**best, representationUsingType: PNG, properties: &*props];
        (w, hh, png?)
    };
    if w <= 0 || hh <= 0 || size.width <= 0.0 {
        return None;
    }
    let k = w as f64 / size.width;
    shape.png = ns_data(&png);
    shape.w = w as u32;
    shape.h = hh as u32;
    shape.hot_x = hot.x * k;
    shape.hot_y = hot.y * k;
    Some(shape)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use AxSwitch::{Enhanced, Manual};

    /// Apps by pid: the bundle id and each attribute's value (absent: the app does not
    /// say), and every set asked for.
    #[derive(Default)]
    struct Fake {
        apps: HashMap<i32, (String, HashMap<AxSwitch, bool>)>,
        voice_over: bool,
        sets: Vec<(i32, AxSwitch, bool)>,
    }

    impl Fake {
        fn app(mut self, pid: i32, bundle: &str, attrs: &[(AxSwitch, bool)]) -> Self {
            self.apps
                .insert(pid, (bundle.into(), attrs.iter().copied().collect()));
            self
        }
    }

    impl AppAx for Fake {
        fn get(&mut self, pid: i32, attr: AxSwitch) -> Option<bool> {
            self.apps.get(&pid)?.1.get(&attr).copied()
        }
        fn set(&mut self, pid: i32, attr: AxSwitch, on: bool) {
            self.sets.push((pid, attr, on));
            if let Some(a) = self.apps.get_mut(&pid) {
                a.1.insert(attr, on);
            }
        }
        fn bundle(&mut self, pid: i32) -> String {
            self.apps.get(&pid).map(|a| a.0.clone()).unwrap_or_default()
        }
        fn voice_over(&mut self) -> bool {
            self.voice_over
        }
    }

    #[test]
    fn what_was_turned_on_is_turned_off_and_nothing_else() {
        let mut ax = Fake::default()
            .app(10, "com.google.Chrome", &[(Manual, false)])
            .app(20, "com.tinyspeck.slackmacgap", &[(Manual, true)])
            .app(30, "com.apple.finder", &[]);
        let mut sw = Switches::default();
        for _ in 0..3 {
            sw.wake(&mut ax, 10, "com.google.Chrome");
            sw.wake(&mut ax, 20, "com.tinyspeck.slackmacgap");
            sw.wake(&mut ax, 30, "com.apple.finder");
        }
        // Each app once; Slack's was on already; Finder does not say, so it is noted.
        assert_eq!(
            ax.sets,
            [(10, Manual, true), (10, Enhanced, true), (30, Manual, true)]
        );
        assert_eq!(
            sw.on.iter().map(|s| (s.pid, s.attr)).collect::<Vec<_>>(),
            [(10, Manual), (10, Enhanced), (30, Manual)]
        );
        ax.sets.clear();
        sw.restore(&mut ax);
        assert_eq!(
            ax.sets,
            [
                (10, Manual, false),
                (10, Enhanced, false),
                (30, Manual, false)
            ]
        );
        assert!(!ax.apps[&10].1[&Enhanced]);
        assert!(ax.apps[&20].1[&Manual], "Slack keeps its own switch");
        assert!(sw.on.is_empty());
        // Restored twice (stop, then drop): nothing more.
        ax.sets.clear();
        sw.restore(&mut ax);
        assert!(ax.sets.is_empty());
    }

    #[test]
    fn an_app_that_quit_or_a_reused_pid_is_left_alone() {
        let mut ax =
            Fake::default()
                .app(10, "com.google.Chrome", &[])
                .app(11, "com.brave.Browser", &[]);
        let mut sw = Switches::default();
        sw.wake(&mut ax, 10, "com.google.Chrome");
        sw.wake(&mut ax, 11, "com.brave.Browser");
        // Chrome quit and its pid is another app now; Brave quit.
        ax.apps
            .insert(10, ("com.apple.Terminal".into(), HashMap::new()));
        ax.apps.remove(&11);
        ax.sets.clear();
        sw.restore(&mut ax);
        assert!(ax.sets.is_empty());
    }

    #[test]
    fn voice_over_keeps_every_switch() {
        let mut ax = Fake::default().app(10, "com.google.Chrome", &[]);
        let mut sw = Switches::default();
        sw.wake(&mut ax, 10, "com.google.Chrome");
        ax.voice_over = true;
        ax.sets.clear();
        sw.restore(&mut ax);
        assert!(ax.sets.is_empty());
    }

    #[test]
    fn a_saved_list_is_restored_by_pid_and_bundle() {
        let mut ax = Fake::default().app(10, "com.google.Chrome", &[(Enhanced, true)]);
        let list = [
            Switched {
                pid: 10,
                bundle: "com.google.Chrome".into(),
                attr: Enhanced,
            },
            Switched {
                pid: 99,
                bundle: "com.google.Chrome".into(),
                attr: Manual,
            },
        ];
        restore_with(&mut ax, &list);
        assert_eq!(ax.sets, [(10, Enhanced, false)]);
        assert_eq!(AxSwitch::parse(Enhanced.as_str()), Some(Enhanced));
        assert_eq!(AxSwitch::parse("AXRole"), None);
    }
}
