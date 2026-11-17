//! U.18: `gmclip-app perf --asr` runs a video's whole analysis and reports the resident
//! size's peak per phase (SPEC §13 "+ ASR ≤ 2.2 GB"). On h79's 60 s reel; skips without
//! it or without the speech model.

use std::path::Path;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[test]
fn asr_memory_is_sampled_through_the_whole_analysis() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let r = gmclip_app::perf::asr(Path::new(video())).unwrap();
    eprintln!("{r}");
    if r["asr_error"].is_string() {
        eprintln!("skipped: no speech model");
        return;
    }
    assert_eq!(r["phase"], "done", "{r}");
    let peak = r["rss_mb"]["peak"].as_f64().unwrap();
    assert!(peak > 0.0 && peak.is_finite(), "{r}");
    assert!(
        r["rss_mb"]["by_phase"]["transcribing"].as_f64().is_some(),
        "{r}"
    );
    assert!(
        (59.0..62.0).contains(&r["source_s"].as_f64().unwrap()),
        "{r}"
    );
    assert!(peak <= 2200.0, "S11: editing + ASR ≤ 2.2 GB, {peak} MB");
}
