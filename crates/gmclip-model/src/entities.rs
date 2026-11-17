//! Every entity kind of SPEC §4.3, as plain data with serde + schemars.
//!
//! Rules (SPEC §3.2): every element of an id-bearing collection is its own kind with a
//! parent id (`#[entity(parent)]`) and an `OrderKey` (`#[entity(order)]`) where ordered;
//! no entity embeds another entity's list. Sparse per-framing or per-word maps are
//! `#[entity(map)]` fields, so an op addresses one key. Every stored time point is an
//! [`Anchor`], an `at_src`/`at_local` value ([`Flicks`]) or a [`FrameRef`]; nothing
//! stores free seconds. Floats appear only for gains, opacities, styles, scores and
//! normalised geometry.

use std::collections::BTreeMap;

use gmclip_macros::Entity;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anchor::{Anchor, FrameRef, WordId};
use crate::entity::AsParent;
use crate::geometry::{CanvasBox, NormRect, Place};
use crate::ids::{EntityId, EventId, Id, RunId};
use crate::order::OrderKey;
use crate::time::{Flicks, Rational};

// ---------------------------------------------------------------------------------------
// Shared value types
// ---------------------------------------------------------------------------------------

/// A source or a sync group: where a moment or part takes its media from.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaRef {
    Source(Id<Source>),
    Group(Id<SyncGroup>),
}

/// A span of a source or asset used by a clip or an audio overlay.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetOrSpan {
    Asset(Id<Asset>),
    Span {
        media: MediaRef,
        src_in: Flicks,
        src_out: Flicks,
    },
}

/// Region roles (SPEC §4.3). `private`, `chat` and `alerts` form the private class.
#[derive(
    Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RegionRole {
    Cam,
    Screen,
    Chat,
    Alerts,
    Banner,
    Private,
    CaptionsBurned,
}

impl RegionRole {
    pub fn is_private_class(self) -> bool {
        matches!(
            self,
            RegionRole::Private | RegionRole::Chat | RegionRole::Alerts
        )
    }
}

/// A layer's role: a region role, `detail` (a second crop of `screen`) or `other`.
#[derive(
    Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LayerRole {
    Region(RegionRole),
    Detail,
    Other,
}

impl LayerRole {
    /// Does a hold or hide naming `sel` apply to this layer? Rules naming `screen` also
    /// apply to `detail` (SPEC §4.3); naming `private` applies to the private class.
    pub fn selected_by(self, sel: LayerRole) -> bool {
        match (self, sel) {
            (a, b) if a == b => true,
            (LayerRole::Detail, LayerRole::Region(RegionRole::Screen)) => true,
            (LayerRole::Region(r), LayerRole::Region(RegionRole::Private)) => r.is_private_class(),
            _ => false,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Canvas {
    pub w: u32,
    pub h: u32,
}

/// Per-framing opt-out of a shared content decision (holds, hides).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct FramingOptOut {
    #[serde(default)]
    pub ignore: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Curve {
    /// Maps progress `p ∈ [0, 1]` through the curve.
    pub fn apply(self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self {
            Curve::Linear => p,
            Curve::EaseIn => p * p,
            Curve::EaseOut => 1.0 - (1.0 - p) * (1.0 - p),
            Curve::EaseInOut => {
                if p < 0.5 {
                    2.0 * p * p
                } else {
                    1.0 - 2.0 * (1.0 - p) * (1.0 - p)
                }
            }
        }
    }

    /// `∫₀ᵖ apply` (decision 0033: how far a speed ramp has gone): ½ at 1 for linear and
    /// ease-in-out, ⅓ for ease-in, ⅔ for ease-out.
    pub fn integral(self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        let q = 1.0 - p;
        match self {
            Curve::Linear => p * p / 2.0,
            Curve::EaseIn => p * p * p / 3.0,
            Curve::EaseOut => p - (1.0 - q * q * q) / 3.0,
            Curve::EaseInOut => {
                if p < 0.5 {
                    2.0 * p * p * p / 3.0
                } else {
                    p - 0.5 + 2.0 * q * q * q / 3.0
                }
            }
        }
    }
}

/// A run of styled text (captions, overlays, imported cues).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TextRun {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<bool>,
}

// ---------------------------------------------------------------------------------------
// Project and project-level maps
// ---------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProjectMeta {
    pub name: String,
    pub created_ms: i64,
    pub app_version: String,
}

/// The project: one recording session (SPEC §4.3). One per document.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "project")]
pub struct Project {
    #[entity(id)]
    pub id: Id<Project>,
    pub meta: ProjectMeta,
    /// `active_asr` per source or group id → ASR revision (R-MODEL-4).
    #[entity(map)]
    pub active_asr: BTreeMap<String, String>,
    pub profile_ref: Option<String>,
}

/// Project-level caption spelling corrections (heard → written). One entity, one rev;
/// its entries are map keys (SPEC §3.2).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "caption_corrections")]
pub struct CaptionCorrections {
    #[entity(id)]
    pub id: Id<CaptionCorrections>,
    #[entity(map)]
    pub entries: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Term {
    pub written: String,
    #[serde(default)]
    pub heard_as: Vec<String>,
}

/// Project terms (product names the transcriber gets wrong).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "terms")]
pub struct Terms {
    #[entity(id)]
    pub id: Id<Terms>,
    #[entity(map)]
    pub entries: BTreeMap<String, Term>,
}

/// Caption keep pairs ("two minutes" never split): key = the pair, value = its words.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "keep_pairs")]
pub struct KeepPairs {
    #[entity(id)]
    pub id: Id<KeepPairs>,
    #[entity(map)]
    pub entries: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TextStyle {
    pub font: String,
    /// em size in short-side units (SPEC Conventions, Units).
    pub size: f64,
    pub fill: String,
    pub outline: Option<Outline>,
    pub shadow: Option<Shadow>,
    pub align: TextAlign,
    pub max_width: Option<f64>,
    pub fit: TextFit,
    pub line_height: f64,
    pub letter_spacing: f64,
    pub case: TextCase,
    pub pill: Option<Pill>,
}

/// Named styles (caption and text presets).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "styles")]
pub struct Styles {
    #[entity(id)]
    pub id: Id<Styles>,
    #[entity(map)]
    pub entries: BTreeMap<String, TextStyle>,
}

/// A stored asset (≤ 20 MB inline; never fonts).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "asset")]
pub struct Asset {
    #[entity(id)]
    pub id: Id<Asset>,
    pub name: String,
    pub sha256: String,
    pub size: u64,
    pub media_type: String,
    /// A picture's size, and a video's rate and length (decision 0028; absent on assets
    /// stored before it, and on images, whose size their overlay carries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<AssetProbe>,
}

/// What the host measured of a stored picture or video (decision 0028).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AssetProbe {
    pub w: u32,
    pub h: u32,
    /// A video's nominal frame rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
    /// A video's frame count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "font")]
pub struct Font {
    #[entity(id)]
    pub id: Id<Font>,
    pub family: String,
    pub style: String,
    pub sha256: Option<String>,
    /// em per libass size unit for this font (SPEC Conventions, Units).
    pub em_per_size: f64,
}

// ---------------------------------------------------------------------------------------
// Sources, regions, sync
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TrackProbe {
    pub index: u32,
    pub kind: TrackKind,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<Rational>,
    pub timescale: u32,
    pub gop_max_s: Option<f64>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AudioTrackProbe {
    pub index: u32,
    pub codec: String,
    pub rate: u32,
    pub layout: String,
    pub bitrate: Option<u32>,
    pub speech_ratio: Option<f64>,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Probe {
    pub duration: Flicks,
    pub tracks: Vec<TrackProbe>,
    pub audio_tracks: Vec<AudioTrackProbe>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AudioRoles {
    pub voice: Option<u32>,
    #[serde(default)]
    pub program: Vec<u32>,
    #[serde(default)]
    pub exclude: Vec<u32>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "source")]
pub struct Source {
    #[entity(id)]
    pub id: Id<Source>,
    pub path: String,
    pub fingerprint: String,
    pub probe: Probe,
    pub audio_roles: AudioRoles,
    pub stream_layout: Option<Id<StreamLayout>>,
    pub relink_hint: Option<String>,
    /// The cursor gmclip's recorder logged for this screen file (decision 0045): drawn
    /// over its picture from the log, since the recording hides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<SourceCursor>,
    /// Markers dropped while recording (⌃⌥M, decision 0052), on this file's clock, in
    /// order: marker `n` is the `n`-th of the recording.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Flicks>,
}

/// A recorded screen file's cursor (decision 0045, SPEC §5.18): the input track
/// (`rec::InputTrack` JSON, a stored asset), its cursor images, and how it is drawn.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SourceCursor {
    /// The input track asset (`application/vnd.gmclip.input+json`).
    pub track: Id<Asset>,
    /// The cursor images, by the track's shape index (PNG assets).
    pub shapes: Vec<Id<Asset>>,
    /// Drawn at all (false: the picture shows no cursor).
    pub visible: bool,
    /// Size × the recorded cursor's (1 = as on screen).
    pub scale: f64,
    /// Smoothing window, ms each side (0 = the raw path).
    pub smooth_ms: u32,
    /// Fades out after this long without moving (0 = never).
    pub hide_idle_ms: u32,
    /// A ring where each click lands (read when `click` is absent: true = a ring).
    pub clicks: bool,
    /// How a click is shown (decision 0046; absent: from `clicks`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<CursorClick>,
    /// The cursor's feel (decision 0046), 0–1: 0 = the raw path; a critically damped
    /// spring whose time constant grows to 250 ms at 1. Supersedes `smooth_ms` when > 0.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub smoothness: f64,
    /// The cursor dips to 80 % for 120 ms on each click.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub click_shrink: bool,
    /// Slow moves smaller than this many track px are held still, 0–20 (0 = off).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub shake_px: u32,
    /// The last N ms of each part glide back to its first position, 0–10 000 (0 = off).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub loop_ms: u32,
    /// Motion blur on frames where the cursor moves (off by default: it costs sub-frames).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub motion_blur: bool,
}

/// How a recorded cursor shows a click (decision 0046).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CursorClick {
    None,
    /// A ring grows from the click and fades.
    Ring,
    /// A soft filled circle expands under a ring.
    Ripple,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

impl SourceCursor {
    /// How a newly imported recording's cursor is drawn.
    pub fn new(track: Id<Asset>, shapes: Vec<Id<Asset>>) -> Self {
        Self {
            track,
            shapes,
            visible: true,
            scale: 1.0,
            smooth_ms: 0,
            hide_idle_ms: 0,
            clicks: false,
            click: None,
            smoothness: 0.0,
            click_shrink: false,
            shake_px: 0,
            loop_ms: 0,
            motion_blur: false,
        }
    }

    /// The click style in force: `click`, else a ring when `clicks`.
    pub fn click_style(&self) -> CursorClick {
        self.click.unwrap_or(if self.clicks {
            CursorClick::Ring
        } else {
            CursorClick::None
        })
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegionFrom {
    Layout,
    Detected,
    Human,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "region")]
pub struct Region {
    #[entity(id)]
    pub id: Id<Region>,
    #[entity(parent)]
    pub source: Id<Source>,
    pub role: RegionRole,
    pub rect: NormRect,
    pub from: RegionFrom,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "sync_group")]
pub struct SyncGroup {
    #[entity(id)]
    pub id: Id<SyncGroup>,
    pub master_audio: Option<Id<SyncMember>>,
    pub picture: Option<Id<SyncMember>>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Picture,
    Audio,
    Both,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncMethod {
    Manual,
    AudioXcorr,
    FrameMatch,
    Import,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "sync_member")]
pub struct SyncMember {
    #[entity(id)]
    pub id: Id<SyncMember>,
    #[entity(parent)]
    pub group: Id<SyncGroup>,
    #[entity(order)]
    pub order: OrderKey,
    pub source: Id<Source>,
    pub role: MemberRole,
    pub rate_ppm: f64,
    pub method: SyncMethod,
    pub confidence: f64,
    /// This member's sound heard under the group's audio, at this gain (decision 0051: a
    /// recording's system audio under the mic, −12 dB when it opens); `None`, or at most
    /// [`UNDER_OFF_DB`], is not heard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under_db: Option<f64>,
}

/// An under gain at or below this is silence (decision 0051).
pub const UNDER_OFF_DB: f64 = -60.0;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SrcRange {
    pub start: Flicks,
    pub end: Flicks,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "sync_segment")]
pub struct SyncSegment {
    #[entity(id)]
    pub id: Id<SyncSegment>,
    #[entity(parent)]
    pub member: Id<SyncMember>,
    #[entity(order)]
    pub order: OrderKey,
    pub src_range: SrcRange,
    pub offset: Flicks,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StreamRegion {
    pub role: RegionRole,
    pub rect: NormRect,
}

/// A library stream layout (SPEC §5.7.2): regions and audio roles that later imports of
/// the same scene inherit.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "stream_layout")]
pub struct StreamLayout {
    #[entity(id)]
    pub id: Id<StreamLayout>,
    pub name: String,
    pub canvas_size: Canvas,
    pub fingerprint: String,
    pub regions: Vec<StreamRegion>,
    pub audio_roles: Option<AudioRoles>,
    pub default_template: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Moments, cuts, story, review
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Edges {
    Word,
    Line,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub start: Anchor,
    pub end: Anchor,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MomentSpan {
    pub start: Anchor,
    pub end: Anchor,
    pub edges: Edges,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PayoffFrom {
    Agent,
    Human,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Payoff {
    pub start: Anchor,
    pub end: Anchor,
    pub from: PayoffFrom,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Excerpt {
    pub text: String,
    pub exact: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MomentOrigin {
    Local,
    Sweep,
    Scout,
    Variant,
    Manual,
    Agent,
    /// A spike in an imported chat log (L.03): the chat rate or reaction words rose well
    /// above the stream's own baseline there. Added in model epoch 1 (additive).
    Chat,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MomentState {
    New,
    Staged,
    Archived,
    Used,
    Posted,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct VisualRating {
    pub verdict: String,
    pub r#match: Option<String>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "moment")]
pub struct Moment {
    #[entity(id)]
    pub id: Id<Moment>,
    pub media: MediaRef,
    pub span: MomentSpan,
    pub payoff: Option<Payoff>,
    pub quote: String,
    pub excerpt: Excerpt,
    /// 1–5 (AI emits 2–5; local highlights 2–4; 1 is human-only).
    pub score: u8,
    pub r#type: String,
    pub why: String,
    pub payoff_hint: Option<String>,
    pub text_hook: Option<String>,
    pub origin: MomentOrigin,
    pub origin_ref: Option<String>,
    pub entry_angle: Option<String>,
    pub variant_of: Option<Id<Moment>>,
    pub visual: Option<VisualRating>,
    pub state: MomentState,
    pub pass_ref: Option<String>,
    pub harness_label: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunSummary {
    pub run: RunId,
    /// Untrusted (written by an agent).
    pub text: String,
    /// The judgement calls the run recorded, each with the field values that undo it.
    pub reversible_choices: Vec<ReversibleChoice>,
}

/// One judgement call a run made (SPEC §10.2 "AI EDIT PASS"), restorable in one
/// transaction: every field it names goes back to exactly the recorded value.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReversibleChoice {
    /// What the run chose, in its words (untrusted).
    pub label: String,
    pub restore: Vec<FieldRestore>,
}

/// A field (or one map key) and the value a restore puts back.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FieldRestore {
    pub entity: EntityId,
    /// The field's name, as ops name it.
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The value as canonical JSON text, checked against the field's type when it was
    /// recorded.
    pub value: String,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CutLineage {
    pub forked_from: Option<Id<Cut>>,
    #[serde(default)]
    pub covers: Vec<String>,
    /// The stream edit's topics a long-form was made from, in chapter order (decision
    /// 0011). Absent on cuts made before topics existed, so their bytes are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<Id<Topic>>,
    /// What the parent (the stream edit) kept inside those topics when this cut last
    /// took its changes: the base `cut_parent_diff` compares the parent against, so a
    /// long-form's own trims are never mistaken for the parent's edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_base: Vec<BaseRange>,
}

/// One kept stretch of a parent inside one topic (`CutLineage::parent_base`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BaseRange {
    pub topic: Id<Topic>,
    pub media: MediaRef,
    pub start: Flicks,
    pub end: Flicks,
}

/// What a cut is in its project (decision 0011): the one stream edit (the main cut,
/// holding every video: decision 0036), a 16:9 long-form derived from it, or a short.
/// Cuts made before the field existed are shorts, and a short's bytes carry no `kind` at
/// all, so their stored rows and ops are unchanged (R-CMD-5 additive; the model epoch
/// that introduced it is `MODEL_EPOCH` 1).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CutKind {
    Stream,
    Long,
    #[default]
    Short,
}

impl CutKind {
    pub fn is_short(&self) -> bool {
        *self == CutKind::Short
    }
    pub fn name(self) -> &'static str {
        match self {
            CutKind::Stream => "stream",
            CutKind::Long => "long",
            CutKind::Short => "short",
        }
    }
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "cut")]
pub struct Cut {
    #[entity(id)]
    pub id: Id<Cut>,
    pub title: String,
    pub moment: Option<Id<Moment>>,
    pub brief: String,
    pub notes: String,
    pub last_run_summary: Option<RunSummary>,
    pub lineage: CutLineage,
    /// `Cut.story_arc` = ordered suggestion ids (SPEC §4.3 StorySuggestion).
    pub story_arc: Vec<Id<StorySuggestion>>,
    /// Loop the last frame (AJ's style rule; decision 0025): `Some(true)` = this cut loops,
    /// so its last frame should show the opening's layout and crop (lint
    /// `loop_last_frame`, fix `loop_fix`); `Some(false)` = it does not; `None` = not set.
    pub loop_check: Option<bool>,
    /// Stream edit, long-form or short (decision 0011); absent = short.
    #[serde(default, skip_serializing_if = "CutKind::is_short")]
    pub kind: CutKind,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartRole {
    Hook,
    Setup,
    Evidence,
    Payoff,
    Counterpoint,
    Reaction,
    Punchline,
    Context,
    Other,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionState {
    Suggested,
    Rejected,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "story_suggestion")]
pub struct StorySuggestion {
    #[entity(id)]
    pub id: Id<StorySuggestion>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    #[entity(order)]
    pub order: OrderKey,
    pub span: Span,
    pub role: PartRole,
    pub relevance: f64,
    pub quote: String,
    pub state: SuggestionState,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Keep,
    Change,
    Drop,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "review")]
pub struct Review {
    #[entity(id)]
    pub id: Id<Review>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub at_ms: i64,
    pub author: String,
    /// Untrusted when the author is not the local human.
    pub text: String,
    pub verdict: Verdict,
    pub export_ref: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Topics and fluff proposals on the stream edit (decision 0011)
// ---------------------------------------------------------------------------------------

/// Who made a topic or a fluff proposal.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditOrigin {
    Human,
    Agent,
}

/// A proposal's state. A human's topic is `accepted` from the start (a decision); an
/// agent's arrives `proposed` and only a human moves it on (the R-STORY-1 rule).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Proposed,
    Accepted,
    Rejected,
}

/// A labelled, word-edged range of the stream edit (decision 0011). Accepted topics
/// never overlap one another (one level); gaps between them are untopiced talk. A
/// proposal may overlap human work only when it arrives flagged `conflict`, and a
/// proposed split or merge names the accepted topics it would replace in `replaces`.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "topic")]
pub struct Topic {
    #[entity(id)]
    pub id: Id<Topic>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    #[entity(order)]
    pub order: OrderKey,
    /// The source or sync group the span's times are on.
    pub media: MediaRef,
    /// `word_start` → `word_end` anchors.
    pub span: Span,
    /// Untrusted when `origin` is agent.
    pub title: String,
    /// `#rrggbb`.
    pub colour: String,
    pub origin: EditOrigin,
    pub state: ProposalState,
    /// Untrusted when `origin` is agent.
    pub notes: String,
    /// The proposal overlaps a human's topic or edit ("conflicts with your edit"):
    /// shown, never applied silently.
    pub conflict: bool,
    /// Accepted topics this proposal would replace when applied (a proposed split or
    /// merge); empty for a plain proposal.
    pub replaces: Vec<Id<Topic>>,
    /// The pass and prompt version that proposed it (R-AI-5), if an agent did.
    pub pass_ref: Option<String>,
}

/// A sweep-style proposal to strike a stretch of the stream edit as fluff (decision
/// 0011 §4): previewed, then accepted (struck, one transaction) or rejected.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "fluff_proposal")]
pub struct FluffProposal {
    #[entity(id)]
    pub id: Id<FluffProposal>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    #[entity(order)]
    pub order: OrderKey,
    pub media: MediaRef,
    pub span: Span,
    /// Why it is fluff (untrusted: agent prose).
    pub reason: String,
    pub origin: EditOrigin,
    pub state: ProposalState,
    /// The stretch overlaps an edit the human made after the pass began.
    pub conflict: bool,
    pub pass_ref: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Comments, suggested edits and thumbnails (decision 0015)
// ---------------------------------------------------------------------------------------

/// A point on the source clock: the media a part plays and a time on it. Comments and
/// cover frames keep this instead of a cut time, so they stay on the same picture when
/// parts are trimmed, moved or reordered (and read "not in the cut" when it is cut out).
/// `part` is the part it was set on, a hint only.
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SrcAt {
    pub media: MediaRef,
    pub at_src: Flicks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<Id<Part>>,
}

/// A comment on a cut: a thread's first comment, or a reply to one (`reply_to`). Pinned
/// to a source time and optionally a box drawn on the frame (normalised to the output
/// frame). Authors: `human`, `review-link:<id>[/<login>]` or `launched:<run>`; text from
/// anyone but `human` is untrusted (R-INJ-1).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "comment")]
pub struct Comment {
    #[entity(id)]
    pub id: Id<Comment>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    /// The thread's first comment, when this one is a reply.
    pub reply_to: Option<Id<Comment>>,
    pub at: Option<SrcAt>,
    /// A box on the frame, as fractions of the output canvas.
    pub frame_box: Option<NormRect>,
    pub author: String,
    pub text: String,
    pub at_ms: i64,
    /// Threads only (a reply never carries it).
    pub resolved: bool,
    /// The export it was written on (its export id), if any.
    pub export_ref: Option<String>,
}

/// An edit a review-link editor suggests (decision 0015): recorded, never applied until
/// the owner accepts it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SuggestedEdit {
    /// New source edges for a part.
    Trim {
        part: Id<Part>,
        src_in: Flicks,
        src_out: Flicks,
    },
    /// Strike a stretch of the media (words) from every part that plays it; `words` is
    /// what the suggester saw there (untrusted, display only).
    Strike {
        media: MediaRef,
        start: Flicks,
        end: Flicks,
        words: String,
    },
    /// Move the captions: one part's (with `part`) or the whole framing's.
    CaptionMove {
        framing: Id<Framing>,
        part: Option<Id<Part>>,
        place: Place,
    },
}

/// A suggested edit and the owner's decision on it (the R-STORY-1 rule: proposed until a
/// human accepts or rejects it).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "edit_proposal")]
pub struct EditProposal {
    #[entity(id)]
    pub id: Id<EditProposal>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub edit: SuggestedEdit,
    pub author: String,
    /// Why (untrusted).
    pub note: String,
    pub at_ms: i64,
    pub state: ProposalState,
    /// The comment thread it belongs to, if any.
    pub comment: Option<Id<Comment>>,
}

/// How a thumbnail's title is drawn (brand-kit values are copied in, never linked).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ThumbStyle {
    /// A font family the project has (`font_add`), or empty for the app's default face.
    pub font: String,
    /// Cap height as a fraction of the frame height.
    pub size: f64,
    /// `#RRGGBB`.
    pub fill: String,
    pub outline: String,
    /// Words wrapped in `*…*` take this colour.
    pub accent: String,
    pub uppercase: bool,
}

/// Where the title sits (a layout an agent can return instead of an image).
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ThumbLayout {
    /// The title's box, as fractions of the frame.
    pub title_box: NormRect,
    pub align: TextAlign,
}

/// One thumbnail version of a cut: a cover frame from the footage plus a title. Every
/// version is kept; one is `picked`.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "thumbnail")]
pub struct Thumbnail {
    #[entity(id)]
    pub id: Id<Thumbnail>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub cover: SrcAt,
    /// The title drawn on the frame (untrusted when an agent wrote it).
    pub title: String,
    /// Text-hook options an agent pass offered (≤ 3); `title` is the one in use.
    pub hook_options: Vec<String>,
    pub style: ThumbStyle,
    pub layout: ThumbLayout,
    pub origin: EditOrigin,
    pub author: String,
    pub at_ms: i64,
    pub picked: bool,
    /// What the agent said about it (untrusted).
    pub notes: String,
}

// ---------------------------------------------------------------------------------------
// Alternate openings and title/description drafts (decision 0024)
// ---------------------------------------------------------------------------------------

/// One stretch of an opening: a source range on a media, like a part's.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OpeningSpan {
    pub media: MediaRef,
    pub src_in: Flicks,
    pub src_out: Flicks,
}

/// Where an alternate opening came from.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpeningFrom {
    /// A moment's span (a different hook take).
    Moment,
    /// A story suggestion's span.
    Suggestion,
    /// Picked by hand (words in the transcript, or typed times).
    Manual,
    /// The cut's own opening before another one was made the main version.
    WasMain,
}

/// An alternate opening of a short (decision 0024): the cut is the main version, and
/// each `Opening` is a sibling version that swaps the cut's opening parts (its leading
/// hook parts) for these spans and shares everything after them — one cut, never copies
/// drifting apart. Making one the main version swaps it with the cut's own opening in one
/// transaction (the old opening is kept as an `Opening`, `was_main`).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "opening")]
pub struct Opening {
    #[entity(id)]
    pub id: Id<Opening>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    /// A short name ("the reaction take"); untrusted when an agent wrote it.
    pub label: String,
    /// The stretches that replace the cut's opening, in order (≥ 1).
    pub spans: Vec<OpeningSpan>,
    /// The text hook this version shows instead of the cut's, if any (untrusted when an
    /// agent wrote it).
    pub hook_text: Option<String>,
    pub from: OpeningFrom,
    /// The moment or story suggestion id it was taken from.
    pub from_ref: Option<String>,
    pub origin: EditOrigin,
    pub at_ms: i64,
}

/// The platforms a title/description draft is written for.
#[derive(
    Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CopyPlatform {
    Youtube,
    Shorts,
    Tiktok,
    Instagram,
}

impl CopyPlatform {
    pub const ALL: [CopyPlatform; 4] = [
        CopyPlatform::Youtube,
        CopyPlatform::Shorts,
        CopyPlatform::Tiktok,
        CopyPlatform::Instagram,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CopyPlatform::Youtube => "youtube",
            CopyPlatform::Shorts => "shorts",
            CopyPlatform::Tiktok => "tiktok",
            CopyPlatform::Instagram => "instagram",
        }
    }

    pub fn parse(s: &str) -> Option<CopyPlatform> {
        CopyPlatform::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// A title and description drafted for one platform (decision 0024): a proposal the
/// human accepts, edits or rejects. Nothing here publishes or approves anything; an
/// accepted draft is text the human copies.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "copy_draft")]
pub struct CopyDraft {
    #[entity(id)]
    pub id: Id<CopyDraft>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub platform: CopyPlatform,
    /// Untrusted when `origin` is agent and the human has not edited it.
    pub title: String,
    pub description: String,
    pub state: ProposalState,
    pub origin: EditOrigin,
    /// The human changed the text.
    pub edited: bool,
    /// The pass and prompt version that drafted it (R-AI-5).
    pub pass_ref: Option<String>,
    pub author: String,
    pub at_ms: i64,
}

// ---------------------------------------------------------------------------------------
// Parts and per-part content decisions
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartFrom {
    Hook,
    Suggested,
    Manual,
    Agent,
    Import,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct PartLineage {
    pub split_of: Option<Id<Part>>,
    #[serde(default)]
    pub merged: Vec<Id<Part>>,
    pub suggestion: Option<Id<StorySuggestion>>,
    pub import_role: Option<String>,
}

/// A patch value for an optional field: set it, or clear it. (`Option<Option<T>>` would
/// not survive serde: `Some(None)` and `None` both encode as null.)
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptSet<T> {
    Clear,
    Set(T),
}

impl<T: Clone> OptSet<T> {
    pub fn get(&self) -> Option<T> {
        match self {
            OptSet::Clear => None,
            OptSet::Set(v) => Some(v.clone()),
        }
    }
}

/// A sparse patch over one layer (SPEC §4.3.1): each `Some` field wins over what the
/// stack below says; `removed: Some(true)` is a tombstone, `Some(false)` revives.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct LayerPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<NormRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<Place>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather: Option<Feather>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<OptSet<Grade>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<Matte>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_map: Option<TimeMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_of_range: Option<OutOfRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<OptSet<CoverStyle>>,
    /// The layer's frame (decision 0046): set or cleared for this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<OptSet<LayerFrame>>,
}

impl LayerPatch {
    /// `self` then `later`: field-wise, later wins.
    pub fn then(&self, later: &LayerPatch) -> LayerPatch {
        LayerPatch {
            removed: later.removed.or(self.removed),
            crop: later.crop.or(self.crop),
            place: later.place.or(self.place),
            z: later.z.or(self.z),
            opacity: later.opacity.or(self.opacity),
            visible: later.visible.or(self.visible),
            mirror: later.mirror.or(self.mirror),
            feather: later.feather.or(self.feather),
            grade: later.grade.clone().or_else(|| self.grade.clone()),
            matte: later.matte.clone().or_else(|| self.matte.clone()),
            time_map: later.time_map.or(self.time_map),
            out_of_range: later.out_of_range.or(self.out_of_range),
            cover: later.cover.clone().or_else(|| self.cover.clone()),
            frame: later.frame.clone().or_else(|| self.frame.clone()),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == LayerPatch::default()
    }
}

/// One framing's look for a part: an optional linked layout (live) and sparse
/// overrides per layer id (SPEC §4.3.1).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct PartLook {
    pub layout_ref: Option<Id<Layout>>,
    #[serde(default)]
    pub overrides: BTreeMap<Id<Layer>, LayerPatch>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "part")]
pub struct Part {
    #[entity(id)]
    pub id: Id<Part>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    #[entity(order)]
    pub order: OrderKey,
    pub media: MediaRef,
    pub src_in: Flicks,
    pub src_out: Flicks,
    pub audio_from: Option<Id<SyncMember>>,
    pub role: PartRole,
    pub from: PartFrom,
    pub lineage: PartLineage,
    pub gain_db: f64,
    pub speed: f64,
    /// An eased speed change inside the part, from `speed` to `ramp.to` (decision 0033).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp: Option<SpeedRamp>,
    /// Parts grouped on the timeline move together (decision 0037); a split keeps both
    /// halves in the group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_group: Option<Id<PartGroup>>,
    /// The gain of every sound under this part's audio (decision 0051), in place of each
    /// sync member's own `under_db`; at most [`UNDER_OFF_DB`] silences them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under_db: Option<f64>,
    #[entity(map)]
    pub look: BTreeMap<Id<Framing>, PartLook>,
    #[entity(map)]
    pub caption_place_override: BTreeMap<Id<Framing>, Place>,
}

/// A group of parts on the timeline (decision 0037): only an id the members share, not
/// an entity of its own.
#[derive(Debug)]
pub enum PartGroup {}

/// An eased speed change inside a part (decision 0033): the part plays at its `speed` up
/// to source time `src_start`, eases to `to` along `curve` by `src_end`, and plays at `to`
/// after it. On the source clock, so a trim or split keeps it on the footage it speeds;
/// the part of it outside the part's own range is clipped off
/// ([`crate::clock::speed_profile`]).
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpeedRamp {
    pub to: f64,
    pub src_start: Flicks,
    pub src_end: Flicks,
    pub curve: Curve,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    Cut,
    Ease { dur_ms: u32, curve: Curve },
}

/// A visual split: from `at_src` on, this framing's layers take `overrides` (sparse,
/// per layer id). Partitioned by `at_src` when the part splits (R-MODEL-2).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "visual_split")]
pub struct VisualSplit {
    #[entity(id)]
    pub id: Id<VisualSplit>,
    #[entity(parent)]
    pub part: Id<Part>,
    pub framing: Id<Framing>,
    pub at_src: Flicks,
    pub overrides: BTreeMap<Id<Layer>, LayerPatch>,
    pub transition: Transition,
    /// What wrote the split, when a feature owns it (decision 0025): a punch-in's pair.
    /// Absent on every hand-made split (and on every split older than the field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<SplitTag>,
}

/// A feature that owns a visual split (decision 0025), so it can find and remove its own.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SplitTag {
    /// A punch-in on a stressed word: the eased zoom in.
    PunchIn,
    /// The punch-in's eased return, at the end of its phrase.
    PunchOut,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldAt {
    AtSrc(Flicks),
    Frame(FrameRef),
}

/// Hold the named layers on one frame between two anchors (shared across framings).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "content_hold")]
pub struct ContentHold {
    #[entity(id)]
    pub id: Id<ContentHold>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub from: Anchor,
    pub to: Anchor,
    pub at: HoldAt,
    /// Empty = every layer but `cam`.
    pub roles: Vec<LayerRole>,
    #[entity(map)]
    pub per_framing: BTreeMap<Id<Framing>, FramingOptOut>,
}

/// Hide the named layers between two anchors (shared across framings).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "hide_roles")]
pub struct HideRoles {
    #[entity(id)]
    pub id: Id<HideRoles>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub from: Anchor,
    pub to: Anchor,
    pub roles: Vec<LayerRole>,
    #[entity(map)]
    pub per_framing: BTreeMap<Id<Framing>, FramingOptOut>,
}

// ---------------------------------------------------------------------------------------
// Framings, layouts, layers
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AspectClass {
    Portrait,
    Landscape,
    Square,
    Other,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackdropFit {
    Cover,
    Contain,
    Stretch,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    Solid {
        colour: String,
    },
    Plate {
        name: String,
    },
    Backdrop {
        asset: Id<Asset>,
        r#loop: bool,
        fit: BackdropFit,
    },
    BlurredSource,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionGrouping {
    StoryV5,
    SingleWord,
    AdaptiveWords,
    ReadablePhrase,
    Phrase,
}

/// How a caption cue enters and leaves (SPEC §5.10 style `animation` [M2]).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionAnimation {
    /// Cut in, cut out (Story; every golden cut).
    #[default]
    None,
    /// Word bounce: scale 72 → 118 → 100 % in, 100 → 76 % out, with alpha fades.
    Bounce,
    /// A smaller overshoot in (85 → 108 → 100 %), no fade.
    Pop,
    /// Alpha in and out, no scale.
    Fade,
}

/// What a keyword does on top of its colour.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeywordAnimation {
    /// The keyword colour only.
    #[default]
    None,
    /// The cue pulses to 112 % and the keyword's colour pops in from the fill.
    Pop,
}

/// A framing's caption look beyond the text style: animation, keyword animation and the
/// keyword colour (absent = the reference yellow `#FFD400FF`).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CaptionLook {
    #[serde(default)]
    pub animation: CaptionAnimation,
    #[serde(default)]
    pub keyword_animation: KeywordAnimation,
    /// `#RRGGBBAA`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_colour: Option<String>,
    /// Mark the words the speaker stresses as keywords on their own (decision 0025): the
    /// style asks for it, so the app applies the stressed-word proposals instead of only
    /// listing them. Off = proposals only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_emphasis: bool,
}

impl CaptionLook {
    /// Nothing beyond the reference's look (no animation, the default keyword colour, no
    /// automatic emphasis).
    pub fn is_plain(&self) -> bool {
        self.animation == CaptionAnimation::None
            && self.keyword_animation == KeywordAnimation::None
            && self.keyword_colour.is_none()
            && !self.auto_emphasis
    }
}

/// A caption preset (SPEC §8 style library): the text style, grouping and look a caption
/// style gallery tile applies.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptionPreset {
    pub style: TextStyle,
    pub grouping: CaptionGrouping,
    #[serde(default)]
    pub look: CaptionLook,
}

/// The project's caption presets (saved styles), by name.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "caption_presets")]
pub struct CaptionPresets {
    #[entity(id)]
    pub id: Id<CaptionPresets>,
    #[entity(map)]
    pub entries: BTreeMap<String, CaptionPreset>,
}

/// One item of the project's style library (SPEC §8 [M2]: text styles, pill presets,
/// band sets, layouts and stream layouts; caption presets are [`CaptionPresets`]).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryItem {
    /// A named text style (applied as a `Styles` entry).
    TextStyle(TextStyle),
    /// A section-pill style: a text style with its pill box (applied as a `Styles` entry).
    PillPreset(TextStyle),
    /// A framing's named bands (fractions of canvas height).
    Bands(BandSet),
    /// A layout's layer stack and caption seat.
    Layout(LayoutPreset),
    /// A stream layout's regions (normalised to its canvas).
    StreamLayout(StreamLayoutPreset),
    /// A background plate: a gradient drawn under every layer (decision 0029).
    Plate(PlatePreset),
}

/// A background plate (decision 0029): `#RRGGBB` stops spread evenly from the canvas's top
/// to its bottom.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PlatePreset {
    pub stops: Vec<String>,
}

impl LibraryItem {
    /// The kind's name, as the library keys and the commands say it.
    pub fn kind(&self) -> &'static str {
        match self {
            LibraryItem::TextStyle(_) => "text_style",
            LibraryItem::PillPreset(_) => "pill_preset",
            LibraryItem::Bands(_) => "bands",
            LibraryItem::Layout(_) => "layout",
            LibraryItem::StreamLayout(_) => "stream_layout",
            LibraryItem::Plate(_) => "plate",
        }
    }
}

/// A saved layout: its layers (identity-free) and caption seat.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayoutPreset {
    pub aspect_class: AspectClass,
    #[serde(default)]
    pub caption_place: Option<Place>,
    pub layers: Vec<LayerPreset>,
}

/// One layer of a [`LayoutPreset`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayerPreset {
    pub kind: LayerKind,
    pub role: LayerRole,
    pub crop: NormRect,
    pub place: Place,
    pub z: i32,
    pub opacity: f64,
    pub visible: bool,
    #[serde(default)]
    pub cover: Option<CoverStyle>,
    /// The layer's frame (decision 0046).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<LayerFrame>,
}

/// A saved stream layout: the regions of a scene (SPEC §5.7.2), normalised.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StreamLayoutPreset {
    pub canvas_size: Canvas,
    pub regions: Vec<StreamRegion>,
}

/// The project's style library beyond caption presets, keyed `<kind>:<name>`.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "style_library")]
pub struct StyleLibrary {
    #[entity(id)]
    pub id: Id<StyleLibrary>,
    #[entity(map)]
    pub entries: BTreeMap<String, LibraryItem>,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Band {
    pub y0: f64,
    pub y1: f64,
}

/// Named bands per framing (fractions of canvas height).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct BandSet {
    #[serde(default)]
    pub bands: BTreeMap<String, Band>,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoudnessMode {
    AsRecorded,
    Static {
        integrated_lufs: f64,
        true_peak_dbtp: f64,
    },
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "framing")]
pub struct Framing {
    #[entity(id)]
    pub id: Id<Framing>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub name: String,
    pub canvas: Canvas,
    pub fps: Rational,
    pub aspect_class: AspectClass,
    pub background: Background,
    pub caption_style_ref: Option<String>,
    pub caption_grouping: CaptionGrouping,
    pub caption_place: Option<Place>,
    pub bands: BandSet,
    pub safe_zones: String,
    pub clips_enabled: Vec<Id<Clip>>,
    pub loudness_mode: Option<LoudnessMode>,
    pub export_presets: Vec<String>,
    /// Caption animation and keyword look (absent = the reference's: none, yellow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_look: Option<CaptionLook>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayoutScope {
    Cut,
    Project,
    Library,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "layout")]
pub struct Layout {
    #[entity(id)]
    pub id: Id<Layout>,
    /// The owning cut when `scope = cut`.
    #[entity(parent)]
    pub cut: Option<Id<Cut>>,
    pub scope: LayoutScope,
    pub name: String,
    pub aspect_class: AspectClass,
    pub caption_place: Option<Place>,
    pub region_relative: bool,
}

/// Who owns a layer's stack position (SPEC §4.3 `Layer.owner`).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayerOwner {
    Layout(Id<Layout>),
    Framing(Id<Framing>),
    /// A layer a part adds in one framing (an addition in the sparse override).
    PartOverride {
        part: Id<Part>,
        framing: Id<Framing>,
    },
    Clip(Id<Clip>),
}

impl AsParent for LayerOwner {
    fn as_parent(&self) -> Option<EntityId> {
        match self {
            LayerOwner::Layout(l) => l.as_parent(),
            LayerOwner::Framing(f) => f.as_parent(),
            LayerOwner::PartOverride { part, .. } => part.as_parent(),
            LayerOwner::Clip(c) => c.as_parent(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    SourceView,
    Face,
    Extra,
    VideoClip,
    Cover,
}

#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Feather {
    pub l: f64,
    pub r: f64,
    pub t: f64,
    pub b: f64,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Grade {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChromaKey {
    pub colour: String,
    pub similarity: f64,
    pub blend: f64,
    pub despill_mix: f64,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Matte {
    None,
    Chroma(ChromaKey),
    /// A person cut-out (SPEC §5.8): `model` names the segmentation family
    /// (`apple_vision`), `soften` the edge softening radius in source pixels (0–32), and
    /// `strokes` the brush corrections painted on top of the tracked mask (a part's
    /// override carries them, so they apply to that part only).
    Roto {
        model: String,
        soften: f64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        strokes: Vec<MatteStroke>,
    },
}

/// One edge-brush stroke on a roto matte, painted on a reference frame and applied to
/// every frame of the part.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MatteStroke {
    pub mode: StrokeMode,
    /// Brush radius as a fraction of the source height.
    pub radius: f64,
    /// Soft edge as a fraction of the radius (0 hard … 1 soft to the centre).
    #[serde(default)]
    pub feather: f64,
    /// The polyline, in normalised source coordinates `[x/width, y/height]`.
    pub points: Vec<[f64; 2]>,
    /// The source time of the frame it was painted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_src: Option<Flicks>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokeMode {
    /// Adds to the person.
    Add,
    /// Takes away from the person.
    Subtract,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeMap {
    Live,
    Hold(HoldAt),
    FreezeFirst,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutOfRange {
    Hide,
    HoldFirst,
    HoldLast,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverStyle {
    Blur { radius: f64 },
    Pixelate { block: f64 },
    Solid { colour: u32 },
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "layer")]
pub struct Layer {
    #[entity(id)]
    pub id: Id<Layer>,
    #[entity(parent)]
    pub owner: LayerOwner,
    #[entity(order)]
    pub order: OrderKey,
    pub kind: LayerKind,
    pub member: Option<Id<SyncMember>>,
    pub role: LayerRole,
    pub crop: NormRect,
    pub place: Place,
    pub z: i32,
    pub opacity: f64,
    pub visible: bool,
    pub mirror: bool,
    pub feather: Feather,
    /// Unkeyed layers only.
    pub grade: Option<Grade>,
    pub matte: Matte,
    pub time_map: TimeMap,
    pub out_of_range: OutOfRange,
    pub cover: Option<CoverStyle>,
    /// A cover pinned to a moving point (decision 0033): the point's keys on its video's
    /// clock, as for an overlay (decision 0031). The cover sits where its crop and place
    /// put it when the point is at the first key, and moves with the point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<OverlayTrack>,
    /// The layer's frame (decision 0046, "screen look"): rounded corners, a shadow under
    /// it and a border on it. Absent: drawn as a plain rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<LayerFrame>,
}

/// A layer's frame (decision 0046): its placed picture with rounded corners, a soft shadow
/// beneath and a border along the inside of the rounded edge.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayerFrame {
    /// Corner radius as a fraction of the placed layer's short side, 0–0.5 (0.5 on a
    /// square place is a circle).
    pub radius: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<FrameShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<FrameBorder>,
}

/// A frame's shadow: the rounded shape moved down by `offset_y` and softened over `blur`
/// (both fractions of the canvas's short side), `colour` (`#RRGGBB`) at `opacity`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FrameShadow {
    /// 0–0.5 of the canvas's short side.
    pub blur: f64,
    /// −0.5–0.5 of the canvas's short side (positive: down).
    pub offset_y: f64,
    /// 0–1.
    pub opacity: f64,
    pub colour: String,
}

/// A frame's border: `width` (a fraction of the canvas's short side, 0–0.1) inside the
/// rounded edge, in `colour` (`#RRGGBB`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FrameBorder {
    pub width: f64,
    pub colour: String,
}

// ---------------------------------------------------------------------------------------
// Overlays, keyframes, audio
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayParent {
    Cut(Id<Cut>),
    Clip(Id<Clip>),
}

impl AsParent for OverlayParent {
    fn as_parent(&self) -> Option<EntityId> {
        match self {
            OverlayParent::Cut(c) => c.as_parent(),
            OverlayParent::Clip(c) => c.as_parent(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Image,
    Text,
    Video,
    Shape,
    Counter,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayEnd {
    At(Anchor),
    DurMs(u32),
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayGeom {
    Place(Place),
    Box(CanvasBox),
}

#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct OverlayPerFraming {
    pub visible: bool,
    pub place: Option<Place>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanMode {
    ClampToStretch,
    Continue,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Centre,
    Right,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextFit {
    None,
    Shrink,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextCase {
    AsIs,
    Upper,
    Lower,
    Title,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Outline {
    pub w: f64,
    pub colour: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Shadow {
    pub dx: f64,
    pub dy: f64,
    pub blur: f64,
    pub colour: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Pill {
    pub on: bool,
    pub colour: String,
    pub opacity: f64,
    pub radius: f64,
    pub pad_x: f64,
    pub pad_y: f64,
    /// One `radius`-rounded box per line (SPEC §5.9, the TikTok text look) instead of
    /// libass's box per glyph (BorderStyle 3, what `.ass` exports reproduce).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub per_line: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayText {
    pub runs: Vec<TextRun>,
    pub style: TextStyle,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CounterStart {
    Measurement(Id<Measurement>),
    Anchor(Anchor),
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CounterClock {
    Src,
    Out,
}

/// A run clock (SPEC §4.3 `counter`): before `start` it shows `running_label` with
/// `0:00`; from `stop` it freezes with `done_label`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Counter {
    pub start: CounterStart,
    pub stop: Option<CounterStart>,
    pub clock: CounterClock,
    pub format: String,
    pub running_label: String,
    pub done_label: String,
}

/// Where a number shown on screen came from (R-AI-4).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NumberProvenance {
    pub text: String,
    pub measurement: Option<Id<Measurement>>,
    pub note: Option<String>,
    /// The word of the recording where the speaker says this number ("seven" or "7"):
    /// the speaker's own number, checked against the transcript as a measurement is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<crate::anchor::WordRef>,
}

/// An image overlay's picture: a stored asset (re-encoded PNG) and its natural size.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayImage {
    pub asset: Id<Asset>,
    pub w: u32,
    pub h: u32,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    /// A box on the overlay's `geom` box.
    Box,
    /// An arrow from `from` to `to`.
    Arrow,
}

/// A callout (SPEC §5.9 shape overlay): lengths in short-side units (thousandths of the
/// canvas's short side: 4 is 4.3 px on 1080), points as fractions of the canvas.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayShape {
    pub kind: ShapeKind,
    /// `#RRGGBBAA`.
    pub stroke: String,
    /// Stroke width (short-side units; 0 = none for a box).
    pub width: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// Corner radius (short-side units).
    #[serde(default)]
    pub radius: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<[f64; 2]>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "overlay")]
pub struct Overlay {
    #[entity(id)]
    pub id: Id<Overlay>,
    #[entity(parent)]
    pub parent: OverlayParent,
    pub kind: OverlayKind,
    pub start: Anchor,
    pub end: OverlayEnd,
    pub band: Option<String>,
    pub z: i32,
    pub opacity: f64,
    pub geom: OverlayGeom,
    #[entity(map)]
    pub per_framing: BTreeMap<Id<Framing>, OverlayPerFraming>,
    pub span_mode: SpanMode,
    pub text: Option<OverlayText>,
    pub counter: Option<Counter>,
    pub role: Option<String>,
    pub number_provenance: Vec<NumberProvenance>,
    /// An image overlay's picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<OverlayImage>,
    /// A shape overlay's callout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<OverlayShape>,
    /// A video overlay's media (decision 0028): picture-in-picture over its span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<OverlayVideo>,
    /// The moving point the overlay is pinned to (decision 0031).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<OverlayTrack>,
    /// An image or video overlay drawn among the part's picture layers (decision 0042):
    /// just under the first layer whose `z` is at least this (`i32::MIN` = behind them
    /// all); absent = above the picture, as every overlay was before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<i32>,
}

/// A moving point an overlay follows (decision 0031): where a tracker found it on a
/// video's picture (normalised, origin top left) at times on that video's clock. The
/// overlay is drawn moved by the point's move since the overlay's start, through the
/// layer that shows the video, so a re-framed cut keeps it pinned.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayTrack {
    pub media: MediaRef,
    pub keys: Vec<TrackPoint>,
    /// What followed it (`apple_vision.track_object`).
    pub method: String,
}

/// The tracked point at one source time.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TrackPoint {
    pub t: Flicks,
    pub x: f64,
    pub y: f64,
}

/// A video overlay's media (decision 0028): a span of a project video, or a stored video
/// asset from its start. It plays from its start over the overlay's span, silent;
/// `loop` starts it again when the span outlasts it, else its last frame holds.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayVideo {
    pub media: AssetOrSpan,
    #[serde(default, rename = "loop")]
    pub r#loop: bool,
    /// The part of the picture shown (normalised); absent = all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<NormRect>,
    /// A roto cut-out of the video's person (decision 0042); only `Matte::Roto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<Matte>,
    /// Runs on another video's clock (graphics rendered against a talk): the overlay shows
    /// wherever the cut plays the locked video's times inside the span, each frame the
    /// span's frame of that moment, so trims, cuts and reorders keep it in step. Its start
    /// and end anchors then only place it on the timeline's lanes. Only with a span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<VideoLock>,
}

/// What a video overlay's span is locked to: `media`'s time `t` shows the span's frame at
/// `t − offset` (0 when the overlay was rendered on the same clock).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct VideoLock {
    pub media: MediaRef,
    #[serde(default)]
    pub offset: Flicks,
}

/// Brand colours (`#RRGGBB`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BrandColours {
    pub primary: String,
    pub accent: String,
    pub text: String,
    pub background: String,
}

impl Default for BrandColours {
    fn default() -> Self {
        BrandColours {
            primary: "#0A84FF".into(),
            accent: "#FFD400".into(),
            text: "#FFFFFF".into(),
            background: "#0C1118".into(),
        }
    }
}

/// A run clock template's clock (its measurement is chosen when it is inserted).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TemplateCounter {
    pub clock: CounterClock,
    pub format: String,
    pub running_label: String,
    pub done_label: String,
}

/// An overlay without its times or parent (SPEC §8 overlay templates).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OverlayTemplate {
    pub kind: OverlayKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<String>,
    pub z: i32,
    pub opacity: f64,
    pub geom: OverlayGeom,
    pub dur_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<OverlayText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<TemplateCounter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<OverlayImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<OverlayShape>,
}

/// The project's brand kit (SPEC §8; profile content, never authority — R-PROF-4): what
/// new cuts take by default, the lower third and end card, and overlay templates.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "brand_kit")]
pub struct BrandKit {
    #[entity(id)]
    pub id: Id<BrandKit>,
    pub name: String,
    pub handle: String,
    pub heading_font: Option<String>,
    pub body_font: Option<String>,
    pub colours: BrandColours,
    pub logo: Option<OverlayImage>,
    pub caption_preset: Option<String>,
    #[entity(map)]
    pub templates: BTreeMap<String, OverlayTemplate>,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "keyframe")]
pub struct Keyframe {
    #[entity(id)]
    pub id: Id<Keyframe>,
    #[entity(parent)]
    pub overlay: Id<Overlay>,
    #[entity(order)]
    pub order: OrderKey,
    pub at_local: Anchor,
    pub place: Option<Place>,
    pub scale: Option<f64>,
    pub opacity: Option<f64>,
    pub ease: Curve,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Duck {
    pub depth_db: f64,
    pub attack_ms: u32,
    pub release_ms: u32,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "audio_overlay")]
pub struct AudioOverlay {
    #[entity(id)]
    pub id: Id<AudioOverlay>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub media: AssetOrSpan,
    pub start: Anchor,
    pub end: Anchor,
    pub gain_db: f64,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    pub duck_under_speech: Option<Duck>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GainCurve {
    LinearDb,
}

/// A gain envelope segment (anchored). Defaults: ramps 80 / 120 ms, `linear_db`.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "gain_segment")]
pub struct GainSegment {
    #[entity(id)]
    pub id: Id<GainSegment>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    #[entity(order)]
    pub order: OrderKey,
    pub start: Anchor,
    pub end: Anchor,
    pub gain_db: f64,
    pub ramp_in_ms: u32,
    pub ramp_out_ms: u32,
    pub curve: GainCurve,
    pub measurement_ref: Option<Id<Measurement>>,
}

// ---------------------------------------------------------------------------------------
// Captions
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionSource {
    Supplied,
    /// The cut's own audio: each part's words are heard from what the cut plays, and a
    /// part whose audio is identical to its source reuses the source transcript's words
    /// (SPEC §5.10, decision 0022). `stream` (the source words through every part) was
    /// folded in on 2026-10-16 and reads as this.
    #[serde(alias = "stream")]
    Heard,
    Imported,
}

/// A hand-made cue boundary at a word (see `CaptionDoc::cue_breaks`).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CueBreak {
    /// A cue starts at this word.
    Split,
    /// This word stays in the cue of the word before it.
    Join,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionEdit {
    Text(String),
    Drop,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WordTimeOverride {
    pub start: Option<Flicks>,
    pub end: Option<Flicks>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NoCaptionRange {
    pub from: Anchor,
    pub to: Anchor,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Subline {
    pub content: String,
    pub style_ref: Option<String>,
}

/// A cut's captions (one per cut).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "caption_doc")]
pub struct CaptionDoc {
    #[entity(id)]
    pub id: Id<CaptionDoc>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub enabled: bool,
    pub source: CaptionSource,
    #[entity(map)]
    pub edits: BTreeMap<WordId, CaptionEdit>,
    #[entity(map)]
    pub word_time_overrides: BTreeMap<WordId, WordTimeOverride>,
    /// `id → word` (SPEC §4.3).
    #[entity(map)]
    pub keywords: BTreeMap<String, WordId>,
    /// Keyword terms (the reference's `keywords`): every caption word matching one is starred, across
    /// the cut. A term of several words stars each word of a run that matches it.
    #[serde(default)]
    pub keyword_terms: Vec<String>,
    #[entity(map)]
    pub no_caption_ranges: BTreeMap<String, NoCaptionRange>,
    /// Hand-made cue boundaries over the built grouping (the Short tab's "split here" /
    /// "join with next"): `split` starts a cue at the word, `join` keeps it in the cue of
    /// the word before.
    #[entity(map)]
    #[serde(default)]
    pub cue_breaks: BTreeMap<WordId, CueBreak>,
    pub subline: Option<Subline>,
    pub drop_fillers: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CueProvenance {
    Import,
}

/// An imported cue, held verbatim (only with `CaptionDoc.source: imported`).
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "cue")]
pub struct Cue {
    #[entity(id)]
    pub id: Id<Cue>,
    #[entity(parent)]
    pub caption_doc: Id<CaptionDoc>,
    #[entity(order)]
    pub order: OrderKey,
    pub from: Anchor,
    pub to: Anchor,
    pub runs: Vec<TextRun>,
    pub style_ref: Option<String>,
    pub provenance: CueProvenance,
}

// ---------------------------------------------------------------------------------------
// Clips, measurements
// ---------------------------------------------------------------------------------------

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClipSlot {
    Intro,
    Outro,
    After(Id<Part>),
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClipWord {
    pub word: String,
    pub start: Flicks,
    pub end: Flicks,
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "clip")]
pub struct Clip {
    #[entity(id)]
    pub id: Id<Clip>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    pub media: AssetOrSpan,
    pub slot: ClipSlot,
    #[entity(map)]
    pub look: BTreeMap<Id<Framing>, PartLook>,
    pub key: Option<ChromaKey>,
    pub gain_db: f64,
    pub match_loudness_to_cut: bool,
    pub words: Vec<ClipWord>,
    pub caption_place: Option<Place>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    Interval,
    Edge,
    Loudness,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasureEnd {
    Frame(FrameRef),
    Anchor(Anchor),
    Event(EventId),
}

#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "measurement")]
pub struct Measurement {
    #[entity(id)]
    pub id: Id<Measurement>,
    pub kind: MeasurementKind,
    pub from: MeasureEnd,
    pub to: Option<MeasureEnd>,
    pub value: f64,
    pub unit: String,
    pub evidence_frames: Vec<FrameRef>,
    pub method: String,
    pub author: String,
}

// ---------------------------------------------------------------------------------------
// Sections (decision 0046)
// ---------------------------------------------------------------------------------------

/// A span of a cut, drawn as a lane above the parts and dragged across part edges like
/// Screen Studio's (decision 0046): a zoom on the screen, or a layout taken for a while.
/// Its ends are anchored as overlays' are, so trims and reorders carry it.
#[derive(Entity, Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[entity(kind = "section")]
pub struct Section {
    #[entity(id)]
    pub id: Id<Section>,
    #[entity(parent)]
    pub cut: Id<Cut>,
    /// The framing it applies to (absent: every framing of the cut).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<Id<Framing>>,
    pub start: Anchor,
    pub end: Anchor,
    pub kind: SectionKind,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// The target layer drawn through a crop `depth`× tighter than its own.
    Zoom(ZoomSpec),
    /// The framing's layers take `layout` over the span, eased from and back to what was
    /// there (the 0016 ease).
    Layout {
        layout: Id<Layout>,
        #[serde(default = "default_layout_ease_ms")]
        ease_ms: u32,
    },
}

fn default_layout_ease_ms() -> u32 {
    400
}

/// A zoom section (decision 0046). Defaults: depth 2, target `cursor`, 700 ms eases,
/// layer `screen`, no camera shrink, no motion blur, hand-made.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ZoomSpec {
    /// How many times tighter than the layer's own crop (1–6).
    #[serde(default = "default_zoom_depth")]
    pub depth: f64,
    #[serde(default)]
    pub target: ZoomTarget,
    #[serde(default = "default_zoom_ease_ms")]
    pub ease_in_ms: u32,
    #[serde(default = "default_zoom_ease_ms")]
    pub ease_out_ms: u32,
    /// Which layers zoom.
    #[serde(default)]
    pub layer: ZoomLayer,
    /// Camera layers shrink toward their nearest canvas corner to this share (0.3–1)
    /// while zoomed (absent: they stay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_scale: Option<f64>,
    /// 5 sub-samples across half a frame while the crop moves (off by default).
    #[serde(default)]
    pub motion_blur: bool,
    /// Written by `zoom_auto` (replaced when it runs again); hand-made ones are kept.
    #[serde(default)]
    pub auto: bool,
}

fn default_zoom_depth() -> f64 {
    2.0
}

fn default_zoom_ease_ms() -> u32 {
    700
}

impl Default for ZoomSpec {
    fn default() -> Self {
        ZoomSpec {
            depth: default_zoom_depth(),
            target: ZoomTarget::default(),
            ease_in_ms: default_zoom_ease_ms(),
            ease_out_ms: default_zoom_ease_ms(),
            layer: ZoomLayer::default(),
            camera_scale: None,
            motion_blur: false,
            auto: false,
        }
    }
}

/// What a zoom is centred on.
#[derive(Copy, Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZoomTarget {
    /// The recorded cursor (decision 0045), through a critically damped spring.
    #[default]
    Cursor,
    /// The caret, else the focused field, from the input log.
    Focus,
    /// A fixed point, normalised in the target layer's crop.
    Point { x: f64, y: f64 },
}

/// Which layers a zoom draws tighter.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZoomLayer {
    /// Screen and detail layers.
    #[default]
    Screen,
    /// Layers that draw a recorded cursor.
    Cursor,
    /// Every picture layer but covers.
    All,
}
