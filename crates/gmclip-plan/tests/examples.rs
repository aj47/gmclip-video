//! Every `schemas/examples/*.renderplan.json` is accepted (W0-07), and the h79 example is
//! RP §2.3 itself.

mod common;

use gmclip_plan::validate::{continuous_join, out_samples, part_samples};
use serde_json::Value;

fn examples() -> Vec<(String, Vec<u8>)> {
    let dir = common::repo().join("schemas/examples");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".renderplan.json"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&p).unwrap())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_example_is_accepted() {
    let all = examples();
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    for required in [
        "h79-parts-1-3.renderplan.json",
        "native-mode.renderplan.json",
        "bench-60-m0.renderplan.json",
    ] {
        assert!(names.contains(&required), "missing example {required}");
    }
    // B-13 writes h79-v4.renderplan.json; once it exists it is checked here too.
    for (name, bytes) in &all {
        let r = gmclip_plan::check(bytes);
        assert!(r.errors.is_empty(), "{name}: {:?}", r.errors);
    }
}

#[test]
fn h79_example_is_rp_2_3() {
    let file: Value = serde_json::from_slice(
        &std::fs::read(common::repo().join("schemas/examples/h79-parts-1-3.renderplan.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(file, common::h79());
}

/// RP §2.3's derivation notes, recomputed from the typed plan.
#[test]
fn h79_numbers() {
    let p = common::validate(&common::h79()).unwrap();
    assert_eq!(p.output.frames, 397);
    let fps = p.output.fps;
    let samples: Vec<u64> = p
        .parts
        .iter()
        .map(|x| part_samples(x.out, fps).unwrap())
        .collect();
    assert_eq!(samples, [88_000, 193_600, 353_600]);
    assert_eq!(samples.iter().sum::<u64>(), out_samples(397, fps).unwrap());
    // Both joins are discontinuous, so every fade is allowed.
    assert!(!continuous_join(&p, 0) && !continuous_join(&p, 1));
    assert_eq!(p.text_events.as_ref().unwrap().len(), 14);
}

#[test]
fn bench_60_m0_has_its_shape() {
    let bytes =
        std::fs::read(common::repo().join("schemas/examples/bench-60-m0.renderplan.json")).unwrap();
    let p = gmclip_plan::parse(&bytes).unwrap();
    assert_eq!(p.output.frames, 1800);
    assert_eq!(p.parts.len(), 41);
    assert!(
        (0..40).all(|j| !continuous_join(&p, j)),
        "40 discontinuous joins"
    );
    assert!(p.parts.iter().all(|x| x.layers.len() == 4));
    let roles = |r: gmclip_plan::types::TextRole| {
        p.text_events
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.role == r)
            .count()
    };
    use gmclip_plan::types::TextRole::*;
    assert_eq!(
        (roles(Caption), roles(SectionPill), roles(TextHook)),
        (66, 6, 1)
    );
    assert!(roles(RunClock) > 0);
    assert_eq!(p.audio.envelopes.as_ref().unwrap().len(), 1);
}

#[test]
fn native_mode_has_a_continuous_join() {
    let bytes =
        std::fs::read(common::repo().join("schemas/examples/native-mode.renderplan.json")).unwrap();
    let p = gmclip_plan::parse(&bytes).unwrap();
    assert_eq!(p.sources.len(), 1);
    assert!(continuous_join(&p, 0));
    assert!(!continuous_join(&p, 1));
}

/// Deserialise → serialise gives back the same document (numbers compared by value), so
/// the types neither drop nor invent fields.
#[test]
fn examples_round_trip_through_the_types() {
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Value::Array(x), Value::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
            }
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
            }
            _ => a == b,
        }
    }
    for (name, bytes) in examples() {
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        let plan = gmclip_plan::parse(&bytes).unwrap();
        let again = serde_json::to_value(&plan).unwrap();
        assert!(same(&original, &again), "{name} does not round-trip");
    }
}

/// `cargo xt bench60`'s BENCH-60-M0 plan (lab sources, H-15) validates and has SPEC §13's
/// shape: 1800 frames, 41 parts, 40 discontinuous joins, two-source (b) parts.
#[test]
fn bench_60_m0_lab_plan_validates() {
    let bytes =
        std::fs::read(common::repo().join("test-media/bench-60-m0.renderplan.json")).unwrap();
    let p = gmclip_plan::parse(&bytes).unwrap();
    assert_eq!(p.output.frames, 1800);
    assert_eq!(p.parts.len(), 41);
    assert!((0..40).all(|j| !continuous_join(&p, j)));
    assert!(p.parts.iter().all(|x| x.layers.len() == 4));
    assert_eq!(p.parts[1].picture.source.as_str(), "b-mkv");
    assert_eq!(p.parts[1].audio.source.as_str(), "b-proxy");
    use gmclip_plan::types::TextRole::*;
    let n = |r| {
        p.text_events
            .as_ref()
            .unwrap()
            .iter()
            .filter(|e| e.role == r)
            .count()
    };
    assert_eq!((n(Caption), n(SectionPill), n(TextHook)), (66, 6, 1));
    assert!(n(RunClock) > 0);
    assert_eq!(p.audio.envelopes.as_ref().unwrap().len(), 1);
}
