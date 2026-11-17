//! The h79 grader: checks G0–G14 exactly as SPEC §17.1.1 defines them, computed from a
//! bundle ([`super::bundle`]), never judged. Spec values changed by calibration on the
//! golden are recorded in `evals/h79/calibration.md` and marked `calibrated:` below.
//!
//! A check's status is `pass`, `fail`, `pending` (a MUST input the pipeline cannot produce
//! yet, e.g. G8(b) needs the runner's hook to the B-08 cue builder) or `n/a` (G0/G7 on a
//! reference bundle, which has no run). A run **passes** when every MUST check passes; a
//! run with no failure but a pending MUST check is `provisional` and cannot count toward a
//! gate (the batch scorer refuses it).

use std::collections::BTreeMap;

use gmclip_ai::launcher::{Harness, transcript};
use serde::Serialize;
use serde_json::Value;

use super::bundle::{Bundle, Mode, Truth, Word};
use super::ink::{self, FontMetrics};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Pending,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    pub id: &'static str,
    pub must: bool,
    pub status: Status,
    /// The headline number (spec's "v4" column), when the check has one.
    pub value: Value,
    pub detail: Vec<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Provisional,
    Fail,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub mode: Mode,
    pub verdict: Verdict,
    pub failed: Vec<&'static str>,
    pub pending: Vec<&'static str>,
    pub checks: Vec<CheckResult>,
}

impl Report {
    pub fn get(&self, id: &str) -> &CheckResult {
        self.checks
            .iter()
            .find(|c| c.id == id)
            .expect("every check is reported")
    }
}

/// Reference data loaded once.
#[derive(Clone, Debug)]
pub struct Reference {
    pub truth: Truth,
    pub golden_words: Vec<Word>,
    pub font: FontMetrics,
}

impl Reference {
    pub fn load(dir: &std::path::Path) -> Result<Self, String> {
        let truth = super::bundle::load_truth(dir)?;
        let golden_words = super::bundle::load_words(&dir.join(&truth.golden_words_file))?;
        let font: FontMetrics = serde_json::from_slice(
            &std::fs::read(dir.join(&truth.font_metrics_file)).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            truth,
            golden_words,
            font,
        })
    }
}

// ---- plan access (plain JSON) ----------------------------------------------------

#[derive(Clone, Debug)]
struct Layer {
    role: String,
    visible: bool,
    crop: [f64; 4],
    place: [f64; 4],
}

#[derive(Clone, Debug)]
struct Part {
    id: String,
    out: [u64; 2],
    role: Option<String>,
    /// Master-clock seconds `[start, end)`: from the audio when its source is the truth's
    /// master audio (the proxy runs on the stream clock), else from `native`.
    master: Option<[f64; 2]>,
    /// The two disagree by more than 0.1 s (a mislabelled `native`).
    master_conflict: bool,
    audio_source: String,
    first_sample: u64,
    layers: Vec<Layer>,
}

impl Part {
    fn frames(&self) -> u64 {
        self.out[1].saturating_sub(self.out[0])
    }
}

#[derive(Clone, Debug)]
struct TextEvent {
    id: String,
    role: String,
    out: [u64; 2],
    text: String,
    size: f64,
    align: u8,
    pos: [f64; 2],
    letter_spacing: f64,
    provenance: Option<String>,
    /// sha256 of the font the event's style names (`None`: unresolvable).
    font_sha256: Option<String>,
    /// Lowest fill alpha over the event's runs (0–255).
    min_alpha: u8,
}

fn rect(v: &Value) -> [f64; 4] {
    let g = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    [g("x"), g("y"), g("w"), g("h")]
}

fn range(v: &Value) -> Option<[u64; 2]> {
    Some([v.get(0)?.as_u64()?, v.get(1)?.as_u64()?])
}

struct Plan {
    fps: f64,
    fps_num: u64,
    fps_den: u64,
    frames: u64,
    canvas: [u64; 2],
    parts: Vec<Part>,
    events: Vec<TextEvent>,
    /// source id → sha256
    sources: BTreeMap<String, String>,
}

fn alpha_of(rgba: Option<&str>) -> u8 {
    rgba.and_then(|s| s.strip_prefix('#'))
        .filter(|s| s.len() == 8)
        .and_then(|s| u8::from_str_radix(&s[6..8], 16).ok())
        .unwrap_or(255)
}

fn read_plan(p: &Value, master_audio_sha: &str) -> Result<Plan, String> {
    let fps_num = p
        .pointer("/output/fps/num")
        .and_then(Value::as_u64)
        .ok_or("plan: output.fps")?;
    let fps_den = p
        .pointer("/output/fps/den")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    let fps = fps_num as f64 / fps_den as f64;
    if fps <= 0.0 {
        return Err("plan: fps must be positive".into());
    }
    let frames = p
        .pointer("/output/frames")
        .and_then(Value::as_u64)
        .ok_or("plan: output.frames")?;
    let canvas = [
        p.pointer("/output/canvas/w")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        p.pointer("/output/canvas/h")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    ];
    let sources: BTreeMap<String, String> = p["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some((
                s["id"].as_str()?.to_owned(),
                s.pointer("/fingerprint/sha256")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut parts = Vec::new();
    for v in p
        .get("parts")
        .and_then(Value::as_array)
        .ok_or("plan: parts")?
    {
        let out = range(&v["out"]).ok_or("plan: part.out")?;
        let native = range(&v["native"]["frames"]).map(|r| {
            let nf = v
                .pointer("/native/fps/num")
                .and_then(Value::as_f64)
                .unwrap_or(fps)
                / v.pointer("/native/fps/den")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .max(1.0);
            [r[0] as f64 / nf, r[1] as f64 / nf]
        });
        let audio_source = v
            .pointer("/audio/source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let first_sample = v
            .pointer("/audio/first_sample")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let rate = v
            .pointer("/audio/rate")
            .and_then(Value::as_f64)
            .unwrap_or(48_000.0)
            .max(1.0);
        let from_audio = (sources.get(&audio_source).map(String::as_str) == Some(master_audio_sha))
            .then(|| {
                let s0 = first_sample as f64 / rate;
                [s0, s0 + (out[1].saturating_sub(out[0])) as f64 / fps]
            });
        let master_conflict =
            matches!((native, from_audio), (Some(n), Some(a)) if (n[0] - a[0]).abs() > 0.1);
        parts.push(Part {
            id: v["id"].as_str().unwrap_or("").to_owned(),
            out,
            role: v.get("role").and_then(Value::as_str).map(str::to_owned),
            master: from_audio.or(native),
            master_conflict,
            audio_source,
            first_sample,
            layers: v["layers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|l| Layer {
                    role: l["role"].as_str().unwrap_or("").to_owned(),
                    visible: l.get("visible").and_then(Value::as_bool).unwrap_or(true),
                    crop: rect(&l["crop"]),
                    place: rect(&l["place"]),
                })
                .collect(),
        });
    }
    let styles = p.get("text_styles").cloned().unwrap_or(Value::Null);
    let fonts: BTreeMap<String, String> = p["fonts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some((
                f["id"].as_str()?.to_owned(),
                f["sha256"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut events = Vec::new();
    for e in p
        .get("text_events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let style = &styles[e["style"].as_str().unwrap_or("")];
        let style_fill = style["fill"].as_str();
        let runs: Vec<&Value> = e["runs"].as_array().into_iter().flatten().collect();
        events.push(TextEvent {
            id: e["id"].as_str().unwrap_or("").to_owned(),
            role: e["role"].as_str().unwrap_or("").to_owned(),
            out: range(&e["out"]).unwrap_or([0, 0]),
            text: runs.iter().filter_map(|r| r["text"].as_str()).collect(),
            size: e["size"].as_f64().unwrap_or(0.0),
            align: e
                .get("align")
                .and_then(Value::as_u64)
                .or_else(|| style.get("align").and_then(Value::as_u64))
                .unwrap_or(5)
                .clamp(1, 9) as u8,
            pos: [
                e["pos"]["x"].as_f64().unwrap_or(0.0),
                e["pos"]["y"].as_f64().unwrap_or(0.0),
            ],
            letter_spacing: style
                .get("letter_spacing")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            provenance: e
                .pointer("/provenance/kind")
                .and_then(Value::as_str)
                .map(str::to_owned),
            font_sha256: style["font"].as_str().and_then(|f| fonts.get(f).cloned()),
            min_alpha: runs
                .iter()
                .map(|r| alpha_of(r["fill"].as_str().or(style_fill)))
                .min()
                .unwrap_or_else(|| alpha_of(style_fill)),
        });
    }
    Ok(Plan {
        fps,
        fps_num,
        fps_den,
        frames,
        canvas,
        parts,
        events,
        sources,
    })
}

/// The joins `verify{joins}` checks: the discontinuous ones (RenderPlan rule 7: a join is
/// continuous when both parts read the same audio source and the second starts where the
/// first ends).
pub fn discontinuous_joins(plan: &Value) -> usize {
    let Ok(p) = read_plan(plan, "") else { return 0 };
    let s = |f: u64| -> u64 { f * 48_000 * p.fps_den / p.fps_num.max(1) };
    p.parts
        .windows(2)
        .filter(|w| {
            let (a, b) = (&w[0], &w[1]);
            !(a.audio_source == b.audio_source
                && b.first_sample == a.first_sample + s(a.out[1]) - s(a.out[0]))
        })
        .count()
}

// ---- helpers --------------------------------------------------------------------

fn median(mut v: Vec<f64>) -> f64 {
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

fn overlaps(a: [f64; 2], b: [f64; 2]) -> bool {
    a[0] < b[1] && b[0] < a[1]
}

fn iou(a: [f64; 4], b: [f64; 4]) -> f64 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Normalised word: lowercase alphanumerics (and `'`).
pub fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// story_v5's dropped fillers (SPEC §5.10).
const FILLERS: &[&str] = &["uh", "um", "uhh", "umm", "er", "hmm"];

const STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "of", "to", "in", "on", "at", "for", "with", "is", "it",
    "its", "it's", "this", "that", "i", "i'm", "we", "you", "my", "so", "uh", "um", "be", "was",
    "are", "just", "like", "let's", "what's", "okay",
];

fn content_words(ws: &[String]) -> Vec<String> {
    ws.iter()
        .map(|w| norm(w))
        .filter(|w| !w.is_empty() && !STOP.contains(&w.as_str()))
        .collect()
}

fn lcs(a: &[String], b: &[String]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        prev = cur;
    }
    prev[b.len()]
}

#[derive(Clone, Debug)]
enum NumberForm {
    Integer(u64),
    /// Seconds.
    MinSec(u64),
    Decimal(f64, i32),
    Other,
}

/// Numbers shown in a text: integers, decimals and `m:ss`.
fn shown_numbers(text: &str) -> Vec<(String, NumberForm)> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == ':' || chars[i] == '.')
            {
                i += 1;
            }
            let tok: String = chars[start..i]
                .iter()
                .collect::<String>()
                .trim_end_matches(['.', ':'])
                .to_owned();
            let form = if let Some((m, s)) = tok.split_once(':') {
                match (m.parse::<u64>(), s.parse::<u64>()) {
                    (Ok(m), Ok(s)) if s < 60 && s_len_ok(s_str(&tok)) => m
                        .checked_mul(60)
                        .and_then(|x| x.checked_add(s))
                        .map_or(NumberForm::Other, NumberForm::MinSec),
                    _ => NumberForm::Other,
                }
            } else if tok.contains('.') {
                tok.parse::<f64>()
                    .map(|v| {
                        NumberForm::Decimal(v, tok.split('.').nth(1).map_or(0, str::len) as i32)
                    })
                    .unwrap_or(NumberForm::Other)
            } else {
                tok.parse::<u64>()
                    .map(NumberForm::Integer)
                    .unwrap_or(NumberForm::Other)
            };
            out.push((tok, form));
        } else {
            i += 1;
        }
    }
    out
}

fn s_str(tok: &str) -> &str {
    tok.split_once(':').map_or("", |(_, s)| s)
}

fn s_len_ok(s: &str) -> bool {
    s.len() == 2
}

/// SPEC §5.13: the displayed value agrees with the provenance value within its display
/// rounding (`m:ss` floored; integers ±1; decimals ±½ of the last digit).
fn agrees(form: &NumberForm, value: f64) -> bool {
    match form {
        NumberForm::Integer(n) => (*n as f64 - value).abs() <= 1.0,
        NumberForm::MinSec(s) => value >= 0.0 && value.floor() as u64 == *s,
        NumberForm::Decimal(v, d) => (v - value).abs() <= 0.5 * 10f64.powi(-d),
        NumberForm::Other => false,
    }
}

/// Every string an argument carries: values (trimmed), object keys, and strings nested as
/// JSON inside strings.
fn strings_in(v: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    match v {
        Value::String(s) => {
            let t = s.trim();
            if (t.starts_with('{') || t.starts_with('['))
                && let Ok(inner) = serde_json::from_str::<Value>(t)
            {
                strings_in(&inner, out, depth + 1);
            }
            out.push(t.to_owned());
        }
        Value::Array(a) => a.iter().for_each(|x| strings_in(x, out, depth + 1)),
        Value::Object(m) => {
            for (k, x) in m {
                out.push(k.trim().to_owned());
                strings_in(x, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// A string that names a file: absolute, home-relative, `file:`, a drive, a traversal,
/// or any relative path with a `/` (not a ratio like `30/1`, not a `gmclip://` resource).
fn path_like(s: &str) -> bool {
    let ratio = s.split_once('/').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    });
    if s.starts_with("gmclip://") || ratio {
        return false;
    }
    s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with("file:")
        || s.contains("..")
        || s.contains('\\')
        || s.contains('/')
        || s.as_bytes().get(1) == Some(&b':')
            && s.as_bytes()
                .get(2)
                .is_some_and(|c| *c == b'\\' || *c == b'/')
}

/// `C:\…` / `C:/…`: a Windows drive path.
fn drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// `true` when absolute `p` is inside `root` after lexical normalisation; macOS
/// `/private/var` and `/var` name the same place. Windows drive paths compare with
/// either separator and without case (NTFS), and only with a drive root.
fn inside(p: &str, root: &str) -> bool {
    fn lex(s: &str) -> Vec<String> {
        let s = s
            .strip_prefix("/private")
            .filter(|r| r.starts_with("/var/") || r.starts_with("/tmp/"))
            .unwrap_or(s);
        let win = drive_path(s);
        let s = if win {
            s.replace('\\', "/")
        } else {
            s.to_owned()
        };
        let mut v: Vec<String> = Vec::new();
        for c in s.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    v.pop();
                }
                c if win => v.push(c.to_lowercase()),
                c => v.push(c.to_owned()),
            }
        }
        v
    }
    let unix = p.starts_with('/');
    let win = drive_path(p) && drive_path(root);
    if !(unix || win) || root.is_empty() {
        return false;
    }
    let (p, r) = (lex(p), lex(root));
    p.len() >= r.len() && p[..r.len()] == r[..]
}

/// Anyone but a human or an import is an agent (identities are `launched:<run>`,
/// `local:<client>`, `agent`, …).
fn agent_authored(author: &str) -> bool {
    !matches!(author, "human" | "import")
}

fn c(
    id: &'static str,
    must: bool,
    status: Status,
    value: Value,
    detail: Vec<String>,
) -> CheckResult {
    CheckResult {
        id,
        must,
        status,
        value,
        detail,
    }
}

fn pass_if(ok: bool) -> Status {
    if ok { Status::Pass } else { Status::Fail }
}

/// Calibrated guards (calibration.md): the most output that may sit in hidden-cam
/// full-subject parts, and in no-caption ranges.
const FULL_SUBJECT_SHARE_MAX: f64 = 0.35;
const NO_CAPTION_SHARE_MAX: f64 = 0.15;

// ---- the checks -------------------------------------------------------------------

/// Grades one bundle.
pub fn grade(b: &Bundle, r: &Reference) -> Result<Report, String> {
    let t = &r.truth;
    let th = &t.thresholds;
    let plan = read_plan(&b.plan, &t.sources.audio.sha256)?;
    let pj = &b.project;
    let fps = plan.fps;
    let facts_by_part: BTreeMap<&str, &super::bundle::PartFact> =
        pj.cut.parts.iter().map(|p| (p.id.as_str(), p)).collect();
    let overlay_facts: BTreeMap<&str, &super::bundle::OverlayFact> =
        pj.cut.overlays.iter().map(|o| (o.id.as_str(), o)).collect();
    // G1's export, the one every export check reads
    let candidates: Vec<&super::bundle::ExportFact> = pj
        .exports
        .iter()
        .filter(|e| e.framing == pj.cut.framing && !e.blocked)
        .filter(|e| {
            b.mode == Mode::Reference
                || (e.created_by_run.is_some() && e.created_by_run == pj.run_id)
        })
        .collect();
    let export = (candidates.len() == 1).then(|| candidates[0]);
    let mut checks = Vec::new();

    // G0 isolation ------------------------------------------------------------------
    checks.push(match (b.mode, &b.transcript) {
        (Mode::Reference, _) => c("G0", true, Status::NotApplicable, Value::Null, vec!["reference bundle: no run".into()]),
        (Mode::Run, None) => c("G0", true, Status::Fail, Value::Null, vec!["no client transcript".into()]),
        (Mode::Run, Some(text)) => {
            let mut d = Vec::new();
            let run = b.run.clone().unwrap_or(Value::Null);
            let harness = run["harness"].as_str().and_then(Harness::parse);
            if harness.is_none() {
                d.push(format!("run.json names no known harness ({:?})", run["harness"]));
            }
            let parsed = transcript::parse(harness.unwrap_or(Harness::ClaudeCode), text);
            let foreign: Vec<&str> = parsed.calls.iter().filter(|c| !c.is_gmclip()).map(|c| c.name.as_str()).collect();
            if !foreign.is_empty() {
                d.push(format!("non-gmclip tool calls: {foreign:?}"));
            }
            let root = pj.fixture_root.clone().unwrap_or_default();
            let mut outside = Vec::new();
            for call in parsed.calls.iter().filter(|c| c.is_gmclip()) {
                let mut ss = Vec::new();
                strings_in(&call.input, &mut ss, 0);
                for s in ss.into_iter().filter(|s| path_like(s)) {
                    if !inside(&s, &root) {
                        outside.push(format!("{}: {s}", call.name));
                    }
                }
            }
            if !outside.is_empty() {
                d.push(format!("gmclip calls naming paths outside the fixture: {outside:?}"));
            }
            let timed_out = run["timed_out"].as_bool();
            if timed_out != Some(false) {
                d.push(format!("wall clock: timed_out = {timed_out:?}"));
            }
            if parsed.junk_lines > 0 || !parsed.result_seen {
                d.push(format!("garbled or cut-off transcript: {} non-JSON lines, closing line seen {}", parsed.junk_lines, parsed.result_seen));
            }
            let sha = gmclip_ai::launcher::sha256::hex(text.as_bytes());
            let bound = run["transcript_sha256"].as_str() == Some(sha.as_str())
                && run["run"].as_str().is_some()
                && run["run"].as_str() == pj.run_id.as_deref()
                && run["cut"].as_str() == Some(pj.cut.id.as_str());
            if !bound {
                d.push("the transcript, run record and project are not one run (sha256, run id or cut differ)".into());
            }
            let launcher_says = run["non_gmclip_calls"].as_u64().unwrap_or(0) + run["out_of_policy_calls"].as_u64().unwrap_or(0);
            if launcher_says > 0 {
                d.push(format!("the launcher's own record counts {launcher_says} non-gmclip / out-of-policy calls"));
            }
            let ok = d.is_empty();
            c("G0", true, pass_if(ok), Value::from(parsed.calls.len()), d)
        }
    });

    // G1 export valid ------------------------------------------------------------------
    {
        let mut d = Vec::new();
        if candidates.len() != 1 {
            d.push(format!(
                "{} non-blocked {} export(s) by this run (want exactly 1)",
                candidates.len(),
                pj.cut.framing
            ));
        }
        let ok = export.is_some_and(|e| {
            let v_ok = e
                .verify_export
                .as_ref()
                .is_some_and(|v| v.ok && v.render_sig.as_deref() == Some(e.render_sig.as_str()));
            let fresh = pj.cut.current_render_sig.as_deref() == Some(e.render_sig.as_str());
            let same_plan = e.plan_sha256 == b.plan_sha256;
            let shape = plan.canvas == [1080, 1920] && plan.fps_num == 30 && plan.fps_den == 1;
            if !v_ok {
                d.push("verify{export} missing, red, or not bound to this render_sig".into());
            }
            if !fresh {
                d.push("stale: the export's render_sig is not the current cut's".into());
            }
            if !same_plan {
                d.push("the export record's plan_sha256 is not this bundle's plan.json".into());
            }
            if !shape {
                d.push(format!(
                    "the plan is {:?} @ {}/{}, not the 9:16 framing (1080×1920 @ 30)",
                    plan.canvas, plan.fps_num, plan.fps_den
                ));
            }
            v_ok && fresh && same_plan && shape
        });
        checks.push(c("G1", true, pass_if(ok), Value::from(candidates.len()), d));
    }

    // G2 duration -------------------------------------------------------------------
    let secs = plan.frames as f64 / fps;
    checks.push(c(
        "G2",
        true,
        pass_if(secs >= th.duration_s[0] && secs <= th.duration_s[1]),
        Value::from((secs * 100.0).round() / 100.0),
        vec![format!(
            "{} frames / {fps} = {secs:.3} s; want [{}, {}]",
            plan.frames, th.duration_s[0], th.duration_s[1]
        )],
    ));

    let role_of = |p: &Part| -> Option<String> {
        facts_by_part
            .get(p.id.as_str())
            .map(|f| f.role.clone())
            .or_else(|| p.role.clone())
    };

    // G3 payoff first -----------------------------------------------------------------
    {
        let mut d = Vec::new();
        let ok = match plan.parts.first() {
            None => false,
            Some(p1) => {
                let role = role_of(p1);
                let inside_payoff = p1
                    .master
                    .is_some_and(|m| m[0] >= t.payoff_region_s[0] && m[1] <= t.payoff_region_s[1]);
                d.push(format!(
                    "part 1 {} role {role:?}, master {:?}, payoff {:?}",
                    p1.id, p1.master, t.payoff_region_s
                ));
                if p1.master_conflict {
                    d.push(
                        "part 1's native frames disagree with its master audio by > 0.1 s".into(),
                    );
                }
                role.as_deref() == Some("hook") && inside_payoff && !p1.master_conflict
            }
        };
        checks.push(c("G3", true, pass_if(ok), Value::Bool(ok), d));
    }

    // G4 story arc ----------------------------------------------------------------------
    {
        let rest: Vec<&Part> = plan.parts.iter().skip(1).collect();
        let hit = |w: [f64; 2]| {
            plan.parts
                .iter()
                .any(|p| p.master.is_some_and(|m| overlaps(m, w)))
        };
        let (a1, a2, a3) = (
            hit(t.setup_region_s),
            hit(t.result_window_s),
            hit(t.reaction_region_s),
        );
        // longest non-decreasing subsequence by source start, weighted by output duration
        let mut best: Vec<f64> = Vec::new();
        for (i, p) in rest.iter().enumerate() {
            let w = p.frames() as f64;
            let mut b_i = w;
            for j in 0..i {
                if let (Some(mj), Some(mi)) = (rest[j].master, p.master)
                    && mj[0] <= mi[0]
                {
                    b_i = b_i.max(best[j] + w);
                }
            }
            best.push(b_i);
        }
        let total: f64 = rest.iter().map(|p| p.frames() as f64).sum();
        let share = if total > 0.0 {
            best.iter().cloned().fold(0.0, f64::max) / total
        } else {
            0.0
        };
        let ok = a1 && a2 && a3 && share >= th.arc_monotone_share;
        checks.push(c(
            "G4",
            true,
            pass_if(ok),
            Value::from((share * 1000.0).round() / 1000.0),
            vec![format!("(a) setup {a1}, result {a2}, reaction {a3}; (b) monotone share {share:.3} (want ≥ {})", th.arc_monotone_share)],
        ));
    }

    // G5 not jumpy ----------------------------------------------------------------------
    {
        let durs: Vec<f64> = plan.parts.iter().map(|p| p.frames() as f64 / fps).collect();
        let med = median(durs.clone());
        let min = durs.iter().cloned().fold(f64::INFINITY, f64::min);
        let ok = !durs.is_empty()
            && med >= th.median_part_s_min
            && min >= th.part_s_min
            && durs.len() <= th.parts_max;
        checks.push(c(
            "G5",
            true,
            pass_if(ok),
            serde_json::json!({"median_s": (med * 100.0).round() / 100.0, "min_s": (min * 100.0).round() / 100.0, "parts": durs.len()}),
            vec![format!(
                "median {med:.3} s (≥ {}), min {min:.3} s (≥ {}), {} parts (≤ {})",
                th.median_part_s_min,
                th.part_s_min,
                durs.len(),
                th.parts_max
            )],
        ));
    }

    // G6 joins: verify{joins} green on the export's render_sig ------------------------
    {
        let mut d = Vec::new();
        let joins = discontinuous_joins(&b.plan);
        let ok = match export.and_then(|e| e.verify_joins.as_ref().map(|v| (e, v))) {
            None => {
                d.push("no verify{joins} on this run's export record".into());
                false
            }
            Some((e, v)) => {
                let bound = v.render_sig.as_deref() == Some(e.render_sig.as_str());
                let all = v.joins_checked == Some(joins);
                if !v.ok {
                    d.push(format!("verify{{joins}} red: {:?}", v.failures));
                }
                if !bound {
                    d.push("verify{joins} ran on another render_sig (stale)".into());
                }
                if !all {
                    d.push(format!("verify{{joins}} checked {:?} joins; the plan has {joins} discontinuous joins", v.joins_checked));
                }
                v.ok && bound && all
            }
        };
        checks.push(c("G6", true, pass_if(ok), Value::Bool(ok), d));
    }

    // G7 anchored times -----------------------------------------------------------------
    {
        let mut bad = Vec::new();
        let no_facts = pj.cut.parts.iter().all(|p| p.edges.is_empty());
        if b.mode == Mode::Run {
            for p in &plan.parts {
                let sides: Vec<&str> = facts_by_part
                    .get(p.id.as_str())
                    .map(|f| f.edges.iter().map(|e| e.side.as_str()).collect())
                    .unwrap_or_default();
                if !(sides.contains(&"start") && sides.contains(&"end")) {
                    bad.push(format!("part {} has no anchor facts for both edges", p.id));
                }
            }
            for e in plan.events.iter().filter(|e| e.role != "caption") {
                if overlay_facts
                    .get(e.id.as_str())
                    .and_then(|o| o.time_provenance.as_ref())
                    .is_none()
                {
                    bad.push(format!("overlay {} has no time provenance", e.id));
                }
            }
        }
        for p in &pj.cut.parts {
            for e in &p.edges {
                if e.time_provenance == "raw" {
                    bad.push(format!("part {} {} edge is raw", p.id, e.side));
                }
                if e.edges.as_deref() == Some("line") && e.measure_id.is_none() {
                    bad.push(format!(
                        "part {} {} edge is line-edged without measure{{edge}}",
                        p.id, e.side
                    ));
                }
                if agent_authored(&e.author) && e.offset_ms.is_some_and(|o| o != 0.0) {
                    bad.push(format!(
                        "part {} {} edge has an agent-typed offset_ms",
                        p.id, e.side
                    ));
                }
            }
        }
        for o in &pj.cut.overlays {
            if o.time_provenance.as_deref() == Some("raw") {
                bad.push(format!("overlay {} is raw", o.id));
            }
            if agent_authored(&o.author) && o.offset_ms.is_some_and(|x| x != 0.0) {
                bad.push(format!("overlay {} has an agent-typed offset_ms", o.id));
            }
        }
        for g in &pj.cut.gain_segments {
            if g.time_provenance == "raw" {
                bad.push(format!("gain segment {} is raw", g.id));
            }
        }
        for n in pj.cut.captions.iter().flat_map(|c| &c.no_caption) {
            if n.time_provenance == "raw" {
                bad.push(format!("no-caption range {:?} is raw", n.out));
            }
        }
        let status = if b.mode == Mode::Reference && no_facts {
            Status::NotApplicable
        } else {
            pass_if(bad.is_empty())
        };
        checks.push(c("G7", true, status, Value::from(bad.len()), bad));
    }

    // G8 captions ------------------------------------------------------------------------
    {
        let mut d = Vec::new();
        let cd = pj.cut.captions.clone().unwrap_or_default();
        let a = cd.enabled && cd.source == "heard" && cd.grouping == "story_v5";
        if !a {
            d.push(format!(
                "(a) CaptionDoc enabled {} source {:?} grouping {:?}",
                cd.enabled, cd.source, cd.grouping
            ));
        }
        // calibrated guard: captions cannot be switched off by covering the cut with
        // no-caption ranges
        let no_cap: u64 = cd
            .no_caption
            .iter()
            .map(|n| n.out[1].saturating_sub(n.out[0]))
            .sum();
        let no_cap_share = if plan.frames == 0 {
            1.0
        } else {
            no_cap as f64 / plan.frames as f64
        };
        if no_cap_share > NO_CAPTION_SHARE_MAX {
            d.push(format!("no-caption ranges cover {no_cap_share:.3} of the output (max {NO_CAPTION_SHARE_MAX})"));
        }
        let b_status = match export.and_then(|e| e.cues_equal_reference) {
            Some(true) => Status::Pass,
            Some(false) => {
                d.push("(b) the record's cues differ from the reference story_v5 run".into());
                Status::Fail
            }
            None => {
                d.push("(b) pending: the reference story_v5 run over the record's heard words (runner hook to B-08)".into());
                Status::Pending
            }
        };
        let cues: Vec<&TextEvent> = plan.events.iter().filter(|e| e.role == "caption").collect();
        let mut c_bad = Vec::new();
        for cue in &cues {
            let words: Vec<&str> = cue.text.split_whitespace().collect();
            let frames = cue.out[1].saturating_sub(cue.out[0]);
            let dur = frames as f64 / fps;
            let chars = words.join(" ").chars().count();
            let n_ok = (1..=th.cue_words_max).contains(&words.len())
                || (words.len() == 4 && chars <= th.cue4_chars_max && dur <= th.cue4_s_max);
            if !n_ok || frames < th.cue_frames_min {
                c_bad.push(format!(
                    "{} {:?} ({} words, {frames} frames)",
                    cue.id,
                    cue.text,
                    words.len()
                ));
            }
        }
        if !c_bad.is_empty() {
            d.push(format!(
                "(c) cues outside 1–3 words / ≥ {} frames: {c_bad:?}",
                th.cue_frames_min
            ));
        }
        if cues.is_empty() {
            d.push("(c) no caption cues".into());
        }
        // (d) coverage of round-trip ASR words outside no-caption ranges
        let d_status = match &b.asr {
            None => {
                d.push("(d) pending: no round-trip ASR of the export (asr.json)".into());
                Status::Pending
            }
            Some(words) => {
                let blocked = |f: f64| {
                    cd.no_caption
                        .iter()
                        .any(|n| f >= n.out[0] as f64 && f < n.out[1] as f64)
                };
                let mut total = 0usize;
                let mut covered = 0usize;
                let mut missed = Vec::new();
                // calibrated: fillers are not counted (story_v5 drops them), hyphenated
                // words count per part, and a cue covers a word whose midpoint is within
                // 0.5 s of it (round-trip ASR times drift against cue timing)
                for w in words {
                    for part in w.word.split('-').filter(|p| !p.is_empty()) {
                        let mid = (w.start + w.end) / 2.0 * fps;
                        let nw = norm(part);
                        if blocked(mid) || nw.is_empty() || FILLERS.contains(&nw.as_str()) {
                            continue;
                        }
                        total += 1;
                        let hit = cues.iter().any(|c| {
                            mid >= c.out[0] as f64 - 0.5 * fps
                                && mid < c.out[1] as f64 + 0.5 * fps
                                && c.text
                                    .split(|ch: char| ch.is_whitespace() || ch == '-')
                                    .any(|x| norm(x) == nw)
                        });
                        if hit {
                            covered += 1;
                        } else {
                            missed.push(part.to_owned());
                        }
                    }
                }
                let share = if total == 0 {
                    0.0
                } else {
                    covered as f64 / total as f64
                };
                d.push(format!("(d) {covered}/{total} = {share:.3} round-trip words inside a cue containing them; missed {missed:?}"));
                pass_if(share >= th.caption_coverage_min)
            }
        };
        let c_ok = c_bad.is_empty() && !cues.is_empty();
        let status = if !a
            || !c_ok
            || no_cap_share > NO_CAPTION_SHARE_MAX
            || b_status == Status::Fail
            || d_status == Status::Fail
        {
            Status::Fail
        } else if b_status == Status::Pending || d_status == Status::Pending {
            Status::Pending
        } else {
            Status::Pass
        };
        checks.push(c("G8", true, status, Value::from(cues.len()), d));
    }

    // G9 text hook -------------------------------------------------------------------------
    let hooks: Vec<&TextEvent> = plan
        .events
        .iter()
        .filter(|e| e.role == "text_hook")
        .collect();
    let font_ok = |e: &TextEvent| e.font_sha256.as_deref() == Some(r.font.font.sha256.as_str());
    {
        let mut d = Vec::new();
        let canvas_w = plan.canvas[0] as f64;
        let ok = if hooks.len() != 1 {
            d.push(format!(
                "{} text_hook overlays (want exactly 1)",
                hooks.len()
            ));
            false
        } else {
            let h = hooks[0];
            let end_s = h.out[1] as f64 / fps;
            let words = h.text.split_whitespace().count();
            let lines: Vec<&str> = h.text.split('\n').collect();
            let long = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
            let mut ok = h.out[0] == 0 && end_s >= th.hook_end_s[0] && end_s <= th.hook_end_s[1];
            d.push(format!("starts at frame {}, ends {end_s:.2} s", h.out[0]));
            ok &= (th.hook_words[0]..=th.hook_words[1]).contains(&words);
            ok &= lines.len() <= th.hook_lines_max && long <= th.hook_chars_per_line_max;
            d.push(format!(
                "{words} words, {} lines, longest {long} chars",
                lines.len()
            ));
            if h.min_alpha < 0x80 {
                d.push(format!("hook fill alpha {} (not visible)", h.min_alpha));
                ok = false;
            }
            if !font_ok(h) {
                d.push("the hook's font is not the one the ink box is measured for".into());
                ok = false;
            }
            match ink::ink_bbox(
                &r.font,
                &h.text,
                h.size,
                h.align,
                h.pos[0],
                h.pos[1],
                h.letter_spacing,
            ) {
                None => {
                    d.push("hook has no ink".into());
                    ok = false;
                }
                Some(bb) => {
                    let band = bb[1] >= th.hook_band_y[0]
                        && bb[3] <= th.hook_band_y[1]
                        && bb[0] >= 0.0
                        && bb[2] <= canvas_w;
                    d.push(format!(
                        "hook ink [{:.0}, {:.0}, {:.0}, {:.0}] (band y {:?})",
                        bb[0], bb[1], bb[2], bb[3], th.hook_band_y
                    ));
                    ok &= band;
                }
            }
            ok
        };
        // IG zones over the hook and captions
        let zones = [
            [0.0, 0.0, canvas_w, th.ig_top_px],
            [
                canvas_w - th.ig_right.w,
                th.ig_right.y[0],
                canvas_w,
                th.ig_right.y[1],
            ],
        ];
        let mut in_zone = Vec::new();
        for e in plan
            .events
            .iter()
            .filter(|e| e.role == "text_hook" || e.role == "caption")
        {
            if !font_ok(e) {
                in_zone.push(format!("{}: font not measurable", e.id));
                continue;
            }
            if let Some(bb) = ink::ink_bbox(
                &r.font,
                &e.text,
                e.size,
                e.align,
                e.pos[0],
                e.pos[1],
                e.letter_spacing,
            ) && zones.iter().any(|z| ink::intersects(bb, *z))
            {
                in_zone.push(format!(
                    "{} {:?} ink [{:.0},{:.0},{:.0},{:.0}]",
                    e.id, e.text, bb[0], bb[1], bb[2], bb[3]
                ));
            }
        }
        if !in_zone.is_empty() {
            d.push(format!("ink in IG overlay zones: {in_zone:?}"));
        }
        checks.push(c(
            "G9",
            true,
            pass_if(ok && in_zone.is_empty()),
            Value::from(
                hooks
                    .first()
                    .map_or(0, |h| h.text.split_whitespace().count()),
            ),
            d,
        ));
    }

    // G10 measured number --------------------------------------------------------------------
    {
        let mut d = Vec::new();
        // evidence frame → master seconds, by the source's hash
        let to_master = |src: &str, frame: u64| -> Option<f64> {
            let sha = plan.sources.get(src)?;
            let off = if sha == &t.sources.picture.sha256 {
                t.sources.picture.frame_offset
            } else if sha == &t.sources.audio.sha256 {
                t.sources.audio.frame_offset
            } else {
                return None;
            };
            Some((frame as f64 - off as f64) / fps)
        };
        let ok = (|| {
            let h = hooks.first()?;
            let nums = shown_numbers(&h.text);
            if nums.is_empty() {
                d.push("the hook shows no number".into());
                return Some(false);
            }
            let prov = overlay_facts.get(h.id.as_str()).and_then(|o| o.number_provenance.clone());
            let Some(prov) = prov.filter(|p| p.kind == "measurement") else {
                d.push("the hook's number provenance is not a measurement".into());
                return Some(false);
            };
            let Some(m) = prov.id.as_deref().and_then(|id| pj.measurements.iter().find(|m| m.id == id)) else {
                d.push(format!("measurement {:?} not found", prov.id));
                return Some(false);
            };
            let (Some(s0), Some(s1)) = (m.start_s, m.stop_s) else {
                d.push("measurement has no start/stop".into());
                return Some(false);
            };
            let rc = &t.run_clock;
            let tol = rc.tolerance_s + 1.0 / fps;
            let mut distinct: Vec<(&str, u64)> = m.evidence.iter().map(|e| (e.source.as_str(), e.frame_index)).collect();
            distinct.sort();
            distinct.dedup();
            let at: Vec<Option<f64>> = distinct.iter().map(|(s, f)| to_master(s, *f)).collect();
            let evidence_ok = distinct.len() >= th.evidence_frames_min
                && at.iter().any(|x| x.is_some_and(|x| (x - s0).abs() <= tol))
                && at.iter().any(|x| x.is_some_and(|x| (x - s1).abs() <= tol));
            let near = (s0 - rc.start_s).abs() <= rc.tolerance_s && (s1 - rc.stop_s).abs() <= rc.tolerance_s;
            let value = s1 - s0;
            let agree = nums.iter().any(|(_, f)| agrees(f, value));
            d.push(format!(
                "measurement {} {} [{s0:.3}, {s1:.3}] = {value:.3} s; evidence at {:?} s; shown {:?}",
                m.id,
                m.kind,
                at,
                nums.iter().map(|n| &n.0).collect::<Vec<_>>()
            ));
            if !evidence_ok {
                d.push("evidence: need ≥ 2 distinct frames of the stream's sources, one at the start and one at the stop".into());
            }
            if !near {
                d.push(format!("start/stop not within ±{} s of {}/{}", rc.tolerance_s, rc.start_s, rc.stop_s));
            }
            if !agree {
                d.push("the shown number disagrees with the measurement (SPEC §5.13 rounding)".into());
            }
            Some(m.kind == "interval" && evidence_ok && near && agree)
        })()
        .unwrap_or(false);
        checks.push(c(
            "G10",
            true,
            pass_if(ok),
            Value::from(
                hooks
                    .first()
                    .map(|h| {
                        shown_numbers(&h.text)
                            .into_iter()
                            .map(|n| n.0)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            ),
            d,
        ));
    }

    // G11 all numbers ----------------------------------------------------------------------
    {
        let mut bad = Vec::new();
        for e in plan.events.iter().filter(|e| e.role != "caption") {
            if !e.text.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
            let fact = overlay_facts.get(e.id.as_str());
            let author = fact.map(|f| f.author.as_str()).unwrap_or("");
            let prov = fact
                .and_then(|f| f.number_provenance.as_ref())
                .map(|p| p.kind.clone())
                .or_else(|| e.provenance.clone().filter(|k| k != "agent"));
            match prov {
                None => bad.push(format!("{} {:?} has no number_provenance", e.id, e.text)),
                Some(k) if agent_authored(author) && k != "measurement" && k != "transcript" => bad
                    .push(format!(
                        "{} {:?} is agent-authored ({author:?}) with {k} provenance",
                        e.id, e.text
                    )),
                _ => {}
            }
        }
        if let Some(title) = &pj.cut.title
            && title.text.chars().any(|c| c.is_ascii_digit())
        {
            match &title.number_provenance {
                None => bad.push(format!("title {:?} has no number_provenance", title.text)),
                Some(p)
                    if agent_authored(&title.author)
                        && p.kind != "measurement"
                        && p.kind != "transcript" =>
                {
                    bad.push(format!(
                        "title is agent-authored with {} provenance",
                        p.kind
                    ))
                }
                _ => {}
            }
        }
        match export {
            None => bad.push("no single export record to read lints from".into()),
            Some(e) => {
                let errors: Vec<&str> = e
                    .lints_at_export
                    .iter()
                    .filter(|l| l.level == "error")
                    .map(|l| l.code.as_str())
                    .collect();
                if !errors.is_empty() {
                    bad.push(format!("error-level lints at export: {errors:?}"));
                }
            }
        }
        checks.push(c(
            "G11",
            true,
            pass_if(bad.is_empty()),
            Value::from(bad.len()),
            bad,
        ));
    }

    // G12 steady layout ----------------------------------------------------------------------
    {
        let cam_t = t.cam_place_px;
        let tol = th.cam_place_tol;
        let (mut cam_frames, mut cam_good, mut screen_frames, mut screen_good, mut full_subject) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut layouts: Vec<String> = Vec::new();
        for p in &plan.parts {
            let cam = p.layers.iter().find(|l| l.role == "cam" && l.visible);
            let role = role_of(p).unwrap_or_default();
            if let Some(cl) = cam {
                cam_frames += p.frames();
                let place_ok =
                    (0..3).all(|k| (cl.place[k] - cam_t[k]).abs() <= tol * cam_t[k].abs());
                if iou(cl.crop, t.cam_region_px) >= th.cam_iou_min && place_ok {
                    cam_good += p.frames();
                }
            }
            // calibrated: a payoff beat with the cam hidden may fill the frame (AGENTS.md
            // rule 3); every other part is held to the screen band, and such full-subject
            // parts are capped
            let full = cam.is_none() && (role == "hook" || role == "payoff");
            if full {
                full_subject += p.frames();
            } else if let Some(s) = p.layers.iter().find(|l| l.role == "screen" && l.visible) {
                screen_frames += p.frames();
                if s.place[1] >= th.screen_band_y[0]
                    && s.place[1] + s.place[3] <= th.screen_band_y[1]
                {
                    screen_good += p.frames();
                }
            } else {
                screen_frames += p.frames();
            }
            let mut key: Vec<String> = p
                .layers
                .iter()
                .filter(|l| l.visible)
                .map(|l| {
                    format!(
                        "{}:{:?}:{:?}",
                        l.role,
                        l.crop.map(|x| x.round() as i64),
                        l.place.map(|x| x.round() as i64)
                    )
                })
                .collect();
            key.sort();
            let key = key.join("|");
            if !layouts.contains(&key) {
                layouts.push(key);
            }
        }
        let share = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        let (cam_share, screen_share) = (
            share(cam_good, cam_frames),
            share(screen_good, screen_frames),
        );
        let full_share = share(full_subject, plan.frames);
        let ok = cam_share >= th.cam_share_min
            && screen_share >= th.screen_share_min
            && full_share <= FULL_SUBJECT_SHARE_MAX
            && layouts.len() <= th.layouts_max;
        checks.push(c(
            "G12",
            true,
            pass_if(ok),
            serde_json::json!({"layouts": layouts.len(), "cam_share": (cam_share * 1000.0).round() / 1000.0, "screen_share": (screen_share * 1000.0).round() / 1000.0}),
            vec![format!(
                "cam on target {cam_good}/{cam_frames} frames ({cam_share:.3}); screen in y {:?} {screen_good}/{screen_frames} ({screen_share:.3}); full-subject payoff parts {full_share:.3} of output (max {FULL_SUBJECT_SHARE_MAX}); {} distinct layouts (≤ {})",
                th.screen_band_y,
                layouts.len(),
                th.layouts_max
            )],
        ));
    }

    // G13 content recall (reported) -----------------------------------------------------------
    {
        let golden: Vec<String> = content_words(
            &r.golden_words
                .iter()
                .map(|w| w.word.clone())
                .collect::<Vec<_>>(),
        );
        let (status, value, d) = match &b.asr {
            None => (
                Status::Pending,
                Value::Null,
                vec!["no round-trip ASR".to_owned()],
            ),
            Some(ws) => {
                let got = content_words(&ws.iter().map(|w| w.word.clone()).collect::<Vec<_>>());
                let recall = if golden.is_empty() {
                    0.0
                } else {
                    lcs(&golden, &got) as f64 / golden.len() as f64
                };
                (
                    if recall >= th.content_recall_report_gate {
                        Status::Pass
                    } else {
                        Status::Fail
                    },
                    Value::from((recall * 1000.0).round() / 1000.0),
                    vec![format!(
                        "LCS recall {recall:.3} of {} golden content words (proposed M3 gate ≥ {})",
                        golden.len(),
                        th.content_recall_report_gate
                    )],
                )
            }
        };
        checks.push(c("G13", false, status, value, d));
    }

    // G14 loudness, gain, hygiene (reported) -----------------------------------------------------
    {
        let mut d = Vec::new();
        let loud_ok = export.and_then(|e| e.loudness.as_ref()).map(|l| {
            let ok = l.mode == "static"
                && (l.integrated_lufs - (-16.0)).abs() <= th.loudness_tol_lu
                && l.true_peak_dbtp <= th.true_peak_max_dbtp;
            d.push(format!(
                "{} I {:.2} LUFS (framing: static −16), TP {:.2} dBTP",
                l.mode, l.integrated_lufs, l.true_peak_dbtp
            ));
            ok
        });
        let unmeasured: Vec<&str> = pj
            .cut
            .gain_segments
            .iter()
            .filter(|g| agent_authored(&g.author) && g.measurement_ref.is_none())
            .map(|g| g.id.as_str())
            .collect();
        if !unmeasured.is_empty() {
            d.push(format!(
                "agent gain segments without measurement_ref: {unmeasured:?}"
            ));
        }
        let mut counts = serde_json::Map::new();
        if let Some(text) = &b.transcript {
            for code in ["Unauthorized", "Invalid", "Conflict"] {
                counts.insert(
                    code.into(),
                    Value::from(
                        text.matches(&format!("\\\"code\\\":\\\"{code}")).count()
                            + text.matches(&format!("\"code\":\"{code}")).count(),
                    ),
                );
            }
            let harness = b
                .run
                .as_ref()
                .and_then(|r| r["harness"].as_str())
                .and_then(Harness::parse)
                .unwrap_or(Harness::ClaudeCode);
            let parsed = transcript::parse(harness, text);
            counts.insert("tool_calls".into(), Value::from(parsed.calls.len()));
            counts.insert("tokens_in".into(), Value::from(parsed.usage.input_tokens));
            counts.insert("tokens_out".into(), Value::from(parsed.usage.output_tokens));
        }
        if let Some(run) = &b.run {
            counts.insert("wall_clock_ms".into(), run["elapsed_ms"].clone());
        }
        let status = match loud_ok {
            None => Status::Pending,
            Some(ok) => pass_if(ok && unmeasured.is_empty()),
        };
        checks.push(c("G14", false, status, Value::Object(counts), d));
    }

    let failed: Vec<&'static str> = checks
        .iter()
        .filter(|c| c.must && c.status == Status::Fail)
        .map(|c| c.id)
        .collect();
    let pending: Vec<&'static str> = checks
        .iter()
        .filter(|c| c.must && c.status == Status::Pending)
        .map(|c| c.id)
        .collect();
    let verdict = if !failed.is_empty() {
        Verdict::Fail
    } else if !pending.is_empty() {
        Verdict::Provisional
    } else {
        Verdict::Pass
    };
    Ok(Report {
        schema: "gmclip.eval.h79.grade/0",
        mode: b.mode,
        verdict,
        failed,
        pending,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_rounding_rule() {
        let n = |s: &str| {
            shown_numbers(s)
                .into_iter()
                .map(|x| x.1)
                .collect::<Vec<_>>()
        };
        assert!(agrees(&n("in 67 seconds")[0], 67.434));
        assert!(agrees(&n("DONE IN 1:07")[0], 67.434));
        assert!(!agrees(&n("in 60 seconds")[0], 67.434));
        assert!(!agrees(&n("1:08")[0], 67.434));
        assert!(agrees(&n("67.4 s")[0], 67.434));
        assert!(!agrees(&n("67.5 s")[0], 67.434));
        assert!(n("no digits").is_empty());
        // no overflow on absurd m:ss
        assert!(matches!(n("99999999999999999999:07")[0], NumberForm::Other));
        assert!(matches!(n("307999999999999999:07")[0], NumberForm::Other));
    }

    #[test]
    fn path_containment_is_lexical_and_strict() {
        assert!(inside("/fx/home/a.mp4", "/fx/home"));
        assert!(!inside("/fx/home/../../etc/passwd", "/fx/home"));
        assert!(!inside("/fx/homey/a", "/fx/home"));
        assert!(!inside("relative/a", "/fx/home"));
        assert!(inside(
            "/private/var/folders/x/T/fx/a",
            "/var/folders/x/T/fx"
        ));
        // Windows drive paths: either separator, any case, never across drives or forms
        assert!(inside(r"C:\Users\x\fx\a.mp4", r"c:\users\x\fx"));
        assert!(inside("C:/Users/x/fx/a.mp4", r"C:\Users\x\fx"));
        assert!(!inside(r"C:\Users\x\fx\..\..\secret", r"C:\Users\x\fx"));
        assert!(!inside(r"D:\Users\x\fx\a", r"C:\Users\x\fx"));
        assert!(!inside(r"C:\Users\x\fxy\a", r"C:\Users\x\fx"));
        assert!(!inside(r"C:\Users\x\fx\a", "/Users/x/fx"));
        for p in [
            "~/x",
            "C:\\x",
            "a/../b",
            "Documents/youtube/x.json",
            "file:///etc",
            " /etc/passwd".trim(),
        ] {
            assert!(path_like(p), "{p}");
        }
        for p in ["cut_01", "30/1", "gmclip://cut/1", "9:16"] {
            assert!(!path_like(p), "{p}");
        }
    }

    #[test]
    fn nested_json_strings_and_keys_are_scanned() {
        let v = serde_json::json!({"ops": "[{\"path\": \"/Users/x/secret\"}]", "/etc/hosts": 1, "a": "  /tmp/y "});
        let mut out = Vec::new();
        strings_in(&v, &mut out, 0);
        assert!(out.contains(&"/Users/x/secret".to_owned()));
        assert!(out.contains(&"/etc/hosts".to_owned()));
        assert!(out.contains(&"/tmp/y".to_owned()));
    }

    #[test]
    fn lcs_and_norm() {
        let a: Vec<String> = ["Hi,", "talking", "banana!"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: Vec<String> = ["talking", "banana"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lcs(&content_words(&a), &content_words(&b)), 2);
    }

    #[test]
    fn v4_has_three_continuous_joins() {
        let p: Value = serde_json::from_slice(
            &std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../schemas/examples/h79-v4.renderplan.json"
            ))
            .unwrap(),
        )
        .unwrap();
        // B-11: h79's 3 continuous joins
        assert_eq!(discontinuous_joins(&p), 13 - 3);
    }

    #[test]
    fn malformed_plans_do_not_panic() {
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"output": {"fps": {"num": 0, "den": 0}, "frames": 1}, "parts": []}),
            serde_json::json!({"output": {"fps": {"num": 30, "den": 1}, "frames": 1}, "parts": [{"out": [5, 1]}],
                "text_events": [{"id": "h", "role": "text_hook", "out": [0, 1], "align": 0, "runs": [{"text": "9999999999999999999:59"}]}]}),
        ] {
            let _ = read_plan(&bad, "");
            let _ = discontinuous_joins(&bad);
        }
    }
}
