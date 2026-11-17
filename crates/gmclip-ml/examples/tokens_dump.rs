//! Dev: raw parakeet-rs tokens for a span. `-- <vid> <start> <dur>`
#[path = "../tests/common/mod.rs"]
mod common;

use gmclip_ml::models::{ModelCache, PARAKEET_TDT_V3_INT8};
use gmclip_ml::runtime::OrtRuntime;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode, Transcriber};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cache = ModelCache::default_cache().unwrap();
    let rt = OrtRuntime::init(&cache).unwrap();
    let m = cache.verify(&PARAKEET_TDT_V3_INT8).unwrap();
    let cfg = ExecutionConfig::new().with_environment(rt.environment().clone());
    let mut p = ParakeetTDT::from_pretrained(&m.dir, Some(cfg)).unwrap();
    let pcm = common::voice_16k(
        &common::proxy(&a[1]),
        a[2].parse().unwrap(),
        Some(a[3].parse().unwrap()),
    );
    let r = p
        .transcribe_samples(pcm, 16_000, 1, Some(TimestampMode::Tokens))
        .unwrap();
    for t in r.tokens {
        println!("{:7.2} {:7.2} {:?}", t.start, t.end, t.text);
    }
}
