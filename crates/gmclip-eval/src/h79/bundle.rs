//! What the h79 grader reads: the truth, and one run's bundle.
//!
//! A bundle is a directory:
//!
//! | file | what | from |
//! |---|---|---|
//! | `plan.json` | the exported cut's RenderPlan (`gmclip.renderplan/0`) | the export record |
//! | `project.json` | facts from the run's project DB (`gmclip.eval.h79.project/0`, [`Project`]) | the runner, via `cut_get`/`export` reads |
//! | `transcript.jsonl` | the client transcript (`stream-json` / Codex `--json`) | the launcher |
//! | `run.json` | the launcher's run record | the launcher |
//! | `asr.json` | round-trip ASR of the exported MP4 (`{words:[{word,start,end}]}`, output seconds) | the runner |
//!
//! The grader parses the RenderPlan as plain JSON on purpose: it must not share code with
//! `gmclip-plan`, the thing under test (decisions/0006).
//!
//! TODO(A2): the runner fills `project.json` from the live project DB once A2's MCP server
//! and the export record exist; until then the self-test bundles are assembled from the v4
//! build (`selftest.rs`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `evals/h79/truth.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct Truth {
    pub golden: Golden,
    pub payoff_region_s: [f64; 2],
    pub setup_region_s: [f64; 2],
    pub reaction_region_s: [f64; 2],
    pub result_window_s: [f64; 2],
    pub run_clock: RunClock,
    pub cam_region_px: [f64; 4],
    pub cam_place_px: [f64; 4],
    pub thresholds: Thresholds,
    pub forbidden_fixture_strings: Vec<String>,
    pub golden_words_file: String,
    pub font_metrics_file: String,
    pub sources: TruthSources,
    /// truth.json's own approval flag (AJ-03); results made before it record that.
    #[serde(default)]
    pub approved_by_aj: bool,
}

/// The picture and audio sources by hash, with the picture's frame offset to the master
/// (stream) clock.
#[derive(Clone, Debug, Deserialize)]
pub struct TruthSources {
    pub picture: TruthSource,
    pub audio: TruthSource,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TruthSource {
    pub sha256: String,
    /// `master frame = source frame - frame_offset` (picture: −1642; audio: 0).
    #[serde(default)]
    pub frame_offset: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Golden {
    pub frames: u64,
    pub fps: u64,
    pub part_frames: Vec<u64>,
    pub words: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunClock {
    pub start_s: f64,
    pub stop_s: f64,
    pub tolerance_s: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Thresholds {
    pub duration_s: [f64; 2],
    pub arc_monotone_share: f64,
    pub median_part_s_min: f64,
    pub part_s_min: f64,
    pub parts_max: usize,
    pub hook_end_s: [f64; 2],
    pub hook_words: [usize; 2],
    pub hook_lines_max: usize,
    pub hook_chars_per_line_max: usize,
    pub hook_band_y: [f64; 2],
    pub ig_top_px: f64,
    pub ig_right: IgRight,
    pub cue_words_max: usize,
    pub cue4_chars_max: usize,
    pub cue4_s_max: f64,
    pub cue_frames_min: u64,
    pub caption_coverage_min: f64,
    pub evidence_frames_min: usize,
    pub cam_iou_min: f64,
    pub cam_place_tol: f64,
    pub cam_share_min: f64,
    pub screen_band_y: [f64; 2],
    pub screen_share_min: f64,
    pub layouts_max: usize,
    pub content_recall_report_gate: f64,
    pub loudness_tol_lu: f64,
    pub true_peak_max_dbtp: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IgRight {
    pub w: f64,
    pub y: [f64; 2],
}

/// One word with times (seconds).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Words {
    pub words: Vec<Word>,
}

/// A provenance as the project records it (SPEC R-AI-4).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Provenance {
    /// `measurement` | `transcript` | `ocr` | `human` | `import`.
    pub kind: String,
    /// The `MeasurementId` for `measurement`.
    #[serde(default)]
    pub id: Option<String>,
}

/// A part edge's anchor (SPEC §4.1.3; R-AI-1).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EdgeFact {
    /// `start` | `end`.
    pub side: String,
    /// `raw` | `word` | `measured` | `cut_point` | `import` | …
    pub time_provenance: String,
    /// `word` | `line` (R-AI-1 `edges`).
    #[serde(default)]
    pub edges: Option<String>,
    /// The `measure{edge}` that resolved a line edge.
    #[serde(default)]
    pub measure_id: Option<String>,
    #[serde(default)]
    pub offset_ms: Option<f64>,
    /// `agent` | `human` | `import`.
    #[serde(default)]
    pub author: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PartFact {
    /// The RenderPlan part id.
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub edges: Vec<EdgeFact>,
}

/// A text overlay / pill / counter (matched to a RenderPlan text event by id).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OverlayFact {
    pub id: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub number_provenance: Option<Provenance>,
    #[serde(default)]
    pub time_provenance: Option<String>,
    #[serde(default)]
    pub offset_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NoCaption {
    /// Output frames `[start, end)`.
    pub out: [u64; 2],
    pub time_provenance: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CaptionDoc {
    pub enabled: bool,
    pub source: String,
    pub grouping: String,
    #[serde(default)]
    pub no_caption: Vec<NoCaption>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct GainSegment {
    pub id: String,
    pub time_provenance: String,
    #[serde(default)]
    pub measurement_ref: Option<String>,
    #[serde(default)]
    pub author: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TitleFact {
    pub text: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub number_provenance: Option<Provenance>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Lint {
    pub level: String,
    pub code: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CutFacts {
    pub id: String,
    /// The export framing this eval grades (`9:16`).
    pub framing: String,
    #[serde(default)]
    pub title: Option<TitleFact>,
    pub parts: Vec<PartFact>,
    #[serde(default)]
    pub overlays: Vec<OverlayFact>,
    #[serde(default)]
    pub captions: Option<CaptionDoc>,
    #[serde(default)]
    pub gain_segments: Vec<GainSegment>,
    #[serde(default)]
    pub current_render_sig: Option<String>,
}

/// `{source, frame_index}` (a presentation index, SPEC §11.2 `measure`).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EvidenceFrame {
    pub source: String,
    pub frame_index: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Measurement {
    pub id: String,
    /// `interval` | `edge` | `loudness`.
    pub kind: String,
    /// Master-clock seconds.
    #[serde(default)]
    pub start_s: Option<f64>,
    #[serde(default)]
    pub stop_s: Option<f64>,
    #[serde(default)]
    pub evidence: Vec<EvidenceFrame>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Verify {
    pub ok: bool,
    /// The render_sig the verify ran on.
    #[serde(default)]
    pub render_sig: Option<String>,
    /// `verify{joins}`: how many joins it checked.
    #[serde(default)]
    pub joins_checked: Option<usize>,
    #[serde(default)]
    pub failures: Vec<Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Loudness {
    pub mode: String,
    #[serde(default)]
    pub target_lufs: Option<f64>,
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
}

/// An export record (SPEC §5.14).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExportFact {
    pub id: String,
    pub framing: String,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub created_by_run: Option<String>,
    pub render_sig: String,
    /// sha256 of the RenderPlan JSON bytes the export rendered (= the bundle's plan.json).
    pub plan_sha256: String,
    #[serde(default)]
    pub verify_export: Option<Verify>,
    #[serde(default)]
    pub verify_joins: Option<Verify>,
    #[serde(default)]
    pub lints_at_export: Vec<Lint>,
    #[serde(default)]
    pub loudness: Option<Loudness>,
    /// G8(b): the record's cue list equals the reference `story_v5` run over its heard
    /// words and CaptionDoc. `None` = not computed (pending the runner hook to B-08).
    #[serde(default)]
    pub cues_equal_reference: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Project {
    pub schema: String,
    /// The launcher run that produced the bundle (`None` for reference bundles).
    #[serde(default)]
    pub run_id: Option<String>,
    /// Absolute path of the fixture home the run was given (G0's path check).
    #[serde(default)]
    pub fixture_root: Option<String>,
    pub cut: CutFacts,
    #[serde(default)]
    pub measurements: Vec<Measurement>,
    #[serde(default)]
    pub exports: Vec<ExportFact>,
}

/// Whether the bundle is a run (G0 and G7 required) or a reference cut (self-tests).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Run,
    Reference,
}

/// One loaded bundle.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub plan: Value,
    pub plan_sha256: String,
    pub project: Project,
    pub transcript: Option<String>,
    pub run: Option<Value>,
    pub asr: Option<Vec<Word>>,
    pub mode: Mode,
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T, String> {
    let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", p.display()))
}

impl Bundle {
    pub fn load(dir: &Path, mode: Mode) -> Result<Self, String> {
        let plan_bytes =
            std::fs::read(dir.join("plan.json")).map_err(|e| format!("plan.json: {e}"))?;
        let plan: Value =
            serde_json::from_slice(&plan_bytes).map_err(|e| format!("plan.json: {e}"))?;
        let project: Project = read_json(&dir.join("project.json"))?;
        let opt = |name: &str| dir.join(name).exists().then(|| dir.join(name));
        let transcript = opt("transcript.jsonl").and_then(|p| std::fs::read_to_string(p).ok());
        let run = match opt("run.json") {
            Some(p) => Some(read_json(&p)?),
            None => None,
        };
        let asr = match opt("asr.json") {
            Some(p) => Some(read_json::<Words>(&p)?.words),
            None => None,
        };
        Ok(Self {
            plan,
            plan_sha256: gmclip_ai::launcher::sha256::hex(&plan_bytes),
            project,
            transcript,
            run,
            asr,
            mode,
        })
    }
}

/// `evals/h79` (the directory holding truth.json).
pub fn h79_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/h79")
}

pub fn load_truth(dir: &Path) -> Result<Truth, String> {
    read_json(&dir.join("truth.json"))
}

pub fn load_words(path: &Path) -> Result<Vec<Word>, String> {
    Ok(read_json::<Words>(path)?.words)
}
