//! The one per-frame compose (H.04, R-COMP-1): the export's video pass, `render_frames`
//! (stills and contact sheets) and the preview session all composite an output frame
//! through [`FrameComposer::compose`], so preview and export cannot drift. Callers differ
//! only in where pictures come from ([`FramePictures`]: the export's decoders, or the
//! preview's, which may read the scrub proxy) and in what they do with the canvas after
//! (NV12 to the encoder, NV12 read back, gizmos over it).
//!
//! The steps, in order: the plan's draw list for the frame → live layer overrides (a
//! gizmo drag in the preview; none in export) → `place` scaled from the plan canvas to
//! the compositor's size (× 1 at the canvas size, so exact) → one import per distinct
//! `(source, frame)` → crops scaled into a proxy picture's pixels (never in export) →
//! layers, each recorded cursor just above its layer (decision 0045; zoom sections,
//! decision 0046, tighten the crops first, and a zoom's motion blur draws a moving layer
//! as its sub-samples) → text tiles (with a preview's live text moves, never in export) →
//! submit.

use std::collections::HashMap;
use std::sync::Arc;

use gmclip_gpu::GpuContext;
use gmclip_media::decode::DecodedFrame;
use gmclip_plan::Plan;
use gmclip_plan::types::Rect;

use super::compose_err;
use super::os::{Importer, PlaneTextures};
use crate::compositor::gpu::{GpuCompositor, GpuPlanes};
use crate::compositor::{ChromaSiting, LayerDraw, SourceClocks, frame_draw_with};
use crate::ops::RenderError;

/// A live, uncommitted change to one layer while a gizmo is dragged (canvas pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerOverride {
    pub part_id: String,
    pub layer_id: String,
    pub place: Option<Rect>,
    pub crop: Option<Rect>,
}

/// A live, uncommitted move of text events while a text overlay or the caption seat is
/// dragged (U.12; canvas pixels): every event of `role` anchored at `at` (within half a
/// pixel; `None`: every event of the role, the caption seat) is drawn `(dx, dy)` away.
#[derive(Clone, Debug, PartialEq)]
pub struct TextShift {
    pub role: gmclip_plan::types::TextRole,
    pub at: Option<(f64, f64)>,
    pub dx: f64,
    pub dy: f64,
}

impl TextShift {
    /// The offset for `ev`, when this shift moves it.
    pub fn offset(&self, ev: &crate::text::TextEvent) -> Option<(f64, f64)> {
        let here = self
            .at
            .is_none_or(|(x, y)| (ev.pos.x - x).abs() < 0.5 && (ev.pos.y - y).abs() < 0.5);
        (ev.role == self.role && here).then_some((self.dx, self.dy))
    }
}

/// Source pixels → picture pixels for a crop (`None`: the picture is the source's size).
pub(super) type CropScale = Option<(f64, f64)>;

/// Where a frame's pictures come from.
pub(super) trait FramePictures {
    /// The draw moved to another part: held / frozen pictures belong to one part.
    fn part_changed(&mut self);
    /// The picture for `layer` at output frame `out_frame`, and its crop scale when it
    /// is smaller than the source (a scrub proxy picture).
    fn picture(
        &mut self,
        layer: &LayerDraw,
        out_frame: u64,
    ) -> Result<(Arc<DecodedFrame>, CropScale), RenderError>;
    /// Each picture's chroma siting (by plan source id).
    fn siting(&self, source: &str) -> ChromaSiting;
    /// A source's picture size (its decoded frames, not a proxy's).
    fn source_size(&self, source: &str) -> (u32, u32);
    /// The sources' presentation clocks (picture rate conversion, decision 0017).
    fn clocks(&self) -> &dyn SourceClocks;
}

/// The export's pictures: its decoders, nothing scaled.
pub(super) struct ExportPictures<'a>(pub &'a mut super::Pictures);

impl FramePictures for ExportPictures<'_> {
    fn part_changed(&mut self) {
        self.0.stills.clear();
    }

    fn picture(
        &mut self,
        l: &LayerDraw,
        f: u64,
    ) -> Result<(Arc<DecodedFrame>, CropScale), RenderError> {
        Ok((
            self.0.get(&l.stream(), &l.source, l.frame, l.still, f)?,
            None,
        ))
    }

    fn siting(&self, source: &str) -> ChromaSiting {
        self.0.meta.get(source).map_or(ChromaSiting::Left, |m| m.2)
    }

    fn source_size(&self, source: &str) -> (u32, u32) {
        self.0.meta.get(source).map_or((0, 0), |m| (m.0, m.1))
    }

    fn clocks(&self) -> &dyn SourceClocks {
        &*self.0
    }
}

/// The compositor, its text layer and the importer that feeds it, at one size.
pub(super) struct FrameComposer {
    importer: Importer,
    comp: GpuCompositor,
    text: crate::compositor::text_layer::TextLayer,
    /// Recorded cursors (decision 0045), drawn over the layers that show them.
    cursors: crate::compositor::cursor::CursorLayer,
    /// Zoom sections (decision 0046), applied to the draw list before the cursors.
    zooms: crate::compositor::zoom::ZoomLayer,
    size: (u32, u32),
    last_part: Option<String>,
    /// Roto mattes (decision 0013): the track store and the draft policy.
    mattes: super::mattes::Mattes,
    /// The last frame drew at least one draft matte.
    last_draft: bool,
    /// Matte view (preview only): matted layers' masks in white on black, nothing else.
    matte_view: bool,
    /// Live text moves (preview only, a gizmo drag; export never sets any).
    text_shifts: Vec<TextShift>,
}

impl FrameComposer {
    /// A composer at `size` (the plan's canvas for export; the displayed size for the
    /// preview, whose text tiles are then scaled from the canvas).
    pub(super) fn new(
        ctx: &GpuContext,
        plan: &Plan,
        size: (u32, u32),
    ) -> Result<Self, RenderError> {
        let importer = Importer::new(ctx)?;
        let text = crate::compositor::text_layer::TextLayer::new(plan).map_err(compose_err(0))?;
        Ok(Self {
            importer,
            comp: Self::compositor(ctx, plan, size),
            text,
            cursors: crate::compositor::cursor::CursorLayer::new(plan),
            zooms: crate::compositor::zoom::ZoomLayer::new(plan),
            size,
            last_part: None,
            mattes: super::mattes::Mattes::new(
                Arc::new(crate::matte::store::MatteStore::in_memory()),
                super::mattes::DraftPolicy::Allow,
            ),
            last_draft: false,
            matte_view: false,
            text_shifts: Vec::new(),
        })
    }

    fn compositor(ctx: &GpuContext, plan: &Plan, size: (u32, u32)) -> GpuCompositor {
        let mut comp = GpuCompositor::new(&ctx.device, size.0, size.1);
        comp.set_tile_scale(
            size.0 as f32 / plan.output.canvas.w.get() as f32,
            size.1 as f32 / plan.output.canvas.h.get() as f32,
        );
        comp
    }

    /// Where matted layers' masks come from (the store) and whether a missing track
    /// may be drawn as a live draft.
    pub(super) fn set_mattes(&mut self, m: super::mattes::Mattes) {
        self.mattes = m;
    }

    pub(super) fn mattes(&self) -> &super::mattes::Mattes {
        &self.mattes
    }

    /// Whether the last composed frame drew a draft matte (the "draft matte" badge).
    pub(super) fn drew_draft(&self) -> bool {
        self.last_draft
    }

    /// The text moves the next frames draw (the preview's live text drag; never export).
    pub(super) fn set_text_shifts(&mut self, shifts: Vec<TextShift>) {
        self.text_shifts = shifts;
    }

    /// Matte view on or off (the preview's "show the cut-out" toggle; never export).
    pub(super) fn set_matte_view(&mut self, on: bool) {
        self.matte_view = on;
    }

    pub(super) fn canvas(&self) -> &wgpu::Texture {
        self.comp.canvas()
    }

    /// Re-targets the size (a new compositor; the text layer and importer are kept).
    pub(super) fn resize(&mut self, ctx: &GpuContext, plan: &Plan, size: (u32, u32)) {
        if size != self.size {
            self.comp = Self::compositor(ctx, plan, size);
            self.size = size;
        }
    }

    /// A new plan: held pictures are let go, fonts and images re-read when they changed or
    /// the plan starts drawing text or images.
    pub(super) fn set_plan(&mut self, old: &Plan, plan: &Plan) -> Result<(), RenderError> {
        self.last_part = None;
        // tiles are cached by content, so a rebuilt layer is cheap
        if crate::compositor::text_layer::TextLayer::stale(old, plan) {
            self.text =
                crate::compositor::text_layer::TextLayer::new(plan).map_err(compose_err(0))?;
        }
        if plan.zooms != old.zooms || plan.cursors != old.cursors || plan.parts != old.parts {
            self.zooms.set_plan(plan);
        }
        if plan.cursors != old.cursors || plan.images != old.images {
            self.cursors.set_plan(plan);
        }
        Ok(())
    }

    /// Composites output frame `f` of `plan` into the canvas (submitted to the queue).
    pub(super) fn compose(
        &mut self,
        ctx: &GpuContext,
        plan: &Plan,
        f: u64,
        overrides: &[LayerOverride],
        pics: &mut dyn FramePictures,
    ) -> Result<(), RenderError> {
        let (w, h) = self.size;
        let sx = f64::from(w) / plan.output.canvas.w.get() as f64;
        let sy = f64::from(h) / plan.output.canvas.h.get() as f64;
        let mut draw = frame_draw_with(plan, f, pics.clocks()).map_err(compose_err(f))?;
        if self.last_part.as_deref() != Some(draw.part_id.as_str()) {
            pics.part_changed();
            self.last_part = Some(draw.part_id.clone());
        }
        for l in &mut draw.layers {
            if let Some(o) = overrides
                .iter()
                .find(|o| o.part_id == draw.part_id && o.layer_id == l.layer_id)
            {
                if let Some(p) = o.place {
                    l.place = p;
                }
                if let Some(c) = o.crop {
                    l.crop = c;
                }
            }
        }
        // zoom sections (decision 0046): tighter crops and shrunk cameras, before the
        // cursors so each follows its layer's zoomed crop
        self.zooms.apply(plan, &mut draw);
        // recorded cursors through each layer's crop and place in plan canvas px (none in
        // matte view, whose layers are only the matted ones)
        let mut cursor_tiles = if self.matte_view {
            Vec::new()
        } else {
            self.cursors.tiles(plan, &draw)
        };
        // a zoom's motion blur (decision 0046, off unless asked for): a moving layer as
        // its sub-samples, copies in the draw list every compositor draws alike
        let at = self.zooms.expand_blur(&mut draw);
        crate::compositor::zoom::remap_tiles(&mut cursor_tiles, &at);
        for l in &mut draw.layers {
            l.place = Rect {
                x: l.place.x * sx,
                y: l.place.y * sy,
                w: l.place.w * sx,
                h: l.place.h * sy,
            };
        }
        let mut imported: Vec<(PlaneTextures, ChromaSiting, f32)> =
            Vec::with_capacity(draw.layers.len());
        let mut cache: HashMap<(String, u64), (usize, CropScale)> = HashMap::new();
        let mut which = Vec::with_capacity(draw.layers.len());
        let mut crop_scale: Vec<CropScale> = Vec::with_capacity(draw.layers.len());
        let mut pictures: Vec<Arc<DecodedFrame>> = Vec::with_capacity(draw.layers.len());
        for l in &draw.layers {
            let key = (l.source.clone(), l.frame);
            if let Some(&(k, sc)) = cache.get(&key) {
                which.push(k);
                crop_scale.push(sc);
                continue;
            }
            let (pic, sc) = pics.picture(l, f)?;
            let (planes, scale) = self.importer.import(&pic, f)?;
            cache.insert(key, (imported.len(), sc));
            which.push(imported.len());
            crop_scale.push(sc);
            imported.push((planes, pics.siting(&l.source), scale));
            pictures.push(pic);
        }
        // roto mattes: the stored track's mask (or a draft), crop still in source px
        self.last_draft = false;
        let part_id = draw.part_id.clone();
        for (i, l) in draw.layers.iter_mut().enumerate() {
            if l.matte.is_some() {
                let size = pics.source_size(&l.source);
                self.mattes.resolve(
                    plan,
                    &part_id,
                    l,
                    &pictures[which[i]],
                    crop_scale[i],
                    size,
                    f,
                )?;
                self.last_draft |= l.matte.as_ref().is_some_and(|m| m.draft);
            }
        }
        if self.matte_view {
            // the masks alone: white where kept, on black; unmatted layers left out
            draw.background = [0.0; 3];
            let keep: Vec<bool> = draw.layers.iter().map(|l| l.matte.is_some()).collect();
            for l in &mut draw.layers {
                if let Some(m) = l.matte.as_mut() {
                    m.view = true;
                }
            }
            let mut k = 0;
            let mut i = 0;
            draw.layers.retain(|_| {
                let r = keep[i];
                i += 1;
                r
            });
            which.retain(|_| {
                let r = keep[k];
                k += 1;
                r
            });
            let mut j = 0;
            crop_scale.retain(|_| {
                let r = keep[j];
                j += 1;
                r
            });
        }
        // the blurred-source fill's picture (often the screen layer's own frame)
        let backdrop_k = match &draw.backdrop {
            Some(b) => {
                let key = (b.source.clone(), b.frame);
                match cache.get(&key) {
                    Some(&(k, _)) => Some(k),
                    None => {
                        let probe = LayerDraw {
                            layer_id: "backdrop".into(),
                            source: b.source.clone(),
                            frame: b.frame,
                            crop: Rect {
                                x: 0.0,
                                y: 0.0,
                                w: 1.0,
                                h: 1.0,
                            },
                            place: Rect {
                                x: 0.0,
                                y: 0.0,
                                w: 1.0,
                                h: 1.0,
                            },
                            opacity: 1.0,
                            still: false,
                            matte: None,
                            cover: None,
                            shape: None,
                        };
                        let (pic, sc) = pics.picture(&probe, f)?;
                        let (planes, scale) = self.importer.import(&pic, f)?;
                        cache.insert(key, (imported.len(), sc));
                        imported.push((planes, pics.siting(&b.source), scale));
                        Some(imported.len() - 1)
                    }
                }
            }
            None => None,
        };
        // a proxy picture is smaller than the source: crops are in its pixels
        for (l, sc) in draw.layers.iter_mut().zip(&crop_scale) {
            if let Some((sx, sy)) = sc {
                l.crop = Rect {
                    x: l.crop.x * sx,
                    y: l.crop.y * sy,
                    w: l.crop.w * sx,
                    h: l.crop.h * sy,
                };
            }
        }
        let views: Vec<(wgpu::TextureView, wgpu::TextureView)> = imported
            .iter()
            .map(|(p, _, _)| {
                (
                    p.y.create_view(&Default::default()),
                    p.uv.create_view(&Default::default()),
                )
            })
            .collect();
        let gp: Vec<GpuPlanes<'_>> = which
            .iter()
            .map(|&k| {
                let (p, siting, scale) = &imported[k];
                GpuPlanes {
                    y: &views[k].0,
                    uv: &views[k].1,
                    width: p.width,
                    height: p.height,
                    code_scale: *scale,
                    siting: *siting,
                }
            })
            .collect();
        let bp = backdrop_k.map(|k| {
            let (p, siting, scale) = &imported[k];
            GpuPlanes {
                y: &views[k].0,
                uv: &views[k].1,
                width: p.width,
                height: p.height,
                code_scale: *scale,
                siting: *siting,
            }
        });
        let under = self.text.under(plan).map_err(compose_err(f))?;
        self.comp
            .set_under(if self.matte_view { None } else { under });
        // the tiles above every layer, and the images drawn among them (decision 0042;
        // none in matte view, whose layers are only the matted ones)
        let tiles_at = if self.matte_view {
            Vec::new()
        } else {
            std::mem::take(&mut draw.tiles_at)
        };
        let (tiles, among) = if self.text_shifts.is_empty() {
            self.text.active_split(plan, f, &|_| None, &tiles_at)
        } else {
            let shifts = &self.text_shifts;
            self.text.active_split(
                plan,
                f,
                &|ev| shifts.iter().find_map(|s| s.offset(ev)),
                &tiles_at,
            )
        }
        .map_err(compose_err(f))?;
        self.comp
            .set_among(crate::compositor::cursor::merge_among(among, cursor_tiles));
        let mut ce = ctx.device.create_command_encoder(&Default::default());
        self.comp
            .encode_with_backdrop(&ctx.queue, &mut ce, &draw, &gp, bp.as_ref())
            .map_err(compose_err(f))?;
        self.comp.draw_tiles(&ctx.queue, &mut ce, &tiles);
        ctx.queue.submit([ce.finish()]);
        drop(gp);
        drop(views);
        drop(imported);
        drop(pictures);
        self.importer.flush();
        Ok(())
    }
}
