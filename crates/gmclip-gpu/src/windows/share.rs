//! Dragging an export out of the window (Windows: OLE drag and drop), the twin of
//! `apple::share::drag_out`. The shell makes the file's data object (what Explorer, mail
//! and chat apps read: the file, its shell item and name), gmclip only answers the drag's
//! questions (`IDropSource`): copy, the system's cursors, and the drag ends when the left
//! button is released (drop) or Esc is pressed (cancel).
//!
//! Called on the window's thread (single-threaded apartment, OLE initialised by the
//! webview's host) while the mouse button that started the drag is still down;
//! `DoDragDrop` runs the drag's modal loop and returns when it ends.

use std::path::Path;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName};
use windows::core::{BOOL, HRESULT, HSTRING, implement};

use super::mf::os;
use crate::GpuError;

/// The drag's source: gmclip offers a copy and lets the system draw the cursors.
#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drags `file` out of the window as a copy; `true` when it was dropped somewhere.
pub fn drag_out(file: &Path) -> Result<bool, GpuError> {
    // the shell parses only plain backslash paths (`C:\x\y`): no `/`, no verbatim `\\?\`
    let plain = file.to_string_lossy().replace('/', "\\");
    let plain = plain
        .strip_prefix(r"\\?\")
        .filter(|r| r.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&plain);
    let path = HSTRING::from(plain);
    // SAFETY: `path` outlives the calls; the shell item and its data object are released
    // by their wrappers; the source is a COM object made here and kept alive by the
    // reference `DoDragDrop` holds for the drag; `effect` is a local the call fills.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&path, None)
            .map_err(|e| os("SHCreateItemFromParsingName", &e))?;
        let data: IDataObject = item
            .BindToHandler(None, &BHID_DataObject)
            .map_err(|e| os("IShellItem::BindToHandler(data object)", &e))?;
        let source: IDropSource = Source.into();
        let mut effect = DROPEFFECT(0);
        let hr = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        if hr == DRAGDROP_S_DROP {
            Ok(effect.0 != 0)
        } else if hr == DRAGDROP_S_CANCEL {
            Ok(false)
        } else {
            Err(GpuError::Os {
                step: "DoDragDrop",
                status: i64::from(hr.0),
            })
        }
    }
}
