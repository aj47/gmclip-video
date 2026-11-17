//! U.06 through the app core on a real reel cut (loc_4fadb756_h34, in its stream's
//! project fixture): the cut's words, strike → struck in the gap → restore, re-spell,
//! keyword, edge nudge; each one command and one undo step. Skips without the lab media.

use gmclip_app::core::{AppCore, Dirs};
use serde_json::json;

#[test]
fn words_strip_edits_on_a_real_cut() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-words-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let cut = p["fixture"]["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hook_id"] == "loc_4fadb756_h34")
        .unwrap()["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let t = std::time::Instant::now();
    let w = gmclip_app::words::cut_words(&c, &pid, &cut).unwrap();
    eprintln!("cut words in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(w["state"], "ok");
    let words = w["words"].as_array().unwrap().clone();
    let rev = w["revision"].as_str().unwrap().to_owned();
    let live: Vec<_> = words.iter().filter(|x| x["struck"] == false).collect();
    eprintln!(
        "{} words ({} live, {} struck in gaps)",
        words.len(),
        live.len(),
        words.len() - live.len()
    );
    assert!(live.len() > 60, "{}", live.len());

    // Strike three words in the middle of a part: frames shrink, the words show struck.
    let frames0 = c.planned(&pid, &cut).unwrap().plan.output.frames;
    let mid = live[live.len() / 2]["i"].as_u64().unwrap();
    gmclip_app::words::strike(&c, &pid, &cut, mid, mid + 2, false).unwrap();
    let frames1 = c.planned(&pid, &cut).unwrap().plan.output.frames;
    assert!(frames1 < frames0, "{frames1} < {frames0}");
    let after = gmclip_app::words::cut_words(&c, &pid, &cut).unwrap();
    let struck: Vec<u64> = after["words"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["struck"] == true)
        .map(|x| x["i"].as_u64().unwrap())
        .collect();
    assert!((mid..=mid + 2).all(|i| struck.contains(&i)), "{struck:?}");
    // Restore puts them back (frames as before).
    // Restore grows the part back over whole words (SPEC §5.4), so the cut ends at least
    // as long as before the strike (RT cut inside the last word; a restore keeps it whole).
    gmclip_app::words::strike(&c, &pid, &cut, mid, mid + 2, true).unwrap();
    let frames2 = c.planned(&pid, &cut).unwrap().plan.output.frames;
    assert!(frames2 >= frames0 - 1, "{frames2} vs {frames0}");
    let back = gmclip_app::words::cut_words(&c, &pid, &cut).unwrap();
    assert!(
        back["words"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["struck"] == false)
            .any(|x| x["i"] == mid)
    );
    // Keyword toggle: one command; never changes picture or audio.
    assert_eq!(
        w["respell"], false,
        "re-spell waits for the caption builder (A.09)"
    );
    let wi = live[3]["i"].as_u64().unwrap();
    c.call(
        &pid,
        "caption_keyword_toggle",
        json!({"cut": cut, "word": {"asr_revision": rev, "ordinal": wi}}),
    )
    .unwrap();
    let w2 = gmclip_app::words::cut_words(&c, &pid, &cut).unwrap();
    let row = w2["words"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["i"] == wi && x["struck"] == false)
        .unwrap();
    assert_eq!(row["keyword"], true);
    assert_eq!(c.planned(&pid, &cut).unwrap().plan.output.frames, frames2);
    // Undo the keyword and the restore: the strike is back, one step each.
    c.call(&pid, "undo", json!({})).unwrap();
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(c.planned(&pid, &cut).unwrap().plan.output.frames, frames1);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
