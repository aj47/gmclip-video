//! Launched-run policy at the holder (R-AGT-1/3/5, R-OWN-3, R-INJ-4 server side): runs end
//! for their live connections too, registration comes only from the launcher, carries the
//! brief and may only narrow scopes, grants are audited, media tools are withheld without
//! `--media`, human-only commands are refused to every agent, exports stay in
//! `export_dir`, and nothing prints a credential or a code.

mod common;

use common::World;
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{Control, LaunchRegister};
use gmclip_mcp::wire;
use serde_json::{Value, json};

fn reg(fx: &common::Fx, cred: &[u8; 32]) -> LaunchRegister {
    LaunchRegister {
        project: fx.project.clone(),
        cut: fx.cut.clone(),
        credential: wire::hex(cred),
        wall_clock_s: 600,
        media: false,
        private_regions: false,
        code: None,
        brief_sha256: Some("ab".repeat(32)),
        scopes: None,
        full_access: false,
        grant: None,
    }
}

async fn launch(w: &World, r: LaunchRegister, cred: [u8; 32]) -> (String, common::Client) {
    let v = w.control(&Control::LaunchRegister(r)).await;
    assert_eq!(v["ok"], true, "{v}");
    let run = v["result"]["run"].as_str().unwrap().to_owned();
    let c = w
        .client(
            "claude-code",
            Some(&RunCredential::new(run.clone(), cred.to_vec())),
        )
        .await;
    (run, c)
}

fn audit(w: &World) -> String {
    std::fs::read_to_string(w.env.app_data.join("audit.log")).unwrap_or_default()
}

/// The audit log's records with this action and command.
fn audited(w: &World, action: &str, command: &str) -> Vec<Value> {
    audit(w)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["action"] == action && v["command"] == command)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_run_cuts_off_its_live_connection() {
    let w = World::start().await;
    let fx = w.fixture("e.gmclip", 1, "e");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let (run, c) = launch(&w, reg(&fx, &[4; 32]), [4; 32]).await;
    c.ok("cut_get", json!({"cut": fx.cut})).await;
    w.control(&Control::LaunchEnd { run }).await;
    // the same, already-open connection is refused now
    let e = c.call("cut_get", json!({"cut": fx.cut})).await.unwrap_err();
    assert_eq!(e["code"], "Unauthorized", "{e}");
    let e = c
        .call("command_run", json!({"command": "run_end"}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "Unauthorized", "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_binds_the_brief_narrows_scopes_and_is_audited() {
    let w = World::start().await;
    let fx = w.fixture("s.gmclip", 1, "s");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    // widening is refused
    let mut r = reg(&fx, &[1; 32]);
    r.scopes = Some(vec!["read".into(), "read_media".into()]);
    let v = w.control(&Control::LaunchRegister(r)).await;
    assert_eq!(v["error"]["code"], "Invalid", "{v}");
    // the brief hash is required and must be 64 hex
    let mut r = reg(&fx, &[1; 32]);
    r.brief_sha256 = None;
    assert_eq!(
        w.control(&Control::LaunchRegister(r)).await["error"]["code"],
        "Invalid"
    );
    let mut r = reg(&fx, &[1; 32]);
    r.brief_sha256 = Some("xyz".into());
    assert_eq!(
        w.control(&Control::LaunchRegister(r)).await["error"]["code"],
        "Invalid"
    );
    // narrowing to read-only: an edit is then Unauthorized
    let mut r = reg(&fx, &[2; 32]);
    r.scopes = Some(vec!["read".into()]);
    let (run, c) = launch(&w, r, [2; 32]).await;
    let mine = c.ok("cut_get", json!({"cut": fx.cut})).await;
    let as_of = mine["as_of"].as_u64().unwrap();
    let e = c
        .call("cut_edit", json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [{"op": "cut_set", "notes": "x"}]}))
        .await
        .unwrap_err();
    assert_eq!(e, json!({"code": "Unauthorized", "scope": "edit"}));
    let tools = c.tools().await;
    assert!(!tools.contains(&"cut_edit".to_owned()), "{tools:?}");
    // the registration is in the audit log with its scopes and brief hash
    let a = audit(&w);
    assert!(a.contains(&format!("launched:{run} scopes=read")), "{a}");
    assert!(a.contains(&"ab".repeat(32)), "{a}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_tools_are_withheld_without_media_and_grants_are_audited() {
    let w = World::start().await;
    let fx = w.fixture("m.gmclip", 1, "m");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let (_, plain) = launch(&w, reg(&fx, &[3; 32]), [3; 32]).await;
    let tools = plain.tools().await;
    assert!(
        !tools.iter().any(|t| t == "see" || t == "listen"),
        "{tools:?}"
    );
    // exactly the surface its default scopes allow
    let mut want: Vec<String> = gmclip_mcp::tools::defs()
        .into_iter()
        .filter(|d| {
            use gmclip_core::command::Scope as S;
            matches!(d.scope, S::Read | S::Edit | S::Render | S::Ai)
        })
        .map(|d| d.name.to_owned())
        .collect();
    let mut got = tools.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    // with the Owner code: offered, and the grant is audited (ok), a wrong one too
    let args = gmclip_mcp::holder::launch_grant_args(&fx.project, &fx.cut, true, false);
    let mut bad = reg(&fx, &[5; 32]);
    bad.media = true;
    bad.code = Some("WRONGCODE".into());
    assert_eq!(
        w.control(&Control::LaunchRegister(bad)).await["error"]["scope"],
        "owner"
    );
    let code = w.owner_code("launch_grant", &args).await;
    let mut r = reg(&fx, &[5; 32]);
    r.media = true;
    r.code = Some(code.clone());
    let (_, media) = launch(&w, r, [5; 32]).await;
    assert!(media.tools().await.iter().any(|t| t == "see"));
    let outcomes: Vec<String> = audited(&w, "owner_call", "launch_grant")
        .iter()
        .map(|v| v["outcome"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(outcomes, ["refused", "ok"]);
    assert!(!audit(&w).contains(&code), "the code is never logged");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_only_commands_are_refused_to_every_agent() {
    let w = World::start().await;
    let fx = w.fixture("h.gmclip", 1, "h");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let local = w.client("claude-code", None).await;
    let (_, launched) = launch(&w, reg(&fx, &[6; 32]), [6; 32]).await;
    for c in [&local, &launched] {
        for cmd in [
            "feedback_append",
            "Feedback-Append",
            "clippy_send",
            "send_to_clippy",
            "clippy_handoff",
        ] {
            let e = c
                .call(
                    "command_run",
                    json!({"command": cmd, "args": {"text": "x"}}),
                )
                .await
                .unwrap_err();
            assert_eq!(
                e,
                json!({"code": "Unauthorized", "scope": "human_only"}),
                "{cmd}"
            );
        }
        let b = c
            .ok("command_batch", json!({"calls": [{"command": "feedback_append", "args": {}}], "continue_on_error": true}))
            .await;
        assert_eq!(b["results"][0]["error"]["scope"], "human_only", "{b}");
        // never found, never described
        let s = c
            .ok(
                "commands_search",
                json!({"query": "feedback clippy handoff"}),
            )
            .await;
        let names: Vec<&str> = s["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            !names.iter().any(|n| gmclip_mcp::tools::human_only(n)),
            "{names:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_launched_run_reads_only_its_cut_and_export_takes_no_path() {
    let w = World::start().await;
    let fx = w.fixture("r.gmclip", 2, "r");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let (_, c) = launch(&w, reg(&fx, &[7; 32]), [7; 32]).await;
    let ov = c.ok("project_overview", json!({})).await;
    let cuts: Vec<&str> = ov["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert_eq!(cuts, [fx.cut.as_str()]);
    for (tool, args) in [
        ("cut_get", json!({"cut": fx.other_cut})),
        ("lint", json!({"cut": fx.other_cut})),
        ("project_overview", json!({"project": "/somewhere/else"})),
        (
            "command_run",
            json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
        ),
    ] {
        let e = c.call(tool, args.clone()).await.unwrap_err();
        assert_eq!(e["code"], "Unauthorized", "{tool} {args}: {e}");
    }
    // an export takes no path (its output is always under export_dir): a smuggled `path`
    // argument is refused by the schema, never honoured
    let as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    let e = c
        .call(
            "export",
            json!({"cut": fx.cut, "base": {"as_of": as_of}, "path": "/tmp/gmclip-canary-2.mp4"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], "Invalid", "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_only_from_the_launcher() {
    let w = World::start().await;
    let fx = w.fixture("t.gmclip", 1, "t");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    // this test process is the holder's own executable, so it may register (in-process
    // World); a connection whose executable is something else is refused: shown by
    // pointing the Owner's list away and checking the rule directly
    assert!(
        !gmclip_mcp::settings::OwnerSettings::load(&w.env.app_data)
            .trusted_launchers
            .iter()
            .any(|_| true)
    );
    let v = w
        .control(&Control::LaunchRegister(reg(&fx, &[8; 32])))
        .await;
    assert_eq!(v["ok"], true, "{v}");
    let conn = gmclip_mcp::holder::Conn {
        id: "c-test".into(),
        pid: None,
        exe: Some(std::path::PathBuf::from("/usr/bin/python3")),
        client_exe: None,
        identity: std::sync::Mutex::new(Some("local:x".into())),
        launched: None,
        granted: None,
        session: None,
        touched: std::sync::Mutex::new(Default::default()),
        logged: std::sync::Mutex::new(false),
    };
    let r = w
        .holder
        .launch_register(&reg(&fx, &[9; 32]), &conn)
        .unwrap_err();
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"code": "Unauthorized", "scope": "launcher"})
    );
    assert!(audit(&w).contains("refused: not the launcher"));
}

#[test]
fn debug_never_prints_credentials_or_codes() {
    let mut r = LaunchRegister {
        project: "p".into(),
        cut: "c".into(),
        credential: "deadbeef".repeat(8),
        wall_clock_s: 60,
        media: true,
        private_regions: false,
        code: Some("SECRETCODE".into()),
        brief_sha256: None,
        scopes: None,
        full_access: false,
        grant: None,
    };
    let d = format!("{:?} {:?}", r, Control::LaunchRegister(r.clone()));
    assert!(!d.contains("deadbeef") && !d.contains("SECRETCODE"), "{d}");
    r.code = None;
    let d = format!(
        "{:?}",
        Control::OwnerCall {
            command: "roots_add".into(),
            args: Value::Null,
            code: "SECRETCODE".into()
        }
    );
    assert!(!d.contains("SECRETCODE"), "{d}");
}

/// A run bound to one cut learns nothing about another cut's entities through the
/// commands that name them by id: not their revisions (a stale base's `Conflict`), not
/// their values (which validation error comes back), not even when nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_cut_entities_are_no_oracle() {
    let w = World::start().await;
    let fx = w.fixture("o.gmclip", 1, "o");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    // a part on the other cut, added by a local session
    let local = w.client("app", None).await;
    let ov = local.ok("project_overview", json!({})).await;
    let source = ov["sources"][0]["id"].as_str().unwrap().to_owned();
    let as_of = ov["as_of"].as_u64().unwrap();
    let added = local
        .ok(
            "command_run",
            json!({"command": "part_add", "args": {"cut": fx.other_cut, "media": {"source": source}, "src_in": 0, "src_out": 2 * common::SEC}, "base": {"as_of": as_of}}),
        )
        .await;
    let other_part = added["output"]["id"]
        .as_str()
        .or_else(|| added["output"]["part"].as_str())
        .unwrap()
        .to_owned();
    let (_, c) = launch(&w, reg(&fx, &[10; 32]), [10; 32]).await;
    for (args, base) in [
        // a stale base: would have been a Conflict naming the other cut's revisions
        (
            json!({"part": other_part, "src_out": common::SEC}),
            json!({"as_of": 0}),
        ),
        // an out-of-range trim: would have been Invalid (src_out <= src_in)
        (
            json!({"part": other_part, "src_out": -5}),
            json!({"as_of": 9_999_999}),
        ),
        // an in-range one
        (
            json!({"part": other_part, "src_out": common::SEC}),
            json!({"as_of": 9_999_999}),
        ),
    ] {
        let e = c
            .call(
                "command_run",
                json!({"command": "part_trim", "args": args, "base": base}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            e,
            json!({"code": "Unauthorized", "scope": "cut_binding"}),
            "{args} {base}"
        );
    }
    // its own part still trims
    let mine = c.ok("cut_get", json!({"cut": fx.cut})).await;
    let as_of = mine["as_of"].as_u64().unwrap();
    c.ok(
        "command_run",
        json!({"command": "part_trim", "args": {"part": fx.parts[0], "src_out": 100 * common::SEC + common::SEC}, "base": {"as_of": as_of}}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_run_is_audited_and_the_holder_debug_shows_no_codes() {
    let w = World::start().await;
    let fx = w.fixture("d.gmclip", 1, "d");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let (run, _c) = launch(&w, reg(&fx, &[11; 32]), [11; 32]).await;
    let args = gmclip_mcp::holder::launch_grant_args(&fx.project, &fx.cut, true, false);
    let code = w.owner_code("launch_grant", &args).await;
    let d = format!("{:?}", w.holder);
    assert!(!d.contains(&code), "{d}");
    assert!(!d.contains(&wire::hex(&[11; 32])), "{d}");
    w.control(&Control::LaunchEnd { run: run.clone() }).await;
    assert_eq!(
        audited(&w, "launch_end", &format!("launched:{run}")).len(),
        1
    );
}

/// A pass limited to its output (grow-story: `story_suggestions_set` only, R-STORY-1):
/// a part edit is refused, the suggestion write is not, and the limit goes with the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pass_limited_to_its_output_cannot_edit_parts() {
    let w = World::start().await;
    let fx = w.fixture("g.gmclip", 2, "g");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let (run, c) = launch(&w, reg(&fx, &[9; 32]), [9; 32]).await;
    w.holder
        .restrict_commands(&run, &["story_suggestions_set", "run_end"]);
    let e = c
        .call(
            "command_run",
            json!({"command": "part_trim", "args": {"part": fx.parts[0], "src_out": 101 * common::SEC}, "base": {"as_of": 1}}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], "Unauthorized", "{e}");
    let raw = |t: i64| json!({"raw": {"time": {"clock": "src", "t": t * common::SEC}, "reason": "gather"}});
    let read = c.ok("cut_get", json!({"cut": fx.cut})).await;
    let as_of = read["as_of"].as_u64().expect("cut_get says as_of");
    c.ok(
        "command_run",
        json!({"command": "story_suggestions_set", "args": {"cut": fx.cut, "suggestions": [
            {"span": {"start": raw(110), "end": raw(112)}, "role": "setup", "relevance": 0.5, "quote": "x"}
        ]}, "base": {"as_of": as_of}}),
    )
    .await;
    w.control(&Control::LaunchEnd { run: run.clone() }).await;
    assert!(
        w.holder.run_may(&run, "part_trim"),
        "the limit ends with the run"
    );
}

/// Decision 0043: a full-access session's run (gmclip registers it for a human who chose
/// "Full access") sees media without an Owner code and may run project-wide commands on
/// its own project; a plain run bound to one cut may not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_access_run_sees_media_and_works_project_wide() {
    let w = World::start().await;
    let fx = w.fixture("f.gmclip", 1, "f");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let mut r = reg(&fx, &[7; 32]);
    r.full_access = true;
    let (_, full) = launch(&w, r, [7; 32]).await;
    assert!(
        full.tools().await.iter().any(|t| t == "see"),
        "read_media without a code"
    );
    assert!(
        audited(&w, "owner_call", "launch_grant").is_empty(),
        "no Owner code asked"
    );
    let import = json!({"command": "source_import", "args": {"path": "/nonexistent/clip.mp4"}});
    let e = full.call("command_run", import.clone()).await.unwrap_err();
    assert_ne!(e["scope"], "cut_binding", "{e}");
    let (_, plain) = launch(&w, reg(&fx, &[8; 32]), [8; 32]).await;
    let e = plain.call("command_run", import).await.unwrap_err();
    assert_eq!(e["scope"], "cut_binding", "{e}");
}
