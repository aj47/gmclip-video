//! Backdrops and video overlays (decision 0028) through the app core: a picked video or
//! still becomes the framing's backdrop (stored once by its hash, with its probe), a picked
//! video goes on the cut picture-in-picture, and the cut plans and renders with both. The
//! steps use the lab media folder (`GMCLIP_LAB_MEDIA`, default
//! `~/Development/gmclip-lab-media`) and skip without it.

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_plan::types::Background;
use serde_json::json;

fn lab(rel: &str) -> PathBuf {
    let d = std::env::var_os("GMCLIP_LAB_MEDIA").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join("Development/gmclip-lab-media")
        },
        PathBuf::from,
    );
    d.join(rel)
}

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-backdrop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

#[test]
fn a_picked_video_or_still_is_the_backdrop_and_a_video_goes_on_top() {
    let video = lab("renders/own-vm-1080.mp4");
    if !video.is_file() {
        eprintln!("skipped: no lab media");
        return;
    }
    let (c, root) = core("set");
    let o = gmclip_app::video::open_video(&c, &video).unwrap();
    let pid = o["project"].as_str().unwrap().to_owned();
    let cut = o["cut"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    // a 9:16 framing for the cut so the backdrop shows round the picture
    let call = |name: &str, a: serde_json::Value| {
        gmclip_app::backdrop::dispatch(&c, &pid, name, &a)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
    };
    // the 8 MB MP4 is stored as it is, by its hash, with its probe
    let r = call(
        "backdrop_set",
        json!({"cut": cut, "path": video, "loop": true, "fit": "cover"}),
    );
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let asset = doc
        .iter::<gmclip_model::entities::Asset>()
        .find(|a| a.id.0.to_string() == r["asset"].as_str().unwrap())
        .unwrap()
        .clone();
    assert_eq!(asset.media_type, "video/mp4");
    let probe = asset.probe.unwrap();
    assert_eq!((probe.w, probe.h), (1920, 1080));
    assert!(probe.frames.unwrap() > 1600, "{probe:?}");
    let stored = c
        .fonts()
        .asset_file_typed(&asset.sha256, &asset.media_type)
        .unwrap();
    assert!(stored.is_file() && stored.extension().unwrap() == "mp4");
    let pl = c.planned(&pid, &cut).unwrap();
    let Background::Backdrop {
        video: Some(v),
        place,
        ..
    } = &pl.plan.background
    else {
        panic!("{:?}", pl.plan.background);
    };
    assert_eq!(v.r#loop, Some(true));
    assert_eq!(place.w, pl.plan.output.canvas.w.get() as f64);
    // a still: re-encoded as PNG, with its size
    let png = root.join("plate.png");
    let (w, h) = (300u32, 200u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % 256) as u8, 40, 90, 255])
        .collect();
    std::fs::write(
        &png,
        gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
            width: w,
            height: h,
            px,
        })
        .unwrap(),
    )
    .unwrap();
    call(
        "backdrop_set",
        json!({"cut": cut, "path": png, "fit": "contain"}),
    );
    let pl = c.planned(&pid, &cut).unwrap();
    let Background::Backdrop {
        image: Some(_),
        place,
        ..
    } = &pl.plan.background
    else {
        panic!("{:?}", pl.plan.background);
    };
    assert!(place.w <= pl.plan.output.canvas.w.get() as f64 + 1e-9);
    // stretch is refused
    assert!(
        gmclip_app::backdrop::dispatch(
            &c,
            &pid,
            "backdrop_set",
            &json!({"cut": cut, "path": png, "fit": "stretch"})
        )
        .is_err()
    );
    // a picked video on top, at the playhead, for its own length (here capped at 60 s)
    let r = call(
        "video_overlay_add",
        json!({"cut": cut, "frame": 30, "path": video, "loop": false}),
    );
    assert!(r["overlay"].is_string());
    let pl = c.planned(&pid, &cut).unwrap();
    let ev = &pl.plan.clip_events.as_ref().unwrap()[0];
    assert_eq!(ev.out.start(), 30);
    assert_eq!(ev.picture.r#loop, None);
    // and the view lists it, with the backdrop
    let view = gmclip_app::studio::overlays_view(&c, &pid, &cut).unwrap();
    assert!(
        view["overlays"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "video" && o["out"][0] == 30)
    );
    assert_eq!(view["backdrop"]["video"], false);
    // a frame of the cut renders through the export's compose with both on it
    #[cfg(target_os = "macos")]
    {
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        let shots = gmclip_render::export::render_frames(&pl.plan, &mut inputs, &[45]).unwrap();
        assert_eq!(shots.len(), 1);
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0031: "Pin to motion" on a box callout over a moving square (an ffmpeg-made
/// video, a dev tool): Apple Vision follows it through the take, the overlay is pinned
/// (one undo step), and the plan moves the callout with it.
#[cfg(target_os = "macos")]
#[test]
fn pin_to_motion_follows_a_moving_square() {
    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
    if !std::path::Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let (c, root) = core("track");
    std::fs::create_dir_all(&root).unwrap();
    let video = root.join("square.mp4");
    let out = std::process::Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "smptehdbars=s=1280x720:r=30:d=3"])
        .args(["-f", "lavfi", "-i", "color=c=white:s=80x80:r=30:d=3"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
        .args([
            "-filter_complex",
            "[1]drawgrid=w=16:h=16:t=4:c=black[sq];[0][sq]overlay=x='200+t*130':y=320,format=yuv420p[v]",
            "-map",
            "[v]",
            "-map",
            "2:a",
        ])
        .args(["-c:v", "libx264", "-crf", "14", "-bf", "0", "-c:a", "aac", "-ac", "2"])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let o = gmclip_app::video::open_video(&c, &video).unwrap();
    let pid = o["project"].as_str().unwrap().to_owned();
    let cut = o["cut"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    let (cw, ch) = (
        pl.plan.output.canvas.w.get() as f64,
        pl.plan.output.canvas.h.get() as f64,
    );
    let part = c
        .project(&pid)
        .unwrap()
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Part>()
        .next()
        .unwrap()
        .id;
    // a box over the square's start (centre 240, 360 of 1280×720)
    let (sx, sy) = (240.0 / 1280.0, 360.0 / 720.0);
    let ov = c
        .call(
            &pid,
            "overlay_add",
            json!({"cut": cut, "kind": "shape", "start": {"part_start": {"part": part.0.to_string()}}, "end": {"dur_ms": 2000},
                   "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": sx - 0.05, "y": sy - 0.08, "w": 0.1, "h": 0.16},
                   "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 6}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = gmclip_app::backdrop::dispatch(
        &c,
        &pid,
        "overlay_track",
        &json!({"cut": cut, "overlay": ov}),
    )
    .unwrap();
    eprintln!("pinned: {r}");
    assert_eq!(r["lost"], false, "{r}");
    assert!(r["keys"].as_u64().unwrap() >= 55, "{r}");
    let pl = c.planned(&pid, &cut).unwrap();
    let t = &pl.plan.tracks.as_ref().expect("a track in the plan")[0];
    // 2 s later the square is 260 source px right: on this canvas, × cw/1280
    let (dx, dy) = t.offset(0, 59);
    let want = 130.0 * 59.0 / 30.0 * cw / 1280.0;
    assert!(
        (f64::from(dx) - want).abs() < 0.03 * cw,
        "moved {dx} px, want ≈ {want:.0}"
    );
    assert!(f64::from(dy).abs() < 0.02 * ch, "moved {dy} px down");
    // the view says it is pinned; unpinning is one command
    let view = gmclip_app::studio::overlays_view(&c, &pid, &cut).unwrap();
    assert!(view["overlays"][0]["tracked"].as_u64().unwrap() >= 55);
    c.call(
        &pid,
        "overlay_track_set",
        json!({"overlay": ov, "track": null}),
    )
    .unwrap();
    assert!(c.planned(&pid, &cut).unwrap().plan.tracks.is_none());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A `secs`-long 1280×720 30 fps video of colour bars with an 80 px gridded square moving
/// right at 130 px/s from x 200 (centre 240 + 130 t, 360), made with ffmpeg (a dev tool).
#[cfg(target_os = "macos")]
fn square_video(root: &std::path::Path, secs: u32) -> Option<PathBuf> {
    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
    if !std::path::Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return None;
    }
    std::fs::create_dir_all(root).unwrap();
    let video = root.join("square.mp4");
    let out = std::process::Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("smptehdbars=s=1280x720:r=30:d={secs}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c=white:s=80x80:r=30:d={secs}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:sample_rate=48000:duration={secs}"))
        .args([
            "-filter_complex",
            "[1]drawgrid=w=16:h=16:t=4:c=black[sq];[0][sq]overlay=x='200+t*130':y=320,format=yuv420p[v]",
            "-map",
            "[v]",
            "-map",
            "2:a",
        ])
        .args(["-c:v", "libx264", "-crf", "14", "-bf", "0", "-c:a", "aac", "-ac", "2"])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(video)
}

/// The video opened as a project whose cut plays two takes of it, 0–2 s and 3–5 s
/// (captions off): `(core, project, cut, take ids)`.
#[cfg(target_os = "macos")]
fn two_takes(c: &Arc<AppCore>, video: &std::path::Path) -> (String, String, Vec<String>) {
    const SEC: i64 = 705_600_000;
    let o = gmclip_app::video::open_video(c, video).unwrap();
    let pid = o["project"].as_str().unwrap().to_owned();
    let cut = o["cut"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let part = c
        .project(&pid)
        .unwrap()
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Part>()
        .next()
        .unwrap()
        .id
        .0
        .to_string();
    c.call(
        &pid,
        "part_trim",
        json!({"part": part, "src_in": 0, "src_out": 5 * SEC}),
    )
    .unwrap();
    let mid = c
        .call(&pid, "part_split", json!({"part": part, "at": 2 * SEC}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = c
        .call(&pid, "part_split", json!({"part": mid, "at": 3 * SEC}))
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(&pid, "part_delete", json!({"part": mid})).unwrap();
    (pid, cut, vec![part, second])
}

/// Decision 0033: "Pin to motion" on a callout spanning two takes of the moving square (0–2 s
/// and 3–5 s): Vision follows it through the second take too (the 1 s between is tracked
/// through), so the callout is where the square is at 3 s when the second take starts.
#[cfg(target_os = "macos")]
#[test]
fn pin_to_motion_follows_across_takes() {
    let (c, root) = core("track-takes");
    let Some(video) = square_video(&root, 6) else {
        return;
    };
    let (pid, cut, parts) = two_takes(&c, &video);
    let pl = c.planned(&pid, &cut).unwrap();
    assert_eq!(pl.plan.output.frames, 120);
    let cw = pl.plan.output.canvas.w.get() as f64;
    let ov = c
        .call(
            &pid,
            "overlay_add",
            json!({"cut": cut, "kind": "shape", "start": {"part_start": {"part": parts[0]}}, "end": {"dur_ms": 4000},
                   "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": 240.0 / 1280.0 - 0.05, "y": 0.42, "w": 0.1, "h": 0.16},
                   "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 6}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = gmclip_app::backdrop::dispatch(
        &c,
        &pid,
        "overlay_track",
        &json!({"cut": cut, "overlay": ov}),
    )
    .unwrap();
    eprintln!("pinned: {r}");
    assert_eq!(r["lost"], false, "{r}");
    assert_eq!(r["takes"], 2, "{r}");
    let pl = c.planned(&pid, &cut).unwrap();
    let t = &pl.plan.tracks.as_ref().expect("a track")[0];
    assert!(
        t.keys.iter().any(|k| k.frame >= 60),
        "keys on the second take"
    );
    // the second take starts at 3 s: 390 source px right of the start; it ends 4.97 s in
    for (f, secs) in [(59u64, 59.0 / 30.0), (60, 3.0), (119, 3.0 + 59.0 / 30.0)] {
        let want = 130.0 * secs * cw / 1280.0;
        let dx = f64::from(t.offset(0, f).0);
        assert!(
            (dx - want).abs() < 0.03 * cw,
            "frame {f}: moved {dx}, want ≈ {want:.0}"
        );
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0033: "Blur here" puts a blur cover over the square at the start, and "Pin to
/// motion" on the cover follows the square through both takes: the cover's place moves with
/// it, its crop by the same move in the video's pixels; the overlays view lists the cover
/// as pinned; unpinning is one command.
#[cfg(target_os = "macos")]
#[test]
fn a_cover_pinned_to_motion_follows_the_square() {
    let (c, root) = core("track-cover");
    let Some(video) = square_video(&root, 6) else {
        return;
    };
    let (pid, cut, _) = two_takes(&c, &video);
    let pl = c.planned(&pid, &cut).unwrap();
    let (cw, ch) = (
        pl.plan.output.canvas.w.get() as f64,
        pl.plan.output.canvas.h.get() as f64,
    );
    // where the screen layer draws the square's centre (240, 360) on frame 0
    let d = gmclip_render::compositor::frame_draw(&pl.plan, 0).unwrap();
    let l = d
        .layers
        .iter()
        .filter(|l| !l.is_extra())
        .max_by(|a, b| (a.place.w * a.place.h).total_cmp(&(b.place.w * b.place.h)))
        .unwrap();
    let (px, py) = (
        l.place.x + (240.0 - l.crop.x) * l.place.w / l.crop.w,
        l.place.y + (360.0 - l.crop.y) * l.place.h / l.crop.h,
    );
    let k = l.place.w / l.crop.w;
    let added = gmclip_app::backdrop::dispatch(
        &c,
        &pid,
        "cover_add",
        &json!({"cut": cut, "frame": 0, "x": px / cw, "y": py / ch, "w": 100.0 * k / cw}),
    )
    .unwrap();
    let layer = added["layer"].as_str().unwrap().to_owned();
    let view = gmclip_app::studio::overlays_view(&c, &pid, &cut).unwrap();
    assert_eq!(view["covers"][0]["id"], layer.as_str());
    assert_eq!(view["covers"][0]["style"], "blur");
    let r = gmclip_app::backdrop::dispatch(
        &c,
        &pid,
        "cover_track",
        &json!({"cut": cut, "layer": layer, "frame": 0}),
    )
    .unwrap();
    eprintln!("cover pinned: {r}");
    assert_eq!(r["lost"], false, "{r}");
    let pl = c.planned(&pid, &cut).unwrap();
    let cover = pl.plan.parts[0]
        .layers
        .iter()
        .find(|l| l.cover.is_some())
        .expect("the cover on the first take");
    let t = cover.track.as_ref().expect("a pinned cover");
    let want = 130.0 * 59.0 / 30.0 * k;
    let (dx, dy) = t.offset(59);
    assert!(
        (f64::from(dx) - want).abs() < 0.03 * cw,
        "moved {dx}, want ≈ {want:.0}"
    );
    assert!(f64::from(dy).abs() < 0.02 * ch, "moved {dy} down");
    let d = gmclip_render::compositor::frame_draw(&pl.plan, 59).unwrap();
    let drawn = d.layers.iter().find(|l| l.cover.is_some()).unwrap();
    assert!((drawn.place.x - cover.place.x - f64::from(dx)).abs() < 1e-9);
    // and through the second take (3–5 s): where the square is at 3 s when it starts
    assert_eq!(r["takes"], 2, "{r}");
    let t2 = pl.plan.parts[1]
        .layers
        .iter()
        .find(|l| l.cover.is_some())
        .and_then(|l| l.track.clone())
        .expect("the cover pinned on the second take");
    let want = 130.0 * 3.0 * k;
    let dx = f64::from(t2.offset(60).0);
    assert!(
        (dx - want).abs() < 0.03 * cw,
        "second take moved {dx}, want ≈ {want:.0}"
    );
    let view = gmclip_app::studio::overlays_view(&c, &pid, &cut).unwrap();
    assert!(view["covers"][0]["tracked"].as_u64().unwrap() >= 55);
    c.call(
        &pid,
        "layer_track_set",
        json!({"layer": layer, "track": null}),
    )
    .unwrap();
    let pl = c.planned(&pid, &cut).unwrap();
    assert!(
        pl.plan
            .parts
            .iter()
            .flat_map(|p| &p.layers)
            .all(|l| l.track.is_none())
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
