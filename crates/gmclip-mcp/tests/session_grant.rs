//! Decision 0039 (AJ's session that could not add his videos): gmclip's library is
//! readable and its projects open by name or id; a session the human started with full
//! access proves a grant whose folders its gmclip tools may read too, while runs bound to
//! one cut get no grant; a refused path says which folders are readable and how the human
//! adds one. Decision 0057: the human's home folder is a read root for every agent by
//! default (the worlds here set no human home unless a test does), and the Owner narrows
//! it by removing it.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{World, code};
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{AppHost, Control, LaunchRegister};
use gmclip_mcp::wire;
use gmclip_proto::CmdError;
use serde_json::{Value, json};

#[derive(Default)]
struct Stub(Mutex<Vec<(String, Value)>>);

const COMMANDS: &[(&str, &str)] = &[
    (
        "videos_add",
        "Adds videos to the project. Args: {files: [{path}]}.",
    ),
    ("project_open_home", "Opens a project folder. Args: {path}."),
];

impl AppHost for Stub {
    fn commands(&self) -> &'static [(&'static str, &'static str)] {
        COMMANDS
    }
    fn call(
        &self,
        _: Option<&str>,
        command: &str,
        args: &Value,
        _: &gmclip_core::Caller,
    ) -> Result<Value, CmdError> {
        self.0
            .lock()
            .unwrap()
            .push((command.to_owned(), args.clone()));
        Ok(json!({"ok": true}))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_projects_open_by_name_id_or_home() {
    let w = World::start().await;
    let lib = w.base.join("Movies/gmclip/Library");
    std::fs::create_dir_all(&lib).unwrap();
    w.holder.set_library_dir(lib.clone());
    let fx = common::fixture(&w.env.app_data, &lib.join("BB IDE Assets.gmclip"), 1, "a");
    let other = common::fixture(&w.env.app_data, &lib.join("bb-mobile2.gmclip"), 1, "b");
    // a symlink in the library is not followed: it makes nothing outside a root
    let outside = w.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.mp4"), b"x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, lib.join("escape.gmclip")).unwrap();

    let c = w.client("claude-code", None).await;
    // listed under `library` (names untrusted)
    let l = c
        .ok("command_run", json!({"command": "project_list"}))
        .await;
    let listed = l["output"]["library"]
        .as_array()
        .or_else(|| l["library"].as_array())
        .unwrap_or_else(|| panic!("{l}"))
        .clone();
    assert_eq!(listed.len(), 2, "{l}");
    assert!(listed.iter().all(|p| p["name"]["untrusted"] == true), "{l}");

    // by its name (as the window shows it), by its folder, by id, by home
    let r = c
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": "bb ide assets"}}),
        )
        .await;
    assert_eq!(
        r["output"]["id"].as_str().or(r["id"].as_str()),
        Some(fx.project.as_str()),
        "{r}"
    );
    let r = c
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": "bb-mobile2.gmclip"}}),
        )
        .await;
    assert!(r.to_string().contains(&other.project), "{r}");
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.project}}),
    )
    .await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": lib.join("bb-mobile2.gmclip").to_str().unwrap()}}),
    )
    .await;
    // an unknown name says how to find one
    let e = c
        .call(
            "command_run",
            json!({"command": "project_open", "args": {"path": "nope"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Invalid", "{e}");
    assert!(e["reason"].as_str().unwrap().contains("library"), "{e}");
    // the symlinked folder stays outside
    #[cfg(unix)]
    {
        let e = c
            .call(
                "command_run",
                json!({"command": "project_open", "args": {"path": lib.join("escape.gmclip").to_str().unwrap()}}),
            )
            .await
            .unwrap_err();
        assert_eq!(code(&e), "Unauthorized", "{e}");
    }

    // the app's project_open_home takes a library name too, handed over as the home
    let stub = Arc::new(Stub::default());
    w.holder.set_app(stub.clone());
    c.ok(
        "command_run",
        json!({"command": "project_open_home", "args": {"path": "BB IDE Assets", "project": fx.project}}),
    )
    .await;
    let home = stub.0.lock().unwrap()[0].1["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        home,
        lib.join("BB IDE Assets.gmclip")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_access_session_reads_its_granted_folders_and_no_one_else_does() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    let stub = Arc::new(Stub::default());
    w.holder.set_app(stub.clone());
    // the human's home stand-in: a Movies folder outside every Owner root
    let home = w.base.join("home");
    let movies = home.join("Movies");
    std::fs::create_dir_all(&movies).unwrap();
    let clip = movies.join("bb-mobile2.mp4");
    std::fs::write(&clip, b"x").unwrap();
    let add = json!({"command": "videos_add", "args": {"project": fx.project, "files": [{"path": clip.to_str().unwrap()}]}});

    let outsider = w.client("some-agent", None).await;
    outsider
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
        )
        .await;
    // an outside agent: refused, and told what it may read and how to get more
    let raw = outsider.call_raw("command_run", add.clone()).await;
    assert_eq!(
        raw["error"],
        json!({"code": "Unauthorized", "scope": "file_roots"}),
        "{raw}"
    );
    let readable = raw["help"]["readable"].as_array().unwrap();
    assert!(
        readable.iter().any(|r| r.as_str() == w.rec.to_str()),
        "the Owner's roots are listed: {raw}"
    );
    assert!(
        !raw.to_string().contains("bb-mobile2"),
        "the refused path is never echoed: {raw}"
    );
    let display = raw["display"].as_str().unwrap();
    assert!(display.contains("Folders agents may read"), "{display}");
    assert!(display.contains(w.rec.to_str().unwrap()), "{display}");

    // the session's grant: the home folder, proved by the session's shim
    let cred = [7u8; 32];
    let id = w
        .holder
        .grant_register(
            &wire::hex(&cred),
            vec![home.clone()],
            Duration::from_secs(600),
        )
        .unwrap();
    let s = w
        .client(
            "claude-code",
            Some(&RunCredential::new(id.clone(), cred.to_vec())),
        )
        .await;
    // a grant is not an identity: the session is still the agent's local one
    assert_eq!(s.identity, None);
    s.ok("command_run", add.clone()).await;
    assert_eq!(
        stub.0.lock().unwrap()[0].1["files"][0]["path"],
        clip.canonicalize().unwrap().to_str().unwrap()
    );
    // ... and source_import the same way
    let e = s
        .call("command_run", json!({"command": "source_import", "project": fx.project, "args": {"path": clip.to_str().unwrap()}}))
        .await
        .unwrap_err();
    assert_ne!(
        code(&e),
        "Unauthorized",
        "past the roots (no importer in a bare holder): {e}"
    );
    // the outsider still is not
    let e = outsider.call("command_run", add.clone()).await.unwrap_err();
    assert_eq!(code(&e), "Unauthorized", "{e}");
    // a wrong credential for the grant is refused at the handshake
    let me = gmclip_mcp::paths::our_exe();
    let bad = gmclip_mcp::client::connect_as(
        &w.env,
        wire::Mode::Mcp,
        false,
        Some(&RunCredential::new(id.clone(), vec![8u8; 32])),
        me.as_deref(),
    )
    .await;
    assert!(bad.is_err());
    // the grant ends with the session: the live connection is back to the roots
    w.holder.grant_end(&id);
    let e = s.call("command_run", add.clone()).await.unwrap_err();
    assert_eq!(code(&e), "Unauthorized", "{e}");
    assert_eq!(stub.0.lock().unwrap().len(), 1);
}

/// Decision 0057 (AJ: "make the default everything available and ship it"): on a fresh
/// read-roots config an outside agent opens a video anywhere in the human's home folder;
/// removing the home folder on the Owner path narrows agents to the listed roots, and
/// adding it back restores the default. Path safety is unchanged: a symlink out of the
/// home and `..` are still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_home_folder_is_readable_by_default_and_the_owner_can_narrow_it() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    let stub = Arc::new(Stub::default());
    w.holder.set_app(stub.clone());
    let home = w.base.join("home");
    let movies = home.join("Movies").join("tasklet-application").join("raw");
    std::fs::create_dir_all(&movies).unwrap();
    let clip = movies.join("take-a.mp4");
    std::fs::write(&clip, b"x").unwrap();
    w.holder.set_human_home(home.clone());
    let add = |p: &std::path::Path| json!({"command": "videos_add", "args": {"project": fx.project, "files": [{"path": p.to_str().unwrap()}]}});

    // an outside agent with no grant and no Owner change
    let outsider = w.client("claude-code", None).await;
    outsider
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
        )
        .await;
    outsider.ok("command_run", add(&clip)).await;
    assert_eq!(
        stub.0.lock().unwrap()[0].1["files"][0]["path"],
        clip.canonicalize().unwrap().to_str().unwrap()
    );
    // path safety stays: a symlink out of the home, and `..`
    let outside = w.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.mp4"), b"x").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, home.join("escape")).unwrap();
        let e = outsider
            .call("command_run", add(&home.join("escape").join("secret.mp4")))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "Unauthorized", "{e}");
    }
    let dotdot = format!("{}/../outside/secret.mp4", home.display());
    let e = outsider
        .call("command_run", json!({"command": "videos_add", "args": {"project": fx.project, "files": [{"path": dotdot}]}}))
        .await
        .unwrap_err();
    assert_ne!(code(&e), "Ok", "{e}");
    assert_eq!(stub.0.lock().unwrap().len(), 1);

    // the Owner removes the home folder: agents are back to the listed roots
    let args = json!({"path": home.to_str().unwrap()});
    let c1 = w.owner_code("roots_remove", &args).await;
    let r = w
        .control(&Control::OwnerCall {
            command: "roots_remove".into(),
            args: args.clone(),
            code: c1,
        })
        .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(w.holder.settings().home_read, Some(false));
    let raw = outsider.call_raw("command_run", add(&clip)).await;
    assert_eq!(
        raw["error"],
        json!({"code": "Unauthorized", "scope": "file_roots"}),
        "{raw}"
    );
    assert!(
        !raw["help"]["readable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str() == home.to_str()),
        "{raw}"
    );
    // ... and adds it back: the default again
    let c2 = w.owner_code("roots_add", &args).await;
    let r = w
        .control(&Control::OwnerCall {
            command: "roots_add".into(),
            args,
            code: c2,
        })
        .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(w.holder.settings().home_read, None);
    assert!(!w.holder.settings().read_roots.contains(&home));
    outsider.ok("command_run", add(&clip)).await;
    assert_eq!(stub.0.lock().unwrap().len(), 2);

    // a folder that is not there is refused (AJ's `~`-less "/Movies/…" was saved)
    let missing =
        json!({"path": w.base.join("Movies/tasklet-application").to_str().unwrap(), "write": true});
    let c3 = w.owner_code("roots_add", &missing).await;
    let r = w
        .control(&Control::OwnerCall {
            command: "roots_add".into(),
            args: missing,
            code: c3,
        })
        .await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(w.holder.settings().write_roots.is_empty());
    // a write root is a read root: with the home off, a folder added "and write" reads
    let mut st = w.holder.settings();
    st.home_read = Some(false);
    st.write_roots = vec![movies.clone()];
    st.save(&w.env.app_data).unwrap();
    outsider.ok("command_run", add(&clip)).await;
    assert_eq!(stub.0.lock().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grants_never_reach_runs_bound_to_one_cut() {
    let w = World::start().await;
    let fx = w.fixture("p.gmclip", 1, "p");
    w.holder.set_app(Arc::new(Stub::default()));
    // a relative or short grant is refused
    assert!(
        w.holder
            .grant_register(
                &wire::hex(&[1u8; 32]),
                vec!["rel".into()],
                Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(
        w.holder
            .grant_register("00", vec![], Duration::from_secs(60))
            .is_err()
    );
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let r = w
        .control(&Control::LaunchRegister(LaunchRegister {
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
        }))
        .await;
    let run = r["result"]["run"].as_str().unwrap().to_owned();
    let l = w
        .client("claude-code", Some(&RunCredential::new(run, vec![6u8; 32])))
        .await;
    let raw = l
        .call_raw(
            "command_run",
            json!({"command": "project_open", "args": {"path": "/etc"}}),
        )
        .await;
    assert_eq!(code(&raw["error"]), "Unauthorized", "{raw}");
}
