//! A2.17 in the app: a cut's checks add `export_stale` and `sibling_exported` to the core
//! lint, from the export records. On AJ's h79 file.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
use serde_json::json;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
const F: i64 = 705_600_000;

fn rules(c: &AppCore, pid: &str, cut: &str) -> Vec<String> {
    gmclip_app::checks::checks(c, pid, cut).unwrap()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap().to_owned())
        .collect()
}

fn export(c: &AppCore, pid: &str, cut: &str, id: &str) {
    c.add_export(
        pid,
        ExportEntry {
            id: id.into(),
            fps: 30,
            fps_den: 1,
            cut: cut.into(),
            title: "x".into(),
            file: std::env::temp_dir().join(format!("{id}.mp4")),
            frames: 30,
            seconds: 1.0,
            render_sig: Some("an older cut".into()),
            at_ms: 1,
            notes: vec![],
            verdict: None,
            verify: None,
            opening_sig: None,
            opening_label: None,
            framing: None,
            plan_sig: None,
        },
    )
    .unwrap();
}

#[test]
fn stale_export_and_exported_siblings() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-checks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, a) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let r = rules(&c, &pid, &a);
    assert!(
        !r.contains(&"export_stale".to_owned()) && !r.contains(&"sibling_exported".to_owned()),
        "{r:?}"
    );
    // an export of an older version of this cut
    export(&c, &pid, &a, "e-a");
    assert!(rules(&c, &pid, &a).contains(&"export_stale".to_owned()));
    // A2.17: its one-click fix is the export itself ("re-export", SPEC §12.6); the app's
    // fix call runs only registry commands, and only for the finding the page named
    let v = gmclip_app::checks::checks(&c, &pid, &a).unwrap();
    let n = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .position(|f| f["rule"] == "export_stale")
        .unwrap();
    assert_eq!(v["findings"][n]["fix"]["action"], "export");
    assert_eq!(
        gmclip_app::checks::fix(&c, &pid, &a, n, "export_stale")
            .unwrap_err()
            .code,
        "Unsupported"
    );
    assert_eq!(
        gmclip_app::checks::fix(&c, &pid, &a, n, "term_gate")
            .unwrap_err()
            .code,
        "Conflict"
    );
    // a sibling cut over 2 s of the same seconds, exported
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(&a).unwrap();
    let media = doc
        .get::<gmclip_model::entities::Part>(gmclip_model::clock::cut_layout(&doc, cid)[0].part)
        .unwrap()
        .media;
    let b = c
        .call(&pid, "cut_create", json!({"title": "sibling"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": b, "media": media, "src_in": 10 * F, "src_out": 12 * F}),
    )
    .unwrap();
    assert!(
        !rules(&c, &pid, &a).contains(&"sibling_exported".to_owned()),
        "not exported yet"
    );
    export(&c, &pid, &b, "e-b");
    assert!(rules(&c, &pid, &a).contains(&"sibling_exported".to_owned()));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
