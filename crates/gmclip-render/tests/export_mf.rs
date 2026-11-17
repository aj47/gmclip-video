//! M5 (lab): the export on Windows — Media Foundation decode, the wgpu compositor (DX12
//! or Vulkan), NV12 read back, Media Foundation H.264 and AAC encode, the shared mux,
//! verify and install. A 4-s, three-part plan derived from
//! `schemas/examples/native-mode.renderplan.json` over the lab pack's 1080p30 H.264
//! source (a seek, a held frame, two layers, static loudness).
//!
//! Checks: the export verifies and installs; the written file has the plan's frame
//! count, IDRs exactly on the GOP grid and 4 s of audio; decoded back through Media
//! Foundation, every frame matches `render_frames` (the same compose, before the
//! encoder) at high PSNR; the encoder input is reproducible. Lab media (never
//! committed) in `$GMCLIP_LAB_MEDIA`; needs a GPU, so ignored by default:
//!
//!   cargo test --release -p gmclip-render --test export_mf -- --ignored --nocapture
#![cfg(windows)]

use gmclip_media::backend::mf::MfH264Decoder;
use gmclip_media::decode::{EncodedSample, FrameImage, SubmitFlags, VideoDecoder};
use gmclip_media::demux::{self, Codec, TrackKind};
use gmclip_media::index::SampleIdx;
use gmclip_render::export::{DemuxInputs, ExportOptions, Inputs, export, render_frames};

mod mf_lab;

use mf_lab::{FRAMES, lab, plan};

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
}

/// RGB → Y′ (BT.709, video range) for comparing the encoded file with the stills.
fn luma_of(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| {
            let (r, g, b) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
            (16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0).round() as u8
        })
        .collect()
}

#[test]
#[ignore = "lab: needs a GPU, Media Foundation and $GMCLIP_LAB_MEDIA"]
fn three_part_plan_exports_verifies_and_decodes_back() {
    let Some(src) = lab() else { return };
    let plan = plan(&src);
    let dir = std::env::temp_dir().join(format!("gmclip-export-mf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("native.mp4");
    let opts = ExportOptions {
        hash_encoder_input: true,
        ..ExportOptions::default()
    };
    let rep = export(&plan, &mut DemuxInputs::new(None), &out, &opts).unwrap();
    eprintln!(
        "exported {} frames in {:.2} s ({:.0} fps), peak {:.2} Mbps, rung {}, {} seeks",
        rep.frames,
        rep.seconds,
        rep.frames as f64 / rep.seconds,
        rep.video_peak_bps as f64 / 1e6,
        rep.rate_rung,
        rep.video_seeks
    );
    for n in &rep.notes {
        eprintln!("  note: {n}");
    }
    assert_eq!(rep.frames, FRAMES);
    assert!(
        rep.notes.iter().any(|n| n.starts_with("verified:")),
        "{:?}",
        rep.notes
    );
    assert_eq!(rep.encoder_input_hashes.len() as u64, FRAMES);

    // the file: frame count, IDRs on the GOP grid, the audio's length
    let bytes = std::fs::read(&out).unwrap();
    let mut s = demux::io::SliceSource(&bytes);
    let d = demux::index(&mut s).unwrap();
    let v = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap();
    let a = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap();
    assert_eq!(v.samples.len() as u64, FRAMES);
    assert_eq!((v.width, v.height), (1080, 1920));
    let mut pts: Vec<(i64, usize)> = (0..v.samples.len())
        .map(|i| (v.samples.dts[i] + i64::from(v.samples.cts_off[i]), i))
        .collect();
    pts.sort_unstable();
    let keys: Vec<usize> = pts
        .iter()
        .enumerate()
        .filter(|(_, (_, i))| v.samples.sync[*i])
        .map(|(n, _)| n)
        .collect();
    assert_eq!(keys, [0, 30, 60, 90], "IDRs on the GOP grid");
    for i in 0..v.samples.len() {
        assert!(
            v.samples.dts[i] <= v.samples.dts[i] + i64::from(v.samples.cts_off[i]),
            "DTS ≤ PTS"
        );
    }
    let audio_packets = a.samples.len();
    assert_eq!(audio_packets, (FRAMES as usize * 1600).div_ceil(1024));

    // decoded back through Media Foundation, against the stills of the same compose
    let Codec::H264 { avcc } = &v.codec else {
        panic!("not H.264")
    };
    let mut dec = MfH264Decoder::new(avcc, 1080, 1920).unwrap();
    let mut decoded: Vec<(i64, Vec<u8>)> = Vec::new();
    for i in 0..v.samples.len() {
        let (o, n) = (v.samples.offset[i] as usize, v.samples.size[i] as usize);
        dec.submit(
            &EncodedSample {
                sample: SampleIdx(i as u32),
                data: &bytes[o..o + n],
                pts: v.samples.dts[i] + i64::from(v.samples.cts_off[i]),
                dts: v.samples.dts[i],
                sync: v.samples.sync[i],
            },
            SubmitFlags::default(),
        )
        .unwrap();
        while let Some(f) = dec.poll().unwrap() {
            let FrameImage::Cpu { y, .. } = f.image;
            decoded.push((f.pts, y));
        }
    }
    dec.flush().unwrap();
    while let Some(f) = dec.poll().unwrap() {
        let FrameImage::Cpu { y, .. } = f.image;
        decoded.push((f.pts, y));
    }
    decoded.sort_by_key(|f| f.0);
    assert_eq!(decoded.len() as u64, FRAMES);
    let at: Vec<u64> = vec![0, 29, 30, 44, 45, 60, 89, 90, 119];
    let stills = render_frames(&plan, &mut DemuxInputs::new(None), &at).unwrap();
    let mut worst = f64::INFINITY;
    for (f, img) in &stills {
        let want = luma_of(&img.px);
        let got = &decoded[*f as usize].1;
        let p = psnr(&want, got);
        eprintln!("frame {f}: Y PSNR {p:.1} dB");
        worst = worst.min(p);
    }
    assert!(worst > 32.0, "worst Y PSNR {worst:.1} dB");

    // the compositor's output is reproducible (decision 0005)
    let again = export(
        &plan,
        &mut DemuxInputs::new(None),
        &dir.join("again.mp4"),
        &opts,
    )
    .unwrap();
    assert_eq!(again.encoder_input_hashes, rep.encoder_input_hashes);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The voice mix captions are heard from (SPEC §5.10) on Windows: part 2 has no gain and
/// no fades, so its output range is the source's own decoded samples; range reads agree
/// with one whole read (within the AAC seek tolerance).
#[test]
#[ignore = "lab: needs Media Foundation and $GMCLIP_LAB_MEDIA"]
fn voice_mix_reads_the_cut_audio() {
    use gmclip_media::backend::aac::AacTrackDecoder;
    use gmclip_render::export::VoiceMix;
    let Some(src) = lab() else { return };
    let plan = plan(&src);
    let mut mix = VoiceMix::open(&plan, &mut DemuxInputs::new(None)).unwrap();
    let total = mix.total();
    assert_eq!(total, FRAMES * 1600);
    let mut whole = vec![0f32; (total * 2) as usize];
    mix.read(0, &mut whole).unwrap();
    for (a, n) in [
        (0u64, 4800u64),
        (71_000, 3000),
        (143_500, 1000),
        (191_000, 1000),
    ] {
        let mut part = vec![0f32; (n * 2) as usize];
        mix.read(a, &mut part).unwrap();
        // a range starts its AAC decode at a seek: within the seek tolerance of one
        // straight decode (`gmclip-media/tests/mf_audio.rs`: 3.5e-5)
        let w = &whole[(a * 2) as usize..((a + n) * 2) as usize];
        let max = part
            .iter()
            .zip(w)
            .map(|(x, y)| (x - y).abs())
            .fold(0f32, f32::max);
        eprintln!("range {a}+{n}: max |Δ| {max:.2e} against one whole read");
        assert!(max < 1e-4, "range at {a}: max |Δ| {max}");
    }
    // the source's samples 240000.. (frame 150), decoded on their own (the skip from its
    // edit list, as the export opens it)
    let mut a = DemuxInputs::new(None).audio(&plan, "a-prime").unwrap();
    let mut dec = AacTrackDecoder::new(&a.asc, 48_000, 2, a.skip, None).unwrap();
    let mut pcm = Vec::new();
    let mut buf = Vec::new();
    for i in 0..a.source.index().len() {
        a.source.read(SampleIdx(i as u32), &mut buf).unwrap();
        dec.decode(&buf, &mut pcm).unwrap();
    }
    dec.finish(&mut pcm).unwrap();
    let got = &whole[72_000 * 2..144_000 * 2];
    let want = &pcm[240_000 * 2..312_000 * 2];
    let max = got
        .iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    eprintln!("voice mix part 2 against the source: max |Δ| {max:.2e}");
    assert!(max < 1e-4, "max |Δ| {max}");
}

/// 10-bit sources (P010 into 16-bit planes, macOS's code scale): the lab pack's 8-bit and
/// 10-bit VP9 files are the same ffmpeg testsrc2 frames, so the plan's pictures from
/// either render alike (a wrong code scale shifts every level).
#[test]
#[ignore = "lab: needs a GPU, Media Foundation and $GMCLIP_LAB_MEDIA"]
fn ten_bit_pictures_render_like_eight_bit() {
    let Some(src) = lab() else { return };
    let dir = src.parent().unwrap().to_path_buf();
    let (v8, v10) = (dir.join("vp9.mp4"), dir.join("vp9_10bit.mp4"));
    if !v8.is_file() || !v10.is_file() {
        eprintln!("skipped: no VP9 lab files");
        return;
    }
    // the plan's audio stays on the H.264 file; its pictures come from a VP9 file
    let with = |video: &std::path::Path| {
        let mut v: serde_json::Value = serde_json::to_value(plan(&src)).unwrap();
        v["sources"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"id": "pic", "path": video.to_str().unwrap()}));
        for (k, first) in [(0usize, 0u64), (1, 60), (2, 30)] {
            let p = &mut v["parts"][k];
            p["picture"] = serde_json::json!({"source": "pic", "first_frame": first});
            for l in p["layers"].as_array_mut().unwrap() {
                if l["time_map"]["mode"] == "hold" {
                    l["time_map"]["frame"] = 70.into();
                }
                if l.get("picture_source").is_some() {
                    l["picture_source"] = "pic".into();
                }
            }
        }
        let p = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
        let errors = gmclip_plan::check_plan(&p);
        assert!(errors.is_empty(), "{errors:?}");
        p
    };
    let at = [0u64, 50, 100];
    let a = render_frames(&with(&v8), &mut DemuxInputs::new(None), &at).unwrap();
    let b = render_frames(&with(&v10), &mut DemuxInputs::new(None), &at).unwrap();
    for ((f, x), (_, y)) in a.iter().zip(&b) {
        let p = psnr(&x.px, &y.px);
        let mean = |v: &[u8]| v.iter().map(|&c| f64::from(c)).sum::<f64>() / v.len() as f64;
        eprintln!(
            "frame {f}: 8-bit vs 10-bit source PSNR {p:.1} dB (means {:.1} / {:.1})",
            mean(&x.px),
            mean(&y.px)
        );
        assert!(p > 30.0, "frame {f}: {p:.1} dB");
        assert!(
            (mean(&x.px) - mean(&y.px)).abs() < 2.0,
            "frame {f}: level shift"
        );
    }
}

/// The YouTube 1440p preset at 60 fps (K3/K7: H.264 High 5.1, VBR 36 Mbit/s, peak 54,
/// GOP ½ s) on this PC's Media Foundation encoder: the lab pack's 60 fps clip scaled to a
/// 2560×1440 canvas over three parts (a seek, a held screen, a freeze), as
/// `planner::preset_plan` sets the output. Prints and checks what the file carries: its
/// level (avcC), its average and peak rate, and whether the encoder refused the rate and
/// the export fell back (`encoder: refused …` note, 24/25 Mbit/s).
#[test]
#[ignore = "lab: needs a GPU, Media Foundation and $GMCLIP_LAB_MEDIA"]
fn youtube_1440p60_preset_on_the_mf_encoder() {
    let Some(dir) = std::env::var_os("GMCLIP_LAB_MEDIA").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set GMCLIP_LAB_MEDIA");
        return;
    };
    let src = dir.join("h264_main_60fps.mp4");
    if !src.is_file() {
        eprintln!("skipped: no {}", src.display());
        return;
    }
    // the source as the plan must expect it
    let bytes = std::fs::read(&src).unwrap();
    let d = demux::index(&mut demux::io::SliceSource(&bytes)).unwrap();
    let sv = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap();
    let sa = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap();
    let (fps, frames) = (60u64, 240u64);
    let tick = u64::from(sv.timescale) / fps;
    let per_frame = u64::from(sa.sample_rate) / fps;

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text =
        std::fs::read_to_string(root.join("schemas/examples/native-mode.renderplan.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let o = &mut v["output"];
    o["canvas"] = serde_json::json!({"w": 2560, "h": 1440});
    o["fps"] = serde_json::json!({"num": fps, "den": 1});
    o["frames"] = frames.into();
    // the preset's video (planner::preset_plan for Youtube1440 above 30 fps)
    o["video"]["level"] = "5.1".into();
    o["video"]["gop_frames"] = (fps / 2).into();
    o["video"]["rate"] =
        serde_json::json!({"mode": "vbr", "target_bps": 36_000_000, "max_bps": 54_000_000});
    let s = &mut v["sources"][0];
    s["path"] = src.to_str().unwrap().into();
    s["expect"]["video"]["w"] = sv.width.into();
    s["expect"]["video"]["h"] = sv.height.into();
    s["expect"]["video"]["fps"] = serde_json::json!({"num": fps, "den": 1});
    s["expect"]["video"]["timescale"] = sv.timescale.into();
    s["expect"]["audio"]["rate"] = sa.sample_rate.into();
    s["expect"]["audio"]["channels"] = sa.channels.into();
    let full = serde_json::json!({"x": 0, "y": 0, "w": sv.width, "h": sv.height});
    let canvas = serde_json::json!({"x": 0, "y": 0, "w": 2560, "h": 1440});
    let ranges = [(0u64, 120u64, 0u64), (120, 180, 200), (180, 240, 60)];
    for (k, (a, b, first)) in ranges.into_iter().enumerate() {
        let p = &mut v["parts"][k];
        p["out"] = serde_json::json!([a, b]);
        p["picture"]["first_frame"] = first.into();
        p["picture"]["expect_pts"] =
            serde_json::json!({"ticks": first * tick, "timescale": sv.timescale});
        p["audio"]["first_sample"] = (first * per_frame).into();
        p["audio"]["rate"] = sa.sample_rate.into();
        // one full-frame screen layer, keeping each part's time map
        let mut screen = p["layers"][0].clone();
        screen["crop"] = full.clone();
        screen["place"] = canvas.clone();
        if screen["time_map"]["mode"] == "hold" {
            screen["time_map"]["frame"] = 230.into();
        }
        p["layers"] = serde_json::json!([screen]);
    }
    v["background"] = serde_json::json!({"kind": "colour", "rgb": "#0c1118"});
    v["audio"]["loudness"] =
        serde_json::json!({"mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5});
    if let Some(c) = v.as_object_mut() {
        c.remove("checks");
    }
    // a peak past the social 25 Mbit/s ceiling is a /1 plan (decision 0027), as the
    // planner writes it
    v["schema"] = "gmclip.renderplan/1".into();
    let plan = gmclip_plan::parse(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
    let errors = gmclip_plan::check_plan(&plan);
    assert!(errors.is_empty(), "{errors:?}");

    let tmp = std::env::temp_dir().join(format!("gmclip-export-1440p60-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("yt1440p60.mp4");
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions::default(),
    )
    .unwrap();
    for n in &rep.notes {
        eprintln!("  note: {n}");
    }
    let fell_back = rep.notes.iter().find(|n| n.starts_with("encoder: refused"));

    // what the file carries
    let bytes = std::fs::read(&out).unwrap();
    let d = demux::index(&mut demux::io::SliceSource(&bytes)).unwrap();
    let t = d
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap();
    let Codec::H264 { avcc } = &t.codec else {
        panic!("not H.264: {:?}", t.codec)
    };
    let (profile, level) = (avcc[1], avcc[3]);
    let secs = frames as f64 / fps as f64;
    let video_bytes: u64 = t.samples.size.iter().map(|&s| u64::from(s)).sum();
    let avg = video_bytes as f64 * 8.0 / secs;
    // the busiest one-second window
    let mut peak = 0u64;
    for w in 0..=(t.samples.len().saturating_sub(fps as usize)) {
        let b: u64 = t.samples.size[w..w + fps as usize]
            .iter()
            .map(|&s| u64::from(s))
            .sum();
        peak = peak.max(b * 8);
    }
    eprintln!(
        "1440p60 on MF: {}x{} {} frames in {:.2} s ({:.0} fps); avcC profile {profile} level {}.{}; \
         average {:.1} Mbit/s, busiest second {:.1} Mbit/s (report peak {:.1}), rung {}; {}",
        t.width,
        t.height,
        rep.frames,
        rep.seconds,
        rep.frames as f64 / rep.seconds,
        level / 10,
        level % 10,
        avg / 1e6,
        peak as f64 / 1e6,
        rep.video_peak_bps as f64 / 1e6,
        rep.rate_rung,
        fell_back.map_or("no fallback".to_owned(), |n| n.clone())
    );
    assert_eq!((t.width, t.height), (2560, 1440));
    assert_eq!(t.samples.len() as u64, frames);
    assert_eq!(profile, 100, "High");
    assert!(
        rep.notes.iter().any(|n| n.starts_with("verified:")),
        "{:?}",
        rep.notes
    );
    // the level the plan asked for, unless the encoder refused it (then it says so)
    if fell_back.is_none() {
        assert_eq!(level, 51, "level 5.1 in avcC");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
