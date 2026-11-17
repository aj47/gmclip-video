//! Decision 0046: zoom sections in the one compose. A zoom eases a layer's crop from its
//! own to one `depth`× tighter around its target and back (ease-in-out); a cursor target
//! goes through a spring that gives a frame the same crop seeked or played through; the
//! recorded cursor follows the zoomed crop; camera layers shrink toward their corner; a
//! zoom that starts where another ends carries on from its crop.

use std::sync::Arc;

use gmclip_model::rec::{InputTrack, ShapeInfo};
use gmclip_plan::Plan;
use gmclip_plan::types::Rect;
use gmclip_render::compositor::cursor::CursorLayer;
use gmclip_render::compositor::frame_draw;
use gmclip_render::compositor::zoom::{ZoomLayer, ease_in_out, full_crop, lerp_rect};
use serde_json::{Value, json};

/// Source time of part 0's first frame (first_frame 79612 at 30 fps), µs.
const T0: i64 = 79_612 * 1_000_000 / 30;

/// Part 0's screen crop in the RP example.
const OWN: Rect = Rect {
    x: 1222.0,
    y: 392.0,
    w: 418.0,
    h: 688.0,
};

/// The RP example on `/1` with `zooms`, a cursor `cur` on part 0's layers (its files at
/// `paths`, which may not exist: a test hands the track over itself).
fn plan(zooms: Value, paths: (&str, &str)) -> Plan {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["images"] =
        json!([{"id": "arrow", "path": paths.1, "sha256": "aa".repeat(32), "w": 16, "h": 16}]);
    v["cursors"] = json!([{"id": "cur", "source": "mkv1440", "path": paths.0,
        "sha256": "bb".repeat(32), "shapes": ["arrow"], "scale": 1}]);
    v["parts"][0]["layers"][0]["cursor"] = json!("cur");
    v["zooms"] = zooms;
    gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
}

fn no_files() -> (&'static str, &'static str) {
    ("/nonexistent/input.json", "/nonexistent/arrow.png")
}

/// Layer `id`'s crop and place on frame `f`, zoomed by `z`.
fn layer(z: &mut ZoomLayer, p: &Plan, f: u64, id: &str) -> (Rect, Rect) {
    let mut d = frame_draw(p, f).unwrap();
    z.apply(p, &mut d);
    let l = d.layers.iter().find(|l| l.layer_id == id).unwrap();
    (l.crop, l.place)
}

fn close(a: Rect, b: Rect) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)]
        .iter()
        .all(|(x, y)| (x - y).abs() < 1e-6)
}

fn point_zoom(out: [u64; 2], ease: [u64; 2]) -> Value {
    json!({"id": "z", "out": out, "depth": 2, "target": {"point": [0.5, 0.5]},
        "ease_in": ease[0], "ease_out": ease[1],
        "layers": [{"part": "01-payoff-banana", "layer": "screen"}]})
}

#[test]
fn the_crop_eases_in_and_out_around_its_target() {
    let p = plan(json!([point_zoom([0, 55], [10, 10])]), no_files());
    let mut z = ZoomLayer::new(&p);
    // 2× tighter, centred on the crop's middle
    let full = Rect {
        x: 1222.0 + 209.0 - 104.5,
        y: 392.0 + 344.0 - 172.0,
        w: 209.0,
        h: 344.0,
    };
    assert!(close(full_crop(OWN, 2.0, (1431.0, 736.0)), full));
    let (c0, _) = layer(&mut z, &p, 0, "screen");
    assert!(close(c0, OWN), "progress 0: its own crop {c0:?}");
    let (c5, _) = layer(&mut z, &p, 5, "screen");
    assert_eq!(ease_in_out(0.5), 0.5);
    assert!(
        close(c5, lerp_rect(OWN, full, 0.5)),
        "progress ½: halfway {c5:?}"
    );
    for f in [10, 30, 44] {
        let (c, _) = layer(&mut z, &p, f, "screen");
        assert!(close(c, full), "progress 1 on frame {f}: {c:?}");
    }
    let (c54, _) = layer(&mut z, &p, 54, "screen");
    assert!(close(c54, OWN), "eased back out by its last frame {c54:?}");
    // the layers it does not name stay
    let mut d = frame_draw(&p, 30).unwrap();
    z.apply(&p, &mut d);
    let plain = frame_draw(&p, 30).unwrap();
    for (a, b) in d.layers.iter().zip(&plain.layers) {
        if a.layer_id != "screen" {
            assert_eq!(a, b);
        }
    }
    // the target stays inside the layer's own crop
    assert!(close(
        full_crop(OWN, 2.0, (0.0, 0.0)),
        Rect {
            x: 1222.0,
            y: 392.0,
            w: 209.0,
            h: 344.0
        }
    ));
}

/// A track whose cursor rests at (1300, 500) until `T0 + 0.5 s`, then at (1600, 900).
fn jump() -> InputTrack {
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
    t.cursor = (0..400)
        .map(|i| {
            let at = T0 - 1_000_000 + i * 10_000;
            let (x, y) = if at < T0 + 500_000 {
                (1300.0, 500.0)
            } else {
                (1600.0, 900.0)
            };
            (at, x, y, 0)
        })
        .collect();
    t
}

#[test]
fn a_cursor_target_springs_the_same_seeked_or_played() {
    let zooms = json!([{"id": "z", "out": [0, 55], "depth": 4, "target": {"cursor": "cur"},
        "ease_in": 0, "ease_out": 0,
        "layers": [{"part": "01-payoff-banana", "layer": "screen"}]}]);
    let p = plan(zooms, no_files());
    let track = Arc::new(jump());
    let mut played = ZoomLayer::new(&p);
    played.set_track("cur", track.clone());
    let seq: Vec<Rect> = (0..55)
        .map(|f| layer(&mut played, &p, f, "screen").0)
        .collect();
    for f in [40, 16, 3, 54] {
        let mut seeked = ZoomLayer::new(&p);
        seeked.set_track("cur", track.clone());
        let c = layer(&mut seeked, &p, f, "screen").0;
        assert_eq!(c, seq[f as usize], "frame {f} seeked");
    }
    let centre = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
    // at rest on the first target
    let (x0, y0) = centre(seq[0]);
    assert!(
        (x0 - 1300.0).abs() < 1e-6 && (y0 - 500.0).abs() < 1e-6,
        "{:?}",
        seq[0]
    );
    // the jump at frame 15 is followed smoothly: part way after 100 ms, nearly there
    // after a second (≈ 4 time constants), never past it
    let (x18, _) = centre(seq[18]);
    assert!(x18 > 1300.0 && x18 < 1500.0, "{x18}");
    let xs: Vec<f64> = seq.iter().map(|r| centre(*r).0).collect();
    assert!(
        xs.windows(2).all(|w| w[1] >= w[0] - 1e-9),
        "monotone: {xs:?}"
    );
    assert!((xs[54] - 1600.0).abs() < 25.0, "{}", xs[54]);
}

#[test]
fn the_recorded_cursor_follows_the_zoomed_crop() {
    let dir = std::env::temp_dir().join(format!("gmclip-zoom-cursor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tp = dir.join("input.json");
    std::fs::write(&tp, serde_json::to_vec(&jump()).unwrap()).unwrap();
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: 16,
        height: 16,
        px: vec![255; 16 * 16 * 4],
    })
    .unwrap();
    let ip = dir.join("arrow.png");
    std::fs::write(&ip, png).unwrap();
    let paths = (tp.to_str().unwrap(), ip.to_str().unwrap());
    let mut zoom = point_zoom([0, 55], [0, 0]);
    zoom["target"] = json!({"point": [0.2, 0.15]});
    let p = plan(json!([zoom]), paths);
    let mut z = ZoomLayer::new(&p);
    assert!(z.notes().is_empty(), "{:?}", z.notes());
    let mut d = frame_draw(&p, 3).unwrap();
    z.apply(&p, &mut d);
    let zoomed = d.layers[0].crop;
    // (1300, 500) is inside the zoomed crop's top-left
    assert!(
        close(
            zoomed,
            Rect {
                x: 1222.0,
                y: 392.0,
                w: 209.0,
                h: 344.0
            }
        ),
        "{zoomed:?}"
    );
    let tiles = |d: &gmclip_render::compositor::FrameDraw| -> Vec<(usize, i32, i32, u32, u32)> {
        CursorLayer::new(&p)
            .tiles(&p, d)
            .iter()
            .map(|(i, t)| (*i, t.x, t.y, t.width, t.height))
            .collect()
    };
    let at_zoom = tiles(&d);
    let plain = tiles(&frame_draw(&p, 3).unwrap());
    assert_eq!(at_zoom.len(), 1);
    assert_ne!(at_zoom, plain, "the cursor moved with the zoom");
    // where the zoomed crop puts (1300, 500), twice the size
    let pl = d.layers[0].place;
    let (sx, sy) = (pl.w / zoomed.w, pl.h / zoomed.h);
    let want_x = (pl.x + (1300.0 - zoomed.x) * sx).round() as i32;
    let want_y = (pl.y + (500.0 - zoomed.y) * sy).round() as i32;
    assert_eq!((at_zoom[0].1, at_zoom[0].2), (want_x, want_y));
    assert_eq!(at_zoom[0].3, plain[0].3 * 2, "drawn 2× as the picture is");
}

#[test]
fn camera_layers_shrink_toward_their_corner() {
    let mut z0 = point_zoom([55, 176], [20, 20]);
    z0["layers"] = json!([{"part": "02-claim", "layer": "screen"}]);
    z0["camera_scale"] = json!(0.5);
    z0["camera_layers"] = json!([{"part": "02-claim", "layer": "cam"}]);
    let p = plan(json!([z0]), no_files());
    let mut z = ZoomLayer::new(&p);
    // part 1's cam place (160, 1120, 760, 520): its centre is in the bottom-left quarter
    // of the 1080×1920 canvas, so its left and bottom edges stay
    let own = Rect {
        x: 160.0,
        y: 1120.0,
        w: 760.0,
        h: 520.0,
    };
    assert_eq!(layer(&mut z, &p, 55, "cam").1, own, "progress 0");
    let half = layer(&mut z, &p, 65, "cam").1;
    assert!(
        close(
            half,
            Rect {
                x: 160.0,
                y: 1120.0 + 520.0 * 0.25,
                w: 570.0,
                h: 390.0
            }
        ),
        "{half:?}"
    );
    let full = layer(&mut z, &p, 100, "cam").1;
    assert!(
        close(
            full,
            Rect {
                x: 160.0,
                y: 1380.0,
                w: 380.0,
                h: 260.0
            }
        ),
        "{full:?}"
    );
}

#[test]
fn a_zoom_that_starts_where_another_ends_carries_on_from_its_crop() {
    let a = point_zoom([0, 30], [10, 10]);
    let mut b = point_zoom([30, 55], [10, 10]);
    b["id"] = json!("z2");
    b["depth"] = json!(3);
    b["target"] = json!({"point": [0.2, 0.2]});
    let p = plan(json!([a, b]), no_files());
    let mut z = ZoomLayer::new(&p);
    let a_full = full_crop(OWN, 2.0, (1431.0, 736.0));
    let b_full = full_crop(OWN, 3.0, (1222.0 + 0.2 * 418.0, 392.0 + 0.2 * 688.0));
    assert!(
        close(layer(&mut z, &p, 29, "screen").0, a_full),
        "no ease out before the next"
    );
    assert!(
        close(layer(&mut z, &p, 30, "screen").0, a_full),
        "starts from its crop"
    );
    assert!(close(
        layer(&mut z, &p, 35, "screen").0,
        lerp_rect(a_full, b_full, 0.5)
    ));
    assert!(close(layer(&mut z, &p, 40, "screen").0, b_full));
    assert!(
        close(layer(&mut z, &p, 54, "screen").0, OWN),
        "eases out at the end"
    );
}

/// The zoom goes through the shared compose (`render_frames`, the preview's and export's):
/// the picture is drawn through the zoomed crop and the cursor with it, 2× larger.
#[cfg(target_os = "macos")]
#[test]
fn the_shared_compose_draws_the_zoom() {
    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
    if !std::path::Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-zoom-gpu-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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
    let track = dir.join("input.json");
    std::fs::write(&track, serde_json::to_vec(&t).unwrap()).unwrap();
    let png = dir.join("arrow.png");
    std::fs::write(
        &png,
        gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
            width: 16,
            height: 16,
            px: vec![255; 16 * 16 * 4],
        })
        .unwrap(),
    )
    .unwrap();
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
    let plan = |zooms: Value| -> Plan {
        let mut v = json!({
            "schema": "gmclip.renderplan/1",
            "output": {"canvas": {"w": 640, "h": 360}, "fps": {"num": 30, "den": 1},
                "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
                "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                          "rate": {"mode": "vbr", "target_bps": 8000000, "max_bps": 12000000}, "timescale": 15360},
                "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
                "container": {"moov_first": true, "edit_lists": false}, "frames": 10},
            "background": {"kind": "colour", "rgb": "#000000"},
            "sources": [{"id": "s1", "path": src.display().to_string()}],
            "images": [{"id": "arrow", "path": png.display().to_string(), "sha256": "aa".repeat(32), "w": 16, "h": 16}],
            "cursors": [{"id": "cur", "source": "s1", "path": track.display().to_string(), "sha256": "bb".repeat(32),
                "shapes": ["arrow"], "scale": 1}],
            "parts": [{"id": "p1", "out": [0, 10], "picture": {"source": "s1", "first_frame": 0},
                "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
                "layers": [{"id": "screen", "role": "screen", "crop": {"x": 80, "y": 45, "w": 160, "h": 90},
                            "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0, "cursor": "cur"}]}],
            "audio": {"loudness": {"mode": "as_recorded"}},
        });
        v["zooms"] = zooms;
        gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
    };
    // 2× on the crop's top-left corner: crop (80, 45, 80, 45), 8 canvas px per source px,
    // so the cursor at (120, 70) lands at (320, 200), 32 px
    let p = plan(
        json!([{"id": "z", "out": [0, 10], "depth": 2, "target": {"point": [0, 0]},
        "ease_in": 0, "ease_out": 0, "layers": [{"part": "p1", "layer": "screen"}]}]),
    );
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let shots = gmclip_render::export::render_frames(&p, &mut inputs, &[0]).unwrap();
    let img = &shots[0].1;
    let g = |x: usize, y: usize| img.px[(y * img.width as usize + x) * 3 + 1];
    for (x, y) in [(320, 200), (350, 230), (335, 215)] {
        assert!(g(x, y) > 245, "cursor at ({x}, {y}): {}", g(x, y));
    }
    for (x, y) in [(319, 215), (353, 215), (335, 233), (100, 300)] {
        assert!(g(x, y) < 100, "picture at ({x}, {y}): {}", g(x, y));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------
// Motion blur (off unless asked for)
// ---------------------------------------------------------------------------------------

/// Any frame of any source: a 2560×1440 picture with detail everywhere.
struct Textured(Arc<gmclip_render::compositor::cpu::Nv12Image>);

impl gmclip_render::compositor::cpu::FrameProvider for Textured {
    fn frame(
        &mut self,
        _: &str,
        _: u64,
    ) -> Result<
        Arc<gmclip_render::compositor::cpu::Nv12Image>,
        gmclip_render::compositor::ComposeError,
    > {
        Ok(self.0.clone())
    }
}

fn textured() -> Textured {
    let (w, h) = (2560u32, 1440u32);
    let y = (0..h)
        .flat_map(|y| (0..w).map(move |x| (16 + (x * 7 + y * 3 + (x / 13) * (y / 11)) % 220) as u8))
        .collect();
    Textured(Arc::new(gmclip_render::compositor::cpu::Nv12Image {
        width: w,
        height: h,
        y,
        uv: [128, 128].repeat((w as usize / 2) * (h as usize / 2)),
        siting: gmclip_render::compositor::ChromaSiting::Left,
    }))
}

/// Frame `f` drawn on the CPU at a quarter of the canvas, through the zooms (and their
/// blur): the draw list and its pixels.
fn drawn(
    z: &mut ZoomLayer,
    p: &Plan,
    f: u64,
    pics: &mut Textured,
) -> (gmclip_render::compositor::FrameDraw, Vec<[f32; 4]>) {
    let mut d = frame_draw(p, f).unwrap();
    z.apply(p, &mut d);
    z.expand_blur(&mut d);
    for l in &mut d.layers {
        l.place = Rect {
            x: l.place.x / 4.0,
            y: l.place.y / 4.0,
            w: l.place.w / 4.0,
            h: l.place.h / 4.0,
        };
    }
    let img = gmclip_render::compositor::cpu::compose(&d, 270, 480, pics).unwrap();
    (d, img.px)
}

#[test]
fn motion_blur_draws_only_the_frames_where_the_zoom_moves() {
    let off = plan(json!([point_zoom([0, 55], [10, 10])]), no_files());
    let mut on_z = point_zoom([0, 55], [10, 10]);
    on_z["motion_blur"] = json!(true);
    let on = plan(json!([on_z]), no_files());
    assert_eq!(
        off.zooms.as_ref().unwrap()[0].motion_blur,
        None,
        "off by default"
    );
    let (mut zo, mut zb) = (ZoomLayer::new(&off), ZoomLayer::new(&on));
    let mut pics = textured();

    // off: exactly the zoom as before (no copies; the draw is apply's alone)
    for f in [0, 3, 30, 50] {
        let mut d = frame_draw(&off, f).unwrap();
        zo.apply(&off, &mut d);
        let before = d.clone();
        assert_eq!(
            zo.expand_blur(&mut d),
            (0..=d.layers.len()).collect::<Vec<_>>()
        );
        assert_eq!(d, before, "frame {f}");
    }

    // still frames (its first, settled, after it): the same pixels as without blur
    for f in [0, 30, 55] {
        let (a, pa) = drawn(&mut zo, &off, f, &mut pics);
        let (b, pb) = drawn(&mut zb, &on, f, &mut pics);
        assert_eq!(a, b, "frame {f}");
        assert_eq!(pa, pb, "frame {f}");
    }

    // a moving frame: five copies of the screen across the last half frame, the k-th at 1/k
    let (a, pa) = drawn(&mut zo, &off, 3, &mut pics);
    let (b, pb) = drawn(&mut zb, &on, 3, &mut pics);
    let copies: Vec<_> = b.layers.iter().filter(|l| l.layer_id == "screen").collect();
    assert_eq!(copies.len(), 5);
    assert_eq!(
        copies.iter().map(|l| l.opacity).collect::<Vec<_>>(),
        [1.0, 0.5, 1.0 / 3.0, 0.25, 0.2]
    );
    let own = a.layers.iter().find(|l| l.layer_id == "screen").unwrap();
    assert_eq!(copies[0].crop, own.crop, "the frame's own crop first");
    let mut d2 = frame_draw(&on, 2).unwrap();
    zb.apply(&on, &mut d2);
    let prev = d2
        .layers
        .iter()
        .find(|l| l.layer_id == "screen")
        .unwrap()
        .crop;
    assert!(
        close(copies[4].crop, lerp_rect(own.crop, prev, 0.5)),
        "half a frame back"
    );
    assert_eq!(b.layers.len(), a.layers.len() + 4);
    assert!(pa != pb, "a moving frame is blurred");
    // the copies draw the mean of the five sub-samples
    let mut mean = vec![[0f32; 4]; pa.len()];
    for c in &copies {
        let mut one = a.clone();
        let l = one
            .layers
            .iter_mut()
            .find(|l| l.layer_id == "screen")
            .unwrap();
        l.crop = c.crop;
        let px = gmclip_render::compositor::cpu::compose(&one, 270, 480, &mut pics)
            .unwrap()
            .px;
        for (m, p) in mean.iter_mut().zip(px) {
            for k in 0..4 {
                m[k] += p[k] / 5.0;
            }
        }
    }
    // (inside the place, drawn at a quarter: its anti-aliased edge is only near it)
    let pl = own.place;
    let within = |i: usize, m: f64| {
        let (x, y) = ((i % 270) as f64, (i / 270) as f64);
        x + 1.0 + m >= pl.x && x <= pl.x + pl.w + m && y + 1.0 + m >= pl.y && y <= pl.y + pl.h + m
    };
    let inside = |i: usize| within(i, 0.0);
    let interior = |i: usize| !within(i, 0.0) || within(i, -1.0);
    let worst = mean
        .iter()
        .zip(&pb)
        .enumerate()
        .filter(|(i, _)| inside(*i) && interior(*i))
        .map(|(_, (m, p))| (0..3).map(|k| (m[k] - p[k]).abs()).fold(0.0, f32::max))
        .fold(0.0, f32::max);
    assert!(worst < 1e-3, "the mean of the sub-samples, off by {worst}");
    // pixels outside the screen layer's place are untouched
    let outside: Vec<_> = pa
        .iter()
        .zip(&pb)
        .enumerate()
        .filter(|(i, (x, y))| !inside(*i) && x != y)
        .map(|(i, (x, y))| (i % 270, i / 270, *x, *y))
        .take(5)
        .collect();
    assert!(outside.is_empty(), "{outside:?} {pl:?}");
}

#[test]
fn tiles_among_the_layers_follow_the_copies() {
    let mut on_z = point_zoom([0, 55], [10, 10]);
    on_z["motion_blur"] = json!(true);
    let on = plan(json!([on_z]), no_files());
    let mut z = ZoomLayer::new(&on);
    let mut d = frame_draw(&on, 3).unwrap();
    z.apply(&on, &mut d);
    assert!(z.blurs());
    let i = d
        .layers
        .iter()
        .position(|l| l.layer_id == "screen")
        .unwrap();
    let n = d.layers.len();
    d.tiles_at = vec![
        (i, "under".into()),
        (i + 1, "over".into()),
        (n, "top".into()),
    ];
    let mut cursor = vec![(i + 1, ())];
    let map = z.expand_blur(&mut d);
    gmclip_render::compositor::zoom::remap_tiles(&mut cursor, &map);
    assert_eq!(
        d.tiles_at,
        [
            (i, "under".to_owned()),
            (i + 5, "over".into()),
            (n + 4, "top".into())
        ]
    );
    assert_eq!(cursor, [(i + 5, ())], "the cursor above every copy");
    assert!(!z.blurs(), "used once");
}

/// The shared compose (`render_frames`, the preview's and export's, on the GPU) draws a
/// zoom's motion blur: the same frames as without it where the crop holds still, others
/// where it moves.
#[cfg(target_os = "macos")]
#[test]
fn the_shared_compose_draws_the_motion_blur() {
    const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
    if !std::path::Path::new(FFMPEG).exists() {
        eprintln!("skipped: no {FFMPEG}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gmclip-zoom-blur-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("test.mp4");
    let ok = std::process::Command::new(FFMPEG)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=320x180:r=30:d=1",
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
    let plan = |blur: bool| -> Plan {
        let mut z = json!({"id": "z", "out": [0, 10], "depth": 2, "target": {"point": [0.3, 0.3]},
            "ease_in": 6, "ease_out": 0, "layers": [{"part": "p1", "layer": "screen"}]});
        if blur {
            z["motion_blur"] = json!(true);
        }
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
            "parts": [{"id": "p1", "out": [0, 10], "picture": {"source": "s1", "first_frame": 0},
                "audio": {"source": "s1", "first_sample": 0, "rate": 48000},
                "layers": [{"id": "screen", "role": "screen", "crop": {"x": 0, "y": 0, "w": 320, "h": 180},
                            "place": {"x": 0, "y": 0, "w": 640, "h": 360}, "z": 0}]}],
            "audio": {"loudness": {"mode": "as_recorded"}},
            "zooms": [z],
        });
        gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
    };
    let frames = [0, 3, 8];
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let off = gmclip_render::export::render_frames(&plan(false), &mut inputs, &frames).unwrap();
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let on = gmclip_render::export::render_frames(&plan(true), &mut inputs, &frames).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(off[0].1.px, on[0].1.px, "its first frame: still");
    assert_eq!(off[2].1.px, on[2].1.px, "settled: still");
    let differ = off[1]
        .1
        .px
        .iter()
        .zip(&on[1].1.px)
        .filter(|(a, b)| a.abs_diff(**b) > 8)
        .count();
    assert!(
        differ > off[1].1.px.len() / 20,
        "easing in: blurred ({differ} values differ)"
    );
}
