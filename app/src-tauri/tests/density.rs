//! D.04: the local density gate matches the reference numbers on five of AJ's streams
//! (moments, active-talk minutes, rate, verdict, uncarded stretches), each a project
//! fixture measured from its transcript and moments; the reference is
//! `lab/fixtures/density/reference.json`. The flagged path runs on a thinned stream
//! (`lab/fixtures/density/Tg4qTpEDRRo-thinned.json`). Committed fixtures only: runs
//! everywhere.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::Value;
const STREAMS: [&str; 5] = [
    "Btcs-kuuQ2Y",
    "loc_54b47e51",
    "Tg4qTpEDRRo",
    "loc_4fadb756",
    "loc_086da141",
];

fn reference() -> Vec<Value> {
    let v: Value = serde_json::from_slice(
        &std::fs::read(gmclip_model::lab::fixture("density/reference.json")).unwrap(),
    )
    .unwrap();
    v["streams"].as_array().unwrap().clone()
}

#[test]
fn density_matches_the_reference_on_five_streams() {
    let theirs = reference();
    let root = std::env::temp_dir().join(format!("gmclip-density-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let mut compared = 0;
    for vid in STREAMS {
        let Some(r) = theirs.iter().find(|r| r["video_id"] == vid) else {
            continue;
        };
        let imp = gmclip_app::labfixture::open_project(&c, vid).unwrap();
        let pid = imp["id"].as_str().unwrap().to_owned();
        c.analysis.cancel_project(&pid);
        let p = c.project(&pid).unwrap();
        let list = gmclip_app::review::moments_list(&p, &c.transcripts);
        let videos = list["videos"].as_array().unwrap();
        let d = videos
            .iter()
            .find(|v| !v["density"].is_null())
            .map(|v| v["density"].clone())
            .unwrap_or_else(|| panic!("{vid}: no density"));
        let f = |v: &Value, k: &str| v[k].as_f64().unwrap();
        eprintln!(
            "{vid}: ours {} moments, {:.1} active min, {:.2}/min, {} stretches; reference {} moments, {} min, {}/min, {} gaps",
            d["moments"],
            f(&d, "active_min"),
            f(&d, "per_min"),
            d["stretches"].as_array().unwrap().len(),
            r["hooks"],
            r["active_min"],
            r["per_min"],
            r["gaps"].as_array().unwrap().len()
        );
        assert_eq!(d["moments"], r["hooks"], "{vid}");
        // the reference counts a few lines whose end is before their start as negative
        // talk, and the fixtures hold no such line, so the active minutes may differ by
        // those lines (0.2 % on loc_54b47e51)
        let am = f(r, "active_min");
        assert!(
            (f(&d, "active_min") - am).abs() <= 0.051f64.max(am * 0.005),
            "{vid}"
        );
        assert!(
            (f(&d, "per_min") - f(r, "per_min")).abs() <= 0.0051,
            "{vid}"
        );
        let near = |a: &Value, b: &Value| (a.as_i64().unwrap() - b.as_i64().unwrap()).abs() <= 1;
        assert!(near(&d["target"], &r["target"]), "{vid}");
        assert!(near(&d["short_by"], &r["short_by"]), "{vid}");
        assert_eq!(d["ok"], r["ok"], "{vid}");
        let (ours, gaps) = (
            d["stretches"].as_array().unwrap(),
            r["gaps"].as_array().unwrap(),
        );
        assert_eq!(ours.len(), gaps.len(), "{vid}");
        for (a, b) in ours.iter().zip(gaps) {
            assert!((f(a, "start_s") - f(b, "t_start")).abs() <= 0.051, "{vid}");
            assert!((f(a, "end_s") - f(b, "t_end")).abs() <= 0.051, "{vid}");
            assert_eq!(a["words"], b["words"], "{vid}");
        }
        compared += 1;
        let _ = c.close_project(&pid);
    }
    assert_eq!(compared, STREAMS.len());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The flagged path: one stream thinned to every eighth moment is under the floor, with
/// the reference's uncarded stretches.
#[test]
fn a_thinned_stream_is_flagged_with_the_same_stretches() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(gmclip_model::lab::fixture(
            "density/Tg4qTpEDRRo-thinned.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let lines: Vec<gmclip_core::density::Line> = fx["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| gmclip_core::density::Line {
            start: l[0].as_f64().unwrap(),
            end: l[1].as_f64().unwrap(),
            words: l[2].as_u64().unwrap() as usize,
        })
        .collect();
    let spans: Vec<(f64, f64)> = fx["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s[0].as_f64().unwrap(), s[1].as_f64().unwrap()))
        .collect();
    let r = &fx["reference"];
    let d = gmclip_core::density::density(&lines, &spans).unwrap();
    eprintln!(
        "thinned Tg4qTpEDRRo: ours {:.2}/min ok={} {} stretches; reference {}/min ok={} {} gaps",
        d.per_min,
        d.ok,
        d.stretches.len(),
        r["per_min"],
        r["ok"],
        r["gaps"].as_array().unwrap().len()
    );
    assert!(!d.ok);
    assert_eq!(r["ok"], false);
    assert_eq!(d.moments as u64, r["hooks"].as_u64().unwrap());
    assert!((d.active_min - r["active_min"].as_f64().unwrap()).abs() <= 0.051);
    assert!((d.per_min - r["per_min"].as_f64().unwrap()).abs() <= 0.0051);
    assert_eq!(d.target, r["target"].as_i64().unwrap());
    assert_eq!(d.short_by, r["short_by"].as_i64().unwrap());
    let gaps = r["gaps"].as_array().unwrap();
    assert!(!gaps.is_empty());
    assert_eq!(d.stretches.len(), gaps.len());
    for (a, b) in d.stretches.iter().zip(gaps) {
        assert!((a.start_s - b["t_start"].as_f64().unwrap()).abs() <= 0.051);
        assert!((a.end_s - b["t_end"].as_f64().unwrap()).abs() <= 0.051);
        assert_eq!(a.words as u64, b["words"].as_u64().unwrap());
    }
}
