//! Whole-plan operations the CLI delegates to (`gmclip render`, `still`; A-01), and their
//! error type (A-02 shell): H-10 render, H-05b still; where one is not built it returns
//! [`RenderError::Unsupported`] naming its ticket, so the CLI exits cleanly with a JSON
//! error.

use std::fmt;
use std::path::Path;

use gmclip_media::MediaError;
use gmclip_plan::{Plan, PlanError};

/// Why a render-side operation failed (SPEC R-CMD-6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderError {
    /// The plan was refused (`Invalid{path, reason}` / `Unsupported`).
    Plan(PlanError),
    /// A media step failed: `{step, frame?, source?, os_status?}`.
    Media(MediaError),
    /// A render step failed at an output frame. `reason` never carries content
    /// (caption text, paths under `$HOME`).
    Failed {
        step: &'static str,
        frame: Option<u64>,
        reason: String,
    },
    /// Not built yet, or not supported for this plan or platform.
    Unsupported { step: &'static str, reason: String },
    /// An input other than a RenderPlan (an golden cut) is malformed. `reason` never
    /// quotes more than 16 bytes of it.
    Invalid { step: &'static str, reason: String },
    /// A file could not be read or written; `what` names which input (never its path).
    Io {
        step: &'static str,
        what: &'static str,
        kind: std::io::ErrorKind,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(e) => e.fmt(f),
            Self::Media(e) => e.fmt(f),
            Self::Failed {
                step,
                frame: Some(n),
                reason,
            } => write!(f, "{step} failed at frame {n}: {reason}"),
            Self::Failed { step, reason, .. } => write!(f, "{step} failed: {reason}"),
            Self::Unsupported { step, reason } => write!(f, "unsupported at {step}: {reason}"),
            Self::Invalid { step, reason } => write!(f, "invalid input at {step}: {reason}"),
            Self::Io { step, what, kind } => write!(f, "{step}: {what}: {kind}"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<PlanError> for RenderError {
    fn from(e: PlanError) -> Self {
        Self::Plan(e)
    }
}

impl From<MediaError> for RenderError {
    fn from(e: MediaError) -> Self {
        Self::Media(e)
    }
}

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn not_built(step: &'static str, ticket: &str) -> RenderError {
    RenderError::Unsupported {
        step,
        reason: format!("not built yet ({ticket})"),
    }
}

/// What a finished render wrote.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct RenderSummary {
    pub frames: u64,
}

/// Renders a validated plan to an MP4 (H-10): sources opened through the demuxers
/// (B-01/B-02, relative paths against the working directory), the export pipeline,
/// verify (B-15) and the export record (B-16).
pub fn render(plan: &Plan, out: &Path) -> Result<RenderSummary, RenderError> {
    let mut inputs = crate::export::DemuxInputs::new(None);
    let rep = crate::export::export(
        plan,
        &mut inputs,
        out,
        &crate::export::ExportOptions::default(),
    )?;
    Ok(RenderSummary { frames: rep.frames })
}

/// Renders output frames `at` of a validated plan as one contact sheet PNG at `out`
/// (H-05b; IG overlay zones tinted).
#[cfg(any(target_os = "macos", windows))]
pub fn still(plan: &Plan, at: &[u64], out: &Path) -> Result<(), RenderError> {
    use crate::export::sheet::{SheetCell, contact_sheet, write_png};
    if let Some(&f) = at.iter().find(|&&f| f >= plan.output.frames) {
        return Err(RenderError::Invalid {
            step: "still",
            reason: format!("frame {f} is past the plan's {} frames", plan.output.frames),
        });
    }
    let mut inputs = crate::export::DemuxInputs::new(None);
    let frames = crate::export::render_frames(plan, &mut inputs, at)?;
    let cells: Vec<SheetCell> = frames
        .into_iter()
        .map(|(frame, ours)| SheetCell {
            frame,
            ours,
            reference: None,
        })
        .collect();
    let sheet = contact_sheet(&cells, 4.min(cells.len().max(1)), 4);
    write_png(&sheet, out).map_err(|e| RenderError::Io {
        step: "still",
        what: "contact sheet",
        kind: e.kind(),
    })
}

/// Renders output frames (macOS and Windows; Linux M6).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn still(_plan: &Plan, _at: &[u64], _out: &Path) -> Result<(), RenderError> {
    Err(not_built("still", "macOS and Windows only"))
}
