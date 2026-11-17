//! Playback audio and the clock (SPEC §6.3), lab: on AJ's h79 reel through the real
//! output device (silent: GMCLIP_PLAYBACK_SILENT zeroes the samples, the device still
//! runs and clocks). The clock advances at 48 kHz × rate against host time with no
//! underruns; a range stops at its exact end sample; the viewer shows the frame being
//! heard: A/V offset at frame handoff reported, ≤ 1 frame.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::playback::{Audio, now_ns};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn setup(tag: &str) -> Option<(Arc<AppCore>, String, String)> {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return None;
    }
    if std::env::var_os("GMCLIP_PLAYBACK_SILENT").is_none() {
        eprintln!("note: GMCLIP_PLAYBACK_SILENT is not set, so this test is audible");
    }
    let root = std::env::temp_dir().join(format!("gmclip-pb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    Some((
        c,
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    ))
}

/// Positions sampled for `secs`: (host ns, plan sample).
fn sample(a: &Audio, secs: f64) -> Vec<(u64, f64)> {
    let t0 = Instant::now();
    let mut v = Vec::new();
    while t0.elapsed().as_secs_f64() < secs {
        if let Some((p, _)) = a.position() {
            v.push((now_ns(), p));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    v
}

fn slope(v: &[(u64, f64)]) -> f64 {
    let (a, b) = (v[v.len() / 5], v[v.len() - 1]);
    (b.1 - a.1) / ((b.0 - a.0) as f64 / 1e9)
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs an audio output device + AJ's media (cargo test -- --ignored)"]
fn the_clock_follows_the_device() {
    let Some((c, pid, cut)) = setup("clock") else {
        return;
    };
    let plan = c.planned(&pid, &cut).unwrap().plan;
    let mut a = Audio::open().expect("an output device");
    eprintln!("device latency {:.1} ms", a.latency_ms());
    let t0 = Instant::now();
    a.play(Arc::clone(&plan), 48_000.0, None, 1.0, false);
    while a.position().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "the device never started"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!("first sample heard after {:?}", t0.elapsed());
    let v = sample(&a, 2.0);
    let r1 = slope(&v);
    eprintln!("×1: {r1:.0} samples/s over {} readings", v.len());
    assert!((r1 - 48_000.0).abs() < 480.0, "{r1}");
    assert_eq!(a.underruns(), 0);

    a.play(Arc::clone(&plan), 96_000.0, None, 2.0, false);
    std::thread::sleep(Duration::from_millis(300));
    let r2 = slope(&sample(&a, 1.5));
    eprintln!("×2: {r2:.0} samples/s");
    assert!((r2 - 96_000.0).abs() < 960.0, "{r2}");

    // a range stops on its exact end sample
    a.play(Arc::clone(&plan), 10_000.0, Some(34_000), 1.0, false);
    let t1 = Instant::now();
    let end = loop {
        if let Some((p, true)) = a.position() {
            break p;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(3),
            "the range never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    eprintln!(
        "range [10000, 34000) ended at {end} after {:?}",
        t1.elapsed()
    );
    assert_eq!(end, 34_000.0);
    assert_eq!(a.underruns(), 0, "no underruns");
    a.stop();
    drop(a);
    c.shutdown();
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs an audio output device, VideoToolbox + AJ's media"]
fn the_viewer_shows_the_frame_being_heard() {
    use gmclip_app::viewer::ViewerMsg;
    let Some((c, pid, cut)) = setup("viewer") else {
        return;
    };
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    // the frame socket's reader
    let s2 = Arc::clone(&slot);
    std::thread::spawn(move || {
        loop {
            let _ = s2.take(Duration::from_millis(100));
        }
    });
    viewer.send(ViewerMsg::Load {
        project: pid.clone(),
        cut: cut.clone(),
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(1500));
    viewer.send(ViewerMsg::Play);
    std::thread::sleep(Duration::from_secs(6));
    let st = viewer.state();
    viewer.send(ViewerMsg::Pause);
    eprintln!("{}", serde_json::to_string_pretty(&st).unwrap());
    assert!(st.audio.device);
    assert!(
        st.audio.av_frames > 100,
        "{} frames measured",
        st.audio.av_frames
    );
    assert!(
        st.audio.av_max_ms <= 1000.0 / 30.0,
        "A/V offset {} ms > 1 frame",
        st.audio.av_max_ms
    );
    assert_eq!(st.audio.underruns, 0);
    assert!(st.frame > 150, "playback advanced ({})", st.frame);
    // P: one word, exact (source ms → output samples), then it stops by itself
    viewer.send(ViewerMsg::PlaySrc {
        from_ms: 2000,
        to_ms: 2400,
    });
    std::thread::sleep(Duration::from_millis(900));
    let st = viewer.state();
    assert!(!st.playing, "stopped at the word's end");
    assert_eq!(
        st.frame,
        (2400 * 30 - 1) / 1000,
        "paused on the word's last frame"
    );
    c.shutdown();
}

/// A multi-part RT cut (h37: 1440p picture + proxy audio, joins every few seconds):
/// playback across the joins without underruns, the frame following the audio.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs an audio output device, VideoToolbox + the lab media"]
fn joins_play_without_underruns() {
    use gmclip_app::viewer::ViewerMsg;
    if !gmclip_model::lab::has(&["preview"]) {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-pb-joins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let cut = p["fixture"]["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hook_id"] == "loc_4fadb756_h37")
        .unwrap()["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let plan = c.planned(&pid, &cut).unwrap().plan;
    let joins = plan.parts.len().saturating_sub(1);
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    let s2 = Arc::clone(&slot);
    std::thread::spawn(move || {
        loop {
            let _ = s2.take(Duration::from_millis(100));
        }
    });
    viewer.send(ViewerMsg::Load {
        project: pid,
        cut,
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(2000));
    viewer.send(ViewerMsg::Play);
    let secs = (plan.output.frames as f64 / 30.0).min(30.0);
    std::thread::sleep(Duration::from_secs_f64(secs));
    let st = viewer.state();
    viewer.send(ViewerMsg::Pause);
    eprintln!(
        "h37: {} parts ({joins} joins), played {secs:.0} s: {:?}, dropped {}",
        plan.parts.len(),
        st.audio,
        st.dropped
    );
    assert_eq!(st.audio.underruns, 0);
    assert!(st.audio.av_max_ms <= 1000.0 / 30.0);
    c.shutdown();
}

/// H.02: switching the output device mid-play (AirPods connecting) reopens the engine on
/// the system's device; the play continues, the clock adds that device's latency, and the
/// A/V offset is measured again from the switch (the viewer's `AudioDeviceChanged`, which
/// the once-a-second device check also takes).
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs an audio output device, VideoToolbox + AJ's media"]
fn a_device_switch_mid_play_recovers() {
    use gmclip_app::viewer::ViewerMsg;
    let Some((c, pid, cut)) = setup("switch") else {
        return;
    };
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    let s2 = Arc::clone(&slot);
    std::thread::spawn(move || {
        loop {
            let _ = s2.take(Duration::from_millis(100));
        }
    });
    viewer.send(ViewerMsg::Load {
        project: pid,
        cut,
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(1500));
    viewer.send(ViewerMsg::Play);
    std::thread::sleep(Duration::from_secs(3));
    let before = viewer.state();
    assert!(before.audio.av_frames > 50, "{:?}", before.audio);
    viewer.send(ViewerMsg::AudioDeviceChanged);
    std::thread::sleep(Duration::from_millis(150));
    let just = viewer.state();
    std::thread::sleep(Duration::from_secs(3));
    let after = viewer.state();
    viewer.send(ViewerMsg::Pause);
    eprintln!(
        "before {:?}\njust after {:?}\nafter {:?}",
        before.audio, just.audio, after.audio
    );
    assert!(after.playing, "the play continues on the new device");
    assert_eq!(after.audio.reopens, 1);
    assert!(after.audio.device);
    // measured again: fewer frames than an unbroken 6 s would give, enough to be sure
    assert!(
        just.audio.av_frames < before.audio.av_frames,
        "A/V restarted at the switch"
    );
    assert!(after.audio.av_frames > 50, "{:?}", after.audio);
    assert!(
        after.audio.av_max_ms <= 1000.0 / 30.0,
        "A/V offset {} ms > 1 frame on the new device",
        after.audio.av_max_ms
    );
    assert_eq!(after.audio.underruns, 0);
    assert!(
        after.frame > before.frame + 60,
        "playback advanced ({} → {})",
        before.frame,
        after.frame
    );
    c.shutdown();
}
