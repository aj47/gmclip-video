//! Kits seed new framings (SPEC §10.5, §5.7.1 step 2 "template wins"; lane S): with a
//! profile made from a kit in use, a cut's new 9:16 framing takes the kit's stack, its
//! static loudness and its caption preset; with no profile it stays the full-frame crop at
//! AJ's −16 LUFS.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::{Framing, LoudnessMode};

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

fn nine16(
    c: &std::sync::Arc<AppCore>,
    root: &Path,
    kit: Option<&str>,
) -> (usize, Option<f64>, String) {
    if let Some(k) = kit {
        let dir = root.join(format!("Profiles/{k}"));
        gmclip_core::profile::kits::create_from_kit(&dir, k, k).unwrap();
        gmclip_core::profile::register_local(&c.dirs.app_data, &dir).unwrap();
        c.agents
            .owner_call(
                "profile_use",
                &serde_json::json!({"path": dir.to_string_lossy()}),
            )
            .unwrap();
    }
    let v = gmclip_app::video::open_video(c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    let f = gmclip_app::framing::set_output(c, &pid, &cut, "9:16").unwrap();
    let cv = c.cut_view(&pid, &cut).unwrap();
    let layers = cv["parts"][0]["layers"].as_array().unwrap().len();
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let fr = doc
        .get(gmclip_model::Id::<Framing>::new(
            gmclip_model::Ulid::parse(f["framing"].as_str().unwrap()).unwrap(),
        ))
        .unwrap()
        .clone();
    let lufs = match fr.loudness_mode {
        Some(LoudnessMode::Static {
            integrated_lufs, ..
        }) => Some(integrated_lufs),
        _ => None,
    };
    let style = format!("{:?} {:?}", fr.caption_style_ref, fr.caption_grouping);
    (layers, lufs, style)
}

fn core_at(root: &Path) -> std::sync::Arc<AppCore> {
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
    c
}

#[test]
fn a_kit_profile_seeds_the_new_vertical_framing() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let base = std::env::temp_dir().join(format!("gmclip-kits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    // no profile: one full-frame layer at −16 LUFS
    let root = base.join("none");
    let c = core_at(&root);
    let (layers, lufs, _) = nine16(&c, &root, None);
    assert_eq!((layers, lufs), (1, Some(-16.0)));
    c.shutdown();
    // gaming (no camera box on this source → the centred crop), −14 LUFS, Bold pop
    let root = base.join("gaming");
    let c = core_at(&root);
    let (layers, lufs, style) = nine16(&c, &root, Some("gaming-variety"));
    assert_eq!((layers, lufs), (1, Some(-14.0)));
    assert!(style.contains("bold_pop"), "{style}");
    c.shutdown();
    // screen + face: AJ's panel and face, two layers
    let root = base.join("screen");
    let c = core_at(&root);
    let (layers, lufs, _) = nine16(&c, &root, Some("screen-face"));
    assert_eq!((layers, lufs), (2, Some(-14.0)));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&base);
}
