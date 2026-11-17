//! The native Owner sheet (R-OWN-1b; backlog U.11): an NSAlert drawn by Rust — never
//! DOM — naming what the Owner command will do, from the core's own effect text. The
//! webview can ask for a sheet; only the button in it says yes.
//!
//! With no window (the dev bridge, tests) there is no sheet and every request is refused
//! ("no Owner command runs from the dev bridge").

use std::sync::OnceLock;

/// Shows a sheet and returns whether the human pressed the action button.
pub type SheetFn = Box<dyn Fn(String, String, String) -> bool + Send + Sync>;

static SHEET: OnceLock<SheetFn> = OnceLock::new();

/// Installs the sheet (the window's main thread runs it); once per launch.
pub fn install(f: SheetFn) {
    let _ = SHEET.set(f);
}

/// Asks the human; false with no window.
pub fn confirm(title: &str, text: &str, action: &str) -> bool {
    match SHEET.get() {
        Some(f) => f(title.to_owned(), text.to_owned(), action.to_owned()),
        None => false,
    }
}

/// The NSAlert itself (main thread only).
#[cfg(target_os = "macos")]
pub fn alert_on_main_thread(title: &str, text: &str, action: &str) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn};
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(text));
    a.addButtonWithTitle(&NSString::from_str(action));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    a.runModal() == NSAlertFirstButtonReturn
}

#[cfg(not(target_os = "macos"))]
pub fn alert_on_main_thread(_: &str, _: &str, _: &str) -> bool {
    false
}
