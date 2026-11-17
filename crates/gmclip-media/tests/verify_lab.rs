//! B-15 lab: verify v0 on real files, and the dev-only join checks (R-EXP-2).
//!
//! 1. The remux probe (`h79-v4.export-audio.mp4`, B-06) against B-13's h79 v4 plan:
//!    container checks, and the audio as AVFoundation decodes it against our planned mix
//!    (B-11) — first-sample alignment, every discontinuous join's A/V offset, drift.
//! 2. `tools/join_asr.py` on the same render vs the planned mix (passes), and on a render
//!    with 250 ms muted right after one join (fails at that join only).
//! 3. The 48-join plan: 49 parts alternating two real 48 kHz sources (stand-ins for the
//!    corpus's (a′)/(b′), which W0-05 has not landed), through our whole audio path —
//!    graph, delivery loudness, AudioToolbox AAC fed from sample D, `Mp4Writer` — then
//!    decoded by AVFoundation and ffmpeg: offsets at all 48 joins and their slope.
//!
//! Needs ffmpeg, afconvert, swift, transcribe-anything and the lab media.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use gmclip_media::audio::{AudioPlan, loudness, wav};
use gmclip_media::verify::{self, Expect};

fn probe_dir() -> PathBuf {
    std::env::var_os("GMCLIP_PROBE_DIR").map_or_else(
        || {
            Path::new(&std::env::var("HOME").unwrap())
                .join("Development/gmclip-artifacts/remux-probe")
        },
        PathBuf::from,
    )
}

fn print_report(name: &str, r: &verify::VerifyReport) {
    for c in &r.checks {
        println!(
            "{name}: [{}] {} — {}",
            if c.ok { "ok" } else { "FAIL" },
            c.step,
            c.detail
        );
    }
    let worst = r.joins.iter().map(|j| j.lag_ms.abs()).fold(0f64, f64::max);
    println!(
        "{name}: {} joins, worst |offset| {worst:.3} ms, drift {:?} ms/join",
        r.joins.len(),
        r.drift_ms_per_join
    );
}

fn h79_plan_and_mix(dir: &Path) -> (gmclip_plan::Plan, PathBuf, Vec<f32>) {
    // the example with its lab paths resolved (the slicer reads its sources)
    let mut plan = gmclip_plan::parse(
        &std::fs::read(repo_root().join("schemas/examples/h79-v4.renderplan.json")).unwrap(),
    )
    .unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    std::fs::create_dir_all(dir).unwrap();
    let plan_path = dir.join("h79-v4.renderplan.json");
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
    tool(&[
        "plan_slices.py",
        plan_path.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    let mix = mix_from_slices(dir, &plan);
    let _ = std::fs::remove_dir_all(dir.join("slices"));
    (plan, plan_path, mix)
}

#[test]
#[ignore = "lab: needs the remux probe (run remux_lab first), ffmpeg, swift and the lab media"]
fn verify_on_the_remux_probe() {
    let dir = lab_dir("verify-probe");
    let file_path = probe_dir().join("h79-v4.export-audio.mp4");
    let file = std::fs::read(&file_path).expect("run remux_lab first");
    let (plan, _, mix) = h79_plan_and_mix(&dir);
    let e = Expect::from_plan(&plan);
    let ap = AudioPlan::from_plan(&plan).unwrap();
    let pcm = dir.join("avf.f32");
    avf_probe(&file_path, Some(&pcm));
    let decoded = read_f32(&pcm);
    let r = verify::verify(&file, &e, Some((&decoded, &mix, &ap)));
    print_report("h79 probe", &r);
    // The probe is RT's picture re-wrapped with export-made audio (B-06): RT never tagged
    // colour, so that one check fails, and it alone.
    let failing: Vec<&str> = r.checks.iter().filter(|c| !c.ok).map(|c| c.step).collect();
    assert_eq!(failing, ["colour"], "{:?}", r.checks);
    assert!(
        r.joins.iter().all(|j| j.lag_samples == 0),
        "every join at 0 samples"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn join_asr(plan: &Path, render: &Path, reference: &Path, out: &Path) -> (bool, serde_json::Value) {
    let st = Command::new("python3")
        .arg(repo_root().join("tools/join_asr.py"))
        .args([
            "--plan",
            plan.to_str().unwrap(),
            "--render",
            render.to_str().unwrap(),
        ])
        .args([
            "--reference",
            reference.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    (
        st.success(),
        serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap(),
    )
}

#[test]
#[ignore = "lab: needs the remux probe, ffmpeg, transcribe-anything and the lab media"]
fn join_asr_passes_a_clean_render_and_catches_a_clipped_join() {
    // R-EXP-2's roles: the reference is RT's MP4, the render is gmclip's audio (the
    // planned mix, until H-10 renders).
    let dir = lab_dir("join-asr");
    let (plan, plan_path, mix) = h79_plan_and_mix(&dir);
    let reference = gmclip_model::lab::media("h79/h79-v4.mp4");
    let reference = reference.as_path();
    let render = dir.join("planned.wav");
    wav::write_f32(std::fs::File::create(&render).unwrap(), 48_000, 2, &mix).unwrap();
    let (ok, rep) = join_asr(&plan_path, &render, reference, &dir.join("clean.json"));
    println!("clean: {}", serde_json::to_string(&rep["joins"]).unwrap());
    assert!(ok, "the planned mix vs RT's MP4: {rep}");
    let ap = AudioPlan::from_plan(&plan).unwrap();
    let j = ap.joins().into_iter().find(|j| !j.continuous).unwrap();
    let first_id = plan.parts[j.index].id.as_str().to_owned();
    // Mutant 1: 250 ms muted just after the first discontinuous join (02-claim, 1.833 s,
    // where "we were able…" starts): that join fails, and only it.
    let at = (j.at as usize + 960) * 2;
    let mut muted = mix.clone();
    muted[at..at + 2 * 12_000].iter_mut().for_each(|s| *s = 0.0);
    let bad = dir.join("muted.wav");
    wav::write_f32(std::fs::File::create(&bad).unwrap(), 48_000, 2, &muted).unwrap();
    let (ok, rep) = join_asr(&plan_path, &bad, reference, &dir.join("muted.json"));
    println!("muted: {}", serde_json::to_string(&rep["joins"]).unwrap());
    assert!(!ok);
    let failing: Vec<String> = rep["joins"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["ok"] == false)
        .map(|r| r["part"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        failing,
        std::slice::from_ref(&first_id),
        "only the muted join"
    );
    // Mutant 2 (SPEC G6's): an 80 ms clip into the word at that join (80 ms removed, the
    // rest pulled earlier). Recorded, not asserted: ASR may still hear a clipped word.
    let mut clipped = mix.clone();
    clipped.drain(at..at + 2 * 3840);
    clipped.resize(mix.len(), 0.0);
    let bad2 = dir.join("clipped80.wav");
    wav::write_f32(std::fs::File::create(&bad2).unwrap(), 48_000, 2, &clipped).unwrap();
    let (ok2, rep2) = join_asr(&plan_path, &bad2, reference, &dir.join("clipped80.json"));
    let first = rep2["joins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["part"] == first_id.as_str())
        .unwrap()
        .clone();
    println!("80 ms clip: tool ok = {ok2}; at {first_id}: {first}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 49 parts × 15 frames alternating two sources (speech regions of two streams'
/// proxies), every join discontinuous.
fn forty_eight_join_plan() -> serde_json::Value {
    let a = gmclip_model::lab::media_str("preview/loc_4fadb756.proxy.mp4");
    let b = gmclip_model::lab::media_str("preview/loc_086da141.proxy.mp4");
    let parts: Vec<_> = (0..49u64)
        .map(|i| {
            let (src, base) = if i % 2 == 0 {
                ("a", 2_615u64 * 48_000)
            } else {
                ("b", 600u64 * 48_000)
            };
            serde_json::json!({ "id": format!("p{i:02}"), "out": [i * 15, i * 15 + 15],
                "picture": { "source": src, "first_frame": 0 }, "layers": [],
                "audio": { "source": src, "first_sample": base + i * 3 * 48_000, "rate": 48000,
                           "fade_in": { "samples": 192, "shape": "raised_cosine" },
                           "fade_out": { "samples": 192, "shape": "raised_cosine" } } })
        })
        .collect();
    serde_json::json!({ "schema": "gmclip.renderplan/0",
        "meta": { "title": "48-join drift plan (B-15)" },
        "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 49 * 15,
                    "video": { "codec": "h264", "profile": "high", "level": "4.0" },
                    "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
        "background": { "kind": "colour", "rgb": "#000000" },
        "sources": [ { "id": "a", "path": a }, { "id": "b", "path": b } ],
        "parts": parts,
        "audio": { "loudness": { "mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5 } } })
}

#[test]
#[ignore = "lab: needs ffmpeg, afconvert, swift and the lab media's proxies"]
fn forty_eight_joins_no_drift() {
    let dir = lab_dir("join48");
    let plan_json = forty_eight_join_plan();
    let plan_path = dir.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan_json).unwrap()).unwrap();
    let plan = gmclip_plan::parse(&std::fs::read(&plan_path).unwrap()).expect("valid");
    let ap = AudioPlan::from_plan(&plan).unwrap();
    assert_eq!(ap.joins().iter().filter(|j| !j.continuous).count(), 48);
    tool(&[
        "plan_slices.py",
        plan_path.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    let mix = mix_from_slices(&dir, &plan);
    let _ = std::fs::remove_dir_all(dir.join("slices"));
    // Delivery loudness (decision 0004) with AudioToolbox's AAC, then the export's encode.
    let (final_pcm, d) =
        loudness::normalize_for_delivery(&mix, 2, 48_000, -16.0, -1.5, |x: &[f32]| {
            aac_round_trip(&dir, x)
        })
        .unwrap();
    println!(
        "48-join: delivered I {:.2} LUFS, TP {:.2} dBTP (re-encoded: {})",
        d.delivered_integrated_lufs, d.delivered_tp_dbtp, d.reencoded
    );
    let n = final_pcm.len() / 2;
    let m4a = aac_encode_cut(&dir, &final_pcm);
    let video = dir.join("v.mp4");
    tool(&[
        "mux_fixtures.py",
        "video",
        video.to_str().unwrap(),
        &format!("{}", 49.0 * 15.0 / 30.0),
    ]);
    let out = dir.join("join48.mp4");
    rewrap(&video, &m4a, n as u64, &out);
    let file = std::fs::read(&out).unwrap();
    let e = Expect::from_plan(&plan);
    for (decoder, decoded) in [
        ("AVFoundation", {
            let p = dir.join("avf.f32");
            avf_probe(&out, Some(&p));
            read_f32(&p)
        }),
        ("ffmpeg", {
            let p = dir.join("ff.f32");
            let st = Command::new(FFMPEG)
                .args(["-v", "error", "-y", "-i"])
                .arg(&out)
                .args(["-map", "0:a", "-f", "f32le"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            read_f32(&p)
        }),
    ] {
        let r = verify::verify(
            &file,
            &e,
            Some((&decoded[..(2 * n).min(decoded.len())], &mix, &ap)),
        );
        print_report(&format!("48-join / {decoder}"), &r);
        let lags: Vec<i64> = r.joins.iter().map(|j| j.lag_samples).collect();
        println!("48-join / {decoder}: lags {lags:?}");
        for c in &r.checks {
            assert!(c.ok, "{decoder}: {}: {}", c.step, c.detail);
        }
        assert_eq!(r.joins.len(), 48);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
