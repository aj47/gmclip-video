//! The model planner (SPEC §6.5 step 1, M1 ticket A18): one cut in one framing →
//! RenderPlan v0, for the M0 feature set — parts on a source or sync group, layer
//! stacks through [`gmclip_model::effective`] (linked layouts, part overrides, content
//! holds), imported captions (`CaptionDoc.source: imported` cues), heard captions built
//! from the active transcripts ([`captions`]), text overlays (hook, section pills,
//! run-clock ticks, counters expanded by [`counter`]) on any anchor the resolver places,
//! gain segments, and the reference's click-guard fades at every discontinuous join.
//!
//! What it refuses ([`PlannerError`]) is everything RenderPlan v0 or this M1 slice cannot
//! say: speed changes, non-source-view layers, mattes, mirrors, grades, covers, feather,
//! `at_src` holds (they need the anchor resolver), supplied captions, non-integer frame
//! rates and times off the frame grid.
//!
//! Conventions the model does not store and the planner therefore fixes:
//! - text anchoring and style constants: [`style`];
//! - captions are text layer 1; overlays keep their `z` as their layer;
//! - a fade of [`FADE_SAMPLES`] samples (linear) at the start and end of the cut and on
//!   both sides of every discontinuous join, none at a continuous one;
//! - layer ids `screen`, `cam`, `detail-N` by role; text ids `hook`, `pill-N`,
//!   `clock-N`, `cNN` (the converter's); source ids `s1, s2, …` and part ids `p01, …`
//!   (the model keeps no names for either);
//! - source fingerprints are stored as `sha256:<hex>;size:<bytes>` ([`fingerprint`]).

pub mod captions;
pub mod counter;
pub mod groupings;
pub mod heard;
pub mod sections;
pub mod style;

use std::collections::BTreeMap;

use gmclip_model::anchor::Anchor;
use gmclip_model::clock::ModelTime;
use gmclip_model::effective::{effective, effective_caption_place, effective_overlay_place};
use gmclip_model::entities::*;
use gmclip_model::time::FLICKS_PER_SECOND;
use gmclip_model::{Doc, Id};
use gmclip_plan::consts::{AacLc, Bt709, ConstFalse, ConstU64, H264, LinearDb, SchemaVersion, Vbr};
use gmclip_plan::scalars::{Even, FpsNum, Id as PId, Range, Rgb, Rgba, RoleName, Sha256};
use gmclip_plan::types as rp;

/// The generator name the planner writes into `meta.generator`.
pub const GENERATOR: &str = "gmclip planner";
/// The text layer captions draw on.
pub const CAPTION_LAYER: i64 = 1;
/// The reference's click guard at a discontinuous join: 144 samples, linear.
pub const FADE_SAMPLES: u64 = 144;
/// The blurred-source fill: canvas px per small-picture px (≈ σ 14 px at 1080 wide).
pub const BLURRED_BLUR: u64 = 24;
/// …dimmed by this much, so the sharp layers stand out.
pub const BLURRED_DIM: f64 = 0.3;
/// …over the reference's background where no picture is drawn.
pub const BLURRED_FALLBACK: &str = "#0c1118";
/// The output audio rate (RenderPlan v0 allows only this).
pub const AUDIO_RATE: i64 = 48_000;

/// Why a cut does not plan. Shape-only text (no user strings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerError(pub String);

impl std::fmt::Display for PlannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlannerError {}

fn err<T>(m: impl Into<String>) -> Result<T, PlannerError> {
    Err(PlannerError(m.into()))
}

/// What the planner needs from outside the document: font files are never stored in a
/// project (SPEC §4.4 `assets`: "never fonts").
pub trait PlanHost {
    /// The file of a font face, if this machine has it.
    fn font_path(&self, font: &Font) -> Option<String>;
    /// The stored file of an asset (an overlay image's PNG), if this machine has it.
    fn asset_path(&self, _asset: &Asset) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------------------
// Encodings shared with the importer
// ---------------------------------------------------------------------------------------

/// `Source.fingerprint` as the importer writes it: `sha256:<hex>;size:<bytes>`, either
/// part optional.
pub fn fingerprint(sha256: Option<&str>, size: Option<u64>) -> String {
    let mut v = Vec::new();
    if let Some(s) = sha256 {
        v.push(format!("sha256:{s}"));
    }
    if let Some(n) = size {
        v.push(format!("size:{n}"));
    }
    v.join(";")
}

/// The parts of a [`fingerprint`]: `(sha256, size)`.
pub fn fingerprint_parts(s: &str) -> (Option<String>, Option<u64>) {
    let (mut sha, mut size) = (None, None);
    for p in s.split(';') {
        if let Some(h) = p.strip_prefix("sha256:") {
            sha = Some(h.to_owned());
        } else if let Some(n) = p.strip_prefix("size:") {
            size = n.parse().ok();
        }
    }
    (sha, size)
}

/// The `RawTime.reason` of an imported time: `import:<file>#<key>`.
pub fn import_reason(file: &str, key: &str) -> String {
    format!("import:{file}#{key}")
}

/// RenderPlan provenance for a raw time's reason (`import:<file>#<key>` only).
pub fn reason_provenance(reason: &str) -> Option<rp::Provenance> {
    let rest = reason.strip_prefix("import:")?;
    let (file, key) = rest.rsplit_once('#')?;
    Some(rp::Provenance {
        kind: rp::ProvenanceKind::Import,
        detail: Some(rp::ProvenanceDetail {
            file: Some(file.to_owned()),
            key: Some(key.to_owned()),
            ..rp::ProvenanceDetail::default()
        }),
    })
}

/// Flicks per frame at an integer rate, when exact.
pub fn flicks_per_frame(fps: i64) -> Option<i64> {
    (fps > 0 && FLICKS_PER_SECOND % fps == 0).then(|| FLICKS_PER_SECOND / fps)
}

/// Channels for an `AudioTrackProbe.layout`.
pub fn layout_channels(layout: &str) -> Option<i64> {
    match layout {
        "mono" => Some(1),
        "stereo" => Some(2),
        other => other.strip_suffix("ch").and_then(|n| n.parse().ok()),
    }
}

/// `AudioTrackProbe.layout` for a channel count.
pub fn channels_layout(ch: i64) -> String {
    match ch {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n}ch"),
    }
}

/// The RenderPlan layer role name of a model role.
pub fn role_name(r: LayerRole) -> Option<&'static str> {
    Some(match r {
        LayerRole::Region(RegionRole::Screen) => "screen",
        LayerRole::Region(RegionRole::Cam) => "cam",
        LayerRole::Detail => "detail",
        _ => return None,
    })
}

/// The model role of a RenderPlan layer role name.
pub fn model_role(name: &str) -> Option<LayerRole> {
    Some(match name {
        "screen" => LayerRole::Region(RegionRole::Screen),
        "cam" => LayerRole::Region(RegionRole::Cam),
        "detail" => LayerRole::Detail,
        _ => return None,
    })
}

/// Overlay role strings (RenderPlan `TextRole` names).
pub fn text_role(role: &str) -> Option<rp::TextRole> {
    Some(match role {
        "text_hook" => rp::TextRole::TextHook,
        "section_pill" => rp::TextRole::SectionPill,
        "run_clock" => rp::TextRole::RunClock,
        "note" => rp::TextRole::Note,
        "caption" => rp::TextRole::Caption,
        _ => return None,
    })
}

/// The overlay role string of a RenderPlan text role.
pub fn text_role_name(r: &rp::TextRole) -> &'static str {
    match r {
        rp::TextRole::TextHook => "text_hook",
        rp::TextRole::SectionPill => "section_pill",
        rp::TextRole::RunClock => "run_clock",
        rp::TextRole::Note => "note",
        rp::TextRole::Caption => "caption",
    }
}

/// The base style name of a styles-map key (`Speech@84` → `Speech`): keys with `@`
/// are fitted-size variants of the base style, used by imported cues.
pub fn base_style(name: &str) -> &str {
    name.split_once('@').map_or(name, |(b, _)| b)
}

// ---------------------------------------------------------------------------------------
// The planner
// ---------------------------------------------------------------------------------------

struct Ctx<'d> {
    doc: &'d Doc,
    cut: Id<Cut>,
    fpf: i64,
    /// Each part's output frames, for part-edge anchors.
    edges: BTreeMap<Id<Part>, (u64, u64)>,
    canvas: (u32, u32),
}

/// Output frame of a cut-clock time: `⌈t·F − ½⌉` (SPEC §4.1.1, ties to the earlier).
fn frame_of_cut_time(t: i64, fpf: i64) -> u64 {
    let num = 2 * i128::from(t) - i128::from(fpf);
    let den = 2 * i128::from(fpf);
    (num.div_euclid(den) + i128::from(num.rem_euclid(den) != 0)).max(0) as u64
}

/// A run clock's runs as the reference draws them: "GENERATING " white + the time in the keyword
/// yellow; "DONE IN " green + the time white.
fn clock_runs(label: &str, done_label: &str) -> Result<Vec<rp::Run>, PlannerError> {
    let Some((lab, val)) = label.rsplit_once(' ') else {
        return Ok(vec![rp::Run {
            text: label.to_owned(),
            fill: None,
        }]);
    };
    let colour = |c: &str| Rgba::new(c.to_owned()).map_err(PlannerError);
    let done = !done_label.is_empty() && label.starts_with(done_label);
    Ok(if done {
        vec![
            rp::Run {
                text: format!("{lab} "),
                fill: Some(colour("#3DDC84FF")?),
            },
            rp::Run {
                text: val.to_owned(),
                fill: None,
            },
        ]
    } else {
        vec![
            rp::Run {
                text: format!("{lab} "),
                fill: None,
            },
            rp::Run {
                text: val.to_owned(),
                fill: Some(colour(captions::KEYWORD_FILL)?),
            },
        ]
    })
}

/// A cut time → output sample (nearest).
fn cut_samples(t: gmclip_model::Flicks) -> Result<u64, PlannerError> {
    if t.0 < 0 {
        return err("a gain time before the cut");
    }
    let s = i128::from(t.0) * i128::from(AUDIO_RATE);
    let f = i128::from(FLICKS_PER_SECOND);
    Ok(((s + f / 2) / f) as u64)
}

impl Ctx<'_> {
    /// An anchor on the cut clock → output frame, with its provenance. M1 plans
    /// imported times: raw cut-clock times on the frame grid, and cut edges.
    fn frame(&self, a: &Anchor, total: u64) -> Result<(u64, Option<rp::Provenance>), PlannerError> {
        match a {
            Anchor::Raw(r) => match &r.time {
                ModelTime::Cut { cut, t } if *cut == self.cut => {
                    if t.0 < 0 || t.0 % self.fpf != 0 {
                        return err("a raw cut time off the frame grid");
                    }
                    Ok(((t.0 / self.fpf) as u64, reason_provenance(&r.reason)))
                }
                // a source-clock time (the app's spans): the first part that plays it
                _ => {
                    let t = self.cut_time(a)?;
                    Ok((
                        frame_of_cut_time(t.0, self.fpf).min(total),
                        reason_provenance(&r.reason),
                    ))
                }
            },
            Anchor::PartStart(e)
                if e.offset_ms.unwrap_or(0) == 0 && self.edges.contains_key(&e.part) =>
            {
                Ok((self.edges[&e.part].0, None))
            }
            Anchor::PartEnd(e)
                if e.offset_ms.unwrap_or(0) == 0 && self.edges.contains_key(&e.part) =>
            {
                Ok((self.edges[&e.part].1, None))
            }
            Anchor::CutStart(e) if e.cut == self.cut && e.offset_ms.unwrap_or(0) == 0 => {
                Ok((0, None))
            }
            Anchor::CutEnd(e) if e.cut == self.cut && e.offset_ms.unwrap_or(0) == 0 => {
                Ok((total, None))
            }
            // Words, cut points, measurements, frames, offsets: the anchor resolver's
            // source time, then the first part (in order) that plays it.
            other => {
                let t = self.cut_time(other)?;
                Ok((frame_of_cut_time(t.0, self.fpf).min(total), None))
            }
        }
    }

    /// Any anchor → cut time through the resolver: its source time, then the first part
    /// (in order) that plays it.
    /// An overlay's output frames `[a, z)` (none when empty) and its start's provenance.
    fn span(
        &self,
        o: &Overlay,
        total: u64,
    ) -> Result<Option<(u64, u64, Option<rp::Provenance>)>, PlannerError> {
        let fpf = self.fpf;
        let (a, prov) = self.frame(&o.start, total)?;
        let z = match &o.end {
            OverlayEnd::At(e) => self.frame(e, total)?.0,
            OverlayEnd::DurMs(ms) => {
                // ⌈t·F − ½⌉ on the end time (SPEC §4.1.1 tie rule)
                let t = i128::from(a) * i128::from(fpf)
                    + i128::from(*ms) * i128::from(FLICKS_PER_SECOND / 1000);
                let num = 2 * t - i128::from(fpf);
                let den = 2 * i128::from(fpf);
                (num.div_euclid(den) + i128::from(num.rem_euclid(den) != 0)) as u64
            }
        };
        let z = z.min(total);
        Ok((z > a).then_some((a, z, prov)))
    }

    fn cut_time(&self, a: &Anchor) -> Result<gmclip_model::Flicks, PlannerError> {
        let r = crate::anchors::resolve(self.doc, Some(self.cut), a, "/anchor")
            .map_err(|_| PlannerError("an anchor that does not resolve".into()))?;
        // the resolved cut time is on the anchor's own media; a bare source time would
        // land in whichever part first plays that time, of any video
        r.cut_t.ok_or(PlannerError(
            "an anchor on media this cut does not play".into(),
        ))
    }

    fn samples(&self, a: &Anchor) -> Result<(u64, Option<rp::Provenance>), PlannerError> {
        match a {
            Anchor::Raw(r) => match &r.time {
                ModelTime::Cut { cut, t } if *cut == self.cut => {
                    let s = i128::from(t.0) * i128::from(AUDIO_RATE);
                    if t.0 < 0 || s % i128::from(FLICKS_PER_SECOND) != 0 {
                        return err("a gain time off the sample grid");
                    }
                    Ok((
                        (s / i128::from(FLICKS_PER_SECOND)) as u64,
                        reason_provenance(&r.reason),
                    ))
                }
                _ => {
                    let t = self.cut_time(a)?;
                    Ok((cut_samples(t)?, reason_provenance(&r.reason)))
                }
            },
            other => Ok((cut_samples(self.cut_time(other)?)?, None)),
        }
    }
}

/// A source as RenderPlan sees it: its plan id and pixel size.
struct PlanSource {
    id: PId,
    dims: Option<(u32, u32)>,
    /// The probed nominal picture rate `(num, den)`, when known.
    rate: Option<(u64, u64)>,
}

struct Sources<'a> {
    doc: &'a Doc,
    order: Vec<Id<Source>>,
    by_id: BTreeMap<Id<Source>, PlanSource>,
}

impl Sources<'_> {
    fn get(&mut self, s: Id<Source>) -> Result<&PlanSource, PlannerError> {
        if !self.by_id.contains_key(&s) {
            let src = self
                .doc
                .get(s)
                .ok_or(PlannerError("a part's source is gone".into()))?;
            let dims = src
                .probe
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video)
                .and_then(|t| Some((t.width?, t.height?)));
            let rate = src
                .probe
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video)
                .and_then(|t| t.fps)
                .filter(|r| r.num > 0 && r.den > 0)
                .map(|r| (r.num as u64, u64::from(r.den)));
            let id = PId::new(format!("s{}", self.order.len() + 1)).map_err(PlannerError)?;
            self.order.push(s);
            self.by_id.insert(s, PlanSource { id, dims, rate });
        }
        Ok(&self.by_id[&s])
    }

    fn plan(&self) -> Result<Vec<rp::Source>, PlannerError> {
        let mut out = Vec::new();
        for s in &self.order {
            let src = self.doc.get(*s).ok_or(PlannerError("source".into()))?;
            let (sha, size) = fingerprint_parts(&src.fingerprint);
            let sha = match sha {
                Some(h) => Some(Sha256::new(h).map_err(PlannerError)?),
                None => None,
            };
            let video = src.probe.tracks.iter().find(|t| t.kind == TrackKind::Video);
            // the mic of a multi-track recording (audio roles; decision 0009); a voice track
            // the probe does not list falls back to the first, as an absent field does
            let audio = src
                .audio_roles
                .voice
                .and_then(|v| src.probe.audio_tracks.iter().find(|t| t.index == v))
                .or_else(|| src.probe.audio_tracks.first());
            let audio_track = src
                .audio_roles
                .voice
                .filter(|v| src.probe.audio_tracks.iter().any(|t| t.index == *v));
            let vcodec = |c: &str| match c {
                "h264" => Some(rp::VideoCodec::H264),
                "hevc" => Some(rp::VideoCodec::Hevc),
                "vp9" => Some(rp::VideoCodec::Vp9),
                "av1" => Some(rp::VideoCodec::Av1),
                _ => None,
            };
            let expect = rp::Expect {
                video: video.map(|t| rp::ExpectVideo {
                    codec: vcodec(&t.codec),
                    w: t.width.map(i64::from),
                    h: t.height.map(i64::from),
                    // only what was probed: an invented rate would hide RP rule 3
                    fps: t.fps.filter(|r| r.num > 0).map(|r| rp::Rational {
                        num: r.num as u64,
                        den: u64::from(r.den),
                    }),
                    timescale: (t.timescale != 0).then_some(i64::from(t.timescale)),
                }),
                audio: audio.map(|a| rp::ExpectAudio {
                    codec: (!a.codec.is_empty()).then(|| a.codec.clone()),
                    rate: Some(i64::from(a.rate)),
                    channels: layout_channels(&a.layout),
                }),
            };
            out.push(rp::Source {
                id: self.by_id[s].id.clone(),
                path: src.path.clone(),
                audio_track,
                fingerprint: (sha.is_some() || size.is_some()).then_some(rp::Fingerprint {
                    size,
                    mtime_ns: None,
                    sha256: sha,
                }),
                expect: (expect.video.is_some() || expect.audio.is_some()).then_some(expect),
            });
        }
        Ok(out)
    }
}

/// Where a part's audio comes from at media time `t`: `(source, offset)`, the source's
/// time being `t − offset` (a source is its own clock; a sync group's master audio
/// member, or its first audio member, through the `SyncSegment` holding `t`). `None`
/// when the group has no audio member or no segment covers `t`, or the member carries a
/// rate correction.
pub fn audio_source(doc: &Doc, media: MediaRef, t: i64) -> Option<(Id<Source>, i64)> {
    match media {
        MediaRef::Source(s) => Some((s, 0)),
        MediaRef::Group(g) => {
            let group = doc.get(g)?;
            let m = group.master_audio.and_then(|m| doc.get(m)).or_else(|| {
                doc.children::<SyncMember>(g.entity())
                    .filter_map(|m| doc.get(m))
                    .find(|m| m.role == MemberRole::Audio || m.role == MemberRole::Both)
            })?;
            if m.rate_ppm != 0.0 {
                return None;
            }
            Some((m.source, offset_at(doc, m.id, t)?))
        }
    }
}

/// Fades at every edge of `parts` that is not a continuous join (rule 7): none where a
/// part's audio carries straight on into the next one's.
pub(crate) fn join_fades(parts: &mut [rp::Part], out_fps: rp::Fps) {
    let fade = rp::Fade {
        samples: FADE_SAMPLES,
        shape: rp::FadeShape::Linear,
    };
    let joins: Vec<bool> = (0..parts.len())
        .map(|i| {
            i + 1 < parts.len() && {
                let (x, y) = (&parts[i], &parts[i + 1]);
                x.audio.source == y.audio.source
                    && gmclip_plan::validate::part_samples(x.out, out_fps)
                        .map(|n| x.audio.first_sample + n)
                        == Some(y.audio.first_sample)
            }
        })
        .collect();
    for i in 0..parts.len() {
        let cont_in = i > 0 && joins[i - 1];
        let cont_out = joins[i];
        parts[i].audio.fade_in = Some((!cont_in).then_some(fade));
        parts[i].audio.fade_out = Some((!cont_out).then_some(fade));
    }
}

/// The member offset at group time `t` (the `SyncSegment` whose range holds it).
fn offset_at(doc: &Doc, m: Id<SyncMember>, t: i64) -> Option<i64> {
    doc.children::<SyncSegment>(m.entity())
        .filter_map(|s| doc.get(s))
        .find(|s| s.src_range.start.0 <= t && t < s.src_range.end.0)
        .map(|s| s.offset.0)
}

/// Picture and audio for a part: `(picture source, picture offset, audio source,
/// audio offset)` at `src_in`, with the no-straddle check at the last frame.
/// The slowest and fastest part speeds (decision 0030).
pub const SPEED_RANGE: (f64, f64) = (0.25, 4.0);

/// Speeds whose sound is time-stretched with the pitch kept; outside it is muted
/// (decision 0030: a stretch past 2× either way smears speech).
pub const STRETCH_RANGE: (f64, f64) = (0.5, 2.0);

/// A speed as `(num, den)` in hundredths, reduced, inside [`SPEED_RANGE`].
fn hundredths(speed: f64) -> Result<(u64, u64), PlannerError> {
    if !speed.is_finite() || speed < SPEED_RANGE.0 || speed > SPEED_RANGE.1 {
        return err("a speed outside ¼× to 4×");
    }
    let num = (speed * 100.0).round() as u64;
    let g = gcd(num, 100);
    Ok((num / g, 100 / g))
}

/// `∫₀¹ curve` as `(num, den)` (decision 0033: a ramp's average speed is
/// `a + (b − a) · ∫curve`).
fn curve_integral(c: gmclip_model::entities::Curve) -> (i128, i128) {
    use gmclip_model::entities::Curve;
    match c {
        Curve::Linear | Curve::EaseInOut => (1, 2),
        Curve::EaseIn => (1, 3),
        Curve::EaseOut => (2, 3),
    }
}

fn ease_curve(c: gmclip_model::entities::Curve) -> rp::EaseCurve {
    use gmclip_model::entities::Curve;
    match c {
        Curve::Linear => rp::EaseCurve::Linear,
        Curve::EaseIn => rp::EaseCurve::EaseIn,
        Curve::EaseOut => rp::EaseCurve::EaseOut,
        Curve::EaseInOut => rp::EaseCurve::EaseInOut,
    }
}

/// A part's speed for RenderPlan (decisions 0030, 0033) and, when it ramps, its length in
/// output frames. A ramp's source span (clipped to the part, [`clock::speed_profile`]) on
/// the frame grid becomes output frames: it starts `r0 / a` frames in (the one tie rule)
/// and lasts its source length over its average speed `a + (b − a) · ∫curve`; the part
/// then runs at `b` until the source the plan's integral reaches is the part's.
///
/// [`clock::speed_profile`]: gmclip_model::clock::speed_profile
pub fn plan_part_speed(
    p: &Part,
    fpf: i64,
) -> Result<(Option<rp::PartSpeed>, Option<i64>), PlannerError> {
    use gmclip_model::clock::{effective_speed, mul_div_ties_earlier, speed_profile};
    let (Some(prof), Some(r)) = (speed_profile(p), p.ramp) else {
        return Ok((plan_speed(effective_speed(p))?, None));
    };
    let (an, ad) = hundredths(prof.a)?;
    let (bn, bd) = hundredths(prof.b)?;
    if an * bd == bn * ad {
        return Ok((plan_speed(prof.a)?, None));
    }
    let fpf_f = fpf as f64;
    let s = (p.src_out.0 - p.src_in.0) / fpf;
    let r0 = (prof.r0 / fpf_f).round() as i64;
    let r1 = ((prof.r1 / fpf_f).round() as i64).max(r0 + 1).min(s);
    let (i_an, i_ad, i_bn, i_bd) = (an as i128, ad as i128, bn as i128, bd as i128);
    let from =
        mul_div_ties_earlier(r0, i_ad, i_an).ok_or(PlannerError("a ramp too long".into()))?;
    let (cn, cd) = curve_integral(r.curve);
    // average speed over the ramp: a + (b − a)·c, as one fraction
    let avg_num = i_an * i_bd * cd + (i_bn * i_ad - i_an * i_bd) * cn;
    let avg_den = i_ad * i_bd * cd;
    let frames = mul_div_ties_earlier(r1 - r0, avg_den, avg_num)
        .ok_or(PlannerError("a ramp too long".into()))?
        .max(1);
    let stretch = |v: f64| (STRETCH_RANGE.0..=STRETCH_RANGE.1).contains(&v);
    let sp = rp::PartSpeed {
        num: an,
        den: ad,
        audio: (!(stretch(prof.a) && stretch(prof.b))).then_some(rp::SpeedAudio::Mute),
        ramp: Some(rp::SpeedRamp {
            to: rp::Rational { num: bn, den: bd },
            from: from.max(0) as u64,
            frames: frames as u64,
            curve: ease_curve(r.curve),
        }),
    };
    // the rest of the part at `b`: (s − advance(from + frames)) / b, rounded, ≥ 0
    let end = (from.max(0) + frames) as u64;
    let (un, ud) = sp
        .advance(end)
        .ok_or(PlannerError("a ramp too long".into()))?;
    let rest_num = (i128::from(s) * ud - un).max(0) * i_bd;
    let rest_den = ud * i_bn;
    let tail = i64::try_from((2 * rest_num + rest_den) / (2 * rest_den)).unwrap_or(0);
    Ok((Some(sp), Some(end as i64 + tail)))
}

/// A part's speed as RenderPlan's `num/den` (hundredths, reduced), or `None` at 1×
/// (decision 0030).
pub fn plan_speed(speed: f64) -> Result<Option<rp::PartSpeed>, PlannerError> {
    if speed == 1.0 {
        return Ok(None);
    }
    if !speed.is_finite() || speed < SPEED_RANGE.0 || speed > SPEED_RANGE.1 {
        return err("a speed outside ¼× to 4×");
    }
    let num = (speed * 100.0).round() as u64;
    if num == 100 {
        return Ok(None);
    }
    let g = gcd(num, 100);
    Ok(Some(rp::PartSpeed {
        num: num / g,
        den: 100 / g,
        audio: (!(STRETCH_RANGE.0..=STRETCH_RANGE.1).contains(&speed))
            .then_some(rp::SpeedAudio::Mute),
        ramp: None,
    }))
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// A media's length in output frames: `⌊n · out / src⌋`, at least 1 (decision 0028).
fn out_len(n: u64, src: (u64, u64), out: rp::Fps) -> u64 {
    let (on, od) = out.rate();
    let v = u128::from(n) * u128::from(on) * u128::from(src.1)
        / (u128::from(od) * u128::from(src.0).max(1));
    u64::try_from(v).unwrap_or(u64::MAX).max(1)
}

/// `Some(rate)` when a picture's nominal rate differs from the output's (decision 0017).
fn other_rate(rate: Option<(u64, u64)>, out: rp::Fps) -> Option<rp::Rational> {
    let (on, od) = out.rate();
    rate.filter(|&(n, d)| u128::from(n) * u128::from(od) != u128::from(on) * u128::from(d))
        .map(|(num, den)| rp::Rational { num, den })
}

/// A stored video asset as a plan source: the source, its size, rate and length in its
/// own frames.
type AssetSource = (rp::Source, (u32, u32), (u64, u64), u64);

/// A stored video asset as a plan source (decision 0028).
fn asset_source(asset: &Asset, host: &dyn PlanHost) -> Result<AssetSource, PlannerError> {
    let probe = asset
        .probe
        .filter(|_| asset.media_type.starts_with("video/"))
        .ok_or(PlannerError("a video asset without its probe".into()))?;
    let fps = probe
        .fps
        .filter(|r| r.num > 0 && r.den > 0)
        .ok_or(PlannerError("a video asset without its rate".into()))?;
    let rate = (fps.num as u64, u64::from(fps.den));
    let frames = probe
        .frames
        .filter(|&n| n > 0)
        .ok_or(PlannerError("a video asset without its length".into()))?;
    let src = rp::Source {
        id: PId::new(format!("asset-{}", asset.id.0).to_ascii_lowercase()).map_err(PlannerError)?,
        path: host.asset_path(asset).ok_or(PlannerError(
            "a video asset this machine does not have".into(),
        ))?,
        audio_track: None,
        fingerprint: Some(rp::Fingerprint {
            size: Some(asset.size),
            mtime_ns: None,
            sha256: Some(Sha256::new(asset.sha256.clone()).map_err(PlannerError)?),
        }),
        expect: Some(rp::Expect {
            video: Some(rp::ExpectVideo {
                codec: None,
                w: Some(i64::from(probe.w)),
                h: Some(i64::from(probe.h)),
                fps: Some(rp::Rational {
                    num: rate.0,
                    den: rate.1,
                }),
                timescale: None,
            }),
            audio: None,
        }),
    };
    Ok((src, (probe.w, probe.h), rate, frames))
}

/// A stored sound (or a video's sound) as a plan source for an audio overlay (decision
/// 0051): read at whatever rate it has.
fn sound_asset_source(asset: &Asset, host: &dyn PlanHost) -> Result<rp::Source, PlannerError> {
    Ok(rp::Source {
        id: PId::new(format!("asset-{}", asset.id.0).to_ascii_lowercase()).map_err(PlannerError)?,
        path: host
            .asset_path(asset)
            .ok_or(PlannerError("a sound this machine does not have".into()))?,
        audio_track: None,
        fingerprint: Some(rp::Fingerprint {
            size: Some(asset.size),
            mtime_ns: None,
            sha256: Some(Sha256::new(asset.sha256.clone()).map_err(PlannerError)?),
        }),
        expect: None,
    })
}

/// Where the cut's voice speaks (decision 0051), on the output sample clock: every word of
/// each part's transcript the part plays, gaps under [`SPEECH_GAP_MS`] closed, in order
/// and apart. What a ducked sound ducks under.
fn speech_ranges(
    doc: &Doc,
    view: &crate::analysis::AnalysisView,
    ctx: &Ctx<'_>,
    total: u64,
) -> Vec<Range> {
    let mut v: Vec<[u64; 2]> = Vec::new();
    let to_samples = |flicks: i64| {
        u64::try_from(i128::from(flicks) * i128::from(AUDIO_RATE) / i128::from(FLICKS_PER_SECOND))
            .unwrap_or(0)
    };
    for (pid, (f0, _)) in &ctx.edges {
        let Some(p) = doc.get(*pid) else { continue };
        let owner = match p.media {
            MediaRef::Source(s) => s.0.to_string(),
            MediaRef::Group(g) => g.0.to_string(),
        };
        let Some(t) = view.active(doc, &owner) else {
            continue;
        };
        let at = i64::try_from(*f0).unwrap_or(0) * ctx.fpf;
        let out = |src: i64| {
            at + gmclip_model::clock::src_to_local(p, gmclip_model::Flicks(src - p.src_in.0)).0
        };
        for w in &t.words {
            let (s, e) = (w.start.max(p.src_in.0), w.end.min(p.src_out.0));
            if e > s {
                v.push([to_samples(out(s)), to_samples(out(e)).min(total)]);
            }
        }
    }
    v.sort_unstable();
    let gap = SPEECH_GAP_MS * (AUDIO_RATE as u64 / 1000);
    let mut merged: Vec<[u64; 2]> = Vec::new();
    for r in v.into_iter().filter(|r| r[1] > r[0]) {
        match merged.last_mut() {
            Some(l) if r[0] <= l[1] + gap => l[1] = l[1].max(r[1]),
            _ => merged.push(r),
        }
    }
    merged.into_iter().map(Range).collect()
}

/// Words closer than this are one stretch of speech to duck under (decision 0051).
pub const SPEECH_GAP_MS: u64 = 300;

/// Where a picture of `w × h` lands on a `cw × ch` canvas, `(crop, place)` in picture and
/// canvas px (decision 0028): `cover` fills the canvas with the picture's centre at the
/// canvas's aspect; `contain` shows all of it, centred. A stretch is planned as a cover
/// (RenderPlan keeps every crop at its place's aspect, rule 5).
fn fit_rects(w: f64, h: f64, cw: f64, ch: f64, fit: BackdropFit) -> (rp::Rect, rp::Rect) {
    let canvas = rp::Rect {
        x: 0.0,
        y: 0.0,
        w: cw,
        h: ch,
    };
    match fit {
        BackdropFit::Contain => {
            let s = (cw / w).min(ch / h);
            let (pw, ph) = (w * s, h * s);
            (
                rp::Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                },
                rp::Rect {
                    x: (cw - pw) / 2.0,
                    y: (ch - ph) / 2.0,
                    w: pw,
                    h: ph,
                },
            )
        }
        BackdropFit::Cover | BackdropFit::Stretch => {
            let crop = if w * ch > h * cw {
                let cropw = h * cw / ch;
                rp::Rect {
                    x: (w - cropw) / 2.0,
                    y: 0.0,
                    w: cropw,
                    h,
                }
            } else {
                let croph = w * ch / cw;
                rp::Rect {
                    x: 0.0,
                    y: (h - croph) / 2.0,
                    w,
                    h: croph,
                }
            };
            (crop, canvas)
        }
    }
}

/// A framing's backdrop (SPEC §5.11, decision 0028): a stored video (on the output clock,
/// looping or holding its last frame) or a still image, fitted to the canvas. Returns
/// the plan background and the source or image it reads.
fn plan_backdrop(
    doc: &Doc,
    asset: Id<Asset>,
    r#loop: bool,
    fit: BackdropFit,
    host: &dyn PlanHost,
    canvas: (f64, f64),
    out: rp::Fps,
) -> Result<(rp::Background, Option<rp::Source>, Option<rp::Image>), PlannerError> {
    let a = doc
        .get(asset)
        .ok_or(PlannerError("the backdrop's asset is gone".into()))?;
    let rgb = Rgb::new(BLURRED_FALLBACK.to_owned()).map_err(PlannerError)?;
    if a.media_type.starts_with("video/") {
        let (src, (w, h), rate, frames) = asset_source(a, host)?;
        let (crop, place) = fit_rects(f64::from(w), f64::from(h), canvas.0, canvas.1, fit);
        let video = rp::ClipPicture {
            source: src.id.clone(),
            first_frame: 0,
            fps: other_rate(Some(rate), out),
            frames: Some(out_len(frames, rate, out)),
            r#loop: r#loop.then_some(true),
        };
        return Ok((
            rp::Background::Backdrop {
                rgb,
                crop,
                place,
                video: Some(video),
                image: None,
            },
            Some(src),
            None,
        ));
    }
    let probe = a
        .probe
        .ok_or(PlannerError("a backdrop image without its size".into()))?;
    let id = PId::new(format!("backdrop-{}", a.id.0).to_ascii_lowercase()).map_err(PlannerError)?;
    let image = rp::Image {
        id: id.clone(),
        path: host.asset_path(a).ok_or(PlannerError(
            "a backdrop image this machine does not have".into(),
        ))?,
        sha256: Sha256::new(a.sha256.clone()).map_err(PlannerError)?,
        w: u64::from(probe.w),
        h: u64::from(probe.h),
    };
    let (crop, place) = fit_rects(
        f64::from(probe.w),
        f64::from(probe.h),
        canvas.0,
        canvas.1,
        fit,
    );
    Ok((
        rp::Background::Backdrop {
            rgb,
            crop,
            place,
            video: None,
            image: Some(id),
        },
        None,
        Some(image),
    ))
}

/// An overlay's moving point on the output (decision 0031): each key of its track that
/// a part of the cut plays (every take that plays it, decision 0033), in output order, as
/// an output frame inside `[a, z)` and the canvas pixel where
/// the layer showing that video draws the point (the topmost visible picture layer whose
/// crop holds it).
fn track_keys(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    t: &OverlayTrack,
    (a, z): (u64, u64),
    fpf: i64,
    canvas: (f64, f64),
) -> Vec<rp::TrackKey> {
    let (cw, ch) = canvas;
    let parts: Vec<&Part> = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .filter(|p| p.media == t.media)
        .collect();
    let Some(dims) = crate::commands::part::picture_size(doc, t.media) else {
        return Vec::new();
    };
    let (sw, sh) = (f64::from(dims.0), f64::from(dims.1));
    let mut out: Vec<rp::TrackKey> = Vec::new();
    // every take of the video plays its own keys (decision 0033: a track across takes,
    // in any order, a moment shown twice twice)
    let played = parts.iter().flat_map(|p| {
        t.keys
            .iter()
            .filter(|k| p.src_in.0 <= k.t.0 && k.t.0 < p.src_out.0)
            .map(move |k| (*p, k))
    });
    for (p, k) in played {
        let Some(ct) = gmclip_model::clock::src_to_cut(doc, p.id, k.t) else {
            continue;
        };
        let f = frame_of_cut_time(ct.0, fpf);
        if f < a || f >= z {
            continue;
        }
        let Some(stack) = effective(doc, p.id, framing, k.t) else {
            continue;
        };
        let mut layers: Vec<&gmclip_model::effective::LayerValues> = stack
            .layers
            .iter()
            .map(|l| &l.values)
            .filter(|v| {
                v.visible
                    && v.opacity > 0.0
                    && v.cover.is_none()
                    && v.crop.w > 0.0
                    && v.crop.h > 0.0
                    && (v.crop.x..=v.crop.x + v.crop.w).contains(&k.x)
                    && (v.crop.y..=v.crop.y + v.crop.h).contains(&k.y)
            })
            .collect();
        layers.sort_by_key(|v| v.z);
        let Some(v) = layers.last() else {
            continue;
        };
        // the place's height from its width (SPEC §4.2)
        let ph = v.place.w * (cw / ch) * (v.crop.h * sh) / (v.crop.w * sw);
        let x = (v.place.x + (k.x - v.crop.x) / v.crop.w * v.place.w) * cw;
        let y = (v.place.y + (k.y - v.crop.y) / v.crop.h * ph) * ch;
        out.push(rp::TrackKey { frame: f, x, y });
    }
    out.sort_by_key(|k| k.frame);
    out.dedup_by_key(|k| k.frame);
    out
}

/// The picture source of a media at media time `t`, and the source's time offset
/// (`source time = t − offset`): a source is its own clock; a sync group shows its
/// picture member through the segment holding `t`.
fn picture_of(doc: &Doc, media: MediaRef, t: i64) -> Result<(Id<Source>, i64), PlannerError> {
    match media {
        MediaRef::Source(s) => Ok((s, 0)),
        MediaRef::Group(g) => {
            let group = doc
                .get(g)
                .ok_or(PlannerError("a video overlay's sync group is gone".into()))?;
            let m = group
                .picture
                .and_then(|m| doc.get(m))
                .or_else(|| {
                    doc.children::<SyncMember>(g.entity())
                        .filter_map(|m| doc.get(m))
                        .find(|m| matches!(m.role, MemberRole::Picture | MemberRole::Both))
                })
                .ok_or(PlannerError("the group has no picture member".into()))?;
            let off = offset_at(doc, m.id, t).ok_or(PlannerError(
                "no sync segment covers the video overlay's start".into(),
            ))?;
            Ok((m.source, off))
        }
    }
}

fn media_of(
    doc: &Doc,
    p: &Part,
    last: i64,
) -> Result<(Id<Source>, i64, Id<Source>, i64), PlannerError> {
    match p.media {
        MediaRef::Source(s) => Ok((s, 0, s, 0)),
        MediaRef::Group(g) => {
            let group = doc
                .get(g)
                .ok_or(PlannerError("a part's sync group is gone".into()))?;
            let members: Vec<&SyncMember> = doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .collect();
            let pick = |want: Option<Id<SyncMember>>, role: MemberRole| {
                want.and_then(|w| doc.get(w)).or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|m| m.role == role || m.role == MemberRole::Both)
                })
            };
            let pic = pick(group.picture, MemberRole::Picture)
                .ok_or(PlannerError("the group has no picture member".into()))?;
            let aud = pick(p.audio_from.or(group.master_audio), MemberRole::Audio)
                .ok_or(PlannerError("the group has no audio member".into()))?;
            let off = |m: &SyncMember| -> Result<i64, PlannerError> {
                if m.rate_ppm != 0.0 {
                    return err("a sync member with a rate correction (not in RenderPlan v0)");
                }
                let a = offset_at(doc, m.id, p.src_in.0).ok_or(PlannerError(
                    "no sync segment covers the part's start".into(),
                ))?;
                if offset_at(doc, m.id, last) != Some(a) {
                    return err("a part straddles a sync offset change");
                }
                Ok(a)
            };
            Ok((pic.source, off(pic)?, aud.source, off(aud)?))
        }
    }
}

/// The sounds heard under a part's audio (decision 0051): every other member of its
/// sync group with an audio track, at the part's `under_db` or else the member's own,
/// louder than [`UNDER_OFF_DB`], as `(source, offset, gain)`. Members with a rate
/// correction or an offset change inside the part are left out (as a layer would refuse
/// them), and so is everything on a part that changes speed.
fn under_of(doc: &Doc, p: &Part, aud: Id<Source>, last: i64) -> Vec<(Id<Source>, i64, f64)> {
    let MediaRef::Group(g) = p.media else {
        return Vec::new();
    };
    if p.speed != 1.0 || p.ramp.is_some() {
        return Vec::new();
    }
    doc.children::<SyncMember>(g.entity())
        .filter_map(|m| doc.get(m))
        .filter(|m| m.source != aud && m.rate_ppm == 0.0)
        .filter(|m| {
            doc.get(m.source)
                .is_some_and(|s| !s.probe.audio_tracks.is_empty())
        })
        .filter_map(|m| {
            let db = p.under_db.or(m.under_db)?;
            let off = offset_at(doc, m.id, p.src_in.0)?;
            (db > UNDER_OFF_DB && offset_at(doc, m.id, last) == Some(off)).then_some((
                m.source,
                off,
                db.min(12.0),
            ))
        })
        .collect()
}

/// `effective()` is read once per part (at `src_in`), so anything that changes a layer
/// inside the part — a visual split, or a hold or hide whose range starts or ends inside
/// it — is refused rather than applied to the whole part or not at all.
/// The output frames (relative to the part's first) where a cut visual split inside
/// the part starts a new look: each becomes its own plan part, joined continuously.
fn split_points(
    doc: &Doc,
    p: &Part,
    framing: Id<Framing>,
    fpf: i64,
) -> Result<Vec<i64>, PlannerError> {
    let mut v = Vec::new();
    for s in gmclip_model::effective::splits_of(doc, p.id, framing) {
        if s.at_src > p.src_in && s.at_src < p.src_out {
            if s.at_src.0 % fpf != 0 {
                return err("a visual split off the frame grid");
            }
            let k = (s.at_src.0 - p.src_in.0) / fpf;
            if v.last() != Some(&k) {
                v.push(k);
            }
        }
    }
    Ok(v)
}

/// The model part behind each plan part [`plan_cut`] makes for `cut` in `framing`, in
/// plan order (a part with cut visual splits inside it is several plan parts, and so is
/// one a layout section starts or ends inside, decision 0046).
pub fn plan_part_owners(doc: &Doc, cut: Id<Cut>, framing: Id<Framing>) -> Vec<Id<Part>> {
    // the layout sections' edges, in output frames, and each part's first output frame
    let fpf = doc.get(framing).and_then(|f| framing_fpf(f.fps));
    let lens = fpf
        .map(|fpf| part_out_lens(doc, cut, fpf))
        .unwrap_or_default();
    let spans = match fpf {
        Some(fpf) => {
            let ctx = Ctx {
                doc,
                cut,
                fpf,
                edges: BTreeMap::new(),
                canvas: (0, 0),
            };
            let total = lens.iter().map(|x| x.1).sum();
            sections::layout_spans(&ctx, framing, total, (1, 1))
        }
        None => Vec::new(),
    };
    let mut base = 0u64;
    let mut v = Vec::new();
    for pid in doc.children::<Part>(cut.entity()) {
        let Some(p) = doc.get(pid) else { continue };
        let len = lens.iter().find(|x| x.0 == pid).map(|x| (x.1, x.2));
        if spans.is_empty() || fpf.is_none() {
            let mut at: Vec<i64> = gmclip_model::effective::splits_of(doc, pid, framing)
                .iter()
                .filter(|s| s.at_src > p.src_in && s.at_src < p.src_out)
                .map(|s| s.at_src.0)
                .collect();
            at.dedup();
            v.extend(std::iter::repeat_n(pid, 1 + at.len()));
        } else {
            let fpf = fpf.unwrap_or(1);
            // as the part loop: its split points (frames into it) and the section edges
            let mut at: Vec<i64> = gmclip_model::effective::splits_of(doc, pid, framing)
                .iter()
                .filter(|s| s.at_src > p.src_in && s.at_src < p.src_out)
                .map(|s| (s.at_src.0 - p.src_in.0) / fpf)
                .collect();
            if let Some((n, false)) = len {
                at.extend(sections::inner_edges(&spans, base, n));
            }
            at.sort_unstable();
            at.dedup();
            v.extend(std::iter::repeat_n(pid, 1 + at.len()));
        }
        base += len.map_or(0, |x| x.0);
    }
    v
}

/// Each part's length in output frames as the part loop plans it, and whether it plays at
/// another speed (no section edge splits it). A part that does not plan has none.
fn part_out_lens(doc: &Doc, cut: Id<Cut>, fpf: i64) -> Vec<(Id<Part>, u64, bool)> {
    let mut v = Vec::new();
    for pid in doc.children::<Part>(cut.entity()) {
        let Some(p) = doc.get(pid) else { continue };
        let Ok((speed, ramp_len)) = plan_part_speed(p, fpf) else {
            continue;
        };
        let src_in = nearest_frame(p.src_in.0, fpf);
        let src_out = nearest_frame(p.src_out.0, fpf);
        if src_in < 0 || src_out <= src_in {
            continue;
        }
        let (a, b) = (src_in / fpf, src_out / fpf);
        let n = match speed {
            Some(_) if ramp_len.is_some() => ramp_len,
            Some(sp) => gmclip_model::clock::mul_div_ties_earlier(
                b - a,
                i128::from(sp.den),
                i128::from(sp.num),
            ),
            None => Some(b - a),
        };
        if let Some(n) = n.filter(|n| *n >= 1) {
            v.push((pid, n as u64, speed.is_some()));
        }
    }
    v
}

fn whole_part_only(doc: &Doc, p: &Part) -> Result<(), PlannerError> {
    // visual splits inside a part become plan parts of their own (`split_points`); an
    // eased one carries `layers[].ease` (RenderPlan `/1`, decision 0016)
    let partial = |from: &gmclip_model::anchor::Anchor, to: &gmclip_model::anchor::Anchor| match (
        gmclip_model::clock::anchor_src(doc, from),
        gmclip_model::clock::anchor_src(doc, to),
    ) {
        (Some(a), Some(b)) => {
            let overlaps = a < p.src_out && b > p.src_in;
            overlaps && !(a <= p.src_in && b >= p.src_out)
        }
        _ => false,
    };
    for h in doc
        .children::<ContentHold>(p.cut.entity())
        .filter_map(|h| doc.get(h))
    {
        if partial(&h.from, &h.to) {
            return err("a content hold that starts or ends inside a part");
        }
    }
    for h in doc
        .children::<HideRoles>(p.cut.entity())
        .filter_map(|h| doc.get(h))
    {
        if partial(&h.from, &h.to) {
            return err("a hide that starts or ends inside a part");
        }
    }
    Ok(())
}

fn layer_of(
    v: &gmclip_model::effective::LayerValues,
    id: String,
    canvas: (u32, u32),
    dims: (u32, u32),
    pic: Id<Source>,
) -> Result<rp::Layer, PlannerError> {
    // a cover layer (decision 0021) obscures a region of the picture: any region role
    let cover = match (v.kind, v.cover) {
        (LayerKind::Cover, Some(c)) => Some(cover_of(c)?),
        (LayerKind::Cover, None) => return err("a cover layer with no cover style"),
        (LayerKind::SourceView, None) => None,
        _ => {
            return err(
                "a layer feature outside the M0 set (kind, chroma key, mirror, grade, cover, feather)",
            );
        }
    };
    let role = if cover.is_some() {
        cover_role_name(v.role)
    } else {
        role_name(v.role)
    }
    .ok_or(PlannerError(
        "a layer role RenderPlan v0 has no name for".into(),
    ))?;
    if matches!(v.matte, Matte::Chroma(_))
        || (cover.is_some() && !matches!(v.matte, Matte::None))
        || v.mirror
        || v.grade.is_some()
        || v.feather != Feather::default()
    {
        return err(
            "a layer feature outside the M0 set (kind, chroma key, mirror, grade, cover, feather)",
        );
    }
    let (cw, ch) = (f64::from(canvas.0), f64::from(canvas.1));
    let (sw, sh) = (f64::from(dims.0), f64::from(dims.1));
    let time_map = match v.time_map {
        TimeMap::Live => rp::TimeMap::Live,
        TimeMap::FreezeFirst => rp::TimeMap::FreezeFirst,
        TimeMap::Hold(HoldAt::Frame(f)) => {
            if f.source != pic || f.frame_index < 0 {
                return err("a hold frame on another source than the part's picture");
            }
            rp::TimeMap::Hold {
                frame: f.frame_index as u64,
                provenance: None,
            }
        }
        TimeMap::Hold(HoldAt::AtSrc(_)) => {
            return err("an at_src hold (needs the anchor resolver)");
        }
    };
    let cam = v.role == LayerRole::Region(RegionRole::Cam);
    Ok(rp::Layer {
        id: PId::new(id).map_err(PlannerError)?,
        role: RoleName::new(role).map_err(PlannerError)?,
        picture_source: None,
        crop: rp::Rect {
            x: v.crop.x * sw,
            y: v.crop.y * sh,
            w: v.crop.w * sw,
            h: v.crop.h * sh,
        },
        place: rp::Rect {
            x: v.place.x * cw,
            y: v.place.y * ch,
            w: v.place.w * cw,
            h: v.place.derived_h(&v.crop, canvas, dims) * ch,
        },
        z: i64::from(v.z),
        visible: (!v.visible).then_some(false),
        opacity: (v.opacity != 1.0).then_some(v.opacity),
        resample: None,
        // The converter writes a time map on every layer a hold can reach (all but the
        // camera, SPEC §4.3 ContentHold default roles); the camera's only when not live.
        time_map: (!cam || time_map != rp::TimeMap::Live).then_some(time_map),
        matte: roto_of(&v.matte, (sw, sh))?,
        ease: None,
        cursor: None,
        // a cover is never framed (decision 0046)
        frame: if cover.is_some() {
            None
        } else {
            v.frame.as_ref().map(frame_of).transpose()?
        },
        first_frame: None,
        fps: None,
        cover,
        track: None,
    })
}

/// A plan layer's opacity as drawn (0 when hidden).
fn shown_opacity(l: &rp::Layer) -> f64 {
    if l.visible == Some(false) {
        0.0
    } else {
        l.opacity.unwrap_or(1.0)
    }
}

fn lerp_rect(a: rp::Rect, b: rp::Rect, t: f64) -> rp::Rect {
    let l = |a: f64, b: f64| a + (b - a) * t;
    rp::Rect {
        x: l(a.x, b.x),
        y: l(a.y, b.y),
        w: l(a.w, b.w),
        h: l(a.h, b.h),
    }
}

/// Decision 0046: a layout section's ease on `layer` (its own values where it settles)
/// from `(crop, place, opacity as drawn)` over `frames`, ease-in-out. A layer the ease
/// hides or shows fades: it is drawn throughout, at opacity 0 where it is hidden.
fn ease_section_layer(layer: &mut rp::Layer, from: (rp::Rect, rp::Rect, f64), frames: u64) {
    let (crop, place, op) = from;
    if layer.visible == Some(false) {
        if op <= 0.0 {
            return;
        }
        layer.visible = None;
        layer.opacity = Some(0.0);
    }
    let to = shown_opacity(layer);
    if crop == layer.crop && place == layer.place && op == to {
        return;
    }
    layer.ease = Some(rp::LayerEase {
        from_crop: crop,
        from_place: place,
        from_opacity: (op != to).then_some(op),
        frames: frames.max(1),
        curve: rp::EaseCurve::EaseInOut,
    });
}

/// A model layer frame as the plan's (decision 0046).
fn frame_of(f: &gmclip_model::entities::LayerFrame) -> Result<rp::LayerFrame, PlannerError> {
    let rgb = |c: &str| {
        gmclip_plan::scalars::Rgb::new(c.to_owned())
            .map_err(|_| PlannerError(format!("a frame colour {c:?} that is not #RRGGBB")))
    };
    Ok(rp::LayerFrame {
        radius: f.radius,
        shadow: f
            .shadow
            .as_ref()
            .map(|s| -> Result<rp::FrameShadow, PlannerError> {
                Ok(rp::FrameShadow {
                    blur: s.blur,
                    offset_y: s.offset_y,
                    opacity: s.opacity,
                    rgb: rgb(&s.colour)?,
                })
            })
            .transpose()?,
        border: f
            .border
            .as_ref()
            .map(|b| -> Result<rp::FrameBorder, PlannerError> {
                Ok(rp::FrameBorder {
                    width: b.width,
                    rgb: rgb(&b.colour)?,
                })
            })
            .transpose()?,
    })
}

/// A tracked point at source time `t` (normalised picture coordinates): straight lines
/// between keys, held past the ends.
fn point_at(keys: &[gmclip_model::entities::TrackPoint], t: i64) -> Option<(f64, f64)> {
    let first = keys.first()?;
    if t <= first.t.0 {
        return Some((first.x, first.y));
    }
    let i = keys.partition_point(|k| k.t.0 <= t);
    let (a, b) = match (keys.get(i.wrapping_sub(1)), keys.get(i)) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => return Some((a.x, a.y)),
        _ => return Some((first.x, first.y)),
    };
    let u = (t - a.t.0) as f64 / (b.t.0 - a.t.0).max(1) as f64;
    Some((a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u))
}

/// A pinned cover's track on one plan part (decision 0033): the point at the part's first
/// and last frames (interpolated on the video's clock, held past the track's ends) and at
/// every key the part plays, mapped to the canvas through the cover's own crop and place
/// (which lie over what it covers), with `(x, y)` where the first key puts it. Each move
/// is held so the cover's crop stays on the picture. `None` when the part shows another
/// media.
#[allow(clippy::too_many_arguments)]
fn cover_track(
    t: &OverlayTrack,
    p: &Part,
    layer: &rp::Layer,
    dims: (u32, u32),
    fpf: i64,
    (sa, sb): (i64, i64),
    cursor: u64,
) -> Option<rp::LayerTrack> {
    if t.media != p.media || t.keys.is_empty() || sb <= sa {
        return None;
    }
    let (c, q) = (layer.crop, layer.place);
    if c.w <= 0.0 || c.h <= 0.0 {
        return None;
    }
    let (sw, sh) = (f64::from(dims.0), f64::from(dims.1));
    let (kx, ky) = (q.w / c.w, q.h / c.h);
    let canvas = |(x, y): (f64, f64)| (q.x + (x * sw - c.x) * kx, q.y + (y * sh - c.y) * ky);
    let from = canvas((t.keys[0].x, t.keys[0].y));
    // the crop stays on the picture: the move in canvas px, held to it
    let (lo_x, hi_x) = (-c.x * kx, (sw - c.x - c.w) * kx);
    let (lo_y, hi_y) = (-c.y * ky, (sh - c.y - c.h) * ky);
    let held = |(x, y): (f64, f64)| {
        (
            from.0 + (x - from.0).clamp(lo_x.min(0.0), hi_x.max(0.0)),
            from.1 + (y - from.1).clamp(lo_y.min(0.0), hi_y.max(0.0)),
        )
    };
    let src_of = |k: i64| gmclip_model::clock::local_to_src(p, gmclip_model::Flicks(k * fpf)).0;
    let mut keys: Vec<rp::TrackKey> = Vec::new();
    let mut push = |frame: u64, pt: (f64, f64)| {
        let (x, y) = held(canvas(pt));
        if keys.last().is_none_or(|l| l.frame < frame) {
            keys.push(rp::TrackKey { frame, x, y });
        }
    };
    let (s0, s1) = (src_of(sa), src_of(sb - 1));
    push(cursor, point_at(&t.keys, s0)?);
    for k in &t.keys {
        if k.t.0 <= s0 || k.t.0 >= s1 || k.t < p.src_in || k.t >= p.src_out {
            continue;
        }
        let local = gmclip_model::clock::src_to_local(p, gmclip_model::Flicks(k.t.0 - p.src_in.0));
        let f = frame_of_cut_time(local.0, fpf) as i64;
        if f > sa && f < sb - 1 {
            push(cursor + (f - sa) as u64, (k.x, k.y));
        }
    }
    push(cursor + (sb - 1 - sa) as u64, point_at(&t.keys, s1)?);
    Some(rp::LayerTrack {
        x: from.0,
        y: from.1,
        keys,
    })
}

/// A cover layer's style in plan terms (decision 0021): sizes in canvas px, the solid
/// colour `0xRRGGBB` as `#rrggbb`.
fn cover_of(c: CoverStyle) -> Result<rp::LayerCover, PlannerError> {
    let px = |v: f64| {
        if v.is_finite() {
            Ok(v.clamp(1.0, 400.0))
        } else {
            err("a cover size that is not a number")
        }
    };
    Ok(match c {
        CoverStyle::Blur { radius } => rp::LayerCover::Blur {
            radius: px(radius)?,
        },
        CoverStyle::Pixelate { block } => rp::LayerCover::Pixelate { block: px(block)? },
        CoverStyle::Solid { colour } => rp::LayerCover::Solid {
            rgb: gmclip_plan::scalars::Rgb::new(format!("#{:06x}", colour & 0x00ff_ffff))
                .map_err(PlannerError)?,
        },
    })
}

/// A cover layer's role name: any region role, `detail` or `other` (RP rule 5).
pub fn cover_role_name(r: LayerRole) -> Option<&'static str> {
    Some(match r {
        LayerRole::Region(RegionRole::Chat) => "chat",
        LayerRole::Region(RegionRole::Alerts) => "alerts",
        LayerRole::Region(RegionRole::Banner) => "banner",
        LayerRole::Region(RegionRole::Private) => "private",
        LayerRole::Region(RegionRole::CaptionsBurned) => "captions_burned",
        LayerRole::Other => "other",
        _ => return role_name(r),
    })
}

/// A roto matte in plan terms (decision 0013): soften in source pixels, strokes from
/// normalised source coordinates (radius a fraction of the source height) to pixels.
/// Envelopes in output order with no overlap (RenderPlan rule 7): one that starts inside
/// the one before starts where it ends instead, one wholly inside it is left out, and
/// ramps that no longer fit shrink in proportion. Gain segments overlap only after the
/// parts their anchors sit in were moved; a plan without overlaps is unchanged.
fn no_overlaps(envelopes: &mut Vec<rp::Envelope>) {
    let mut spans: Vec<(u64, u64)> = envelopes
        .iter()
        .map(|e| (e.out_samples.start(), e.out_samples.end()))
        .collect();
    spans.sort_unstable();
    if spans.windows(2).all(|w| w[1].0 >= w[0].1) {
        return;
    }
    envelopes.sort_by_key(|e| e.out_samples.start());
    let mut end = 0u64;
    envelopes.retain_mut(|e| {
        let (a, z) = (e.out_samples.start().max(end), e.out_samples.end());
        if z <= a {
            return false;
        }
        if a != e.out_samples.start() {
            e.out_samples = Range([a, z]);
        }
        let (len, ramps) = (z - a, e.ramp_in_samples + e.ramp_out_samples);
        if ramps > len {
            e.ramp_in_samples = e.ramp_in_samples * len / ramps;
            e.ramp_out_samples = len - e.ramp_in_samples;
        }
        end = z;
        true
    });
}

fn roto_of(m: &Matte, (sw, sh): (f64, f64)) -> Result<Option<rp::LayerMatte>, PlannerError> {
    let Matte::Roto {
        model,
        soften,
        strokes,
    } = m
    else {
        return Ok(None);
    };
    if !soften.is_finite() || strokes.len() > 256 {
        return err("a roto matte with a bad soften or more than 256 strokes");
    }
    let strokes: Vec<rp::MatteStroke> = strokes
        .iter()
        .filter(|s| s.radius.is_finite() && s.radius > 0.0 && !s.points.is_empty())
        .map(|s| rp::MatteStroke {
            mode: match s.mode {
                gmclip_model::entities::StrokeMode::Add => rp::StrokeMode::Add,
                gmclip_model::entities::StrokeMode::Subtract => rp::StrokeMode::Subtract,
            },
            radius: (s.radius * sh).clamp(0.01, 4096.0),
            feather: (s.feather > 0.0).then_some(s.feather.clamp(0.0, 1.0)),
            points: s
                .points
                .iter()
                .take(4096)
                .map(|p| rp::Point {
                    x: p[0] * sw,
                    y: p[1] * sh,
                })
                .collect(),
        })
        .collect();
    Ok(Some(rp::LayerMatte {
        kind: gmclip_plan::consts::Roto,
        model: model.clone(),
        soften: (*soften > 0.0).then_some(soften.clamp(0.0, 32.0)),
        strokes: (!strokes.is_empty()).then_some(strokes),
    }))
}

/// The M0 export block (H.264 High at the level the canvas fits, 4.0 up to 1080p30; 16/24 Mbit/s VBR, 1 s closed GOPs, AAC-LC
/// 192 kbit/s, bt709 limited, moov first, no edit lists): what the converter writes
/// for every golden cut, and the only output the M1 planner plans.
pub fn m0_output(w: u32, h: u32, fps: rp::Fps, frames: u64) -> Result<rp::Output, PlannerError> {
    Ok(rp::Output {
        canvas: rp::Canvas {
            w: Even::new(u64::from(w)).map_err(PlannerError)?,
            h: Even::new(u64::from(h)).map_err(PlannerError)?,
        },
        fps,
        frames,
        color: Some(rp::Color {
            matrix: Some(Bt709),
            transfer: Some(Bt709),
            primaries: Some(Bt709),
            range: Some(rp::ColorRange::Limited),
        }),
        video: rp::Video {
            codec: H264,
            profile: Some(rp::VideoProfile::High),
            level: Some(
                gmclip_plan::validate::fitting_level(w.into(), h.into(), fps.rounded()).into(),
            ),
            bframes: Some(2),
            gop_frames: Some(fps.rounded()),
            closed_gop: Some(true),
            rate: Some(rp::VideoRate {
                mode: Vbr,
                target_bps: Some(16_000_000),
                max_bps: Some(24_000_000),
            }),
            timescale: Some(15_360),
        },
        audio: rp::OutputAudio {
            codec: AacLc,
            rate: ConstU64,
            channels: ConstU64,
            bitrate: Some(192_000),
        },
        container: Some(rp::Container {
            moov_first: Some(true),
            edit_lists: Some(ConstFalse),
        }),
    })
}

/// Plans `cut` in `framing`. See the module docs.
pub fn plan_cut(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
) -> Result<rp::Plan, PlannerError> {
    plan_cut_with(doc, cut, framing, host, None)
}

/// [`plan_cut`] with the project's transcripts, which heard captions are built from.
pub fn plan_cut_with(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
    view: Option<&crate::analysis::AnalysisView>,
) -> Result<rp::Plan, PlannerError> {
    plan_cut_traced(doc, cut, framing, host, view).map(|(p, _)| p)
}

/// Which words a built caption cue shows (the Short tab's split / join tool).
#[derive(Clone, Debug, PartialEq)]
pub struct CueTrace {
    /// The caption event's id in the plan (`c01`, …).
    pub event: String,
    pub words: Vec<gmclip_model::anchor::WordId>,
}

/// [`plan_cut_with`], also saying which words each built caption cue shows.
pub fn plan_cut_traced(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
    view: Option<&crate::analysis::AnalysisView>,
) -> Result<(rp::Plan, Vec<CueTrace>), PlannerError> {
    plan_cut_inner(doc, cut, framing, host, view, None, heard::HearPolicy::Rule)
        .map(|(p, t, _)| (p, t))
}

/// [`plan_cut_traced`] under a hearing policy, with the caption audit: which parts reuse
/// their source words, which stretches are heard, and which of those have no words yet
/// ([`heard::Audit::missing`], to hear and plan again; until then their parts show the
/// source words).
pub fn plan_cut_heard(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
    view: Option<&crate::analysis::AnalysisView>,
    policy: heard::HearPolicy,
) -> Result<(rp::Plan, Vec<CueTrace>, heard::Audit), PlannerError> {
    plan_cut_inner(doc, cut, framing, host, view, None, policy)
}

/// Captions the planner builds from words (SPEC §5.10 `heard`): each part's own audio,
/// its source transcript's words where that audio is identical ([`heard`]).
pub fn builds_from_words(s: CaptionSource) -> bool {
    matches!(s, CaptionSource::Heard)
}

/// An export preset (SPEC §5.14, §6.5.1) other than the framing's own.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExportPreset {
    /// YouTube 16:9 1080p: 1920×1080, H.264 High 4.0 (4.2 above 30 fps), VBR 12 Mbit/s
    /// at 30 fps (18 at 60), GOP ½ s.
    Youtube1080,
    /// YouTube 16:9 1440p (offered when the source has ≥ 1440 lines): 2560×1440, H.264
    /// High 5.1, VBR 24 Mbit/s at 30 fps (36 at 60), GOP ½ s.
    Youtube1440,
    /// YouTube 16:9 2160p (offered when the source has ≥ 2160 lines): 3840×2160, H.264
    /// High 5.1 (5.2 above 30 fps), VBR 45 Mbit/s at 30 fps (68 at 60), GOP ½ s.
    Youtube2160,
}

impl ExportPreset {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "youtube-1080p" => Some(Self::Youtube1080),
            "youtube-1440p" => Some(Self::Youtube1440),
            "youtube-2160p" => Some(Self::Youtube2160),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Youtube1080 => "youtube-1080p",
            Self::Youtube1440 => "youtube-1440p",
            Self::Youtube2160 => "youtube-2160p",
        }
    }

    pub fn canvas(self) -> (u32, u32) {
        match self {
            Self::Youtube1080 => (1920, 1080),
            Self::Youtube1440 => (2560, 1440),
            Self::Youtube2160 => (3840, 2160),
        }
    }
}

/// Plans a 16:9 framing at an upload preset: the layout is resolution-independent
/// (places are fractions of the canvas, text in short-side units), so the framing is
/// planned on the preset's canvas and the encode block is the preset's. A framing that is
/// not 16:9 is refused.
pub fn plan_cut_preset(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
    view: Option<&crate::analysis::AnalysisView>,
    preset: ExportPreset,
) -> Result<(rp::Plan, Vec<CueTrace>), PlannerError> {
    let f = doc
        .get(framing)
        .ok_or(PlannerError("no such framing".into()))?;
    if u64::from(f.canvas.w) * 9 != u64::from(f.canvas.h) * 16 {
        return err("the YouTube presets are for a 16:9 framing");
    }
    let (mut plan, traces, _) = plan_cut_inner(
        doc,
        cut,
        framing,
        host,
        view,
        Some(preset.canvas()),
        heard::HearPolicy::Rule,
    )?;
    let fps = plan.output.fps.rounded();
    let v = &mut plan.output.video;
    let fast = fps > 30;
    let (target, level) = match preset {
        ExportPreset::Youtube1080 => (
            if fast { 18_000_000 } else { 12_000_000 },
            if fast { "4.2" } else { "4.0" },
        ),
        ExportPreset::Youtube1440 => (if fast { 36_000_000 } else { 24_000_000 }, "5.1"),
        ExportPreset::Youtube2160 => (
            if fast { 68_000_000 } else { 45_000_000 },
            if fast { "5.2" } else { "5.1" },
        ),
    };
    v.level = Some(level.into());
    v.gop_frames = Some((fps / 2).max(1));
    // an upload preset has no cap beyond its level's (SPEC §6.5.1): the peak may run
    // to 1.5 × the target, which takes a 1080p60 or 1440p plan past the social 25 Mbit/s
    // ceiling, so such a plan is written `/1` (decision 0027)
    let max = target * 3 / 2;
    v.rate = Some(rp::VideoRate {
        mode: Vbr,
        target_bps: Some(target),
        max_bps: Some(max),
    });
    plan.schema = schema_for(&plan);
    Ok((plan, traces))
}

fn plan_cut_inner(
    doc: &Doc,
    cut: Id<Cut>,
    framing: Id<Framing>,
    host: &dyn PlanHost,
    view: Option<&crate::analysis::AnalysisView>,
    canvas: Option<(u32, u32)>,
    policy: heard::HearPolicy,
) -> Result<(rp::Plan, Vec<CueTrace>, heard::Audit), PlannerError> {
    let mut traces: Vec<CueTrace> = Vec::new();
    let mut audit = heard::Audit::default();
    let c = doc.get(cut).ok_or(PlannerError("no such cut".into()))?;
    let f = doc
        .get(framing)
        .ok_or(PlannerError("no such framing".into()))?;
    if f.cut != cut {
        return err("the framing is not this cut's");
    }
    let (fnum, fden) = (u64::try_from(f.fps.num).unwrap_or(0), u64::from(f.fps.den));
    // an integer rate, or an NTSC one (`/1`, decision 0020)
    let out_fps = rp::Fps::new(fnum, fden).map_err(|_| {
        PlannerError(format!(
            "this cut's frame rate ({} fps) is not one gmclip exports (24, 25, 30, 48, 50 or 60 fps, or 23.976, 29.97 or 59.94)",
            fps_words(fnum, fden)
        ))
    })?;
    let fpf = out_fps.flicks_per_frame() as i64;
    let mut ctx = Ctx {
        doc,
        cut,
        fpf,
        edges: BTreeMap::new(),
        canvas: canvas.unwrap_or((f.canvas.w, f.canvas.h)),
    };
    let (cw, ch) = (f64::from(ctx.canvas.0), f64::from(ctx.canvas.1));
    let short = cw.min(ch);
    // output sample at output frame `f` (rule 4; NTSC rounds down, decision 0020)
    let s_at = |f: u64| gmclip_plan::validate::out_samples(f, out_fps).unwrap_or(u64::MAX);
    // where the last part's sound ended: (plan audio source, its source time, sample), so
    // a part that continues it continues the sample count exactly
    let mut sound_end: Option<(PId, i128, u64)> = None;

    // decision 0046: the layout sections in output frames (the parts' lengths ahead)
    let layout_spans = {
        let total = part_out_lens(doc, cut, fpf).iter().map(|x| x.1).sum();
        sections::layout_spans(&ctx, framing, total, (fnum, fden))
    };

    // ---- parts
    let mut sources = Sources {
        doc,
        order: Vec::new(),
        by_id: BTreeMap::new(),
    };
    let mut parts: Vec<rp::Part> = Vec::new();
    let mut part_ids: Vec<Id<Part>> = Vec::new();
    let mut cursor: u64 = 0;
    for (k, pid) in doc.children::<Part>(cut.entity()).enumerate() {
        // errors name the part (1-based, in cut order) so the app can say where
        let mut one = || -> Result<(), PlannerError> {
            let p = doc.get(pid).ok_or(PlannerError("part".into()))?;
            // decision 0030: a speed, ¼ to 4 (to the nearest hundredth); 0033: or a ramp
            let (speed, ramp_len) = plan_part_speed(p, fpf)?;
            // word times and agent splits land between frames; the one tie rule (SPEC
            // §4.1.1) puts them on the output grid so RenderPlan can say them
            let src_in = nearest_frame(p.src_in.0, fpf);
            let src_out = nearest_frame(p.src_out.0, fpf);
            if src_in < 0 || src_out <= src_in {
                return err("a part edge off the frame grid or before the source's start");
            }
            let (a, b) = (src_in / fpf, src_out / fpf);
            let (pic, pic_off, aud, aud_off) = media_of(doc, p, src_out - fpf)?;
            if pic_off % fpf != 0 {
                return err("a picture offset off the frame grid");
            }
            // SPEC §4.3 SyncSegment: a member's time is the group's time minus its offset
            let first_frame = (i128::from(src_in) - i128::from(pic_off)) / i128::from(fpf);
            let aud_t = i128::from(src_in) - i128::from(aud_off);
            let s = aud_t * i128::from(AUDIO_RATE);
            // exact on an integer rate's frame grid; an NTSC frame edge rounds down
            if first_frame < 0 || first_frame > i128::from(u32::MAX) || s < 0 {
                return err("a part starts before its media or past any real file");
            }
            let first_frame = first_frame as i64;
            let mut first_sample = (s / i128::from(FLICKS_PER_SECOND)) as u64;
            let pic_src = sources.get(pic)?;
            let (pic_id, dims) = (pic_src.id.clone(), pic_src.dims);
            // a picture at another nominal rate is read by time (decision 0017)
            let pic_rate = pic_src
                .rate
                .filter(|&(n, d)| {
                    u128::from(n) * u128::from(fden) != u128::from(fnum) * u128::from(d)
                })
                .map(|(num, den)| rp::Rational { num, den });
            let aud_id = sources.get(aud)?.id.clone();
            if let Some((sid, t, at)) = &sound_end
                && *sid == aud_id
                && *t == aud_t
            {
                first_sample = *at;
            }
            // decision 0051: the group's other sounds under this one, `delta` samples on
            // from it (member time = group time − offset)
            let mut unders: Vec<(PId, i128, f64)> = Vec::new();
            if speed.is_none() {
                for (s, off, db) in under_of(doc, p, aud, src_out - fpf) {
                    let id = sources.get(s)?.id.clone();
                    let delta = ((i128::from(aud_off) - i128::from(off)) * i128::from(AUDIO_RATE))
                        .div_euclid(i128::from(FLICKS_PER_SECOND));
                    unders.push((id, delta, db));
                }
            }
            let base = cursor;
            let dims = dims.ok_or(PlannerError("the picture source has no probed size".into()))?;

            whole_part_only(doc, p)?;
            let mut bounds = vec![0i64];
            let splits = split_points(doc, p, framing, fpf)?;
            // a sped part lasts (b − a) / speed output frames (the one tie rule)
            let out_len = match speed {
                Some(_) if !splits.is_empty() => {
                    return err("a visual split inside a part at another speed");
                }
                Some(_) if ramp_len.is_some() => ramp_len
                    .filter(|n| *n >= 1)
                    .ok_or(PlannerError("a part too short for its speed".into()))?,
                Some(sp) => gmclip_model::clock::mul_div_ties_earlier(
                    b - a,
                    i128::from(sp.den),
                    i128::from(sp.num),
                )
                .filter(|n| *n >= 1)
                .ok_or(PlannerError("a part too short for its speed".into()))?,
                None => b - a,
            };
            // decision 0046: a layout section's edges split the part as a visual split
            // does (not one at another speed: it takes the section where it starts)
            let mut splits = splits;
            if speed.is_none() {
                splits.extend(sections::inner_edges(&layout_spans, base, out_len as u64));
                splits.sort_unstable();
                splits.dedup();
            }
            bounds.extend(splits);
            bounds.push(out_len);
            for w in bounds.windows(2) {
                let (sa, sb) = (w[0], w[1]);
                let t0 = gmclip_model::Flicks(src_in + sa * fpf);
                // an eased split starting here: its own values are where it settles, and
                // the stack at its start is where the ease begins (decision 0016)
                let eased = gmclip_model::effective::splits_of(doc, p.id, framing)
                    .into_iter()
                    .rfind(|s| s.at_src == t0 && sa > 0)
                    .and_then(|s| match s.transition {
                        Transition::Ease { dur_ms, curve } if dur_ms > 0 => Some((dur_ms, curve)),
                        _ => None,
                    });
                let stack = if eased.is_some() {
                    gmclip_model::effective::effective_settled(doc, pid, framing, t0)
                } else {
                    effective(doc, pid, framing, t0)
                }
                .ok_or(PlannerError("the part's stack does not resolve".into()))?;
                let from_stack = match eased {
                    Some(_) => Some(
                        effective(doc, pid, framing, t0)
                            .ok_or(PlannerError("the part's stack does not resolve".into()))?,
                    ),
                    None => None,
                };
                let mut layers = Vec::new();
                let mut details = 0;
                let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
                // decision 0046: a layout section over this plan part's start gives the
                // layers it maps its layout's values (settled, or eased from what was
                // there); the layers it does not map keep their own
                let sec_at = sections::layout_at(&layout_spans, cursor);
                let map =
                    |l: Option<Id<Layout>>| l.map(|l| sections::map_layout(doc, l, &stack.layers));
                let (sec_before, sec_after) = match &sec_at {
                    sections::LayoutAt::Own => (None, None),
                    sections::LayoutAt::Settled(l) => (None, map(Some(*l))),
                    sections::LayoutAt::Ease { before, after, .. } => (map(*before), map(*after)),
                };
                for (li, l) in stack.layers.iter().enumerate() {
                    let mapped_before = sec_before.as_ref().and_then(|m| m[li].clone());
                    let mapped_after = sec_after.as_ref().and_then(|m| m[li].clone());
                    let sec_layer = mapped_before.is_some() || mapped_after.is_some();
                    // decision 0046: a layer of another member of the part's sync group (a
                    // recording's camera file beside its screen) reads that file live, on
                    // the part's clock: (plan source, its size, its source, its first frame,
                    // its rate when not the framing's: decision 0047)
                    #[allow(clippy::type_complexity)]
                    let other: Option<(
                        PId,
                        (u32, u32),
                        Id<Source>,
                        u64,
                        Option<rp::Rational>,
                    )> = match l.values.member {
                        None => None,
                        Some(m) => {
                            let m = doc
                                .get(m)
                                .ok_or(PlannerError("a layer's sync member is gone".into()))?;
                            if m.source == pic {
                                None
                            } else {
                                if !matches!(p.media, MediaRef::Group(g) if g == m.group) {
                                    return err("a layer reading a member of another group");
                                }
                                if speed.is_some() {
                                    return err(
                                        "a layer of another member on a part that changes speed",
                                    );
                                }
                                if m.rate_ppm != 0.0 {
                                    return err(
                                        "a sync member with a rate correction (not in RenderPlan v0)",
                                    );
                                }
                                let off = offset_at(doc, m.id, t0.0).ok_or(PlannerError(
                                    "no sync segment covers the part's start".into(),
                                ))?;
                                if offset_at(doc, m.id, src_out - fpf) != Some(off) {
                                    return err("a part straddles a sync offset change");
                                }
                                if off % fpf != 0 {
                                    return err("a member offset off the frame grid");
                                }
                                let f0 = (t0.0 - off) / fpf;
                                if f0 < 0 {
                                    return err("a layer's member starts after the part");
                                }
                                let ps = sources.get(m.source)?;
                                let d = ps.dims.ok_or(PlannerError(
                                    "a layer's member has no probed picture size".into(),
                                ))?;
                                // a camera at another rate is read by time (decision 0047)
                                let rate = ps
                                    .rate
                                    .filter(|&(n, d)| {
                                        u128::from(n) * u128::from(fden)
                                            != u128::from(fnum) * u128::from(d)
                                    })
                                    .map(|(num, den)| rp::Rational { num, den });
                                Some((ps.id.clone(), d, m.source, f0 as u64, rate))
                            }
                        }
                    };
                    let (dims, pic) = match &other {
                        Some((_, d, s, _, _)) => (*d, *s),
                        None => (dims, pic),
                    };
                    let role = role_name(l.values.role).unwrap_or("other");
                    let id = if role == "detail" {
                        details += 1;
                        format!("detail-{}", details - 1)
                    } else {
                        let n = seen.entry(role).or_insert(0);
                        *n += 1;
                        if *n == 1 {
                            role.to_owned()
                        } else {
                            format!("{role}-{n}")
                        }
                    };
                    let mut layer = layer_of(
                        mapped_after.as_ref().unwrap_or(&l.values),
                        id.clone(),
                        ctx.canvas,
                        dims,
                        pic,
                    )?;
                    if let Some((sid, _, _, f0, rate)) = &other {
                        layer.picture_source = Some(sid.clone());
                        layer.fps = *rate;
                        if !matches!(layer.time_map, Some(rp::TimeMap::Hold { .. })) {
                            layer.first_frame = Some(*f0);
                        }
                    }
                    if sec_layer
                        && let sections::LayoutAt::Ease { e, n, .. } = sec_at
                        && layer.matte.is_none()
                    {
                        // where the ease is at this plan part's start: carried on from the
                        // plan part before inside one model part, else this part's own
                        // states before and after it at the ease's progress
                        let prev = parts.last().filter(|_| sa > 0).and_then(|pp| {
                            pp.layers.iter().find(|x| x.id == layer.id).map(|x| (pp, x))
                        });
                        let from = match prev {
                            Some((pp, pl)) => match &pl.ease {
                                Some(pe) => pe.at(
                                    cursor - pp.out.start(),
                                    pl.crop,
                                    pl.place,
                                    shown_opacity(pl),
                                ),
                                None => (pl.crop, pl.place, shown_opacity(pl)),
                            },
                            None => {
                                let b = layer_of(
                                    mapped_before.as_ref().unwrap_or(&l.values),
                                    id.clone(),
                                    ctx.canvas,
                                    dims,
                                    pic,
                                )?;
                                #[allow(clippy::cast_precision_loss)]
                                let t =
                                    rp::EaseCurve::EaseInOut.apply((cursor - e) as f64 / n as f64);
                                (
                                    lerp_rect(b.crop, layer.crop, t),
                                    lerp_rect(b.place, layer.place, t),
                                    shown_opacity(&b)
                                        + (shown_opacity(&layer) - shown_opacity(&b)) * t,
                                )
                            }
                        };
                        ease_section_layer(&mut layer, from, e + n - cursor);
                    } else if !sec_layer
                        && let (Some((dur_ms, curve)), Some(fs)) = (eased, &from_stack)
                        && let Some(fl) = fs.layers.iter().find(|x| x.id == l.id)
                    {
                        let from = layer_of(&fl.values, id, ctx.canvas, dims, pic)?;
                        if from.crop != layer.crop
                            || from.place != layer.place
                            || from.opacity != layer.opacity
                        {
                            if layer.matte.is_some() {
                                return err("an eased visual split on a cut-out layer");
                            }
                            let frames =
                                ((u64::from(dur_ms) * fnum + 500 * fden) / (1000 * fden)).max(1);
                            layer.ease = Some(rp::LayerEase {
                                from_crop: from.crop,
                                from_place: from.place,
                                from_opacity: from
                                    .opacity
                                    .filter(|_| from.opacity != layer.opacity),
                                frames,
                                curve: match curve {
                                    gmclip_model::entities::Curve::Linear => rp::EaseCurve::Linear,
                                    gmclip_model::entities::Curve::EaseIn => rp::EaseCurve::EaseIn,
                                    gmclip_model::entities::Curve::EaseOut => {
                                        rp::EaseCurve::EaseOut
                                    }
                                    gmclip_model::entities::Curve::EaseInOut => {
                                        rp::EaseCurve::EaseInOut
                                    }
                                },
                            });
                        }
                    }
                    // a cover pinned to a moving point (decision 0033)
                    if layer.cover.is_some()
                        && let Some(t) = doc.get(l.id).and_then(|x| x.track.as_ref())
                    {
                        layer.track = cover_track(t, p, &layer, dims, fpf, (sa, sb), cursor);
                    }
                    layers.push(layer);
                }
                let n = (sb - sa) as u64;
                let audio_first = first_sample + (s_at(cursor) - s_at(base));
                let under: Vec<rp::UnderAudio> = unders
                    .iter()
                    .filter_map(|(id, delta, db)| {
                        let at = u64::try_from(i128::from(audio_first) + delta).ok()?;
                        Some(rp::UnderAudio {
                            source: id.clone(),
                            first_sample: at,
                            gain_db: *db,
                        })
                    })
                    .collect();
                parts.push(rp::Part {
                    id: PId::new(format!("p{:02}", parts.len() + 1)).map_err(PlannerError)?,
                    label: None,
                    role: None,
                    out: Range([cursor, cursor + n]),
                    picture: rp::Picture {
                        source: pic_id.clone(),
                        first_frame: (first_frame + sa) as u64,
                        expect_pts: None,
                        fps: pic_rate,
                    },
                    audio: rp::PartAudio {
                        source: aud_id.clone(),
                        first_sample: audio_first,
                        rate: ConstU64,
                        gain_db: Some(p.gain_db),
                        fade_in: None,
                        fade_out: None,
                        under: (!under.is_empty()).then_some(under),
                    },
                    native: Some(rp::Native {
                        frames: Some(if speed.is_some() {
                            Range([a as u64, b as u64])
                        } else {
                            Range([(a + sa) as u64, (a + sb) as u64])
                        }),
                        fps: Some(rp::Rational {
                            num: fnum,
                            den: fden,
                        }),
                    }),
                    layers,
                    speed,
                });
                part_ids.push(pid);
                cursor += n;
            }
            // a sped part's sound does not continue into the next part's
            sound_end = speed.is_none().then(|| {
                (
                    aud_id.clone(),
                    i128::from(src_out) - i128::from(aud_off),
                    first_sample + (s_at(cursor) - s_at(base)),
                )
            });
            Ok(())
        };
        one().map_err(|e| PlannerError(format!("part {}: {}", k + 1, e.0)))?;
    }
    if parts.is_empty() {
        return err("the cut has no parts");
    }
    let total = cursor;
    // a model part split by visual splits spans its plan parts
    ctx.edges = BTreeMap::new();
    for (id, p) in part_ids.iter().zip(&parts) {
        let e = ctx.edges.entry(*id).or_insert((p.out.start(), p.out.end()));
        e.0 = e.0.min(p.out.start());
        e.1 = e.1.max(p.out.end());
    }
    // one entry per model part (captions find a word's part among these)
    let (model_ids, model_starts): (Vec<Id<Part>>, Vec<u64>) = {
        let mut ids = Vec::new();
        let mut ps = Vec::new();
        for (id, p) in part_ids.iter().zip(&parts) {
            if !ids.contains(id) {
                ids.push(*id);
                ps.push(p.out.start());
            }
        }
        (ids, ps)
    };
    join_fades(&mut parts, out_fps);

    // ---- gain (before the text: captions hear the voice mix it shapes)
    let mut envelopes: Vec<rp::Envelope> = Vec::new();
    for (i, gid) in doc.children::<GainSegment>(cut.entity()).enumerate() {
        let g = doc.get(gid).ok_or(PlannerError("gain".into()))?;
        let (a, prov) = ctx.samples(&g.start)?;
        let (z, _) = ctx.samples(&g.end)?;
        // a segment's ends are anchored to the words or parts it spans: after the parts
        // are moved it may run backwards (left out) or overlap another (trimmed below)
        if z <= a {
            continue;
        }
        // a segment shorter than its two ramps (drawn short, or its words trimmed away):
        // the ramps shrink in proportion to fit, as overlapping ones do below
        let (mut rin, mut rout) = (
            u64::from(g.ramp_in_ms) * (AUDIO_RATE as u64 / 1000),
            u64::from(g.ramp_out_ms) * (AUDIO_RATE as u64 / 1000),
        );
        if rin + rout > z - a {
            let len = z - a;
            let sum = rin + rout;
            rin = rin * len / sum;
            rout = len - rin;
        }
        envelopes.push(rp::Envelope {
            id: PId::new(format!("gain-{i}")).map_err(PlannerError)?,
            out_samples: Range([a, z]),
            gain_db: g.gain_db,
            ramp_in_samples: rin,
            ramp_out_samples: rout,
            curve: LinearDb,
            provenance: prov,
        });
    }
    no_overlaps(&mut envelopes);

    // ---- text
    let styles = doc.iter::<Styles>().next();
    let fonts_by_name: BTreeMap<String, &Font> = doc
        .iter::<Font>()
        .map(|ft| (style::font_name(&ft.family, &ft.style), ft))
        .collect();
    let mut used_fonts: Vec<&Font> = Vec::new();
    let mut font_of = |name: &str| -> Result<(PId, f64), PlannerError> {
        let ft = fonts_by_name.get(name).copied().ok_or(PlannerError(
            "a text style names a font the project does not have".into(),
        ))?;
        if !used_fonts.iter().any(|u| u.id == ft.id) {
            used_fonts.push(ft);
        }
        Ok((
            PId::new(style::font_id(name)).map_err(PlannerError)?,
            ft.em_per_size,
        ))
    };
    let mut text_styles: BTreeMap<PId, rp::TextStyle> = BTreeMap::new();
    let mut base: Vec<(String, TextStyle)> = Vec::new();
    // styles and fonts only when something draws text (a cut without text needs no font)
    let draws_text = doc.children::<Overlay>(cut.entity()).next().is_some()
        || doc
            .children::<CaptionDoc>(cut.entity())
            .filter_map(|c| doc.get(c))
            .any(|c| {
                c.enabled
                    && (builds_from_words(c.source)
                        || doc.children::<Cue>(c.id.entity()).next().is_some())
            });
    if let Some(st) = styles.filter(|_| draws_text) {
        for (name, s) in &st.entries {
            if name.contains('@') {
                continue;
            }
            let (fid, em) = font_of(&s.font)?;
            text_styles.insert(
                PId::new(name.clone()).map_err(PlannerError)?,
                style::to_plan(s, &fid, em, short).map_err(PlannerError)?,
            );
            base.push((name.clone(), s.clone()));
        }
    }
    let rgba = |c: &Option<String>| -> Result<Option<Rgba>, PlannerError> {
        c.as_ref()
            .map(|c| Rgba::new(c.clone()).map_err(PlannerError))
            .transpose()
    };
    let runs = |rs: &[TextRun], case: TextCase| -> Result<Vec<rp::Run>, PlannerError> {
        rs.iter()
            .map(|r| {
                Ok(rp::Run {
                    text: style::cased(&r.text, case),
                    fill: rgba(&r.colour)?,
                })
            })
            .collect()
    };

    // overlays: (slot, start, id) — hooks, then pills and clocks, as the converter
    struct Ov<'o> {
        slot: u8,
        start: u64,
        o: &'o Overlay,
    }
    let mut ovs: Vec<Ov> = Vec::new();
    let mut images: Vec<rp::Image> = Vec::new();
    let mut image_events: Vec<rp::ImageEvent> = Vec::new();
    let mut shape_events: Vec<rp::ShapeEvent> = Vec::new();
    let mut clip_events: Vec<rp::ClipEvent> = Vec::new();
    // overlays pinned to a moving point (decision 0031)
    let mut event_tracks: Vec<rp::EventTrack> = Vec::new();
    let mut pin = |o: &Overlay, kind: rp::TrackedKind, event: &PId, span: (u64, u64)| {
        if let Some(t) = &o.track {
            let keys = track_keys(doc, cut, framing, t, span, fpf, (cw, ch));
            if !keys.is_empty() {
                event_tracks.push(rp::EventTrack {
                    kind,
                    event: event.clone(),
                    keys,
                });
            }
        }
    };
    // video assets a clip event reads (decision 0028), after the parts' sources
    let mut clip_sources: Vec<rp::Source> = Vec::new();
    for oid in doc.children::<Overlay>(cut.entity()) {
        let o = doc.get(oid).ok_or(PlannerError("overlay".into()))?;
        if o.parent != OverlayParent::Cut(cut) || !effective_overlay_place(o, framing).0 {
            continue;
        }
        if o.kind == OverlayKind::Video {
            // decision 0028: a video picture-in-picture over the overlay's span, silent
            if o.span_mode != SpanMode::ClampToStretch
                || doc.children::<Keyframe>(o.id.entity()).next().is_some()
            {
                return err("an overlay with keyframes or span continuation (M4)");
            }
            let v = o
                .video
                .as_ref()
                .ok_or(PlannerError("a video overlay without its video".into()))?;
            // a locked video is placed by its clock, not its anchors (below)
            let (a, z) = if v.lock.is_some() {
                (0, 0)
            } else {
                let Some((a, z, _)) = ctx.span(o, total)? else {
                    continue;
                };
                (a, z)
            };
            let OverlayGeom::Place(pl) = effective_overlay_place(o, framing).1 else {
                return err("a video overlay on a box (place it by its corner and width)");
            };
            // (source, dims, first frame, rate, frames, the span on the locked clock in frames)
            let (source, dims, first_frame, fps, frames, locked) = match &v.media {
                AssetOrSpan::Asset(aid) => {
                    let asset = doc
                        .get(*aid)
                        .ok_or(PlannerError("a video overlay's asset is gone".into()))?;
                    let (src, dims, rate, n) = asset_source(asset, host)?;
                    let id = src.id.clone();
                    if !clip_sources.iter().any(|s: &rp::Source| s.id == id) {
                        clip_sources.push(src);
                    }
                    if v.lock.is_some() {
                        return err("a locked video overlay plays a span of a video");
                    }
                    (
                        id,
                        dims,
                        0,
                        other_rate(Some(rate), out_fps),
                        out_len(n, rate, out_fps),
                        None,
                    )
                }
                AssetOrSpan::Span {
                    media,
                    src_in,
                    src_out,
                } => {
                    let (sid, off) = picture_of(doc, *media, src_in.0)?;
                    let ps = sources.get(sid)?;
                    let dims = ps.dims.ok_or(PlannerError(
                        "the video overlay's source has no probed size".into(),
                    ))?;
                    // the nearest output frame on the source's clock (ties to the earlier)
                    let near = |t: i64| -> u64 {
                        let num = 2 * i128::from(t) - i128::from(fpf);
                        let den = 2 * i128::from(fpf);
                        u64::try_from(num.div_euclid(den) + i128::from(num.rem_euclid(den) != 0))
                            .unwrap_or(0)
                    };
                    let f0 = near(src_in.0 - off);
                    let n = near(src_out.0 - off).saturating_sub(f0).max(1);
                    let locked = v.lock.as_ref().map(|l| {
                        let w0 = near(src_in.0 + l.offset.0);
                        (l.media, w0, w0 + n)
                    });
                    (
                        ps.id.clone(),
                        dims,
                        f0,
                        other_rate(ps.rate, out_fps),
                        n,
                        locked,
                    )
                }
            };
            let (w, h) = (f64::from(dims.0), f64::from(dims.1));
            let c = v.crop.unwrap_or(gmclip_model::geometry::NormRect::FULL);
            let crop = rp::Rect {
                x: c.x * w,
                y: c.y * h,
                w: c.w * w,
                h: c.h * h,
            };
            let pw = pl.w * cw;
            let matte = match &v.matte {
                Some(m) => roto_of(m, (w, h))?,
                None => None,
            };
            let event = |id: usize,
                         out: [u64; 2],
                         first_frame: u64,
                         frames: u64|
             -> Result<rp::ClipEvent, PlannerError> {
                Ok(rp::ClipEvent {
                    id: PId::new(format!("clip-{id}")).map_err(PlannerError)?,
                    out: Range(out),
                    layer: Some(i64::from(o.z)),
                    picture: rp::ClipPicture {
                        source: source.clone(),
                        first_frame,
                        fps,
                        frames: Some(frames),
                        r#loop: v.r#loop.then_some(true),
                    },
                    crop,
                    place: rp::Rect {
                        x: pl.x * cw,
                        y: pl.y * ch,
                        w: pw,
                        h: pw * crop.h / crop.w,
                    },
                    opacity: (o.opacity < 1.0).then_some(o.opacity.max(0.0)),
                    // decision 0042: among the part's layers, cut out like one of them
                    z: o.under.map(i64::from),
                    matte: matte.clone(),
                })
            };
            let Some((lock, w0, w1)) = locked else {
                clip_events.push(event(clip_events.len() + 1, [a, z], first_frame, frames)?);
                continue;
            };
            // Locked: wherever a part plays the locked video inside the span, the span's
            // frame of that moment; a part that runs on from the one before (a visual split)
            // continues the same event.
            let mut last: Option<usize> = None;
            for (pid, pp) in part_ids.iter().zip(&parts) {
                let Some(mp) = doc.get(*pid) else { continue };
                // the part's picture on the locked video's own frames (a sped part or one
                // at another rate is not on its clock frame for frame: no graphic there)
                if mp.media != lock || pp.speed.is_some() || pp.picture.fps.is_some() {
                    continue;
                }
                let (o0, o1) = (pp.out.start(), pp.out.end());
                let l0 = pp.picture.first_frame;
                let (x0, x1) = (l0.max(w0), (l0 + (o1 - o0)).min(w1));
                if x1 <= x0 {
                    continue;
                }
                let out = [o0 + (x0 - l0), o0 + (x1 - l0)];
                let ff = first_frame + (x0 - w0);
                if let Some(i) = last
                    && clip_events[i].out.end() == out[0]
                    && clip_events[i].picture.first_frame + (out[0] - clip_events[i].out.start())
                        == ff
                {
                    let e = &mut clip_events[i];
                    e.out = Range([e.out.start(), out[1]]);
                    e.picture.frames = Some(out[1] - e.out.start());
                    continue;
                }
                clip_events.push(event(clip_events.len() + 1, out, ff, x1 - x0)?);
                last = Some(clip_events.len() - 1);
            }
            continue;
        }
        if matches!(o.kind, OverlayKind::Image | OverlayKind::Shape) {
            if o.span_mode != SpanMode::ClampToStretch
                || doc.children::<Keyframe>(o.id.entity()).next().is_some()
            {
                return err("an overlay with keyframes or span continuation (M4)");
            }
            let Some((a, z, prov)) = ctx.span(o, total)? else {
                continue;
            };
            let out = Range([a, z]);
            let layer = Some(i64::from(o.z));
            let geom = effective_overlay_place(o, framing).1;
            if o.kind == OverlayKind::Image {
                let im = o
                    .image
                    .ok_or(PlannerError("an image overlay without its image".into()))?;
                let asset = doc
                    .get(im.asset)
                    .ok_or(PlannerError("an image overlay's asset is gone".into()))?;
                let iid = PId::new(format!("img-{}", im.asset.0).to_ascii_lowercase())
                    .map_err(PlannerError)?;
                if !images.iter().any(|i: &rp::Image| i.id == iid) {
                    images.push(rp::Image {
                        id: iid.clone(),
                        path: host.asset_path(asset).ok_or(PlannerError(
                            "an overlay image this machine does not have".into(),
                        ))?,
                        sha256: Sha256::new(asset.sha256.clone()).map_err(PlannerError)?,
                        w: u64::from(im.w),
                        h: u64::from(im.h),
                    });
                }
                let OverlayGeom::Place(p) = geom else {
                    return err("an image overlay on a box (place it by its corner and width)");
                };
                let w = p.w * cw;
                let eid =
                    PId::new(format!("image-{}", image_events.len() + 1)).map_err(PlannerError)?;
                pin(o, rp::TrackedKind::Image, &eid, (a, z));
                image_events.push(rp::ImageEvent {
                    id: eid,
                    out,
                    layer,
                    image: iid,
                    place: rp::Rect {
                        x: p.x * cw,
                        y: p.y * ch,
                        w,
                        h: w * f64::from(im.h) / f64::from(im.w),
                    },
                    opacity: (o.opacity < 1.0).then_some(o.opacity.max(0.0)),
                    provenance: prov,
                    z: o.under.map(i64::from),
                });
            } else {
                let sh = o
                    .shape
                    .as_ref()
                    .ok_or(PlannerError("a shape overlay without its shape".into()))?;
                let px = |u: f64| u * short / 1000.0;
                let faded = |c: &str| -> Result<Rgba, PlannerError> {
                    let c = Rgba::new(c.to_owned()).map_err(PlannerError)?;
                    if o.opacity >= 1.0 {
                        return Ok(c);
                    }
                    let a = u8::from_str_radix(&c.as_str()[7..9], 16).unwrap_or(255);
                    let a = (f64::from(a) * o.opacity.max(0.0)).round() as u8;
                    Rgba::new(format!("{}{a:02X}", &c.as_str()[..7])).map_err(PlannerError)
                };
                let shape = match sh.kind {
                    ShapeKind::Box => {
                        let OverlayGeom::Box(b) = geom else {
                            return err("a box callout without its box");
                        };
                        rp::Shape::Box {
                            rect: rp::Rect {
                                x: b.x * cw,
                                y: b.y * ch,
                                w: b.w * cw,
                                h: b.h * ch,
                            },
                            width: px(sh.width),
                            stroke: (sh.width > 0.0).then(|| faded(&sh.stroke)).transpose()?,
                            fill: sh.fill.as_deref().map(faded).transpose()?,
                            radius: (sh.radius > 0.0).then(|| px(sh.radius)),
                        }
                    }
                    ShapeKind::Arrow => {
                        let (Some(f), Some(t)) = (sh.from, sh.to) else {
                            return err("an arrow without its ends");
                        };
                        rp::Shape::Arrow {
                            from: rp::Point {
                                x: f[0] * cw,
                                y: f[1] * ch,
                            },
                            to: rp::Point {
                                x: t[0] * cw,
                                y: t[1] * ch,
                            },
                            width: px(sh.width),
                            stroke: faded(&sh.stroke)?,
                            head: None,
                        }
                    }
                };
                let eid =
                    PId::new(format!("shape-{}", shape_events.len() + 1)).map_err(PlannerError)?;
                pin(o, rp::TrackedKind::Shape, &eid, (a, z));
                shape_events.push(rp::ShapeEvent {
                    id: eid,
                    out,
                    layer,
                    shape,
                    provenance: prov,
                });
            }
            continue;
        }
        if o.opacity != 1.0
            || o.span_mode != SpanMode::ClampToStretch
            || matches!(effective_overlay_place(o, framing).1, OverlayGeom::Box(_))
            || doc.children::<Keyframe>(o.id.entity()).next().is_some()
        {
            return err(
                "an overlay with opacity, keyframes, a box or span continuation (not in the M1 planner)",
            );
        }
        let counter_ok = o.kind == OverlayKind::Counter && o.counter.is_some();
        if (o.kind != OverlayKind::Text || o.counter.is_some()) && !counter_ok {
            return err("an overlay kind the planner does not draw (image, video, shape)");
        }
        let role = o.role.as_deref().and_then(text_role).ok_or(PlannerError(
            "a text overlay without a RenderPlan text role".into(),
        ))?;
        let slot = match role {
            rp::TextRole::TextHook => 0,
            rp::TextRole::Caption => {
                return err("a caption as an overlay (captions are cues)");
            }
            _ => 1,
        };
        let (start, _) = ctx.frame(&o.start, total)?;
        ovs.push(Ov { slot, start, o });
    }
    ovs.sort_by_key(|a| (a.slot, a.start, a.o.id));
    let mut events: Vec<rp::TextEvent> = Vec::new();
    let (mut hooks, mut pills, mut clocks, mut notes) = (0, 0, 0, 0);
    for Ov { o, .. } in &ovs {
        let text = o
            .text
            .as_ref()
            .ok_or(PlannerError("a text overlay without text".into()))?;
        let role = o
            .role
            .as_deref()
            .and_then(text_role)
            .ok_or(PlannerError("role".into()))?;
        let Some((a, z, prov)) = ctx.span(o, total)? else {
            continue;
        };
        let id = match role {
            rp::TextRole::TextHook => {
                hooks += 1;
                if hooks == 1 {
                    "hook".to_owned()
                } else {
                    format!("hook-{hooks}")
                }
            }
            rp::TextRole::RunClock => {
                clocks += 1;
                format!("clock-{clocks}")
            }
            rp::TextRole::SectionPill => {
                pills += 1;
                format!("pill-{pills}")
            }
            _ => {
                notes += 1;
                format!("note-{notes}")
            }
        };
        let (fid, em) = font_of(&text.style.font)?;
        let style_id = match base
            .iter()
            .find(|(_, s)| style::same_but_size(s, &text.style))
        {
            Some((n, _)) => PId::new(n.clone()).map_err(PlannerError)?,
            None => {
                let n =
                    PId::new(format!("o-{}", o.id.0).to_ascii_lowercase()).map_err(PlannerError)?;
                text_styles.insert(
                    n.clone(),
                    style::to_plan(&text.style, &fid, em, short).map_err(PlannerError)?,
                );
                n
            }
        };
        let geom = effective_overlay_place(o, framing).1;
        let (x, y) = match geom {
            OverlayGeom::Place(p) => (p.x * cw, p.y * ch),
            OverlayGeom::Box(_) => return err("an overlay box"),
        };
        let boxed = text.style.pill.as_ref().is_some_and(|p| p.on);
        if let Some(cn) = &o.counter {
            let (start, stop) = counter::span(doc, cut, cn).map_err(PlannerError)?;
            let src_at = |k: u64| -> Option<i64> {
                let i = parts
                    .iter()
                    .position(|p| p.out.start() <= k && k < p.out.end())?;
                let n0 = parts[i].native.as_ref()?.frames.as_ref()?.start();
                Some((n0 + (k - parts[i].out.start())) as i64 * fpf)
            };
            let start_out =
                crate::anchors::to_cut(doc, cut, start, None).map(|t| frame_of_cut_time(t.0, fpf));
            let segs = counter::segments(cn, start, stop, a, z, fpf, &src_at, start_out);
            clocks -= 1;
            for (from, to, label) in segs {
                clocks += 1;
                events.push(rp::TextEvent {
                    id: PId::new(format!("clock-{clocks}")).map_err(PlannerError)?,
                    out: Range([from, to]),
                    layer: Some(i64::from(o.z)),
                    role,
                    style: style_id.clone(),
                    pos: rp::Point { x, y },
                    align: Some(style::numpad(text.style.align, boxed)),
                    size: style::libass_size(&text.style, em, short),
                    runs: clock_runs(&label, &cn.done_label)?,
                    provenance: prov.clone(),
                    anim: None,
                });
            }
            continue;
        }
        let eid = PId::new(id).map_err(PlannerError)?;
        pin(o, rp::TrackedKind::Text, &eid, (a, z));
        events.push(rp::TextEvent {
            id: eid,
            out: Range([a, z]),
            layer: Some(i64::from(o.z)),
            role,
            style: style_id,
            pos: rp::Point { x, y },
            align: Some(style::numpad(text.style.align, boxed)),
            size: style::libass_size(&text.style, em, short),
            runs: runs(&text.runs, text.style.case)?,
            provenance: prov,
            anim: None,
        });
    }

    // captions: imported cues
    if let Some(cd) = doc
        .children::<CaptionDoc>(cut.entity())
        .filter_map(|c| doc.get(c))
        .next()
        .filter(|c| c.enabled)
    {
        if doc.children::<CaptionDoc>(cut.entity()).count() > 1 {
            return err("a cut with more than one caption doc");
        }
        if cd.subline.is_some() {
            return err("a caption subline (not in the planner yet)");
        }
        if builds_from_words(cd.source) {
            let view = view.ok_or(PlannerError(
                "heard or stream captions need the project's transcripts".into(),
            ))?;
            let sref = f
                .caption_style_ref
                .clone()
                .unwrap_or_else(|| "Speech".into());
            let st = styles
                .and_then(|s| s.entries.get(&sref))
                .ok_or(PlannerError(
                    "captions need a caption style (the caption panel's Story preset)".into(),
                ))?;
            let (_, em) = font_of(&st.font)?;
            let starts: Vec<(Id<Part>, i64)> = model_ids
                .iter()
                .zip(&model_starts)
                .map(|(id, s)| (*id, *s as i64 * fpf))
                .collect();
            let mut spans = Vec::new();
            let mut src_spans = Vec::new();
            for r in cd.no_caption_ranges.values() {
                if let Some(s) = captions::src_span(&r.from, &r.to) {
                    src_spans.push(s);
                }
                if let (Ok((x, _)), Ok((y, _))) =
                    (ctx.frame(&r.from, total), ctx.frame(&r.to, total))
                {
                    spans.push((x as i64 * fpf, y as i64 * fpf));
                }
            }
            let skip = |t: i64, media: &MediaRef, mid: i64| {
                spans.iter().any(|(x, y)| t >= *x && t < *y)
                    || src_spans.iter().any(|s| s.covers(media, mid))
            };
            // which parts reuse their source words, which stretches are heard (§5.10)
            let owner: Vec<usize> = part_ids
                .iter()
                .map(|id| model_ids.iter().position(|m| m == id).unwrap_or(0))
                .collect();
            let transcribed: Vec<bool> = model_ids
                .iter()
                .map(|id| {
                    doc.get(*id)
                        .is_some_and(|p| plays_transcribed(doc, view, p, fpf))
                })
                .collect();
            audit = heard::audit(
                &parts,
                &envelopes,
                &sources.plan()?,
                out_fps,
                s_at(total),
                &owner,
                &|i| owner.get(i).is_some_and(|o| transcribed[*o]),
                policy,
            );
            let mut hearing = captions::Hearing::default();
            // output flicks per sample (705 600 000 / 48 000)
            let flicks_of = |n: u64| n as i64 * (FLICKS_PER_SECOND / AUDIO_RATE);
            for st in &audit.stretches {
                match view.heard(&st.sig) {
                    Some(w) => hearing.heard.push(captions::HeardUse {
                        parts: [owner[st.parts[0]], owner[st.parts[1] - 1] + 1],
                        out: [flicks_of(st.out[0]), flicks_of(st.out[1])],
                        window0: flicks_of(st.window[0]),
                        words: std::sync::Arc::clone(w),
                    }),
                    None => audit.missing.push(st.clone()),
                }
            }
            // nothing to show yet while the stretches wait to be heard (not an error: the
            // audit says what to hear, and the plan after hearing has them)
            let ws = match captions::words(doc, view, cd, &starts, &skip, &hearing) {
                Err(_) if !audit.missing.is_empty() => Vec::new(),
                r => r.map_err(PlannerError)?,
            };
            let cues = if f.caption_grouping == CaptionGrouping::StoryV5 {
                captions::group(
                    &ws,
                    f.caption_grouping,
                    &cd.cue_breaks,
                    &captions::keep_pairs(doc),
                    total as i64 * fpf,
                )
            } else {
                groupings::group(
                    &ws,
                    f.caption_grouping,
                    &cd.cue_breaks,
                    &captions::keep_pairs(doc),
                    total as i64 * fpf,
                )
            };
            let look = f.caption_look.clone().unwrap_or_default();
            let kw_fill = Rgba::new(
                look.keyword_colour
                    .clone()
                    .unwrap_or_else(|| captions::KEYWORD_FILL.to_owned()),
            )
            .map_err(PlannerError)?;
            for (n, cue) in cues.iter().enumerate() {
                let a = frame_of_cut_time(cue.start, fpf).min(total);
                let z = frame_of_cut_time(cue.end, fpf).min(total);
                if z <= a {
                    continue;
                }
                let place = effective_caption_place(doc, model_ids[cue.part], framing, None)
                    .unwrap_or(if ch > cw {
                        gmclip_model::geometry::Place {
                            x: 0.5,
                            y: 1065.0 / 1920.0,
                            w: 0.9,
                        }
                    } else {
                        gmclip_model::geometry::Place {
                            x: 0.5,
                            y: 0.84,
                            w: 0.9,
                        }
                    });
                let k = cue.words.len();
                let rs = cue
                    .words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        Ok(rp::Run {
                            text: style::cased(
                                &if i + 1 < k {
                                    format!("{} ", w.text)
                                } else {
                                    w.text.clone()
                                },
                                st.case,
                            ),
                            fill: w.keyword.then(|| kw_fill.clone()),
                        })
                    })
                    .collect::<Result<Vec<_>, PlannerError>>()?;
                traces.push(CueTrace {
                    event: format!("c{:02}", n + 1),
                    words: cue.words.iter().map(|w| w.id.clone()).collect(),
                });
                events.push(rp::TextEvent {
                    id: PId::new(format!("c{:02}", n + 1)).map_err(PlannerError)?,
                    out: Range([a, z]),
                    layer: Some(CAPTION_LAYER),
                    role: rp::TextRole::Caption,
                    style: PId::new(sref.clone()).map_err(PlannerError)?,
                    pos: rp::Point {
                        x: place.x * cw,
                        y: place.y * ch,
                    },
                    align: None,
                    size: style::libass_size(st, em, short),
                    runs: rs,
                    provenance: None,
                    anim: caption_anim(&look, &cue.words),
                });
            }
        } else if cd.source != CaptionSource::Imported {
            return err("supplied captions (not in the planner yet)");
        } else if !cd.edits.is_empty() || !cd.word_time_overrides.is_empty() {
            return err(
                "caption edits or word time overrides on imported cues (re-spell needs heard captions)",
            );
        }
        // no_caption_ranges and keywords shape *built* captions; imported cues are
        // already the result, so they are not reapplied here
        let entries = styles.map(|s| &s.entries);
        let mut n = 0;
        for cid in doc
            .children::<Cue>(cd.id.entity())
            .filter(|_| cd.source == CaptionSource::Imported)
        {
            let cue = doc.get(cid).ok_or(PlannerError("cue".into()))?;
            let (a, prov) = ctx.frame(&cue.from, total)?;
            let z = ctx.frame(&cue.to, total)?.0.min(total);
            if z <= a {
                continue;
            }
            let sref = cue
                .style_ref
                .as_deref()
                .ok_or(PlannerError("a cue without a style".into()))?;
            let st = entries.and_then(|e| e.get(sref)).ok_or(PlannerError(
                "a cue names a style the project does not have".into(),
            ))?;
            let (_, em) = font_of(&st.font)?;
            // the part showing the cue's first frame decides its seat
            let pi = parts
                .iter()
                .position(|p| p.out.start() <= a && a < p.out.end())
                .ok_or(PlannerError("a cue outside the cut".into()))?;
            let place = effective_caption_place(doc, part_ids[pi], framing, None)
                .ok_or(PlannerError("captions without a caption place".into()))?;
            n += 1;
            events.push(rp::TextEvent {
                id: PId::new(format!("c{n:02}")).map_err(PlannerError)?,
                out: Range([a, z]),
                layer: Some(CAPTION_LAYER),
                role: rp::TextRole::Caption,
                style: PId::new(base_style(sref)).map_err(PlannerError)?,
                pos: rp::Point {
                    x: place.x * cw,
                    y: place.y * ch,
                },
                align: None,
                size: style::libass_size(st, em, short),
                runs: runs(&cue.runs, st.case)?,
                provenance: prov,
                anim: None,
            });
        }
    }
    let mut fonts = Vec::new();
    for ft in &used_fonts {
        let name = style::font_name(&ft.family, &ft.style);
        fonts.push(rp::Font {
            id: PId::new(style::font_id(&name)).map_err(PlannerError)?,
            family: Some(ft.family.clone()),
            style: (!ft.style.is_empty()).then(|| ft.style.clone()),
            path: host.font_path(ft).ok_or(PlannerError(
                "a font file this machine does not have".into(),
            ))?,
            sha256: Sha256::new(
                ft.sha256
                    .clone()
                    .ok_or(PlannerError("a font without a sha256".into()))?,
            )
            .map_err(PlannerError)?,
            em_per_size: ft.em_per_size,
        });
    }
    // text_styles only when something draws text (the converter's shape)
    let text_styles = (!events.is_empty()).then_some(text_styles);

    let background = match &f.background {
        Background::Solid { colour } => rp::Background::Colour {
            rgb: Rgb::new(colour.clone()).map_err(PlannerError)?,
        },
        // SPEC §5.7 blurred source: the part's own picture under the layers
        Background::BlurredSource => rp::Background::BlurredSource {
            rgb: Rgb::new(BLURRED_FALLBACK.to_owned()).map_err(PlannerError)?,
            blur: BLURRED_BLUR,
            dim: BLURRED_DIM,
        },
        Background::Backdrop { asset, r#loop, fit } => {
            let (b, src, image) =
                plan_backdrop(doc, *asset, *r#loop, *fit, host, (cw, ch), out_fps)?;
            // one source per asset: a video overlay may show the same file
            if let Some(src) = src
                && !clip_sources.iter().any(|s| s.id == src.id)
            {
                clip_sources.push(src);
            }
            if let Some(image) = image
                && !images.iter().any(|i| i.id == image.id)
            {
                images.push(image);
            }
            b
        }
        // decision 0029: a plate of the project's style library (or a built-in one)
        Background::Plate { name } => rp::Background::Plate {
            stops: crate::commands::stylelib::plate_stops(doc, name)
                .ok_or(PlannerError(format!(
                    "the plate \"{name}\" is not in the style library"
                )))?
                .into_iter()
                .map(Rgb::new)
                .collect::<Result<_, _>>()
                .map_err(PlannerError)?,
        },
    };
    let cursors = plan_cursors(doc, &sources, &mut parts, &mut images, host)?;
    // decision 0046: zoom sections over the planned parts (layout sections are eased
    // splits of the parts, above)
    let zooms = sections::plan_zooms(&ctx, framing, &parts, total, (fnum, fden))?;
    // ---- music and sound effects over the cut, ducked under the voice (decision 0051)
    let total_samples = s_at(total);
    let mut sounds: Vec<rp::AudioOverlay> = Vec::new();
    for (i, oid) in doc.children::<AudioOverlay>(cut.entity()).enumerate() {
        let o = doc.get(oid).ok_or(PlannerError("sound".into()))?;
        let (a, _) = ctx.samples(&o.start)?;
        let (z, _) = ctx.samples(&o.end)?;
        let mut z = z.min(total_samples);
        let (source, first_sample) = match &o.media {
            AssetOrSpan::Asset(aid) => {
                let asset = doc
                    .get(*aid)
                    .ok_or(PlannerError("a sound's asset is gone".into()))?;
                let src = sound_asset_source(asset, host)?;
                let id = src.id.clone();
                if !clip_sources.iter().any(|s| s.id == id) {
                    clip_sources.push(src);
                }
                (id, 0)
            }
            AssetOrSpan::Span {
                media,
                src_in,
                src_out,
            } => {
                let (sid, off) = audio_source(doc, *media, src_in.0).ok_or(PlannerError(
                    "a sound's media has no sound at its start".into(),
                ))?;
                let id = sources.get(sid)?.id.clone();
                let s = (i128::from(src_in.0) - i128::from(off)) * i128::from(AUDIO_RATE);
                let len = i128::from(src_out.0 - src_in.0) * i128::from(AUDIO_RATE);
                let f = i128::from(FLICKS_PER_SECOND);
                if s < 0 {
                    return err("a sound starts before its media");
                }
                z = z.min(a.saturating_add(u64::try_from(len / f).unwrap_or(0)));
                (id, (s / f) as u64)
            }
        };
        if z <= a {
            continue;
        }
        let len = z - a;
        let ms = |v: u32| (u64::from(v) * (AUDIO_RATE as u64 / 1000)).min(len / 2);
        let (fi, fo) = (ms(o.fade_in_ms), ms(o.fade_out_ms));
        sounds.push(rp::AudioOverlay {
            id: PId::new(format!("sound-{}", i + 1)).map_err(PlannerError)?,
            source,
            first_sample,
            out_samples: Range([a, z]),
            gain_db: o.gain_db.clamp(UNDER_OFF_DB, 12.0),
            fade_in_samples: (fi > 0).then_some(fi),
            fade_out_samples: (fo > 0).then_some(fo),
            duck: o.duck_under_speech.as_ref().map(|d| rp::Duck {
                depth_db: -d.depth_db.abs().min(40.0),
                attack_samples: (u64::from(d.attack_ms) * 48).max(1),
                release_samples: (u64::from(d.release_ms) * 48).max(1),
            }),
        });
    }
    let speech = if sounds.iter().any(|s| s.duck.is_some()) {
        view.map(|v| speech_ranges(doc, v, &ctx, total_samples))
            .filter(|r| !r.is_empty())
    } else {
        None
    };
    let hook_id = c
        .moment
        .and_then(|m| doc.get(m))
        .and_then(|m| m.origin_ref.clone());
    let mut plan = rp::Plan {
        schema: SchemaVersion::V0,
        meta: Some(rp::Meta {
            title: (!c.title.is_empty()).then(|| c.title.clone()),
            hook_id,
            content_id: None,
            covers: (!c.lineage.covers.is_empty()).then(|| c.lineage.covers.clone()),
            generator: Some(rp::Generator {
                name: GENERATOR.into(),
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            inputs: None,
            import_report: None,
        }),
        output: m0_output(ctx.canvas.0, ctx.canvas.1, out_fps, total)?,
        background,
        sources: {
            let mut v = sources.plan()?;
            v.extend(clip_sources);
            v
        },
        fonts: (!fonts.is_empty()).then_some(fonts),
        text_styles,
        parts,
        text_events: (!events.is_empty()).then_some(events),
        images: (!images.is_empty()).then_some(images),
        image_events: (!image_events.is_empty()).then_some(image_events),
        shape_events: (!shape_events.is_empty()).then_some(shape_events),
        clip_events: (!clip_events.is_empty()).then_some(clip_events),
        tracks: (!event_tracks.is_empty()).then_some(event_tracks),
        cursors: (!cursors.is_empty()).then_some(cursors),
        zooms: (!zooms.is_empty()).then_some(zooms),
        audio: rp::Audio {
            envelopes: (!envelopes.is_empty()).then_some(envelopes),
            overlays: (!sounds.is_empty()).then_some(sounds),
            speech,
            loudness: match f.loudness_mode {
                None | Some(LoudnessMode::AsRecorded) => rp::Loudness::AsRecorded,
                Some(LoudnessMode::Static {
                    integrated_lufs,
                    true_peak_dbtp,
                }) => rp::Loudness::Static {
                    integrated_lufs,
                    true_peak_dbtp,
                },
            },
        },
        checks: (f.safe_zones == "instagram").then_some(rp::Checks {
            reference: None,
            safe_zones: Some(rp::SafeZones::Instagram),
        }),
    };
    plan.schema = schema_for(&plan);
    if let Some(e) = gmclip_plan::check_plan(&plan).first() {
        return Err(PlannerError(human_plan_error(&plan, e)));
    }
    Ok((plan, traces, audit))
}

/// Decision 0045: the recorded cursor of each picture source that shows one, once per
/// source (its track and its images by shape index, at the host's stored files), named by
/// every layer of that source but covers. A source whose track or images this machine
/// does not have, or whose images have no stored size, draws no cursor.
fn plan_cursors(
    doc: &Doc,
    sources: &Sources<'_>,
    parts: &mut [rp::Part],
    images: &mut Vec<rp::Image>,
    host: &dyn PlanHost,
) -> Result<Vec<rp::CursorTrack>, PlannerError> {
    let mut out: Vec<rp::CursorTrack> = Vec::new();
    let mut none: Vec<PId> = Vec::new();
    for p in parts.iter_mut() {
        for l in &mut p.layers {
            if l.cover.is_some() {
                continue;
            }
            let pic = l.picture_source.as_ref().unwrap_or(&p.picture.source);
            if none.contains(pic) {
                continue;
            }
            if let Some(c) = out.iter().find(|c| &c.source == pic) {
                l.cursor = Some(c.id.clone());
                continue;
            }
            let src = sources
                .order
                .iter()
                .find(|s| &sources.by_id[s].id == pic)
                .and_then(|s| doc.get(*s));
            let planned = match src.and_then(|s| s.cursor.as_ref()).filter(|c| c.visible) {
                Some(sc) => cursor_of(doc, sc, pic, out.len() + 1, images, host)?,
                None => None,
            };
            match planned {
                Some(c) => {
                    l.cursor = Some(c.id.clone());
                    out.push(c);
                }
                None => none.push(pic.clone()),
            }
        }
    }
    Ok(out)
}

/// One source's planned cursor (its images added to `images`), or `None` when this
/// machine cannot draw it.
fn cursor_of(
    doc: &Doc,
    sc: &SourceCursor,
    source: &PId,
    n: usize,
    images: &mut Vec<rp::Image>,
    host: &dyn PlanHost,
) -> Result<Option<rp::CursorTrack>, PlannerError> {
    let Some(track) = doc.get(sc.track) else {
        return Ok(None);
    };
    let Some(path) = host.asset_path(track) else {
        return Ok(None);
    };
    let mut shapes = Vec::with_capacity(sc.shapes.len());
    let mut add = Vec::new();
    for a in &sc.shapes {
        let Some((asset, probe, path)) = doc
            .get(*a)
            .and_then(|x| Some((x, x.probe?, host.asset_path(x)?)))
        else {
            return Ok(None);
        };
        let id = PId::new(format!("cursor-{}", a.0).to_ascii_lowercase()).map_err(PlannerError)?;
        if !images.iter().chain(&add).any(|i: &rp::Image| i.id == id) {
            add.push(rp::Image {
                id: id.clone(),
                path,
                sha256: Sha256::new(asset.sha256.clone()).map_err(PlannerError)?,
                w: u64::from(probe.w),
                h: u64::from(probe.h),
            });
        }
        shapes.push(id);
    }
    images.extend(add);
    Ok(Some(rp::CursorTrack {
        id: PId::new(format!("cursor-{n}")).map_err(PlannerError)?,
        source: source.clone(),
        path,
        sha256: Sha256::new(track.sha256.clone()).map_err(PlannerError)?,
        shapes,
        // the plan takes (0, 8]
        scale: if sc.scale.is_finite() && sc.scale > 0.0 {
            sc.scale.min(8.0)
        } else {
            1.0
        },
        smooth_ms: (sc.smooth_ms > 0).then_some(u64::from(sc.smooth_ms.min(1000))),
        hide_idle_ms: (sc.hide_idle_ms > 0).then_some(u64::from(sc.hide_idle_ms.min(600_000))),
        clicks: (sc.click_style() != CursorClick::None).then_some(true),
        // decision 0046: the feel, clamped to the plan's ranges; absent when off, so a
        // cursor without it plans the bytes it did
        click: (sc.click_style() == CursorClick::Ripple).then_some(rp::CursorClick::Ripple),
        smoothness: (sc.smoothness.is_finite() && sc.smoothness > 0.0)
            .then(|| sc.smoothness.min(1.0)),
        click_shrink: sc.click_shrink.then_some(true),
        shake_px: (sc.shake_px > 0).then(|| f64::from(sc.shake_px.min(20))),
        loop_ms: (sc.loop_ms > 0).then(|| u64::from(sc.loop_ms.min(10_000))),
        motion_blur: sc.motion_blur.then_some(true),
    }))
}

/// Whether a part's words can be its media's transcript's: the media has an active
/// transcript, and the part plays the member that transcript heard (a sync group is heard
/// through its master audio member, `audio_from` may pick another; a source through its
/// voice track, which the plan's `audio_track` also reads, decision 0009).
fn plays_transcribed(doc: &Doc, view: &crate::analysis::AnalysisView, p: &Part, fpf: i64) -> bool {
    let owner = match p.media {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    };
    if view.active(doc, &owner).is_none() {
        return false;
    }
    match p.media {
        MediaRef::Source(_) => true,
        MediaRef::Group(g) => {
            let Ok((_, _, aud, _)) = media_of(doc, p, p.src_out.0 - fpf) else {
                return false;
            };
            let Some(group) = doc.get(g) else {
                return false;
            };
            // the member the group's transcript heard (as the app picks it)
            let heard = group.master_audio.and_then(|m| doc.get(m)).or_else(|| {
                doc.children::<SyncMember>(g.entity())
                    .filter_map(|m| doc.get(m))
                    .find(|m| matches!(m.role, MemberRole::Audio | MemberRole::Both))
            });
            heard.is_some_and(|m| m.source == aud)
        }
    }
}

/// The output rate a cut of a picture at `num/den` fps is made at: the rate itself when
/// RenderPlan exports it, NTSC rates exactly (29.97 → 30000/1001, decision 0020), else
/// one it converts into evenly where there is one (15 → 30, 120 → 60, 144 → 48; the
/// picture converts, decision 0017), else the nearest; 30 for an unknown rate.
pub fn output_rate_for(num: i64, den: u32) -> gmclip_model::Rational {
    let int = gmclip_model::Rational::new;
    if num <= 0 || den == 0 {
        return int(30, 1);
    }
    let g = gcd_u64(num as u64, u64::from(den)).max(1);
    let (n, d) = (num as u64 / g, u64::from(den) / g);
    if rp::Fps::new(n, d).is_ok() {
        return int(n as i64, d as u32);
    }
    let r = (num as f64 / f64::from(den)).round().max(1.0) as u64;
    let allowed = FpsNum::ALLOWED;
    // a slow source: the smallest even multiple; a fast one: the largest even divisor
    let even = if r < 24 {
        allowed.into_iter().find(|&c| c.is_multiple_of(r))
    } else {
        allowed
            .into_iter()
            .rev()
            .find(|&c| c < r && r.is_multiple_of(c))
    };
    let near = || {
        allowed
            .into_iter()
            .min_by_key(|&c| (c.abs_diff(r), c))
            .unwrap_or(30)
    };
    int(even.unwrap_or_else(near) as i64, 1)
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Flicks per frame of a framing's rate, when RenderPlan exports it (exact for every
/// allowed rate, NTSC included).
pub fn framing_fpf(r: gmclip_model::Rational) -> Option<i64> {
    let fps = rp::Fps::new(u64::try_from(r.num).ok()?, u64::from(r.den)).ok()?;
    Some(fps.flicks_per_frame() as i64)
}

/// SPEC §4.1.1: `t` at the nearest frame of a grid of `fpf` flicks per frame, ties to
/// the earlier (`k = ⌈t/fpf − ½⌉`, exact in i128). Past the i64 range it saturates, so
/// an overflowing time stays out of range (and refused) rather than wrapping.
pub fn nearest_frame(t: i64, fpf: i64) -> i64 {
    let (t, f) = (i128::from(t), i128::from(fpf.max(1)));
    let k = (2 * t - f).div_euclid(2 * f) + i128::from((2 * t - f).rem_euclid(2 * f) != 0);
    i64::try_from(k * f).unwrap_or(if k < 0 { i64::MIN } else { i64::MAX })
}

/// A frame rate as a person says it: `30`, `29.97`, `23.976`.
pub fn fps_words(num: u64, den: u64) -> String {
    if den == 0 {
        return "?".into();
    }
    if num.is_multiple_of(den) {
        return (num / den).to_string();
    }
    let s = format!("{:.3}", num as f64 / den as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A plan the planner made that does not validate, in words (the cut view shows it with
/// what to do): what about the video does not fit, never a JSON pointer alone.
fn human_plan_error(plan: &rp::Plan, e: &gmclip_plan::PlanError) -> String {
    let path = e.path();
    let source = path
        .strip_prefix("/sources/")
        .and_then(|r| r.split('/').next())
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| plan.sources.get(i));
    let name = source
        .map(|s| {
            std::path::Path::new(&s.path)
                .file_name()
                .map_or(s.path.clone(), |n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "this video".into());
    let expect = source.and_then(|s| s.expect.as_ref());
    if path.ends_with("/expect/video/fps") {
        let src = expect
            .and_then(|x| x.video.as_ref())
            .and_then(|v| v.fps)
            .map_or("?".into(), |r| fps_words(r.num, r.den));
        return format!(
            "this video's frame rate ({src} fps, {name}) cannot be shown in this cut's {} fps",
            fps_words(plan.output.fps.num.get(), plan.output.fps.den.get())
        );
    }
    if path.ends_with("/expect/audio/rate") {
        let rate = expect
            .and_then(|x| x.audio.as_ref())
            .and_then(|a| a.rate)
            .unwrap_or(0);
        return format!(
            "this video's sound is at {} kHz ({name}); only 48 kHz sound plays for now",
            rate as f64 / 1000.0
        );
    }
    format!("the planned RenderPlan fails validation: {e}")
}

/// A caption cue's `/1` animation for a framing's look (none for the reference's plain look).
fn caption_anim(look: &CaptionLook, words: &[captions::CapWord]) -> Option<rp::TextAnim> {
    let kind = match look.animation {
        CaptionAnimation::None => None,
        CaptionAnimation::Bounce => Some(rp::AnimKind::Bounce),
        CaptionAnimation::Pop => Some(rp::AnimKind::Pop),
        CaptionAnimation::Fade => Some(rp::AnimKind::Fade),
    };
    let pops: Vec<u64> = if look.keyword_animation == KeywordAnimation::Pop {
        (0..words.len() as u64)
            .filter(|&i| words[i as usize].keyword)
            .collect()
    } else {
        Vec::new()
    };
    (kind.is_some() || !pops.is_empty()).then(|| rp::TextAnim {
        kind,
        pop_runs: (!pops.is_empty()).then_some(pops),
    })
}

/// The schema a plan needs: `/1` when it uses any of its additions (decisions 0014, 0016, …,
/// 0045's cursors, 0051's sound).
pub fn schema_for(plan: &rp::Plan) -> SchemaVersion {
    // a heard source at another rate than 48 kHz is read converted (decision 0051)
    let heard = |id: &PId| {
        plan.parts.iter().any(|p| &p.audio.source == id)
            || plan
                .audio
                .overlays
                .iter()
                .flatten()
                .any(|o| &o.source == id)
    };
    let v1 = plan.images.is_some()
        || plan.parts.iter().any(|p| p.audio.under.is_some())
        || plan.audio.overlays.is_some()
        || plan.audio.speech.is_some()
        || plan.sources.iter().any(|s| {
            heard(&s.id)
                && s.expect
                    .as_ref()
                    .and_then(|e| e.audio.as_ref())
                    .and_then(|a| a.rate)
                    .is_some_and(|r| r != AUDIO_RATE)
        })
        || plan.image_events.is_some()
        || plan.shape_events.is_some()
        || plan.clip_events.is_some()
        || plan.parts.iter().any(|p| p.speed.is_some())
        || plan.tracks.is_some()
        || plan.cursors.is_some()
        || plan.zooms.is_some()
        || matches!(
            plan.background,
            rp::Background::BlurredSource { .. } | rp::Background::Backdrop { .. }
        )
        || plan.text_events.iter().flatten().any(|e| e.anim.is_some())
        || plan.parts.iter().any(|p| {
            p.layers.iter().any(|l| {
                l.ease.is_some()
                    || l.cover.is_some()
                    || l.frame.is_some()
                    || l.first_frame.is_some()
                    || l.fps.is_some()
            }) || p.picture.fps.is_some()
        })
        || plan.output.fps.is_ntsc()
        || plan
            .output
            .video
            .rate
            .as_ref()
            .and_then(|r| r.max_bps)
            .is_some_and(|m| m > gmclip_plan::validate::SOCIAL_MAX_BPS);
    if v1 {
        SchemaVersion::V1
    } else {
        SchemaVersion::V0
    }
}

/// One planned cut of a project: its hook id (the moment's `origin_ref`), cut id and
/// title, and the plan or why it does not plan.
#[derive(Debug)]
pub struct PlannedCut {
    pub hook_id: Option<String>,
    pub cut: Id<Cut>,
    pub title: String,
    pub plan: Result<rp::Plan, PlannerError>,
    /// With [`plan_project_with`]: the caption audit and what hearing it took.
    pub heard: Option<HeardReport>,
}

/// How a cut's captions were heard (`gmclip plan-cut`).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct HeardReport {
    pub audit: heard::Audit,
    /// Wall seconds the hearer took (render, resample, ASR) for the stretches it heard,
    /// the speech model already loaded.
    pub hear_seconds: f64,
    /// Stretches heard in this run (the rest were already stored).
    pub heard_now: usize,
    /// Why the missing stretches could not be heard (their parts keep the source words;
    /// a caption build never fails, R-CAP-2).
    pub hear_error: Option<String>,
}

/// How [`plan_project_with`] builds captions.
#[derive(Clone, Copy, Default)]
pub struct CaptionsAs<'a> {
    /// Plan every enabled caption doc as this source, fillers dropped (in memory only).
    pub source: Option<CaptionSource>,
    pub policy: heard::HearPolicy,
    /// Hears the stretches with no stored words; `None`: they show the source words.
    pub hearer: Option<&'a dyn heard::Hearer>,
    /// Only the cut of this hook (the others are not planned).
    pub hook: Option<&'a str>,
}

impl std::fmt::Debug for CaptionsAs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptionsAs")
            .field("source", &self.source)
            .field("policy", &self.policy)
            .field("hearer", &self.hearer.is_some())
            .field("hook", &self.hook)
            .finish()
    }
}

/// Opens the project database at `path` (taking its lock for the duration) and plans
/// every cut that has a framing, in each cut's first framing.
pub fn plan_project(
    path: &std::path::Path,
    host: &dyn PlanHost,
) -> Result<Vec<PlannedCut>, PlannerError> {
    let (store, doc, _) = gmclip_store::Store::open(
        path,
        &gmclip_store::OpenOptions {
            kind: gmclip_store::HolderKind::Cli,
            host: None,
            force_other_host: false,
        },
    )
    .map_err(|e| PlannerError(format!("open the project: {e}")))?;
    let mut out = Vec::new();
    for c in doc.iter::<Cut>() {
        let Some(f) = doc.children::<Framing>(c.id.entity()).next() else {
            continue;
        };
        out.push(PlannedCut {
            hook_id: c
                .moment
                .and_then(|m| doc.get(m))
                .and_then(|m| m.origin_ref.clone()),
            cut: c.id,
            title: c.title.clone(),
            plan: plan_cut(&doc, c.id, f, host),
            heard: None,
        });
    }
    store
        .close(&doc)
        .map_err(|e| PlannerError(format!("close the project: {e}")))?;
    Ok(out)
}

/// [`plan_project`] with the project's stored transcripts, and the words already heard
/// from its cuts' audio. `captions.source` plans every enabled caption doc as that source,
/// fillers dropped the way the Short tab turns captions on, in memory only: the project is
/// not changed (`gmclip plan-cut --captions heard`, A.09's measurement). Stretches with
/// no stored words are heard by `captions.hearer` and the cut planned again with them.
pub fn plan_project_with(
    path: &std::path::Path,
    host: &dyn PlanHost,
    captions: CaptionsAs<'_>,
) -> Result<Vec<PlannedCut>, PlannerError> {
    let (store, doc, _) = gmclip_store::Store::open(
        path,
        &gmclip_store::OpenOptions {
            kind: gmclip_store::HolderKind::Cli,
            host: None,
            force_other_host: false,
        },
    )
    .map_err(|e| PlannerError(format!("open the project: {e}")))?;
    let mut view = crate::analysis::AnalysisView::default();
    if let Ok(keys) = store.analysis_keys() {
        for (owner, _, rev) in keys.iter().filter(|k| k.1 == "words") {
            if let Ok(Some(w)) = store.analysis(owner, "words", rev) {
                let lines = store.analysis(owner, "lines", rev).ok().flatten();
                view.insert_bodies(owner, &w, lines.as_deref());
            }
        }
        for (owner, _, rev) in keys.iter().filter(|k| k.1 == heard::KIND) {
            if let Ok(Some(b)) = store.analysis(owner, heard::KIND, rev) {
                view.insert_heard(&b);
            }
        }
    }
    let planned = match captions.source {
        None => doc.clone(),
        Some(source) => {
            let mut ids = || gmclip_model::Ulid(0);
            let mut st =
                gmclip_model::Staging::new(doc.clone(), gmclip_model::Author::System, &mut ids);
            for cd in doc.iter::<CaptionDoc>().filter(|c| c.enabled) {
                let mut cd = cd.clone();
                cd.source = source;
                cd.drop_fillers = true;
                st.replace(cd)
                    .map_err(|e| PlannerError(format!("captions: {e}")))?;
            }
            st.into_work()
        }
    };
    let mut out = Vec::new();
    for c in planned.iter::<Cut>() {
        let Some(f) = planned.children::<Framing>(c.id.entity()).next() else {
            continue;
        };
        let hook_id = c
            .moment
            .and_then(|m| planned.get(m))
            .and_then(|m| m.origin_ref.clone());
        if captions.hook.is_some_and(|h| hook_id.as_deref() != Some(h)) {
            continue;
        }
        let mut report = HeardReport::default();
        let mut plan = plan_cut_heard(&planned, c.id, f, host, Some(&view), captions.policy);
        if let Ok((_, _, a)) = &plan {
            report.audit = a.clone();
        }
        if let (Ok((p, _, a)), Some(h)) = (&plan, captions.hearer)
            && !a.missing.is_empty()
        {
            match h.warm().and_then(|()| {
                let t1 = std::time::Instant::now();
                h.hear(p, &a.missing).map(|w| (w, t1))
            }) {
                Ok((heard, t1)) => {
                    report.hear_seconds = t1.elapsed().as_secs_f64();
                    report.heard_now = heard.len();
                    for st in &heard {
                        view.insert_heard(&st.encode());
                    }
                    plan = plan_cut_heard(&planned, c.id, f, host, Some(&view), captions.policy);
                    if let Ok((_, _, a)) = &plan {
                        report.audit = a.clone();
                    }
                }
                Err(e) => report.hear_error = Some(e),
            }
        }
        out.push(PlannedCut {
            hook_id,
            cut: c.id,
            title: c.title.clone(),
            plan: plan.map(|p| p.0),
            heard: Some(report),
        });
    }
    store
        .close(&doc)
        .map_err(|e| PlannerError(format!("close the project: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    fn env(id: &str, a: u64, z: u64, ramp: u64) -> rp::Envelope {
        rp::Envelope {
            id: PId::new(id.to_owned()).unwrap(),
            out_samples: Range([a, z]),
            gain_db: 6.0,
            ramp_in_samples: ramp,
            ramp_out_samples: ramp,
            curve: LinearDb,
            provenance: None,
        }
    }

    /// Gain segments moved with their parts: overlaps are trimmed, one inside another
    /// drops out, ramps shrink to fit; envelopes that do not overlap are left as they are.
    #[test]
    fn moved_gain_segments_never_overlap() {
        let mut ok = vec![env("gain-0", 500, 900, 10), env("gain-1", 0, 400, 10)];
        let before = ok.clone();
        no_overlaps(&mut ok);
        assert_eq!(ok, before, "no overlap: untouched, in its own order");

        let mut v = vec![
            env("gain-0", 100, 1000, 300),
            env("gain-1", 0, 400, 50),
            env("gain-2", 450, 600, 10),
        ];
        no_overlaps(&mut v);
        let spans: Vec<(&str, u64, u64)> = v
            .iter()
            .map(|e| (e.id.as_str(), e.out_samples.start(), e.out_samples.end()))
            .collect();
        assert_eq!(spans, [("gain-1", 0, 400), ("gain-0", 400, 1000)]);
        assert!(
            v.iter()
                .all(|e| e.ramp_in_samples + e.ramp_out_samples <= e.out_samples.len())
        );
        assert_eq!((v[1].ramp_in_samples, v[1].ramp_out_samples), (300, 300));
    }
}
