//! B-12 lab: static loudness on the 11 RT golden plans (SPEC R-COMP-2's set: the ten
//! `reels/*` cuts and h79 v4). Each plan's audio is mixed by the graph
//! (B-11, checked against RT's graph as in `audio_lab.rs`), normalised with
//! `static {-16 LUFS, -1.5 dBTP}`, and re-measured: I within ±0.2 LU of −16, TP ≤ −1.5
//! dBTP. Our meter is cross-checked against ffmpeg's `ebur128` (peak=true) on the same
//! PCM. Needs ffmpeg and the golden cuts in the lab media folder; run with `--run-ignored only`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use gmclip_media::audio::{AudioPlan, PcmSource, loudness, wav};

/// The lab media folder (the golden cuts' `h79/` and `reels/`).
fn production() -> String {
    gmclip_model::lab::root().to_string_lossy().into_owned()
}

const PLANS: [(&str, &str); 11] = [
    ("h79", "h79"),
    ("h03", "reels/loc_086da141_h03"),
    ("h25", "reels/loc_086da141_h25"),
    ("h47", "reels/loc_086da141_h47"),
    ("h34", "reels/loc_4fadb756_h34"),
    ("h37", "reels/loc_4fadb756_h37"),
    ("h48", "reels/loc_4fadb756_h48"),
    ("h41", "reels/loc_bd89f38b_h41"),
    ("h43", "reels/loc_bd89f38b_h43"),
    ("h53", "reels/loc_bd89f38b_h53"),
    ("h88", "reels/loc_bd89f38b_h88"),
];

/// The plans among `set` whose receipt and sound sources are on this machine.
fn present(
    set: &[(&'static str, &'static str)],
) -> gmclip_model::lab::Present<(&'static str, &'static str)> {
    gmclip_model::lab::present(
        set.iter().copied(),
        |(name, _)| name.to_string(),
        |(_, dir)| {
            let cut = dir.strip_prefix("reels/").unwrap_or("loc_4fadb756_h79");
            let mut v = golden_audio_sources(cut);
            v.push(gmclip_model::lab::media(format!(
                "{dir}/render-receipt.json"
            )));
            v
        },
    )
}

struct Slices(HashMap<(String, u64), PathBuf>);

impl PcmSource for Slices {
    fn read(&mut self, source: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
        let path = self.0.get(&(source.to_owned(), first)).ok_or("no slice")?;
        let pcm = wav::read(path).map_err(|e| e.to_string())?;
        if pcm.samples.len() != out.len() {
            return Err(format!(
                "{}: {} samples, want {}",
                path.display(),
                pcm.samples.len(),
                out.len()
            ));
        }
        out.copy_from_slice(&pcm.samples);
        Ok(())
    }
}

fn slices(dir: &Path, plan: &gmclip_plan::Plan, prefix: &str) -> Slices {
    Slices(
        plan.parts
            .iter()
            .map(|p| {
                let key = (p.audio.source.as_str().to_owned(), p.audio.first_sample);
                (
                    key,
                    dir.join("slices")
                        .join(format!("{prefix}{}.wav", p.id.as_str())),
                )
            })
            .collect(),
    )
}

/// ffmpeg's ebur128 summary (I, true peak) of an interleaved stereo buffer.
fn ffmpeg_ebur128(dir: &Path, pcm: &[f32]) -> (f64, f64) {
    let f = dir.join("m.wav");
    wav::write_f32(std::fs::File::create(&f).unwrap(), 48_000, 2, pcm).unwrap();
    let out = Command::new(FFMPEG)
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(&f)
        .args(["-af", "ebur128=peak=true:framelog=quiet", "-f", "null", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    let summary = &text[text.rfind("Summary:").expect("ebur128 summary")..];
    let num = |key: &str| -> f64 {
        let at = summary.find(key).unwrap() + key.len();
        summary[at..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    (num("I:"), num("Peak:"))
}

#[test]
#[ignore = "lab: needs ffmpeg and the golden cuts in the lab media folder"]
fn static_loudness_on_the_11_golden_plans() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("loudness");
    let set = present(&PLANS);
    if set.skip_if_empty() {
        return;
    }
    let mut rows = Vec::new();
    for (name, dir) in set.items.iter().copied() {
        let work = root.join(name);
        let receipt = format!("{}/{dir}/render-receipt.json", production());
        tool(&["golden_audio_ref.py", &receipt, work.to_str().unwrap()]);
        let typed = gmclip_plan::parse(&std::fs::read(work.join("plan-audio.json")).unwrap())
            .expect("valid plan");
        let plan = AudioPlan::from_plan(&typed).unwrap();

        // B-11 on all 11: the graph against RT's graph (no loudnorm).
        let mut mix = plan.mix(&mut slices(&work, &typed, "")).unwrap();
        let reference = wav::read(&work.join("ref.wav")).unwrap();
        assert_eq!(mix.len(), reference.samples.len());
        let graph_diff = mix
            .iter()
            .zip(&reference.samples)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(graph_diff <= 1e-4, "{name}: graph vs RT {graph_diff}");

        let before = loudness::measure(&mix, 2, 48_000);
        let (ff_i0, ff_tp0) = ffmpeg_ebur128(&work, &mix);
        let r = loudness::normalize_static(&mut mix, 2, 48_000, -16.0, -1.5);
        let (ff_i1, ff_tp1) = ffmpeg_ebur128(&work, &mix);
        let row = format!(
            "{name}: graph Δ {graph_diff:.1e} | before I {:.2} (ffmpeg {ff_i0:.1}) TP {:.2} (ffmpeg {ff_tp0:.1}) | gain {:+.2} dB, limiter >0.1 dB {:.1}% / >1 dB {:.1}% / >3 dB {:.1}% of frames, deepest {:.2} dB | after I {:.2} (ffmpeg {ff_i1:.1}) TP {:.2} (ffmpeg {ff_tp1:.1})",
            before.integrated_lufs,
            before.true_peak_dbtp,
            r.gain_db,
            100.0 * r.limiter.frames_over_0_1_db as f64 * 2.0 / mix.len() as f64,
            100.0 * r.limiter.frames_over_1_db as f64 * 2.0 / mix.len() as f64,
            100.0 * r.limiter.frames_over_3_db as f64 * 2.0 / mix.len() as f64,
            r.limiter.deepest_db,
            r.after.integrated_lufs,
            r.after.true_peak_dbtp
        );
        println!("{row}");
        rows.push(row);
        assert!(
            (r.after.integrated_lufs + 16.0).abs() <= 0.2,
            "{name}: I {}",
            r.after.integrated_lufs
        );
        assert!(
            r.after.true_peak_dbtp <= -1.5,
            "{name}: TP {}",
            r.after.true_peak_dbtp
        );
        // Our meter agrees with ffmpeg's (it prints one decimal).
        assert!(
            (before.integrated_lufs - ff_i0).abs() <= 0.1,
            "{name}: I vs ffmpeg"
        );
        assert!(
            (r.after.integrated_lufs - ff_i1).abs() <= 0.1,
            "{name}: I vs ffmpeg after"
        );
        assert!(
            (before.true_peak_dbtp - ff_tp0).abs() <= 0.3,
            "{name}: TP vs ffmpeg"
        );
        assert!(ff_tp1 <= -1.4, "{name}: ffmpeg TP after {ff_tp1}");
        // Both measurements go into the export record.
        let rec = gmclip_media::export_record::LoudnessRecord::from(&r);
        assert_eq!(
            (rec.integrated, rec.true_peak),
            (Some(r.after.integrated_lufs), Some(r.after.true_peak_dbtp))
        );
        assert!(rec.before.is_some());
        assert!(r.reached);
        let _ = std::fs::remove_dir_all(&work); // the disk is shared and nearly full
    }
    let measured = format!("B-12 measured on {}", set.note("golden plans"));
    println!("{measured}");
    rows.push(measured);
    std::fs::write(root.join("report.txt"), rows.join("\n") + "\n").unwrap();
}

/// AudioToolbox AAC-LC 192k round trip via afconvert (the encoder H-09 wraps).
fn aac_round_trip(dir: &Path, pcm: &[f32]) -> Result<Vec<f32>, String> {
    let wav_in = dir.join("enc-in.wav");
    let m4a = dir.join("enc.m4a");
    let wav_out = dir.join("enc-out.wav");
    wav::write_f32(std::fs::File::create(&wav_in).unwrap(), 48_000, 2, pcm)
        .map_err(|e| e.to_string())?;
    let enc = Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac@48000", "-b", "192000", "-s", "0"])
        .arg(&wav_in)
        .arg(&m4a)
        .status()
        .map_err(|e| e.to_string())?;
    let dec = Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", "LEF32@48000"])
        .arg(&m4a)
        .arg(&wav_out)
        .status()
        .map_err(|e| e.to_string())?;
    if !enc.success() || !dec.success() {
        return Err("afconvert failed".into());
    }
    let out = wav::read(&wav_out).map_err(|e| e.to_string())?.samples;
    for f in [&wav_in, &m4a, &wav_out] {
        let _ = std::fs::remove_file(f);
    }
    Ok(out)
}

/// Decision 0004: the −1.5 dBTP ceiling holds on the delivered AAC (decoded), with at
/// most one re-encode at a ceiling lowered by the overshoot.
#[test]
#[ignore = "lab: needs ffmpeg, afconvert and the golden cuts in the lab media folder"]
fn delivered_true_peak_after_aac() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("delivery");
    let set = present(&[PLANS[0], PLANS[2], PLANS[8]]);
    if set.skip_if_empty() {
        return;
    }
    println!("decision 0004 checked on {}", set.note("plans"));
    for (name, dir) in set.items.iter().copied() {
        let work = root.join(name);
        let receipt = format!("{}/{dir}/render-receipt.json", production());
        tool(&[
            "golden_audio_ref.py",
            &receipt,
            work.to_str().unwrap(),
            "--no-ref",
        ]);
        let typed = gmclip_plan::parse(&std::fs::read(work.join("plan-audio.json")).unwrap())
            .expect("valid plan");
        let plan = AudioPlan::from_plan(&typed).unwrap();
        let mix = plan.mix(&mut slices(&work, &typed, "")).unwrap();
        let (_, r) =
            loudness::normalize_for_delivery(&mix, 2, 48_000, -16.0, -1.5, |x: &[f32]| {
                aac_round_trip(&work, x)
            })
            .unwrap();
        println!(
            "{name}: mix TP {:.2} → AAC TP {:.2}{} | delivered TP {:.2} dBTP, I {:.2} LUFS (ceiling used {:.2})",
            r.mix.after.true_peak_dbtp,
            r.first_delivered_tp_dbtp,
            if r.reencoded { " → re-encoded" } else { "" },
            r.delivered_tp_dbtp,
            r.delivered_integrated_lufs,
            r.ceiling_dbtp
        );
        assert!(
            r.within_ceiling,
            "{name}: delivered TP {}",
            r.delivered_tp_dbtp
        );
        assert!(
            (r.delivered_integrated_lufs + 16.0).abs() <= 0.3,
            "{name}: delivered I {}",
            r.delivered_integrated_lufs
        );
        let _ = std::fs::remove_dir_all(&work);
    }
}
