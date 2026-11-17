//! `cargo xt assert-no-dev-bridge <binary or .app>` (R-DEV-1, backlog U.19): fails when a
//! release artifact carries the dev bridge (the token-protected loopback WebSocket that
//! drives the UI in tests) or the agent stub model that only dev-bridge builds start.
//! It looks for strings only the Rust side of those features compiles in (the web UI's
//! bridge client is inert without a bridge to talk to and is not checked).

use std::path::{Path, PathBuf};

/// Strings only `devbridge.rs` and the dev-bridge agent stub put in the binary. (The
/// stub model's own text is not one since 2026-10-15: release builds carry it for the S7
/// probe the app runs on a new Claude Code build, which only ever talks to its own
/// loopback stub. What stays dev-only is pointing an *agent run* at a stub, which the
/// `GMCLIP_AGENT_STUB_TURNS` hook does.)
const MARKERS: &[&str] = &[
    "dev bridge refused a connection",
    "GMCLIP_AGENT_STUB_TURNS",
    "GMCLIP_TEST_CLIENT_IS_NEW",
    "dev_frame_cycle",
    "dev_review_add",
    "dev_review_link",
    "dev_approval_add",
    "dev_batch_start",
    "dev_agent_burst",
    "dev_stub_turns",
];

fn binary_of(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|e| e == "app") {
        p.join("Contents/MacOS/gmclip-app")
    } else {
        p.to_path_buf()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let p = args
        .first()
        .map(PathBuf::from)
        .ok_or("usage: cargo xt assert-no-dev-bridge <binary or .app>")?;
    let bin = binary_of(&p);
    let bytes = std::fs::read(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let found: Vec<&str> = MARKERS
        .iter()
        .copied()
        .filter(|m| find(&bytes, m.as_bytes()))
        .collect();
    if found.is_empty() {
        println!("no dev bridge in {} ({} bytes)", bin.display(), bytes.len());
        Ok(())
    } else {
        Err(format!(
            "{} carries the dev bridge: {found:?}",
            bin.display()
        ))
    }
}
