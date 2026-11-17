//! Shared helpers: repo paths and the `json` blocks of `spec/renderplan-v0.md`.
#![allow(dead_code)]

use std::path::PathBuf;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A repo file as text, with CRLF normalised (a Windows checkout may convert).
pub fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .replace("\r\n", "\n")
}

/// The ```json block that follows the heading starting with `heading` in RP.
pub fn spec_block(heading: &str) -> String {
    let md = read("spec/renderplan-v0.md");
    let at = md
        .find(&format!("\n{heading}"))
        .unwrap_or_else(|| panic!("no heading {heading}"));
    let rest = &md[at..];
    let start = rest.find("```json\n").expect("json block") + "```json\n".len();
    let len = rest[start..].find("```").expect("closing fence");
    rest[start..start + len].to_owned()
}

/// The RP §2.3 example (h79 v4 parts 1–3).
pub fn h79() -> serde_json::Value {
    serde_json::from_str(&spec_block("### 2.3")).unwrap()
}

/// Validates a JSON value with the full pipeline.
pub fn validate(v: &serde_json::Value) -> Result<gmclip_plan::Plan, gmclip_plan::PlanError> {
    gmclip_plan::parse(serde_json::to_string(v).unwrap().as_bytes())
}
