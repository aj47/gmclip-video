//! H.06 `verify{export}` in the app (R-EXP-6): the cut view carries the last export's
//! verify; a re-verify checks the file (a regular file, never a symlink), its SHA-256
//! against its export record and, when the cut still plans to what was exported, its
//! container against that plan; a cut changed since is said, not guessed at. On AJ's h79
//! file; the full export → re-verify is lab (VideoToolbox).
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
use serde_json::json;

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-xv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

fn hex(b: &[u8]) -> String {
    gmclip_ai::launcher::sha256::hex(b)
}

/// A file and its export record, as an export of an older version of the cut.
fn fake_export(c: &AppCore, pid: &str, cut: &str, dir: &Path, id: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join(format!("{id}.mp4"));
    let bytes = b"not really an mp4, but these exact bytes".to_vec();
    std::fs::write(&file, &bytes).unwrap();
    let rec: gmclip_media::export_record::ExportRecord = serde_json::from_value(json!({
        "render_sig": "an older cut",
        "preset": "reels-1080x1920",
        "duration_expected": {"frames": 30, "fps_num": 30, "fps_den": 1, "audio_samples": 48000},
        "file": file.file_name().unwrap().to_string_lossy(),
        "sha256": hex(&bytes),
        "app_version": "test",
        "renderer_version": "test",
    }))
    .unwrap();
    rec.write(&file).unwrap();
    c.add_export(
        pid,
        ExportEntry {
            id: id.into(),
            fps: 30,
            fps_den: 1,
            cut: cut.into(),
            title: "x".into(),
            file: file.clone(),
            frames: 30,
            seconds: 1.0,
            render_sig: Some("an older cut".into()),
            at_ms: 1,
            notes: vec!["verified: 14 checks passed (B-15 verify v0)".into()],
            verdict: None,
            verify: gmclip_app::exportverify::at_export(
                &["verified: 14 checks passed (B-15 verify v0)".to_owned()],
                1,
            ),
            plan_sig: None,
            opening_sig: None,
            opening_label: None,
            framing: None,
        },
    )
    .unwrap();
    file
}

#[test]
fn reverify_checks_the_file_its_record_and_says_when_the_cut_changed() {
    if !Path::new(video()).is_file() {
        return;
    }
    let (c, root) = core("unit");
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let file = fake_export(&c, &pid, &cut, &root.join("out"), "e-1");

    // the cut view carries the verify the export came with
    let cv = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(cv["export"]["state"], "stale");
    assert_eq!(cv["export"]["verify"]["how"], "export");
    assert_eq!(cv["export"]["verify"]["passed"], true);

    // intact file and record; the cut changed since, so the plan checks are not run
    let r = gmclip_app::exportverify::reverify(&c, &pid, "e-1").unwrap();
    assert_eq!(r["passed"], true, "{r}");
    assert_eq!(r["how"], "reverify");
    let steps: Vec<&str> = r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["step"].as_str().unwrap())
        .collect();
    assert_eq!(steps, ["file", "sha256"]);
    let not_run = r["not_run"].to_string();
    assert!(
        not_run.contains("the cut changed after this export"),
        "{not_run}"
    );
    assert!(not_run.contains("audio alignment"), "{not_run}");
    // recorded on the entry, and shown as the cut's last verify
    assert_eq!(
        c.exports(&pid).unwrap()[0]
            .verify
            .as_ref()
            .map(|v| v.how.as_str()),
        Some("reverify")
    );
    let cv = c.cut_view(&pid, &cut).unwrap();
    assert_eq!(cv["export"]["verify"]["how"], "reverify");

    // the file changed after export → its SHA-256 no longer matches its record
    std::fs::write(&file, b"someone edited it").unwrap();
    let r = gmclip_app::exportverify::reverify(&c, &pid, "e-1").unwrap();
    assert_eq!(r["passed"], false);
    assert!(
        r["summary"]
            .as_str()
            .unwrap()
            .starts_with("failed at sha256"),
        "{r}"
    );
    assert_eq!(
        c.cut_view(&pid, &cut).unwrap()["export"]["verify"]["passed"],
        false
    );

    // a symlink at the export's path is not read
    let elsewhere = root.join("elsewhere.mp4");
    std::fs::rename(&file, &elsewhere).unwrap();
    // (Windows: a file symlink needs a privilege a test does not have; the rename has
    // left the export missing for the next check)
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
        let r = gmclip_app::exportverify::reverify(&c, &pid, "e-1").unwrap();
        assert_eq!(r["passed"], false);
        assert!(r["summary"].as_str().unwrap().contains("symlink"), "{r}");
        std::fs::remove_file(&file).unwrap();
    }
    // and a missing one says so
    let r = gmclip_app::exportverify::reverify(&c, &pid, "e-1").unwrap();
    assert!(
        r["summary"].as_str().unwrap().starts_with("failed at file"),
        "{r}"
    );
    assert!(gmclip_app::exportverify::reverify(&c, &pid, "nope").is_err());

    // the entry survives a reopen with its verify (exports.json)
    let saved =
        std::fs::read_to_string(c.project(&pid).unwrap().home.join("exports.json")).unwrap();
    assert!(saved.contains("\"how\": \"reverify\""), "{saved}");
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "lab: needs VideoToolbox + AJ's media"]
fn a_fresh_export_reverifies_green_against_its_plan() {
    if !Path::new(video()).is_file() {
        return;
    }
    let (c, root) = core("lab");
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
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
    assert_eq!(cv["export"]["verify"]["how"], "export", "{}", cv["export"]);
    assert_eq!(cv["export"]["verify"]["passed"], true);
    let eid = cv["export"]["id"].as_str().unwrap().to_owned();
    let r = gmclip_app::exportverify::reverify(&c, &pid, &eid).unwrap();
    eprintln!("{r:#}");
    assert_eq!(r["passed"], true, "{r}");
    let n = r["checks"].as_array().unwrap().len();
    assert!(n > 2, "container checks ran against the plan: {n}");
    assert!(!r["not_run"].to_string().contains("cut changed"));
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
