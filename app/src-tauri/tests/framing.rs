//! 9:16 output, presets, per-part crops and "apply to all parts" on an opened video.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

fn crops(c: &AppCore, p: &str, cut: &str) -> Vec<Vec<[f64; 4]>> {
    let v = c.cut_view(p, cut).unwrap();
    v["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pt| {
            pt["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| {
                    let a = l["crop_n"].as_array().unwrap();
                    [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap())
                })
                .collect()
        })
        .collect()
}

#[test]
fn vertical_crops() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-framing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    c.analysis.cancel_project(v["project"].as_str().unwrap());
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    assert_eq!(c.cut_view(&pid, &cut).unwrap()["framing"]["aspect"], "16:9");

    // 9:16: a 1080×1920 framing, one centred 9:16 crop.
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    let cv = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(cv["canvas"], json!({"w": 1080, "h": 1920}));
    assert_eq!(cv["framing"]["aspect"], "9:16");
    let cr = crops(&c, &pid, &cut);
    let r = cr[0][0];
    // The crop's pixel aspect is the canvas's (9:16) on a 1920×1080 source.
    assert!(
        ((r[2] * 1920.0) / (r[3] * 1080.0) - 9.0 / 16.0).abs() < 0.01,
        "{r:?}"
    );
    assert!((r[0] + r[2] / 2.0 - 0.5).abs() < 0.01, "centred: {r:?}");

    // Split, then Screen + facecam on part 1 only: two layers there, one on part 2.
    let frames = cv["frames"].as_u64().unwrap();
    c.split_at_frame(&pid, &cut, frames / 2).unwrap();
    let cv = c.cut_view(&pid, &cut).unwrap();
    let p1 = cv["parts"][0]["id"].as_str().unwrap().to_owned();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, std::slice::from_ref(&p1), "screen_cam")
        .unwrap();
    let cr = crops(&c, &pid, &cut);
    assert_eq!((cr[0].len(), cr[1].len()), (2, 1));
    assert_eq!(
        c.cut_view(&pid, &cut).unwrap()["framing"]["part_layouts"][&p1],
        "Screen + facecam 9:16"
    );

    // A crop on part 1's screen layer is part 1's own.
    let layer = c.cut_view(&pid, &cut).unwrap()["parts"][0]["layers"][0]["model"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::framing::crop_norm(&c, &pid, &cut, &p1, &layer, [0.1, 0.05, 0.6, 0.5]).unwrap();
    let cr = crops(&c, &pid, &cut);
    assert!(
        (cr[0][0][0] - 0.1).abs() < 1e-3 && (cr[0][0][2] - 0.6).abs() < 1e-3,
        "{:?}",
        cr[0][0]
    );

    // Apply to all: part 2 gets the layout and the crop, in one undo step.
    gmclip_app::framing::apply_to_all(&c, &pid, &cut, &p1).unwrap();
    let cr = crops(&c, &pid, &cut);
    assert_eq!(cr[1].len(), 2);
    assert!((cr[1][0][0] - 0.1).abs() < 1e-3, "{:?}", cr[1][0]);
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(crops(&c, &pid, &cut)[1].len(), 1);

    // Back to 16:9: the full picture again; the 9:16 work stays for later.
    gmclip_app::framing::set_output(&c, &pid, &cut, "16:9").unwrap();
    assert_eq!(
        c.cut_view(&pid, &cut).unwrap()["canvas"],
        json!({"w": 1920, "h": 1080})
    );
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    assert_eq!(crops(&c, &pid, &cut)[0].len(), 2);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A whole export of a 9:16 Screen + facecam cut (a minute of 1080×1920): `cargo test
/// --test framing -- --ignored`.
#[test]
#[ignore = "exports a minute of video"]
fn vertical_export() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-vexport-{}", std::process::id()));
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
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();
    let jobs = std::sync::Arc::new(gmclip_app::jobs::Jobs::default());
    let t0 = std::time::Instant::now();
    jobs.start_export(&c, &pid, &cut).unwrap();
    let j = loop {
        let j = jobs.list().into_iter().next().unwrap();
        if j.state != "running" && j.state != "queued" {
            break j;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    eprintln!("export {} in {:?}: {:?}", j.state, t0.elapsed(), j.error);
    assert_eq!(j.state, "done", "{:?}", j.error);
    let e = &c.project(&pid).unwrap().exports.lock().unwrap()[0].clone();
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&e.file)
        .output();
    if let Ok(o) = out {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
        eprintln!("{} → {s}", e.file.display());
        assert_eq!(s, "1080,1920");
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The newer built-in layouts (AJ 2026-10-19) on both output shapes: each layer's crop has
/// its place's shape (nothing stretched) and every place is on the canvas.
#[test]
fn new_built_in_layouts_fit_both_shapes() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-layouts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    c.analysis.cancel_project(v["project"].as_str().unwrap());
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    for aspect in ["16:9", "9:16"] {
        gmclip_app::framing::set_output(&c, &pid, &cut, aspect).unwrap();
        for (preset, n) in [("cam_corner", 2), ("halves", 2), ("cam_full", 1)] {
            gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], preset).unwrap();
            let cv = c.cut_view(&pid, &cut).unwrap();
            let (cw, ch) = (
                cv["canvas"]["w"].as_f64().unwrap(),
                cv["canvas"]["h"].as_f64().unwrap(),
            );
            let ls = cv["parts"][0]["layers"].as_array().unwrap();
            assert_eq!(ls.len(), n, "{aspect} {preset}");
            for l in ls {
                let cr: Vec<f64> = l["crop_n"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap())
                    .collect();
                let pl: Vec<f64> = l["place"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap())
                    .collect();
                let src = l["src_size"].as_array().unwrap();
                let (sw, sh) = (src[0].as_f64().unwrap(), src[1].as_f64().unwrap());
                let crop_a = cr[2] * sw / (cr[3] * sh);
                let place_a = pl[2] / pl[3];
                assert!(
                    (crop_a / place_a - 1.0).abs() < 0.02,
                    "{aspect} {preset}: crop {crop_a} vs place {place_a}"
                );
                assert!(
                    pl[0] >= -0.5
                        && pl[1] >= -0.5
                        && pl[0] + pl[2] <= cw + 0.5
                        && pl[1] + pl[3] <= ch + 0.5,
                    "{aspect} {preset}: {pl:?} on {cw}×{ch}"
                );
            }
            assert!(c.planned(&pid, &cut).is_ok(), "{aspect} {preset} plans");
        }
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
