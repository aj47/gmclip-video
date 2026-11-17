//! Creator profiles (SPEC §10.5; SR R-PROF-1/2/4/5).
//!
//! A profile is a folder holding **content only**: `profile.json` (rubric, playbook,
//! bands, caption preset and seats, terms, keep pairs, default framings, loudness…),
//! optional Markdown documents beside it, `stream-layouts.json` presets, and the
//! human-only feedback log `feedback.jsonl` ([`feedback`]).
//!
//! - **No authority** (R-PROF-4): harness, budgets (incl. the remote-fix allowance),
//!   auto-run, token scopes / `read_media`, MCP settings, export folder, agent roots and
//!   the style-file path live in the Owner settings, never here. A profile may *suggest*
//!   them: [`load`] moves every authority-shaped key it finds — at the top level, under
//!   `content` or `suggests`, at any depth — into [`Profile::suggestions`], which nothing
//!   applies; the Owner path does that, one line at a time.
//! - **Style rules** (R-PROF-3/5): an Owner-set file read read-only into a hash-pinned
//!   snapshot; a change on disk is served only after the Owner accepts its diff
//!   ([`style`]).
//! - **Kits** ([`kits`]): `spec/kits/<kit>.md` (+ `<kit>.style.md`) as starting profiles.
//!
//! Every text a profile carries is creator- or other-people-authored: untrusted when it
//! reaches an agent (R-INJ-1). gmclip writes into a profile folder only `feedback.jsonl`
//! appends (human-initiated) and, when it creates a profile, the folder itself.

pub mod feedback;
pub mod kits;
pub mod layouts;
pub mod style;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The profile file name inside a profile folder.
pub const PROFILE_FILE: &str = "profile.json";
/// The schema tag `profile.json` carries.
pub const SCHEMA: &str = "gmclip.profile/0";
/// The largest `profile.json` read.
pub const MAX_PROFILE_BYTES: u64 = 1 << 20;

/// Keys that are authority, wherever they appear (R-PROF-4). Matched on the key name
/// with `-` read as `_`, case-insensitively.
pub const AUTHORITY_KEYS: &[&str] = &[
    "harness",
    "budget",
    "budgets",
    "batch_budget",
    "remote_fix_allowance",
    "remote_fix",
    "auto_run",
    "autorun",
    "token_scopes",
    "scopes",
    "default_scopes",
    "read_media",
    "mcp",
    "mcp_settings",
    "mcp_connect",
    "export_folder",
    "export_dir",
    "agent_roots",
    "read_roots",
    "write_roots",
    "roots",
    "file_roots",
    "recordings",
    "style_path",
    "style_file",
    "style_rules_path",
];

/// The content fields a profile may set (SPEC §10.5).
pub const CONTENT_KEYS: &[&str] = &[
    "voice_guide",
    "brand_guide",
    "rubric",
    "hook_playbook",
    "retention_rules",
    "style_rules",
    "presets",
    "caption_preset",
    "caption_seats",
    "bands",
    "duration_bands",
    "terms",
    "keep_pairs",
    "default_framings",
    "export_presets",
    "loudness",
    "brand_kit",
    "recipe",
    "golden_exports",
    "default_template",
    "text_hook",
    "trigger_phrases",
    "caption_grouping",
    "activity_crop",
    "terms_prompt",
    // lane S: the kit's rubric asset reference, and where the creator posts (setup)
    "rubric_ref",
    "platforms",
    "safe_zones",
];

/// Where a profile came from (SPEC §10.5: profiles from others are untrusted imports).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Made on this machine (by setup or an import of the user's own library).
    Local,
    /// Received from someone else: its style text is served inside
    /// `<untrusted source="external-profile">` and never as server instructions.
    External,
}

/// One authority value a profile suggested and nothing applied.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Suggestion {
    /// The JSON path it was found at (`/suggests/harness`).
    pub path: String,
    /// The authority key it matched.
    pub key: String,
    /// The suggested value, at most 200 characters of its JSON.
    pub value: String,
    /// The Owner command that would apply it, when one exists at this milestone.
    pub owner_command: Option<String>,
}

/// A loaded profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub dir: PathBuf,
    pub name: String,
    pub origin: Origin,
    pub kit: Option<String>,
    /// Content fields, by name.
    pub content: BTreeMap<String, Value>,
    /// Authority values the file carried: listed, never applied (R-PROF-4).
    pub suggestions: Vec<Suggestion>,
    /// Keys that are neither content nor authority, ignored and listed.
    pub ignored: Vec<String>,
}

/// Why a profile does not load. Shape-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProfileError {}

fn perr<T>(m: impl Into<String>) -> Result<T, ProfileError> {
    Err(ProfileError(m.into()))
}

fn norm_key(k: &str) -> String {
    k.to_ascii_lowercase().replace('-', "_")
}

/// A key folded for the authority test: ASCII letters and digits only, lower case
/// (`autoRun`, `auto-run`, `AUTO RUN` and `auto\u{200B}_run` all fold to `autorun`).
fn fold(k: &str) -> String {
    k.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether `key` names an authority field (any spelling that folds to one).
pub fn is_authority(key: &str) -> bool {
    let f = fold(key);
    AUTHORITY_KEYS.iter().any(|a| fold(a) == f)
}

fn owner_command_for(key: &str) -> Option<String> {
    match norm_key(key).as_str() {
        "style_path" | "style_file" | "style_rules_path" => {
            Some("gmclip owner style set <path>".into())
        }
        "read_roots" | "agent_roots" | "roots" | "file_roots" => {
            Some("gmclip owner roots add <path>".into())
        }
        "write_roots" => Some("gmclip owner roots add --write <path>".into()),
        "recordings" => Some("gmclip owner recordings <path>".into()),
        _ => None,
    }
}

/// Removes every authority key below `v` (any depth) into `out`.
fn strip_authority(v: &mut Value, path: &str, out: &mut Vec<Suggestion>) {
    match v {
        Value::Object(m) => {
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                let p = format!("{path}/{k}");
                // a key that is not plain ASCII cannot be told apart from an authority
                // key by lookalike letters: it is treated as one (never applied)
                if is_authority(&k) || !k.is_ascii() {
                    if let Some(val) = m.remove(&k) {
                        let mut text = val.to_string();
                        if text.chars().count() > 200 {
                            text = text.chars().take(200).collect::<String>() + "…";
                        }
                        out.push(Suggestion {
                            path: p,
                            key: norm_key(&k),
                            value: text,
                            owner_command: owner_command_for(&k),
                        });
                    }
                } else if let Some(child) = m.get_mut(&k) {
                    strip_authority(child, &p, out);
                }
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter_mut().enumerate() {
                strip_authority(x, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// Parses `profile.json` text. `dir` is where it was read from.
pub fn parse(dir: &Path, text: &str) -> Result<Profile, ProfileError> {
    let mut v: Value =
        serde_json::from_str(text).map_err(|_| ProfileError("profile.json is not JSON".into()))?;
    let Value::Object(_) = v else {
        return perr("profile.json is not an object");
    };
    let mut suggestions = Vec::new();
    strip_authority(&mut v, "", &mut suggestions);
    let mut m = match v {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    if m.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return perr("profile.json's schema is not gmclip.profile/0");
    }
    let name = m
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    let origin = match m.get("origin").and_then(Value::as_str) {
        Some("local") | None => Origin::Local,
        _ => Origin::External,
    };
    let kit = m.get("kit").and_then(Value::as_str).map(str::to_owned);
    let mut content = BTreeMap::new();
    let mut ignored = Vec::new();
    if let Some(Value::Object(c)) = m.remove("content") {
        for (k, val) in c {
            if CONTENT_KEYS.contains(&norm_key(&k).as_str()) {
                content.insert(norm_key(&k), val);
            } else {
                ignored.push(format!("/content/{k}"));
            }
        }
    }
    for k in m.keys() {
        if !matches!(
            k.as_str(),
            "schema" | "name" | "origin" | "kit" | "suggests"
        ) {
            ignored.push(format!("/{k}"));
        }
    }
    Ok(Profile {
        dir: dir.to_path_buf(),
        name,
        origin,
        kit,
        content,
        suggestions,
        ignored,
    })
}

/// Loads the profile in `dir` (`profile.json`, at most 1 MiB, no symlinks). Its
/// `origin` is what the file says; [`load_in`] decides it from app data instead.
pub fn load(dir: &Path) -> Result<Profile, ProfileError> {
    let text = style::read_confined(&dir.join(PROFILE_FILE), MAX_PROFILE_BYTES)
        .map_err(|e| ProfileError(format!("profile.json: {e}")))?;
    parse(dir, &text)
}

/// The app-data list of profile folders gmclip made on this machine.
pub const LOCAL_PROFILES: &str = "local-profiles.json";

/// Records a folder gmclip just made as a local profile.
pub fn register_local(app: &Path, dir: &Path) -> Result<(), ProfileError> {
    let dir = dir
        .canonicalize()
        .map_err(|e| ProfileError(format!("profile folder ({})", e.kind())))?;
    let f = app.join(LOCAL_PROFILES);
    std::fs::create_dir_all(app).map_err(|e| ProfileError(format!("app data ({})", e.kind())))?;
    // read-modify-write under an exclusive lock: two writers at once (two CLI calls, the
    // app and the CLI) would otherwise each drop the other's entry
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(app.join(format!("{LOCAL_PROFILES}.lock")))
        .map_err(|e| ProfileError(format!("app data ({})", e.kind())))?;
    lock.lock()
        .map_err(|e| ProfileError(format!("app data lock ({})", e.kind())))?;
    let mut v: Vec<PathBuf> = std::fs::read(&f)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if !v.contains(&dir) {
        v.push(dir);
    }
    // a temp name of its own: two writers at once (two CLI calls, the app and the CLI)
    // must not rename each other's file away
    let tmp = app.join(format!(
        "{LOCAL_PROFILES}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap_or_default())
        .and_then(|()| std::fs::rename(&tmp, &f))
        .map_err(|e| ProfileError(format!("app data ({})", e.kind())))
}

/// Whether gmclip made `dir` on this machine (else it came from someone else).
pub fn is_local(app: &Path, dir: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    std::fs::read(app.join(LOCAL_PROFILES))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<PathBuf>>(&b).ok())
        .is_some_and(|v| v.contains(&dir))
}

/// Loads a profile with its origin decided by this machine, not by the file: a
/// profile gmclip did not make here is `external`, whatever it says (R-PROF-5).
pub fn load_in(app: &Path, dir: &Path) -> Result<Profile, ProfileError> {
    let mut p = load(dir)?;
    p.origin = if is_local(app, dir) {
        Origin::Local
    } else {
        Origin::External
    };
    Ok(p)
}

/// Writes a new profile folder: `profile.json` from `content` (content keys only) and
/// `suggests` (authority values, listed for the Owner). Refuses an existing
/// `profile.json`; gmclip never rewrites a profile it did not just create.
pub fn create(
    dir: &Path,
    name: &str,
    kit: Option<&str>,
    content: &BTreeMap<String, Value>,
    suggests: &BTreeMap<String, Value>,
) -> Result<(), ProfileError> {
    let file = dir.join(PROFILE_FILE);
    if file.exists() {
        return perr("the folder already has a profile.json");
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| ProfileError(format!("create the profile folder: {}", e.kind())))?;
    let file_tmp_guard = std::fs::symlink_metadata(dir.join(".profile.json.tmp"));
    if file_tmp_guard.is_ok() {
        return perr("the folder has a stray .profile.json.tmp");
    }
    let mut c = Map::new();
    for (k, v) in content {
        if !CONTENT_KEYS.contains(&k.as_str()) {
            return perr(format!("`{k}` is not a profile content field"));
        }
        c.insert(k.clone(), v.clone());
    }
    let mut s = Map::new();
    for (k, v) in suggests {
        if !is_authority(k) {
            return perr(format!("`{k}` is not an authority field"));
        }
        s.insert(k.clone(), v.clone());
    }
    let mut doc = Map::new();
    doc.insert("schema".into(), Value::String(SCHEMA.into()));
    doc.insert("name".into(), Value::String(name.into()));
    doc.insert("origin".into(), Value::String("local".into()));
    if let Some(k) = kit {
        doc.insert("kit".into(), Value::String(k.into()));
    }
    doc.insert("content".into(), Value::Object(c));
    if !s.is_empty() {
        doc.insert("suggests".into(), Value::Object(s));
    }
    let tmp = dir.join(".profile.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&Value::Object(doc)).unwrap_or_default(),
    )
    .map_err(|e| ProfileError(format!("write profile.json: {}", e.kind())))?;
    std::fs::rename(&tmp, &file)
        .map_err(|e| ProfileError(format!("write profile.json: {}", e.kind())))?;
    Ok(())
}

/// `help{topic:"style"}`'s content: the pinned style text (never the file's newer,
/// unaccepted text), whether a change waits for the Owner, and the last 50 feedback
/// records of the active profile. All text here is creator- or other-authored and is
/// returned raw; the transport canonicalises and marks it (R-INJ-1/6).
pub fn style_help(app: &Path, profile: Option<&Path>) -> Value {
    let pin = style::pinned(app);
    let pending = style::changed(app);
    serde_json::json!({
        "style": pin.map(|p| serde_json::json!({
            "path": p.path,
            "sha256": p.sha256,
            "read_at": p.read_at,
            "text": p.text,
            // where the pinned file came from, decided when it was pinned
            "source": if p.external { "external-profile" } else { "style-file" },
            "pending_change": pending,
        })),
        "feedback": profile.map(|d| feedback::last(d, feedback::SERVED)).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SR R-PROF-4's acceptance profile: every authority value becomes a suggestion,
    /// wherever it hides.
    #[test]
    fn authority_fields_are_suggestions_only() {
        let text = r#"{"schema": "gmclip.profile/0", "name": "x",
            "harness": "my agent",
            "content": {"rubric": "r", "budget": 500, "Read-Media": true,
                        "presets": {"auto_run": true, "ok": 1}, "nonsense": 1},
            "suggests": {"style_path": "~/.ssh/config", "export_folder": "/"},
            "extra": 1}"#;
        let p = parse(Path::new("/p"), text).unwrap();
        let keys: Vec<&str> = p.suggestions.iter().map(|s| s.key.as_str()).collect();
        let sneaky = parse(
            Path::new("/p"),
            r#"{"schema": "gmclip.profile/0", "content": {"presets": {"autoRun": 1, "export Folder": 1, "read\u200bmedia": 1, "rеad_roots": 1, "ok": 2}}}"#,
        )
        .unwrap();
        assert_eq!(
            sneaky.content["presets"],
            serde_json::json!({"ok": 2}),
            "{:?}",
            sneaky.suggestions
        );
        for k in [
            "harness",
            "budget",
            "read_media",
            "auto_run",
            "style_path",
            "export_folder",
        ] {
            assert!(keys.contains(&k), "{k} in {keys:?}");
        }
        assert_eq!(p.content["presets"], serde_json::json!({"ok": 1}));
        assert!(p.content.contains_key("rubric"));
        assert!(!p.content.contains_key("budget"));
        assert_eq!(
            p.ignored,
            vec!["/content/nonsense".to_owned(), "/extra".to_owned()]
        );
        let style = p
            .suggestions
            .iter()
            .find(|s| s.key == "style_path")
            .unwrap();
        assert_eq!(
            style.owner_command.as_deref(),
            Some("gmclip owner style set <path>")
        );
        assert!(parse(Path::new("/p"), r#"{"schema": "other"}"#).is_err());
    }
}
