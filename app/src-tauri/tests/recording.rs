//! Decision 0045: a gmclip recording opens as a project whose screen videos carry their
//! recorded cursor (`Source.cursor`: the input track and cursor images stored as
//! assets), so the cut plans a `cursors[]` entry for the layers showing them. Lab: needs
//! a session folder made by `gmclip record start` (`GMCLIP_LAB_REC=<folder>`); skips
//! without one.

use std::path::PathBuf;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::Value;

#[test]
fn a_recording_opens_with_its_cursor_attached() {
    let Some(dir) = std::env::var_os("GMCLIP_LAB_REC").map(PathBuf::from) else {
        eprintln!("skipped: set GMCLIP_LAB_REC to a session folder");
        return;
    };
    let dir = dir.canonicalize().unwrap();
    let session = gmclip_media::rec::Session::read(&dir).unwrap();
    let root = std::env::temp_dir().join(format!("gmclip-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let r = gmclip_app::recording::open(&c, &dir).unwrap();
    let pid = r["project"].as_str().unwrap().to_owned();
    let cut = r["cut"].as_str().unwrap().to_owned();
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let with_cursor: Vec<_> = doc
        .iter::<gmclip_model::entities::Source>()
        .filter(|s| s.cursor.is_some())
        .collect();
    assert_eq!(with_cursor.len(), session.screens.len(), "{r}");
    for s in &with_cursor {
        let cur = s.cursor.as_ref().unwrap();
        let track = doc.get(cur.track).unwrap();
        assert_eq!(track.media_type, gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE);
        let file = c.fonts().asset_file_typed(&track.sha256, &track.media_type).unwrap();
        let t: gmclip_model::rec::InputTrack =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        t.check().unwrap();
        assert!(!t.cursor.is_empty());
    }
    let plan = c.planned(&pid, &cut).unwrap().plan;
    let v = serde_json::to_value(&plan).unwrap();
    assert!(
        v["cursors"].as_array().is_some_and(|a| !a.is_empty()),
        "the cut plans the recorded cursor: {}",
        v["cursors"]
    );
    c.analysis.shutdown(Duration::from_secs(20));
    // `GMCLIP_LAB_REC_PLAN=<file>` keeps the plan (and the assets it names), to look at a
    // frame with `gmclip still`.
    match std::env::var_os("GMCLIP_LAB_REC_PLAN") {
        Some(out) => std::fs::write(out, serde_json::to_vec_pretty(&plan).unwrap()).unwrap(),
        None => {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// A session folder around a lab screen render: one screen file (linked), a 32 px
/// cursor image, and an input log with moves, a click at 2 s and typing into a field
/// with a caret from 3.5 to 5 s.
fn synthetic_session(root: &std::path::Path, video: &std::path::Path) -> PathBuf {
    synthetic_session_with_face(root, video, None)
}

/// [`synthetic_session`], with `face` (linked) as the camera file beside the screen.
fn synthetic_session_with_face(
    root: &std::path::Path,
    video: &std::path::Path,
    face: Option<&std::path::Path>,
) -> PathBuf {
    use gmclip_media::rec::RawEvent;
    let mut src = gmclip_media::demux::io::FileSource::open(video).unwrap();
    let d = gmclip_media::demux::index(&mut src).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.width > 0)
        .unwrap();
    let (w, h) = (t.width, t.height);
    let dir = root.join("Recording synthetic");
    std::fs::create_dir_all(dir.join("cursors")).unwrap();
    std::os::unix::fs::symlink(video, dir.join("screen-1.mp4")).unwrap();
    // a white arrow-ish square with a black rim, hotspot top left
    let mut px = vec![0u8; 32 * 32 * 4];
    for y in 0..32u32 {
        for x in 0..32u32 {
            if x <= y && y < 28 {
                let rim = x == 0 || x == y || y == 27;
                let i = ((y * 32 + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(if rim { &[0, 0, 0, 255] } else { &[255, 255, 255, 255] });
            }
        }
    }
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 { width: 32, height: 32, px }).unwrap();
    let sha = gmclip_core::profile::style::sha256_hex(&png);
    std::fs::write(dir.join("cursors").join(format!("{sha}.png")), &png).unwrap();
    // points: the picture is a 2× display at the origin
    let (pw, ph) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let mut ev = vec![RawEvent::Shape {
        t: 0,
        sha256: sha,
        w: 32,
        h: 32,
        hot_x: 1.0,
        hot_y: 1.0,
        pt_w: 16.0,
        pt_h: 16.0,
    }];
    for i in 0..180i64 {
        let f = i as f64 / 180.0;
        ev.push(RawEvent::Cursor { t: i * 33_333, s: 0, x: pw * (0.2 + 0.6 * f), y: ph * (0.3 + 0.3 * f) });
    }
    ev.push(RawEvent::Button { t: 2_000_000, x: pw * 0.4, y: ph * 0.4, b: 0, down: true, n: 1 });
    ev.push(RawEvent::Button { t: 2_080_000, x: pw * 0.4, y: ph * 0.4, b: 0, down: false, n: 1 });
    ev.push(RawEvent::Focus {
        t: 3_400_000,
        rect: Some([pw * 0.55, ph * 0.7, pw * 0.3, 30.0]),
        caret: Some([pw * 0.6, ph * 0.7 + 4.0, 2.0, 22.0]),
        window: None,
        role: "AXTextField".into(),
        subrole: String::new(),
        app: "com.example".into(),
    });
    for k in 0..15i64 {
        ev.push(RawEvent::Keys { t: 3_500_000 + k * 100_000, n: 1 });
    }
    ev.sort_by_key(RawEvent::t);
    let lines: String = ev.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect();
    std::fs::write(dir.join("events.ndjson"), lines).unwrap();
    let face = face.map(|f| {
        std::os::unix::fs::symlink(f, dir.join("face.mp4")).unwrap();
        let mut src = gmclip_media::demux::io::FileSource::open(f).unwrap();
        let d = gmclip_media::demux::index(&mut src).unwrap();
        let t = d.tracks.iter().find(|t| t.width > 0).unwrap();
        serde_json::json!({"file": "face.mp4", "camera": "Lab camera", "mic": null,
                           "width": t.width, "height": t.height, "frames": 0, "dropped": 0})
    });
    let session = serde_json::json!({
        "schema": "gmclip.rec/1", "name": "Recording synthetic", "app_version": "test",
        "screens": [{"file": "screen-1.mp4",
                     "display": {"id": 1, "name": "Lab", "x": 0.0, "y": 0.0, "w": pw, "h": ph, "scale": 2.0},
                     "width": w, "height": h, "fps": 30, "frames": 0, "dropped": 0}],
        "face": face, "events": "events.ndjson", "permissions": {}, "missing": [],
        "duration_us": 6_000_000, "complete": true,
    });
    std::fs::write(dir.join("session.json"), serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    dir
}

/// Decision 0046: the one-click look on a recording plans the plate, the framed screen
/// (rounded corners, shadow), the recorded cursor with its feel, and zoom sections from
/// the log (one at the click, one on the caret). Lab: the screen render in the lab media.
#[test]
fn the_screen_studio_look_plans_from_a_recording() {
    let video = PathBuf::from(gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    let Ok(video) = video.canonicalize() else {
        eprintln!("skipped: no lab screen render");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-look-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = synthetic_session(&root, &video);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let r = gmclip_app::recording::open(&c, &dir).unwrap();
    let (pid, cut) = (
        r["project"].as_str().unwrap().to_owned(),
        r["cut"].as_str().unwrap().to_owned(),
    );
    let look = gmclip_app::screenstudio::apply(&c, &pid, &cut, None).unwrap();
    let v = serde_json::to_value(c.planned(&pid, &cut).unwrap().plan).unwrap();
    assert_eq!(v["background"]["kind"], "plate", "{look} {}", v["background"]);
    assert!(v["cursors"][0]["smoothness"].as_f64().is_some_and(|s| s > 0.5), "{}", v["cursors"]);
    let zooms = v["zooms"].as_array().cloned().unwrap_or_default();
    assert!(zooms.len() >= 2, "{look} zooms: {}", v["zooms"]);
    let framed = v["parts"][0]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["frame"]["radius"].as_f64().is_some_and(|r| r > 0.0));
    assert!(framed, "a framed screen layer: {}", v["parts"][0]["layers"]);
    // "Undo look": back to the restore point taken before it, in one step
    c.call(&pid, "restore_to", serde_json::json!({"snapshot": look["before"]})).unwrap();
    let v2 = serde_json::to_value(c.planned(&pid, &cut).unwrap().plan).unwrap();
    assert!(v2.get("zooms").is_none_or(Value::is_null), "undone: {}", v2["zooms"]);
    c.analysis.shutdown(Duration::from_secs(20));
    match std::env::var_os("GMCLIP_LAB_LOOK_PLAN") {
        Some(out) => std::fs::write(out, serde_json::to_vec_pretty(&v).unwrap()).unwrap(),
        None => {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// AJ 2026-10-20: cropping a recording's camera showed the screen. The crop editor's
/// picture for the camera layer is the camera file, at its own shape; for the screen, the
/// screen. A crop set on a grouped layer is set on the whole group in one undo step.
/// Lab: the screen render, and a 9:16 reel standing in for the camera file.
#[test]
fn the_camera_is_cropped_over_its_own_picture_and_a_group_takes_one_crop() {
    let video = PathBuf::from(gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    let face = PathBuf::from(gmclip_model::lab::media_str(
        "reels/loc_086da141_h03/loc_086da141_h03-v5.mp4",
    ));
    let (Ok(video), Ok(face)) = (video.canonicalize(), face.canonicalize()) else {
        eprintln!("skipped: no lab screen render or reel");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-camcrop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = synthetic_session_with_face(&root, &video, Some(&face));
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let r = gmclip_app::recording::open(&c, &dir).unwrap();
    let (pid, cut) = (
        r["project"].as_str().unwrap().to_owned(),
        r["cut"].as_str().unwrap().to_owned(),
    );
    let view = c.cut_view(&pid, &cut).unwrap();
    let part = &view["parts"][0];
    let part_id = part["id"].as_str().unwrap().to_owned();
    let layers = part["layers"].as_array().unwrap();
    let cam = layers
        .iter()
        .find(|l| l["source"] != part["source"])
        .unwrap_or_else(|| panic!("a camera layer of the face file: {r} {}", part["layers"]));
    let screen = layers
        .iter()
        .find(|l| l["source"] == part["source"])
        .unwrap();
    let (cam_m, screen_m) = (
        cam["model"].as_str().unwrap().to_owned(),
        screen["model"].as_str().unwrap().to_owned(),
    );
    let canvas = |layer: Option<&str>| {
        let pl = gmclip_app::framing::source_view_plan(&c, &pid, &cut, &part_id, layer).unwrap();
        let l = pl.plan.parts[0].layers[0].clone();
        (
            (pl.plan.output.canvas.w.get(), pl.plan.output.canvas.h.get()),
            l.picture_source.map(|s| s.as_str().to_owned()),
        )
    };
    // the camera layer: the face file's own picture (portrait), read from that file
    let (size, src) = canvas(Some(&cam_m));
    assert!(size.1 > size.0, "the camera file's shape: {size:?}");
    assert_eq!(src.as_deref(), cam["source"].as_str());
    // the screen (and no layer asked for): the screen's picture, as before
    for l in [Some(screen_m.as_str()), None] {
        let (size, src) = canvas(l);
        assert_eq!(size, (1920, 1080), "{l:?}");
        assert_eq!(src, None, "{l:?}");
    }
    // a crop on a grouped layer: the same crop on each layer of the group, one undo
    let group = vec![
        (part_id.clone(), cam_m.clone()),
        (part_id.clone(), screen_m.clone()),
    ];
    let crop_of = |m: &str| {
        let v = c.cut_view(&pid, &cut).unwrap();
        v["parts"][0]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["model"] == m)
            .unwrap()["crop_n"]
            .clone()
    };
    let before = (crop_of(&cam_m), crop_of(&screen_m));
    gmclip_app::framing::crop_norm_group(
        &c,
        &pid,
        &cut,
        &part_id,
        &cam_m,
        [0.25, 0.25, 0.5, 0.5],
        &group,
    )
    .unwrap();
    for m in [&cam_m, &screen_m] {
        let n: Vec<f64> = crop_of(m)
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        for (a, b) in n.iter().zip([0.25, 0.25, 0.5, 0.5]) {
            assert!((a - b).abs() < 1e-3, "{m}: {n:?}");
        }
    }
    c.call(&pid, "undo", serde_json::json!({})).unwrap();
    assert_eq!(
        (crop_of(&cam_m), crop_of(&screen_m)),
        before,
        "one undo puts both back"
    );
    // not in the group: that layer alone
    gmclip_app::framing::crop_norm_group(
        &c,
        &pid,
        &cut,
        &part_id,
        &cam_m,
        [0.1, 0.1, 0.5, 0.5],
        &group[1..],
    )
    .unwrap();
    assert_eq!(crop_of(&screen_m), before.1);
    c.analysis.shutdown(Duration::from_secs(20));
    let _ = std::fs::remove_dir_all(&root);
}

/// The cursor log is optional metadata; a truncated or missing log must not lose video.
#[test]
fn a_damaged_cursor_log_does_not_block_video_and_capture_drops_are_reported() {
    let video = gmclip_model::lab::media("renders/own-vm-1080.mp4");
    let Ok(video) = video.canonicalize() else {
        eprintln!("skipped: no lab screen render");
        return;
    };
    for missing_log in [false, true] {
        let root = std::env::temp_dir().join(format!("gmclip-damaged-log-{}-{missing_log}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = synthetic_session(&root, &video);
        if missing_log {
            std::fs::remove_file(dir.join("events.ndjson")).unwrap();
        } else {
            std::fs::write(dir.join("events.ndjson"), "{damaged middle line\n{}\n").unwrap();
        }
        let mut session = gmclip_media::rec::Session::read(&dir).unwrap();
        session.screens[0].dropped = 17;
        std::fs::write(dir.join("session.json"), serde_json::to_vec(&session).unwrap()).unwrap();
        let c = AppCore::new(Dirs {
            app_data: root.join("app"), library: root.join("lib"), exports: root.join("exports"),
        });
        let r = gmclip_app::recording::open(&c, &dir).unwrap();
        let notes = r["notes"].as_array().unwrap();
        assert!(notes.iter().any(|n| n.as_str().unwrap().contains("cursor log could not be read")), "{r}");
        assert!(notes.iter().any(|n| n.as_str().unwrap().contains("17 samples dropped")), "{r}");
        let plan = c.planned(r["project"].as_str().unwrap(), r["cut"].as_str().unwrap()).unwrap().plan;
        gmclip_plan::parse(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(!plan.parts.is_empty());
        assert!(dir.join("screen-1.mp4").exists());
        c.analysis.shutdown(Duration::from_secs(20));
        drop(c);
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Decision 0052: a recording's markers land on its files and on the cut's clock, and its
/// first 9:16 framing is "Screen over camera" (the face file below the screen) instead of
/// a crop of the screen alone. Lab: the screen render, and a reel standing in for the
/// camera file.
#[test]
fn a_recording_carries_its_markers_and_stacks_its_camera_in_9_16() {
    let video = PathBuf::from(gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    let face = PathBuf::from(gmclip_model::lab::media_str(
        "reels/loc_086da141_h03/loc_086da141_h03-v5.mp4",
    ));
    let (Ok(video), Ok(face)) = (video.canonicalize(), face.canonicalize()) else {
        eprintln!("skipped: no lab screen render or reel");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-recmarks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = synthetic_session_with_face(&root, &video, Some(&face));
    let mut log = std::fs::read_to_string(dir.join("events.ndjson")).unwrap();
    for t in [1_250_000i64, 4_000_400] {
        log.push_str(&serde_json::to_string(&gmclip_media::rec::RawEvent::Marker { t }).unwrap());
        log.push('\n');
    }
    std::fs::write(dir.join("events.ndjson"), log).unwrap();
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let r = gmclip_app::recording::open(&c, &dir).unwrap();
    let (pid, cut) = (
        r["project"].as_str().unwrap().to_owned(),
        r["cut"].as_str().unwrap().to_owned(),
    );
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let marked: Vec<Vec<i64>> = doc
        .iter::<gmclip_model::entities::Source>()
        .map(|s| s.markers.iter().map(|f| f.0 / 705_600).collect())
        .collect();
    assert_eq!(marked, vec![vec![1250, 4000]; 2], "both files: {r}");
    let listed = c.call(&pid, "marker_list", serde_json::json!({"cut": cut})).unwrap();
    let at: Vec<(u64, i64)> = listed["output"]["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["n"].as_u64().unwrap(), m["cut_ms"].as_i64().unwrap()))
        .collect();
    assert_eq!(at, [(1, 1250), (2, 4000)], "{listed}");

    // 9:16: the screen across the top, the face file filling the rest
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    let view = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(view["canvas"], serde_json::json!({"w": 1080, "h": 1920}));
    let part = &view["parts"][0];
    let layers = part["layers"].as_array().unwrap();
    let cam = layers
        .iter()
        .find(|l| l["source"] != part["source"])
        .unwrap_or_else(|| panic!("the face file in 9:16: {}", part["layers"]));
    let screen = layers.iter().find(|l| l["source"] == part["source"]).unwrap();
    // places in canvas px: [x, y, w, h]
    let place = |l: &Value, i: usize| l["place"][i].as_f64().unwrap_or(f64::NAN);
    assert_eq!((place(screen, 0), place(screen, 1)), (0.0, 0.0), "{screen}");
    assert!((place(screen, 2) - 1080.0).abs() < 1.0, "{screen}");
    // a 16:9 screen across 1080 px is 607.5 px tall; the camera fills the rest below
    assert!((place(screen, 3) - 607.5).abs() < 1.0, "{screen}");
    assert!((place(cam, 1) - 607.5).abs() < 1.0, "{cam}");
    assert!((place(cam, 1) + place(cam, 3) - 1920.0).abs() < 1.0, "{cam}");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    assert!(
        doc.iter::<gmclip_model::entities::Layout>().any(|l| l.name == "Screen over camera"),
        "the built-in layout is made"
    );
    let plan = c.planned(&pid, &cut).unwrap().plan;
    gmclip_plan::parse(&serde_json::to_vec(&plan).unwrap()).unwrap();
    c.analysis.shutdown(Duration::from_secs(20));
    let _ = std::fs::remove_dir_all(&root);
}
