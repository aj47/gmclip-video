//! Decision 0045: the recorded cursor is drawn from its input track through the layer
//! that shows the source, just above it: a tighter crop moves it and draws it larger,
//! exactly as a cursor baked into the pixels would be; it is clipped to the place, fades
//! when idle, and a track that cannot be read draws nothing and leaves a note.
//!
//! Decision 0046, the cursor's feel: the spring is the same seeked or played through and
//! is the raw path at smoothness 0, shake is held still, a loop ends where the part
//! began, a ripple fills and a click shrinks the cursor, and motion blur is off unless
//! asked for.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_model::rec::{InputTrack, ShapeInfo};
use gmclip_plan::Plan;
use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, compose_with};
use gmclip_render::compositor::cursor::CursorLayer;
use gmclip_render::compositor::{ChromaSiting, ComposeError, frame_draw};
use serde_json::{Value, json};

/// Source time of part 0's first frame (first_frame 79612 at 30 fps), µs.
const T0: i64 = 79_612 * 1_000_000 / 30;

/// A 2560×1440 track whose cursor rests at (1300, 500) from 1 s before `T0` to 2 s after,
/// one 8×8 pt shape (16×16 px, hotspot top left) at backing scale 2.
fn track() -> InputTrack {
    let mut t = InputTrack::new(2560, 1440, 2.0);
    t.shapes.push(ShapeInfo {
        sha256: "aa".repeat(32),
        w: 16,
        h: 16,
        hot_x: 0.0,
        hot_y: 0.0,
        pt_w: 8.0,
        pt_h: 8.0,
    });
    t.cursor = (0..30)
        .map(|i| (T0 - 1_000_000 + i * 100_000, 1300.0, 500.0, 0))
        .collect();
    t
}

/// Writes the track and a white 16×16 PNG to a fresh folder: (track, png) paths.
fn files(name: &str, t: &InputTrack) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("gmclip-cursor-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tp = dir.join("input.json");
    std::fs::write(&tp, serde_json::to_vec(t).unwrap()).unwrap();
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: 16,
        height: 16,
        px: vec![255; 16 * 16 * 4],
    })
    .unwrap();
    let ip = dir.join("arrow.png");
    std::fs::write(&ip, png).unwrap();
    (
        tp.to_string_lossy().into_owned(),
        ip.to_string_lossy().into_owned(),
    )
}

/// The RP example on `/1`, a 64×64 canvas, part 0's screen layer `crop` into the whole
/// canvas, with the cursor.
fn plan(paths: &(String, String), crop: [f64; 4], cursor: Value) -> Plan {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["output"]["canvas"] = json!({"w": 64, "h": 64});
    v["images"] =
        json!([{"id": "arrow", "path": paths.1, "sha256": "aa".repeat(32), "w": 16, "h": 16}]);
    let mut c = json!({"id": "cur", "source": "mkv1440", "path": paths.0,
        "sha256": "bb".repeat(32), "shapes": ["arrow"], "scale": 2});
    for (k, x) in cursor.as_object().unwrap() {
        c[k] = x.clone();
    }
    v["cursors"] = json!([c]);
    let l = &mut v["parts"][0]["layers"][0];
    l["crop"] = json!({"x": crop[0], "y": crop[1], "w": crop[2], "h": crop[3]});
    l["place"] = json!({"x": 0, "y": 0, "w": 64, "h": 64});
    l["cursor"] = json!("cur");
    gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
}

/// (index among the layers, x, y, w, h) of each cursor tile on frame `f`.
fn tiles(p: &Plan, f: u64) -> Vec<(usize, i32, i32, u32, u32)> {
    let draw = frame_draw(p, f).unwrap();
    CursorLayer::new(p)
        .tiles(p, &draw)
        .iter()
        .map(|(i, t)| (*i, t.x, t.y, t.width, t.height))
        .collect()
}

#[test]
fn the_cursor_moves_and_scales_with_the_crop() {
    let paths = files("zoom", &track());
    // 256 source px into 64: ¼ zoom; (1300, 500) lands at (25, 25), 8 pt × 2 × ¼ × 2 = 8 px
    let wide = plan(&paths, [1200.0, 400.0, 256.0, 256.0], json!({}));
    assert_eq!(
        tiles(&wide, 0),
        [(1, 25, 25, 8, 8)],
        "just above the screen layer"
    );
    // a 2× tighter crop: twice the size, moved with the picture
    let tight = plan(&paths, [1236.0, 436.0, 128.0, 128.0], json!({}));
    assert_eq!(tiles(&tight, 0), [(1, 32, 32, 16, 16)]);
    // near the place's edge it is clipped to the place
    let edge = plan(&paths, [1180.0, 380.0, 128.0, 128.0], json!({}));
    assert_eq!(tiles(&edge, 0), [(1, 60, 60, 4, 4)]);

    // drawn by the compositor: white where the tile is, the picture around it
    let draw = frame_draw(&tight, 0).unwrap();
    let among = CursorLayer::new(&tight).tiles(&tight, &draw);
    let mut pics = Flat(HashMap::from([(
        "mkv1440".to_owned(),
        Arc::new(flat(2560, 1440, 16)),
    )]));
    let c = compose_with(&draw, 64, 64, &mut pics, None, &among).unwrap();
    let at = |x: usize, y: usize| c.px[y * 64 + x];
    for (x, y) in [(32, 32), (47, 47), (40, 35)] {
        assert!(at(x, y)[0] > 0.99, "({x}, {y}) {:?}", at(x, y));
    }
    for (x, y) in [(31, 32), (48, 40), (40, 48)] {
        assert!(at(x, y)[0] < 0.01, "({x}, {y}) {:?}", at(x, y));
    }
}

#[test]
fn an_idle_cursor_fades_and_a_bad_track_draws_nothing() {
    let paths = files("idle", &track());
    let crop = [1236.0, 436.0, 128.0, 128.0];
    // still for 1 s at frame 0: past 600 ms + the 300 ms fade, gone
    assert!(tiles(&plan(&paths, crop, json!({"hide_idle_ms": 600})), 0).is_empty());
    assert_eq!(
        tiles(&plan(&paths, crop, json!({"hide_idle_ms": 5000})), 0).len(),
        1
    );
    // a click 100 ms ago: a ring under the cursor
    let mut t = track();
    t.clicks.push(gmclip_model::rec::Click {
        t: T0 - 100_000,
        x: 1300.0,
        y: 500.0,
        button: 0,
        down: true,
        count: 1,
    });
    let clicked = files("click", &t);
    let ts = tiles(&plan(&clicked, crop, json!({"clicks": true})), 0);
    assert_eq!(ts.len(), 2, "{ts:?}");
    assert_eq!(ts[1], (1, 32, 32, 16, 16));
    // a missing track and a malformed one: no cursor, a note, no error
    for bad in ["/nonexistent/input.json", paths.1.as_str()] {
        let p = plan(&(bad.to_owned(), paths.1.clone()), crop, json!({}));
        let layer = CursorLayer::new(&p);
        assert_eq!(layer.notes().len(), 1, "{:?}", layer.notes());
        assert!(tiles(&p, 0).is_empty());
    }
}

/// A track whose cursor sweeps right from (1250, 500) at 100 px/s from `T0`, at rest
/// before, sampled every 10 ms from 4 s before `T0` to 3 s after.
fn sweep() -> InputTrack {
    let mut t = track();
    t.cursor = (0..700)
        .map(|i| {
            let at = T0 - 4_000_000 + i * 10_000;
            let x = 1250.0 + (at - T0).max(0) as f32 * 0.0001;
            (at, x, 500.0, 0)
        })
        .collect();
    t
}

type Drawn = Vec<(usize, i32, i32, u32, u32, Vec<u8>)>;

/// Every cursor tile of frame `f`, with its pixels, from `layer`.
fn drawn(layer: &mut CursorLayer, p: &Plan, f: u64) -> Drawn {
    let draw = frame_draw(p, f).unwrap();
    layer
        .tiles(p, &draw)
        .iter()
        .map(|(i, t)| (*i, t.x, t.y, t.width, t.height, t.rgba.clone()))
        .collect()
}

/// A crop at 1 canvas px per source px around (1300, 500): the cursor is 32 px wide.
const NEAR: [f64; 4] = [1268.0, 468.0, 64.0, 64.0];

#[test]
fn the_spring_is_the_same_seeked_or_played_through() {
    let paths = files("spring", &sweep());
    let crop = [1200.0, 436.0, 256.0, 256.0];
    let feel = json!({"smoothness": 1, "shake_px": 3, "loop_ms": 500, "motion_blur": true,
        "click": "ripple", "click_shrink": true});
    let p = plan(&paths, crop, feel);
    // export: one layer, frame after frame; preview: back to front, a fresh layer each
    let mut straight = CursorLayer::new(&p);
    let played: Vec<Drawn> = (0..55).map(|f| drawn(&mut straight, &p, f)).collect();
    for f in (0..55).rev() {
        let seeked = drawn(&mut CursorLayer::new(&p), &p, f);
        assert_eq!(seeked, played[f as usize], "frame {f}");
    }
    // the spring lags the sweep: 500 ms in, behind the raw path
    let raw = plan(&paths, crop, json!({}));
    let (s, r) = (tiles(&p, 15)[0].1, tiles(&raw, 15)[0].1);
    assert!(s < r, "smoothed {s} behind raw {r}");
    // and at rest long enough it is where the raw path is
    assert_eq!(tiles(&p, 0), tiles(&raw, 0));
}

#[test]
fn smoothness_0_is_the_raw_path() {
    let paths = files("smooth0", &sweep());
    let raw = plan(&paths, NEAR, json!({}));
    let zero = plan(
        &paths,
        NEAR,
        json!({"smoothness": 0, "shake_px": 0, "loop_ms": 0}),
    );
    for f in 0..55 {
        assert_eq!(tiles(&zero, f), tiles(&raw, f), "frame {f}");
    }
    assert_ne!(tiles(&raw, 0), tiles(&raw, 30), "the raw path moves");
}

#[test]
fn shake_is_held_still() {
    // resting at (1300, 500), trembling ±1.5 px every 10 ms
    let mut t = track();
    t.cursor = (0..500)
        .map(|i| {
            let d = [0.0, 1.5, -1.5, 0.5, -1.0][i as usize % 5];
            (T0 - 2_000_000 + i * 10_000, 1300.0 + d, 500.0 - d, 0)
        })
        .collect();
    let paths = files("shake", &t);
    let xs = |p: &Plan| -> Vec<(i32, i32)> {
        (0..55)
            .map(|f| (tiles(p, f)[0].1, tiles(p, f)[0].2))
            .collect()
    };
    let trembling = xs(&plan(&paths, NEAR, json!({})));
    assert!(
        trembling.iter().any(|x| *x != trembling[0]),
        "{trembling:?}"
    );
    let held = xs(&plan(&paths, NEAR, json!({"shake_px": 4})));
    assert!(held.iter().all(|x| *x == held[0]), "{held:?}");
    // a real move goes through
    let paths = files("shake-move", &sweep());
    let wide = [1200.0, 436.0, 256.0, 256.0];
    let moving = xs(&plan(&paths, wide, json!({"shake_px": 4})));
    assert!(moving[54].0 > moving[0].0 + 40, "{moving:?}");
}

#[test]
fn a_loop_ends_where_the_part_began() {
    let paths = files("loop", &sweep());
    let crop = [1200.0, 436.0, 256.0, 256.0];
    let raw = plan(&paths, crop, json!({}));
    let looped = plan(&paths, crop, json!({"loop_ms": 1000}));
    assert_eq!(
        tiles(&looped, 54),
        tiles(&looped, 0),
        "the last frame is the first's"
    );
    assert_ne!(tiles(&raw, 54), tiles(&raw, 0));
    // before the last second, the path as recorded; inside it, on the way back
    assert_eq!(tiles(&looped, 20), tiles(&raw, 20));
    let (mid, from, to) = (
        tiles(&looped, 39)[0].1,
        tiles(&raw, 39)[0].1,
        tiles(&raw, 0)[0].1,
    );
    assert!(to < mid && mid < from, "{to} < {mid} < {from}");
}

#[test]
fn a_ripple_fills_and_a_click_shrinks_the_cursor() {
    let mut t = track();
    t.clicks.push(gmclip_model::rec::Click {
        t: T0 - 60_000,
        x: 1300.0,
        y: 500.0,
        button: 0,
        down: true,
        count: 1,
    });
    let paths = files("ripple", &t);
    // the mark's pixel at the click (canvas (32, 32))
    let centre = |cursor: Value| {
        let p = plan(&paths, NEAR, cursor);
        let d = drawn(&mut CursorLayer::new(&p), &p, 0);
        let (_, x, y, w, _, px) = &d[0];
        px[(((32 - y) * *w as i32 + (32 - x)) * 4 + 3) as usize]
    };
    assert_eq!(centre(json!({"clicks": true})), 0, "a ring is hollow");
    assert_eq!(centre(json!({"click": "ring"})), 0);
    let fill = centre(json!({"click": "ripple"}));
    assert!(
        (40..=70).contains(&fill),
        "a soft disc under the ripple: {fill}"
    );
    // `click: none` wins over `clicks`
    let none = plan(&paths, NEAR, json!({"clicks": true, "click": "none"}));
    assert_eq!(tiles(&none, 0).len(), 1, "the cursor only");

    // half way through the shrink: 80 % of 32 px, the hotspot where it was
    let plain = tiles(&plan(&paths, NEAR, json!({})), 0);
    assert_eq!(plain, [(1, 32, 32, 32, 32)]);
    let shrunk = tiles(&plan(&paths, NEAR, json!({"click_shrink": true})), 0);
    assert_eq!(shrunk, [(1, 32, 32, 26, 26)]);
    // 120 ms on, back to its size
    assert_eq!(
        tiles(&plan(&paths, NEAR, json!({"click_shrink": true})), 2),
        plain
    );
}

#[test]
fn motion_blur_is_off_unless_asked_for() {
    let paths = files("blur", &sweep());
    let crop = [1200.0, 436.0, 256.0, 256.0];
    let off = plan(&paths, crop, json!({}));
    let said_off = plan(&paths, crop, json!({"motion_blur": false}));
    let on = plan(&paths, crop, json!({"motion_blur": true}));
    // 100 px/s at ¼: under 2 canvas px a frame, not blurred; at 1×, 3.3 px, blurred
    for f in [0, 20, 40] {
        assert_eq!(tiles(&off, f), [tiles(&off, f)[0]]);
        assert_eq!(tiles(&off, f)[0].3, 8, "one cursor-sized tile");
        assert_eq!(tiles(&said_off, f), tiles(&off, f));
        assert_eq!(tiles(&on, f), tiles(&off, f), "too slow to blur at ¼");
    }
    let near = |v: Value| plan(&paths, NEAR, v);
    let (off, on) = (near(json!({})), near(json!({"motion_blur": true})));
    assert_eq!(tiles(&off, 0), tiles(&on, 0), "at rest, never blurred");
    let sharp = drawn(&mut CursorLayer::new(&off), &off, 15);
    let blurred = drawn(&mut CursorLayer::new(&on), &on, 15);
    assert_eq!((sharp.len(), blurred.len()), (1, 1));
    let (s, b) = (&sharp[0], &blurred[0]);
    assert_eq!(s.3, 32);
    assert!(
        b.3 > 32 && b.1 < s.1,
        "smeared back along the move: {:?} vs {:?}",
        b.1..b.1 + b.3 as i32,
        s.1..s.1 + s.3 as i32
    );
    // the trailing edge is part-covered, the middle where every copy lands is solid
    let a = |x: i32| b.5[((16 * b.3 as i32 + (x - b.1)) * 4 + 3) as usize];
    assert!(a(b.1) > 0 && a(b.1) < 255, "{}", a(b.1));
    assert_eq!(a(s.1 + 16), 255);
}

struct Flat(HashMap<String, Arc<Nv12Image>>);

impl FrameProvider for Flat {
    fn frame(&mut self, source: &str, _: u64) -> Result<Arc<Nv12Image>, ComposeError> {
        self.0
            .get(source)
            .cloned()
            .ok_or_else(|| ComposeError::Source(source.into()))
    }
}

fn flat(w: u32, h: u32, y: u8) -> Nv12Image {
    Nv12Image {
        width: w,
        height: h,
        y: vec![y; (w * h) as usize],
        uv: vec![128; (w.div_ceil(2) * h.div_ceil(2) * 2) as usize],
        siting: ChromaSiting::Left,
    }
}

/// R-COMP-1: the shared compose (`render_frames`, the preview's body and the export's
/// frame) draws the cursor where the CPU path does: a 320×180 grey source zoomed 4× into
/// a 640×360 canvas, a 4 pt cursor at (120, 70) drawn 16 px wide at (160, 100). The
/// source is made with ffmpeg (a dev tool); skipped without it. Needs Metal.
#[cfg(target_os = "macos")]
#[test]
fn the_shared_compose_draws_the_cursor() {
    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
    if !std::path::Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let mut t = InputTrack::new(320, 180, 1.0);
    t.shapes.push(ShapeInfo {
        sha256: "aa".repeat(32),
        w: 16,
        h: 16,
        hot_x: 0.0,
        hot_y: 0.0,
        pt_w: 4.0,
        pt_h: 4.0,
    });
    t.cursor = (0..20).map(|i| (i * 100_000, 120.0, 70.0, 0)).collect();
    let (track, png) = files("gpu", &t);
    let dir = std::path::Path::new(&track).parent().unwrap().to_owned();
    let src = dir.join("grey.mp4");
    let ok = std::process::Command::new(FFMPEG)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x404040:s=320x180:r=30:d=1",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
        ])
        .args([
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-bf", "0", "-c:a", "aac", "-ac", "2",
        ])
        .args(["-shortest"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(ok.success());
    let v = json!({
        "schema": "gmclip.renderplan/1",
        "output": {"canvas": {"w": 640, "h": 360}, "fps": {"num": 30, "den": 1},
            "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
            "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                      "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
            "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
            "container": {"moov_first": true, "edit_lists": false}, "frames": 10},
        "background": {"kind": "colour", "rgb": "#000000"},
        "sources": [{"id": "s1", "path": src.display().to_string()}],
        "images": [{"id": "arrow", "path": png, "sha256": "aa".repeat(32), "w": 16, "h": 16}],
        "cursors": [{"id": "cur", "source": "s1", "path": track, "sha256": "bb".repeat(32),
            "shapes": ["arrow"], "scale": 1}],
        "parts": [{"id": "p1", "out": [0, 10], "picture": {"source": "s1", "first_frame": 0},
            "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
            "layers": [{"id": "screen", "role": "screen", "crop": {"x": 80, "y": 45, "w": 160, "h": 90},
                        "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0, "cursor": "cur"}]}],
        "audio": {"loudness": {"mode": "as_recorded"}},
    });
    let plan: Plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(tiles(&plan, 0), [(1, 160, 100, 16, 16)]);
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let shots = gmclip_render::export::render_frames(&plan, &mut inputs, &[0]).unwrap();
    let img = &shots[0].1;
    let g = |x: usize, y: usize| img.px[(y * img.width as usize + x) * 3 + 1];
    for (x, y) in [(160, 100), (175, 115), (168, 108)] {
        assert!(g(x, y) > 245, "cursor at ({x}, {y}): {}", g(x, y));
    }
    for (x, y) in [(159, 108), (176, 108), (168, 116), (300, 300)] {
        assert!(g(x, y) < 100, "picture at ({x}, {y}): {}", g(x, y));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
