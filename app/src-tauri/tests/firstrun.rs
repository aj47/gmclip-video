//! The stranger setup and the first-run checklist (SPEC §12.5; lane S): four answers →
//! one summary line per default; Apply raises exactly one native sheet listing every Owner
//! change, makes a profile from the kit with the posting answers as content, and applies
//! each change; the same answers again reuse that profile, changed ones make a new one
//! (gmclip never rewrites a profile). The checklist appears after the first real import,
//! ticks steps from what the project holds, and can be dismissed.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::firstrun::{self, Answers};

static SHEETS: AtomicUsize = AtomicUsize::new(0);

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[test]
fn four_answers_one_sheet_a_kit_profile_and_the_checklist() {
    let root = std::env::temp_dir().join(format!("gmclip-firstrun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Recordings")).unwrap();
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.agents.start(
        &c,
        gmclip_mcp::paths::Env {
            app_data: root.join("app"),
            socket: root.join("s").join("s"),
        },
    );
    let pv = firstrun::preview(&c);
    assert_eq!(pv["kits"].as_array().unwrap().len(), 4);
    assert_eq!(pv["platforms"].as_array().unwrap().len(), 4);
    assert!(
        pv["agents"]["clients"].is_array(),
        "the launcher's detected agents"
    );
    assert_eq!(pv["done"], false);
    assert!(pv["models"].is_object());

    let rec = root.join("Recordings").to_string_lossy().into_owned();
    let a = Answers {
        kit: "gaming-variety".into(),
        platforms: vec!["tiktok".into(), "youtube".into()],
        finder: "self".into(),
        harness: None,
        recordings: Some(rec.clone()),
    };
    // a dry run: the summary and the sheet's lines, nothing applied
    let d = firstrun::apply(&c, &a, true).unwrap();
    let ids: Vec<&str> = d["summary"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    for want in [
        "kit",
        "layout",
        "captions",
        "band",
        "platforms",
        "framings",
        "zones",
        "loudness",
        "finder",
        "media",
        "recordings",
    ] {
        assert!(ids.contains(&want), "{want} in {ids:?}");
    }
    assert_eq!(d["sheet"].as_array().unwrap().len(), 2, "{d}");
    assert_eq!(SHEETS.load(Ordering::SeqCst), 0);
    // bad answers are refused before any sheet
    let mut bad = a.clone();
    bad.platforms = vec!["myspace".into()];
    assert!(firstrun::apply(&c, &bad, false).is_err());
    let mut bad = a.clone();
    bad.finder = "agent".into();
    assert!(
        firstrun::apply(&c, &bad, false).is_err(),
        "an agent must be named"
    );

    let shown = Arc::new(Mutex::new(String::new()));
    let s2 = Arc::clone(&shown);
    gmclip_app::sheet::install(Box::new(move |_, text, _| {
        SHEETS.fetch_add(1, Ordering::SeqCst);
        *s2.lock().unwrap() = text;
        true
    }));
    let r = firstrun::apply(&c, &a, false).unwrap();
    assert_eq!(SHEETS.load(Ordering::SeqCst), 1, "exactly one sheet");
    let text = shown.lock().unwrap().clone();
    assert!(text.contains("profile") && text.contains(&rec), "{text}");
    let prof = std::path::PathBuf::from(r["profile"].as_str().unwrap());
    let p = gmclip_core::profile::load(&prof).unwrap();
    assert_eq!(p.kit.as_deref(), Some("gaming-variety"));
    assert_eq!(
        p.content["default_framings"],
        serde_json::json!(["9:16", "16:9"])
    );
    assert_eq!(
        p.content["export_presets"],
        serde_json::json!(["reels-1080x1920", "youtube-1080p"])
    );
    assert_eq!(p.content["loudness"]["integrated_lufs"], -14.0);
    assert_eq!(p.content["default_template"], "gameplay_facecam_split");
    assert!(
        p.content["rubric_ref"]
            .as_str()
            .unwrap()
            .starts_with("gaming-variety@1:")
    );
    assert!(
        p.suggestions.iter().all(|s| s.key == "style_path"),
        "content only"
    );
    let st = gmclip_app::owner::settings(&c);
    assert_eq!(st["profile"].as_str().unwrap(), prof.to_str().unwrap());
    assert_eq!(st["recordings"].as_str().unwrap(), rec);
    assert_eq!(firstrun::preview(&c)["done"], true);

    // the same answers reuse the profile; changed places make a new one
    let r2 = firstrun::apply(&c, &a, false).unwrap();
    assert_eq!(r2["new_profile"], false);
    let mut b = a.clone();
    b.platforms = vec!["reels".into()];
    let r3 = firstrun::apply(&c, &b, false).unwrap();
    assert_eq!(r3["new_profile"], true);
    assert_ne!(r3["profile"], r["profile"]);
    assert_eq!(SHEETS.load(Ordering::SeqCst), 3);

    // the checklist: hidden before the first import, then five steps
    let cl = firstrun::checklist(&c);
    assert_eq!(cl["show"], false);
    if Path::new(video()).is_file() {
        let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
        c.analysis.cancel_project(v["project"].as_str().unwrap());
        let cl = firstrun::checklist(&c);
        assert_eq!(cl["show"], true, "{cl}");
        assert_eq!(cl["steps"].as_array().unwrap().len(), 5);
        // a portrait video opens in a vertical framing: the layout step is done
        assert_eq!(cl["steps"][0]["done"], true, "{cl}");
        assert_eq!(cl["steps"][4]["done"], false);
        let cl = firstrun::dismiss_checklist(&c, true).unwrap();
        assert_eq!(cl["show"], false);
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
