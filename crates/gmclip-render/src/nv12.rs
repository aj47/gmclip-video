//! R'G'B' → NV12 output (H-07, SPEC §6.5 step 2, §7 "Output BT.709 limited").
//!
//! The compositor's RGBA16F result (premultiplied, composited over an opaque
//! background, compat BT.709 R'G'B') becomes two planes in two render passes:
//! - **Y** (full size, R8Unorm): `Y = 16 + 219·(Kr·R' + Kg·G' + Kb·B')`.
//! - **CbCr** (half size, Rg8Unorm): chroma **left-cosited** horizontally (MPEG-2 /
//!   H.264 default `chroma_loc_type 0`): each chroma sample sits on even luma column
//!   `2x` and filters columns `2x−1, 2x, 2x+1` with `[1 2 1]/4`; vertically it sits
//!   between rows `2y` and `2y+1` and averages them. Edges clamp.
//!
//! Every input sample is clamped to [0, 1] before filtering, so the GPU and the CPU
//! reference ([`rgb_to_nv12_cpu`]) compute the same expression; they differ only in
//! float rounding (≤ 1 code value, the H-07 bound).
//!
//! On macOS the planes are a pooled IOSurface `CVPixelBuffer` imported as render
//! targets ([`apple::Nv12Output`]), so the encoder reads the GPU's output with no copy.
//! The S3/H-07 probe on Apple M1 Pro: the texture cache grants render-target usage
//! (`MTLTextureUsage` 0x5) on 8- and 16-bit planes, so the compute-write fallback is
//! not needed there.

/// Luma coefficients of BT.709.
pub const KR: f64 = 0.2126;
pub const KB: f64 = 0.0722;
pub const KG: f64 = 1.0 - KR - KB;

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;

const KR: f32 = 0.2126;
const KB: f32 = 0.0722;
const KG: f32 = 0.7152;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn rgb(p: vec2<i32>) -> vec3<f32> {
    let d = vec2<i32>(textureDimensions(src));
    let q = clamp(p, vec2<i32>(0, 0), d - vec2<i32>(1, 1));
    return clamp(textureLoad(src, q, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_y(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = rgb(vec2<i32>(pos.xy));
    let y = KR * c.r + KG * c.g + KB * c.b;
    return vec4<f32>((16.0 + 219.0 * y) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_uv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = 2 * i32(pos.x);
    let y = 2 * i32(pos.y);
    var acc = vec3<f32>(0.0);
    for (var r = 0; r < 2; r++) {
        acc += 0.25 * rgb(vec2<i32>(x - 1, y + r))
             + 0.5 * rgb(vec2<i32>(x, y + r))
             + 0.25 * rgb(vec2<i32>(x + 1, y + r));
    }
    acc = acc * 0.5;
    let yv = KR * acc.r + KG * acc.g + KB * acc.b;
    let cb = (acc.b - yv) / (2.0 * (1.0 - KB));
    let cr = (acc.r - yv) / (2.0 * (1.0 - KR));
    return vec4<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0, 0.0, 1.0);
}
"#;

/// The two render pipelines. Device-agnostic: targets are any R8Unorm / Rg8Unorm
/// views of the right sizes.
#[derive(Debug)]
pub struct Nv12Converter {
    y: wgpu::RenderPipeline,
    uv: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl Nv12Converter {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let make = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            y: make("fs_y", wgpu::TextureFormat::R8Unorm),
            uv: make("fs_uv", wgpu::TextureFormat::Rg8Unorm),
            layout,
        }
    }

    /// Records the Y and CbCr passes. `src` is the R'G'B' frame (any float format,
    /// same size as `y`); `uv` is half size, rounded up.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        y: &wgpu::TextureView,
        uv: &wgpu::TextureView,
    ) {
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            }],
        });
        for (pipeline, target) in [(&self.y, y), (&self.uv, uv)] {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// CPU reference of the same conversion, from straight R'G'B' in [0, 1] (row-major,
/// `w × h`). Returns `(Y, interleaved CbCr)` tightly packed.
pub fn rgb_to_nv12_cpu(rgb: &[[f32; 3]], w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
    let px = |x: isize, y: isize| -> [f64; 3] {
        let xc = x.clamp(0, w as isize - 1) as usize;
        let yc = y.clamp(0, h as isize - 1) as usize;
        let c = rgb[yc * w + xc];
        [
            f64::from(c[0]).clamp(0.0, 1.0),
            f64::from(c[1]).clamp(0.0, 1.0),
            f64::from(c[2]).clamp(0.0, 1.0),
        ]
    };
    let code = |v: f64| (v.clamp(0.0, 255.0)).round() as u8;
    let mut y_plane = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = px(x as isize, y as isize);
            let l = KR * c[0] + KG * c[1] + KB * c[2];
            y_plane[y * w + x] = code(16.0 + 219.0 * l);
        }
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut uv = vec![0u8; cw * ch * 2];
    for cy in 0..ch {
        for cx in 0..cw {
            let (x, y) = (2 * cx as isize, 2 * cy as isize);
            let mut acc = [0f64; 3];
            for r in 0..2 {
                for (dx, wgt) in [(-1, 0.25), (0, 0.5), (1, 0.25)] {
                    let c = px(x + dx, y + r);
                    for k in 0..3 {
                        acc[k] += wgt * c[k];
                    }
                }
            }
            let acc = acc.map(|v| v * 0.5);
            let l = KR * acc[0] + KG * acc[1] + KB * acc[2];
            let cb = (acc[2] - l) / (2.0 * (1.0 - KB));
            let cr = (acc[0] - l) / (2.0 * (1.0 - KR));
            uv[(cy * cw + cx) * 2] = code(128.0 + 224.0 * cb);
            uv[(cy * cw + cx) * 2 + 1] = code(128.0 + 224.0 * cr);
        }
    }
    (y_plane, uv)
}

/// The macOS output: pooled IOSurface `CVPixelBuffer`s rendered as NV12 planes, with a
/// bounded number of frames in flight.
#[cfg(target_os = "macos")]
pub mod apple {
    use std::collections::VecDeque;

    use gmclip_gpu::GpuError;
    use gmclip_gpu::apple::{
        ImportUsage, MetalImporter, PixelBuffer, PixelBufferPool, PlaneFormat,
    };

    use super::Nv12Converter;

    /// A frame whose GPU work was submitted but may not have finished.
    #[derive(Debug)]
    struct Pending {
        buffer: PixelBuffer,
        submission: wgpu::SubmissionIndex,
    }

    /// Converts R'G'B' frames into pooled NV12 pixel buffers.
    ///
    /// A buffer is handed out only after its GPU work completed: [`Self::render`]
    /// queues up to `max_in_flight` frames and returns the oldest finished one once
    /// the queue is full; [`Self::finish`] returns the rest. The pool is bounded
    /// (`max_in_flight` + `downstream_hold` + 4 buffers, counting buffers whose last
    /// GPU use wgpu has not yet retired); when every buffer is out,
    /// [`Self::render`] fails with [`GpuError::PoolExhausted`] (back-pressure: the
    /// encoder or the caller must release frames).
    #[derive(Debug)]
    pub struct Nv12Output {
        device: wgpu::Device,
        queue: wgpu::Queue,
        conv: Nv12Converter,
        importer: MetalImporter,
        pool: PixelBufferPool,
        pending: VecDeque<Pending>,
        max_in_flight: usize,
    }

    impl Nv12Output {
        /// `max_in_flight` is 3–4 (SPEC §6.5); `downstream_hold` is how many finished
        /// buffers the consumer (encoder) may keep at once.
        pub fn new(
            ctx: &gmclip_gpu::GpuContext,
            width: u32,
            height: u32,
            max_in_flight: usize,
            downstream_hold: usize,
        ) -> Result<Self, GpuError> {
            let max_in_flight = max_in_flight.clamp(1, 8);
            let cap = (max_in_flight + downstream_hold + 4) as u32;
            let pool = PixelBufferPool::new(width, height, PlaneFormat::Nv12Video, cap)?
                .with_threshold(cap);
            let importer = MetalImporter::new(ctx)?;
            // Fail here, not on the first frame, if this Mac cannot render into pool
            // planes (the compute-write fallback is not built: H-07 probe).
            importer.import(&pool.get()?, ImportUsage::RENDER)?;
            Ok(Self {
                device: ctx.device.clone(),
                queue: ctx.queue.clone(),
                conv: Nv12Converter::new(&ctx.device),
                importer,
                pool,
                pending: VecDeque::new(),
                max_in_flight,
            })
        }

        fn wait(&self, s: &wgpu::SubmissionIndex) -> Result<(), GpuError> {
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(s.clone()),
                    timeout: None,
                })
                .map(|_| ())
                .map_err(|e| GpuError::Device(e.to_string()))
        }

        /// Renders `src` (the canvas: same size as the output, a non-sRGB float
        /// format) into a pool buffer. Returns the oldest finished buffer once
        /// `max_in_flight` frames are queued.
        pub fn render(&mut self, src: &wgpu::TextureView) -> Result<Option<PixelBuffer>, GpuError> {
            let (w, h) = self.pool.size();
            let t = src.texture();
            if (t.width(), t.height()) != (w, h) || t.format().is_srgb() {
                return Err(GpuError::Unsupported(format!(
                    "NV12 source {}x{} {:?}, output {w}x{h}",
                    t.width(),
                    t.height(),
                    t.format()
                )));
            }
            let buffer = match self.pool.get() {
                Err(GpuError::PoolExhausted(_)) => {
                    // Buffers the caller already dropped come back only when wgpu
                    // retires their textures (the import keepalive) and the texture
                    // cache lets go: do that housekeeping, then try once more.
                    self.device
                        .poll(wgpu::PollType::Poll)
                        .map_err(|e| GpuError::Device(e.to_string()))?;
                    self.importer.flush();
                    self.pool.get()?
                }
                other => other?,
            };
            let planes = self.importer.import(&buffer, ImportUsage::RENDER)?;
            let vy = planes.y.create_view(&Default::default());
            let vuv = planes.uv.create_view(&Default::default());
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.conv.encode(&self.device, &mut enc, src, &vy, &vuv);
            let submission = self.queue.submit([enc.finish()]);
            self.importer.flush();
            self.pending.push_back(Pending { buffer, submission });
            if self.pending.len() > self.max_in_flight {
                let p = self.pending.pop_front().expect("queue is non-empty");
                self.wait(&p.submission)?;
                return Ok(Some(p.buffer));
            }
            Ok(None)
        }

        /// Waits for everything queued and returns those buffers, oldest first.
        pub fn finish(&mut self) -> Result<Vec<PixelBuffer>, GpuError> {
            let mut out = Vec::with_capacity(self.pending.len());
            while let Some(p) = self.pending.pop_front() {
                self.wait(&p.submission)?;
                out.push(p.buffer);
            }
            Ok(out)
        }

        /// One frame, synchronously (stills, tests). Errors if frames are queued.
        pub fn render_now(&mut self, src: &wgpu::TextureView) -> Result<PixelBuffer, GpuError> {
            if !self.pending.is_empty() {
                return Err(GpuError::Unsupported(
                    "render_now with frames queued".into(),
                ));
            }
            self.render(src)?;
            self.finish()?
                .pop()
                .ok_or_else(|| GpuError::Device("no frame".into()))
        }
    }
}
