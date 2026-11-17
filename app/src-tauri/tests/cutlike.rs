//! L.05 "New cut like this" on an opened talk: a finished 9:16 cut (screen + facecam
//! layout, an inset on its first take, a second framing, captions seated higher, a text
//! hook, a pill) gives its look — not its parts — to a new cut on another moment: the
//! planned layers, caption style and seat, hook and pill styles and places are the
//! original's; the template is saved by name (content only, R-PROF-4) and applies again,
//! and a project style changed since is left alone (the template's comes in under its own
//! name). Uses a finished reel from the lab media (read only); skips without it or
//! without the speech model and the caption face.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

/// What a plan looks like, without what it shows: the first part's layers, and each text
/// role's style, position and size class.
fn look(c: &AppCore, p: &str, cut: &str) -> Value {
    let pl = c.planned(p, cut).unwrap();
    let v = serde_json::to_value(&*pl.plan).unwrap();
    let layers: Vec<Value> = v["parts"][0]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            json!([
                l["role"],
                l["crop"],
                l["place"],
                l["z"],
                l["visible"],
                l["opacity"]
            ])
        })
        .collect();
    let styles = &v["text_styles"];
    let mut texts: Vec<Value> = v["text_events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| {
            let st = &styles[e["style"].as_str().unwrap_or("")];
            json!([e["role"], st, e["pos"]["x"], e["pos"]["y"]])
        })
        .collect();
    texts.sort_by_key(|x| x.to_string());
    texts.dedup();
    json!({"canvas": v["output"]["canvas"], "fps": v["output"]["fps"], "layers": layers, "texts": texts,
           "loudness": v["audio"]["loudness"]})
}

fn text_of(c: &AppCore, p: &str, cut: &str, role: &str) -> Vec<String> {
    let pl = c.planned(p, cut).unwrap();
    pl.plan
        .text_events
        .as_ref()
        .map(|evs| {
            evs.iter()
                .filter(|e| gmclip_core::planner::text_role_name(&e.role) == role)
                .map(|e| e.runs.iter().map(|r| r.text.as_str()).collect::<String>())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_new_cut_like_this() {
    if !Path::new(video()).is_file() || gmclip_app::fonts::by_name("Neometric Heavy").is_none() {
        eprintln!("skipped: no test video or caption face");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-cutlike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    let t0 = Instant::now();
    loop {
        let st = gmclip_app::analysis::status(&c, &pid);
        let ph = st["sources"][0]["phase"].as_str().unwrap_or("").to_owned();
        if ph == "done" || ph == "failed" || t0.elapsed() > Duration::from_secs(300) {
            if st["sources"][0]["asr_error"].is_string() {
                eprintln!("skipped: no speech model");
                return;
            }
            assert_eq!(ph, "done", "{st}");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // the finished cut: a moment over 1–12 s made a 9:16 short, then shaped
    let m1 = c
        .call(
            &pid,
            "moment_mark",
            json!({"start_s": 1.0, "end_s": 12.0, "snap": "sentence", "from": "tap"}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let made = gmclip_app::cutnew::from_moment(&c, &pid, &m1).unwrap();
    let src_cut = made["cut"].as_str().unwrap().to_owned();
    gmclip_app::short::caption_y_set(&c, &pid, &src_cut, 1300.0).unwrap();
    gmclip_app::short::hook_set(
        &c,
        &pid,
        &src_cut,
        "Every project gets its own VM",
        None,
        Some(3500),
    )
    .unwrap();
    let part = c.cut_view(&pid, &src_cut).unwrap()["parts"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::short::pill_add(&c, &pid, &src_cut, &part, "Why a VM").unwrap();
    gmclip_app::takelayout::detail_add(&c, &pid, &src_cut, &part, None, None).unwrap();
    let f2 = c
        .call(
            &pid,
            "framing_add",
            json!({"cut": src_cut, "name": "face big", "aspect": "9:16"}),
        )
        .unwrap()["output"]["framing"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!f2.is_empty());
    let original = look(&c, &pid, &src_cut);

    // another moment, with its own hook line
    let media = {
        let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
        let m = doc.iter::<gmclip_model::entities::Moment>().next().unwrap();
        serde_json::to_value(m.media).unwrap()
    };
    let m2 = c
        .call(&pid, "moment_add", json!({"media": media, "start_s": 14.0, "end_s": 26.0, "quote": "a", "score": 3, "type": "story",
                                          "why": "test", "text_hook": "A VM per project, really"}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = gmclip_app::cutlike::ipc(
        &c,
        "cut_like",
        Some(&pid),
        &json!({"cut": src_cut, "moment": m2, "save_as": "VM look"}),
    )
    .unwrap();
    let new_cut = r["cut"].as_str().unwrap().to_owned();
    assert_eq!(r["framings"].as_array().unwrap().len(), 2, "{r}");
    let copy = look(&c, &pid, &new_cut);
    assert_eq!(copy, original, "the look carries over");
    // not its parts: one part over the new moment; the hook says the new moment's line
    let cv = c.cut_view(&pid, &new_cut).unwrap();
    assert_eq!(cv["parts"].as_array().unwrap().len(), 1);
    assert_eq!(
        text_of(&c, &pid, &new_cut, "text_hook")
            .join(" ")
            .replace('\n', " "),
        "A VM per project, really"
    );
    assert_eq!(
        text_of(&c, &pid, &new_cut, "section_pill"),
        vec!["Why a VM".to_owned()]
    );
    assert_ne!(
        text_of(&c, &pid, &new_cut, "caption"),
        text_of(&c, &pid, &src_cut, "caption")
    );

    // saved by name, content only
    let list = gmclip_app::cutlike::ipc(&c, "cut_templates", None, &json!({})).unwrap();
    assert_eq!(list["templates"][0]["name"], "VM look");
    let stored = std::fs::read_to_string(root.join("app/cut-templates.json")).unwrap();
    for authority in [
        "harness",
        "budget",
        "export_dir",
        "read_media",
        "scope",
        "roots",
        "\"path\"",
        "style_path",
        "auto_run",
    ] {
        assert!(
            !stored.contains(authority),
            "a template carries no authority field ({authority})"
        );
    }

    // the project's caption style changes afterwards: the template brings its own, under
    // its own name, and the original cut keeps the project's
    let p = c.project(&pid).unwrap();
    let mut speech = p
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Styles>()
        .next()
        .unwrap()
        .entries["Speech"]
        .clone();
    speech.size *= 0.8;
    c.call(
        &pid,
        "text_style_set",
        json!({"name": "Speech", "style": speech}),
    )
    .unwrap();
    let r2 = gmclip_app::cutlike::ipc(
        &c,
        "cut_like",
        Some(&pid),
        &json!({"template": "VM look", "moment": m2}),
    )
    .unwrap();
    assert_eq!(r2["styles_renamed"]["Speech"], "Speech-VM-look", "{r2}");
    let again = look(&c, &pid, r2["cut"].as_str().unwrap());
    assert_eq!(again["layers"], original["layers"]);
    let cap = |l: &Value| {
        l["texts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t[0] == "caption")
            .cloned()
            .unwrap()
    };
    assert_eq!(
        cap(&again)[1],
        cap(&original)[1],
        "the template's caption style"
    );
    assert_ne!(
        cap(&look(&c, &pid, &src_cut))[1],
        cap(&original)[1],
        "the project's own changed"
    );
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
