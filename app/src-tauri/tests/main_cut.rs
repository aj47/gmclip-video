//! The main cut in the app (decision 0036, AJ 2026-10-18: "work off a main project view
//! each time we start a new project pretty much by importing a file"): the first video
//! makes the project's main cut, a second goes on its end as parts of its own; trim,
//! blade, reorder, Tighten and the transcript work on those parts; the map lists one main
//! cut with both videos. On h79 v4 (60.2 s) and v3 (22.5 s) from the lab media (read
//! only). Skips without the reels.
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::video::AddFile;

fn media(rel: &str) -> String {
    gmclip_model::lab::media_str(rel)
}

fn app(root: &Path) -> Arc<AppCore> {
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

#[test]
fn a_second_video_goes_on_the_end_of_the_main_cut() {
    let (v4, v3) = (media("h79/h79-v4.mp4"), media("h79/h79-v3.mp4"));
    if !Path::new(&v4).is_file() || !Path::new(&v3).is_file() {
        eprintln!("skipped: the h79 reels are not here");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-main-cut-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root);
    // one video: it is the main cut
    let v = gmclip_app::video::open_video(&c, Path::new(&v4)).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    let main = v["cut"].as_str().unwrap().to_owned();
    assert_eq!(c.cut_view(&pid, &main).unwrap()["kind"], "stream");
    let one = c.cut_view(&pid, &main).unwrap()["frames"].as_u64().unwrap();

    // a second video: on the end of the same cut
    let r = gmclip_app::video::add_many(
        &c,
        &pid,
        &[AddFile {
            path: v3.clone().into(),
            voice: None,
        }],
        &[],
    )
    .unwrap();
    assert_eq!(
        r["cut"],
        main.as_str(),
        "the project still opens on its main cut"
    );
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["kind"], "stream");
    let parts = view["parts"].as_array().unwrap().clone();
    assert_eq!(parts.len(), 2);
    assert_ne!(parts[0]["source"], parts[1]["source"], "one part per video");
    let frames = view["frames"].as_u64().unwrap();
    assert_eq!(parts[1]["out"][1].as_u64().unwrap(), frames);
    assert_eq!(
        parts[0]["out"][1].as_u64().unwrap(),
        one,
        "the first video first"
    );
    assert!((2400..2520).contains(&frames), "{frames}");
    // Home lists one recent project of 2 videos, opening on the main cut
    let rec = gmclip_app::video::recent_json(&c);
    assert_eq!(rec["projects"].as_array().unwrap().len(), 1, "{rec}");
    assert_eq!(rec["projects"][0]["videos"], 2);
    assert!(
        rec["projects"][0]["thumb"]
            .as_str()
            .unwrap()
            .ends_with("h79-v4.mp4")
    );
    let again = gmclip_app::video::open_video(&c, Path::new(&v3)).unwrap();
    assert_eq!(again["cut"], main.as_str());
    // the map: one main cut holding both videos, no project timeline
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    assert_eq!(m["streams"].as_array().unwrap().len(), 1);
    assert_eq!(m["stream"]["cut"], main.as_str());
    assert_eq!(m["stream"]["videos"].as_array().unwrap().len(), 2);
    assert!(m.get("timeline").is_none());
    assert!(
        m["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["stream"] == main.as_str())
    );

    // the in edge dragged 90 frames right trims 3 s off the first video's part
    let p1 = parts[0]["id"].as_str().unwrap().to_owned();
    c.trim_by(&pid, &main, &p1, "in", 90).unwrap();
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["frames"].as_u64().unwrap(), frames - 90);
    // the blade splits the first video's part in two
    c.split_at_frame(&pid, &main, 300).unwrap();
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["parts"].as_array().unwrap().len(), 3);
    assert_eq!(view["frames"].as_u64().unwrap(), frames - 90);
    // the second video's part dragged to the front
    let p3 = view["parts"][2]["id"].as_str().unwrap().to_owned();
    gmclip_app::tighten::reorder_to(&c, &pid, &main, &p3, 0).unwrap();
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["parts"][0]["id"], p3.as_str());
    assert_eq!(view["parts"][0]["out"], serde_json::json!([0, 675]));
    // the transcript pane lists both videos as played
    let tv = gmclip_app::transcript::transcript_view(&c, &pid, &main).unwrap();
    let vids = tv["videos"].as_array().unwrap();
    assert_eq!(vids.len(), 2);
    assert!(vids.iter().all(|v| v["in_cut"] == true), "{tv}");
    // removing the second video takes its part off the main cut, ⌘Z brings it back
    let doc_src = m["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("h79-v3.mp4"))
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::video::remove_source(&c, &pid, &doc_src).unwrap();
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["parts"].as_array().unwrap().len(), 2);
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    let view = c.cut_view(&pid, &main).unwrap();
    assert_eq!(view["parts"].as_array().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(&root);
}

/// Waits until every source of the project is analysed (≤ 180 s).
fn wait_analysed(c: &AppCore, pid: &str) {
    let t0 = std::time::Instant::now();
    loop {
        let st = gmclip_app::analysis::status(c, pid);
        let all = st["sources"]
            .as_array()
            .is_some_and(|v| !v.is_empty() && v.iter().all(|s| s["phase"] == "done"));
        if all {
            return;
        }
        assert!(t0.elapsed().as_secs() < 180, "not analysed: {st}");
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// AJ 2026-10-17 ("i tried to find pauses in project video it didnt work"; "i just lost
/// audio playback in preview after a cut"): on a main cut of two videos Tighten finds the
/// pauses in both and cuts them, and after the blade the sound still builds (no fade at
/// the continuous join, rule 7).
#[test]
fn pauses_and_sound_on_the_main_cut() {
    let (v4, v3) = (media("h79/h79-v4.mp4"), media("h79/h79-v3.mp4"));
    if !Path::new(&v4).is_file() || !Path::new(&v3).is_file() {
        eprintln!("skipped: the h79 reels are not here");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-tl-pauses-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root);
    let v = gmclip_app::video::open_video(&c, Path::new(&v4)).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    let r = gmclip_app::video::add_many(
        &c,
        &pid,
        &[AddFile {
            path: v3.into(),
            voice: None,
        }],
        &[],
    )
    .unwrap();
    let t = r["cut"].as_str().unwrap().to_owned();
    wait_analysed(&c, &pid);
    let sound = |c: &AppCore| {
        gmclip_media::audio::AudioPlan::from_plan(&c.planned(&pid, &t).unwrap().plan)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    c.split_at_frame(&pid, &t, 300).unwrap();
    assert_eq!(sound(&c), Ok(()), "the sound after the blade");

    let pv = gmclip_app::tighten::sweep_preview(&c, &pid, &t, "pauses", 0.4, 0.15).unwrap();
    let cuts = pv["cuts"].as_array().unwrap();
    assert!(cuts.len() >= 5, "{pv}");
    assert!(
        cuts.iter().all(|x| x["out"].is_array()),
        "every pause has its place: {pv}"
    );
    let before = c.cut_view(&pid, &t).unwrap()["frames"].as_u64().unwrap();
    let take: Vec<usize> = (0..cuts.len()).collect();
    let ok =
        gmclip_app::tighten::sweep_apply(&c, &pid, &t, pv["preview_id"].as_str().unwrap(), &take)
            .unwrap();
    assert_eq!(ok["applied"].as_u64(), Some(cuts.len() as u64));
    let after = c.cut_view(&pid, &t).unwrap()["frames"].as_u64().unwrap();
    assert!(after < before, "{before} -> {after}");
    assert_eq!(sound(&c), Ok(()), "the sound after the sweep");
    // one ⌘Z puts them all back
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    assert_eq!(
        c.cut_view(&pid, &t).unwrap()["frames"].as_u64(),
        Some(before)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Runs what the app's cut screen calls on a project's main cut, on a copy of a real project
/// (`GMCLIP_PROBE=<app data>/projects/<id>.sqlite`, its home from the `.home` beside
/// it) and prints each reply or error. Diagnostic; ignored by default.
#[test]
#[ignore]
fn probe_a_real_project() {
    let Ok(db) = std::env::var("GMCLIP_PROBE") else {
        return;
    };
    let db = std::path::PathBuf::from(db);
    let id = db.file_stem().unwrap().to_string_lossy().into_owned();
    let src_dir = db.parent().unwrap();
    let home = std::fs::read_to_string(src_dir.join(format!("{id}.home"))).unwrap();
    let home = std::path::PathBuf::from(home.trim());
    let root = std::env::temp_dir().join(format!("gmclip-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let projects = root.join("app/projects");
    std::fs::create_dir_all(&projects).unwrap();
    for ext in ["sqlite", "sqlite-wal", "sqlite-shm"] {
        let f = src_dir.join(format!("{id}.{ext}"));
        if f.is_file() {
            std::fs::copy(&f, projects.join(format!("{id}.{ext}"))).unwrap();
        }
    }
    let new_home = root.join("home.gmclip");
    std::fs::create_dir_all(&new_home).unwrap();
    for e in std::fs::read_dir(&home).unwrap().flatten() {
        std::fs::copy(e.path(), new_home.join(e.file_name())).unwrap();
    }
    std::fs::write(
        projects.join(format!("{id}.home")),
        new_home
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_bytes(),
    )
    .unwrap();
    let c = app(&root);
    let pj = c.open_home(&new_home).unwrap();
    let pid = pj["id"].as_str().unwrap().to_owned();
    let say = |what: &str, r: Result<serde_json::Value, gmclip_app::error::AppError>| match r {
        Ok(v) => {
            let s = v.to_string();
            println!("OK  {what}: {}", &s[..s.len().min(600)]);
        }
        Err(e) => println!("ERR {what}: {e:?}"),
    };
    let t = gmclip_app::video::main_cut(&c, &pid).unwrap().unwrap();
    say(
        "project_map",
        gmclip_app::streamedit::project_map_view(&c, &pid),
    );
    say("cut_view", c.cut_view(&pid, &t));
    say(
        "transcript",
        gmclip_app::transcript::transcript_view(&c, &pid, &t),
    );
    say("cut_words", gmclip_app::words::cut_words(&c, &pid, &t));
    say("lanes", gmclip_app::lanes::view(&c, &pid, &t));
    for k in ["energy", "quiet", "words"] {
        let col = gmclip_app::columns::column(&c, &pid, &t, k);
        println!("COL {k}: {:?} bytes", col.map(|b| b.len()));
    }
    say("audio", gmclip_app::tighten::timeline_audio(&c, &pid, &t));
    let _ = std::fs::remove_dir_all(&root);
}
