//! B-11 lab: the audio graph on the h79 and h34 plans against RT's own graph.
//!
//! `tools/golden_audio_ref.py` writes, per cut: the plan's audio fields (from the RT render
//! receipt), each part's source slice decoded by ffmpeg (standing in for Lane H's
//! decoders), and `ref.wav` = RT's audio graph without loudnorm run by ffmpeg over RT's
//! own per-part intermediates. Our graph mixes the slices; the result must match the
//! reference to 1e-4 (B-11), and every continuous join must be sample-continuous.
//! Needs ffmpeg and the golden cuts in the lab media folder. Run with `--run-ignored only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use common::*;
use gmclip_media::audio::{AudioPlan, PcmSource, wav};

/// The lab media folder (the golden cuts' `h79/` and `reels/`).
fn production() -> String {
    gmclip_model::lab::root().to_string_lossy().into_owned()
}

/// Part slices on disk, looked up by (source, first_sample). (Two parts reusing the very
/// same samples would share a key; no golden cut does.)
struct Slices {
    by_start: HashMap<(String, u64), PathBuf>,
}

impl Slices {
    fn new(dir: &Path, plan: &serde_json::Value) -> Self {
        Self::with_prefix(dir, plan, "")
    }

    /// `prefix = "rt-"` reads RT's own intermediates (decoded by the tool) instead.
    fn with_prefix(dir: &Path, plan: &serde_json::Value, prefix: &str) -> Self {
        let mut by_start = HashMap::new();
        for p in plan["parts"].as_array().unwrap() {
            let key = (
                p["audio"]["source"].as_str().unwrap().to_owned(),
                p["audio"]["first_sample"].as_u64().unwrap(),
            );
            by_start.insert(
                key,
                dir.join("slices")
                    .join(format!("{prefix}{}.wav", p["id"].as_str().unwrap())),
            );
        }
        Self { by_start }
    }
}

impl PcmSource for Slices {
    fn read(&mut self, source: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
        let path = self
            .by_start
            .get(&(source.to_owned(), first))
            .ok_or("no slice")?;
        let pcm = wav::read(path).map_err(|e| e.to_string())?;
        if pcm.rate != 48_000 || pcm.channels != 2 || pcm.samples.len() != out.len() {
            return Err(format!(
                "{}: {} samples, want {}",
                path.display(),
                pcm.samples.len(),
                out.len()
            ));
        }
        out.copy_from_slice(&pcm.samples);
        Ok(())
    }
}

/// Runs the tool for one cut and returns (dir, plan JSON).
pub fn prepare(name: &str, receipt: &str, with_ref: bool) -> (PathBuf, serde_json::Value) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("rt-audio")
        .join(name);
    let receipt = format!("{}/{receipt}", production());
    let mut args = vec![
        "golden_audio_ref.py",
        receipt.as_str(),
        dir.to_str().unwrap(),
    ];
    if !with_ref {
        args.push("--no-ref");
    }
    tool(&args);
    let plan =
        serde_json::from_slice(&std::fs::read(dir.join("plan-audio.json")).unwrap()).unwrap();
    (dir, plan)
}

fn check(name: &str, receipt: &str) {
    let (dir, plan_json) = prepare(name, receipt, true);
    let typed = gmclip_plan::parse(&std::fs::read(dir.join("plan-audio.json")).unwrap())
        .expect("a valid RenderPlan v0");
    let plan = AudioPlan::from_plan(&typed).unwrap();
    let ours = plan.mix(&mut Slices::new(&dir, &plan_json)).unwrap();
    let reference = wav::read(&dir.join("ref.wav")).unwrap();
    assert_eq!(ours.len(), reference.samples.len(), "{name}: length");
    let (mut max, mut at) = (0f32, 0usize);
    for (i, (a, b)) in ours.iter().zip(&reference.samples).enumerate() {
        let d = (a - b).abs();
        if d > max {
            (max, at) = (d, i);
        }
    }
    // Our slices vs RT's own intermediates: the plan addresses the samples RT rendered.
    let sv: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("slices-vs-rt.json")).unwrap()).unwrap();
    let slice_max = sv
        .as_object()
        .unwrap()
        .values()
        .map(|v| v["max_abs_diff"].as_f64().unwrap())
        .fold(0f64, f64::max);
    let joins = plan.joins();
    let continuous: Vec<_> = joins.iter().filter(|j| j.continuous).collect();
    println!(
        "{name}: {} samples, {} parts, {} joins ({} continuous), {} envelopes; max |ours − RT ref| = {max:.3e} at frame {}; max |slice − RT intermediate| = {slice_max:.3e}",
        ours.len() / 2,
        plan.parts.len(),
        joins.len(),
        continuous.len(),
        plan.envelopes.len(),
        at / 2
    );
    assert!(
        slice_max <= 2.5e-7,
        "{name}: slices differ from RT's intermediates by {slice_max}"
    );
    // The graph alone: fed RT's own intermediates, it reproduces ffmpeg's graph exactly.
    let same_input = plan
        .mix(&mut Slices::with_prefix(&dir, &plan_json, "rt-"))
        .unwrap();
    let dsp_max = same_input
        .iter()
        .zip(&reference.samples)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    println!("{name}: graph fed RT's intermediates: max |ours − RT ref| = {dsp_max:.3e}");
    assert!(
        dsp_max <= 1e-6,
        "{name}: DSP differs from ffmpeg's graph by {dsp_max}"
    );
    assert!(
        max <= 1e-4,
        "{name}: max abs difference {max} at sample {at}"
    );

    // Continuous joins: the mix across the join equals ONE decode of the source span
    // (times the envelope gain, applied exactly as the graph does): no fade, no step, no
    // lost or doubled sample.
    for j in continuous {
        let part = &plan.parts[j.index];
        let span = wav::read(&dir.join("slices").join(format!("join-{}.wav", part.id)))
            .unwrap()
            .samples;
        let half = span.len() / 4; // frames either side
        let mut worst = 0usize;
        for k in 0..2 * half {
            let n = j.at as usize - half + k;
            let in_env = plan
                .envelopes
                .iter()
                .any(|e| (e.out[0]..e.out[1]).contains(&(n as u64)));
            let g = 10f64.powf(plan.envelope_db(n as u64) / 20.0);
            for ch in 0..2 {
                let x = span[2 * k + ch];
                let want = if in_env { (f64::from(x) * g) as f32 } else { x };
                if ours[2 * n + ch] != want {
                    worst += 1;
                }
            }
        }
        println!(
            "  continuous join at {} ({}): ±{half} samples bit-identical to one decode: {}",
            j.at,
            part.id,
            worst == 0
        );
        assert_eq!(
            worst, 0,
            "{name}: continuous join at {} differs from a single decode",
            j.at
        );
    }
}

#[test]
#[ignore = "lab: needs ffmpeg and the golden cuts in the lab media folder"]
fn h79_graph_matches_rt() {
    check("h79", "h79/render-receipt.json");
}

#[test]
#[ignore = "lab: needs ffmpeg and the golden cuts in the lab media folder"]
fn h34_graph_matches_rt() {
    check("h34", "reels/loc_4fadb756_h34/render-receipt.json");
}

/// One decoded source in memory.
struct Whole(Vec<f32>);

impl PcmSource for Whole {
    fn read(&mut self, _: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
        let a = first as usize * 2;
        out.copy_from_slice(self.0.get(a..a + out.len()).ok_or("past the end")?);
        Ok(())
    }
}

/// The paths RT never exercises (every golden part has gain 0 and linear fades):
/// raised-cosine fades, non-zero part gains and two envelopes, against ffmpeg's
/// `afade curve=hsin`, `volume` and the same `aeval` expression.
#[test]
#[ignore = "lab: needs ffmpeg"]
fn raised_cosine_gain_and_envelopes_match_ffmpeg() {
    let dir = lab_dir("graph-synthetic");
    // Deterministic noise, 5 s stereo.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let noise: Vec<f32> = (0..5 * 48_000 * 2)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.8
        })
        .collect();
    let src = dir.join("noise.wav");
    wav::write_f32(std::fs::File::create(&src).unwrap(), 48_000, 2, &noise).unwrap();
    let hsin = serde_json::json!({ "samples": 192, "shape": "raised_cosine" });
    // (out frames, first sample, gain dB)
    let parts = [
        ([0u64, 30u64], 1000u64, 0.0f64),
        ([30, 60], 100_000, -6.0),
        ([60, 90], 150_001, 3.7),
    ];
    let plan_json = serde_json::json!({
        "schema": "gmclip.renderplan/0",
        "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 90,
                    "video": { "codec": "h264" }, "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
        "background": { "kind": "colour", "rgb": "#000000" },
        "sources": [ { "id": "noise", "path": src.to_str().unwrap() } ],
        "parts": parts.iter().map(|(out, first, gain)| serde_json::json!({
            "id": format!("p{}", out[0]), "out": out, "picture": { "source": "noise", "first_frame": 0 }, "layers": [],
            "audio": { "source": "noise", "first_sample": first, "rate": 48000, "gain_db": gain, "fade_in": hsin, "fade_out": hsin } })).collect::<Vec<_>>(),
        "audio": { "loudness": { "mode": "as_recorded" }, "envelopes": [
            { "id": "up", "out_samples": [10_000, 40_000], "gain_db": 5, "ramp_in_samples": 960, "ramp_out_samples": 1920, "curve": "linear_db" },
            { "id": "down", "out_samples": [100_000, 130_000], "gain_db": -8, "ramp_in_samples": 480, "ramp_out_samples": 480, "curve": "linear_db" } ] }
    });
    let typed = gmclip_plan::parse(&serde_json::to_vec(&plan_json).unwrap()).unwrap();
    let plan = AudioPlan::from_plan(&typed).unwrap();
    let ours = plan.mix(&mut Whole(noise)).unwrap();

    let mut chains = Vec::new();
    for (i, (out, first, gain)) in parts.iter().enumerate() {
        let n = (out[1] - out[0]) * 1600;
        chains.push(format!(
            "[0:a]atrim=start_sample={first}:end_sample={},asetpts=N/SR/TB,aformat=sample_fmts=fltp,volume={gain:.6}dB,\
             afade=t=in:ss=0:ns=192:curve=hsin,afade=t=out:ss={}:ns=192:curve=hsin[a{i}]",
            first + n,
            n - 192
        ));
    }
    let w = |s0: u64, s1: u64, ri: u64, ro: u64| {
        format!("max(0,min(1,min((n-{s0})/{ri},({s1}-n)/{ro})))")
    };
    let m = format!(
        "pow(10,((5*{})+(-8*{}))/20)",
        w(10_000, 40_000, 960, 1920),
        w(100_000, 130_000, 480, 480)
    );
    chains.push(format!(
        "[a0][a1][a2]concat=n=3:v=0:a=1,aeval=exprs='val(0)*{m}|val(1)*{m}':channel_layout=stereo,aformat=sample_fmts=flt[out]"
    ));
    let reference = dir.join("ref.wav");
    let st = std::process::Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args([
            "-filter_complex",
            &chains.join(";"),
            "-map",
            "[out]",
            "-c:a",
            "pcm_f32le",
        ])
        .arg(&reference)
        .status()
        .unwrap();
    assert!(st.success());
    let r = wav::read(&reference).unwrap().samples;
    assert_eq!(r.len(), ours.len());
    let max = ours
        .iter()
        .zip(&r)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    println!("synthetic hsin/gain/envelopes: max |ours − ffmpeg| = {max:.3e}");
    assert!(max <= 1e-6, "{max}");
}
