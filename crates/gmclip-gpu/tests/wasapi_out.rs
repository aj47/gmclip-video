//! WASAPI playback output (M5) on the machine's real default device, playing silence:
//! the callback runs on the audio thread, gets stereo buffers whose host times move
//! forward with the samples, keeps running after a stop/start, and drop joins cleanly.
//! Needs an audio device, so ignored by default:
//!
//!   cargo test -p gmclip-gpu --test wasapi_out -- --ignored --nocapture
#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gmclip_gpu::windows::audio_out::{AudioOut, RATE, default_output_device, host_now_ns};

#[test]
#[ignore = "needs an audio output device"]
fn silence_plays_with_a_clock_that_follows_the_samples() {
    assert_ne!(default_output_device(), 0, "no default output device");
    // (frames in the buffer, host time of its first sample, host time of the call)
    let seen: Arc<Mutex<Vec<(usize, u64, u64)>>> = Arc::default();
    let log = Arc::clone(&seen);
    // a preallocated log; the test's own closure may lock, the app's never does
    log.lock().unwrap().reserve(4096);
    let mut out = AudioOut::open(Box::new(move |buf: &mut [f32], at: u64| {
        let now = host_now_ns();
        buf.fill(0.0);
        if let Ok(mut l) = log.try_lock()
            && l.len() < l.capacity()
        {
            l.push((buf.len() / 2, at, now));
        }
    }))
    .unwrap();
    eprintln!("latency {} µs", out.latency_ns() / 1000);
    out.start().unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    out.stop();
    let n_stopped = seen.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        seen.lock().unwrap().len(),
        n_stopped,
        "callbacks after stop"
    );
    out.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    out.stop();
    drop(out);
    let s = seen.lock().unwrap().clone();
    assert!(s.len() > 20, "too few callbacks: {}", s.len());
    let frames: usize = s.iter().map(|x| x.0).sum();
    eprintln!(
        "{} callbacks, {frames} frames ({:.2} s) in ~1.3 s of play; buffer sizes {}..{}",
        s.len(),
        frames as f64 / f64::from(RATE),
        s.iter().map(|x| x.0).min().unwrap(),
        s.iter().map(|x| x.0).max().unwrap()
    );
    // each buffer is heard after it is filled, and no earlier than the one before it
    for w in s.windows(2) {
        assert!(w[1].1 >= w[0].1, "the clock went backwards: {w:?}");
    }
    // (the closure reads its clock a moment after the pump did: 1 ms of slack)
    for x in &s {
        assert!(x.1 + 1_000_000 >= x.2, "heard before it was filled: {x:?}");
        assert!(x.1 < x.2 + 500_000_000, "half a second ahead: {x:?}");
    }
    let mut lead: Vec<i64> = s.iter().map(|x| x.1 as i64 - x.2 as i64).collect();
    lead.sort_unstable();
    eprintln!(
        "lead (heard − filled): min {:.2} ms, median {:.2} ms, max {:.2} ms",
        lead[0] as f64 / 1e6,
        lead[lead.len() / 2] as f64 / 1e6,
        lead[lead.len() - 1] as f64 / 1e6
    );
    // in the first second the samples keep pace with real time (±20 %)
    let first: Vec<_> = s.iter().take(n_stopped).collect();
    let span = (first.last().unwrap().2 - first[0].2) as f64 / 1e9;
    let played = first.iter().map(|x| x.0).sum::<usize>() as f64 / f64::from(RATE);
    eprintln!("first run: {played:.3} s of samples over {span:.3} s");
    assert!(
        (played / span - 1.0).abs() < 0.2 || span < 0.5,
        "{played} vs {span}"
    );
}
