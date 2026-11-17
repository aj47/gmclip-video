//! The RP §2.1 rules a schema cannot express, on a typed [`Plan`].
//!
//! Runs after the schema pass (so every value is in range and every enum is known).
//! Rule by rule:
//!
//! | rule | checked here |
//! |---|---|
//! | 1 integers, `[start, end)` | every `range` has `start < end` |
//! | 2 tiling | `parts[].out` tile `[0, output.frames)` in order; every `text_events[].out` ⊆ `[0, frames)` |
//! | 3 picture | a picture source whose `expect.video.fps` differs from `output.fps` is `Unsupported` unless every part showing it converts (`picture.fps`, `/1`, decision 0017: ≠ `output.fps`, = the source's declared rate) and every layer reading it is of another source with `fps` (decision 0047); `first_frame + len` fits |
//! | 4 fps, audio | `S(f) = f·48000/fps` ([`out_samples`]) is exact; each part's sample span fits; envelopes lie on `[0, S(frames)]` and do not overlap; an audio source whose `expect.audio.rate` ≠ 48 000 is `Unsupported` under `/0` (`/1` converts it, decision 0051) |
//! | 5 layers | aspect `|ph/pw − ch/cw| / (ch/cw) ≤ 0.005`; `role` ∈ [`LAYER_ROLES`] |
//! | 6 text | (schema: `size` required on every event) |
//! | 7 audio post | no fade at a continuous join; each fade fits its part; an envelope's ramps fit inside it |
//! | 10 referential integrity | every source/font/style reference resolves; ids unique per collection |
//! | 11 closed objects | (schema) |
//! | backdrop, clip events (`/1`) | a backdrop has one of `video`/`image`; every source and image it names exists; a clip event lies inside `[0, frames)`; crop/place aspect as rule 5; a picture at another rate carries `fps` (decision 0028) |
//! | tracks (`/1`) | each names an event of its kind once; keys increase inside its `out` (decision 0031) |
//! | layer tracks (`/1`) | only on a cover; keys increase inside the part's `out` (decision 0033) |
//! | layer frames (`/1`) | not on a cover (decision 0046); ranges are the schema's |
//! | live other source (`/1`) | `first_frame` only with a `picture_source` other than the part's, not held, on a part with no `speed` or `picture.fps` (decision 0046); `fps` only with such a `picture_source`, ≠ `output.fps`, = the source's declared rate (decision 0047) |
//! | zooms (`/1`) | ids unique; `out` inside the output; layers and camera layers name a layer (not a cover) of a part the zoom overlaps; a `cursor`/`focus` target names a `cursors[].id`; `ease_in + ease_out ≤ len(out)`; depth 1–6 (decision 0046) |
//! | cursors (`/1`) | ids unique; each names a source and its shapes `images[].id`s; a layer's `cursor` names one of its own picture source (decision 0045) |
//! | speed (`/1`) | `parts[].speed` in [¼, 4] and ≠ 1; a join touching a sped part is never continuous (decision 0030); a `ramp` goes to another speed in [¼, 4] over 1–10⁶ frames inside the part, terms ≤ 10⁴, and lets the start speed be 1 (decision 0033) |
//! | sound (`/1`) | `parts[].audio.under` names sources, not on a sped part; `audio.overlays` ids unique, sources exist, inside `[0, S(frames)]`, fades fit; `audio.speech` ranges in order, apart, inside the output (decision 0051) |
//! | encode (`/1`) | `rate.max_bps` above 25 Mbit/s only on a landscape canvas and at most the level's MaxBR ([`level_max_bps`]); then `target_bps ≤ max_bps` (decision 0027) |
//!
//! Rules 8 (loudness modes), 9 (`render_sig`, B-16) and 12 (evolution) have nothing to
//! check on a single plan beyond the schema. **Derived** rather than quoted from RP
//! (listed for the freeze review): a fade longer than its part; an envelope past
//! `S(output.frames)`; an audio source whose `expect.audio.rate` ≠ 48 000
//! (`Unsupported`); a frame or sample index past i64::MAX.

use std::collections::{BTreeMap, HashMap};

use crate::error::{PlanError, join_key};
use crate::scalars::Range;
use crate::types::{Fps, Plan};

/// `layers[].role` values (rule 5): SPEC `RegionRole`s, `detail`, `other`.
pub const LAYER_ROLES: &[&str] = &[
    "cam",
    "screen",
    "chat",
    "alerts",
    "banner",
    "private",
    "captions_burned",
    "detail",
    "other",
];

/// Aspect tolerance between a layer's crop and place (rule 5).
pub const ASPECT_TOLERANCE: f64 = 0.005;

/// The audio rate of every v0 plan (rule 4).
pub const AUDIO_RATE: u64 = 48_000;

/// The largest `output.video.rate.max_bps` RenderPlan `/0` takes, and the ceiling of the
/// social presets (SPEC §6.5.1: never above 25 Mbit/s peak). It is also H.264 High
/// level 4.0's MaxBR (20 000 kbit/s × 1.25).
pub const SOCIAL_MAX_BPS: u64 = 25_000_000;

/// The MaxBR of an H.264 High-profile level (Table A-1's MaxBR × 1.25, bits per second),
/// for the levels the upload presets use; `None` for any other `level` string.
pub fn level_max_bps(level: &str) -> Option<u64> {
    match level {
        "4.0" => Some(25_000_000),
        "4.1" | "4.2" => Some(62_500_000),
        "5.0" => Some(168_750_000),
        "5.1" | "5.2" => Some(300_000_000),
        _ => None,
    }
}

/// The lowest preset level (4.0, 4.2, 5.1) whose MaxFS and MaxMBPS (H.264 Table A-1)
/// hold a `w`×`h` picture at `fps`; 5.1 past that, which an encoder may still refuse.
pub fn fitting_level(w: u64, h: u64, fps: u64) -> &'static str {
    let fs = w.div_ceil(16) * h.div_ceil(16);
    let mbps = fs * fps;
    if fs <= 8_192 && mbps <= 245_760 {
        "4.0"
    } else if fs <= 8_704 && mbps <= 522_240 {
        "4.2"
    } else {
        "5.1"
    }
}

/// `S(f) = ⌊f · 48000 · fps.den / fps.num⌋`: the output sample at output frame `f`
/// (rule 4). Exact for every integer rate; an NTSC rate (`/1`, decision 0020) rounds
/// down, so its frames carry 1601 or 1602 samples at 29.97 and the counts telescope.
/// `None` if it does not fit in u64.
pub fn out_samples(f: u64, fps: Fps) -> Option<u64> {
    let (num, den) = fps.rate();
    let s = u128::from(f) * u128::from(AUDIO_RATE) * u128::from(den);
    u64::try_from(s / u128::from(num)).ok()
}

/// Whether a nominal rate equals the output rate.
fn same_rate(r: crate::types::Rational, fps: Fps) -> bool {
    let (n, d) = fps.rate();
    u128::from(r.num) * u128::from(d) == u128::from(n) * u128::from(r.den)
}

/// Samples part `out` contributes: `S(out[1]) − S(out[0])`.
pub fn part_samples(out: Range, fps: Fps) -> Option<u64> {
    Some(out_samples(out.end(), fps)?.saturating_sub(out_samples(out.start(), fps)?))
}

/// Whether the join between parts `p` and `p + 1` is continuous (rule 7): same audio
/// source, and `p+1` starts on the sample right after `p`'s last.
pub fn continuous_join(plan: &Plan, p: usize) -> bool {
    let (Some(a), Some(b)) = (plan.parts.get(p), plan.parts.get(p + 1)) else {
        return false;
    };
    // a part at another speed is never continuous with its neighbours (decision 0030)
    a.speed.is_none()
        && b.speed.is_none()
        && a.audio.source == b.audio.source
        && part_samples(a.out, plan.output.fps).and_then(|n| a.audio.first_sample.checked_add(n))
            == Some(b.audio.first_sample)
}

/// The rule violations in `plan`: every `Invalid` first, then any `Unsupported` (which
/// presumes a well-formed plan). This skips the schema's bounds, so it is only for a plan
/// that came through [`crate::check`]; a `Plan` built in code goes through
/// [`crate::check_plan`].
pub(crate) fn rules(plan: &Plan) -> Vec<PlanError> {
    let mut v = Check {
        plan,
        errs: Vec::new(),
    };
    v.run();
    let (mut invalid, unsupported): (Vec<_>, Vec<_>) = v
        .errs
        .into_iter()
        .partition(|e| matches!(e, PlanError::Invalid { .. }));
    invalid.extend(unsupported);
    invalid
}

/// `/1` zooms (decision 0046): ids unique; inside `[0, frames)`; each layer a layer of a
/// part the zoom overlaps (a cover never); a cursor or focus target names a `cursors[].id`;
/// the eases fit inside the span; the depth is 1–6.
impl Check<'_> {
    fn zooms(&mut self) {
        let plan = self.plan;
        let zooms = plan.zooms.as_deref().unwrap_or_default();
        self.unique("/zooms", zooms.iter().map(|z| z.id.as_str()), "id");
        let cursors = plan.cursors.as_deref().unwrap_or_default();
        for (i, z) in zooms.iter().enumerate() {
            let base = format!("/zooms/{i}");
            if !self.range(format!("{base}/out"), z.out) {
                continue;
            }
            if z.out.end() > plan.output.frames {
                self.invalid(format!("{base}/out"), "ends after output.frames");
            }
            if !(z.depth.is_finite() && (1.0..=6.0).contains(&z.depth)) {
                self.invalid(format!("{base}/depth"), "1–6");
            }
            if z.ease_in.saturating_add(z.ease_out) > z.out.len() {
                self.invalid(format!("{base}/ease_in"), "the eases do not fit inside out");
            }
            match &z.target {
                crate::types::ZoomTarget::Cursor(id) | crate::types::ZoomTarget::Focus(id) => {
                    if !cursors.iter().any(|c| &c.id == id) {
                        self.invalid(format!("{base}/target"), "names no cursors[].id");
                    }
                }
                crate::types::ZoomTarget::Point(p) => {
                    if !p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                        self.invalid(format!("{base}/target/point"), "x and y 0–1");
                    }
                }
            }
            if let Some(c) = z.camera_scale
                && !(c.is_finite() && (0.3..=1.0).contains(&c))
            {
                self.invalid(format!("{base}/camera_scale"), "0.3–1");
            }
            let mut refs: Vec<(String, &crate::types::LayerRef)> = z
                .layers
                .iter()
                .enumerate()
                .map(|(k, r)| (format!("{base}/layers/{k}"), r))
                .collect();
            refs.extend(
                z.camera_layers
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(k, r)| (format!("{base}/camera_layers/{k}"), r)),
            );
            for (at, r) in refs {
                let Some(p) = plan.parts.iter().find(|p| p.id == r.part) else {
                    self.invalid(format!("{at}/part"), "names no parts[].id");
                    continue;
                };
                if p.out.end() <= z.out.start() || p.out.start() >= z.out.end() {
                    self.invalid(format!("{at}/part"), "a part the zoom does not overlap");
                }
                match p.layers.iter().find(|l| l.id == r.layer) {
                    None => self.invalid(format!("{at}/layer"), "names no layer of that part"),
                    Some(l) if l.cover.is_some() => {
                        self.invalid(format!("{at}/layer"), "a cover layer");
                    }
                    Some(_) => {}
                }
            }
        }
    }
}

/// Largest frame or sample index a renderer working in i64 can address.
const MAX_INDEX: u64 = i64::MAX as u64;

/// The largest `num` or `den` of a ramped part's speeds (decision 0033; the planner writes
/// hundredths), so its integral stays exact in i128.
const MAX_RAMP_TERM: u64 = 10_000;

/// The longest speed ramp, in output frames (decision 0033).
const MAX_RAMP_FRAMES: u64 = 1_000_000;

struct Check<'p> {
    plan: &'p Plan,
    errs: Vec<PlanError>,
}

impl Check<'_> {
    fn invalid(&mut self, path: impl Into<String>, reason: impl Into<String>) {
        self.errs.push(PlanError::invalid(path, reason));
    }

    fn unsupported(&mut self, path: impl Into<String>, reason: impl Into<String>) {
        self.errs.push(PlanError::unsupported(path, reason));
    }

    /// Rule 1: `start < end`. Returns whether the range is usable.
    fn range(&mut self, path: String, r: Range) -> bool {
        if r.start() < r.end() {
            true
        } else {
            self.invalid(path, "range start must be < end");
            false
        }
    }

    /// Rule 10: ids unique within one collection. Reports each repeat at its own path.
    fn unique<'a>(&mut self, base: &str, ids: impl Iterator<Item = &'a str>, field: &str) {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, id) in ids.enumerate() {
            if let Some(first) = seen.get(id) {
                let reason = format!("duplicate id (first used at {base}/{first}/{field})");
                self.invalid(format!("{base}/{i}/{field}"), reason);
            } else {
                seen.insert(id, i);
            }
        }
    }

    /// Rule 5's aspect tolerance between a crop and its place.
    fn aspect(&mut self, path: &str, c: crate::types::Rect, q: crate::types::Rect) {
        let crop_ar = c.h / c.w;
        let diff = (q.h / q.w - crop_ar).abs() / crop_ar;
        if diff.is_nan() || diff > ASPECT_TOLERANCE + 1e-12 {
            self.invalid(
                path,
                format!(
                    "aspect differs from the crop's by {:.3} % (max 0.5 %)",
                    diff * 100.0
                ),
            );
        }
    }

    /// A backdrop's or clip event's picture (decision 0028): its source exists, and a
    /// source at another rate than `output.fps` is read by time (`fps`, rule 3).
    fn clip_picture(
        &mut self,
        base: &str,
        p: &crate::types::ClipPicture,
        sources: &HashMap<&str, usize>,
    ) {
        let fps = self.plan.output.fps;
        let Some(&k) = sources.get(p.source.as_str()) else {
            self.invalid(format!("{base}/source"), "names no sources[].id");
            return;
        };
        let declared = self.plan.sources[k]
            .expect
            .as_ref()
            .and_then(|e| e.video.as_ref())
            .and_then(|v| v.fps);
        match p.fps {
            Some(r) if same_rate(r, fps) => self.invalid(
                format!("{base}/fps"),
                "equals output.fps (present only when the rates differ)",
            ),
            Some(r)
                if declared.is_some_and(|d| {
                    u128::from(d.num) * u128::from(r.den) != u128::from(r.num) * u128::from(d.den)
                }) =>
            {
                self.invalid(
                    format!("{base}/fps"),
                    "differs from the source's expect.video.fps",
                );
            }
            None if declared.is_some_and(|d| !same_rate(d, fps)) => self.unsupported(
                format!("{base}/fps"),
                "a source at another rate than output.fps needs fps (rate conversion)",
            ),
            _ => {}
        }
        if p.first_frame > MAX_INDEX / 2 {
            self.invalid(format!("{base}/first_frame"), "frame range overflows");
        }
    }

    fn run(&mut self) {
        let plan = self.plan;
        let fps = plan.output.fps;
        let frames = plan.output.frames;
        let total_samples = out_samples(frames, fps);
        if total_samples.is_none_or(|t| t > MAX_INDEX) {
            self.invalid("/output/frames", "too many frames for the sample clock");
        }
        if !fps.is_valid() {
            self.invalid(
                "/output/fps",
                "an integer rate over 1, or 24000, 30000 or 60000 over 1001",
            );
        }

        // `/1` upload rates (decision 0027): above the social ceiling only on a landscape
        // canvas, and never above the level's own MaxBR
        if let Some(r) = &plan.output.video.rate
            && let Some(max) = r.max_bps
            && max > SOCIAL_MAX_BPS
        {
            let (w, h) = (plan.output.canvas.w.get(), plan.output.canvas.h.get());
            let cap = plan.output.video.level.as_deref().and_then(level_max_bps);
            if w <= h {
                self.invalid(
                    "/output/video/rate/max_bps",
                    "above 25 Mbit/s only for an upload preset (a landscape canvas)",
                );
            } else if cap.is_none_or(|c| max > c) {
                self.invalid(
                    "/output/video/rate/max_bps",
                    "above the MaxBR of output.video.level (4.1/4.2: 62.5, 5.0: 168.75, 5.1/5.2: 300 Mbit/s)",
                );
            }
            if r.target_bps.is_some_and(|t| t > max) {
                self.invalid("/output/video/rate/target_bps", "above max_bps");
            }
        }

        // Sources: ids, and what v0 cannot render (rule 3 fps, rule 4 audio rate).
        self.unique("/sources", plan.sources.iter().map(|s| s.id.as_str()), "id");
        let source_index: HashMap<&str, usize> = plan
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        for (i, s) in plan.sources.iter().enumerate() {
            let id = s.id.as_str();
            let as_picture = plan.parts.iter().any(|p| {
                p.picture.source.as_str() == id
                    || p.layers
                        .iter()
                        .any(|l| l.picture_source.as_ref().is_some_and(|x| x.as_str() == id))
            });
            let as_audio = plan.parts.iter().any(|p| {
                p.audio.source.as_str() == id
                    || p.audio
                        .under
                        .iter()
                        .flatten()
                        .any(|u| u.source.as_str() == id)
            }) || plan
                .audio
                .overlays
                .iter()
                .flatten()
                .any(|o| o.source.as_str() == id);
            if s.audio_track == Some(0) {
                self.invalid(
                    format!("/sources/{i}/audio_track"),
                    "a container track id (1 or more)",
                );
            }
            let expect = s.expect.as_ref();
            // a part that converts the rate (`picture.fps`, `/1`, decision 0017) reads
            // the source by time, as does a layer of another source with `fps` (decision
            // 0047); every other use must be at the output rate
            let converted = plan
                .parts
                .iter()
                .filter(|p| p.picture.source.as_str() == id)
                .all(|p| p.picture.fps.is_some())
                && plan.parts.iter().all(|p| {
                    p.layers
                        .iter()
                        .filter(|l| l.picture_source.as_ref().is_some_and(|x| x.as_str() == id))
                        .all(|l| l.fps.is_some() && p.picture.source.as_str() != id)
                });
            if as_picture
                && !converted
                && let Some(f) = expect.and_then(|e| e.video.as_ref()).and_then(|v| v.fps)
                && !same_rate(f, fps)
            {
                self.unsupported(
                    format!("/sources/{i}/expect/video/fps"),
                    "a picture source at another rate than output.fps needs parts[].picture.fps (rate conversion, RenderPlan /1)",
                );
            }
            // `/1` reads a source at any rate converted to 48 kHz (decision 0051)
            if as_audio
                && plan.schema == crate::consts::SchemaVersion::V0
                && let Some(rate) = expect.and_then(|e| e.audio.as_ref()).and_then(|a| a.rate)
                && rate != AUDIO_RATE as i64
            {
                self.unsupported(
                    format!("/sources/{i}/expect/audio/rate"),
                    "v0 reads 48 kHz audio only (another rate is RenderPlan /1)",
                );
            }
        }

        // Fonts and styles (rule 10).
        let fonts = plan.fonts.as_deref().unwrap_or_default();
        self.unique("/fonts", fonts.iter().map(|f| f.id.as_str()), "id");
        let styles = plan.text_styles.as_ref();
        for (name, style) in styles.into_iter().flatten() {
            if !fonts.iter().any(|f| f.id == style.font) {
                self.invalid(
                    join_key(&join_key("/text_styles", name.as_str()), "font"),
                    "names no fonts[].id",
                );
            }
        }

        // Parts: ids, ranges, tiling (rules 1, 2), sources (10), sample spans (4),
        // layers (5, 10), fades (7).
        self.unique("/parts", plan.parts.iter().map(|p| p.id.as_str()), "id");
        let mut expected_start = Some(0_u64);
        for (i, p) in plan.parts.iter().enumerate() {
            let base = format!("/parts/{i}");
            let ok = self.range(format!("{base}/out"), p.out);
            if ok {
                match expected_start {
                    Some(0) if i == 0 && p.out.start() != 0 => {
                        self.invalid(
                            format!("{base}/out"),
                            "the first part must start at frame 0",
                        );
                    }
                    Some(e) if p.out.start() > e => {
                        self.invalid(
                            format!("{base}/out"),
                            "gap: starts after the previous part ends",
                        );
                    }
                    Some(e) if p.out.start() < e => {
                        self.invalid(
                            format!("{base}/out"),
                            "overlap: starts before the previous part ends",
                        );
                    }
                    _ => {}
                }
                expected_start = Some(p.out.end());
            } else {
                expected_start = None;
            }
            if let Some(n) = &p.native
                && let Some(r) = n.frames
            {
                self.range(format!("{base}/native/frames"), r);
            }

            if !source_index.contains_key(p.picture.source.as_str()) {
                self.invalid(format!("{base}/picture/source"), "names no sources[].id");
            }
            if let Some(r) = p.picture.fps {
                let declared = source_index
                    .get(p.picture.source.as_str())
                    .and_then(|&k| plan.sources[k].expect.as_ref())
                    .and_then(|e| e.video.as_ref())
                    .and_then(|v| v.fps);
                if same_rate(r, fps) {
                    self.invalid(
                        format!("{base}/picture/fps"),
                        "equals output.fps (present only when the rates differ)",
                    );
                } else if declared.is_some_and(|d| {
                    u128::from(d.num) * u128::from(r.den) != u128::from(r.num) * u128::from(d.den)
                }) {
                    self.invalid(
                        format!("{base}/picture/fps"),
                        "differs from the source's expect.video.fps",
                    );
                }
            }
            // decision 0030: a speed in [1/4, 4], never 1 (but a ramp may start at 1)
            if let Some(sp) = p.speed {
                let (n, d) = (u128::from(sp.num), u128::from(sp.den));
                if (n == d && sp.ramp.is_none()) || n * 4 < d || n > 4 * d {
                    self.invalid(
                        format!("{base}/speed"),
                        "num/den in [1/4, 4] and not 1 (absent at normal speed)",
                    );
                }
                // decision 0033: a ramp to another speed in [1/4, 4], inside the part
                if let Some(r) = sp.ramp {
                    let (bn, bd) = (u128::from(r.to.num), u128::from(r.to.den));
                    if bd == 0 || bn * 4 < bd || bn > 4 * bd || bn * d == n * bd {
                        self.invalid(
                            format!("{base}/speed/ramp/to"),
                            "num/den in [1/4, 4] and not the part's own speed",
                        );
                    }
                    if [sp.num, sp.den, r.to.num, r.to.den]
                        .iter()
                        .any(|v| *v > MAX_RAMP_TERM)
                    {
                        self.invalid(
                            format!("{base}/speed/ramp"),
                            "num and den of both speeds at most 10000",
                        );
                    }
                    if r.frames == 0
                        || r.frames > MAX_RAMP_FRAMES
                        || r.from.checked_add(r.frames).is_none_or(|e| e > p.out.len())
                    {
                        self.invalid(
                            format!("{base}/speed/ramp/frames"),
                            "1 to 1000000 frames, inside the part",
                        );
                    }
                }
            }
            let reach = match p.speed {
                Some(sp) if sp.ramp.is_some() => sp.source_frames(p.out.len()).unwrap_or(u64::MAX),
                Some(sp) => sp.source_samples(p.out.len()),
                None => p.out.len(),
            };
            if ok
                && p.picture
                    .first_frame
                    .checked_add(reach)
                    .is_none_or(|e| e > MAX_INDEX)
            {
                self.invalid(
                    format!("{base}/picture/first_frame"),
                    "frame range overflows",
                );
            }

            if !source_index.contains_key(p.audio.source.as_str()) {
                self.invalid(format!("{base}/audio/source"), "names no sources[].id");
            }
            let samples = if ok { part_samples(p.out, fps) } else { None };
            if let Some(n) = samples {
                if p.audio
                    .first_sample
                    .checked_add(n)
                    .is_none_or(|e| e > MAX_INDEX)
                {
                    self.invalid(
                        format!("{base}/audio/first_sample"),
                        "sample range overflows",
                    );
                }
                for (at, fade) in [
                    ("fade_in", p.audio.fade_in()),
                    ("fade_out", p.audio.fade_out()),
                ] {
                    if fade.is_some_and(|f| f.samples > n) {
                        self.invalid(
                            format!("{base}/audio/{at}"),
                            "fade is longer than the part's own samples",
                        );
                    }
                }
            }
            // decision 0051: sources heard under the part's audio
            for (j, u) in p.audio.under.iter().flatten().enumerate() {
                let ub = format!("{base}/audio/under/{j}");
                if !source_index.contains_key(u.source.as_str()) {
                    self.invalid(format!("{ub}/source"), "names no sources[].id");
                }
                if p.speed.is_some() {
                    self.invalid(
                        ub.clone(),
                        "a part that changes speed hears only its own audio",
                    );
                }
                if samples
                    .is_some_and(|n| u.first_sample.checked_add(n).is_none_or(|e| e > MAX_INDEX))
                {
                    self.invalid(format!("{ub}/first_sample"), "sample range overflows");
                }
            }

            self.unique(
                &format!("{base}/layers"),
                p.layers.iter().map(|l| l.id.as_str()),
                "id",
            );
            for (j, l) in p.layers.iter().enumerate() {
                let lb = format!("{base}/layers/{j}");
                if !LAYER_ROLES.contains(&l.role.as_str()) {
                    self.invalid(
                        format!("{lb}/role"),
                        format!("must be one of {}", LAYER_ROLES.join(", ")),
                    );
                }
                if let Some(ps) = &l.picture_source
                    && !source_index.contains_key(ps.as_str())
                {
                    self.invalid(format!("{lb}/picture_source"), "names no sources[].id");
                }
                // decision 0021: a cover obscures its crop; it is never cut out
                if l.cover.is_some() && l.matte.is_some() {
                    self.invalid(format!("{lb}/cover"), "a cover layer has no matte");
                }
                // decision 0046: a frame shapes a picture; a cover is not framed
                if l.cover.is_some() && l.frame.is_some() {
                    self.invalid(format!("{lb}/frame"), "a cover layer has no frame");
                }
                // decision 0047: a layer of another source at its own rate
                if let Some(r) = l.fps {
                    let declared = l
                        .picture_source
                        .as_ref()
                        .and_then(|x| source_index.get(x.as_str()))
                        .and_then(|&k| plan.sources[k].expect.as_ref())
                        .and_then(|e| e.video.as_ref())
                        .and_then(|v| v.fps);
                    if l.picture_source
                        .as_ref()
                        .is_none_or(|x| x == &p.picture.source)
                    {
                        self.invalid(
                            format!("{lb}/fps"),
                            "only with a picture_source other than the part's",
                        );
                    } else if same_rate(r, fps) {
                        self.invalid(
                            format!("{lb}/fps"),
                            "equals output.fps (present only when the rates differ)",
                        );
                    } else if declared.is_some_and(|d| {
                        u128::from(d.num) * u128::from(r.den)
                            != u128::from(r.num) * u128::from(d.den)
                    }) {
                        self.invalid(
                            format!("{lb}/fps"),
                            "differs from the source's expect.video.fps",
                        );
                    }
                }
                // decision 0046: another source read live, on the part's own clock
                if l.first_frame.is_some() {
                    let other = l
                        .picture_source
                        .as_ref()
                        .is_some_and(|x| x != &p.picture.source);
                    if !other {
                        self.invalid(
                            format!("{lb}/first_frame"),
                            "only with a picture_source other than the part's",
                        );
                    } else if matches!(l.time_map, Some(crate::types::TimeMap::Hold { .. })) {
                        self.invalid(
                            format!("{lb}/first_frame"),
                            "a held layer reads its hold frame",
                        );
                    } else if p.speed.is_some() {
                        self.unsupported(
                            format!("{lb}/first_frame"),
                            "another source read live on a part that changes speed",
                        );
                    } else if l.first_frame.is_some_and(|f| f > MAX_INDEX / 2) {
                        self.invalid(format!("{lb}/first_frame"), "frame range overflows");
                    }
                }
                // decision 0033: a pinned cover's keys increase inside the part
                if let Some(t) = &l.track {
                    if l.cover.is_none() {
                        self.invalid(format!("{lb}/track"), "only a cover layer is pinned");
                    }
                    let inside = t.keys.iter().all(|k| {
                        p.out.start() <= k.frame
                            && k.frame < p.out.end()
                            && k.x.is_finite()
                            && k.y.is_finite()
                    });
                    if t.keys.is_empty()
                        || !inside
                        || !t.keys.windows(2).all(|w| w[0].frame < w[1].frame)
                        || !(t.x.is_finite() && t.y.is_finite())
                    {
                        self.invalid(
                            format!("{lb}/track/keys"),
                            "frames increasing inside the part's out, points finite",
                        );
                    }
                }
                let (c, q) = (l.crop, l.place);
                let crop_ar = c.h / c.w;
                let place_ar = q.h / q.w;
                let diff = (place_ar - crop_ar).abs() / crop_ar;
                // NaN cannot occur (crop/place sides are > 0); written so it would refuse.
                if diff.is_nan() || diff > ASPECT_TOLERANCE + 1e-12 {
                    self.invalid(
                        format!("{lb}/place"),
                        format!(
                            "aspect differs from the crop's by {:.3} % (max 0.5 %)",
                            diff * 100.0
                        ),
                    );
                }
            }
        }
        if let Some(end) = expected_start
            && !plan.parts.is_empty()
            && end != frames
        {
            self.invalid(
                "/output/frames",
                format!("parts end at frame {end}, so output.frames must equal it"),
            );
        }

        // Rule 7: no fade at a continuous join.
        for p in 0..plan.parts.len().saturating_sub(1) {
            if continuous_join(plan, p) {
                if plan.parts[p].audio.fade_out().is_some() {
                    self.invalid(
                        format!("/parts/{p}/audio/fade_out"),
                        "fade at a continuous join (rule 7)",
                    );
                }
                if plan.parts[p + 1].audio.fade_in().is_some() {
                    self.invalid(
                        format!("/parts/{}/audio/fade_in", p + 1),
                        "fade at a continuous join (rule 7)",
                    );
                }
            }
        }

        // Text events (rules 1, 2, 10).
        let events = plan.text_events.as_deref().unwrap_or_default();
        self.unique("/text_events", events.iter().map(|e| e.id.as_str()), "id");
        for (i, e) in events.iter().enumerate() {
            let base = format!("/text_events/{i}");
            if self.range(format!("{base}/out"), e.out) && e.out.end() > frames {
                self.invalid(format!("{base}/out"), "ends after output.frames");
            }
            if !styles.is_some_and(|s| s.contains_key(e.style.as_str())) {
                self.invalid(format!("{base}/style"), "names no text_styles key");
            }
            let pops = e.anim.as_ref().and_then(|a| a.pop_runs.as_deref());
            for (k, r) in pops.unwrap_or_default().iter().enumerate() {
                if *r >= e.runs.len() as u64 {
                    self.invalid(format!("{base}/anim/pop_runs/{k}"), "names no runs[] index");
                }
            }
        }

        // `/1` image and shape events (rules 1, 2, 10; decision 0014).
        let images = plan.images.as_deref().unwrap_or_default();
        self.unique("/images", images.iter().map(|m| m.id.as_str()), "id");
        let ievs = plan.image_events.as_deref().unwrap_or_default();
        self.unique("/image_events", ievs.iter().map(|e| e.id.as_str()), "id");
        for (i, e) in ievs.iter().enumerate() {
            let base = format!("/image_events/{i}");
            if self.range(format!("{base}/out"), e.out) && e.out.end() > frames {
                self.invalid(format!("{base}/out"), "ends after output.frames");
            }
            if !images.iter().any(|m| m.id == e.image) {
                self.invalid(format!("{base}/image"), "names no images[].id");
            }
        }
        let sevs = plan.shape_events.as_deref().unwrap_or_default();
        self.unique("/shape_events", sevs.iter().map(|e| e.id.as_str()), "id");
        for (i, e) in sevs.iter().enumerate() {
            let base = format!("/shape_events/{i}");
            if self.range(format!("{base}/out"), e.out) && e.out.end() > frames {
                self.invalid(format!("{base}/out"), "ends after output.frames");
            }
        }

        // `/1` backdrop and clip events (rules 1, 2, 3, 5, 10; decision 0028).
        if let crate::types::Background::Backdrop {
            crop,
            place,
            video,
            image,
            ..
        } = &plan.background
        {
            match (video, image) {
                (Some(v), None) => self.clip_picture("/background/video", v, &source_index),
                (None, Some(id)) => {
                    if !images.iter().any(|m| &m.id == id) {
                        self.invalid("/background/image", "names no images[].id");
                    }
                }
                _ => self.invalid(
                    "/background",
                    "a backdrop has exactly one of video and image",
                ),
            }
            self.aspect("/background/place", *crop, *place);
        }
        let cevs = plan.clip_events.as_deref().unwrap_or_default();
        self.unique("/clip_events", cevs.iter().map(|e| e.id.as_str()), "id");
        for (i, e) in cevs.iter().enumerate() {
            let base = format!("/clip_events/{i}");
            if self.range(format!("{base}/out"), e.out) && e.out.end() > frames {
                self.invalid(format!("{base}/out"), "ends after output.frames");
            }
            self.clip_picture(&format!("{base}/picture"), &e.picture, &source_index);
            self.aspect(&format!("{base}/place"), e.crop, e.place);
        }

        // `/1` tracks (decision 0031): the event exists, keys increase inside its span.
        let tracks = plan.tracks.as_deref().unwrap_or_default();
        let mut seen: Vec<(crate::types::TrackedKind, &str)> = Vec::new();
        for (i, t) in tracks.iter().enumerate() {
            let base = format!("/tracks/{i}");
            let out = match t.kind {
                crate::types::TrackedKind::Text => {
                    events.iter().find(|e| e.id == t.event).map(|e| e.out)
                }
                crate::types::TrackedKind::Image => {
                    ievs.iter().find(|e| e.id == t.event).map(|e| e.out)
                }
                crate::types::TrackedKind::Shape => {
                    sevs.iter().find(|e| e.id == t.event).map(|e| e.out)
                }
            };
            let Some(out) = out else {
                self.invalid(format!("{base}/event"), "names no event of that kind");
                continue;
            };
            if seen.contains(&(t.kind, t.event.as_str())) {
                self.invalid(format!("{base}/event"), "an event tracked twice");
            }
            seen.push((t.kind, t.event.as_str()));
            for (k, key) in t.keys.iter().enumerate() {
                let at = format!("{base}/keys/{k}");
                if key.frame < out.start() || key.frame >= out.end() {
                    self.invalid(format!("{at}/frame"), "outside the event's out");
                } else if k > 0 && key.frame <= t.keys[k - 1].frame {
                    self.invalid(format!("{at}/frame"), "keys must increase");
                }
                if !(key.x.is_finite() && key.y.is_finite()) {
                    self.invalid(at, "a finite point");
                }
            }
        }

        // `/1` cursors (decision 0045): a source's track and images; a layer draws its own.
        let cursors = plan.cursors.as_deref().unwrap_or_default();
        self.unique("/cursors", cursors.iter().map(|c| c.id.as_str()), "id");
        for (i, c) in cursors.iter().enumerate() {
            let base = format!("/cursors/{i}");
            if !source_index.contains_key(c.source.as_str()) {
                self.invalid(format!("{base}/source"), "names no sources[].id");
            }
            for (k, s) in c.shapes.iter().enumerate() {
                if !images.iter().any(|m| &m.id == s) {
                    self.invalid(format!("{base}/shapes/{k}"), "names no images[].id");
                }
            }
        }
        for (pi, p) in plan.parts.iter().enumerate() {
            for (j, l) in p.layers.iter().enumerate() {
                let Some(id) = &l.cursor else { continue };
                let at = format!("/parts/{pi}/layers/{j}/cursor");
                let pic = l.picture_source.as_ref().unwrap_or(&p.picture.source);
                match cursors.iter().find(|c| &c.id == id) {
                    None => self.invalid(at, "names no cursors[].id"),
                    Some(c) if &c.source != pic => {
                        self.invalid(at, "a cursor of another source than the layer's picture");
                    }
                    Some(_) => {}
                }
            }
        }

        // `/1` zooms (decision 0046).
        self.zooms();

        // Envelopes (rules 1, 4, 7, 10).
        let envs = plan.audio.envelopes.as_deref().unwrap_or_default();
        self.unique("/audio/envelopes", envs.iter().map(|e| e.id.as_str()), "id");
        let mut spans: BTreeMap<(u64, usize), u64> = BTreeMap::new();
        for (i, e) in envs.iter().enumerate() {
            let base = format!("/audio/envelopes/{i}");
            if !self.range(format!("{base}/out_samples"), e.out_samples) {
                continue;
            }
            if total_samples.is_some_and(|t| e.out_samples.end() > t) {
                self.invalid(
                    format!("{base}/out_samples"),
                    "ends after the last output sample, S(output.frames)",
                );
            }
            let len = e.out_samples.len();
            if e.ramp_in_samples.saturating_add(e.ramp_out_samples) > len {
                let at = if e.ramp_in_samples > len {
                    "ramp_in_samples"
                } else {
                    "ramp_out_samples"
                };
                self.invalid(
                    format!("{base}/{at}"),
                    "ramps do not fit inside the envelope",
                );
            }
            spans.insert((e.out_samples.start(), i), e.out_samples.end());
        }
        let mut prev: Option<(usize, u64)> = None;
        for (&(start, i), &end) in &spans {
            if let Some((j, prev_end)) = prev
                && start < prev_end
            {
                self.invalid(
                    format!("/audio/envelopes/{i}/out_samples"),
                    format!("overlaps /audio/envelopes/{j}"),
                );
            }
            if prev.is_none_or(|(_, e)| end > e) {
                prev = Some((i, end));
            }
        }

        // `/1` audio overlays and the speech they duck under (decision 0051).
        let overlays = plan.audio.overlays.as_deref().unwrap_or_default();
        self.unique(
            "/audio/overlays",
            overlays.iter().map(|o| o.id.as_str()),
            "id",
        );
        for (i, o) in overlays.iter().enumerate() {
            let base = format!("/audio/overlays/{i}");
            if !source_index.contains_key(o.source.as_str()) {
                self.invalid(format!("{base}/source"), "names no sources[].id");
            }
            if !self.range(format!("{base}/out_samples"), o.out_samples) {
                continue;
            }
            if total_samples.is_some_and(|t| o.out_samples.end() > t) {
                self.invalid(
                    format!("{base}/out_samples"),
                    "ends after the last output sample, S(output.frames)",
                );
            }
            let len = o.out_samples.len();
            if o.first_sample
                .checked_add(len)
                .is_none_or(|e| e > MAX_INDEX)
            {
                self.invalid(format!("{base}/first_sample"), "sample range overflows");
            }
            let (fi, fo) = (
                o.fade_in_samples.unwrap_or(0),
                o.fade_out_samples.unwrap_or(0),
            );
            if fi.saturating_add(fo) > len {
                self.invalid(
                    format!("{base}/fade_in_samples"),
                    "fades do not fit inside the overlay",
                );
            }
        }
        let mut speech_end = 0u64;
        for (i, r) in plan.audio.speech.iter().flatten().enumerate() {
            let at = format!("/audio/speech/{i}");
            if !self.range(at.clone(), *r) {
                continue;
            }
            if r.start() < speech_end || total_samples.is_some_and(|t| r.end() > t) {
                self.invalid(at, "in order, apart, inside [0, S(output.frames)]");
            }
            speech_end = r.end();
        }
    }
}
