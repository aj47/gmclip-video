//! U.11: Owner commands from the app need the native sheet; the code the sheet's button
//! mints is single use and bound to the command and its exact arguments.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

#[test]
fn owner_sheet_and_codes() {
    let root = std::env::temp_dir().join(format!("gmo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let env = gmclip_mcp::paths::Env {
        app_data: root.join("app"),
        socket: root.join("s").join("s"),
    };
    c.agents.start(&c, env);
    let folder = root.join("Footage");
    std::fs::create_dir_all(&folder).unwrap();
    let args = json!({"path": folder.to_string_lossy()});

    // No sheet (no window): refused.
    let e = gmclip_app::owner::request(&c, "roots_add", &args).unwrap_err();
    assert_eq!(e.code, "Unauthorized", "{e:?}");

    // The sheet says yes: the folder becomes a read root.
    gmclip_app::sheet::install(Box::new(|title, text, _| {
        assert!(
            title.contains("folder") && text.contains("may read files"),
            "{title} / {text}"
        );
        true
    }));
    gmclip_app::owner::request(&c, "roots_add", &args).unwrap();
    let st = gmclip_app::owner::settings(&c);
    assert!(st["read_roots"].to_string().contains("Footage"), "{st}");

    // A code is bound to its command and arguments, and used once.
    let code = c.agents.owner_code("roots_remove", &args).unwrap();
    let other = json!({"path": "/tmp"});
    assert!(
        c.agents
            .owner_call_with("roots_remove", &other, &code)
            .is_err(),
        "other arguments"
    );
    let code = c.agents.owner_code("roots_remove", &args).unwrap();
    assert!(
        c.agents
            .owner_call_with("revoke", &json!({"identity": "local:x"}), &code)
            .is_err(),
        "another command"
    );
    let code = c.agents.owner_code("roots_remove", &args).unwrap();
    c.agents
        .owner_call_with("roots_remove", &args, &code)
        .unwrap();
    assert!(
        c.agents
            .owner_call_with("roots_remove", &args, &code)
            .is_err(),
        "replayed"
    );
    assert!(
        !gmclip_app::owner::settings(&c)["read_roots"]
            .to_string()
            .contains("Footage")
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn claude_is_found_without_the_shell_path() {
    let root = std::env::temp_dir().join(format!("gmc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    // Whatever PATH says, an nvm install is found (an app from the Dock has no nvm on PATH).
    let nvm = std::env::var_os("HOME")
        .map(|h| std::path::Path::new(&h).join(".nvm/versions/node"))
        .and_then(|d| std::fs::read_dir(d).ok())
        .is_some_and(|mut vs| vs.any(|v| v.is_ok_and(|v| v.path().join("bin/claude").exists())));
    if nvm {
        let found = gmclip_app::agent::find_claude(&c).unwrap();
        assert!(found.ends_with("bin/claude"), "{}", found.display());
    }
    let st = gmclip_app::agent::client_status(&c);
    assert!(st["found"].is_boolean());
    // A path that is not a program is refused before any sheet.
    assert!(
        gmclip_app::agent::set_client(
            &c,
            "/nonexistent/claude",
            gmclip_ai::launcher::Harness::ClaudeCode
        )
        .is_err()
    );
    // G.02: every client is listed (installed or not); the default is Claude Code until
    // the Owner picks another, and only a known client can be picked
    let cl = gmclip_app::agent::clients_json(&c, true);
    assert_eq!(cl["clients"].as_array().unwrap().len(), 4);
    assert_eq!(cl["default"], "claude-code");
    for x in cl["clients"].as_array().unwrap() {
        let st = x["status"].as_str().unwrap();
        assert!(
            ["ready", "allowed", "check", "blocked", "refused", "missing"].contains(&st),
            "{x}"
        );
        assert_eq!(x["installed"] == false, st == "missing", "{x}");
    }
    assert!(gmclip_app::agent::set_default(&c, "gemini").is_err());
    assert_eq!(
        gmclip_app::agent::default_harness(&c),
        gmclip_ai::launcher::Harness::ClaudeCode
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
