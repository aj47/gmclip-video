//! Lane V's app-shell commands (decision 0015), routed from [`crate::ipc`]: comments,
//! review-link asks, golden cuts, thumbnails. Thin: every change to a cut is a registry
//! command, frame ↔ source conversions happen here so the UI does no model math (R-CMD-2).

use std::sync::Arc;

use serde_json::Value;

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// The commands routed here (each is also in `ipc::APP_COMMANDS`).
pub const COMMANDS: &[&str] = &[
    "comments_view",
    "comment_add_at",
    "comments_fix",
    "review_ask",
    "golden_view",
    "golden_mark",
    "golden_compare",
    "thumbs_view",
    "thumb_add_at",
    "thumb_cover",
    "thumb_render",
    "thumb_agent",
];

fn s<'a>(a: &'a Value, k: &str) -> AppResult<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
}

fn frame(a: &Value, k: &str) -> Option<u64> {
    a.get(k).and_then(Value::as_u64)
}

/// Runs one of [`COMMANDS`].
pub fn dispatch(
    core: &Arc<AppCore>,
    cmd: &str,
    project: Option<&str>,
    a: &Value,
) -> AppResult<Value> {
    let project = || -> AppResult<&str> {
        project
            .or_else(|| a.get("project").and_then(Value::as_str))
            .ok_or_else(|| AppError::invalid("`project` is required"))
    };
    match cmd {
        "comments_view" => crate::collab::view(core, project()?, s(a, "cut")?),
        "comment_add_at" => crate::collab::add_at(
            core,
            project()?,
            s(a, "cut")?,
            frame(a, "frame"),
            a.get("box"),
            s(a, "text")?,
            a.get("reply_to").and_then(Value::as_str),
        ),
        "comments_fix" => {
            let only = a.get("comments").and_then(Value::as_array).map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            });
            crate::collab::fix(
                core,
                project()?,
                s(a, "cut")?,
                only,
                a.get("harness").and_then(Value::as_str),
            )
        }
        "review_ask" => crate::collab::ask_act(
            core,
            s(a, "ask")?,
            a.get("start").and_then(Value::as_bool).unwrap_or(false),
        ),
        "golden_view" => crate::golden::view(core, project()?, s(a, "cut")?),
        "golden_mark" => crate::golden::mark(
            core,
            project()?,
            s(a, "cut")?,
            a.get("on").and_then(Value::as_bool).unwrap_or(true),
        ),
        "golden_compare" => crate::golden::compare(core, project()?, s(a, "cut")?),
        "thumbs_view" => crate::thumbs::view(core, project()?, s(a, "cut")?),
        "thumb_add_at" => crate::thumbs::add(
            core,
            project()?,
            s(a, "cut")?,
            frame(a, "frame").ok_or_else(|| AppError::invalid("`frame` is required"))?,
            a.get("title").and_then(Value::as_str).unwrap_or_default(),
            a.get("style"),
            a.get("pick").and_then(Value::as_bool).unwrap_or(false),
        ),
        "thumb_cover" => crate::thumbs::cover(
            core,
            project()?,
            s(a, "cut")?,
            s(a, "thumbnail")?,
            frame(a, "frame").ok_or_else(|| AppError::invalid("`frame` is required"))?,
        ),
        "thumb_render" => crate::thumbs::render(core, project()?, s(a, "thumbnail")?),
        "thumb_agent" => crate::thumbs::agent(
            core,
            project()?,
            s(a, "cut")?,
            frame(a, "frame"),
            a.get("harness").and_then(Value::as_str),
        ),
        _ => Err(AppError::invalid("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_command_is_declared_to_the_ui() {
        for c in super::COMMANDS {
            assert!(crate::ipc::APP_COMMANDS.contains(c), "{c}");
        }
    }
}
