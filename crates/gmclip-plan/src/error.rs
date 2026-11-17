//! `PlanError`: why a RenderPlan was refused (SPEC R-CMD-6 `Invalid{path, reason}`,
//! `Unsupported{…, reason}`).
//!
//! `path` is an RFC 6901 JSON Pointer into the plan (`""` is the whole document). A
//! `reason` never quotes a value from the plan, and the keys in `path` that are not
//! schema property names (unknown keys, `text_styles` names) are cut so one path echoes
//! at most 16 caller bytes (R-CMD-6), with control characters replaced by `?`.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Serialize;

/// The most caller bytes one error's path may echo (R-CMD-6).
pub const MAX_ECHO_BYTES: usize = 16;

/// Why a plan cannot be rendered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(tag = "code")]
pub enum PlanError {
    /// The plan breaks the schema or an RP §2.1 rule.
    #[error("invalid plan at {}: {reason}", display_path(path))]
    Invalid { path: String, reason: String },
    /// The plan is well formed but asks for something v0 does not render (RP §2.6,
    /// rule 3's CFR/fps condition, a non-48 kHz audio source).
    #[error("unsupported at {}: {reason}", display_path(path))]
    Unsupported { path: String, reason: String },
}

impl PlanError {
    pub fn invalid(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn unsupported(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// The JSON Pointer the error names.
    pub fn path(&self) -> &str {
        match self {
            Self::Invalid { path, .. } | Self::Unsupported { path, .. } => path,
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Invalid { reason, .. } | Self::Unsupported { reason, .. } => reason,
        }
    }

    /// The stable error code (`"Invalid"` or `"Unsupported"`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid { .. } => "Invalid",
            Self::Unsupported { .. } => "Unsupported",
        }
    }
}

fn display_path(p: &str) -> &str {
    if p.is_empty() { "/" } else { p }
}

/// Every property name the committed schema defines. A path segment equal to one of
/// these is schema vocabulary, not caller input, and does not count against the echo
/// budget.
fn known_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        fn walk(v: &serde_json::Value, out: &mut HashSet<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    if let Some(serde_json::Value::Object(props)) = m.get("properties") {
                        out.extend(props.keys().cloned());
                    }
                    m.values().for_each(|x| walk(x, out));
                }
                serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut out = HashSet::new();
        walk(crate::schema::schema(), &mut out);
        out
    })
}

fn unescape(seg: &str) -> String {
    seg.replace("~1", "/").replace("~0", "~")
}

/// Caller bytes already echoed in `path`: the segments that are neither schema names
/// nor array indices.
fn echoed(path: &str) -> usize {
    path.split('/')
        .skip(1)
        .map(unescape)
        .filter(|s| !known_names().contains(s) && s.parse::<usize>().is_err())
        .map(|s| s.trim_end_matches('…').len())
        .sum()
}

/// `base` + `/` + an object key, as an RFC 6901 segment (`~` → `~0`, `/` → `~1`).
///
/// A key that is not a schema property name is caller input: control characters become
/// `?`, and it is cut (at a char boundary, marked `…`) so the whole path echoes at most
/// [`MAX_ECHO_BYTES`] caller bytes (R-CMD-6).
pub fn join_key(base: &str, key: &str) -> String {
    let seg = if known_names().contains(key) {
        key.to_owned()
    } else {
        let clean: String = key
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        let budget = MAX_ECHO_BYTES.saturating_sub(echoed(base));
        if clean.len() > budget {
            let mut end = budget;
            while !clean.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &clean[..end])
        } else {
            clean
        }
    };
    format!("{base}/{}", seg.replace('~', "~0").replace('/', "~1"))
}

/// `base` + `/` + an array index.
pub fn join_index(base: &str, i: usize) -> String {
    format!("{base}/{i}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_escaped_and_cut() {
        assert_eq!(join_key("", "a/b~c"), "/a~1b~0c");
        assert_eq!(
            join_key("/audio/envelopes/0", "ramp_out_samples"),
            "/audio/envelopes/0/ramp_out_samples"
        );
        assert_eq!(
            join_key("/meta", "seventeen_bytes_x"),
            "/meta/seventeen_bytes_…"
        );
        // a 2-byte char straddling byte 16 is dropped whole
        assert_eq!(join_key("", "aaaaaaaaaaaaaaaé"), "/aaaaaaaaaaaaaaa…");
        assert_eq!(join_key("", "a\u{1b}[31m\n"), "/a?[31m?");
    }

    #[test]
    fn a_path_echoes_at_most_16_caller_bytes_in_all() {
        let style = "S".repeat(96);
        let base = join_key("/text_styles", &style);
        assert_eq!(base, format!("/text_styles/{}…", "S".repeat(16)));
        assert_eq!(join_key(&base, "bogus_key"), format!("{base}/…"));
        // schema names after it are still spelled out
        assert_eq!(join_key(&base, "font"), format!("{base}/font"));
        let short = join_key("/text_styles", "Hook");
        assert_eq!(join_key(&short, "max_width"), "/text_styles/Hook/max_width");
        assert_eq!(
            join_key(&short, "a_very_long_unknown"),
            "/text_styles/Hook/a_very_long_…"
        );
    }

    #[test]
    fn serialises_with_a_code_tag() {
        let e = PlanError::invalid("/parts/0/out", "start must be < end");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "Invalid", "path": "/parts/0/out", "reason": "start must be < end"})
        );
    }
}
