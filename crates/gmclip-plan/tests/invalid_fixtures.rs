//! `schemas/examples/invalid/*.json`: each is refused with the code and pointer
//! `index.json` names (W0-07: ≥ 12 further invalid fixtures, each with `Invalid{path}`).

mod common;

use serde::Deserialize;

#[derive(Deserialize)]
struct Row {
    file: String,
    rule: String,
    code: String,
    path: String,
}

#[test]
fn every_fixture_is_refused_at_its_pointer() {
    let dir = common::repo().join("schemas/examples/invalid");
    let index: Vec<Row> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("index.json")).unwrap()).unwrap();
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json") && n != "index.json")
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = index.iter().map(|r| r.file.clone()).collect();
    listed.sort();
    assert_eq!(on_disk, listed, "index.json and the fixture files disagree");

    let mut invalid = 0;
    for r in &index {
        let bytes = std::fs::read(dir.join(&r.file)).unwrap();
        let err = gmclip_plan::parse(&bytes)
            .err()
            .unwrap_or_else(|| panic!("{} (rule {}) was accepted", r.file, r.rule));
        assert_eq!(err.code(), r.code, "{}: {err}", r.file);
        assert_eq!(err.path(), r.path, "{}: {err}", r.file);
        assert!(!err.reason().is_empty());
        invalid += usize::from(r.code == "Invalid");
    }
    assert!(invalid >= 12, "only {invalid} Invalid fixtures");
}
