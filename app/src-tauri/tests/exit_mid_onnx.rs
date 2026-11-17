//! Exiting while a thread is inside ONNX Runtime, with no quit first: the test returns as
//! the speech model starts loading, so the test binary's `exit` runs while the analysis
//! thread is inside `CreateSession`. ONNX Runtime's static teardown then crashed the
//! binary (SIGSEGV in `DataTypeImpl::GetDataType`, the `proxy` binary on 2026-10-16,
//! after its quit's 10 s wait ran out on a loaded Mac; or SIGABRT "recursive_mutex lock
//! failed"). The exit gate (`gmclip_ml::runtime::quiesce`, registered by the core) waits
//! for the load and lets nothing in after it. This binary's own exit is the check.

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
fn exiting_mid_model_load_waits_for_onnx_runtime() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-exitmid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    // until a thread is inside ONNX Runtime (loading a model, or running one)
    let t0 = Instant::now();
    while gmclip_ml::runtime::calls_inside() == 0 {
        if t0.elapsed() > Duration::from_secs(300) {
            let st = gmclip_app::analysis::status(&c, &pid);
            eprintln!("skipped: nothing entered ONNX Runtime (no speech model?): {st}");
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "exiting with {} ONNX Runtime calls inside",
        gmclip_ml::runtime::calls_inside()
    );
    // no quit: the core and its threads are left running into the process's exit
    std::mem::forget(c);
}
