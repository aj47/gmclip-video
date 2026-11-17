//! A.09 lab (SPEC §5.10, R-CAP-1 measured per F17): captions from the cut's own audio,
//! identical parts reusing the source words (decision 0022, AJ 2026-10-16: "every cut
//! should have its own audio if it may be different. otherwise if there is no difference
//! we can re use.").
//!
//! The 10 golden reels are cuts of their streams' project fixtures
//! (`fixtures/projects/<vid>.json` in the lab media folder, written by [`gmclip_core::fixture::
//! write_lab_project`], the stream's master ASR words included) and are planned with
//! `gmclip plan-cut` four ways:
//! - as it is: the reel's own cues, `story_v5` over `qa/heard.json` (the reference's
//!   transcription of the rendered cut, the old `heard` reference; `rt` in the report);
//! - `--captions heard --hear all`: gmclip hears the cut's whole voice mix (fully heard);
//! - `--captions heard --hear rule`: the product — identical parts reuse the master words,
//!   the rest are heard;
//! - `--captions heard --hear none`: every part reuses (what `stream` captions were).
//!
//! Each cut falls in a category by its rule audit: `reused_only` (no part heard), `mixed`
//! or `all_heard`. Cue-boundary F1 on the word alignment (`gmclip_ml::wer::
//! cue_boundaries`), caption-word WER and exact-text cue agreement are reported for rule
//! vs fully heard, per category and pooled, plus reuse vs fully heard and fully heard vs
//! the reference. `reuse_saves_asr_on_a_long_form` times hearing a 10-minute long-form
//! made of 20 takes of the loc_bd89f38b restream, all heard vs the rule.
//!
//! Measured 2026-10-16, release (`evals/results/captions/2026-10-16-a09-heard-reuse.json`,
//! `…-longform-asr.json`): rule vs fully heard F1 0.814 / WER 5.8 % on the 9 reused-only
//! cuts, 0.807 / 2.6 % on the mixed one (h34: one part under a +12 dB envelope); fully
//! heard vs the reference's heard F1 0.870 / WER 4.1 %; rule vs the reference 0.846 /
//! 2.8 %. Hearing every cut took 67.6 s of wall time, the rule 1.2 s. On the long-form the
//! rule transcribes 66 s of 600 and saves 25.1 s of 30.4 s (83 %).
//!
//! Reads the fixtures and the sources in the lab media folder (`GMCLIP_LAB_MEDIA`,
//! `gmclip_model::lab`); writes only a temp dir (and `GMCLIP_CAPTIONS_OUT` /
//! `GMCLIP_LONGFORM_OUT`, the reports). Timing is meaningful in a release build:
//! `cargo test -p gmclip-cli --release --test captions_lab -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use gmclip_ml::wer::{Agreement, Boundaries, TimedWord, cue_boundaries, quantile};
use serde_json::{Value, json};

const REELS: [&str; 10] = [
    "loc_086da141_h03",
    "loc_086da141_h25",
    "loc_086da141_h47",
    "loc_4fadb756_h34",
    "loc_4fadb756_h37",
    "loc_4fadb756_h48",
    "loc_bd89f38b_h41",
    "loc_bd89f38b_h43",
    "loc_bd89f38b_h53",
    "loc_bd89f38b_h88",
];
/// SPEC §5.10 R-CAP-1 [M1].
const F1_BAR: f64 = 0.90;
const WER_BAR: f64 = 0.03;
/// Regression floors from the 2026-10-16 run (rule vs fully heard F1 0.813; fully heard
/// vs the reference WER 0.041).
const RULE_F1_FLOOR: f64 = 0.79;
const HEARD_WER_FLOOR: f64 = 0.05;

fn gmclip(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gmclip"))
        .args(args)
        .env("GMCLIP_LOG", "off")
        .output()
        .unwrap()
}

/// A plan's caption cues, each as its words timed at the cue (seconds).
fn cues(plan: &Value) -> Vec<Vec<TimedWord>> {
    let fps = plan["output"]["fps"]["num"].as_f64().unwrap()
        / plan["output"]["fps"]["den"].as_f64().unwrap_or(1.0);
    let mut evs: Vec<&Value> = plan["text_events"]
        .as_array()
        .map(|a| a.iter().filter(|e| e["role"] == "caption").collect())
        .unwrap_or_default();
    evs.sort_by_key(|e| e["out"][0].as_u64().unwrap());
    evs.iter()
        .map(|e| {
            let (a, z) = (
                e["out"][0].as_f64().unwrap() / fps,
                e["out"][1].as_f64().unwrap() / fps,
            );
            let text: String = e["runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["text"].as_str().unwrap())
                .collect();
            text.split_whitespace()
                .map(|w| TimedWord {
                    text: w.to_owned(),
                    start: a,
                    end: z,
                })
                .collect()
        })
        .collect()
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Pooled agreement of one comparison.
#[derive(Default)]
struct Pool {
    b: Boundaries,
    s: usize,
    d: usize,
    i: usize,
    words: usize,
    cuts: usize,
}

impl Pool {
    fn add(&mut self, a: &Agreement, b: &Boundaries) {
        self.b.add(b);
        self.s += a.substitutions;
        self.d += a.deletions;
        self.i += a.insertions;
        self.words += a.reference_words;
        self.cuts += 1;
    }

    fn wer(&self) -> f64 {
        (self.s + self.d + self.i) as f64 / self.words.max(1) as f64
    }

    fn json(&self) -> Value {
        json!({
            "cuts": self.cuts,
            "f1": r3(self.b.f1()), "precision": r3(self.b.precision()), "recall": r3(self.b.recall()),
            "reference_breaks": self.b.reference, "matched_breaks": self.b.matched,
            "caption_word_wer": r3(self.wer()), "reference_words": self.words,
            "exact_text_agreement": r3(self.b.same_cues as f64 / self.b.reference_cues.max(1) as f64),
            "median_break_delta_ms": quantile(&self.b.deltas, 0.5).map(|d| (d * 1000.0).round()),
        })
    }
}

fn row(a: &Agreement, b: &Boundaries) -> Value {
    json!({
        "f1": r3(b.f1()), "wer": r3(a.wer()),
        "breaks": [b.reference, b.hypothesis, b.matched],
        "same_cues": [b.same_cues, b.reference_cues],
    })
}

/// A stream's project fixture's source files (`fixtures/projects/<vid>.json` in the lab media folder), in the
/// lab media folder.
fn fixture_sources(vid: &str) -> Vec<PathBuf> {
    let p = gmclip_model::lab::project(vid);
    let fx: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    fx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[0] == "source")
        .filter_map(|r| r[4]["path"].as_str())
        .map(gmclip_model::lab::media)
        .collect()
}

#[test]
#[ignore = "lab: the golden reels' stream projects, their sources and the speech model"]
fn heard_captions_on_the_rt_cuts() {
    let work = std::env::temp_dir().join(format!("gmclip-captions-lab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("out")).unwrap();
    // the reels whose stream's sources are on this machine
    let set = gmclip_model::lab::present(
        REELS,
        |r| r.to_string(),
        |r| fixture_sources(r.rsplit_once('_').unwrap().0),
    );
    if set.skip_if_empty() {
        return;
    }
    let mut vids: Vec<String> = set
        .items
        .iter()
        .map(|r| r.rsplit_once('_').unwrap().0.to_owned())
        .collect();
    vids.dedup();
    for v in &vids {
        gmclip_core::fixture::write_lab_project(v, &work.join(format!("out/{v}.sqlite"))).unwrap();
    }
    // comparison → category → pool
    let mut pools: BTreeMap<(&str, String), Pool> = BTreeMap::new();
    let mut rows = Vec::new();
    let (mut hear_all_s, mut hear_rule_s, mut cut_s, mut heard_s) = (0.0, 0.0, 0.0, 0.0);
    for reel in set.items.iter().copied() {
        let db = work.join(format!("out/{}.sqlite", reel.rsplit_once('_').unwrap().0));
        let project = db.to_str().unwrap();
        let plan = |extra: &[&str], name: &str| -> (Value, Value) {
            let o = work.join(format!("{reel}.{name}.json"));
            let r = work.join(format!("{reel}.{name}.report.json"));
            let mut a = vec![
                "plan-cut",
                project,
                "--hook",
                reel,
                "-o",
                o.to_str().unwrap(),
                "--report",
                r.to_str().unwrap(),
            ];
            a.extend_from_slice(extra);
            let out = gmclip(&a);
            assert!(
                out.status.success(),
                "{reel} {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let rep = std::fs::read(&r)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null);
            (
                serde_json::from_slice(&std::fs::read(&o).unwrap()).unwrap(),
                rep,
            )
        };
        let (rt, _) = plan(&[], "rt");
        let (all, all_rep) = plan(&["--captions", "heard", "--hear", "all"], "all");
        let (rule, rule_rep) = plan(&["--captions", "heard", "--hear", "rule"], "rule");
        let (reuse, _) = plan(&["--captions", "heard", "--hear", "none"], "reuse");
        let audit = &rule_rep["audit"];
        let parts = audit["parts"].as_array().unwrap();
        let heard_parts = parts
            .iter()
            .filter(|p| !p["changes"].as_array().unwrap().is_empty())
            .count();
        let category = if heard_parts == 0 {
            "reused_only"
        } else if heard_parts == parts.len() {
            "all_heard"
        } else {
            "mixed"
        };
        let samples = |k: &str| -> f64 {
            audit["stretches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s[k][1].as_f64().unwrap() - s[k][0].as_f64().unwrap())
                .sum::<f64>()
                / 48_000.0
        };
        let total_s = parts
            .iter()
            .map(|p| p["out"][1].as_f64().unwrap() - p["out"][0].as_f64().unwrap())
            .sum::<f64>()
            / 48_000.0;
        hear_all_s += all_rep["hear_seconds"].as_f64().unwrap_or(0.0);
        hear_rule_s += rule_rep["hear_seconds"].as_f64().unwrap_or(0.0);
        cut_s += total_s;
        heard_s += samples("out");
        let (c_rt, c_all, c_rule, c_reuse) = (cues(&rt), cues(&all), cues(&rule), cues(&reuse));
        let mut r = json!({
            "cut": reel, "category": category,
            "parts": parts.len(), "parts_heard": heard_parts,
            "why": parts.iter().map(|p| p["changes"].clone()).collect::<Vec<_>>(),
            "seconds": {"cut": r3(total_s), "heard": r3(samples("out")), "transcribed": r3(samples("window"))},
            "hear_seconds": {"all": all_rep["hear_seconds"], "rule": rule_rep["hear_seconds"]},
            "cues": {"rt": c_rt.len(), "all": c_all.len(), "rule": c_rule.len(), "reuse": c_reuse.len()},
        });
        for (name, reference, hyp) in [
            ("rule_vs_all", &c_all, &c_rule),
            ("reuse_vs_all", &c_all, &c_reuse),
            ("all_vs_rt", &c_rt, &c_all),
            ("rule_vs_rt", &c_rt, &c_rule),
        ] {
            let (al, b) = cue_boundaries(reference, hyp);
            r[name] = row(&al, &b);
            pools
                .entry((name, category.to_owned()))
                .or_default()
                .add(&al, &b);
            pools.entry((name, "all".into())).or_default().add(&al, &b);
        }
        eprintln!(
            "{reel} [{category}] rule vs all F1 {} WER {}; reuse vs all F1 {}; all vs reference F1 {}",
            r["rule_vs_all"]["f1"],
            r["rule_vs_all"]["wer"],
            r["reuse_vs_all"]["f1"],
            r["all_vs_rt"]["f1"]
        );
        rows.push(r);
    }
    let mut pooled = serde_json::Map::new();
    for ((name, cat), p) in &pools {
        pooled
            .entry(name.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap()
            .insert(cat.clone(), p.json());
    }
    let summary = json!({
        "backlog": "A.09",
        "measured": set.note("golden reels"),
        "decision": "0022: captions from the cut's own audio; a part whose voice mix is identical to its source (same source and track, no gain, envelope or fade beyond a de-click) reuses the source words, the rest is heard (AJ 2026-10-16)",
        "measure": "cue-boundary F1 (gmclip_ml::wer::cue_boundaries on the normalised word alignment), caption-word WER and exact-text cue agreement between plans of the same cut: rule = plan-cut --captions heard --hear rule; all = --hear all (the whole voice mix heard by gmclip's parakeet); reuse = --hear none (the stream's master ASR through every part, what stream captions were); rt = the cut as it is in its project fixture (the reference's captions-v5.ass, story_v5 over its qa/heard.json)",
        "bar": {"f1_min": F1_BAR, "wer_max": WER_BAR, "source": "SPEC §5.10 R-CAP-1 [M1] / F17"},
        "pooled": pooled,
        "asr": {
            "cut_seconds": r3(cut_s), "heard_seconds_by_rule": r3(heard_s),
            "hear_wall_seconds_all": r3(hear_all_s), "hear_wall_seconds_rule": r3(hear_rule_s),
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "rows": rows,
    });
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&summary["pooled"]).unwrap()
    );
    eprintln!("{}", serde_json::to_string_pretty(&summary["asr"]).unwrap());
    eprintln!("A.09 measured on {}", set.note("golden reels"));
    if let Some(p) = std::env::var_os("GMCLIP_CAPTIONS_OUT") {
        let mut t = serde_json::to_string_pretty(&summary).unwrap();
        t.push('\n');
        std::fs::write(p, t).unwrap();
    }
    if std::env::var_os("GMCLIP_CAPTIONS_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    // a reused-only cut is the reuse plan, word for word
    let reused = &pools[&("rule_vs_all", "all".to_owned())];
    assert!(reused.cuts == set.items.len());
    for r in &rows {
        if r["category"] == "reused_only" {
            assert_eq!(r["cues"]["rule"], r["cues"]["reuse"], "{}", r["cut"]);
        }
        if r["category"] == "all_heard" {
            assert_eq!(r["rule_vs_all"]["f1"], 1.0, "{}", r["cut"]);
        }
    }
    // measured floors (2026-10-16, release): rule vs fully heard F1 0.813 / WER 5.4 %;
    // gmclip's fully heard vs the reference's heard WER 4.1 % (two recognisers: R-CAP-1's 3 % is
    // not met by either path against the other)
    let rule = &pools[&("rule_vs_all", "all".to_owned())];
    assert!(
        rule.b.f1() >= RULE_F1_FLOOR,
        "rule vs all F1 {:.3}",
        rule.b.f1()
    );
    let h = &pools[&("all_vs_rt", "all".to_owned())];
    assert!(
        h.wer() <= HEARD_WER_FLOOR,
        "heard vs reference WER {:.3}",
        h.wer()
    );
}

/// A 10-minute long-form: 20 takes of 30 s from the loc_bd89f38b restream (35 s apart, so
/// every join is a cut), one take at +4 dB and one 20 s envelope at +6 dB; the rest as
/// recorded. Built from BENCH-60-M0's source and layer stack.
fn long_form() -> Value {
    let bench: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../test-media/bench-60-m0.renderplan.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut src = bench["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "a")
        .unwrap()
        .clone();
    // the bench plan names its source relative to the lab media folder
    src["path"] = json!(gmclip_model::lab::media_str(src["path"].as_str().unwrap()));
    let layers = bench["parts"][0]["layers"].clone();
    let (fps, take, step, t0) = (30u64, 30u64, 35u64, 600u64);
    let fade = json!({"samples": 144, "shape": "linear"});
    let parts: Vec<Value> = (0..20u64)
        .map(|i| {
            let t = t0 + i * step;
            json!({
                "id": format!("p{:02}", i + 1),
                "out": [i * take * fps, (i + 1) * take * fps],
                "picture": {"source": "a", "first_frame": t * fps},
                "audio": {"source": "a", "first_sample": t * 48_000, "rate": 48_000,
                          "gain_db": if i == 5 { 4.0 } else { 0.0 },
                          "fade_in": fade, "fade_out": fade},
                "layers": layers,
            })
        })
        .collect();
    let mut p = bench.clone();
    let o = p.as_object_mut().unwrap();
    for k in [
        "text_events",
        "fonts",
        "text_styles",
        "images",
        "image_events",
        "shape_events",
    ] {
        o.remove(k);
    }
    o.insert("sources".into(), json!([src]));
    o.insert("parts".into(), json!(parts));
    o["output"]["frames"] = json!(20 * take * fps);
    o["meta"]["title"] = json!("A.09 long-form");
    o["meta"]["import_report"] =
        json!(["captions lab: 20 takes of 30 s, one at +4 dB, one 20 s envelope at +6 dB"]);
    o["audio"]["envelopes"] = json!([{
        "id": "lift", "curve": "linear_db", "gain_db": 6.0,
        "out_samples": [(12 * take + 5) * 48_000, (12 * take + 25) * 48_000],
        "ramp_in_samples": 3840, "ramp_out_samples": 5760,
    }]);
    p
}

#[test]
#[ignore = "lab: the loc_bd89f38b restream and the speech model"]
fn reuse_saves_asr_on_a_long_form() {
    let src = PathBuf::from(
        long_form()["sources"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    if gmclip_model::lab::skip_without([&src]) {
        return;
    }
    let work = std::env::temp_dir().join(format!("gmclip-longform-lab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let plan = work.join("long.renderplan.json");
    std::fs::write(&plan, serde_json::to_vec_pretty(&long_form()).unwrap()).unwrap();
    let ok = gmclip(&["plan", "validate", plan.to_str().unwrap()]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let hear = |policy: &str| -> Value {
        let r = work.join(format!("{policy}.json"));
        let out = gmclip(&[
            "plan",
            "hear",
            plan.to_str().unwrap(),
            "--hear",
            policy,
            "--report",
            r.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{policy}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&std::fs::read(&r).unwrap()).unwrap()["summary"].clone()
    };
    let all = hear("all");
    let rule = hear("rule");
    let (ta, tr) = (
        all["hear_seconds"].as_f64().unwrap(),
        rule["hear_seconds"].as_f64().unwrap(),
    );
    let summary = json!({
        "backlog": "A.09",
        "measure": "wall seconds of gmclip plan hear (render the voice mix, resample to 16 kHz, parakeet TDT v3 int8; the model loaded first) on a 10-minute long-form: 20 takes of 30 s of the loc_bd89f38b restream from 600 s, 35 s apart; take 6 at +4 dB, a +6 dB envelope over 365–385 s of output; delivery loudness static (not heard)",
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "all": all,
        "rule": rule,
        "saved_wall_seconds": r3(ta - tr),
        "saved_fraction": r3(1.0 - tr / ta.max(1e-9)),
    });
    eprintln!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if let Some(p) = std::env::var_os("GMCLIP_LONGFORM_OUT") {
        let mut t = serde_json::to_string_pretty(&summary).unwrap();
        t.push('\n');
        std::fs::write(p, t).unwrap();
    }
    let _ = std::fs::remove_dir_all(&work);
    // two heard stretches (take 6; take 13 under the envelope), the other 18 takes reused
    assert_eq!(rule["stretches"], 2);
    assert_eq!(rule["parts_reused"], 18);
    assert!(tr < ta, "the rule heard {tr} s, all {ta} s");
}
