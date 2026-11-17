//! Decision 0050: one of the app's commands an agent runs through MCP is the agent's call.
//! Its registry writes are made as the agent, in the agent's run (so the run's undo takes
//! them back and History names the agent), and an agent's `export_start` meets R-LINT-2's
//! gate like the `export` tool.

use std::sync::{Arc, Mutex};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::Caller;
use gmclip_mcp::holder::{AppHost, Conn};
use gmclip_model::entities::Overlay;
use serde_json::json;

fn world(tag: &str) -> (std::path::PathBuf, Arc<AppCore>, String, String) {
    let root = std::env::temp_dir().join(format!("gmclip-calls-{tag}-{}", std::process::id()));
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
    let pid = c.create_project("calls").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cut = c.call(&pid, "cut_create", json!({"title": "t"})).unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = c
        .call(
            &pid,
            "source_add",
            json!({"path": "/m/talk.mp4", "fingerprint": "fp",
                   "probe": {"duration": 10 * 705_600_000_i64, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080,
                              "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}], "audio_tracks": []}}),
        )
        .unwrap()["output"]["id"]
        .clone();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": {"source": source}, "src_in": 0, "src_out": 4 * 705_600_000_i64}),
    )
    .unwrap();
    let framing = c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap()["output"]["id"]
        .clone();
    c.call(
        &pid,
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}),
    )
    .unwrap();
    (root, c, pid, cut)
}

fn hooks(c: &AppCore, pid: &str) -> usize {
    let p = c.project(pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    doc.iter::<Overlay>().count()
}

#[test]
fn an_agents_app_command_writes_in_its_run_and_is_undone_with_it() {
    let (root, c, pid, cut) = world("run");
    let host =
        gmclip_app::agentapp::Host(gmclip_app::ipc::Ctx::headless(c.clone(), Arc::default()));
    let agent = Caller::agent("local:test-agent", "conn-a");
    host.call(
        Some(&pid),
        "hook_set",
        &json!({"project": pid, "cut": cut, "text": "We grew 67 percent"}),
        &agent,
    )
    .unwrap();
    assert_eq!(hooks(&c, &pid), 1);
    let runs = c.project(&pid).unwrap().handle.open_runs();
    let run = runs
        .iter()
        .find(|r| r.identity == "local:test-agent")
        .unwrap_or_else(|| panic!("the hook is not in the agent's run: {runs:?}"))
        .run
        .clone();
    // the window's own calls stay the human's, outside every run
    c.call(&pid, "cut_set", json!({"cut": cut, "notes": "mine"}))
        .unwrap();
    assert_eq!(c.project(&pid).unwrap().handle.open_runs().len(), 1);
    // History's "Undo this agent": the run's undo takes the hook back, the note stays
    c.call(&pid, "run_undo", json!({"run": run})).unwrap();
    assert_eq!(hooks(&c, &pid), 0);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_agents_export_start_meets_the_export_gate() {
    let (root, c, pid, cut) = world("gate");
    let h = c.agents.holder().unwrap();
    h.set_app(Arc::new(gmclip_app::agentapp::Host(
        gmclip_app::ipc::Ctx::headless(c.clone(), Arc::default()),
    )));
    let conn = Conn {
        id: h.next_conn_id(),
        pid: None,
        exe: None,
        client_exe: None,
        identity: Mutex::new(Some("local:test-agent".into())),
        launched: None,
        granted: None,
        session: None,
        touched: Mutex::new(Default::default()),
        logged: Mutex::new(true),
    };
    gmclip_mcp::tools::run_command(
        &h,
        &conn,
        Some(&pid),
        "hook_set",
        json!({"cut": cut, "text": "We grew 67 percent"}),
        None,
        None,
    )
    .unwrap();
    let e = gmclip_mcp::tools::run_command(
        &h,
        &conn,
        Some(&pid),
        "export_start",
        json!({"cut": cut}),
        None,
        None,
    )
    .unwrap_err();
    let gmclip_proto::CmdError::NeedsApproval { plan } = e else {
        panic!("{e:?}")
    };
    assert_eq!(
        plan["blocked_by"][0]["rule"], "number_without_provenance",
        "{plan}"
    );
    assert_eq!(plan["export"]["command"], "export_start", "{plan}");
    // the agent's hook is in its run, which ends when its connection does
    assert!(conn.touched.lock().unwrap().contains(&pid));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0050: an agent's refused export waits on Home's "Needs you" (and so the top
/// bar's badge); "Approve and export" runs it as the human, the ask is done, and the job
/// is the agent's to wait on. A cut changed since the ask is not exported unseen.
#[test]
fn a_refused_export_waits_on_home_and_the_human_exports_it() {
    let (root, c, pid, cut) = world("ask");
    let h = c.agents.holder().unwrap();
    let jobs: Arc<gmclip_app::jobs::Jobs> = Arc::default();
    h.set_app(Arc::new(gmclip_app::agentapp::Host(
        gmclip_app::ipc::Ctx::headless(c.clone(), jobs.clone()),
    )));
    let conn = Conn {
        id: h.next_conn_id(),
        pid: None,
        exe: None,
        client_exe: None,
        identity: Mutex::new(Some("local:test-agent".into())),
        launched: None,
        granted: None,
        session: None,
        touched: Mutex::new(Default::default()),
        logged: Mutex::new(true),
    };
    let run = |command: &str, args: serde_json::Value| {
        gmclip_mcp::tools::call(
            &h,
            &conn,
            "command_run",
            json!({"command": command, "args": args}),
        )
    };
    run(
        "hook_set",
        json!({"cut": cut, "text": "We grew 67 percent"}),
    )
    .unwrap();
    let e = run("export_start", json!({"cut": cut})).unwrap_err();
    let gmclip_proto::CmdError::NeedsApproval { plan } = e else {
        panic!("{e:?}")
    };
    let id = plan["approval"].as_str().unwrap().to_owned();
    let home = gmclip_app::library::home_view(&c, &jobs);
    let ask = home["needs_you"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["action"] == "approval")
        .unwrap_or_else(|| panic!("not on Home: {home}"))
        .clone();
    assert_eq!(ask["approval"], id.as_str());
    assert_eq!(ask["title"], "t", "the cut's own title: {ask}");
    assert_eq!(ask["exportable"], true);
    // the cut changed after the ask: not exported unseen
    c.call(&pid, "cut_set", json!({"cut": cut, "notes": "later"}))
        .unwrap();
    let e = gmclip_app::board::approval_export(&c, &jobs, &id).unwrap_err();
    assert_eq!(e.code, "Conflict", "{e:?}");
    assert_eq!(h.approvals().len(), 1);
    // asked again of the cut as it is now, and approved
    let e = run("export_start", json!({"cut": cut})).unwrap_err();
    let gmclip_proto::CmdError::NeedsApproval { plan } = e else {
        panic!("{e:?}")
    };
    let id = plan["approval"].as_str().unwrap().to_owned();
    h.approval_resolve(None, Some("local:test-agent"));
    let e = run("export_start", json!({"cut": cut})).unwrap_err();
    let gmclip_proto::CmdError::NeedsApproval { plan } = e else {
        panic!("{e:?}")
    };
    let again = plan["approval"].as_str().unwrap().to_owned();
    assert_ne!(again, id, "a dismissed ask is gone");
    let r = gmclip_app::board::approval_export(&c, &jobs, &again).unwrap();
    let job = r["exported"]["job"].as_str().unwrap().to_owned();
    assert!(h.approvals().is_empty(), "the ask is done");
    let home = gmclip_app::library::home_view(&c, &jobs);
    assert!(
        home["needs_you"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["action"] != "approval"),
        "{home}"
    );
    // the human's export is the agent's job to wait on
    let w = run("job_wait", json!({"job": job, "timeout_s": 0})).unwrap();
    assert_eq!(w["kind"], "export", "{w}");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// R-MCP-2 / decision 0050: the agent panel's Claude Code line adds gmclip at user scope,
/// so every folder Claude Code runs in has it, not only the one it was typed in.
#[test]
fn the_claude_code_line_adds_gmclip_for_every_folder() {
    let (root, c, _, _) = world("line");
    let st = c.agents.status(None);
    let line = st["connect"]["claude_code"].as_str().unwrap();
    assert!(
        line.starts_with("claude mcp add --scope user gmclip -- ") && line.ends_with(" mcp"),
        "{line}"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
