//! U.15 essentials: the import sheet on a two-audio-track file (made here with ffmpeg
//! from AJ's h79 reel: track 1 a tone standing in for game audio, track 2 his voice).
//! It asks "Which track is your mic?", suggests the voice by speech ratio, gives a 3 s
//! preview per track at the same moment, and capability lines with fixes; opening with
//! the chosen mic sets the source's voice track before transcription, which then hears
//! the voice. Skips without the reel or ffmpeg (a dev tool, never shipped).

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn ffmpeg() -> Option<&'static str> {
    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
}

#[test]
fn which_track_is_your_mic() {
    let (true, Some(ff)) = (Path::new(video()).is_file(), ffmpeg()) else {
        eprintln!("skipped: no test video or ffmpeg");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-imp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let two = root.join("two-tracks.mp4");
    let st = std::process::Command::new(ff)
        .args(["-v", "error", "-y", "-i", video(), "-f", "lavfi", "-i"])
        .arg("sine=f=440:d=61,volume=0.3")
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "0:a",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
        ])
        .arg(&two)
        .status()
        .unwrap();
    assert!(st.success());
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let t0 = Instant::now();
    let s = gmclip_app::importsheet::inspect(&c, two.to_str().unwrap()).unwrap();
    eprintln!("inspect {:?}", t0.elapsed());
    let mut show = s.clone();
    for t in show["tracks"].as_array_mut().unwrap() {
        t["preview"] = t["preview"].as_str().map(|p| p.len()).into();
    }
    eprintln!("{}", serde_json::to_string_pretty(&show).unwrap());
    assert_eq!(s["ask"], true);
    assert_eq!(s["sheet"], true);
    assert_eq!(s["video"]["level"], "ok");
    let tracks = s["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    let voice = tracks[1]["index"].as_u64().unwrap();
    for t in tracks {
        assert_eq!(t["line"]["level"], "ok");
        // 3 s at 16 kHz, 16-bit: 96000 bytes → 128000 base64 chars
        assert_eq!(t["preview"].as_str().unwrap().len(), 128_000);
    }
    if tracks[1]["speech"].is_number() {
        assert_eq!(s["suggested"], voice, "the voice track is suggested");
        assert!(tracks[1]["speech"].as_f64().unwrap() > tracks[0]["speech"].as_f64().unwrap());
    } else {
        eprintln!("no VAD model here: no suggestion");
    }

    // open with the mic: the source's voice track is set before transcription
    let v = gmclip_app::video::open_video_with(&c, &two, Some(voice as u32)).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let src = doc.iter::<gmclip_model::entities::Source>().next().unwrap();
    assert_eq!(src.audio_roles.voice, Some(voice as u32));
    assert_eq!(src.probe.audio_tracks.len(), 2);
    // a known file does not ask again
    assert_eq!(
        gmclip_app::importsheet::inspect(&c, two.to_str().unwrap()).unwrap()["sheet"],
        false
    );
    // transcription hears the voice (the tone track has no words)
    let sid = src.id.0.to_string();
    let pr = loop {
        let pr = c.analysis.progress(&pid, &sid).unwrap();
        if pr.phase == "done" || pr.phase == "failed" || t0.elapsed() > Duration::from_secs(300) {
            break pr;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if pr.asr_error.is_none() {
        let text: String = pr
            .words
            .iter()
            .map(|w| w.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            pr.words.len() > 50 && text.contains("banana"),
            "{} words",
            pr.words.len()
        );
    }
    // the core command refuses a track the source does not have
    assert!(
        c.call(
            &pid,
            "source_audio_roles_set",
            serde_json::json!({"source": sid, "voice": 99})
        )
        .is_err()
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// HEVC video and 44.1 kHz mono audio: the sheet shows, with the exact fixes.
#[test]
fn unsupported_codecs_show_their_fix() {
    let (true, Some(ff)) = (Path::new(video()).is_file(), ffmpeg()) else {
        eprintln!("skipped: no test video or ffmpeg");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-imp2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let f = root.join("hevc.mp4");
    let st = std::process::Command::new(ff)
        .args([
            "-v",
            "error",
            "-y",
            "-t",
            "2",
            "-i",
            video(),
            "-c:v",
            "hevc_videotoolbox",
            "-tag:v",
            "hvc1",
            "-c:a",
            "aac",
            "-ar",
            "44100",
            "-ac",
            "1",
        ])
        .arg(&f)
        .status()
        .unwrap();
    if !st.success() {
        eprintln!("skipped: this ffmpeg cannot write HEVC");
        return;
    }
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let s = gmclip_app::importsheet::inspect(&c, f.to_str().unwrap()).unwrap();
    eprintln!("{s}");
    assert_eq!(s["sheet"], true);
    assert_eq!(s["ask"], false);
    assert_eq!(s["video"]["level"], "no");
    assert!(s["video"]["text"].as_str().unwrap().starts_with("HEVC"));
    assert!(
        s["video"]["fix"]
            .as_str()
            .unwrap()
            .contains("Apple VT H264 Hardware Encoder")
    );
    // decision 0051: 44.1 kHz is converted to 48 kHz as it is read
    let a = &s["tracks"][0]["line"];
    assert_eq!(a["level"], "ok");
    assert!(a["text"].as_str().unwrap().contains("44.1 kHz mono"));
    assert!(a["text"].as_str().unwrap().contains("converted to 48 kHz"));
    assert_eq!(s["notes"][0].as_str().unwrap(), gmclip_app::caps::ONE_TRACK);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Mean volume (dB) of a file's audio, optionally after a filter.
#[cfg(any(target_os = "macos", windows))]
fn mean_db(ff: &str, f: &Path, filter: Option<&str>) -> f64 {
    let mut af = String::new();
    if let Some(x) = filter {
        af.push_str(x);
        af.push(',');
    }
    af.push_str("volumedetect");
    let o = std::process::Command::new(ff)
        .args(["-v", "info", "-nostats", "-i"])
        .arg(f)
        .args(["-af", &af, "-f", "null", "-"])
        .output()
        .unwrap();
    let e = String::from_utf8_lossy(&o.stderr);
    e.lines()
        .find_map(|l| l.split("mean_volume: ").nth(1))
        .and_then(|v| v.trim_end_matches(" dB").trim().parse().ok())
        .unwrap()
}

/// Lab (decision 0009): the export carries the chosen mic. On a file whose track 1 is a
/// 440 Hz tone and track 2 the voice, the plan names track 2 and the exported audio is
/// the voice (notching 440 Hz out barely changes it), where the first-track default
/// exports the tone (the notch removes nearly all of it).
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's media (cargo test -- --ignored)"]
fn the_export_carries_the_chosen_mic() {
    let (true, Some(ff)) = (Path::new(video()).is_file(), ffmpeg()) else {
        eprintln!("skipped: no test video or ffmpeg");
        return;
    };
    let root = std::env::temp_dir().join(format!("gmclip-imp3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let two = root.join("two-tracks.mp4");
    assert!(
        std::process::Command::new(ff)
            .args([
                "-v",
                "error",
                "-y",
                "-t",
                "12",
                "-i",
                video(),
                "-f",
                "lavfi",
                "-i"
            ])
            .arg("sine=f=440:d=12,volume=0.3")
            .args([
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "0:a",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-shortest"
            ])
            .arg(&two)
            .status()
            .unwrap()
            .success()
    );
    let notch = "bandreject=f=440:width_type=h:w=200,bandreject=f=440:width_type=h:w=200";
    let export = |tag: &str, voice: Option<u32>| -> std::path::PathBuf {
        let c = AppCore::new(Dirs {
            app_data: root.join(format!("app-{tag}")),
            library: root.join(format!("lib-{tag}")),
            exports: root.join(format!("exports-{tag}")),
        });
        let v = gmclip_app::video::open_video_with(&c, &two, voice).unwrap();
        let (pid, cut) = (
            v["project"].as_str().unwrap().to_owned(),
            v["cut"].as_str().unwrap().to_owned(),
        );
        let pl = c.planned(&pid, &cut).unwrap();
        let src = pl
            .plan
            .sources
            .iter()
            .find(|s| s.expect.as_ref().is_some_and(|e| e.audio.is_some()))
            .unwrap();
        assert_eq!(src.audio_track, voice, "the plan names the mic");
        let jobs = std::sync::Arc::new(gmclip_app::jobs::Jobs::default());
        let id = jobs.start_export(&c, &pid, &cut).unwrap()["job"]
            .as_str()
            .unwrap()
            .to_owned();
        loop {
            let st = jobs.list().into_iter().find(|x| x.id == id).unwrap();
            if st.state == "done" {
                break;
            }
            assert!(st.state != "failed", "export failed: {:?}", st.error);
            std::thread::sleep(Duration::from_millis(250));
        }
        let f = c.exports(&pid).unwrap()[0].file.clone();
        // let transcription finish: ending the process mid-ASR trips ONNX Runtime's teardown
        let sid = gmclip_app::analysis::status(&c, &pid)["sources"][0]["source"]
            .as_str()
            .unwrap()
            .to_owned();
        let t0 = Instant::now();
        while c
            .analysis
            .progress(&pid, &sid)
            .is_some_and(|p| p.phase != "done" && p.phase != "failed")
            && t0.elapsed() < Duration::from_secs(120)
        {
            std::thread::sleep(Duration::from_millis(200));
        }
        c.shutdown();
        f
    };
    let mic = export("mic", Some(3));
    let first = export("first", None);
    let (m0, m1) = (mean_db(ff, &mic, None), mean_db(ff, &mic, Some(notch)));
    let (f0, f1) = (mean_db(ff, &first, None), mean_db(ff, &first, Some(notch)));
    eprintln!("mic export {m0} dB → notched {m1} dB; first-track export {f0} dB → notched {f1} dB");
    assert!(m0 - m1 < 6.0, "the mic export is the voice");
    assert!(f0 - f1 > 15.0, "the default export is the tone");
    let _ = std::fs::remove_dir_all(&root);
}
