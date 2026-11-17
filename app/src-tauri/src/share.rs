//! Share… and drag-out on an export tile (SPEC §5.14). Only a file the export list names
//! is ever shared or dragged (never a path from the page); AppKit runs on the main thread
//! through the runner the window installs (`gmclip_gpu::apple::share`).

use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// Runs a closure on the main thread; false when there is no window to run it in.
pub type MainRunner = Box<dyn Fn(Box<dyn FnOnce() + Send>) -> bool + Send + Sync>;

static RUNNER: OnceLock<MainRunner> = OnceLock::new();

/// The window installs its main-thread runner once.
pub fn install(r: MainRunner) {
    let _ = RUNNER.set(r);
}

fn export_file(core: &AppCore, project: &str, export: &str) -> AppResult<PathBuf> {
    let e = core
        .exports(project)?
        .into_iter()
        .find(|e| e.id == export)
        .ok_or_else(|| AppError::not_found("no such export"))?;
    if !e.file.is_file() {
        return Err(AppError::not_found(
            "the exported file has moved or been deleted",
        ));
    }
    Ok(e.file)
}

fn on_main(f: impl FnOnce() -> Result<(), String> + Send + 'static) -> AppResult<Value> {
    let run = RUNNER
        .get()
        .ok_or_else(|| AppError::new("Unsupported", "no window to show it in"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if !run(Box::new(move || {
        let _ = tx.send(f());
    })) {
        return Err(AppError::new("Unsupported", "no window to show it in"));
    }
    rx.recv()
        .map_err(|_| AppError::internal("the window went away"))?
        .map_err(|e| AppError::new("Unsupported", e))?;
    Ok(json!({}))
}

/// The macOS share sheet for an export, anchored at page point `(x, y)`.
pub fn share(core: &AppCore, project: &str, export: &str, x: f64, y: f64) -> AppResult<Value> {
    let file = export_file(core, project, export)?;
    if !(x.is_finite() && y.is_finite()) {
        return Err(AppError::invalid("a point in the window"));
    }
    on_main(move || platform::share(&file, x, y))
}

/// Starts dragging an export out of the window from the current mouse drag.
pub fn drag(core: &AppCore, project: &str, export: &str) -> AppResult<Value> {
    let file = export_file(core, project, export)?;
    on_main(move || platform::drag(&file))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;

    pub fn share(file: &Path, x: f64, y: f64) -> Result<(), String> {
        gmclip_gpu::apple::share::share(&[file], x, y).map_err(|e| e.to_string())
    }

    pub fn drag(file: &Path) -> Result<(), String> {
        gmclip_gpu::apple::share::drag_out(file).map_err(|e| e.to_string())
    }
}

/// Windows: drag-out through OLE drag and drop; no share sheet yet.
#[cfg(windows)]
mod platform {
    use std::path::Path;

    pub fn share(_file: &Path, _x: f64, _y: f64) -> Result<(), String> {
        Err("Share is macOS-only for now: drag the export out instead".into())
    }

    pub fn drag(file: &Path) -> Result<(), String> {
        gmclip_gpu::windows::share::drag_out(file)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::path::Path;

    pub fn share(_file: &Path, _x: f64, _y: f64) -> Result<(), String> {
        Err("Share is macOS-only for now".into())
    }

    pub fn drag(_file: &Path) -> Result<(), String> {
        Err("drag-out runs on macOS and Windows".into())
    }
}
