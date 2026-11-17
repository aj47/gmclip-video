//! U.09 on an opened talk: RT captions from the cut's own words (keywords, corrections),
//! a text hook until a word, a section pill, a measured run clock — planned and exported.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

fn texts(
    c: &AppCore,
    p: &str,
    cut: &str,
    role: &str,
) -> Vec<(u64, u64, String, Vec<Option<String>>)> {
    let pl = c.planned(p, cut).unwrap();
    pl.plan
        .text_events
        .as_ref()
        .map(|evs| {
            evs.iter()
                .filter(|e| gmclip_core::planner::text_role_name(&e.role) == role)
                .map(|e| {
                    (
                        e.out.start(),
                        e.out.end(),
                        e.runs.iter().map(|r| r.text.as_str()).collect::<String>(),
                        e.runs
                            .iter()
                            .map(|r| r.fill.as_ref().map(|f| f.as_str().to_owned()))
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn short_core() {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-short-{}", std::process::id()));
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
    let t0 = Instant::now();
    loop {
        let st = gmclip_app::analysis::status(&c, &pid);
        let ph = st["sources"][0]["phase"].as_str().unwrap_or("").to_owned();
        if ph == "done" || ph == "failed" || t0.elapsed() > Duration::from_secs(300) {
            if st["sources"][0]["asr_error"].is_string() {
                eprintln!("skipped: no speech model");
                return;
            }
            assert_eq!(ph, "done", "{st}");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();

    // Captions grouped as RT groups them: 1–3 words (4 only when short, ≤ 18 characters),
    // seated at y 1065, in order, not overlapping.
    gmclip_app::short::captions_set(&c, &pid, &cut, true).unwrap();
    let caps = texts(&c, &pid, &cut, "caption");
    eprintln!(
        "{} cues; first: {:?}",
        caps.len(),
        caps.iter().take(8).map(|c| c.2.clone()).collect::<Vec<_>>()
    );
    assert!(caps.len() > 40);
    assert!(caps.iter().all(|c| {
        let n = c.2.split_whitespace().count();
        (1..=3).contains(&n) || (n == 4 && c.2.trim().chars().count() <= 18)
    }));
    assert!(caps.windows(2).all(|w| w[0].1 <= w[1].0));
    let pl = c.planned(&pid, &cut).unwrap();
    let ev = &pl.plan.text_events.as_ref().unwrap()[0];
    assert!((ev.pos.y - 1065.0).abs() < 1.0, "{:?}", ev.pos);

    // A keyword turns its run yellow.
    let tv = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    let rev = tv["revision"].as_str().unwrap().to_owned();
    let k = tv["frame"]
        .as_array()
        .unwrap()
        .iter()
        .position(|f| f.as_i64().unwrap() > 90)
        .unwrap();
    c.call(
        &pid,
        "caption_keyword_toggle",
        json!({"cut": cut, "word": {"asr_revision": rev, "ordinal": k}}),
    )
    .unwrap();
    assert!(
        texts(&c, &pid, &cut, "caption")
            .iter()
            .any(|c| c.3.iter().any(|f| f.as_deref() == Some("#FFD400FF")))
    );

    // A correction re-spells every match.
    let word = tv["text"][k]
        .as_str()
        .unwrap()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    let r = gmclip_app::short::caption_fix(&c, &pid, &cut, &word, "ZZTOP").unwrap();
    assert!(r["words"].as_u64().unwrap() >= 1);
    assert!(
        texts(&c, &pid, &cut, "caption")
            .iter()
            .any(|c| c.2.contains("ZZTOP"))
    );
    let sv = gmclip_app::short::short_view(&c, &pid, &cut).unwrap();
    assert_eq!(sv["fixes"][0]["to"], "ZZTOP");

    // The hook: frame 0 until the end of word 8.
    gmclip_app::short::hook_set(
        &c,
        &pid,
        &cut,
        "Every project gets its own VM",
        Some(8),
        None,
    )
    .unwrap();
    let hook = texts(&c, &pid, &cut, "text_hook");
    let w8_end = (tv["start_ms"][8].as_i64().unwrap() + tv["dur_ms"][8].as_i64().unwrap()) as f64
        / 1000.0
        * 30.0;
    assert_eq!(hook.len(), 1);
    assert_eq!(hook[0].2, "Every project\ngets its own VM");
    assert_eq!(hook[0].0, 0);
    assert!(
        (hook[0].1 as f64 - w8_end).abs() <= 1.0,
        "{hook:?} vs {w8_end}"
    );

    // A pill on part 1, then a measured run clock (frames 120 → 400: 9.33 s).
    gmclip_app::short::pill_add(
        &c,
        &pid,
        &cut,
        c.cut_view(&pid, &cut).unwrap()["parts"][0]["id"]
            .as_str()
            .unwrap(),
        "MY DEV VM SETUP",
    )
    .unwrap();
    let pill = texts(&c, &pid, &cut, "section_pill");
    assert_eq!(pill[0].2, "MY DEV VM SETUP");
    assert_eq!(
        pill[0].0, hook[0].1,
        "the pill waits for the hook to clear the band"
    );
    let m = gmclip_app::short::run_measure(&c, &pid, &cut, 120, 400).unwrap();
    assert!(
        (m["seconds"].as_f64().unwrap() - 280.0 / 30.0).abs() < 0.05,
        "{m}"
    );
    gmclip_app::short::clock_add(
        &c,
        &pid,
        &cut,
        m["measurement"].as_str().unwrap(),
        "BOOTING",
        "DONE IN",
    )
    .unwrap();
    let clock = texts(&c, &pid, &cut, "run_clock");
    eprintln!(
        "clock: {:?} … {:?}",
        &clock[..2.min(clock.len())],
        clock.last()
    );
    assert_eq!(clock[0].0, 120);
    assert!(clock[0].2.starts_with("BOOTING 0:0"));
    assert_eq!(clock.last().unwrap().2, "DONE IN 0:09");
    assert!(clock.iter().any(|c| c.2 == "BOOTING 0:05"));
    assert_eq!(
        texts(&c, &pid, &cut, "section_pill")[0].1,
        clock[0].0,
        "the pill hands the band to the clock"
    );

    // It exports.
    let jobs = std::sync::Arc::new(gmclip_app::jobs::Jobs::default());
    jobs.start_export(&c, &pid, &cut).unwrap();
    let j = loop {
        let j = jobs.list().into_iter().next().unwrap();
        if j.state != "running" && j.state != "queued" {
            break j;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(j.state, "done", "{:?}", j.error);
    let e = c.project(&pid).unwrap().exports.lock().unwrap()[0].clone();
    if let Ok(dir) = std::env::var("GMCLIP_KEEP_EXPORT") {
        let _ = std::fs::copy(&e.file, Path::new(&dir).join("short-core.mp4"));
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
