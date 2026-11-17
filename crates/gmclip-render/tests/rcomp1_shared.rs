//! H.04 (R-COMP-1): one per-frame compose body. The export's video pass, `render_frames`
//! and the preview session composite through `export::pipeline::compose::FrameComposer` (shared by macOS and Windows);
//! this guard fails if a second copy of the body (a compositor, its layer encode or its
//! text tiles) appears anywhere else under `src/export/`, where it could drift from the
//! export. The byte-identity itself is `rcomp1_lab` (lab).

use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            // `/` separators on every OS: the checks below match path suffixes
            out.push((
                p.display().to_string().replace('\\', "/"),
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
}

#[test]
fn the_compose_body_exists_once() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/export");
    let mut files = Vec::new();
    sources(&root, &mut files);
    for needle in [
        "GpuCompositor::new(",
        ".draw_tiles(",
        "comp\n            .encode(",
        "comp.encode(",
    ] {
        let hits: Vec<&str> = files
            .iter()
            .filter(|(_, s)| s.contains(needle))
            .map(|(p, _)| p.as_str())
            .collect();
        assert!(
            hits.iter().all(|p| p.ends_with("pipeline/compose.rs")),
            "`{needle}` outside the shared compose: {hits:?}"
        );
    }
    let compose = files
        .iter()
        .find(|(p, _)| p.ends_with("pipeline/compose.rs"))
        .map(|(_, s)| s.as_str())
        .expect("export/pipeline/compose.rs");
    assert!(compose.contains("GpuCompositor::new(") && compose.contains(".draw_tiles("));
    // every caller goes through it
    for (p, s) in &files {
        if p.ends_with("export/pipeline.rs") || p.ends_with("pipeline/preview.rs") {
            assert!(
                s.contains(".compose("),
                "{p} does not use FrameComposer::compose"
            );
        }
    }
}
