//! `schemas/renderplan@0.json` equals RP §2.2 (W0-07), and the crate embeds that file.

mod common;

use serde_json::Value;

#[test]
fn committed_schema_equals_rp_2_2() {
    let spec: Value = serde_json::from_str(&common::spec_block("### 2.2")).unwrap();
    let file: Value = serde_json::from_str(&common::read("schemas/renderplan@0.json")).unwrap();
    assert_eq!(
        spec, file,
        "schemas/renderplan@0.json differs from spec/renderplan-v0.md §2.2"
    );
    assert_eq!(file["title"], "gmclip.renderplan/0");
}

#[test]
fn the_crate_embeds_the_committed_file() {
    assert_eq!(
        gmclip_plan::schema::SCHEMA_JSON.replace("\r\n", "\n"),
        common::read("schemas/renderplan@0.json")
    );
    assert_eq!(
        gmclip_plan::schema::schema()["properties"]["schema"]["const"],
        gmclip_plan::SCHEMA_V0
    );
}

/// Every object in the schema is closed, or is the one map (RP §2.7's walk).
#[test]
fn every_object_schema_is_closed() {
    fn walk(v: &Value, at: &str, open: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                let is_object = m.get("type") == Some(&Value::from("object"));
                if is_object {
                    let closed = m.get("additionalProperties") == Some(&Value::Bool(false));
                    let map = m.contains_key("propertyNames")
                        && m.get("additionalProperties").is_some_and(Value::is_object);
                    if !closed && !map {
                        open.push(at.to_owned());
                    }
                }
                for (k, x) in m {
                    walk(x, &format!("{at}/{k}"), open);
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, &format!("{at}/{i}"), open);
                }
            }
            _ => {}
        }
    }
    let mut open = Vec::new();
    walk(gmclip_plan::schema::schema(), "", &mut open);
    assert!(open.is_empty(), "open object schemas: {open:?}");
}
