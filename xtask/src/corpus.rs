//! `cargo xt corpus verify|print` (W0-05, SPEC §17 Corpus): check every row of
//! `test-media/manifest.toml` against the bytes on this machine.
//!
//! - `verify [--skip-missing] [--class C] [--id ID]`: every selected row's file exists,
//!   has the recorded size and sha256, and, where the row names a goldens file, that
//!   file is in the tree and records the same source sha256. A missing lab file
//!   (lab-only, window, golden) fails unless `--skip-missing` (a machine without
//!   the lab sources, e.g. the Air before they are copied); a missing corpus file always
//!   fails (`tools/make_corpus.sh` recreates it), as does a size or hash mismatch or a
//!   run that checked nothing. Loading also requires the golden set to be complete.
//! - `print [--class C] [--id ID]`: the size and sha256 of each present file, as
//!   `id size sha256`, for updating rows after `tools/make_corpus.sh` reruns.
//!
//! Hashing shells out to `sha256sum` (macOS 14+, Linux) or `shasum -a 256`: xtask runs
//! unoptimised and the lab sources are several GB.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::minitoml::{self, Table, Value};

pub const CLASSES: &[&str] = &["corpus", "lab-only", "golden", "window"];

/// One `[[file]]` row.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub class: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub goldens: Option<String>,
}

fn field<'a>(t: &'a Table, k: &str, id: &str) -> Result<&'a Value, String> {
    t.get(k).ok_or_else(|| format!("row '{id}': missing '{k}'"))
}

/// Parse and validate the manifest; relative paths resolve against `test-media/`.
pub fn load(manifest: &Path) -> Result<Vec<Row>, String> {
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let doc = minitoml::parse(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    match doc.get("schema").and_then(Value::as_str) {
        Some("gmclip.test-media/0") => {}
        other => {
            return Err(format!(
                "manifest schema must be gmclip.test-media/0, got {other:?}"
            ));
        }
    }
    let base = manifest.parent().unwrap_or(Path::new("."));
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    for v in doc
        .get("file")
        .and_then(Value::as_array)
        .unwrap_or_default()
    {
        let t = v.as_table().ok_or("[[file]] must be a table")?;
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a [[file]] row has no id")?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("duplicate id '{id}'"));
        }
        let s = |k: &str| -> Result<String, String> {
            field(t, k, id)?
                .as_str()
                .map(str::to_owned)
                .ok_or(format!("row '{id}': '{k}' must be a string"))
        };
        let class = s("class")?;
        if !CLASSES.contains(&class.as_str()) {
            return Err(format!("row '{id}': class '{class}' not in {CLASSES:?}"));
        }
        let sha256 = s("sha256")?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(format!(
                "row '{id}': sha256 must be 64 lowercase hex digits"
            ));
        }
        s("purpose")?;
        let size = field(t, "size", id)?
            .as_int()
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(format!("row '{id}': size must be a non-negative integer"))?;
        let raw = s("path")?;
        // `lab:` names a file in the lab media folder; a leading `/` is absolute on every
        // OS (Windows would otherwise join it under test-media\, with no drive letter)
        let path = if let Some(rel) = raw.strip_prefix("lab:") {
            lab_media().join(rel)
        } else if raw.starts_with('/') || Path::new(&raw).is_absolute() {
            PathBuf::from(&raw)
        } else {
            base.join(&raw)
        };
        if class == "corpus" {
            s("recipe")?;
            s("licence")?;
            s("r2")?;
        }
        let goldens = t.get("goldens").and_then(Value::as_str).map(str::to_owned);
        rows.push(Row {
            id: id.to_owned(),
            class,
            path,
            sha256,
            size,
            goldens,
        });
    }
    if rows.is_empty() {
        return Err("manifest has no [[file]] rows".into());
    }
    golden_set_complete(&rows)?;
    Ok(rows)
}

/// The lab media folder (the same rule as `gmclip_model::lab::root`): `GMCLIP_LAB_MEDIA`,
/// else `~/Development/gmclip-lab-media`.
pub fn lab_media() -> PathBuf {
    if let Some(p) = std::env::var_os("GMCLIP_LAB_MEDIA").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Development/gmclip-lab-media")
}

/// The private golden set (W0-05): the 11 golden cuts, each with its plan, spec,
/// receipt, ASS and reference MP4, and heard.json for every cut but h79 v4 (whose cues
/// are imported from its ASS). Row ids are `golden-<cut>-<kind>`. (A manifest with no
/// golden rows at all is a fixture; the committed one is tested to have them.)
fn golden_set_complete(rows: &[Row]) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut cuts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.class == "golden") {
        let rest =
            r.id.strip_prefix("golden-")
                .ok_or(format!("golden id '{}' must start with golden-", r.id))?;
        let (cut, kind) = rest
            .split_once('-')
            .ok_or(format!("golden id '{}' has no kind", r.id))?;
        cuts.entry(cut)
            .or_default()
            .insert(kind.split('-').next().unwrap_or(kind));
    }
    if !cuts.is_empty() && cuts.len() != 11 {
        return Err(format!("golden set has {} cuts, not 11", cuts.len()));
    }
    for (cut, kinds) in &cuts {
        let mut need = vec!["plan", "spec", "receipt", "ass", "reference"];
        if *cut != "h79" {
            need.push("heard");
        }
        for k in need {
            if !kinds.contains(k) {
                return Err(format!("golden cut {cut} has no {k} row"));
            }
        }
    }
    Ok(())
}

/// sha256 of a file via the platform tool.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let attempts: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (tool, args) in attempts {
        let Ok(out) = Command::new(tool).args(args).arg(path).output() else {
            continue;
        };
        if !out.status.success() {
            return Err(format!(
                "{tool} {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let hex = text.split_whitespace().next().unwrap_or_default();
        if hex.len() == 64 {
            return Ok(hex.to_ascii_lowercase());
        }
    }
    // Windows: certutil ships with the OS ("SHA256 hash of …:" then the hex on its own line)
    if cfg!(windows)
        && let Ok(out) = Command::new("certutil")
            .args(["-hashfile"])
            .arg(path)
            .arg("SHA256")
            .output()
    {
        if !out.status.success() {
            return Err(format!(
                "certutil {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(hex) = text
            .lines()
            .map(|l| l.trim().replace(' ', ""))
            .find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Ok(hex.to_ascii_lowercase());
        }
    }
    Err("no sha256 tool found (need sha256sum, shasum or certutil)".into())
}

enum Outcome {
    Ok,
    Missing,
    Bad(String),
}

fn check_row(root: &Path, r: &Row) -> Outcome {
    let meta = match std::fs::metadata(&r.path) {
        Ok(m) => m,
        Err(_) => return Outcome::Missing,
    };
    if meta.len() != r.size {
        return Outcome::Bad(format!("size {} != manifest {}", meta.len(), r.size));
    }
    match sha256_file(&r.path) {
        Ok(h) if h == r.sha256 => {}
        Ok(h) => return Outcome::Bad(format!("sha256 {h} != manifest {}", r.sha256)),
        Err(e) => return Outcome::Bad(e),
    }
    if let Some(g) = &r.goldens {
        let gp = root.join("test-media").join(g);
        let Ok(text) = std::fs::read_to_string(&gp) else {
            return Outcome::Bad(format!("goldens file {} missing", gp.display()));
        };
        let recorded = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["source"]["sha256"].as_str().map(str::to_owned));
        if recorded.as_deref() != Some(r.sha256.as_str()) {
            return Outcome::Bad(format!(
                "goldens {g} were made from sha256 {recorded:?}, not this file"
            ));
        }
    }
    Outcome::Ok
}

struct Filter {
    class: Option<String>,
    id: Option<String>,
    skip_missing: bool,
}

fn parse_args(args: &[String]) -> Result<Filter, String> {
    let mut f = Filter {
        class: None,
        id: None,
        skip_missing: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--skip-missing" => f.skip_missing = true,
            "--class" => f.class = Some(it.next().ok_or("--class needs a value")?.clone()),
            "--id" => f.id = Some(it.next().ok_or("--id needs a value")?.clone()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(f)
}

/// Run `cargo xt corpus <args>`.
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let (cmd, rest) = args
        .split_first()
        .ok_or("usage: cargo xt corpus <verify|print> [...]")?;
    let filter = parse_args(rest)?;
    let rows: Vec<Row> = load(&root.join("test-media/manifest.toml"))?
        .into_iter()
        .filter(|r| filter.class.as_ref().is_none_or(|c| &r.class == c))
        .filter(|r| filter.id.as_ref().is_none_or(|i| &r.id == i))
        .collect();
    match cmd.as_str() {
        "verify" => verify(root, &rows, filter.skip_missing),
        "print" => {
            for r in &rows {
                match std::fs::metadata(&r.path) {
                    Ok(m) => println!("{} {} {}", r.id, m.len(), sha256_file(&r.path)?),
                    Err(_) => println!("{} missing {}", r.id, r.path.display()),
                }
            }
            Ok(())
        }
        other => Err(format!("unknown corpus command '{other}'")),
    }
}

fn verify(root: &Path, rows: &[Row], skip_missing: bool) -> Result<(), String> {
    let results: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::new());
    let next = Mutex::new(0usize);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().expect("lock");
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(r) = rows.get(i) else { break };
                    let o = check_row(root, r);
                    results.lock().expect("lock").push((i, o));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("lock");
    results.sort_by_key(|(i, _)| *i);
    let (mut ok, mut missing, mut bad) = (0, Vec::new(), Vec::new());
    let mut missing_corpus = false;
    for (i, o) in results {
        let r = &rows[i];
        match o {
            Outcome::Ok => ok += 1,
            Outcome::Missing => {
                missing_corpus |= r.class == "corpus";
                missing.push(format!("  missing  {:<40} {}", r.id, r.path.display()))
            }
            Outcome::Bad(e) => bad.push(format!("  MISMATCH {:<40} {e}", r.id)),
        }
    }
    for l in missing.iter().chain(&bad) {
        println!("{l}");
    }
    println!(
        "corpus verify: {ok} ok, {} missing{}, {} mismatched ({} rows)",
        missing.len(),
        if skip_missing && !missing.is_empty() {
            " (skipped)"
        } else {
            ""
        },
        bad.len(),
        rows.len()
    );
    // `--skip-missing` covers the lab files only: the corpus is reproducible anywhere.
    if !bad.is_empty() || (!missing.is_empty() && !skip_missing) || missing_corpus {
        return Err("corpus verify failed".into());
    }
    if ok == 0 {
        return Err("corpus verify checked nothing".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_manifest_parses() {
        let rows = load(&crate::repo_root().join("test-media/manifest.toml")).unwrap();
        for class in CLASSES {
            assert!(rows.iter().any(|r| r.class == *class), "no {class} rows");
        }
    }

    #[test]
    fn bad_rows_are_rejected() {
        let dir = std::env::temp_dir().join(format!("gmclip-corpus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = dir.join("manifest.toml");
        let good = "schema = \"gmclip.test-media/0\"\n[[file]]\nid = \"x\"\nclass = \"lab-only\"\npath = \"/x\"\nsize = 1\npurpose = \"p\"\nsha256 = \"";
        let hex = "0".repeat(64);
        std::fs::write(&m, format!("{good}{hex}\"\n")).unwrap();
        assert!(load(&m).is_ok());
        std::fs::write(&m, format!("{good}{}\"\n", "A".repeat(64))).unwrap();
        assert!(load(&m).is_err());
        std::fs::write(&m, format!("{good}{hex}\"\n[[file]]\nid = \"x\"\n")).unwrap();
        assert!(load(&m).unwrap_err().contains("duplicate"));
        std::fs::write(
            &m,
            format!("{}{hex}\"\n", good.replace("lab-only", "corpus")),
        )
        .unwrap();
        assert!(load(&m).unwrap_err().contains("recipe"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wrong_hash_fails_and_a_right_one_passes() {
        let dir = std::env::temp_dir().join(format!("gmclip-corpus-h-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f.bin");
        std::fs::write(&f, b"abc").unwrap();
        let mut r = Row {
            id: "f".into(),
            class: "lab-only".into(),
            path: f.clone(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            size: 3,
            goldens: None,
        };
        assert!(matches!(check_row(&dir, &r), Outcome::Ok));
        r.sha256 = "0".repeat(64);
        assert!(matches!(check_row(&dir, &r), Outcome::Bad(_)));
        r.path = dir.join("absent");
        assert!(matches!(check_row(&dir, &r), Outcome::Missing));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
