//! Rolling backups (R-STORE-5, SPEC §14; lane S): a copy at quit when the project
//! changed, one after 30 minutes of editing (never while an edit is unsaved), restore from
//! a copy (the current state kept first as `before-restore`, so the restore is undoable),
//! and "Free up" removing only rebuildable caches — never projects or backups.

use std::path::Path;
use std::sync::Mutex;

use gmclip_app::backups;
use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::Cut;
use serde_json::json;

fn core_at(root: &Path) -> std::sync::Arc<AppCore> {
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

fn cuts(c: &AppCore, pid: &str) -> usize {
    c.project(pid)
        .unwrap()
        .handle
        .snapshot()
        .doc
        .iter::<Cut>()
        .count()
}

fn edit(c: &AppCore, pid: &str, n: usize) {
    for i in 0..n {
        c.call(pid, "cut_create", json!({"title": format!("cut {i}")}))
            .unwrap();
    }
}

fn wait_saved(c: &AppCore, pid: &str) {
    let p = c.project(pid).unwrap();
    p.handle.flush();
}

#[test]
fn quit_and_timer_copies_restore_and_undo_the_restore() {
    let root = std::env::temp_dir().join(format!("gmclip-bk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = core_at(&root);
    let v = c.create_project("Backed up").unwrap();
    let pid = v["id"].as_str().unwrap().to_owned();
    let home = std::path::PathBuf::from(v["home"].as_str().unwrap_or_default());
    edit(&c, &pid, 3);
    c.shutdown();
    // a copy at quit, of the closed project
    let l = backups::list(&c, &pid).unwrap();
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0].reason, "quit");
    let first = l[0].clone();

    // reopen, edit more: the timer copies after 30 minutes of editing, not before
    let c = core_at(&root);
    let home = if home.as_os_str().is_empty() {
        root.join("lib").join("Backed up.gmclip")
    } else {
        home
    };
    c.open_home(&home).unwrap();
    // an unchanged project gets no new copy at quit
    edit(&c, &pid, 4);
    wait_saved(&c, &pid);
    let seen = Mutex::default();
    let t = gmclip_app::core::now_ms();
    backups::tick(&c, &seen, t);
    backups::tick(&c, &seen, t + backups::EVERY_MS - 1);
    assert_eq!(
        backups::list(&c, &pid).unwrap().len(),
        1,
        "not before 30 min"
    );
    backups::tick(&c, &seen, t + backups::EVERY_MS);
    let l = backups::list(&c, &pid).unwrap();
    assert_eq!(l.len(), 2);
    assert_eq!(l[0].reason, "timer");
    assert_eq!(cuts(&c, &pid), 7);

    // restore the quit copy: 3 cuts, and the 7-cut state kept as before-restore
    let r = backups::restore(&c, &pid, &first.id).unwrap();
    assert_eq!(r["restored"]["id"], first.id.as_str());
    assert!(c.is_open(&pid), "reopened");
    assert_eq!(cuts(&c, &pid), 3);
    let l = backups::list(&c, &pid).unwrap();
    let before = l
        .iter()
        .find(|b| b.reason == "before-restore")
        .unwrap()
        .clone();
    // the restore is undone by restoring what it replaced
    backups::restore(&c, &pid, &before.id).unwrap();
    assert_eq!(cuts(&c, &pid), 7);
    // Home lists the project with its copies
    let ov = backups::overview(&c);
    assert_eq!(ov["projects"][0]["project"], pid.as_str());
    assert!(ov["projects"][0]["backups"].as_array().unwrap().len() >= 3);
    // an unchanged project gets no new copy at quit
    let n = backups::list(&c, &pid).unwrap().len();
    backups::tick(&c, &seen, t + 3 * backups::EVERY_MS);
    c.shutdown();
    assert!(backups::list(&c, &pid).unwrap().len() <= n + 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn free_up_removes_only_rebuildable_caches() {
    let root = std::env::temp_dir().join(format!("gmclip-free-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = core_at(&root);
    let v = c.create_project("Open one").unwrap();
    let open = v["id"].as_str().unwrap().to_owned();
    edit(&c, &open, 2);
    let closed = "01M50G1NEYB1KHQRJ6RYFV05HG";
    // a non-standard app data folder keeps its caches in `<app data>/cache`
    let cache = gmclip_app::proxy::cache_root(&root.join("app"));
    for (id, kind) in [
        (closed, "proxies"),
        (closed, "clips"),
        (open.as_str(), "proxies"),
    ] {
        let d = cache.join(id).join(kind);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x.mp4"), vec![0u8; 100_000]).unwrap();
    }
    std::fs::create_dir_all(cache.join("thumbs")).unwrap();
    std::fs::write(cache.join("thumbs/a.jpg"), vec![0u8; 5_000]).unwrap();
    let u = gmclip_app::storage::usage(&c);
    let row = |id: &str| {
        u["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(row("proxies")["bytes"], 200_000);
    assert_eq!(row("projects")["freeable"], false);
    assert!(row("projects")["bytes"].as_u64().unwrap() > 0);
    // the open project's proxy is kept
    assert_eq!(u["freeable_bytes"], 205_000);
    let projects_before = gmclip_app::storage::size_of(&root.join("app/projects"));
    assert!(gmclip_app::storage::free(&c, &["projects".into()]).is_err());
    let r = gmclip_app::storage::free(&c, &[]).unwrap();
    assert_eq!(r["freed_bytes"], 205_000);
    assert!(
        cache.join(&open).join("proxies/x.mp4").is_file(),
        "in use: kept"
    );
    assert!(!cache.join(closed).join("clips").exists());
    assert!(gmclip_app::storage::size_of(&root.join("app/projects")) >= projects_before);
    assert_eq!(cuts(&c, &open), 2, "the project is untouched");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
