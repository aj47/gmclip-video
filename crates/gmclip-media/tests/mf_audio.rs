//! Media Foundation AAC decode (M5) against Apple AudioToolbox's decode of the same
//! file (the decoder gmclip uses on the Mac). Lab media (never committed) in
//! `$GMCLIP_LAB_MEDIA`: `h264_high_aac.mp4` and `ref/h264_high_aac.audiotoolbox.f32.wav`
//! (float32 LE, 48 kHz stereo, the edit list honoured: exactly 8.000 s). Two conforming
//! decoders are not bit-identical: the gate is the SNR and the length.
#![cfg(windows)]

mod support;

use std::path::PathBuf;

use gmclip_media::backend::SampleSource;
use gmclip_media::backend::aac::{AacRandomReader, AacTrackDecoder};
use gmclip_media::demux::{self, Codec, TrackKind};
use gmclip_media::index::SampleIdx;

fn lab(name: &str) -> Option<PathBuf> {
    // the lab media folder, else where the labpack is generated
    let dir = std::env::var_os("GMCLIP_LAB_MEDIA")
        .filter(|d| !d.is_empty())
        .map_or_else(support::lab_dir, PathBuf::from);
    let p = dir.join(name);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: no {} (set GMCLIP_LAB_MEDIA)", p.display());
        None
    }
}

/// The data chunk of a float32 WAV as samples.
fn wav_f32(p: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(p).unwrap();
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            return b[i + 8..(i + 8 + len).min(b.len())]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
        }
        i += 8 + len + (len & 1);
    }
    panic!("no data chunk in {}", p.display())
}

struct Aac {
    src: support::FileSource,
    asc: Vec<u8>,
    rate: u32,
    channels: u32,
    skip: u64,
    valid: Option<u64>,
}

fn open(path: &std::path::Path) -> Aac {
    let mut f = demux::io::FileSource::open(path).unwrap();
    let d = demux::index(&mut f).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("an audio track");
    let Codec::Aac { asc } = &t.codec else {
        panic!("not AAC: {:?}", t.codec)
    };
    // the edit list: priming to skip (media ticks = samples) and the valid length
    let (skip, valid) = match t.edits.first() {
        Some(e) if e.media_time >= 0 && t.timescale == t.sample_rate => (
            e.media_time as u64,
            Some(e.segment_duration * u64::from(t.sample_rate) / u64::from(t.movie_timescale)),
        ),
        _ => (0, None),
    };
    eprintln!(
        "AAC {} Hz × {}, {} packets, asc {:02x?}, edit skip {skip} valid {valid:?}",
        t.sample_rate,
        t.channels,
        t.samples.offset.len(),
        asc
    );
    Aac {
        src: support::FileSource::open(path.to_str().unwrap(), t.samples.clone()),
        asc: asc.clone(),
        rate: t.sample_rate,
        channels: u32::from(t.channels),
        skip,
        valid,
    }
}

fn snr_db(want: &[f32], got: &[f32]) -> (f64, f32) {
    let (mut sig, mut err, mut worst) = (0f64, 0f64, 0f32);
    for (a, b) in want.iter().zip(got) {
        sig += f64::from(*a) * f64::from(*a);
        let d = a - b;
        err += f64::from(d) * f64::from(d);
        worst = worst.max(d.abs());
    }
    (10.0 * (sig / err.max(1e-30)).log10(), worst)
}

#[test]
fn mf_aac_matches_audiotoolbox() {
    let (Some(mp4), Some(wav)) = (
        lab("h264_high_aac.mp4"),
        lab("ref/h264_high_aac.audiotoolbox.f32.wav"),
    ) else {
        return;
    };
    let want = wav_f32(&wav);
    let mut a = open(&mp4);
    let mut dec = AacTrackDecoder::new(&a.asc, a.rate, a.channels, a.skip, a.valid).unwrap();
    let mut got = Vec::new();
    let mut buf = Vec::new();
    let t = std::time::Instant::now();
    for k in 0..a.src.index().len() {
        a.src.read(SampleIdx(k as u32), &mut buf).unwrap();
        dec.decode(&buf, &mut got).unwrap();
    }
    dec.finish(&mut got).unwrap();
    let secs = got.len() as f64 / 2.0 / f64::from(a.rate);
    eprintln!(
        "MF decoded {} frames ({secs:.3} s) in {:?}; AudioToolbox {} frames",
        got.len() / 2,
        t.elapsed(),
        want.len() / 2
    );
    let (db, worst) = snr_db(&want, &got);
    // the lag that best aligns them, in case the decoders' delays differ
    let best = (-2048i64..=2048)
        .map(|lag| {
            let (w, g) = if lag >= 0 {
                (&want[..], &got[(lag as usize * 2).min(got.len())..])
            } else {
                (&want[(-lag) as usize * 2..], &got[..])
            };
            let n = w.len().min(g.len()).min(96_000);
            (lag, snr_db(&w[..n], &g[..n]).0)
        })
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap();
    eprintln!(
        "vs AudioToolbox: SNR {db:.1} dB, max |Δ| {worst:.2e}; best alignment lag {} samples ({:.1} dB)",
        best.0, best.1
    );
    assert_eq!(got.len(), want.len(), "the edit list's length");
    assert_eq!(best.0, 0, "MF and AudioToolbox disagree on the priming");
    assert!(
        db >= 60.0,
        "MF's decode differs from AudioToolbox's: {db:.1} dB"
    );
}

/// `AacRandomReader` on MF: reads after seeks (two packets of pre-roll, the flush in
/// between) equal the sequential decode.
#[test]
fn mf_random_reads_equal_the_sequential_decode() {
    let Some(mp4) = lab("h264_high_aac.mp4") else {
        return;
    };
    let mut a = open(&mp4);
    let mut seq = Vec::new();
    {
        let mut dec = AacTrackDecoder::new(&a.asc, a.rate, a.channels, a.skip, None).unwrap();
        let mut buf = Vec::new();
        for k in 0..a.src.index().len() {
            a.src.read(SampleIdx(k as u32), &mut buf).unwrap();
            dec.decode(&buf, &mut seq).unwrap();
        }
        dec.finish(&mut seq).unwrap();
    }
    let a = open(&mp4);
    let ch = a.channels as usize;
    let mut r = AacRandomReader::new(a.src, &a.asc, a.rate, a.channels, a.skip).unwrap();
    let total = (seq.len() / ch) as u64;
    let mut worst = 0f32;
    let mut rng = support::Rng(7);
    for _ in 0..60 {
        let first = rng.next() % (total - 4096);
        let n = 64 + (rng.next() % 3000) as usize;
        let mut out = vec![0f32; n * ch];
        r.read(first, &mut out).unwrap();
        let want = &seq[first as usize * ch..(first as usize + n) * ch];
        for (x, y) in want.iter().zip(&out) {
            worst = worst.max((x - y).abs());
        }
    }
    eprintln!(
        "MF AacRandomReader: {} seeks; worst |Δ| vs sequential {worst:.3e}",
        r.seeks
    );
    assert!(r.seeks > 10);
    assert!(worst < 1e-4, "{worst}");
}
