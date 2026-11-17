//! Grader self-tests (SPEC §17.1.1 "Grader self-tests"; backlog §2.4, E02).
//!
//! - **(1) Golden.** v4's own RenderPlan (`schemas/examples/h79-v4.renderplan.json`) with
//!   the project facts of the v4 build ([`golden_project`]) passes G1–G12; G0 and G7 are
//!   run-only. G8(b) stays `pending` until the runner can call the B-08 cue builder, so
//!   the golden's verdict is `provisional` with no failure.
//! - **(2) v3** (`evals/h79/selftest/v3.plan.json`) fails G2 and G5.
//! - **(3) Eight mutants**, each built from the golden, fail exactly their check.
//! - **(4) The oracle** (E03) passes G0–G12: needs lane A2's tools, so it is approximated
//!   here by the golden as a *run* bundle with a gmclip-only transcript
//!   ([`golden_run`]); the real oracle replaces it (TODO(A2)/E03).
//!
//! Facts the plan cannot carry (part roles, anchors, provenance, the export record, the
//! run-clock measurement) are those of the v4 build: `build_v4.py` measured the run from
//! the ComfyUI queue (0→1 at frame 1375 of the 2565 s window = 2610.833 s) to the first
//! frame the banana is drawn (frame 98 of the 2675 s window = 2678.267 s), cut every edge
//! at a measured energy minimum, and lifted the generated voice by +10 dB on measured
//! spans. Mutant 5's `verify{joins}` result is what verify would report for that render
//! (synthetic until `gmclip render` + `verify{joins}` exist; TODO(H-10/A25)).

use serde_json::{Value, json};

use super::bundle::{
    Bundle, CaptionDoc, EdgeFact, EvidenceFrame, ExportFact, Lint, Loudness, Measurement, Mode,
    NoCaption, OverlayFact, PartFact, Project, Provenance, TitleFact, Verify, Word,
};
use super::grade::{self, Reference, Report, Status, Verdict};

/// The MKV picture's frame offset after ~1000 s (its measured alignment to the proxy).
const MKV_OFFSET: i64 = -1642;
const FIXTURE_ROOT: &str = "/fixture/h79";
const RUN_ID: &str = "selftest-run";

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn v4_plan_bytes() -> Result<Vec<u8>, String> {
    std::fs::read(repo().join("schemas/examples/h79-v4.renderplan.json"))
        .map_err(|e| format!("v4 plan: {e}"))
}

fn role_for(id: &str) -> &'static str {
    match id.get(..2).unwrap_or("") {
        "01" => "hook",
        "02" | "09" | "10" | "11" | "14" => "payoff",
        "12" | "13" => "reaction",
        "08" => "evidence",
        _ => "setup",
    }
}

/// The project facts of the v4 build for `plan`.
pub fn golden_project(plan: &Value, plan_sha256: &str) -> Project {
    let parts = plan["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let id = p["id"].as_str().unwrap_or("").to_owned();
            let edge = |side: &str| EdgeFact {
                side: side.into(),
                time_provenance: "cut_point".into(),
                edges: Some("word".into()),
                measure_id: None,
                offset_ms: None,
                author: "human".into(),
            };
            PartFact {
                role: role_for(&id).into(),
                edges: vec![edge("start"), edge("end")],
                id,
            }
        })
        .collect();
    let overlays = plan["text_events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["role"] != "caption")
        .map(|e| {
            let id = e["id"].as_str().unwrap_or("").to_owned();
            let text: String = e["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["text"].as_str())
                .collect();
            let measured =
                id == "hook" || text.starts_with("DONE IN") || text.starts_with("GENERATING");
            OverlayFact {
                number_provenance: text.chars().any(|c| c.is_ascii_digit()).then(|| {
                    if measured {
                        Provenance {
                            kind: "measurement".into(),
                            id: Some("m-run".into()),
                        }
                    } else {
                        Provenance {
                            kind: "human".into(),
                            id: None,
                        }
                    }
                }),
                author: "human".into(),
                time_provenance: Some("measured".into()),
                offset_ms: None,
                id,
            }
        })
        .collect();
    let gain_segments = plan
        .pointer("/audio/envelopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| super::bundle::GainSegment {
            id: e["id"].as_str().unwrap_or("").into(),
            time_provenance: "measured".into(),
            measurement_ref: Some(format!("m-loud-{}", e["id"].as_str().unwrap_or(""))),
            author: "human".into(),
        })
        .collect();
    let sig = format!("sig-{}", &plan_sha256[..16]);
    let joins = super::grade::discontinuous_joins(plan);
    Project {
        schema: "gmclip.eval.h79.project/0".into(),
        run_id: None,
        fixture_root: Some(FIXTURE_ROOT.into()),
        cut: super::bundle::CutFacts {
            id: "h79-eval".into(),
            framing: "9:16".into(),
            title: plan
                .pointer("/meta/title")
                .and_then(Value::as_str)
                .map(|t| TitleFact {
                    text: t.into(),
                    author: "human".into(),
                    number_provenance: Some(Provenance {
                        kind: "measurement".into(),
                        id: Some("m-run".into()),
                    }),
                }),
            parts,
            overlays,
            captions: Some(CaptionDoc {
                enabled: true,
                source: "heard".into(),
                grouping: "story_v5".into(),
                // the generated voice: its own on-film text carries it (build_v4.py
                // BANANA_VOICE 2708.40–2710.40 = all of part 01; 2692.80–2694.95 in part 11)
                no_caption: vec![
                    NoCaption {
                        out: [0, 55],
                        time_provenance: "measured".into(),
                    },
                    NoCaption {
                        out: [1335, 1400],
                        time_provenance: "measured".into(),
                    },
                ],
            }),
            gain_segments,
            current_render_sig: Some(sig.clone()),
        },
        measurements: vec![Measurement {
            id: "m-run".into(),
            kind: "interval".into(),
            start_s: Some(2610.833),
            stop_s: Some(2678.267),
            evidence: vec![
                // ComfyUI queue 0→1: frame 1375 of the 2565 s window = native 78325
                EvidenceFrame {
                    source: "mkv1440".into(),
                    frame_index: (78325 + MKV_OFFSET) as u64,
                },
                // the banana first drawn in the player: frame 98 of the 2675 s window = native 80348
                EvidenceFrame {
                    source: "mkv1440".into(),
                    frame_index: (80348 + MKV_OFFSET) as u64,
                },
            ],
        }],
        exports: vec![ExportFact {
            id: "export-9x16".into(),
            framing: "9:16".into(),
            blocked: false,
            created_by_run: None,
            render_sig: sig.clone(),
            plan_sha256: plan_sha256.into(),
            verify_export: Some(Verify {
                ok: true,
                render_sig: Some(sig.clone()),
                joins_checked: None,
                failures: vec![],
            }),
            verify_joins: Some(Verify {
                ok: true,
                render_sig: Some(sig),
                joins_checked: Some(joins),
                failures: vec![],
            }),
            lints_at_export: vec![Lint {
                level: "warning".into(),
                code: "agent_grading_review".into(),
            }],
            // B-12 on the golden plans: I −15.99…−16.01 LUFS, TP −1.51 dBTP
            loudness: Some(Loudness {
                mode: "static".into(),
                target_lufs: Some(-16.0),
                integrated_lufs: -16.0,
                true_peak_dbtp: -1.51,
            }),
            cues_equal_reference: None,
        }],
    }
}

fn bundle(plan: Value, mode: Mode, asr: Option<Vec<Word>>) -> Bundle {
    let bytes = serde_json::to_vec(&plan).unwrap_or_default();
    let sha = gmclip_ai::launcher::sha256::hex(&bytes);
    let project = golden_project(&plan, &sha);
    Bundle {
        plan,
        plan_sha256: sha,
        project,
        transcript: None,
        run: None,
        asr,
        mode,
    }
}

/// Re-derives the facts that follow from the plan bytes (sha, sigs) after a mutation.
fn refresh(b: &mut Bundle) {
    let bytes = serde_json::to_vec(&b.plan).unwrap_or_default();
    b.plan_sha256 = gmclip_ai::launcher::sha256::hex(&bytes);
    let sig = format!("sig-{}", &b.plan_sha256[..16]);
    b.project.cut.current_render_sig = Some(sig.clone());
    for e in &mut b.project.exports {
        e.plan_sha256 = b.plan_sha256.clone();
        e.render_sig = sig.clone();
        if let Some(v) = &mut e.verify_export {
            v.render_sig = Some(sig.clone());
        }
        if let Some(v) = &mut e.verify_joins {
            v.render_sig = Some(sig.clone());
        }
    }
}

/// (1) The golden as a reference bundle.
pub fn golden(r: &Reference) -> Result<Bundle, String> {
    let plan: Value = serde_json::from_slice(&v4_plan_bytes()?).map_err(|e| e.to_string())?;
    Ok(bundle(plan, Mode::Reference, Some(r.golden_words.clone())))
}

/// A clean launcher transcript: gmclip calls only.
pub fn clean_transcript() -> String {
    [
        json!({"type":"system","subtype":"init","tools":["mcp__gmclip__project_overview","mcp__gmclip__cut_edit","mcp__gmclip__export"]}),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"mcp__gmclip__project_overview","input":{}}]}}),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"mcp__gmclip__see","input":{"target":"source","at":[76683]}}]}}),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"mcp__gmclip__export","input":{"cut":"h79-eval","framing":"9:16","path":format!("{FIXTURE_ROOT}/exports/h79.mp4")}}]}}),
        json!({"type":"result","result":"Exported.","usage":{"input_tokens":1000,"output_tokens":200}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n")
}

/// (4, approximated) The golden as a run bundle with a gmclip-only transcript.
pub fn golden_run(r: &Reference) -> Result<Bundle, String> {
    let mut b = golden(r)?;
    b.mode = Mode::Run;
    b.project.run_id = Some(RUN_ID.into());
    for e in &mut b.project.exports {
        e.created_by_run = Some(RUN_ID.into());
    }
    set_transcript(&mut b, clean_transcript());
    Ok(b)
}

/// Puts `t` in a run bundle with a run record bound to it (as the launcher writes it).
pub fn set_transcript(b: &mut Bundle, t: String) {
    let sha = gmclip_ai::launcher::sha256::hex(t.as_bytes());
    b.run = Some(json!({
        "schema": "gmclip.agent-run/0",
        "run": RUN_ID,
        "cut": b.project.cut.id,
        "harness": "claude-code",
        "timed_out": false,
        "elapsed_ms": 1_800_000,
        "transcript_sha256": sha,
        "binary": {"version_output": "2.1.300 (Claude Code)"},
        "argv": ["/opt/claude", "-p", "--model", "claude-opus-5-5"],
    }));
    b.transcript = Some(t);
}

/// (2) v3.
pub fn v3(r: &Reference) -> Result<Bundle, String> {
    let p = repo().join("evals/h79/selftest/v3.plan.json");
    let plan: Value =
        serde_json::from_slice(&std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?)
            .map_err(|e| e.to_string())?;
    let _ = r;
    Ok(bundle(plan, Mode::Reference, None))
}

fn part_mut(plan: &mut Value, idx: usize) -> &mut Value {
    &mut plan["parts"][idx]
}

fn hook_mut(plan: &mut Value) -> &mut Value {
    let i = plan["text_events"]
        .as_array()
        .and_then(|a| a.iter().position(|e| e["role"] == "text_hook"))
        .unwrap_or(0);
    &mut plan["text_events"][i]
}

/// Shifts a part's source window by `frames` (picture, native and audio together), keeping
/// its output range.
fn shift_source(part: &mut Value, frames: i64) {
    let add = |v: &mut Value, d: i64| {
        if let Some(n) = v.as_i64() {
            *v = Value::from(n + d);
        }
    };
    add(&mut part["picture"]["first_frame"], frames);
    add(&mut part["native"]["frames"][0], frames);
    add(&mut part["native"]["frames"][1], frames);
    add(&mut part["audio"]["first_sample"], frames * 1600);
}

/// The eight mutants: `(name, the check it must fail, bundle)`.
pub fn mutants(r: &Reference) -> Result<Vec<(&'static str, &'static str, Bundle)>, String> {
    let mut out = Vec::new();

    // G3: part 1 moved to the prompt (same length; a stretch of the prompt reading)
    let mut b = golden(r)?;
    {
        let p1 = part_mut(&mut b.plan, 0);
        let native0 = p1["native"]["frames"][0].as_i64().unwrap_or(0);
        let target = (2588.0f64 * 30.0).round() as i64; // inside the setup region
        shift_source(p1, target - native0);
    }
    refresh(&mut b);
    out.push(("part 1 moved to the prompt", "G3", b));

    // G7: a raw anchor
    let mut b = golden(r)?;
    b.project.cut.parts[5].edges[1].time_provenance = "raw".into();
    out.push(("a raw anchor on a part edge", "G7", b));

    // G10: "60" with the 67.4 s measurement
    let mut b = golden(r)?;
    {
        let h = hook_mut(&mut b.plan);
        for run in h["runs"].as_array_mut().into_iter().flatten() {
            if let Some(t) = run["text"].as_str() {
                run["text"] = Value::from(t.replace("67", "60"));
            }
        }
    }
    refresh(&mut b);
    out.push(("\"60\" shown with the 67.4 s measurement", "G10", b));

    // G9: a hook at y 600
    let mut b = golden(r)?;
    hook_mut(&mut b.plan)["pos"]["y"] = Value::from(600.0);
    refresh(&mut b);
    out.push(("the hook at y 600", "G9", b));

    // G6: an 80 ms clip into "minutes" at a join (part 02 ends on "…two minutes."; its
    // source window moves 2 frames = 67 ms earlier, the nearest frame to 80 ms). verify
    // {joins} on that render reports the clipped word.
    let mut b = golden(r)?;
    shift_source(part_mut(&mut b.plan, 1), -2);
    refresh(&mut b);
    if let Some(v) = b.project.exports[0].verify_joins.as_mut() {
        v.ok = false;
        v.failures = vec![
            json!({"join": "02-claim|03-terrible-prompt", "word": "minutes", "clipped_ms": 80, "synthetic": true}),
        ];
    }
    out.push(("an 80 ms clip into \"minutes\" at a join", "G6", b));

    // G8: a cue merged to 5 words (adjacent cues of 3 + 2 words whose merged ink stays out
    // of the overlay zones, so only the word rule trips)
    let mut b = golden(r)?;
    {
        let ev = b.plan["text_events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let words = |e: &Value| -> usize {
            e["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["text"].as_str())
                .collect::<String>()
                .split_whitespace()
                .count()
        };
        let text_of = |e: &Value| -> String {
            e["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["text"].as_str())
                .collect()
        };
        let mut pick = None;
        for i in 0..ev.len().saturating_sub(1) {
            let (a, c) = (&ev[i], &ev[i + 1]);
            if a["role"] == "caption"
                && c["role"] == "caption"
                && words(a) + words(c) == 5
                && a["out"][1] == c["out"][0]
            {
                let merged = format!("{} {}", text_of(a), text_of(c));
                let size = a["size"].as_f64().unwrap_or(86.0);
                let bb = super::ink::ink_bbox(
                    &r.font,
                    &merged,
                    size,
                    5,
                    a["pos"]["x"].as_f64().unwrap_or(540.0),
                    a["pos"]["y"].as_f64().unwrap_or(1065.0),
                    0.0,
                );
                if bb
                    .is_some_and(|bb| bb[2] < 1080.0 - r.truth.thresholds.ig_right.w && bb[0] > 0.0)
                {
                    pick = Some(i);
                    break;
                }
            }
        }
        let i = pick.ok_or("no 3+2 adjacent cue pair fits on one line")?;
        let next = ev[i + 1].clone();
        let arr = b.plan["text_events"].as_array_mut().ok_or("text_events")?;
        let mut runs = arr[i]["runs"].as_array().cloned().unwrap_or_default();
        runs.push(json!({"text": " "}));
        runs.extend(next["runs"].as_array().cloned().unwrap_or_default());
        arr[i]["runs"] = Value::from(runs);
        arr[i]["out"][1] = next["out"][1].clone();
        arr.remove(i + 1);
    }
    refresh(&mut b);
    out.push(("a cue merged to 5 words", "G8", b));

    // G12: the cam place moved 200 px
    let mut b = golden(r)?;
    for p in b.plan["parts"].as_array_mut().into_iter().flatten() {
        for l in p["layers"].as_array_mut().into_iter().flatten() {
            if l["role"] == "cam" && l.get("visible").and_then(Value::as_bool) != Some(false) {
                let y = l["place"]["y"].as_f64().unwrap_or(0.0);
                l["place"]["y"] = Value::from(y + 200.0);
            }
        }
    }
    refresh(&mut b);
    out.push(("the cam place moved 200 px", "G12", b));

    // G0: a Bash call in the transcript
    let mut b = golden_run(r)?;
    let mut t = clean_transcript();
    t.push('\n');
    t.push_str(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Bash","input":{"command":"cat ~/clips/production/h79-v3-2026-10-04/render-spec-v4.json"}}]}}).to_string());
    // the record is bound to this transcript, so only the Bash call can fail G0
    set_transcript(&mut b, t);
    out.push(("a Bash call in the transcript", "G0", b));

    // G11: an agent-authored pill whose number claims human provenance
    let mut b = golden(r)?;
    if let Some(o) = b.project.cut.overlays.iter_mut().find(|o| o.id == "pill-1") {
        o.author = "launched:run".into();
        o.number_provenance = Some(Provenance {
            kind: "human".into(),
            id: None,
        });
    }
    out.push((
        "an agent pill whose number claims human provenance",
        "G11",
        b,
    ));

    Ok(out)
}

/// One self-test row.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Row {
    pub name: String,
    pub expect: String,
    pub ok: bool,
    pub verdict: Verdict,
    pub failed: Vec<&'static str>,
    pub pending: Vec<&'static str>,
}

/// Runs every self-test; `Ok(rows)` with each row's `ok`.
pub fn run_all(r: &Reference) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut row = |name: &str, expect: String, rep: &Report, ok: bool| {
        rows.push(Row {
            name: name.into(),
            expect,
            ok,
            verdict: rep.verdict,
            failed: rep.failed.clone(),
            pending: rep.pending.clone(),
        });
    };
    let g = grade::grade(&golden(r)?, r)?;
    let g_ok = g.failed.is_empty() && g.pending.iter().all(|p| *p == "G8");
    row(
        "(1) golden v4 (reference)",
        "no failure; only G8(b) pending".into(),
        &g,
        g_ok,
    );
    let gr = grade::grade(&golden_run(r)?, r)?;
    row(
        "(4~) golden v4 as a gmclip-only run",
        "G0–G12 pass; only G8(b) pending".into(),
        &gr,
        gr.failed.is_empty() && gr.get("G0").status == Status::Pass,
    );
    let v = grade::grade(&v3(r)?, r)?;
    row(
        "(2) v3",
        "fails G2 and G5".into(),
        &v,
        v.failed.contains(&"G2") && v.failed.contains(&"G5"),
    );
    for (name, check, b) in mutants(r)? {
        let rep = grade::grade(&b, r)?;
        let ok = rep.failed == [check];
        row(
            &format!("(3) {name}"),
            format!("fails exactly {check}"),
            &rep,
            ok,
        );
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> Reference {
        Reference::load(&super::super::bundle::h79_dir()).unwrap()
    }

    #[test]
    fn golden_passes_g1_to_g12() {
        let r = reference();
        let rep = grade::grade(&golden(&r).unwrap(), &r).unwrap();
        for c in &rep.checks {
            eprintln!("{} {:?} {} {:?}", c.id, c.status, c.value, c.detail);
        }
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(
            rep.pending,
            ["G8"],
            "only G8(b) waits (runner hook to B-08)"
        );
        assert_eq!(rep.verdict, Verdict::Provisional);
        // the spec's "v4" column
        assert_eq!(rep.get("G2").value, json!(60.23));
        assert_eq!(rep.get("G4").value, json!(0.931));
        assert_eq!(rep.get("G5").value["median_s"], json!(3.65));
        assert_eq!(rep.get("G5").value["parts"], json!(14));
        assert_eq!(rep.get("G9").value, json!(7));
        assert_eq!(rep.get("G10").value, json!(["67"]));
        assert_eq!(rep.get("G12").value["layouts"], json!(7));
        assert_eq!(rep.get("G13").value, json!(1.0));
    }

    #[test]
    fn v3_fails_duration_and_jumpiness() {
        let r = reference();
        let rep = grade::grade(&v3(&r).unwrap(), &r).unwrap();
        assert!(
            rep.failed.contains(&"G2") && rep.failed.contains(&"G5"),
            "{:?}",
            rep.failed
        );
        assert_eq!(rep.get("G2").value, json!(22.5));
    }

    #[test]
    fn each_mutant_fails_exactly_its_check() {
        let r = reference();
        let ms = mutants(&r).unwrap();
        assert_eq!(
            ms.len(),
            9,
            "the spec's eight plus the G11 agent-authorship mutant"
        );
        for (name, check, b) in ms {
            let rep = grade::grade(&b, &r).unwrap();
            assert_eq!(
                rep.failed,
                [check],
                "{name}: {:?}",
                rep.checks
                    .iter()
                    .filter(|c| c.status == Status::Fail)
                    .map(|c| (c.id, &c.detail))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn golden_as_a_clean_run_passes_isolation() {
        let r = reference();
        let rep = grade::grade(&golden_run(&r).unwrap(), &r).unwrap();
        assert_eq!(
            rep.get("G0").status,
            Status::Pass,
            "{:?}",
            rep.get("G0").detail
        );
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    }
}
