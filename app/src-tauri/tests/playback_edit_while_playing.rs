//! Decision 0053: an edit committed while the cut plays reaches the sound in time. The
//! playback engine runs headless on the fixture cut of `gmclip-render`'s
//! `preview_export_identity` (skipped without ffmpeg) and this test is its device, paced
//! in real time. A second before the second join, the play is retargeted
//! (`Audio::retarget`, what the viewer does on a commit) to an edit whose third part is
//! another stretch of the source, `lead` ms before the join is heard:
//!
//! - **(B)** the edit is after the playhead (the samples before the join are the same);
//! - **(C)** the first part is also deleted, before the playhead: the play goes on 24
//!   frames earlier in the edited plan (the clock keeps the old numbering, shifted).
//!
//! Each lead is honoured when every sample from the join on is the edited plan's (within
//! 10⁻⁴: the edited plan's sources are read from a fresh seek). Before it, the sound is the
//! old plan's without a break: no underrun, no sample off. Leads of 200 ms and more must
//! be honoured; the shortest one honoured is printed (the device queue is ~170 ms, and a
//! retarget mixes again what is queued from 2 chunks past the one being heard). The
//! viewer's frame mapping (`viewer::same_frame`) is checked on the same edits.
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::playback::{Audio, plan_mix};
use gmclip_plan::Plan;
use gmclip_render::export::{DemuxInputs, VoiceMix};

#[path = "../../../crates/gmclip-render/tests/identity/mod.rs"]
mod identity;

/// Device buffer (frames), as CoreAudio's default on a Mac.
const BUF: usize = 512;
const TOL: f32 = 1e-4;

fn mix_of(plan: &Plan) -> Vec<f32> {
    let mut vm = VoiceMix::open(plan, &mut DemuxInputs::new(None)).unwrap();
    let mut out = vec![0.0f32; vm.total() as usize * 2];
    vm.read(0, &mut out).unwrap();
    out
}

/// The fixture without its gain envelope (it would not survive deleting the first part).
fn plain(repo: &Path, a: &Path, b: &Path) -> Plan {
    let mut p = identity::plan(repo, a, b, 0);
    p.audio.envelopes = None;
    p
}

/// The edit: the third part shows (a) from its start instead of frame 70; with
/// `delete_first`, the first part is gone too.
fn edited(base: &Plan, delete_first: bool) -> Plan {
    let mut p = base.clone();
    p.parts[2].picture.first_frame = 0;
    p.parts[2].audio.first_sample = 0;
    if delete_first {
        let n = p.parts[0].out.len();
        p.parts.remove(0);
        for q in &mut p.parts {
            q.out = gmclip_plan::scalars::Range([q.out.start() - n, q.out.end() - n]);
        }
        p.output.frames -= n;
        p.text_events = None;
        p.parts[0].audio.fade_in = Some(Some(gmclip_plan::types::Fade {
            samples: 144,
            shape: gmclip_plan::types::FadeShape::Linear,
        }));
    }
    let errs = gmclip_plan::check_plan(&p);
    assert!(errs.is_empty(), "{errs:?}");
    p
}

/// Plays `old` from 1 s before `join` (plan samples), retargets to `new` (shifted by
/// `shift`) `lead_ms` before the join is heard, and returns the heard samples from the
/// play's start to 0.4 s after the join, and the underruns.
fn play(old: &Arc<Plan>, new: &Arc<Plan>, shift: i64, join: u64, lead_ms: u64) -> (Vec<f32>, u64) {
    let (mut a, mut render) = Audio::headless(Box::new(|p: &Arc<Plan>| plan_mix(p).ok())).unwrap();
    let from = join - 48_000;
    let switch = join - lead_ms * 48;
    let until = join + 19_200;
    a.play(Arc::clone(old), from as f64, None, 1.0, false);
    std::thread::sleep(Duration::from_millis(300));
    let mut buf = vec![0.0f32; BUF * 2];
    let mut heard = Vec::new();
    let mut at = from;
    let mut sent = false;
    let t0 = Instant::now();
    let mut n = 0u64;
    while at < until {
        if !sent && at >= switch {
            a.retarget(Arc::clone(new), shift);
            sent = true;
        }
        render.fill(
            &mut buf,
            1_000_000_000 + n * BUF as u64 * 1_000_000_000 / 48_000,
        );
        heard.extend_from_slice(&buf);
        at += BUF as u64;
        n += 1;
        // real time: the next buffer is due one buffer later
        let due = t0 + Duration::from_secs_f64(n as f64 * BUF as f64 / 48_000.0);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    let u = a.underruns();
    a.stop();
    (heard, u)
}

/// Samples of `heard` (a play from `from`, old numbering) in `[lo, hi)` that are not
/// `mix` at `shift` samples on.
fn off(heard: &[f32], from: u64, mix: &[f32], shift: i64, lo: u64, hi: u64) -> usize {
    (lo..hi)
        .filter(|&s| {
            let k = (s - from) as usize;
            let m = (s as i64 + shift) as usize;
            (0..2).any(|ch| (heard[k * 2 + ch] - mix[m * 2 + ch]).abs() > TOL)
        })
        .count()
}

#[test]
fn an_edit_200_ms_before_a_join_is_heard_at_the_join() {
    let dir = std::env::temp_dir().join(format!("gmclip-pb-edit-{}", std::process::id()));
    let Some((a, b)) = identity::sources(&dir) else {
        return;
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let base = Arc::new(plain(&repo, &a, &b));
    let join = identity::joins(&base)[1] * 1600;
    let old_mix = mix_of(&base);
    // the viewer keeps the frame on screen: part 2's frame 30 is frame 6 once part 1 (24
    // frames) is deleted, and stays 30 when the edit is after it
    let c = edited(&base, true);
    assert_eq!(
        gmclip_app::viewer::same_frame(&base, &edited(&base, false), 30),
        30
    );
    assert_eq!(gmclip_app::viewer::same_frame(&base, &c, 30), 6);
    assert_eq!(gmclip_app::viewer::same_frame(&base, &c, 47), 23);
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for (name, delete_first) in [
        ("B, after the playhead", false),
        ("C, the first part deleted", true),
    ] {
        let new = Arc::new(edited(&base, delete_first));
        let shift = if delete_first {
            -(base.parts[0].out.len() as i64) * 1600
        } else {
            0
        };
        let new_mix = mix_of(&new);
        let mut honoured = Vec::new();
        for lead in [50u64, 100, 150, 200, 250, 300, 400] {
            let (heard, underruns) = play(&base, &new, shift, join, lead);
            let from = join - 48_000;
            // the old plan's sound up to the join, without a break (past the play's fade-in)
            let before = off(&heard, from, &old_mix, 0, from + 64, join);
            let after_new = off(&heard, from, &new_mix, shift, join + 64, join + 19_200);
            let ok = after_new == 0;
            eprintln!(
                "{name}: lead {lead} ms: {} (after the join {after_new} samples off the edit), \
                 before the join {before} off the old plan, {underruns} underruns",
                if ok { "honoured" } else { "not honoured" }
            );
            if before != 0 || underruns != 0 {
                failures.push(format!("{name} lead {lead}: {before} samples off before the join, {underruns} underruns"));
            }
            if ok {
                honoured.push(lead);
            } else if lead >= 200 {
                failures.push(format!("{name} lead {lead}: not honoured"));
            }
        }
        report.push(format!(
            "{name}: shortest lead honoured {:?} ms",
            honoured.first()
        ));
    }
    eprintln!("edit while playing (decision 0053): {}", report.join("; "));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}
