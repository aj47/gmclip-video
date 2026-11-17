//! The person's UI preferences (app settings, never profile fields, SPEC §12): small view
//! choices that should follow them across launches and projects — hover scrub on the
//! timeline, the safe-zone overlay, the project map open or closed, the render stats line,
//! which first-run coach marks they have seen, the timeline's waveform, and the cut view's
//! layout (which panes show, their dragged sizes, focus), the transcript's times and text
//! sizes, the colour theme and the density of the controls, the workspace layouts the
//! person saved (named pane arrangements, switched from the View menu), and the developer
//! setting (decision 0054: the team and in-app agent surfaces, every side panel in the bar,
//! the preview's measuring tools).
//!
//! One JSON file in the app data folder, `ui-prefs.json`, written whole and atomically
//! (temp file + rename). Only the keys below are kept, each with its own type; anything
//! else in a write is refused, so a webview bug cannot grow the file.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

const FILE: &str = "ui-prefs.json";
/// Coach marks remembered (ids are short words).
const MAX_SEEN: usize = 64;

/// Every key and its default.
pub fn defaults() -> Value {
    json!({
        "hover_scrub": true,
        "safe_zones": false,
        "map_open": true,
        "render_stats": false,
        "waveform": true,
        "coach_seen": [],
        "side_open": true,
        "timeline_open": true,
        "words_open": true,
        "lanes_open": true,
        "focus": false,
        "side_w": 0,
        "map_w": 0,
        "tl_h": 0,
        "tr_times": false,
        "tr_size": 14,
        "read_size": 20,
        "theme": "system",
        "density": "comfortable",
        "open_panes": "last",
        "workspaces": [],
        // Settings › Recording (decision 0045): off until turned on, which asks for the
        // five permissions recording needs.
        "recording": false,
        // Settings › Developer (decision 0054)
        "developer": false,
    })
}

/// Saved workspace layouts kept.
const MAX_WORKSPACES: usize = 24;
/// The side-panel tabs a workspace may open on (layout.ts `SideTab`), and the names they
/// had before decision 0054 (kept, so a saved layout still loads; layout.ts `tabFrom` maps
/// them: the merged ones to their new tab, the rest to Words).
const SIDE_TABS: &[&str] = &[
    "words",
    "look",
    "captions",
    "story",
    "polish",
    "thumbnail",
    "review",
    "agent",
    "transcript",
    "inspector",
    "layout",
    "short",
    "tighten",
];

/// One saved workspace: a name and the pane keys it sets, each as its own preference
/// would be; nothing else.
fn workspace_ok(w: &Value) -> bool {
    let Some(o) = w.as_object() else {
        return false;
    };
    let named = o.get("name").and_then(Value::as_str).is_some_and(|n| {
        let n = n.trim();
        !n.is_empty() && n.chars().count() <= 40 && !n.chars().any(char::is_control)
    });
    named
        && o.iter().all(|(k, v)| match k.as_str() {
            "name" => true,
            "reading" => v.is_boolean(),
            "side_tab" => v.as_str().is_some_and(|t| SIDE_TABS.contains(&t)),
            "map_open" | "side_open" | "timeline_open" | "words_open" | "lanes_open" | "focus"
            | "side_w" | "map_w" | "tl_h" => valid(k, v).is_ok(),
            _ => false,
        })
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The saved preferences over the defaults (a missing or unreadable file reads as the
/// defaults; unknown keys in it are dropped).
pub fn load(dir: &Path) -> Value {
    let mut out = defaults();
    let saved: Value = std::fs::read(file(dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    if let (Some(o), Some(s)) = (out.as_object_mut(), saved.as_object()) {
        for (k, v) in s {
            if valid(k, v).is_ok() {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

fn valid(key: &str, v: &Value) -> AppResult<()> {
    match key {
        "hover_scrub" | "safe_zones" | "map_open" | "render_stats" | "waveform" | "recording"
        | "developer" => {
            if v.is_boolean() {
                Ok(())
            } else {
                Err(AppError::invalid(format!(
                    "{key} is on or off (true/false)"
                )))
            }
        }
        "side_open" | "timeline_open" | "words_open" | "lanes_open" | "focus" | "tr_times" => {
            if v.is_boolean() {
                Ok(())
            } else {
                Err(AppError::invalid(format!(
                    "{key} is on or off (true/false)"
                )))
            }
        }
        // a dragged pane size in px (0 = the layout's own), a text size in px
        "side_w" | "map_w" | "tl_h" | "tr_size" | "read_size" => {
            let (lo, hi) = match key {
                "side_w" => (200, 1600),
                "map_w" => (140, 640),
                "tl_h" => (80, 1600),
                "tr_size" => (10, 28),
                _ => (12, 40),
            };
            let ok = v
                .as_u64()
                .is_some_and(|n| (n == 0 && !key.ends_with("size")) || (lo..=hi).contains(&n));
            if ok {
                Ok(())
            } else {
                Err(AppError::invalid(format!(
                    "{key} is a whole number of px from {lo} to {hi}"
                )))
            }
        }
        "theme" | "density" | "open_panes" => {
            let allowed: &[&str] = match key {
                "theme" => &["dark", "light", "system"],
                "density" => &["comfortable", "compact"],
                _ => &["last", "all", "focus"],
            };
            if v.as_str().is_some_and(|s| allowed.contains(&s)) {
                Ok(())
            } else {
                Err(AppError::invalid(format!(
                    "{key} is one of {}",
                    allowed.join(", ")
                )))
            }
        }
        "workspaces" => {
            let ok = v
                .as_array()
                .is_some_and(|a| a.len() <= MAX_WORKSPACES && a.iter().all(workspace_ok));
            if ok {
                Ok(())
            } else {
                Err(AppError::invalid(format!(
                    "workspaces is a list of at most {MAX_WORKSPACES} named layouts (pane keys only)"
                )))
            }
        }
        "coach_seen" => {
            let ok = v.as_array().is_some_and(|a| {
                a.len() <= MAX_SEEN
                    && a.iter().all(|x| {
                        x.as_str().is_some_and(|s| {
                            !s.is_empty()
                                && s.len() <= 32
                                && s.bytes()
                                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-')
                        })
                    })
            });
            if ok {
                Ok(())
            } else {
                Err(AppError::invalid("coach_seen is a short list of short ids"))
            }
        }
        _ => Err(AppError::invalid(format!("no UI preference named {key}"))),
    }
}

/// One write at a time: each write reads the file, merges and replaces it, so two at once
/// (two settings changed within a frame) would each drop the other's key.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Merges `patch` into the saved preferences and writes them; returns all of them.
pub fn set(dir: &Path, patch: &Value) -> AppResult<Value> {
    let _one = WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let p: &Map<String, Value> = patch
        .as_object()
        .ok_or_else(|| AppError::invalid("preferences are an object"))?;
    for (k, v) in p {
        valid(k, v)?;
    }
    let mut all = load(dir);
    if let Some(o) = all.as_object_mut() {
        for (k, v) in p {
            o.insert(k.clone(), v.clone());
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::internal(format!("saving preferences: {}", e.kind())))?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all).unwrap_or_default())
        .and_then(|()| std::fs::rename(&tmp, file(dir)))
        .map_err(|e| AppError::internal(format!("saving preferences: {}", e.kind())))?;
    Ok(all)
}

pub fn get(core: &AppCore) -> Value {
    load(&core.dirs.app_data)
}

pub fn put(core: &AppCore, patch: &Value) -> AppResult<Value> {
    let all = set(&core.dirs.app_data, patch)?;
    crate::shell::sync_view_menu(&all);
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_then_a_write_persists_and_unknown_keys_are_refused() {
        let dir = std::env::temp_dir().join(format!("gmclip-uiprefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir)["hover_scrub"], true);
        let all = set(&dir, &json!({"hover_scrub": false})).unwrap();
        assert_eq!(all["hover_scrub"], false);
        assert_eq!(all["safe_zones"], false);
        assert_eq!(load(&dir)["hover_scrub"], false);
        // nothing else can be written, and a bad type is refused
        assert!(set(&dir, &json!({"anything": 1})).is_err());
        assert!(set(&dir, &json!({"hover_scrub": "yes"})).is_err());
        assert!(set(&dir, &json!({"coach_seen": ["Timeline!"]})).is_err());
        assert!(set(&dir, &json!({"coach_seen": ["timeline", "words"]})).is_ok());
        // a hand-edited file with junk keeps only what it may
        std::fs::write(
            file(&dir),
            br#"{"hover_scrub": false, "evil": "x", "map_open": 3}"#,
        )
        .unwrap();
        let l = load(&dir);
        assert_eq!(l["hover_scrub"], false);
        assert_eq!(l["map_open"], true);
        assert!(l.get("evil").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layout_saved_before_the_tabs_merged_still_loads() {
        // decision 0054: "short" and "inspector" are tab names from before the merge (the
        // app maps them); a file naming them keeps its layouts, and the developer setting
        // starts off
        let dir = std::env::temp_dir().join(format!("gmclip-uiprefs-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = json!([{"name": "Old", "side_tab": "short"}, {"name": "Older", "side_tab": "inspector"}, {"name": "New", "side_tab": "look"}]);
        std::fs::write(
            file(&dir),
            serde_json::to_vec(&json!({ "workspaces": w })).unwrap(),
        )
        .unwrap();
        let l = load(&dir);
        assert_eq!(l["workspaces"], w);
        assert_eq!(l["developer"], false);
        assert_eq!(
            set(&dir, &json!({"developer": true})).unwrap()["developer"],
            true
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspaces_keep_only_named_pane_layouts() {
        let dir = std::env::temp_dir().join(format!("gmclip-uiprefs-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir)["workspaces"], json!([]));
        let w = json!([{"name": "Captions", "map_open": false, "side_tab": "short", "tl_h": 320, "reading": false}]);
        assert_eq!(
            set(&dir, &json!({ "workspaces": w })).unwrap()["workspaces"],
            w
        );
        for bad in [
            json!([{"map_open": false}]),
            json!([{"name": " "}]),
            json!([{"name": "x", "evil": 1}]),
            json!([{"name": "x", "tl_h": 5}]),
            json!([{"name": "x", "side_tab": "nope"}]),
            json!({"name": "x"}),
        ] {
            assert!(set(&dir, &json!({ "workspaces": bad })).is_err(), "{bad}");
        }
        let many: Vec<Value> = (0..25).map(|i| json!({"name": format!("w{i}")})).collect();
        assert!(set(&dir, &json!({ "workspaces": many })).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_at_the_same_time_keep_every_key() {
        let dir = std::env::temp_dir().join(format!("gmclip-uiprefs-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // all true by default: each thread turns one off
        let keys = [
            "side_open",
            "timeline_open",
            "words_open",
            "lanes_open",
            "map_open",
            "hover_scrub",
        ];
        std::thread::scope(|s| {
            for k in keys {
                let dir = &dir;
                s.spawn(move || set(dir, &json!({ k: false })).unwrap());
            }
        });
        let l = load(&dir);
        for k in keys {
            assert_eq!(l[k], false, "{k} kept");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn layout_and_appearance_keep_their_types_and_ranges() {
        let dir = std::env::temp_dir().join(format!("gmclip-uiprefs-l-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = load(&dir);
        assert_eq!(d["side_open"], true);
        assert_eq!(d["theme"], "system");
        assert_eq!(d["side_w"], 0);
        let all = set(
            &dir,
            &json!({"side_open": false, "focus": true, "side_w": 420, "tl_h": 0, "theme": "system", "density": "compact", "open_panes": "all", "tr_times": false, "tr_size": 16, "read_size": 24}),
        )
        .unwrap();
        assert_eq!(all["side_w"], 420);
        assert_eq!(load(&dir)["density"], "compact");
        assert_eq!(load(&dir)["focus"], true);
        // out of range, the wrong type, an unknown choice: refused
        assert!(set(&dir, &json!({"side_w": 50})).is_err());
        assert!(set(&dir, &json!({"side_w": -1})).is_err());
        assert!(set(&dir, &json!({"side_w": 300.5})).is_err());
        assert!(set(&dir, &json!({"tr_size": 0})).is_err());
        assert!(set(&dir, &json!({"read_size": 99})).is_err());
        assert!(set(&dir, &json!({"theme": "neon"})).is_err());
        assert!(set(&dir, &json!({"density": 1})).is_err());
        assert!(set(&dir, &json!({"focus": "on"})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
