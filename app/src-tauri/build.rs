//! Tauri's build step, restricted to the app's own two commands (R-UI-SEC-1): the
//! capability in `capabilities/main.json` grants exactly these to the main window and
//! nothing else (no plugin permissions).
fn main() {
    // R-DEV-1: the dev bridge never reaches a release build.
    if std::env::var("CARGO_FEATURE_DEV_BRIDGE").is_ok()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!("the `dev-bridge` feature is refused in the release profile (R-DEV-1)");
    }
    analysis_code_hash();
    let attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["gm_call", "gm_subscribe"]));
    if let Err(e) = tauri_build::try_build(attrs) {
        panic!("tauri-build: {e:#}");
    }
}

/// `GMCLIP_ANALYSIS_CODE_HASH`: a hash of the code that decides what an analysis hears
/// (the CLI's analysis host, gmclip-analysis, gmclip-ml, gmclip-media), part of the e2e
/// analysis cache's key (`src/analysiscache.rs`): an edit there hears the test videos again.
fn analysis_code_hash() {
    use std::hash::{DefaultHasher, Hasher};
    use std::path::{Path, PathBuf};
    fn files(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            entries.sort();
            for e in entries {
                files(&e, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p.to_path_buf());
        }
    }
    let crates = Path::new("../../crates");
    let roots = [
        crates.join("gmclip-cli/src/analysis.rs"),
        crates.join("gmclip-analysis/src"),
        crates.join("gmclip-ml/src"),
        crates.join("gmclip-media/src"),
    ];
    let mut all = Vec::new();
    for r in &roots {
        println!("cargo:rerun-if-changed={}", r.display());
        files(r, &mut all);
    }
    let mut h = DefaultHasher::new();
    for f in &all {
        h.write(f.to_string_lossy().as_bytes());
        h.write(&std::fs::read(f).unwrap_or_default());
    }
    println!(
        "cargo:rustc-env=GMCLIP_ANALYSIS_CODE_HASH={:016x}",
        h.finish()
    );
}
