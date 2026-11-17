//! Analysis blobs: immutable, columnar, keyed by `(owner, kind, revision)` (SPEC §4.4,
//! `gmclip-store`'s `analysis` table).
//!
//! - **owner**: the source or sync-group id whose voice track was analysed.
//! - **kind**: what was computed ([`Kind`]).
//! - **revision**: which algorithm, parameters and input ([`revision`]). A blob is never
//!   rewritten: new parameters or a new input fingerprint give a new revision, so a
//!   cached blob is valid exactly when its key matches. Nothing is stored on failure
//!   ([`cached`]).
//!
//! The quiet grid has no blob of its own: its threshold is measured per span (§5.5), so
//! it is derived on demand from the cached `energy_40ms` blob
//! ([`crate::silence::QuietGrid::for_span`]), which is cheap (one pass over 25 frames/s).
//!
//! The body is deterministic CBOR (RFC 8949 §4.2.1, [`gmclip_model::cbor`]):
//!
//! ```text
//! { "v": 1, "kind": text, "params": { text: int | text | float },
//!   "columns": [ { "name": text, "type": "f32le" | "i64le" | "u8" | "utf8",
//!                  "len": uint, "data": bytes, ("offsets": bytes) } ] }
//! ```
//!
//! Numeric columns are packed little-endian bytes (one CBOR byte string each, not an
//! array of numbers), `utf8` columns are the concatenated text plus `len + 1` `i64le`
//! offsets. Decoding is bounded by the model's CBOR limits (64 MiB, depth 128).

use std::collections::BTreeMap;

use ciborium::Value;
use gmclip_model::cbor;

use crate::AnalysisError;
use crate::energy::{Energy, Hop};
use crate::peaks::Peaks;

/// Blob format version.
pub const VERSION: u64 = 1;

/// The stored analysis kinds.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Energy10,
    Energy20,
    Energy40,
    Peaks100,
    /// Silero VAD speech regions.
    Vad,
    /// ASR words (gmclip-ml).
    AsrWords,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Energy10 => "energy_10ms",
            Kind::Energy20 => "energy_20ms",
            Kind::Energy40 => "energy_40ms",
            Kind::Peaks100 => "peaks_100hz",
            Kind::Vad => "vad_speech",
            Kind::AsrWords => "asr_words",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "energy_10ms" => Kind::Energy10,
            "energy_20ms" => Kind::Energy20,
            "energy_40ms" => Kind::Energy40,
            "peaks_100hz" => Kind::Peaks100,
            "vad_speech" => Kind::Vad,
            "asr_words" => Kind::AsrWords,
            _ => return None,
        })
    }

    pub const fn energy(hop: Hop) -> Self {
        match hop {
            Hop::Ms10 => Kind::Energy10,
            Hop::Ms20 => Kind::Energy20,
            Hop::Ms40 => Kind::Energy40,
        }
    }
}

/// `(owner, kind, revision)`: the `analysis` table's primary key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnalysisKey {
    pub owner: String,
    pub kind: String,
    pub revision: String,
}

/// A revision string: `<algorithm>@<algorithm version>/<input fingerprint>`. The input
/// fingerprint names the bytes analysed (source sha256, track, and range if partial).
pub fn revision(algorithm: &str, algorithm_version: u32, input_fingerprint: &str) -> String {
    format!("{algorithm}@{algorithm_version}/{input_fingerprint}")
}

/// A parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Int(i64),
    Text(String),
    Float(f64),
}

/// One column.
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    F32(Vec<f32>),
    I64(Vec<i64>),
    U8(Vec<u8>),
    Utf8(Vec<String>),
}

impl Column {
    pub fn len(&self) -> usize {
        match self {
            Column::F32(v) => v.len(),
            Column::I64(v) => v.len(),
            Column::U8(v) => v.len(),
            Column::Utf8(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A decoded blob.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Blob {
    pub kind: String,
    pub params: BTreeMap<String, Param>,
    pub columns: Vec<(String, Column)>,
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn pack<T, const N: usize>(v: &[T], f: impl Fn(&T) -> [u8; N]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * N);
    for x in v {
        out.extend_from_slice(&f(x));
    }
    out
}

impl Blob {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind: kind.as_str().to_owned(),
            ..Self::default()
        }
    }

    pub fn param(mut self, k: &str, v: Param) -> Self {
        self.params.insert(k.to_owned(), v);
        self
    }

    pub fn column(mut self, name: &str, c: Column) -> Self {
        self.columns.push((name.to_owned(), c));
        self
    }

    pub fn get(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }

    pub fn int(&self, k: &str) -> Option<i64> {
        match self.params.get(k)? {
            Param::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Canonical CBOR bytes.
    pub fn encode(&self) -> Result<Vec<u8>, AnalysisError> {
        let params = self
            .params
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Param::Int(i) => Value::Integer((*i).into()),
                    Param::Text(s) => text(s),
                    Param::Float(f) => Value::Float(*f),
                };
                (text(k), v)
            })
            .collect();
        let mut cols = Vec::with_capacity(self.columns.len());
        for (name, c) in &self.columns {
            let mut m = vec![(text("name"), text(name))];
            let (ty, data) = match c {
                Column::F32(v) => ("f32le", pack(v, |x| x.to_le_bytes())),
                Column::I64(v) => ("i64le", pack(v, |x| x.to_le_bytes())),
                Column::U8(v) => ("u8", v.clone()),
                Column::Utf8(v) => {
                    let mut offsets = Vec::with_capacity(v.len() + 1);
                    let mut bytes = Vec::new();
                    offsets.push(0i64);
                    for s in v {
                        bytes.extend_from_slice(s.as_bytes());
                        offsets.push(bytes.len() as i64);
                    }
                    m.push((
                        text("offsets"),
                        Value::Bytes(pack(&offsets, |x| x.to_le_bytes())),
                    ));
                    ("utf8", bytes)
                }
            };
            m.push((text("type"), text(ty)));
            m.push((text("len"), Value::Integer((c.len() as u64).into())));
            m.push((text("data"), Value::Bytes(data)));
            cols.push(Value::Map(m));
        }
        let mut v = Value::Map(vec![
            (text("v"), Value::Integer(VERSION.into())),
            (text("kind"), text(&self.kind)),
            (text("params"), Value::Map(params)),
            (text("columns"), Value::Array(cols)),
        ]);
        cbor::canonicalize(&mut v).map_err(|_| AnalysisError::Corrupt("duplicate key"))?;
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).map_err(|_| AnalysisError::Corrupt("encode"))?;
        if out.len() > cbor::MAX_LEN {
            // it could be stored but never decoded again
            return Err(AnalysisError::Unsupported("blob larger than 64 MiB"));
        }
        Ok(out)
    }

    /// Decodes and validates a blob (untrusted input: bounded, every length checked).
    pub fn decode(bytes: &[u8]) -> Result<Self, AnalysisError> {
        let v: Value = cbor::from_slice(bytes).map_err(|_| AnalysisError::Corrupt("cbor"))?;
        let Value::Map(mut top) = v else {
            return Err(AnalysisError::Corrupt("not a map"));
        };
        fn field(m: &mut [(Value, Value)], k: &str) -> Option<Value> {
            // moved out, not cloned: a column's bytes are copied once
            m.iter_mut()
                .find(|(key, _)| matches!(key, Value::Text(t) if t == k))
                .map(|(_, v)| std::mem::replace(v, Value::Null))
        }
        match field(&mut top, "v") {
            Some(Value::Integer(i)) if u64::try_from(i).ok() == Some(VERSION) => {}
            _ => return Err(AnalysisError::Corrupt("version")),
        }
        let Some(Value::Text(kind)) = field(&mut top, "kind") else {
            return Err(AnalysisError::Corrupt("kind"));
        };
        let mut blob = Blob {
            kind,
            ..Blob::default()
        };
        let Some(Value::Map(params)) = field(&mut top, "params") else {
            return Err(AnalysisError::Corrupt("params"));
        };
        for (k, v) in params {
            let Value::Text(k) = k else {
                return Err(AnalysisError::Corrupt("param key"));
            };
            let p = match v {
                Value::Integer(i) => {
                    Param::Int(i64::try_from(i).map_err(|_| AnalysisError::Corrupt("param int"))?)
                }
                Value::Text(t) => Param::Text(t),
                Value::Float(f) => Param::Float(f),
                _ => return Err(AnalysisError::Corrupt("param type")),
            };
            blob.params.insert(k, p);
        }
        let Some(Value::Array(cols)) = field(&mut top, "columns") else {
            return Err(AnalysisError::Corrupt("columns"));
        };
        for c in cols {
            let Value::Map(mut m) = c else {
                return Err(AnalysisError::Corrupt("column"));
            };
            let (
                Some(Value::Text(name)),
                Some(Value::Text(ty)),
                Some(Value::Integer(len)),
                Some(Value::Bytes(data)),
            ) = (
                field(&mut m, "name"),
                field(&mut m, "type"),
                field(&mut m, "len"),
                field(&mut m, "data"),
            )
            else {
                return Err(AnalysisError::Corrupt("column fields"));
            };
            let len =
                usize::try_from(u64::try_from(len).map_err(|_| AnalysisError::Corrupt("len"))?)
                    .map_err(|_| AnalysisError::Corrupt("len"))?;
            let col = match ty.as_str() {
                "f32le" => {
                    if data.len() != len.checked_mul(4).ok_or(AnalysisError::Corrupt("len"))? {
                        return Err(AnalysisError::Corrupt("f32 column length"));
                    }
                    Column::F32(
                        data.chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect(),
                    )
                }
                "i64le" => Column::I64(unpack_i64(&data, len)?),
                "u8" => {
                    if data.len() != len {
                        return Err(AnalysisError::Corrupt("u8 column length"));
                    }
                    Column::U8(data)
                }
                "utf8" => {
                    let Some(Value::Bytes(off)) = field(&mut m, "offsets") else {
                        return Err(AnalysisError::Corrupt("utf8 offsets"));
                    };
                    let off = unpack_i64(
                        &off,
                        len.checked_add(1).ok_or(AnalysisError::Corrupt("len"))?,
                    )?;
                    if off.windows(2).any(|w| w[1] < w[0]) {
                        return Err(AnalysisError::Corrupt("utf8 offsets not monotonic"));
                    }
                    let mut v = Vec::with_capacity(len.min(data.len() + 1));
                    for w in off.windows(2) {
                        let (a, b) = (usize::try_from(w[0]), usize::try_from(w[1]));
                        let (Ok(a), Ok(b)) = (a, b) else {
                            return Err(AnalysisError::Corrupt("utf8 offset"));
                        };
                        let s = data
                            .get(a..b)
                            .ok_or(AnalysisError::Corrupt("utf8 offset"))?;
                        v.push(
                            std::str::from_utf8(s)
                                .map_err(|_| AnalysisError::Corrupt("utf8"))?
                                .to_owned(),
                        );
                    }
                    if off.first() != Some(&0)
                        || off.last().map(|&l| l as usize) != Some(data.len())
                    {
                        return Err(AnalysisError::Corrupt("utf8 offsets"));
                    }
                    Column::Utf8(v)
                }
                _ => return Err(AnalysisError::Corrupt("column type")),
            };
            blob.columns.push((name, col));
        }
        Ok(blob)
    }
}

fn unpack_i64(data: &[u8], len: usize) -> Result<Vec<i64>, AnalysisError> {
    if data.len() != len.checked_mul(8).ok_or(AnalysisError::Corrupt("len"))? {
        return Err(AnalysisError::Corrupt("i64 column length"));
    }
    Ok(data
        .chunks_exact(8)
        .map(|b| i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .collect())
}

/// Returns the blob cached under `key`, or computes, encodes and stores it. `get` and
/// `put` are the store's (`gmclip_store::Store::analysis` / `put_analysis`, which
/// refuses different bytes under an existing key); a failed
/// computation or encoding stores nothing ("never cached on failure"), and a cached
/// blob that no longer decodes is recomputed under the same key only by a caller that
/// bumps the revision (blobs are immutable).
pub fn cached<E>(
    get: impl FnOnce() -> Result<Option<Vec<u8>>, E>,
    compute: impl FnOnce() -> Result<Blob, E>,
    put: impl FnOnce(&[u8]) -> Result<(), E>,
) -> Result<Blob, E>
where
    E: From<AnalysisError>,
{
    if let Some(bytes) = get()? {
        return Ok(Blob::decode(&bytes)?);
    }
    let blob = compute()?;
    let bytes = blob.encode()?;
    put(&bytes)?;
    Ok(blob)
}

/// The algorithm names and versions that go into revisions.
pub const ENERGY_ALGORITHM: (&str, u32) = ("rms", 1);
pub const PEAKS_ALGORITHM: (&str, u32) = ("minmax", 1);

impl Energy {
    pub fn to_blob(&self, hop: Hop) -> Blob {
        Blob::new(Kind::energy(hop))
            .param("rate", Param::Int(i64::from(self.rate)))
            .param("window", Param::Int(self.window as i64))
            .param("hop", Param::Int(self.hop as i64))
            .column("db", Column::F32(self.db.clone()))
    }

    pub fn from_blob(b: &Blob) -> Result<Self, AnalysisError> {
        let rate = b.int("rate").and_then(|r| u32::try_from(r).ok());
        let window = b.int("window").and_then(|r| usize::try_from(r).ok());
        let hop = b.int("hop").and_then(|r| usize::try_from(r).ok());
        let (Some(rate), Some(window), Some(hop), Some(Column::F32(db))) =
            (rate, window, hop, b.get("db"))
        else {
            return Err(AnalysisError::Corrupt("energy blob"));
        };
        if window == 0 || hop == 0 || rate == 0 {
            return Err(AnalysisError::Corrupt("energy blob params"));
        }
        Ok(Self {
            rate,
            window,
            hop,
            db: db.clone(),
        })
    }
}

impl Peaks {
    pub fn to_blob(&self) -> Blob {
        Blob::new(Kind::Peaks100)
            .param("rate", Param::Int(i64::from(self.rate)))
            .column("min", Column::F32(self.min.clone()))
            .column("max", Column::F32(self.max.clone()))
    }

    pub fn from_blob(b: &Blob) -> Result<Self, AnalysisError> {
        let rate = b.int("rate").and_then(|r| u32::try_from(r).ok());
        let (Some(rate), Some(Column::F32(min)), Some(Column::F32(max))) =
            (rate, b.get("min"), b.get("max"))
        else {
            return Err(AnalysisError::Corrupt("peaks blob"));
        };
        if min.len() != max.len() {
            return Err(AnalysisError::Corrupt("peaks columns differ"));
        }
        Ok(Self {
            rate,
            min: min.clone(),
            max: max.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_is_canonical() {
        let b = Blob::new(Kind::AsrWords)
            .param("rate", Param::Int(16_000))
            .param("model", Param::Text("parakeet".into()))
            .param("threshold_db", Param::Float(-47.5))
            .column("start", Column::I64(vec![0, 44_100, -3]))
            .column(
                "text",
                Column::Utf8(vec!["héllo".into(), String::new(), "uh".into()]),
            )
            .column("conf", Column::F32(vec![0.5, f32::NAN, 1.0]))
            .column("flag", Column::U8(vec![1, 0, 1]));
        let bytes = b.encode().unwrap();
        assert!(cbor::is_canonical(&bytes));
        let d = Blob::decode(&bytes).unwrap();
        assert_eq!(d.kind, "asr_words");
        assert_eq!(d.params, b.params);
        assert_eq!(d.get("start"), b.get("start"));
        assert_eq!(d.get("text"), b.get("text"));
        let Some(Column::F32(c)) = d.get("conf") else {
            panic!()
        };
        assert!(c[1].is_nan() && c[2] == 1.0);
        // deterministic
        assert_eq!(b.encode().unwrap(), bytes);
    }

    #[test]
    fn energy_and_peaks_round_trip() {
        let e = Energy::at_hop(&vec![0.5; 16_000], 16_000, Hop::Ms20).unwrap();
        let b = Blob::decode(&e.to_blob(Hop::Ms20).encode().unwrap()).unwrap();
        assert_eq!(Kind::parse(&b.kind), Some(Kind::Energy20));
        assert_eq!(Energy::from_blob(&b).unwrap(), e);
        let p = Peaks::compute(&[0.1, -0.2, 0.3], 16_000).unwrap();
        assert_eq!(
            Peaks::from_blob(&Blob::decode(&p.to_blob().encode().unwrap()).unwrap()).unwrap(),
            p
        );
    }

    #[test]
    fn cached_computes_once_and_never_stores_failure() {
        let store = std::cell::RefCell::new(None::<Vec<u8>>);
        let mk = || Ok::<_, AnalysisError>(Blob::new(Kind::Vad).column("x", Column::I64(vec![1])));
        let calls = std::cell::Cell::new(0);
        for _ in 0..2 {
            let b = cached(
                || Ok(store.borrow().clone()),
                || {
                    calls.set(calls.get() + 1);
                    mk()
                },
                |bytes| {
                    *store.borrow_mut() = Some(bytes.to_vec());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(b.get("x"), Some(&Column::I64(vec![1])));
        }
        assert_eq!(calls.get(), 1);
        let empty = std::cell::RefCell::new(None::<Vec<u8>>);
        let r = cached(
            || Ok(empty.borrow().clone()),
            || Err(AnalysisError::Invalid("boom")),
            |b| {
                *empty.borrow_mut() = Some(b.to_vec());
                Ok(())
            },
        );
        assert!(r.is_err());
        assert!(empty.borrow().is_none());
    }

    #[test]
    fn hostile_bytes_are_refused_not_panicked() {
        let good = Blob::new(Kind::Vad)
            .column("start", Column::I64(vec![1, 2]))
            .column("t", Column::Utf8(vec!["ab".into()]))
            .encode()
            .unwrap();
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0xff;
            let _ = Blob::decode(&b);
            let _ = Blob::decode(&good[..i]);
        }
        assert!(Blob::decode(b"\xa0").is_err());
        assert_eq!(revision("rms", 1, "sha256:ab/t1"), "rms@1/sha256:ab/t1");
    }
}
