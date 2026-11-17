//! U.05 through the app core: a stream's project fixture, its moments
//! listed with times from the imported transcript, a state change, an excerpt edit,
//! and the review plan (excerpt first word → last word + 1 s). Skips without the lab media.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

#[test]
fn stream_moments_list_state_excerpt_review() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-moments-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let t = std::time::Instant::now();
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    eprintln!(
        "fixture {:.1} s: {}",
        t.elapsed().as_secs_f64(),
        p["fixture"]
    );
    let pid = p["id"].as_str().unwrap().to_owned();
    let proj = c.project(&pid).unwrap();
    let t = std::time::Instant::now();
    let list = gmclip_app::review::moments_list(&proj, &c.transcripts);
    let ms = list["moments"].as_array().unwrap().clone();
    eprintln!(
        "{} moments listed in {:.0} ms (transcript: {})",
        ms.len(),
        t.elapsed().as_secs_f64() * 1000.0,
        list["transcript"]
    );
    assert!(ms.len() > 20, "{}", ms.len());
    assert_eq!(list["transcript"], true);
    let timed = ms.iter().filter(|m| !m["review_s"].is_null()).count();
    assert!(
        timed * 10 >= ms.len() * 9,
        "{timed} of {} have review spans",
        ms.len()
    );
    // A review span starts at or after the moment and runs ≥ 1 s.
    for m in ms.iter().filter(|m| !m["review_s"].is_null()).take(50) {
        let (a, b) = (
            m["review_s"][0].as_f64().unwrap(),
            m["review_s"][1].as_f64().unwrap(),
        );
        assert!(b - a >= 1.0, "{m}");
        assert!(a >= m["start_s"].as_f64().unwrap() - 2.01, "{m}");
    }
    // Stage one, edit an excerpt (one exact, one not).
    let m = ms
        .iter()
        .find(|m| m["state"] == "new" && !m["review_s"].is_null())
        .unwrap();
    let id = m["id"].as_str().unwrap();
    c.call(
        &pid,
        "moment_set_state",
        json!({"moment": id, "state": "staged"}),
    )
    .unwrap();
    let quote = m["quote"].as_str().unwrap();
    let words: Vec<&str> = quote.split_whitespace().collect();
    let exact = words[..words.len().min(4)].join(" ");
    let r = c
        .call(
            &pid,
            "moment_excerpt_set",
            json!({"moment": id, "text": exact}),
        )
        .unwrap();
    assert_eq!(r["output"]["exact"], true, "{r}");
    let r = c
        .call(
            &pid,
            "moment_excerpt_set",
            json!({"moment": id, "text": "words nobody said here"}),
        )
        .unwrap();
    assert_eq!(r["output"]["exact"], false);
    let again = gmclip_app::review::moments_list(&proj, &c.transcripts);
    let row: &Value = again["moments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    assert_eq!(row["state"], "staged");
    // The review plan for an exact excerpt plays its words.
    c.call(
        &pid,
        "moment_excerpt_set",
        json!({"moment": id, "text": exact}),
    )
    .unwrap();
    let (plan, (a, b)) = gmclip_app::review::review_for(&proj, &c.transcripts, id).unwrap();
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    assert!(b > a);
    eprintln!(
        "review {a:.2}–{b:.2} s, {} frames at {}x{}",
        plan.output.frames,
        plan.output.canvas.w.get(),
        plan.output.canvas.h.get()
    );
    // The stream's reel-tools cuts (A33) plan through the model planner.
    for rc in p["fixture"]["cuts"].as_array().unwrap() {
        let cut = rc["cut"].as_str().unwrap();
        let pl = c
            .planned(&pid, cut)
            .unwrap_or_else(|e| panic!("{}: {e}", rc["hook_id"]));
        assert!(gmclip_plan::check_plan(&pl.plan).is_empty());
        let v = c.cut_view(&pid, cut).unwrap();
        eprintln!(
            "{}: {} parts, {} frames, {} caption events",
            rc["hook_id"],
            v["parts"].as_array().unwrap().len(),
            v["frames"],
            v["captions"].as_array().unwrap().len()
        );
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
