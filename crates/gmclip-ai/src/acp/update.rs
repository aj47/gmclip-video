//! What an agent sends over ACP, typed: `session/update` notifications and
//! `session/request_permission` requests. Every string an agent writes is untrusted:
//! it is canonicalised (R-INJ-6) here, before anything shows or stores it. Unknown
//! update kinds are kept by name ([`Update::Other`]) so a newer agent never breaks a
//! session.

use serde_json::Value;

use crate::launcher::transcript::canonicalise_untrusted;

/// One `session/update`.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Text of the human's message (an agent replays these when it loads a session).
    UserText(String),
    AgentText(String),
    Thought(String),
    /// A new tool call.
    ToolCall(ToolCall),
    /// A change to a tool call (only the fields it names are set).
    ToolCallUpdate(ToolCall),
    /// The agent's plan, whole (each update replaces the last).
    Plan(Vec<PlanEntry>),
    /// The session's mode changed (e.g. Claude Code's `acceptEdits`).
    Mode(String),
    /// The slash commands the agent offers.
    Commands(Vec<String>),
    /// Context use, when the agent reports it.
    Usage {
        used: Option<u64>,
        size: Option<u64>,
    },
    /// The agent named the session.
    Title(String),
    /// A kind this client does not know (its `sessionUpdate`).
    Other(String),
}

/// A tool call or its update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub title: Option<String>,
    /// `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other`…
    pub kind: Option<String>,
    /// `pending`, `in_progress`, `completed`, `failed`.
    pub status: Option<String>,
    pub content: Vec<ToolContent>,
    /// `path` or `path:line`.
    pub locations: Vec<String>,
    pub raw_input: Option<Value>,
    pub raw_output: Option<Value>,
}

impl ToolCall {
    /// Whether this call is one of gmclip's own MCP tools (by the name the agent gave
    /// it). The agent names its calls, so this decides only what gmclip may approve
    /// without asking; the holder still checks every call's scopes.
    pub fn is_gmclip(&self) -> bool {
        let t = self.title.as_deref().unwrap_or("");
        let server = self
            .raw_input
            .as_ref()
            .and_then(|r| r.get("server").or_else(|| r.get("server_name")))
            .and_then(Value::as_str);
        t.starts_with("mcp__gmclip__")
            || t.starts_with("gmclip/")
            || t.starts_with("gmclip.")
            || t.starts_with("gmclip_")
            || server == Some("gmclip")
    }
}

/// What a tool call shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolContent {
    Text(String),
    Diff {
        path: String,
        old: Option<String>,
        new: String,
    },
    /// A terminal the agent runs (by id; this client offers no terminals).
    Terminal(String),
}

/// One step of the agent's plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// `pending`, `in_progress`, `completed`.
    pub status: String,
    /// `high`, `medium`, `low`.
    pub priority: String,
}

/// The agent asks before running a tool.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionRequest {
    pub session: String,
    pub tool: ToolCall,
    pub options: Vec<PermissionOption>,
}

/// One answer the agent offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once`, `reject_always`.
    pub kind: String,
}

impl PermissionRequest {
    /// The first option of `kind`, else of the same family (`allow_*` / `reject_*`).
    pub fn option(&self, kind: &str) -> Option<&PermissionOption> {
        let family = kind.split('_').next().unwrap_or(kind);
        self.options
            .iter()
            .find(|o| o.kind == kind)
            .or_else(|| self.options.iter().find(|o| o.kind.starts_with(family)))
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(canonicalise_untrusted)
}

/// The text a content block carries (images and audio as a placeholder).
pub fn text_of(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => s(block, "text"),
        "image" => Some("[image]".into()),
        "audio" => Some("[audio]".into()),
        "resource_link" => s(block, "name").or_else(|| s(block, "uri")),
        "resource" => block
            .get("resource")
            .and_then(|r| s(r, "text").or_else(|| s(r, "uri"))),
        _ => None,
    }
}

/// Parses the `update` object of a `session/update`.
pub fn parse(u: &Value) -> Update {
    let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
    let chunk = || u.get("content").and_then(text_of).unwrap_or_default();
    match kind {
        "user_message_chunk" => Update::UserText(chunk()),
        "agent_message_chunk" => Update::AgentText(chunk()),
        "agent_thought_chunk" => Update::Thought(chunk()),
        "tool_call" => Update::ToolCall(parse_tool(u)),
        "tool_call_update" => Update::ToolCallUpdate(parse_tool(u)),
        "plan" => Update::Plan(
            u.get("entries")
                .and_then(Value::as_array)
                .map(|es| {
                    es.iter()
                        .map(|e| PlanEntry {
                            content: s(e, "content").unwrap_or_default(),
                            status: s(e, "status").unwrap_or_else(|| "pending".into()),
                            priority: s(e, "priority").unwrap_or_else(|| "medium".into()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "current_mode_update" => Update::Mode(
            s(u, "currentModeId")
                .or_else(|| s(u, "modeId"))
                .unwrap_or_default(),
        ),
        "available_commands_update" => Update::Commands(
            u.get("availableCommands")
                .and_then(Value::as_array)
                .map(|cs| cs.iter().filter_map(|c| s(c, "name")).collect())
                .unwrap_or_default(),
        ),
        "usage_update" => Update::Usage {
            used: u.get("used").and_then(Value::as_u64),
            size: u.get("size").and_then(Value::as_u64),
        },
        "session_info_update" => {
            s(u, "title").map_or_else(|| Update::Other(kind.into()), Update::Title)
        }
        other => Update::Other(other.into()),
    }
}

/// Parses a tool call (or its update, or the `toolCall` of a permission request).
pub fn parse_tool(v: &Value) -> ToolCall {
    let content = v
        .get("content")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .filter_map(|c| match c.get("type").and_then(Value::as_str)? {
                    "content" => c.get("content").and_then(text_of).map(ToolContent::Text),
                    "diff" => Some(ToolContent::Diff {
                        path: s(c, "path").unwrap_or_default(),
                        old: s(c, "oldText"),
                        new: s(c, "newText").unwrap_or_default(),
                    }),
                    "terminal" => s(c, "terminalId").map(ToolContent::Terminal),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let locations = v
        .get("locations")
        .and_then(Value::as_array)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| {
                    let p = s(l, "path")?;
                    Some(match l.get("line").and_then(Value::as_u64) {
                        Some(n) => format!("{p}:{n}"),
                        None => p,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ToolCall {
        id: s(v, "toolCallId").unwrap_or_default(),
        title: s(v, "title"),
        kind: s(v, "kind"),
        status: s(v, "status"),
        content,
        locations,
        raw_input: v.get("rawInput").cloned(),
        raw_output: v.get("rawOutput").cloned(),
    }
}

/// Parses the params of a `session/request_permission`.
pub fn parse_permission(p: &Value) -> Option<PermissionRequest> {
    Some(PermissionRequest {
        session: s(p, "sessionId")?,
        tool: parse_tool(p.get("toolCall")?),
        options: p
            .get("options")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|o| {
                Some(PermissionOption {
                    id: o.get("optionId").and_then(Value::as_str)?.to_owned(),
                    name: s(o, "name").unwrap_or_default(),
                    kind: s(o, "kind").unwrap_or_default(),
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chunks_tools_and_plans() {
        assert_eq!(
            parse(
                &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi\u{200B}"}})
            ),
            Update::AgentText("hi".into())
        );
        let t = parse(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "mcp__gmclip__cut_trim",
            "kind": "edit", "status": "pending",
            "content": [{"type": "diff", "path": "/a", "oldText": null, "newText": "x"}],
            "locations": [{"path": "/a", "line": 3}]
        }));
        let Update::ToolCall(t) = t else { panic!() };
        assert!(t.is_gmclip());
        assert_eq!(t.locations, vec!["/a:3"]);
        assert_eq!(
            t.content,
            vec![ToolContent::Diff {
                path: "/a".into(),
                old: None,
                new: "x".into()
            }]
        );
        let p = parse(
            &json!({"sessionUpdate": "plan", "entries": [{"content": "trim", "status": "in_progress", "priority": "high"}]}),
        );
        assert_eq!(
            p,
            Update::Plan(vec![PlanEntry {
                content: "trim".into(),
                status: "in_progress".into(),
                priority: "high".into()
            }])
        );
        assert_eq!(
            parse(&json!({"sessionUpdate": "brand_new"})),
            Update::Other("brand_new".into())
        );
    }

    #[test]
    fn permission_options_by_family() {
        let r = parse_permission(&json!({
            "sessionId": "s", "toolCall": {"toolCallId": "t", "title": "`rm -rf x`"},
            "options": [{"optionId": "a", "name": "Allow", "kind": "allow_once"}, {"optionId": "r", "name": "Reject", "kind": "reject_once"}]
        }))
        .unwrap();
        assert!(!r.tool.is_gmclip());
        assert_eq!(r.option("reject_always").unwrap().id, "r");
        assert_eq!(r.option("allow_once").unwrap().id, "a");
    }
}
