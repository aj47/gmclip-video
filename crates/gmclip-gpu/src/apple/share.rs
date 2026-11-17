//! Share… and drag-out of a finished export (SPEC §5.14: every export tile has Reveal in
//! Finder, Share… — the macOS share sheet with AirDrop, Messages, Save to Photos, Mail —
//! and drag-out). AppKit is reached through the Objective-C runtime (`msg_send!` on
//! classes looked up by name, as `vision.rs` does), so no binding crate is added. Every
//! function here must run on the main thread and says so with an error otherwise; the
//! app's window is the key (or main) window's content view.

use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::{msg_send, sel};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// Why a share or drag did not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShareError {
    #[error("not on the main thread")]
    NotMain,
    #[error("no window to show it in")]
    NoWindow,
    #[error("AppKit is missing {0}")]
    Missing(&'static str),
    #[error("no file to share")]
    NoFile,
    #[error("no mouse event to start a drag from")]
    NoEvent,
}

fn class(name: &'static str) -> Result<&'static AnyClass, ShareError> {
    AnyClass::get(&std::ffi::CString::new(name).map_err(|_| ShareError::Missing(name))?)
        .ok_or(ShareError::Missing(name))
}

fn is_main() -> bool {
    let Ok(c) = class("NSThread") else {
        return false;
    };
    // SAFETY: +[NSThread isMainThread] takes no arguments and returns a BOOL.
    let main: Bool = unsafe { msg_send![c, isMainThread] };
    main.as_bool()
}

/// `file://` URLs for the paths, as an `NSArray` (autoreleased objects retained here).
fn urls(paths: &[&Path]) -> Result<Retained<AnyObject>, ShareError> {
    let ns_string = class("NSString")?;
    let ns_url = class("NSURL")?;
    let ns_mutable_array = class("NSMutableArray")?;
    // SAFETY: +[NSMutableArray array] returns an autoreleased, empty array.
    let arr: Option<Retained<AnyObject>> = unsafe { msg_send![ns_mutable_array, array] };
    let arr = arr.ok_or(ShareError::Missing("NSMutableArray"))?;
    for p in paths {
        let s = p.to_str().ok_or(ShareError::NoFile)?;
        let c = std::ffi::CString::new(s).map_err(|_| ShareError::NoFile)?;
        // SAFETY: a NUL-terminated UTF-8 string; returns an autoreleased NSString or nil.
        let ns: Option<Retained<AnyObject>> =
            unsafe { msg_send![ns_string, stringWithUTF8String: c.as_ptr()] };
        let ns = ns.ok_or(ShareError::NoFile)?;
        // SAFETY: an NSString path; returns an autoreleased file URL or nil.
        let url: Option<Retained<AnyObject>> = unsafe { msg_send![ns_url, fileURLWithPath: &*ns] };
        let url = url.ok_or(ShareError::NoFile)?;
        // SAFETY: adds a non-nil object to the mutable array (retained by it).
        let () = unsafe { msg_send![&*arr, addObject: &*url] };
    }
    Ok(arr)
}

/// The app window's content view and its height (to flip page coordinates).
fn content_view() -> Result<(Retained<AnyObject>, f64, bool), ShareError> {
    let app_class = class("NSApplication")?;
    // SAFETY: +sharedApplication on the main thread (checked by the callers).
    let app: Option<Retained<AnyObject>> = unsafe { msg_send![app_class, sharedApplication] };
    let app = app.ok_or(ShareError::NoWindow)?;
    // SAFETY: -keyWindow / -mainWindow return the app's windows or nil.
    let mut win: Option<Retained<AnyObject>> = unsafe { msg_send![&*app, keyWindow] };
    if win.is_none() {
        // SAFETY: as above.
        win = unsafe { msg_send![&*app, mainWindow] };
    }
    let win = win.ok_or(ShareError::NoWindow)?;
    // SAFETY: -[NSWindow contentView] returns the view or nil.
    let view: Option<Retained<AnyObject>> = unsafe { msg_send![&*win, contentView] };
    let view = view.ok_or(ShareError::NoWindow)?;
    // SAFETY: -[NSView frame] returns an NSRect (CGRect) by value; -isFlipped a BOOL.
    let frame: CGRect = unsafe { msg_send![&*view, frame] };
    // SAFETY: as above.
    let flipped: Bool = unsafe { msg_send![&*view, isFlipped] };
    Ok((view, frame.size.height, flipped.as_bool()))
}

thread_local! {
    /// The picker on screen (kept alive while AppKit shows it).
    static PICKER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Shows the share sheet for `paths` at `(x, y)` (page points from the window's top
/// left), the way the Finder's Share button does: AirDrop, Messages, Mail, Save to Photos
/// and the rest of the user's sharing services.
pub fn share(paths: &[&Path], x: f64, y: f64) -> Result<(), ShareError> {
    if !is_main() {
        return Err(ShareError::NotMain);
    }
    if paths.is_empty() {
        return Err(ShareError::NoFile);
    }
    let items = urls(paths)?;
    let (view, h, flipped) = content_view()?;
    let picker_class = class("NSSharingServicePicker")?;
    // SAFETY: +alloc then -initWithItems: with an NSArray of NSURLs (NSURL conforms to
    // NSPasteboardWriting, which the initialiser requires of each item).
    let picker: Option<Retained<AnyObject>> = unsafe {
        let a: Allocated<AnyObject> = msg_send![picker_class, alloc];
        msg_send![a, initWithItems: &*items]
    };
    let picker = picker.ok_or(ShareError::Missing("NSSharingServicePicker"))?;
    let rect = CGRect::new(
        CGPoint::new(x, if flipped { y } else { h - y }),
        CGSize::new(1.0, 1.0),
    );
    // SAFETY: shows the picker anchored to a rect in the content view's coordinates;
    // NSRectEdge is an NSUInteger (1 = NSMinYEdge).
    let () = unsafe {
        msg_send![&*picker, showRelativeToRect: rect, ofView: &*view, preferredEdge: 1usize]
    };
    PICKER.with(|p| *p.borrow_mut() = Some(picker));
    Ok(())
}

/// The dragging source class (`NSDraggingSource`): copy outside the app, nothing inside.
fn drag_source_class() -> Result<&'static AnyClass, ShareError> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    extern "C-unwind" fn op_mask(
        _this: &AnyObject,
        _cmd: Sel,
        _session: *mut AnyObject,
        context: isize,
    ) -> usize {
        // NSDraggingContextOutsideApplication = 0 → NSDragOperationCopy (1); inside: none
        if context == 0 { 1 } else { 0 }
    }
    let c = CLASS.get_or_init(|| {
        let superclass = AnyClass::get(c"NSObject")?;
        let mut b = ClassBuilder::new(c"GmclipExportDragSource", superclass)?;
        if let Some(p) = AnyProtocol::get(c"NSDraggingSource") {
            b.add_protocol(p);
        }
        // SAFETY: the function's signature matches the selector's
        // (id self, SEL _cmd, NSDraggingSession *, NSDraggingContext) -> NSDragOperation.
        unsafe {
            b.add_method(
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                op_mask as extern "C-unwind" fn(_, _, _, _) -> _,
            );
        }
        Some(b.register())
    });
    c.ok_or(ShareError::Missing("a dragging source class"))
}

thread_local! {
    static SOURCE: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Starts dragging `path` out of the window (to the Finder, Mail, a chat…) from the mouse
/// event AppKit is handling now, with a `w × h` point frame under the pointer.
pub fn drag_out(path: &Path) -> Result<(), ShareError> {
    if !is_main() {
        return Err(ShareError::NotMain);
    }
    let (view, _, _) = content_view()?;
    let app_class = class("NSApplication")?;
    // SAFETY: +sharedApplication / -currentEvent on the main thread.
    let event: Option<Retained<AnyObject>> = unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        msg_send![app, currentEvent]
    };
    let event = event.ok_or(ShareError::NoEvent)?;
    let items = urls(&[path])?;
    // SAFETY: -[NSArray firstObject] of a one-URL array.
    let url: Option<Retained<AnyObject>> = unsafe { msg_send![&*items, firstObject] };
    let url = url.ok_or(ShareError::NoFile)?;
    let item_class = class("NSDraggingItem")?;
    // SAFETY: -initWithPasteboardWriter: with an NSURL (an NSPasteboardWriting).
    let item: Option<Retained<AnyObject>> = unsafe {
        let a: Allocated<AnyObject> = msg_send![item_class, alloc];
        msg_send![a, initWithPasteboardWriter: &*url]
    };
    let item = item.ok_or(ShareError::Missing("NSDraggingItem"))?;
    // SAFETY: -[NSEvent locationInWindow] (an NSPoint) and -[NSView convertPoint:fromView:]
    // with a nil view (window coordinates).
    let at: CGPoint = unsafe {
        let p: CGPoint = msg_send![&*event, locationInWindow];
        msg_send![&*view, convertPoint: p, fromView: std::ptr::null::<AnyObject>()]
    };
    let frame = CGRect::new(
        CGPoint::new(at.x - 24.0, at.y - 24.0),
        CGSize::new(48.0, 48.0),
    );
    // SAFETY: a dragging frame and no contents image (AppKit draws the file's icon).
    let () = unsafe {
        msg_send![&*item, setDraggingFrame: frame, contents: std::ptr::null::<AnyObject>()]
    };
    let ns_array = class("NSArray")?;
    // SAFETY: +arrayWithObject: with the non-nil item.
    let arr: Option<Retained<AnyObject>> = unsafe { msg_send![ns_array, arrayWithObject: &*item] };
    let arr = arr.ok_or(ShareError::Missing("NSArray"))?;
    let src_class = drag_source_class()?;
    // SAFETY: +new of our NSObject subclass.
    let source: Option<Retained<AnyObject>> = unsafe { msg_send![src_class, new] };
    let source = source.ok_or(ShareError::Missing("a dragging source"))?;
    // SAFETY: items, the current mouse event, and a source implementing
    // draggingSession:sourceOperationMaskForDraggingContext:.
    let session: Option<Retained<AnyObject>> = unsafe {
        msg_send![&*view, beginDraggingSessionWithItems: &*arr, event: &*event, source: &*source]
    };
    session.ok_or(ShareError::NoEvent)?;
    SOURCE.with(|s| *s.borrow_mut() = Some(source));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_the_main_thread_nothing_is_shown() {
        // the test harness runs tests on worker threads
        let p = Path::new("/tmp/x.mp4");
        let r = std::thread::spawn(move || share(&[p], 0.0, 0.0))
            .join()
            .unwrap();
        assert_eq!(r, Err(ShareError::NotMain));
        let r = std::thread::spawn(move || drag_out(p)).join().unwrap();
        assert_eq!(r, Err(ShareError::NotMain));
    }

    #[test]
    fn the_items_and_the_source_class_build() {
        let p = Path::new("/tmp/a b.mp4");
        let arr = urls(&[p, Path::new("/tmp/c.mov")]).unwrap();
        // SAFETY: -count of an NSArray.
        let n: usize = unsafe { msg_send![&*arr, count] };
        assert_eq!(n, 2);
        let c = drag_source_class().unwrap();
        // SAFETY: +instancesRespondToSelector: with a selector.
        let ok: Bool = unsafe {
            msg_send![c, instancesRespondToSelector: sel!(draggingSession:sourceOperationMaskForDraggingContext:)]
        };
        assert!(ok.as_bool());
        assert!(class("NSSharingServicePicker").is_ok());
    }
}
