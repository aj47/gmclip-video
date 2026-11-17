//! Media Foundation encode (M5) round trips through Media Foundation's own decoders:
//! AAC's priming is the constant the export compensates for, and its output matches the
//! input; H.264 comes back at high PSNR with every frame, the caller's timestamps and
//! keyframes exactly on the GOP grid. Synthetic input, no lab media.
#![cfg(windows)]

use gmclip_gpu::windows::{
    AAC_ENCODER_PRIMING, AacDecoder, AacEncoder, H264Decoder, H264Encoder, H264EncoderConfig,
};

/// A stereo test signal with no period a lag search could lock onto: a rising chirp on
/// the left, a falling one on the right, and a click at `click` on both channels.
fn signal(frames: usize, click: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames * 2);
    let tau = 2.0 * std::f64::consts::PI;
    for i in 0..frames {
        let t = i as f64 / 48_000.0;
        let l = 0.3 * (tau * (200.0 * t + 150.0 * t * t)).sin();
        let r = 0.3 * (tau * (900.0 * t - 100.0 * t * t)).sin();
        let c = if i == click { 0.6 } else { 0.0 };
        v.push((l + c) as f32);
        v.push((r + c) as f32);
    }
    v
}

#[test]
fn aac_round_trip_has_the_documented_priming() {
    let frames = 48_000 * 2;
    let pcm = signal(frames, 12_345);
    let mut enc = AacEncoder::new(48_000, 2, 192_000).unwrap();
    let asc = enc.asc().to_vec();
    assert_eq!(asc.len(), 2, "AAC-LC 48 kHz stereo ASC: {asc:02x?}");
    assert_eq!(asc, [0x11, 0x90], "AAC-LC, 48 kHz, 2 channels");
    enc.push(&pcm);
    let packets = enc.pull(true).unwrap();
    let mut dec = AacDecoder::new(&asc, 48_000, 2).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        dec.decode(p, &mut out).unwrap();
    }
    dec.drain(&mut out).unwrap();
    let bytes: usize = packets.iter().map(Vec::len).sum();
    let kbps = bytes as f64 * 8.0 / (frames as f64 / 48_000.0) / 1000.0;
    eprintln!(
        "{} packets, {} frames out, {kbps:.1} kbit/s",
        packets.len(),
        out.len() / 2
    );
    // the lag that best aligns output with input (left channel)
    let best = (0..4096usize)
        .map(|lag| {
            let e: f64 = (0..frames - 8192)
                .step_by(7)
                .map(|i| {
                    let d = f64::from(out[(i + lag) * 2]) - f64::from(pcm[i * 2]);
                    d * d
                })
                .sum();
            (lag, e)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    let lag = best.0;
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for i in 2048..frames - 8192 {
        for c in 0..2 {
            let a = f64::from(pcm[i * 2 + c]);
            let b = f64::from(out[(i + lag) * 2 + c]);
            sig += a * a;
            err += (a - b) * (a - b);
        }
    }
    let snr = 10.0 * (sig / err).log10();
    // the first block too: no priming means no garbage at the start
    let (mut s0, mut e0) = (0.0f64, 0.0f64);
    for i in 0..1024 {
        let a = f64::from(pcm[i * 2]);
        let b = f64::from(out[(i + lag) * 2]);
        s0 += a * a;
        e0 += (a - b) * (a - b);
    }
    let snr0 = 10.0 * (s0 / e0).log10();
    eprintln!("priming (lag) {lag}, SNR {snr:.1} dB, first block {snr0:.1} dB");
    assert_eq!(lag, AAC_ENCODER_PRIMING as usize, "encoder priming");
    assert!(snr > 30.0, "SNR {snr:.1} dB");
    assert!((150.0..230.0).contains(&kbps), "{kbps:.1} kbit/s");
    assert!(
        out.len() / 2 >= frames + lag - 1024,
        "the tail is flushed: {} of {}",
        out.len() / 2,
        frames + lag
    );
}

/// A moving picture: a diagonal gradient scrolling, chroma ramps.
fn picture(w: usize, h: usize, n: usize) -> (Vec<u8>, Vec<u8>) {
    let mut y = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            y[r * w + c] = (16 + ((c + r + n * 4) % 200)) as u8;
        }
    }
    let mut uv = vec![0u8; w * h / 2];
    for r in 0..h / 2 {
        for c in 0..w / 2 {
            uv[r * w + 2 * c] = (64 + (c + n) % 128) as u8;
            uv[r * w + 2 * c + 1] = (64 + (r + 2 * n) % 128) as u8;
        }
    }
    (y, uv)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
}

fn round_trip(w: u32, h: u32, b_frames: bool, hardware: bool) {
    let (wu, hu) = (w as usize, h as usize);
    let mut cfg = H264EncoderConfig::reels(w, h);
    cfg.b_frames = b_frames;
    cfg.gop = 30;
    let frames = 75usize;
    let ticks = 512i64; // 15360 / 30
    let mut enc = H264Encoder::open(&cfg, hardware).unwrap();
    assert_eq!(enc.is_hardware(), hardware);
    let t0 = std::time::Instant::now();
    let mut packets = Vec::new();
    for n in 0..frames {
        let (y, uv) = picture(wu, hu, n);
        enc.encode(&y, &uv, n as i64 * ticks, ticks, n % 30 == 0)
            .unwrap();
        while let Some(p) = enc.take() {
            packets.push(p);
        }
    }
    enc.finish().unwrap();
    while let Some(p) = enc.take() {
        packets.push(p);
    }
    let el = t0.elapsed();
    eprintln!(
        "{} {w}x{h} B={b_frames}: {} packets in {el:?} ({:.0} fps)",
        enc.name(),
        packets.len(),
        frames as f64 / el.as_secs_f64()
    );
    assert_eq!(packets.len(), frames, "every frame comes out");
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    let keys: Vec<i64> = packets
        .iter()
        .filter(|p| p.keyframe)
        .map(|p| p.pts / ticks)
        .collect();
    assert_eq!(keys, [0, 30, 60], "keyframes on the GOP grid");
    let reordered = pts.windows(2).any(|w| w[1] < w[0]);
    eprintln!("B-frames used: {}", enc.b_frames());
    assert_eq!(
        reordered,
        enc.b_frames() > 0,
        "decode order differs with B-frames only"
    );
    pts.sort_unstable();
    assert_eq!(
        pts,
        (0..frames as i64).map(|n| n * ticks).collect::<Vec<_>>()
    );
    // the stream's profile and level (the first SPS): High at the preset's level 4.0 from
    // Microsoft's encoder; NVIDIA's (through Media Foundation) ignores the level and takes
    // no B-frame count, so exports keep the software encoder (logged here)
    let sps = sps_of(&packets[0].data).expect("an SPS in the first IDR");
    eprintln!("profile_idc {}, level_idc {}", sps[1], sps[3]);
    assert_eq!(sps[1], 100, "High profile");
    if !hardware {
        assert_eq!(sps[3], 40, "level_idc");
        assert_eq!(enc.b_frames(), if b_frames { 2 } else { 0 });
    }
    // every IDR starts with its parameter sets (Annex B: SPS then PPS)
    for p in packets.iter().filter(|p| p.keyframe) {
        let nals = nal_types(&p.data);
        assert!(
            nals.contains(&7) && nals.contains(&8) && nals.contains(&5),
            "{nals:?}"
        );
    }
    // decode and compare
    let mut dec = H264Decoder::new(w, h).unwrap();
    let mut got = Vec::new();
    for p in &packets {
        dec.decode(&p.data, p.pts).unwrap();
        while let Some(f) = dec.take() {
            got.push(f);
        }
    }
    dec.drain().unwrap();
    while let Some(f) = dec.take() {
        got.push(f);
    }
    assert_eq!(got.len(), frames);
    let mut worst = f64::INFINITY;
    for f in &got {
        let n = (f.time / ticks) as usize;
        assert_eq!((f.width, f.height), (w, h));
        let (y, uv) = picture(wu, hu, n);
        worst = worst.min(psnr(&y, &f.y)).min(psnr(&uv, &f.uv) + 0.0);
    }
    eprintln!("{w}x{h}: worst PSNR {worst:.1} dB");
    assert!(worst > 35.0, "worst PSNR {worst:.1} dB");
}

/// The first SPS NAL unit (header byte included) of an Annex B buffer.
fn sps_of(annex_b: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            if annex_b[i + 3] & 0x1f == 7 {
                return annex_b.get(i + 3..i + 7);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

fn nal_types(annex_b: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            v.push(annex_b[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

#[test]
fn h264_portrait_with_b_frames_round_trips() {
    round_trip(1080, 1920, true, false);
}

#[test]
fn h264_landscape_1080_without_b_frames_round_trips() {
    round_trip(1920, 1080, false, false);
}

#[test]
#[ignore = "needs a GPU with a hardware H.264 encoder"]
fn h264_on_the_gpu_round_trips() {
    round_trip(1080, 1920, true, true);
    round_trip(1920, 1080, false, true);
}
