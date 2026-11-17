//! Export record and `render_sig` (B-16; SPEC §5.14, RenderPlan rule 9).
//!
//! `render_sig` (RenderPlan rule 9 as amended by decision 0004) = BLAKE3 over (1) the
//! RFC 8785 canonical JSON of the plan with what does not change pixels or samples
//! removed — the top-level `meta`, every `provenance` object, every string `path` member,
//! and `sources[].fingerprint.mtime_ns` — and every non-integer number quantised to
//! 1/1024 (round half to even), followed by (2) the raw 32-byte SHA-256 digest of each
//! source (`sources[].fingerprint.sha256`) and then each font (`fonts[].sha256`), in
//! array order (`/1` then each image's and each cursor track's, decisions 0014, 0045). Lowercase hex. A source without a `sha256` makes it absent, with the
//! reason. So the same cut signs the same on the M4 Pro, the Air and CI, and whether the
//! reel converter or the model planner wrote it (SPEC §6.5's M1 seam test).
//!
//! [`install_export`] puts a finished render in place; the replaced file and its record
//! move to `superseded/` (last three kept, [`rotate_superseded`]).

pub mod blake3;
pub mod jcs;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `render_sig`, or why there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderSig {
    Sig(String),
    Absent(String),
}

/// Drops what decision 0004 excludes, then [`normalise_values`].
fn normalise(v: &mut Value) {
    if let Some(m) = v.as_object_mut() {
        m.remove("meta");
        if let Some(sources) = m.get_mut("sources").and_then(Value::as_array_mut) {
            for s in sources {
                if let Some(fp) = s.get_mut("fingerprint").and_then(Value::as_object_mut) {
                    fp.remove("mtime_ns");
                }
            }
        }
    }
    normalise_values(v);
}

/// Removes every string `path` and every object `provenance` member, at any depth, and
/// quantises every non-integer number to 1/1024.
fn normalise_values(v: &mut Value) {
    match v {
        Value::Object(m) => {
            // Only string-valued `path` members: a map keyed by id (`text_styles`) may
            // legitimately hold an entry called "path", whose value is an object.
            if m.get("path").is_some_and(Value::is_string) {
                m.remove("path");
            }
            if m.get("provenance").is_some_and(Value::is_object) {
                m.remove("provenance");
            }
            m.values_mut().for_each(normalise_values);
        }
        Value::Array(a) => a.iter_mut().for_each(normalise_values),
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64().map(|f| (f * 1024.0).round_ties_even() / 1024.0)
                && let Some(q) = serde_json::Number::from_f64(f)
            {
                *n = q;
            }
        }
        _ => {}
    }
}

fn digest(hex: &str) -> Option<[u8; 32]> {
    // Exactly the schema's ^[0-9a-f]{64}$ (uppercase or signs would sign differently).
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `render_sig` of a RenderPlan given as JSON.
#[must_use]
pub fn render_sig(plan: &Value) -> RenderSig {
    if plan
        .get("sources")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return RenderSig::Absent("the plan has no sources".into());
    }
    let mut tail = Vec::new();
    for (i, s) in plan
        .get("sources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let id = s.get("id").and_then(Value::as_str).unwrap_or("?");
        match s
            .pointer("/fingerprint/sha256")
            .and_then(Value::as_str)
            .and_then(digest)
        {
            Some(d) => tail.extend_from_slice(&d),
            None => {
                return RenderSig::Absent(format!("sources[{i}] ({id}) has no fingerprint.sha256"));
            }
        }
    }
    for (i, f) in plan
        .get("fonts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let id = f.get("id").and_then(Value::as_str).unwrap_or("?");
        match f.get("sha256").and_then(Value::as_str).and_then(digest) {
            Some(d) => tail.extend_from_slice(&d),
            None => return RenderSig::Absent(format!("fonts[{i}] ({id}) has no sha256")),
        }
    }
    // `/1` (decision 0014): image files follow the fonts, in array order
    for (i, f) in plan
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let id = f.get("id").and_then(Value::as_str).unwrap_or("?");
        match f.get("sha256").and_then(Value::as_str).and_then(digest) {
            Some(d) => tail.extend_from_slice(&d),
            None => return RenderSig::Absent(format!("images[{i}] ({id}) has no sha256")),
        }
    }
    // `/1` (decision 0045): recorded cursor tracks follow the images, in array order
    for (i, f) in plan
        .get("cursors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let id = f.get("id").and_then(Value::as_str).unwrap_or("?");
        match f.get("sha256").and_then(Value::as_str).and_then(digest) {
            Some(d) => tail.extend_from_slice(&d),
            None => return RenderSig::Absent(format!("cursors[{i}] ({id}) has no sha256")),
        }
    }
    let mut v = plan.clone();
    normalise(&mut v);
    let canon = match jcs::canonicalize(&v) {
        Ok(c) => c,
        Err(e) => return RenderSig::Absent(e.to_string()),
    };
    let mut h = blake3::Hasher::new();
    h.update(&canon).update(&tail);
    RenderSig::Sig(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `render_sig` of a typed plan (serialised as its schema's JSON first).
#[must_use]
pub fn render_sig_of(plan: &gmclip_plan::Plan) -> RenderSig {
    match serde_json::to_value(plan) {
        Ok(v) => render_sig(&v),
        Err(e) => RenderSig::Absent(format!("plan does not serialise: {e}")),
    }
}

/// Loudness as measured on the export's mix (B-12): after normalisation, and before.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessRecord {
    /// LUFS; null for a silent export (−inf).
    pub integrated: Option<f64>,
    /// dBTP; null for a silent export.
    pub true_peak: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<LoudnessBefore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    /// `static` mode: whether the target was reached within ±0.2 LU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_reached: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessBefore {
    pub integrated: Option<f64>,
    pub true_peak: Option<f64>,
}

/// What the export was meant to last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationExpected {
    pub frames: u64,
    pub fps_num: u64,
    pub fps_den: u64,
    pub audio_samples: u64,
}

/// The export record (SPEC §5.14), written beside the MP4 as `<stem>.export.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub render_sig: Option<String>,
    /// Why `render_sig` is absent, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_sig_absent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<String>,
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_rev: Option<String>,
    pub duration_expected: DurationExpected,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_hash: Option<String>,
    pub file: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<LoudnessRecord>,
    #[serde(default)]
    pub draft_mattes: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub blocked: Vec<String>,
    pub app_version: String,
    pub renderer_version: String,
}

impl ExportRecord {
    /// Sets `render_sig` / `render_sig_absent` from a plan.
    pub fn set_sig(&mut self, sig: RenderSig) {
        match sig {
            RenderSig::Sig(s) => {
                self.render_sig = Some(s);
                self.render_sig_absent = None;
            }
            RenderSig::Absent(why) => {
                self.render_sig = None;
                self.render_sig_absent = Some(why);
            }
        }
    }

    /// Where the record of `mp4` lives.
    #[must_use]
    pub fn path_for(mp4: &Path) -> PathBuf {
        let stem = mp4
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        mp4.with_file_name(format!("{stem}.export.json"))
    }

    /// Writes the record beside `mp4` (temp file + rename).
    pub fn write(&self, mp4: &Path) -> std::io::Result<PathBuf> {
        let path = Self::path_for(mp4);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn read(mp4: &Path) -> std::io::Result<Self> {
        serde_json::from_slice(&std::fs::read(Self::path_for(mp4))?).map_err(std::io::Error::other)
    }
}

/// How many replaced files `superseded/` keeps.
pub const SUPERSEDED_KEEP: usize = 3;

/// Moves the current `out` (and its record) to `superseded/<stem>.<n>[.<ext>]`, `n`
/// one past the highest there, rewriting the moved record's `file` to its new name, then
/// deletes all but the newest `keep` (≥ 1). Returns where the old file went (None when
/// there was none). Prefer [`install_export`], which only rotates once the new file is
/// complete. Not safe against two rotations of the same `out` at once.
pub fn rotate_superseded(out: &Path, keep: usize) -> std::io::Result<Option<PathBuf>> {
    if keep == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "keep must be ≥ 1",
        ));
    }
    if !out.exists() {
        return Ok(None);
    }
    let dir = out.parent().unwrap_or(Path::new(".")).join("superseded");
    std::fs::create_dir_all(&dir)?;
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = out.extension().map(|s| s.to_string_lossy().into_owned());
    let media_name = |n: u64| match &ext {
        Some(e) => format!("{stem}.{n}.{e}"),
        None => format!("{stem}.{n}"),
    };
    let index_of = |name: &str| -> Option<u64> {
        let rest = name.strip_prefix(&format!("{stem}."))?;
        let (n, tail) = rest.split_once('.').unwrap_or((rest, ""));
        let ok = tail == "export.json" || ext.as_deref().map_or(tail.is_empty(), |e| tail == e);
        ok.then(|| n.parse().ok()).flatten()
    };
    let mut seen: Vec<u64> = std::fs::read_dir(&dir)?
        .filter_map(|e| {
            e.ok()
                .and_then(|e| index_of(&e.file_name().to_string_lossy()))
        })
        .collect();
    let n = seen.iter().copied().max().map_or(1, |m| m + 1);
    let moved = dir.join(media_name(n));
    std::fs::rename(out, &moved)?;
    let record = ExportRecord::path_for(out);
    if record.exists() {
        let moved_record = dir.join(format!("{stem}.{n}.export.json"));
        if let Err(e) = std::fs::rename(&record, &moved_record) {
            // Put the media back so the pair stays together.
            let _ = std::fs::rename(&moved, out);
            return Err(e);
        }
        if let Ok(mut r) = serde_json::from_slice::<ExportRecord>(&std::fs::read(&moved_record)?) {
            r.file = moved.to_string_lossy().into_owned();
            let _ = std::fs::write(
                &moved_record,
                serde_json::to_vec_pretty(&r).map_err(std::io::Error::other)?,
            );
        }
    }
    seen.push(n);
    seen.sort_unstable();
    seen.dedup();
    let drop = seen.len().saturating_sub(keep);
    for old in seen.iter().take(drop) {
        for name in [media_name(*old), format!("{stem}.{old}.export.json")] {
            match std::fs::remove_file(dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    Ok(Some(moved))
}

/// Installs a finished export: `rendered` (a complete file, e.g. `out.partial.mp4`)
/// becomes `out`, the previous `out` and its record rotate into `superseded/`, and
/// `record` is written beside `out`. A render that failed or was cancelled never calls
/// this, so the current final is never lost. If moving the new file in fails, the old
/// one is put back.
pub fn install_export(
    rendered: &Path,
    out: &Path,
    record: &ExportRecord,
    keep: usize,
) -> std::io::Result<Option<PathBuf>> {
    if !rendered.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the rendered file is missing",
        ));
    }
    let moved = rotate_superseded(out, keep)?;
    if let Err(e) = std::fs::rename(rendered, out) {
        if let Some(m) = &moved {
            let _ = std::fs::rename(m, out);
        }
        return Err(e);
    }
    record.write(out)?;
    Ok(moved)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    fn example() -> Value {
        serde_json::from_str(include_str!(
            "../../../../schemas/examples/h79-parts-1-3.renderplan.json"
        ))
        .unwrap()
    }

    fn sig(v: &Value) -> String {
        match render_sig(v) {
            RenderSig::Sig(s) => s,
            RenderSig::Absent(why) => panic!("absent: {why}"),
        }
    }

    /// The pinned signature of RP §2.3's example (schemas/examples/h79-parts-1-3).
    /// Any OS that computes another value fails here: this is the cross-machine check
    /// (CI on macOS/Windows/Linux runs it; the Air runs the same binary test).
    const H79_PARTS_1_3_SIG: &str =
        "595ef8b322511093d5f609b3ae7430ea4ef57a213b2af5ddd32f603c9011a5c7";

    #[test]
    fn pinned_signature_of_the_rp_example() {
        assert_eq!(sig(&example()), H79_PARTS_1_3_SIG);
    }

    #[test]
    fn paths_do_not_change_the_signature() {
        let a = example();
        let mut b = a.clone();
        b["sources"][0]["path"] = json!("/Volumes/Air/streams/source.mkv");
        b["sources"][1]["path"] = json!("C:\\ci\\proxy.mp4");
        b["fonts"][0]["path"] = json!("/usr/share/fonts/neometric.otf");
        assert_eq!(sig(&a), sig(&b));
        // Typed round trip (f64 fields re-serialised as floats) signs the same.
        let typed = gmclip_plan::parse(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!(render_sig_of(&typed), RenderSig::Sig(sig(&a)));
    }

    #[test]
    fn content_changes_do() {
        let a = example();
        let mut b = a.clone();
        b["parts"][0]["out"] = json!([0, 54]);
        assert_ne!(sig(&a), sig(&b));
        let mut c = a.clone();
        c["sources"][0]["fingerprint"]["sha256"] = json!("0".repeat(64));
        assert_ne!(sig(&a), sig(&c));
        // Below the 1/1024 quantum: same; at it: different.
        let mut d = a.clone();
        d["parts"][0]["layers"][0]["crop"]["x"] = json!(1222.0001);
        assert_eq!(sig(&a), sig(&d));
        let mut e = a;
        e["parts"][0]["layers"][0]["crop"]["x"] = json!(1222.0 + 1.0 / 1024.0);
        assert_ne!(sig(&d), sig(&e));
    }

    /// Decision 0045: a cursor track's file signs with the plan, its path does not.
    #[test]
    fn a_cursor_track_file_signs() {
        let mut a = example();
        a["cursors"] = json!([{"id": "c", "source": "mkv1440", "path": "/a/input.json",
            "sha256": "1".repeat(64), "shapes": [], "scale": 1.0}]);
        let mut b = a.clone();
        b["cursors"][0]["path"] = json!("/b/input.json");
        assert_eq!(sig(&a), sig(&b));
        b["cursors"][0]["sha256"] = json!("2".repeat(64));
        assert_ne!(sig(&a), sig(&b));
        b["cursors"][0]["sha256"] = json!("nope");
        assert_eq!(
            render_sig(&b),
            RenderSig::Absent("cursors[0] (c) has no sha256".into())
        );
    }

    #[test]
    fn absent_without_a_source_sha256() {
        let mut a = example();
        a["sources"][1]["fingerprint"]
            .as_object_mut()
            .unwrap()
            .remove("sha256");
        assert_eq!(
            render_sig(&a),
            RenderSig::Absent("sources[1] (proxy) has no fingerprint.sha256".into())
        );
        let mut r = record();
        r.set_sig(render_sig(&a));
        assert!(r.render_sig.is_none() && r.render_sig_absent.is_some());
    }

    #[test]
    fn key_order_and_whitespace_do_not_matter() {
        let a = example();
        let text = serde_json::to_string_pretty(&a).unwrap();
        let b: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(sig(&a), sig(&b));
    }

    fn record() -> ExportRecord {
        ExportRecord {
            render_sig: None,
            render_sig_absent: None,
            framing: Some("9:16".into()),
            preset: "reels".into(),
            input_rev: None,
            duration_expected: DurationExpected {
                frames: 397,
                fps_num: 30,
                fps_den: 1,
                audio_samples: 635_200,
            },
            frames_hash: None,
            file: "out.mp4".into(),
            sha256: "0".repeat(64),
            loudness: Some(LoudnessRecord {
                integrated: Some(-16.0),
                true_peak: Some(-1.6),
                before: Some(LoudnessBefore {
                    integrated: Some(-22.1),
                    true_peak: Some(-4.0),
                }),
                gain_db: Some(6.1),
                target_reached: Some(true),
            }),
            draft_mattes: vec![],
            notes: vec![],
            blocked: vec![],
            app_version: "0.0.1".into(),
            renderer_version: "0.0.1".into(),
        }
    }

    #[test]
    fn meta_provenance_and_mtimes_do_not_change_the_signature() {
        // Decision 0004: the converter and the planner sign the same cut alike.
        let a = example();
        let mut b = a.clone();
        b["meta"] = json!({ "title": "same cut, other generator", "generator": { "name": "planner", "version": "9" } });
        b["text_events"][0]["provenance"] = json!({ "kind": "agent" });
        b["audio"]["envelopes"][0]
            .as_object_mut()
            .unwrap()
            .remove("provenance");
        b["sources"][0]["fingerprint"]["mtime_ns"] = json!(1_234_567_890_123_456_789_i64);
        assert_eq!(sig(&a), sig(&b));
        // Size is content and stays signed.
        let mut c = a.clone();
        c["sources"][0]["fingerprint"]["size"] = json!(1);
        assert_ne!(sig(&a), sig(&c));
    }

    #[test]
    fn a_text_style_called_path_is_signed() {
        let a = example();
        let mut b = a.clone();
        b["text_styles"]["path"] = b["text_styles"]["Speech"].clone();
        assert_ne!(sig(&a), sig(&b));
        let mut c = b.clone();
        c["text_styles"]["path"]["size"] = json!(90);
        assert_ne!(sig(&b), sig(&c));
    }

    #[test]
    fn digests_must_be_lowercase_hex() {
        let mut a = example();
        let upper = a["sources"][0]["fingerprint"]["sha256"]
            .as_str()
            .unwrap()
            .to_uppercase();
        a["sources"][0]["fingerprint"]["sha256"] = json!(upper);
        assert!(matches!(render_sig(&a), RenderSig::Absent(_)));
        let mut b = example();
        b["sources"] = json!([]);
        assert_eq!(
            render_sig(&b),
            RenderSig::Absent("the plan has no sources".into())
        );
    }

    #[test]
    fn install_keeps_the_current_final_until_the_new_one_is_complete() {
        let dir = std::env::temp_dir().join(format!("gmclip-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("final.mp4");
        let partial = dir.join("final.partial.mp4");
        std::fs::write(&partial, "one").unwrap();
        assert!(
            install_export(&partial, &out, &record(), 3)
                .unwrap()
                .is_none()
        );
        // A cancelled render leaves no partial: nothing moves.
        assert!(install_export(&partial, &out, &record(), 3).is_err());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "one");
        std::fs::write(&partial, "two").unwrap();
        let moved = install_export(&partial, &out, &record(), 3)
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "two");
        // The superseded record names its own file.
        let r: ExportRecord = serde_json::from_slice(
            &std::fs::read(dir.join("superseded/final.1.export.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(r.file, moved.to_string_lossy());
        assert!(rotate_superseded(&out, 0).is_err());
        // No extension: still rotated and pruned.
        let bare = dir.join("bare");
        for k in 0..5 {
            std::fs::write(&bare, format!("{k}")).unwrap();
            rotate_superseded(&bare, 2).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.join("superseded"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("bare"))
            .collect();
        names.sort();
        assert_eq!(names, ["bare.4", "bare.5"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn superseded_keeps_the_last_three() {
        let dir = std::env::temp_dir().join(format!("gmclip-superseded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("final.mp4");
        for k in 1..=5u32 {
            let moved = rotate_superseded(&out, SUPERSEDED_KEEP).unwrap();
            assert_eq!(moved.is_some(), k > 1);
            std::fs::write(&out, format!("render {k}")).unwrap();
            let mut r = record();
            r.notes = vec![format!("render {k}")];
            r.write(&out).unwrap();
        }
        let sup = dir.join("superseded");
        let mut names: Vec<String> = std::fs::read_dir(&sup)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // Renders 1–4 were replaced; the 4th replacement rotated render 1 out.
        assert_eq!(
            names,
            [
                "final.2.export.json",
                "final.2.mp4",
                "final.3.export.json",
                "final.3.mp4",
                "final.4.export.json",
                "final.4.mp4"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(sup.join("final.2.mp4")).unwrap(),
            "render 2"
        );
        assert_eq!(ExportRecord::read(&out).unwrap().notes, ["render 5"]);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "render 5");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod dump {
    /// `GMCLIP_DUMP_CANON=<file>` writes the canonical bytes of the RP example (for the
    /// independent cross-check in the B-16 notes).
    #[test]
    fn dump_canonical_bytes_when_asked() {
        if let Ok(p) = std::env::var("GMCLIP_DUMP_CANON") {
            let mut v: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../schemas/examples/h79-parts-1-3.renderplan.json"
            ))
            .unwrap();
            super::normalise(&mut v);
            std::fs::write(p, super::jcs::canonicalize(&v).unwrap()).unwrap();
        }
    }
}
