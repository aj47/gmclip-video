//! The app's own commands through MCP (AJ 2026-10-18: "everything we can do with gui we
//! should be able to do with agent"): an agent finds them with `commands_search` and
//! `command_describe` and runs them with `command_run`; their path arguments are checked
//! against the agent read roots and handed over canonical; the reply's text sits under
//! `untrusted`; a run bound to one cut gets none of them; a bare holder has none.

mod common;

use std::sync::{Arc, Mutex};

use common::{World, code};
use gmclip_core::{CallRequest, Caller};
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{AppHost, Control, LaunchRegister};
use gmclip_mcp::wire;
use gmclip_proto::CmdError;
use serde_json::{Value, json};

#[derive(Default)]
struct Stub(Mutex<Vec<(Option<String>, String, Value)>>);

const COMMANDS: &[(&str, &str)] = &[
    (
        "project_new",
        "Creates an empty project and opens it. Args: {name}.",
    ),
    (
        "video_open",
        "Opens a video as a new project. Args: {path}.",
    ),
    (
        "project_consolidate",
        "Copies the project into one folder. Args: {dest?}.",
    ),
];

impl AppHost for Stub {
    fn commands(&self) -> &'static [(&'static str, &'static str)] {
        COMMANDS
    }
    fn call(
        &self,
        project: Option<&str>,
        command: &str,
        args: &Value,
        _caller: &Caller,
    ) -> Result<Value, CmdError> {
        self.0
            .lock()
            .unwrap()
            .push((project.map(str::to_owned), command.to_owned(), args.clone()));
        Ok(json!({"name": "made by the app\u{202E}", "command": command}))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_run_the_apps_commands_inside_their_roots() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    let c = w.client("t", None).await;
    // a bare holder has no app commands
    let e = c
        .call(
            "command_run",
            json!({"command": "project_new", "args": {"name": "x"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound", "{e}");
    let stub = Arc::new(Stub::default());
    w.holder.set_app(stub.clone());

    // found and described like the registry's
    let s = c
        .ok("commands_search", json!({"query": "new project"}))
        .await;
    let hit = s["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "project_new")
        .unwrap_or_else(|| panic!("{s}"));
    assert_eq!(hit["scope"], "app");
    let d = c
        .ok("command_describe", json!({"name": "video_open"}))
        .await;
    assert_eq!(d["scope"], "app", "{d}");

    // no project open: none is passed; the reply's text is canonicalised and untrusted
    let r = c
        .ok(
            "command_run",
            json!({"command": "project_new", "args": {"name": "BB IDE Assets"}}),
        )
        .await;
    assert_eq!(r["untrusted"]["command"], "project_new", "{r}");
    assert!(
        !r["untrusted"]["name"]
            .as_str()
            .unwrap()
            .contains('\u{202E}')
    );
    {
        let calls = stub.0.lock().unwrap();
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].2["name"], "BB IDE Assets");
    }

    // paths: outside the roots refused before the app sees them; inside, canonical
    let outside = w.base.join("elsewhere.mp4");
    std::fs::write(&outside, b"x").unwrap();
    let e = c
        .call(
            "command_run",
            json!({"command": "video_open", "args": {"path": outside.to_str().unwrap()}}),
        )
        .await
        .unwrap_err();
    assert_eq!(e, json!({"code": "Unauthorized", "scope": "file_roots"}));
    let e = c
        .call("command_run", json!({"command": "video_open", "args": {"path": format!("{}/../elsewhere.mp4", w.rec.display())}}))
        .await
        .unwrap_err();
    assert!(matches!(code(&e), "Invalid" | "Unauthorized"), "{e}");
    assert_eq!(stub.0.lock().unwrap().len(), 1);
    let inside = w.rec.join("take 2.mp4");
    std::fs::write(&inside, b"x").unwrap();
    c.ok(
        "command_run",
        json!({"command": "video_open", "args": {"path": inside.to_str().unwrap()}}),
    )
    .await;
    assert_eq!(
        stub.0.lock().unwrap()[1].2["path"],
        inside.canonicalize().unwrap().to_str().unwrap()
    );

    // with a project open it is passed along; a new folder inside the roots is allowed
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let dest = w.rec.join("consolidated");
    c.ok(
        "command_run",
        json!({"command": "project_consolidate", "args": {"dest": dest.to_str().unwrap()}}),
    )
    .await;
    {
        let calls = stub.0.lock().unwrap();
        assert_eq!(calls[2].0.as_deref(), Some(fx.project.as_str()));
        assert_eq!(calls[2].2["project"], fx.project);
        assert_eq!(
            calls[2].2["dest"],
            w.rec
                .canonicalize()
                .unwrap()
                .join("consolidated")
                .to_str()
                .unwrap()
        );
    }

    // a run bound to one cut: none of the app's commands, and search does not list them
    let reg = LaunchRegister {
        project: fx.project.clone(),
        cut: fx.cut.clone(),
        credential: wire::hex(&[6u8; 32]),
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
        .client("claude-code", Some(&RunCredential::new(run, vec![6u8; 32])))
        .await;
    let e = l
        .call(
            "command_run",
            json!({"command": "project_new", "args": {"name": "x"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unauthorized", "{e}");
    let s = l
        .ok("commands_search", json!({"query": "new project"}))
        .await;
    assert!(
        s["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["scope"] != "app"),
        "{s}"
    );
    assert_eq!(stub.0.lock().unwrap().len(), 3);
}

/// The app as it runs an agent's command (decision 0050): `hook_set` writes through the
/// project's actor as the caller it is handed; `export_start` starts job `job-7`, done at
/// once. What the app's IPC does with the caller is `app/src-tauri`'s to test.
struct Writes {
    holder: Arc<gmclip_mcp::holder::Holder>,
    exports: Mutex<Vec<String>>,
}

/// A one-second plan of one source, for the app's finished export.
fn plan() -> gmclip_core::gmclip_plan::Plan {
    let v = json!({
        "schema": "gmclip.renderplan/0",
        "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
            "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2}, "frames": 30},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "s", "path": "/m/s.mp4"}],
        "parts": [{"id": "p1", "out": [0, 30], "picture": {"source": "s", "first_frame": 0}, "audio": {"source": "s", "first_sample": 0, "rate": 48000},
                   "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": 640, "h": 360}, "place": {"x": 0, "y": 656, "w": 1080, "h": 607.5}, "z": 0}]}],
        "audio": {"loudness": {"mode": "as_recorded"}},
    });
    gmclip_core::gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
}

const WRITES: &[(&str, &str)] = &[
    ("hook_set", "Sets the hook text. Args: {cut, text}."),
    ("export_start", "Exports a cut (a job). Args: {cut}."),
];

impl AppHost for Writes {
    fn commands(&self) -> &'static [(&'static str, &'static str)] {
        WRITES
    }
    fn call(
        &self,
        project: Option<&str>,
        command: &str,
        args: &Value,
        caller: &Caller,
    ) -> Result<Value, CmdError> {
        let p = self.holder.resolve(project)?;
        if command == "export_start" {
            let cut = args["cut"].as_str().unwrap_or_default().to_owned();
            self.exports.lock().unwrap().push(cut);
            return Ok(json!({"job": "job-7"}));
        }
        let as_of = p.handle.snapshot().doc.rev();
        let ok = p.handle.call(CallRequest {
            command: "overlay_add".into(),
            args: json!({"cut": args["cut"], "kind": "text", "start": {"cut_start": {"cut": args["cut"]}}, "end": {"dur_ms": 1000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9},
                "text": {"runs": [{"text": args["text"]}], "style": {"font": "f", "size": 80.0, "fill": "#ffffff", "outline": null, "shadow": null, "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}}}),
            base: (caller.kind == gmclip_proto::CallerKind::Agent)
                .then_some(gmclip_proto::Base::AsOf { as_of }),
            request_id: None,
            caller: caller.clone(),
        })?;
        Ok(ok.output)
    }
    fn job(&self, id: &str, _wait: std::time::Duration) -> Option<gmclip_mcp::holder::AppJob> {
        let p = self.holder.project_list().pop()?;
        let cut = self.exports.lock().unwrap().last()?.clone();
        (id == "job-7").then(|| gmclip_mcp::holder::AppJob {
            state: "done".into(),
            project: p.id,
            cut,
            progress: None,
            result: Some(json!({"out": "/x.mp4"})),
            plan: Some(Arc::new(plan())),
            out: Some("/x.mp4".into()),
        })
    }
}

/// Decision 0050: an app command an agent runs is the agent's call. Its edits land in the
/// agent's run (so `run_undo` takes them back); `export_start` meets R-LINT-2's gate (a
/// number on screen with no provenance blocks it, unseen by the app); the export it then
/// starts is one of its jobs for `job_wait` and `verify`, and nobody else's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_command_is_the_agents_own_call() {
    let w = World::start().await;
    let fx = w.fixture("own.gmclip", 1, "own");
    let app = Arc::new(Writes {
        holder: Arc::clone(&w.holder),
        exports: Mutex::default(),
    });
    w.holder.set_app(app.clone());
    let c = w.client("t", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    c.ok(
        "command_run",
        json!({"command": "hook_set", "args": {"cut": fx.cut, "text": "67 seconds"}}),
    )
    .await;
    // the hook blocks the agent's export from the app's command too
    let e = c
        .call(
            "command_run",
            json!({"command": "export_start", "args": {"cut": fx.cut}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NeedsApproval", "{e}");
    assert_eq!(
        e["plan"]["blocked_by"][0]["rule"], "number_without_provenance",
        "{e}"
    );
    assert_eq!(e["plan"]["export"]["command"], "export_start", "{e}");
    assert!(
        app.exports.lock().unwrap().is_empty(),
        "the app never saw it"
    );
    // the hook was the agent's: in its run, undone with it
    let ended = c
        .ok(
            "command_run",
            json!({"command": "run_end", "base": {"as_of": 0}}),
        )
        .await;
    let run = ended["output"]["run"]
        .as_str()
        .unwrap_or_else(|| panic!("the app's edit opened no run: {ended}"))
        .to_owned();
    c.ok(
        "command_run",
        json!({"command": "run_undo", "args": {"run": run}, "base": {"as_of": 0}}),
    )
    .await;
    let look = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "overlays"}))
        .await;
    assert!(
        !look.to_string().contains("67 seconds"),
        "the run's undo took the hook back: {look}"
    );
    // nothing blocks it now: the app's job is the agent's to wait on and verify
    let r = c
        .ok(
            "command_run",
            json!({"command": "export_start", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(r["untrusted"]["job"], "job-7", "{r}");
    let j = c
        .ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": "job-7"}}),
        )
        .await;
    assert_eq!(j["state"], "done", "{j}");
    let e = c
        .call(
            "verify",
            json!({"cut": fx.cut, "kind": "export", "export": "job-7"}),
        )
        .await
        .unwrap_err();
    assert_ne!(code(&e), "NotFound", "verify finds the app's job: {e}");
    // another agent does not see it
    let other = w.client("someone-else", None).await;
    let e = other
        .call(
            "command_run",
            json!({"command": "job_wait", "args": {"job": "job-7"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound", "{e}");
}
