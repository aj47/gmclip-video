//! Kits (SPEC §10.5): `spec/kits/<kit>.md` (its profile content as one fenced `json`
//! block), `spec/kits/<kit>.style.md` (its starting style rules) and
//! `spec/kits/<kit>.rubric.md` (its moment rubric, a versioned prompt asset, R-AI-5),
//! compiled in. A new profile from a kit gets that content in `profile.json` — with the
//! rubric asset's text as `rubric` and its reference as `rubric_ref` — and the style rules
//! as `style.md`, which the Owner may then pin as the style file (R-PROF-5): the kit only
//! suggests it.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::{CONTENT_KEYS, ProfileError, create};

/// `(id, name, kit.md, style.md)`.
pub const KITS: &[(&str, &str, &str, &str)] = &[
    (
        "gaming-variety",
        "Gaming / variety",
        include_str!("../../../../spec/kits/gaming-variety.md"),
        include_str!("../../../../spec/kits/gaming-variety.style.md"),
    ),
    (
        "screen-face",
        "Screen + face",
        include_str!("../../../../spec/kits/screen-face.md"),
        include_str!("../../../../spec/kits/screen-face.style.md"),
    ),
    (
        "talking-head",
        "Talking head / podcast",
        include_str!("../../../../spec/kits/talking-head.md"),
        include_str!("../../../../spec/kits/talking-head.style.md"),
    ),
    (
        "blank",
        "Blank",
        include_str!("../../../../spec/kits/blank.md"),
        include_str!("../../../../spec/kits/blank.style.md"),
    ),
];

/// `(kit id, version, rubric.md)`: each kit's moment rubric (Blank has none).
pub const RUBRICS: &[(&str, u32, &str)] = &[
    (
        "gaming-variety",
        1,
        include_str!("../../../../spec/kits/gaming-variety.rubric.md"),
    ),
    (
        "screen-face",
        1,
        include_str!("../../../../spec/kits/screen-face.rubric.md"),
    ),
    (
        "talking-head",
        1,
        include_str!("../../../../spec/kits/talking-head.rubric.md"),
    ),
];

/// A kit's rubric: its text and `<kit>@<version>:<first 12 hex of its SHA-256>`.
pub fn rubric(id: &str) -> Option<(&'static str, String)> {
    use sha2::{Digest, Sha256};
    let (_, v, text) = RUBRICS.iter().find(|r| r.0 == id)?;
    let h: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some((text, format!("{id}@{v}:{}", &h[..12])))
}

/// The content block of a kit's Markdown: the first fenced `json` block.
pub fn content_of(md: &str) -> Result<BTreeMap<String, Value>, ProfileError> {
    let start = md
        .find("```json")
        .ok_or_else(|| ProfileError("the kit has no ```json block".into()))?;
    let rest = &md[start + "```json".len()..];
    let end = rest
        .find("```")
        .ok_or_else(|| ProfileError("the kit's json block is not closed".into()))?;
    let v: Value = serde_json::from_str(&rest[..end])
        .map_err(|_| ProfileError("the kit's json block is not JSON".into()))?;
    let Value::Object(m) = v else {
        return Err(ProfileError("the kit's json block is not an object".into()));
    };
    let mut out = BTreeMap::new();
    for (k, val) in m {
        if !CONTENT_KEYS.contains(&k.as_str()) {
            return Err(ProfileError(format!(
                "kit field `{k}` is not profile content"
            )));
        }
        out.insert(k, val);
    }
    Ok(out)
}

/// A kit by id.
pub fn kit(id: &str) -> Option<(&'static str, &'static str, &'static str)> {
    KITS.iter().find(|k| k.0 == id).map(|k| (k.1, k.2, k.3))
}

/// Creates a profile folder from a kit: `profile.json` and `style.md`.
pub fn create_from_kit(dir: &Path, id: &str, name: &str) -> Result<(), ProfileError> {
    create_from_kit_with(dir, id, name, &BTreeMap::new())
}

/// [`create_from_kit`] with more content on top of the kit's (the stranger setup's
/// answers: platforms, framings, presets, safe zones, loudness). `extra` holds content
/// fields only.
pub fn create_from_kit_with(
    dir: &Path,
    id: &str,
    name: &str,
    extra: &BTreeMap<String, Value>,
) -> Result<(), ProfileError> {
    let (_, md, style) = kit(id).ok_or_else(|| ProfileError("no such kit".into()))?;
    let mut content = content_of(md)?;
    if let Some((text, r)) = rubric(id) {
        content.insert("rubric".into(), Value::String(text.to_owned()));
        content.insert("rubric_ref".into(), Value::String(r));
    }
    for (k, v) in extra {
        content.insert(k.clone(), v.clone());
    }
    let mut suggests = BTreeMap::new();
    let style_path = dir.join("style.md");
    suggests.insert(
        "style_path".to_owned(),
        Value::String(style_path.to_string_lossy().into_owned()),
    );
    if std::fs::symlink_metadata(&style_path).is_ok() {
        return Err(ProfileError("the folder already has a style.md".into()));
    }
    create(dir, name, Some(id), &content, &suggests)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&style_path)
        .map_err(|e| ProfileError(format!("write style.md ({})", e.kind())))?;
    use std::io::Write;
    f.write_all(style.as_bytes())
        .map_err(|e| ProfileError(format!("write style.md ({})", e.kind())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kit_parses_and_holds_content_only() {
        for (id, _, md, style) in KITS {
            let c = content_of(md).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(c.keys().all(|k| !super::super::is_authority(k)), "{id}");
            assert!(!style.trim().is_empty(), "{id}");
        }
        let g = content_of(kit("gaming-variety").unwrap().1).unwrap();
        assert_eq!(g["duration_bands"]["9:16"], serde_json::json!([8, 45]));
        assert_eq!(g["caption_grouping"], "adaptive_words");
        assert_eq!(g["text_hook"], false);
        let s = content_of(kit("screen-face").unwrap().1).unwrap();
        assert_eq!(s["duration_bands"]["16:9"], serde_json::json!([180, 900]));
        assert_eq!(
            content_of(kit("talking-head").unwrap().1).unwrap()["duration_bands"]["9:16"],
            serde_json::json!([20, 90])
        );
        assert!(content_of(kit("blank").unwrap().1).unwrap().is_empty());
        // every kit but Blank names a template, a caption preset and a rubric asset
        for (id, ..) in KITS.iter().filter(|k| k.0 != "blank") {
            let c = content_of(kit(id).unwrap().1).unwrap();
            assert!(c["default_template"].is_string(), "{id}");
            assert!(c["caption_preset"].is_string(), "{id}");
            let (text, r) = rubric(id).unwrap();
            assert!(
                text.contains("2–5") && r.starts_with(&format!("{id}@1:")),
                "{id}"
            );
        }
        assert!(rubric("blank").is_none());
    }

    #[test]
    fn a_profile_from_a_kit_loads_with_only_a_style_suggestion() {
        let d = std::env::temp_dir().join(format!("gmclip-kit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        create_from_kit(&d, "screen-face", "Mine").unwrap();
        let p = super::super::load(&d).unwrap();
        assert_eq!(p.kit.as_deref(), Some("screen-face"));
        assert!(p.content.contains_key("rubric"));
        // the rubric is the kit's asset, with its reference
        let (text, r) = rubric("screen-face").unwrap();
        assert_eq!(p.content["rubric"], text);
        assert_eq!(p.content["rubric_ref"], r.as_str());
        assert_eq!(p.suggestions.len(), 1);
        assert_eq!(p.suggestions[0].key, "style_path");
        assert!(d.join("style.md").exists());
        assert!(
            create_from_kit(&d, "screen-face", "Mine").is_err(),
            "never over an existing profile"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
