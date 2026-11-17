//! Golden cuts (decision 0015; backlog V.05): finished cuts the creator marks as the
//! references their next ones should look and move like. Profile content (SPEC §10.5
//! "golden exports"): kept per active profile in app data (`golden-cuts.json`; the profile
//! folder is never written), each with a spec gmclip measured from the cut's plan when it
//! was marked: length, takes and pacing, layouts, caption look and grouping, text hook,
//! pills, run clock, loudness.
//!
//! - Every agent brief gets the goldens' specs appended (`with_golden`), the titles as
//!   untrusted text.
//! - "Compare to golden" measures the open cut the same way and shows it against each
//!   golden and the goldens' band (min–max), with what is outside it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gmclip_plan::types::{Loudness, Plan, TextRole};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{AppCore, now_ms};
use crate::error::{AppError, AppResult};

/// What gmclip measures of a cut (all from its RenderPlan).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub duration_s: f64,
    pub canvas: [u64; 2],
    pub parts: usize,
    pub median_take_s: f64,
    pub shortest_take_s: f64,
    pub longest_take_s: f64,
    pub cuts_per_min: f64,
    /// Distinct layouts (the roles of the layers a take shows, in z order).
    pub layouts: usize,
    /// The last take's layout is the first's (reference rule 10, "loop").
    pub loops: bool,
    pub captions: usize,
    pub caption_words_median: f64,
    pub caption_words_max: usize,
    /// Caption height on the frame (median of the cues' anchors, fraction of the height).
    pub caption_y: Option<f64>,
    /// `<font id> <size>` of the caption style.
    pub caption_font: Option<String>,
    pub text_hook_s: Option<f64>,
    pub pills: usize,
    pub run_clock: bool,
    /// `as recorded` or `-16 LUFS`.
    pub loudness: String,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Measures a plan.
pub fn spec_of(plan: &Plan) -> Spec {
    let fps = plan.output.fps.as_f64();
    let s = |f: u64| f as f64 / fps;
    let duration_s = s(plan.output.frames);
    let mut takes: Vec<f64> = plan
        .parts
        .iter()
        .map(|p| s(p.out.end() - p.out.start()))
        .collect();
    let layout = |p: &gmclip_plan::types::Part| -> String {
        let mut l: Vec<(i64, String)> = p
            .layers
            .iter()
            .filter(|l| l.is_visible())
            .map(|l| (l.z, l.role.as_str().to_owned()))
            .collect();
        l.sort();
        l.into_iter().map(|x| x.1).collect::<Vec<_>>().join("+")
    };
    let layouts: Vec<String> = plan.parts.iter().map(layout).collect();
    let mut distinct = layouts.clone();
    distinct.sort();
    distinct.dedup();
    let ev = plan.text_events.as_deref().unwrap_or_default();
    let caps: Vec<&gmclip_plan::types::TextEvent> =
        ev.iter().filter(|e| e.role == TextRole::Caption).collect();
    let mut words: Vec<f64> = caps
        .iter()
        .map(|e| {
            e.runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<String>()
                .split_whitespace()
                .count() as f64
        })
        .collect();
    let words_max = words.iter().copied().fold(0.0, f64::max) as usize;
    let h = plan.output.canvas.h.get() as f64;
    let mut ys: Vec<f64> = caps.iter().map(|e| e.pos.y / h).collect();
    let caption_font = caps.first().map(|e| {
        let font = plan
            .text_styles
            .as_ref()
            .and_then(|m| m.get(&e.style))
            .map_or_else(
                || e.style.as_str().to_owned(),
                |st| st.font.as_str().to_owned(),
            );
        format!("{font} {:.0}", e.size)
    });
    let hook: f64 = ev
        .iter()
        .filter(|e| e.role == TextRole::TextHook)
        .map(|e| s(e.out.end() - e.out.start()))
        .sum();
    let n = plan.parts.len();
    Spec {
        duration_s,
        canvas: [plan.output.canvas.w.get(), plan.output.canvas.h.get()],
        parts: n,
        median_take_s: median(&mut takes),
        shortest_take_s: takes
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min)
            .min(duration_s),
        longest_take_s: takes.iter().copied().fold(0.0, f64::max),
        cuts_per_min: if duration_s > 0.0 {
            n.saturating_sub(1) as f64 * 60.0 / duration_s
        } else {
            0.0
        },
        layouts: distinct.len(),
        loops: n > 1 && layouts.first() == layouts.last(),
        captions: caps.len(),
        caption_words_median: median(&mut words),
        caption_words_max: words_max,
        caption_y: (!ys.is_empty()).then(|| median(&mut ys)),
        caption_font,
        text_hook_s: (hook > 0.0).then_some(hook),
        pills: ev
            .iter()
            .filter(|e| e.role == TextRole::SectionPill)
            .count(),
        run_clock: ev.iter().any(|e| e.role == TextRole::RunClock),
        loudness: match &plan.audio.loudness {
            Loudness::AsRecorded => "as recorded".into(),
            Loudness::Static {
                integrated_lufs, ..
            } => format!("{integrated_lufs} LUFS"),
        },
    }
}

/// One golden cut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Golden {
    pub project: String,
    pub project_name: String,
    pub cut: String,
    pub title: String,
    pub marked_ms: i64,
    /// The export it was marked on (its id and file).
    pub export: Option<String>,
    pub file: Option<PathBuf>,
    pub spec: Spec,
}

type Store = BTreeMap<String, Vec<Golden>>;

fn path(core: &AppCore) -> PathBuf {
    core.dirs.app_data.join("golden-cuts.json")
}

fn profile_key(core: &AppCore) -> String {
    gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data)
        .profile
        .map_or_else(
            || "(no profile)".to_owned(),
            |p| p.to_string_lossy().into_owned(),
        )
}

fn load(core: &AppCore) -> Store {
    std::fs::read(path(core))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(core: &AppCore, s: &Store) -> AppResult<()> {
    let b = serde_json::to_vec_pretty(s).map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&path(core), &b)
        .map_err(|e| AppError::internal(format!("saving the golden cuts ({})", e.kind())))
}

/// The active profile's golden cuts.
pub fn list(core: &AppCore) -> Vec<Golden> {
    load(core).remove(&profile_key(core)).unwrap_or_default()
}

/// Marks (`on`) or unmarks a cut as golden. Marking needs a finished cut: one with an
/// export; the spec is measured from the cut's plan now.
pub fn mark(core: &AppCore, project: &str, cut: &str, on: bool) -> AppResult<Value> {
    let mut store = load(core);
    let key = profile_key(core);
    let list = store.entry(key).or_default();
    list.retain(|g| !(g.project == project && g.cut == cut));
    if on {
        let p = core.project(project)?;
        let last = core
            .exports(project)?
            .into_iter()
            .filter(|e| e.cut == cut)
            .max_by_key(|e| e.at_ms)
            .ok_or_else(|| {
                AppError::invalid("export the cut first: a golden cut is a finished one")
            })?;
        let pl = core.planned(project, cut)?;
        let title = {
            let cid = crate::planner::ulid_id::<gmclip_model::entities::Cut>(cut)?;
            p.handle
                .snapshot()
                .doc
                .get(cid)
                .map(|c| c.title.clone())
                .unwrap_or_default()
        };
        if list.len() >= 50 {
            return Err(AppError::invalid("at most 50 golden cuts per profile"));
        }
        list.push(Golden {
            project: project.to_owned(),
            project_name: p.info.name.clone(),
            cut: cut.to_owned(),
            title,
            marked_ms: now_ms(),
            export: Some(last.id.clone()),
            file: Some(last.file.clone()),
            spec: spec_of(&pl.plan),
        });
    }
    save(core, &store)?;
    Ok(json!({"golden": on, "count": list_len(core)}))
}

fn list_len(core: &AppCore) -> usize {
    list(core).len()
}

/// Whether this cut is golden.
pub fn is_golden(core: &AppCore, project: &str, cut: &str) -> bool {
    list(core)
        .iter()
        .any(|g| g.project == project && g.cut == cut)
}

/// One line of numbers per golden (for briefs and the panel).
pub fn spec_line(s: &Spec) -> String {
    let mut v = vec![
        format!("{:.1} s", s.duration_s),
        format!("{}×{}", s.canvas[0], s.canvas[1]),
        format!(
            "{} takes (median {:.1} s, {:.1}–{:.1} s)",
            s.parts, s.median_take_s, s.shortest_take_s, s.longest_take_s
        ),
        format!("{:.1} cuts/min", s.cuts_per_min),
        format!(
            "{} layout{}",
            s.layouts,
            if s.layouts == 1 { "" } else { "s" }
        ),
    ];
    if s.loops {
        v.push("ends on the opening layout".into());
    }
    if s.captions > 0 {
        v.push(format!(
            "captions {} cues, {:.0}–{} words{}{}",
            s.captions,
            s.caption_words_median,
            s.caption_words_max,
            s.caption_y
                .map(|y| format!(" at {:.0}% height", y * 100.0))
                .unwrap_or_default(),
            s.caption_font
                .as_ref()
                .map(|f| format!(" ({f})"))
                .unwrap_or_default()
        ));
    } else {
        v.push("no captions".into());
    }
    if let Some(h) = s.text_hook_s {
        v.push(format!("text hook {h:.1} s"));
    }
    if s.pills > 0 {
        v.push(format!(
            "{} pill{}",
            s.pills,
            if s.pills == 1 { "" } else { "s" }
        ));
    }
    if s.run_clock {
        v.push("run clock".into());
    }
    v.push(format!("loudness {}", s.loudness));
    v.join(" · ")
}

/// The goldens as a brief section (none → empty).
pub fn brief_block(core: &AppCore) -> String {
    let g = list(core);
    if g.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Golden cuts (the editor's approved references: match their pacing and look)\n",
    );
    for x in g.iter().rev().take(10) {
        out.push_str(&format!(
            "- {} · {}\n",
            gmclip_mcp::canon::block(&x.title, gmclip_mcp::canon::Kind::Text),
            spec_line(&x.spec)
        ));
    }
    out
}

/// A brief with the golden section appended (unchanged when there are none).
pub fn with_golden(core: &AppCore, brief: String) -> String {
    let b = brief_block(core);
    if b.is_empty() || brief.contains("## Golden cuts") {
        brief
    } else {
        brief + &b
    }
}

fn range(vals: &[f64]) -> Option<(f64, f64)> {
    let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    lo.is_finite().then_some((lo, hi))
}

/// "Compare to golden": this cut's spec beside each golden's, with the goldens' band per
/// number and what is outside it.
pub fn compare(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let pl = core.planned(project, cut)?;
    let me = spec_of(&pl.plan);
    let goldens: Vec<Golden> = list(core)
        .into_iter()
        .filter(|g| !(g.project == project && g.cut == cut))
        .collect();
    type Row = (&'static str, &'static str, fn(&Spec) -> Option<f64>);
    let rows: [Row; 7] = [
        ("duration_s", "Length (s)", |s| Some(s.duration_s)),
        ("median_take_s", "Median take (s)", |s| {
            Some(s.median_take_s)
        }),
        ("cuts_per_min", "Cuts per minute", |s| Some(s.cuts_per_min)),
        ("layouts", "Layouts", |s| Some(s.layouts as f64)),
        ("caption_words_max", "Caption words (max)", |s| {
            (s.captions > 0).then_some(s.caption_words_max as f64)
        }),
        ("caption_y", "Caption height (%)", |s| {
            s.caption_y.map(|y| y * 100.0)
        }),
        ("text_hook_s", "Text hook (s)", |s| s.text_hook_s),
    ];
    let mut out_rows = Vec::new();
    let mut outside = Vec::new();
    for (key, label, f) in rows {
        let vals: Vec<f64> = goldens.iter().filter_map(|g| f(&g.spec)).collect();
        let band = range(&vals);
        let mine = f(&me);
        // a band of one golden gets a tolerance so a single reference is usable
        let tol = |lo: f64, hi: f64| -> (f64, f64) {
            if vals.len() == 1 {
                let d = (lo.abs() * 0.15).max(0.5);
                (lo - d, hi + d)
            } else {
                (lo, hi)
            }
        };
        let status = match (mine, band) {
            (Some(m), Some((lo, hi))) => {
                let (lo, hi) = tol(lo, hi);
                if m < lo {
                    "below"
                } else if m > hi {
                    "above"
                } else {
                    "in"
                }
            }
            (None, Some(_)) => "missing",
            _ => "n/a",
        };
        if status == "below" || status == "above" || status == "missing" {
            outside.push(label);
        }
        out_rows.push(json!({"key": key, "label": label, "mine": mine,
                             "band": band.map(|(a, b)| [a, b]), "status": status,
                             "goldens": goldens.iter().map(|g| f(&g.spec)).collect::<Vec<_>>()}));
    }
    // the categorical ones
    let loops_all = !goldens.is_empty() && goldens.iter().all(|g| g.spec.loops);
    if loops_all && !me.loops {
        outside.push("Ends on the opening layout");
    }
    let canvas_same = goldens.iter().all(|g| g.spec.canvas == me.canvas);
    Ok(json!({
        "mine": me, "mine_line": spec_line(&me),
        "goldens": goldens.iter().map(|g| json!({"project": g.project, "project_name": g.project_name, "cut": g.cut,
            "title": g.title, "marked_ms": g.marked_ms, "spec": g.spec, "line": spec_line(&g.spec)})).collect::<Vec<_>>(),
        "rows": out_rows,
        "outside": outside,
        "loops_expected": loops_all,
        "canvas_matches": canvas_same,
    }))
}

/// The golden panel: is this cut golden, the active profile's goldens.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let g = list(core);
    Ok(json!({
        "golden": g.iter().any(|x| x.project == project && x.cut == cut),
        "goldens": g.iter().map(|x| json!({"project": x.project, "project_name": x.project_name, "cut": x.cut,
            "title": x.title, "marked_ms": x.marked_ms, "line": spec_line(&x.spec)})).collect::<Vec<_>>(),
    }))
}
