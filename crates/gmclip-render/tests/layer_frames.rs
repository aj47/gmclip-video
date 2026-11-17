//! Decision 0046 ("screen look", "camera bubble"): a layer's frame — rounded corners, a
//! shadow beneath and a border on top — on the CPU path (the GPU path is checked against
//! it in `gpu_compose::gpu_frames_match_the_cpu`), and the plan field reaching the draw
//! list.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_plan::types::Rect;
use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, compose};
use gmclip_render::compositor::{
    BorderDraw, ChromaSiting, ComposeError, FrameDraw, LayerDraw, ShadowDraw, ShapeDraw, frame_draw,
};
use serde_json::json;

struct Frames(HashMap<(String, u64), Arc<Nv12Image>>);

impl FrameProvider for Frames {
    fn frame(&mut self, source: &str, frame: u64) -> Result<Arc<Nv12Image>, ComposeError> {
        self.0
            .get(&(source.to_owned(), frame))
            .cloned()
            .ok_or_else(|| ComposeError::Source(format!("{source}#{frame}")))
    }
}

/// A flat white picture (Y 235: R'G'B' 1).
fn white(w: u32, h: u32) -> Nv12Image {
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    Nv12Image {
        width: w,
        height: h,
        y: vec![235; (w * h) as usize],
        uv: [128, 128].repeat(cw * ch),
        siting: ChromaSiting::Left,
    }
}

fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

fn framed(place: Rect, opacity: f32, shape: ShapeDraw) -> LayerDraw {
    LayerDraw {
        layer_id: "screen".into(),
        source: "s".into(),
        frame: 0,
        crop: r(0.0, 0.0, 64.0, 64.0),
        place,
        opacity,
        still: false,
        matte: None,
        cover: None,
        shape: Some(shape),
    }
}

/// One framed layer over a black canvas `w × h`: the red channel per pixel.
fn draw(w: u32, h: u32, l: LayerDraw) -> Vec<[f32; 4]> {
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(white(64, 64)))]));
    let d = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0; 3],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![l],
    };
    compose(&d, w, h, &mut frames).unwrap().px
}

fn plain(radius: f32) -> ShapeDraw {
    ShapeDraw {
        radius,
        shadow: None,
        border: None,
    }
}

#[test]
fn rounded_corners_cut_the_picture_and_keep_its_edges() {
    // a 100 × 60 place, radius 0.25 of 60 = 15 px
    let px = draw(
        120,
        80,
        framed(r(10.0, 10.0, 100.0, 60.0), 1.0, plain(0.25)),
    );
    let at = |x: u32, y: u32| px[(y * 120 + x) as usize][0];
    // the corner pixel is outside the arc: background
    assert_eq!(at(10, 10), 0.0);
    assert_eq!(at(109, 69), 0.0);
    // the middle of each straight edge and the centre are the picture
    for (x, y) in [(60, 10), (10, 40), (109, 40), (60, 69), (60, 40)] {
        assert!((at(x, y) - 1.0).abs() < 1e-4, "({x},{y}) {}", at(x, y));
    }
    // across the arc's 45° point the alpha is antialiased: a pixel straddling it is partial
    // (the arc's centre (25, 25), 15 px away along the diagonal: (14.39, 14.39))
    let v = at(14, 14);
    assert!(v > 0.05 && v < 0.95, "{v}");
    // a plain (unframed) layer of the same place fills the corner
    let mut l = framed(r(10.0, 10.0, 100.0, 60.0), 1.0, plain(0.0));
    l.shape = None;
    let px = draw(120, 80, l);
    assert!((px[(10 * 120 + 10) as usize][0] - 1.0).abs() < 1e-4);
}

#[test]
fn radius_half_on_a_square_place_is_a_circle() {
    let n = 64.0;
    let px = draw(80, 80, framed(r(8.0, 8.0, n, n), 1.0, plain(0.5)));
    let area: f32 = px.iter().map(|p| p[0]).sum();
    let want = std::f32::consts::PI * (n as f32 / 2.0).powi(2);
    assert!(
        (area - want).abs() / want < 0.005,
        "covered {area} px, a circle {want}"
    );
    // the bounding box's corners are background, the tangent points the picture
    assert_eq!(px[(8 * 80 + 8) as usize][0], 0.0);
    assert!(px[(40 * 80 + 8) as usize][0] > 0.99);
    assert!(px[(8 * 80 + 40) as usize][0] > 0.99);
}

#[test]
fn a_shadow_is_drawn_beneath_moved_down_and_softened() {
    // white canvas, a black shadow at 0.6 opacity, offset 10 px, blur 10 px (canvas short
    // side 100: fractions 0.1)
    let shape = ShapeDraw {
        radius: 0.1,
        shadow: Some(ShadowDraw {
            blur: 0.1,
            offset_y: 0.1,
            opacity: 0.6,
            rgb: [0.0; 3],
        }),
        border: None,
    };
    let l = framed(r(20.0, 20.0, 60.0, 40.0), 1.0, shape);
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(white(64, 64)))]));
    let d = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [1.0; 3],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![l],
    };
    let px = compose(&d, 100, 100, &mut frames).unwrap().px;
    let at = |x: u32, y: u32| px[(y * 100 + x) as usize][0];
    // the picture (white) covers the shadow under it
    assert!((at(50, 40) - 1.0).abs() < 1e-4);
    // just below the picture (y 60–69 is the moved shape's inside): darkened by the full
    // shadow, 1 − 0.6
    assert!((at(50, 62) - 0.4).abs() < 1e-3, "{}", at(50, 62));
    // the moved shape's bottom edge (y 70) is half way: 1 − 0.6 · ½
    assert!(
        (at(50, 69) - (1.0 - 0.6 * 0.5)).abs() < 0.05,
        "{}",
        at(50, 69)
    );
    // past blur / 2 below that edge: untouched; and above the picture there is none
    // within the offset (the shape moved down by 10, softened over ±5)
    assert_eq!(at(50, 77), 1.0);
    assert_eq!(at(50, 12), 1.0);
    // monotonic falloff below the picture
    let col: Vec<f32> = (62..77).map(|y| at(50, y)).collect();
    assert!(col.windows(2).all(|w| w[1] >= w[0] - 1e-6), "{col:?}");
}

#[test]
fn a_border_runs_inside_the_rounded_edge_on_top_of_the_picture() {
    // canvas short side 100: a 0.04 border is 4 px, red, on a white picture
    let shape = ShapeDraw {
        radius: 0.2,
        shadow: None,
        border: Some(BorderDraw {
            width: 0.04,
            rgb: [1.0, 0.0, 0.0],
        }),
    };
    let px = draw(100, 100, framed(r(10.0, 10.0, 80.0, 80.0), 1.0, shape));
    let at = |x: u32, y: u32| px[(y * 100 + x) as usize];
    // the left edge's middle: 4 px of red, then the picture
    for x in 10..14 {
        let p = at(x, 50);
        assert!(p[0] > 0.999 && p[1] < 1e-4, "({x}) {p:?}");
    }
    let p = at(14, 50);
    assert!((p[1] - 1.0).abs() < 1e-4, "{p:?}");
    // the corner stays outside the shape (no border there), the arc gets the border
    assert_eq!(at(10, 10)[0], 0.0);
    let diag = at(16, 16);
    assert!(diag[0] > 0.5 && diag[1] < 0.5, "{diag:?}");
}

#[test]
fn opacity_scales_the_picture_shadow_and_border_alike() {
    let shape = ShapeDraw {
        radius: 0.25,
        shadow: None,
        border: Some(BorderDraw {
            width: 0.05,
            rgb: [1.0, 0.0, 0.0],
        }),
    };
    let px = draw(100, 100, framed(r(10.0, 10.0, 80.0, 80.0), 0.5, shape));
    let at = |x: u32, y: u32| px[(y * 100 + x) as usize];
    // centre: half white over black
    assert!((at(50, 50)[1] - 0.5).abs() < 1e-4);
    // the border at half opacity over the half-opaque picture: red 0.5 + 0.5·0.5
    let b = at(11, 50);
    assert!(
        (b[0] - 0.75).abs() < 1e-3 && (b[1] - 0.25).abs() < 1e-3,
        "{b:?}"
    );
    assert_eq!(at(10, 10)[0], 0.0);
}

#[test]
fn a_plan_frame_reaches_the_draw_list() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    let frame = json!({"radius": 0.5,
        "shadow": {"blur": 0.04, "offset_y": 0.01, "opacity": 0.4, "rgb": "#000000"},
        "border": {"width": 0.005, "rgb": "#FFFFFF"}});
    let layers = v["parts"][0]["layers"].as_array_mut().unwrap();
    let cam = layers
        .iter_mut()
        .find(|l| l["role"] == "cam")
        .expect("a cam layer");
    cam["frame"] = frame;
    cam["visible"] = json!(true);
    let p = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let d = frame_draw(&p, 0).unwrap();
    let cam = d.layers.iter().find(|l| l.layer_id == "cam").unwrap();
    let s = cam.shape.as_ref().expect("the frame is drawn");
    assert_eq!(s.radius, 0.5);
    assert_eq!(s.border.unwrap().rgb, [1.0, 1.0, 1.0]);
    assert!((s.shadow.unwrap().opacity - 0.4).abs() < 1e-6);
    assert!(
        d.layers
            .iter()
            .filter(|l| l.layer_id != "cam")
            .all(|l| l.shape.is_none())
    );
}

/// Decision 0046: a layer of another source read live (a recording's camera file beside
/// its screen) takes that source's `first_frame + (f − out[0])`, `freeze_first` its
/// `first_frame`; without `first_frame` it stays refused unless held.
#[test]
fn another_source_is_read_live_from_its_first_frame() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "face", "path": "face.mov"}));
    let cam = &mut v["parts"][1]["layers"][1];
    cam["picture_source"] = json!("face");
    cam["first_frame"] = json!(300);
    cam["visible"] = json!(true);
    cam["frame"] = json!({"radius": 0.5});
    let p = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let at = |f: u64| {
        let d = frame_draw(&p, f).unwrap();
        let l = d
            .layers
            .iter()
            .find(|l| l.layer_id == "cam")
            .unwrap()
            .clone();
        (l.source, l.frame, l.still)
    };
    assert_eq!(at(55), ("face".to_owned(), 300, false));
    assert_eq!(at(100), ("face".to_owned(), 345, false));
    // the part's own layer is unchanged
    let d = frame_draw(&p, 100).unwrap();
    let s = d.layers.iter().find(|l| l.layer_id == "screen").unwrap();
    assert_eq!((s.source.as_str(), s.frame), ("mkv1440", 79825 + 45));
    // the stream the export decodes it from
    assert!(
        gmclip_render::compositor::plan_streams(&p)
            .iter()
            .any(|(k, s)| k == "face" && s == "face")
    );
    // freeze_first: its first frame throughout
    v["parts"][1]["layers"][1]["time_map"] = json!({"mode": "freeze_first"});
    let p = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let d = frame_draw(&p, 100).unwrap();
    let l = d.layers.iter().find(|l| l.layer_id == "cam").unwrap();
    assert_eq!((l.frame, l.still), (300, true));
    // without first_frame: refused (validation), as before
    let mut w = v.clone();
    w["parts"][1]["layers"][1]
        .as_object_mut()
        .unwrap()
        .remove("first_frame");
    w["parts"][1]["layers"][1]["time_map"] = json!({"mode": "live"});
    let pw: gmclip_plan::Plan = serde_json::from_value(w).unwrap();
    assert!(frame_draw(&pw, 100).is_err());
}

/// Decision 0047: that other source at its own rate (a 25 fps camera in a 30 fps cut,
/// `layers[].fps`) is read by time: nominally `⌊(first_frame + k) · 25 / 30⌋`, and by
/// the source's own timestamps when the compose has them (a dropped frame shifts it).
#[test]
fn another_source_at_another_rate_is_read_by_time() {
    struct Clocks(gmclip_plan::retime::PtsClock);
    impl gmclip_render::compositor::SourceClocks for Clocks {
        fn clock(&self, source: &str) -> Option<&gmclip_plan::retime::PtsClock> {
            (source == "face").then_some(&self.0)
        }
    }
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let text = std::fs::read_to_string(format!(
        "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
    ))
    .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"].as_array_mut().unwrap().push(
        json!({"id": "face", "path": "face.mov", "expect": {"video": {"fps": {"num": 25, "den": 1}}}}),
    );
    let cam = &mut v["parts"][1]["layers"][1];
    cam["picture_source"] = json!("face");
    cam["first_frame"] = json!(300);
    cam["fps"] = json!({"num": 25, "den": 1});
    cam["visible"] = json!(true);
    let p = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let cam_frame = |f: u64, clocks: &dyn gmclip_render::compositor::SourceClocks| {
        let d = gmclip_render::compositor::frame_draw_with(&p, f, clocks).unwrap();
        d.layers.iter().find(|l| l.layer_id == "cam").unwrap().frame
    };
    let nominal = gmclip_render::compositor::NominalClocks;
    // the part starts at output frame 55: 300 / 30 s is face frame 250
    assert_eq!(cam_frame(55, &nominal), 250);
    assert_eq!(cam_frame(100, &nominal), 287);
    // 40 ms apart, one frame dropped before index 280: 11.5 s is index 286
    let pts = (0..400i64).map(|i| if i < 280 { i * 40 } else { (i + 1) * 40 });
    let clocks = Clocks(gmclip_plan::retime::PtsClock::new(1000, pts));
    assert_eq!(cam_frame(55, &clocks), 250);
    assert_eq!(cam_frame(100, &clocks), 286);
    // the screen is untouched
    let d = frame_draw(&p, 100).unwrap();
    let s = d.layers.iter().find(|l| l.layer_id == "screen").unwrap();
    assert_eq!((s.source.as_str(), s.frame), ("mkv1440", 79825 + 45));
}

#[test]
fn camera_clock_is_independent_of_screen_rate_conversion() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{root}/schemas/examples/h79-parts-1-3.renderplan.json"
        ))
        .unwrap(),
    )
    .unwrap();
    v["schema"] = json!("gmclip.renderplan/1");
    v["sources"][0]["expect"]["video"]["fps"] = json!({"num": 42, "den": 1});
    for part in v["parts"].as_array_mut().unwrap() {
        part["picture"]["fps"] = json!({"num": 42, "den": 1});
    }
    v["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "face", "path": "face.mov",
        "expect": {"video": {"fps": {"num": 25, "den": 1}}}}));
    let cam = &mut v["parts"][1]["layers"][1];
    cam["picture_source"] = json!("face");
    cam["first_frame"] = json!(300);
    cam["fps"] = json!({"num": 25, "den": 1});
    cam["visible"] = json!(true);
    let p = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let d = frame_draw(&p, 100).unwrap();
    assert_eq!(
        d.layers.iter().find(|l| l.layer_id == "cam").unwrap().frame,
        287
    );
    assert_eq!(
        d.layers
            .iter()
            .find(|l| l.layer_id == "screen")
            .unwrap()
            .frame,
        (79825 + 45) * 42 / 30
    );
}
