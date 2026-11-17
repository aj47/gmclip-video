//! Transcripts (SPEC §4.4 `analysis`): an ASR revision's words and its line table, each
//! line's word range being the words whose midpoints fall inside the line's times; the
//! `words` / `lines` analysis blobs they are stored as.

use gmclip_model::time::FLICKS_PER_SECOND;
use serde::{Deserialize, Serialize};

/// One transcript word. Times are source flicks on the media's own clock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: i64,
    pub end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// One transcript line and its word range `[w0, w1)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub start: i64,
    pub end: i64,
    pub text: String,
    pub words: [u32; 2],
}

/// One ASR revision of a source's (or sync group's) transcript.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    /// The ASR revision the words and lines belong to.
    pub revision: String,
    pub words: Vec<Word>,
    pub lines: Vec<Line>,
}

impl Transcript {
    /// The line starting at `t` (within 1 ms).
    pub fn line_starting(&self, t: i64) -> Option<u32> {
        let tol = FLICKS_PER_SECOND / 1000;
        self.lines
            .iter()
            .position(|l| (l.start - t).abs() <= tol)
            .map(|i| i as u32)
    }

    /// The line ending at `t` (within 1 ms).
    pub fn line_ending(&self, t: i64) -> Option<u32> {
        let tol = FLICKS_PER_SECOND / 1000;
        self.lines
            .iter()
            .position(|l| (l.end - t).abs() <= tol)
            .map(|i| i as u32)
    }

    /// The line whose times hold `t`, else the first line starting within 5 s after it.
    pub fn line_at(&self, t: i64) -> Option<u32> {
        if let Some(i) = self.lines.iter().position(|l| l.start <= t && t <= l.end) {
            return Some(i as u32);
        }
        self.lines
            .iter()
            .position(|l| l.start > t && l.start - t <= 5 * FLICKS_PER_SECOND)
            .map(|i| i as u32)
    }

    pub fn words_body(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": "gmclip.analysis.words/0",
            "revision": self.revision,
            "untrusted": true,
            "words": self.words,
        }))
        .unwrap_or_default()
    }

    pub fn lines_body(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": "gmclip.analysis.lines/0",
            "revision": self.revision,
            "untrusted": true,
            "lines": self.lines,
        }))
        .unwrap_or_default()
    }
}
