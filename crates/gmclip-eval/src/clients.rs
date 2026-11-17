//! `evals/h79/clients.toml`: client pins and the launcher's tested ranges. A tiny reader
//! for the file's subset of TOML (sections, `key = "string"`, comments).

use std::collections::BTreeMap;

use gmclip_ai::launcher::{Harness, version};

/// `section -> key -> value` (top-level keys under `""`).
pub fn parse(text: &str) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_owned();
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = \"value\"", n + 1))?;
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| format!("line {}: values are double-quoted strings", n + 1))?;
        out.entry(section.clone())
            .or_default()
            .insert(k.trim().to_owned(), v.to_owned());
    }
    Ok(out)
}

/// The launcher's range for `h` in the file's `">=a.b.c, <d.e.f"` form.
pub fn range_text(h: Harness) -> String {
    match version::supported(h) {
        Some(r) => format!(">={}, <{}", r.min, r.max_exclusive),
        None => "none".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> BTreeMap<String, BTreeMap<String, String>> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../evals/h79/clients.toml");
        parse(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// The launcher runs exactly what clients.toml says passed S7 or was accepted with
    /// its findings (Codex 0.160, decision 0012 amendment 2026-10-16).
    #[test]
    fn clients_toml_matches_the_launcher() {
        let f = file();
        for h in [Harness::ClaudeCode, Harness::Codex] {
            let s = &f[h.as_str()];
            assert_eq!(s["versions"], range_text(h), "{h:?}");
            let tested = version::parse(&s["tested"]).unwrap();
            let gate = version::check(h, Some(tested));
            let runs = s["s7"] == "pass" || s.contains_key("accepted");
            assert_eq!(
                gate == version::Verdict::Supported,
                runs,
                "{h:?}: s7 = {:?}, gate {gate:?}",
                s["s7"]
            );
        }
        assert!(f["codex"]["accepted"].contains("dont worry about codex safety"));
        assert_eq!(f[""]["wall_clock"], "90m");
        assert_eq!(f["claude-code"]["model"], "claude-opus-5-5");
    }
}
