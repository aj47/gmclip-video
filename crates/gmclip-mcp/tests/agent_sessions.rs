//! Agent sessions (decision 0050): an agent that starts `gmclip mcp` (or `gmclip call`) per
//! call names its session with `GMCLIP_SESSION`, and every connection naming it is one
//! identity (`local:session-<label>`) with one run, which outlives each connection. A
//! label is not a credential: it widens no scope, never stands for a launched run, and a
//! malformed one is refused.

mod common;

use std::time::Duration;

use common::{World, code};
use gmclip_mcp::client::{self, RunCredential};
use gmclip_mcp::holder::{Control, LaunchRegister};
use gmclip_mcp::paths;
use gmclip_mcp::wire;
use serde_json::{Value, json};

fn as_of(v: &Value) -> u64 {
    v["as_of"].as_u64().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_is_one_identity_and_one_run_across_connections() {
    let w = World::start().await;
    let fx = w.fixture("s.gmclip", 2, "s");
    let me = paths::our_exe();
    // the thread's first call, on a connection of its own
    let a = w.client_session("claude-code", None, "thread-a").await;
    a.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let r = a.ok("cut_get", json!({"cut": fx.cut})).await;
    a.ok(
        "command_run",
        json!({"command": "cut_set", "args": {"cut": fx.cut, "notes": "one"}, "base": {"as_of": as_of(&r)}}),
    )
    .await;
    a.svc.cancel().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // its next call through `gmclip call`: the same session, the same run
    let (r, failed) = client::call_once(
        &w.env,
        None,
        Some("thread-a"),
        me.as_deref(),
        "cut_get",
        json!({"cut": fx.cut}),
    )
    .await
    .unwrap();
    assert!(!failed, "{r}");
    let (v, failed) = client::call_once(
        &w.env,
        None,
        Some("thread-a"),
        me.as_deref(),
        "cut_set",
        json!({"cut": fx.cut, "notes": "two", "base": {"as_of": as_of(&r)}}),
    )
    .await
    .unwrap();
    assert!(!failed, "{v}");
    let p = w.holder.resolve(Some(&fx.project)).unwrap();
    let runs = p.handle.open_runs();
    assert_eq!(runs.len(), 1, "one run for the session: {runs:?}");
    assert_eq!(runs[0].identity, "local:session-thread-a");
    let run = runs[0].run.clone();
    // a sibling thread is another session: it cannot undo this one's run
    let b = w.client_session("claude-code", None, "thread-b").await;
    let e = b
        .call(
            "command_run",
            json!({"command": "run_undo", "args": {"run": run}, "base": {"as_of": 0}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound", "{e}");
    // a connection of the session under another client name is still the session
    let a2 = w.client_session("a-script", None, "thread-a").await;
    let ended = a2
        .ok(
            "command_run",
            json!({"command": "run_end", "base": {"as_of": 0}}),
        )
        .await;
    assert_eq!(ended["output"]["run"], run, "{ended}");
    assert_eq!(ended["output"]["transactions"], 2, "{ended}");
    a2.ok(
        "command_run",
        json!({"command": "run_undo", "args": {"run": run}, "base": {"as_of": 0}}),
    )
    .await;
    let r = a2.ok("cut_get", json!({"cut": fx.cut})).await;
    assert!(!r.to_string().contains("\"two\""), "{r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_label_widens_nothing_and_never_stands_for_a_launched_run() {
    let w = World::start().await;
    let fx = w.fixture("l.gmclip", 1, "l");
    let me = paths::our_exe();
    // malformed (and a launched run's identity is not a label): refused at the handshake
    for bad in ["launched:x", "", "a b", &"x".repeat(65)] {
        let r = client::connect_with(
            &w.env,
            wire::Mode::Mcp,
            false,
            None,
            Some(bad),
            me.as_deref(),
        )
        .await;
        assert!(
            matches!(r, Err(client::ClientError::Refused(_))),
            "{bad:?}: {r:?}"
        );
    }
    // a session sees what any outside agent sees
    let plain = w.client("claude-code", None).await;
    plain
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
        )
        .await;
    let sess = w.client_session("claude-code", None, "thread-a").await;
    assert_eq!(plain.tools().await, sess.tools().await);
    // a launched run's proof wins over a label: still bound to its cut
    let reg = LaunchRegister {
        project: fx.project.clone(),
        cut: fx.cut.clone(),
        credential: wire::hex(&[7u8; 32]),
        wall_clock_s: 600,
        media: false,
        private_regions: false,
        code: None,
        brief_sha256: Some("0".repeat(64)),
        scopes: None,
        full_access: false,
        grant: None,
    };
    let r = w.control(&Control::LaunchRegister(reg)).await;
    let run = r["result"]["run"].as_str().unwrap().to_owned();
    let l = w
        .client_session(
            "claude-code",
            Some(&RunCredential::new(run.clone(), vec![7u8; 32])),
            "thread-a",
        )
        .await;
    assert_eq!(
        l.identity.as_deref(),
        Some(format!("launched:{run}").as_str())
    );
    let e = l
        .call("cut_get", json!({"cut": fx.other_cut}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unauthorized", "{e}");
}
