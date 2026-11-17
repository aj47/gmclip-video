//! Cover layers (decision 0021, SPEC §5.7): a layer's `cover` draws its crop into its
//! place blurred, pixelated or as a solid fill, at its z among the other layers. CPU
//! tests run everywhere; the GPU path against the CPU is lab (Metal).

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_plan::types::Rect;
use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, RgbaImage, compose};
use gmclip_render::compositor::{
    ChromaSiting, ComposeError, CoverDraw, CoverKind, FrameDraw, LayerDraw, frame_draw,
};

struct Frames(HashMap<(String, u64), Arc<Nv12Image>>);

impl FrameProvider for Frames {
    fn frame(&mut self, source: &str, frame: u64) -> Result<Arc<Nv12Image>, ComposeError> {
        self.0
            .get(&(source.to_owned(), frame))
            .cloned()
            .ok_or_else(|| ComposeError::Source(format!("{source}#{frame}")))
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
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    Nv12Image {
        width: w,
        height: h,
        y: (0..w * h).map(|_| n()).collect(),
        uv: (0..cw * ch * 2).map(|_| n()).collect(),
        siting: ChromaSiting::Left,
    }
}

fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

fn layer(id: &str, crop: Rect, place: Rect, cover: Option<CoverDraw>) -> LayerDraw {
    LayerDraw {
        layer_id: id.into(),
        source: "s".into(),
        frame: 0,
        crop,
        place,
        opacity: 1.0,
        still: false,
        matte: None,
        cover,
        shape: None,
    }
}

const SRC: (u32, u32) = (320, 180);
const OUT: (u32, u32) = (180, 320);

/// The screen (the whole source across the canvas) and a cover over its top-right
/// quarter, placed where that quarter lands.
fn draw(cover: Option<CoverDraw>, opacity: f32) -> FrameDraw {
    let screen = layer(
        "screen",
        r(0.0, 0.0, 320.0, 180.0),
        r(0.0, 100.0, 180.0, 101.25),
        None,
    );
    let mut layers = vec![screen];
    if let Some(c) = cover {
        let mut l = layer(
            "cover",
            r(160.0, 0.0, 160.0, 90.0),
            r(90.0, 100.0, 90.0, 50.625),
            Some(c),
        );
        l.opacity = opacity;
        layers.push(l);
    }
    FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.0, 0.0, 0.0],
        backdrop: None,
        tiles_at: vec![],
        layers,
    }
}

fn frames() -> Frames {
    Frames(HashMap::from([(
        ("s".to_owned(), 0),
        Arc::new(noise(SRC.0, SRC.1, 5)),
    )]))
}

fn render(d: &FrameDraw) -> RgbaImage {
    compose(d, OUT.0, OUT.1, &mut frames()).unwrap()
}

fn px(img: &RgbaImage, x: u32, y: u32) -> [f32; 4] {
    img.px[(y * img.width + x) as usize]
}

fn luma(p: [f32; 4]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

#[test]
fn solid_cover_fills_its_place_and_nothing_else() {
    let plain = render(&draw(None, 1.0));
    let c = CoverDraw {
        kind: CoverKind::Solid([0.25, 0.5, 0.75]),
        grid: (1, 1),
    };
    let cov = render(&draw(Some(c.clone()), 1.0));
    for y in 0..OUT.1 {
        for x in 0..OUT.0 {
            let inside = (90..180).contains(&x) && (100..150).contains(&y);
            let (a, b) = (px(&plain, x, y), px(&cov, x, y));
            if inside {
                assert_eq!(&b[..3], &[0.25, 0.5, 0.75], "({x}, {y})");
            } else if y != 150 {
                assert_eq!(a, b, "({x}, {y}) outside the cover");
            }
        }
    }
    // row 150 is 0.625 covered: a blend of the fill and the screen
    let (a, b) = (px(&plain, 120, 150), px(&cov, 120, 150));
    let want = 0.25 * 0.625 + a[0] * 0.375;
    assert!((b[0] - want).abs() < 1e-5, "{b:?} {want}");
    // half opacity: halfway between the screen and the fill
    let half = render(&draw(Some(c), 0.5));
    let (a, b) = (px(&plain, 120, 120), px(&half, 120, 120));
    assert!((b[1] - (0.5 * 0.5 + a[1] * 0.5)).abs() < 1e-5);
}

#[test]
fn pixelate_cover_draws_blocks() {
    // 90 px across → 6 blocks of 15 px; 50.625 px down → 3 blocks of ~16.9 px
    let c = CoverDraw {
        kind: CoverKind::Pixelate,
        grid: (6, 3),
    };
    let img = render(&draw(Some(c), 1.0));
    for by in 0..3u32 {
        for bx in 0..6u32 {
            let x0 = 90 + bx * 15;
            let y0 = 100 + (f64::from(by) * 50.625 / 3.0).ceil() as u32;
            let y1 = 100 + (f64::from(by + 1) * 50.625 / 3.0).floor() as u32;
            let first = px(&img, x0, y0);
            for y in y0..y1.min(150) {
                for x in x0..x0 + 15 {
                    assert_eq!(px(&img, x, y), first, "block ({bx}, {by}) at ({x}, {y})");
                }
            }
        }
    }
    // neighbouring blocks differ (the source is noise)
    assert_ne!(px(&img, 90, 101), px(&img, 105, 101));
}

#[test]
fn blur_cover_is_smooth_and_hides_the_detail() {
    let plain = render(&draw(None, 1.0));
    let c = CoverDraw {
        kind: CoverKind::Blur,
        grid: (6, 3),
    };
    let img = render(&draw(Some(c), 1.0));
    let step = |im: &RgbaImage| {
        (101..149)
            .flat_map(|y| (91..179).map(move |x| (x, y)))
            .map(|(x, y)| (luma(px(im, x, y)) - luma(px(im, x - 1, y))).abs())
            .fold(0.0f32, f32::max)
    };
    let (sharp, soft) = (step(&plain), step(&img));
    assert!(soft < 0.03, "blurred step {soft}");
    assert!(sharp > 0.3, "the screen is noise: {sharp}");
    // outside the cover nothing moved
    assert_eq!(px(&plain, 40, 120), px(&img, 40, 120));
    assert_eq!(px(&plain, 120, 180), px(&img, 120, 180));
}

/// A cover sits at its z: a layer above it draws over it.
#[test]
fn a_cover_is_drawn_at_its_z() {
    let c = CoverDraw {
        kind: CoverKind::Solid([1.0, 0.0, 0.0]),
        grid: (1, 1),
    };
    let mut d = draw(Some(c), 1.0);
    d.layers.push(layer(
        "face",
        r(0.0, 0.0, 32.0, 18.0),
        r(100.0, 110.0, 32.0, 18.0),
        None,
    ));
    let img = render(&d);
    assert_ne!(
        &px(&img, 110, 115)[..3],
        &[1.0, 0.0, 0.0],
        "the face is on top"
    );
    assert_eq!(
        &px(&img, 150, 140)[..3],
        &[1.0, 0.0, 0.0],
        "the cover elsewhere"
    );
}

/// The plan's `cover` reaches the draw list with its grid; the 8-bit canvas is pinned
/// (a regression golden, like the blurred fill).
#[test]
fn plan_cover_draws_and_golden() {
    let mut v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas/examples/h79-parts-1-3.renderplan.json"),
        )
        .unwrap(),
    )
    .unwrap();
    v["schema"] = serde_json::json!("gmclip.renderplan/1");
    let screen = v["parts"][0]["layers"][0].clone();
    let mut cover = screen.clone();
    cover["id"] = serde_json::json!("cover");
    cover["z"] = serde_json::json!(5);
    cover["cover"] = serde_json::json!({"kind": "blur", "radius": 24});
    v["parts"][0]["layers"].as_array_mut().unwrap().push(cover);
    let plan = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
    let d = frame_draw(&plan, 0).unwrap();
    let c = d
        .layers
        .iter()
        .find(|l| l.layer_id == "cover")
        .and_then(|l| l.cover.clone())
        .expect("the cover is drawn");
    let p = plan.parts[0].layers[0].place;
    assert_eq!(c.kind, CoverKind::Blur);
    assert_eq!(
        c.grid,
        ((p.w / 24.0).round() as u32, (p.h / 24.0).round() as u32)
    );
    // golden: three covers over the noise screen
    let mut bytes = Vec::new();
    for c in [
        CoverDraw {
            kind: CoverKind::Blur,
            grid: (6, 3),
        },
        CoverDraw {
            kind: CoverKind::Pixelate,
            grid: (9, 5),
        },
        CoverDraw {
            kind: CoverKind::Solid([0.047, 0.067, 0.094]),
            grid: (1, 1),
        },
    ] {
        let img = render(&draw(Some(c), 0.9));
        bytes.extend(
            img.px
                .iter()
                .flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 255.0).round() as u8)),
        );
    }
    let mut hsh: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        hsh ^= u64::from(*b);
        hsh = hsh.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/cover_layers.txt");
    if std::env::var_os("GMCLIP_BLESS").is_some() {
        std::fs::write(&path, format!("{hsh:016x}\n")).unwrap();
    }
    let want = std::fs::read_to_string(&path).expect("committed golden");
    assert_eq!(
        format!("{hsh:016x}"),
        want.trim(),
        "the cover pixels changed"
    );
}

/// Decision 0033: a pinned cover's place moves by its point's move in whole pixels and its
/// crop by the same move in source pixels, in the draw list the preview and the export
/// share; a cover without a track stays.
#[test]
fn a_pinned_cover_moves_with_its_point() {
    let mut v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas/examples/h79-parts-1-3.renderplan.json"),
        )
        .unwrap(),
    )
    .unwrap();
    v["schema"] = serde_json::json!("gmclip.renderplan/1");
    let screen = v["parts"][0]["layers"][0].clone();
    let a = v["parts"][0]["out"][0].as_u64().unwrap();
    for (id, z) in [("pinned", 5), ("still", 6)] {
        let mut cover = screen.clone();
        cover["id"] = serde_json::json!(id);
        cover["z"] = serde_json::json!(z);
        cover["cover"] = serde_json::json!({"kind": "pixelate", "block": 20});
        if id == "pinned" {
            cover["track"] = serde_json::json!({"x": 300, "y": 400, "keys": [
                {"frame": a, "x": 300, "y": 400}, {"frame": a + 20, "x": 340, "y": 370.4}]});
        }
        v["parts"][0]["layers"].as_array_mut().unwrap().push(cover);
    }
    let plan = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
    let at = |f: u64, id: &str| {
        let d = frame_draw(&plan, f).unwrap();
        let l = d.layers.iter().find(|l| l.layer_id == id).unwrap();
        (l.crop, l.place)
    };
    let (c0, p0) = at(a, "pinned");
    let (c1, p1) = at(a + 10, "pinned");
    let (c2, p2) = at(a + 30, "pinned");
    assert_eq!((p1.x - p0.x, p1.y - p0.y), (20.0, -15.0));
    assert_eq!(
        (p2.x - p0.x, p2.y - p0.y),
        (40.0, -30.0),
        "held after the last key"
    );
    let (kx, ky) = (c0.w / p0.w, c0.h / p0.h);
    assert!((c2.x - c0.x - 40.0 * kx).abs() < 1e-9 && (c2.y - c0.y + 30.0 * ky).abs() < 1e-9);
    assert_eq!((c1.w, c1.h, p1.w, p1.h), (c0.w, c0.h, p0.w, p0.h));
    assert_eq!(at(a, "still"), at(a + 30, "still"));
}

/// The GPU cover pass against the CPU: within 2 code values in NV12 for each style, at
/// full and half opacity, with a layer above the cover.
#[cfg(target_os = "macos")]
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn gpu_covers_match_the_cpu() {
    use gmclip_gpu::apple::{CpuPlanes, ImportUsage, MetalImporter, PixelBuffer, PlaneFormat};
    use gmclip_gpu::{GpuContext, GpuOptions};
    use gmclip_render::compositor::gpu::{GpuCompositor, GpuPlanes};
    use gmclip_render::nv12::apple::Nv12Output;
    use gmclip_render::nv12::rgb_to_nv12_cpu;

    let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
    let importer = MetalImporter::new(&ctx).unwrap();
    let mut comp = GpuCompositor::new(&ctx.device, OUT.0, OUT.1);
    let mut out = Nv12Output::new(&ctx, OUT.0, OUT.1, 3, 1).unwrap();
    let src = noise(SRC.0, SRC.1, 5);
    let mut pb = PixelBuffer::new(SRC.0, SRC.1, PlaneFormat::Nv12Video).unwrap();
    pb.write_planes(&CpuPlanes {
        width: SRC.0,
        height: SRC.1,
        format: PlaneFormat::Nv12Video,
        y: src.y.clone(),
        uv: src.uv.clone(),
    })
    .unwrap();
    let planes = importer.import(&pb, ImportUsage::SAMPLE).unwrap();
    let (vy, vuv) = (
        planes.y.create_view(&Default::default()),
        planes.uv.create_view(&Default::default()),
    );
    let mut worst = 0u8;
    for (kind, grid) in [
        (CoverKind::Blur, (6, 3)),
        (CoverKind::Pixelate, (9, 5)),
        (CoverKind::Solid([0.25, 0.5, 0.75]), (1, 1)),
    ] {
        for opacity in [1.0, 0.5] {
            let mut d = draw(Some(CoverDraw { kind, grid }), opacity);
            d.layers.push(layer(
                "face",
                r(0.0, 0.0, 32.0, 18.0),
                r(100.0, 110.0, 32.0, 18.0),
                None,
            ));
            let cpu_img = render(&d);
            let cpu = rgb_to_nv12_cpu(&cpu_img.rgb(), OUT.0 as usize, OUT.1 as usize);
            let gp: Vec<GpuPlanes<'_>> = d
                .layers
                .iter()
                .map(|_| GpuPlanes {
                    y: &vy,
                    uv: &vuv,
                    width: SRC.0,
                    height: SRC.1,
                    code_scale: 255.0,
                    siting: ChromaSiting::Left,
                })
                .collect();
            let mut enc = ctx.device.create_command_encoder(&Default::default());
            comp.encode(&ctx.queue, &mut enc, &d, &gp).unwrap();
            ctx.queue.submit([enc.finish()]);
            let view = comp.canvas().create_view(&Default::default());
            let gpu = out.render_now(&view).unwrap().read_planes().unwrap();
            let dmax = cpu
                .0
                .iter()
                .zip(&gpu.y)
                .chain(cpu.1.iter().zip(&gpu.uv))
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            eprintln!("cover {kind:?} grid {grid:?} opacity {opacity}: GPU vs CPU max |Δ| {dmax}");
            worst = worst.max(dmax);
        }
    }
    assert!(worst <= 2, "GPU vs CPU covers: {worst}");
}
