//! U.08 essentials on an opened talk: the audio lane, pauses/fillers sweeps (preview,
//! apply a chosen subset of a preview in one undo; a moved cut refuses the preview), the
//! screen hold (h, to the sentence's end), reorder by index.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

fn frames(c: &AppCore, p: &str, cut: &str) -> u64 {
    c.cut_view(p, cut).unwrap()["frames"].as_u64().unwrap()
}

#[test]
fn timeline_essentials() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-tighten-{}", std::process::id()));
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
    let t0 = Instant::now();
    loop {
        let st = gmclip_app::analysis::status(&c, &pid);
        let ph = st["sources"][0]["phase"].as_str().unwrap_or("").to_owned();
        if ph == "done" || ph == "failed" || t0.elapsed() > Duration::from_secs(300) {
            assert_eq!(ph, "done", "{st}");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("analysis in {:?}", t0.elapsed());

    // Audio lane: energy per output frame and quiet regions.
    let a = gmclip_app::tighten::timeline_audio(&c, &pid, &cut).unwrap();
    assert_eq!(a["analysed"], true);
    let db = a["parts"][0]["db"].as_array().unwrap();
    let q = a["parts"][0]["quiet"].as_array().unwrap();
    eprintln!("audio lane: {} points, {} quiet regions", db.len(), q.len());
    assert!(db.len() > 500);
    assert!(!q.is_empty());
    // the timeline's onsets-only ask (D.02: its levels come from gm://data): the same
    // onsets, no levels or regions
    let o = gmclip_app::tighten::timeline_audio_with(&c, &pid, &cut, true).unwrap();
    assert_eq!(o["analysed"], true);
    assert_eq!(o["parts"][0]["onsets"], a["parts"][0]["onsets"]);
    assert_eq!(o["parts"][0]["db"].as_array().map(Vec::len), Some(0));
    assert_eq!(o["parts"][0]["quiet"].as_array().map(Vec::len), Some(0));

    // Pauses ≥ 0.4 s: preview, then apply the first three in one transaction.
    let f0 = frames(&c, &pid, &cut);
    let pv = gmclip_app::tighten::sweep_preview(&c, &pid, &cut, "pauses", 0.4, 0.15).unwrap();
    let n = pv["cuts"].as_array().unwrap().len();
    eprintln!("pauses: {n} cuts, {} s", pv["removes_s"]);
    assert!(n >= 3, "{pv}");
    let take3: f64 = pv["cuts"].as_array().unwrap()[..3]
        .iter()
        .map(|x| x["seconds"].as_f64().unwrap())
        .sum();
    let id = pv["preview_id"].as_str().unwrap();
    gmclip_app::tighten::sweep_apply(&c, &pid, &cut, id, &[0, 1, 2]).unwrap();
    let f1 = frames(&c, &pid, &cut);
    assert!(
        ((f0 - f1) as f64 / 30.0 - take3).abs() < 0.2,
        "{f0} → {f1}, {take3} s"
    );
    // A preview of the cut before that sweep is refused: the cut moved (A2.06).
    let e = gmclip_app::tighten::sweep_apply(&c, &pid, &cut, id, &[0]).unwrap_err();
    assert_eq!(e.code, "Conflict", "{e:?}");
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(frames(&c, &pid, &cut), f0, "one undo restores the sweep");
    let fl = gmclip_app::tighten::sweep_preview(&c, &pid, &cut, "fillers", 0.4, 0.15).unwrap();
    eprintln!("fillers: {} cuts", fl["cuts"].as_array().unwrap().len());

    // h: hold the screen from frame 300 to the end of the sentence (hold_screen_here);
    // h again removes it.
    let h = gmclip_app::tighten::hold_toggle(&c, &pid, &cut, 300).unwrap();
    eprintln!("held: {h}");
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(pl.plan.parts.len() >= 2);
    let tm = &pl.plan.parts[1].layers[0].time_map;
    assert!(
        matches!(tm, Some(gmclip_plan::types::TimeMap::Hold { .. })),
        "{tm:?}"
    );
    assert_eq!(frames(&c, &pid, &cut), f0);
    gmclip_app::tighten::hold_toggle(&c, &pid, &cut, 300).unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(!matches!(
        pl.plan.parts[1].layers[0].time_map,
        Some(gmclip_plan::types::TimeMap::Hold { .. })
    ));

    // Reorder: part 2 first.
    let p2 = c.cut_view(&pid, &cut).unwrap()["parts"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::tighten::reorder_to(&c, &pid, &cut, &p2, 0).unwrap();
    assert_eq!(
        c.cut_view(&pid, &cut).unwrap()["parts"][0]["id"],
        p2.as_str()
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
