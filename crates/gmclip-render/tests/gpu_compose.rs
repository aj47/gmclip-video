//! H-05 on any GPU (the 5080 PC, M5 prep): the GPU compositor against the CPU one on
//! synthetic 2560×1440 NV12 sources composited to a 1080×1920 frame, through plain wgpu
//! textures (no OS import, no ffmpeg, no media), so it runs on DX12, Vulkan or Metal.
//! Both canvases go through the same CPU NV12 conversion; the gate is `compositor_lab`'s
//! (max |GPU − CPU| ≤ 2 codes). Needs a GPU, so ignored by default:
//!
//!   WGPU_BACKEND=dx12 cargo test -p gmclip-render --test gpu_compose -- --ignored --nocapture
//!   WGPU_BACKEND=vulkan …   (the backend override applies off Apple only)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gmclip_gpu::{GpuContext, GpuOptions};
use gmclip_plan::types::Rect;
use gmclip_render::compositor::cpu::{FrameProvider, Nv12Image, compose_with};
use gmclip_render::compositor::gpu::{GpuCompositor, GpuPlanes};
use gmclip_render::compositor::{ChromaSiting, ComposeError, FrameDraw, LayerDraw};
use gmclip_render::nv12::rgb_to_nv12_cpu;
use gmclip_render::text::raster::Tile;

const W: u32 = 2560;
const H: u32 = 1440;
const OUT_W: u32 = 1080;
const OUT_H: u32 = 1920;

struct Frames(HashMap<(String, u64), Arc<Nv12Image>>);

impl FrameProvider for Frames {
    fn frame(&mut self, source: &str, frame: u64) -> Result<Arc<Nv12Image>, ComposeError> {
        self.0
            .get(&(source.to_owned(), frame))
            .cloned()
            .ok_or_else(|| ComposeError::Source(format!("{source}#{frame}")))
    }
}

/// A smooth picture (gradients and a soft disc), like video rather than noise.
fn picture(seed: u32) -> Nv12Image {
    let (cw, ch) = (W.div_ceil(2), H.div_ceil(2));
    let mut y = vec![0u8; (W * H) as usize];
    for j in 0..H {
        for i in 0..W {
            let (fx, fy) = (i as f32 / W as f32, j as f32 / H as f32);
            let d = ((fx - 0.6).powi(2) + (fy - 0.4).powi(2)).sqrt();
            let v = 40.0 + 120.0 * fx + 50.0 * fy + 40.0 * (1.0 - (d * 4.0).min(1.0));
            y[(j * W + i) as usize] = (v + (seed % 7) as f32).clamp(16.0, 235.0) as u8;
        }
    }
    let mut uv = vec![0u8; (cw * ch * 2) as usize];
    for j in 0..ch {
        for i in 0..cw {
            let k = ((j * cw + i) * 2) as usize;
            uv[k] = (100.0 + 50.0 * i as f32 / cw as f32) as u8;
            uv[k + 1] = (150.0 - 40.0 * j as f32 / ch as f32) as u8;
        }
    }
    Nv12Image {
        width: W,
        height: H,
        y,
        uv,
        siting: ChromaSiting::Left,
    }
}

/// Worst case for filtering: white noise in every plane.
fn noise(seed: u32) -> Nv12Image {
    let mut s = seed | 1;
    let mut n = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (16 + s % 220) as u8
    };
    let (cw, ch) = (W.div_ceil(2) as usize, H.div_ceil(2) as usize);
    Nv12Image {
        width: W,
        height: H,
        y: (0..(W * H) as usize).map(|_| n()).collect(),
        uv: (0..cw * ch * 2).map(|_| n()).collect(),
        siting: ChromaSiting::Left,
    }
}

fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

fn layer(id: &str, source: &str, crop: Rect, place: Rect, opacity: f32) -> LayerDraw {
    LayerDraw {
        layer_id: id.into(),
        source: source.into(),
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

/// The 9:16 gameplay + facecam shape: a facecam crop scaled up across the top, the
/// game scaled down below it, a 1:1 detail at a fractional place with opacity.
fn draw(source: &str) -> FrameDraw {
    FrameDraw {
        out_frame: 0,
        part_id: "p".into(),
        background: [0.047, 0.067, 0.094],
        backdrop: None,
        tiles_at: vec![],
        layers: vec![
            layer(
                "cam",
                source,
                r(1980.0, 1000.0, 480.0, 360.0),
                r(0.0, 0.0, 1080.0, 810.0),
                1.0,
            ),
            layer(
                "screen",
                source,
                r(0.0, 0.0, 2560.0, 1440.0),
                r(0.0, 830.0, 1080.0, 607.5),
                1.0,
            ),
            layer(
                "detail",
                source,
                r(1200.0, 300.0, 400.0, 300.0),
                r(340.5, 1500.25, 400.0, 300.0),
                0.8,
            ),
        ],
    }
}

fn upload(ctx: &GpuContext, img: &Nv12Image) -> (wgpu::Texture, wgpu::Texture) {
    let tex = |w: u32, h: u32, format, bpp: u32, data: &[u8]| {
        let t = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("source plane"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * bpp),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        t
    };
    let y = tex(
        img.width,
        img.height,
        wgpu::TextureFormat::R8Unorm,
        1,
        &img.y,
    );
    let (cw, ch) = (img.width.div_ceil(2), img.height.div_ceil(2));
    let uv = tex(cw, ch, wgpu::TextureFormat::Rg8Unorm, 2, &img.uv);
    (y, uv)
}

fn f16_to_f32(h: u16) -> f32 {
    let s = if h >> 15 == 1 { -1.0 } else { 1.0 };
    let e = i32::from((h >> 10) & 0x1f);
    let m = f32::from(h & 0x3ff);
    s * match e {
        0 => m / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Reads the Rgba16Float canvas back as RGB.
fn read_canvas(ctx: &GpuContext, canvas: &wgpu::Texture) -> Vec<[f32; 3]> {
    let row = OUT_W * 8;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canvas readback"),
        size: u64::from(padded * OUT_H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: canvas,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(OUT_H),
            },
        },
        wgpu::Extent3d {
            width: OUT_W,
            height: OUT_H,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    ctx.wait_idle().unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((OUT_W * OUT_H) as usize);
    for y in 0..OUT_H {
        let rowb = &data[(y * padded) as usize..][..row as usize];
        for px in rowb.chunks_exact(8) {
            let c = |k: usize| f16_to_f32(u16::from_le_bytes([px[k], px[k + 1]]));
            out.push([c(0), c(2), c(4)]);
        }
    }
    out
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// One draw on the CPU and on the GPU: (max |ΔY|, max |ΔCbCr|) in NV12 codes, GPU ms
/// per frame (submit + wait, mean of 30 after a warm-up) and CPU ms.
fn compare(
    ctx: &GpuContext,
    comp: &mut GpuCompositor,
    d: &FrameDraw,
    name: &str,
    img: &Arc<Nv12Image>,
    among: &[(usize, Arc<Tile>)],
) -> (u8, u8, f64, f64) {
    let t = Instant::now();
    let mut frames = Frames(HashMap::from([((name.to_owned(), 0), Arc::clone(img))]));
    let canvas = compose_with(d, OUT_W, OUT_H, &mut frames, None, among).unwrap();
    comp.set_among(among.to_vec());
    let cpu_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = rgb_to_nv12_cpu(&canvas.rgb(), OUT_W as usize, OUT_H as usize);
    let (ty, tuv) = upload(ctx, img);
    let (vy, vuv) = (
        ty.create_view(&Default::default()),
        tuv.create_view(&Default::default()),
    );
    let planes: Vec<GpuPlanes<'_>> = d
        .layers
        .iter()
        .map(|_| GpuPlanes {
            y: &vy,
            uv: &vuv,
            width: W,
            height: H,
            code_scale: 255.0,
            siting: img.siting,
        })
        .collect();
    let mut frame = || {
        let mut enc = ctx.device.create_command_encoder(&Default::default());
        comp.encode(&ctx.queue, &mut enc, d, &planes).unwrap();
        ctx.queue.submit([enc.finish()]);
        ctx.wait_idle().unwrap();
    };
    frame();
    const N: u32 = 30;
    let t = Instant::now();
    for _ in 0..N {
        frame();
    }
    let gpu_ms = t.elapsed().as_secs_f64() * 1e3 / f64::from(N);
    let gpu_rgb = read_canvas(ctx, comp.canvas());
    let gpu = rgb_to_nv12_cpu(&gpu_rgb, OUT_W as usize, OUT_H as usize);
    (
        max_diff(&gpu.0, &cpu.0),
        max_diff(&gpu.1, &cpu.1),
        gpu_ms,
        cpu_ms,
    )
}

fn gpu() -> GpuContext {
    let ctx = GpuContext::new(&GpuOptions::default()).expect("a GPU adapter");
    let info = ctx.adapter.get_info();
    eprintln!(
        "adapter: {} ({:?}, driver {} {})",
        info.name, info.backend, info.driver, info.driver_info
    );
    ctx
}

#[test]
#[ignore = "needs a GPU: cargo test -p gmclip-render --test gpu_compose -- --ignored"]
fn gpu_matches_cpu_on_a_1080x1920_frame() {
    let ctx = gpu();
    let backend = ctx.adapter.get_info().backend;
    let mut comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
    for (name, img, gate) in [("picture", picture(3), 2u8), ("noise", noise(7), 2u8)] {
        let img = Arc::new(img);
        let (dy, duv, gpu_ms, cpu_ms) = compare(&ctx, &mut comp, &draw(name), name, &img, &[]);
        eprintln!(
            "{name}: {backend:?} max |GPU − CPU| Y {dy} CbCr {duv}; 1080×1920 frame, 3 layers: GPU {gpu_ms:.2} ms (submit + wait, mean of 30), CPU {cpu_ms:.0} ms"
        );
        assert!(
            dy <= gate && duv <= gate,
            "{name}: GPU and CPU paths diverge"
        );
    }
}

/// Cover layers (decision 0021) on any GPU: a cover over part of the game area, each
/// kind at full and partial opacity, under the 1:1 detail (a layer drawn above it), with
/// the plan's grid (place / 24 for blur and pixelate). Same gate as the layers.
#[test]
#[ignore = "needs a GPU: cargo test -p gmclip-render --test gpu_compose -- --ignored"]
fn gpu_covers_match_the_cpu_on_a_1080x1920_frame() {
    use gmclip_render::compositor::{CoverDraw, CoverKind};
    let ctx = gpu();
    let backend = ctx.adapter.get_info().backend;
    let mut comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
    let place = r(60.0, 1180.0, 600.0, 240.0);
    let grid = (
        (place.w / 24.0).round() as u32,
        (place.h / 24.0).round() as u32,
    );
    let mut worst = 0u8;
    for (name, img) in [("picture", picture(5)), ("noise", noise(9))] {
        let img = Arc::new(img);
        for (kind, label) in [
            (CoverKind::Blur, "blur"),
            (CoverKind::Pixelate, "pixelate"),
            (CoverKind::Solid([0.047, 0.067, 0.094]), "solid"),
        ] {
            for opacity in [1.0f32, 0.6] {
                let mut d = draw(name);
                // the chat region of the game, covered where it lands on the canvas
                let mut cover = layer(
                    "cover",
                    name,
                    r(1600.0, 900.0, 900.0, 360.0),
                    place,
                    opacity,
                );
                cover.cover = Some(CoverDraw {
                    kind,
                    grid: if matches!(kind, CoverKind::Solid(_)) {
                        (1, 1)
                    } else {
                        grid
                    },
                });
                d.layers.insert(2, cover);
                let (dy, duv, gpu_ms, cpu_ms) = compare(&ctx, &mut comp, &d, name, &img, &[]);
                eprintln!(
                    "cover {label} @{opacity} on {name}: {backend:?} max |GPU − CPU| Y {dy} CbCr {duv}; 4 layers: GPU {gpu_ms:.2} ms, CPU {cpu_ms:.0} ms"
                );
                worst = worst.max(dy).max(duv);
            }
        }
    }
    assert!(worst <= 2, "covers: GPU and CPU paths diverge ({worst})");
}

/// Decision 0042 on any GPU: image tiles drawn among the layers (before the first, between
/// two, after the last), half transparent, and on a compositor that has drawn no layer yet
/// (its "nothing visible" path). Same gate as the layers.
#[test]
#[ignore = "needs a GPU: cargo test -p gmclip-render --test gpu_compose -- --ignored"]
fn gpu_tiles_among_the_layers_match_the_cpu() {
    let ctx = gpu();
    // premultiplied: half-transparent red, and an opaque green bar
    let tile = |x: i32, y: i32, w: u32, h: u32, px: [u8; 4]| {
        Arc::new(Tile {
            x,
            y,
            width: w,
            height: h,
            rgba: px.repeat((w * h) as usize),
        })
    };
    let red = tile(200, 300, 700, 900, [128, 0, 0, 128]);
    let green = tile(0, 1000, 1080, 120, [0, 255, 0, 255]);
    let img = Arc::new(picture(11));
    let mut worst = 0u8;
    {
        let mut comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
        let mut empty = draw("picture");
        empty.layers.clear();
        let (dy, duv, _, _) = compare(
            &ctx,
            &mut comp,
            &empty,
            "picture",
            &img,
            &[(0, Arc::clone(&red))],
        );
        eprintln!("tiles with no layer: max |GPU − CPU| Y {dy} CbCr {duv}");
        worst = worst.max(dy).max(duv);
    }
    let mut comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
    let d = draw("picture");
    let n = d.layers.len();
    for among in [
        vec![(0, Arc::clone(&red))],
        vec![(1, Arc::clone(&red)), (1, Arc::clone(&green))],
        vec![(0, Arc::clone(&green)), (n, Arc::clone(&red))],
    ] {
        let at: Vec<usize> = among.iter().map(|(i, _)| *i).collect();
        let (dy, duv, _, _) = compare(&ctx, &mut comp, &d, "picture", &img, &among);
        eprintln!("tiles at {at:?}: max |GPU − CPU| Y {dy} CbCr {duv}");
        worst = worst.max(dy).max(duv);
    }
    assert!(
        worst <= 2,
        "tiles among layers: GPU and CPU paths diverge ({worst})"
    );
}

/// Decision 0046 on any GPU: layer frames — the screen with rounded corners, a soft shadow
/// and a thin border; the camera as a circle bubble with a white border; the detail at
/// 0.8 opacity with a rounded frame and a shadow — with a tile drawn among them (decision
/// 0042). Same gate as the layers (R-COMP-1: the preview and the export compose on this
/// path, the CPU is the reference).
#[test]
#[ignore = "needs a GPU: cargo test -p gmclip-render --test gpu_compose -- --ignored"]
fn gpu_frames_match_the_cpu() {
    use gmclip_render::compositor::{BorderDraw, ShadowDraw, ShapeDraw};
    let ctx = gpu();
    let backend = ctx.adapter.get_info().backend;
    let mut comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
    let shadow = ShadowDraw {
        blur: 0.05,
        offset_y: 0.015,
        opacity: 0.45,
        rgb: [0.0; 3],
    };
    let mut worst = 0u8;
    for (name, img) in [("picture", picture(13)), ("noise", noise(17))] {
        let img = Arc::new(img);
        let mut d = draw(name);
        // the camera: a square place, a circle with a white border
        d.layers[0].place = r(700.25, 1450.5, 300.0, 300.0);
        d.layers[0].crop = r(1980.0, 1000.0, 360.0, 360.0);
        d.layers[0].shape = Some(ShapeDraw {
            radius: 0.5,
            shadow: Some(shadow),
            border: Some(BorderDraw {
                width: 0.005,
                rgb: [1.0; 3],
            }),
        });
        // the screen: padded, rounded, shadowed, a dark border
        d.layers[1].place = r(64.8, 830.0, 950.4, 534.6);
        d.layers[1].shape = Some(ShapeDraw {
            radius: 0.03,
            shadow: Some(shadow),
            border: Some(BorderDraw {
                width: 0.002,
                rgb: [0.1, 0.1, 0.1],
            }),
        });
        d.layers[2].shape = Some(ShapeDraw {
            radius: 0.2,
            shadow: Some(ShadowDraw {
                blur: 0.0,
                offset_y: 0.01,
                opacity: 1.0,
                rgb: [0.9, 0.2, 0.1],
            }),
            border: None,
        });
        // the camera drawn last (over the screen), a tile between screen and detail
        let cam = d.layers.remove(0);
        d.layers.push(cam);
        let tile = Arc::new(Tile {
            x: 100,
            y: 1300,
            width: 600,
            height: 200,
            rgba: [0, 0, 128, 128].repeat(600 * 200),
        });
        let (dy, duv, gpu_ms, cpu_ms) =
            compare(&ctx, &mut comp, &d, name, &img, &[(1, Arc::clone(&tile))]);
        eprintln!(
            "frames on {name}: {backend:?} max |GPU − CPU| Y {dy} CbCr {duv}; GPU {gpu_ms:.2} ms, CPU {cpu_ms:.0} ms"
        );
        worst = worst.max(dy).max(duv);
    }
    assert!(worst <= 2, "frames: GPU and CPU paths diverge ({worst})");
}
