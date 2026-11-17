//! Decision 0028: the backdrop (a video or a still under every part, on the output clock)
//! and clip events (videos picture-in-picture over a span). Both enter the one draw list
//! the preview and the export share, so the CPU path here and the GPU path (R-COMP-1,
//! `rcomp1_lab::preview_equals_export_with_backdrop_and_clips`) draw them alike.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_plan::Plan;
use gmclip_render::compositor::cpu::{
    FrameProvider, Nv12Image, compose, compose_under, compose_with,
};
use gmclip_render::compositor::text_layer::TextLayer;
use gmclip_render::compositor::{
    BACKDROP_LAYER, ChromaSiting, ComposeError, frame_draw, plan_streams,
};
use serde_json::{Value, json};

/// The RP example (h79 parts 1–3: 397 frames, joins at 55 and 176) on `/1`, with a
/// looping backdrop video and two clip events.
fn plan_json() -> Value {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "rain", "path": "rain.mp4"}));
    v["background"] = json!({"kind": "backdrop", "rgb": "#000000",
        "crop": {"x": 0, "y": 0, "w": 540, "h": 960}, "place": {"x": 0, "y": 0, "w": 1080, "h": 1920},
        "video": {"source": "rain", "first_frame": 10, "frames": 100, "loop": true}});
    v["clip_events"] = json!([
        {"id": "top", "out": [50, 60], "layer": 5,
         "picture": {"source": "rain", "first_frame": 500},
         "crop": {"x": 0, "y": 0, "w": 160, "h": 90}, "place": {"x": 100, "y": 100, "w": 320, "h": 180}},
        {"id": "under", "out": [40, 70], "layer": 1, "opacity": 0.5,
         "picture": {"source": "mkv1440", "first_frame": 1000, "frames": 5},
         "crop": {"x": 0, "y": 0, "w": 2560, "h": 1440}, "place": {"x": 0, "y": 0, "w": 320, "h": 180}}
    ]);
    v
}

fn plan(v: &Value) -> Plan {
    gmclip_plan::parse(&serde_json::to_vec(v).unwrap()).unwrap()
}

#[test]
fn the_backdrop_runs_on_the_output_clock_across_joins() {
    let p = plan(&plan_json());
    // the join at 55: the part changes, the backdrop's frame keeps counting
    let (a, b) = (frame_draw(&p, 54).unwrap(), frame_draw(&p, 55).unwrap());
    assert_ne!(a.part_id, b.part_id);
    let (la, lb) = (&a.layers[0], &b.layers[0]);
    assert_eq!(
        (la.layer_id.as_str(), lb.layer_id.as_str()),
        (BACKDROP_LAYER, BACKDROP_LAYER)
    );
    assert_eq!((la.frame, lb.frame), (10 + 54, 10 + 55));
    assert!(la.is_extra() && !la.still && la.matte.is_none());
    // it loops every 100 output frames
    assert_eq!(frame_draw(&p, 99).unwrap().layers[0].frame, 109);
    assert_eq!(frame_draw(&p, 100).unwrap().layers[0].frame, 10);
    assert_eq!(frame_draw(&p, 396).unwrap().layers[0].frame, 10 + 96);
    // the part's own layers follow, unchanged
    assert!(
        a.layers[1..]
            .iter()
            .any(|l| l.layer_id == "screen" && !l.is_extra())
    );
}

#[test]
fn clip_events_draw_above_the_parts_layers_in_their_span() {
    let p = plan(&plan_json());
    let ids = |f: u64| -> Vec<String> {
        frame_draw(&p, f)
            .unwrap()
            .layers
            .iter()
            .map(|l| l.layer_id.clone())
            .collect()
    };
    assert!(!ids(39).iter().any(|i| i.starts_with("@clip:")));
    // at 52 both: `under` (layer 1) below `top` (layer 5), both above every part layer
    let at = ids(52);
    let n = at.len();
    assert_eq!(&at[n - 2..], ["@clip:under", "@clip:top"]);
    assert!(at[1..n - 2].iter().all(|i| !i.starts_with('@')));
    let d = frame_draw(&p, 52).unwrap();
    let under = &d.layers[n - 2];
    // `under` holds its last of 5 frames; `top` plays straight from its first frame
    assert_eq!(under.frame, 1004);
    assert_eq!(under.opacity, 0.5);
    assert_eq!(d.layers[n - 1].frame, 502);
    assert!(!ids(70).iter().any(|i| i == "@clip:under"));
    // each reads on its own stream: a part and a clip of one recording never share one
    assert_eq!(under.stream(), "@clip:mkv1440");
    assert_eq!(d.layers[0].stream(), BACKDROP_LAYER);
    let screen = d.layers.iter().find(|l| l.layer_id == "screen").unwrap();
    assert_eq!(screen.stream(), "mkv1440");
    assert_eq!(
        plan_streams(&p),
        [
            ("mkv1440".to_owned(), "mkv1440".to_owned()),
            (BACKDROP_LAYER.to_owned(), "rain".to_owned()),
            ("@clip:rain".to_owned(), "rain".to_owned()),
            ("@clip:mkv1440".to_owned(), "mkv1440".to_owned()),
        ]
    );
}

#[test]
fn a_plan_without_them_draws_as_before() {
    let mut v = plan_json();
    v["schema"] = json!("gmclip.renderplan/0");
    v["background"] = json!({"kind": "colour", "rgb": "#0c1118"});
    v.as_object_mut().unwrap().remove("clip_events");
    v["sources"].as_array_mut().unwrap().pop();
    let p = plan(&v);
    for f in [0, 54, 55, 396] {
        let d = frame_draw(&p, f).unwrap();
        assert!(d.layers.iter().all(|l| !l.is_extra()), "frame {f}");
    }
    assert_eq!(
        plan_streams(&p),
        [("mkv1440".to_owned(), "mkv1440".to_owned())]
    );
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

/// The backdrop video fills the canvas under the part's layers; a clip draws over them.
#[test]
fn the_cpu_compose_draws_them_in_order() {
    let mut v = plan_json();
    // a small canvas-sized plan: one part, one screen layer in the middle
    v["output"]["canvas"] = json!({"w": 64, "h": 64});
    v["background"]["crop"] = json!({"x": 0, "y": 0, "w": 32, "h": 32});
    v["background"]["place"] = json!({"x": 0, "y": 0, "w": 64, "h": 64});
    v.as_object_mut().unwrap().remove("text_events");
    v.as_object_mut().unwrap().remove("text_styles");
    v.as_object_mut().unwrap().remove("fonts");
    for p in v["parts"].as_array_mut().unwrap() {
        p["layers"] = json!([{"id": "screen", "role": "screen", "z": 0,
            "crop": {"x": 0, "y": 0, "w": 16, "h": 16}, "place": {"x": 16, "y": 16, "w": 32, "h": 32}}]);
    }
    v["clip_events"] = json!([{"id": "pip", "out": [50, 60],
        "picture": {"source": "rain", "first_frame": 0},
        "crop": {"x": 0, "y": 0, "w": 32, "h": 32}, "place": {"x": 40, "y": 40, "w": 16, "h": 16}}]);
    let p = plan(&v);
    let mut frames = Flat(HashMap::from([
        ("rain".to_owned(), Arc::new(flat(32, 32, 200))),
        ("mkv1440".to_owned(), Arc::new(flat(16, 16, 60))),
    ]));
    let grey = |img: &gmclip_render::compositor::cpu::RgbaImage, x: usize, y: usize| {
        (img.px[y * 64 + x][0] * 255.0).round() as i32
    };
    let out = compose(&frame_draw(&p, 52).unwrap(), 64, 64, &mut frames).unwrap();
    let (bd, screen) = (((200.0 - 16.0) / 219.0 * 255.0f64).round() as i32, 51);
    assert!(
        (grey(&out, 2, 2) - bd).abs() <= 1,
        "backdrop {}",
        grey(&out, 2, 2)
    );
    assert!((grey(&out, 20, 20) - screen).abs() <= 1, "screen over it");
    assert!(
        (grey(&out, 47, 47) - bd).abs() <= 1,
        "the clip over the screen"
    );
    let out = compose(&frame_draw(&p, 60).unwrap(), 64, 64, &mut frames).unwrap();
    assert!((grey(&out, 47, 47) - screen).abs() <= 1, "the clip is over");
}

/// A still backdrop: the image's crop resampled to its place, under every layer; built
/// once and kept while the backdrop is the same.
#[test]
fn a_still_backdrop_is_a_tile_under_the_layers() {
    let dir = std::env::temp_dir().join(format!("gmclip-still-bd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 40×20: left half red, right half blue
    let (w, h) = (40u32, 20u32);
    let mut px = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            px.extend_from_slice(if x < 20 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 255, 255]
            });
        }
    }
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px,
    })
    .unwrap();
    let path = dir.join("still.png");
    std::fs::write(&path, &png).unwrap();
    let mut v = plan_json();
    v["output"]["canvas"] = json!({"w": 64, "h": 64});
    v.as_object_mut().unwrap().remove("clip_events");
    v["sources"].as_array_mut().unwrap().pop();
    v.as_object_mut().unwrap().remove("text_events");
    v.as_object_mut().unwrap().remove("text_styles");
    v.as_object_mut().unwrap().remove("fonts");
    v["images"] = json!([{"id": "still", "path": path.display().to_string(),
        "sha256": gmclip_render::sha256::hex(&png), "w": w, "h": h}]);
    // the right (blue) half's 20×20 square, cover-fitted to the canvas
    v["background"] = json!({"kind": "backdrop", "rgb": "#000000",
        "crop": {"x": 20, "y": 0, "w": 20, "h": 20}, "place": {"x": 0, "y": 0, "w": 64, "h": 64},
        "image": "still"});
    for p in v["parts"].as_array_mut().unwrap() {
        p["layers"] = json!([{"id": "screen", "role": "screen", "z": 0,
            "crop": {"x": 0, "y": 0, "w": 16, "h": 16}, "place": {"x": 16, "y": 16, "w": 32, "h": 32}}]);
    }
    let p = plan(&v);
    let mut text = TextLayer::new(&p).unwrap();
    let t = text.under(&p).unwrap().unwrap();
    assert_eq!((t.x, t.y, t.width, t.height), (0, 0, 64, 64));
    assert!(Arc::ptr_eq(&t, &text.under(&p).unwrap().unwrap()), "kept");
    let d = frame_draw(&p, 10).unwrap();
    assert!(
        d.layers.iter().all(|l| !l.is_extra()),
        "a still is not a layer"
    );
    let mut frames = Flat(HashMap::from([(
        "mkv1440".to_owned(),
        Arc::new(flat(16, 16, 60)),
    )]));
    let out = compose_under(&d, 64, 64, &mut frames, Some(&t)).unwrap();
    let px = |x: usize, y: usize| out.px[y * 64 + x];
    assert!(
        px(2, 2)[2] > 0.99 && px(2, 2)[0] < 0.01,
        "blue backdrop {:?}",
        px(2, 2)
    );
    assert!(
        (px(30, 30)[0] - px(30, 30)[2]).abs() < 0.01,
        "the grey screen over it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Decision 0029: a plate is a gradient tile under every layer — its stops spread evenly
/// from the top row to the bottom, mixed in 8-bit codes; the clear colour is the first stop.
#[test]
fn a_plate_is_a_vertical_gradient_under_the_layers() {
    use gmclip_plan::scalars::Rgb;
    use gmclip_render::compositor::text_layer::plate_tile;
    let stops = [
        Rgb::new("#000000").unwrap(),
        Rgb::new("#FF8000").unwrap(),
        Rgb::new("#0000FF").unwrap(),
    ];
    let t = plate_tile(&stops, 4, 101).unwrap();
    assert_eq!((t.width, t.height), (4, 101));
    let px = |y: usize| &t.rgba[y * 16..y * 16 + 4];
    // row 50's centre is the middle stop exactly; the ends are half a row in
    assert_eq!(px(50), [255, 128, 0, 255]);
    assert_eq!(px(0), [3, 1, 0, 255]);
    assert_eq!(px(100), [3, 1, 252, 255]);
    // every column of a row is the same, and alpha is opaque
    assert!(
        t.rgba
            .chunks_exact(16)
            .all(|r| r[..4] == r[4..8] && r[3] == 255)
    );
    assert!(plate_tile(&stops[..1], 4, 4).is_err());

    let mut v = plan_json();
    v["schema"] = json!("gmclip.renderplan/0");
    v["output"]["canvas"] = json!({"w": 64, "h": 64});
    v.as_object_mut().unwrap().remove("clip_events");
    v["sources"].as_array_mut().unwrap().pop();
    v.as_object_mut().unwrap().remove("text_events");
    v.as_object_mut().unwrap().remove("text_styles");
    v.as_object_mut().unwrap().remove("fonts");
    v["background"] = json!({"kind": "plate", "stops": ["#102030", "#405060"]});
    for p in v["parts"].as_array_mut().unwrap() {
        p["layers"] = json!([{"id": "screen", "role": "screen", "z": 0,
            "crop": {"x": 0, "y": 0, "w": 16, "h": 16}, "place": {"x": 16, "y": 16, "w": 32, "h": 32}}]);
    }
    let p = plan(&v);
    let d = frame_draw(&p, 3).unwrap();
    assert_eq!(
        d.background.map(|c| (c * 255.0).round() as u8),
        [0x10, 0x20, 0x30]
    );
    let mut text = TextLayer::new(&p).unwrap();
    let t = text.under(&p).unwrap().unwrap();
    assert_eq!((t.width, t.height), (64, 64));
    let mut frames = Flat(HashMap::from([(
        "mkv1440".to_owned(),
        Arc::new(flat(16, 16, 60)),
    )]));
    let out = compose_under(&d, 64, 64, &mut frames, Some(&t)).unwrap();
    let code = |x: usize, y: usize| (out.px[y * 64 + x][2] * 255.0).round() as i32;
    assert!(code(2, 0) < code(2, 63), "darker at the top");
    assert_eq!((code(2, 0), code(2, 63)), (0x30, 0x60));
}

/// Decision 0031: a tracked overlay's tile moves by its point's move since the event's
/// start, in whole pixels; an untracked one stays.
#[test]
fn a_tracked_callout_moves_with_its_point() {
    let mut v = plan_json();
    v["schema"] = json!("gmclip.renderplan/1");
    v["background"] = json!({"kind": "colour", "rgb": "#000000"});
    v.as_object_mut().unwrap().remove("clip_events");
    v["sources"].as_array_mut().unwrap().pop();
    v.as_object_mut().unwrap().remove("text_events");
    v.as_object_mut().unwrap().remove("text_styles");
    v.as_object_mut().unwrap().remove("fonts");
    v["shape_events"] = json!([
        {"id": "box", "out": [10, 50], "shape": {"kind": "box", "rect": {"x": 100, "y": 200, "w": 80, "h": 40}, "width": 6, "stroke": "#FFD400FF"}},
        {"id": "still", "out": [10, 50], "shape": {"kind": "box", "rect": {"x": 500, "y": 200, "w": 80, "h": 40}, "width": 6, "stroke": "#FFD400FF"}}
    ]);
    v["tracks"] = json!([{"kind": "shape", "event": "box",
        "keys": [{"frame": 10, "x": 140, "y": 220}, {"frame": 30, "x": 240, "y": 200}]}]);
    let p = plan(&v);
    let mut text = TextLayer::new(&p).unwrap();
    let at = |text: &mut TextLayer, f: u64| -> Vec<(i32, i32)> {
        text.active(&p, f)
            .unwrap()
            .iter()
            .map(|t| (t.x, t.y))
            .collect()
    };
    let start = at(&mut text, 10);
    let mid = at(&mut text, 20);
    let end = at(&mut text, 45);
    assert_eq!((mid[0].0 - start[0].0, mid[0].1 - start[0].1), (50, -10));
    assert_eq!((end[0].0 - start[0].0, end[0].1 - start[0].1), (100, -20));
    assert_eq!(start[1], end[1], "the untracked callout stays");
}

/// Decision 0042: a clip event with `z` is drawn among the part's layers, just under the
/// first whose `z` is at least its own; one without stays above them all.
#[test]
fn a_clip_with_z_is_drawn_among_the_layers() {
    let mut v = plan_json();
    v["clip_events"][1]["z"] = json!(1);
    let p = plan(&v);
    let ids = |p: &Plan, f: u64| -> Vec<String> {
        frame_draw(p, f)
            .unwrap()
            .layers
            .iter()
            .map(|l| l.layer_id.clone())
            .collect()
    };
    // part 2: screen z 0, cam z 1
    assert_eq!(
        ids(&p, 57),
        [BACKDROP_LAYER, "screen", "@clip:under", "cam", "@clip:top"]
    );
    v["clip_events"][1]["z"] = json!(-5);
    let p = plan(&v);
    assert_eq!(
        ids(&p, 57),
        [BACKDROP_LAYER, "@clip:under", "screen", "cam", "@clip:top"]
    );
    v["clip_events"][1]["z"] = json!(9);
    let p = plan(&v);
    assert_eq!(
        ids(&p, 57),
        [BACKDROP_LAYER, "screen", "cam", "@clip:under", "@clip:top"]
    );
    assert!(frame_draw(&p, 57).unwrap().tiles_at.is_empty());
    // a matte on a clip reaches its draw, spanning the frames it shows
    v["clip_events"][0]["matte"] = json!({"kind": "roto", "model": "apple_vision"});
    let p = plan(&v);
    let d = frame_draw(&p, 57).unwrap();
    let top = d.layers.iter().find(|l| l.layer_id == "@clip:top").unwrap();
    assert_eq!(top.matte.as_ref().unwrap().span, (500, 10));
}

/// Decision 0042: an image event with `z` is a tile drawn among the layers (CPU here; the
/// GPU draws the same list), not above them; one without stays on top.
#[test]
fn an_image_with_z_is_drawn_under_a_layer() {
    let dir = std::env::temp_dir().join(format!("gmclip-among-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (8u32, 8u32);
    let png = gmclip_media::image::encode_png(&gmclip_media::image::Rgba8 {
        width: w,
        height: h,
        px: [255u8, 0, 0, 255].repeat((w * h) as usize),
    })
    .unwrap();
    let path = dir.join("red.png");
    std::fs::write(&path, &png).unwrap();
    let mut v = plan_json();
    v["output"]["canvas"] = json!({"w": 64, "h": 64});
    v["background"] = json!({"kind": "colour", "rgb": "#000000"});
    v.as_object_mut().unwrap().remove("clip_events");
    v["sources"].as_array_mut().unwrap().pop();
    v.as_object_mut().unwrap().remove("text_events");
    v.as_object_mut().unwrap().remove("text_styles");
    v.as_object_mut().unwrap().remove("fonts");
    for p in v["parts"].as_array_mut().unwrap() {
        p["layers"] = json!([{"id": "screen", "role": "screen", "z": 0,
            "crop": {"x": 0, "y": 0, "w": 16, "h": 16}, "place": {"x": 16, "y": 16, "w": 32, "h": 32}}]);
    }
    v["images"] = json!([{"id": "red", "path": path.display().to_string(),
        "sha256": gmclip_render::sha256::hex(&png), "w": w, "h": h}]);
    // the whole canvas red, over the screen (no z) or under it (z 0)
    v["image_events"] = json!([{"id": "bg", "out": [0, 397], "image": "red",
        "place": {"x": 0, "y": 0, "w": 64, "h": 64}}]);
    let mut frames = Flat(HashMap::from([(
        "mkv1440".to_owned(),
        Arc::new(flat(16, 16, 60)),
    )]));
    let red_at_centre = |v: &Value, frames: &mut Flat| {
        let p = plan(v);
        let d = frame_draw(&p, 10).unwrap();
        let mut text = TextLayer::new(&p).unwrap();
        let (top, among) = text.active_split(&p, 10, &|_| None, &d.tiles_at).unwrap();
        let mut out = compose_with(&d, 64, 64, frames, None, &among).unwrap();
        gmclip_render::compositor::text_layer::composite_cpu(&mut out, &top);
        (
            out.px[32 * 64 + 32][0],
            out.px[2 * 64 + 2][0],
            d.tiles_at,
            top.len(),
        )
    };
    let (centre, edge, at, top) = red_at_centre(&v, &mut frames);
    assert!(centre > 0.99 && edge > 0.99, "on top: red everywhere");
    assert!(at.is_empty() && top == 1);
    v["image_events"][0]["z"] = json!(0);
    let (centre, edge, at, top) = red_at_centre(&v, &mut frames);
    assert_eq!(at, [(0, "bg".to_owned())], "before the screen");
    assert_eq!(top, 0, "not drawn on top as well");
    assert!(edge > 0.99, "red around the screen");
    assert!(centre < 0.3, "the screen over it: {centre}");
    let _ = std::fs::remove_dir_all(&dir);
}
