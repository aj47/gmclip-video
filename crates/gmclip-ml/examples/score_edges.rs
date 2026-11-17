//! R-ML-2 scorer: predicts every edge in the given marks files with gmclip's refinement
//! (audible edges + arbiter, `gmclip_ml::edgeset::predict`) and scores the marked ones
//! (median ≤ 40 ms, p95 ≤ 120 ms). With no marks yet it is a dry run: it reports how
//! each edge would be predicted.
//!
//! `cargo run -p gmclip-ml --release --features lab --example score_edges -- <marks-*.json>…`
#[path = "../tests/common/mod.rs"]
mod common;

use std::path::Path;

use gmclip_ml::CancelToken;
use gmclip_ml::arbiter::ArbiterParams;
use gmclip_ml::edgeset::{MarkFile, predict, score};
use gmclip_ml::parakeet::ParakeetOptions;

/// Audio either side of the clip given to ASR: Parakeet drops words within ~1 s of a
/// window edge, so the words of interest sit well inside the decoded window.
const PAD_S: f64 = 6.0;

fn main() {
    let asr = common::asr(ParakeetOptions::default());
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse().ok());
    let tdt = common::aligner();
    let mut params = ArbiterParams::default();
    if let Some(c) = env("GMCLIP_ARB_CONTEXT_MS") {
        params.context_ms = c;
    }
    if let Some(n) = env("GMCLIP_ARB_CANDIDATES") {
        params.candidates = n as usize;
    }
    println!("arbiter {params:?}");
    let mut marks = Vec::new();
    let mut preds = Vec::new();
    for f in std::env::args().skip(1) {
        let file = MarkFile::parse(&std::fs::read(&f).expect("read marks")).expect("marks file");
        let src = Path::new(&file.audio_path);
        if std::env::var_os("GMCLIP_SKIP_HASH").is_none() {
            let (h, _) = gmclip_ml::sha256::file_hex(src).expect("audio");
            assert_eq!(
                h, file.audio_sha256,
                "{f}: the audio changed since the marks were made"
            );
        }
        for it in &file.items {
            let start = (it.clip_start - PAD_S).max(0.0);
            let pcm = common::voice_16k(src, start, Some(it.clip_end + PAD_S - start));
            let p = predict(
                &asr,
                &tdt,
                &pcm,
                start,
                it,
                &params,
                &CancelToken::default(),
            )
            .expect("predict");
            let err = match (p.at, it.mark) {
                (Some(a), Some(m)) => format!("{:+6.0} ms", 1000.0 * (a - m)),
                _ => String::new(),
            };
            println!(
                "{:<22} {:<8} {:>14} | {:<14} {:?} {} {err} {}",
                it.id,
                format!("{:?}", it.kind),
                it.w1.text,
                it.w2.text,
                p.method,
                p.at.map(|a| format!("{a:.3}")).unwrap_or_default(),
                p.detail
            );
            preds.push(p);
        }
        marks.extend(file.items);
    }
    let s = score(&marks, &preds);
    let mut methods = std::collections::BTreeMap::<String, usize>::new();
    for p in &preds {
        *methods
            .entry(format!("{:?}/{:?}", p.kind, p.method))
            .or_default() += 1;
    }
    println!("predictions: {methods:?}");
    println!("{}", serde_json::to_string_pretty(&s).unwrap());
    if s.marked == 0 {
        println!("no marks yet: dry run only (R-ML-2 needs AJ's 300 marks)");
    }
}
