//! The holder's `crop_follow_cursor` (SPEC §5.7; `gmclip_core::cursor`): reads a part's
//! screen layer at 2 fps, finds the mouse cursor and plans a zoomed crop path that
//! follows it; preview by default, applied as one `cursor_follow_apply` transaction
//! (eased visual splits, so the preview and the export draw it through the shared
//! compose). The app's "Follow cursor" switch calls [`follow_cursor`] too.
//!
//! A screen recorded by gmclip (decision 0045) has an input log: the follow reads the
//! cursor from it (no frames decoded), and `crop_zoom_focus` ([`zoom_focus`],
//! `gmclip_core::zoom`) zooms on the caret and on clicks from it.

use gmclip_core::activity::{LumaFrame, NRect};
use gmclip_core::cursor::{self, FollowRules};
use gmclip_core::planner::plan_part_owners;
use gmclip_core::{CallRequest, CmdError};
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::rec::InputTrack;
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::Base;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::holder::{Conn, Holder, OpenProject};

/// Samples per second read.
pub const SAMPLES_PER_S: u64 = 2;
/// Frames are read at most this wide (a 1080p render's arrow is ≈ 12 px high: read at
/// full size; a 1440p recording's at half).
pub const LUMA_W: u32 = 1920;
/// Default zoom and ease.
pub const DEFAULT_ZOOM: f64 = 2.0;
pub const DEFAULT_EASE_MS: u32 = 600;

/// What a follow found for one part.
#[derive(Clone, Debug)]
pub struct CursorFollow {
    pub framing: Id<Framing>,
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    /// Samples read, and those where the cursor was placed.
    pub samples: usize,
    pub found: usize,
    /// `(at, crop)`: the moves (`None`: back to the layer's own crop).
    pub keys: Vec<(Flicks, Option<NormRect>)>,
    pub ease_ms: u32,
    pub max_zoom: f64,
    /// Where the cursor came from: `input_log` (the recording's log) or `pixels` (the
    /// detector on decoded frames).
    pub method: &'static str,
    /// Why a source with a log was read from its pixels after all.
    pub note: Option<String>,
}

impl CursorFollow {
    /// The `cursor_follow_apply` arguments that write it.
    pub fn apply_args(&self) -> Value {
        json!({
            "framing": self.framing, "part": self.part, "layer": self.layer,
            "ease_ms": self.ease_ms,
            "keys": self.keys.iter().map(|(at, c)| json!({"at": at, "crop": c})).collect::<Vec<_>>(),
        })
    }

    /// The reply's summary.
    pub fn listed(&self) -> Value {
        json!({
            "part": self.part, "layer": self.layer, "samples": self.samples,
            "found": self.found, "moves": self.keys.len(), "max_zoom": self.max_zoom,
            "ease_ms": self.ease_ms, "method": self.method, "note": self.note,
            "keys": self.keys.iter().take(64).map(|(at, c)| json!({"at": at, "crop": c})).collect::<Vec<_>>(),
        })
    }
}

fn framing_of(doc: &Doc, cut: Id<Cut>, f: Option<Id<Framing>>) -> Result<Id<Framing>, CmdError> {
    match f {
        Some(id) => {
            if doc.get(id).is_none_or(|x| x.cut != cut) {
                return Err(CmdError::invalid("/framing", "not a framing of this cut"));
            }
            Ok(id)
        }
        None => doc
            .children::<Framing>(cut.entity())
            .next()
            .ok_or_else(|| CmdError::invalid("/framing", "the cut has no framing")),
    }
}

fn nr(r: &NormRect) -> NRect {
    NRect {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
    }
}

fn nrect(c: &NRect) -> NormRect {
    NormRect {
        x: c.x,
        y: c.y,
        w: c.w,
        h: c.h,
    }
}

/// The input track of the source `part`'s picture comes from, and that member's sync
/// offset (flicks): `Ok(None)` when the source has no log. The bytes come from the
/// media host ([`gmclip_core::MediaRender::asset_bytes`]), else from app data's
/// `assets/<sha256>.json`, checked against the asset's sha256.
pub fn input_track(
    media: &dyn gmclip_core::MediaRender,
    doc: &Doc,
    part: &Part,
) -> Result<Option<(InputTrack, i64)>, String> {
    let Some((sid, off)) = gmclip_core::zoom::picture_of(doc, part) else {
        return Ok(None);
    };
    let Some(sc) = doc.get(sid).and_then(|s| s.cursor.as_ref()) else {
        return Ok(None);
    };
    let asset = doc
        .get(sc.track)
        .ok_or_else(|| "the input track's asset is gone".to_owned())?;
    let bytes = match media.asset_bytes(asset) {
        Ok(b) => b,
        Err(gmclip_core::host::HostError::Unsupported(_)) => {
            let dir = crate::paths::app_data().map_err(|_| "no app data folder".to_owned())?;
            let hex = &asset.sha256;
            if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("the input track's asset has no sha256".into());
            }
            let b = std::fs::read(dir.join("assets").join(format!("{hex}.json")))
                .map_err(|_| "the input track is not on this Mac".to_owned())?;
            use sha2::Digest;
            let got: String = sha2::Sha256::digest(&b)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect();
            if !got.eq_ignore_ascii_case(hex) {
                return Err("the input track's file does not match its asset".into());
            }
            b
        }
        Err(e) => return Err(format!("the input track could not be read ({e:?})")),
    };
    let track: InputTrack = serde_json::from_slice(&bytes)
        .map_err(|_| "the input track is not gmclip.input/1 JSON".to_owned())?;
    track.check()?;
    Ok(Some((track, off)))
}

/// Sample `i`'s source time: `src_in + i · step` frames (on the output frame grid).
fn sample_at(p: &Part, i: usize, step: u64, fpf: i64) -> Flicks {
    Flicks(p.src_in.0 + i as i64 * step as i64 * fpf)
}

/// The model framing's flicks per frame and the samples' step (frames) at `per_s`.
fn grid(doc: &Doc, framing: Id<Framing>, per_s: u64) -> Result<(i64, u64), CmdError> {
    let fps = doc
        .get(framing)
        .map(|f| f.fps)
        .ok_or_else(|| CmdError::invalid("/framing", "not found"))?;
    if fps.num <= 0 || fps.den == 0 {
        return Err(CmdError::invalid("/framing", "no frame rate"));
    }
    let fpf = gmclip_model::time::FLICKS_PER_SECOND * i64::from(fps.den) / fps.num;
    let rounded = ((fps.num as u64) + u64::from(fps.den) / 2) / u64::from(fps.den);
    Ok((fpf, (rounded / per_s).max(1)))
}

/// Keys at samples, as moves on the frame grid strictly inside the part, each after
/// the one before (with what each key carries).
fn keys_at<T>(
    p: &Part,
    step: u64,
    fpf: i64,
    keys: impl Iterator<Item = (usize, Option<NRect>, T)>,
) -> Vec<(Flicks, Option<NormRect>, T)> {
    let mut out = Vec::new();
    let mut last = p.src_in;
    for (sample, crop, x) in keys {
        let at = Flicks(p.src_in.0 + (sample as i64 * step as i64).max(1) * fpf);
        if at <= last || at >= p.src_out {
            continue;
        }
        last = at;
        out.push((at, crop.as_ref().map(nrect), x));
    }
    out
}

/// A follow's keys, as moves.
fn follow_keys(
    p: &Part,
    step: u64,
    fpf: i64,
    keys: Vec<cursor::CropKey>,
) -> Vec<(Flicks, Option<NormRect>)> {
    keys_at(
        p,
        step,
        fpf,
        keys.into_iter().map(|k| (k.sample, k.crop, ())),
    )
    .into_iter()
    .map(|(at, c, ())| (at, c))
    .collect()
}

/// Finds the cursor on `part`'s screen layer and plans the crop path. `off` plans no
/// moves (applying it removes the follow).
#[allow(clippy::too_many_arguments)]
pub fn follow_cursor(
    planner: &dyn gmclip_core::Planner,
    media: &dyn gmclip_core::MediaRender,
    snap: &gmclip_core::Snapshot,
    cut: Id<Cut>,
    part: Id<Part>,
    framing: Option<Id<Framing>>,
    layer: Option<Id<Layer>>,
    max_zoom: Option<f64>,
    ease_ms: Option<u32>,
    off: bool,
) -> Result<CursorFollow, CmdError> {
    let doc = &snap.doc;
    let framing = framing_of(doc, cut, framing)?;
    let p = doc
        .get(part)
        .filter(|p| p.cut == cut)
        .ok_or_else(|| CmdError::invalid("/part", "not a part of this cut"))?;
    let max_zoom = max_zoom.unwrap_or(DEFAULT_ZOOM);
    if !(1.2..=4.0).contains(&max_zoom) {
        return Err(CmdError::invalid("/max_zoom", "1.2–4"));
    }
    let ease_ms = ease_ms.unwrap_or(DEFAULT_EASE_MS);
    if !(100..=2000).contains(&ease_ms) {
        return Err(CmdError::invalid("/ease_ms", "100–2000 ms"));
    }
    let stack = gmclip_model::effective::effective(doc, part, framing, p.src_in)
        .ok_or_else(|| CmdError::invalid("/part", "the part's stack does not resolve"))?;
    let ml = stack
        .layers
        .iter()
        .find(|l| match layer {
            Some(id) => l.id == id,
            None => l.values.role == LayerRole::Region(RegionRole::Screen),
        })
        .ok_or_else(|| CmdError::invalid("/layer", "the part has no screen layer"))?;
    let mut out = CursorFollow {
        framing,
        part,
        layer: ml.id,
        samples: 0,
        found: 0,
        keys: Vec::new(),
        ease_ms,
        max_zoom,
        method: "pixels",
        note: None,
    };
    if off {
        return Ok(out);
    }
    let base = nr(&ml.values.crop);
    let rules = FollowRules::new(base, max_zoom);
    // a recording with an input log: the cursor from the log, no frames decoded
    match input_track(media, doc, p) {
        Ok(Some((track, offset))) => {
            let (fpf, step) = grid(doc, framing, SAMPLES_PER_S)?;
            let n = ((p.src_out.0 - p.src_in.0) / fpf).max(0) as u64;
            let (w, h) = (f64::from(track.w), f64::from(track.h));
            let points: Vec<Option<(f64, f64)>> = (0..n.div_ceil(step) as usize)
                .map(|i| {
                    let t = gmclip_core::zoom::flicks_to_us(sample_at(p, i, step, fpf).0 - offset);
                    track
                        .cursor_at(t)
                        .map(|c| (f64::from(c.x) / w, f64::from(c.y) / h))
                })
                .collect();
            out.method = "input_log";
            out.samples = points.len();
            out.found = points.iter().filter(|p| p.is_some()).count();
            let keys = cursor::crop_path(&points, &rules);
            out.keys = follow_keys(p, step, fpf, keys);
            return Ok(out);
        }
        Ok(None) => {}
        Err(why) => out.note = Some(format!("{why}: read from the pixels")),
    }
    let plan = planner.plan(doc, cut, framing)?;
    let owners = plan_part_owners(doc, cut, framing);
    let fpf = plan.output.fps.flicks_per_frame() as i64;
    let step = (plan.output.fps.rounded() / SAMPLES_PER_S).max(1);
    // the part's first plan part: its picture runs on continuously through the others
    let k = owners
        .iter()
        .position(|o| *o == part)
        .ok_or_else(|| CmdError::invalid("/part", "not in the plan"))?;
    let pp = &plan.parts[k];
    let src_id = pp.picture.source.clone();
    let n = ((p.src_out.0 - p.src_in.0) / fpf).max(0) as u64;
    let frames: Vec<u64> = (0..n)
        .step_by(step as usize)
        .map(|i| gmclip_core::gmclip_plan::retime::nominal_frame(pp, i, plan.output.fps))
        .collect();
    out.samples = frames.len();
    if frames.len() < 2 {
        return Ok(out);
    }
    let luma: Vec<LumaFrame> = media
        .luma_frames(&plan, src_id.as_str(), &frames, LUMA_W)
        .map_err(CmdError::from)?;
    // what is not the screen: other layers' crops (the camera inset) and the source's
    // other regions (chat, alerts)
    let mut exclude: Vec<NRect> = stack
        .layers
        .iter()
        .filter(|l| l.id != ml.id && l.values.role != LayerRole::Detail)
        .map(|l| nr(&l.values.crop))
        .filter(|r| r.w * r.h < 0.5)
        .collect();
    if let Some(path) = plan
        .sources
        .iter()
        .find(|s| s.id == src_id)
        .map(|s| s.path.clone())
        && let Some(src) = doc.iter::<Source>().find(|s| s.path == path)
    {
        exclude.extend(
            doc.children::<Region>(src.id.entity())
                .filter_map(|r| doc.get(r))
                .filter(|r| r.role != RegionRole::Screen)
                .map(|r| nr(&r.rect)),
        );
    }
    let points = cursor::track(&luma, &base, &exclude);
    out.found = points.iter().filter(|p| p.is_some()).count();
    let keys = cursor::crop_path(&points, &rules);
    out.keys = follow_keys(p, step, fpf, keys);
    Ok(out)
}

/// Zoom samples per second (the track is exact; 10 Hz is fine enough for a 1 s gap).
pub const ZOOM_SAMPLES_PER_S: u64 = 10;

/// What a zoom to focus planned for one part.
#[derive(Clone, Debug)]
pub struct ZoomFocus {
    pub framing: Id<Framing>,
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    pub samples: usize,
    /// `(at, crop, why)`: the moves (`None`: back to the layer's own crop).
    pub keys: Vec<(Flicks, Option<NormRect>, &'static str)>,
    pub ease_ms: u32,
    pub max_zoom: f64,
}

impl ZoomFocus {
    /// The `cursor_follow_apply` arguments that write it.
    pub fn apply_args(&self) -> Value {
        json!({
            "framing": self.framing, "part": self.part, "layer": self.layer,
            "ease_ms": self.ease_ms,
            "keys": self.keys.iter().map(|(at, c, _)| json!({"at": at, "crop": c})).collect::<Vec<_>>(),
        })
    }

    /// The reply's summary.
    pub fn listed(&self) -> Value {
        json!({
            "part": self.part, "layer": self.layer, "samples": self.samples,
            "moves": self.keys.len(), "max_zoom": self.max_zoom, "ease_ms": self.ease_ms,
            "method": "input_log",
            "keys": self.keys.iter().take(64).map(|(at, c, why)| json!({"at": at, "crop": c, "why": why})).collect::<Vec<_>>(),
        })
    }
}

/// Plans zoom to focus and clicks on `part`'s screen layer from its recording's input
/// log (`gmclip_core::zoom`). `off` plans no moves (applying it removes the zoom). A
/// source without a log is refused: there is nothing to aim at.
#[allow(clippy::too_many_arguments)]
pub fn zoom_focus(
    media: &dyn gmclip_core::MediaRender,
    snap: &gmclip_core::Snapshot,
    cut: Id<Cut>,
    part: Id<Part>,
    framing: Option<Id<Framing>>,
    layer: Option<Id<Layer>>,
    max_zoom: Option<f64>,
    ease_ms: Option<u32>,
    off: bool,
) -> Result<ZoomFocus, CmdError> {
    let doc = &snap.doc;
    let framing = framing_of(doc, cut, framing)?;
    let p = doc
        .get(part)
        .filter(|p| p.cut == cut)
        .ok_or_else(|| CmdError::invalid("/part", "not a part of this cut"))?;
    let max_zoom = max_zoom.unwrap_or(DEFAULT_ZOOM);
    if !(1.2..=4.0).contains(&max_zoom) {
        return Err(CmdError::invalid("/max_zoom", "1.2–4"));
    }
    let ease_ms = ease_ms.unwrap_or(DEFAULT_EASE_MS);
    if !(100..=2000).contains(&ease_ms) {
        return Err(CmdError::invalid("/ease_ms", "100–2000 ms"));
    }
    let stack = gmclip_model::effective::effective(doc, part, framing, p.src_in)
        .ok_or_else(|| CmdError::invalid("/part", "the part's stack does not resolve"))?;
    let ml = stack
        .layers
        .iter()
        .find(|l| match layer {
            Some(id) => l.id == id,
            None => l.values.role == LayerRole::Region(RegionRole::Screen),
        })
        .ok_or_else(|| CmdError::invalid("/layer", "the part has no screen layer"))?;
    let mut out = ZoomFocus {
        framing,
        part,
        layer: ml.id,
        samples: 0,
        keys: Vec::new(),
        ease_ms,
        max_zoom,
    };
    if off {
        return Ok(out);
    }
    let (track, offset) = match input_track(media, doc, p) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(CmdError::invalid(
                "/part",
                "the part's screen source has no input log (recorded outside gmclip): use crop_follow_cursor",
            ));
        }
        Err(_) => {
            return Err(CmdError::invalid(
                "/part",
                "the part's input log could not be read on this Mac",
            ));
        }
    };
    let (fpf, step) = grid(doc, framing, ZOOM_SAMPLES_PER_S)?;
    let n = ((p.src_out.0 - p.src_in.0) / fpf).max(0) as u64;
    let samples: Vec<i64> = (0..n.div_ceil(step) as usize)
        .map(|i| gmclip_core::zoom::flicks_to_us(sample_at(p, i, step, fpf).0 - offset))
        .collect();
    out.samples = samples.len();
    let rules = gmclip_core::zoom::ZoomRules::new(nr(&ml.values.crop), max_zoom);
    let keys = gmclip_core::zoom::zoom_path(&track, &samples, &rules);
    out.keys = keys_at(
        p,
        step,
        fpf,
        keys.iter().map(|k| (k.sample, k.crop, k.why.as_str())),
    );
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    cut: String,
    part: String,
    #[serde(default)]
    framing: Option<String>,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    max_zoom: Option<f64>,
    #[serde(default)]
    ease_ms: Option<u32>,
    #[serde(default)]
    off: Option<bool>,
    #[serde(default)]
    dry_run: Option<bool>,
}

fn ulid_of<E>(s: &str, path: &str) -> Result<Id<E>, CmdError> {
    gmclip_model::Ulid::parse(s.split_once(':').map_or(s, |(_, r)| r))
        .map(Id::<E>::new)
        .map_err(|_| CmdError::invalid(path, "not an id"))
}

/// `crop_follow_cursor{cut, part, framing?, layer?, max_zoom?, ease_ms?, off?, dry_run?}`.
#[allow(clippy::too_many_arguments)]
pub fn crop_follow_cursor(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: Value,
    base: Option<Base>,
    request_id: Option<String>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: Args = serde_json::from_value(args).map_err(|_| {
        CmdError::invalid(
            "/args",
            "{cut, part, framing?, layer?, max_zoom?, ease_ms?, off?, dry_run?}",
        )
    })?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let part: Id<Part> = ulid_of(&a.part, "/part")?;
    let framing: Option<Id<Framing>> = a
        .framing
        .as_deref()
        .map(|s| ulid_of(s, "/framing"))
        .transpose()?;
    let layer: Option<Id<Layer>> = a
        .layer
        .as_deref()
        .map(|s| ulid_of(s, "/layer"))
        .transpose()?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let f = follow_cursor(
        holder.planner.as_ref(),
        holder.media.as_ref(),
        &snap,
        cut,
        part,
        framing,
        layer,
        a.max_zoom,
        a.ease_ms,
        a.off.unwrap_or(false),
    )?;
    let dry = a.dry_run.unwrap_or(true);
    let mut v = json!({"dry_run": dry, "follow": f.listed(), "as_of": doc.rev()});
    if dry {
        v["display"] = json!(format!(
            "follow cursor ({}): cursor found in {}/{} samples, {} move(s) (preview: run again with dry_run:false and base to apply) · as_of {}",
            f.method,
            f.found,
            f.samples,
            f.keys.len(),
            doc.rev()
        ));
        return Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES));
    }
    let ok = p.handle.call(CallRequest {
        command: "cursor_follow_apply".into(),
        args: f.apply_args(),
        base,
        request_id,
        caller: conn.caller(),
    })?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    v["committed"] = json!(ok.committed);
    v["as_of"] = json!(ok.as_of);
    v["display"] = json!(format!(
        "follow cursor: {} move(s) written; committed rev {:?} · as_of {}",
        f.keys.len(),
        ok.committed,
        ok.as_of
    ));
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

/// `crop_zoom_focus{cut, part, framing?, layer?, max_zoom?, ease_ms?, off?, dry_run?}`.
#[allow(clippy::too_many_arguments)]
pub fn crop_zoom_focus(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: Value,
    base: Option<Base>,
    request_id: Option<String>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: Args = serde_json::from_value(args).map_err(|_| {
        CmdError::invalid(
            "/args",
            "{cut, part, framing?, layer?, max_zoom?, ease_ms?, off?, dry_run?}",
        )
    })?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let part: Id<Part> = ulid_of(&a.part, "/part")?;
    let framing: Option<Id<Framing>> = a
        .framing
        .as_deref()
        .map(|s| ulid_of(s, "/framing"))
        .transpose()?;
    let layer: Option<Id<Layer>> = a
        .layer
        .as_deref()
        .map(|s| ulid_of(s, "/layer"))
        .transpose()?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let z = zoom_focus(
        holder.media.as_ref(),
        &snap,
        cut,
        part,
        framing,
        layer,
        a.max_zoom,
        a.ease_ms,
        a.off.unwrap_or(false),
    )?;
    let dry = a.dry_run.unwrap_or(true);
    let mut v = json!({"dry_run": dry, "zoom": z.listed(), "as_of": doc.rev()});
    if dry {
        v["display"] = json!(format!(
            "zoom to focus: {} move(s) from the input log (preview: run again with dry_run:false and base to apply) · as_of {}",
            z.keys.len(),
            doc.rev()
        ));
        return Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES));
    }
    let ok = p.handle.call(CallRequest {
        command: "cursor_follow_apply".into(),
        args: z.apply_args(),
        base,
        request_id,
        caller: conn.caller(),
    })?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    v["committed"] = json!(ok.committed);
    v["as_of"] = json!(ok.as_of);
    v["display"] = json!(format!(
        "zoom to focus: {} move(s) written; committed rev {:?} · as_of {}",
        z.keys.len(),
        ok.committed,
        ok.as_of
    ));
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

// -------------------------------------------------------------------------------------
// zoom_auto (decision 0046): the input log's zoom to focus as zoom sections
// -------------------------------------------------------------------------------------

/// What `zoom_auto` planned: the automatic zoom sections, in cut ms.
#[derive(Clone, Debug)]
pub struct ZoomAutoPlan {
    pub framing: Option<Id<Framing>>,
    /// `section_auto_apply`'s `sections`: `{start_ms, end_ms, zoom}`.
    pub sections: Vec<Value>,
    /// Parts read from their input log, and parts skipped (no log, no screen layer).
    pub parts_read: usize,
    pub parts_skipped: usize,
}

impl ZoomAutoPlan {
    /// The `section_auto_apply` arguments that write it.
    pub fn apply_args(&self, cut: Id<Cut>) -> Value {
        json!({"cut": cut, "framing": self.framing, "sections": self.sections})
    }
}

/// One zoom section being built: `[start, end)` cut flicks, its target and depth.
struct Run {
    start: i64,
    end: Option<i64>,
    focus: bool,
    depth: f64,
}

/// Plans zoom sections from the input logs of `cut`'s parts (`gmclip_core::zoom`): each
/// key into a caret or field starts a `focus` zoom, a click key a `cursor` zoom, at the
/// key's depth (the layer's crop over the key's); an idle key ends it after its ease out,
/// a part's end ends it there. A key of the same target and depth continues the zoom.
pub fn zoom_auto_plan(
    media: &dyn gmclip_core::MediaRender,
    snap: &gmclip_core::Snapshot,
    cut: Id<Cut>,
    framing: Option<Id<Framing>>,
    max_zoom: Option<f64>,
) -> Result<ZoomAutoPlan, CmdError> {
    let doc = &snap.doc;
    let read_framing = framing_of(doc, cut, framing)?;
    let max_zoom = max_zoom.unwrap_or(DEFAULT_ZOOM);
    if !(1.2..=4.0).contains(&max_zoom) {
        return Err(CmdError::invalid("/max_zoom", "1.2–4"));
    }
    let ease = gmclip_model::clock::ms_to_flicks(700).0;
    let (fpf, step) = grid(doc, read_framing, ZOOM_SAMPLES_PER_S)?;
    let mut runs: Vec<Run> = Vec::new();
    let (mut read, mut skipped) = (0, 0);
    for span in gmclip_model::clock::cut_layout(doc, cut) {
        let Some(p) = doc.get(span.part) else {
            continue;
        };
        let part_end = span.start.0 + span.dur.0;
        let base =
            gmclip_model::effective::effective(doc, p.id, read_framing, p.src_in).and_then(|st| {
                st.layers
                    .iter()
                    .find(|l| l.values.role == LayerRole::Region(RegionRole::Screen))
                    .map(|l| nr(&l.values.crop))
            });
        let (Some(base), Ok(Some((track, offset)))) = (base, input_track(media, doc, p)) else {
            skipped += 1;
            continue;
        };
        read += 1;
        let n = ((p.src_out.0 - p.src_in.0) / fpf).max(0) as u64;
        let srcs: Vec<Flicks> = (0..n.div_ceil(step) as usize)
            .map(|i| sample_at(p, i, step, fpf))
            .collect();
        let samples: Vec<i64> = srcs
            .iter()
            .map(|t| gmclip_core::zoom::flicks_to_us(t.0 - offset))
            .collect();
        let rules = gmclip_core::zoom::ZoomRules::new(base, max_zoom);
        let keys = gmclip_core::zoom::zoom_path(&track, &samples, &rules);
        let cut_at = |i: usize| {
            gmclip_model::clock::src_to_cut(doc, p.id, srcs[i])
                .map_or(span.start.0, |t| t.0)
                .clamp(span.start.0, part_end)
        };
        for k in &keys {
            let t = cut_at(k.sample);
            let open = runs.last_mut().filter(|r| r.end.is_none());
            match &k.crop {
                Some(c) => {
                    let focus = k.why != gmclip_core::zoom::ZoomWhy::Click;
                    let depth = ((base.w / c.w.max(1e-6)) * 100.0).round() / 100.0;
                    let depth = depth.clamp(1.0, 6.0);
                    match open {
                        Some(r) if r.focus == focus && (r.depth - depth).abs() < 0.05 => {}
                        Some(r) => {
                            r.end = Some(t);
                            runs.push(Run {
                                start: t,
                                end: None,
                                focus,
                                depth,
                            });
                        }
                        None => runs.push(Run {
                            start: t,
                            end: None,
                            focus,
                            depth,
                        }),
                    }
                }
                None => {
                    if let Some(r) = open {
                        r.end = Some((t + ease).min(part_end));
                    }
                }
            }
        }
        if let Some(r) = runs.last_mut().filter(|r| r.end.is_none()) {
            r.end = Some(part_end);
        }
    }
    let ms =
        |f: i64| (f + gmclip_model::clock::FLICKS_PER_MS / 2) / gmclip_model::clock::FLICKS_PER_MS;
    let sections = runs
        .iter()
        .filter_map(|r| {
            let (a, b) = (ms(r.start), ms(r.end?));
            // a zoom too short to ease in and out is left out
            (b - a >= 300).then(|| {
                json!({"start_ms": a, "end_ms": b, "zoom": {
                    "depth": r.depth,
                    "target": if r.focus { "focus" } else { "cursor" },
                    "auto": true,
                }})
            })
        })
        .collect();
    Ok(ZoomAutoPlan {
        framing,
        sections,
        parts_read: read,
        parts_skipped: skipped,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AutoArgs {
    cut: String,
    #[serde(default)]
    framing: Option<String>,
    #[serde(default)]
    max_zoom: Option<f64>,
    #[serde(default)]
    dry_run: Option<bool>,
}

/// `zoom_auto{cut, framing?, max_zoom?, dry_run?}` (decision 0046).
#[allow(clippy::too_many_arguments)]
pub fn zoom_auto(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: Value,
    base: Option<Base>,
    request_id: Option<String>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: AutoArgs = serde_json::from_value(args)
        .map_err(|_| CmdError::invalid("/args", "{cut, framing?, max_zoom?, dry_run?}"))?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let framing: Option<Id<Framing>> = a
        .framing
        .as_deref()
        .map(|s| ulid_of(s, "/framing"))
        .transpose()?;
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let z = zoom_auto_plan(holder.media.as_ref(), &snap, cut, framing, a.max_zoom)?;
    if z.parts_read == 0 {
        return Err(CmdError::invalid(
            "/cut",
            "no part's screen has an input log (recorded with gmclip): add zoom sections by hand with section_add",
        ));
    }
    let dry = a.dry_run.unwrap_or(true);
    let mut v = json!({
        "dry_run": dry, "sections": z.sections, "parts_read": z.parts_read,
        "parts_skipped": z.parts_skipped, "as_of": doc.rev(),
    });
    if dry {
        v["display"] = json!(format!(
            "zoom auto: {} zoom section(s) from {} part(s)' input log (preview: run again with dry_run:false and base to apply; earlier automatic zooms are replaced, hand-made ones kept) · as_of {}",
            z.sections.len(),
            z.parts_read,
            doc.rev()
        ));
        return Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES));
    }
    let ok = p.handle.call(CallRequest {
        command: "section_auto_apply".into(),
        args: z.apply_args(cut),
        base,
        request_id,
        caller: conn.caller(),
    })?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    v["ids"] = ok.output["ids"].clone();
    v["removed"] = ok.output["removed"].clone();
    v["committed"] = json!(ok.committed);
    v["as_of"] = json!(ok.as_of);
    v["display"] = json!(format!(
        "zoom auto: {} zoom section(s) written, {} earlier automatic one(s) replaced; committed rev {:?} · as_of {}",
        z.sections.len(),
        ok.output["removed"],
        ok.committed,
        ok.as_of
    ));
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}
