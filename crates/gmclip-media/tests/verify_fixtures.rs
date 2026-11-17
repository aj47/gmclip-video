//! B-15 on the CPU: a good synthetic export passes every verify check, and each
//! corrupted fixture fails with the right `step` (the backlog asks for 6; there are 11 container + 6 audio).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Cursor;

use gmclip_media::audio::graph::{AudioPlan, LoudnessMode, PcmSource};
use gmclip_media::audio::priming;
use gmclip_media::mux::{ColorTags, Mp4Writer, MuxPlan, Sample, TrackConfig, inspect};
use gmclip_media::verify::{self, Expect};

const AVCC_HIGH_40: &[u8] = &[
    0x01, 0x64, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x28, 0x01, 0x00, 0x02, 0x68,
    0xee,
];
const ASC_LC_48K_STEREO: &[u8] = &[0x11, 0x90];
const FRAMES: u32 = 60;

struct Opts {
    frames: u32,
    width: u16,
    height: u16,
    avcc: Vec<u8>,
    asc: Vec<u8>,
    colour: Option<ColorTags>,
    gapless: bool,
    /// Audio samples (default: the cut, frames × 1600).
    audio: Option<u64>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            frames: FRAMES,
            width: 1080,
            height: 1920,
            avcc: AVCC_HIGH_40.to_vec(),
            asc: ASC_LC_48K_STEREO.to_vec(),
            colour: Some(ColorTags::BT709_LIMITED),
            gapless: true,
            audio: None,
        }
    }
}

/// A synthetic export: b-pyramid video (depth 2) + AAC-shaped audio of the cut length.
fn export(o: &Opts) -> Vec<u8> {
    let n = o.audio.unwrap_or(u64::from(o.frames) * 1600);
    let durs = priming::packet_durations(n);
    let mut order = vec![0i64];
    let mut p = 0;
    while p + 3 < i64::from(o.frames) {
        order.extend([p + 3, p + 2, p + 1]);
        p += 3;
    }
    order.extend(p + 1..i64::from(o.frames));
    let tracks = vec![
        TrackConfig::h264(o.avcc.clone(), o.width, o.height, 30, 1, o.colour),
        TrackConfig::aac(o.asc.clone(), 2, 48_000),
    ];
    let plan = MuxPlan {
        sample_counts: vec![o.frames, durs.len() as u32],
        max_payload_bytes: Some(1 << 20),
        gapless: o.gapless.then(|| priming::gapless(n)),
    };
    let mut w = Mp4Writer::create(Cursor::new(Vec::new()), tracks, &plan).unwrap();
    for (j, &f) in order.iter().enumerate() {
        w.write_sample(
            0,
            &Sample {
                data: &[0, 0, 0, 2, 9, 0xf0],
                dts: (j as i64 - 2) * 512,
                pts: f * 512,
                duration: 512,
                sync: f % 30 == 0,
            },
        )
        .unwrap();
    }
    let mut t = 0i64;
    for d in durs {
        w.write_sample(
            1,
            &Sample {
                data: &[0x21, 0x10],
                dts: t,
                pts: t,
                duration: d,
                sync: true,
            },
        )
        .unwrap();
        t += i64::from(d);
    }
    w.finish().unwrap().0.into_inner()
}

fn expect() -> Expect {
    Expect {
        frames: u64::from(FRAMES),
        fps: 30,
        fps_den: 1,
        width: 1080,
        height: 1920,
        profile_idc: Some(100),
        level: Some("4.0".to_owned()),
        full_range: false,
        loudness: LoudnessMode::AsRecorded,
    }
}

fn first_failure(file: &[u8]) -> Option<&'static str> {
    verify::verify(file, &expect(), None)
        .first_failure()
        .map(|e| e.step)
}

/// Grows the first `trak` by an `edts/elst` box, taking the room from the `free` box after
/// `moov` so no sample moves.
fn inject_edts(file: &[u8]) -> Vec<u8> {
    let top = inspect::parse(file).unwrap();
    let moov = top.iter().find(|b| &b.fourcc == b"moov").unwrap();
    let free = top.iter().find(|b| &b.fourcc == b"free").unwrap();
    let trak = moov.children.iter().find(|b| &b.fourcc == b"trak").unwrap();
    let mut edts = Vec::new();
    edts.extend_from_slice(&36u32.to_be_bytes());
    edts.extend_from_slice(b"edts");
    edts.extend_from_slice(&28u32.to_be_bytes());
    edts.extend_from_slice(b"elst\0\0\0\0");
    edts.extend_from_slice(&1u32.to_be_bytes());
    edts.extend_from_slice(&2000u32.to_be_bytes());
    edts.extend_from_slice(&0u32.to_be_bytes());
    edts.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    let at = (trak.offset + trak.size) as usize;
    let mut out = file[..at].to_vec();
    out.extend_from_slice(&edts);
    out.extend_from_slice(&file[at..free.offset as usize]);
    out.extend_from_slice(&((free.size - 36) as u32).to_be_bytes());
    out.extend_from_slice(b"free");
    out.extend_from_slice(&file[free.offset as usize + 8 + 36..]);
    let bump = |o: &mut Vec<u8>, at: u64, by: u32| {
        let a = at as usize;
        let v = u32::from_be_bytes(o[a..a + 4].try_into().unwrap()) + by;
        o[a..a + 4].copy_from_slice(&v.to_be_bytes());
    };
    bump(&mut out, moov.offset, 36);
    bump(&mut out, trak.offset, 36);
    assert_eq!(out.len(), file.len());
    out
}

/// Renames the `pasp` box (same size), so the file has no pixel aspect.
fn drop_pasp(file: &[u8]) -> Vec<u8> {
    let top = inspect::parse(file).unwrap();
    let pasp = top.iter().find_map(|b| b.find(b"pasp")).unwrap();
    let mut out = file.to_vec();
    out[pasp.offset as usize + 4..pasp.offset as usize + 8].copy_from_slice(b"free");
    out
}

/// Sets the first video `ctts` entry's offset to `+off` ticks (first PTS no longer 0).
fn shift_first_pts(file: &[u8], off: i32) -> Vec<u8> {
    let top = inspect::parse(file).unwrap();
    let ctts = top.iter().find_map(|b| b.find(b"ctts")).unwrap();
    let at = (ctts.offset + ctts.header + 8 + 4) as usize; // version/flags, count, first count
    let mut out = file.to_vec();
    out[at..at + 4].copy_from_slice(&off.to_be_bytes());
    out
}

#[test]
fn a_good_export_passes_every_container_check() {
    let r = verify::verify(&export(&Opts::default()), &expect(), None);
    for c in &r.checks {
        assert!(c.ok, "{}: {}", c.step, c.detail);
    }
    let steps: Vec<&str> = r.checks.iter().map(|c| c.step).collect();
    assert_eq!(
        steps,
        [
            "demux",
            "codec",
            "profile",
            "canvas",
            "sar",
            "frames",
            "gapless",
            "duration",
            "edts",
            "first_pts",
            "colour"
        ]
    );
}

#[test]
fn corrupted_exports_fail_at_the_right_step() {
    let good = export(&Opts::default());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("demux", good[..good.len().min(200)].to_vec()),
        (
            "codec",
            export(&Opts {
                asc: vec![0x29, 0x90],
                ..Opts::default()
            }),
        ), // AOT 5 (HE-AAC)
        (
            "profile",
            export(&Opts {
                avcc: {
                    let mut a = AVCC_HIGH_40.to_vec();
                    a[1] = 77;
                    a
                },
                ..Opts::default()
            }),
        ),
        (
            "canvas",
            export(&Opts {
                height: 1080,
                ..Opts::default()
            }),
        ),
        (
            "frames",
            export(&Opts {
                frames: FRAMES - 1,
                ..Opts::default()
            }),
        ),
        ("sar", drop_pasp(&good)),
        (
            "duration",
            export(&Opts {
                audio: Some(96_000 - 1600),
                ..Opts::default()
            }),
        ),
        ("edts", inject_edts(&good)),
        ("first_pts", shift_first_pts(&good, 512)),
        (
            "colour",
            export(&Opts {
                colour: Some(ColorTags {
                    primaries: 6,
                    transfer: 6,
                    matrix: 6,
                    full_range: false,
                }),
                ..Opts::default()
            }),
        ),
        (
            "gapless",
            export(&Opts {
                gapless: false,
                ..Opts::default()
            }),
        ),
    ];
    for (step, file) in cases {
        assert_eq!(first_failure(&file), Some(step), "fixture for {step}");
    }
}

/// Deterministic noise sources for the audio checks.
struct Noise;
fn noise(src: &str, k: u64) -> f32 {
    // splitmix64: white enough that its autocorrelation has no false peaks.
    let mut z = (k + if src == "b" { 7_777_777 } else { 0 }).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.3
}
impl PcmSource for Noise {
    fn read(&mut self, source: &str, first: u64, out: &mut [f32]) -> Result<(), String> {
        for (i, f) in out.chunks_mut(2).enumerate() {
            let v = noise(source, first + i as u64);
            f[0] = v;
            f[1] = v;
        }
        Ok(())
    }
}

/// 10 parts of 6 frames alternating sources a/b: 9 discontinuous joins.
fn audio_plan() -> AudioPlan {
    AudioPlan::from_plan(
        &gmclip_plan::parse(&serde_json::to_vec(&audio_plan_json()).unwrap()).unwrap(),
    )
    .unwrap()
}

fn audio_plan_json() -> serde_json::Value {
    let parts: Vec<_> = (0..10u64)
        .map(|i| {
            let src = if i % 2 == 0 { "a" } else { "b" };
            serde_json::json!({ "id": format!("p{i}"), "out": [i * 6, i * 6 + 6], "picture": { "source": src, "first_frame": 0 },
                "layers": [], "audio": { "source": src, "first_sample": 100_000 * i, "rate": 48000 } })
        })
        .collect();
    let v = serde_json::json!({ "schema": "gmclip.renderplan/0",
        "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 60,
                    "video": { "codec": "h264" }, "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2 } },
        "background": { "kind": "colour", "rgb": "#000000" },
        "sources": [ { "id": "a", "path": "/a" }, { "id": "b", "path": "/b" } ],
        "parts": parts, "audio": { "loudness": { "mode": "as_recorded" } } });
    v
}

/// Delays `x` by `k` frames (positive: later), keeping the length.
fn delayed(x: &[f32], k: usize) -> Vec<f32> {
    let mut y = vec![0f32; 2 * k];
    y.extend_from_slice(&x[..x.len() - 2 * k]);
    y
}

#[test]
fn audio_checks_pass_and_fail_by_step() {
    let plan = audio_plan();
    let planned = plan.mix(&mut Noise).unwrap();
    let e = expect();
    let steps = |decoded: &[f32]| -> Vec<&'static str> {
        verify::audio(decoded, &planned, &plan, &e)
            .0
            .into_iter()
            .filter(|c| !c.ok)
            .map(|c| c.step)
            .collect()
    };
    let (checks, joins, drift) = verify::audio(&planned, &planned, &plan, &e);
    assert!(checks.iter().all(|c| c.ok), "{checks:?}");
    assert_eq!(joins.len(), 9);
    assert!(joins.iter().all(|j| j.lag_samples == 0));
    assert_eq!(drift, Some(0.0));
    // 1 ms is the limit for the first window: 48 samples pass; 60 (1.25 ms), 600 (½ frame
    // and more) and 2400 (1.5 frames, the search edge) fail on the measured lag.
    assert_eq!(steps(&delayed(&planned, 48)), Vec::<&str>::new());
    for k in [60usize, 600, 2400] {
        let (c, j, _) = verify::audio(&delayed(&planned, k), &planned, &plan, &e);
        let sync = c.iter().find(|c| c.step == "audio_sync").unwrap();
        assert!(
            !sync.ok && sync.detail.contains(&format!("lag {k} ")),
            "{k}: {}",
            sync.detail
        );
        if k >= 600 {
            assert!(j.iter().all(|j| j.lag_samples == k as i64), "{k}: {j:?}");
        }
    }
    // One short part (3 frames, 50 ms) misplaced by 25 ms inside a correct cut: the
    // per-part window catches it where the ±0.5 s window alone would not.
    let short = AudioPlan::from_plan(&{
        let mut p = gmclip_plan::parse(&serde_json::to_vec(&audio_plan_json()).unwrap()).unwrap();
        p.parts[5].out = gmclip_plan::scalars::Range([30, 33]);
        p.parts[6].out = gmclip_plan::scalars::Range([33, 42]);
        p
    })
    .unwrap();
    let planned_s = short.mix(&mut Noise).unwrap();
    let mut misplaced = planned_s.clone();
    let (a, b) = (
        short.parts[5].out[0] as usize * 2,
        short.parts[5].out[1] as usize * 2,
    );
    let part: Vec<f32> = planned_s[a..b].to_vec();
    misplaced[a..b].fill(0.0);
    let shift = 2 * 1200;
    misplaced[a + shift..(b + shift).min(planned_s.len())]
        .copy_from_slice(&part[..(b - a).min(planned_s.len() - a - shift)]);
    let (c, j, _) = verify::audio(&misplaced, &planned_s, &short, &e);
    assert!(c.iter().any(|c| c.step == "join_av" && !c.ok), "{j:?}");
    // A slip of 15 ms that starts after the 5th join: the first window passes, the late
    // joins fail; a slip growing by 0.3 ms per join is drift.
    let mut slipped = planned.clone();
    let cut = (plan.joins()[4].at as usize) * 2;
    slipped.splice(cut..cut, std::iter::repeat_n(0.0, 2 * 720));
    slipped.truncate(planned.len());
    assert_eq!(steps(&slipped), vec!["join_av", "drift"]);
    let mut drifting = planned.clone();
    for (n, j) in plan.joins().iter().enumerate() {
        let at = (j.at as usize) * 2;
        drifting.splice(at..at, std::iter::repeat_n(0.0, 2 * (14 + n % 2)));
    }
    drifting.truncate(planned.len());
    let (c, _, d) = verify::audio(&drifting, &planned, &plan, &e);
    assert!(d.unwrap() > 0.1, "{d:?}");
    assert!(c.iter().any(|c| c.step == "drift" && !c.ok));
    // Loudness is checked on the delivered audio against a static target.
    let st = Expect {
        loudness: LoudnessMode::Static {
            integrated_lufs: -16.0,
            true_peak_dbtp: -1.5,
        },
        ..e.clone()
    };
    let failing: Vec<_> = verify::audio(&planned, &planned, &plan, &st)
        .0
        .into_iter()
        .filter(|c| !c.ok)
        .map(|c| c.step)
        .collect();
    assert_eq!(failing, vec!["loudness"]);
}
