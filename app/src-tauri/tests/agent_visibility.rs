//! A2.14 (R-MCP-1c UI, R-CC-4 until M4): the first connection from a new program is an
//! event on the app's bus — once per executable path — listed on the activity rail until
//! the human reads it; and a cut with an agent run open on it says so in its view.

use std::path::Path;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::{CallRequest, Caller};
use gmclip_proto::Base;
use serde_json::json;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn rec(client: &str, identity: &str) -> gmclip_mcp::settings::ConnRecord {
    gmclip_mcp::settings::ConnRecord {
        ts: 1,
        connection: "c-1".into(),
        identity: identity.into(),
        uid: gmclip_mcp::paths::our_uid(),
        pid: None,
        exe: None,
        client_exe: Some(client.into()),
        mode: "mcp".into(),
    }
}

#[test]
fn first_seen_is_one_event_per_program_and_the_rail_lists_it_until_read() {
    let root = std::env::temp_dir().join(format!("gmclip-vis-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.agents.start(
        &c,
        gmclip_mcp::paths::Env {
            app_data: root.join("app"),
            socket: root.join("s").join("s"),
        },
    );
    let sub = c.bus.subscribe("home");
    let h = c.agents.holder().unwrap();
    h.record_connection(&rec("/opt/tools/claude", "local:claude-code"));
    h.record_connection(&rec("/opt/tools/claude", "local:claude-code"));
    h.record_connection(&rec("/opt/tools/claude", "local:renamed"));
    let evs = sub.wait(Duration::from_millis(200)).unwrap_or_default();
    let fs: Vec<_> = evs.iter().filter(|e| e["kind"] == "first_seen").collect();
    assert_eq!(fs.len(), 1, "{evs:?}");
    assert_eq!(fs[0]["identity"], "local:claude-code");
    assert_eq!(fs[0]["exe"], "/opt/tools/claude");
    h.record_connection(&rec("/usr/local/bin/codex", "local:codex"));
    assert_eq!(
        sub.drain()
            .iter()
            .filter(|e| e["kind"] == "first_seen")
            .count(),
        1,
        "a second program"
    );
    let a = gmclip_app::board::activity(&c);
    let listed = a["first_seen"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{a}");
    // read: the rail stops listing what the human acknowledged
    let first = listed[0]["ts"].as_i64().unwrap();
    gmclip_app::board::first_seen_ack(&c, first).unwrap();
    let a = gmclip_app::board::activity(&c);
    assert!(
        a["first_seen"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["ts"].as_i64().unwrap() > first),
        "{a}"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cut_with_an_open_agent_run_says_an_agent_is_working_on_it() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-vis-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    assert!(c.cut_view(&pid, &cut).unwrap()["agent_working"].is_null());
    let h = c.project(&pid).unwrap().handle.clone();
    let agent = |command: &str, args: serde_json::Value| {
        h.call(CallRequest {
            command: command.into(),
            args,
            base: Some(Base::AsOf {
                as_of: h.snapshot().doc.rev(),
            }),
            request_id: None,
            caller: Caller::agent("local:test-agent", "conn-1"),
        })
        .unwrap()
    };
    agent(
        "cut_set",
        json!({"cut": cut, "notes": "AI EDIT PASS: trimmed"}),
    );
    let w = &c.cut_view(&pid, &cut).unwrap()["agent_working"];
    assert_eq!(w["identity"], "local:test-agent", "{w}");
    agent("run_end", json!({}));
    assert!(c.cut_view(&pid, &cut).unwrap()["agent_working"].is_null());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
