//! R-EXP-5 lab half (B-05): real AAC encoders and OS decode. Needs ffmpeg, ffprobe,
//! afconvert and swift (dev tools via `tools/`). Run with `--run-ignored only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::*;
use gmclip_media::audio::{priming, wav};
use gmclip_media::mux::inspect;

/// The encoder delay an encoder declared in its own file: `elst` media_time (libavcodec)
/// or the iTunSMPB tag's second field (AudioToolbox via afconvert).
fn declared_priming(m4a: &[u8]) -> i64 {
    let tree = inspect::parse(m4a).unwrap();
    if tree.iter().any(|b| b.find(b"elst").is_some()) {
        let elst = box_payload(m4a, b"elst");
        return if elst[0] == 1 {
            i64::from_be_bytes(elst[16..24].try_into().unwrap())
        } else {
            i64::from(i32::from_be_bytes(elst[12..16].try_into().unwrap()))
        };
    }
    let at = m4a
        .windows(8)
        .position(|w| w == b"iTunSMPB")
        .expect("elst or iTunSMPB");
    let text = String::from_utf8_lossy(&m4a[at..(at + 200).min(m4a.len())]).into_owned();
    let start = text.find(" 00000000 ").expect("iTunSMPB value") + 10;
    i64::from_str_radix(&text[start..start + 8], 16).unwrap()
}

/// A non-periodic test cut: a log chirp 150 Hz → 6 kHz, the right channel inverted.
fn chirp_cut(n: usize) -> Vec<f32> {
    let (f0, f1, t1) = (150f64, 6000f64, n as f64 / 48_000.0);
    let k = (f1 / f0).ln() / t1;
    (0..n)
        .flat_map(|i| {
            let t = i as f64 / 48_000.0;
            let s = (std::f64::consts::TAU * f0 * ((k * t).exp() - 1.0) / k).sin() as f32 * 0.25;
            [s, -s]
        })
        .collect()
}

/// B-05 / R-EXP-5 lab: real AAC encoders with priming D (libavcodec 1024, AudioToolbox
/// 2112), fed from cut sample D, muxed with x264 B-frame video, then decoded by ffmpeg
/// and by AVFoundation. Measures where cut sample q lands in each decoder's output.
#[test]
#[ignore = "lab: needs ffmpeg, ffprobe, afconvert, swift"]
fn priming_round_trip_real_encoders_and_os_decode() {
    let dir = lab_dir("priming");
    let v = dir.join("v.mp4");
    tool(&["mux_fixtures.py", "video", v.to_str().unwrap(), "2"]);
    let n = 96_000usize;
    let cut = chirp_cut(n);
    let left: Vec<f32> = cut.iter().step_by(2).copied().collect();
    for (enc, d) in [("lavc", 1024usize), ("at", 2112usize)] {
        let feed = priming::encoder_feed(&cut, 2, d, 48_000);
        let feed_path = dir.join(format!("feed-{d}.wav"));
        wav::write_f32(std::fs::File::create(&feed_path).unwrap(), 48_000, 2, &feed).unwrap();
        let prefix = dir.join(format!("a{d}"));
        tool(&[
            "mux_fixtures.py",
            "aac",
            feed_path.to_str().unwrap(),
            prefix.to_str().unwrap(),
        ]);
        let m4a = dir.join(format!("a{d}-{enc}.m4a"));
        assert_eq!(
            declared_priming(&std::fs::read(&m4a).unwrap()),
            d as i64,
            "{enc} priming"
        );
        let out = dir.join(format!("out-{enc}.mp4"));
        let report = rewrap(&v, &m4a, n as u64, &out);
        assert_eq!(report.tracks[1].duration, n as u64);
        // Container rules on the real-encoder file, by our own parser.
        let tracks = inspect::tracks(&std::fs::read(&out).unwrap()).unwrap();
        assert!(tracks.iter().all(|t| !t.has_edts), "no edts");
        assert_eq!(
            tracks[0].ctts_version,
            Some(1),
            "x264 B-frames → signed ctts"
        );
        assert!(tracks[0].cslg.is_some(), "cslg present");
        assert_eq!(tracks[0].samples.iter().map(|s| s.pts).min(), Some(0));

        // Container facts from ffprobe -show_packets: first video and audio PTS 0.
        let pk = ffprobe_json(
            &out,
            &[
                "-show_packets",
                "-show_entries",
                "packet=stream_index,pts,dts,duration",
            ],
        );
        let pkts = pk["packets"].as_array().unwrap();
        let first = |si: u64| {
            pkts.iter()
                .find(|p| p["stream_index"] == si)
                .unwrap()
                .clone()
        };
        let min_vpts = pkts
            .iter()
            .filter(|p| p["stream_index"] == 0)
            .map(|p| p["pts"].as_i64().unwrap())
            .min();
        assert_eq!(min_vpts, Some(0), "first video PTS 0");
        assert!(
            first(0)["dts"].as_i64().unwrap() < 0,
            "ffprobe shows DTS below 0 (ctts v1 + cslg shift)"
        );
        assert_eq!(first(1)["pts"], 0, "audio sample 0 at t = 0");
        assert!(ffprobe_warnings(&out).is_empty());
        assert!(ffmpeg_decode_errors(&out).is_empty());

        // ffmpeg decode (no edts → priming not trimmed): cut sample q at position q.
        let ff_pcm = dir.join(format!("ff-{enc}.f32"));
        let st = std::process::Command::new(FFMPEG)
            .args(["-v", "error", "-y", "-i"])
            .arg(&out)
            .args(["-map", "0:a", "-af", "pan=mono|c0=c0", "-f", "f32le"])
            .arg(&ff_pcm)
            .status()
            .unwrap();
        assert!(st.success());
        let ff = read_f32(&ff_pcm);
        let (lag, corr) = xcorr_lag(&left, &ff, d + 2048, n - 4096, 3000);
        println!(
            "{enc} D={d}: ffmpeg decoded {} samples, lag {lag}, corr {corr:.4}",
            ff.len()
        );
        assert_eq!(
            lag, 0,
            "ffmpeg: cut sample q decodes at position q (D = {d})"
        );
        assert!(corr > 0.98);
        // ffmpeg decodes the whole last access unit (it does not trim the tag's padding
        // in this output path): N + 256 samples, the extra ones after the cut's end.
        assert_eq!(ff.len(), n.div_ceil(1024) * 1024, "ffmpeg decoded length");

        // AVFoundation (the QuickTime/Safari stack, via AVAssetReader): without the
        // gapless tag it assumes 2112 samples of priming (lag −2112, 44 ms early); with
        // iTunSMPB priming 0 / length N it plays cut sample q at q and decodes exactly N.
        let avf_pcm = dir.join(format!("avf-{enc}.f32"));
        let avf = avf_probe(&out, Some(&avf_pcm));
        let at: Vec<f32> = read_f32(&avf_pcm).iter().step_by(2).copied().collect();
        let (alag, acorr) = xcorr_lag(&left, &at, d + 2048, n - 4096, 3000);
        println!(
            "{enc} D={d}: AVFoundation decoded {} samples, lag {alag}, corr {acorr:.4}; {avf}",
            at.len()
        );
        assert!(acorr > 0.98);
        assert_eq!(
            alag, 0,
            "AVFoundation: cut sample q at position q (D = {d})"
        );
        assert_eq!(at.len(), n, "AVFoundation decoded length");
    }
}
