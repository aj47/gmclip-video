//! RP §2.7's probe table, replayed on the Rust validator: every reject row is refused
//! with `Invalid` at the named pointer, and the two accept rows pass.

mod common;

use serde_json::{Value, json};

type Mutation = fn(&mut Value);

/// The mutated plan is refused, and one of its errors is `Invalid` at `path`. (The
/// mutations go through `serde_json::Value`, which sorts keys, so "first in document
/// order" is not meaningful here; the fixture files test first-error order.)
fn rejects(name: &str, path: &str, m: Mutation) {
    let mut v = common::h79();
    m(&mut v);
    let r = gmclip_plan::check(serde_json::to_string(&v).unwrap().as_bytes());
    assert!(!r.errors.is_empty(), "{name}: accepted");
    assert!(
        r.errors
            .iter()
            .any(|e| e.code() == "Invalid" && e.path() == path),
        "{name}: no Invalid at {path}: {:?}",
        r.errors
    );
}

#[test]
fn the_example_itself_is_valid() {
    common::validate(&common::h79()).unwrap();
}

#[test]
fn rp_2_7_reject_rows() {
    let rows: &[(&str, &str, Mutation)] = &[
        ("layer visable typo", "/parts/1/layers/1/visable", |v| {
            v["parts"][1]["layers"][1]["visable"] = json!(false);
        }),
        ("meta.typo", "/meta/typo", |v| v["meta"]["typo"] = json!(1)),
        (
            "provenance.detail.anything",
            "/text_events/0/provenance/detail/anything",
            |v| {
                v["text_events"][0]["provenance"]["detail"]["anything"] = json!("x");
            },
        ),
        (
            "text_style max_width/fit",
            "/text_styles/Hook/max_width",
            |v| {
                v["text_styles"]["Hook"]["max_width"] = json!(1000);
                v["text_styles"]["Hook"]["fit"] = json!(true);
            },
        ),
        (
            "text_style line_height",
            // "Speech" + "line_height" is 17 caller bytes; R-CMD-6 allows 16 per path.
            "/text_styles/Speech/line_heigh…",
            |v| {
                v["text_styles"]["Speech"]["line_height"] = json!(1.2);
            },
        ),
        ("part.audio.crossfade", "/parts/0/audio/crossfade", |v| {
            v["parts"][0]["audio"]["crossfade"] = json!({"samples": 480});
        }),
        (
            "time_map hold + at_src",
            "/parts/0/layers/0/time_map/at_src",
            |v| {
                v["parts"][0]["layers"][0]["time_map"] =
                    json!({"mode": "hold", "frame": 10, "at_src": 2.5});
            },
        ),
        ("background colour + stops", "/background/stops", |v| {
            v["background"]["stops"] = json!(["#000000", "#ffffff"]);
        }),
        ("fps 30000/1001", "/output/fps/num", |v| {
            v["output"]["fps"] = json!({"num": 30000, "den": 1001});
        }),
        ("top-level image_events", "/image_events", |v| {
            v["image_events"] = json!([])
        }),
        ("text events without fonts", "/fonts", |v| {
            v.as_object_mut().unwrap().remove("fonts");
        }),
        ("fonts[].em_per_size missing", "/fonts/0/em_per_size", |v| {
            v["fonts"][0].as_object_mut().unwrap().remove("em_per_size");
        }),
        ("text event without size", "/text_events/3/size", |v| {
            v["text_events"][3].as_object_mut().unwrap().remove("size");
        }),
        ("text_events[].style Nope", "/text_events/4/style", |v| {
            v["text_events"][4]["style"] = json!("Nope");
        }),
        (
            "text_styles.Hook.font nope",
            "/text_styles/Hook/font",
            |v| {
                v["text_styles"]["Hook"]["font"] = json!("nope");
            },
        ),
        (
            "parts[1].picture.source nope",
            "/parts/1/picture/source",
            |v| {
                v["parts"][1]["picture"]["source"] = json!("nope");
            },
        ),
        ("duplicate text event id", "/text_events/6/id", |v| {
            v["text_events"][6]["id"] = json!("c01");
        }),
        ("parts gap [56, 176]", "/parts/1/out", |v| {
            v["parts"][1]["out"] = json!([56, 176])
        }),
        (
            "output.frames 1807 over 397 frames of parts",
            "/output/frames",
            |v| {
                v["output"]["frames"] = json!(1807);
            },
        ),
        (
            "text event past output.frames",
            "/text_events/13/out",
            |v| {
                v["text_events"][13]["out"] = json!([349, 398]);
            },
        ),
        (
            "fade on a continuous join",
            "/parts/0/audio/fade_out",
            |v| {
                // part 2 starts on the sample after part 1's last, on the same source
                v["parts"][1]["audio"]["first_sample"] = json!(130_006_400 + 88_000);
            },
        ),
    ];
    for (name, path, m) in rows {
        rejects(name, path, *m);
    }
    assert_eq!(rows.len(), 21);
}

#[test]
fn a_continuous_join_without_fades_is_fine() {
    let mut v = common::h79();
    v["parts"][1]["audio"]["first_sample"] = json!(130_006_400 + 88_000);
    v["parts"][0]["audio"]["fade_out"] = Value::Null;
    v["parts"][1]["audio"]
        .as_object_mut()
        .unwrap()
        .remove("fade_in");
    common::validate(&v).unwrap();
}

#[test]
fn rp_2_7_accept_rows() {
    let mut v = common::h79();
    v["parts"][1]["layers"][0]["role"] = json!("chat");
    v["text_events"][0]["provenance"]["kind"] = json!("ocr");
    common::validate(&v).unwrap();
}

/// The same next sample on a *different* source is a discontinuous join: fades allowed.
#[test]
fn a_join_onto_another_source_is_discontinuous() {
    let mut v = common::h79();
    v["parts"][1]["audio"]["first_sample"] = json!(130_006_400 + 88_000);
    v["parts"][1]["audio"]["source"] = json!("mkv1440");
    let p = common::validate(&v).unwrap();
    assert!(!gmclip_plan::validate::continuous_join(&p, 0));
}

/// A plan built in code is held to the schema's bounds as well as the rules.
#[test]
fn check_plan_applies_the_schema_bounds() {
    let mut p = common::validate(&common::h79()).unwrap();
    assert!(gmclip_plan::check_plan(&p).is_empty());
    p.parts[0].layers[0].opacity = Some(1.5);
    let e = gmclip_plan::check_plan(&p);
    assert_eq!(e[0].path(), "/parts/0/layers/0/opacity", "{e:?}");
    p.parts.clear();
    assert_eq!(gmclip_plan::check_plan(&p)[0].path(), "/parts");
}

/// Invalid comes before Unsupported: an invalid plan is never reported as merely
/// unsupported.
#[test]
fn invalid_errors_come_first() {
    let mut v = common::h79();
    v["sources"][0]["expect"]["video"]["fps"] = json!({"num": 60, "den": 1});
    v["text_events"][13]["out"] = json!([349, 349]);
    let r = gmclip_plan::check(serde_json::to_string(&v).unwrap().as_bytes());
    assert_eq!(r.errors[0].code(), "Invalid", "{:?}", r.errors);
    assert_eq!(r.errors.last().unwrap().code(), "Unsupported");
}

/// Thousands of unknown keys: linear time, and the report is capped.
#[test]
fn many_unknown_keys_are_capped_and_fast() {
    let mut v = common::h79();
    for i in 0..20_000 {
        v["meta"][format!("k{i}")] = json!(1);
    }
    let t = std::time::Instant::now();
    let r = gmclip_plan::check(serde_json::to_string(&v).unwrap().as_bytes());
    assert_eq!(r.errors.len(), gmclip_plan::MAX_ERRORS);
    assert_eq!(r.more, 20_000 - gmclip_plan::MAX_ERRORS);
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}
