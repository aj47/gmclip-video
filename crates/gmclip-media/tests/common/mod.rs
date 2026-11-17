//! Shared helpers for gmclip-media's integration tests. Lab helpers shell out to dev
//! tools (`tools/`, ffmpeg, ffprobe, swift); hosted-CI tests never call them.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use gmclip_media::audio::priming;
use gmclip_media::mux::{ColorTags, Mp4Writer, MuxPlan, Sample, TrackConfig, inspect};
use std::process::Command;

pub const FFMPEG: &str = "/opt/homebrew/bin/ffmpeg";
pub const FFPROBE: &str = "/opt/homebrew/bin/ffprobe";

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A fresh scratch directory for one lab test.
pub fn lab_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs `python3 tools/<args[0]> args[1..]`, panicking with its stderr on failure.
pub fn tool(args: &[&str]) {
    let script = repo_root().join("tools").join(args[0]);
    let out = Command::new("python3")
        .arg(script)
        .args(&args[1..])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ffprobe's own diagnostics at warning level and above (empty = zero warnings).
pub fn ffprobe_warnings(f: &Path) -> String {
    let out = Command::new(FFPROBE)
        .args(["-v", "warning", "-show_format", "-show_streams"])
        .arg(f)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A full ffmpeg decode of every stream; returns warnings and errors (empty = clean).
pub fn ffmpeg_decode_errors(f: &Path) -> String {
    let out = Command::new(FFMPEG)
        .args(["-v", "warning", "-nostdin", "-i"])
        .arg(f)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    let mut s = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        s.push_str("(non-zero exit)");
    }
    s
}

pub fn ffprobe_json(f: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(FFPROBE)
        .args(["-v", "error", "-of", "json"])
        .args(args)
        .arg(f)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `swift tools/avf_probe.swift` (AVFoundation decode) → its JSON report.
pub fn avf_probe(f: &Path, pcm_out: Option<&Path>) -> serde_json::Value {
    let mut c = Command::new("swift");
    c.arg(repo_root().join("tools/avf_probe.swift")).arg(f);
    if let Some(p) = pcm_out {
        c.arg(p);
    }
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "avf_probe: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The payload of the first box with this fourcc anywhere in the file.
pub fn box_payload(file: &[u8], fourcc: &[u8; 4]) -> Vec<u8> {
    let tree = gmclip_media::mux::inspect::parse(file).unwrap();
    tree.iter()
        .find_map(|b| {
            if &b.fourcc == fourcc {
                Some(b)
            } else {
                b.find(fourcc)
            }
        })
        .unwrap()
        .payload(file)
        .to_vec()
}

/// The AudioSpecificConfig inside an `esds` payload (DecoderSpecificInfo, tag 5).
pub fn esds_asc(esds: &[u8]) -> Vec<u8> {
    let mut i = 4; // version + flags
    while i < esds.len() {
        let tag = esds[i];
        i += 1;
        let mut len = 0usize;
        loop {
            let b = esds[i];
            i += 1;
            len = (len << 7) | usize::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
        match tag {
            0x03 => i += 3, // ES_ID + flags (no optional fields in these files)
            0x04 => i += 13,
            0x05 => return esds[i..i + len].to_vec(),
            _ => i += len,
        }
    }
    panic!("no DecoderSpecificInfo");
}

/// Reads raw little-endian f32 samples.
pub fn read_f32(p: &Path) -> Vec<f32> {
    std::fs::read(p)
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// The lag in `[-max, max]` that best aligns `got` to `want` (both mono), by normalised
/// cross-correlation over `want[from..to]`; returns (lag, correlation). `got[q + lag]`
/// matches `want[q]`.
pub fn xcorr_lag(want: &[f32], got: &[f32], from: usize, to: usize, max: i64) -> (i64, f64) {
    let mut best = (0i64, f64::MIN);
    let ew: f64 = want[from..to].iter().map(|x| f64::from(*x).powi(2)).sum();
    for lag in -max..=max {
        let mut dot = 0f64;
        let mut eg = 0f64;
        for (q, w) in want.iter().enumerate().take(to).skip(from) {
            let k = q as i64 + lag;
            if k < 0 || k as usize >= got.len() {
                continue;
            }
            let g = f64::from(got[k as usize]);
            dot += f64::from(*w) * g;
            eg += g * g;
        }
        let c = if eg > 0.0 {
            dot / (ew * eg).sqrt()
        } else {
            0.0
        };
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

/// Re-wraps the video of `v` and the first `ceil(n/1024)` AAC packets of `a` (the last
/// shortened to make `n` samples) with our writer. Video PTS keep their order and are
/// shifted so the first is 0; DTS are shifted by the same amount (so they start below 0
/// by the reorder delay, as VT's rebased DTS do).
pub fn rewrap(
    v: &std::path::Path,
    a: &std::path::Path,
    n: u64,
    out: &std::path::Path,
) -> gmclip_media::mux::MuxReport {
    let vb = std::fs::read(v).unwrap();
    let ab = std::fs::read(a).unwrap();
    let vt = inspect::tracks(&vb)
        .unwrap()
        .into_iter()
        .find(|t| &t.handler == b"vide")
        .unwrap();
    let at = inspect::tracks(&ab)
        .unwrap()
        .into_iter()
        .find(|t| &t.handler == b"soun")
        .unwrap();
    let avcc = box_payload(&vb, b"avcC");
    let asc = esds_asc(&box_payload(&ab, b"esds"));
    let shift = vt.samples.iter().map(|s| s.pts).min().unwrap();
    let durs = priming::packet_durations(n);
    assert!(
        at.samples.len() >= durs.len(),
        "encoder gave {} packets, need {}",
        at.samples.len(),
        durs.len()
    );

    let tracks = vec![
        TrackConfig::h264(avcc, 1080, 1920, 30, 1, Some(ColorTags::BT709_LIMITED)),
        TrackConfig::aac(asc, 2, 48_000),
    ];
    let plan = MuxPlan {
        gapless: Some(priming::gapless(n)),
        sample_counts: vec![vt.samples.len() as u32, durs.len() as u32],
        max_payload_bytes: Some((vb.len() + ab.len()) as u64),
    };
    let mut w = Mp4Writer::create(std::fs::File::create(out).unwrap(), tracks, &plan).unwrap();
    // Interleave by decode time.
    let (mut i, mut j, mut adts) = (0usize, 0usize, 0i64);
    while i < vt.samples.len() || j < durs.len() {
        let vtime = vt.samples.get(i).map(|s| (s.dts - shift) as f64 / 15_360.0);
        let atime = (j < durs.len()).then(|| adts as f64 / 48_000.0);
        let take_video = match (vtime, atime) {
            (Some(x), Some(y)) => x <= y,
            (Some(_), None) => true,
            _ => false,
        };
        if take_video {
            let s = vt.samples[i];
            let data = &vb[s.offset as usize..s.offset as usize + s.size as usize];
            w.write_sample(
                0,
                &Sample {
                    data,
                    dts: s.dts - shift,
                    pts: s.pts - shift,
                    duration: s.duration,
                    sync: s.sync,
                },
            )
            .unwrap();
            i += 1;
        } else {
            let s = at.samples[j];
            let data = &ab[s.offset as usize..s.offset as usize + s.size as usize];
            w.write_sample(
                1,
                &Sample {
                    data,
                    dts: adts,
                    pts: adts,
                    duration: durs[j],
                    sync: true,
                },
            )
            .unwrap();
            adts += i64::from(durs[j]);
            j += 1;
        }
    }
    w.finish().unwrap().1
}

/// AudioToolbox AAC-LC 192k round trip via afconvert: encode `pcm` (48 kHz stereo) and
/// decode it again (afconvert honours the encoder's own priming tag). Scratch files are
/// removed.
pub fn aac_round_trip(dir: &Path, pcm: &[f32]) -> Result<Vec<f32>, String> {
    let wav_in = dir.join("rt-in.wav");
    let m4a = dir.join("rt.m4a");
    let wav_out = dir.join("rt-out.wav");
    gmclip_media::audio::wav::write_f32(std::fs::File::create(&wav_in).unwrap(), 48_000, 2, pcm)
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
    let out = gmclip_media::audio::wav::read(&wav_out)
        .map_err(|e| e.to_string())?
        .samples;
    for f in [&wav_in, &m4a, &wav_out] {
        let _ = std::fs::remove_file(f);
    }
    Ok(out)
}

/// Encodes a cut for muxing as the export does (SPEC §6.5 step 3): AudioToolbox AAC
/// (priming 2112) fed from cut sample 2112. Returns the m4a path.
pub fn aac_encode_cut(dir: &Path, cut: &[f32]) -> PathBuf {
    let feed = priming::encoder_feed(cut, 2, 2112, 48_000);
    let wav_in = dir.join("feed.wav");
    gmclip_media::audio::wav::write_f32(std::fs::File::create(&wav_in).unwrap(), 48_000, 2, &feed)
        .unwrap();
    let m4a = dir.join("cut.m4a");
    let st = Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac@48000", "-b", "192000", "-s", "0"])
        .arg(&wav_in)
        .arg(&m4a)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::remove_file(&wav_in).unwrap();
    m4a
}

/// Mixes a plan's audio from slices `tools/plan_slices.py` decoded into `dir/slices`.
pub fn mix_from_slices(dir: &Path, plan: &gmclip_plan::Plan) -> Vec<f32> {
    struct S(std::collections::HashMap<(String, u64), PathBuf>);
    impl gmclip_media::audio::PcmSource for S {
        fn read(&mut self, source: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
            let p = self.0.get(&(source.to_owned(), first)).ok_or("no slice")?;
            let pcm = gmclip_media::audio::wav::read(p).map_err(|e| e.to_string())?;
            out.copy_from_slice(pcm.samples.get(..out.len()).ok_or("short slice")?);
            Ok(())
        }
    }
    let s = S(plan
        .parts
        .iter()
        .map(|p| {
            (
                (p.audio.source.as_str().to_owned(), p.audio.first_sample),
                dir.join("slices").join(format!("{}.wav", p.id.as_str())),
            )
        })
        .collect());
    let mut s = s;
    gmclip_media::audio::AudioPlan::from_plan(plan)
        .unwrap()
        .mix(&mut s)
        .unwrap()
}

/// The files a golden cut's sound is read from: its frozen RenderPlan's
/// (`lab/fixtures/golden-11/<name>.renderplan.json`) audio sources, in the lab media folder.
pub fn golden_audio_sources(name: &str) -> Vec<PathBuf> {
    let p = gmclip_model::lab::fixture(format!("golden-11/{name}.renderplan.json"));
    let mut plan = gmclip_plan::parse(&std::fs::read(&p).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    plan.sources
        .iter()
        .filter(|s| plan.parts.iter().any(|p| p.audio.source == s.id))
        .map(|s| PathBuf::from(&s.path))
        .collect()
}
