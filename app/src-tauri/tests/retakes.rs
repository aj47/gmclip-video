//! Decision 0040 in the window: the Tighten tab's Retakes sweep on a cut whose speaker
//! restarts a sentence — previewed with the earlier attempt's place on the timeline,
//! applied as one transaction, the kept attempt still played.

use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;

fn core(tag: &str) -> (Arc<AppCore>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-retakes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

#[test]
fn the_tighten_tab_sweeps_retakes() {
    let (c, root) = core("tab");
    let p = c.create_project("Retakes").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let src = c
        .call(
            &pid,
            "source_add",
            json!({"path": "/nowhere/take.mp4", "fingerprint": "0000000000000000000000000000000000000000000000000000000000000000",
                   "probe": {"duration": 60 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264",
                              "width": 1920, "height": 1080, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                             "audio_tracks": []}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let said =
        "Alright, here is my first time Alright, this is the first screen after you install BB.";
    let mut t = SEC;
    let words: Vec<Value> = said
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let a = t;
            t += SEC * 2 / 5;
            if i == 5 {
                t += SEC; // the pause before the restart
            }
            json!({"text": w, "start": a, "end": a + SEC / 3})
        })
        .collect();
    let h = c.project(&pid).unwrap();
    h.handle
        .put_analysis(
            &src,
            "words",
            "r1",
            serde_json::to_vec(
                &json!({"schema": "gmclip.analysis.words/0", "revision": "r1", "words": words}),
            )
            .unwrap(),
        )
        .unwrap();
    // the caption styles a cut with captions plans with
    gmclip_app::short::ensure_presets(&c, &pid).unwrap();
    let cut = c
        .call(&pid, "cut_create", json!({"title": "main"}))
        .map(|r| {
            let o = &r["output"];
            o["id"].as_str().or(o["cut"].as_str()).unwrap().to_owned()
        })
        .unwrap();
    c.call(
        &pid,
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    )
    .unwrap();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": {"source": src}, "src_in": 0, "src_out": 12 * SEC}),
    )
    .unwrap();

    let pv = gmclip_app::tighten::sweep_preview(&c, &pid, &cut, "retakes", 0.4, 0.15).unwrap();
    assert_eq!(pv["kind"], "retakes");
    let cuts = pv["cuts"].as_array().unwrap();
    assert_eq!(cuts.len(), 1, "{pv}");
    assert!(cuts[0]["out"].is_array(), "its place on the timeline: {pv}");
    assert!(
        cuts[0]["reason"].as_str().unwrap().starts_with("restarted"),
        "{pv}"
    );
    gmclip_app::tighten::sweep_apply(&c, &pid, &cut, pv["preview_id"].as_str().unwrap(), &[0])
        .unwrap();
    // the earlier attempt is gone, the restart plays from its first word
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let cid: gmclip_model::Id<gmclip_model::entities::Cut> =
        gmclip_model::Id::new(gmclip_model::Ulid::parse(&cut).unwrap());
    let parts: Vec<(i64, i64)> = doc
        .children::<gmclip_model::entities::Part>(cid.entity())
        .filter_map(|x| doc.get(x))
        .map(|p| (p.src_in.0, p.src_out.0))
        .collect();
    let restart = words[6]["start"].as_i64().unwrap();
    assert!(
        parts.iter().any(|&(a, b)| a <= restart && b > restart),
        "the kept attempt plays: {parts:?}"
    );
    let first = words[0]["start"].as_i64().unwrap();
    assert!(
        !parts
            .iter()
            .any(|&(a, b)| a <= first + SEC / 10 && b > first + SEC / 10),
        "the earlier attempt does not: {parts:?}"
    );
    // nothing left to sweep
    let again = gmclip_app::tighten::sweep_preview(&c, &pid, &cut, "retakes", 0.4, 0.15).unwrap();
    assert_eq!(again["cuts"].as_array().unwrap().len(), 0, "{again}");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
