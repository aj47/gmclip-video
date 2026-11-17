//! `cargo xt supply`: no `build.rs` or proc-macro crate new to the tree (SR R-SUP-1).
//!
//! Third-party crates that run code at build time (a `custom-build` target, or a
//! `proc-macro` crate) are listed in `supply-chain/build-and-proc-macro.txt`, one
//! `<crate> <kind>` per line. A crate in the resolved graph (every target, every
//! feature) that is not on the list fails the check; adding a line is AJ's decision
//! after an audit (R-SUP-2; the file is CODEOWNERS-protected).
//!
//! Not covered here (W0-11, still open): "a crate published < 30 days ago" needs the
//! crates.io API and is not checked yet.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Resolved `(crate, kind)` pairs that run code at build time, excluding workspace members.
fn build_time_crates(manifest: &Path) -> Result<BTreeSet<String>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--all-features",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed for {}:\n{}",
            manifest.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))?;
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut found = BTreeSet::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        if members.contains(p["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let name = p["name"].as_str().unwrap_or_default();
        for t in p["targets"].as_array().into_iter().flatten() {
            for k in t["kind"].as_array().into_iter().flatten() {
                if let Some(k @ ("custom-build" | "proc-macro")) = k.as_str() {
                    found.insert(format!("{name} {k}"));
                }
            }
        }
    }
    Ok(found)
}

fn baseline(root: &Path) -> Result<BTreeSet<String>, String> {
    let path = root.join("supply-chain/build-and-proc-macro.txt");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect())
}

/// Checks the workspace at `manifest` against the repo's baseline.
pub fn check(root: &Path, manifest: &Path) -> Result<String, String> {
    let found = build_time_crates(manifest)?;
    let allowed = baseline(root)?;
    let new: Vec<&String> = found.difference(&allowed).collect();
    if new.is_empty() {
        Ok(format!(
            "supply ok: {} build-script / proc-macro crates, all on the baseline",
            found.len()
        ))
    } else {
        Err(format!(
            "cargo xt supply: {} build-time crate(s) new to the tree; each needs AJ's audit and a line in supply-chain/build-and-proc-macro.txt (SR R-SUP-1):\n  {}",
            new.len(),
            new.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_root;

    #[test]
    fn the_workspace_matches_its_baseline() {
        let root = repo_root();
        let r = check(&root, &root.join("Cargo.toml"));
        assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}
