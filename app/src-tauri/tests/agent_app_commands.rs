//! The app commands agents may run (`agentapp`, AJ 2026-10-18): each is one of the app's
//! IPC commands, none is shadowed by a registry or holder command of the same name, and
//! none is an Owner action, a human-only or agent-denied command, or agent, session,
//! review-link, feedback, Clippy, owner or viewer plumbing.

use std::collections::BTreeSet;

use gmclip_app::agentapp::COMMANDS;
use gmclip_app::ipc::APP_COMMANDS;

#[test]
fn the_agent_list_is_app_commands_only_and_none_is_the_humans() {
    let mut seen = BTreeSet::new();
    let mut bad = vec![];
    let mut check = |ok: bool, why: String| {
        if !ok {
            bad.push(why);
        }
    };
    for (name, doc) in COMMANDS {
        check(seen.insert(*name), format!("{name} twice"));
        check(!doc.is_empty(), format!("{name} has no doc"));
        check(
            APP_COMMANDS.contains(name),
            format!("{name} is not an app command"),
        );
        check(
            gmclip_core::registry::find(name).is_none(),
            format!("{name} is a registry command (that one wins)"),
        );
        check(
            !gmclip_mcp::tools::HOLDER_COMMANDS.contains(name),
            format!("{name} is a holder command (that one wins)"),
        );
        check(
            !gmclip_mcp::holder::OWNER_COMMANDS.contains(name),
            format!("{name} is an Owner action"),
        );
        check(
            !gmclip_mcp::tools::AGENT_DENY.contains(name),
            format!("{name} is denied to agents"),
        );
        check(
            !gmclip_mcp::tools::human_only(name),
            format!("{name} is human-only"),
        );
        for p in [
            "session_",
            "agent_",
            "agents_",
            "viewer_",
            "owner_",
            "review_",
            "clippy_",
            "feedback_",
            "approval_",
            "setup_",
            "ui_prefs",
            "app_",
            "dev_",
        ] {
            check(
                !name.starts_with(p),
                format!("{name}: {p}* stays the human's"),
            );
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    for human in [
        "export_verdict",
        "golden_mark",
        "thumb_cover",
        "comments_fix",
        "backup_restore",
        "style_pick",
        "style_review",
        "files_dropped",
        "video_pick",
        "videos_pick",
        "export_reveal",
        "export_share",
        "export_drag",
        "batch_start",
    ] {
        assert!(!seen.contains(human), "{human} stays the human's");
    }
}
