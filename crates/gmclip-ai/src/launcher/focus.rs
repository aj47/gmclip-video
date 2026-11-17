//! What a running agent is looking at (backlog G.04): the latest `see`, `listen` or
//! `transcript_read` call in the run's own tool-call log (its transcript), as a short
//! line for the activity rail — the frame times, the audio range, the transcript span or
//! search, and the cut, source or part it is on.
//!
//! The arguments are the model's, so every string taken from them is canonicalised and
//! clipped (R-INJ-6) and the UI shows the line as plain text.

use serde::Serialize;
use serde_json::Value;

use super::Harness;
use super::transcript::{self, canonicalise_untrusted};

/// The tools whose target is "what the agent is looking at".
pub const LOOKING_TOOLS: [&str; 3] = ["see", "listen", "transcript_read"];

/// The latest looking call.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Focus {
    /// `see`, `listen` or `transcript_read`.
    pub tool: &'static str,
    /// One line, e.g. "Frames of cut 7f3a at 12.4 s, 15 s".
    pub label: String,
    /// The cut or source it names.
    pub target: Option<String>,
    /// `see`: the frame times (s, on the cut).
    pub frames_s: Option<Vec<f64>>,
    /// `listen` / `transcript_read`: the source range (s).
    pub range_s: Option<[f64; 2]>,
    /// The part an anchor names (`transcript_read{around: …part…}`).
    pub part: Option<String>,
    /// How many tool calls the run had made when it looked (1-based).
    pub call: usize,
}

fn clip(s: &str, n: usize) -> String {
    let c = canonicalise_untrusted(s);
    let one: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n {
        format!("{}…", one.chars().take(n).collect::<String>())
    } else {
        one
    }
}

fn secs(t: f64) -> String {
    if !t.is_finite() || t < 0.0 {
        return "?".into();
    }
    let whole = t.floor() as u64;
    let tenth = ((t - t.floor()) * 10.0).round() as u64 % 10;
    if whole >= 60 {
        format!("{}:{:02}.{tenth}", whole / 60, whole % 60)
    } else {
        format!("{whole}.{tenth} s")
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite())
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| clip(s, 40))
}

/// The first `part` / `part_id` string anywhere in an anchor (depth-limited).
fn part_in(v: &Value, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    match v {
        Value::Object(m) => {
            for k in ["part", "part_id"] {
                if let Some(s) = m.get(k).and_then(Value::as_str) {
                    return Some(clip(s, 24));
                }
            }
            m.values().find_map(|x| part_in(x, depth - 1))
        }
        Value::Array(a) => a.iter().find_map(|x| part_in(x, depth - 1)),
        _ => None,
    }
}

/// What one call looks at.
pub fn of_call(tool: &str, input: &Value, call: usize) -> Option<Focus> {
    let tool: &'static str = LOOKING_TOOLS.into_iter().find(|t| *t == tool)?;
    let mut f = Focus {
        tool,
        label: String::new(),
        target: None,
        frames_s: None,
        range_s: None,
        part: None,
        call,
    };
    match tool {
        "see" => {
            let target = input
                .get("target")
                .and_then(Value::as_str)
                .map(|t| clip(t.strip_prefix("cut:").unwrap_or(t), 24));
            let at: Option<Vec<f64>> = input.get("at").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_f64)
                    .filter(|x| x.is_finite())
                    .take(16)
                    .collect()
            });
            let when = match (&at, num(input, "every_s")) {
                (Some(a), _) if !a.is_empty() => {
                    let shown: Vec<String> = a.iter().take(4).map(|t| secs(*t)).collect();
                    let more = if a.len() > 4 {
                        format!(" (+{})", a.len() - 4)
                    } else {
                        String::new()
                    };
                    format!(" at {}{more}", shown.join(", "))
                }
                (_, Some(e)) => format!(" every {}", secs(e)),
                _ => String::new(),
            };
            f.label = format!("Frames of cut {}{when}", target.as_deref().unwrap_or("?"));
            f.target = target;
            f.frames_s = at.filter(|a| !a.is_empty());
        }
        "listen" => {
            let src = text(input, "source");
            let r = num(input, "from_s").zip(num(input, "to_s"));
            f.label = match r {
                Some((a, b)) => format!(
                    "Audio of {} {}–{}",
                    src.as_deref().unwrap_or("?"),
                    secs(a),
                    secs(b)
                ),
                None => format!("Audio of {}", src.as_deref().unwrap_or("?")),
            };
            f.target = src;
            f.range_s = r.map(|(a, b)| [a, b]);
        }
        _ => {
            let src = text(input, "source");
            // no source: the project's whole transcript
            let s = src
                .as_deref()
                .map_or_else(|| "Transcript".to_owned(), |s| format!("Transcript of {s}"));
            let r = num(input, "from_s").zip(num(input, "to_s"));
            f.part = input.get("around").and_then(|a| part_in(a, 4));
            f.label = if let Some(q) = text(input, "search") {
                format!("{s}: searching “{q}”")
            } else if let Some((a, b)) = r {
                format!("{s} {}–{}", secs(a), secs(b))
            } else if let Some(p) = &f.part {
                format!("{s} around part {p}")
            } else if input.get("around").is_some() {
                format!("{s} around a word")
            } else if input.get("cursor").is_some() {
                format!("{s} (next page)")
            } else {
                s
            };
            f.target = src;
            f.range_s = r.map(|(a, b)| [a, b]);
        }
    }
    Some(f)
}

/// The latest looking call in a (possibly still growing) transcript of `h`.
pub fn latest(h: Harness, text: &str) -> Option<Focus> {
    let calls = transcript::parse(h, text).calls;
    calls.iter().enumerate().rev().find_map(|(i, c)| {
        let bare = c.name.strip_prefix("mcp__gmclip__")?;
        of_call(bare, &c.input, i + 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn see_listen_and_transcript_read_labels() {
        let f = of_call("see", &json!({"target": "cut:7f3a", "at": [12.4, 75.0]}), 3).unwrap();
        assert_eq!(f.label, "Frames of cut 7f3a at 12.4 s, 1:15.0");
        assert_eq!(f.frames_s, Some(vec![12.4, 75.0]));
        let f = of_call(
            "listen",
            &json!({"source": "src1", "from_s": 120.0, "to_s": 135.5}),
            1,
        )
        .unwrap();
        assert_eq!(f.label, "Audio of src1 2:00.0–2:15.5");
        assert_eq!(f.range_s, Some([120.0, 135.5]));
        let f = of_call(
            "transcript_read",
            &json!({"source": "src1", "around": {"part_edge": {"part": "p9", "edge": "start"}}}),
            2,
        )
        .unwrap();
        assert_eq!(f.label, "Transcript of src1 around part p9");
        assert_eq!(f.part.as_deref(), Some("p9"));
        let f = of_call(
            "transcript_read",
            &json!({"source": "s", "search": "five\u{202e} seconds"}),
            2,
        )
        .unwrap();
        assert_eq!(f.label, "Transcript of s: searching “five seconds”");
        assert!(of_call("cut_edit", &json!({}), 1).is_none());
        // no source (or an empty one) reads the project's whole transcript: no double space
        for input in [
            json!({"from_s": 10.0, "to_s": 20.0}),
            json!({"source": " ", "from_s": 10.0, "to_s": 20.0}),
        ] {
            let f = of_call("transcript_read", &input, 4).unwrap();
            assert_eq!(f.label, "Transcript 10.0 s–20.0 s");
            assert_eq!(f.target, None);
        }
        assert_eq!(
            of_call("transcript_read", &json!({}), 5).unwrap().label,
            "Transcript"
        );
    }

    #[test]
    fn latest_is_the_last_looking_call_in_any_client_log() {
        let claude = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"mcp__gmclip__listen","input":{"source":"s","from_s":1,"to_s":2}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"mcp__gmclip__see","input":{"target":"cut:c1","every_s":2}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c","name":"mcp__gmclip__cut_edit","input":{}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"d","name":"mcp__gmclip_"#;
        let f = latest(Harness::ClaudeCode, claude).unwrap();
        assert_eq!((f.tool, f.call), ("see", 2));
        assert_eq!(f.label, "Frames of cut c1 every 2.0 s");
        let pi = r#"{"type":"tool_execution_start","toolCallId":"x","toolName":"mcp__gmclip__transcript_read","args":{"source":"s","from_s":0,"to_s":60}}"#;
        assert_eq!(
            latest(Harness::Pi, pi).unwrap().label,
            "Transcript of s 0.0 s–1:00.0"
        );
        let oc = r#"{"type":"tool_use","sessionID":"s","part":{"tool":"gmclip_listen","state":{"status":"completed","input":{"source":"a","from_s":3,"to_s":4}}}}"#;
        assert_eq!(latest(Harness::OpenCode, oc).unwrap().tool, "listen");
        let codex = r#"{"type":"item.started","item":{"id":"i","type":"mcp_tool_call","server":"gmclip","tool":"see","arguments":{"target":"cut:z","at":[1]}}}"#;
        assert_eq!(
            latest(Harness::Codex, codex).unwrap().target.as_deref(),
            Some("z")
        );
        // another server's `see` is not gmclip's
        let other = r#"{"type":"tool_execution_start","toolCallId":"x","toolName":"mcp__evil__see","args":{}}"#;
        assert!(latest(Harness::Pi, other).is_none());
    }
}
