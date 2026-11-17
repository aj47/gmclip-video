//! The preview path (SPEC D4/D5, §6.3 frame stream; lane U): the export's per-frame
//! compositor at the **displayed** size, plus gizmos drawn in the same pass, read back
//! as NV12 for the frame socket.
//!
//! One session per open viewer. It keeps the export's decoders (a forward
//! [`FrameStream`] per source for live layers, a seeking reader for held ones), so
//! playback decodes sequentially and a scrub seeks. Layer `place` rectangles are
//! scaled from the plan canvas to the display size before compositing, so Lanczos
//! sees the true downscale; at display size = canvas size with no gizmos and no
//! overrides this is the export's frame (R-COMP-1's path): both composite through the
//! same [`super::compose::FrameComposer::compose`] (H.04), so they cannot drift.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_gpu::{GpuContext, GpuOptions};
use gmclip_media::backend::seek::seek_start;
use gmclip_media::decode::DecodedFrame;
use gmclip_media::index::SampleIdx;
use gmclip_plan::Plan;

use super::compose::{CropScale, FrameComposer, FramePictures};
pub use super::compose::{LayerOverride, TextShift};
use super::os::Nv12Output;
use super::proxy::ProxyReader;
use super::{Pictures, failed, gpu};
use crate::compositor::gpu::GpuCompositor;
use crate::compositor::{ChromaSiting, LayerDraw, frame_draw_with};
use crate::export::Inputs;
use crate::ops::RenderError;

/// A solid quad drawn over the composite (gizmo outlines and handles), in display
/// pixels, premultiplied-alpha colour.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GizmoQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rgba: [f32; 4],
}

/// One preview frame as NV12 planes, tightly packed.
#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

/// Draws gizmo quads over the canvas (premultiplied over, RGBA16F).
#[derive(Debug)]
struct GizmoPass {
    pipe: wgpu::RenderPipeline,
    /// Reused vertex buffer, grown when a frame needs more.
    buf: Option<wgpu::Buffer>,
}

const GIZMO_SHADER: &str = r#"
struct V { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> V {
    var o: V; o.pos = vec4<f32>(p, 0.0, 1.0); o.c = c; return o;
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> { return v.c; }
"#;

impl GizmoPass {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gizmo"),
            source: wgpu::ShaderSource::Wgsl(GIZMO_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gizmo"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gizmo"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 24,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: GpuCompositor::CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: over,
                        alpha: over,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipe, buf: None }
    }

    fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        quads: &[GizmoQuad],
    ) {
        if quads.is_empty() {
            return;
        }
        let (w, h) = (size.0 as f32, size.1 as f32);
        let mut v: Vec<f32> = Vec::with_capacity(quads.len() * 36);
        for q in quads {
            let (x0, x1) = (q.x / w * 2.0 - 1.0, (q.x + q.w) / w * 2.0 - 1.0);
            let (y0, y1) = (1.0 - q.y / h * 2.0, 1.0 - (q.y + q.h) / h * 2.0);
            for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y0), (x1, y1), (x0, y1)] {
                v.extend_from_slice(&[x, y]);
                v.extend_from_slice(&q.rgba);
            }
        }
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        let need = bytes.len() as u64;
        if self.buf.as_ref().is_none_or(|b| b.size() < need) {
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gizmo-quads"),
                size: need.next_power_of_two().max(4096),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let Some(buf) = self.buf.as_ref() else { return };
        queue.write_buffer(buf, 0, &bytes);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gizmo"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_vertex_buffer(0, buf.slice(..need));
        pass.draw(0..(quads.len() as u32 * 6), 0..1);
    }
}

/// Where a frame's pictures came from (the viewer reports the worst of its layers).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScrubPath {
    /// Frame-exact from the master (playback, paused frames).
    Exact,
    /// The scrub proxy (H.05: ≤ 640 px, GOP 4).
    Proxy,
    /// The master's keyframe at or before the frame (the proxy does not cover it yet).
    Keyframe,
}

impl ScrubPath {
    pub fn as_str(self) -> &'static str {
        match self {
            ScrubPath::Exact => "exact",
            ScrubPath::Proxy => "proxy",
            ScrubPath::Keyframe => "keyframe",
        }
    }
}

/// A scrub picture, its crop scale and where it came from.
type ScrubPicture = (Arc<DecodedFrame>, CropScale, ScrubPath);

/// A source's proxy reader, or when it was last looked for.
enum ProxySlot {
    Open(Box<ProxyReader>),
    Missing(Instant),
}

/// Spare decode streams per source: playback prerolls up to this many joins ahead.
const SPARES: usize = 2;
/// How far ahead (output frames) a join is prerolled: 1.5 s at 30 fps, plus a frame per
/// 4 samples its decode chain needs (a long GOP starts earlier), at most [`MAX_AHEAD`].
/// Measured on BENCH-60-M0 and h79: a flat 3 s keeps more decode sessions in flight at
/// once and loses (the hardware decoder is shared).
const AHEAD: u64 = 45;
const MAX_AHEAD: u64 = 150;

/// A preview of one plan at one display size.
pub struct PreviewSession {
    ctx: GpuContext,
    plan: Arc<Plan>,
    /// The export's per-frame compose (H.04), at the display size.
    composer: FrameComposer,
    src: PreviewPictures,
    nv12: Nv12Output,
    gizmo: GizmoPass,
    size: (u32, u32),
}

/// The preview's pictures: the export's decoders (with spares for prerolled joins), and
/// for scrubs the scrub proxy or the master's keyframes.
struct PreviewPictures {
    pictures: Pictures,
    /// Scrub proxy folders by source path (from the app's proxy manager).
    proxy_dirs: HashMap<String, PathBuf>,
    /// Proxy readers by plan source id.
    proxies: HashMap<String, ProxySlot>,
    /// The last keyframe decoded for a scrub, per source: `(sync sample, picture)`.
    keyframes: HashMap<String, (usize, Arc<DecodedFrame>)>,
}

/// One frame's picture source: exact, or a scrub's (and the worst path it took).
struct PreviewFetch<'a> {
    src: &'a mut PreviewPictures,
    plan: &'a Plan,
    scrub: bool,
    path: ScrubPath,
}

impl FramePictures for PreviewFetch<'_> {
    fn part_changed(&mut self) {
        self.src.pictures.stills.clear();
    }

    fn picture(
        &mut self,
        l: &LayerDraw,
        f: u64,
    ) -> Result<(Arc<DecodedFrame>, CropScale), RenderError> {
        if self.scrub && !l.still {
            let (pic, sc, p) = self.src.scrub_picture(self.plan, &l.source, l.frame, f)?;
            self.path = self.path.max(p);
            Ok((pic, sc))
        } else {
            Ok((
                self.src
                    .pictures
                    .get(&l.stream(), &l.source, l.frame, l.still, f)?,
                None,
            ))
        }
    }

    fn siting(&self, source: &str) -> ChromaSiting {
        self.src
            .pictures
            .meta
            .get(source)
            .map_or(ChromaSiting::Left, |m| m.2)
    }

    fn source_size(&self, source: &str) -> (u32, u32) {
        self.src
            .pictures
            .meta
            .get(source)
            .map_or((0, 0), |m| (m.0, m.1))
    }

    fn clocks(&self) -> &dyn crate::compositor::SourceClocks {
        &self.src.pictures
    }
}

impl std::fmt::Debug for PreviewSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewSession")
            .field("size", &self.size)
            .field("frames", &self.plan.output.frames)
            .finish_non_exhaustive()
    }
}

fn even(v: u32) -> u32 {
    (v & !1).max(2)
}

/// The display size for a requested box: the plan's aspect, even sides (rounded down),
/// never larger than the box or the canvas.
pub fn display_size(plan: &Plan, max_w: u32, max_h: u32) -> (u32, u32) {
    let (cw, ch) = (
        plan.output.canvas.w.get() as f64,
        plan.output.canvas.h.get() as f64,
    );
    let s = (f64::from(max_w.max(2)) / cw)
        .min(f64::from(max_h.max(2)) / ch)
        .min(1.0);
    (even((cw * s).round() as u32), even((ch * s).round() as u32))
}

impl PreviewPictures {
    /// The proxy reader for plan source `id`, opened on first use (a folder without a
    /// proxy yet is looked at again after a second).
    fn proxy(&mut self, plan: &Plan, id: &str) -> Option<&mut ProxyReader> {
        let retry = match self.proxies.get(id) {
            Some(ProxySlot::Open(_)) => false,
            Some(ProxySlot::Missing(at)) => at.elapsed() >= Duration::from_secs(1),
            None => true,
        };
        if retry {
            let path = plan
                .sources
                .iter()
                .find(|s| s.id.as_str() == id)
                .map(|s| s.path.clone());
            let slot = match path.and_then(|p| self.proxy_dirs.get(&p)) {
                Some(dir) => match ProxyReader::open(dir) {
                    Some(r) => ProxySlot::Open(Box::new(r)),
                    None => ProxySlot::Missing(Instant::now()),
                },
                None => ProxySlot::Missing(Instant::now()),
            };
            self.proxies.insert(id.to_owned(), slot);
        }
        match self.proxies.get_mut(id) {
            Some(ProxySlot::Open(r)) => Some(r),
            _ => None,
        }
    }

    /// A scrub picture of `(source, frame)`: the proxy's when it covers the frame (with
    /// the crop scale from source to proxy pixels), else the master's keyframe at or
    /// before it.
    fn scrub_picture(
        &mut self,
        plan: &Plan,
        source: &str,
        frame: u64,
        out_frame: u64,
    ) -> Result<ScrubPicture, RenderError> {
        let src_size = self.pictures.meta.get(source).map(|m| (m.0, m.1));
        if let Some(r) = self.proxy(plan, source)
            && r.covers(frame)
        {
            let (pw, ph) = r.size();
            let m = r.manifest();
            let (sw, sh) = src_size.unwrap_or((m.source_w, m.source_h));
            if let Ok(pic) = r.frame(frame) {
                let scale = (
                    f64::from(pw) / f64::from(sw.max(1)),
                    f64::from(ph) / f64::from(sh.max(1)),
                );
                return Ok((pic, Some(scale), ScrubPath::Proxy));
            }
        }
        let r = self
            .pictures
            .still_readers
            .get_mut(source)
            .ok_or_else(|| failed("compose", Some(out_frame), "unknown source"))?;
        let idx = r.source().index();
        let t = *idx
            .pts_order
            .get(frame as usize)
            .ok_or_else(|| failed("decode", Some(out_frame), "frame past the source"))?;
        let k = seek_start(idx, SampleIdx(t))
            .ok_or_else(|| failed("decode", Some(out_frame), "no keyframe before the frame"))?;
        if let Some((kk, pic)) = self.keyframes.get(source)
            && *kk == k
        {
            return Ok((pic.clone(), None, ScrubPath::Keyframe));
        }
        let pts = idx.dts[k] + i64::from(idx.cts_off[k]);
        let pic = Arc::new(r.frame_at_pts(pts).map_err(|mut e| {
            e.source_id = Some(source.to_owned());
            RenderError::Media(e)
        })?);
        self.keyframes.insert(source.to_owned(), (k, pic.clone()));
        Ok((pic, None, ScrubPath::Keyframe))
    }
}

impl PreviewSession {
    /// Opens decoders for every source the plan shows and a compositor at `size`
    /// (clamped by [`display_size`]).
    pub fn open(
        plan: Arc<Plan>,
        inputs: &mut dyn Inputs,
        size: (u32, u32),
    ) -> Result<Self, RenderError> {
        let ctx = GpuContext::new(&GpuOptions::default()).map_err(gpu("gpu_device", None))?;
        let size = display_size(&plan, size.0, size.1);
        let pictures = Pictures::open_with_spares(&plan, inputs, SPARES)?;
        let composer = FrameComposer::new(&ctx, &plan, size)?;
        let nv12 = Nv12Output::new(&ctx, size.0, size.1, 1, 1).map_err(gpu("nv12_output", None))?;
        let gizmo = GizmoPass::new(&ctx.device);
        Ok(Self {
            ctx,
            plan,
            composer,
            src: PreviewPictures {
                pictures,
                proxy_dirs: HashMap::new(),
                proxies: HashMap::new(),
                keyframes: HashMap::new(),
            },
            nv12,
            gizmo,
            size,
        })
    }

    /// The roto mask-track cache (SPEC §5.8): matted layers draw its accurate tracks
    /// where they exist, else a live draft (see [`Self::drew_draft_matte`]).
    pub fn set_matte_store(&mut self, store: Arc<crate::matte::store::MatteStore>) {
        self.composer.set_mattes(super::mattes::Mattes::new(
            store,
            super::mattes::DraftPolicy::Allow,
        ));
    }

    /// Whether the last rendered frame drew a draft matte (the viewer's "draft matte"
    /// badge, SPEC §5.8).
    pub fn drew_draft_matte(&self) -> bool {
        self.composer.drew_draft()
    }

    /// Text moves the next frames draw (U.12: a text overlay or the caption seat dragged
    /// on the viewer previews in the frame itself; empty = none). Preview only.
    pub fn set_text_shifts(&mut self, shifts: Vec<TextShift>) {
        self.composer.set_text_shifts(shifts);
    }

    /// Matte view (SPEC §5.8 "matte view"): the next frames show matted layers' masks in
    /// white on black, so a bad cut-out is easy to see. Preview only.
    pub fn set_matte_view(&mut self, on: bool) {
        self.composer.set_matte_view(on);
    }

    /// The scrub proxy folders, by source path (H.05). Sources without one scrub on
    /// their keyframes.
    pub fn set_proxy_dirs(&mut self, dirs: HashMap<String, PathBuf>) {
        if dirs != self.src.proxy_dirs {
            self.src.proxy_dirs = dirs;
            self.src.proxies.clear();
        }
    }

    pub fn plan(&self) -> &Arc<Plan> {
        &self.plan
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Swaps in an edited plan. Decoders are kept when the sources are the same
    /// (an edit that only trims or moves layers costs no re-open).
    pub fn set_plan(
        &mut self,
        plan: Arc<Plan>,
        inputs: &mut dyn Inputs,
    ) -> Result<(), RenderError> {
        // the same files read on the same decode streams (a new backdrop or clip event of a
        // source already open needs its own stream, decision 0028)
        let same = plan.sources.len() == self.plan.sources.len()
            && plan
                .sources
                .iter()
                .zip(&self.plan.sources)
                .all(|(a, b)| a.id == b.id && a.path == b.path)
            && crate::compositor::plan_streams(&plan)
                == crate::compositor::plan_streams(&self.plan);
        if !same {
            self.src.pictures = Pictures::open_with_spares(&plan, inputs, SPARES)?;
            self.src.proxies.clear();
            self.src.keyframes.clear();
        }
        self.src.pictures.stills.clear();
        self.composer.set_plan(&self.plan, &plan)?;
        let keep = self.size;
        self.plan = plan;
        let size = display_size(&self.plan, keep.0, keep.1);
        if size != self.size {
            self.resize(size)?;
        }
        Ok(())
    }

    /// Starts decoding the first frame of the next joins (≤ [`SPARES`], within
    /// [`AHEAD`] frames) on spare streams, where the join is not a continuation of the
    /// same source (SPEC §6.3), so playback does not stall on a new decode chain.
    fn preroll_ahead(&mut self, f: u64) {
        let mut want: HashMap<String, Vec<u64>> = HashMap::new();
        let mut n = 0;
        for p in &self.plan.parts {
            let s = p.out.start();
            // The join at `f` itself stays wanted until this frame has taken it.
            if s == 0 || s < f || s > f + MAX_AHEAD {
                continue;
            }
            if n >= SPARES {
                break;
            }
            let Ok(next) = frame_draw_with(&self.plan, s, &self.src.pictures) else {
                continue;
            };
            let chain = next
                .layers
                .iter()
                .filter(|l| !l.still)
                .filter_map(|l| self.src.pictures.chain_len(&l.stream(), l.frame))
                .max()
                .unwrap_or(0);
            if s > f + (AHEAD + chain / 4).min(MAX_AHEAD) {
                continue;
            }
            n += 1;
            let prev = frame_draw_with(&self.plan, s - 1, &self.src.pictures).ok();
            for l in next.layers.iter().filter(|l| !l.still) {
                let key = l.stream();
                let continuous = prev.as_ref().is_some_and(|pd| {
                    pd.layers
                        .iter()
                        .any(|pl| !pl.still && pl.stream() == key && pl.frame + 1 == l.frame)
                });
                let v = want.entry(key).or_default();
                if !continuous && !v.contains(&l.frame) {
                    v.push(l.frame);
                }
            }
        }
        for (src, frames) in want {
            self.src.pictures.preroll(&src, &frames);
        }
    }

    /// Re-targets the display size (clamped by [`display_size`]).
    pub fn resize(&mut self, size: (u32, u32)) -> Result<(), RenderError> {
        let size = display_size(&self.plan, size.0, size.1);
        if size == self.size {
            return Ok(());
        }
        self.composer.resize(&self.ctx, &self.plan, size);
        self.nv12 =
            Nv12Output::new(&self.ctx, size.0, size.1, 1, 1).map_err(gpu("nv12_output", None))?;
        self.size = size;
        Ok(())
    }

    /// Composites output frame `f` with `overrides` applied and `gizmos` drawn on top,
    /// and reads it back as NV12.
    pub fn render(
        &mut self,
        f: u64,
        overrides: &[LayerOverride],
        gizmos: &[GizmoQuad],
    ) -> Result<PreviewFrame, RenderError> {
        self.render_with(f, overrides, gizmos, false).map(|r| r.0)
    }

    /// Like [`Self::render`] for a scrub (H.05): live layers read the scrub proxy where
    /// it covers the frame, else the master's keyframe at or before it (never a decode
    /// chain); the result says which. Held layers, playback and paused frames are
    /// unchanged.
    pub fn render_scrub(
        &mut self,
        f: u64,
        overrides: &[LayerOverride],
        gizmos: &[GizmoQuad],
    ) -> Result<(PreviewFrame, ScrubPath), RenderError> {
        self.render_with(f, overrides, gizmos, true)
    }

    fn render_with(
        &mut self,
        f: u64,
        overrides: &[LayerOverride],
        gizmos: &[GizmoQuad],
        scrub: bool,
    ) -> Result<(PreviewFrame, ScrubPath), RenderError> {
        let (w, h) = self.size;
        if !scrub {
            self.preroll_ahead(f);
        }
        // the export's per-frame compose (H.04): nothing preview-only happens before the
        // gizmos, so at the canvas size with no overrides this is the export's frame
        let mut fetch = PreviewFetch {
            src: &mut self.src,
            plan: &self.plan,
            scrub,
            path: ScrubPath::Exact,
        };
        self.composer
            .compose(&self.ctx, &self.plan, f, overrides, &mut fetch)?;
        let path = fetch.path;
        let canvas_view = self.composer.canvas().create_view(&Default::default());
        if !gizmos.is_empty() {
            let mut ge = self.ctx.device.create_command_encoder(&Default::default());
            self.gizmo.encode(
                &self.ctx.device,
                &self.ctx.queue,
                &mut ge,
                &canvas_view,
                (w, h),
                gizmos,
            );
            self.ctx.queue.submit([ge.finish()]);
        }
        let pb = self
            .nv12
            .render_now(&canvas_view)
            .map_err(gpu("nv12_output", Some(f)))?;
        let p = pb.read_planes().map_err(gpu("nv12_readback", Some(f)))?;
        Ok((
            PreviewFrame {
                width: w,
                height: h,
                y: p.y,
                uv: p.uv,
            },
            path,
        ))
    }
}
