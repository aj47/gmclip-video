//! The schema pass: checks a [`JVal`] against the committed `schemas/renderplan@0.json`.
//!
//! This is a JSON Schema 2020-12 evaluator for exactly the keywords that file uses
//! ([`KEYWORDS`]); a test walks the schema and fails if it ever uses another, so the
//! file stays the single source of truth for the structural rules and nothing in it is
//! silently skipped. Errors are `Invalid{path}` with RFC 6901 pointers, in document
//! order.
//!
//! Deliberate strictness beyond JSON Schema: wherever the schema wants an integer (a
//! `type: integer`, or an integer `const`/`enum` such as `fps.num` or `rate`), the value
//! must be an integer literal that fits in i64 (`397`, not `397.0` or `1e3`; RP rule 1,
//! "integers only"). `-0` is the integer 0.

use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::error::{PlanError, join_index, join_key};
use crate::json::JVal;
use crate::pattern::Pattern;

/// The committed schema, byte for byte (equal to RP §2.2; see `tests/schema_file.rs`).
pub const SCHEMA_JSON: &str = include_str!("../../../schemas/renderplan@0.json");

/// The committed `/1` schema (decision 0014): `/0` plus image and shape events, the
/// blurred-source background and text animation. A plan whose `schema` is
/// `gmclip.renderplan/1` is checked against it; every other plan against `/0`.
pub const SCHEMA_V1_JSON: &str = include_str!("../../../schemas/renderplan@1.json");

/// Every keyword the evaluator understands. Annotations are accepted and ignored.
pub const KEYWORDS: &[&str] = &[
    // applied
    "type",
    "const",
    "enum",
    "pattern",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "multipleOf",
    "minItems",
    "maxItems",
    "prefixItems",
    "items",
    "required",
    "properties",
    "additionalProperties",
    "propertyNames",
    "dependentRequired",
    "oneOf",
    "$ref",
    // annotations / structure
    "$schema",
    "$id",
    "$defs",
    "title",
    "description",
    "default",
];

/// The parsed committed schema.
pub fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(SCHEMA_JSON).expect("schemas/renderplan@0.json is valid JSON")
    })
}

/// The parsed committed `/1` schema.
pub fn schema_v1() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(SCHEMA_V1_JSON).expect("schemas/renderplan@1.json is valid JSON")
    })
}

/// Checks `inst` against the committed schema its `schema` member names (`/1`, else
/// `/0`). Empty means it conforms.
pub fn check(inst: &JVal) -> Vec<PlanError> {
    let v1 = matches!(inst.get("schema"), Some(JVal::Str(s)) if s == crate::SCHEMA_V1);
    let root = if v1 { schema_v1() } else { schema() };
    let mut errs = Vec::new();
    Eval { root }.node(root, inst, "", &mut errs);
    errs
}

struct Eval<'s> {
    root: &'s Value,
}

impl<'s> Eval<'s> {
    fn resolve(&self, r: &str) -> &'s Value {
        let name = r
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("unsupported $ref {r}"));
        self.root
            .get("$defs")
            .and_then(|d| d.get(name))
            .unwrap_or_else(|| panic!("dangling $ref {r}"))
    }

    fn node(&self, s: &'s Value, inst: &JVal, path: &str, errs: &mut Vec<PlanError>) {
        let Some(s) = s.as_object() else {
            if s == &Value::Bool(false) {
                errs.push(PlanError::invalid(path, "not allowed"));
            }
            return;
        };
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            self.node(self.resolve(r), inst, path, errs);
        }
        if let Some(branches) = s.get("oneOf").and_then(Value::as_array) {
            self.one_of(branches, inst, path, errs);
        }
        let ty = s.get("type").map(|t| {
            t.as_str()
                .unwrap_or_else(|| panic!("schema `type` must be a string at {path}"))
        });
        if let Some(t) = ty
            && !type_ok(t, inst)
        {
            errs.push(PlanError::invalid(path, type_reason(t, inst)));
            return;
        }
        let pinned = s.get("const").into_iter().chain(
            s.get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );
        let wants_integer = ty == Some("integer") || pinned.clone().any(Value::is_i64);
        if let JVal::Num(n) = inst
            && wants_integer
        {
            if n.as_i64().is_none() && n.is_u64() {
                errs.push(PlanError::invalid(path, "integer out of range"));
                return;
            }
            if n.as_i64().is_none() {
                errs.push(PlanError::invalid(path, "must be an integer literal"));
                return;
            }
        }
        if let Some(c) = s.get("const")
            && !equal(inst, c)
        {
            errs.push(PlanError::invalid(path, format!("must be {c}")));
            return;
        }
        if let Some(e) = s.get("enum").and_then(Value::as_array)
            && !e.iter().any(|c| equal(inst, c))
        {
            let list: Vec<String> = e.iter().map(Value::to_string).collect();
            errs.push(PlanError::invalid(
                path,
                format!("must be one of {}", list.join(", ")),
            ));
            return;
        }
        match inst {
            JVal::Str(v) => self.string(s, v, path, errs),
            JVal::Num(n) => number(s, n, path, errs),
            JVal::Arr(items) => self.array(s, items, path, errs),
            JVal::Obj(kv) => self.object(s, kv, path, errs),
            JVal::Null | JVal::Bool(_) => {}
        }
    }

    fn string(&self, s: &Map<String, Value>, v: &str, path: &str, errs: &mut Vec<PlanError>) {
        if let Some(p) = s.get("pattern").and_then(Value::as_str) {
            let pat = Pattern::parse(p).unwrap_or_else(|e| panic!("schema pattern {p}: {e}"));
            if !pat.is_match(v) {
                errs.push(PlanError::invalid(path, format!("must match {p}")));
            }
        }
    }

    fn array(
        &self,
        s: &'s Map<String, Value>,
        items: &[JVal],
        path: &str,
        errs: &mut Vec<PlanError>,
    ) {
        let n = items.len();
        if let Some(min) = s.get("minItems").and_then(Value::as_u64)
            && (n as u64) < min
        {
            errs.push(PlanError::invalid(
                path,
                format!("needs at least {min} items"),
            ));
            return;
        }
        if let Some(max) = s.get("maxItems").and_then(Value::as_u64)
            && (n as u64) > max
        {
            errs.push(PlanError::invalid(
                path,
                format!("allows at most {max} items"),
            ));
            return;
        }
        let prefix = s
            .get("prefixItems")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for (i, item) in items.iter().enumerate() {
            let sub = prefix.get(i).or_else(|| s.get("items"));
            if let Some(sub) = sub {
                self.node(sub, item, &join_index(path, i), errs);
            }
        }
    }

    fn object(
        &self,
        s: &'s Map<String, Value>,
        kv: &[(String, JVal)],
        path: &str,
        errs: &mut Vec<PlanError>,
    ) {
        let props = s.get("properties").and_then(Value::as_object);
        let additional = s.get("additionalProperties");
        for (k, v) in kv {
            let child = join_key(path, k);
            if let Some(names) = s.get("propertyNames") {
                let before = errs.len();
                self.node(names, &JVal::Str(k.clone()), &child, errs);
                if errs.len() > before {
                    for e in &mut errs[before..] {
                        if let PlanError::Invalid { reason, .. } = e {
                            *reason = format!("key {reason}");
                        }
                    }
                    continue;
                }
            }
            match props.and_then(|p| p.get(k)) {
                Some(sub) => self.node(sub, v, &child, errs),
                None => match additional {
                    Some(Value::Bool(false)) => {
                        errs.push(PlanError::invalid(child, "unknown key"));
                    }
                    Some(sub @ Value::Object(_)) => self.node(sub, v, &child, errs),
                    _ => {}
                },
            }
        }
        let has = |k: &str| kv.iter().any(|(key, _)| key == k);
        for r in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !has(r) {
                errs.push(PlanError::invalid(
                    join_key(path, r),
                    "missing required key",
                ));
            }
        }
        if let Some(dep) = s.get("dependentRequired").and_then(Value::as_object) {
            for (trigger, needs) in dep {
                if !has(trigger) {
                    continue;
                }
                for n in needs
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if !has(n) {
                        errs.push(PlanError::invalid(
                            join_key(path, n),
                            format!("required when {trigger} is present"),
                        ));
                    }
                }
            }
        }
    }

    /// `oneOf`: exactly one branch must hold. When none does, the error names the
    /// branch the value was evidently aiming at (same JSON type, same discriminator
    /// `const`s), so a typo inside a `hold` time map reports the typo, not "no branch".
    fn one_of(&self, branches: &'s [Value], inst: &JVal, path: &str, errs: &mut Vec<PlanError>) {
        let results: Vec<Vec<PlanError>> = branches
            .iter()
            .map(|b| {
                let mut e = Vec::new();
                self.node(b, inst, path, &mut e);
                e
            })
            .collect();
        let passing = results.iter().filter(|e| e.is_empty()).count();
        if passing == 1 {
            return;
        }
        if passing > 1 {
            errs.push(PlanError::invalid(
                path,
                "matches more than one allowed form",
            ));
            return;
        }
        let aimed: Vec<usize> = (0..branches.len())
            .filter(|&i| self.aims_at(&branches[i], inst))
            .collect();
        if let [i] = aimed[..] {
            errs.extend(results[i].iter().cloned());
            return;
        }
        // An object whose discriminator matches no branch: name the discriminator.
        if let JVal::Obj(_) = inst
            && let Some((key, allowed)) = self.discriminator(branches)
        {
            let reason = match inst.get(&key) {
                Some(_) => format!("must be one of {}", allowed.join(", ")),
                None => "missing required key".to_owned(),
            };
            errs.push(PlanError::invalid(join_key(path, &key), reason));
            return;
        }
        errs.push(PlanError::invalid(
            path,
            "matches none of the allowed forms",
        ));
    }

    fn branch(&self, b: &'s Value) -> &'s Value {
        match b.get("$ref").and_then(Value::as_str) {
            Some(r) => self.resolve(r),
            None => b,
        }
    }

    fn aims_at(&self, b: &'s Value, inst: &JVal) -> bool {
        let b = self.branch(b);
        if let Some(t) = b.get("type").and_then(Value::as_str)
            && !type_ok(t, inst)
        {
            return false;
        }
        let Some(props) = b.get("properties").and_then(Value::as_object) else {
            return true;
        };
        props.iter().all(|(k, sub)| match sub.get("const") {
            Some(c) => inst.get(k).is_some_and(|v| equal(v, c)),
            None => true,
        })
    }

    /// The one property every branch pins with a `const`, and its allowed values.
    fn discriminator(&self, branches: &'s [Value]) -> Option<(String, Vec<String>)> {
        let mut key: Option<&str> = None;
        let mut allowed = Vec::new();
        for b in branches {
            let props = self.branch(b).get("properties")?.as_object()?;
            let (k, c) = props
                .iter()
                .find_map(|(k, sub)| sub.get("const").map(|c| (k.as_str(), c)))?;
            if key.is_some_and(|prev| prev != k) {
                return None;
            }
            key = Some(k);
            allowed.push(c.to_string());
        }
        key.map(|k| (k.to_owned(), allowed))
    }
}

fn type_ok(t: &str, inst: &JVal) -> bool {
    match (t, inst) {
        ("number", JVal::Num(_)) => true,
        (t, v) => v.type_name() == t,
    }
}

fn type_reason(t: &str, inst: &JVal) -> String {
    match (t, inst) {
        ("integer", JVal::Num(_)) => "must be an integer literal".to_owned(),
        _ => format!(
            "must be {} {t}",
            if t.starts_with(['a', 'i', 'o']) {
                "an"
            } else {
                "a"
            }
        ),
    }
}

/// JSON equality with numbers compared by value (`1` equals `1.0`).
fn equal(inst: &JVal, v: &Value) -> bool {
    match (inst, v) {
        (JVal::Null, Value::Null) => true,
        (JVal::Bool(a), Value::Bool(b)) => a == b,
        (JVal::Str(a), Value::String(b)) => a == b,
        // An integer in the schema matches only an integer literal (RP rule 1).
        (JVal::Num(a), Value::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(x), Some(y)) => x == y,
            (None, Some(_)) => false,
            _ => a.as_f64() == b.as_f64(),
        },
        (JVal::Arr(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equal(x, y))
        }
        (JVal::Obj(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| equal(x, y)))
        }
        _ => false,
    }
}

fn number(s: &Map<String, Value>, n: &serde_json::Number, path: &str, errs: &mut Vec<PlanError>) {
    // Integers compare exactly in i128; anything else in f64.
    let cmp = |bound: &Value| -> Option<std::cmp::Ordering> {
        match (n.as_i64(), bound.as_i64()) {
            (Some(a), Some(b)) => Some(i128::from(a).cmp(&i128::from(b))),
            _ => n.as_f64()?.partial_cmp(&bound.as_f64()?),
        }
    };
    use std::cmp::Ordering::{Equal, Greater, Less};
    if let Some(b) = s.get("minimum")
        && cmp(b) == Some(Less)
    {
        errs.push(PlanError::invalid(path, format!("must be ≥ {b}")));
    }
    if let Some(b) = s.get("maximum")
        && cmp(b) == Some(Greater)
    {
        errs.push(PlanError::invalid(path, format!("must be ≤ {b}")));
    }
    if let Some(b) = s.get("exclusiveMinimum")
        && matches!(cmp(b), Some(Less | Equal))
    {
        errs.push(PlanError::invalid(path, format!("must be > {b}")));
    }
    if let Some(m) = s.get("multipleOf") {
        let m = m
            .as_i64()
            .unwrap_or_else(|| panic!("schema `multipleOf` must be an integer at {path}"));
        let ok = n.as_i64().is_some_and(|v| m != 0 && v % m == 0);
        if !ok {
            errs.push(PlanError::invalid(
                path,
                format!("must be a multiple of {m}"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    fn errs(schema_json: &str, inst: &str) -> Vec<PlanError> {
        let root: Value = serde_json::from_str(schema_json).unwrap();
        let mut e = Vec::new();
        Eval { root: &root }.node(&root, &parse(inst.as_bytes()).unwrap(), "", &mut e);
        e
    }

    #[test]
    fn every_keyword_in_the_committed_schema_is_understood() {
        fn walk(s: &Value, at: &str) {
            let Some(m) = s.as_object() else { return };
            for (k, sub) in m {
                assert!(
                    KEYWORDS.contains(&k.as_str()),
                    "unknown keyword {k} at {at}"
                );
                let at = format!("{at}/{k}");
                match k.as_str() {
                    "properties" | "$defs" => {
                        for (name, x) in sub.as_object().unwrap() {
                            walk(x, &format!("{at}/{name}"));
                        }
                    }
                    "prefixItems" | "oneOf" => {
                        for (i, x) in sub.as_array().unwrap().iter().enumerate() {
                            walk(x, &format!("{at}/{i}"));
                        }
                    }
                    "items" | "additionalProperties" | "propertyNames" => walk(sub, &at),
                    _ => {}
                }
            }
        }
        walk(schema(), "");
        walk(schema_v1(), "");
    }

    #[test]
    fn every_pattern_parses() {
        fn walk(v: &Value) {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(p)) = m.get("pattern") {
                        Pattern::parse(p).unwrap_or_else(|e| panic!("{p}: {e}"));
                    }
                    m.values().for_each(walk);
                }
                Value::Array(a) => a.iter().for_each(walk),
                _ => {}
            }
        }
        walk(schema());
        walk(schema_v1());
    }

    #[test]
    fn integers_must_be_integer_literals() {
        let s = r#"{"type":"integer","minimum":1}"#;
        assert!(errs(s, "3").is_empty());
        assert_eq!(errs(s, "3.0")[0].reason(), "must be an integer literal");
        assert_eq!(errs(s, "0")[0].reason(), "must be ≥ 1");
        assert_eq!(
            errs(s, "18446744073709551615")[0].reason(),
            "integer out of range"
        );
    }

    #[test]
    fn numbers_compare_exactly() {
        let s = r#"{"type":"number","exclusiveMinimum":0,"maximum":2}"#;
        assert!(errs(s, "0.5").is_empty());
        assert!(errs(s, "2").is_empty());
        assert_eq!(errs(s, "0")[0].reason(), "must be > 0");
        assert_eq!(errs(s, "2.0001")[0].reason(), "must be ≤ 2");
    }

    #[test]
    fn one_of_reports_the_aimed_branch() {
        let s = r#"{"oneOf":[
            {"type":"object","additionalProperties":false,"required":["mode"],"properties":{"mode":{"const":"live"}}},
            {"type":"object","additionalProperties":false,"required":["mode","frame"],
             "properties":{"mode":{"const":"hold"},"frame":{"type":"integer"}}}]}"#;
        assert!(errs(s, r#"{"mode":"hold","frame":3}"#).is_empty());
        let e = errs(s, r#"{"mode":"hold","frame":3,"at_src":1}"#);
        assert_eq!(e, vec![PlanError::invalid("/at_src", "unknown key")]);
        let e = errs(s, r#"{"mode":"loop"}"#);
        assert_eq!(e[0].path(), "/mode");
        assert!(e[0].reason().contains("\"live\""), "{e:?}");
        let e = errs(s, r#"{"frame":3}"#);
        assert_eq!(e, vec![PlanError::invalid("/mode", "missing required key")]);
    }
}
