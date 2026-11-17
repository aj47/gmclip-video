//! A2.20: a profile's `stream-layouts.json` preset is matched to a source automatically.
//! "Make a cut" on a video with no camera box of its own, under a profile whose preset
//! names that video (a word of its path) on its picture's canvas, gives the source that
//! layout and seeds the face from its camera box; a preset on another canvas or naming
//! another stream is not used. Skips without the test video (the lab media, read only).

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}
const CAM: [f64; 4] = [0.75, 0.7, 0.25, 0.3];

fn preset(name: &str, w: u32, h: u32, cam: [f64; 4]) -> Value {
    json!({"layout": {"id": "01J00000000000000000000001", "name": name, "canvas_size": {"w": w, "h": h},
        "fingerprint": "", "audio_roles": null, "default_template": null,
        "regions": [{"role": "cam", "rect": {"x": cam[0], "y": cam[1], "w": cam[2], "h": cam[3]}},
                    {"role": "screen", "rect": {"x": 0.0, "y": 0.0, "w": 0.75, "h": 1.0}}]},
        "note": format!("{name}.md"), "note_sha256": "00", "evidence": [], "untrusted": true})
}

#[test]
fn make_a_cut_uses_the_profile_layout_that_names_the_source() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-slmatch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let prof = root.join("profile");
    std::fs::create_dir_all(&prof).unwrap();
    std::fs::write(
        prof.join("stream-layouts.json"),
        serde_json::to_vec(
            &json!({"schema": "gmclip.stream-layouts/0", "untrusted": true, "presets": [
                preset("berigame-restream-layout", 2560, 1440, [0.0, 0.76, 0.22, 0.24]),
                preset("r9FexJOoBgY wrong canvas", 2560, 1440, [0.0, 0.0, 0.1, 0.1]),
                preset("r9FexJOoBgY scene", 1920, 1080, CAM),
            ]}),
        )
        .unwrap(),
    )
    .unwrap();
    gmclip_mcp::settings::OwnerSettings {
        profile: Some(prof.clone()),
        ..gmclip_mcp::settings::OwnerSettings::load(&root.join("app"))
    }
    .save(&root.join("app"))
    .unwrap();
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let src = doc.iter::<gmclip_model::entities::Source>().next().unwrap();
    assert!(src.stream_layout.is_none());
    let m = c
        .call(
            &pid,
            "moment_add",
            json!({"media": {"source": src.id}, "start_s": 2.0, "end_s": 9.0,
            "quote": "a", "score": 3, "type": "story", "why": "test"}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let made = gmclip_app::cutnew::from_moment(&c, &pid, &m).unwrap();
    assert_eq!(
        made["stream_layout"]["matched"], "r9FexJOoBgY scene",
        "{made}"
    );
    assert_eq!(made["stream_layout"]["by"], json!(["r9fexjoobgy"]));
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let src = doc.get(src.id).unwrap();
    let sl: &gmclip_model::entities::StreamLayout = doc.get(src.stream_layout.unwrap()).unwrap();
    assert_eq!((sl.canvas_size.w, sl.canvas_size.h), (1920, 1080));
    // the seeded face is the preset's camera box
    let cut = made["cut"].as_str().unwrap();
    // (no transcript here: captions off so the cut plans)
    gmclip_app::short::captions_set(&c, &pid, cut, false).unwrap();
    let cv = c.cut_view(&pid, cut).unwrap();
    let cam: Vec<f64> = cv["parts"][0]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["role"] == "cam")
        .unwrap_or_else(|| panic!("no cam layer: {cv}"))["crop_n"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(
        cam.iter().zip(CAM).all(|(a, b)| (a - b).abs() < 1e-3),
        "{cam:?}"
    );
    // a second cut finds the box already there: nothing more is matched
    let again = gmclip_app::cutnew::from_moment(&c, &pid, &m).unwrap();
    assert_eq!(again["stream_layout"]["matched"], Value::Null);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    assert_eq!(
        doc.iter::<gmclip_model::entities::StreamLayout>().count(),
        1
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
