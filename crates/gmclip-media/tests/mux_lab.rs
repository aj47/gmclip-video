//! Lab tests for the muxer (B-04, B-05): real coded media in, our MP4 out, opened by
//! ffprobe/ffmpeg and decoded by AVFoundation (the QuickTime/Safari stack). Needs
//! `/opt/homebrew/bin/ffmpeg`, `ffprobe`, `/usr/bin/afconvert` and `swift` — dev tools
//! only, called from `tools/`. Run: `cargo nextest run -p gmclip-media --run-ignored only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::*;
use gmclip_media::audio::wav;

/// B-04 acceptance: a re-wrap of x264 (B-frames, reorder depth 2) + libavcodec AAC
/// opens in ffprobe and ffmpeg with zero warnings and decodes fully in AVFoundation.
#[test]
#[ignore = "lab: needs ffmpeg, ffprobe, afconvert, swift"]
fn rewrap_opens_with_zero_warnings_and_decodes_in_avfoundation() {
    let dir = lab_dir("rewrap");
    let v = dir.join("v.mp4");
    tool(&["mux_fixtures.py", "video", v.to_str().unwrap(), "2"]);
    let feed: Vec<f32> = (0..96_000)
        .flat_map(|i| {
            let s = (f64::from(i) * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.3;
            [s, s]
        })
        .collect();
    let feed_path = dir.join("feed.wav");
    wav::write_f32(std::fs::File::create(&feed_path).unwrap(), 48_000, 2, &feed).unwrap();
    let prefix = dir.join("a");
    tool(&[
        "mux_fixtures.py",
        "aac",
        feed_path.to_str().unwrap(),
        prefix.to_str().unwrap(),
    ]);
    let out = dir.join("out.mp4");
    let report = rewrap(&v, &dir.join("a-lavc.m4a"), 96_000, &out);
    println!("{report:#?}");

    let warnings = ffprobe_warnings(&out);
    assert!(warnings.is_empty(), "ffprobe warnings: {warnings}");
    let errs = ffmpeg_decode_errors(&out);
    assert!(errs.is_empty(), "ffmpeg decode: {errs}");
    let streams = ffprobe_json(&out, &["-show_streams", "-count_frames"]);
    let vs = &streams["streams"][0];
    assert_eq!(vs["codec_name"], "h264");
    assert_eq!(vs["has_b_frames"], 2);
    assert_eq!(vs["nb_read_frames"], "60");
    assert_eq!(vs["color_primaries"], "bt709");
    assert_eq!(vs["time_base"], "1/15360");
    let a = &streams["streams"][1];
    assert_eq!(a["codec_name"], "aac");
    assert_eq!(a["duration_ts"], 96_000);

    let avf = avf_probe(&out, None);
    println!("{avf}");
    let t = &avf["tracks"];
    let vt = t
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["media_type"] == "vide")
        .unwrap();
    assert_eq!(vt["decoded"], 60);
    assert_eq!(vt["first_pts_s"], 0.0);
    assert_eq!(vt["primaries"], "ITU_R_709_2");
    let at = t
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["media_type"] == "soun")
        .unwrap();
    // With the iTunSMPB gapless tag (priming 0, length N) AVFoundation decodes exactly
    // the cut's length; without it, it assumes 2112 samples of priming (B-05).
    assert_eq!(at["decoded"].as_u64().unwrap(), 96_000);
    assert_eq!(at["first_pts_s"], 0.0);
}
