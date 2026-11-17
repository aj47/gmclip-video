//! Lab helpers: decode with the dev-only ffmpeg (never linked; SPEC §15.4), load the
//! pinned models from the cache, read AJ's transcripts from the lab media folder (read-only).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use gmclip_ml::models::{ModelCache, PARAKEET_TDT_V3_INT8};
use gmclip_ml::parakeet::{ParakeetAsr, ParakeetOptions};
use gmclip_ml::runtime::OrtRuntime;
use gmclip_ml::wer::TimedWord;

pub const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
/// The lab media folder (`GMCLIP_LAB_MEDIA`).
pub fn lab() -> std::path::PathBuf {
    gmclip_model::lab::root()
}

pub fn proxy(vid: &str) -> PathBuf {
    lab().join(format!("preview/{vid}.proxy.mp4"))
}

/// The source's first audio track, `[start, start + dur)` s, decoded by ffmpeg at its
/// native 48 kHz stereo, then down-mixed and resampled by gmclip-analysis.
pub fn voice_16k(src: &Path, start: f64, dur: Option<f64>) -> Vec<f32> {
    let mut c = Command::new(FFMPEG);
    c.args(["-v", "error", "-nostdin"]);
    if start > 0.0 {
        c.args(["-ss", &format!("{start:.6}")]);
    }
    if let Some(d) = dur {
        c.args(["-t", &format!("{d:.6}")]);
    }
    c.arg("-i").arg(src);
    c.args([
        "-map", "0:a:0", "-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "-",
    ]);
    let out = c.output().expect("ffmpeg");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pcm: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    gmclip_analysis::pcm::voice_16k(&pcm, 48_000, 2).expect("resample")
}

pub fn asr(opts: ParakeetOptions) -> ParakeetAsr {
    let cache = ModelCache::default_cache().expect("cache");
    let rt = OrtRuntime::init(&cache).expect("ORT (run tools/fetch_models.sh)");
    let model = cache
        .verify(&PARAKEET_TDT_V3_INT8)
        .expect("Parakeet (run tools/fetch_models.sh)");
    ParakeetAsr::load(rt, &model, opts).expect("load")
}

pub fn aligner() -> gmclip_ml::tdt::TdtAligner {
    use gmclip_ml::models::PARAKEET_ALIGNER;
    let cache = ModelCache::default_cache().expect("cache");
    let rt = OrtRuntime::init(&cache).expect("ORT");
    let asr = cache.verify(&PARAKEET_TDT_V3_INT8).expect("Parakeet");
    let al = cache
        .verify(&PARAKEET_ALIGNER)
        .expect("aligner (run tools/fetch_models.sh)");
    gmclip_ml::tdt::TdtAligner::load(rt, &asr, &al, 8).expect("aligner")
}

/// Words of a `transcribe_anything.parakeet_coreml.v1` document, seconds.
pub fn coreml_words(path: &Path) -> Vec<TimedWord> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read transcript")).expect("json");
    let mut out = Vec::new();
    for s in v["sentences"].as_array().expect("sentences") {
        for w in s["words"].as_array().expect("words") {
            out.push(TimedWord {
                text: w["word"].as_str().unwrap_or_default().to_owned(),
                start: w["start"].as_f64().unwrap_or(0.0),
                end: w["end"].as_f64().unwrap_or(0.0),
            });
        }
    }
    out
}
