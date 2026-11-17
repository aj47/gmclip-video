//! The review link's write bodies (decision 0015): comments, resolve, suggested edits and
//! approve, parsed from JSON into bounded, typed values before anything reaches the
//! catalog. std + serde_json only, so FZ13 (`fuzz/fuzz_targets/fz13_review.rs`) builds
//! it by path. Every id is 1–64 ASCII alphanumerics, every time a finite number of
//! seconds in a sane range, every box inside the frame, every text ≤ 2000 characters;
//! unknown keys are refused (a typo never silently drops a field).

use serde_json::{Map, Value};

/// The longest text a link may write.
pub const MAX_TEXT: usize = 2_000;
/// The latest time (seconds on a cut) a link may name: 6 h.
pub const MAX_T: f64 = 6.0 * 3600.0;

/// An id the server accepts (cut, comment, part, export).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn obj<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, &'static str> {
    let o = v.as_object().ok_or("a JSON object")?;
    if o.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err("an unknown field");
    }
    Ok(o)
}

fn id(o: &Map<String, Value>, k: &str) -> Result<String, &'static str> {
    o.get(k)
        .and_then(Value::as_str)
        .filter(|s| valid_id(s))
        .map(str::to_owned)
        .ok_or("an id")
}

fn opt_id(o: &Map<String, Value>, k: &str) -> Result<Option<String>, &'static str> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => id(o, k).map(Some),
    }
}

fn text(o: &Map<String, Value>, k: &str, required: bool) -> Result<String, &'static str> {
    match o.get(k) {
        None | Some(Value::Null) if !required => Ok(String::new()),
        Some(Value::String(s)) if s.chars().count() <= MAX_TEXT => {
            if required && s.trim().is_empty() {
                Err("some text")
            } else {
                Ok(s.clone())
            }
        }
        _ => Err("text of at most 2000 characters"),
    }
}

fn secs(o: &Map<String, Value>, k: &str) -> Result<Option<f64>, &'static str> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|t| t.is_finite() && (0.0..=MAX_T).contains(t))
            .map(Some)
            .ok_or("seconds from 0 to 6 h"),
    }
}

fn frac(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
}

/// A box on the frame (fractions of the output frame), inside it, with w and h > 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn frame_box(o: &Map<String, Value>) -> Result<Option<FrameBox>, &'static str> {
    let Some(b) = o.get("box").filter(|b| !b.is_null()) else {
        return Ok(None);
    };
    let b = obj(b, &["x", "y", "w", "h"])?;
    let (Some(x), Some(y), Some(w), Some(h)) = (
        frac(b.get("x")),
        frac(b.get("y")),
        frac(b.get("w")),
        frac(b.get("h")),
    ) else {
        return Err("a box of fractions");
    };
    if w <= 0.0 || h <= 0.0 || x + w > 1.0 + 1e-9 || y + h > 1.0 + 1e-9 {
        return Err("a box inside the frame");
    }
    Ok(Some(FrameBox { x, y, w, h }))
}

/// `POST /api/comment`: a new thread at `t` (seconds on the cut as it plays now) with
/// an optional box, or a reply to `reply_to`.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentBody {
    pub cut: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub t: Option<f64>,
    pub frame_box: Option<FrameBox>,
    pub export_ref: Option<String>,
}

pub fn comment(v: &Value) -> Result<CommentBody, &'static str> {
    let o = obj(v, &["cut", "text", "reply_to", "t", "box", "export_ref"])?;
    let c = CommentBody {
        cut: id(o, "cut")?,
        text: text(o, "text", true)?,
        reply_to: opt_id(o, "reply_to")?,
        t: secs(o, "t")?,
        frame_box: frame_box(o)?,
        export_ref: opt_id(o, "export_ref")?,
    };
    if c.reply_to.is_some() && (c.t.is_some() || c.frame_box.is_some()) {
        return Err("a reply sits on its thread's time and box");
    }
    Ok(c)
}

/// `POST /api/resolve`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveBody {
    pub comment: String,
    pub resolved: bool,
}

pub fn resolve(v: &Value) -> Result<ResolveBody, &'static str> {
    let o = obj(v, &["comment", "resolved"])?;
    Ok(ResolveBody {
        comment: id(o, "comment")?,
        resolved: o
            .get("resolved")
            .and_then(Value::as_bool)
            .ok_or("resolved: true or false")?,
    })
}

/// What an editor suggests, in the terms the phone has (seconds on the cut, a part id).
#[derive(Clone, Debug, PartialEq)]
pub enum Suggestion {
    /// Take `head_s` off the part's start and `tail_s` off its end.
    Trim {
        part: String,
        head_s: f64,
        tail_s: f64,
    },
    /// Strike what the cut plays from `from_s` to `to_s` (inside one part).
    Strike { from_s: f64, to_s: f64 },
    /// Captions at height `y` (fraction of the frame): the part playing at `t` only, or
    /// the whole cut when `t` is absent.
    CaptionMove { y: f64, t: Option<f64> },
}

/// `POST /api/suggest` (editor links only).
#[derive(Clone, Debug, PartialEq)]
pub struct SuggestBody {
    pub cut: String,
    pub edit: Suggestion,
    pub note: String,
    pub comment: Option<String>,
}

pub fn suggest(v: &Value) -> Result<SuggestBody, &'static str> {
    let o = obj(
        v,
        &[
            "cut", "kind", "note", "comment", "part", "head_s", "tail_s", "from_s", "to_s", "y",
            "t",
        ],
    )?;
    let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
    let need = |k: &str| secs(o, k).and_then(|x| x.ok_or("seconds"));
    let only = |allowed: &[&str]| -> Result<(), &'static str> {
        let common = ["cut", "kind", "note", "comment"];
        if o.keys()
            .any(|k| !common.contains(&k.as_str()) && !allowed.contains(&k.as_str()))
        {
            return Err("a field this kind does not take");
        }
        Ok(())
    };
    let edit = match kind {
        "trim" => {
            only(&["part", "head_s", "tail_s"])?;
            let (h, t) = (
                secs(o, "head_s")?.unwrap_or(0.0),
                secs(o, "tail_s")?.unwrap_or(0.0),
            );
            if h == 0.0 && t == 0.0 {
                return Err("something to trim");
            }
            Suggestion::Trim {
                part: id(o, "part")?,
                head_s: h,
                tail_s: t,
            }
        }
        "strike" => {
            only(&["from_s", "to_s"])?;
            let (a, b) = (need("from_s")?, need("to_s")?);
            if b <= a || b - a > 120.0 {
                return Err("from before to, at most 2 min apart");
            }
            Suggestion::Strike { from_s: a, to_s: b }
        }
        "caption_move" => {
            only(&["y", "t"])?;
            let y = frac(o.get("y"))
                .filter(|y| (0.05..=0.95).contains(y))
                .ok_or("y from 0.05 to 0.95")?;
            Suggestion::CaptionMove {
                y,
                t: secs(o, "t")?,
            }
        }
        _ => return Err("kind: trim, strike or caption_move"),
    };
    Ok(SuggestBody {
        cut: id(o, "cut")?,
        edit,
        note: text(o, "note", false)?,
        comment: opt_id(o, "comment")?,
    })
}

/// `POST /api/approve`: hand this export to Clippy's calendar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApproveBody {
    pub export: String,
}

pub fn approve(v: &Value) -> Result<ApproveBody, &'static str> {
    let o = obj(v, &["export"])?;
    Ok(ApproveBody {
        export: id(o, "export")?,
    })
}

/// Parses a write body for `path` (the fuzz entry point): `Ok(())` when it is one of the
/// write routes and the body is well formed.
pub fn parse(path: &str, body: &[u8]) -> Result<(), &'static str> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "JSON")?;
    match path {
        "/api/comment" => comment(&v).map(|_| ()),
        "/api/resolve" => resolve(&v).map(|_| ()),
        "/api/suggest" => suggest(&v).map(|_| ()),
        "/api/approve" => approve(&v).map(|_| ()),
        _ => Err("route"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comments() {
        let c = comment(&json!({"cut": "c1", "text": "hi", "t": 2.5, "box": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}})).unwrap();
        assert_eq!(c.t, Some(2.5));
        assert_eq!(c.frame_box.unwrap().w, 0.5);
        assert!(comment(&json!({"cut": "c1", "text": " "})).is_err());
        assert!(comment(&json!({"cut": "c/1", "text": "x"})).is_err());
        assert!(comment(&json!({"cut": "c1", "text": "x", "t": -1})).is_err());
        assert!(comment(&json!({"cut": "c1", "text": "x", "t": 1e9})).is_err());
        assert!(
            comment(
                &json!({"cut": "c1", "text": "x", "box": {"x": 0.9, "y": 0, "w": 0.5, "h": 0.1}})
            )
            .is_err()
        );
        assert!(comment(&json!({"cut": "c1", "text": "x", "evil": 1})).is_err());
        assert!(comment(&json!({"cut": "c1", "text": "x", "reply_to": "r1", "t": 1})).is_err());
        assert!(comment(&json!({"cut": "c1", "text": "x".repeat(2001)})).is_err());
    }

    #[test]
    fn suggestions() {
        let s =
            suggest(&json!({"cut": "c1", "kind": "trim", "part": "p1", "head_s": 0.5})).unwrap();
        assert_eq!(
            s.edit,
            Suggestion::Trim {
                part: "p1".into(),
                head_s: 0.5,
                tail_s: 0.0
            }
        );
        assert!(suggest(&json!({"cut": "c1", "kind": "trim", "part": "p1"})).is_err());
        assert!(
            suggest(&json!({"cut": "c1", "kind": "trim", "part": "p1", "head_s": 1, "y": 0.5}))
                .is_err()
        );
        assert!(suggest(&json!({"cut": "c1", "kind": "strike", "from_s": 3, "to_s": 2})).is_err());
        assert!(suggest(&json!({"cut": "c1", "kind": "caption_move", "y": 0.99})).is_err());
        assert!(suggest(&json!({"cut": "c1", "kind": "delete_everything"})).is_err());
        assert!(parse("/api/approve", br#"{"export": "abc"}"#).is_ok());
        assert!(parse("/api/approve", br#"{"export": "../x"}"#).is_err());
        assert!(parse("/api/other", b"{}").is_err());
    }
}
