//! H-01 acceptance (lab: needs Metal). Run under the Metal validation layer with
//! `MTL_DEBUG_LAYER=1 cargo test -p gmclip-gpu --features lab --release -- --test-threads=1`.
#![cfg(target_os = "macos")]

use gmclip_gpu::apple::{
    CpuPlanes, ImportUsage, MetalImporter, PixelBuffer, PixelBufferPool, PlaneFormat,
    PlaneTextures, live_keepalives, phys_footprint,
};
use gmclip_gpu::{GpuContext, GpuOptions};

const LAB: &str = "lab: needs Metal (run with --features lab)";

fn pattern(w: u32, h: u32, format: PlaneFormat, seed: u32) -> CpuPlanes {
    let mut p = CpuPlanes::new(w, h, format);
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let mask: u16 = if format == PlaneFormat::P010Video {
        0xFFC0
    } else {
        0xFF
    };
    for plane in [&mut p.y, &mut p.uv] {
        if format.bytes_per_sample() == 2 {
            for c in plane.chunks_exact_mut(2) {
                let v = (next() as u16) & mask;
                c.copy_from_slice(&v.to_le_bytes());
            }
        } else {
            for b in plane.iter_mut() {
                *b = next() as u8;
            }
        }
    }
    p
}

/// Copies one plane texture back to the CPU, tightly packed.
fn read_texture(ctx: &GpuContext, t: &wgpu::Texture, bytes_per_texel: u32) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let row = w * bytes_per_texel;
    let padded = row.div_ceil(256) * 256;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        t.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        t.size(),
    );
    ctx.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    ctx.wait_idle().unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((row * h) as usize);
    for r in 0..h as usize {
        out.extend_from_slice(&data[r * padded as usize..r * padded as usize + row as usize]);
    }
    out
}

fn check_roundtrip(format: PlaneFormat, w: u32, h: u32) {
    let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
    if format == PlaneFormat::P010Video && !ctx.norm16 {
        panic!("adapter lacks TEXTURE_FORMAT_16BIT_NORM");
    }
    let imp = MetalImporter::new(&ctx).unwrap();
    let mut pb = PixelBuffer::new(w, h, format).unwrap();
    assert!(pb.is_iosurface_backed());
    let pat = pattern(w, h, format, 7);
    pb.write_planes(&pat).unwrap();
    assert_eq!(pb.read_planes().unwrap(), pat, "CPU plane round trip");
    let planes = imp.import(&pb, ImportUsage::SAMPLE).unwrap();
    drop(pb); // the drop callback keeps the buffer alive
    let bps = format.bytes_per_sample() as u32;
    let y = read_texture(&ctx, &planes.y, bps);
    let uv = read_texture(&ctx, &planes.uv, 2 * bps);
    assert_eq!(y, pat.y, "Y plane via Metal");
    assert_eq!(uv, pat.uv, "UV plane via Metal");
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn nv12_planes_import_bit_exact() {
    let _ = LAB;
    check_roundtrip(PlaneFormat::Nv12Video, 1920, 1080);
    check_roundtrip(PlaneFormat::Nv12Video, 2560, 1440);
    check_roundtrip(PlaneFormat::Nv12Video, 1080, 1920);
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn p010_planes_import_bit_exact() {
    check_roundtrip(PlaneFormat::P010Video, 1920, 1080);
}

const SHADER: &str = r#"
@group(0) @binding(0) var ty: texture_2d<f32>;
@group(0) @binding(1) var tuv: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32, 256>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    let dy = textureDimensions(ty);
    let dc = textureDimensions(tuv);
    let y = textureLoad(ty, vec2<u32>((i * 7u) % dy.x, i % dy.y), 0).r;
    let c = textureLoad(tuv, vec2<u32>(i % dc.x, (i * 3u) % dc.y), 0).rg;
    out[i] = y + c.x + c.y;
}
"#;

/// 10,000 import → GPU sample → drop cycles; `phys_footprint` must not move more than
/// 10 MB between the end of warm-up and the end of the run.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn import_drop_10k_cycles_footprint_stable() {
    let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
    let imp = MetalImporter::new(&ctx).unwrap();
    let pool = PixelBufferPool::new(1920, 1080, PlaneFormat::Nv12Video, 6).unwrap();
    // Distinct contents in the pool's buffers.
    {
        let mut held: Vec<_> = (0..6).map(|_| pool.get().unwrap()).collect();
        for (i, pb) in held.iter_mut().enumerate() {
            pb.write_planes(&pattern(1920, 1080, PlaneFormat::Nv12Video, i as u32))
                .unwrap();
        }
    }
    let module = ctx
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
    let pipeline = ctx
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let out = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256 * 4,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let cycle = |planes: &PlaneTextures| {
        let vy = planes.y.create_view(&Default::default());
        let vuv = planes.uv.create_view(&Default::default());
        let bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&vy),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&vuv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut enc = ctx.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(4, 1, 1);
        }
        ctx.queue.submit([enc.finish()]);
    };
    let run = |n: usize| {
        for i in 0..n {
            let pb = pool.get().unwrap();
            let planes = imp.import(&pb, ImportUsage::SAMPLE).unwrap();
            drop(pb);
            cycle(&planes);
            drop(planes);
            if i % 8 == 7 {
                ctx.wait_idle().unwrap();
                imp.flush();
            }
        }
        ctx.wait_idle().unwrap();
        imp.flush();
    };
    run(1_000);
    let f0 = phys_footprint().unwrap();
    let t = std::time::Instant::now();
    run(10_000);
    let dt = t.elapsed();
    let f1 = phys_footprint().unwrap();
    let delta = f1 as i64 - f0 as i64;
    eprintln!(
        "H-01: 10,000 cycles in {:.2?} ({:.1} µs/cycle); phys_footprint {:.1} MB -> {:.1} MB (Δ {:+.2} MB)",
        dt,
        dt.as_secs_f64() * 1e6 / 10_000.0,
        f0 as f64 / 1048576.0,
        f1 as f64 / 1048576.0,
        delta as f64 / 1048576.0
    );
    // The Metal validation layers allocate on their own; the footprint bound is measured
    // without them (the validation run checks only that no assertion fires).
    let validating = ["MTL_DEBUG_LAYER", "MTL_SHADER_VALIDATION"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| v != "0"));
    if !validating {
        assert!(
            delta.abs() <= 10 * 1024 * 1024,
            "phys_footprint moved {delta} bytes"
        );
    }
}

/// The drop callback — not the Rust handle — decides when the CVMetalTexture and the
/// pixel buffer go: still alive after the textures are dropped with GPU work queued,
/// released once wgpu retires the work.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn keepalive_outlives_handle_until_gpu_retires() {
    let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
    let imp = MetalImporter::new(&ctx).unwrap();
    ctx.wait_idle().unwrap();
    let base = live_keepalives();
    let mut pb = PixelBuffer::new(1920, 1080, PlaneFormat::Nv12Video).unwrap();
    pb.write_planes(&pattern(1920, 1080, PlaneFormat::Nv12Video, 3))
        .unwrap();
    let planes = imp.import(&pb, ImportUsage::SAMPLE).unwrap();
    drop(pb);
    assert_eq!(live_keepalives(), base + 2, "two planes imported");
    // Queue GPU work that reads both planes, then drop the handles without polling.
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 1920 * 1080,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = ctx.device.create_command_encoder(&Default::default());
    for t in [&planes.y, &planes.uv] {
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(2048),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: t.width(),
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
    ctx.queue.submit([enc.finish()]);
    drop(planes);
    assert_eq!(
        live_keepalives(),
        base + 2,
        "keepalives must survive the handle drop while the submission is in flight"
    );
    ctx.wait_idle().unwrap();
    assert_eq!(
        live_keepalives(),
        base,
        "released once wgpu retired the submission"
    );
}

/// Render-target and storage imports (H-07's output planes): granted by the cache and
/// usable by wgpu, or refused up front — never a texture that fails at first use.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs Metal (run with --features lab)"
)]
fn render_and_storage_imports() {
    let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
    let imp = MetalImporter::new(&ctx).unwrap();
    for format in [PlaneFormat::Nv12Video, PlaneFormat::P010Video] {
        for (name, usage) in [
            ("render", ImportUsage::RENDER),
            ("storage", ImportUsage::STORAGE),
        ] {
            let pb = PixelBuffer::new(1080, 1920, format).unwrap();
            match imp.import(&pb, usage) {
                Ok(planes) => {
                    if usage.render_target {
                        let mut enc = ctx.device.create_command_encoder(&Default::default());
                        for t in [&planes.y, &planes.uv] {
                            let v = t.create_view(&Default::default());
                            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: None,
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &v,
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 0.5,
                                            g: 0.25,
                                            b: 0.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                ..Default::default()
                            });
                        }
                        ctx.queue.submit([enc.finish()]);
                        ctx.wait_idle().unwrap();
                        let p = pb.read_planes().unwrap();
                        if format == PlaneFormat::Nv12Video {
                            assert_eq!(p.y[0], 128, "clear 0.5 → 128");
                            assert_eq!(&p.uv[0..2], &[128, 64]);
                        }
                    }
                    eprintln!(
                        "H-01 probe: {format:?} {name}: granted {:#x?}",
                        planes.granted_usage
                    );
                }
                Err(e) => eprintln!("H-01 probe: {format:?} {name}: refused: {e}"),
            }
        }
    }
}
