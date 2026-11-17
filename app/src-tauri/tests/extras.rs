//! Recommended extras of 2026-10-16 (lane K5) through the app core: the I/O range
//! ripple-deleted as one undo step, both framings exported as two jobs with their own
//! records, the master's keyframes in the filmstrip before a proxy exists, and the end
//! card that points a short at its long-form.
//!
//! Synthetic projects where no picture is decoded; the steps that decode a real frame or
//! export use AJ's h79 reel (read only) and skip without it.

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-extras-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

fn project(c: &Arc<AppCore>, name: &str) -> String {
    c.create_project(name).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A source of `secs` seconds with a `w`×`h` 30 fps picture (no file behind it).
fn source(c: &AppCore, pid: &str, name: &str, secs: i64, w: u32, h: u32) -> String {
    c.call(
        pid,
        "source_add",
        json!({"path": format!("/nowhere/{name}.mp4"), "fingerprint": format!("sha256:{}", gmclip_ml::sha256::hex(name.as_bytes())),
               "probe": {"duration": secs * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264",
                          "width": w, "height": h, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                         "audio_tracks": []}}),
    )
    .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn cut_with(c: &AppCore, pid: &str, title: &str, src: &str, spans: &[(i64, i64)]) -> String {
    let cut = c.call(pid, "cut_create", json!({"title": title})).unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        pid,
        "framing_create",
        json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    )
    .unwrap();
    c.call(pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    for (a, b) in spans {
        c.call(
            pid,
            "part_add",
            json!({"cut": cut, "media": {"source": src}, "src_in": a * SEC, "src_out": b * SEC}),
        )
        .unwrap();
    }
    cut
}

/// `[out start, out end)` of every part, in output order.
fn outs(c: &AppCore, pid: &str, cut: &str) -> Vec<(u64, u64)> {
    c.cut_view(pid, cut).unwrap()["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["out"][0].as_u64().unwrap(), p["out"][1].as_u64().unwrap()))
        .collect()
}

fn frames(c: &AppCore, pid: &str, cut: &str) -> u64 {
    outs(c, pid, cut).iter().map(|(a, b)| b - a).sum()
}

#[test]
fn the_marked_range_leaves_the_cut_in_one_undo_step() {
    let (c, root) = core("range");
    let pid = project(&c, "Keys");
    let s = source(&c, &pid, "talk", 600, 1920, 1080);
    // three parts of 20 s: [0, 600) [600, 1200) [1200, 1800) on the output clock
    let cut = cut_with(&c, &pid, "keys", &s, &[(10, 30), (100, 120), (200, 220)]);
    assert_eq!(outs(&c, &pid, &cut), [(0, 600), (600, 1200), (1200, 1800)]);

    // across a join: part 1 loses its tail, part 2 its head
    let r = c.delete_range(&pid, &cut, 300, 900).unwrap();
    assert_eq!(r["removed"], 600);
    assert_eq!(outs(&c, &pid, &cut), [(0, 300), (300, 600), (600, 1200)]);
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(
        outs(&c, &pid, &cut),
        [(0, 600), (600, 1200), (1200, 1800)],
        "one undo step"
    );

    // a whole part and more: the middle part goes, its neighbours are trimmed
    c.delete_range(&pid, &cut, 500, 1300).unwrap();
    assert_eq!(outs(&c, &pid, &cut), [(0, 500), (500, 1000)]);
    c.call(&pid, "undo", json!({})).unwrap();

    // inside one part: it splits around the range
    c.delete_range(&pid, &cut, 100, 200).unwrap();
    assert_eq!(
        outs(&c, &pid, &cut),
        [(0, 100), (100, 500), (500, 1100), (1100, 1700)]
    );
    assert_eq!(frames(&c, &pid, &cut), 1700);
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(outs(&c, &pid, &cut).len(), 3);

    // never the whole cut, never a backwards range, never past the end
    assert!(c.delete_range(&pid, &cut, 0, 1800).is_err());
    assert!(c.delete_range(&pid, &cut, 900, 300).is_err());
    assert!(c.delete_range(&pid, &cut, 5000, 6000).is_err());
    assert_eq!(frames(&c, &pid, &cut), 1800);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cut_with_one_framing_is_offered_the_other_before_both_export() {
    let (c, root) = core("framings");
    let pid = project(&c, "Both");
    let s = source(&c, &pid, "talk", 600, 1920, 1080);
    let cut = cut_with(&c, &pid, "both", &s, &[(10, 30)]);
    let shown = c.active_framing(&pid, &cut).unwrap();
    let v = gmclip_app::bothexport::view(&c, &pid, &cut).unwrap();
    assert_eq!(v["missing"], json!(["9:16"]), "{v}");
    assert_eq!(v["framings"][0]["aspect"], "16:9");
    assert_eq!(v["framings"][0]["export"]["state"], "never");
    assert_eq!(v["supported"], true);
    // not without the missing framing (the UI offers to make it first)
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let e = gmclip_app::bothexport::start(&c, &jobs, &pid, &cut, false).unwrap_err();
    assert!(e.message.contains("no 9:16 framing"), "{}", e.message);
    assert!(jobs.list().is_empty());
    // made from the layout defaults; the cut still shows its 16:9
    let m = gmclip_app::bothexport::make(&c, &pid, &cut, "9:16").unwrap();
    assert_eq!(m["made"], true);
    assert_eq!(c.active_framing(&pid, &cut).unwrap(), shown);
    let v = gmclip_app::bothexport::view(&c, &pid, &cut).unwrap();
    assert_eq!(v["missing"], json!([]));
    let aspects: Vec<&str> = v["framings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["aspect"].as_str().unwrap())
        .collect();
    assert_eq!(aspects, ["9:16", "16:9"]);
    assert_eq!(v["framings"][0]["w"], 1080);
    // the 9:16 plans as a Short (1080 × 1920) without being shown
    let pl = c
        .planned_in(&pid, &cut, Some(v["framings"][0]["id"].as_str().unwrap()))
        .unwrap();
    assert_eq!(
        (pl.plan.output.canvas.w.get(), pl.plan.output.canvas.h.get()),
        (1080, 1920)
    );
    // asked again: nothing new
    assert_eq!(
        gmclip_app::bothexport::make(&c, &pid, &cut, "9:16").unwrap()["made"],
        false
    );
    assert!(gmclip_app::bothexport::make(&c, &pid, &cut, "4:3").is_err());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's h79 reel; exports it twice"]
fn both_framings_export_as_two_jobs_with_their_own_records() {
    let Some(video) = h79() else { return };
    let (c, root) = core("both-lab");
    let v = gmclip_app::video::open_video(&c, &video).unwrap();
    let (pid, cut) = (s(&v, "project"), s(&v, "cut"));
    c.analysis.cancel_project(&pid);
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let r = gmclip_app::bothexport::start(&c, &jobs, &pid, &cut, true).unwrap();
    assert_eq!(r["made"], json!(["16:9"]), "{r}");
    let ids: Vec<String> = r["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["job"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 2);
    let t0 = std::time::Instant::now();
    for id in &ids {
        loop {
            let j = jobs.list().into_iter().find(|x| &x.id == id).unwrap();
            if j.state == "done" {
                break;
            }
            assert!(j.state != "failed", "{:?}", j.error);
            assert!(t0.elapsed().as_secs() < 600, "exports took too long");
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    eprintln!(
        "both framings exported in {:.1} s",
        t0.elapsed().as_secs_f64()
    );
    let ex = c.exports(&pid).unwrap();
    assert_eq!(ex.len(), 2);
    assert_ne!(ex[0].framing, ex[1].framing);
    assert!(ex.iter().all(|e| e.framing.is_some() && e.file.is_file()));
    assert_ne!(ex[0].file, ex[1].file);
    let names: Vec<String> = ex
        .iter()
        .map(|e| e.file.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|n| n.contains("9x16")), "{names:?}");
    assert!(names.iter().any(|n| n.contains("16x9")), "{names:?}");
    let v = gmclip_app::bothexport::view(&c, &pid, &cut).unwrap();
    for f in v["framings"].as_array().unwrap() {
        assert_eq!(f["export"]["state"], "fresh", "{f}");
    }
    // the cut's own export state reads the framing it shows
    assert_eq!(c.cut_view(&pid, &cut).unwrap()["export"]["state"], "fresh");
    c.analysis.shutdown(std::time::Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_short_from_a_stream_with_a_long_form_is_offered_an_end_card_to_it() {
    let (c, root) = core("endcard");
    let pid = project(&c, "Stream");
    let s = source(&c, &pid, "stream", 3600, 1920, 1080);
    c.call(&pid, "stream_edit_ensure", json!({})).unwrap();
    let w = |n: i64| json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": n * SEC, "t_src_end": n * SEC + SEC / 2, "normalised_text": "w"});
    let topic = |a: i64, b: i64, t: &str| {
        s_of(
            &c.call(
                &pid,
                "topic_set",
                json!({"start": w(a), "end": w(b), "title": t}),
            )
            .unwrap()["output"],
            "topic",
        )
    };
    let (t1, t2) = (
        topic(100, 400, "GPU prices"),
        topic(1000, 1200, "Benchmarks"),
    );
    let short = cut_with(&c, &pid, "the short", &s, &[(150, 160), (170, 175)]);
    // no long-form yet: nothing offered, and why
    let v = gmclip_app::endcard::view(&c, &pid, &short).unwrap();
    assert_eq!(v["available"], false);
    assert!(v["why"].as_str().unwrap().contains("No long-form"), "{v}");
    assert!(gmclip_app::endcard::add(&c, &pid, &short).is_err());
    // two long-forms: the one playing the short's moment
    let other = s_of(
        &c.call(
            &pid,
            "longform_from_topics",
            json!({"topics": [t2], "title": "Benchmarks"}),
        )
        .unwrap()["output"],
        "cut",
    );
    let long = s_of(
        &c.call(
            &pid,
            "longform_from_topics",
            json!({"topics": [t1], "title": "Every GPU priced"}),
        )
        .unwrap()["output"],
        "cut",
    );
    let v = gmclip_app::endcard::view(&c, &pid, &short).unwrap();
    assert_eq!(v["available"], true, "{v}");
    assert_eq!(v["longform"], long.as_str());
    assert_ne!(v["longform"], other.as_str());
    assert_eq!(v["title"], "Every GPU priced");
    assert_eq!(v["at_s"], 150.0);
    assert_eq!(v["has_card"], false);
    // the long-form's accepted YouTube title is the one the card shows
    let d = c
        .call(&pid, "copy_drafts_add", json!({"cut": long, "drafts": [{"platform": "youtube", "title": "I priced every GPU so you don't have to", "description": "x"}]}))
        .unwrap()["output"]["ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["title"],
        "Every GPU priced",
        "a proposal is not the title"
    );
    c.call(
        &pid,
        "copy_draft_state",
        json!({"draft": d, "state": "accepted"}),
    )
    .unwrap();
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["title"],
        "I priced every GPU so you don't have to"
    );
    // a long-form is not offered a card to itself
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &long).unwrap()["available"],
        false
    );
    // one click: the card (no picture: this recording has no file); one undo takes it off
    c.call(&pid, "font_add", json!({"family": "Figtree", "style": "Black", "sha256": "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef", "em_per_size": 0.8})).unwrap();
    let r = gmclip_app::endcard::add(&c, &pid, &short).unwrap();
    assert_eq!(r["picture"], false);
    assert_eq!(r["ids"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["has_card"],
        true
    );
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["has_card"],
        false
    );
    // Remove is one step too
    gmclip_app::endcard::add(&c, &pid, &short).unwrap();
    assert_eq!(
        gmclip_app::endcard::remove(&c, &pid, &short).unwrap()["removed"],
        3
    );
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["has_card"],
        false
    );
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(
        gmclip_app::endcard::view(&c, &pid, &short).unwrap()["has_card"],
        true
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's h79 reel"]
fn the_end_card_carries_a_real_frame_of_the_long_form() {
    let Some(video) = h79() else { return };
    let (c, root) = core("endcard-lab");
    let v = gmclip_app::video::open_video(&c, &video).unwrap();
    let pid = s(&v, "project");
    c.analysis.cancel_project(&pid);
    let src = c
        .project(&pid)
        .unwrap()
        .handle
        .snapshot()
        .doc
        .iter::<gmclip_model::entities::Source>()
        .next()
        .unwrap()
        .id
        .0
        .to_string();
    c.call(&pid, "stream_edit_ensure", json!({})).unwrap();
    let w = |n: i64| json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": n * SEC, "t_src_end": n * SEC + SEC / 2, "normalised_text": "w"});
    let t = s_of(
        &c.call(
            &pid,
            "topic_set",
            json!({"start": w(5), "end": w(50), "title": "Banana"}),
        )
        .unwrap()["output"],
        "topic",
    );
    c.call(
        &pid,
        "longform_from_topics",
        json!({"topics": [t], "title": "The talking banana, start to finish"}),
    )
    .unwrap();
    let short = cut_with(&c, &pid, "banana short", &src, &[(20, 30)]);
    c.call(&pid, "font_add", json!({"family": "Figtree", "style": "Black", "sha256": "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef", "em_per_size": 0.8})).unwrap();
    let r = gmclip_app::endcard::add(&c, &pid, &short).unwrap();
    assert_eq!(r["picture"], true, "{r}");
    assert_eq!(r["ids"].as_array().unwrap().len(), 4);
    // a 16:9 framing made afterwards (the output toggle, "Export 9:16 + 16:9") lays the
    // card out for its shape: no arrow there
    let wide = gmclip_app::bothexport::make(&c, &pid, &short, "16:9").unwrap()["framing"]
        .as_str()
        .unwrap()
        .to_owned();
    let pl = c.planned_in(&pid, &short, Some(&wide)).unwrap();
    assert!(
        !pl.plan
            .shape_events
            .iter()
            .flatten()
            .any(|e| matches!(e.shape, gmclip_plan::types::Shape::Arrow { .. }))
    );
    assert_eq!(pl.plan.image_events.as_ref().map_or(0, Vec::len), 1);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let a = doc.iter::<gmclip_model::entities::Asset>().next().unwrap();
    assert!(c.fonts().asset_file(&a.sha256).unwrap().is_file());
    c.analysis.shutdown(std::time::Duration::from_secs(10));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

fn s_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// The longest 48 kHz recording on this Mac (2 h 13 min, 720p H.264).
#[cfg(any(target_os = "macos", windows))]
fn long() -> std::path::PathBuf {
    gmclip_model::lab::media("preview/loc_684c44b6.proxy.mp4")
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + a 2 h recording; builds part of its proxy"]
fn a_long_recording_shows_its_first_frame_and_filmstrip_before_its_proxy() {
    if !long().as_path().is_file() {
        eprintln!("skipped: no long recording");
        return;
    }
    let r = gmclip_app::perf::open_first_frame(long().as_path()).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&r).unwrap());
    assert!(r["seconds"].as_f64().unwrap() > 7200.0);
    assert_eq!(
        r["proxy_frames_at_first_frame"], 0,
        "measured before any proxy"
    );
    assert!(r["first_frame_ms"].as_f64().unwrap() < 3000.0, "{r}");
    assert_eq!(
        r["seek_middle"]["scrub_path"], "keyframe",
        "the master's keyframe"
    );
    assert!(
        r["seek_middle"]["exact_ms"].as_f64().unwrap() < 3000.0,
        "{r}"
    );
    // the filmstrip has pictures past what the proxy covers
    let pics = r["filmstrip"]["pictures"].as_u64().unwrap();
    assert!(pics > 20, "{r}");
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn h79() -> Option<std::path::PathBuf> {
    let p = gmclip_model::lab::media("h79/h79-v4.mp4");
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: no h79 reel");
        None
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}
