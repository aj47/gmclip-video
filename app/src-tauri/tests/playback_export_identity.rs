//! Decision 0053: playback sounds as the export on the fixture cut of `gmclip-render`'s
//! `preview_export_identity` (two ffmpeg-made sources, six parts, every kind of join;
//! skipped without ffmpeg). The cut is exported with its mix hashed before loudness; the
//! playback engine runs headless on the mix the app plays (`playback::plan_mix`) and this
//! test is its device, buffer by buffer.
//!
//! - The export's mix is the plan's mix read in ranges (its hashes, block for block).
//! - A play of the whole cut hears that mix bit for bit, except the play's 64-frame
//!   de-click ramps at its start and end, which only attenuate it.
//! - A play of a range from mid-part across two joins hears it within [`AFTER_SEEK`]: a
//!   source's AAC reader that re-seeks decodes from another packet than the export's
//!   did, and AudioToolbox's output keeps a trace of where decoding restarted (measured
//!   ≤ 1.1·10⁻⁵ here, −99 dBFS). A sample off, a gain step or a missing fade is ≥ 10⁻³.
//!
//! The fixture's loudness is as recorded: playback's static gain (no true-peak limiter)
//! is the one designed difference, and it is 1 here.
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::playback::{Audio, DECLICK, Render, plan_mix};
use gmclip_plan::Plan;
use gmclip_render::export::{
    DemuxInputs, ExportOptions, MIX_HASH_BLOCK, VoiceMix, export, mix_block_hashes,
};

#[path = "../../../crates/gmclip-render/tests/identity/mod.rs"]
mod identity;

/// Device buffer (frames), as CoreAudio's default on a Mac.
const BUF: usize = 512;
/// How far a play that starts after a re-seek may be from the export's samples (−80 dBFS).
const AFTER_SEEK: f32 = 1e-4;

/// Plays `[from, to)` of `plan` and returns what the device heard from the play's first
/// sample to its end.
fn hear(a: &mut Audio, render: &mut Render, plan: &Arc<Plan>, from: u64, to: u64) -> Vec<f32> {
    a.play(Arc::clone(plan), from as f64, Some(to), 1.0, false);
    // the producer queues its first chunks before the device asks
    std::thread::sleep(Duration::from_millis(300));
    let mut buf = vec![0.0f32; BUF * 2];
    let mut at_ns = 1_000_000_000u64;
    let mut heard = Vec::new();
    let t0 = Instant::now();
    while heard.len() < ((to - from) as usize) * 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the play did not end"
        );
        render.fill(&mut buf, at_ns);
        at_ns += (BUF as u64 * 1_000_000_000) / 48_000;
        heard.extend_from_slice(&buf);
        std::thread::sleep(Duration::from_millis(2));
    }
    heard.truncate(((to - from) as usize) * 2);
    heard
}

/// Output samples of `heard` (a play from `from`) that are not `mix`'s, within `tol`
/// (0: bit for bit); in a de-click ramp at either end, between 0 and the mix's sample.
/// Also the largest difference outside the ramps.
fn differ(heard: &[f32], mix: &[f32], from: u64, tol: f32) -> (Vec<usize>, f32) {
    let n = heard.len() / 2;
    let mut out = Vec::new();
    let mut worst = 0f32;
    for k in 0..n {
        let ramp = k < DECLICK || k >= n - DECLICK;
        for ch in 0..2 {
            let h = heard[k * 2 + ch];
            let m = mix[(from as usize + k) * 2 + ch];
            let ok = if ramp {
                h.abs() <= m.abs() + tol && (h.abs() <= tol || h.signum() == m.signum())
            } else {
                worst = worst.max((h - m).abs());
                if tol == 0.0 {
                    h.to_bits() == m.to_bits()
                } else {
                    (h - m).abs() <= tol
                }
            };
            if !ok {
                out.push(from as usize + k);
                break;
            }
        }
    }
    (out, worst)
}

#[test]
fn playback_hears_the_export_mix_on_the_fixture_cut() {
    let dir = std::env::temp_dir().join(format!("gmclip-pb-identity-{}", std::process::id()));
    let Some((a, b)) = identity::sources(&dir) else {
        return;
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plan = identity::plan(&repo, &a, &b, 0);
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &dir.join("out.mp4"),
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    // the export's mix, sample by sample: the plan's mix in ranges, proven by its hashes
    let mut vm = VoiceMix::open(&plan, &mut DemuxInputs::new(None)).unwrap();
    let total = vm.total();
    let mut mix = vec![0.0f32; total as usize * 2];
    for (k, c) in mix.chunks_mut(MIX_HASH_BLOCK * 2).enumerate() {
        vm.read((k * MIX_HASH_BLOCK) as u64, c).unwrap();
    }
    assert_eq!(
        mix_block_hashes(&mix),
        rep.mix_hashes,
        "the export's mix is not the plan's mix in ranges"
    );

    let plan = Arc::new(plan);
    let (mut audio, mut render) =
        Audio::headless(Box::new(|p: &Arc<Plan>| plan_mix(p).ok())).unwrap();
    let whole = hear(&mut audio, &mut render, &plan, 0, total);
    let joins = identity::joins(&plan);
    // from inside the second part to just after the fourth join
    let (from, to) = (joins[1] * 1600 - 2_000, joins[3] * 1600 + 3_000);
    audio.stop();
    let range = hear(&mut audio, &mut render, &plan, from, to);
    let (whole_differ, whole_worst) = differ(&whole, &mix, 0, 0.0);
    let (range_differ, range_worst) = differ(&range, &mix, from, AFTER_SEEK);
    let underruns = audio.underruns();
    eprintln!(
        "playback = export (decision 0053): {total} samples, joins {joins:?}; whole play: {} \
         samples differ {:?} (worst {whole_worst:e}); range [{from}, {to}): {} beyond \
         {AFTER_SEEK:e} {:?} (worst {range_worst:e}); {underruns} underruns",
        whole_differ.len(),
        &whole_differ[..whole_differ.len().min(8)],
        range_differ.len(),
        &range_differ[..range_differ.len().min(8)],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(underruns, 0, "the device ran dry: the comparison is off");
    assert!(
        whole.iter().filter(|x| x.abs() > 0.01).count() as u64 > total / 2,
        "playback was silent"
    );
    assert!(
        whole_differ.is_empty(),
        "playback ≠ export at {whole_differ:?}"
    );
    assert!(
        range_differ.is_empty(),
        "playback ≠ export after a seek at {range_differ:?}"
    );
}
