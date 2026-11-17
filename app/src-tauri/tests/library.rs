//! A2.16: the library index lists every project home in the library folder (open or
//! not) with its exports, picks up a closed project's new export, drops a home that went
//! away, and rebuilds from the homes when the index file is deleted.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

#[test]
fn index_follows_the_homes_and_rebuilds() {
    let root = std::env::temp_dir().join(format!("gmclip-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let a = c.create_project("Alpha").unwrap();
    let b = c.create_project("Beta").unwrap();
    let (aid, bid) = (
        a["id"].as_str().unwrap().to_owned(),
        b["id"].as_str().unwrap().to_owned(),
    );
    let bhome = c.project(&bid).unwrap().home.clone();
    let rows = gmclip_app::library::library(&c).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r["open"] == true && r["exports"] == 0));

    // Beta closed, then an export appears in its home (as another device would write it)
    c.close_project(&bid).unwrap();
    let ex = json!([{"id": "x1", "fps": 30, "cut": "c1", "title": "the cut", "file": "/nowhere.mp4",
                    "frames": 30, "seconds": 1.0, "render_sig": null, "at_ms": 42, "notes": []}]);
    std::fs::write(bhome.join("exports.json"), ex.to_string()).unwrap();
    let rows = gmclip_app::library::library(&c).unwrap();
    let beta = rows.iter().find(|r| r["id"] == bid.as_str()).unwrap();
    assert_eq!(beta["open"], false);
    assert_eq!(beta["exports"], 1);
    assert_eq!(beta["last_export"]["title"], "the cut");

    // the index file deleted: the same rows from the homes
    std::fs::remove_file(root.join("app/library.sqlite")).unwrap();
    let again = gmclip_app::library::library(&c).unwrap();
    assert_eq!(again, rows);

    // a home that went away is dropped
    std::fs::remove_dir_all(&bhome).unwrap();
    let rows = gmclip_app::library::library(&c).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], aid.as_str());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The acceptance's scale: 27 projects (AJ's library has 27 streams, 155
/// story cuts), closed, six exports each. Rows from a fresh index and from a warm one;
/// the warm read is what Home pays on every refresh.
#[test]
fn twenty_seven_homes_within_300_ms() {
    let root = std::env::temp_dir().join(format!("gmclip-lib27-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    for k in 0..27 {
        let p = c.create_project(&format!("stream {k}")).unwrap();
        let id = p["id"].as_str().unwrap().to_owned();
        let home = c.project(&id).unwrap().home.clone();
        c.close_project(&id).unwrap();
        let ex: Vec<serde_json::Value> = (0..6)
            .map(|i| json!({"id": format!("x{k}-{i}"), "fps": 30, "cut": format!("c{i}"), "title": format!("cut {i}"),
                            "file": "/nowhere.mp4", "frames": 1800, "seconds": 60.0, "render_sig": null,
                            "at_ms": 1000 + i, "notes": []}))
            .collect();
        std::fs::write(
            home.join("exports.json"),
            serde_json::to_string(&ex).unwrap(),
        )
        .unwrap();
    }
    let t = std::time::Instant::now();
    let rows = gmclip_app::library::library(&c).unwrap();
    let cold = t.elapsed();
    assert_eq!(rows.len(), 27);
    assert!(rows.iter().all(|r| r["exports"] == 6));
    let mut warm = Vec::new();
    for _ in 0..20 {
        let t = std::time::Instant::now();
        gmclip_app::library::library(&c).unwrap();
        warm.push(t.elapsed());
    }
    warm.sort();
    let p95 = warm[18];
    eprintln!(
        "27 homes: cold {cold:?}, warm p50 {:?} p95 {p95:?}",
        warm[10]
    );
    assert!(
        cold.as_millis() <= 300 && p95.as_millis() <= 300,
        "cold {cold:?} warm p95 {p95:?}"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// U.10 / A2.16: Home speaks for the whole library, open projects and closed ones alike —
/// every project's recent exports (newest first), its staged moments that have no cut yet
/// (Needs you → Grow, a closed project's read from its database through the index), and
/// the work running in any project (a batch here). A moment grown into a cut leaves Needs
/// you; the index keeps the moments when its file is deleted.
#[test]
fn home_lists_exports_moments_and_running_work_across_the_library() {
    let video = gmclip_model::lab::media_str("h79/h79-v4.mp4");
    if !std::path::Path::new(&video).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-libhome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let jobs = std::sync::Arc::new(gmclip_app::jobs::Jobs::default());
    // a stream with two staged moments, then closed
    let v = gmclip_app::video::open_video(&c, std::path::Path::new(&video)).unwrap();
    let sid = v["project"].as_str().unwrap().to_owned();
    let shome = c.project(&sid).unwrap().home.clone();
    let mut staged = Vec::new();
    for (a, b, q) in [
        (2.0, 7.0, "the first staged one"),
        (9.0, 14.0, "the second"),
    ] {
        let m = c
            .call(
                &sid,
                "moment_add",
                json!({"start_s": a, "end_s": b, "quote": q, "score": 4, "type": "result", "why": "home"}),
            )
            .unwrap()["output"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        c.call(
            &sid,
            "moment_set_state",
            json!({"moment": m, "state": "staged"}),
        )
        .unwrap();
        staged.push(m);
    }
    // a new (unstaged) moment is not something that needs you
    c.call(
        &sid,
        "moment_add",
        json!({"start_s": 20.0, "end_s": 24.0, "quote": "just found", "score": 3, "type": "result", "why": "home"}),
    )
    .unwrap();
    c.close_project(&sid).unwrap();
    // an open project with an export, and a closed one with an older export
    let a = c.create_project("Alpha").unwrap();
    let aid = a["id"].as_str().unwrap().to_owned();
    let ahome = c.project(&aid).unwrap().home.clone();
    let b = c.create_project("Beta").unwrap();
    let bid = b["id"].as_str().unwrap().to_owned();
    let bhome = c.project(&bid).unwrap().home.clone();
    c.close_project(&bid).unwrap();
    let ex = |id: &str, title: &str, at: i64| {
        json!([{"id": id, "fps": 30, "cut": "c1", "title": title, "file": "/nowhere.mp4",
                "frames": 30, "seconds": 1.0, "render_sig": null, "at_ms": at, "notes": []}])
        .to_string()
    };
    std::fs::write(bhome.join("exports.json"), ex("b1", "beta's cut", 1_000)).unwrap();
    std::fs::write(ahome.join("exports.json"), ex("a1", "alpha's cut", 2_000)).unwrap();

    let h = gmclip_app::library::home_view(&c, &jobs);
    eprintln!("{}", serde_json::to_string_pretty(&h).unwrap());
    // recent exports across the library, newest first, each with its project
    let recent = h["recent"].as_array().unwrap();
    let titles: Vec<&str> = recent
        .iter()
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["alpha's cut", "beta's cut"]);
    assert_eq!(recent[1]["project_name"], "Beta");
    assert!(
        recent[1]["closed_home"].is_string(),
        "a closed project opens from its home"
    );
    assert!(recent[0]["closed_home"].is_null(), "an open one reveals");
    // the closed stream's staged moments need growing; its new one does not
    let grow: Vec<&serde_json::Value> = h["needs_you"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["action"] == "grow")
        .collect();
    assert_eq!(grow.len(), 2, "{grow:?}");
    assert!(grow.iter().all(|n| n["project"] == sid.as_str()
        && n["closed_home"] == shome.canonicalize().unwrap().to_string_lossy().as_ref()
        && staged.contains(&n["moment"].as_str().unwrap().to_owned())));
    assert_eq!(grow[0]["title"], "the first staged one");
    let row = h["library"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["moments"]["total"], 3);
    assert_eq!(row["moments"]["staged"], 2);
    // the index deleted: the same moments, read again from the project's database
    std::fs::remove_file(root.join("app/library.sqlite")).unwrap();
    assert_eq!(
        gmclip_app::library::home_view(&c, &jobs)["needs_you"],
        h["needs_you"]
    );

    // reopened and one moment grown into a cut: it leaves Needs you
    let p = c.open_home(&shome).unwrap();
    assert_eq!(p["id"], sid.as_str());
    gmclip_app::cutnew::from_moment(&c, &sid, &staged[0]).unwrap();
    let h2 = gmclip_app::library::home_view(&c, &jobs);
    let left: Vec<&str> = h2["needs_you"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["action"] == "grow")
        .map(|n| n["moment"].as_str().unwrap())
        .collect();
    assert_eq!(left, [staged[1].as_str()]);
    assert!(
        h2["needs_you"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["closed_home"].is_null() || n["action"] != "grow"),
        "an open project's moment grows in place"
    );

    // running work in any project: a batch while it runs
    let gate = std::sync::Arc::new(std::sync::Mutex::new(()));
    let held = gate.lock().unwrap();
    let g2 = gate.clone();
    let runner: std::sync::Arc<gmclip_app::batch::RunOne> =
        std::sync::Arc::new(move |cut: &str, _| {
            let _g = g2.lock().unwrap();
            gmclip_app::batch::CutEnd::Done(format!("run-{cut}"))
        });
    let cut = c
        .project(&sid)
        .unwrap()
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Cut>()
        .next()
        .unwrap()
        .id
        .0
        .to_string();
    gmclip_app::batch::start_with(
        &c,
        &sid,
        &[cut],
        "edit_pass",
        std::time::Duration::from_secs(60),
        runner,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let h3 = gmclip_app::library::home_view(&c, &jobs);
    let batch = h3["running"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "batch")
        .cloned();
    let batch = batch.expect("the running batch is listed");
    assert_eq!(batch["project"], sid.as_str());
    assert_eq!(batch["total"], 1);
    assert_eq!(batch["done"], 0);
    assert!(
        batch["project_name"]
            .as_str()
            .is_some_and(|n| n.starts_with("h79"))
    );
    drop(held);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let h4 = gmclip_app::library::home_view(&c, &jobs);
    assert!(
        h4["running"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] != "batch")
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
