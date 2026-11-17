//! The seams to rendering and media (R-ARCH-1). Core defines these traits; the app
//! and CLI hand in implementations backed by gmclip-render / gmclip-media. Headless
//! and CI builds use [`NullRenderer`].

use std::path::Path;

use gmclip_model::Flicks;

/// Why a host call failed.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HostError {
    /// This host cannot do that (e.g. the null renderer asked for pixels).
    Unsupported(&'static str),
    /// The host failed; the message is shape-only (no paths, no user text).
    Failed(String),
}

/// One frame request on the output clock of a cut.
#[derive(Clone, Debug)]
pub struct FrameRequest {
    pub canvas: (u32, u32),
    pub at: Flicks,
}

/// What a renderer produced.
#[derive(Clone, Debug)]
pub enum RenderedFrame {
    /// No pixels: the frame was planned but not drawn (null renderer).
    Planned { canvas: (u32, u32) },
    /// Tightly packed RGBA8.
    Rgba { canvas: (u32, u32), pixels: Vec<u8> },
}

/// Draws frames for core (previews, stills, `see`).
pub trait Renderer: Send {
    fn name(&self) -> &'static str;
    fn render_frame(&mut self, req: &FrameRequest) -> Result<RenderedFrame, HostError>;
}

/// Opens and probes media for core (import, analysis).
pub trait MediaHost: Send + Sync {
    fn name(&self) -> &'static str;
    fn probe(&self, path: &Path) -> Result<MediaProbe, HostError>;
}

/// The facts import needs about a source. Shape grows with B-01/B-02.
#[derive(Clone, Debug, Default)]
pub struct MediaProbe {
    pub duration: Flicks,
    pub video_tracks: u32,
    pub audio_tracks: u32,
}

/// A renderer with no GPU: plans frames and draws nothing. Used by headless and CI
/// builds so model, store and core test without a GPU (R-ARCH-1).
#[derive(Copy, Clone, Debug, Default)]
pub struct NullRenderer;

impl Renderer for NullRenderer {
    fn name(&self) -> &'static str {
        "null"
    }

    fn render_frame(&mut self, req: &FrameRequest) -> Result<RenderedFrame, HostError> {
        Ok(RenderedFrame::Planned { canvas: req.canvas })
    }
}

// -------------------------------------------------------------------------------------
// The M1 seams for `see`, `export` and `verify` (SPEC §11.2), and the planner
// -------------------------------------------------------------------------------------

/// A rectangle to black out on the canvas (a private-class region, R-INJ-7), in canvas
/// pixels, with what it hides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub label: String,
}

/// How `see` lays its frames out.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeeLayout {
    /// One frame.
    Frame,
    /// A row of ≤ 6 tiles.
    Strip,
    /// A grid of ≤ 16 tiles.
    Sheet,
}

/// One `see` request: output frames of a plan, each with its masks.
#[derive(Clone, Debug)]
pub struct SeeRequest {
    pub frames: Vec<u64>,
    /// Per frame, the rectangles to black out (empty when the run may see private
    /// regions).
    pub masks: Vec<Vec<MaskRect>>,
    pub layout: SeeLayout,
    /// The image's long side, ≤ 1568 (R-MCP-4).
    pub max_px: u32,
    pub labels: bool,
}

/// A rendered `see` image.
#[derive(Clone, Debug)]
pub struct SeeImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub notes: Vec<String>,
}

/// A finished export.
#[derive(Clone, Debug, Default)]
pub struct ExportDone {
    pub out: std::path::PathBuf,
    pub frames: u64,
    pub seconds: f64,
    pub notes: Vec<String>,
}

/// Renders, exports and verifies RenderPlans for core (the CLI and app implement it on
/// gmclip-render and gmclip-media; core never links them, R-ARCH-1).
pub trait MediaRender: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    /// `see`: frames of `plan` composited (text included), masked, laid out, as PNG.
    fn see(&self, plan: &gmclip_plan::Plan, req: &SeeRequest) -> Result<SeeImage, HostError>;
    /// `export`: renders `plan` to `out` (installing it and its export record).
    fn export(
        &self,
        plan: &gmclip_plan::Plan,
        out: &Path,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ExportDone, HostError>;
    /// `verify{export}` (R-EXP-6, B-15): the container and stream checks of `out`
    /// against `plan`.
    fn verify(&self, plan: &gmclip_plan::Plan, out: &Path) -> Result<serde_json::Value, HostError>;
    /// Roto matte quality per matted part and layer (SPEC §5.8 matte view): coverage,
    /// unsure edges, flicker, lost frames and a 0–100 score from the accurate mask
    /// tracks (built first when `build`; else unbuilt tracks are reported `missing`).
    fn matte_quality(
        &self,
        _plan: &gmclip_plan::Plan,
        _build: bool,
    ) -> Result<serde_json::Value, HostError> {
        Err(HostError::Unsupported("this build has no roto mattes"))
    }
    /// Grey frames of plan source `source` at presentation indexes `frames`, at most
    /// `max_w` wide (the scrub proxy where it is built, else the master): the activity
    /// crop's input (SPEC §5.7).
    fn luma_frames(
        &self,
        _plan: &gmclip_plan::Plan,
        _source: &str,
        _frames: &[u64],
        _max_w: u32,
    ) -> Result<Vec<crate::activity::LumaFrame>, HostError> {
        Err(HostError::Unsupported("this build reads no frames"))
    }
    /// The bytes of a stored asset (a recording's input track, decision 0045). The
    /// default reads nothing; the holder then looks in app data's `assets/`.
    fn asset_bytes(&self, _asset: &gmclip_model::entities::Asset) -> Result<Vec<u8>, HostError> {
        Err(HostError::Unsupported("this build reads no assets"))
    }
    /// `verify{joins}`: the per-join word check (lane D's ASR; a dev path until then).
    fn verify_joins(
        &self,
        _plan: &gmclip_plan::Plan,
        _out: &Path,
    ) -> Result<serde_json::Value, HostError> {
        Err(HostError::Unsupported(
            "join verification needs gmclip's ASR (lane D)",
        ))
    }
}

/// No rendering (headless CI): every call is `Unsupported`.
#[derive(Copy, Clone, Debug, Default)]
pub struct NullMediaRender;

impl MediaRender for NullMediaRender {
    fn name(&self) -> &'static str {
        "null"
    }
    fn see(&self, _: &gmclip_plan::Plan, _: &SeeRequest) -> Result<SeeImage, HostError> {
        Err(HostError::Unsupported("this build renders no frames"))
    }
    fn export(
        &self,
        _: &gmclip_plan::Plan,
        _: &Path,
        _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ExportDone, HostError> {
        Err(HostError::Unsupported("this build exports nothing"))
    }
    fn verify(&self, _: &gmclip_plan::Plan, _: &Path) -> Result<serde_json::Value, HostError> {
        Err(HostError::Unsupported("this build verifies nothing"))
    }
}

/// The model → RenderPlan planner (A18, lane A). The holder calls it for `see` and
/// `export`. Contract: plan `sources[].path` is the model source's path (so `see` can
/// mask a source's private regions); output frame k is cut time `⌈t·F − ½⌉` (SPEC
/// §4.1.1).
pub trait Planner: Send + Sync + std::fmt::Debug {
    fn plan(
        &self,
        doc: &gmclip_model::Doc,
        cut: gmclip_model::Id<gmclip_model::entities::Cut>,
        framing: gmclip_model::Id<gmclip_model::entities::Framing>,
    ) -> Result<gmclip_plan::Plan, gmclip_proto::CmdError>;

    /// [`Planner::plan`] with the project's transcripts (heard captions are built from
    /// them). The default ignores them.
    fn plan_with(
        &self,
        doc: &gmclip_model::Doc,
        cut: gmclip_model::Id<gmclip_model::entities::Cut>,
        framing: gmclip_model::Id<gmclip_model::entities::Framing>,
        _view: &crate::analysis::AnalysisView,
    ) -> Result<gmclip_plan::Plan, gmclip_proto::CmdError> {
        self.plan(doc, cut, framing)
    }
}

/// The model planner (A18, `crate::planner::plan_cut`) behind the seam; `H` supplies
/// what the document does not store (font files).
#[derive(Debug)]
pub struct ModelPlanner<H>(pub H);

impl<H: crate::planner::PlanHost + Send + Sync + std::fmt::Debug> Planner for ModelPlanner<H> {
    fn plan(
        &self,
        doc: &gmclip_model::Doc,
        cut: gmclip_model::Id<gmclip_model::entities::Cut>,
        framing: gmclip_model::Id<gmclip_model::entities::Framing>,
    ) -> Result<gmclip_plan::Plan, gmclip_proto::CmdError> {
        crate::planner::plan_cut(doc, cut, framing, &self.0).map_err(|e| {
            gmclip_proto::CmdError::Unsupported {
                platform: "planner".into(),
                reason: e.0.chars().take(160).collect(),
            }
        })
    }

    fn plan_with(
        &self,
        doc: &gmclip_model::Doc,
        cut: gmclip_model::Id<gmclip_model::entities::Cut>,
        framing: gmclip_model::Id<gmclip_model::entities::Framing>,
        view: &crate::analysis::AnalysisView,
    ) -> Result<gmclip_plan::Plan, gmclip_proto::CmdError> {
        crate::planner::plan_cut_with(doc, cut, framing, &self.0, Some(view)).map_err(|e| {
            gmclip_proto::CmdError::Unsupported {
                platform: "planner".into(),
                reason: e.0.chars().take(160).collect(),
            }
        })
    }
}

/// No planner: every plan is `Unsupported`.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoPlanner;

impl Planner for NoPlanner {
    fn plan(
        &self,
        _: &gmclip_model::Doc,
        _: gmclip_model::Id<gmclip_model::entities::Cut>,
        _: gmclip_model::Id<gmclip_model::entities::Framing>,
    ) -> Result<gmclip_plan::Plan, gmclip_proto::CmdError> {
        Err(gmclip_proto::CmdError::Unsupported {
            platform: "m1".into(),
            reason: "no model planner (A18) in this host".into(),
        })
    }
}

impl From<HostError> for gmclip_proto::CmdError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Unsupported(r) => gmclip_proto::CmdError::Unsupported {
                platform: "host".into(),
                reason: r.into(),
            },
            HostError::Failed(m) => gmclip_proto::CmdError::Internal {
                trace_id: format!("host:{}", m.chars().take(60).collect::<String>()),
            },
        }
    }
}

// -------------------------------------------------------------------------------------
// Analysis and ASR (SPEC §9; lane D's gmclip-ml / gmclip-analysis behind a seam)
// -------------------------------------------------------------------------------------

/// A source's audio for analysis: the file and its voice track (by index among the
/// file's audio tracks; `None` = the first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSource {
    pub path: std::path::PathBuf,
    pub track: Option<u32>,
    /// The source's fingerprint (names the analysed bytes in blob revisions).
    pub fingerprint: String,
}

/// One analysis blob to store under `(owner, kind, revision)` (SPEC §4.4).
#[derive(Clone, Debug)]
pub struct AnalysisBlob {
    pub kind: String,
    pub revision: String,
    pub body: Vec<u8>,
}

/// A word an ASR heard (source times).
#[derive(Clone, Debug, PartialEq)]
pub struct HeardWord {
    pub text: String,
    pub start: Flicks,
    pub end: Flicks,
    pub confidence: Option<f32>,
}

/// An ASR result and the revision that names it (`asr_revision`).
#[derive(Clone, Debug)]
pub struct Transcribed {
    pub revision: String,
    pub words: Vec<HeardWord>,
}

/// Loudness of a span (BS.1770).
#[derive(Clone, Debug, PartialEq)]
pub struct SpanLoudness {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: Option<f64>,
}

/// `listen`: what a span sounds like.
#[derive(Clone, Debug)]
pub struct Heard {
    pub transcribed: Transcribed,
    pub loudness: Option<SpanLoudness>,
    /// RMS dB per 50 ms (summary contour).
    pub energy_db_50ms: Vec<f32>,
    pub silences: Vec<(Flicks, Flicks)>,
}

/// How a word edge is measured (SPEC §9 word edges, R-ANC-2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EdgeRule {
    /// 10 ms energy, an 18 dB drop held 60 ms.
    AudibleStart,
    AudibleEnd,
    /// The energy minimum between two words.
    EnergyMin,
    /// Abutting words: candidate minima checked by re-transcription (lane D's arbiter).
    Arbiter,
}

/// A measured edge.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredEdge {
    pub value: Flicks,
    pub method: String,
    pub window: (Flicks, Flicks),
    pub rms_db: Option<f64>,
}

/// One span a sweep would remove, or skipped and why.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepSpan {
    pub from: Flicks,
    pub to: Flicks,
    pub reason: String,
}

/// A sweep's candidates for some source spans (SPEC §5.5).
#[derive(Clone, Debug, Default)]
pub struct SweepCandidates {
    pub cuts: Vec<SweepSpan>,
    pub skipped: Vec<SweepSpan>,
    /// The analysis revision the candidates come from (recorded on apply).
    pub revision: String,
}

/// Runs analysis and ASR for core (the CLI and app implement it on gmclip-media,
/// gmclip-analysis and gmclip-ml). Core links only the pure-data gmclip-analysis, to read
/// stored blobs (decision 0056); it and the agent surface never link media or ml.
pub trait AnalysisHost: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    /// Waveform, energy (10/20/40 ms), quiet-grid inputs and VAD for a whole source.
    fn analyze(
        &self,
        a: &AudioSource,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<AnalysisBlob>, HostError>;
    /// [`AnalysisHost::analyze`], handing each blob to `each` as soon as it is made: the
    /// timeline's columns (peaks, energy, the quiet grid) are stored before the VAD runs
    /// (D.02). The default runs `analyze` and hands its blobs over in order.
    fn analyze_each(
        &self,
        a: &AudioSource,
        cancel: &std::sync::atomic::AtomicBool,
        each: &mut dyn FnMut(AnalysisBlob) -> Result<(), HostError>,
    ) -> Result<(), HostError> {
        for b in self.analyze(a, cancel)? {
            each(b)?;
        }
        Ok(())
    }
    /// Keeps (`true`) or drops the 16 kHz voice that transcriptions of `a` decode, so the
    /// `analyze` after a whole-source transcription reads no audio again (D.02: the
    /// columns share the ASR's decode). A host that keeps nothing ignores it.
    fn keep_voice(&self, _a: &AudioSource, _keep: bool) {}
    /// ASR of the voice track, whole (`None`) or a span.
    fn transcribe(
        &self,
        a: &AudioSource,
        span: Option<(Flicks, Flicks)>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Transcribed, HostError>;
    /// A fresh hearing of a span (≤ 120 s): words, loudness, energy, silences.
    fn listen(&self, a: &AudioSource, from: Flicks, to: Flicks) -> Result<Heard, HostError>;
    fn loudness(
        &self,
        a: &AudioSource,
        from: Flicks,
        to: Flicks,
    ) -> Result<SpanLoudness, HostError>;
    /// A word edge: `w1` is the word measured (or the first of an abutting pair), `w2`
    /// the next word for `EnergyMin` / `Arbiter`. Words are `(text, start, end)`.
    fn edge(
        &self,
        a: &AudioSource,
        rule: EdgeRule,
        w1: (&str, Flicks, Flicks),
        w2: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError>;
    /// Pauses of at least `min` inside `spans` (per-span threshold, SPEC §5.5), each
    /// keeping `keep` of silence; never across a word in `words`.
    fn pauses(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        min: Flicks,
        keep: Flicks,
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError>;
    /// Fillers among `words` inside `spans`, anchored on the quiet grid; fused ones skipped.
    fn fillers(
        &self,
        a: &AudioSource,
        spans: &[(Flicks, Flicks)],
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError>;
    /// A stored blob read back for `analysis_get` (a bounded summary over a span).
    fn blob_summary(
        &self,
        kind: &str,
        body: &[u8],
        from: Flicks,
        to: Flicks,
        max_points: usize,
    ) -> Result<serde_json::Value, HostError>;
}

/// No analysis in this build: every call is `Unsupported`.
#[derive(Copy, Clone, Debug, Default)]
pub struct NullAnalysisHost;

impl AnalysisHost for NullAnalysisHost {
    fn name(&self) -> &'static str {
        "null"
    }
    fn analyze(
        &self,
        _: &AudioSource,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<AnalysisBlob>, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
    fn transcribe(
        &self,
        _: &AudioSource,
        _: Option<(Flicks, Flicks)>,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Transcribed, HostError> {
        Err(HostError::Unsupported("this build has no ASR"))
    }
    fn listen(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<Heard, HostError> {
        Err(HostError::Unsupported("this build has no ASR"))
    }
    fn loudness(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<SpanLoudness, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
    fn edge(
        &self,
        _: &AudioSource,
        _: EdgeRule,
        _: (&str, Flicks, Flicks),
        _: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
    fn pauses(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: Flicks,
        _: Flicks,
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
    fn fillers(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
    fn blob_summary(
        &self,
        _: &str,
        _: &[u8],
        _: Flicks,
        _: Flicks,
        _: usize,
    ) -> Result<serde_json::Value, HostError> {
        Err(HostError::Unsupported("this build has no analysis"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_renderer_plans_without_pixels() {
        let mut r: Box<dyn Renderer> = Box::new(NullRenderer);
        let f = r
            .render_frame(&FrameRequest {
                canvas: (1080, 1920),
                at: Flicks::ZERO,
            })
            .unwrap();
        assert!(matches!(
            f,
            RenderedFrame::Planned {
                canvas: (1080, 1920)
            }
        ));
    }
}
