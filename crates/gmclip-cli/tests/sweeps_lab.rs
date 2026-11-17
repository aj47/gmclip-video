//! D.03 lab (SPEC §5.5): the sweeps' acceptance numbers on the 10 golden reels
//! (`reels/*` in the lab media folder). Every part's span on its stream (the plan's `a`–`z`, native
//! seconds) goes through gmclip's pause sweep (`CliAnalysis::pauses`: 16 kHz voice, the
//! per-span threshold, the 40 ms quiet grid, words protected over their audible extent,
//! edges on the voice and 0.10 s kept each side inside (decision 0049), < 0.12 s dropped) with the stream's own words (a
//! parakeet pass, `asr/<vid>/out.json` in the lab media folder), and the same decoded samples and
//! words through `tools/pauses_ref.py`, an independent plain-Python reading of the rule:
//! every fragment must match within 10 ms. Fillers: gmclip's filler sweep over each cut's
//! stretch of stream against the ASR's own filler events (a proxy: there are no hand labels;
//! recall/precision against hand labels on 2 streams waits for AJ's labelling, A.10).
//!
//! Reads lab media (GMCLIP_LAB_MEDIA); writes only a temp dir (and `GMCLIP_SWEEPS_OUT`, the report).
//! `cargo test -p gmclip-cli --test sweeps_lab -- --ignored --nocapture`
#![cfg(target_os = "macos")]
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "../src/analysis.rs"]
mod analysis;

use std::path::{Path, PathBuf};
use std::process::Command;

use analysis::{CliAnalysis, track_16k};
use gmclip_core::host::{AnalysisHost, AudioSource, HeardWord};
use gmclip_model::Flicks;
use serde_json::{Value, json};

const SEC: f64 = 705_600_000.0;
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
/// §5.5's defaults.
const MIN_S: f64 = 0.32;
const KEEP_S: f64 = 0.10;
const TOLERANCE_S: f64 = 0.010;

fn f(s: f64) -> Flicks {
    Flicks((s * SEC).round() as i64)
}

fn secs(t: Flicks) -> f64 {
    t.0 as f64 / SEC
}

/// The 48 kHz audio of a stream, in the lab media folder.
fn audio_of(vid: &str) -> PathBuf {
    gmclip_model::lab::media(match vid {
        "loc_bd89f38b" => {
            "streams/i\u{2019}m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4"
                .to_owned()
        }
        v => format!("preview/{v}.proxy.mp4"),
    })
}

/// A stream's words and the ASR's filler events (s).
type Words = (Vec<HeardWord>, Vec<(f64, f64)>);

/// A stream's words and the ASR's filler events, from its parakeet pass.
fn words_of(vid: &str) -> Words {
    let v: Value = serde_json::from_slice(
        &std::fs::read(gmclip_model::lab::media(format!("asr/{vid}/out.json"))).unwrap(),
    )
    .unwrap();
    let words = v["sentences"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["words"].as_array().unwrap().iter())
        .map(|w| HeardWord {
            text: w["word"].as_str().unwrap().to_owned(),
            start: f(w["start"].as_f64().unwrap()),
            end: f(w["end"].as_f64().unwrap()),
            confidence: None,
        })
        .collect();
    let fillers = v["filler_events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| (e["start"].as_f64().unwrap(), e["end"].as_f64().unwrap()))
        .collect();
    (words, fillers)
}

/// Pairs `a` and `b` (start order) whose edges are both within the tolerance.
fn matched(a: &[(f64, f64)], b: &[(f64, f64)]) -> (usize, f64) {
    let mut used = vec![false; b.len()];
    let (mut n, mut worst) = (0, 0.0f64);
    for x in a {
        if let Some(j) = (0..b.len()).find(|&j| {
            !used[j] && (x.0 - b[j].0).abs() <= TOLERANCE_S && (x.1 - b[j].1).abs() <= TOLERANCE_S
        }) {
            used[j] = true;
            n += 1;
            worst = worst.max((x.0 - b[j].0).abs()).max((x.1 - b[j].1).abs());
        }
    }
    (n, worst)
}

#[test]
#[ignore = "lab: lab media and words, /usr/bin/python3"]
fn pauses_match_the_reference_and_fillers_against_the_asr() {
    let host = CliAnalysis::new();
    let work = std::env::temp_dir().join(format!("gmclip-sweeps-lab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let (mut ours_all, mut ref_all, mut match_all) = (0usize, 0usize, 0usize);
    let mut worst_all = 0.0f64;
    let (mut f_events, mut f_cuts, mut f_hit_events, mut f_hit_cuts, mut f_skipped) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut rows = Vec::new();
    let mut words_cache: std::collections::HashMap<String, Words> = Default::default();
    let plan_of = |reel: &str| -> Value {
        serde_json::from_slice(
            &std::fs::read(gmclip_model::lab::media(format!("reels/{reel}/plan.json"))).unwrap(),
        )
        .unwrap()
    };
    // the cuts whose plan, stream audio and words are on this machine
    let set = gmclip_model::lab::present(
        REELS,
        |r| r.to_string(),
        |r| {
            let plan = gmclip_model::lab::media(format!("reels/{r}/plan.json"));
            if !plan.exists() {
                return vec![plan];
            }
            let vid = plan_of(r)["vid"].as_str().unwrap().to_owned();
            vec![
                plan,
                audio_of(&vid),
                gmclip_model::lab::media(format!("asr/{vid}/out.json")),
            ]
        },
    );
    if set.skip_if_empty() {
        return;
    }
    for reel in set.items.iter().copied() {
        let plan = plan_of(reel);
        let vid = plan["vid"].as_str().unwrap().to_owned();
        let audio = AudioSource {
            path: audio_of(&vid),
            track: None,
            fingerprint: format!("lab-{vid}"),
        };
        let (words, events) = words_cache
            .entry(vid.clone())
            .or_insert_with(|| words_of(&vid))
            .clone();
        let dir = work.join(reel);
        std::fs::create_dir_all(&dir).unwrap();
        let mut ref_parts = Vec::new();
        let mut ours: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
        // every part's span, and the stretch of stream the cut was made from (first part's
        // start to last part's end, in stream order): where the pauses RT cut out still are
        let mut tests: Vec<(String, f64, f64)> = plan["cuts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let (a, z) = (c["a"].as_f64().unwrap(), c["z"].as_f64().unwrap());
                (c["id"].as_str().unwrap().to_owned(), a, z)
            })
            .collect();
        let lo = tests.iter().map(|t| t.1).fold(f64::INFINITY, f64::min);
        let hi = tests.iter().map(|t| t.2).fold(0.0, f64::max);
        tests.push(("stretch".into(), lo, hi.min(lo + 300.0)));
        for (id, a, z) in tests {
            let got = host
                .pauses(&audio, &[(f(a), f(z))], f(MIN_S), f(KEEP_S), &words)
                .unwrap();
            ours.push((
                id.clone(),
                got.cuts
                    .iter()
                    .map(|s| (secs(s.from) - a, secs(s.to) - a))
                    .collect(),
            ));
            // the same samples and words for the reference
            let pcm = track_16k(&audio.path, None, f(a), f(z)).unwrap();
            let bytes: Vec<u8> = pcm.iter().flat_map(|x| x.to_le_bytes()).collect();
            std::fs::write(dir.join(format!("{id}.f32")), bytes).unwrap();
            let ws: Vec<Value> = words
                .iter()
                .filter(|w| secs(w.end) > a && secs(w.start) < z)
                .map(|w| json!([secs(w.start) - a, secs(w.end) - a]))
                .collect();
            ref_parts.push(json!({"id": id, "pcm": format!("{id}.f32"), "rate": 16000,
                                  "min_s": MIN_S, "keep_s": KEEP_S, "words": ws}));
        }
        std::fs::write(
            dir.join("parts.json"),
            serde_json::to_vec(&ref_parts).unwrap(),
        )
        .unwrap();
        let out = Command::new("/usr/bin/python3")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/pauses_ref.py"))
            .arg(dir.join("parts.json"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let r: Value = serde_json::from_slice(&out.stdout).unwrap();
        let (mut n_ours, mut n_ref, mut n_match, mut worst) = (0, 0, 0, 0.0f64);
        let mut misses = Vec::new();
        for (id, o) in &ours {
            let rf: Vec<(f64, f64)> = r[id]["cuts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| (x[0].as_f64().unwrap(), x[1].as_f64().unwrap()))
                .collect();
            let (m, w) = matched(o, &rf);
            if m != o.len() || m != rf.len() {
                misses.push(json!({"part": id, "ours": o, "reference": rf}));
            }
            n_ours += o.len();
            n_ref += rf.len();
            n_match += m;
            worst = worst.max(w);
        }
        // fillers over the stretch the cut was made from (its parts are mostly clean
        // already), against the ASR's own filler events there
        let spans = [(f(lo), f(hi.min(lo + 300.0)))];
        let fl = host.fillers(&audio, &spans, &words).unwrap();
        let cuts: Vec<(f64, f64)> = fl.cuts.iter().map(|c| (secs(c.from), secs(c.to))).collect();
        let ev: Vec<(f64, f64)> = events
            .iter()
            .copied()
            .filter(|(s, e)| spans.iter().any(|(a, z)| *s >= secs(*a) && *e <= secs(*z)))
            .collect();
        let overlaps = |x: &(f64, f64), y: &(f64, f64)| x.0 < y.1 && y.0 < x.1;
        f_events += ev.len();
        f_cuts += cuts.len();
        f_skipped += fl.skipped.len();
        f_hit_events += ev
            .iter()
            .filter(|e| cuts.iter().any(|c| overlaps(c, e)))
            .count();
        f_hit_cuts += cuts
            .iter()
            .filter(|c| ev.iter().any(|e| overlaps(c, e)))
            .count();
        rows.push(json!({"cut": reel, "parts": ours.len(), "pause_fragments": n_ours,
            "reference_fragments": n_ref, "matched_10ms": n_match, "max_delta_ms": (worst * 1e4).round() / 10.0,
            "mismatches": misses, "filler_cuts": cuts.len(), "filler_skipped": fl.skipped.len(), "asr_filler_events": ev.len()}));
        ours_all += n_ours;
        ref_all += n_ref;
        match_all += n_match;
        worst_all = worst_all.max(worst);
    }
    let ratio = |a: usize, b: usize| {
        if b == 0 {
            Value::Null
        } else {
            json!((a as f64 / b as f64 * 1000.0).round() / 1000.0)
        }
    };
    let report = json!({
        "schema": "gmclip.lab.sweeps/0",
        "backlog": "D.03",
        "measured": set.note("golden reels"),
        "params": {"min_s": MIN_S, "keep_s": KEEP_S, "tolerance_ms": TOLERANCE_S * 1000.0},
        "pauses": {"fragments": ours_all, "reference_fragments": ref_all, "matched_within_10ms": match_all,
                   "max_delta_ms": (worst_all * 1e4).round() / 10.0, "reference": "tools/pauses_ref.py"},
        "fillers_vs_asr_events_proxy": {"asr_events": f_events, "cuts": f_cuts, "skipped_fused": f_skipped,
            "recall": ratio(f_hit_events, f_events), "precision": ratio(f_hit_cuts, f_cuts),
            "note": "the parakeet pass's own filler events, not hand labels: recall/precision vs hand labels on 2 streams waits for AJ's labels (A.10)"},
        "rows": rows,
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(p) = std::env::var_os("GMCLIP_SWEEPS_OUT") {
        std::fs::write(p, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    }
    let _ = std::fs::remove_dir_all(&work);
    eprintln!("D.03 measured on {}", set.note("golden reels"));
    assert!(
        ours_all > 0,
        "no pause on {} cuts is not a measurement",
        set.items.len()
    );
    assert_eq!(
        (match_all, match_all),
        (ours_all, ref_all),
        "every fragment within 10 ms of the reference"
    );
}
