//! `cargo xt stream-sources` (W0-12): check `test-media/stream-sources.toml`.
//!
//! Always: every `[[source.window]]` (alignment.json) of a `hires_mkv` source has
//! `mkv_first_frame = start_s * fps + offset_at(start_s)` under the piecewise
//! `[[source.offset]]` table, lies wholly in one offset regime and outside every
//! `[[source.unmeasured]]` span; the offsets tile `[0, ∞)`; each source's files are
//! manifest rows with the same (lab) path and sha256. On the lab, when `alignment.json` is
//! in the lab media folder with its recorded sha256, every one of its windows must appear
//! here unchanged (and nothing else).

use std::path::Path;

use crate::minitoml::{self, Table, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub start_s: i64,
    pub end_s: i64,
    pub mkv_first_frame: i64,
    pub frame_offset: i64,
    pub audio_offset_s: f64,
    pub local_offsets_nonzero: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct Offset {
    pub from_s: f64,
    pub until_s: f64,
    pub frames: i64,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub vid: String,
    pub mode: String,
    pub offsets: Vec<Offset>,
    pub windows: Vec<Window>,
    /// Native-time spans no measurement covers.
    pub unmeasured: Vec<(f64, f64)>,
    /// (manifest id, path, sha256) of the picture and the audio member.
    pub picture: (String, String, String),
    pub audio: (String, String, String),
}

pub struct StreamSources {
    pub fps: i64,
    pub sources: Vec<Source>,
    /// Relative to the lab media folder.
    pub alignment_path: String,
    pub alignment_sha256: String,
}

fn get<'a>(t: &'a Table, k: &str) -> Result<&'a Value, String> {
    t.get(k)
        .ok_or_else(|| format!("stream-sources: missing '{k}'"))
}
fn int(t: &Table, k: &str) -> Result<i64, String> {
    get(t, k)?
        .as_int()
        .ok_or_else(|| format!("stream-sources: '{k}' must be an integer"))
}
fn float(t: &Table, k: &str) -> Result<f64, String> {
    get(t, k)?
        .as_float()
        .ok_or_else(|| format!("stream-sources: '{k}' must be a number"))
}
fn string(t: &Table, k: &str) -> Result<String, String> {
    get(t, k)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("stream-sources: '{k}' must be a string"))
}
fn tables<'a>(t: &'a Table, k: &str) -> Vec<&'a Table> {
    t.get(k)
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_table)
        .collect()
}

pub fn parse(text: &str) -> Result<StreamSources, String> {
    let doc = minitoml::parse(text).map_err(|e| format!("stream-sources.toml: {e}"))?;
    if doc.get("schema").and_then(Value::as_str) != Some("gmclip.stream-sources/0") {
        return Err("stream-sources.toml: schema must be gmclip.stream-sources/0".into());
    }
    let prov = get(&doc, "provenance")?
        .as_table()
        .ok_or("provenance must be a table")?;
    let al = get(prov, "alignment")?
        .as_table()
        .ok_or("provenance.alignment")?;
    let mut sources = Vec::new();
    for s in tables(&doc, "source") {
        let mode = string(s, "mode")?;
        for k in [
            "picture_path",
            "picture_sha256",
            "audio_path",
            "audio_sha256",
            "picture_manifest_id",
            "audio_manifest_id",
        ] {
            string(s, k)?;
        }
        let offsets = tables(s, "offset")
            .into_iter()
            .map(|o| {
                Ok(Offset {
                    from_s: float(o, "from_s")?,
                    until_s: float(o, "until_s")?,
                    frames: int(o, "frames")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let windows = tables(s, "window")
            .into_iter()
            .map(|w| {
                Ok(Window {
                    start_s: int(w, "start_s")?,
                    end_s: int(w, "end_s")?,
                    mkv_first_frame: int(w, "mkv_first_frame")?,
                    frame_offset: int(w, "frame_offset")?,
                    audio_offset_s: float(w, "audio_offset_s")?,
                    local_offsets_nonzero: get(w, "local_offsets_nonzero")?
                        .as_array()
                        .ok_or("local_offsets_nonzero must be an array")?
                        .iter()
                        .map(|v| v.as_int().ok_or("local_offsets_nonzero holds integers"))
                        .collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let unmeasured = tables(s, "unmeasured")
            .into_iter()
            .map(|u| Ok((float(u, "from_s")?, float(u, "until_s")?)))
            .collect::<Result<Vec<_>, String>>()?;
        let triple = |kind: &str| -> Result<(String, String, String), String> {
            Ok((
                string(s, &format!("{kind}_manifest_id"))?,
                string(s, &format!("{kind}_path"))?,
                string(s, &format!("{kind}_sha256"))?,
            ))
        };
        if mode == "hires_mkv" && offsets.is_empty() {
            return Err(format!(
                "stream-sources: {} is hires_mkv with no [[source.offset]]",
                string(s, "vid")?
            ));
        }
        if mode != "hires_mkv" && mode != "native" {
            return Err(format!("stream-sources: unknown mode '{mode}'"));
        }
        sources.push(Source {
            vid: string(s, "vid")?,
            mode,
            offsets,
            windows,
            unmeasured,
            picture: triple("picture")?,
            audio: triple("audio")?,
        });
    }
    Ok(StreamSources {
        fps: int(&doc, "fps")?,
        sources,
        alignment_path: string(al, "path")?,
        alignment_sha256: string(al, "sha256")?,
    })
}

impl Source {
    /// The picture offset in frames at native second `t`.
    pub fn offset_at(&self, t: f64) -> Option<i64> {
        self.offsets
            .iter()
            .find(|o| o.from_s <= t && t < o.until_s)
            .map(|o| o.frames)
    }
}

impl Source {
    /// The unmeasured span `[a, b)` overlaps, if any.
    fn unmeasured_overlap(&self, a: f64, b: f64) -> Option<(f64, f64)> {
        self.unmeasured
            .iter()
            .copied()
            .find(|&(lo, hi)| a < hi && b > lo)
    }
}

/// Checks that need only the file: offsets tile `[0, ∞)`; every alignment window lies
/// wholly in one offset regime outside the unmeasured spans and its
/// first MKV frame is `start * fps + offset`.
pub fn check_self(ss: &StreamSources) -> Result<usize, String> {
    let mut n = 0;
    for s in &ss.sources {
        let mut edge = 0.0;
        for o in &s.offsets {
            if o.from_s != edge || o.until_s <= o.from_s {
                return Err(format!("{}: offsets must tile [0, ∞) in order", s.vid));
            }
            edge = o.until_s;
        }
        let mut check = |what: &str,
                         start: i64,
                         end_s: f64,
                         first: i64,
                         stated: Option<i64>|
         -> Result<(), String> {
            let t = start as f64;
            if let Some((lo, hi)) = s.unmeasured_overlap(t, end_s) {
                return Err(format!(
                    "{}: {what} [{t}, {end_s}) overlaps unmeasured [{lo}, {hi})",
                    s.vid
                ));
            }
            let off = s
                .offset_at(t)
                .ok_or(format!("{}: no offset at {t}", s.vid))?;
            if s.offset_at(end_s - 1e-6) != Some(off) {
                return Err(format!("{}: {what} at {t} spans two offset regimes", s.vid));
            }
            if stated.is_some_and(|st| st != off) || start * ss.fps + off != first {
                return Err(format!(
                    "{}: {what} at {t}: offset {off} gives first frame {}, the record says {first} (offset {stated:?})",
                    s.vid,
                    start * ss.fps + off
                ));
            }
            n += 1;
            Ok(())
        };
        for w in &s.windows {
            check(
                "alignment window",
                w.start_s,
                w.end_s as f64,
                w.mkv_first_frame,
                Some(w.frame_offset),
            )?;
        }
    }
    Ok(n)
}

/// Every source's picture and audio member is a manifest row with the same path (in the
/// lab media folder) and sha256.
pub fn check_manifest(ss: &StreamSources, rows: &[crate::corpus::Row]) -> Result<(), String> {
    for s in &ss.sources {
        for (id, path, sha) in [&s.picture, &s.audio] {
            let row = rows
                .iter()
                .find(|r| &r.id == id)
                .ok_or(format!("{}: manifest has no row '{id}'", s.vid))?;
            if row.path != crate::corpus::lab_media().join(path) || &row.sha256 != sha {
                return Err(format!(
                    "{}: '{id}' path/sha256 differ from the manifest",
                    s.vid
                ));
            }
        }
    }
    Ok(())
}

/// Lab check against the measured alignment.json.
pub fn check_lab(ss: &StreamSources) -> Result<String, String> {
    let ap = crate::corpus::lab_media().join(&ss.alignment_path);
    if !ap.exists() {
        return Ok(format!(
            "alignment.json not on this machine ({}): lab check skipped",
            ap.display()
        ));
    }
    let sha = crate::corpus::sha256_file(&ap)?;
    if sha != ss.alignment_sha256 {
        return Err(format!(
            "alignment.json sha256 {sha} != recorded {}",
            ss.alignment_sha256
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ap).map_err(|e| e.to_string())?)
            .map_err(|e| format!("alignment.json: {e}"))?;
    let want: Vec<Window> = v["windows"]
        .as_array()
        .ok_or("alignment.json has no windows")?
        .iter()
        .map(|w| Window {
            start_s: w["window"][0].as_i64().unwrap_or(i64::MIN),
            end_s: w["window"][1].as_i64().unwrap_or(i64::MIN),
            mkv_first_frame: w["mkv_first_frame"].as_i64().unwrap_or(i64::MIN),
            frame_offset: w["frame_offset"].as_i64().unwrap_or(i64::MIN),
            audio_offset_s: w["audio_offset_s"].as_f64().unwrap_or(f64::NAN),
            local_offsets_nonzero: w["local_offsets_nonzero"]
                .as_array()
                .map(|a| a.iter().filter_map(serde_json::Value::as_i64).collect())
                .unwrap_or_default(),
        })
        .collect();
    let hires = ss
        .sources
        .iter()
        .find(|s| s.mode == "hires_mkv")
        .ok_or("no hires_mkv source")?;
    if hires.windows != want {
        return Err("stream-sources windows differ from alignment.json".into());
    }
    Ok(format!("alignment.json: {} windows equal", want.len()))
}

pub fn run(root: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(root.join("test-media/stream-sources.toml"))
        .map_err(|e| e.to_string())?;
    let ss = parse(&text)?;
    let n = check_self(&ss)?;
    check_manifest(
        &ss,
        &crate::corpus::load(&root.join("test-media/manifest.toml"))?,
    )?;
    let lab = check_lab(&ss)?;
    Ok(format!(
        "stream-sources: {} sources, {n} alignment windows reproduce their offsets, manifest rows agree; {lab}",
        ss.sources.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed() -> StreamSources {
        parse(
            &std::fs::read_to_string(crate::repo_root().join("test-media/stream-sources.toml"))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn every_alignment_window_reproduces_its_offset() {
        let ss = committed();
        assert_eq!(
            check_self(&ss).unwrap(),
            10,
            "alignment.json has ten windows"
        );
        let rows =
            crate::corpus::load(&crate::repo_root().join("test-media/manifest.toml")).unwrap();
        check_manifest(&ss, &rows).unwrap();
        let s = ss.sources.iter().find(|s| s.vid == "loc_4fadb756").unwrap();
        assert_eq!(s.offset_at(195.0), Some(-503));
        assert_eq!(s.offset_at(999.9), Some(-503));
        assert_eq!(s.offset_at(1000.0), Some(-1642));
        assert_eq!(s.offset_at(2675.0), Some(-1642));
    }

    #[test]
    fn the_lab_streams_are_listed() {
        let ss = committed();
        let mut vids: Vec<&str> = ss.sources.iter().map(|s| s.vid.as_str()).collect();
        vids.sort_unstable();
        assert_eq!(
            vids,
            [
                "loc_086da141",
                "loc_2fceb8f9",
                "loc_4fadb756",
                "loc_bd89f38b"
            ]
        );
    }

    #[test]
    fn a_wrong_offset_is_caught() {
        let text =
            std::fs::read_to_string(crate::repo_root().join("test-media/stream-sources.toml"))
                .unwrap()
                .replacen("mkv_first_frame = 5347", "mkv_first_frame = 5348", 1);
        assert!(check_self(&parse(&text).unwrap()).is_err());
        // a window moved into the unmeasured gap
        let text =
            std::fs::read_to_string(crate::repo_root().join("test-media/stream-sources.toml"))
                .unwrap()
                .replacen("\nuntil_s = 1196.0", "\nuntil_s = 1202.0", 1);
        assert!(
            check_self(&parse(&text).unwrap())
                .unwrap_err()
                .contains("unmeasured")
        );
    }

    #[test]
    fn matches_alignment_json_on_the_lab() {
        // Skips itself (with a note) where alignment.json is absent.
        check_lab(&committed()).unwrap();
    }
}
