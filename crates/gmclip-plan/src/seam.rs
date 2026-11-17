//! The M1 seam (A18, F15): is a planner's RenderPlan the converter's?
//!
//! F15: "identical integers (frames, indices, samples); `crop`/`place` within 1e-6 px
//! except `place.h` ≤ 0.5%, listed; `render_sig` is computed after quantising floats to
//! 1/1024". [`compare`] applies exactly these normalisations, each one **listed** in
//! [`Seam::listed`] when it fired, and reports everything else that differs:
//!
//! 1. converter provenance that no model holds is dropped from both sides:
//!    `meta.generator`, `meta.inputs`, `meta.import_report`, `checks.reference` (the
//!    files the converter read and the MP4 the reference wrote), and `meta.content_id` (Clippy's
//!    id, deferred to M2);
//! 2. names the model does not keep are taken by position: part, source and envelope
//!    ids (and every reference to a source id);
//! 3. `place.h` within 0.5% of the converter's takes the converter's value (the model
//!    derives h from the crop's aspect, SPEC §4.2);
//! 4. a hold's provenance is taken from the converter when the planner's has none (the
//!    model's `ContentHold` keeps the frame, not the plan key it came from).
//!
//! What is left must match: integers and strings exactly, floats within 1e-6, and the
//! [`render_sig_material`] of the two **normalised** plans (RP rule 9's canonical JSON
//! before hashing) must be byte-equal. The raw plans' `render_sig`s still differ — by the
//! generator name, the positional part and source ids and `place.h` — so an export
//! record made from the converter's plan reads as stale against the planner's; A18's
//! "render_sig equal after quantisation" holds for the normalised pair only.

use serde_json::Value;

use crate::Plan;

/// F15's float tolerance (px, dB, ratio).
pub const FLOAT_TOL: f64 = 1e-6;
/// F15's `place.h` tolerance (relative).
pub const PLACE_H_TOL: f64 = 0.005;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seam {
    /// What differs after the normalisations: empty = equal per F15.
    pub differences: Vec<String>,
    /// The normalisations that fired.
    pub listed: Vec<String>,
}

impl Seam {
    pub fn equal(&self) -> bool {
        self.differences.is_empty()
    }
}

fn ptr<'v>(v: &'v mut Value, path: &[&str]) -> Option<&'v mut Value> {
    let mut cur = v;
    for k in path {
        cur = cur.as_object_mut()?.get_mut(*k)?;
    }
    Some(cur)
}

fn drop_key(v: &mut Value, path: &[&str], key: &str) -> bool {
    ptr(v, path)
        .and_then(Value::as_object_mut)
        .is_some_and(|o| o.remove(key).is_some())
}

/// The id of array element `i` under `key`.
fn ids(v: &Value, key: &str) -> Vec<Option<String>> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn rename_sources(v: &mut Value, map: &[(String, String)]) {
    let re = |s: &mut Value| {
        if let Some(x) = s.as_str()
            && let Some((_, to)) = map.iter().find(|(f, _)| f == x)
        {
            *s = Value::String(to.clone());
        }
    };
    if let Some(a) = v.get_mut("sources").and_then(Value::as_array_mut) {
        for s in a {
            if let Some(id) = s.get_mut("id") {
                re(id);
            }
        }
    }
    if let Some(parts) = v.get_mut("parts").and_then(Value::as_array_mut) {
        for p in parts {
            for k in ["picture", "audio"] {
                if let Some(s) = p.get_mut(k).and_then(|x| x.get_mut("source")) {
                    re(s);
                }
            }
            if let Some(ls) = p.get_mut("layers").and_then(Value::as_array_mut) {
                for l in ls {
                    if let Some(s) = l.get_mut("picture_source") {
                        re(s);
                    }
                }
            }
        }
    }
}

/// Compares the converter's plan (`reference`) with the planner's. See the module docs.
pub fn compare(reference: &Plan, planner: &Plan) -> Seam {
    let mut seam = Seam::default();
    let (Ok(mut r), Ok(mut c)) = (
        serde_json::to_value(reference),
        serde_json::to_value(planner),
    ) else {
        seam.differences.push("a plan does not serialise".into());
        return seam;
    };
    // 1. converter provenance
    for v in [&mut r, &mut c] {
        for k in ["generator", "inputs", "import_report"] {
            drop_key(v, &["meta"], k);
        }
        drop_key(v, &["checks"], "reference");
    }
    if drop_key(&mut r, &["meta"], "content_id") {
        seam.listed
            .push("meta.content_id: deferred to M2 (Clippy hand-off)".into());
    }
    drop_key(&mut c, &["meta"], "content_id");
    // 2. positional names
    let mut renamed = Vec::new();
    for key in ["parts", "sources"] {
        let (ri, ci) = (ids(&r, key), ids(&c, key));
        if ri.len() == ci.len() {
            let map: Vec<(String, String)> = ci
                .iter()
                .zip(&ri)
                .filter_map(|(a, b)| Some((a.clone()?, b.clone()?)))
                .collect();
            if key == "sources" {
                rename_sources(&mut c, &map);
            } else if let Some(a) = c.get_mut("parts").and_then(Value::as_array_mut) {
                for (p, (_, to)) in a.iter_mut().zip(&map) {
                    p["id"] = Value::String(to.clone());
                }
            }
            renamed.push(format!("{} {key}", map.len()));
        }
    }
    {
        let ri = r
            .pointer("/audio/envelopes")
            .map(|e| ids(&serde_json::json!({ "e": e }), "e"))
            .unwrap_or_default();
        if let Some(a) = c
            .pointer_mut("/audio/envelopes")
            .and_then(Value::as_array_mut)
            && a.len() == ri.len()
        {
            for (e, id) in a.iter_mut().zip(&ri) {
                if let Some(id) = id {
                    e["id"] = Value::String(id.clone());
                }
            }
            renamed.push(format!("{} envelope", ri.len()));
        }
    }
    seam.listed
        .push(format!("ids by position: {}", renamed.join(", ")));
    // 3./4. per layer
    let (mut hs, mut hmax, mut holds) = (0usize, 0f64, 0usize);
    if let (Some(rp), Some(cp)) = (
        r.get("parts").and_then(Value::as_array).cloned(),
        c.get_mut("parts").and_then(Value::as_array_mut),
    ) {
        for (rpart, cpart) in rp.iter().zip(cp.iter_mut()) {
            let (Some(rl), Some(cl)) = (
                rpart.get("layers").and_then(Value::as_array),
                cpart.get_mut("layers").and_then(Value::as_array_mut),
            ) else {
                continue;
            };
            for (a, b) in rl.iter().zip(cl.iter_mut()) {
                if let (Some(ha), Some(hb)) = (
                    a.pointer("/place/h").and_then(Value::as_f64),
                    b.pointer("/place/h").and_then(Value::as_f64),
                ) && ha != hb
                    && ha > 0.0
                {
                    let rel = (ha - hb).abs() / ha;
                    if rel <= PLACE_H_TOL {
                        b["place"]["h"] = a["place"]["h"].clone();
                        hs += 1;
                        hmax = hmax.max(rel);
                    }
                }
                if a.pointer("/time_map/mode").and_then(Value::as_str) == Some("hold")
                    && b.pointer("/time_map/mode").and_then(Value::as_str) == Some("hold")
                    && b.pointer("/time_map/provenance").is_none()
                    && let Some(p) = a.pointer("/time_map/provenance")
                {
                    b["time_map"]["provenance"] = p.clone();
                    holds += 1;
                }
            }
        }
    }
    if hs > 0 {
        seam.listed.push(format!(
            "place.h: {hs} layer(s) within 0.5% (largest {:.4}%) take the converter's",
            hmax * 100.0
        ));
    }
    if holds > 0 {
        seam.listed.push(format!(
            "hold provenance: {holds} layer(s) take the converter's"
        ));
    }
    diff("", &r, &c, &mut seam.differences);
    if seam.differences.is_empty() && render_sig_material(&r) != render_sig_material(&c) {
        seam.differences
            .push("render_sig material differs after quantisation".into());
    }
    seam
}

fn diff(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    if out.len() >= 50 {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    Some(vb) => diff(&format!("{path}/{k}"), va, vb, out),
                    None => out.push(format!("{path}/{k}: only in the converter's plan")),
                }
            }
            for k in y.keys().filter(|k| !x.contains_key(*k)) {
                out.push(format!("{path}/{k}: only in the planner's plan"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{path}: {} vs {} elements", x.len(), y.len()));
            }
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                diff(&format!("{path}/{i}"), va, vb, out);
            }
        }
        (Value::Number(x), Value::Number(y)) => {
            let same = match (x.as_i64(), y.as_i64(), x.as_u64(), y.as_u64()) {
                (Some(p), Some(q), _, _) => p == q,
                (_, _, Some(p), Some(q)) => p == q,
                _ => match (x.as_f64(), y.as_f64()) {
                    (Some(p), Some(q)) => (p - q).abs() <= FLOAT_TOL,
                    _ => false,
                },
            };
            // an integer field must not turn into a float or back
            let kinds_match = x.is_f64() == y.is_f64();
            if !same || !kinds_match {
                out.push(format!("{path}: {x} vs {y}"));
            }
        }
        _ => {
            if a != b {
                out.push(format!("{path}: differs"));
            }
        }
    }
}

/// RP rule 9's material before hashing, written the way B-16's export record writes it:
/// RFC 8785-style canonical JSON (keys sorted, no whitespace, integral numbers without a
/// fraction, `-0` as `0`), every `path` member removed except a `text_styles` entry that
/// happens to be named `path`, and every float quantised to 1/1024 with ties to even.
pub fn render_sig_material(v: &Value) -> String {
    fn num(n: &serde_json::Number, out: &mut String) {
        if let Some(i) = n.as_i64() {
            out.push_str(&i.to_string());
        } else if let Some(u) = n.as_u64() {
            out.push_str(&u.to_string());
        } else {
            let x = n.as_f64().unwrap_or(0.0) * 1024.0;
            let q = x.round_ties_even() / 1024.0;
            if q == 0.0 {
                out.push('0');
            } else if q.fract() == 0.0 && q.abs() < 9_007_199_254_740_992.0 {
                out.push_str(&(q as i64).to_string());
            } else {
                out.push_str(&q.to_string());
            }
        }
    }
    fn write(v: &Value, map_of_styles: bool, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m
                    .keys()
                    .filter(|k| map_of_styles || k.as_str() != "path")
                    .collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write(&m[k], !map_of_styles && k == "text_styles", out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, false, out);
                }
                out.push(']');
            }
            Value::Number(n) => num(n, out),
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(v, false, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Plan {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/examples/h79-v4.renderplan.json"
        );
        crate::parse(&std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn a_plan_equals_itself() {
        let p = example();
        assert!(compare(&p, &p).equal());
    }

    #[test]
    fn the_listed_normalisations_fire_and_nothing_else_passes() {
        let r = example();
        let mut c = r.clone();
        c.parts[0].id = crate::scalars::Id::new("p01").unwrap();
        c.sources[0].id = crate::scalars::Id::new("s1").unwrap();
        for p in &mut c.parts {
            p.picture.source = crate::scalars::Id::new("s1").unwrap();
        }
        c.parts[0].layers[0].place.h *= 1.004;
        c.parts[1].layers[0].crop.x += 1e-9;
        let s = compare(&r, &c);
        assert!(s.equal(), "{:?}", s.differences);
        assert!(s.listed.iter().any(|l| l.starts_with("place.h: 1 layer")));

        let mut d = r.clone();
        d.parts[0].layers[0].place.h *= 1.01;
        assert!(!compare(&r, &d).equal());
        let mut e = r.clone();
        e.parts[0].picture.first_frame += 1;
        assert!(!compare(&r, &e).equal());
        let mut f = r.clone();
        f.parts[0].layers[0].crop.x += 1e-3;
        assert!(!compare(&r, &f).equal());
    }

    #[test]
    fn material_drops_paths_and_quantises() {
        let a = serde_json::json!({"b": 1.0001, "path": "/x", "a": [{"path": 1, "q": 2}],
            "c": -0.0, "d": 0.5, "t": 1.0 / 2048.0, "u": 3.0 / 2048.0,
            "text_styles": {"path": {"size": 50.0}}});
        assert_eq!(
            render_sig_material(&a),
            r#"{"a":[{"q":2}],"b":1,"c":0,"d":0.5,"t":0,"text_styles":{"path":{"size":50}},"u":0.001953125}"#
        );
    }
}
