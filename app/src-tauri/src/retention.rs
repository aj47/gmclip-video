//! Retention on the timeline (SPEC §5.14 review board; M2 "Work on" pick): a posted
//! cut's audience-retention curve, imported onto one export, mapped onto the cut's `out`
//! clock and drawn as a lane under the timeline with markers at the biggest falls, each
//! labelled with the part (and its beat) playing there.
//!
//! Where a curve comes from — nothing is ever uploaded, and nothing outside gmclip is
//! written:
//! - **a file the person picks** (its text, sent by the page; no path): YouTube Studio's
//!   audience-retention export (`Video position (%), Absolute audience retention (%)`),
//!   any CSV with a time (`m:ss` or seconds) or position column and a retention column,
//!   YouTube Analytics API rows (`elapsedVideoTimeRatio`, `audienceWatchRatio`), a TikTok
//!   or hand-made `retention_points` list (`requested_seconds`, `retention_percent`), or
//!   `{points: [[t_s, percent], …]}`;
//! - **Clippy's own data, read only**: its library's `data/retention.json` (the folder
//!   `GMCLIP_CLIPPY_LIBRARY` names) for the cut's
//!   hook (`moment.origin_ref`), the retention audit it names, and the Studio export that
//!   audit measured (`record.curve_path`); without a curve file, the audit's measured
//!   event points.
//!
//! The curve is stored as an immutable analysis blob on the export (`owner
//! export:<id>`, kind `retention`), so it stays with that file's record.

use std::path::{Path, PathBuf};

use gmclip_model::entities::{Cut, Moment, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{AppCore, ExportEntry, now_ms};
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The largest file or curve read.
const MAX_BYTES: usize = 1 << 20;
const MAX_POINTS: usize = 4_000;
/// Drop-off markers per curve.
const DROPS: usize = 3;

/// One imported curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    /// `[seconds on the cut's out clock, retention %]`, in time order.
    pub points: Vec<[f64; 2]>,
    /// `youtube`, `tiktok`, `instagram` or `unknown`.
    pub platform: String,
    /// `file:<name>` or `clippy:<hook id>`.
    pub source: String,
    /// The export's length the curve was mapped onto.
    pub duration_s: f64,
    pub imported_ms: i64,
    /// What the source said about itself (a window, a measured-at date), shown as text.
    #[serde(default)]
    pub note: Option<String>,
}

/// A fall in the curve: where it starts, how many points it loses over the window, and
/// which part plays there.
#[derive(Clone, Debug, Serialize)]
pub struct Drop {
    pub t_s: f64,
    pub to_s: f64,
    pub from_pct: f64,
    pub to_pct: f64,
    pub drop_pct: f64,
    /// 1-based part number on the cut as it is now, and its beat.
    pub part: Option<usize>,
    pub label: String,
}

fn num(s: &str) -> Option<f64> {
    let t = s.trim().trim_matches('"').trim_end_matches('%').trim();
    if let Some((m, sec)) = t.split_once(':') {
        let (m, sec): (f64, f64) = (m.trim().parse().ok()?, sec.trim().parse().ok()?);
        return Some(m * 60.0 + sec);
    }
    t.parse().ok().filter(|x: &f64| x.is_finite())
}

/// Parses a CSV curve. Columns are found by their header: a position (percent of the
/// video), or a time (seconds or `m:ss`); and a retention percent.
fn parse_csv(text: &str, duration_s: f64) -> Result<(Vec<[f64; 2]>, String), String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let head = lines.next().ok_or("an empty file")?;
    let cols: Vec<String> = head
        .trim_start_matches('\u{feff}')
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_ascii_lowercase())
        .collect();
    let find = |f: &dyn Fn(&str) -> bool| cols.iter().position(|c| f(c));
    let pos = find(&|c| c.contains("position") || c.contains("elapsedvideotimeratio"));
    let time = find(&|c| c.contains("time") || c.contains("second") || c == "t" || c == "sec");
    let ret = find(&|c| {
        c.contains("retention") || c.contains("audiencewatchratio") || c.contains("viewers")
    })
    .ok_or("no retention column (a header with “retention” in it)")?;
    let x = pos
        .or(time)
        .ok_or("no position or time column (a header with “position” or “time”)")?;
    let ratio_x = cols[x].contains("ratio");
    let ratio_y = cols[ret].contains("ratio");
    let platform = if cols[x].contains("video position") {
        "youtube"
    } else {
        "unknown"
    };
    let mut out = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split(',').collect();
        let (Some(a), Some(b)) = (
            f.get(x).and_then(|v| num(v)),
            f.get(ret).and_then(|v| num(v)),
        ) else {
            continue;
        };
        let t = if pos == Some(x) {
            if ratio_x {
                a * duration_s
            } else {
                a / 100.0 * duration_s
            }
        } else {
            a
        };
        out.push([t, if ratio_y { b * 100.0 } else { b }]);
    }
    Ok((out, platform.into()))
}

/// Parses a JSON curve (see the module docs for the shapes).
fn parse_json(v: &Value, duration_s: f64) -> Result<(Vec<[f64; 2]>, String), String> {
    let f = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64);
    // YouTube Analytics API rows
    let rows = v
        .as_array()
        .or_else(|| v.get("rows").and_then(Value::as_array));
    if let Some(rows) = rows
        && rows
            .first()
            .is_some_and(|r| r.get("elapsedVideoTimeRatio").is_some())
    {
        let pts = rows
            .iter()
            .filter_map(|r| {
                Some([
                    f(r, "elapsedVideoTimeRatio")? * duration_s,
                    f(r, "audienceWatchRatio")? * 100.0,
                ])
            })
            .collect();
        return Ok((pts, "youtube".into()));
    }
    // TikTok / hand-made `retention_points`
    let rp = v
        .get("retention_points")
        .or_else(|| v.pointer("/tiktok_comparison/retention_points"))
        .or_else(|| v.pointer("/record/tiktok_comparison/retention_points"));
    if let Some(Value::Array(a)) = rp {
        let pts = a
            .iter()
            .filter_map(|p| {
                Some([
                    f(p, "requested_seconds").or_else(|| f(p, "seconds"))?,
                    f(p, "retention_percent")?,
                ])
            })
            .collect();
        let platform = v
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("tiktok");
        return Ok((pts, platform.into()));
    }
    if let Some(Value::Array(a)) = v.get("points") {
        let pts = a
            .iter()
            .filter_map(|p| match p {
                Value::Array(xy) => Some([xy.first()?.as_f64()?, xy.get(1)?.as_f64()?]),
                o => Some([
                    f(o, "t_s").or_else(|| f(o, "seconds"))?,
                    f(o, "retention_pct").or_else(|| f(o, "percent"))?,
                ]),
            })
            .collect();
        let platform = v
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Ok((pts, platform.into()));
    }
    Err("not a retention curve this app reads (see the import formats)".into())
}

/// A curve from a picked file's text, mapped onto `duration_s` of the export.
pub fn parse(text: &str, duration_s: f64) -> Result<(Vec<[f64; 2]>, String), String> {
    if text.len() > MAX_BYTES {
        return Err("the file is larger than 1 MB".into());
    }
    let t = text.trim_start_matches('\u{feff}').trim_start();
    let (mut pts, platform) = if t.starts_with('{') || t.starts_with('[') {
        let v: Value = serde_json::from_str(t).map_err(|_| "not valid JSON")?;
        parse_json(&v, duration_s)?
    } else {
        parse_csv(t, duration_s)?
    };
    clean(&mut pts, duration_s)?;
    Ok((pts, platform))
}

/// Sorted, finite, inside the export (a point past its end is dropped), sane percents.
fn clean(pts: &mut Vec<[f64; 2]>, duration_s: f64) -> Result<(), String> {
    pts.retain(|p| {
        p[0].is_finite()
            && p[1].is_finite()
            && p[0] >= 0.0
            && p[0] <= duration_s + 0.5
            && (0.0..=1000.0).contains(&p[1])
    });
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    if pts.len() < 2 {
        return Err("fewer than two retention points inside the export's length".into());
    }
    if pts.len() > MAX_POINTS {
        return Err("more than 4000 points".into());
    }
    Ok(())
}

/// Retention at `t` (linear between points).
pub(crate) fn at(pts: &[[f64; 2]], t: f64) -> f64 {
    match pts.iter().position(|p| p[0] >= t) {
        Some(0) => pts[0][1],
        Some(i) => {
            let (a, b) = (pts[i - 1], pts[i]);
            let k = if b[0] > a[0] {
                (t - a[0]) / (b[0] - a[0])
            } else {
                0.0
            };
            a[1] + k * (b[1] - a[1])
        }
        None => pts.last().map_or(0.0, |p| p[1]),
    }
}

/// The biggest falls: over a window of max(1 s, 4 % of the cut), the starts losing the
/// most points, at least a window apart, largest first.
pub fn drops(pts: &[[f64; 2]], duration_s: f64) -> Vec<(f64, f64, f64, f64)> {
    let w = (duration_s * 0.04).max(1.0);
    let step = (duration_s / 400.0).max(0.05);
    let mut cands = Vec::new();
    let mut t = 0.0;
    while t + w <= duration_s + 1e-9 {
        let (a, b) = (at(pts, t), at(pts, t + w));
        if a - b > 0.0 {
            cands.push((t, t + w, a, b));
        }
        t += step;
    }
    cands.sort_by(|x, y| (y.2 - y.3).total_cmp(&(x.2 - x.3)));
    // a fall worth marking: at least 3 points and a fifth of the biggest
    let floor = cands.first().map_or(0.0, |c| (0.2 * (c.2 - c.3)).max(3.0));
    let mut out: Vec<(f64, f64, f64, f64)> = Vec::new();
    for c in cands {
        if out.len() == DROPS || c.2 - c.3 < floor {
            break;
        }
        if out.iter().all(|o| (o.0 - c.0).abs() >= w) {
            out.push(c);
        }
    }
    out
}

fn export_of(core: &AppCore, project: &str, export: &str) -> AppResult<ExportEntry> {
    core.exports(project)?
        .into_iter()
        .find(|e| e.id == export)
        .ok_or_else(|| AppError::not_found("no such export"))
}

/// Stores a curve on an export (an immutable analysis blob).
fn store(core: &AppCore, project: &str, export: &ExportEntry, c: &Curve) -> AppResult<()> {
    let body = serde_json::to_vec(c).map_err(|_| AppError::internal("serialise"))?;
    let rev = gmclip_ai::launcher::sha256::hex(&body);
    let p = core.project(project)?;
    p.handle
        .put_analysis(
            &format!("export:{}", export.id),
            "retention",
            &rev[..16],
            body,
        )
        .map_err(|e| AppError::internal(format!("saving the curve: {e}")))
}

/// Imports a picked file's text onto an export.
pub fn import(
    core: &AppCore,
    project: &str,
    export: &str,
    name: &str,
    text: &str,
) -> AppResult<Value> {
    let e = export_of(core, project, export)?;
    let duration = e.frames as f64 / e.rate();
    let (points, platform) = parse(text, duration).map_err(AppError::invalid)?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    let c = Curve {
        points,
        platform,
        source: format!("file:{name}"),
        duration_s: duration,
        imported_ms: now_ms(),
        note: None,
    };
    store(core, project, &e, &c)?;
    view(core, project, &e.cut)
}

/// A file Clippy's audit names, read only: absolute, a regular file, ≤ 1 MB.
fn read_small(p: &Path) -> Option<Vec<u8>> {
    if !p.is_absolute() || p.components().any(|c| c.as_os_str() == "..") {
        return None;
    }
    let m = std::fs::symlink_metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_BYTES as u64 {
        return None;
    }
    std::fs::read(p).ok()
}

/// Clippy's library on this Mac: the folder `GMCLIP_CLIPPY_LIBRARY` names.
pub fn clippy_library() -> Option<PathBuf> {
    std::env::var_os("GMCLIP_CLIPPY_LIBRARY")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// The curve Clippy measured for `hook`, read from its library at `library` (read only).
pub fn clippy_curve(library: &Path, hook: &str, duration_s: f64) -> Result<Curve, String> {
    let side =
        read_small(&library.join("data/retention.json")).ok_or("Clippy has no retention data")?;
    let v: Value =
        serde_json::from_slice(&side).map_err(|_| "Clippy's retention.json does not read")?;
    let obs = v
        .pointer(&format!(
            "/hooks/{}/observations",
            hook.replace('~', "~0").replace('/', "~1")
        ))
        .and_then(Value::as_array)
        .ok_or("Clippy has no retention measurement for this cut's hook")?;
    let o = obs
        .iter()
        .find(|o| o["platform"] == "youtube")
        .or_else(|| obs.first())
        .ok_or("Clippy has no retention measurement for this cut's hook")?;
    let platform = o["platform"].as_str().unwrap_or("unknown").to_owned();
    let window = o["measurement_window"].as_str().map(str::to_owned);
    // the audit's own export, when it is on this Mac
    let audit = o["source_audit"].as_str().map(PathBuf::from);
    let mut pts: Vec<[f64; 2]> = Vec::new();
    if let Some(a) = audit.as_deref().and_then(read_small)
        && let Ok(av) = serde_json::from_slice::<Value>(&a)
    {
        let rec = &av["record"];
        let video_s = rec["duration_seconds"].as_f64().unwrap_or(duration_s);
        let root = audit
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent);
        if let (Some(root), Some(rel)) = (root, rec["curve_path"].as_str())
            && !rel.starts_with('/')
            && let Some(csv) = read_small(&root.join(rel))
            && let Ok((p, _)) = parse_csv(&String::from_utf8_lossy(&csv), video_s)
        {
            // the curve is on the posted video's clock; this export's may differ
            let k = if video_s > 0.0 {
                duration_s / video_s
            } else {
                1.0
            };
            pts = p.into_iter().map(|[t, r]| [t * k, r]).collect();
        }
    }
    if pts.len() < 2 {
        // the measured event points the audit kept on the card
        for ev in o["events"].as_array().into_iter().flatten() {
            for (t, r) in [
                ("start_seconds", "curve_start"),
                ("end_seconds", "curve_end"),
            ] {
                if let (Some(t), Some(r)) = (ev[t].as_f64(), ev[r].as_f64()) {
                    pts.push([t, r]);
                }
            }
        }
    }
    clean(&mut pts, duration_s)?;
    Ok(Curve {
        points: pts,
        platform,
        source: format!("clippy:{hook}"),
        duration_s,
        imported_ms: now_ms(),
        note: window,
    })
}

/// The hook a cut was made from (its moment's `origin_ref`).
fn hook_of(core: &AppCore, project: &str, cut: &str) -> Option<String> {
    let p = core.project(project).ok()?;
    let doc = p.handle.snapshot().doc.clone();
    let c = doc.get(ulid_id::<Cut>(cut).ok()?)?;
    let m: &Moment = doc.get(c.moment?)?;
    m.origin_ref.clone()
}

/// "From Clippy": the curve Clippy measured for this export's hook, read only.
pub fn from_clippy(core: &AppCore, project: &str, export: &str) -> AppResult<Value> {
    let e = export_of(core, project, export)?;
    let hook = hook_of(core, project, &e.cut)
        .ok_or_else(|| AppError::invalid("this cut did not come from a Clippy hook"))?;
    let library =
        clippy_library().ok_or_else(|| AppError::not_found("no Clippy library on this Mac"))?;
    let duration = e.frames as f64 / e.rate();
    let c = clippy_curve(&library, &hook, duration).map_err(AppError::not_found)?;
    store(core, project, &e, &c)?;
    view(core, project, &e.cut)
}

/// The newest curve stored on one export, if any.
pub fn newest_curve(core: &AppCore, project: &str, export: &str) -> Option<Curve> {
    let p = core.project(project).ok()?;
    let owner = format!("export:{export}");
    p.handle
        .analysis_keys()
        .iter()
        .filter(|(o, k, _)| *o == owner && k == "retention")
        .filter_map(|(o, k, rev)| p.handle.analysis(o, k, rev))
        .filter_map(|b| serde_json::from_slice::<Curve>(&b).ok())
        .max_by_key(|c| c.imported_ms)
}

/// The newest curve on any export of `cut`, with its drops labelled by today's parts.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let exports: Vec<ExportEntry> = core
        .exports(project)?
        .into_iter()
        .filter(|e| e.cut == cut)
        .collect();
    let keys = p.handle.analysis_keys();
    let mut best: Option<(Curve, ExportEntry)> = None;
    for e in &exports {
        let owner = format!("export:{}", e.id);
        for (o, k, rev) in keys
            .iter()
            .filter(|(o, k, _)| *o == owner && k == "retention")
        {
            let Some(b) = p.handle.analysis(o, k, rev) else {
                continue;
            };
            let Ok(c) = serde_json::from_slice::<Curve>(&b) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(x, _)| c.imported_ms > x.imported_ms)
            {
                best = Some((c, e.clone()));
            }
        }
    }
    let Some((c, e)) = best else {
        return Ok(json!({"curve": null}));
    };
    let fps = e.rate();
    // today's parts, on the out clock
    let pl = core.planned(project, cut).ok();
    let doc = p.handle.snapshot().doc.clone();
    let parts: Vec<(f64, f64, String)> = pl
        .as_ref()
        .map(|pl| {
            pl.plan
                .parts
                .iter()
                .zip(pl.index.parts.iter())
                .map(|(pp, ix)| {
                    let role = ulid_id::<Part>(&ix.model)
                        .ok()
                        .and_then(|id| doc.get(id))
                        .map(|m| format!("{:?}", m.role).to_lowercase())
                        .unwrap_or_default();
                    (pp.out.start() as f64 / fps, pp.out.end() as f64 / fps, role)
                })
                .collect()
        })
        .unwrap_or_default();
    let stale = pl
        .as_ref()
        .is_some_and(|pl| crate::core::sig_of(&pl.plan).is_some_and(|s| Some(s) != e.render_sig));
    let drops: Vec<Drop> = drops(&c.points, c.duration_s)
        .into_iter()
        .map(|(t, to, a, b)| {
            let mid = (t + to) / 2.0;
            let k = parts.iter().position(|(s, e, _)| mid >= *s && mid < *e);
            let label = match k {
                Some(i) if !parts[i].2.is_empty() && parts[i].2 != "other" => {
                    format!("−{:.0} pts · part {} ({})", a - b, i + 1, parts[i].2)
                }
                Some(i) => format!("−{:.0} pts · part {}", a - b, i + 1),
                None => format!("−{:.0} pts", a - b),
            };
            Drop {
                t_s: t,
                to_s: to,
                from_pct: a,
                to_pct: b,
                drop_pct: a - b,
                part: k.map(|i| i + 1),
                label,
            }
        })
        .collect();
    Ok(json!({
        "curve": c,
        "export": e.id,
        "fps": e.rate(),
        "drops": drops,
        "stale": stale,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDIO: &str = "Video position (%),Absolute audience retention (%)\n0,140.55\n10,127.21\n20,97.4\n21,90.0\n50,80\n100,60\n";

    #[test]
    fn a_studio_export_maps_onto_the_out_clock() {
        let (p, platform) = parse(STUDIO, 30.0).unwrap();
        assert_eq!(platform, "youtube");
        assert_eq!(p[0], [0.0, 140.55]);
        assert!((p[1][0] - 3.0).abs() < 1e-9 && (p[5][0] - 30.0).abs() < 1e-9);
        let d = drops(&p, 30.0);
        assert!(!d.is_empty());
        // the steepest fall is between 10 % and 21 % of the video (3.0 s … 6.3 s)
        assert!(d[0].0 >= 2.0 && d[0].1 <= 7.5, "{d:?}");
    }

    #[test]
    fn other_shapes_parse_and_junk_is_refused() {
        let api = r#"[{"elapsedVideoTimeRatio":0,"audienceWatchRatio":1.2},{"elapsedVideoTimeRatio":0.5,"audienceWatchRatio":0.8},{"elapsedVideoTimeRatio":1,"audienceWatchRatio":0.5}]"#;
        let (p, pl) = parse(api, 20.0).unwrap();
        assert_eq!((pl.as_str(), p[1]), ("youtube", [10.0, 80.0]));
        let tt = r#"{"retention_points":[{"requested_seconds":0,"retention_percent":100},{"requested_seconds":5,"retention_percent":61}]}"#;
        assert_eq!(parse(tt, 30.0).unwrap().0[1], [5.0, 61.0]);
        let csv = "Time,Retention\n0:00,100%\n0:05,70%\n0:10,55%\n";
        assert_eq!(parse(csv, 12.0).unwrap().0[2], [10.0, 55.0]);
        let pts = r#"{"points":[[0,100],[3,90]],"platform":"instagram"}"#;
        assert_eq!(parse(pts, 5.0).unwrap().1, "instagram");
        assert!(parse("hello", 10.0).is_err());
        assert!(
            parse(
                "Video position (%),Absolute audience retention (%)\n0,100\n",
                10.0
            )
            .is_err()
        );
        assert!(parse(r#"{"nope":1}"#, 10.0).is_err());
        assert!(parse(&"x".repeat(MAX_BYTES + 1), 10.0).is_err());
    }

    #[test]
    fn clippy_data_is_read_and_never_written() {
        let root = std::env::temp_dir().join(format!("gmclip-ret-{}", std::process::id()));
        let ca = root.join("analytics");
        std::fs::create_dir_all(root.join("clippy/data")).unwrap();
        std::fs::create_dir_all(ca.join("audit/per-video")).unwrap();
        std::fs::create_dir_all(ca.join("native/yt")).unwrap();
        std::fs::write(ca.join("native/yt/All.csv"), STUDIO).unwrap();
        let audit = ca.join("audit/per-video/c1.json");
        std::fs::write(
            &audit,
            json!({"record": {"duration_seconds": 60.0, "curve_path": "native/yt/All.csv"}})
                .to_string(),
        )
        .unwrap();
        let side = json!({"hooks": {"h1": {"observations": [{"platform": "youtube", "measurement_window": "lifetime", "source_audit": audit, "events": []}]}}});
        std::fs::write(root.join("clippy/data/retention.json"), side.to_string()).unwrap();
        let before = std::fs::read(root.join("clippy/data/retention.json")).unwrap();
        // the posted video is 60 s, this export 30 s: the curve is scaled onto it
        let c = clippy_curve(&root.join("clippy"), "h1", 30.0).unwrap();
        assert_eq!(
            before,
            std::fs::read(root.join("clippy/data/retention.json")).unwrap()
        );
        assert_eq!(c.points.len(), 6);
        assert!((c.points[5][0] - 30.0).abs() < 1e-9);
        assert_eq!(c.source, "clippy:h1");
        assert_eq!(c.note.as_deref(), Some("lifetime"));
        // no curve file: the audit's event points
        std::fs::remove_file(ca.join("native/yt/All.csv")).unwrap();
        let side2 = json!({"hooks": {"h1": {"observations": [{"platform": "youtube", "source_audit": audit, "events": [{"start_seconds": 0.0, "end_seconds": 3.0, "curve_start": 140.0, "curve_end": 120.0}, {"start_seconds": 3.0, "end_seconds": 7.0, "curve_start": 120.0, "curve_end": 90.0}]}]}}});
        std::fs::write(root.join("clippy/data/retention.json"), side2.to_string()).unwrap();
        let c = clippy_curve(&root.join("clippy"), "h1", 30.0).unwrap();
        assert_eq!(c.points, vec![[0.0, 140.0], [3.0, 120.0], [7.0, 90.0]]);
        assert!(clippy_curve(&root.join("clippy"), "nope", 30.0).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Lab: AJ's own Clippy library, read only — every hook with a measurement gives a
    /// curve (the Studio export the audit measured, or its event points).
    #[test]
    #[ignore = "lab: reads Clippy's library in the lab media folder (read only)"]
    fn ajs_clippy_curves() {
        let library = gmclip_model::lab::media("clippy");
        if !library.join("data/retention.json").is_file() {
            return;
        }
        let side: Value =
            serde_json::from_slice(&std::fs::read(library.join("data/retention.json")).unwrap())
                .unwrap();
        for hook in side["hooks"].as_object().unwrap().keys() {
            let c = clippy_curve(&library, hook, 30.0).unwrap();
            let d = drops(&c.points, 30.0);
            eprintln!(
                "{hook}: {} points ({}), first drop {:?}",
                c.points.len(),
                c.platform,
                d.first()
            );
            assert!(c.points.len() >= 2);
        }
    }
}
