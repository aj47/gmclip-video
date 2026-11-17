//! The history browser's app side (A2.13 / U.14, SPEC §5.12): the core's version list
//! and visual diff, with what people read instead of identities (`you`, `agent:<client>`,
//! `review link`), small pictures of the changed parts from the scrub proxy, how long a
//! restore took (R-HIST-5, shown in the UI), and the version count for the cut header's
//! "↶ N earlier versions".

use std::collections::HashMap;
use std::time::Instant;

use gmclip_model::Id;
use gmclip_model::entities::{MemberRole, Source, SyncGroup, SyncMember};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::AppResult;

/// Changed parts get a thumbnail; at most this many per diff.
const DIFF_THUMBS: usize = 24;
/// Thumbnail width (pixels).
const THUMB_W: u32 = 112;

/// Who an identity is, as the history shows it: the local human is `you`; an agent is
/// `agent:<client>` (a launched run's client from the app's run list when it knows it);
/// a review link is `review link <id>`.
pub fn who(identity: &str, launched: &HashMap<String, String>) -> String {
    if identity == "human" {
        return "you".into();
    }
    if let Some(c) = identity.strip_prefix("local:") {
        return format!("agent:{c}");
    }
    if let Some(r) = identity.strip_prefix("launched:") {
        return launched
            .get(r)
            .map_or_else(|| "agent".to_owned(), |c| format!("agent:{c}"));
    }
    if let Some(l) = identity.strip_prefix("review-link:") {
        let short: String = l.chars().take(8).collect();
        return format!("review link {short}");
    }
    identity.chars().take(60).collect()
}

/// `history_list` for the UI: the core's rows with `then_by`/`run_by` as people read them.
pub fn list(
    core: &AppCore,
    project: &str,
    cut: Option<&str>,
    launched: &HashMap<String, String>,
) -> AppResult<Value> {
    let mut args = json!({"limit": 60});
    if let Some(c) = cut {
        args["cut"] = json!(c);
    }
    let mut out = core.call(project, "history_list", args)?["output"].clone();
    if let Some(vs) = out["versions"].as_array_mut() {
        for v in vs {
            let by: Vec<String> = v["then_by"]
                .as_array()
                .map(|a| {
                    let mut o: Vec<String> = Vec::new();
                    for w in a.iter().filter_map(Value::as_str).map(|i| who(i, launched)) {
                        if !o.contains(&w) {
                            o.push(w);
                        }
                    }
                    o
                })
                .unwrap_or_default();
            v["then_by"] = json!(by);
            if let Some(r) = v["run_by"].as_str() {
                v["run_by"] = json!(who(r, launched));
            }
        }
    }
    Ok(out)
}

/// How many versions the project has (the cut header's "↶ N earlier versions").
pub fn count(core: &AppCore, project: &str) -> AppResult<Value> {
    let out = core.call(project, "history_list", json!({"count_only": true}))?;
    Ok(json!({"total": out["output"]["total"]}))
}

/// `history_diff` for the UI: the core's diff, authors as people read them, and a
/// thumbnail (`{w, h, rgb}` base64 RGB) on each changed part the scrub proxy covers.
pub fn diff(
    core: &AppCore,
    project: &str,
    cut: &str,
    snapshot: &str,
    against: Option<&str>,
    launched: &HashMap<String, String>,
) -> AppResult<Value> {
    let mut args = json!({"snapshot": snapshot, "cut": cut});
    if let Some(a) = against {
        args["against"] = json!(a);
    }
    let mut d = core.call(project, "history_diff", args)?["output"].clone();
    let by: Vec<String> = d["by"]
        .as_array()
        .map(|a| {
            let mut o: Vec<String> = Vec::new();
            for w in a.iter().filter_map(Value::as_str).map(|i| who(i, launched)) {
                if !o.contains(&w) {
                    o.push(w);
                }
            }
            o
        })
        .unwrap_or_default();
    d["by"] = json!(by);
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let mut budget = DIFF_THUMBS;
    let mut thumbs = Thumbs::new(core);
    let mut add = |part: &mut Value| {
        if budget == 0 {
            return;
        }
        let (Some(src), Some(t)) = (part["source"].as_str(), part["src_in_s"].as_f64()) else {
            return;
        };
        if let Some(path) = picture_path(&doc, src) {
            budget -= 1;
            if let Some((w, h, rgb)) = thumbs.at(&path, t + 0.05) {
                part["thumb"] = json!({"w": w, "h": h, "rgb": crate::thumbs::b64(&rgb)});
            }
        }
    };
    for k in ["added", "removed"] {
        if let Some(a) = d[k].as_array_mut() {
            for part in a {
                add(part);
            }
        }
    }
    if let Some(a) = d["trimmed"].as_array_mut() {
        for t in a {
            add(&mut t["to"]);
        }
    }
    Ok(d)
}

/// Restores a cut (or the project) to a version as one undo step, and says how long the
/// core took (R-HIST-5: ≤ 200 ms on a 200-part cut).
pub fn restore(
    core: &AppCore,
    project: &str,
    cut: Option<&str>,
    snapshot: &str,
) -> AppResult<Value> {
    let mut args = json!({"snapshot": snapshot});
    if let Some(c) = cut {
        args["cut"] = json!(c);
    }
    let t0 = Instant::now();
    let mut v = core.call(project, "restore_to", args)?;
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    v["ms"] = json!((ms * 10.0).round() / 10.0);
    Ok(v)
}

/// The source path of a part's picture, by the id the diff names (a source, or a sync
/// group's picture member).
fn picture_path(doc: &gmclip_model::Doc, id: &str) -> Option<String> {
    let ulid = gmclip_model::Ulid::parse(id).ok()?;
    let sid: Id<Source> = Id::new(ulid);
    if let Some(s) = doc.get(sid) {
        return Some(s.path.clone());
    }
    let g: Id<SyncGroup> = Id::new(ulid);
    doc.iter::<SyncMember>()
        .find(|m| m.group == g && matches!(m.role, MemberRole::Picture | MemberRole::Both))
        .and_then(|m| doc.get(m.source))
        .map(|s| s.path.clone())
}

/// Thumbnails from the scrub proxies (macOS), readers opened once per source.
struct Thumbs<'a> {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    core: &'a AppCore,
    #[cfg(any(target_os = "macos", windows))]
    readers: HashMap<String, Option<gmclip_render::export::proxy::ProxyReader>>,
}

impl<'a> Thumbs<'a> {
    fn new(core: &'a AppCore) -> Self {
        Self {
            core,
            #[cfg(any(target_os = "macos", windows))]
            readers: HashMap::new(),
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn at(&mut self, path: &str, t_s: f64) -> Option<(u32, u32, Vec<u8>)> {
        use gmclip_render::export::proxy::ProxyReader;
        let core = self.core;
        let r = self
            .readers
            .entry(path.to_owned())
            .or_insert_with(|| {
                core.proxies
                    .dir_of(path)
                    .and_then(|d| ProxyReader::open(&d))
            })
            .as_mut()?;
        let fps = f64::from(r.manifest().fps.max(1));
        let n = (t_s.max(0.0) * fps).floor() as u64;
        if !r.covers(n) {
            return None;
        }
        let k = r.keyframe_of(n);
        r.thumbnail(k, THUMB_W).ok()
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn at(&mut self, _path: &str, _t_s: f64) -> Option<(u32, u32, Vec<u8>)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_read_as_people() {
        let mut l = HashMap::new();
        l.insert("RUN1".to_owned(), "pi".to_owned());
        assert_eq!(who("human", &l), "you");
        assert_eq!(who("local:claude-code", &l), "agent:claude-code");
        assert_eq!(who("launched:RUN1", &l), "agent:pi");
        assert_eq!(who("launched:OTHER", &l), "agent");
        assert_eq!(
            who("review-link:abcdef0123456789/phone", &l),
            "review link abcdef01"
        );
    }
}
