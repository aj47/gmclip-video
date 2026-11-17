//! H-07: R'G'B' → NV12. CPU-reference checks run everywhere; the GPU path (pooled
//! IOSurface pixel buffers as render targets) is `lab`.

use gmclip_render::nv12::rgb_to_nv12_cpu;

#[test]
fn cpu_reference_known_values() {
    // White, black, red, green, blue, 50% grey: BT.709 limited range.
    let cases: [([f32; 3], u8, u8, u8); 6] = [
        ([1.0, 1.0, 1.0], 235, 128, 128),
        ([0.0, 0.0, 0.0], 16, 128, 128),
        ([1.0, 0.0, 0.0], 63, 102, 240),
        ([0.0, 1.0, 0.0], 173, 42, 26),
        ([0.0, 0.0, 1.0], 32, 240, 118),
        ([0.5, 0.5, 0.5], 126, 128, 128),
    ];
    for (rgb, y, cb, cr) in cases {
        let img = vec![rgb; 4 * 4];
        let (yp, uv) = rgb_to_nv12_cpu(&img, 4, 4);
        assert!(yp.iter().all(|&v| v == y), "{rgb:?} Y {} != {y}", yp[0]);
        assert_eq!((uv[0], uv[1]), (cb, cr), "{rgb:?}");
    }
}

#[test]
fn cpu_reference_chroma_is_left_cosited() {
    // A single bright column at x = 2 lands on chroma sample 1 with weight 1/2 and on
    // nothing else (taps of sample 0 are x = -1, 0, 1; of sample 1 are x = 1, 2, 3).
    let w = 8;
    let h = 2;
    let mut img = vec![[0.0f32; 3]; w * h];
    for y in 0..h {
        img[y * w + 2] = [0.0, 0.0, 1.0];
    }
    let (_, uv) = rgb_to_nv12_cpu(&img, w, h);
    let cb: Vec<u8> = uv.chunks(2).map(|c| c[0]).collect();
    assert_eq!(cb[0], 128);
    assert!(cb[1] > 128 && cb[2] == 128 && cb[3] == 128, "{cb:?}");
    // Odd column x = 1 contributes 1/4 to samples 0 and 1.
    let mut img = vec![[0.0f32; 3]; w * h];
    for y in 0..h {
        img[y * w + 1] = [0.0, 0.0, 1.0];
    }
    let (_, uv) = rgb_to_nv12_cpu(&img, w, h);
    assert_eq!(uv[0], uv[2]);
}

#[test]
fn cpu_reference_chroma_is_vertically_centred() {
    // A bright row 1 contributes half weight to chroma row 0 only (rows 0 and 1 are
    // averaged; row 2 starts chroma row 1).
    let (w, h) = (4, 4);
    let mut img = vec![[0.0f32; 3]; w * h];
    for x in 0..w {
        img[w + x] = [0.0, 0.0, 1.0];
    }
    let (_, uv) = rgb_to_nv12_cpu(&img, w, h);
    let cw = w / 2;
    let cb_row = |r: usize| uv[r * cw * 2];
    assert!(cb_row(0) > 128, "row 0 sees the bright row");
    assert_eq!(cb_row(1), 128, "row 1 does not");
    // Half weight: Cb of the averaged pixel equals Cb of 50% blue.
    let (_, half) = rgb_to_nv12_cpu(&[[0.0, 0.0, 0.5]], 1, 1);
    assert_eq!(cb_row(0), half[0]);
}

#[cfg(target_os = "macos")]
mod lab {
    use gmclip_gpu::{GpuContext, GpuOptions};
    use gmclip_render::nv12::apple::Nv12Output;
    use gmclip_render::nv12::rgb_to_nv12_cpu;

    fn f32_to_f16(v: f32) -> u16 {
        // Round-to-nearest-even for the normal and subnormal range used here (0..=1).
        let b = v.to_bits();
        let sign = ((b >> 16) & 0x8000) as u16;
        let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
        let man = b & 0x7f_ffff;
        if v == 0.0 {
            return sign;
        }
        if exp <= 0 {
            let m = (man | 0x80_0000) >> (1 - exp);
            let r = (m + 0x0fff + ((m >> 13) & 1)) >> 13;
            return sign | r as u16;
        }
        let r = man + 0x0fff + ((man >> 13) & 1);
        let (exp, r) = if r & 0x80_0000 != 0 {
            (exp + 1, 0)
        } else {
            (exp, r)
        };
        sign | ((exp as u16) << 10) | (r >> 13) as u16
    }

    fn f16_to_f32(h: u16) -> f32 {
        let exp = (h >> 10) & 0x1f;
        let man = (h & 0x3ff) as f32;
        let v = if exp == 0 {
            man / 1024.0 * 2f32.powi(-14)
        } else {
            (1.0 + man / 1024.0) * 2f32.powi(exp as i32 - 15)
        };
        if h & 0x8000 != 0 { -v } else { v }
    }

    /// Gradients, a hard-edged checkerboard, saturated bars and seeded noise; values
    /// slightly outside [0, 1] exercise the clamp.
    fn pattern(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut s = 0x1234_5678u32;
        let mut img = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let band = y * 4 / h;
                let c = match band {
                    0 => [x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32, 0.5],
                    1 => {
                        let on = ((x / 7) + (y / 5)) % 2 == 0;
                        if on {
                            [1.02, 0.9, 0.1]
                        } else {
                            [-0.01, 0.2, 0.95]
                        }
                    }
                    2 => {
                        const BARS: [[f32; 3]; 8] = [
                            [1.0, 1.0, 1.0],
                            [1.0, 1.0, 0.0],
                            [0.0, 1.0, 1.0],
                            [0.0, 1.0, 0.0],
                            [1.0, 0.0, 1.0],
                            [1.0, 0.0, 0.0],
                            [0.0, 0.0, 1.0],
                            [0.0, 0.0, 0.0],
                        ];
                        BARS[x * 8 / w]
                    }
                    _ => {
                        let mut n = || {
                            s ^= s << 13;
                            s ^= s >> 17;
                            s ^= s << 5;
                            (s >> 8) as f32 / (1u32 << 24) as f32
                        };
                        [n(), n(), n()]
                    }
                };
                img.push(c);
            }
        }
        img
    }

    fn upload(ctx: &GpuContext, img: &[[f32; 3]], w: u32, h: u32) -> wgpu::Texture {
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rgb16f"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = Vec::with_capacity(img.len() * 8);
        for c in img {
            for v in [c[0], c[1], c[2], 1.0] {
                bytes.extend_from_slice(&f32_to_f16(v).to_le_bytes());
            }
        }
        ctx.queue.write_texture(
            tex.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            tex.size(),
        );
        tex
    }

    #[test]
    #[cfg_attr(
        not(feature = "lab"),
        ignore = "lab: needs Metal (run with --features lab)"
    )]
    fn gpu_matches_cpu_within_one_code_value_and_fps() {
        let ctx = GpuContext::new(&GpuOptions::default()).unwrap();
        let (w, h) = (1080u32, 1920u32);
        let img = pattern(w as usize, h as usize);
        // The reference sees exactly what the GPU sees: the f16-rounded input.
        let img16: Vec<[f32; 3]> = img
            .iter()
            .map(|c| c.map(|v| f16_to_f32(f32_to_f16(v))))
            .collect();
        let (ry, ruv) = rgb_to_nv12_cpu(&img16, w as usize, h as usize);
        let src = upload(&ctx, &img, w, h);
        let view = src.create_view(&Default::default());
        let mut out = Nv12Output::new(&ctx, w, h, 4, 2).unwrap();
        let p = out.render_now(&view).unwrap().read_planes().unwrap();
        let diff = |a: &[u8], b: &[u8]| {
            let mut max = 0u8;
            let mut n = 0usize;
            let mut signed = 0i64;
            for (x, y) in a.iter().zip(b) {
                let d = x.abs_diff(*y);
                max = max.max(d);
                n += usize::from(d > 0);
                signed += i64::from(*x) - i64::from(*y);
            }
            (max, n, signed as f64 / a.len() as f64)
        };
        let (my, ny, sy) = diff(&p.y, &ry);
        let (muv, nuv, suv) = diff(&p.uv, &ruv);
        eprintln!(
            "H-07: {w}x{h} vs CPU reference: Y max |Δ| {my} ({ny} of {} differ, mean {sy:+.5}), CbCr max |Δ| {muv} ({nuv} of {} differ, mean {suv:+.5})",
            ry.len(),
            ruv.len()
        );
        assert!(my <= 1 && muv <= 1);
        // A systematic rounding error (e.g. truncation) also stays within 1 code value;
        // these catch it: almost every sample is exact and there is no bias.
        assert!(
            ny <= ry.len() / 10_000 && nuv <= ruv.len() / 10_000,
            "too many off-by-one samples"
        );
        assert!(sy.abs() < 0.001 && suv.abs() < 0.001, "biased");
        assert_eq!(p.y.len(), ry.len());
        assert_eq!(p.uv.len(), ruv.len());

        // Throughput: compositor-to-NV12 only, 4 in flight, alternating two sources so
        // a skipped frame would show up as the wrong picture.
        let img_b: Vec<[f32; 3]> = img16.iter().map(|c| [c[2], c[0], c[1]]).collect();
        let (by, buv) = rgb_to_nv12_cpu(&img_b, w as usize, h as usize);
        let src_b = upload(&ctx, &img_b, w, h);
        let view_b = src_b.create_view(&Default::default());
        // Warm-up (shader and clock ramp; the first run after a build is slow).
        for _ in 0..60 {
            out.render(&view).unwrap();
        }
        out.finish().unwrap();
        let n = 600usize;
        let mut got: Vec<gmclip_gpu::apple::PixelBuffer> = Vec::new();
        let t = std::time::Instant::now();
        for i in 0..n {
            let v = if i % 2 == 0 { &view } else { &view_b };
            if let Some(b) = out.render(v).unwrap()
                && got.len() < 2
            {
                got.push(b); // keep two (bounded by downstream_hold = 2)
            }
        }
        out.finish().unwrap();
        let fps = n as f64 / t.elapsed().as_secs_f64();
        eprintln!(
            "H-07: {w}x{h} NV12 output {fps:.0} fps (4 in flight, pool buffers as render targets)"
        );
        for (k, b) in got.iter().enumerate() {
            let pl = b.read_planes().unwrap();
            let (ey, euv) = if k % 2 == 0 { (&ry, &ruv) } else { (&by, &buv) };
            assert!(
                diff(&pl.y, ey).0 <= 1 && diff(&pl.uv, euv).0 <= 1,
                "frame {k} is not its own picture"
            );
        }
        // The pool is bounded: holding everything exhausts it instead of growing.
        let mut held = got;
        let exhausted = (0..32).any(|_| match out.render(&view) {
            Ok(Some(b)) => {
                held.push(b);
                false
            }
            Ok(None) => false,
            Err(gmclip_gpu::GpuError::PoolExhausted(_)) => true,
            Err(e) => panic!("{e}"),
        });
        assert!(exhausted, "pool must apply back-pressure");
        assert!(fps >= 240.0, "H-07 needs >= 240 fps, got {fps:.0}");
    }
}
