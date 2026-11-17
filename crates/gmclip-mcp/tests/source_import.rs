//! Several videos per project through MCP (AJ 2026-10-15): an agent lists a project's
//! videos and adds one with `source_import`, only from inside the agent read roots (the
//! holder checks the file through the root's handle before the app's importer sees it);
//! `source_add` and `source_relink` (paths) stay out of reach.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{World, code};
use gmclip_core::Caller;
use gmclip_mcp::holder::SourceImporter;
use gmclip_proto::CmdError;
use serde_json::{Value, json};

#[derive(Default)]
struct Stub(Mutex<Vec<(String, PathBuf)>>);

impl SourceImporter for Stub {
    fn import(&self, project: &str, path: &Path, _: &Caller) -> Result<Value, CmdError> {
        self.0
            .lock()
            .unwrap()
            .push((project.to_owned(), path.to_owned()));
        Ok(json!({"source": "s1", "stream": "c1", "duration_ms": 1000}))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_add_videos_only_from_the_read_roots() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    let c = w.client("t", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    // listing the videos is a read
    let l = c
        .ok("command_run", json!({"command": "source_list", "args": {}}))
        .await;
    assert_eq!(l["output"]["sources"].as_array().unwrap().len(), 1, "{l}");
    let inside = w.rec.join("second camera.mp4");
    std::fs::write(&inside, b"not really a video").unwrap();
    let outside = w.base.join("elsewhere.mp4");
    std::fs::write(&outside, b"x").unwrap();
    let import =
        |p: &Path| json!({"command": "source_import", "args": {"path": p.to_str().unwrap()}});
    // no importer (a bare holder): said, not guessed
    let e = c.call("command_run", import(&inside)).await.unwrap_err();
    assert_eq!(code(&e), "Unsupported", "{e}");
    let stub = Arc::new(Stub::default());
    w.holder.set_importer(stub.clone());
    // outside the roots, a link out of them, `..`: refused before the importer
    let e = c.call("command_run", import(&outside)).await.unwrap_err();
    assert_eq!(e, json!({"code": "Unauthorized", "scope": "file_roots"}));
    if common::symlink_file(&outside, &w.rec.join("link.mp4")) {
        let e = c
            .call("command_run", import(&w.rec.join("link.mp4")))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "Unauthorized", "{e}");
    }
    let e = c.call("command_run", json!({"command": "source_import", "args": {"path": format!("{}/../elsewhere.mp4", w.rec.display())}})).await.unwrap_err();
    assert_eq!(code(&e), "Invalid");
    // a folder is not a file
    let e = c.call("command_run", import(&w.rec)).await.unwrap_err();
    assert_eq!(code(&e), "Invalid");
    assert!(stub.0.lock().unwrap().is_empty());
    // inside: handed over by its canonical path, for this project
    let r = c.ok("command_run", import(&inside)).await;
    assert_eq!(r["source"], "s1", "{r}");
    let calls = stub.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, fx.project);
    assert_eq!(calls[0].1, inside.canonicalize().unwrap());
    // paths stay human: source_add and source_relink are not reachable
    for cmd in ["source_add", "source_relink"] {
        let e = c
            .call("command_run", json!({"command": cmd, "args": {"path": inside.to_str().unwrap(), "fingerprint": "x", "source": "01J00000000000000000000002"}}))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "NotFound", "{cmd}");
    }
}
