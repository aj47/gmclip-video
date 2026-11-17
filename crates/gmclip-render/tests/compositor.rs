//! H-05: compositor v0. CPU-path tests run everywhere (hosted CI); the comparison with
//! ffmpeg composites of the RT graph (crop + swscale Lanczos + overlay, no `ass`) and
//! the GPU path are `lab`.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_plan::types::Rect;
use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, compose};
use gmclip_render::compositor::{ChromaSiting, ComposeError, FrameDraw, LayerDraw, frame_draw};

struct Frames(HashMap<(String, u64), Arc<Nv12Image>>);

impl FrameProvider for Frames {
    fn frame(&mut self, source: &str, frame: u64) -> Result<Arc<Nv12Image>, ComposeError> {
        self.0
            .get(&(source.to_owned(), frame))
            .cloned()
            .ok_or_else(|| ComposeError::Source(format!("{source}#{frame}")))
    }
}

fn flat(w: u32, h: u32, y: u8, cb: u8, cr: u8) -> Nv12Image {
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    Nv12Image {
        width: w,
        height: h,
        y: vec![y; (w * h) as usize],
        uv: [cb, cr].repeat(cw * ch),
        siting: ChromaSiting::Left,
    }
}

fn noise(w: u32, h: u32, seed: u32) -> Nv12Image {
    let mut s = seed | 1;
    let mut n = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (16 + s % 220) as u8
    };
    let mut img = flat(w, h, 0, 0, 0);
    img.y.iter_mut().for_each(|v| *v = n());
    img.uv.iter_mut().for_each(|v| *v = n());
    img
}

fn layer(id: &str, crop: Rect, place: Rect, opacity: f32) -> LayerDraw {
    LayerDraw {
        layer_id: id.into(),
        source: "s".into(),
        frame: 0,
        crop,
        place,
        opacity,
        still: false,
        matte: None,
        cover: None,
        shape: None,
    }
}

fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

#[test]
fn one_to_one_integer_crop_is_exact() {
    let src = noise(64, 48, 3);
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(src.clone()))]));
    let draw = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0, 0.0, 0.0],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![layer(
            "a",
            r(10.0, 6.0, 32.0, 20.0),
            r(5.0, 7.0, 32.0, 20.0),
            1.0,
        )],
    };
    let out = compose(&draw, 50, 40, &mut frames).unwrap();
    for y in 0..20 {
        for x in 0..32 {
            let got = out.px[((y + 7) * 50 + x + 5) as usize];
            let want = src.rgb_at(x + 10, y + 6);
            for k in 0..3 {
                assert!(
                    (got[k] - want[k]).abs() < 1e-5,
                    "({x},{y}) {got:?} {want:?}"
                );
            }
            assert!((got[3] - 1.0).abs() < 1e-6);
        }
    }
    // Outside the place rect: background.
    assert_eq!(out.px[0], [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn flat_source_any_scale_stays_flat() {
    let src = flat(300, 200, 180, 90, 160);
    let want = src.rgb_at(0, 0);
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(src))]));
    for place in [r(0.0, 0.0, 120.0, 80.0), r(3.0, 2.0, 600.0, 400.0)] {
        let draw = FrameDraw {
            out_frame: 0,
            part_id: "p".into(),
            background: [0.0; 3],
            backdrop: None,
            tiles_at: vec![],
            layers: vec![layer("a", r(0.0, 0.0, 300.0, 200.0), place, 1.0)],
        };
        let out = compose(&draw, 610, 410, &mut frames).unwrap();
        let p = out.px[(40 * 610 + 50) as usize];
        for k in 0..3 {
            assert!((p[k] - want[k]).abs() < 1e-4, "{p:?} {want:?}");
        }
    }
}

#[test]
fn fractional_place_edges_and_opacity_blend_premultiplied() {
    let src = flat(16, 16, 235, 128, 128); // white
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(src))]));
    let draw = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0; 3],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![layer(
            "a",
            r(0.0, 0.0, 16.0, 16.0),
            r(2.5, 2.0, 16.0, 16.0),
            0.5,
        )],
    };
    let out = compose(&draw, 24, 24, &mut frames).unwrap();
    let px = |x: usize, y: usize| out.px[y * 24 + x];
    // Pixel 2 is half covered: alpha 0.25 (coverage 0.5 × opacity 0.5).
    assert!((px(2, 5)[0] - 0.25).abs() < 1e-4, "{:?}", px(2, 5));
    assert!((px(5, 5)[0] - 0.5).abs() < 1e-4);
    assert!(
        (px(18, 5)[0] - 0.25).abs() < 1e-4,
        "right edge half covered"
    );
    assert_eq!(px(1, 5)[0], 0.0);
}

#[test]
fn h79_example_plan_draw_lists() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/examples/h79-parts-1-3.renderplan.json"
    );
    let plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    // Part 1: cam hidden (dummy crop) → only the screen layer.
    let d = frame_draw(&plan, 10).unwrap();
    assert_eq!(d.part_id, "01-payoff-banana");
    assert_eq!(
        d.layers
            .iter()
            .map(|l| l.layer_id.as_str())
            .collect::<Vec<_>>(),
        ["screen"]
    );
    assert_eq!(d.layers[0].frame, 79612 + 10);
    assert_eq!(d.background, [12.0 / 255.0, 17.0 / 255.0, 24.0 / 255.0]);
    // Part 3: screen, cam, detail in z order; live frames follow the output clock.
    let d = frame_draw(&plan, 176 + 5).unwrap();
    assert_eq!(
        d.layers
            .iter()
            .map(|l| l.layer_id.as_str())
            .collect::<Vec<_>>(),
        ["screen", "cam", "detail-0"]
    );
    assert!(d.layers.iter().all(|l| l.frame == 75956 + 5));
    assert!(matches!(
        frame_draw(&plan, 397),
        Err(ComposeError::NoPart(397))
    ));
}

#[test]
fn time_map_hold_and_freeze_first() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/examples/h79-parts-1-3.renderplan.json"
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    use gmclip_plan::types::TimeMap;
    plan.parts[1].layers[0].time_map = Some(TimeMap::Hold {
        frame: 12345,
        provenance: None,
    });
    plan.parts[1].layers[1].time_map = Some(TimeMap::FreezeFirst);
    let d = frame_draw(&plan, 100).unwrap();
    assert_eq!(d.layers[0].frame, 12345);
    assert_eq!(d.layers[1].frame, 79825);
}

#[test]
fn fractional_crop_samples_between_pixels() {
    // A horizontal luma ramp (Y = 16 + 2x, flat chroma): at scale 1 a crop starting
    // at x = 10.5 samples halfway between pixels, i.e. the ramp at 10.5 + X.
    let (w, h) = (64u32, 8u32);
    let mut src = flat(w, h, 0, 128, 128);
    for y in 0..h {
        for x in 0..w {
            src.y[(y * w + x) as usize] = (16 + 2 * x) as u8;
        }
    }
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(src.clone()))]));
    let draw = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0; 3],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![layer(
            "a",
            r(10.5, 0.0, 30.0, 8.0),
            r(0.0, 0.0, 30.0, 8.0),
            1.0,
        )],
    };
    let out = compose(&draw, 40, 8, &mut frames).unwrap();
    for x in 4..26 {
        let want = (16.0 + 2.0 * (10.5 + x as f32) - 16.0) / 219.0;
        let got = out.px[(4 * 40 + x) as usize][1];
        assert!((got - want).abs() < 2e-3, "x {x}: {got} vs {want}");
    }
}

#[test]
fn other_source_needs_hold() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/examples/h79-parts-1-3.renderplan.json"
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    let other = plan.sources[0].id.clone();
    // Make a second source id by reusing the first under a layer: same id is allowed;
    // a different id with a live time map is Unsupported.
    plan.parts[1].layers[0].picture_source = Some(gmclip_plan::scalars::Id::new("other").unwrap());
    assert!(matches!(
        frame_draw(&plan, 100),
        Err(ComposeError::Unsupported(_))
    ));
    plan.parts[1].layers[0].time_map = Some(gmclip_plan::types::TimeMap::Hold {
        frame: 7,
        provenance: None,
    });
    let d = frame_draw(&plan, 100).unwrap();
    assert_eq!(
        (d.layers[0].source.as_str(), d.layers[0].frame),
        ("other", 7)
    );
    let _ = other;
}

#[cfg(target_os = "macos")]
#[path = "compositor_lab/mod.rs"]
mod lab;

/// `/1` blurred-source fill (decision 0014): a 16:9 picture under a 9:16 canvas covers it
/// (no background shows), is smooth (a low-pass of the picture), is dimmed, and the layer
/// still draws sharply on top. The 8-bit canvas is pinned (a regression golden).
#[test]
fn blurred_source_fill_covers_smooths_and_dims() {
    use gmclip_render::compositor::BackdropDraw;
    let src = noise(160, 90, 11);
    let mut frames = Frames(HashMap::from([(("s".to_owned(), 0), Arc::new(src))]));
    let backdrop = |dim| BackdropDraw {
        source: "s".into(),
        frame: 0,
        blur: 6,
        dim,
    };
    let mut draw = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [1.0, 0.0, 1.0],
        backdrop: Some(backdrop(0.0)),
        tiles_at: vec![],
        layers: vec![],
    };
    let (w, h) = (54, 96);
    let lit = compose(&draw, w, h, &mut frames).unwrap();
    // covered: never the magenta background
    assert!(
        lit.px
            .iter()
            .all(|p| !(p[0] > 0.9 && p[1] < 0.1 && p[2] > 0.9) && p[3] == 1.0)
    );
    // smooth: neighbouring pixels differ far less than the noise source's ±0.5
    let luma = |p: &[f32; 4]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let max_step = lit
        .px
        .chunks(w as usize)
        .flat_map(|row| row.windows(2).map(|q| (luma(&q[0]) - luma(&q[1])).abs()))
        .fold(0.0f32, f32::max);
    assert!(max_step < 0.08, "{max_step}");
    // dimmed by a quarter: every value 0.75× the undimmed one
    draw.backdrop = Some(backdrop(0.25));
    let dim = compose(&draw, w, h, &mut frames).unwrap();
    for (a, b) in lit.px.iter().zip(&dim.px) {
        assert!((a[1] * 0.75 - b[1]).abs() < 1e-5);
    }
    // a layer on top is drawn over it, unblurred
    draw.layers = vec![layer(
        "screen",
        r(0.0, 0.0, 160.0, 90.0),
        r(0.0, 30.0, 54.0, 30.375),
        1.0,
    )];
    let with_layer = compose(&draw, w, h, &mut frames).unwrap();
    assert_ne!(
        with_layer.px[(40 * w + 27) as usize],
        dim.px[(40 * w + 27) as usize]
    );
    assert_eq!(
        with_layer.px[(5 * w + 27) as usize],
        dim.px[(5 * w + 27) as usize]
    );
    // golden: the 8-bit fill
    let bytes: Vec<u8> = dim
        .px
        .iter()
        .flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect();
    let mut hsh: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        hsh ^= u64::from(*b);
        hsh = hsh.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/blurred_fill.txt");
    if std::env::var_os("GMCLIP_BLESS").is_some() {
        std::fs::write(&path, format!("{hsh:016x}\n")).unwrap();
    }
    let want = std::fs::read_to_string(&path).expect("committed golden");
    assert_eq!(
        format!("{hsh:016x}"),
        want.trim(),
        "the blurred fill changed"
    );
}

/// R.01: a roto matte multiplies the layer's alpha in source pixels. An all-keep mask is
/// the unmasked layer; an all-drop mask leaves the background; a half mask keeps the
/// kept half exactly at 1:1; a layer with a matte and no mask is refused.
#[test]
fn roto_mask_cuts_the_layer_out() {
    use gmclip_render::compositor::MatteDraw;
    use gmclip_render::matte::Mask;
    let src = noise(64, 48, 7);
    let mut frames = Frames(HashMap::from([(("s".into(), 0), Arc::new(src.clone()))]));
    let crop = r(10.0, 6.0, 32.0, 20.0);
    let place = r(5.0, 7.0, 32.0, 20.0);
    let spec = gmclip_plan::types::LayerMatte {
        kind: gmclip_plan::consts::Roto,
        model: "apple_vision".into(),
        soften: None,
        strokes: None,
    };
    let with_mask = |m: Option<Mask>| {
        let mut l = layer("a", crop, place, 1.0);
        l.matte = Some(MatteDraw {
            spec: spec.clone(),
            span: (0, 1),
            mask: m.map(|m| (Arc::new(m), r(10.0, 6.0, 32.0, 20.0))),
            draft: false,
            view: false,
        });
        FrameDraw {
            out_frame: 0,
            part_id: "p".into(),
            background: [0.0, 0.0, 0.0],
            backdrop: None,
            tiles_at: vec![],
            layers: vec![l],
        }
    };
    let plain = FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0, 0.0, 0.0],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![layer("a", crop, place, 1.0)],
    };
    let a = compose(&plain, 50, 40, &mut frames).unwrap();
    let b = compose(
        &with_mask(Some(Mask::filled(32, 20, 255))),
        50,
        40,
        &mut frames,
    )
    .unwrap();
    for (p, q) in a.px.iter().zip(&b.px) {
        for k in 0..4 {
            assert!((p[k] - q[k]).abs() < 1e-5, "{p:?} {q:?}");
        }
    }
    let z = compose(
        &with_mask(Some(Mask::filled(32, 20, 0))),
        50,
        40,
        &mut frames,
    )
    .unwrap();
    assert!(z.px.iter().all(|p| p[..3] == [0.0, 0.0, 0.0]));
    let mut half = Mask::filled(32, 20, 0);
    for y in 0..20 {
        for x in 0..16 {
            half.data[y * 32 + x] = 255;
        }
    }
    let h = compose(&with_mask(Some(half)), 50, 40, &mut frames).unwrap();
    for y in 0..20 {
        let kept = h.px[((y + 7) * 50 + 5) as usize];
        let want = src.rgb_at(10, y + 6);
        assert!((kept[0] - want[0]).abs() < 1e-5, "{kept:?} {want:?}");
        let gone = h.px[((y + 7) * 50 + 5 + 31) as usize];
        assert!(gone[..3].iter().all(|v| v.abs() < 1e-5), "{gone:?}");
    }
    assert!(compose(&with_mask(None), 50, 40, &mut frames).is_err());
}
