//! B04 lab acceptance: Parakeet via parakeet-rs on the three h79 windows vs Hook
//! Ledger's CoreML Parakeet (`asr-2585/2600/2703.json`): WER ≤ 5 %, median word-time
//! delta ≤ 60 ms; throughput and the EP report are printed (S5 input).
//!
//! WER is gated per window; the time delta is gated pooled over the three windows.
//! Both decoders quantise to Parakeet's 80 ms encoder frame and disagree by exactly one
//! frame on many words (CoreML fp16 vs ORT int8), so a single short window can sit at
//! a median of 80 ms (2703: 46 words) while the pool is at 0 ms.
//!
//! `cargo test -p gmclip-ml --release --features lab --test lab_asr -- --nocapture`
#![cfg(feature = "lab")]

mod common;

use std::time::Instant;

use gmclip_analysis::span::flicks_to_samples;
use gmclip_ml::parakeet::ParakeetOptions;
use gmclip_ml::wer::{TimedWord, align, normalised};
use gmclip_ml::{Asr, CancelToken};
use gmclip_model::Flicks;

fn secs(t: Flicks) -> f64 {
    t.0 as f64 / 705_600_000.0
}

#[test]
fn h79_windows_match_the_reference() {
    let asr = common::asr(ParakeetOptions::default());
    println!("asr_revision {}", asr.revision());
    let src = common::proxy("loc_4fadb756");
    let dir = common::lab().join("h79");
    let mut total_audio = 0.0;
    let mut total_wall = 0.0;
    let mut pooled: Vec<f64> = Vec::new();
    let (mut errs, mut refs) = (0usize, 0usize);
    for (file, start, dur) in [
        ("asr-2585.json", 2585.0, 30.0),
        ("asr-2600-proxy.json", 2600.0, 108.0),
        ("asr-2703.json", 2703.0, 27.0),
    ] {
        let pcm = common::voice_16k(&src, start, Some(dur));
        let t = Instant::now();
        let words = asr
            .transcribe(&pcm, Flicks::ZERO, &CancelToken::default())
            .unwrap();
        let wall = t.elapsed().as_secs_f64();
        total_audio += dur;
        total_wall += wall;
        let hyp: Vec<TimedWord> = words
            .iter()
            .map(|w| TimedWord {
                text: w.text.clone(),
                start: secs(w.start),
                end: secs(w.end),
            })
            .collect();
        let reference = common::coreml_words(&dir.join(file));
        let a = align(&normalised(&reference), &normalised(&hyp));
        println!(
            "{file}: ref {} hyp {} words, WER {:.2} % (S {} D {} I {}), agreement {:.1} %, median |Δstart| {:.0} ms, median |Δend| {:.0} ms, {:.1}× real time",
            a.reference_words,
            a.hypothesis_words,
            100.0 * a.wer(),
            a.substitutions,
            a.deletions,
            a.insertions,
            100.0 * a.agreement(),
            1000.0 * a.median_start_delta().unwrap_or(f64::NAN),
            1000.0 * a.median_end_delta().unwrap_or(f64::NAN),
            dur / wall
        );
        assert!(a.wer() <= 0.05, "{file}: WER {:.3}", a.wer());
        pooled.extend(&a.start_deltas);
        errs += a.substitutions + a.deletions + a.insertions;
        refs += a.reference_words;
        // sanity: the last word ends inside the window
        let last = words.last().unwrap();
        assert!(flicks_to_samples(last.end, 16_000) as usize <= pcm.len());
    }
    pooled.sort_by(f64::total_cmp);
    let med = gmclip_ml::wer::quantile(&pooled, 0.5).unwrap();
    let within_80 = pooled.iter().filter(|d| **d <= 0.0801).count() as f64 / pooled.len() as f64;
    println!(
        "pooled: WER {:.2} % over {refs} words, median |Δstart| {:.0} ms, p90 {:.0} ms, {:.1} % within one frame",
        100.0 * errs as f64 / refs as f64,
        1000.0 * med,
        1000.0 * gmclip_ml::wer::quantile(&pooled, 0.9).unwrap(),
        100.0 * within_80
    );
    assert!(med <= 0.060, "pooled median start delta {med}");
    println!(
        "throughput {:.1}× real time over {total_audio:.0} s",
        total_audio / total_wall
    );
    for l in asr.execution_report().lines {
        println!("EP: {l}");
    }
}

/// R-ML-1 / S5 input: a whole stream's voice track, agreement with the reference
/// whole-stream CoreML transcript, throughput and the EP report.
/// `GMCLIP_LAB_VID` (default loc_4fadb756), `GMCLIP_LAB_SECONDS` (default: all).
/// `cargo test -p gmclip-ml --release --features lab --test lab_asr -- --ignored --nocapture`
#[test]
#[ignore = "minutes of CPU; run on a quiet machine"]
fn full_stream_vs_the_reference() {
    let vid = std::env::var("GMCLIP_LAB_VID").unwrap_or_else(|_| "loc_4fadb756".into());
    let secs_limit: Option<f64> = std::env::var("GMCLIP_LAB_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok());
    let mut opts = ParakeetOptions::default();
    if let Some(t) = std::env::var("GMCLIP_ASR_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        opts.intra_threads = t;
    }
    let threads = opts.intra_threads;
    let asr = common::asr(opts);
    let t_dec = Instant::now();
    let pcm = common::voice_16k(&common::proxy(&vid), 0.0, secs_limit);
    let decode_s = t_dec.elapsed().as_secs_f64();
    let audio_s = pcm.len() as f64 / 16_000.0;
    let t = Instant::now();
    let mut chunks = 0;
    let words = asr
        .transcribe_streaming(&pcm, Flicks::ZERO, &CancelToken::default(), &mut |c| {
            chunks += 1;
            assert!(
                c.words
                    .iter()
                    .all(|w| w.start < c.final_until || c.index + 1 == c.of)
            );
        })
        .unwrap();
    let wall = t.elapsed().as_secs_f64();
    let hyp: Vec<TimedWord> = words
        .iter()
        .map(|w| TimedWord {
            text: w.text.clone(),
            start: secs(w.start),
            end: secs(w.end),
        })
        .collect();
    let reference: Vec<TimedWord> =
        common::coreml_words(&common::lab().join(format!("asr/{vid}/out.json")))
            .into_iter()
            .filter(|w| w.start < audio_s)
            .collect();
    let a = align(&normalised(&reference), &normalised(&hyp));
    println!(
        "{vid}: {audio_s:.0} s audio (ffmpeg decode + resample {decode_s:.1} s), {chunks} chunks, ASR {wall:.1} s = {:.1}× real time on {} intra threads",
        audio_s / wall,
        threads
    );
    println!(
        "agreement {:.2} % (WER {:.2} %: S {} D {} I {} over {} ref words), median |Δstart| {:.0} ms, p90 {:.0} ms",
        100.0 * a.agreement(),
        100.0 * a.wer(),
        a.substitutions,
        a.deletions,
        a.insertions,
        a.reference_words,
        1000.0 * a.median_start_delta().unwrap_or(f64::NAN),
        1000.0 * gmclip_ml::wer::quantile(&a.start_deltas, 0.9).unwrap_or(f64::NAN)
    );
    for l in asr.execution_report().lines {
        println!("EP: {l}");
    }
}

/// The import's first window of AJ's 2026-10-19 voice-over (`vo-2026-10-19.mp4`, 0–23 s:
/// 20 s plus 3 s of context) decodes "…every other harness directly." and then nothing;
/// the retake at 12.7–18.6 s comes back only as a hole decoded on its own
/// (`gmclip_ml::holes`).
#[test]
fn speech_after_a_blank_decode_is_heard() {
    let asr = common::asr(ParakeetOptions::default());
    let pcm = common::voice_16k(&common::lab().join("vo-2026-10-19.mp4"), 0.0, Some(23.0));
    let words = asr
        .transcribe(&pcm, Flicks::ZERO, &CancelToken::default())
        .unwrap();
    let text: Vec<String> = normalised(
        &words
            .iter()
            .map(|w| TimedWord {
                text: w.text.clone(),
                start: secs(w.start),
                end: secs(w.end),
            })
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .map(|w| w.text)
    .collect();
    let said = text.join(" ");
    assert!(
        said.contains("no one's really heard of"),
        "the retake is heard: {said}"
    );
    assert_eq!(said.matches("i stopped using").count(), 2, "{said}");
}
