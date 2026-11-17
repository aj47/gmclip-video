//! U.06 remainder on AJ's h79 reel: ＋ add as part, ↧ stretch the last part, re-spell
//! (captions only), word-edge tuning that moves where a strike cuts (sample-exact on the
//! frame grid), ↺ edges. Skips without the reel or the speech model.

use std::path::Path;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
const F: i64 = 705_600_000;

fn parts(c: &AppCore, pid: &str, cut: &str) -> Vec<(i64, i64)> {
    let p = c.project(pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(cut).unwrap();
    gmclip_model::clock::cut_layout(&doc, cid)
        .iter()
        .filter_map(|sp| doc.get(sp.part))
        .map(|pt: &gmclip_model::entities::Part| (pt.src_in.0, pt.src_out.0))
        .collect()
}

/// Snaps a source time to the 30 fps grid as the app does (nearest, ties earlier).
fn snap(t: i64) -> i64 {
    let fpf = F / 30;
    ((t * 2 + fpf - 1) / (2 * fpf)) * fpf
}

#[test]
fn add_stretch_respell_and_tune() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-wt-{}", std::process::id()));
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
    let tv = loop {
        let tv = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
        if tv["state"] == "ok" && tv["text"].as_array().is_some_and(|t| t.len() > 100) {
            break tv;
        }
        if t0.elapsed() > Duration::from_secs(180) {
            eprintln!("skipped: no transcript (speech model?)");
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let ms = |k: &str, i: usize| tv[k][i].as_i64().unwrap();
    let start = |i: usize| ms("start_ms", i) * F / 1000;
    let end = |i: usize| (ms("start_ms", i) + ms("dur_ms", i)) * F / 1000;

    // Tune word 40's start 50 ms later, then strike 40..=45: the cut lands on the tuned
    // edge (on the frame grid), not the ASR one.
    gmclip_app::words::nudge(&c, &pid, &cut, 40, "start", 50, None).unwrap();
    let tv2 = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    assert_eq!(tv2["tuned"]["40"], serde_json::json!([50, 0]));
    gmclip_app::words::strike(&c, &pid, &cut, 40, 45, false).unwrap();
    let ps = parts(&c, &pid, &cut);
    assert_eq!(ps.len(), 2, "{ps:?}");
    // the start in ms is rounded; the tuned edge is ASR start + 50 ms exactly
    let asr40 = tv["start_ms"][40].as_i64().unwrap();
    let tuned40 = (asr40 + 50) * F / 1000;
    assert!(
        (ps[0].1 - snap(tuned40)).abs() <= F / 30,
        "{} vs {}",
        ps[0].1,
        snap(tuned40)
    );
    // ↺ edges
    gmclip_app::words::edge_reset(&c, &pid, &cut, 40, None).unwrap();
    let tv3 = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    assert!(tv3["tuned"].get("40").is_none(), "{}", tv3["tuned"]);

    // ＋ add as part: the struck words come back as a new last part, on their edges.
    gmclip_app::words::add_as_part(&c, &pid, &cut, 40, 45, None).unwrap();
    let ps = parts(&c, &pid, &cut);
    assert_eq!(ps.len(), 3);
    let last = ps[2];
    assert!(
        (last.0 - snap(start(40))).abs() <= F / 30 && (last.1 - snap(end(45))).abs() <= F / 30,
        "{last:?}"
    );

    // ↧ stretch: the last part now ends at word 60.
    gmclip_app::words::stretch_last(&c, &pid, &cut, 60, None).unwrap();
    let ps = parts(&c, &pid, &cut);
    assert!((ps[2].1 - snap(end(60))).abs() <= F / 30, "{:?}", ps[2]);

    // Re-spell: captions only (the transcript's own text and the parts are unchanged).
    gmclip_app::words::respell(&c, &pid, &cut, 10, Some("Bananas"), None).unwrap();
    gmclip_app::words::respell(&c, &pid, &cut, 11, Some(""), None).unwrap();
    let tv4 = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    assert_eq!(tv4["edits"]["10"], "Bananas");
    assert_eq!(tv4["edits"]["11"], "");
    assert_eq!(tv4["text"][10], tv["text"][10]);
    assert_eq!(parts(&c, &pid, &cut), ps);
    gmclip_app::words::respell(&c, &pid, &cut, 10, None, None).unwrap();
    let tv5 = gmclip_app::transcript::transcript_view(&c, &pid, &cut).unwrap();
    assert!(tv5["edits"].get("10").is_none());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
