//! H-09 (AAC-LC 192k encode, priming D read from the encoder, round trip lag 0) and
//! H-04 (AAC decode honouring edit list / priming: lag 0 and max |Δ| ≤ 2⁻¹⁵ against
//! libavcodec) — lab: needs AudioToolbox and ffmpeg.
#![cfg(target_os = "macos")]

mod support;

use std::process::Command;

use gmclip_gpu::apple::AacDecoder;
use gmclip_media::MediaError;
use gmclip_media::backend::AudioSampleSink;
use gmclip_media::backend::SampleSource;
use gmclip_media::backend::vt::{AacTrackDecoder, AacTrackEncoder};
use gmclip_media::index::SampleIdx;
use support::*;

const RATE: u32 = 48_000;

/// Speech-like test mix: a chirp, two tones with tremolo, and band noise; stereo with
/// different content per channel.
fn mix(secs: f32) -> Vec<f32> {
    let n = (secs * RATE as f32) as usize;
    let mut rng = Rng(11);
    let mut out = Vec::with_capacity(n * 2);
    let mut lp = 0.0f32;
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let chirp = (2.0 * std::f32::consts::PI * (200.0 * t + 300.0 * t * t)).sin();
        let trem = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
        let tone = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * trem;
        let white = (rng.next() as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0;
        lp += 0.2 * (white - lp);
        out.push(0.3 * chirp + 0.2 * tone + 0.1 * lp);
        out.push(0.3 * tone + 0.15 * chirp - 0.1 * lp);
    }
    out
}

fn mono(x: &[f32]) -> Vec<f32> {
    x.chunks_exact(2).map(|c| 0.5 * (c[0] + c[1])).collect()
}

/// The lag (in samples) at which `test` best matches `reference` over a window.
fn xcorr_lag(reference: &[f32], test: &[f32], start: usize, len: usize, max_lag: isize) -> isize {
    let (r, t) = (mono(reference), mono(test));
    let mut best = (f64::MIN, 0isize);
    for lag in -max_lag..=max_lag {
        let mut acc = 0f64;
        for i in start..start + len {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < t.len() && i < r.len() {
                acc += f64::from(r[i]) * f64::from(t[j as usize]);
            }
        }
        if acc > best.0 {
            best = (acc, lag);
        }
    }
    best.1
}

#[derive(Default)]
struct Packets {
    asc: Vec<u8>,
    packets: Vec<(Vec<u8>, i64)>,
}

impl AudioSampleSink for Packets {
    fn audio_config(&mut self, asc: &[u8], _sr: u32, _ch: u32) -> Result<(), MediaError> {
        self.asc = asc.to_vec();
        Ok(())
    }
    fn audio_sample(&mut self, data: &[u8], pts: i64, duration: u32) -> Result<(), MediaError> {
        assert_eq!(duration, 1024);
        self.packets.push((data.to_vec(), pts));
        Ok(())
    }
}

/// ADTS framing (dev-only) so ffmpeg can decode raw AAC without a container.
fn adts(asc: &[u8], packets: &[(Vec<u8>, i64)]) -> Vec<u8> {
    let object = (asc[0] >> 3) - 1; // profile = audioObjectType - 1
    let freq = ((asc[0] & 7) << 1) | (asc[1] >> 7);
    let chan = (asc[1] >> 3) & 0xf;
    let mut out = Vec::new();
    for (p, _) in packets {
        let len = p.len() + 7;
        out.extend_from_slice(&[
            0xff,
            0xf1,
            (object << 6) | (freq << 2) | (chan >> 2),
            ((chan & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1f,
            0xfc,
        ]);
        out.extend_from_slice(p);
    }
    out
}

fn ffmpeg_f32(path: &std::path::Path, extra: &[&str]) -> Vec<f32> {
    let raw = run(Command::new(FFMPEG)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(extra)
        .args(["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-"]));
    raw.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs AudioToolbox + ffmpeg (run with --features lab)"
)]
fn h09_aac_encode_round_trip_lag_zero() {
    let input = mix(6.0);
    let mut enc = AacTrackEncoder::new(RATE, 2, 192_000).unwrap();
    let d = enc.priming();
    let mut sink = Packets::default();
    for chunk in input.chunks(2 * 4800) {
        enc.push(chunk, &mut sink).unwrap();
    }
    enc.finish(&mut sink).unwrap();
    let bytes: usize = sink.packets.iter().map(|p| p.0.len()).sum();
    let kbps = bytes as f64 * 8.0 / (input.len() / 2) as f64 * f64::from(RATE) / 1000.0;
    assert_eq!(sink.packets[0].1, 0, "first audio packet at 0");

    // Apple's decoder.
    let mut dec = AacDecoder::new(&sink.asc, RATE, 2).unwrap();
    let mut out = Vec::new();
    for (p, _) in &sink.packets {
        dec.decode(p, &mut out).unwrap();
    }
    let lag_at = xcorr_lag(&input, &out, RATE as usize, RATE as usize, 4096);
    // libavcodec, through ADTS.
    let path = lab_dir().join("h09-roundtrip.aac");
    std::fs::write(&path, adts(&sink.asc, &sink.packets)).unwrap();
    let ff = ffmpeg_f32(&path, &[]);
    let _ = std::fs::remove_file(&path);
    let lag_ff = xcorr_lag(&input, &ff, RATE as usize, RATE as usize, 4096);
    eprintln!(
        "H-09: priming D = {d} (kAudioConverterPrimeInfo); {} packets, {kbps:.1} kbps; ASC {:02x?}; round-trip lag AudioToolbox {lag_at}, libavcodec {lag_ff}",
        sink.packets.len(),
        sink.asc
    );
    assert_eq!(d, 2112, "AudioToolbox AAC-LC priming");
    assert_eq!(lag_at, 0);
    assert_eq!(lag_ff, 0);
    assert!(
        (186.0..198.0).contains(&kbps),
        "{kbps} kbps (CBR 192 ± 3 %)"
    );
    // Every mix sample is covered, and the content after the priming + fade matches.
    let n = input.len() / 2;
    assert_eq!(enc.mix_samples() as usize, n);
    assert!(out.len() / 2 >= n, "decoded {} < mix {n}", out.len() / 2);
    let fade = (RATE / 200) as usize;
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 2 * (d as usize + fade)..2 * n {
        sig += f64::from(input[i]).powi(2);
        err += f64::from(input[i] - out[i]).powi(2);
    }
    let snr = 10.0 * (sig / err).log10();
    eprintln!("H-09: SNR over [D + 5 ms, N) = {snr:.1} dB, per channel included");
    assert!(snr > 20.0, "SNR {snr:.1} dB");
}

fn max_abs_diff(a: &[f32], b: &[f32], start: usize, len: usize) -> f32 {
    a[start..start + len]
        .iter()
        .zip(&b[start..start + len])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Decodes `secs` of a file's first audio track with AudioToolbox, honouring the edit
/// list (skip = −first PTS), and compares with libavcodec.
fn decode_vs_ffmpeg(path: &str, label: &str, secs: Option<f64>) -> (isize, f32, u64) {
    let p = probe(path, "a:0");
    let first_pts = p.index.dts[0] + i64::from(p.index.cts_off[0]);
    assert!(
        first_pts <= 0,
        "an empty edit (leading delay) is not supported in M0"
    );
    let skip = (-first_pts) as u64;
    // The edit list's segment duration, as libavformat reads it (shim until B-01).
    let valid = if secs.is_none() {
        let d = String::from_utf8(run(Command::new(FFPROBE)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=duration_ts",
                "-of",
                "csv=p=0",
            ])
            .arg(path)))
        .unwrap();
        d.trim().parse::<u64>().ok()
    } else {
        None
    };
    let mut src = FileSource::open(path, p.index.clone());
    let mut dec = AacTrackDecoder::new(&p.extradata, RATE, 2, skip, valid).unwrap();
    let n_packets = match secs {
        Some(s) => ((s * f64::from(RATE)) as usize / 1024 + 4).min(p.index.len()),
        None => p.index.len(),
    };
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for i in 0..n_packets {
        src.read(SampleIdx(i as u32), &mut buf).unwrap();
        dec.decode(&buf, &mut out).unwrap();
    }
    let extra: Vec<String> = match secs {
        Some(s) => vec!["-t".into(), format!("{s}")],
        None => vec![],
    };
    let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
    let ff = ffmpeg_f32(std::path::Path::new(path), &extra);
    if let Some(v) = valid {
        // The edit list's segment duration is honoured (the source mix was 8 s =
        // 384,000 samples). libavformat does not trim the trailing padding, so its
        // output is longer; the comparison below covers the common length.
        assert_eq!(
            out.len() as u64 / 2,
            v,
            "{label}: decoded length = edit-list duration"
        );
        assert_eq!(v, 384_000, "{label}: edit-list duration = the mix");
    }
    let n = (out.len().min(ff.len()) / 2).saturating_sub(2048);
    let win = n
        .checked_sub(2048)
        .expect("decode long enough for the xcorr window");
    let lag = xcorr_lag(&ff, &out, 2048, win.min(RATE as usize), 2048);
    let diff = max_abs_diff(&ff, &out, 0, 2 * n);
    eprintln!(
        "H-04 {label}: skip {skip} (first pts {first_pts}), {} frames decoded, lag {lag}, max |Δ| {diff:.3e} (2^-15 = {:.3e})",
        out.len() / 2,
        2f32.powi(-15)
    );
    (lag, diff, skip)
}

/// An AAC file with an edit list, as ffmpeg's AudioToolbox (D = 2112) or native
/// (D = 1024) encoder writes it. The native encoder runs with perceptual noise
/// substitution and intensity stereo off: PNS bands are filled with each decoder's own
/// random noise, so two correct decoders differ there by ~1e-2 (measured), which says
/// nothing about alignment.
fn m4a_with_edit_list(encoder: &str) -> String {
    let p = lab_dir().join(format!("h04-elst-{encoder}-nopns.m4a"));
    if !p.exists() {
        let input = lab_dir().join("h04-mix.wav");
        let pcm = mix(8.0);
        let mut wav = Vec::new();
        let data_len = (pcm.len() * 4) as u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&RATE.to_le_bytes());
        wav.extend_from_slice(&(RATE * 8).to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in &pcm {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&input, wav).unwrap();
        run(Command::new(FFMPEG)
            .args(["-v", "error", "-y", "-i"])
            .arg(&input)
            .args(["-c:a", encoder, "-b:a", "192k"])
            .args(if encoder == "aac" {
                &["-aac_pns", "0", "-aac_is", "0"][..]
            } else {
                &[][..]
            })
            .arg(&p));
    }
    p.to_string_lossy().into_owned()
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs AudioToolbox + ffmpeg (run with --features lab)"
)]
fn h04_decode_honours_edit_list() {
    for (enc, d) in [("aac_at", 2112u64), ("aac", 1024)] {
        let path = m4a_with_edit_list(enc);
        let (lag, diff, skip) = decode_vs_ffmpeg(&path, &format!("elst-{enc}"), None);
        assert_eq!(skip, d, "edit list media_time");
        assert_eq!(lag, 0);
        assert!(diff <= 2f32.powi(-15), "{diff}");
    }
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs AudioToolbox + ffmpeg + AJ's media"
)]
fn h04_decode_real_sources() {
    let set = gmclip_model::lab::present(
        [
            (mmo_1440p(), "mmo-1440p"),
            (proxy_4fadb756(), "proxy-loc_4fadb756"),
        ],
        |s| s.1.to_owned(),
        |s| vec![s.0],
    );
    if set.skip_if_empty() {
        return;
    }
    eprintln!("H-04 real sources: {}", set.note("sources"));
    for (path, label) in set.items {
        let (lag, diff, _) = decode_vs_ffmpeg(path, label, Some(120.0));
        assert_eq!(lag, 0, "{label}");
        assert!(diff <= 2f32.powi(-15), "{label}: {diff}");
    }
}

/// H-10's `AacRandomReader`: reads after seeks (two packets of pre-roll) equal a
/// sequential decode from the start, sample for sample.
#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs AudioToolbox + AJ's media")]
fn random_reader_matches_sequential_decode() {
    use gmclip_media::backend::vt::AacRandomReader;
    let path = proxy_4fadb756();
    let p = probe(path, "a:0");
    let n_packets = 1500usize; // 32 s
    let mut seq = Vec::new();
    {
        let mut src = FileSource::open(path, p.index.clone());
        let mut dec = AacTrackDecoder::new(&p.extradata, RATE, 2, 0, None).unwrap();
        let mut buf = Vec::new();
        for i in 0..n_packets {
            src.read(SampleIdx(i as u32), &mut buf).unwrap();
            dec.decode(&buf, &mut seq).unwrap();
        }
    }
    let mut r = AacRandomReader::new(
        FileSource::open(path, p.index.clone()),
        &p.extradata,
        RATE,
        2,
        0,
    )
    .unwrap();
    let mut worst = 0f32;
    for (first, len) in [
        (1_000_000u64, 4800usize),
        (5_000u64, 2000),
        (5_000 + 2000, 48_000),
        (700_123, 1601),
        (1_400_000, 3000),
        (0, 512),
    ] {
        let mut out = vec![0f32; len * 2];
        r.read(first, &mut out).unwrap();
        let want = &seq[first as usize * 2..(first as usize + len) * 2];
        let d = out
            .iter()
            .zip(want)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        worst = worst.max(d);
    }
    eprintln!(
        "H-10 AacRandomReader: {} seeks; worst |Δ| vs sequential {worst:.3e}",
        r.seeks
    );
    assert!(worst <= 1e-6, "{worst}");
    // an empty edit longer than the priming: silence, then the decode from its start
    let mut r = AacRandomReader::new(
        FileSource::open(path, p.index.clone()),
        &p.extradata,
        RATE,
        2,
        0,
    )
    .unwrap()
    .with_lead(700);
    let mut out = vec![1f32; 1212 * 2];
    r.read(0, &mut out).unwrap();
    assert!(out[..1400].iter().all(|&x| x == 0.0));
    let d = out[1400..]
        .iter()
        .zip(&seq[..1024])
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(d <= 1e-6, "{d}");
    let mut late = vec![0f32; 100 * 2];
    r.read(800, &mut late).unwrap();
    assert!(
        late.iter()
            .zip(&seq[200..400])
            .all(|(a, b)| (a - b).abs() <= 1e-6)
    );
}
