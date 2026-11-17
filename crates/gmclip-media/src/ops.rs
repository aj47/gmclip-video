//! Whole-file media operations the CLI delegates to (`gmclip probe`, `gmclip remux`;
//! A-01). Lane B fills them (B-01/B-02 probe, B-06 remux); until then each returns
//! [`OpError::Unsupported`] naming its ticket, so the CLI exits cleanly with a JSON error.

use std::fmt;
use std::path::Path;

use crate::MediaError;

/// Why a media operation failed (A-02 shell; SPEC R-CMD-6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpError {
    /// A pipeline step failed: `{step, frame?, source?, os_status?}`.
    Media(MediaError),
    /// The input is malformed (a bad box, a parser budget exceeded; R-MEDIA-SEC-2).
    /// `reason` never quotes more than 16 bytes of the input.
    Invalid { step: &'static str, reason: String },
    /// Not built yet, or not supported for this input or platform.
    Unsupported { step: &'static str, reason: String },
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Media(e) => e.fmt(f),
            Self::Invalid { step, reason } => write!(f, "invalid input at {step}: {reason}"),
            Self::Unsupported { step, reason } => write!(f, "unsupported at {step}: {reason}"),
        }
    }
}

impl std::error::Error for OpError {}

impl From<MediaError> for OpError {
    fn from(e: MediaError) -> Self {
        Self::Media(e)
    }
}

/// What `probe` found: tracks, index facts and the capability verdict (B-01, B-02).
/// A shell until then.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ProbeReport {}

impl ProbeReport {
    /// The report as one JSON object, for `gmclip probe`'s stdout.
    pub fn to_json(&self) -> String {
        "{}".to_owned()
    }
}

/// Reads a source's container and index (B-01 MP4/MOV, B-02 MKV/WebM).
pub fn probe(_file: &Path) -> Result<ProbeReport, OpError> {
    Err(OpError::Unsupported {
        step: "probe",
        reason: "not built yet (B-01, B-02)".into(),
    })
}

/// What `remux` did, for `gmclip remux`'s stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemuxSummary(pub crate::mux::remux::RemuxReport);

impl RemuxSummary {
    /// One JSON object (no input bytes, no paths).
    pub fn to_json(&self) -> serde_json::Value {
        let r = &self.0;
        serde_json::json!({
            "video_samples": r.video_samples,
            "audio_samples": r.audio_samples,
            "video_shift_ticks": r.video_shift,
            "audio_edit": r.audio_edit.map(|e| serde_json::json!({ "media_time": e.media_time, "presented": e.presented })),
            "gapless": r.gapless.map(|g| serde_json::json!({ "priming": g.priming, "padding": g.padding, "length": g.length })),
            "offsets": format!("{:?}", r.mux.offsets).to_lowercase(),
            "moov_bytes": r.mux.moov_size,
            "mdat_bytes": r.mux.mdat_size,
        })
    }
}

/// Re-wraps an H.264 + AAC MP4 sample for sample through our parser and writer (B-06).
pub fn remux(input: &Path, out: &Path) -> Result<RemuxSummary, OpError> {
    use crate::mux::remux::RemuxError;
    let media = |step: &'static str, e: &std::io::Error| MediaError {
        step,
        frame: None,
        source_id: None,
        os_status: e.raw_os_error().map(i64::from),
    };
    crate::mux::remux::remux(input, out)
        .map(RemuxSummary)
        .map_err(|e| match e {
            RemuxError::Read(io) => OpError::Media(media("demux", &io)),
            RemuxError::Input(reason) => OpError::Invalid {
                step: "demux",
                reason,
            },
            RemuxError::Write(io) => OpError::Media(media("mux", &io)),
            RemuxError::Mux(e) => OpError::Invalid {
                step: "mux",
                reason: e.to_string(),
            },
        })
}
