//! A2.09 in the app: the rest of the style library (SPEC §8) app-wide — an item saved
//! "as mine" lands in the project's library and in the active profile's, survives being
//! forgotten in the project's list, and applies from mine to a project without being
//! stored there. On AJ's h79 file (read only).

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::studio::dispatch;
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

/// The saved items' `kind:name` (built-in plates left out).
fn names(v: &Value, side: &str) -> Vec<String> {
    v[side]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["builtin"] != true)
        .map(|r| {
            format!(
                "{}:{}",
                r["kind"].as_str().unwrap(),
                r["name"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn my_style_library_items() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-stylelib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    let fid = gmclip_app::framing::framings(&c, &pid, &cut)["active"]
        .as_str()
        .unwrap()
        .to_owned();
    // a layout of the cut, saved as mine
    let lay = c
        .call(&pid, "layout_create", json!({"cut": cut, "name": "panel"}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "screen"},
               "crop": {"x": 0.25, "y": 0.0, "w": 0.5, "h": 1.0}, "place": {"x": 0.0, "y": 0.2, "w": 1.0}}),
    )
    .unwrap();
    let saved = dispatch(
        &c,
        &pid,
        "style_item_save_mine",
        &json!({"name": "Panel", "from": {"layout": lay}}),
    )
    .unwrap();
    assert_eq!(saved["saved"], "layout:Panel");
    let view = dispatch(&c, &pid, "style_items_view", &json!({})).unwrap();
    assert_eq!(names(&view, "project"), ["layout:Panel"]);
    assert_eq!(names(&view, "mine"), ["layout:Panel"]);
    // mine is app data, per profile: a file in the app's data folder
    let lib = std::fs::read_to_string(root.join("app/style-library.json")).unwrap();
    assert!(lib.contains("layout:Panel"));
    // removed from the project's library, it still applies from mine (and is not stored)
    c.call(
        &pid,
        "style_library_save",
        json!({"name": "Panel", "kind": "layout"}),
    )
    .unwrap();
    let made = dispatch(
        &c,
        &pid,
        "style_item_apply",
        &json!({"kind": "layout", "name": "Panel", "mine": true, "to": {"cut": {"cut": cut}}}),
    )
    .unwrap();
    assert!(made["id"].as_str().is_some(), "{made}");
    let view = dispatch(&c, &pid, "style_items_view", &json!({})).unwrap();
    assert!(names(&view, "project").is_empty());
    // forgotten in mine: gone from both
    dispatch(
        &c,
        &pid,
        "style_item_forget_mine",
        &json!({"kind": "layout", "name": "Panel"}),
    )
    .unwrap();
    let view = dispatch(&c, &pid, "style_items_view", &json!({})).unwrap();
    assert!(names(&view, "mine").is_empty());
    assert!(
        dispatch(
            &c,
            &pid,
            "style_item_apply",
            &json!({"kind": "layout", "name": "Panel", "mine": true, "to": {"cut": {"cut": cut}}}),
        )
        .is_err()
    );
    // a framing's bands, saved as mine from the active framing
    let saved = dispatch(
        &c,
        &pid,
        "style_item_save_mine",
        &json!({"name": "Bands", "from": {"framing": fid}}),
    )
    .unwrap();
    assert_eq!(saved["saved"], "bands:Bands");
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0029: a background plate saved as mine (project and profile), listed with the
/// built-in ones, applied from mine to the framing, planned as a RenderPlan plate; a
/// built-in one applies by name.
#[test]
fn my_plates() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-plates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    let fid = gmclip_app::framing::framings(&c, &pid, &cut)["active"]
        .as_str()
        .unwrap()
        .to_owned();
    dispatch(
        &c,
        &pid,
        "plate_save",
        &json!({"name": "Brand", "stops": ["#101820", "#FEE715"]}),
    )
    .unwrap();
    assert!(
        dispatch(
            &c,
            &pid,
            "plate_save",
            &json!({"name": "Bad", "stops": ["#101820"]})
        )
        .is_err()
    );
    let view = dispatch(&c, &pid, "plates_view", &json!({"cut": cut})).unwrap();
    assert_eq!(names(&view, "project"), ["plate:Brand"]);
    let builtin = view["project"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["builtin"] == true)
        .count();
    assert_eq!(builtin, 4, "{view}");
    assert_eq!(names(&view, "mine"), ["plate:Brand"]);
    assert!(view["current"].is_null());
    // from mine onto the framing
    dispatch(
        &c,
        &pid,
        "style_item_apply",
        &json!({"kind": "plate", "name": "Brand", "mine": true, "to": {"framing": fid}}),
    )
    .unwrap();
    let view = dispatch(&c, &pid, "plates_view", &json!({"cut": cut})).unwrap();
    assert_eq!(view["current"], "Brand");
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(
        matches!(&pl.plan.background, gmclip_plan::types::Background::Plate { stops } if stops.len() == 2)
    );
    // a built-in by name
    dispatch(
        &c,
        &pid,
        "style_item_apply",
        &json!({"kind": "plate", "name": "Ember", "to": {"framing": fid}}),
    )
    .unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(
        matches!(&pl.plan.background, gmclip_plan::types::Background::Plate { stops } if stops.len() == 3)
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
