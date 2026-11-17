//! Roto mattes (SPEC §5.8, decision 0013): building mask tracks with Apple Vision
//! `.accurate` (export; macOS only, Windows applies tracks built on a Mac) and resolving each matted layer's mask for a frame —
//! the stored track when there is one, else a live `.fast` draft where drafts are
//! allowed (the preview, or an export the user confirmed with draft mattes).
//!
//! Both preview and export resolve through [`Mattes::resolve`] inside the one per-frame
//! compose (H.04), so the same track gives the same pixels (R-COMP-1).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use gmclip_media::backend::FrameStream;
use gmclip_media::decode::DecodedFrame;
use gmclip_plan::Plan;
use gmclip_plan::types::Rect;

use super::compose::CropScale;
/// The segmentation model's quality (Apple Vision's; on Windows, only to name tracks
/// built on a Mac).
pub use super::os::SegQuality as TrackQuality;
use super::os::{SegQuality, Segmenter, VisionMask, cpu_planes, recipe, segment_crop};
use super::{failed, gpu};
use crate::compositor::LayerDraw;
use crate::export::Inputs;
use crate::matte::store::MatteStore;
use crate::matte::{Mask, MaskPlacement, Track, finish, source_identity, track_key, track_rect};
use crate::ops::RenderError;

/// What happens when a matted layer has no stored track.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DraftPolicy {
    /// Segment the frame live with `.fast` (the preview; an export after "use draft
    /// mattes" was confirmed).
    Allow,
    /// Refuse: the export waits for accurate tracks, so a missing one is a bug.
    Refuse,
}

fn vision_mask(m: VisionMask) -> Mask {
    Mask {
        width: m.width,
        height: m.height,
        data: m.data,
    }
}

/// The model family whose mask is the picture's own bottom half (no track, no
/// segmentation): graphics rendered with alpha, packed as colour over grey.
const PACKED_ALPHA: &str = "packed_alpha";

/// A `packed_alpha` mask: the crop's rectangle `rect` (source px) read from the bottom
/// half of the picture, its luma (video range) as alpha.
fn packed_mask(
    pic: &DecodedFrame,
    rect: (u32, u32, u32, u32),
    src_size: (u32, u32),
    f: u64,
) -> Result<Mask, RenderError> {
    let planes = cpu_planes(&pic.image)
        .ok_or_else(|| failed("matte", Some(f), "packed alpha needs CPU planes"))?
        .map_err(gpu("matte", Some(f)))?;
    let bps = planes.format.bytes_per_sample();
    let (pw, ph) = (planes.width.max(1), planes.height.max(1));
    let kx = f64::from(pw) / f64::from(src_size.0.max(1));
    let ky = f64::from(ph) / f64::from(src_size.1.max(1));
    let half = f64::from(src_size.1) / 2.0;
    let mut m = Mask::filled(rect.2, rect.3, 0);
    for y in 0..rect.3 {
        let py = (((f64::from(rect.1 + y) + half + 0.5) * ky) as u32).min(ph - 1);
        for x in 0..rect.2 {
            let px = (((f64::from(rect.0 + x) + 0.5) * kx) as u32).min(pw - 1);
            let o = ((py * pw + px) as usize) * bps;
            // high byte for 10-bit samples (little-endian); video range to full
            let v = i32::from(planes.y[o + bps - 1]);
            m.data[(y * rect.2 + x) as usize] = ((v - 16) * 255 / 219).clamp(0, 255) as u8;
        }
    }
    Ok(m)
}

/// The model a track is built with for a plan's `model` family.
fn track_model(family: &str) -> Result<SegQuality, RenderError> {
    match family {
        "apple_vision" => Ok(SegQuality::Accurate),
        other => Err(RenderError::Unsupported {
            step: "matte",
            reason: format!("roto model {other:?} (macOS has apple_vision)"),
        }),
    }
}

/// Resolves matted layers' masks frame by frame.
pub(crate) struct Mattes {
    store: Arc<MatteStore>,
    policy: DraftPolicy,
    draft: Option<Segmenter>,
    /// `plan source id → identity` (fingerprint), and its picture size.
    ident: HashMap<String, String>,
    /// Draft masks of the last frames (`(layer key, frame)`), so a paused frame redrawn
    /// does not segment again.
    drafts: Vec<((String, u64), Arc<Mask>)>,
    /// `part/layer` ids drawn with a draft matte.
    pub(crate) used_draft: BTreeSet<String>,
}

impl std::fmt::Debug for Mattes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mattes")
            .field("policy", &self.policy)
            .field("used_draft", &self.used_draft)
            .finish_non_exhaustive()
    }
}

impl Mattes {
    pub(crate) fn new(store: Arc<MatteStore>, policy: DraftPolicy) -> Self {
        Self {
            store,
            policy,
            draft: None,
            ident: HashMap::new(),
            drafts: Vec::new(),
            used_draft: BTreeSet::new(),
        }
    }

    fn identity(&mut self, plan: &Plan, source: &str) -> String {
        if let Some(i) = self.ident.get(source) {
            return i.clone();
        }
        let i = plan
            .sources
            .iter()
            .find(|s| s.id.as_str() == source)
            .map(source_identity)
            .unwrap_or_else(|| source.to_owned());
        self.ident.insert(source.to_owned(), i.clone());
        i
    }

    /// Fills `l.matte.mask` for output frame `f`. `pic` is the layer's picture and
    /// `scale` its crop scale (a scrub proxy picture is smaller than the source);
    /// `src_size` is the source's picture size. `l.crop` is still in source pixels.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve(
        &mut self,
        plan: &Plan,
        part_id: &str,
        l: &mut LayerDraw,
        pic: &DecodedFrame,
        scale: CropScale,
        src_size: (u32, u32),
        f: u64,
    ) -> Result<(), RenderError> {
        let ident = self.identity(plan, &l.source);
        let Some(m) = l.matte.as_mut() else {
            return Ok(());
        };
        let rect = track_rect(&l.crop, src_size.0, src_size.1);
        if m.spec.model == PACKED_ALPHA {
            let raw = packed_mask(pic, rect, src_size, f)?;
            m.draft = false;
            let at = MaskPlacement {
                x: f64::from(rect.0),
                y: f64::from(rect.1),
                sx: 1.0,
                sy: 1.0,
            };
            let strokes = m.spec.strokes.clone().unwrap_or_default();
            let done = finish(&raw, at, m.spec.soften.unwrap_or(0.0), &strokes);
            let (sx, sy) = scale.unwrap_or((1.0, 1.0));
            m.mask = Some((
                Arc::new(done),
                Rect {
                    x: f64::from(rect.0) * sx,
                    y: f64::from(rect.1) * sy,
                    w: f64::from(rect.2) * sx,
                    h: f64::from(rect.3) * sy,
                },
            ));
            return Ok(());
        }
        let quality = track_model(&m.spec.model)?;
        let key = track_key(&ident, m.span.0, m.span.1, rect, quality.model_name());
        // a converting part's clock may pick past its nominal span (VFR): the edge mask
        let idx = l
            .frame
            .saturating_sub(m.span.0)
            .min(m.span.1.saturating_sub(1)) as usize;
        let at = MaskPlacement {
            x: f64::from(rect.0),
            y: f64::from(rect.1),
            sx: 1.0,
            sy: 1.0,
        };
        let raw = match self.store.get(&key).and_then(|t| t.frame(idx)) {
            Some(raw) => {
                m.draft = false;
                Arc::new(raw)
            }
            None => {
                if self.policy == DraftPolicy::Refuse {
                    return Err(failed(
                        "matte",
                        Some(f),
                        format!(
                            "no mask track for layer {} (build tracks first)",
                            l.layer_id
                        ),
                    ));
                }
                m.draft = true;
                self.used_draft.insert(format!("{part_id}/{}", l.layer_id));
                let dkey = (format!("{key}/{}", l.layer_id), l.frame);
                if let Some((_, d)) = self.drafts.iter().find(|(k, _)| *k == dkey) {
                    d.clone()
                } else {
                    let (sx, sy) = scale.unwrap_or((1.0, 1.0));
                    let prect = (
                        (f64::from(rect.0) * sx).floor() as u32,
                        (f64::from(rect.1) * sy).floor() as u32,
                        (f64::from(rect.2) * sx).ceil().max(2.0) as u32,
                        (f64::from(rect.3) * sy).ceil().max(2.0) as u32,
                    );
                    if self.draft.is_none() {
                        self.draft =
                            Some(Segmenter::new(SegQuality::Fast).map_err(gpu("vision", Some(f)))?);
                    }
                    let seg = self.draft.as_mut().expect("just made");
                    let vm = segment_crop(seg, &pic.image, prect, f)?;
                    let d = Arc::new(vision_mask(vm).resize(rect.2, rect.3));
                    self.drafts.push((dkey, d.clone()));
                    if self.drafts.len() > 8 {
                        self.drafts.remove(0);
                    }
                    d
                }
            }
        };
        let strokes = m.spec.strokes.clone().unwrap_or_default();
        let done = finish(&raw, at, m.spec.soften.unwrap_or(0.0), &strokes);
        let (sx, sy) = scale.unwrap_or((1.0, 1.0));
        m.mask = Some((
            Arc::new(done),
            Rect {
                x: f64::from(rect.0) * sx,
                y: f64::from(rect.1) * sy,
                w: f64::from(rect.2) * sx,
                h: f64::from(rect.3) * sy,
            },
        ));
        Ok(())
    }
}

/// A forward decode stream over one source (frames in presentation order).
pub type MatteStream =
    FrameStream<Box<dyn gmclip_media::backend::SampleSource + Send>, super::os::AnyDecoder>;

/// Opens a forward decode stream on a source's video (tracks, lab timings).
pub fn open_stream(v: crate::export::VideoInput) -> Result<MatteStream, RenderError> {
    let (r, src) = recipe(v.source, &v.codec, v.width, v.height)?;
    Ok(FrameStream::new(src, move || r.make()))
}

/// One mask track a plan needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackNeed {
    pub key: String,
    pub source: String,
    /// First source frame and frame count.
    pub span: (u64, u64),
    /// Integer source-pixel rectangle `(x, y, w, h)`.
    pub rect: (u32, u32, u32, u32),
    /// `part/layer` ids that use it.
    pub layers: Vec<String>,
    pub cached: bool,
}

/// What [`build_tracks`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatteBuild {
    pub needs: Vec<TrackNeed>,
    pub built: usize,
    pub frames: u64,
    pub seconds: f64,
}

/// The mask tracks `plan` needs at `quality`, and which are cached already.
pub fn track_needs(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    store: &MatteStore,
    quality: SegQuality,
) -> Result<Vec<TrackNeed>, RenderError> {
    let mut sizes: HashMap<String, (u32, u32)> = HashMap::new();
    let mut needs: Vec<TrackNeed> = Vec::new();
    for p in &plan.parts {
        for l in &p.layers {
            let Some(m) = &l.matte else { continue };
            if !l.is_visible() || l.opacity() <= 0.0 || m.model == PACKED_ALPHA {
                continue;
            }
            track_model(&m.model)?;
            let source = l
                .picture_source
                .as_ref()
                .unwrap_or(&p.picture.source)
                .as_str()
                .to_owned();
            let size = match sizes.get(&source) {
                Some(s) => *s,
                None => {
                    let v = inputs.video(plan, &source)?;
                    sizes.insert(source.clone(), (v.width, v.height));
                    (v.width, v.height)
                }
            };
            let span = match &l.time_map {
                Some(gmclip_plan::types::TimeMap::Hold { frame, .. }) => (*frame, 1),
                Some(gmclip_plan::types::TimeMap::FreezeFirst) => {
                    (gmclip_plan::retime::nominal_frame(p, 0, plan.output.fps), 1)
                }
                _ => gmclip_plan::retime::picture_span(p, plan.output.fps),
            };
            let rect = track_rect(&l.crop, size.0, size.1);
            let ident = plan
                .sources
                .iter()
                .find(|s| s.id.as_str() == source)
                .map(source_identity)
                .unwrap_or_else(|| source.clone());
            let key = track_key(&ident, span.0, span.1, rect, quality.model_name());
            let who = format!("{}/{}", p.id.as_str(), l.id.as_str());
            if let Some(n) = needs.iter_mut().find(|n| n.key == key) {
                n.layers.push(who);
            } else {
                needs.push(TrackNeed {
                    cached: store.contains(&key),
                    key,
                    source,
                    span,
                    rect,
                    layers: vec![who],
                });
            }
        }
    }
    // a matted clip event (decision 0042): its span of its own source
    for e in plan.clip_events.iter().flatten() {
        let Some(m) = &e.matte else { continue };
        if e.opacity.is_some_and(|o| o <= 0.0) || m.model == PACKED_ALPHA {
            continue;
        }
        track_model(&m.model)?;
        let source = e.picture.source.as_str().to_owned();
        let size = match sizes.get(&source) {
            Some(s) => *s,
            None => {
                let v = inputs.video(plan, &source)?;
                sizes.insert(source.clone(), (v.width, v.height));
                (v.width, v.height)
            }
        };
        let span = gmclip_plan::retime::clip_span(e, plan.output.fps);
        let rect = track_rect(&e.crop, size.0, size.1);
        let ident = plan
            .sources
            .iter()
            .find(|s| s.id.as_str() == source)
            .map(source_identity)
            .unwrap_or_else(|| source.clone());
        let key = track_key(&ident, span.0, span.1, rect, quality.model_name());
        let who = format!("{}{}", crate::compositor::CLIP_LAYER, e.id.as_str());
        if let Some(n) = needs.iter_mut().find(|n| n.key == key) {
            n.layers.push(who);
        } else {
            needs.push(TrackNeed {
                cached: store.contains(&key),
                key,
                source,
                span,
                rect,
                layers: vec![who],
            });
        }
    }
    Ok(needs)
}

/// Builds every missing track `plan` needs at `quality` into `store`: each span decoded
/// in order and segmented through one `VNSequenceRequestHandler`, masks resampled to
/// the crop's source pixels, smoothed and stored. `progress` counts frames segmented.
pub fn build_tracks(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    store: &MatteStore,
    quality: SegQuality,
    progress: Option<&AtomicU64>,
    cancel: Option<&AtomicBool>,
) -> Result<MatteBuild, RenderError> {
    let t0 = Instant::now();
    let needs = track_needs(plan, inputs, store, quality)?;
    let mut built = 0;
    let mut frames = 0;
    for n in needs.iter().filter(|n| !n.cached) {
        let mut stream = open_stream(inputs.video(plan, &n.source)?)?;
        let mut seg = Segmenter::sequence(quality).map_err(gpu("vision", None))?;
        let mut masks = Vec::with_capacity(n.span.1 as usize);
        for i in 0..n.span.1 {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(failed("cancelled", None, "mask tracks cancelled"));
            }
            let fr = n.span.0 + i;
            let pic = stream.frame(fr).map_err(|mut e| {
                e.source_id = Some(n.source.clone());
                RenderError::Media(e)
            })?;
            let vm = segment_crop(&mut seg, &pic.image, n.rect, fr)?;
            masks.push(vision_mask(vm).resize(n.rect.2, n.rect.3));
            frames += 1;
            if let Some(p) = progress {
                p.fetch_add(1, Ordering::Relaxed);
            }
        }
        let t = Track::from_masks(quality.model_name(), &masks)
            .ok_or_else(|| failed("matte", None, "empty mask track"))?;
        store.put(&n.key, t).map_err(|e| RenderError::Io {
            step: "matte",
            what: "mask track",
            kind: e.kind(),
        })?;
        built += 1;
    }
    Ok(MatteBuild {
        needs,
        built,
        frames,
        seconds: t0.elapsed().as_secs_f64(),
    })
}

/// One matted layer's quality row (SPEC §5.8 matte view).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct QualityRow {
    pub plan_part: String,
    pub layer: String,
    /// `accurate` (a stored track) or `missing` (not built yet).
    pub status: &'static str,
    pub quality: Option<crate::matte::MatteQuality>,
}

/// Every matted layer's [`crate::matte::MatteQuality`] from its accurate track (soften
/// and brush strokes applied, as drawn), building missing tracks first when `build`.
pub fn quality_report(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    store: &MatteStore,
    build: bool,
) -> Result<Vec<QualityRow>, RenderError> {
    if build {
        build_tracks(plan, inputs, store, SegQuality::Accurate, None, None)?;
    }
    let needs = track_needs(plan, inputs, store, SegQuality::Accurate)?;
    let mut rows = Vec::new();
    for p in &plan.parts {
        for l in &p.layers {
            let Some(m) = &l.matte else { continue };
            let who = format!("{}/{}", p.id.as_str(), l.id.as_str());
            let Some(n) = needs.iter().find(|n| n.layers.contains(&who)) else {
                continue;
            };
            let quality = store.get(&n.key).map(|t| {
                let at = MaskPlacement {
                    x: f64::from(n.rect.0),
                    y: f64::from(n.rect.1),
                    sx: 1.0,
                    sy: 1.0,
                };
                let strokes = m.strokes.clone().unwrap_or_default();
                let masks: Vec<Mask> = (0..t.len())
                    .filter_map(|i| t.frame(i))
                    .map(|raw| finish(&raw, at, m.soften.unwrap_or(0.0), &strokes))
                    .collect();
                crate::matte::quality(&masks)
            });
            rows.push(QualityRow {
                plan_part: p.id.as_str().to_owned(),
                layer: l.id.as_str().to_owned(),
                status: if quality.is_some() {
                    "accurate"
                } else {
                    "missing"
                },
                quality,
            });
        }
    }
    Ok(rows)
}

/// Grey frames of `source` at presentation indexes `frames` (ascending), box-averaged to
/// at most `max_w` wide: `(width, height, luma)`. Decoded from the source in order.
pub fn luma_frames(
    plan: &Plan,
    inputs: &mut dyn Inputs,
    source: &str,
    frames: &[u64],
    max_w: u32,
) -> Result<Vec<(u32, u32, Vec<u8>)>, RenderError> {
    let mut stream = open_stream(inputs.video(plan, source)?)?;
    let mut out = Vec::with_capacity(frames.len());
    for &f in frames {
        let pic = stream.frame(f).map_err(|mut e| {
            e.source_id = Some(source.to_owned());
            RenderError::Media(e)
        })?;
        let planes = cpu_planes(&pic.image)
            .ok_or_else(|| failed("luma", Some(f), "decoder returned CPU planes"))?
            .map_err(gpu("luma", Some(f)))?;
        let bps = planes.format.bytes_per_sample();
        let (w, h) = (planes.width, planes.height);
        let k = w.div_ceil(max_w.max(1)).max(1);
        let (ow, oh) = (w / k, h / k);
        let mut luma = vec![0u8; (ow * oh) as usize];
        for y in 0..oh {
            for x in 0..ow {
                let mut acc = 0u32;
                for yy in 0..k {
                    for xx in 0..k {
                        let o = (((y * k + yy) * w + x * k + xx) as usize) * bps;
                        // high byte for 10-bit samples (little-endian)
                        acc += u32::from(planes.y[o + bps - 1]);
                    }
                }
                luma[(y * ow + x) as usize] = (acc / (k * k)) as u8;
            }
        }
        out.push((ow, oh, luma));
    }
    Ok(out)
}
