//! B06 lab: Silero VAD on the loc_4fadb756 proxy's voice track — speech ratio, speed,
//! and the probabilities against Silero's own framing run in Python onnxruntime on the
//! same PCM: set `GMCLIP_SILERO_DUMP=<file.f32>` to write the first 30 s the model saw,
//! run `tools/silero_ref.py <model> <file.f32> 30 <ref.json>`, then rerun with
//! `GMCLIP_SILERO_REF=<ref.json>`.
//!
//! `cargo test -p gmclip-ml --release --features lab --test lab_vad -- --nocapture`
#![cfg(feature = "lab")]

mod common;

use std::time::Instant;

use gmclip_ml::CancelToken;
use gmclip_ml::models::{ModelCache, SILERO_VAD_V6_2_1};
use gmclip_ml::runtime::OrtRuntime;
use gmclip_ml::vad::{SileroVad, VadParams};

#[test]
fn vad_on_the_loc_4fadb756_proxy() {
    let cache = ModelCache::default_cache().unwrap();
    let rt = OrtRuntime::init(&cache).unwrap();
    let mut vad = SileroVad::load(rt, &cache.verify(&SILERO_VAD_V6_2_1).unwrap()).unwrap();
    let secs: Option<f64> = std::env::var("GMCLIP_LAB_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok());
    let pcm = common::voice_16k(&common::proxy("loc_4fadb756"), 0.0, secs);
    if let Ok(p) = std::env::var("GMCLIP_SILERO_DUMP") {
        let bytes: Vec<u8> = pcm[..480_000.min(pcm.len())]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        std::fs::write(p, bytes).unwrap();
    }
    let t = Instant::now();
    let r = vad
        .detect(&pcm, &VadParams::default(), &CancelToken::default())
        .unwrap();
    let wall = t.elapsed().as_secs_f64();
    let audio = pcm.len() as f64 / 16_000.0;
    println!(
        "{audio:.0} s: {} speech regions, speech ratio {:.1} %, {:.0}× real time",
        r.regions.len(),
        100.0 * r.speech_ratio(),
        audio / wall
    );
    assert!(r.speech_ratio() > 0.3 && r.speech_ratio() < 0.98);
    assert!(r.regions.windows(2).all(|w| w[0].end < w[1].start));
    let blob = r.to_blob(&VadParams::default()).encode().unwrap();
    println!("vad blob {} kB", blob.len() / 1000);
    if let Ok(p) = std::env::var("GMCLIP_SILERO_REF") {
        let reference: Vec<f32> = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        let n = reference.len().min(r.probs.len());
        let worst = (0..n)
            .map(|i| (reference[i] - r.probs[i]).abs())
            .fold(0f32, f32::max);
        println!("max |Δprob| vs Python onnxruntime over {n} chunks: {worst:.2e}");
        // same PCM, same framing; ORT 1.30 (here) vs the Python wheel's 1.24 differ in
        // the last digits (measured 1.9e-3); a framing bug shows as ~0.1–0.5
        assert!(worst < 5e-3, "{worst}");
    }
}
