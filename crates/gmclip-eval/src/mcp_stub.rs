//! `gmclip-eval mcp-stub <record>`: a minimal MCP stdio server standing in for
//! `gmclip mcp` until lane A2's server is on main.
//!
//! It answers `initialize`, `tools/list` (the launcher's M1 surface) and `tools/call`
//! (a fixed text result), and appends to `<record>` one JSON line at start
//! (`{event:"start", env_keys, run_fingerprint}`) and one per message
//! (`{event:"msg", method, tool?}`). `run_fingerprint` is the same non-secret fingerprint
//! the launcher puts on its run record (sha256 of the credential bytes, first 16 hex), so
//! the probe can check that *this run's* credential arrived without anyone writing it
//! down.
//!
//! TODO(A2): the live end-to-end run replaces this with `gmclip mcp` (the launcher's
//! default `mcp_command`); the probe keeps the stub for the zero-spend S7 checks.

use std::io::{BufRead, Write};
use std::path::Path;

use gmclip_ai::launcher::{env::RUN_VAR, sha256, tools};
use serde_json::{Value, json};

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The fingerprint of a hex credential, as `RunCredential::fingerprint` computes it.
/// Accepts `GMCLIP_RUN`'s `<run>:<hex>` form or a bare hex string.
pub fn fingerprint_of_hex(value: &str) -> Option<String> {
    let hex = value.rsplit_once(':').map_or(value, |(_, h)| h);
    unhex(hex).map(|b| sha256::hex(&b)[..16].to_owned())
}

fn append(record: &Path, v: &Value) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record)
    {
        let _ = writeln!(f, "{v}");
    }
}

/// Runs the stub on stdin/stdout until EOF. `only` restricts `tools/list` to those names
/// (what A2's server does for a connection's scopes); `None` lists the whole M1 surface.
pub fn run(
    record: &Path,
    only: Option<&[String]>,
    responses: Option<&Path>,
) -> std::io::Result<()> {
    // canned results per tool (the injection fixtures); default "ok (S7 stub)"
    let canned: serde_json::Map<String, Value> = responses
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut keys: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    keys.sort();
    let fp = std::env::var(RUN_VAR)
        .ok()
        .and_then(|h| fingerprint_of_hex(&h));
    append(
        record,
        &json!({"event": "start", "env_keys": keys, "run_fingerprint": fp}),
    );
    let tool_list: Vec<Value> = tools::M1_SURFACE
        .iter()
        .filter(|t| only.is_none_or(|o| o.iter().any(|n| n == t.name)))
        .map(|t| json!({"name": t.name, "description": format!("gmclip {} (S7 stub)", t.name), "inputSchema": {"type": "object"}}))
        .collect();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let tool = msg
            .pointer("/params/name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        append(
            record,
            &json!({"event": "msg", "method": method, "tool": tool}),
        );
        let Some(id) = msg.get("id") else { continue };
        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": msg.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "gmclip-s7-stub", "version": "0"}
            }),
            "tools/list" => json!({"tools": tool_list}),
            "tools/call" => {
                let text = tool
                    .as_deref()
                    .and_then(|t| canned.get(t))
                    .and_then(Value::as_str)
                    .unwrap_or("ok (S7 stub)");
                json!({"content": [{"type": "text", "text": text}]})
            }
            _ => json!({}),
        };
        writeln!(
            out,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_the_launcher() {
        // RunCredential::fingerprint = sha256(raw bytes)[..16]
        let hex = "07".repeat(32);
        assert_eq!(
            fingerprint_of_hex(&hex).unwrap(),
            sha256::hex(&[7u8; 32])[..16]
        );
        assert_eq!(fingerprint_of_hex("abc"), None);
        // GMCLIP_RUN's `<run>:<hex>` form
        assert_eq!(
            fingerprint_of_hex(&format!("01RUN:{hex}")).unwrap(),
            sha256::hex(&[7u8; 32])[..16]
        );
    }
}
