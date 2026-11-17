//! The Rust types describe exactly the committed schema (W0-07: "Rust types matching the
//! JSON Schema exactly").
//!
//! Walks `schemas/renderplan@1.json` (the superset of `@0`, decision 0014; its `schema`
//! is either version, as the types accept both and the schema pass picks the file) and the
//! schema `schemars` derives from [`Plan`] side by side and compares their **shape** at every node: object property names, required
//! sets and closedness (`additionalProperties: false`), map key patterns, array item
//! shapes, union branches (paired by their discriminator `const`s), JSON types,
//! `const`/`enum` value sets and string patterns. Numeric bounds (`minimum`, `minItems`
//! …) are not compared: the schema pass enforces them from the committed file itself.
//!
//! Optionality is normalised away on both sides (`["x", "null"]`, `anyOf [T, null]`,
//! `enum [v, null]`): the Rust `Option`s are what "not required" looks like there.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

fn committed() -> Value {
    let mut v: Value = serde_json::from_str(gmclip_plan::schema::SCHEMA_V1_JSON).unwrap();
    v["properties"]["schema"] = json!({ "enum": [gmclip_plan::SCHEMA_V0, gmclip_plan::SCHEMA_V1] });
    v
}

/// `@1` only adds to `@0`: every object key of `@0` is in `@1` with the same value, every
/// array of `@0` is a prefix of `@1`'s, except the version's own `$id`, `title` and
/// `schema` const, and `output.fps.den`'s `const 1`, which `@1` widens to `enum [1, 1001]`
/// (decision 0020).
#[test]
fn v1_extends_v0() {
    fn ext(a: &Value, b: &Value, at: &str, out: &mut Vec<String>) {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for (k, va) in x {
                    let here = format!("{at}/{k}");
                    // `output.fps.den`: `const 1` widens to `enum [1, 1001]` (decision 0020);
                    // `max_bps`'s 25 Mbit/s bound widens to 300 for upload rates (0027)
                    if [
                        "/$id",
                        "/title",
                        "/properties/schema/const",
                        "/properties/output/properties/fps/properties/den/const",
                        "/properties/output/properties/video/properties/rate/properties/max_bps/maximum",
                    ]
                    .contains(&here.as_str())
                    {
                        continue;
                    }
                    match y.get(k) {
                        Some(vb) => ext(va, vb, &here, out),
                        None => out.push(format!("{here}: missing in @1")),
                    }
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                if x.len() > y.len() {
                    out.push(format!("{at}: shorter in @1"));
                }
                for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                    ext(va, vb, &format!("{at}/{i}"), out);
                }
            }
            _ if a == b => {}
            _ => out.push(format!("{at}: {a} became {b}")),
        }
    }
    let v0: Value = serde_json::from_str(gmclip_plan::schema::SCHEMA_JSON).unwrap();
    let v1: Value = serde_json::from_str(gmclip_plan::schema::SCHEMA_V1_JSON).unwrap();
    let mut out = Vec::new();
    ext(&v0, &v1, "", &mut out);
    assert!(out.is_empty(), "{out:#?}");
    assert_eq!(v1["title"], gmclip_plan::SCHEMA_V1);
    assert_eq!(v1["properties"]["schema"]["const"], gmclip_plan::SCHEMA_V1);
}

fn derived() -> Value {
    serde_json::to_value(schemars::schema_for!(gmclip_plan::Plan)).unwrap()
}

struct Doc {
    root: Value,
}

impl Doc {
    /// Follows `$ref`s, then strips `null` alternatives.
    fn norm(&self, v: &Value) -> Value {
        let mut v = v.clone();
        while let Some(r) = v.get("$ref").and_then(Value::as_str).map(str::to_owned) {
            let name = r.strip_prefix("#/$defs/").expect("local $ref");
            let target = self.root["$defs"][name].clone();
            let mut merged = target.as_object().cloned().unwrap_or_default();
            for (k, x) in v.as_object().unwrap() {
                if k != "$ref" && k != "description" {
                    merged.insert(k.clone(), x.clone());
                }
            }
            v = Value::Object(merged);
        }
        let m = v.as_object_mut().unwrap();
        m.remove("description");
        m.remove("default");
        m.remove("format");
        if let Some(Value::Array(ts)) = m.get_mut("type") {
            ts.retain(|t| t != "null");
            if ts.len() == 1 {
                let t = ts[0].clone();
                m.insert("type".into(), t);
            }
        }
        if let Some(Value::Array(e)) = m.get_mut("enum") {
            e.retain(|x| !x.is_null());
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(Value::Array(bs)) = m.get(key).cloned() {
                let kept: Vec<Value> = bs
                    .into_iter()
                    .filter(|b| b.get("type") != Some(&json!("null")))
                    .collect();
                m.remove(key);
                if kept.len() == 1 {
                    let inner = self.norm(&kept[0]);
                    for (k, x) in inner.as_object().unwrap() {
                        m.insert(k.clone(), x.clone());
                    }
                } else if kept.iter().all(|b| values(&self.norm(b)).is_some()) {
                    // A union of consts (a documented Rust enum) is an `enum`.
                    let all: Vec<Value> = kept
                        .iter()
                        .flat_map(|b| {
                            let b = self.norm(b);
                            match (b.get("const"), b.get("enum")) {
                                (Some(c), _) => vec![c.clone()],
                                (None, Some(Value::Array(e))) => e.clone(),
                                _ => vec![],
                            }
                        })
                        .collect();
                    m.insert("enum".into(), Value::Array(all));
                } else {
                    m.insert("oneOf".into(), Value::Array(kept));
                }
            }
        }
        v
    }
}

/// The value set a node allows, if it is a `const`/`enum` node (or a union of consts).
fn values(v: &Value) -> Option<BTreeSet<String>> {
    if let Some(c) = v.get("const") {
        return Some(BTreeSet::from([c.to_string()]));
    }
    if let Some(Value::Array(e)) = v.get("enum") {
        return Some(e.iter().map(Value::to_string).collect());
    }
    None
}

/// A union branch's pairing key: its `const` properties, else its property names.
fn branch_key(d: &Doc, b: &Value) -> String {
    let b = d.norm(b);
    let props = b.get("properties").and_then(Value::as_object);
    let consts: Vec<String> = props
        .into_iter()
        .flatten()
        .filter_map(|(k, s)| s.get("const").map(|c| format!("{k}={c}")))
        .collect();
    if !consts.is_empty() {
        return consts.join(",");
    }
    if let Some(vs) = values(&b) {
        return vs.into_iter().collect::<Vec<_>>().join("|");
    }
    let names: BTreeSet<&String> = props.into_iter().flat_map(|p| p.keys()).collect();
    format!("{names:?}")
}

fn push(diffs: &mut Vec<String>, at: &str, what: String) {
    diffs.push(format!("{at}: {what}"));
}

fn compare(a_doc: &Doc, a: &Value, b_doc: &Doc, b: &Value, at: &str, diffs: &mut Vec<String>) {
    let a = a_doc.norm(a);
    let b = b_doc.norm(b);

    // Unions.
    match (a.get("oneOf"), b.get("oneOf")) {
        (Some(Value::Array(x)), Some(Value::Array(y))) => {
            let kx: BTreeMap<String, &Value> =
                x.iter().map(|v| (branch_key(a_doc, v), v)).collect();
            let ky: BTreeMap<String, &Value> =
                y.iter().map(|v| (branch_key(b_doc, v), v)).collect();
            let (sx, sy): (BTreeSet<_>, BTreeSet<_>) = (kx.keys().collect(), ky.keys().collect());
            if sx != sy {
                push(diffs, at, format!("union branches {sx:?} vs {sy:?}"));
                return;
            }
            for (k, va) in &kx {
                compare(a_doc, va, b_doc, ky[k], &format!("{at}<{k}>"), diffs);
            }
            return;
        }
        (None, None) => {}
        _ => {
            // A union of consts on one side may be an enum on the other.
            if values(&a).is_none() || values(&b).is_none() {
                push(diffs, at, "one side is a union, the other is not".into());
                return;
            }
        }
    }

    // Types (only where both say).
    if let (Some(ta), Some(tb)) = (a.get("type"), b.get("type"))
        && ta != tb
    {
        push(diffs, at, format!("type {ta} vs {tb}"));
    }
    // const / enum value sets (only where the committed schema has one; a Rust enum
    // with no committed counterpart is a mismatch too).
    match (values(&a), values(&b)) {
        (Some(x), Some(y)) if x != y => push(diffs, at, format!("values {x:?} vs {y:?}")),
        (Some(x), None) => push(
            diffs,
            at,
            format!("committed allows only {x:?}, the type allows any"),
        ),
        (None, Some(y)) => push(
            diffs,
            at,
            format!("the type allows only {y:?}, committed allows any"),
        ),
        _ => {}
    }
    if a.get("pattern") != b.get("pattern") {
        push(
            diffs,
            at,
            format!("pattern {:?} vs {:?}", a.get("pattern"), b.get("pattern")),
        );
    }

    // Objects.
    let props = |v: &Value| v.get("properties").and_then(Value::as_object).cloned();
    match (props(&a), props(&b)) {
        (Some(pa), Some(pb)) => {
            let na: BTreeSet<&String> = pa.keys().collect();
            let nb: BTreeSet<&String> = pb.keys().collect();
            if na != nb {
                push(
                    diffs,
                    at,
                    format!(
                        "properties only in committed {:?}, only in types {:?}",
                        na.difference(&nb).collect::<Vec<_>>(),
                        nb.difference(&na).collect::<Vec<_>>()
                    ),
                );
            }
            let req = |v: &Value| -> BTreeSet<String> {
                v.get("required")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            };
            if req(&a) != req(&b) {
                push(
                    diffs,
                    at,
                    format!("required {:?} vs {:?}", req(&a), req(&b)),
                );
            }
            let closed = |v: &Value| v.get("additionalProperties") == Some(&json!(false));
            if closed(&a) != closed(&b) {
                push(
                    diffs,
                    at,
                    format!("closed {} vs {}", closed(&a), closed(&b)),
                );
            }
            for k in na.intersection(&nb) {
                compare(a_doc, &pa[*k], b_doc, &pb[*k], &format!("{at}/{k}"), diffs);
            }
        }
        (None, None) => {}
        _ => push(diffs, at, "object with properties on one side only".into()),
    }

    // Maps: committed `propertyNames` + `additionalProperties: {schema}`; derived
    // `patternProperties: {pattern: schema}` + `additionalProperties: false`.
    let map_a = a
        .get("additionalProperties")
        .filter(|x| x.is_object())
        .map(|v| (a.get("propertyNames").map(|p| a_doc.norm(p)), v.clone()));
    let map_b = b
        .get("patternProperties")
        .and_then(Value::as_object)
        .and_then(|pp| pp.iter().next())
        .map(|(pat, v)| (Some(json!({ "type": "string", "pattern": pat })), v.clone()));
    match (map_a, map_b) {
        (Some((ka, va)), Some((kb, vb))) => {
            let pat = |k: &Option<Value>| k.as_ref().and_then(|k| k.get("pattern").cloned());
            if pat(&ka) != pat(&kb) {
                push(
                    diffs,
                    at,
                    format!("map key pattern {:?} vs {:?}", pat(&ka), pat(&kb)),
                );
            }
            compare(a_doc, &va, b_doc, &vb, &format!("{at}/*"), diffs);
        }
        (None, None) => {}
        _ => push(diffs, at, "map on one side only".into()),
    }

    // Arrays.
    match (a.get("prefixItems"), b.get("prefixItems")) {
        (Some(Value::Array(x)), Some(Value::Array(y))) => {
            if x.len() != y.len() {
                push(diffs, at, format!("prefixItems {} vs {}", x.len(), y.len()));
            }
            for (i, (xa, ya)) in x.iter().zip(y).enumerate() {
                compare(a_doc, xa, b_doc, ya, &format!("{at}[{i}]"), diffs);
            }
        }
        (None, None) => {}
        _ => push(diffs, at, "prefixItems on one side only".into()),
    }
    match (a.get("items"), b.get("items")) {
        (Some(x), Some(y)) => compare(a_doc, x, b_doc, y, &format!("{at}[]"), diffs),
        (None, None) => {}
        _ => push(diffs, at, "items on one side only".into()),
    }
}

#[test]
fn derived_schema_has_the_committed_shape() {
    let (a, b) = (Doc { root: committed() }, Doc { root: derived() });
    let mut diffs = Vec::new();
    compare(&a, &a.root.clone(), &b, &b.root.clone(), "", &mut diffs);
    assert!(
        diffs.is_empty(),
        "types and schema differ:\n{}",
        diffs.join("\n")
    );
}

/// The comparison is not vacuous: a schema with one property renamed is caught.
#[test]
fn the_comparison_catches_a_renamed_property() {
    let mut mutated = committed();
    let layer = mutated["$defs"]["layer"]["properties"]
        .as_object_mut()
        .unwrap();
    let v = layer.remove("visible").unwrap();
    layer.insert("visable".into(), v);
    let (a, b) = (Doc { root: mutated }, Doc { root: derived() });
    let mut diffs = Vec::new();
    compare(&a, &a.root.clone(), &b, &b.root.clone(), "", &mut diffs);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("visable"), "{diffs:?}");
}

/// …and so are a changed enum and an opened object.
#[test]
fn the_comparison_catches_enum_and_closedness_changes() {
    let mut mutated = committed();
    mutated["$defs"]["text_event"]["properties"]["role"]["enum"]
        .as_array_mut()
        .unwrap()
        .push(json!("title"));
    mutated["properties"]["meta"]["additionalProperties"] = json!(true);
    let (a, b) = (Doc { root: mutated }, Doc { root: derived() });
    let mut diffs = Vec::new();
    compare(&a, &a.root.clone(), &b, &b.root.clone(), "", &mut diffs);
    assert_eq!(diffs.len(), 2, "{diffs:?}");
}
