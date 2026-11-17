//! Decision 0040: after a restart a session names a project the window has not opened;
//! its Open and "Undo its edits" open it from the library instead of failing with
//! "that project is not open". One not in the library still fails.

use gmclip_app::core::{AppCore, Dirs};

#[test]
fn a_closed_library_project_opens_on_demand() {
    let root = std::env::temp_dir().join(format!("gmclip-reopen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let id = c.create_project("BB IDE New").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.close_project(&id).unwrap();
    assert!(c.project(&id).is_err());
    let p = c.ensure_open(&id).unwrap();
    assert_eq!(p.info.id, id);
    assert!(c.is_open(&id));
    let e = c.ensure_open("01J00000000000000000000000").err().unwrap();
    assert_eq!(e.code, "NotFound");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
