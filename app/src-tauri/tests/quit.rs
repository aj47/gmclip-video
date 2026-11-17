//! Quitting while a transcription runs: the analysis threads are cancelled and joined
//! before the process exits, so ONNX Runtime's static teardown never meets a thread
//! still inside it (it aborted with "recursive_mutex lock failed"). This test binary's
//! own exit is the check: it used to die with SIGABRT after the test passed.
//!
//! And quitting is prompt (QA): the job stops between steps, not after them. It took
//! ~7 s on an idle Mac (the speech model's 640 MB pin check ran to its end, then the
//! model loaded, then a window was heard) and over 10 s on a busy one.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[test]
fn quitting_mid_transcription_exits_cleanly() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-quit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for _ in 0..2 {
        let c = AppCore::new(Dirs {
            app_data: root.join("app"),
            library: root.join("lib"),
            exports: root.join("exports"),
        });
        let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
        let pid = v["project"].as_str().unwrap().to_owned();
        // wait until the model is transcribing, then quit
        let t0 = Instant::now();
        loop {
            let st = gmclip_app::analysis::status(&c, &pid);
            if st.to_string().contains("transcribing") || t0.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(400));
        let t1 = Instant::now();
        c.shutdown();
        eprintln!("shutdown took {:?}", t1.elapsed());
        assert!(
            t1.elapsed() < Duration::from_secs(2),
            "quitting mid-transcription took {:?}",
            t1.elapsed()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
