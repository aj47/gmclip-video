//! Lab half of the H-05 tests, on real 1440p frames of AJ's sources: the h79 parts 1–3
//! and BENCH-60-M0 example plans (H.264 MMO restream with left-sited chroma, VP9 MKV
//! with untagged = centred chroma), plus hold / freeze_first and a fractional rect.
//! Against ffmpeg composites of the RT graph without `ass` (`setparams` siting,
//! `crop=…:exact=1,scale=…:flags=lanczos`, `overlay=…:format=yuv444` over
//! `color=0x0c1118`): luma SSIM inside each layer's place rect ≥ 0.99, chroma SSIM
//! and p99.9 |ΔY| reported, luma PSNR ≥ 40 dB; GPU vs CPU max |ΔY|, |ΔCbCr| ≤ 2.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use gmclip_gpu::apple::{CpuPlanes, ImportUsage, MetalImporter, PixelBuffer, PlaneFormat};
use gmclip_gpu::{GpuContext, GpuOptions};
use gmclip_plan::types::{Rect, TimeMap};
use gmclip_render::compositor::cpu::{Nv12Image, compose};
use gmclip_render::compositor::gpu::{GpuCompositor, GpuPlanes};
use gmclip_render::compositor::{ChromaSiting, FrameDraw, frame_draw};
use gmclip_render::nv12::apple::Nv12Output;
use gmclip_render::nv12::rgb_to_nv12_cpu;

use super::Frames;

const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
/// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
fn mkv() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv"));
    P.as_str()
}
/// `streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4` in the lab media folder.
fn mmo() -> &'static str {
    static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        gmclip_model::lab::media_str(
            "streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
        )
    });
    P.as_str()
}
const W: u32 = 2560;
const H: u32 = 1440;
const OUT_W: u32 = 1080;
const OUT_H: u32 = 1920;

fn lab_dir() -> PathBuf {
    let d = std::env::var_os("GMCLIP_LAB_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lab-media")
        });
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Plan source id → (file, chroma siting).
fn source_file(id: &str) -> (&'static str, ChromaSiting) {
    match id {
        "mkv1440" | "b-mkv" => (mkv(), ChromaSiting::Center),
        "a" => (mmo(), ChromaSiting::Left),
        other => panic!("no lab file for source {other}"),
    }
}

/// One decoded frame as raw yuv420p (cached), by presentation index at 30 fps.
fn source_yuv(id: &str, index: u64) -> PathBuf {
    let (file, _) = source_file(id);
    let tag = if file == mkv() { "mkv1440" } else { "mmo1440" };
    let p = lab_dir().join(format!("h05-{tag}-{index}.yuv"));
    if !p.exists() {
        let ss = format!("{:.6}", index as f64 / 30.0);
        let raw = run(Command::new(FFMPEG).args([
            "-v",
            "error",
            "-ss",
            &ss,
            "-i",
            file,
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ]));
        assert_eq!(raw.len(), (W * H * 3 / 2) as usize);
        std::fs::write(&p, raw).unwrap();
    }
    p
}

fn nv12_from_i420(raw: &[u8], w: u32, h: u32, siting: ChromaSiting) -> Nv12Image {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let u = &raw[w * h..w * h + cw * ch];
    let v = &raw[w * h + cw * ch..];
    let mut uv = Vec::with_capacity(cw * ch * 2);
    for i in 0..cw * ch {
        uv.push(u[i]);
        uv.push(v[i]);
    }
    Nv12Image {
        width: w as u32,
        height: h as u32,
        y: raw[..w * h].to_vec(),
        uv,
        siting,
    }
}

/// ffmpeg's composite of the RT graph for `draw` (integer rects), as yuv420p planes.
fn ffmpeg_reference(draw: &FrameDraw) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut inputs: Vec<(String, u64)> = Vec::new();
    for l in &draw.layers {
        if !inputs.contains(&(l.source.clone(), l.frame)) {
            inputs.push((l.source.clone(), l.frame));
        }
    }
    let mut cmd = Command::new(FFMPEG);
    cmd.args(["-v", "error"]);
    for (src, f) in &inputs {
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{W}x{H}"),
            "-i",
        ])
        .arg(source_yuv(src, *f));
    }
    let mut g = format!("color=c=0x0c1118:s={OUT_W}x{OUT_H}:d=1,format=yuv420p[bg0];");
    for (i, l) in draw.layers.iter().enumerate() {
        let k = inputs
            .iter()
            .position(|x| x == &(l.source.clone(), l.frame))
            .unwrap();
        let site = match source_file(&l.source).1 {
            ChromaSiting::Left => "left",
            ChromaSiting::Center => "center",
        };
        let (c, p) = (l.crop, l.place);
        assert!(
            [c.x, c.y, c.w, c.h, p.x, p.y, p.w, p.h]
                .iter()
                .all(|v| v.fract() == 0.0),
            "integer rects only"
        );
        g += &format!(
            "[{k}:v]setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv:chroma_location={site},crop={}:{}:{}:{}:exact=1,scale={}:{}:flags=lanczos,setsar=1[l{i}];",
            c.w as i64, c.h as i64, c.x as i64, c.y as i64, p.w as i64, p.h as i64
        );
    }
    let n = draw.layers.len();
    for (i, l) in draw.layers.iter().enumerate() {
        g += &format!(
            "[bg{i}][l{i}]overlay={}:{}:format=yuv444[bg{}];",
            l.place.x as i64,
            l.place.y as i64,
            i + 1
        );
    }
    g += &format!("[bg{n}]format=yuv420p[out]");
    let raw = run(cmd.args([
        "-filter_complex",
        &g,
        "-map",
        "[out]",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-",
    ]));
    let ys = (OUT_W * OUT_H) as usize;
    let cs = ys / 4;
    (
        raw[..ys].to_vec(),
        raw[ys..ys + cs].to_vec(),
        raw[ys + cs..ys + 2 * cs].to_vec(),
    )
}

/// Mean SSIM over 8×8 windows (stride 4) inside `[x0, x1) × [y0, y1)`.
fn ssim(a: &[u8], b: &[u8], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> f64 {
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let (mut sum, mut n) = (0.0, 0usize);
    let mut y = y0;
    while y + 8 <= y1 {
        let mut x = x0;
        while x + 8 <= x1 {
            let (mut ma, mut mb, mut va, mut vb, mut cov) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for j in 0..8 {
                for i in 0..8 {
                    let p = (y + j) * stride + x + i;
                    let (pa, pb) = (f64::from(a[p]), f64::from(b[p]));
                    ma += pa;
                    mb += pb;
                    va += pa * pa;
                    vb += pb * pb;
                    cov += pa * pb;
                }
            }
            let k = 64.0;
            let (ma, mb) = (ma / k, mb / k);
            let (va, vb, cov) = (va / k - ma * ma, vb / k - mb * mb, cov / k - ma * mb);
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
            x += 4;
        }
        y += 4;
    }
    sum / n as f64
}

/// 99.9th percentile of |a − b| inside a rect.
fn p999(a: &[u8], b: &[u8], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> u8 {
    let mut d: Vec<u8> = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            d.push(a[y * stride + x].abs_diff(b[y * stride + x]));
        }
    }
    d.sort_unstable();
    d[(d.len() - 1) * 999 / 1000]
}

/// Luma PSNR (dB) inside a rect.
fn psnr(a: &[u8], b: &[u8], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> f64 {
    let mut se = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            se += (f64::from(a[y * stride + x]) - f64::from(b[y * stride + x])).powi(2);
        }
    }
    let mse = se / ((x1 - x0) * (y1 - y0)) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn split_uv(uv: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (
        uv.iter().step_by(2).copied().collect(),
        uv.iter().skip(1).step_by(2).copied().collect(),
    )
}

/// (Y, interleaved CbCr).
type Nv12Planes = (Vec<u8>, Vec<u8>);

struct Lab {
    ctx: GpuContext,
    importer: MetalImporter,
    comp: GpuCompositor,
    out: Nv12Output,
    worst_cpu: f64,
    worst_gpu: f64,
    worst_chroma: f64,
    worst_p999: u8,
    worst_psnr: f64,
    max_gpu_cpu: u8,
    checked: usize,
}

impl Lab {
    fn new() -> Self {
        let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
        let importer = MetalImporter::new(&ctx).unwrap();
        let comp = GpuCompositor::new(&ctx.device, OUT_W, OUT_H);
        let out = Nv12Output::new(&ctx, OUT_W, OUT_H, 3, 1).unwrap();
        Self {
            ctx,
            importer,
            comp,
            out,
            worst_cpu: 1.0,
            worst_gpu: 1.0,
            worst_chroma: 1.0,
            worst_p999: 0,
            worst_psnr: 99.0,
            max_gpu_cpu: 0,
            checked: 0,
        }
    }

    /// Composites `draw` on CPU and GPU; returns (CPU NV12, GPU NV12).
    fn render(
        &mut self,
        draw: &FrameDraw,
        tiles: &[Arc<gmclip_render::text::raster::Tile>],
    ) -> (Nv12Planes, Nv12Planes) {
        let mut images: HashMap<(String, u64), Arc<Nv12Image>> = HashMap::new();
        for l in &draw.layers {
            images
                .entry((l.source.clone(), l.frame))
                .or_insert_with(|| {
                    let raw = std::fs::read(source_yuv(&l.source, l.frame)).unwrap();
                    Arc::new(nv12_from_i420(&raw, W, H, source_file(&l.source).1))
                });
        }
        let mut provider = Frames(images.clone());
        let mut canvas = compose(draw, OUT_W, OUT_H, &mut provider).unwrap();
        gmclip_render::compositor::text_layer::composite_cpu(&mut canvas, tiles);
        let cpu = rgb_to_nv12_cpu(&canvas.rgb(), OUT_W as usize, OUT_H as usize);

        // GPU: each source frame in an IOSurface pixel buffer, imported zero-copy.
        let mut textures = HashMap::new();
        for (key, img) in &images {
            let mut pb = PixelBuffer::new(W, H, PlaneFormat::Nv12Video).unwrap();
            pb.write_planes(&CpuPlanes {
                width: W,
                height: H,
                format: PlaneFormat::Nv12Video,
                y: img.y.clone(),
                uv: img.uv.clone(),
            })
            .unwrap();
            let planes = self.importer.import(&pb, ImportUsage::SAMPLE).unwrap();
            let views = (
                planes.y.create_view(&Default::default()),
                planes.uv.create_view(&Default::default()),
                img.siting,
            );
            textures.insert(key.clone(), (planes, views));
        }
        let gp: Vec<GpuPlanes<'_>> = draw
            .layers
            .iter()
            .map(|l| {
                let (_, (vy, vuv, siting)) = &textures[&(l.source.clone(), l.frame)];
                GpuPlanes {
                    y: vy,
                    uv: vuv,
                    width: W,
                    height: H,
                    code_scale: 255.0,
                    siting: *siting,
                }
            })
            .collect();
        let mut enc = self.ctx.device.create_command_encoder(&Default::default());
        self.comp
            .encode(&self.ctx.queue, &mut enc, draw, &gp)
            .unwrap();
        self.comp.draw_tiles(&self.ctx.queue, &mut enc, tiles);
        self.ctx.queue.submit([enc.finish()]);
        let view = self.comp.canvas().create_view(&Default::default());
        let gpu = self.out.render_now(&view).unwrap().read_planes().unwrap();
        (cpu, (gpu.y, gpu.uv))
    }

    fn check(&mut self, draw: &FrameDraw, label: &str) {
        let (cpu, gpu) = self.render(draw, &[]);
        let (ry, ru, rv) = ffmpeg_reference(draw);
        let (gu, gv) = split_uv(&gpu.1);
        let s = OUT_W as usize;
        for l in &draw.layers {
            let p = l.place;
            let (x0, y0) = (p.x.max(0.0) as usize, p.y.max(0.0) as usize);
            let (x1, y1) = (
                ((p.x + p.w) as usize).min(OUT_W as usize),
                ((p.y + p.h) as usize).min(OUT_H as usize),
            );
            let s_cpu = ssim(&cpu.0, &ry, s, x0, y0, x1, y1);
            let s_gpu = ssim(&gpu.0, &ry, s, x0, y0, x1, y1);
            let s_u = ssim(&gu, &ru, s / 2, x0 / 2, y0 / 2, x1 / 2, y1 / 2);
            let s_v = ssim(&gv, &rv, s / 2, x0 / 2, y0 / 2, x1 / 2, y1 / 2);
            let q = p999(&gpu.0, &ry, s, x0, y0, x1, y1);
            let db = psnr(&gpu.0, &ry, s, x0, y0, x1, y1);
            self.worst_psnr = self.worst_psnr.min(db);
            eprintln!(
                "H-05 {label} out {} layer {:<8} {:?}→{:?}: Y SSIM cpu {s_cpu:.4} gpu {s_gpu:.4}; Cb/Cr SSIM gpu {s_u:.4}/{s_v:.4}; p99.9 |ΔY| {q}, PSNR {db:.1} dB",
                draw.out_frame,
                l.layer_id,
                (l.crop.x, l.crop.y, l.crop.w, l.crop.h),
                (l.place.x, l.place.y, l.place.w, l.place.h)
            );
            self.worst_cpu = self.worst_cpu.min(s_cpu);
            self.worst_gpu = self.worst_gpu.min(s_gpu);
            self.worst_chroma = self.worst_chroma.min(s_u.min(s_v));
            self.worst_p999 = self.worst_p999.max(q);
        }
        let dy = gpu
            .0
            .iter()
            .zip(&cpu.0)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        let duv = gpu
            .1
            .iter()
            .zip(&cpu.1)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        self.max_gpu_cpu = self.max_gpu_cpu.max(dy).max(duv);
        self.checked += 1;
    }
}

fn plan(name: &str) -> gmclip_plan::Plan {
    let path = format!(
        "{}/../../schemas/examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut plan = gmclip_plan::parse(&std::fs::read(path).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    plan
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal + ffmpeg + AJ's media (run with --features lab)"
)]
fn example_plans_vs_ffmpeg_cpu_and_gpu() {
    let mut lab = Lab::new();
    // h79 parts 1–3 (VP9 source): 3 frames per part.
    let h79 = plan("h79-parts-1-3.renderplan.json");
    for part in &h79.parts {
        let (a, b) = (part.out.start(), part.out.end());
        for f in [a, (a + b) / 2, b - 1] {
            lab.check(
                &frame_draw(&h79, f).unwrap(),
                &format!("h79/{}", part.id.as_str()),
            );
        }
    }
    // BENCH-60-M0, first four parts (two H.264, two VP9; 4 layers incl. a 1:1 detail):
    // the ones whose sources are on this machine.
    let bench = plan("bench-60-m0.renderplan.json");
    let bench_parts = gmclip_model::lab::present(
        bench.parts.iter().take(4),
        |p| p.id.as_str().to_owned(),
        |p| {
            std::iter::once(&p.picture.source)
                .chain(p.layers.iter().filter_map(|l| l.picture_source.as_ref()))
                .map(|id| source_file(id.as_str()).0)
                .collect::<Vec<_>>()
        },
    );
    eprintln!("H-05 BENCH-60-M0: {}", bench_parts.note("first parts"));
    for part in bench_parts.items.iter().copied() {
        let (a, b) = (part.out.start(), part.out.end());
        for f in [a, (a + b) / 2, b - 1] {
            lab.check(
                &frame_draw(&bench, f).unwrap(),
                &format!("bench/{}", part.id.as_str()),
            );
        }
    }
    // hold and freeze_first on real frames (h79 part 3: screen held on an earlier
    // frame, detail frozen at the part's first frame).
    let mut held = h79.clone();
    held.parts[2].layers[0].time_map = Some(TimeMap::Hold {
        frame: 75_900,
        provenance: None,
    });
    held.parts[2].layers[2].time_map = Some(TimeMap::FreezeFirst);
    let d = frame_draw(&held, 300).unwrap();
    assert_eq!(
        (d.layers[0].frame, d.layers[1].frame, d.layers[2].frame),
        (75_900, 75_956 + 124, 75_956)
    );
    lab.check(&d, "h79-hold-freeze");
    eprintln!(
        "H-05: {} frames vs ffmpeg; worst luma SSIM cpu {:.4}, gpu {:.4}; worst luma PSNR {:.1} dB; worst chroma SSIM (gpu) {:.4}; worst p99.9 |ΔY| {}; max |GPU − CPU| {}",
        lab.checked,
        lab.worst_cpu,
        lab.worst_gpu,
        lab.worst_psnr,
        lab.worst_chroma,
        lab.worst_p999,
        lab.max_gpu_cpu
    );
    assert!(
        lab.worst_cpu >= 0.99 && lab.worst_gpu >= 0.99,
        "luma SSIM gate"
    );
    // Tighter than SSIM: a half-pixel misregistration or a wrong widening factor on
    // text costs far more than this. (p99.9 is only reported: it reaches ~20 on a
    // 35-row crop upscaled 2.4×, where swscale and our clamp treat the crop's edge
    // rows differently.)
    assert!(lab.worst_psnr >= 40.0, "luma PSNR gate");
    assert!(lab.max_gpu_cpu <= 2, "GPU and CPU paths diverge");

    // A fractional crop and place (no ffmpeg equivalent): CPU vs GPU only.
    let mut d = frame_draw(&h79, 60).unwrap();
    d.layers[0].crop = Rect {
        x: 770.25,
        y: 220.5,
        w: 1280.0,
        h: 900.0,
    };
    d.layers[0].place = Rect {
        x: 160.5,
        y: 420.25,
        w: 760.0,
        h: 534.375,
    };
    let (cpu, gpu) = lab.render(&d, &[]);
    let m = gpu
        .0
        .iter()
        .zip(&cpu.0)
        .chain(gpu.1.iter().zip(&cpu.1))
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    eprintln!("H-05 fractional crop/place: max |GPU − CPU| {m}");
    assert!(m <= 2);

    // Throughput of compositor + NV12 output on a 4-layer bench frame (source (a)).
    if gmclip_model::lab::first_missing([mmo()]).is_some() {
        eprintln!(
            "H-05 throughput row: {}",
            gmclip_model::lab::skip_line(mmo())
        );
        return;
    }
    let draw = frame_draw(&bench, 10).unwrap();
    let _ = lab.render(&draw, &[]); // warm
    let raw = std::fs::read(source_yuv("a", draw.layers[0].frame)).unwrap();
    let img = nv12_from_i420(&raw, W, H, ChromaSiting::Left);
    let mut pb = PixelBuffer::new(W, H, PlaneFormat::Nv12Video).unwrap();
    pb.write_planes(&CpuPlanes {
        width: W,
        height: H,
        format: PlaneFormat::Nv12Video,
        y: img.y,
        uv: img.uv,
    })
    .unwrap();
    let planes = lab.importer.import(&pb, ImportUsage::SAMPLE).unwrap();
    let (vy, vuv) = (
        planes.y.create_view(&Default::default()),
        planes.uv.create_view(&Default::default()),
    );
    let canvas_view = lab.comp.canvas().create_view(&Default::default());
    let n = 300;
    let t = std::time::Instant::now();
    for _ in 0..n {
        let gp: Vec<GpuPlanes<'_>> = draw
            .layers
            .iter()
            .map(|_| GpuPlanes {
                y: &vy,
                uv: &vuv,
                width: W,
                height: H,
                code_scale: 255.0,
                siting: ChromaSiting::Left,
            })
            .collect();
        let mut enc = lab.ctx.device.create_command_encoder(&Default::default());
        lab.comp
            .encode(&lab.ctx.queue, &mut enc, &draw, &gp)
            .unwrap();
        lab.ctx.queue.submit([enc.finish()]);
        lab.out.render(&canvas_view).unwrap();
    }
    lab.out.finish().unwrap();
    eprintln!(
        "H-05: 4-layer 1440p→1080x1920 composite + NV12 output: {:.0} fps",
        n as f64 / t.elapsed().as_secs_f64()
    );
}

/// H-06: text tiles drawn by the GPU compositor match the CPU premultiplied over within
/// 1 code value in NV12 (h79 parts 1–3: hook, pill and captions over real frames).
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal + ffmpeg + AJ's media and fonts (run with --features lab)"
)]
fn text_tiles_gpu_vs_cpu() {
    let mut lab = Lab::new();
    let h79 = plan("h79-parts-1-3.renderplan.json");
    let mut text = gmclip_render::compositor::text_layer::TextLayer::new(&h79).unwrap();
    let (mut worst, mut drawn) = (0u8, 0usize);
    for f in [0, 56, 70, 100, 150, 176, 177, 200, 300, 396] {
        let tiles = text.active(&h79, f).unwrap();
        drawn += tiles.len();
        let ((cy, cuv), (gy, guv)) = lab.render(&frame_draw(&h79, f).unwrap(), &tiles);
        let d = cy
            .iter()
            .zip(&gy)
            .chain(cuv.iter().zip(&guv))
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        worst = worst.max(d);
    }
    eprintln!("H-06: {drawn} tiles over 10 frames; GPU vs CPU max |Δ| {worst} code values");
    assert!(drawn >= 10);
    assert!(worst <= 1, "GPU vs CPU text: {worst}");
}
