//! H.06 lab sweep (R-EXP-4, R-EXP-6): exports from the model in the app.
//!
//! - **The golden-11 cuts export and verify green**: the ten v5 reels (cuts of their
//!   three streams' project fixtures, as `lint_goldens` and `playback` find them) and
//!   h79 v4 (M0's imported RenderPlan) each go through the app's export job; each export's own verify passes,
//!   and a re-verify of the file against its record and its plan passes with the
//!   container checks run.
//! - **A decode failure names its step and frame, and Retry recovers**: a copy of h79 v4
//!   with its video samples zeroed from the middle on fails at the decoder, at an output
//!   frame past the start, saying which; with the file put back the same export runs
//!   through.
//!
//! Lab: VideoToolbox, Metal and the lab media (read only; the exports go to a temp
//! folder). `cargo test --test export_sweep -- --ignored --nocapture`.
#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::jobs::{JobView, Jobs};
/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

/// The ten v5 reels of the golden-11 set.
const REELS: [&str; 10] = [
    "loc_086da141_h03",
    "loc_086da141_h25",
    "loc_086da141_h47",
    "loc_4fadb756_h34",
    "loc_4fadb756_h37",
    "loc_4fadb756_h48",
    "loc_bd89f38b_h41",
    "loc_bd89f38b_h43",
    "loc_bd89f38b_h53",
    "loc_bd89f38b_h88",
];

/// A golden reel's stream (`loc_bd89f38b_h41` → `loc_bd89f38b`).
fn stream_of(hook: &str) -> &str {
    hook.rsplit_once('_').map_or(hook, |x| x.0)
}

/// A stream's project fixture's source files (`fixtures/projects/<vid>.json` in the lab media folder), in the
/// lab media folder.
fn fixture_sources(vid: &str) -> Vec<PathBuf> {
    let p = gmclip_model::lab::project(vid);
    let fx: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    fx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[0] == "source")
        .filter_map(|r| r[4]["path"].as_str())
        .map(gmclip_model::lab::media)
        .collect()
}

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-sweep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

/// Runs one export job to its end.
fn export(jobs: &Arc<Jobs>, c: &Arc<AppCore>, pid: &str, cut: &str) -> JobView {
    let id = jobs.start_export(c, pid, cut).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_owned();
    let t0 = Instant::now();
    loop {
        let s = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if matches!(s.state.as_str(), "done" | "failed" | "cancelled") {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(900), "export stuck");
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "lab: VideoToolbox, Metal and the lab media; exports 11 reels"]
fn the_golden_eleven_export_and_verify_green() {
    // the golden cuts whose files are here: a reel needs its stream fixture's sources,
    // h79 v4 its plan's
    let h79_plan =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/examples/h79-v4.renderplan.json");
    let set = gmclip_model::lab::present(
        REELS.into_iter().chain(["loc_4fadb756_h79"]),
        |h| h.to_string(),
        |h| {
            if *h == "loc_4fadb756_h79" {
                let mut plan = gmclip_plan::parse(&std::fs::read(&h79_plan).unwrap()).unwrap();
                plan.resolve_paths(&gmclip_model::lab::root());
                plan.sources
                    .iter()
                    .map(|s| PathBuf::from(&s.path))
                    .collect()
            } else {
                fixture_sources(stream_of(h))
            }
        },
    );
    if set.skip_if_empty() {
        return;
    }
    let (c, root) = core("rt11");
    // (hook id, project, cut)
    let mut cuts: Vec<(String, String, String)> = Vec::new();
    let mut vids: Vec<&str> = set
        .items
        .iter()
        .filter(|h| REELS.contains(h))
        .map(|h| stream_of(h))
        .collect();
    vids.dedup();
    for vid in vids {
        let imp = gmclip_app::labfixture::open_project(&c, vid).unwrap();
        let pid = imp["id"].as_str().unwrap().to_owned();
        for r in imp["fixture"]["cuts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let (Some(h), Some(cut)) = (r["hook_id"].as_str(), r["cut"].as_str()) else {
                continue;
            };
            if set.items.contains(&h) && !cuts.iter().any(|(x, _, _)| x == h) {
                cuts.push((h.to_owned(), pid.clone(), cut.to_owned()));
            }
        }
    }
    if set.items.contains(&"loc_4fadb756_h79") {
        let made = c.create_project("h79").unwrap();
        let pid = made["id"].as_str().unwrap().to_owned();
        let r = c
            .import(
                &pid,
                &gmclip_app::labfixture::plan_copy(&h79_plan, &root.join("plans")).unwrap(),
            )
            .unwrap();
        cuts.push((
            "loc_4fadb756_h79".into(),
            pid,
            r["cut"].as_str().unwrap().to_owned(),
        ));
    }
    assert_eq!(
        cuts.len(),
        set.items.len(),
        "the golden set on this machine: {:?}",
        cuts.iter().map(|x| &x.0).collect::<Vec<_>>()
    );
    let jobs = Arc::new(Jobs::default());
    let mut rows = Vec::new();
    let mut red = Vec::new();
    for (hook, pid, cut) in &cuts {
        let t0 = Instant::now();
        let j = export(&jobs, &c, pid, cut);
        let secs = t0.elapsed().as_secs_f64();
        if j.state != "done" {
            red.push(format!("{hook}: {} ({:?})", j.state, j.error));
            continue;
        }
        let cv = c.cut_view(pid, cut).unwrap();
        let at_export = cv["export"]["verify"]["passed"] == true;
        let eid = cv["export"]["id"].as_str().unwrap().to_owned();
        let rv = gmclip_app::exportverify::reverify(&c, pid, &eid).unwrap();
        let checks = rv["checks"].as_array().map_or(0, Vec::len);
        let green = at_export
            && rv["passed"] == true
            && checks > 2
            && !rv["not_run"].to_string().contains("cut changed");
        rows.push(format!(
            "{hook}: {} frames in {secs:.1} s; verify at export {}, re-verify {} ({checks} checks)",
            j.frames,
            if at_export { "green" } else { "RED" },
            if rv["passed"] == true { "green" } else { "RED" },
        ));
        if !green {
            red.push(format!("{hook}: {}", rv["summary"]));
        }
        // the export file is not needed past its verify (disk)
        if let Some(f) = c
            .exports(pid)
            .unwrap()
            .into_iter()
            .find(|e| e.id == eid)
            .map(|e| e.file)
        {
            let _ = std::fs::remove_file(f);
        }
    }
    for r in &rows {
        eprintln!("{r}");
    }
    eprintln!("H.06 sweep on {}", set.note("golden cuts"));
    assert!(red.is_empty(), "{red:#?}");
    assert_eq!(rows.len(), set.items.len());
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "lab: VideoToolbox, Metal and AJ's media"]
fn a_decode_failure_names_its_step_and_frame_and_retry_recovers() {
    if gmclip_model::lab::skip_without([video()]) {
        return;
    }
    let (c, root) = core("decode");
    std::fs::create_dir_all(&root).unwrap();
    let copy = root.join("broken.mp4");
    std::fs::copy(video(), &copy).unwrap();
    let v = gmclip_app::video::open_video(&c, &copy).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    c.analysis.cancel_project(&pid);
    // the picture's samples from the middle of the video on: zeros (the sound is left
    // alone, so the mix, made first, still reads)
    let idx = {
        let mut f = gmclip_media::demux::io::FileSource::open(&copy).unwrap();
        let d = gmclip_media::demux::index(&mut f).unwrap();
        d.tracks
            .into_iter()
            .find(|t| t.kind == gmclip_media::demux::TrackKind::Video)
            .unwrap()
            .samples
    };
    let mut bytes = std::fs::read(&copy).unwrap();
    for i in idx.len() / 2..idx.len() {
        let (o, z) = (idx.offset[i] as usize, idx.size[i] as usize);
        bytes[o..o + z].fill(0);
    }
    std::fs::write(&copy, &bytes).unwrap();
    let jobs = Arc::new(Jobs::default());
    let j = export(&jobs, &c, &pid, &cut);
    let err = j.error.clone().unwrap_or_default();
    eprintln!("failed export: {} — {err}", j.state);
    assert_eq!(j.state, "failed");
    // "<step> failed at frame <n>: source <id> frame <m>[, OSStatus <s>]"
    let at = err
        .split(" failed at frame ")
        .nth(1)
        .and_then(|r| r.split(':').next())
        .and_then(|f| f.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no output frame named: {err}"));
    let step = err
        .split(" failed at frame ")
        .next()
        .and_then(|h| h.rsplit(' ').next())
        .unwrap_or_default();
    // Windows: the zeroed samples fail where they become Annex B for Media Foundation
    // (their length prefixes), before the decoder
    let want = if cfg!(windows) {
        "nal_length_truncated"
    } else {
        "vt_decode"
    };
    assert_eq!(step, want, "the decoder's step: {err}");
    assert!(
        at > 0 && at < j.frames,
        "a frame inside the cut: {at} of {}",
        j.frames
    );
    assert!(err.contains("source "), "{err}");
    // put the file back; the same export (Retry) runs through
    std::fs::copy(video(), &copy).unwrap();
    let j2 = export(&jobs, &c, &pid, &cut);
    assert_eq!(j2.state, "done", "{:?}", j2.error);
    assert_eq!(
        c.cut_view(&pid, &cut).unwrap()["export"]["verify"]["passed"],
        true
    );
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
