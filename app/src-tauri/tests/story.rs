//! U.07 through the app core on loc_4fadb756_h37 (its stream's project fixture): brief,
//! typed in/out on the frame grid, nudges, reorder, chronological as one transaction,
//! duration band. Skips without the lab media.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

#[test]
fn story_panel_operations() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-story-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let cut = p["fixture"]["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hook_id"] == "loc_4fadb756_h37")
        .unwrap()["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let v = gmclip_app::story::story_view(&c, &pid, &cut).unwrap();
    let parts = v["parts"].as_array().unwrap().clone();
    eprintln!(
        "h37: {} parts, {} s, band {}",
        parts.len(),
        v["duration_s"],
        v["band"]
    );
    assert_eq!(v["band"], "inside");

    // Brief.
    c.call(
        &pid,
        "cut_set",
        json!({"cut": cut, "brief": "GPU 2% → 100%"}),
    )
    .unwrap();
    assert_eq!(
        gmclip_app::story::story_view(&c, &pid, &cut).unwrap()["brief"],
        "GPU 2% → 100%"
    );

    // Typed out: part 2 ends 0.5 s earlier; the time lands on the frame grid.
    let p2 = parts[1]["id"].as_str().unwrap();
    let out = gmclip_app::story::parse_time(parts[1]["out"].as_str().unwrap()).unwrap();
    let typed = gmclip_app::story::fmt_time(gmclip_model::Flicks(out.0 - 705_600_000 / 2));
    gmclip_app::story::set_time(&c, &pid, &cut, p2, "out", &typed).unwrap();
    let v2 = gmclip_app::story::story_view(&c, &pid, &cut).unwrap();
    let d = parts[1]["dur_s"].as_f64().unwrap() - v2["parts"][1]["dur_s"].as_f64().unwrap();
    assert!((d - 0.5).abs() <= 0.034, "{d}");
    assert!(
        c.planned(&pid, &cut).is_ok(),
        "plans after a typed time (frame grid)"
    );
    // Nudges: ‹ 0.5 s, ⇧› 0.1 s (3 frames at 30 fps).
    gmclip_app::story::nudge_ms(&c, &pid, &cut, p2, "out", 500).unwrap();
    gmclip_app::story::nudge_ms(&c, &pid, &cut, p2, "out", -100).unwrap();
    let v3 = gmclip_app::story::story_view(&c, &pid, &cut).unwrap();
    let d3 = v3["parts"][1]["dur_s"].as_f64().unwrap() - v2["parts"][1]["dur_s"].as_f64().unwrap();
    assert!((d3 - 0.4).abs() <= 0.034, "{d3}");
    // Move part 2 up, then down: back where it was.
    gmclip_app::story::move_part(&c, &pid, &cut, p2, "up").unwrap();
    assert_eq!(
        gmclip_app::story::story_view(&c, &pid, &cut).unwrap()["parts"][0]["id"],
        p2
    );
    gmclip_app::story::move_part(&c, &pid, &cut, p2, "down").unwrap();
    assert_eq!(
        gmclip_app::story::story_view(&c, &pid, &cut).unwrap()["parts"][1]["id"],
        p2
    );
    // Chronological: one transaction; parts sorted by source in.
    let r = gmclip_app::story::chronological(&c, &pid, &cut).unwrap();
    assert!(!r["committed"].is_null());
    let v4 = gmclip_app::story::story_view(&c, &pid, &cut).unwrap();
    let ins: Vec<_> = v4["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| gmclip_app::story::parse_time(x["in"].as_str().unwrap()).unwrap())
        .collect();
    assert!(ins.windows(2).all(|w| w[0] <= w[1]));
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(
        gmclip_app::story::story_view(&c, &pid, &cut).unwrap()["parts"],
        v3["parts"],
        "one undo restores the order"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
