//! Per-take layout by numbers (walkthrough gap 3): typed crops on a model cut, a detail
//! inset, copying a take's layout to the next N takes, and presets kept per profile. On
//! AJ's h79 reel.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::Value;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn layers(c: &AppCore, pid: &str, cut: &str, k: usize) -> Vec<Value> {
    c.cut_view(pid, cut).unwrap()["parts"][k]["layers"]
        .as_array()
        .unwrap()
        .clone()
}

fn role<'a>(ls: &'a [Value], r: &str) -> Vec<&'a Value> {
    ls.iter().filter(|l| l["role"] == r).collect()
}

#[test]
fn typed_crops_insets_copy_next_and_presets() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-tl-{}", std::process::id()));
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
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();
    for f in [300, 600, 900] {
        c.split_at_frame(&pid, &cut, f).unwrap();
    }
    let cv = c.cut_view(&pid, &cut).unwrap();
    let parts: Vec<String> = cv["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    let plan_ids: Vec<String> = cv["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["plan_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(parts.len(), 4);

    // a typed crop on a model cut: this part only, in source pixels
    let ls = layers(&c, &pid, &cut, 0);
    let screen = role(&ls, "screen")[0];
    let before_other = role(&layers(&c, &pid, &cut, 1), "screen")[0]["crop"].clone();
    c.crop_px(
        &pid,
        &cut,
        &plan_ids[0],
        screen["id"].as_str().unwrap(),
        [100.0, 200.0, 540.0, 380.0],
        None,
    )
    .unwrap();
    let got = role(&layers(&c, &pid, &cut, 0), "screen")[0]["crop"].clone();
    let g: Vec<f64> = got
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(
        (g[0] - 100.0).abs() < 1.0 && (g[2] - 540.0).abs() < 1.0,
        "{got}"
    );
    assert_eq!(
        role(&layers(&c, &pid, &cut, 1), "screen")[0]["crop"],
        before_other,
        "the other parts keep theirs"
    );

    // a detail inset on part 0, typed in pixels
    gmclip_app::takelayout::detail_add(
        &c,
        &pid,
        &cut,
        &parts[0],
        Some([400.0, 100.0, 200.0, 40.0]),
        Some([30.0, 800.0, 1020.0]),
    )
    .unwrap();
    let det = role(&layers(&c, &pid, &cut, 0), "detail")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(det.len(), 1);
    assert_eq!(det[0]["addition"], true);
    assert!((det[0]["place"][1].as_f64().unwrap() - 800.0).abs() < 1.0);

    // copy part 0's layout to the next 2 parts; part 3 stays as it was
    let r = gmclip_app::takelayout::copy_layout(&c, &pid, &cut, &parts[0], Some(2)).unwrap();
    assert_eq!(r["parts"], 2);
    for k in 1..=2 {
        let ls = layers(&c, &pid, &cut, k);
        assert_eq!(role(&ls, "screen")[0]["crop"], got, "part {k}");
        assert_eq!(role(&ls, "detail").len(), 1, "part {k} has the inset");
    }
    assert!(role(&layers(&c, &pid, &cut, 3), "detail").is_empty());
    // copying again replaces the inset rather than stacking a second one
    gmclip_app::takelayout::copy_layout(&c, &pid, &cut, &parts[0], Some(1)).unwrap();
    assert_eq!(role(&layers(&c, &pid, &cut, 1), "detail").len(), 1);

    // presets: save part 0's stack, apply it to part 3, delete it
    gmclip_app::takelayout::preset_save(&c, &pid, &cut, &parts[0], "prompt + run").unwrap();
    let ps = gmclip_app::takelayout::presets(&c, &pid, &cut).unwrap();
    assert_eq!(ps["presets"][0]["name"], "prompt + run");
    assert_eq!(ps["presets"][0]["insets"], 1);
    gmclip_app::takelayout::preset_apply(
        &c,
        &pid,
        &cut,
        "prompt + run",
        std::slice::from_ref(&parts[3]),
    )
    .unwrap();
    let ls = layers(&c, &pid, &cut, 3);
    assert_eq!(role(&ls, "screen")[0]["crop"], got);
    assert_eq!(role(&ls, "detail").len(), 1);
    // the inset can be removed again
    let m = role(&ls, "detail")[0]["model"].as_str().unwrap().to_owned();
    gmclip_app::takelayout::layer_delete(&c, &pid, &cut, &m).unwrap();
    assert!(role(&layers(&c, &pid, &cut, 3), "detail").is_empty());
    // renamed, then deleted by its new name
    gmclip_app::takelayout::preset_rename(&c, "prompt + run", "prompt and run").unwrap();
    let ps = gmclip_app::takelayout::presets(&c, &pid, &cut).unwrap();
    assert_eq!(ps["presets"][0]["name"], "prompt and run");
    assert!(gmclip_app::takelayout::preset_rename(&c, "prompt + run", "x").is_err());
    gmclip_app::takelayout::preset_delete(&c, "prompt and run").unwrap();
    assert!(
        gmclip_app::takelayout::presets(&c, &pid, &cut).unwrap()["presets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(c.planned(&pid, &cut).is_ok());

    // a 15 s hook outlasts the opening take: the next take keeps the opening's layout until
    // the hook ends (split there), and the rest of that take keeps its own
    let cv = c.cut_view(&pid, &cut).unwrap();
    let p1 = cv["parts"][1]["plan_id"].as_str().unwrap().to_owned();
    let s1 = role(&layers(&c, &pid, &cut, 1), "screen")[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.crop_px(&pid, &cut, &p1, &s1, [0.0, 0.0, 540.0, 380.0], None)
        .unwrap();
    let other = role(&layers(&c, &pid, &cut, 1), "screen")[0]["crop"].clone();
    gmclip_app::short::hook_set(&c, &pid, &cut, "A long hook", None, Some(15_000)).unwrap();
    // the rule as a check: the layout changes at take 2 while the hook is up
    let sv = gmclip_app::short::short_view(&c, &pid, &cut).unwrap();
    let cv = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(sv["hook_layout"]["take"], 2, "{}", sv["hook_layout"]);
    assert_eq!(sv["hook_layout"]["changes_at"], cv["parts"][1]["out"][0]);
    let r = gmclip_app::takelayout::keep_opening_until_hook(&c, &pid, &cut).unwrap();
    eprintln!("{r}");
    let cv = c.cut_view(&pid, &cut).unwrap();
    let at = r["split_at"].as_i64().unwrap();
    assert!((405..=495).contains(&at), "{r}");
    assert_eq!(cv["parts"][2]["out"][0].as_i64(), Some(at));
    assert_eq!(role(&layers(&c, &pid, &cut, 1), "screen")[0]["crop"], got);
    assert_eq!(role(&layers(&c, &pid, &cut, 2), "screen")[0]["crop"], other);
    // the rule holds now: nothing to warn about
    let sv = gmclip_app::short::short_view(&c, &pid, &cut).unwrap();
    assert!(sv["hook_layout"].is_null(), "{}", sv["hook_layout"]);
    eprintln!("takelayout: all checks ran");
    let _ = std::fs::remove_dir_all(&root);
}
