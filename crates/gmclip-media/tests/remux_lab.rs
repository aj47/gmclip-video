//! B-06 lab: the K2 remux probe on the shipped h79-v4.mp4 and h48-v5.mp4. Two files per
//! cut land in `$GMCLIP_PROBE_DIR` (default `~/Development/gmclip-artifacts/
//! remux-probe`):
//!
//! - `<cut>.remux.mp4` (`mux::remux::remux`): every sample byte-identical; the audio keeps
//!   the input's encoder-delay edit. Checked: bytes, timestamps, framemd5, audio framecrc
//!   *with timestamps* equal to the input's, format duration equal.
//! - `<cut>.export-audio.mp4` (`remux_with_audio`): video byte-identical; audio = the
//!   input's presented audio re-encoded the export way (static −16 LUFS / −1.5 dBTP on the
//!   delivered AAC, AudioToolbox fed from sample D, priming-0 iTunSMPB, no `edts` anywhere) — what H-14a uploads, since it is what an
//!   export is. Checked: no edts, framemd5, audio start 0 and its content at lag 0
//!   against the input in ffmpeg and in AVFoundation, exact length.
//!
//! Run with `--run-ignored only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use gmclip_media::mux::{inspect, remux};

/// Two shipped reels, in the lab media folder.
fn shipped() -> [(&'static str, String); 2] {
    [
        ("h79-v4", gmclip_model::lab::media_str("h79/h79-v4.mp4")),
        (
            "h48-v5",
            gmclip_model::lab::media_str("reels/loc_4fadb756_h48/loc_4fadb756_h48-v5.mp4"),
        ),
    ]
}

pub fn probe_dir() -> PathBuf {
    let d = std::env::var_os("GMCLIP_PROBE_DIR").map_or_else(
        || {
            Path::new(&std::env::var("HOME").unwrap())
                .join("Development/gmclip-artifacts/remux-probe")
        },
        PathBuf::from,
    );
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// ffmpeg `framemd5`/`framecrc` lines for one stream: "pts duration hash".
fn frame_hashes(f: &Path, stream: &str, fmt: &str) -> Vec<String> {
    let out = Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(f)
        .args(["-map", stream, "-f", fmt, "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let c: Vec<&str> = l.split(',').map(str::trim).collect();
            format!("{} {} {}", c[2], c[4], c[5])
        })
        .collect()
}

/// ffmpeg's audio decode (stereo f32) and the stream's start time.
fn ff_audio(f: &Path, dir: &Path) -> (Vec<f32>, f64) {
    let p = dir.join("ff.f32");
    let st = Command::new(FFMPEG)
        .args(["-v", "error", "-y", "-i"])
        .arg(f)
        .args(["-map", "0:a", "-f", "f32le"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(st.success());
    let v = read_f32(&p);
    std::fs::remove_file(&p).unwrap();
    let s = ffprobe_json(f, &["-show_streams", "-select_streams", "a"]);
    let start = s["streams"][0]["start_time"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (v, start)
}

fn left(x: &[f32]) -> Vec<f32> {
    x.iter().step_by(2).copied().collect()
}

#[test]
#[ignore = "lab: needs ffmpeg, ffprobe, afconvert, swift and the shipped renders"]
fn remux_probe() {
    let out_dir = probe_dir();
    let scratch = lab_dir("remux-probe");
    let mut lines = Vec::new();
    for (name, src) in shipped() {
        let src = Path::new(&src);
        let a = std::fs::read(src).unwrap();
        let ta = inspect::tracks(&a).unwrap();
        let (in_audio, in_start) = ff_audio(src, &scratch);
        let in_video = frame_hashes(src, "0:v", "framemd5");

        // --- 1. sample-for-sample remux (audio edit kept) ---
        let out = out_dir.join(format!("{name}.remux.mp4"));
        let r = remux::remux(src, &out).unwrap();
        let b = std::fs::read(&out).unwrap();
        let tb = inspect::tracks(&b).unwrap();
        for (ti, (x, y)) in ta.iter().zip(&tb).enumerate() {
            let sh = if ti == 0 { r.video_shift } else { 0 };
            assert_eq!(x.samples.len(), y.samples.len());
            for (k, (sx, sy)) in x.samples.iter().zip(&y.samples).enumerate() {
                let bx = &a[sx.offset as usize..sx.offset as usize + sx.size as usize];
                let by = &b[sy.offset as usize..sy.offset as usize + sy.size as usize];
                assert!(bx == by, "{name}: track {ti} sample {k} bytes differ");
                assert_eq!(
                    (sy.dts, sy.pts, sy.sync, sy.duration),
                    (sx.dts, sx.pts - sh, sx.sync, sx.duration),
                    "{name}: {ti}/{k}"
                );
            }
            assert_eq!(
                x.entry.config, y.entry.config,
                "{name}: codec config verbatim"
            );
        }
        assert!(
            !tb[0].has_edts && tb[1].has_edts,
            "{name}: video no edts, audio keeps its edit"
        );
        assert_eq!(
            frame_hashes(&out, "0:v", "framemd5"),
            in_video,
            "{name}: video frames + times"
        );
        assert_eq!(
            frame_hashes(&out, "0:a", "framecrc"),
            frame_hashes(src, "0:a", "framecrc"),
            "{name}: audio frames + times"
        );
        let (fa, fb) = (
            ffprobe_json(src, &["-show_format"]),
            ffprobe_json(&out, &["-show_format"]),
        );
        assert_eq!(
            fa["format"]["duration"], fb["format"]["duration"],
            "{name}: format duration"
        );
        assert!(
            ffprobe_warnings(&out).is_empty() && ffmpeg_decode_errors(&out).is_empty(),
            "{name}: clean"
        );
        let avf_pcm = scratch.join("avf.f32");
        let avf = avf_probe(&out, Some(&avf_pcm));
        let at = left(&read_f32(&avf_pcm));
        let (lag_a, _) = xcorr_lag(&left(&in_audio), &at, 48_000, 48_000 * 4, 3000);
        assert_eq!(lag_a, 0, "{name}: AVFoundation, remux");
        lines.push(format!(
            "{name}.remux.mp4: {} video + {} audio samples byte-identical; audio keeps its edit {:?}; video framemd5 and audio framecrc (with timestamps) equal to the input; format duration {} = input; AVFoundation {} frames, audio lag 0",
            tb[0].samples.len(),
            tb[1].samples.len(),
            r.audio_edit,
            fb["format"]["duration"],
            avf["tracks"][0]["decoded"]
        ));

        // --- 2. export-audio: the export's audio scheme, no edts at all ---
        let n = r.audio_edit.map_or(ta[1].media_duration, |e| e.presented) as usize;
        assert_eq!(in_start, 0.0);
        let cut = &in_audio[..2 * n];
        // As an export does: static −16 LUFS with the −1.5 dBTP ceiling on the delivered
        // AAC (decision 0004), then AudioToolbox fed from sample 2112.
        let (delivered, d) = gmclip_media::audio::loudness::normalize_for_delivery(
            cut,
            2,
            48_000,
            -16.0,
            -1.5,
            |x: &[f32]| aac_round_trip(&scratch, x),
        )
        .unwrap();
        assert!(
            d.within_ceiling,
            "{name}: delivered TP {}",
            d.delivered_tp_dbtp
        );
        let m4a = aac_encode_cut(&scratch, &delivered);
        let out2 = out_dir.join(format!("{name}.export-audio.mp4"));
        let r2 = remux::remux_with_audio(src, &m4a, n as u64, &out2).unwrap();
        std::fs::remove_file(&m4a).unwrap();
        let c = std::fs::read(&out2).unwrap();
        let tc = inspect::tracks(&c).unwrap();
        assert!(
            tc.iter().all(|t| !t.has_edts),
            "{name}: export-audio has no edts"
        );
        assert_eq!(inspect::itunsmpb(&c).unwrap(), r2.gapless);
        assert_eq!(
            frame_hashes(&out2, "0:v", "framemd5"),
            in_video,
            "{name}: export-audio video frames + times"
        );
        let (dec, start) = ff_audio(&out2, &scratch);
        assert_eq!(start, 0.0, "{name}: audio starts at 0 in ffmpeg");
        let (lag_f, corr_f) = xcorr_lag(&left(cut), &left(&dec), 48_000, 48_000 * 4, 3000);
        let avf = avf_probe(&out2, Some(&avf_pcm));
        let at = left(&read_f32(&avf_pcm));
        std::fs::remove_file(&avf_pcm).unwrap();
        let (lag_v, corr_v) = xcorr_lag(&left(cut), &at, 48_000, 48_000 * 4, 3000);
        assert!(
            ffprobe_warnings(&out2).is_empty() && ffmpeg_decode_errors(&out2).is_empty(),
            "{name}: clean"
        );
        assert_eq!(
            (lag_f, lag_v),
            (0, 0),
            "{name}: export-audio A/V offset (ffmpeg, AVFoundation)"
        );
        assert_eq!(at.len(), n, "{name}: AVFoundation length");
        lines.push(format!(
            "{name}.export-audio.mp4: video {} samples byte-identical (framemd5 equal); audio re-encoded the export way (−16 LUFS / −1.5 dBTP delivered, AudioToolbox from sample 2112, iTunSMPB {:?}, no edts); audio start 0; content lag vs input: ffmpeg {lag_f} (corr {corr_f:.4}), AVFoundation {lag_v} (corr {corr_v:.4}), AVFoundation length {} = {n}; video avf {}",
            tc[0].samples.len(),
            r2.gapless,
            at.len(),
            avf["tracks"][0]["decoded"]
        ));
        println!("{}\n{}", lines[lines.len() - 2], lines[lines.len() - 1]);
    }
    std::fs::write(out_dir.join("PROBE.txt"), lines.join("\n") + "\n").unwrap();
    for f in ["h79-v4.gmclip-remux.mp4", "h48-v5.gmclip-remux.mp4"] {
        let _ = std::fs::remove_file(out_dir.join(f)); // the first round's files
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
