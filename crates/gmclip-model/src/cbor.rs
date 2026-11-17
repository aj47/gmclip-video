//! Deterministic CBOR (RFC 8949 §4.2.1), the model's storage form (SPEC §4.4).
//!
//! ciborium already writes the preferred serialisation: shortest integer heads,
//! definite lengths, and each float in the shortest of half/single/double that keeps its
//! value exactly. What it does not do is order map keys, so every value goes through
//! [`ciborium::Value`] and [`canonicalize`] before it is written:
//! - map entries sorted by the **bytewise lexicographic order of their encoded keys**
//!   (§4.2.1, not RFC 7049's length-first order), duplicate keys refused;
//! - every NaN written as the half-precision quiet NaN `f9 7e00` (§4.2.2), and −0.0
//!   written as 0.0 (gmclip's profile: `PartialEq` treats them as equal, so they must
//!   not encode differently or a diff could miss a change);
//! - tags are kept; indefinite lengths cannot occur (`Value` has none).
//!
//! Decoding is bounded (`MAX_DEPTH`, `MAX_LEN`), so hostile bytes from an untrusted
//! project (R-STORE-9, FZ5) cannot recurse or allocate without limit.

use ciborium::Value;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::entity::ModelError;

/// Nesting limit when decoding.
pub const MAX_DEPTH: usize = 128;
/// Size limit for one encoded value (a field, an entity, an op row).
pub const MAX_LEN: usize = 64 << 20;

/// Encodes `v` canonically. Panics only if `v`'s `Serialize` impl itself fails, which
/// no model type does (maps with non-string keys are allowed in CBOR).
pub fn to_vec<T: Serialize + ?Sized>(v: &T) -> Vec<u8> {
    try_to_vec(v).expect("model values always serialise to CBOR")
}

/// Encodes `v` canonically, or reports why its `Serialize` impl failed or why the value
/// has no canonical form (duplicate map keys).
pub fn try_to_vec<T: Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, ModelError> {
    let mut value = Value::serialized(v).map_err(|e| ModelError::BadValue(e.to_string()))?;
    canonicalize(&mut value)?;
    let mut out = Vec::new();
    ciborium::into_writer(&value, &mut out).map_err(|e| ModelError::BadValue(e.to_string()))?;
    Ok(out)
}

/// Decodes one canonical value. Trailing bytes are refused.
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ModelError> {
    if bytes.len() > MAX_LEN {
        return Err(ModelError::BadValue("too large".into()));
    }
    let mut cursor = bytes;
    let v = ciborium::de::from_reader_with_recursion_limit(&mut cursor, MAX_DEPTH)
        .map_err(|e| ModelError::BadValue(shape_only(&e)))?;
    if !cursor.is_empty() {
        return Err(ModelError::BadValue("trailing bytes".into()));
    }
    Ok(v)
}

/// Decodes to a generic value and checks it is in canonical form: re-encoding gives
/// the same bytes. Used on untrusted input (FZ5) and by the encode/decode fixpoint test.
pub fn is_canonical(bytes: &[u8]) -> bool {
    match from_slice::<Value>(bytes) {
        Ok(v) => try_to_vec(&v).is_ok_and(|b| b == bytes),
        Err(_) => false,
    }
}

fn shape_only<E: core::fmt::Debug>(e: &ciborium::de::Error<E>) -> String {
    use ciborium::de::Error;
    match e {
        Error::Io(_) => "truncated".into(),
        Error::Syntax(at) => format!("syntax error at byte {at}"),
        Error::Semantic(at, _) => match at {
            Some(at) => format!("type mismatch at byte {at}"),
            None => "type mismatch".into(),
        },
        Error::RecursionLimitExceeded => "nested too deeply".into(),
    }
}

/// Puts a value tree into RFC 8949 §4.2.1 deterministic form in place.
pub fn canonicalize(v: &mut Value) -> Result<(), ModelError> {
    match v {
        Value::Float(f) if f.is_nan() => *f = f64::NAN,
        Value::Float(f) if *f == 0.0 => *f = 0.0,
        Value::Array(items) => {
            for i in items {
                canonicalize(i)?;
            }
        }
        Value::Tag(_, inner) => canonicalize(inner)?,
        Value::Map(entries) => {
            let mut keyed = Vec::with_capacity(entries.len());
            for (mut k, mut val) in core::mem::take(entries) {
                canonicalize(&mut k)?;
                canonicalize(&mut val)?;
                let mut kb = Vec::new();
                ciborium::into_writer(&k, &mut kb)
                    .map_err(|e| ModelError::BadValue(e.to_string()))?;
                keyed.push((kb, k, val));
            }
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            if keyed.windows(2).any(|w| w[0].0 == w[1].0) {
                return Err(ModelError::BadValue("duplicate map key".into()));
            }
            *entries = keyed.into_iter().map(|(_, k, v)| (k, v)).collect();
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn preferred_serialisation_of_numbers() {
        assert_eq!(hex(&to_vec(&0u64)), "00");
        assert_eq!(hex(&to_vec(&24u64)), "1818");
        assert_eq!(hex(&to_vec(&-1i64)), "20");
        assert_eq!(hex(&to_vec(&1.0f64)), "f93c00");
        assert_eq!(hex(&to_vec(&1.5f64)), "f93e00");
        assert_eq!(hex(&to_vec(&100000.0f64)), "fa47c35000");
        assert_eq!(hex(&to_vec(&1.1f64)), "fb3ff199999999999a");
        assert_eq!(hex(&to_vec(&f64::NAN)), "f97e00");
        assert_eq!(hex(&to_vec(&-f64::NAN)), "f97e00");
        assert_eq!(hex(&to_vec(&f64::INFINITY)), "f97c00");
        assert_eq!(hex(&to_vec(&-0.0f64)), "f90000");
    }

    #[test]
    fn map_keys_sort_bytewise_not_length_first() {
        // RFC 8949 §4.2.1: bytewise order of encoded keys. Text keys "b" (61 62) and
        // "aa" (62 61 61): bytewise "b" first (0x61 < 0x62); length-first would agree,
        // but an integer key 10 (0x0a) sorts before -1 (0x20) and "a" (0x61).
        let mut m = BTreeMap::new();
        m.insert("aa".to_owned(), 1u8);
        m.insert("b".to_owned(), 2u8);
        assert_eq!(hex(&to_vec(&m)), "a261620262616101");
        let v = Value::Map(vec![
            (Value::Integer((-1).into()), Value::Null),
            (Value::Text("a".into()), Value::Null),
            (Value::Integer(10.into()), Value::Null),
        ]);
        assert_eq!(hex(&to_vec(&v)), "a30af620f66161f6");
    }

    #[test]
    fn struct_fields_come_out_sorted() {
        #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
        struct S {
            zeta: u8,
            alpha: u8,
            mid: Vec<u8>,
        }
        let s = S {
            zeta: 1,
            alpha: 2,
            mid: vec![3],
        };
        let b = to_vec(&s);
        // a3 · "mid"(63 6d6964) [03] · "zeta"(64 7a657461) 01 · "alpha"(65 616c706861) 02
        assert_eq!(hex(&b), "a3636d69648103647a6574610165616c70686102");
        assert!(is_canonical(&b));
        assert_eq!(from_slice::<S>(&b).unwrap(), s);
    }

    #[test]
    fn non_canonical_and_hostile_bytes_are_refused() {
        // unsorted map {"b":0,"a":0}
        assert!(!is_canonical(&[0xa2, 0x61, 0x62, 0x00, 0x61, 0x61, 0x00]));
        // 1 written in two bytes
        assert!(!is_canonical(&[0x18, 0x01]));
        // indefinite-length array
        assert!(!is_canonical(&[0x9f, 0x01, 0xff]));
        // trailing bytes
        assert!(from_slice::<u8>(&[0x01, 0x02]).is_err());
        // truncated
        assert!(from_slice::<String>(&[0x65, 0x61]).is_err());
        // deep nesting
        let deep = vec![0x81u8; 10_000];
        assert!(from_slice::<Value>(&deep).is_err());
        // a huge declared length does not allocate it
        assert!(
            from_slice::<Vec<u8>>(&[0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err()
        );
    }
}
