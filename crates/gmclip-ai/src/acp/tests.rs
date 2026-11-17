//! A fake ACP agent (a `/bin/sh` script answering by method; request ids are sequential
//! so the canned lines line up) drives a whole session: initialize, new, a prompt with
//! streamed updates and a permission round trip, cancel, stop.

use super::*;
use std::sync::Condvar;
use std::time::Instant;

const AGENT: &str = r#"#!/bin/sh
rec="$1"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" > "$rec/initialize.json"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true}},"authMethods":[{"id":"login","name":"Log in"}],"agentInfo":{"name":"fake","version":"9.9.9"}}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '%s\n' "$line" > "$rec/new.json"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1","modes":{"currentModeId":"default","availableModes":[{"id":"default","name":"Ask"},{"id":"plan","name":"Plan"}]},"configOptions":[{"id":"mdl","name":"Model","category":"model","type":"select","currentValue":"sonnet","options":[{"value":"sonnet","name":"Sonnet"},{"value":"opus","name":"Opus"}]}]}}\n' "$id" ;;
    *'"method":"session/set_config_option"'*)
      printf '%s\n' "$line" > "$rec/model.json"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"configOptions":[]}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' "$line" > "$rec/prompt.json"
      printf 'not json, an adapter log line\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hel"}}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"lo"}}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"`ls`","kind":"execute","status":"pending"}}}\n'
      printf '{"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"t1","title":"`ls`"},"options":[{"optionId":"yes","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}\n'
      IFS= read -r answer
      printf '%s\n' "$answer" > "$rec/permission.json"
      printf '{"jsonrpc":"2.0","id":901,"method":"fs/read_text_file","params":{"sessionId":"s1","path":"/etc/passwd"}}\n'
      IFS= read -r refused
      printf '%s\n' "$refused" > "$rec/fs.json"
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"plan","entries":[{"content":"trim","status":"completed","priority":"high"}]}}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
    *'"method":"session/set_mode"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"no such mode"}}\n' "$id" ;;
    *'"method":"session/cancel"'*)
      printf '%s\n' "$line" > "$rec/cancel.json"
      exit 3 ;;
  esac
done
"#;

#[derive(Default)]
struct Seen {
    updates: Mutex<Vec<(String, Update)>>,
    perms: Mutex<Vec<PermissionRequest>>,
    closed: Mutex<Option<Option<String>>>,
    cv: Condvar,
}

impl Handler for Seen {
    fn update(&self, session: &str, u: Update) {
        self.updates.lock().unwrap().push((session.into(), u));
    }
    fn permission(&self, req: PermissionRequest, reply: PermissionReply) {
        let id = req.option("allow_once").unwrap().id.clone();
        self.perms.lock().unwrap().push(req);
        reply.select(&id);
    }
    fn closed(&self, error: Option<String>) {
        *self.closed.lock().unwrap() = Some(error);
        self.cv.notify_all();
    }
}

fn setup(name: &str) -> (PathBuf, Adapter) {
    let dir = std::env::temp_dir().join(format!("gmclip-acp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("rec")).unwrap();
    let script = dir.join("fake-acp");
    std::fs::write(&script, AGENT).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = Adapter {
        harness: Harness::ClaudeCode,
        program: script,
        args: vec![dir.join("rec").to_string_lossy().into_owned()],
        via: "fake".into(),
        path_dirs: vec![],
    };
    (dir, adapter)
}

fn read(dir: &Path, f: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("rec").join(f)).unwrap()).unwrap()
}

#[test]
fn a_whole_session() {
    let (dir, adapter) = setup("whole");
    let seen = Arc::new(Seen::default());
    let vars = vec![
        (OsString::from("HOME"), OsString::from("/h")),
        (
            OsString::from("ANTHROPIC_API_KEY"),
            OsString::from("sk-secret"),
        ),
        (OsString::from("LC_ALL"), OsString::from("C")),
    ];
    let env = child_env(&vars, &[]);
    assert!(
        env.iter().all(|(k, _)| k != "ANTHROPIC_API_KEY"),
        "API keys never reach the agent"
    );
    assert!(env.iter().any(|(k, _)| k == "LC_ALL") && env.iter().any(|(k, _)| k == "PATH"));
    let conn = Connection::spawn(
        Spawn {
            adapter: &adapter,
            cwd: &dir,
            env,
            stderr_to: Some(&dir.join("stderr.log")),
        },
        seen.clone(),
    )
    .unwrap();

    let init = conn.initialize().unwrap();
    assert!(init.load_session && init.images);
    assert_eq!(init.agent_name.as_deref(), Some("fake"));
    assert_eq!(init.auth_methods[0].id, "login");
    let i = read(&dir, "initialize.json");
    assert_eq!(
        i.pointer("/params/clientCapabilities/terminal"),
        Some(&json!(false))
    );

    let mcp = McpServer {
        name: "gmclip".into(),
        command: "/abs/gmclip".into(),
        args: vec!["mcp".into()],
        env: vec![("GMCLIP_RUN".into(), "r:abc".into())],
    };
    let opened = conn
        .new_session(&dir, &[mcp], Some(json!({"disableBuiltInTools": true})))
        .unwrap();
    assert_eq!(opened.session_id, "s1");
    assert_eq!(opened.current_mode.as_deref(), Some("default"));
    assert_eq!(opened.modes.len(), 2);
    assert_eq!(opened.current_model.as_deref(), Some("sonnet"));
    assert_eq!(
        opened
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["sonnet", "opus"]
    );
    conn.set_model("s1", "opus", opened.model_config.as_deref())
        .unwrap();
    let m = read(&dir, "model.json");
    assert_eq!(m.pointer("/params/configId"), Some(&json!("mdl")));
    assert_eq!(m.pointer("/params/value"), Some(&json!("opus")));
    let n = read(&dir, "new.json");
    assert_eq!(
        n.pointer("/params/mcpServers/0/env/0/name"),
        Some(&json!("GMCLIP_RUN"))
    );
    assert_eq!(
        n.pointer("/params/_meta/disableBuiltInTools"),
        Some(&json!(true))
    );

    let stop = conn
        .prompt("s1", vec![json!({"type": "text", "text": "trim it"})])
        .unwrap();
    assert_eq!(stop, "end_turn");
    let ups = seen.updates.lock().unwrap().clone();
    let text: String = ups
        .iter()
        .filter_map(|(_, u)| match u {
            Update::AgentText(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    assert!(ups.iter().any(
        |(_, u)| matches!(u, Update::ToolCallUpdate(t) if t.status.as_deref() == Some("completed"))
    ));
    assert!(
        ups.iter()
            .any(|(_, u)| matches!(u, Update::Plan(p) if p[0].content == "trim"))
    );
    assert_eq!(
        seen.perms.lock().unwrap()[0].tool.title.as_deref(),
        Some("`ls`")
    );
    assert_eq!(
        read(&dir, "permission.json"),
        json!({"id": 900, "jsonrpc": "2.0", "result": {"outcome": {"outcome": "selected", "optionId": "yes"}}})
    );
    assert_eq!(
        read(&dir, "fs.json").pointer("/error/code"),
        Some(&json!(-32601)),
        "no file system is offered"
    );

    let e = conn.set_mode("s1", "nope").unwrap_err();
    assert_eq!(e.code, -32602);

    conn.cancel("s1").unwrap();
    let g = seen.closed.lock().unwrap();
    let (g, _) = seen
        .cv
        .wait_timeout_while(g, Duration::from_secs(5), |c| c.is_none())
        .unwrap();
    assert!(
        g.as_ref().unwrap().as_ref().unwrap().contains("exited"),
        "an exit nobody asked for is reported"
    );
    drop(g);
    assert!(!conn.alive());
    assert_eq!(
        conn.prompt("s1", vec![]).unwrap_err().code,
        RpcError::CLOSED
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_ends_the_tree_quietly_and_fails_waiters() {
    let (dir, adapter) = setup("stop");
    let seen = Arc::new(Seen::default());
    let conn = Connection::spawn(
        Spawn {
            adapter: &adapter,
            cwd: &dir,
            env: child_env(&[], &[]),
            stderr_to: None,
        },
        seen.clone(),
    )
    .unwrap();
    // a request the fake never answers
    let c2 = conn.clone();
    let waiter = std::thread::spawn(move || c2.request("x/unknown", json!({}), None));
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    conn.stop(Duration::from_secs(2));
    assert_eq!(waiter.join().unwrap().unwrap_err().code, RpcError::CLOSED);
    let g = seen.closed.lock().unwrap();
    let (g, _) = seen
        .cv
        .wait_timeout_while(g, Duration::from_secs(5), |c| c.is_none())
        .unwrap();
    assert_eq!(*g, Some(None), "a stop we asked for is not an error");
    assert!(t0.elapsed() < Duration::from_secs(4));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeouts_and_unanswered_permissions() {
    let (dir, adapter) = setup("timeout");
    struct Drop1;
    impl Handler for Drop1 {
        fn update(&self, _: &str, _: Update) {}
        fn permission(&self, _: PermissionRequest, reply: PermissionReply) {
            drop(reply); // nobody answers: cancelled
        }
        fn closed(&self, _: Option<String>) {}
    }
    let conn = Connection::spawn(
        Spawn {
            adapter: &adapter,
            cwd: &dir,
            env: child_env(&[], &[]),
            stderr_to: None,
        },
        Arc::new(Drop1),
    )
    .unwrap();
    let e = conn
        .request("x/unknown", json!({}), Some(Duration::from_millis(200)))
        .unwrap_err();
    assert_eq!(e.code, RpcError::TIMEOUT);
    conn.initialize().unwrap();
    conn.new_session(&dir, &[], None).unwrap();
    conn.prompt("s1", vec![]).unwrap();
    assert_eq!(
        read(&dir, "permission.json").pointer("/result/outcome/outcome"),
        Some(&json!("cancelled"))
    );
    conn.stop(Duration::from_secs(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adapters_are_found_by_file_then_npx() {
    let dir = std::env::temp_dir().join(format!("gmclip-acp-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mk = |n: &str| {
        let p = bin.join(n);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let path = bin.clone().into_os_string();
    let places = Places {
        path_var: Some(&path),
        home: None,
        system: &[],
    };
    assert!(
        find_adapter(Harness::Codex, &places)
            .unwrap_err()
            .contains("npx")
    );
    assert!(
        find_adapter(Harness::OpenCode, &places)
            .unwrap_err()
            .contains("not installed")
    );
    mk("npx");
    let a = find_adapter(Harness::Codex, &places).unwrap();
    assert_eq!(a.args, vec!["-y", "@zed-industries/codex-acp"]);
    assert_eq!(a.path_dirs, vec![bin.clone()]);
    mk("claude-code-acp");
    let a = find_adapter(Harness::ClaudeCode, &places).unwrap();
    assert_eq!(
        (a.program.clone(), a.via.as_str()),
        (bin.join("claude-code-acp"), "claude-code-acp")
    );
    mk("opencode");
    let a = find_adapter(Harness::OpenCode, &places).unwrap();
    assert_eq!(
        (a.args.clone(), a.via.as_str()),
        (vec!["acp".to_owned()], "opencode acp")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn models_from_either_shape() {
    // the older `models` field
    let (ms, cur, cfg) = models_of(
        &json!({"models": {"currentModelId": "a", "availableModels": [{"modelId": "a", "name": "A"}, {"modelId": "b"}, {"name": "no id"}]}}),
    );
    assert_eq!(
        ms,
        vec![
            Model {
                id: "a".into(),
                name: "A".into()
            },
            Model {
                id: "b".into(),
                name: "b".into()
            }
        ]
    );
    assert_eq!((cur.as_deref(), cfg), (Some("a"), None));
    // a grouped model select wins over `models`
    let (ms, cur, cfg) = models_of(&json!({
        "models": {"availableModels": [{"modelId": "old"}]},
        "configOptions": [
            {"id": "effort", "category": "thought_level", "options": [{"value": "high"}]},
            {"id": "m", "category": "model", "currentValue": "y", "options": [{"group": "g", "name": "G", "options": [{"value": "x", "name": "X"}, {"value": "y", "name": "Y"}]}]}
        ]
    }));
    assert_eq!(
        ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["x", "y"]
    );
    assert_eq!((cur.as_deref(), cfg.as_deref()), (Some("y"), Some("m")));
    // none offered
    assert_eq!(models_of(&json!({})), (vec![], None, None));
}
