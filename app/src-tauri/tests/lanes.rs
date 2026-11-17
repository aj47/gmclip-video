//! U.08 timeline lanes on AJ's h79 reel (a model cut): the cut's overlays, gain segments
//! and measurements come back on the output clock; an overlay drag and a gain drag are a
//! human gesture (an agent's write meanwhile gets Conflict{by: human}) that commits
//! exactly one transaction and re-plans the cut; Esc (end with no commit) writes nothing.

use std::path::Path;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::{CallRequest, Caller};
use gmclip_proto::{Base, CmdError};
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn setup() -> Option<(Arc<AppCore>, String, String, std::path::PathBuf)> {
    if !Path::new(video()).is_file() {
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-lanes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    Some((
        c,
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
        root,
    ))
}

fn out(v: &Value) -> (i64, i64) {
    (v["out"][0].as_i64().unwrap(), v["out"][1].as_i64().unwrap())
}

#[test]
fn lanes_view_and_one_transaction_drags() {
    let Some((c, pid, cut, root)) = setup() else {
        return;
    };
    let rev = || c.project(&pid).unwrap().handle.snapshot().doc.rev();
    // a hook for 3 s, a gain span and a measured run
    gmclip_app::short::hook_set(&c, &pid, &cut, "My PC made this", None, Some(3000)).unwrap();
    gmclip_app::sound::gain_span(&c, &pid, &cut, 300, 450, 10.0).unwrap();
    gmclip_app::short::run_measure(&c, &pid, &cut, 600, 900).unwrap();
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    eprintln!("{v}");
    assert_eq!(v["overlays_editable"], true);
    assert_eq!(v["gains_editable"], true);
    let hook = &v["overlays"][0];
    assert_eq!(hook["role"], "text_hook");
    assert_eq!(out(hook), (0, 90));
    assert_eq!(out(&v["gains"][0]), (300, 450));
    assert_eq!(v["gains"][0]["gain_db"], 10.0);
    let m = &v["measurements"][0];
    assert_eq!(out(m), (600, 900));
    assert_eq!(m["value"], 10.0);
    assert_eq!(m["evidence"], 2, "two evidence frames");

    // an overlay drag: the agent is refused while the human holds it
    let ov = hook["id"].as_str().unwrap().to_owned();
    let g = gmclip_app::lanes::gesture_begin(&c, &pid, "overlay", &ov).unwrap()["gesture"]
        .as_u64()
        .unwrap();
    let h = c.project(&pid).unwrap().handle.clone();
    let agent = || {
        h.call(CallRequest {
            command: "overlay_set".into(),
            args: json!({"overlay": ov, "end": {"dur_ms": 1000}}),
            base: Some(Base::AsOf {
                as_of: h.snapshot().doc.rev(),
            }),
            request_id: None,
            caller: Caller::agent("local:test-agent", "conn-1"),
        })
    };
    match agent() {
        Err(CmdError::Conflict(cf)) => assert_eq!(cf.entities[0].by, "human"),
        other => panic!("expected Conflict{{by: human}}, got {other:?}"),
    }
    let r0 = rev();
    gmclip_app::lanes::overlay_time(&c, &pid, &cut, &ov, Some(30), Some(150), Some(g)).unwrap();
    assert_eq!(rev(), r0 + 1, "one transaction");
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    assert_eq!(out(&v["overlays"][0]), (30, 150));
    let pl = c.planned(&pid, &cut).unwrap().plan;
    let ev = pl
        .text_events
        .as_ref()
        .unwrap()
        .iter()
        .find(|e| e.id.as_str() == "hook")
        .unwrap();
    assert_eq!(
        (ev.out.start(), ev.out.end()),
        (30, 150),
        "the plan moved too"
    );
    // the gesture ended with the commit: the agent writes again
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(agent().is_ok());
    // an edge drag to the cut's start and end
    let frames = pl.output.frames as i64;
    gmclip_app::lanes::overlay_time(&c, &pid, &cut, &ov, Some(0), Some(frames), None).unwrap();
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    assert_eq!(out(&v["overlays"][0]), (0, frames));

    // a gain drag: move it and change its level, one transaction
    let seg = v["gains"][0]["id"].as_str().unwrap().to_owned();
    let g = gmclip_app::lanes::gesture_begin(&c, &pid, "gain", &seg).unwrap()["gesture"]
        .as_u64()
        .unwrap();
    let r0 = rev();
    gmclip_app::lanes::gain_set(
        &c,
        &pid,
        &cut,
        &seg,
        Some(360),
        Some(540),
        Some(6.04),
        Some(g),
    )
    .unwrap();
    assert_eq!(rev(), r0 + 1, "one transaction");
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    assert_eq!(out(&v["gains"][0]), (360, 540));
    assert_eq!(v["gains"][0]["gain_db"], 6.0);
    let pl = c.planned(&pid, &cut).unwrap().plan;
    let env = &pl.audio.envelopes.as_ref().unwrap()[0];
    assert_eq!(
        (env.out_samples.start(), env.out_samples.end()),
        (360 * 1600, 540 * 1600)
    );
    // Esc: a gesture ended with no commit writes nothing
    let r0 = rev();
    let g = gmclip_app::lanes::gesture_begin(&c, &pid, "gain", &seg).unwrap()["gesture"]
        .as_u64()
        .unwrap();
    gmclip_app::lanes::gesture_end(&c, &pid, g);
    assert_eq!(rev(), r0);
    // undo puts the drag back (one step)
    c.call(&pid, "undo", json!({})).unwrap();
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    assert_eq!(out(&v["gains"][0]), (300, 450), "undo: {v}");
    c.call(&pid, "redo", json!({})).unwrap();
    // an out-edge drag alone
    gmclip_app::lanes::gain_set(&c, &pid, &cut, &seg, None, Some(600), None, None).unwrap();
    let v = gmclip_app::lanes::view(&c, &pid, &cut).unwrap();
    assert_eq!(out(&v["gains"][0]), (360, 600), "{v}");
    // a span outside the cut is refused
    assert!(
        gmclip_app::lanes::gain_set(&c, &pid, &cut, &seg, Some(500), Some(400), None, None)
            .is_err()
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
