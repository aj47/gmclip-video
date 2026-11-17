//! H.02: the playback callback's real-time rules, without a device. The engine runs
//! headless with a synthetic mix and this test plays the device: it calls
//! `Render::fill` buffer by buffer on its own thread, through plays, a seek mid-play, a
//! rate change, a stop, a scrub of rapid grain steps, a play started over a grain, a
//! range that ends and a loop that wraps.
//!
//! - **Never allocates or frees**: a counting global allocator counts this thread's
//!   heap operations while `fill` runs (`gmclip_gpu::alloc_probe`): zero. (Locks: the callback holds no
//!   `Mutex`; its clock is a sequence lock of atomics and both channels are only
//!   `try_`-used, so std never locks them to wake a waiter.)
//! - **No clicks**: the mix is a constant 0.8, the worst case for a hard edge (a cut is
//!   a 0.8 step). Every grain, every start, stop and switch is faded, so no two
//!   consecutive samples differ by more than a 64-frame ramp's step.
//! - **Scrub grains** are 80 ms, play back to back, and the newest step wins.

use std::sync::Arc;
use std::time::Duration;

use gmclip_app::playback::{Audio, DECLICK, GRAIN, Mix, Render};
use gmclip_gpu::alloc_probe;
use gmclip_plan::Plan;

#[global_allocator]
static ALLOC: alloc_probe::Counting = alloc_probe::Counting;

const LEVEL: f32 = 0.8;
const TOTAL: u64 = 48_000 * 60;
/// Device buffer (frames), as CoreAudio's default on a Mac.
const BUF: usize = 512;

/// A constant level: any hard edge in the output is a step of `LEVEL`.
struct Dc;

impl Mix for Dc {
    fn total(&self) -> u64 {
        TOTAL
    }
    fn mix(&mut self, _from: u64, out: &mut [f32]) {
        out.fill(LEVEL);
    }
}

fn plan() -> Arc<Plan> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/examples/h79-v4.renderplan.json");
    let mut plan = gmclip_plan::parse(&std::fs::read(p).unwrap()).unwrap();
    plan.resolve_paths(&gmclip_model::lab::root());
    Arc::new(plan)
}

/// The device: buffer after buffer, each heard 512 frames after the one before.
struct Device {
    render: Render,
    buf: Vec<f32>,
    at_ns: u64,
    heard: Vec<f32>,
    allocs: u64,
}

impl Device {
    fn new(render: Render) -> Device {
        Device {
            render,
            buf: vec![0.0; BUF * 2],
            at_ns: 1_000_000_000,
            heard: Vec::with_capacity(48_000 * 2 * 120),
            allocs: 0,
        }
    }

    /// `n` buffers, a little faster than real time (the producer keeps up easily).
    fn pump(&mut self, n: usize) {
        for _ in 0..n {
            alloc_probe::start();
            self.render.fill(&mut self.buf, self.at_ns);
            self.allocs += alloc_probe::stop();
            self.at_ns += (BUF as u64 * 1_000_000_000) / 48_000;
            self.heard.extend_from_slice(&self.buf);
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// The largest step between consecutive samples of one channel.
    fn worst_step(&self) -> (f32, usize) {
        let mut worst = (0f32, 0);
        for ch in 0..2 {
            let s: Vec<f32> = self.heard.iter().skip(ch).step_by(2).copied().collect();
            for (i, w) in s.windows(2).enumerate() {
                let d = (w[1] - w[0]).abs();
                if d > worst.0 {
                    worst = (d, i);
                }
            }
        }
        worst
    }
}

#[test]
fn the_callback_never_allocates_and_never_clicks() {
    let (mut a, render) =
        Audio::headless(Box::new(|_: &Arc<Plan>| Some(Box::new(Dc) as Box<dyn Mix>))).unwrap();
    // the probe counts (a heap operation on this thread is seen)
    alloc_probe::start();
    drop(std::hint::black_box(vec![1u8; 64]));
    assert!(
        alloc_probe::stop() >= 2,
        "the allocation probe is not installed"
    );
    let mut dev = Device::new(render);
    let p = plan();
    dev.pump(5);
    // a play from silence, then a seek mid-play, a faster rate, a stop
    a.play(Arc::clone(&p), 0.0, None, 1.0, false);
    std::thread::sleep(Duration::from_millis(30));
    dev.pump(40);
    a.play(Arc::clone(&p), 480_000.0, None, 1.0, false);
    std::thread::sleep(Duration::from_millis(10));
    dev.pump(40);
    a.play(Arc::clone(&p), 600_000.0, None, 1.5, false);
    std::thread::sleep(Duration::from_millis(10));
    dev.pump(30);
    a.stop();
    dev.pump(10);
    // a scrub: a step every two buffers (~21 ms), faster than a grain (80 ms)
    for k in 0..40u64 {
        a.grain(Arc::clone(&p), 100_000 + k * 1_600);
        dev.pump(2);
    }
    // a play started while a grain sounds
    a.play(Arc::clone(&p), 200_000.0, None, 1.0, false);
    std::thread::sleep(Duration::from_millis(10));
    dev.pump(30);
    // a range that ends on its exact sample
    a.play(Arc::clone(&p), 300_000.0, Some(310_000), 1.0, false);
    std::thread::sleep(Duration::from_millis(10));
    dev.pump(40);
    let end = a.position().expect("the range was heard");
    assert_eq!(end, (310_000.0, true), "the range ends on its exact sample");
    // a loop of the cut's last half second wraps (to the start) without a click
    a.play(Arc::clone(&p), (TOTAL - 24_000) as f64, None, 1.0, true);
    std::thread::sleep(Duration::from_millis(10));
    dev.pump(80);
    a.stop();
    dev.pump(10);
    eprintln!(
        "{} buffers heard, {} allocations in the callback, {} underruns",
        dev.heard.len() / (BUF * 2),
        dev.allocs,
        a.underruns()
    );
    assert_eq!(dev.allocs, 0, "the callback allocated or freed");
    let (step, at) = dev.worst_step();
    // a 64-frame ramp of 0.8 steps by 0.0125; a grain's 5 ms raised cosine by ≤ 0.0053
    let ramp = LEVEL / DECLICK as f32;
    eprintln!("largest step between samples {step:.4} at frame {at} (ramp step {ramp:.4})");
    assert!(
        step <= ramp * 1.01,
        "a click: {step} between frames {at} and {} (ramp {ramp})",
        at + 1
    );
    assert!(
        dev.heard.iter().any(|&x| x > 0.79),
        "the level was heard at full"
    );
}

#[test]
fn scrub_grains_play_back_to_back_and_the_newest_step_wins() {
    let (mut a, render) =
        Audio::headless(Box::new(|_: &Arc<Plan>| Some(Box::new(Dc) as Box<dyn Mix>))).unwrap();
    let mut dev = Device::new(render);
    let p = plan();
    // 30 steps faster than anything can be heard, then the device runs
    for k in 0..30u64 {
        a.grain(Arc::clone(&p), 50_000 + k * 3_000);
    }
    let newest = 50_000 + 29 * 3_000;
    std::thread::sleep(Duration::from_millis(30));
    dev.pump(60);
    // grains heard: runs of sound between silences
    let left: Vec<f32> = dev.heard.iter().step_by(2).copied().collect();
    let mut grains = Vec::new();
    let mut start = None;
    for (i, &x) in left.iter().enumerate() {
        match (start, x > 1e-3) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                grains.push(i - s);
                start = None;
            }
            _ => {}
        }
    }
    eprintln!("grains heard (frames above silence): {grains:?}");
    assert!(
        !grains.is_empty() && grains.len() <= 2,
        "steps between grains are skipped, not queued: {grains:?}"
    );
    for g in &grains {
        assert!(
            (GRAIN - 16..=GRAIN).contains(g),
            "a grain is {GRAIN} frames: {g}"
        );
    }
    // the last grain heard is the newest step's
    let (pos, ended) = a.position().unwrap();
    assert!(ended);
    assert_eq!(pos, (newest + GRAIN as u64) as f64);
    assert_eq!(
        a.underruns(),
        0,
        "the gaps between grains are not underruns"
    );
}
