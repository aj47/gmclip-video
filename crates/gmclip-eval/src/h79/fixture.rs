//! `gmclip-eval fixture check`: the h79 fixture is what its manifest says, and does not
//! leak the answer (SPEC §17.1.1 "Task hygiene"; backlog E01).
//!
//! - every file listed in `evals/h79/fixture/manifest.json` (plus `style.md` and
//!   `brief.md`) has its recorded sha256, and no unlisted file sits in the fixture;
//! - a grep of the fixture, the style file and the brief for the truth's forbidden strings
//!   (`"67"`, `"1:07"`, `"2610.8"`, `"2678.2"`, `"My PC made"`) finds nothing.
//!   Calibrated: a needle that starts with a digit matches only as its own number (not
//!   inside `h67`, `2671.5`, `0.67` or the timestamp `00:41:07`); the others match
//!   case-insensitively anywhere;
//! - the excluded inputs are absent: no story record for h79 and no path into h79's own
//!   production folder (`production/h79…`, its render spec). Sibling cuts' story records
//!   keep their own `production/` paths: they are other cuts' material, not the answer.

use std::path::Path;

use serde_json::Value;

/// One problem found.
pub type Finding = String;

/// Occurrences of `needle` in `hay` under the calibrated rule above.
pub fn forbidden_hits(hay: &str, needle: &str) -> usize {
    if needle.starts_with(|c: char| c.is_ascii_digit()) {
        let b = hay.as_bytes();
        let mut n = 0;
        let mut i = 0;
        while let Some(off) = hay[i..].find(needle) {
            let at = i + off;
            let end = at + needle.len();
            let before = at.checked_sub(1).map(|k| b[k]);
            let after = b.get(end).copied();
            // inside a longer number, time or id it is not this number ("h67", "167",
            // "0.67", "41:07"); "67.", "67.4", "67s" and "1:07" are
            let joined_before = before
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':'));
            // a decimal needle ("2610.8") also matches its longer spellings ("2610.833")
            let integer = needle.bytes().all(|c| c.is_ascii_digit());
            let joined_after = integer && after.is_some_and(|c| c.is_ascii_digit());
            if !joined_before && !joined_after {
                n += 1;
            }
            i = end;
        }
        n
    } else {
        hay.to_lowercase().matches(&needle.to_lowercase()).count()
    }
}

/// Checks the fixture under `h79_dir` (= `evals/h79`) against `forbidden`.
pub fn check(h79_dir: &Path, forbidden: &[String]) -> Result<Vec<Finding>, String> {
    let fix = h79_dir.join("fixture");
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(fix.join("manifest.json")).map_err(|e| format!("manifest: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let mut findings = Vec::new();
    let mut listed = std::collections::BTreeSet::new();
    for f in manifest["files"].as_array().into_iter().flatten() {
        let rel = f["path"].as_str().unwrap_or("");
        let p = fix.join(rel);
        listed.insert(std::fs::canonicalize(&p).unwrap_or(p.clone()));
        match std::fs::read(&p) {
            Err(e) => findings.push(format!("{rel}: {e}")),
            Ok(b) => {
                let got = gmclip_ai::launcher::sha256::hex(&b);
                if Some(got.as_str()) != f["sha256"].as_str() {
                    findings.push(format!("{rel}: sha256 {got} ≠ manifest"));
                }
                let text = String::from_utf8_lossy(&b);
                for needle in forbidden {
                    let n = forbidden_hits(&text, needle);
                    if n > 0 {
                        findings.push(format!("{rel}: {n}× forbidden {needle:?}"));
                    }
                }
                if text.contains("production/h79") || text.contains("render-spec-v4") {
                    findings.push(format!("{rel}: names h79's production files"));
                }
            }
        }
    }
    for e in std::fs::read_dir(&fix)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let p = std::fs::canonicalize(e.path()).unwrap_or(e.path());
        if p.file_name().is_some_and(|n| n == "manifest.json") {
            continue;
        }
        if !listed.contains(&p) {
            findings.push(format!("{}: not in the manifest", p.display()));
        }
    }
    if let Ok(cuts) = std::fs::read(fix.join("cuts.json")) {
        let v: Value = serde_json::from_slice(&cuts).map_err(|e| e.to_string())?;
        if v["cuts"].get("loc_4fadb756_h79").is_some() {
            findings.push("cuts.json carries h79's own story".into());
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_tokens() {
        assert_eq!(forbidden_hits("in 67 seconds.", "67"), 1);
        assert_eq!(forbidden_hits("ends 67.", "67"), 1);
        assert_eq!(
            forbidden_hits("loc_4fadb756_h67 2671.5 0.67 167 670", "67"),
            0
        );
        assert_eq!(forbidden_hits("67.4 s", "67"), 1);
        assert_eq!(forbidden_hits("in 67s", "67"), 1);
        assert_eq!(
            forbidden_hits("[00:41:07.5 - 00:41:09.0] DONE IN 1:07", "1:07"),
            1
        );
        assert_eq!(forbidden_hits("[67]", "67"), 1);
        assert_eq!(forbidden_hits("My PC made this", "my pc made"), 1);
        assert_eq!(forbidden_hits("t=2610.833", "2610.8"), 1);
    }

    /// The committed fixture passes its own check.
    #[test]
    fn the_fixture_is_clean_and_as_listed() {
        let dir = super::super::bundle::h79_dir();
        let truth = super::super::bundle::load_truth(&dir).unwrap();
        let f = check(&dir, &truth.forbidden_fixture_strings).unwrap();
        assert!(f.is_empty(), "{f:#?}");
    }
}
