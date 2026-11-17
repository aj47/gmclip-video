//! A stub model endpoint on loopback: records every request a client sends and answers
//! each with a 400 `invalid_request_error`, so a probed client ends its turn having
//! spent nothing. What the S7 probe learns comes from the recorded requests: the system
//! prompt, the messages and the tool list the client would have sent to a real model.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One recorded request.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names only (values may hold the stub key).
    pub header_names: Vec<String>,
    pub content_encoding: Option<String>,
    pub body: String,
}

/// How the stub answers.
#[derive(Clone, Debug)]
pub enum Script {
    /// Every request gets a 400: the client stops after its first turn.
    Refuse,
    /// Anthropic Messages: the first request that offers tools is answered with
    /// `tool_use` blocks for each `(name, input)`; once the conversation carries tool
    /// results the answer is a closing text. Other requests get a short text.
    AnthropicToolUse(Vec<(String, serde_json::Value)>),
    /// Anthropic Messages over several turns: turn `k` (the number of tool-result rounds
    /// already in the conversation) asks for `turns[k]`; past the last turn it closes.
    /// A string input `"{{pass_run}}"` becomes the newest `pass_run` a tool result in the
    /// conversation named (a scripted pass steps the run gmclip started for it).
    AnthropicTurns(Vec<Vec<(String, serde_json::Value)>>),
}

/// A running stub; stops accepting when dropped (its thread exits on the next accept).
#[derive(Debug)]
pub struct StubModel {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    script: Arc<Mutex<Script>>,
}

/// The body every request gets.
pub const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"gmclip S7 stub: no model here"}}"#;

impl StubModel {
    pub fn start() -> std::io::Result<Self> {
        Self::start_with(Script::Refuse)
    }

    pub fn start_with(script: Script) -> std::io::Result<Self> {
        let script = Arc::new(Mutex::new(script));
        let shared = Arc::clone(&script);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&requests);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let sink = Arc::clone(&sink);
                let script = Arc::clone(&script);
                std::thread::spawn(move || {
                    let _ = serve(conn, &sink, &script);
                });
            }
        });
        Ok(Self {
            base_url,
            requests,
            script: shared,
        })
    }

    /// Replaces the script (used once the stub's own URL is known).
    pub fn set_script(&self, s: Script) {
        if let Ok(mut g) = self.script.lock() {
            *g = s;
        }
    }

    pub fn take(&self) -> Vec<Recorded> {
        self.requests.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

fn serve(
    conn: TcpStream,
    sink: &Mutex<Vec<Recorded>>,
    script: &Mutex<Script>,
) -> std::io::Result<()> {
    conn.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut writer = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut len = 0usize;
    let mut chunked = false;
    let mut header_names = Vec::new();
    let mut content_encoding = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            return Ok(());
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
                "content-encoding" => content_encoding = Some(v.to_owned()),
                _ => {}
            }
            header_names.push(k);
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            r.read_line(&mut size)?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if n == 0 {
                let mut crlf = String::new();
                r.read_line(&mut crlf)?;
                break;
            }
            let mut chunk = vec![0u8; n];
            r.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf)?;
        }
    } else if len > 0 {
        body = vec![0u8; len.min(64 << 20)];
        r.read_exact(&mut body)?;
    }
    if let Ok(mut g) = sink.lock() {
        g.push(Recorded {
            method: method.clone(),
            path: path.clone(),
            header_names,
            content_encoding,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    let script = script.lock().map(|g| g.clone()).unwrap_or(Script::Refuse);
    let (status, ctype, text) = match &script {
        _ if method == "GET" => (
            "404 Not Found",
            "application/json",
            r#"{"data":[]}"#.to_owned(),
        ),
        Script::Refuse => ("400 Bad Request", "application/json", REFUSAL.to_owned()),
        Script::AnthropicTurns(turns) => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            // the turn is one past the newest turn whose tool results came back (ids carry
            // their turn: toolu_s7_t<k>_<i>), so retries and split messages cannot repeat one
            let rounds = req["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|m| m["content"].as_array().into_iter().flatten())
                .filter(|b| b["type"] == "tool_result")
                .filter_map(|b| {
                    b["tool_use_id"]
                        .as_str()?
                        .strip_prefix("toolu_s7_t")?
                        .split('_')
                        .next()?
                        .parse::<usize>()
                        .ok()
                })
                .max()
                .map_or(0, |k| k + 1);
            let calls: Vec<(String, serde_json::Value)> =
                if req["tools"].as_array().is_some_and(|t| !t.is_empty()) {
                    let run = last_pass_run(&String::from_utf8_lossy(&body));
                    turns
                        .get(rounds)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .map(|(n, v)| (n.clone(), fill(v, run.as_deref())))
                        .collect()
                } else {
                    Vec::new()
                };
            anthropic_reply(&req, &calls, Some(rounds))
        }
        Script::AnthropicToolUse(calls) => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            anthropic_reply(&req, calls, None)
        }
    };
    write!(
        writer,
        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    )?;
    writer.flush()?;
    Ok(())
}

/// The newest `"pass_run": "<id>"` in a request body (tool results carry gmclip's JSON
/// as text, so the quotes may be escaped).
fn last_pass_run(body: &str) -> Option<String> {
    let flat = body.replace("\\\"", "\"");
    let i = flat.rfind("\"pass_run\":")? + "\"pass_run\":".len();
    let rest = flat[i..].trim_start().strip_prefix('"')?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!id.is_empty()).then_some(id)
}

/// `"{{pass_run}}"` anywhere in `v` replaced by `run`.
fn fill(v: &serde_json::Value, run: Option<&str>) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) if s == "{{pass_run}}" => {
            Value::String(run.unwrap_or("no-pass-run-yet").to_owned())
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, run)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, run))).collect())
        }
        x => x.clone(),
    }
}

fn anthropic_reply(
    req: &serde_json::Value,
    calls: &[(String, serde_json::Value)],
    turn: Option<usize>,
) -> (&'static str, &'static str, String) {
    let multi_turn = turn.is_some();
    use serde_json::json;
    let has_results = req["messages"].as_array().into_iter().flatten().any(|m| {
        m["content"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|b| b["type"] == "tool_result")
    });
    let offers_tools = req["tools"].as_array().is_some_and(|t| !t.is_empty());
    let blocks: Vec<serde_json::Value> =
        if offers_tools && (multi_turn || !has_results) && !calls.is_empty() {
            calls
                .iter()
                .enumerate()
                .map(|(i, (name, input))| {
                    let id = match turn {
                        Some(t) => format!("toolu_s7_t{t}_{i}"),
                        None => format!("toolu_s7_{i}"),
                    };
                    json!({"type": "tool_use", "id": id, "name": name, "input": input})
                })
                .collect()
        } else {
            vec![json!({"type": "text", "text": "ok"})]
        };
    let stop = if blocks.iter().any(|b| b["type"] == "tool_use") {
        "tool_use"
    } else {
        "end_turn"
    };
    let model = req["model"]
        .as_str()
        .unwrap_or("claude-opus-5-5")
        .to_owned();
    if req["stream"] != true {
        let msg = json!({"id": "msg_s7", "type": "message", "role": "assistant", "model": model, "content": blocks,
            "stop_reason": stop, "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}});
        return ("200 OK", "application/json", msg.to_string());
    }
    let mut sse = String::new();
    let mut ev = |name: &str, data: serde_json::Value| {
        sse.push_str(&format!("event: {name}\ndata: {data}\n\n"))
    };
    ev(
        "message_start",
        json!({"type": "message_start", "message": {"id": "msg_s7", "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}}}),
    );
    for (i, b) in blocks.iter().enumerate() {
        if b["type"] == "tool_use" {
            ev(
                "content_block_start",
                json!({"type": "content_block_start", "index": i,
                "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}}),
            );
            ev(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": i,
                "delta": {"type": "input_json_delta", "partial_json": b["input"].to_string()}}),
            );
        } else {
            ev(
                "content_block_start",
                json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}}),
            );
            ev(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}}),
            );
        }
        ev(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": i}),
        );
    }
    ev(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 1}}),
    );
    ev("message_stop", json!({"type": "message_stop"}));
    ("200 OK", "text/event-stream", sse)
}

/// Every tool name a request offers the model, with namespaces joined by `/`. Handles
/// the Anthropic Messages shape (`tools[].name`) and Codex's Responses shape (tools
/// nested in `additional_tools` namespaces; built-ins by `type`).
pub fn tool_names(body: &serde_json::Value) -> Vec<String> {
    fn walk(v: &serde_json::Value, ns: &[String], out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(serde_json::Value::Array(tools)) = m.get("tools") {
                    let mut nns = ns.to_vec();
                    if m.get("type").and_then(|t| t.as_str()) == Some("namespace")
                        && let Some(n) = m.get("name").and_then(|n| n.as_str())
                    {
                        nns.push(n.to_owned());
                    }
                    for t in tools {
                        if t.get("type").and_then(|x| x.as_str()) == Some("namespace") {
                            walk(t, &nns, out);
                        } else if let Some(name) = t
                            .get("name")
                            .and_then(|x| x.as_str())
                            .or_else(|| t.get("type").and_then(|x| x.as_str()))
                        {
                            let mut full = nns.clone();
                            full.push(name.to_owned());
                            let full = full.join("/");
                            // Codex code mode: nested tools are documented in `exec`'s
                            // description as "### `name`" sections
                            if let Some(d) = t.get("description").and_then(|x| x.as_str()) {
                                for nested in d.split("### `").skip(1) {
                                    if let Some(n) = nested.split('`').next() {
                                        out.push(format!("{full}>{n}"));
                                    }
                                }
                            }
                            out.push(full);
                        }
                    }
                }
                for (k, x) in m {
                    if k != "tools" {
                        walk(x, ns, out);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, ns, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(body, &[], &mut out);
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_refuses() {
        let s = StubModel::start().unwrap();
        let addr = s.base_url.trim_start_matches("http://").to_owned();
        let mut c = TcpStream::connect(&addr).unwrap();
        let body = r#"{"tools":[{"name":"mcp__gmclip__lint"}]}"#;
        write!(c, "POST /v1/messages HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\nx-api-key: secret\r\n\r\n{body}", body.len()).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400") && resp.ends_with(REFUSAL));
        let got = s.take();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, body);
        assert!(got[0].header_names.contains(&"x-api-key".to_owned()));
        assert!(!format!("{got:?}").contains("secret"));
    }

    #[test]
    fn a_scripted_turn_steps_the_pass_run_it_was_given() {
        // the tool result carries gmclip's JSON as escaped text
        let body = serde_json::json!({"messages": [{"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "toolu_s7_t0_0", "content": "{\"pass_run\":\"pr_0a1b2c3d4e5f6071\",\"step\":{}}"}]}]})
        .to_string();
        assert_eq!(last_pass_run(&body).as_deref(), Some("pr_0a1b2c3d4e5f6071"));
        assert_eq!(last_pass_run("{}"), None);
        let v = serde_json::json!({"pass_run": "{{pass_run}}", "submit": [{"quote": "{{pass_run}} stays"}]});
        let f = fill(&v, Some("pr_1"));
        assert_eq!(f["pass_run"], "pr_1");
        assert_eq!(f["submit"][0]["quote"], "{{pass_run}} stays");
    }

    #[test]
    fn chunked_bodies() {
        let s = StubModel::start().unwrap();
        let mut c = TcpStream::connect(s.base_url.trim_start_matches("http://")).unwrap();
        write!(c, "POST /v1/responses HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert_eq!(s.take()[0].body, "abcde");
    }

    #[test]
    fn tool_names_in_both_shapes() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"tools":[{"name":"Bash"},{"name":"mcp__gmclip__lint"}]}"#)
                .unwrap();
        assert_eq!(tool_names(&a), ["Bash", "mcp__gmclip__lint"]);
        let b: serde_json::Value = serde_json::from_str(
            r#"{"input":[{"type":"additional_tools","tools":[{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"exec"}]},{"type":"namespace","name":"clock","tools":[{"type":"function","name":"sleep"}]}]}]}"#,
        )
        .unwrap();
        assert_eq!(tool_names(&b), ["clock/sleep", "functions/exec"]);
        let c: serde_json::Value = serde_json::from_str(
            r#"{"tools":[{"name":"exec","description":"x\n### `apply_patch`\ny\n### `clock__curr_time`\n"}]}"#,
        )
        .unwrap();
        assert_eq!(
            tool_names(&c),
            ["exec", "exec>apply_patch", "exec>clock__curr_time"]
        );
    }
}
