//! H.06 rest: the Mac is kept awake during an export (R-JOB-2, caffeinate's power
//! assertion, released after), and the cut view says what the export does to loudness
//! and what the last one measured. Lab for the export.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};

#[test]
fn keep_awake_holds_and_releases() {
    let mut k = gmclip_app::jobs::KeepAwake::new();
    // (Windows: a power request; listing it, `powercfg /requests`, needs an admin)
    assert!(
        k.held(),
        "caffeinate -i is running / the power request is set"
    );
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/pmset")
            .args(["-g", "assertions"])
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&out.stdout);
        assert!(
            txt.contains("caffeinate"),
            "the assertion is visible to the OS"
        );
    }
    drop(k);
    // a second hold after the first is released
    assert!(gmclip_app::jobs::KeepAwake::new().held());
}

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's media"]
fn loudness_is_shown_before_and_measured_after() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-exx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let cv = c.cut_view(&pid, &cut).unwrap();
    eprintln!("before: {}", cv["loudness"]);
    assert!(cv["loudness"]["mode"].is_string());
    assert!(cv["loudness"]["last"].is_null());
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let id = jobs.start_export(&c, &pid, &cut).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_owned();
    loop {
        let s = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if s.state == "done" {
            break;
        }
        assert!(s.state != "failed", "{:?}", s.error);
        std::thread::sleep(Duration::from_millis(250));
    }
    let cv = c.cut_view(&pid, &cut).unwrap();
    eprintln!("after: {} {}", cv["loudness"], cv["export"]);
    eprintln!(
        "sigs: export {:?} now {}",
        c.exports(&pid).unwrap()[0].render_sig,
        cv["render_sig"]
    );
    assert_eq!(cv["export"]["state"], "fresh", "{}", cv["export"]);
    assert!(cv["loudness"]["last"]["integrated"].is_number());
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
