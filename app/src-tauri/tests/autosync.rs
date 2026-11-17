//! The pair dialog's measured offset on AJ's loc_4fadb756 (the proxy with the voice, the
//! 1440p MKV): RT measured −503 frames early in the stream and −1642 later, and the
//! hi-res file lost some seconds between. Lab: skips without AJ's files.

use std::path::Path;

/// `preview/loc_4fadb756.proxy.mp4` in the lab media folder.
fn proxy() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("preview/loc_4fadb756.proxy.mp4"));
    P.as_str()
}
/// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
fn mkv() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv"));
    P.as_str()
}

#[test]
fn finds_rts_piecewise_offset() {
    if !Path::new(proxy()).is_file() || !Path::new(mkv()).is_file() {
        eprintln!("skipped: AJ's loc_4fadb756 files are not here");
        return;
    }
    let v = gmclip_app::autosync::propose_json(proxy(), mkv(), None).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&v).unwrap());
    let st = v["stretches"].as_array().unwrap();
    assert_eq!(st.len(), 2, "{v}");
    assert_eq!(st[0]["frames"], 503.0, "{v}");
    assert_eq!(st[1]["frames"], 1642.0, "{v}");
    assert!(st[0]["picture"].as_bool().unwrap() && st[1]["picture"].as_bool().unwrap());
    let change = st[1]["from_s"].as_f64().unwrap();
    assert!((700.0..=1000.0).contains(&change), "change at {change}");
    assert!(
        st.iter().all(|s| s["confidence"].as_f64().unwrap() > 0.5),
        "{v}"
    );
}
