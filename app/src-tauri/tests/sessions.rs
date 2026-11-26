//! Decision 0035: agent sessions over ACP, through the app core, against a fake ACP
//! agent (a `/bin/sh` script; no model, no network). A session starts on a click,
//! streams its transcript on the `agents` scope, approves gmclip's own tools by itself,
//! refuses other tools under gmclip access and asks the human under full access, takes
//! follow-up messages, and is kept on disk when it ends.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_ai::acp::Adapter;
use gmclip_ai::launcher::Harness;
use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::sessions::{self, StartArgs};
use serde_json::{Value, json};

const AGENT: &str = r#"#!/bin/sh
rec="$1"
n=0
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"agentInfo":{"name":"fake-agent","version":"1.0.0"}}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '%s\n' "$line" > "$rec/new.json"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"acp-1","modes":{"currentModeId":"default","availableModes":[{"id":"default","name":"Ask"},{"id":"plan","name":"Plan"}]}}}\n' "$id" ;;
    *'"method":"session/resume"'*)
      printf '%s\n' "$line" > "$rec/resume.json"
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      n=$((n+1))
      printf '%s\n' "$line" > "$rec/prompt$n.json"
      if [ "$n" = 1 ]; then
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Work"}}}}\n'
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ing"}}}}\n'
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"mcp__gmclip__project_list","kind":"other","status":"pending","rawInput":{}}}}\n'
        printf '{"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{"sessionId":"acp-1","toolCall":{"toolCallId":"t1","title":"mcp__gmclip__project_list"},"options":[{"optionId":"ok","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}\n'
        IFS= read -r a; printf '%s\n' "$a" > "$rec/perm1.json"
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"2 projects"}}]}}}\n'
        printf '{"jsonrpc":"2.0","id":901,"method":"session/request_permission","params":{"sessionId":"acp-1","toolCall":{"toolCallId":"t2","title":"`rm -rf x`","kind":"execute"},"options":[{"optionId":"ok","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}\n'
        IFS= read -r a; printf '%s\n' "$a" > "$rec/perm2.json"
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"plan","entries":[{"content":"list projects","status":"completed","priority":"high"}]}}}\n'
      else
        printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Again"}}}}\n'
      fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
"#;

fn setup(tag: &str) -> (Arc<AppCore>, PathBuf, PathBuf) {
    setup_with(tag, AGENT)
}

fn setup_with(tag: &str, agent: &str) -> (Arc<AppCore>, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-sessions-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let rec = root.join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    let script = root.join("fake-acp");
    std::fs::write(&script, agent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.sessions.set_adapter(Adapter {
        harness: Harness::ClaudeCode,
        program: script,
        args: vec![rec.to_string_lossy().into_owned()],
        via: "fake-acp".into(),
        path_dirs: vec![],
    });
    (c, root, rec)
}

fn wait_for(c: &AppCore, id: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let t0 = Instant::now();
    loop {
        let v = sessions::get(c, id).unwrap();
        if ok(&v) {
            return v;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "waiting for {what}: {v:#}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read(rec: &Path, f: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(rec.join(f)).unwrap()).unwrap()
}

fn start(c: &Arc<AppCore>, access: &str, message: &str) -> String {
    let r = sessions::start(
        c,
        &json!({}),
        StartArgs {
            harness: Harness::ClaudeCode,
            access: access.into(),
            message: message.into(),
            media: false,
            folder: None,
            model: None,
        },
    )
    .unwrap();
    assert_eq!(r["session"]["state"], "starting");
    assert_eq!(r["session"]["target"]["kind"], "library");
    r["session"]["id"].as_str().unwrap().to_owned()
}

#[cfg(unix)]
#[test]
fn full_access_asks_the_human_streams_and_takes_follow_ups() {
    let (c, root, rec) = setup("full");
    let sub = c.bus.subscribe("agents");
    let id = start(&c, "full", "tidy the library\nsecond line");

    let v = wait_for(&c, &id, "the permission question", |v| {
        v["session"]["state"] == "waiting"
    });
    let s = &v["session"];
    assert_eq!(s["title"], "tidy the library");
    assert_eq!(s["agent"], "fake-agent 1.0.0");
    assert_eq!(s["mode"], "default");
    assert_eq!(s["modes"].as_array().unwrap().len(), 2);
    assert!(s["resumable"].as_bool().unwrap());
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries[0]["type"], "user");
    assert_eq!(
        entries[1],
        json!({"type": "agent", "text": "Working"}),
        "chunks join"
    );
    let tool = entries.iter().find(|e| e["type"] == "tool").unwrap();
    assert_eq!(tool["gmclip"], true);
    assert_eq!(tool["status"], "completed");
    assert_eq!(tool["detail"], "2 projects");
    // gmclip's own tool was approved without asking
    assert_eq!(
        read(&rec, "perm1.json")["result"]["outcome"]["optionId"],
        "ok"
    );
    let q = entries.iter().find(|e| e["type"] == "permission").unwrap();
    assert_eq!(q["title"], "`rm -rf x`");
    assert!(q["chosen"].is_null());
    let perm = q["id"].as_str().unwrap().to_owned();

    // the session's folder, the agent's settings and gmclip as its MCP server
    let new = read(&rec, "new.json");
    assert!(
        new["params"].get("_meta").is_none(),
        "full access leaves the agent's settings alone"
    );
    let cwd = new["params"]["cwd"].as_str().unwrap();
    assert!(
        Path::new(cwd).starts_with(root.join("app").join("agent-sessions")),
        "{cwd}"
    );
    assert_eq!(new["params"]["mcpServers"][0]["name"], "gmclip");
    assert_eq!(new["params"]["mcpServers"][0]["args"], json!(["mcp"]));
    let first = read(&rec, "prompt1.json")["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        first.contains("You are working in gmclip") && first.contains("tidy the library"),
        "{first}"
    );
    assert!(first.contains("the human approves each one"));

    assert!(
        sessions::answer(&c, &id, &perm, "nope").is_err(),
        "only the agent's options"
    );
    sessions::answer(&c, &id, &perm, "no").unwrap();
    assert!(
        sessions::answer(&c, &id, &perm, "no").is_err(),
        "answered once"
    );
    let v = wait_for(&c, &id, "the turn's end", |v| {
        v["session"]["state"] == "idle"
    });
    assert_eq!(
        read(&rec, "perm2.json")["result"]["outcome"]["optionId"],
        "no"
    );
    let entries = v["entries"].as_array().unwrap();
    let q = entries.iter().find(|e| e["type"] == "permission").unwrap();
    assert_eq!(q["chosen"], "no");
    assert_eq!(entries.last().unwrap()["type"], "plan");
    assert_eq!(v["session"]["stop_reason"], "end_turn");

    // a follow-up goes in as it is (the preamble is the first message's only)
    sessions::prompt(&c, &id, "now the next one").unwrap();
    assert!(
        sessions::prompt(&c, &id, "too soon").is_err(),
        "one turn at a time"
    );
    let v = wait_for(&c, &id, "the second turn", |v| {
        v["session"]["state"] == "idle"
            && v["entries"].as_array().unwrap().last().unwrap()["text"] == "Again"
    });
    assert_eq!(
        read(&rec, "prompt2.json")["params"]["prompt"][0]["text"],
        "now the next one"
    );
    assert!(
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "user" && e["text"] == "now the next one")
    );

    let kinds: Vec<String> = std::iter::from_fn(|| sub.wait(Duration::from_millis(50)))
        .take(50)
        .flatten()
        .filter_map(|e| e["kind"].as_str().map(str::to_owned))
        .collect();
    for k in ["session", "entry", "append"] {
        assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
    }

    assert!(
        sessions::forget(&c, &id).is_err(),
        "a running session is stopped first"
    );
    sessions::stop(&c, &id).unwrap();
    let v = wait_for(&c, &id, "the end", |v| v["session"]["state"] == "ended");
    assert!(v["session"]["run"].is_null());
    assert!(sessions::prompt(&c, &id, "hello?").is_err());
    let kept = root.join("app/agent-sessions").join(format!("{id}.json"));
    assert!(kept.is_file(), "kept on disk");
    assert_eq!(sessions::list(&c)["live"], 0);
    sessions::forget(&c, &id).unwrap();
    assert!(!kept.exists());
    assert!(sessions::get(&c, &id).is_err());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn gmclip_access_refuses_other_tools_and_switches_claude_codes_own_off() {
    let (c, root, rec) = setup("gmclip");
    let id = start(&c, "gmclip", "find the best moment");
    let v = wait_for(&c, &id, "the turn's end", |v| {
        v["session"]["state"] == "idle" && v["session"]["stop_reason"] == "end_turn"
    });
    assert_eq!(
        read(&rec, "perm1.json")["result"]["outcome"]["optionId"],
        "ok",
        "gmclip's tools run"
    );
    assert_eq!(
        read(&rec, "perm2.json")["result"]["outcome"]["optionId"],
        "no",
        "anything else is refused"
    );
    let entries = v["entries"].as_array().unwrap();
    assert!(
        entries.iter().all(|e| e["type"] != "permission"),
        "nothing waits for the human"
    );
    assert!(
        entries.iter().any(|e| e["type"] == "notice"
            && e["text"].as_str().unwrap().contains("gmclip's tools only"))
    );
    let new = read(&rec, "new.json");
    assert_eq!(new["params"]["_meta"]["disableBuiltInTools"], true);
    assert_eq!(
        new["params"]["_meta"]["claudeCode"]["options"]["settingSources"],
        json!([])
    );
    let first = read(&rec, "prompt1.json")["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(first.contains("Use only the gmclip MCP tools."));
    // the folder is an empty run folder, not the app's
    assert!(!Path::new(new["params"]["cwd"].as_str().unwrap()).starts_with(root.join("app")));
    // full-access-only modes stay closed
    assert!(sessions::set_mode(&c, &id, "bypassPermissions").is_err());
    assert!(sessions::set_mode(&c, &id, "missing").is_err());
    c.shutdown();
    assert_eq!(
        sessions::get(&c, &id).unwrap()["session"]["state"],
        "ended",
        "quitting ends live sessions"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bad_starts_are_refused() {
    let (c, root, _) = setup("bad");
    let go = |access: &str, message: &str, target: Value, media: bool| {
        sessions::start(
            &c,
            &target,
            StartArgs {
                harness: Harness::ClaudeCode,
                access: access.into(),
                message: message.into(),
                media,
                folder: None,
                model: None,
            },
        )
    };
    assert!(
        go("gmclip", "  ", json!({}), false).is_err(),
        "empty message"
    );
    assert!(
        go("root", "hi", json!({}), false).is_err(),
        "unknown access"
    );
    assert!(
        go(
            "gmclip",
            "hi",
            json!({"project": "01ARZ3NDEKTSV4RRFFQ69G5FAV"}),
            false
        )
        .is_err(),
        "closed project"
    );
    assert!(
        go("gmclip", "hi", json!({}), true).is_err(),
        "footage is for one cut"
    );
    assert!(sessions::get(&c, "nope").is_err());
    let a = sessions::agents(&c);
    assert_eq!(a["agents"].as_array().unwrap().len(), 4);
    assert_eq!(a["agents"][0]["via"], "fake-acp");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0039: a session the human starts with full access (on anything but one cut)
/// hands its gmclip shim a session grant for the human's home folder, and is told which
/// files gmclip may open; the grant ends with the session. gmclip access gets none.
#[cfg(unix)]
#[test]
fn full_access_sessions_carry_a_grant_for_the_home_folder_until_they_end() {
    let (c, root, rec) = setup("grant");
    let env = gmclip_mcp::paths::Env {
        app_data: root.join("app"),
        socket: root.join("s").join("s"),
    };
    c.agents.start(&c, env.clone());
    let id = start(&c, "bypass", "make a project from my movies");
    wait_for(&c, &id, "the turn's end", |v| {
        v["session"]["state"] == "idle" && v["session"]["stop_reason"] == "end_turn"
    });
    let new = read(&rec, "new.json");
    let mcp = &new["params"]["mcpServers"][0];
    let run = mcp["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "GMCLIP_RUN")
        .and_then(|e| e["value"].as_str())
        .unwrap_or_else(|| panic!("a grant is passed: {mcp}"))
        .to_owned();
    assert!(run.starts_with("grant-"), "{run}");
    let first = read(&rec, "prompt1.json")["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(first.contains("home folder"), "{first}");
    assert!(first.contains("open by name or id"), "{first}");

    // the holder takes the grant's proof while the session lives, and not after
    let (gid, hex) = run.split_once(':').unwrap();
    let cred = gmclip_mcp::client::RunCredential::new(
        gid.to_owned(),
        gmclip_mcp::wire::unhex(hex).unwrap(),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let me = gmclip_mcp::paths::our_exe();
    let connect = || {
        rt.block_on(gmclip_mcp::client::connect_as(
            &env,
            gmclip_mcp::wire::Mode::Mcp,
            false,
            Some(&cred),
            me.as_deref(),
        ))
    };
    assert!(connect().is_ok(), "the grant is live");
    sessions::stop(&c, &id).unwrap();
    assert!(connect().is_err(), "the grant ended with the session");

    // gmclip access: no grant; the agent is told it may still read the home folder,
    // a default read root for every agent (decision 0057)
    let id = start(&c, "gmclip", "make a project from my movies");
    wait_for(&c, &id, "the turn's end", |v| {
        v["session"]["state"] == "idle" && v["session"]["stop_reason"] == "end_turn"
    });
    let new = read(&rec, "new.json");
    assert!(
        !new["params"]["mcpServers"][0]
            .to_string()
            .contains("GMCLIP_RUN"),
        "{new}"
    );
    let first = read(&rec, "prompt1.json")["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        first.contains("anywhere in the human's home folder"),
        "{first}"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn an_ended_session_is_resumed_with_its_transcript_and_the_next_message() {
    // Claude Code's adapter offers `session/resume`, not `session/load`
    let agent = AGENT.replace(
        r#""agentCapabilities":{"loadSession":true}"#,
        r#""agentCapabilities":{"loadSession":false,"sessionCapabilities":{"resume":{}}}"#,
    );
    let (c, root, rec) = setup_with("resume", &agent);
    let id = start(&c, "bypass", "find the best moment");
    let v = wait_for(&c, &id, "the first turn", |v| {
        v["session"]["state"] == "idle"
    });
    assert!(v["session"]["resumable"].as_bool().unwrap());
    let before = v["entries"].as_array().unwrap().len();
    sessions::stop(&c, &id).unwrap();
    wait_for(&c, &id, "the end", |v| v["session"]["state"] == "ended");

    sessions::resume(&c, &id, Some("and now the next one")).unwrap();
    let v = wait_for(&c, &id, "the resumed turn", |v| {
        v["session"]["state"] == "idle"
            && v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "user" && e["text"] == "and now the next one")
    });
    let r = read(&rec, "resume.json");
    assert_eq!(r["params"]["sessionId"], "acp-1");
    assert!(
        r["params"]["cwd"].as_str().unwrap().ends_with(&id),
        "its own folder"
    );
    let entries = v["entries"].as_array().unwrap();
    assert!(entries.len() > before, "the kept transcript goes on");
    assert_eq!(entries[0]["type"], "user", "nothing was cleared");
    assert_eq!(
        read(&rec, "prompt1.json")["params"]["prompt"][0]["text"],
        "and now the next one",
        "the message is the resumed agent's first turn"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn an_agent_that_cannot_resume_says_so() {
    let agent = AGENT.replace(r#"{"loadSession":true}"#, r#"{"loadSession":false}"#);
    let (c, root, _rec) = setup_with("noresume", &agent);
    let id = start(&c, "bypass", "find the best moment");
    let v = wait_for(&c, &id, "the first turn", |v| {
        v["session"]["state"] == "idle"
    });
    assert!(!v["session"]["resumable"].as_bool().unwrap());
    let before = v["entries"].as_array().unwrap().clone();
    sessions::stop(&c, &id).unwrap();
    wait_for(&c, &id, "the end", |v| v["session"]["state"] == "ended");
    sessions::resume(&c, &id, Some("hello?")).unwrap();
    let v = wait_for(&c, &id, "the refusal", |v| {
        !matches!(v["session"]["state"].as_str(), Some("starting"))
            && v["session"]["error"].is_string()
    });
    assert!(
        v["session"]["error"]
            .as_str()
            .unwrap()
            .contains("cannot pick a session up again")
    );
    let after = v["entries"].as_array().unwrap();
    assert_eq!(after[..before.len()], before[..], "the transcript is kept");
    assert!(
        after.iter().all(|e| e["text"] != "hello?"),
        "the message was not sent"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
