//! The app-shell commands of "Ideas 2026-10-16 (library and layout)" (lane K4), routed
//! from `ipc` in one place: draw your layout once, the term list and "Fix everywhere",
//! compare long-forms, B-roll from your own library and reaction moments. Library search
//! by topic is in `libsearch` (`library_topic_open`).

use serde_json::Value;

use crate::error::{AppError, AppResult};

/// Every command this module answers (each is also in `ipc::APP_COMMANDS`).
pub const COMMANDS: &[&str] = &[
    "layout_draw_view",
    "layout_draw_frame",
    "layout_draw_save",
    "term_fix_everywhere",
    "terms_view",
    "longform_compare_view",
    "broll_suggest",
    "broll_add",
    "reactions_find",
];

pub fn ipc(ctx: &crate::ipc::Ctx, command: &str, project: &str, a: &Value) -> AppResult<Value> {
    let core = &ctx.core;
    let s = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
    };
    match command {
        "layout_draw_view" | "layout_draw_frame" | "layout_draw_save" => {
            crate::drawlayout::ipc(core, command, project, a)
        }
        "term_fix_everywhere" | "terms_view" => crate::termlist::ipc(core, command, project, a),
        "longform_compare_view" => crate::longcompare::view(core, project, s("a")?, s("b")?),
        "broll_suggest" | "broll_add" => crate::broll::ipc(core, command, project, a),
        "reactions_find" => crate::reactions::find_moments(core, project),
        _ => Err(AppError::invalid("unknown command")),
    }
}
