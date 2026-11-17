//! RenderPlan v0 types (RP §2.2), one Rust type per schema object.
//!
//! Every record is `deny_unknown_fields` (RP rule 11), every optional field is an
//! `Option` that is skipped when absent (so a plan round-trips without gaining
//! defaults), and `schemars` derives a schema whose shape — property names, required
//! sets, closedness, enum values and JSON types — is checked against the committed
//! `schemas/renderplan@0.json` by `tests/schema_matches_types.rs`. The committed file is
//! the contract; these types follow it.
//!
//! Integers: counts and indices that the schema bounds at ≥ 0 are `u64`; signed ones
//! (`z`, `layer`, `mtime_ns`, `ticks`, the unconstrained `expect` numbers) are `i64`.
//! Constraints the type system does not carry (`minimum`, patterns, `multipleOf` …) are
//! enforced by the schema pass in [`crate::schema`] before a plan is deserialised.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::consts::{
    AacLc, Bt709, ConstFalse, ConstU64, H264, Lanczos3, LibassBs3, LinearDb, Roto, RoundJoin,
    SchemaVersion, Vbr,
};
use crate::scalars::{Even, FpsDen, FpsNum, Id, Range, Rgb, Rgba, RoleName, Sha256};

/// A whole RenderPlan (`gmclip.renderplan/0`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: SchemaVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    pub output: Output,
    pub background: Background,
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<Vec<Font>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_styles: Option<BTreeMap<Id, TextStyle>>,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_events: Option<Vec<TextEvent>>,
    /// `/1`: the image files image events draw (decoded per R-MEDIA-SEC-3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Image>>,
    /// `/1`: images drawn above every layer on `[out[0], out[1])`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_events: Option<Vec<ImageEvent>>,
    /// `/1`: callouts (boxes and arrows) drawn above every layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_events: Option<Vec<ShapeEvent>>,
    /// `/1`: videos drawn picture-in-picture above every part layer (decision 0028).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_events: Option<Vec<ClipEvent>>,
    /// `/1`: overlays pinned to a moving point (decision 0031).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<EventTrack>>,
    /// `/1`: recorded cursors, drawn over the layers that name them (decision 0045).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursors: Option<Vec<CursorTrack>>,
    /// `/1`: zoom sections (decision 0046), applied to the layers they name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zooms: Option<Vec<Zoom>>,
    pub audio: Audio,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Checks>,
}

impl Plan {
    /// Resolves every relative file path — sources, fonts, images and the reference
    /// render — against `base` (the folder the plan's files are relative to). Absolute
    /// paths are kept. Paths never change `render_sig` (RP rule 9).
    pub fn resolve_paths(&mut self, base: &std::path::Path) {
        let fix = |p: &mut String| {
            if std::path::Path::new(p.as_str()).is_relative() {
                *p = base.join(p.as_str()).to_string_lossy().into_owned();
            }
        };
        self.sources.iter_mut().for_each(|s| fix(&mut s.path));
        self.fonts
            .iter_mut()
            .flatten()
            .for_each(|f| fix(&mut f.path));
        self.images
            .iter_mut()
            .flatten()
            .for_each(|i| fix(&mut i.path));
        self.cursors
            .iter_mut()
            .flatten()
            .for_each(|c| fix(&mut c.path));
        if let Some(p) = self
            .checks
            .as_mut()
            .and_then(|c| c.reference.as_mut())
            .and_then(|r| r.path.as_mut())
        {
            fix(p);
        }
    }
}

/// Descriptive metadata; never affects pixels or samples.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// the reference `merges[]` / `merged_content_ids[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<Generator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<Input>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_report: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Generator {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub role: InputRole,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputRole {
    #[serde(rename = "reference_mp4")]
    ReferenceMp4,
    Model,
}

/// The output: canvas, clock and encoder settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub canvas: Canvas,
    pub fps: Fps,
    /// Total output frames; `parts[].out` tile `[0, frames)` (rule 2).
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    pub video: Video,
    pub audio: OutputAudio,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    pub w: Even,
    pub h: Even,
}

/// The output rate (rule 4): `num` ∈ {24, 25, 30, 48, 50, 60} over 1, or under `/1`
/// (decision 0020) {24000, 30000, 60000} over 1001.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fps {
    pub num: FpsNum,
    pub den: FpsDen,
}

impl Fps {
    /// An integer rate (panics on one RenderPlan does not allow; for constants and tests).
    pub fn integer(n: u64) -> Self {
        Self {
            num: FpsNum::new(n).expect("an allowed integer rate"),
            den: FpsDen::ONE,
        }
    }

    /// A rate from `num/den` when RenderPlan allows it (1 or 1001, matching numerators).
    pub fn new(num: u64, den: u64) -> Result<Self, String> {
        let ok = (den == 1 && FpsNum::ALLOWED.contains(&num))
            || (den == 1001 && FpsNum::NTSC.contains(&num));
        if !ok {
            return Err(format!(
                "{num}/{den} is not an output rate (24, 25, 30, 48, 50, 60, or 24000, 30000, 60000 over 1001)"
            ));
        }
        Ok(Self {
            num: FpsNum::new(num)?,
            den: FpsDen::new(den)?,
        })
    }

    /// `(num, den)`.
    pub fn rate(self) -> (u64, u64) {
        (self.num.get(), self.den.get())
    }

    /// Frames per second.
    pub fn as_f64(self) -> f64 {
        self.num.get() as f64 / self.den.get() as f64
    }

    /// Whether `num/den` is a pair RenderPlan allows (rule 4).
    pub fn is_valid(self) -> bool {
        Self::new(self.num.get(), self.den.get()).is_ok()
    }

    /// Whether this is an NTSC rate (`/1` only).
    pub fn is_ntsc(self) -> bool {
        self.den.get() != 1
    }

    /// Flicks (1/705 600 000 s) per frame: exact for every allowed rate.
    pub fn flicks_per_frame(self) -> u64 {
        705_600_000 * self.den.get() / self.num.get()
    }

    /// The integer rate nearest (30 for 29.97): GOPs, animation step counts.
    pub fn rounded(self) -> u64 {
        (self.num.get() + self.den.get() / 2) / self.den.get()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Color {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Bt709>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Bt709>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries: Option<Bt709>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ColorRange>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorRange {
    Limited,
    Full,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Video {
    pub codec: H264,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<VideoProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bframes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gop_frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_gop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<VideoRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timescale: Option<u64>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfile {
    High,
    Main,
    ConstrainedBaseline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VideoRate {
    pub mode: Vbr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_bps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bps: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputAudio {
    pub codec: AacLc,
    pub rate: ConstU64<48000>,
    pub channels: ConstU64<2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Container {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moov_first: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_lists: Option<ConstFalse>,
}

/// What is drawn under every layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Background {
    /// SPEC "solid colour".
    Colour {
        rgb: Rgb,
    },
    Plate {
        stops: Vec<Rgb>,
    },
    /// `/1`: the part's own picture, scaled to cover the canvas, blurred and dimmed
    /// (SPEC §5.7 "blurred source"); `rgb` shows where no picture is drawn.
    BlurredSource {
        rgb: Rgb,
        /// The picture is resampled to `canvas / blur` (Lanczos-3) and back up with a cubic
        /// B-spline: about a Gaussian of σ ≈ 0.6·blur canvas px.
        blur: u64,
        /// 0 = as is, 1 = black.
        dim: f64,
    },
    /// `/1` (decision 0028): a video or a still under every part, on the output clock, so
    /// it runs on across part joins. `crop` (picture px) is drawn into `place` (canvas px)
    /// through the layer path; `rgb` shows where it does not cover. Exactly one of
    /// `video` and `image`.
    Backdrop {
        rgb: Rgb,
        crop: Rect,
        place: Rect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video: Option<ClipPicture>,
        /// An `images[].id`: a still backdrop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<Id>,
    },
}

/// Which frame of a video a backdrop or a clip event shows (`/1`, decision 0028): its
/// `k`-th output frame (from the output's frame 0 for a backdrop, from `out[0]` for a clip
/// event) is `k' = k mod frames` when it loops, else `min(k, frames − 1)` (`frames`
/// absent: `k' = k`); then presentation index `first_frame + k'`, or with `fps` the frame
/// presented at or before `(first_frame + k') / output.fps` on the source's clock
/// (decision 0017's rule).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipPicture {
    pub source: Id,
    pub first_frame: u64,
    /// The source's nominal rate when it differs from `output.fps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
    /// The media's length in output frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub frames: Option<u64>,
    /// Start again from `first_frame` after `frames` (else the last frame holds).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "loop")]
    pub r#loop: Option<bool>,
}

impl ClipPicture {
    /// The media's own output frame shown `k` output frames in.
    pub fn local(&self, k: u64) -> u64 {
        match self.frames {
            Some(n) if n > 0 && self.r#loop == Some(true) => k % n,
            Some(n) if n > 0 => k.min(n - 1),
            _ => k,
        }
    }
}

/// An overlay pinned to a moving point (`/1`, decision 0031): the point's canvas position
/// at some output frames of the event. On output frame `f` the event is drawn moved by
/// `p(f) − p(out[0])`, rounded to whole pixels, where `p` is the keys' straight-line
/// interpolation (held before the first key and after the last).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventTrack {
    pub kind: TrackedKind,
    /// The event's id in its collection.
    pub event: Id,
    pub keys: Vec<TrackKey>,
}

/// Which collection a tracked event is in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackedKind {
    Text,
    Image,
    Shape,
}

/// The pinned point on an output frame (canvas px).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackKey {
    pub frame: u64,
    pub x: f64,
    pub y: f64,
}

/// The point at output frame `f` of keys in increasing frames (straight lines between
/// keys, held past the ends).
fn track_at(k: &[TrackKey], f: u64) -> (f64, f64) {
    let Some(first) = k.first() else {
        return (0.0, 0.0);
    };
    if f <= first.frame {
        return (first.x, first.y);
    }
    let i = k.partition_point(|x| x.frame <= f);
    if i >= k.len() {
        let l = k[k.len() - 1];
        return (l.x, l.y);
    }
    let (a, b) = (k[i - 1], k[i]);
    let t = (f - a.frame) as f64 / (b.frame - a.frame) as f64;
    (a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

impl EventTrack {
    /// The point at output frame `f` (straight lines between keys, held past the ends).
    pub fn at(&self, f: u64) -> (f64, f64) {
        track_at(&self.keys, f)
    }

    /// The event's move on output frame `f`, from where it is drawn at `start`
    /// (`out[0]`): whole pixels, rounded half away from zero.
    pub fn offset(&self, start: u64, f: u64) -> (i32, i32) {
        let (a, b) = (self.at(start), self.at(f));
        ((b.0 - a.0).round() as i32, (b.1 - a.1).round() as i32)
    }
}

/// A video drawn picture-in-picture on `[out[0], out[1])` (`/1`, decision 0028): `crop`
/// (source px) into `place` (canvas px) through the layer path, above every part layer
/// and under every image, shape and text event, by `layer` then array order. Silent.
/// With `z` (decision 0042) it is drawn among the part's layers instead: just under the
/// first one whose `z` is at least its own (above them all when none is), and `matte`
/// cuts it out like a part layer's (decision 0013).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipEvent {
    pub id: Id,
    pub out: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<i64>,
    pub picture: ClipPicture,
    pub crop: Rect,
    pub place: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Drawn among the part's layers, in their `z` space (decision 0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<LayerMatte>,
}

/// A media file a part reads picture or audio from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: Id,
    pub path: String,
    /// The container track id (MP4 `track_ID`, Matroska `TrackNumber`) every part's
    /// audio from this source is read from; absent = the first audio track (rule 4,
    /// decision 0009: the mic of a multi-track recording).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub audio_track: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<ExpectVideo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<ExpectAudio>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectVideo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<VideoCodec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timescale: Option<i64>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
    Vp9,
    Av1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectAudio {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<i64>,
}

/// `num/den`, both ≥ 1.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rational {
    pub num: u64,
    pub den: u64,
}

/// A font file and its line metric (rule 6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Font {
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    pub path: String,
    pub sha256: Sha256,
    /// unitsPerEm / (usWinAscent + usWinDescent).
    pub em_per_size: f64,
}

/// A named text style; `size` is nominal, events carry their own (rule 6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextStyle {
    pub font: Id,
    pub size: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Outline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub text_box: Option<TextBox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<RoundJoin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dx: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dy: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<ShadowOf>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadowOf {
    FillAndOutline,
    Fill,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextBox {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<LibassBs3>,
}

/// One part: `out` output frames of one picture source and one audio source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub out: Range,
    pub picture: Picture,
    pub audio: PartAudio,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
    pub layers: Vec<Layer>,
    /// `/1` (decision 0030): the part plays its media at `num/den` × (¼ to 4, never 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<PartSpeed>,
}

/// A part's constant speed (`/1`, decision 0030). Output frame `k` of the part shows the
/// picture at source time `(first_frame + k · num/den) / output.fps` (by the source's clock
/// when the part converts its rate, else frame `⌊first_frame + k · num/den⌋`); its sound
/// reads `S(out1) − S(out0)` output samples' worth × `num/den` source samples from
/// `first_sample`, time-stretched to the part's length with its pitch kept, or silent.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSpeed {
    #[schemars(range(min = 1))]
    pub num: u64,
    #[schemars(range(min = 1))]
    pub den: u64,
    /// `stretch` (default) or `mute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<SpeedAudio>,
    /// `/1` (decision 0033): the speed eases from `num/den` to `ramp.to` over the part's
    /// output frames `[from, from + frames)` and holds `to` after them. With a ramp,
    /// `num/den` may be 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp: Option<SpeedRamp>,
}

/// An eased speed change inside a part (`/1`, decision 0033). The speed on the part's
/// output frame `x` is `A + (B − A) · curve((x − from) / frames)` (`A` the part's
/// `num/den`, `B` = `to`, the curve clamped to [0, 1]), and the source the part has
/// advanced by its output frame `k` is that speed's integral from 0 to `k`, in source
/// frames at the output rate ([`PartSpeed::advance`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpeedRamp {
    /// The speed reached at the ramp's end, `num/den` in [¼, 4].
    pub to: Rational,
    /// The part's output frame the ramp starts on (0 = its first).
    pub from: u64,
    /// The ramp's length in output frames (≥ 1).
    #[schemars(range(min = 1))]
    pub frames: u64,
    pub curve: EaseCurve,
}

/// `12 N³ · ∫₀^(j/N) curve` for `0 ≤ j ≤ N`: an integer for each of the four curves (each
/// is piecewise quadratic, so its integral is a cubic with these denominators).
fn ramp_integral(curve: EaseCurve, j: i128, n: i128) -> i128 {
    match curve {
        EaseCurve::Linear => 6 * n * j * j,
        EaseCurve::EaseIn => 4 * j * j * j,
        EaseCurve::EaseOut => 12 * n * n * j - 4 * (n * n * n - (n - j) * (n - j) * (n - j)),
        EaseCurve::EaseInOut => {
            if 2 * j < n {
                8 * j * j * j
            } else {
                12 * n * n * j - 6 * n * n * n + 8 * (n - j) * (n - j) * (n - j)
            }
        }
    }
}

/// [`ramp_integral`] at a real `j` (the sound's per-sample position).
fn ramp_integral_f64(curve: EaseCurve, j: f64, n: f64) -> f64 {
    match curve {
        EaseCurve::Linear => 6.0 * n * j * j,
        EaseCurve::EaseIn => 4.0 * j * j * j,
        EaseCurve::EaseOut => 12.0 * n * n * j - 4.0 * (n * n * n - (n - j) * (n - j) * (n - j)),
        EaseCurve::EaseInOut => {
            if 2.0 * j < n {
                8.0 * j * j * j
            } else {
                12.0 * n * n * j - 6.0 * n * n * n + 8.0 * (n - j) * (n - j) * (n - j)
            }
        }
    }
}

fn gcd_i128(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

impl PartSpeed {
    /// Whether the sound plays (time-stretched) rather than being muted.
    pub fn stretches(&self) -> bool {
        self.audio != Some(SpeedAudio::Mute)
    }

    /// The source samples a part of `n` output samples reads at its constant speed:
    /// `⌈n · num / den⌉` (a ramp reads [`PartSpeed::source_samples_at`]).
    pub fn source_samples(&self, n: u64) -> u64 {
        let v = (u128::from(n) * u128::from(self.num)).div_ceil(u128::from(self.den.max(1)));
        u64::try_from(v).unwrap_or(u64::MAX)
    }

    /// The source the part has advanced by its output frame `k`, in source frames at the
    /// output rate, as an exact fraction `(num, den)` (`den > 0`, reduced): `k · num/den`
    /// at a constant speed; with a ramp, the speed's integral
    /// `A·k + (B − A) · T(k) / (12 N²)` where `T` is `12 N³ ∫ curve` over the ramp's
    /// frames so far ([`SpeedRamp`]). `None` when the arithmetic would overflow (a plan
    /// that does is refused by validation).
    pub fn advance(&self, k: u64) -> Option<(i128, i128)> {
        let (an, ad) = (i128::from(self.num), i128::from(self.den.max(1)));
        let k = i128::from(k);
        let Some(r) = self.ramp else {
            return Some((an.checked_mul(k)?, ad));
        };
        let (bn, bd) = (i128::from(r.to.num), i128::from(r.to.den.max(1)));
        let (f, n) = (i128::from(r.from), i128::from(r.frames.max(1)));
        let d12 = 12i128.checked_mul(n)?.checked_mul(n)?;
        let t = if k <= f {
            0
        } else if k < f + n {
            ramp_integral(r.curve, k - f, n)
        } else {
            ramp_integral(r.curve, n, n).checked_add(d12.checked_mul(k - f - n)?)?
        };
        let num = an
            .checked_mul(bd)?
            .checked_mul(d12)?
            .checked_mul(k)?
            .checked_add((bn * ad - an * bd).checked_mul(t)?)?;
        let den = ad.checked_mul(bd)?.checked_mul(d12)?;
        let g = gcd_i128(num, den);
        Some((num / g, den / g))
    }

    /// The whole source frames a part of `k` output frames reaches: `⌈advance(k)⌉`.
    pub fn source_frames(&self, k: u64) -> Option<u64> {
        let (n, d) = self.advance(k)?;
        u64::try_from(n.max(0).div_euclid(d) + i128::from(n.rem_euclid(d) != 0)).ok()
    }

    /// [`PartSpeed::advance`] at a real output frame `x` (the sound's clock), in f64.
    pub fn advance_f64(&self, x: f64) -> f64 {
        let a = self.num as f64 / self.den.max(1) as f64;
        let Some(r) = self.ramp else {
            return x * a;
        };
        let b = r.to.num as f64 / r.to.den.max(1) as f64;
        let (f, n) = (r.from as f64, r.frames.max(1) as f64);
        let t = if x <= f {
            0.0
        } else if x < f + n {
            ramp_integral_f64(r.curve, x - f, n)
        } else {
            ramp_integral_f64(r.curve, n, n) + 12.0 * n * n * (x - f - n)
        };
        a * x + (b - a) * t / (12.0 * n * n)
    }

    /// The source samples a part of `n` output samples reads, `s_per_frame` output samples
    /// a frame (`48000 · fps.den / fps.num`): [`PartSpeed::source_samples`] at a constant
    /// speed, `⌈advance_f64(n / s_per_frame) · s_per_frame⌉` with a ramp.
    pub fn source_samples_at(&self, n: u64, s_per_frame: f64) -> u64 {
        if self.ramp.is_none() {
            return self.source_samples(n);
        }
        (self.advance_f64(n as f64 / s_per_frame) * s_per_frame).ceil() as u64
    }

    /// The slowest and fastest speed the part plays at, as `(num, den)` pairs.
    pub fn extremes(&self) -> ((u64, u64), (u64, u64)) {
        let a = (self.num, self.den);
        match self.ramp {
            Some(r) => {
                let b = (r.to.num, r.to.den);
                if u128::from(a.0) * u128::from(b.1) <= u128::from(b.0) * u128::from(a.1) {
                    (a, b)
                } else {
                    (b, a)
                }
            }
            None => (a, a),
        }
    }
}

/// What a sped-up or slowed part's sound does (decision 0030).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpeedAudio {
    /// Time-stretched to the part's length, pitch kept (WSOLA).
    Stretch,
    /// Silent.
    Mute,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Picture {
    pub source: Id,
    /// Presentation index shown on the part's first output frame (rule 3). With
    /// [`Picture::fps`] it counts `output.fps` frames on the source's own clock instead.
    pub first_frame: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_pts: Option<ExpectPts>,
    /// The source's nominal picture rate when it differs from `output.fps` (`/1`,
    /// decision 0017, picture rate conversion): output frame `f` shows the source frame
    /// presented at or before source time `(first_frame + f − out[0]) / output.fps`
    /// ([`crate::retime`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectPts {
    pub ticks: i64,
    pub timescale: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartAudio {
    pub source: Id,
    /// Sample index at 48 kHz after edit-list and priming removal (rule 4).
    pub first_sample: u64,
    pub rate: ConstU64<48000>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    /// Absent and `null` both mean "no fade"; the distinction round-trips.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::scalars::nullable"
    )]
    #[schemars(with = "Option<Fade>")]
    pub fade_in: Option<Option<Fade>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::scalars::nullable"
    )]
    #[schemars(with = "Option<Fade>")]
    pub fade_out: Option<Option<Fade>>,
    /// `/1` (decision 0051): other sources heard under the part's own audio (a
    /// recording's system audio under the mic), each from its `first_sample` for the
    /// part's length at its own gain; the part's fades shape the sum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<Vec<UnderAudio>>,
}

/// A source heard under a part's audio (`/1`, decision 0051).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnderAudio {
    pub source: Id,
    /// Sample index at 48 kHz after edit-list and priming removal (rule 4).
    pub first_sample: u64,
    pub gain_db: f64,
}

impl PartAudio {
    /// The fade-in, if any (absent and `null` are the same).
    pub fn fade_in(&self) -> Option<&Fade> {
        self.fade_in.as_ref().and_then(Option::as_ref)
    }

    /// The fade-out, if any (absent and `null` are the same).
    pub fn fade_out(&self) -> Option<&Fade> {
        self.fade_out.as_ref().and_then(Option::as_ref)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fade {
    pub samples: u64,
    pub shape: FadeShape,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FadeShape {
    Linear,
    RaisedCosine,
}

/// Informational: the stream-clock frame range a part came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Native {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
}

/// One drawn crop of a picture source (rule 5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub id: Id,
    /// A SPEC `RegionRole`, `detail` or `other` (rule 5; see [`crate::validate::LAYER_ROLES`]).
    pub role: RoleName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_source: Option<Id>,
    pub crop: Rect,
    pub place: Rect,
    pub z: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resample: Option<Lanczos3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_map: Option<TimeMap>,
    /// A person cut-out over the crop (decision 0013, SPEC §5.8 roto): the layer's alpha
    /// is multiplied by the mask track of its part span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<LayerMatte>,
    /// An eased visual split (`/1`, decision 0016): over the part's first `frames`
    /// output frames the crop, place and opacity move from `from_*` to the layer's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease: Option<LayerEase>,
    /// A cover layer (`/1`, decision 0021, SPEC §5.7): the crop is drawn into `place`
    /// obscured — blurred, pixelated, or as a solid fill — instead of as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<LayerCover>,
    /// A cover pinned to a moving point (`/1`, decision 0033): it follows the point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<LayerTrack>,
    /// The recorded cursor drawn over this layer (`/1`, decision 0045): a `cursors[].id`
    /// of the layer's picture source, mapped through its crop and place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Id>,
    /// The layer's frame (`/1`, decision 0046): rounded corners, a shadow beneath, a
    /// border on top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<LayerFrame>,
    /// A live picture of another source (`/1`, decision 0046: the camera file of a
    /// recording beside its screen): with `picture_source`, output frame `f` of the part
    /// shows that source's frame `first_frame + (f − out[0])` (`freeze_first`: its
    /// `first_frame`). Only on parts that neither change speed nor convert their rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_frame: Option<u64>,
    /// The nominal rate of the layer's `picture_source` when it differs from `output.fps`
    /// (`/1`, decision 0047: a 25 fps camera beside a 30 fps screen). `first_frame` then
    /// counts `output.fps` frames on that source's presentation clock, and output frame
    /// `f` shows the source frame presented at or before `(first_frame + f − out[0]) /
    /// output.fps`, as `parts[].picture.fps` does (decision 0017). `hold` is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
}

/// A layer's frame (`/1`, decision 0046). With the placed rectangle `P` (canvas px),
/// `S = min(P.w, P.h)` and the canvas's short side `C`, the shape is `P` with corners of
/// radius `r = radius·S`; `d(x, y)` is the signed distance from a pixel centre to its edge
/// (negative inside). The picture's alpha is `clamp(½ − d, 0, 1)` (in place of the
/// rectangle's coverage) × opacity. Under it, the shadow: the shape moved down by
/// `offset_y·C`, alpha `opacity × smoothstep(clamp(½ − d′/(blur·C), 0, 1))` (`d′` its
/// distance; a hard edge when `blur·C < 1`) × the layer's opacity. Over it, the border:
/// alpha `(clamp(½ − d) − clamp(½ − d − width·C)) ×` the layer's opacity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerFrame {
    pub radius: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<FrameShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<FrameBorder>,
}

/// A frame's shadow (`/1`, decision 0046): fractions of the canvas's short side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameShadow {
    pub blur: f64,
    pub offset_y: f64,
    pub opacity: f64,
    pub rgb: Rgb,
}

/// A frame's border (`/1`, decision 0046): `width` a fraction of the canvas's short side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameBorder {
    pub width: f64,
    pub rgb: Rgb,
}

/// A cover layer pinned to a moving point (`/1`, decision 0033): the point's canvas
/// position at some output frames of the part, and `(x, y)`, where it is when the layer
/// sits at its own crop and place. On output frame `f` the place moves by
/// `d = p(f) − (x, y)` rounded to whole pixels (`p` the keys' straight-line interpolation,
/// held past the ends, as [`EventTrack::at`]) and the crop by `d` scaled by
/// `crop / place`, so the cover stays over what it covers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerTrack {
    pub x: f64,
    pub y: f64,
    pub keys: Vec<TrackKey>,
}

impl LayerTrack {
    /// The move on output frame `f`: whole canvas pixels, rounded half away from zero.
    pub fn offset(&self, f: u64) -> (i32, i32) {
        let (px, py) = track_at(&self.keys, f);
        ((px - self.x).round() as i32, (py - self.y).round() as i32)
    }

    /// `crop` and `place` moved on output frame `f`.
    pub fn moved(&self, f: u64, crop: Rect, place: Rect) -> (Rect, Rect) {
        let (dx, dy) = self.offset(f);
        let (dx, dy) = (f64::from(dx), f64::from(dy));
        let sx = if place.w > 0.0 { crop.w / place.w } else { 0.0 };
        let sy = if place.h > 0.0 { crop.h / place.h } else { 0.0 };
        (
            Rect {
                x: crop.x + dx * sx,
                y: crop.y + dy * sy,
                ..crop
            },
            Rect {
                x: place.x + dx,
                y: place.y + dy,
                ..place
            },
        )
    }
}

/// How a cover layer obscures its crop (`/1`, decision 0021). Sizes are canvas px.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerCover {
    /// The crop resampled down to `place / radius` (Lanczos-3, the layer path) and back
    /// up with a cubic B-spline: about a Gaussian of σ ≈ 0.6·radius.
    Blur { radius: f64 },
    /// The crop resampled down to `place / block` and drawn back as blocks (nearest).
    Pixelate { block: f64 },
    /// A solid fill: nothing of the picture shows.
    Solid { rgb: Rgb },
}

impl LayerCover {
    /// The small picture a blurred or pixelated cover is resampled through, for a place
    /// `w × h` canvas px: `round(w / k) × round(h / k)` (each ≥ 1, ≤ 4096). A solid
    /// cover reads no picture (1 × 1).
    pub fn grid(&self, w: f64, h: f64) -> (u32, u32) {
        let k = match self {
            LayerCover::Blur { radius } => *radius,
            LayerCover::Pixelate { block } => *block,
            LayerCover::Solid { .. } => return (1, 1),
        };
        let n = |len: f64| {
            let v = (len / k.max(1.0)).round();
            if v.is_finite() {
                v.clamp(1.0, 4096.0) as u32
            } else {
                1
            }
        };
        (n(w), n(h))
    }
}

/// A layer's ease in (`/1`, decision 0016). At output frame `k` of the part (0-based),
/// `t = curve(k / frames)` (1 from `frames` on) and each value is `from + (own − from)·t`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerEase {
    pub from_crop: Rect,
    pub from_place: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_opacity: Option<f64>,
    pub frames: u64,
    pub curve: EaseCurve,
}

/// The progress curve of a [`LayerEase`] (the model's `Curve`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EaseCurve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl EaseCurve {
    /// Maps progress `p ∈ [0, 1]` through the curve (the same arithmetic as the model's
    /// `Curve::apply`, so a preview, an export and `effective()` agree).
    pub fn apply(self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self {
            EaseCurve::Linear => p,
            EaseCurve::EaseIn => p * p,
            EaseCurve::EaseOut => 1.0 - (1.0 - p) * (1.0 - p),
            EaseCurve::EaseInOut => {
                if p < 0.5 {
                    2.0 * p * p
                } else {
                    1.0 - 2.0 * (1.0 - p) * (1.0 - p)
                }
            }
        }
    }
}

impl LayerEase {
    /// Crop, place and opacity at output frame `k` of the part, given the layer's own.
    pub fn at(&self, k: u64, crop: Rect, place: Rect, opacity: f64) -> (Rect, Rect, f64) {
        if self.frames == 0 || k >= self.frames {
            return (crop, place, opacity);
        }
        #[allow(clippy::cast_precision_loss)]
        let t = self.curve.apply(k as f64 / self.frames as f64);
        let l = |a: f64, b: f64| a + (b - a) * t;
        let r = |a: Rect, b: Rect| Rect {
            x: l(a.x, b.x),
            y: l(a.y, b.y),
            w: l(a.w, b.w),
            h: l(a.h, b.h),
        };
        (
            r(self.from_crop, crop),
            r(self.from_place, place),
            l(self.from_opacity.unwrap_or(opacity), opacity),
        )
    }
}

/// A roto matte on a layer (decision 0013): the mask track of the layer's crop over its
/// part, softened by `soften` source pixels, with brush corrections painted on top.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerMatte {
    pub kind: Roto,
    /// The segmentation model family (`apple_vision`); draft or accurate is the
    /// renderer's choice, recorded on the export (`draft_mattes`).
    pub model: String,
    /// Edge softening radius in source pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soften: Option<f64>,
    /// Brush corrections, in order, applied to every frame of the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strokes: Option<Vec<MatteStroke>>,
}

/// One brush stroke on a matte: a polyline in source pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatteStroke {
    pub mode: StrokeMode,
    /// Brush radius in source pixels.
    pub radius: f64,
    /// Soft edge as a fraction of the radius (0 hard, 1 soft to the centre).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather: Option<f64>,
    pub points: Vec<Point>,
}

/// Whether a stroke adds to the person or takes away from it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokeMode {
    Add,
    Subtract,
}

impl Layer {
    /// `visible`, default true.
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    /// `opacity`, default 1.
    pub fn opacity(&self) -> f64 {
        self.opacity.unwrap_or(1.0)
    }
}

/// Which source frame a layer shows (rule 5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimeMap {
    Live,
    FreezeFirst,
    Hold {
        /// Absolute presentation index; may lie outside the part.
        frame: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Provenance>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Where a value came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub kind: ProvenanceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ProvenanceDetail>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceKind {
    Import,
    Measurement,
    Transcript,
    Ocr,
    Human,
    /// An agent-typed value with no other provenance.
    Agent,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One text block drawn above every layer on `[out[0], out[1])` (rule 6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextEvent {
    pub id: Id,
    pub out: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<i64>,
    pub role: TextRole,
    /// A `text_styles` key (rule 10).
    pub style: Id,
    pub pos: Point,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<u8>,
    /// Final size in ASS units; renderers never refit (rule 6).
    pub size: f64,
    pub runs: Vec<Run>,
    /// `/1`: how the event enters and leaves (SPEC §5.10 caption animation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim: Option<TextAnim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// A text event's animation (`/1`; curves in decision 0014 and [`crate::anim`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextAnim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AnimKind>,
    /// Runs (by index) that pop: the event pulses and these runs' fill comes in from the
    /// style's fill (keyword animation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pop_runs: Option<Vec<u64>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnimKind {
    Bounce,
    Pop,
    Fade,
}

/// An image file (`/1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub id: Id,
    pub path: String,
    pub sha256: Sha256,
    /// Natural size in pixels (the decoder checks it).
    pub w: u64,
    pub h: u64,
}

/// A recorded cursor (`/1`, decision 0045): the input track of one picture source (a
/// `gmclip.input/1` JSON file, cursor positions in the source's pixels on its clock) and
/// its cursor images, by the track's shape index. Drawn over each layer that names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorTrack {
    pub id: Id,
    /// The `sources[].id` whose picture the track describes.
    pub source: Id,
    pub path: String,
    pub sha256: Sha256,
    /// `images[].id` by the track's shape index.
    pub shapes: Vec<Id>,
    /// Size × the recorded cursor's.
    pub scale: f64,
    /// Smoothing window, ms each side (absent: the raw path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smooth_ms: Option<u64>,
    /// Fades out after this long without moving (absent: never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_idle_ms: Option<u64>,
    /// A ring where each click lands (read when `click` is absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicks: Option<bool>,
    /// How a click shows (decision 0046; absent: a ring when `clicks`, else none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<CursorClick>,
    /// 0–1: a critically damped spring on the position, time constant 250 ms × this
    /// (absent or 0: the raw path, or `smooth_ms`'s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothness: Option<f64>,
    /// The cursor dips to 80 % for 120 ms on each click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_shrink: Option<bool>,
    /// Slow moves smaller than this (track px, 0–20) are held still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shake_px: Option<f64>,
    /// Over the last this-many ms (0–10 000) of each part, glide back to the position at
    /// its first frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_ms: Option<u64>,
    /// 5 sub-samples along each move (absent: off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<bool>,
}

/// How a recorded cursor shows a click (decision 0046).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CursorClick {
    None,
    Ring,
    Ripple,
}

impl CursorTrack {
    /// The click style in force: `click`, else a ring when `clicks`.
    pub fn click_style(&self) -> CursorClick {
        self.click.unwrap_or(if self.clicks == Some(true) {
            CursorClick::Ring
        } else {
            CursorClick::None
        })
    }
}

/// An image drawn into `place` (canvas px) on `[out[0], out[1])` (`/1`); with `z`, among
/// the part's layers like a clip event's (decision 0042).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageEvent {
    pub id: Id,
    pub out: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<i64>,
    /// An `images[].id`.
    pub image: Id,
    pub place: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Drawn among the part's layers, in their `z` space (decision 0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<i64>,
}

/// A callout drawn on `[out[0], out[1])` (`/1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeEvent {
    pub id: Id,
    pub out: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<i64>,
    pub shape: Shape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// A box or an arrow, in canvas px.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    Box {
        rect: Rect,
        /// Stroke width (0 = no stroke).
        width: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Rgba>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Rgba>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f64>,
    },
    Arrow {
        from: Point,
        /// The head's tip.
        to: Point,
        width: f64,
        stroke: Rgba,
        /// Head length along the shaft (default 3 × width).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<f64>,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextRole {
    Caption,
    TextHook,
    SectionPill,
    RunClock,
    Note,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Rgba>,
}

/// Audio post-processing (rules 7, 8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelopes: Option<Vec<Envelope>>,
    /// `/1` (decision 0051): music and sound effects summed with the parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlays: Option<Vec<AudioOverlay>>,
    /// `/1` (decision 0051): the output samples where the cut's voice speaks, in order and
    /// apart; what a ducked overlay ducks under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<Vec<Range>>,
    pub loudness: Loudness,
}

/// A sound laid over the cut (`/1`, decision 0051): output sample `n` in `out_samples`
/// plays `source`'s sample `first_sample + n − out_samples[0]` (silence past its end) ×
/// `gain_db`, faded in and out (linear) and ducked under `audio.speech`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioOverlay {
    pub id: Id,
    pub source: Id,
    pub first_sample: u64,
    pub out_samples: Range,
    pub gain_db: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_samples: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_samples: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duck: Option<Duck>,
}

/// Ducking under speech (`/1`, decision 0051): `depth_db` (≤ 0) at full weight inside a
/// speech range, eased in linearly over `attack_samples` before it and out over
/// `release_samples` after it.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Duck {
    pub depth_db: f64,
    pub attack_samples: u64,
    pub release_samples: u64,
}

/// A linear-dB gain ramp on the output sample clock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub id: Id,
    pub out_samples: Range,
    pub gain_db: f64,
    pub ramp_in_samples: u64,
    pub ramp_out_samples: u64,
    pub curve: LinearDb,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Loudness {
    AsRecorded,
    Static {
        integrated_lufs: f64,
        true_peak_dbtp: f64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Reference>,
    /// TikTok/Shorts/YouTube zones join in `/1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_zones: Option<SafeZones>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafeZones {
    Instagram,
}

/// A zoom section (`/1`, decision 0046): on `[out[0], out[1])` each named layer is drawn
/// through a crop `depth`× tighter than its own, centred on `target` and kept inside its
/// own crop, eased in over the first `ease_in` frames and out over the last `ease_out`
/// (ease-in-out). See `gmclip_render::compositor::zoom` for the exact arithmetic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Zoom {
    pub id: Id,
    pub out: Range,
    /// The layers drawn tighter, by part (each a part the zoom overlaps).
    pub layers: Vec<LayerRef>,
    /// 1–6.
    pub depth: f64,
    pub target: ZoomTarget,
    /// Frames.
    pub ease_in: u64,
    pub ease_out: u64,
    /// Camera layers shrink toward their nearest canvas corner to this share (0.3–1),
    /// by the same progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_scale: Option<f64>,
    /// The camera layers `camera_scale` shrinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_layers: Option<Vec<LayerRef>>,
    /// 5 sub-samples across half a frame on frames where the crop moves (absent: off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<bool>,
}

/// A layer of one part: `parts[].id` and that part's `layers[].id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerRef {
    pub part: Id,
    pub layer: Id,
}

/// What a zoom is centred on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ZoomTarget {
    /// The recorded cursor of `cursors[].id`, through a critically damped spring.
    Cursor(Id),
    /// The caret, else the focused field, of `cursors[].id`'s input track (else its
    /// cursor), through the same spring.
    Focus(Id),
    /// A fixed point, normalised in the layer's own crop.
    Point([f64; 2]),
}
