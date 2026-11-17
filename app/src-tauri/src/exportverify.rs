//! `verify{export}` in the app (H.06; SPEC R-EXP-6): the result of the last verify of
//! each export, kept on its entry in `<home>/exports.json`, and a re-verify on demand.
//!
//! - **At export** the renderer verifies the written bytes before it installs them (B-15:
//!   container facts, then the audio as the OS decodes it against the encoded mix); a
//!   failing check fails the export, so a recorded export starts green with the
//!   renderer's own summary line.
//! - **Re-verify** reads the file again: it must still be a regular file (never a
//!   symlink), its SHA-256 must match its export record, and — when the cut still plans
//!   to what was exported (same `render_sig`, or the same plan digest when the source has
//!   no content hash), in its framing or at an upload preset — every container check
//!   runs against that plan. A cut changed since is said, not guessed at: the file and its
//!   record are checked and the rest is listed as not run. The audio checks need the
//!   encoded mix, which only the export has; they are listed as not run too.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{AppCore, ExportEntry, now_ms};
use crate::error::{AppError, AppResult};

/// One check of a verify.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

/// A verify of one export file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verify {
    pub passed: bool,
    pub at_ms: i64,
    /// `export` (the renderer's, before the file was installed) or `reverify`.
    pub how: String,
    /// One line for the cut's export popover.
    pub summary: String,
    #[serde(default)]
    pub checks: Vec<Check>,
    /// What this verify did not check, and why.
    #[serde(default)]
    pub not_run: Vec<String>,
}

/// The audio checks a re-verify cannot run.
pub const AUDIO_NOT_RUN: &str = "audio alignment, per-join A/V offset and loudness: checked at export against the encoded mix (a re-verify has only the file)";

/// The verify an export carries from the renderer: its "verified: …" note, if any.
pub fn at_export(notes: &[String], at_ms: i64) -> Option<Verify> {
    notes
        .iter()
        .find(|n| n.starts_with("verified: "))
        .map(|n| Verify {
            passed: true,
            at_ms,
            how: "export".into(),
            summary: n.clone(),
            checks: Vec::new(),
            not_run: Vec::new(),
        })
}

fn check(step: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        step: step.into(),
        ok,
        detail: detail.into(),
    }
}

/// Reads a regular file (refusing a symlink or anything else at the path).
fn read_regular(p: &Path) -> Result<Vec<u8>, String> {
    let m = std::fs::symlink_metadata(p).map_err(|e| format!("not found ({})", e.kind()))?;
    if m.file_type().is_symlink() {
        return Err("a symlink is at the export's path".into());
    }
    if !m.is_file() {
        return Err("not a regular file".into());
    }
    let mut f = std::fs::File::open(p).map_err(|e| format!("cannot be read ({})", e.kind()))?;
    let mut b = Vec::with_capacity(usize::try_from(m.len()).unwrap_or(0));
    f.read_to_end(&mut b)
        .map_err(|e| format!("cannot be read ({})", e.kind()))?;
    Ok(b)
}

/// The plan this export was made from, if the cut still plans to it (its framing, else
/// one of its upload presets).
fn export_plan(core: &AppCore, project: &str, e: &ExportEntry) -> Option<gmclip_plan::Plan> {
    let same = |plan: &gmclip_plan::Plan| match (&e.render_sig, crate::core::sig_of(plan)) {
        (Some(a), Some(b)) => *a == b,
        (None, None) => e.plan_sig.is_some() && e.plan_sig == crate::core::plan_sig(plan),
        _ => false,
    };
    if let Ok(pl) = core.planned(project, &e.cut)
        && same(&pl.plan)
    {
        return Some((*pl.plan).clone());
    }
    core.upload_presets(project, &e.cut).iter().find_map(|p| {
        core.planned_preset(project, &e.cut, p)
            .ok()
            .filter(|pl| same(&pl.plan))
            .map(|pl| (*pl.plan).clone())
    })
}

/// Re-verifies one file against its entry (no state written).
pub fn verify_file(core: &AppCore, project: &str, e: &ExportEntry) -> Verify {
    let mut checks = Vec::new();
    let mut not_run = Vec::new();
    let bytes = match read_regular(&e.file) {
        Ok(b) => {
            checks.push(check(
                "file",
                true,
                format!("{} bytes at {}", b.len(), e.file.display()),
            ));
            Some(b)
        }
        Err(why) => {
            checks.push(check("file", false, why));
            None
        }
    };
    if let Some(b) = &bytes {
        match gmclip_media::export_record::ExportRecord::read(&e.file) {
            Ok(r) => {
                let got = gmclip_ai::launcher::sha256::hex(b);
                checks.push(if got.eq_ignore_ascii_case(&r.sha256) {
                    check("sha256", true, "matches the export record")
                } else {
                    check(
                        "sha256",
                        false,
                        "the file changed after export (its SHA-256 differs from its record)",
                    )
                });
            }
            Err(_) => checks.push(check(
                "sha256",
                false,
                "no readable export record beside the file",
            )),
        }
        match export_plan(core, project, e) {
            Some(plan) => {
                let expect = gmclip_media::verify::Expect::from_plan(&plan);
                for c in gmclip_media::verify::container(b, &expect) {
                    checks.push(check(c.step, c.ok, c.detail));
                }
            }
            None => not_run.push(
                "container checks against the plan: the cut changed after this export (re-export to verify the cut as it is)".into(),
            ),
        }
        not_run.push(AUDIO_NOT_RUN.into());
    }
    let passed = checks.iter().all(|c| c.ok);
    let summary = match checks.iter().find(|c| !c.ok) {
        Some(c) => format!("failed at {}: {}", c.step, c.detail),
        None => format!("{} checks passed", checks.len()),
    };
    Verify {
        passed,
        at_ms: now_ms(),
        how: "reverify".into(),
        summary,
        checks,
        not_run,
    }
}

/// `export_verify {export}`: re-verifies an export, records the result on its entry and
/// returns it.
pub fn reverify(core: &AppCore, project: &str, export: &str) -> AppResult<Value> {
    let e = core
        .exports(project)?
        .into_iter()
        .find(|e| e.id == export)
        .ok_or_else(|| AppError::not_found("no such export"))?;
    let v = verify_file(core, project, &e);
    core.set_export_verify(project, export, v.clone())?;
    core.bus.project(
        project,
        crate::events::ProjectEvent::ExportVerify {
            cut: e.cut.clone(),
            export: export.to_owned(),
        },
    );
    Ok(serde_json::to_value(v).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_export_note_is_the_first_verify() {
        let notes = vec![
            "loudness: −16 LUFS".to_owned(),
            "verified: 14 checks passed (B-15 verify v0); 3 discontinuous joins, worst A/V offset 0.02 ms, drift None ms/join".to_owned(),
        ];
        let v = at_export(&notes, 7).unwrap();
        assert!(v.passed);
        assert_eq!(v.how, "export");
        assert!(v.summary.starts_with("verified: 14 checks passed"));
        assert_eq!(v.at_ms, 7);
        assert!(at_export(&["not verified: …".to_owned()], 7).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_or_a_folder_is_not_read() {
        let d = std::env::temp_dir().join(format!("gmclip-xv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.mp4");
        std::fs::write(&f, b"abc").unwrap();
        let l = d.join("l.mp4");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        assert_eq!(read_regular(&f).unwrap(), b"abc");
        assert!(read_regular(&l).unwrap_err().contains("symlink"));
        assert!(read_regular(&d).unwrap_err().contains("regular"));
        assert!(
            read_regular(&d.join("none"))
                .unwrap_err()
                .contains("not found")
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
