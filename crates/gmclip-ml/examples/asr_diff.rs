//! Dev: transcribe a span of a stream proxy and print the word diff against a
//! CoreML Parakeet transcript. `cargo run -p gmclip-ml --release --features lab
//! --example asr_diff -- <vid> <start> <dur> <ref.json> [window_s]`
#[path = "../tests/common/mod.rs"]
mod common;

use gmclip_ml::parakeet::ParakeetOptions;
use gmclip_ml::wer::{TimedWord, align, normalised};
use gmclip_ml::{Asr, CancelToken};
use gmclip_model::Flicks;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (vid, start, dur, reference) = (
        &a[1],
        a[2].parse::<f64>().unwrap(),
        a[3].parse::<f64>().unwrap(),
        &a[4],
    );
    let mut opts = ParakeetOptions::default();
    if let Some(w) = a.get(5) {
        opts.window_s = w.parse().unwrap();
    }
    if let Some(t) = std::env::var("THREADS").ok().and_then(|t| t.parse().ok()) {
        opts.intra_threads = t;
    }
    let asr = common::asr(opts);
    let pcm = common::voice_16k(&common::proxy(vid), start, Some(dur));
    let words = asr
        .transcribe(&pcm, Flicks::ZERO, &CancelToken::default())
        .unwrap();
    let hyp: Vec<TimedWord> = words
        .iter()
        .map(|w| TimedWord {
            text: w.text.clone(),
            start: w.start.0 as f64 / 705_600_000.0,
            end: w.end.0 as f64 / 705_600_000.0,
        })
        .collect();
    if std::env::var_os("DUMP").is_some() {
        for w in &hyp {
            println!("{:8.2} {:8.2} {}", w.start, w.end, w.text);
        }
    }
    // a whole-stream transcript (absolute times): keep the span, make it relative
    let mut reference_words = common::coreml_words(std::path::Path::new(reference));
    if std::env::var_os("REF_ABS").is_some() {
        reference_words.retain(|w| w.start >= start && w.start < start + dur);
        for w in &mut reference_words {
            w.start -= start;
            w.end -= start;
        }
    }
    let r = normalised(&reference_words);
    let h = normalised(&hyp);
    let al = align(&r, &h);
    println!(
        "WER {:.2} %  S {} D {} I {}",
        100.0 * al.wer(),
        al.substitutions,
        al.deletions,
        al.insertions
    );
    let (mut i, mut j) = (0, 0);
    for &(ri, hj) in al
        .matches
        .iter()
        .chain(std::iter::once(&(r.len(), h.len())))
    {
        if ri > i || hj > j {
            let rr: Vec<String> = r[i..ri]
                .iter()
                .map(|w| format!("{}@{:.2}", w.text, w.start))
                .collect();
            let hh: Vec<String> = h[j..hj]
                .iter()
                .map(|w| format!("{}@{:.2}", w.text, w.start))
                .collect();
            println!("  ref [{}]  hyp [{}]", rr.join(" "), hh.join(" "));
        }
        i = ri + 1;
        j = hj + 1;
    }
}
