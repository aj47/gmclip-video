//! Project fixtures for tests (`gmclip.project-fixture/0`): a project's entity rows and
//! analysis blobs as one JSON file, written back to a fresh project database with
//! [`gmclip_store::write_fresh`] (one checkpoint, no op rows), so a test opens the same
//! document every time without the media it was made from being analysed again.
//!
//! - `rows`: `[kind, id, rev, author, body]` per entity; `body` is the entity's value as
//!   JSON (its canonical CBOR read as JSON, re-encoded canonically on load).
//! - `analysis`: `{owner, kind, revision, body}`; a JSON `body` is stored as its bytes.
//! - A `source` row whose `path` is relative names a file under the lab media folder
//!   (`GMCLIP_LAB_MEDIA`), resolved against `media_root` on load.
#![doc(hidden)]

use std::path::Path;

use gmclip_model::entities::Source;
use gmclip_model::{Author, Doc, cbor};
use serde_json::{Value, json};

/// The fixture schema this module reads and writes.
pub const SCHEMA: &str = "gmclip.project-fixture/0";

/// A document and its analysis blobs as a fixture (sources' paths as they are).
pub fn dump(doc: &Doc, analysis: &[(String, String, String, Vec<u8>)]) -> Result<Value, String> {
    let mut rows = Vec::new();
    for e in doc.entity_ids() {
        let Some(body) = doc.encode(e) else { continue };
        let v: Value =
            cbor::from_slice(&body.0).map_err(|x| format!("{} {}: {x}", e.kind.0, e.id))?;
        if cbor::to_vec(&v) != body.0 {
            return Err(format!(
                "{} {}: does not round-trip as JSON",
                e.kind.0, e.id
            ));
        }
        let by = doc.last_author(e).unwrap_or(Author::System);
        rows.push(json!([
            e.kind.0,
            e.id.to_string(),
            doc.entity_rev(e).unwrap_or(doc.rev()),
            serde_json::to_value(by).map_err(|x| x.to_string())?,
            v
        ]));
    }
    let analysis: Vec<Value> = analysis
        .iter()
        .map(|(o, k, r, b)| {
            let body = serde_json::from_slice::<Value>(b)
                .unwrap_or_else(|_| json!({"utf8": String::from_utf8_lossy(b)}));
            json!({"owner": o, "kind": k, "revision": r, "body": body})
        })
        .collect();
    Ok(json!({"schema": SCHEMA, "rev": doc.rev(), "rows": rows, "analysis": analysis}))
}

/// The document a fixture holds, with relative source paths resolved against
/// `media_root`, and its analysis blobs.
#[allow(clippy::type_complexity)]
pub fn load(
    fx: &Value,
    media_root: &Path,
) -> Result<(Doc, Vec<(String, String, String, Vec<u8>)>), String> {
    if fx["schema"] != SCHEMA {
        return Err(format!("not a {SCHEMA} fixture"));
    }
    let mut doc = Doc::new();
    for r in fx["rows"].as_array().ok_or("no rows")? {
        let kind = r[0]
            .as_str()
            .and_then(gmclip_model::kinds::kind_by_name)
            .ok_or("a row of an unknown kind")?;
        let id = r[1]
            .as_str()
            .and_then(|s| gmclip_model::Ulid::parse(s).ok())
            .ok_or("a row without an id")?;
        let rev = r[2].as_u64().ok_or("a row without a rev")?;
        let by: Author = serde_json::from_value(r[3].clone()).map_err(|e| e.to_string())?;
        let mut bytes = cbor::to_vec(&r[4]);
        if kind.0 == "source" {
            let mut s: Source = cbor::from_slice(&bytes).map_err(|e| e.to_string())?;
            if Path::new(&s.path).is_relative() {
                s.path = media_root.join(&s.path).to_string_lossy().into_owned();
            }
            bytes = cbor::to_vec(&s);
        }
        doc.load_row(kind, id, rev, by, &bytes)
            .map_err(|e| format!("{} {id}: {e:?}", kind.0))?;
    }
    doc.set_rev(fx["rev"].as_u64().ok_or("no rev")?);
    let mut analysis = Vec::new();
    for a in fx["analysis"].as_array().into_iter().flatten() {
        let s = |k: &str| a[k].as_str().map(str::to_owned).ok_or("an analysis key");
        let body = match a["body"].get("utf8").and_then(Value::as_str) {
            Some(t) if a["body"].as_object().is_some_and(|o| o.len() == 1) => t.as_bytes().to_vec(),
            _ => serde_json::to_vec(&a["body"]).map_err(|e| e.to_string())?,
        };
        analysis.push((s("owner")?, s("kind")?, s("revision")?, body));
    }
    Ok((doc, analysis))
}

/// Writes the fixture `fx` as a fresh project database at `db`.
pub fn write_db(fx: &Value, media_root: &Path, db: &Path) -> Result<(), String> {
    let (doc, analysis) = load(fx, media_root)?;
    gmclip_store::write_fresh(db, &doc, &analysis).map_err(|e| e.to_string())
}

/// The frozen stream project `name` ([`gmclip_model::lab::project`]) written as a fresh project database at `db`, its
/// sources resolved against the lab media folder ([`gmclip_model::lab::root`]): what a
/// lab test opens instead of importing the stream.
pub fn write_lab_project(name: &str, db: &Path) -> Result<Value, String> {
    let p = gmclip_model::lab::project(name);
    let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let fx: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    write_db(&fx, &gmclip_model::lab::root(), db)?;
    Ok(fx["summary"].clone())
}
