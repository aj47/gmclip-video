//! Project fixtures as library projects (tests, and the e2e runner through
//! `gmclip-app lab-project`): a committed `gmclip.project-fixture/0` file
//! ([`gmclip_core::fixture`]) becomes a project home in the library with its database in
//! the app data, its sources resolved against the lab media folder
//! ([`gmclip_model::lab::root`]).
#![doc(hidden)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::core::{AppCore, safe_name};
use crate::error::{AppError, AppResult};

/// Writes the fixture at `path` as a new project home in the library; returns the home
/// and the fixture's `summary`.
pub fn create(core: &AppCore, path: &Path) -> AppResult<(PathBuf, Value)> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::not_found(format!("no fixture there ({})", e.kind())))?;
    let fx: Value =
        serde_json::from_slice(&bytes).map_err(|_| AppError::invalid("the fixture is not JSON"))?;
    let name = fx["name"].as_str().unwrap_or("fixture").to_owned();
    std::fs::create_dir_all(&core.dirs.library)
        .map_err(|e| AppError::internal(format!("library: {}", e.kind())))?;
    let stem = safe_name(&name);
    let mut home = core.dirs.library.join(format!("{stem}.gmclip"));
    let mut n = 2;
    while home.exists() {
        home = core.dirs.library.join(format!("{stem} {n}.gmclip"));
        n += 1;
    }
    let info = gmclip_mcp::project::create_home(&core.dirs.app_data, &home, &name)
        .map_err(|e| AppError::internal(format!("creating the project: {}", e.kind())))?;
    let db = gmclip_mcp::project::db_path(&core.dirs.app_data, &info.id).map_err(AppError::from)?;
    gmclip_core::fixture::write_db(&fx, &gmclip_model::lab::root(), &db)
        .map_err(|e| AppError::invalid(format!("the fixture did not load: {e}")))?;
    Ok((home, fx["summary"].clone()))
}

/// [`create`], then opens the project; its `fixture` field is the fixture's `summary`.
pub fn open(core: &Arc<AppCore>, path: &Path) -> AppResult<Value> {
    let (home, summary) = create(core, path)?;
    let mut p = core.open_home(&home)?;
    p["fixture"] = summary;
    Ok(p)
}

/// [`open`] on the frozen stream project `name` ([`gmclip_model::lab::project`]).
pub fn open_project(core: &Arc<AppCore>, name: &str) -> AppResult<Value> {
    open(core, &gmclip_model::lab::project(name))
}

/// A copy of the RenderPlan at `path` in `dir` (same file name) with its relative file
/// paths resolved against the lab media folder, for code that imports a plan from a file.
pub fn plan_copy(path: &Path, dir: &Path) -> AppResult<PathBuf> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::not_found(format!("no plan there ({})", e.kind())))?;
    let mut plan = gmclip_plan::parse(&bytes)
        .map_err(|e| AppError::invalid(format!("not a RenderPlan: {}", e.reason())))?;
    plan.resolve_paths(&gmclip_model::lab::root());
    let mut text =
        serde_json::to_string_pretty(&plan).map_err(|e| AppError::internal(e.to_string()))?;
    text.push('\n');
    std::fs::create_dir_all(dir).map_err(|e| AppError::internal(e.kind().to_string()))?;
    let out = dir.join(path.file_name().unwrap_or_default());
    std::fs::write(&out, text).map_err(|e| AppError::internal(e.kind().to_string()))?;
    Ok(out)
}
