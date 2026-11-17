//! Strict JSON reading: document order kept, duplicate keys refused.
//!
//! `serde_json::Value` keeps the last of two equal keys, which would let a plan say
//! `"visible": true, …, "visible": false` and have one silently ignored (RP rule 11).
//! [`parse`] builds a [`JVal`] that keeps every key in document order and reports a
//! duplicate with its JSON Pointer.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Number;

use crate::error::{PlanError, join_index, join_key};

/// A JSON value with object keys in document order.
#[derive(Clone, Debug, PartialEq)]
pub enum JVal {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
    Arr(Vec<JVal>),
    Obj(Vec<(String, JVal)>),
}

impl JVal {
    /// The JSON type name, as the schema's `type` spells it (integers are "integer").
    pub fn type_name(&self) -> &'static str {
        match self {
            JVal::Null => "null",
            JVal::Bool(_) => "boolean",
            JVal::Num(n) if n.is_i64() || n.is_u64() => "integer",
            JVal::Num(_) => "number",
            JVal::Str(_) => "string",
            JVal::Arr(_) => "array",
            JVal::Obj(_) => "object",
        }
    }

    /// The value of `key` in an object.
    pub fn get(&self, key: &str) -> Option<&JVal> {
        match self {
            JVal::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Parses `bytes` as one JSON document. Errors are `Invalid` with the pointer of a
/// duplicate key, or `""` and a line/column for a syntax error.
pub fn parse(bytes: &[u8]) -> Result<JVal, PlanError> {
    let dup = RefCell::new(None);
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let seed = Seed {
        path: String::new(),
        dup: &dup,
    };
    let result = seed.deserialize(&mut de).and_then(|v| de.end().map(|()| v));
    match result {
        Ok(v) => Ok(v),
        Err(e) => match dup.into_inner() {
            Some(path) => Err(PlanError::invalid(path, "duplicate key")),
            None => Err(PlanError::invalid(
                "",
                format!(
                    "not valid JSON ({} at line {}, column {})",
                    category(&e),
                    e.line(),
                    e.column()
                ),
            )),
        },
    }
}

fn category(e: &serde_json::Error) -> &'static str {
    match e.classify() {
        serde_json::error::Category::Io => "read error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "unrepresentable value",
        serde_json::error::Category::Eof => "unexpected end of input",
    }
}

struct Seed<'a> {
    path: String,
    dup: &'a RefCell<Option<String>>,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = JVal;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<JVal, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = JVal;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<JVal, E> {
        Ok(JVal::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<JVal, E> {
        Ok(JVal::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<JVal, E> {
        Ok(JVal::Num(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<JVal, E> {
        Ok(JVal::Num(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<JVal, E> {
        // serde_json reads the integer literal `-0` as −0.0; it is the integer 0.
        if v == 0.0 && v.is_sign_negative() {
            return Ok(JVal::Num(0.into()));
        }
        Number::from_f64(v)
            .map(JVal::Num)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<JVal, E> {
        Ok(JVal::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<JVal, E> {
        Ok(JVal::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JVal, A::Error> {
        let mut out = Vec::new();
        loop {
            let seed = Seed {
                path: join_index(&self.path, out.len()),
                dup: self.dup,
            };
            match seq.next_element_seed(seed)? {
                Some(v) => out.push(v),
                None => return Ok(JVal::Arr(out)),
            }
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JVal, A::Error> {
        let mut out: Vec<(String, JVal)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            let path = join_key(&self.path, &key);
            if !seen.insert(key.clone()) {
                *self.dup.borrow_mut() = Some(path);
                return Err(de::Error::custom("duplicate key"));
            }
            let v = map.next_value_seed(Seed {
                path,
                dup: self.dup,
            })?;
            out.push((key, v));
        }
        Ok(JVal::Obj(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_document_order() {
        let v = parse(br#"{"b": 1, "a": [true, null, "x", 1.5]}"#).unwrap();
        let JVal::Obj(kv) = &v else { panic!() };
        assert_eq!(kv[0].0, "b");
        assert_eq!(kv[1].0, "a");
        assert_eq!(v.get("b").unwrap().type_name(), "integer");
    }

    #[test]
    fn duplicate_keys_name_their_path() {
        let e = parse(br#"{"a": [{"x": 1}, {"x": 1, "x": 2}]}"#).unwrap_err();
        assert_eq!(e, PlanError::invalid("/a/1/x", "duplicate key"));
    }

    #[test]
    fn syntax_errors_name_the_line() {
        let e = parse(b"{\n  \"a\": }").unwrap_err();
        assert_eq!(e.path(), "");
        assert!(e.reason().contains("line 2"), "{e}");
        let trailing = parse(b"{} x").unwrap_err();
        assert!(trailing.reason().contains("syntax"), "{trailing}");
    }
}
