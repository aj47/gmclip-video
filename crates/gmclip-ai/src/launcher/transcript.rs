//! Reading a client transcript: every tool call the session made, the token usage, and
//! the harness's own closing summary as **untrusted** text (R-INJ-1, R-INJ-6).
//!
//! - Claude Code `--output-format stream-json`: `assistant` messages carry `tool_use`
//!   blocks (`name` is already `mcp__gmclip__<tool>` for gmclip); the `result` line has
//!   the summary and usage; `system/init` lists the tools the session was given.
//! - Codex `exec --json`: `item.started` / `item.completed` events; `mcp_tool_call`
//!   items name `server` and `tool`; `agent_message` items are the model's text;
//!   `turn.completed` carries usage. Every other item kind (`command_execution`,
//!   `file_change`, `web_search`, …, and any kind this parser does not know) is counted as
//!   a non-gmclip tool call, so an unknown event can only make isolation fail, never pass.
//! - pi `--mode json`: the first line is `{"type":"session","id"}`; every tool pi runs is
//!   a `tool_execution_start` / `_end` pair (`toolName`, `args`, `isError`), whatever the
//!   tool's origin; `message_end` with an assistant message carries text and usage;
//!   `agent_end` closes the run.
//! - opencode `run --format json`: one object per event, each with `sessionID`;
//!   `tool_use` (a finished call: `part.tool`, `part.state.{status,input}`), `text`,
//!   `step_finish` (`part.tokens`, `part.reason`), `error`. MCP tools are named
//!   `<server>_<tool>`; `gmclip_<tool>` is normalised to `mcp__gmclip__<tool>`, anything
//!   else is `opencode:<tool>`, a non-gmclip call.

use serde::Serialize;
use serde_json::Value;

use super::Harness;

/// One tool call, normalised to the `mcp__<server>__<tool>` naming for MCP tools.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: Value,
    /// The client reported the call as failed or refused.
    pub is_error: bool,
}

impl ToolCall {
    pub fn is_gmclip(&self) -> bool {
        self.name.starts_with("mcp__gmclip__") && self.name.len() > "mcp__gmclip__".len()
    }
}

/// Token counts as the client reported them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

/// What a transcript says.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Parsed {
    pub calls: Vec<ToolCall>,
    pub usage: Usage,
    /// Tools the session was given (Claude Code `system/init`; Codex does not report it).
    pub tools_offered: Option<Vec<String>>,
    /// The closing summary, canonicalised (untrusted; never instructions).
    pub summary: Option<String>,
    /// Lines that were not JSON.
    pub junk_lines: usize,
    /// The client wrote its closing line (Claude `result`, Codex `turn.completed`/`turn.failed`,
    /// pi `agent_end`, opencode a final `step_finish` or an `error`).
    pub result_seen: bool,
    /// The client's own session id (pi, opencode: what "continue" resumes; Claude Code and
    /// Codex: recorded only, their sessions are not kept).
    pub session_id: Option<String>,
}

/// Longest summary kept on the run record.
pub const MAX_SUMMARY_BYTES: usize = 16 * 1024;

/// Strips the code points R-INJ-6 removes before untrusted text reaches a model or a
/// human: Unicode Tag characters, bidi controls, zero-width characters and other
/// C0/C1 controls except tab and newline. Idempotent.
///
/// TODO(A2): replace with `gmclip_mcp::canon` once it is on main (one canonicaliser).
pub fn canonicalise_untrusted(s: &str) -> String {
    s.chars().filter(|&c| !invisible(c)).collect()
}

/// Code points that render as nothing (or reorder text) and can hide instructions:
/// Unicode Tag characters, bidi controls, the format characters (category Cf, listed),
/// variation selectors, fillers, and C0/C1 controls other than tab and newline.
fn invisible(c: char) -> bool {
    let u = c as u32;
    let control = (c.is_control() && c != '\n' && c != '\t') || (0x80..=0x9F).contains(&u);
    let format_cf = matches!(
        u,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    );
    let tags = (0xE0000..=0xE007F).contains(&u);
    let selectors = matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x180B..=0x180F);
    let fillers = matches!(
        u,
        0x034F | 0x115F | 0x1160 | 0x3164 | 0xFFA0 | 0x17B4 | 0x17B5
    );
    let separators = matches!(u, 0x2028 | 0x2029);
    control || format_cf || tags || selectors || fillers || separators
}

fn cap(mut s: String) -> String {
    if s.len() > MAX_SUMMARY_BYTES {
        let mut end = MAX_SUMMARY_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str(" […truncated]");
    }
    s
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// Parses a transcript of `h`.
pub fn parse(h: Harness, text: &str) -> Parsed {
    match h {
        Harness::ClaudeCode => parse_claude(text),
        Harness::Codex => parse_codex(text),
        Harness::Pi => parse_pi(text),
        Harness::OpenCode => parse_opencode(text),
    }
}

fn sid(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| super::rundir::valid_run_id(s))
        .map(str::to_owned)
}

fn parse_pi(text: &str) -> Parsed {
    let mut p = Parsed::default();
    // toolCallId → index in calls
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut last_text = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.junk_lines += 1;
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session") => p.session_id = p.session_id.take().or_else(|| sid(&v, "id")),
            Some("tool_execution_start") => {
                let id = v
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let call = ToolCall {
                    name: v
                        .get("toolName")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    input: v.get("args").cloned().unwrap_or(Value::Null),
                    is_error: false,
                };
                if let Some(i) = id {
                    seen.insert(i, p.calls.len());
                }
                p.calls.push(call);
            }
            Some("tool_execution_end") => {
                let failed = v.get("isError").and_then(Value::as_bool) == Some(true);
                match v
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .and_then(|i| seen.get(i))
                {
                    Some(&k) => p.calls[k].is_error = failed,
                    // an end without a start still counts
                    None => p.calls.push(ToolCall {
                        name: v
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        input: v.get("args").cloned().unwrap_or(Value::Null),
                        is_error: failed,
                    }),
                }
            }
            Some("message_end") => {
                let m = v.get("message").unwrap_or(&Value::Null);
                if m.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let t: Vec<&str> = m
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                if !t.is_empty() {
                    last_text = Some(t.join("\n"));
                }
                if let Some(us) = m.get("usage") {
                    p.usage.input_tokens += u(us, "input") + u(us, "cacheWrite");
                    p.usage.output_tokens += u(us, "output");
                    p.usage.cache_read_tokens += u(us, "cacheRead");
                }
            }
            Some("agent_end") => p.result_seen = true,
            _ => {}
        }
    }
    p.summary = last_text.map(|s| cap(canonicalise_untrusted(&s)));
    p
}

fn parse_opencode(text: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut last_text = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.junk_lines += 1;
            continue;
        };
        if p.session_id.is_none() {
            p.session_id = sid(&v, "sessionID");
        }
        let part = v.get("part").unwrap_or(&Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let tool = part.get("tool").and_then(Value::as_str).unwrap_or("?");
                let name = match tool.strip_prefix("gmclip_") {
                    Some(t) if !t.is_empty() => format!("mcp__gmclip__{t}"),
                    _ => format!("opencode:{tool}"),
                };
                let state = part.get("state").unwrap_or(&Value::Null);
                p.calls.push(ToolCall {
                    name,
                    input: state.get("input").cloned().unwrap_or(Value::Null),
                    is_error: state.get("status").and_then(Value::as_str) != Some("completed"),
                });
            }
            Some("text") => {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    last_text = Some(t.to_owned());
                }
            }
            Some("step_finish") => {
                if let Some(t) = part.get("tokens") {
                    p.usage.input_tokens += u(t, "input")
                        + t.pointer("/cache/write")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    p.usage.output_tokens += u(t, "output") + u(t, "reasoning");
                    p.usage.cache_read_tokens += t
                        .pointer("/cache/read")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                }
                // a step that ends by calling tools is not the end of the run
                p.result_seen = part.get("reason").and_then(Value::as_str) != Some("tool-calls");
            }
            Some("error") => p.result_seen = true,
            _ => {}
        }
    }
    p.summary = last_text.map(|s| cap(canonicalise_untrusted(&s)));
    p
}

fn parse_claude(text: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut errors = std::collections::BTreeSet::new();
    let mut ids: Vec<Option<String>> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.junk_lines += 1;
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                p.session_id = sid(&v, "session_id");
                p.tools_offered = v.get("tools").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                });
            }
            Some("assistant") => {
                for block in v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                    // any other tool-shaped block (server_tool_use, mcp_tool_use, a future
                    // kind) counts as a call under its own kind: it can only fail isolation
                    if kind == "tool_use" || kind.ends_with("tool_use") || kind.ends_with("_call") {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                        p.calls.push(ToolCall {
                            name: if kind == "tool_use" {
                                name.to_owned()
                            } else {
                                format!("claude:{kind}:{name}")
                            },
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                            is_error: false,
                        });
                        ids.push(block.get("id").and_then(Value::as_str).map(str::to_owned));
                    }
                }
            }
            Some("user") => {
                for block in v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result")
                        && block.get("is_error").and_then(Value::as_bool) == Some(true)
                        && let Some(id) = block.get("tool_use_id").and_then(Value::as_str)
                    {
                        errors.insert(id.to_owned());
                    }
                }
            }
            Some("result") => {
                p.result_seen = true;
                if let Some(s) = v.get("result").and_then(Value::as_str) {
                    p.summary = Some(cap(canonicalise_untrusted(s)));
                }
                if let Some(us) = v.get("usage") {
                    p.usage = Usage {
                        input_tokens: u(us, "input_tokens") + u(us, "cache_creation_input_tokens"),
                        output_tokens: u(us, "output_tokens"),
                        cache_read_tokens: u(us, "cache_read_input_tokens"),
                    };
                }
            }
            _ => {}
        }
    }
    for (call, id) in p.calls.iter_mut().zip(ids) {
        call.is_error = id.is_some_and(|id| errors.contains(&id));
    }
    p
}

fn parse_codex(text: &str) -> Parsed {
    let mut p = Parsed::default();
    // item id → index in calls (an item is reported started, then completed)
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut last_message = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.junk_lines += 1;
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("item.started" | "item.updated" | "item.completed") => {
                let Some(item) = v.get("item") else { continue };
                let kind = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let id = item.get("id").and_then(Value::as_str).map(str::to_owned);
                let call = match kind {
                    "agent_message" => {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            last_message = Some(t.to_owned());
                        }
                        None
                    }
                    "reasoning" | "error" => None,
                    "mcp_tool_call" => {
                        let server = item.get("server").and_then(Value::as_str).unwrap_or("?");
                        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("?");
                        let failed = item.get("status").and_then(Value::as_str) == Some("failed")
                            || item.get("error").is_some_and(|e| !e.is_null());
                        Some(ToolCall {
                            name: format!("mcp__{server}__{tool}"),
                            input: item.get("arguments").cloned().unwrap_or(Value::Null),
                            is_error: failed,
                        })
                    }
                    other => Some(ToolCall {
                        name: format!("codex:{other}"),
                        input: item.clone(),
                        is_error: item.get("status").and_then(Value::as_str) == Some("failed"),
                    }),
                };
                // an item without an id is counted once, on completion
                let event = v.get("type").and_then(Value::as_str).unwrap_or("");
                if let Some(call) = call.filter(|_| id.is_some() || event == "item.completed") {
                    match id.as_ref().and_then(|i| seen.get(i)) {
                        Some(&k) => p.calls[k] = call,
                        None => {
                            if let Some(i) = id {
                                seen.insert(i, p.calls.len());
                            }
                            p.calls.push(call);
                        }
                    }
                }
            }
            Some("thread.started") => p.session_id = sid(&v, "thread_id"),
            Some("turn.failed") => p.result_seen = true,
            Some("turn.completed") => {
                p.result_seen = true;
                if let Some(us) = v.get("usage") {
                    p.usage.input_tokens += u(us, "input_tokens");
                    p.usage.output_tokens += u(us, "output_tokens");
                    p.usage.cache_read_tokens += u(us, "cached_input_tokens");
                }
            }
            _ => {}
        }
    }
    p.summary = last_message.map(|s| cap(canonicalise_untrusted(&s)));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = r#"{"type":"system","subtype":"init","tools":["mcp__gmclip__lint","mcp__gmclip__cut_get"]}
{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","id":"t1","name":"mcp__gmclip__lint","input":{"cut":"c1"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"denied"}]}}
not json
{"type":"result","result":"Done\u202e.\udb40\udc41 ok","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"output_tokens":7,"cache_read_input_tokens":100}}
"#;

    #[test]
    fn claude_stream_json() {
        let p = parse(Harness::ClaudeCode, CLAUDE);
        assert_eq!(p.calls.len(), 2);
        assert!(p.calls[0].is_gmclip() && !p.calls[0].is_error);
        assert!(!p.calls[1].is_gmclip() && p.calls[1].is_error);
        assert_eq!(p.tools_offered.unwrap().len(), 2);
        assert_eq!(p.summary.as_deref(), Some("Done. ok"));
        assert_eq!(
            p.usage,
            Usage {
                input_tokens: 15,
                output_tokens: 7,
                cache_read_tokens: 100
            }
        );
        assert_eq!(p.junk_lines, 1);
    }

    const CODEX: &str = r#"{"type":"thread.started","thread_id":"x"}
{"type":"item.started","item":{"id":"i1","type":"mcp_tool_call","server":"gmclip","tool":"cut_get","arguments":{"cut":"c"},"status":"in_progress"}}
{"type":"item.completed","item":{"id":"i1","type":"mcp_tool_call","server":"gmclip","tool":"cut_get","arguments":{"cut":"c"},"status":"completed"}}
{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"ls","status":"failed"}}
{"type":"item.completed","item":{"id":"i3","type":"some_future_kind"}}
{"type":"item.completed","item":{"id":"i4","type":"agent_message","text":"All done\u200b."}}
{"type":"turn.completed","usage":{"input_tokens":3,"cached_input_tokens":2,"output_tokens":1}}
"#;

    #[test]
    fn codex_json() {
        let p = parse(Harness::Codex, CODEX);
        let names: Vec<&str> = p.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "mcp__gmclip__cut_get",
                "codex:command_execution",
                "codex:some_future_kind"
            ]
        );
        assert!(p.calls[1].is_error);
        assert_eq!(p.summary.as_deref(), Some("All done."));
        assert_eq!(p.usage.cache_read_tokens, 2);
    }

    const PI: &str = r#"{"type":"session","version":3,"id":"0b5e7c1d-aa","timestamp":"t","cwd":"/r/cwd"}
{"type":"agent_start"}
{"type":"tool_execution_start","toolCallId":"c1","toolName":"mcp__gmclip__transcript_read","args":{"source":"s1","from_s":0,"to_s":60}}
{"type":"tool_execution_end","toolCallId":"c1","toolName":"mcp__gmclip__transcript_read","result":{},"isError":false}
{"type":"tool_execution_start","toolCallId":"c2","toolName":"bash","args":{"command":"ls"}}
{"type":"tool_execution_end","toolCallId":"c2","toolName":"bash","result":{},"isError":true}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Trimmed\u200b 3 parts."}],"usage":{"input":10,"output":4,"cacheRead":50,"cacheWrite":2}}}
{"type":"agent_end","messages":[]}
"#;

    #[test]
    fn pi_json() {
        let p = parse(Harness::Pi, PI);
        assert_eq!(p.session_id.as_deref(), Some("0b5e7c1d-aa"));
        let names: Vec<&str> = p.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["mcp__gmclip__transcript_read", "bash"]);
        assert!(p.calls[0].is_gmclip() && !p.calls[0].is_error);
        assert!(!p.calls[1].is_gmclip() && p.calls[1].is_error);
        assert_eq!(p.summary.as_deref(), Some("Trimmed 3 parts."));
        assert_eq!(p.usage.input_tokens, 12);
        assert_eq!(p.usage.cache_read_tokens, 50);
        assert!(p.result_seen);
    }

    const OPENCODE: &str = r#"{"type":"step_start","timestamp":1,"sessionID":"ses_9a","part":{"type":"step-start"}}
{"type":"tool_use","timestamp":2,"sessionID":"ses_9a","part":{"type":"tool","tool":"gmclip_listen","callID":"x1","state":{"status":"completed","input":{"source":"s1","from_s":10,"to_s":20}}}}
{"type":"tool_use","timestamp":3,"sessionID":"ses_9a","part":{"type":"tool","tool":"bash","callID":"x2","state":{"status":"error","input":{"command":"ls"}}}}
{"type":"step_finish","timestamp":4,"sessionID":"ses_9a","part":{"type":"step-finish","reason":"tool-calls","tokens":{"input":5,"output":1,"reasoning":2,"cache":{"read":7,"write":1}}}}
{"type":"text","timestamp":5,"sessionID":"ses_9a","part":{"type":"text","text":"Listened\u202e once."}}
{"type":"step_finish","timestamp":6,"sessionID":"ses_9a","part":{"type":"step-finish","reason":"stop","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}}
"#;

    #[test]
    fn opencode_json() {
        let p = parse(Harness::OpenCode, OPENCODE);
        assert_eq!(p.session_id.as_deref(), Some("ses_9a"));
        let names: Vec<&str> = p.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["mcp__gmclip__listen", "opencode:bash"]);
        assert!(!p.calls[0].is_error && p.calls[1].is_error);
        assert_eq!(p.summary.as_deref(), Some("Listened once."));
        assert_eq!(p.usage.input_tokens, 7);
        assert_eq!(p.usage.output_tokens, 4);
        assert!(p.result_seen);
        // cut off after a tool step: not a finished run
        let cut: String = OPENCODE.lines().take(4).collect::<Vec<_>>().join("\n");
        assert!(!parse(Harness::OpenCode, &cut).result_seen);
        // a bare `gmclip_` is not a gmclip tool
        let t = r#"{"type":"tool_use","sessionID":"s","part":{"tool":"gmclip_","state":{"status":"completed"}}}"#;
        assert_eq!(
            parse(Harness::OpenCode, t).calls[0].name,
            "opencode:gmclip_"
        );
    }

    #[test]
    fn canonicalisation_is_idempotent_and_strips_smuggling() {
        let s = "a\u{E0041}\u{E0042}b\u{202E}c\u{200D}d\u{0007}e\nf\tg";
        let once = canonicalise_untrusted(s);
        assert_eq!(once, "abcde\nf\tg");
        assert_eq!(canonicalise_untrusted(&once), once);
    }

    #[test]
    fn other_tool_shaped_blocks_count_and_idless_codex_items_count_once() {
        let t = r#"{"type":"assistant","message":{"content":[{"type":"server_tool_use","id":"s1","name":"web_search","input":{}}]}}"#;
        let p = parse(Harness::ClaudeCode, t);
        assert_eq!(p.calls.len(), 1);
        assert!(!p.calls[0].is_gmclip());
        let t = "{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\"}}";
        assert_eq!(parse(Harness::Codex, t).calls.len(), 1);
    }

    #[test]
    fn invisible_formatting_is_stripped() {
        let s = "a\u{FE0F}b\u{E0101}c\u{00AD}d\u{2061}e\u{3164}f\u{2028}g";
        assert_eq!(canonicalise_untrusted(s), "abcdefg");
    }

    #[test]
    fn gmclip_prefix_alone_is_not_a_gmclip_call() {
        let c = ToolCall {
            name: "mcp__gmclip__".into(),
            input: Value::Null,
            is_error: false,
        };
        assert!(!c.is_gmclip());
        let c = ToolCall {
            name: "mcp__gmclipx__lint".into(),
            input: Value::Null,
            is_error: false,
        };
        assert!(!c.is_gmclip());
    }
}
