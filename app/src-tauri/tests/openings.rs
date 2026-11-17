//! Decision 0024 in the app on an opened video: a version (the cut with an alternate
//! opening) plans like the cut, with the shared rest unchanged; an export records which
//! version it is, so retention imported onto it shows on that opening's column only.

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
use serde_json::json;

const SEC: i64 = 705_600_000;

#[test]
fn versions_plan_alike_and_carry_their_own_retention() {
    let video = gmclip_model::lab::media("renders/own-vm-1080.mp4");
    if !video.is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-openings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, &video).unwrap();
    let (pid, stream) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    // a short of the video: a 5 s opening (the hook), then 25 s more
    let media = {
        let p = c.project(&pid).unwrap();
        let doc = p.handle.snapshot().doc.clone();
        let sid: gmclip_model::Id<gmclip_model::entities::Cut> =
            gmclip_app::planner::ulid_id(&stream).unwrap();
        let part = doc
            .children::<gmclip_model::entities::Part>(sid.entity())
            .find_map(|x| doc.get(x))
            .unwrap();
        serde_json::to_value(part.media).unwrap()
    };
    let cut = c
        .call(&pid, "cut_create", json!({"title": "short"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    )
    .unwrap();
    let hook = c
        .call(
            &pid,
            "part_add",
            json!({"cut": cut, "media": media, "src_in": 0, "src_out": 5 * SEC, "role": "hook"}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "after": hook, "media": media, "src_in": 5 * SEC, "src_out": 30 * SEC}),
    )
    .unwrap();
    gmclip_app::short::captions_set(&c, &pid, &cut, false).unwrap();
    let main = c.planned(&pid, &cut).unwrap();
    let fps = main.plan.output.fps.as_f64();
    let alt = c
        .call(
            &pid,
            "opening_add",
            json!({"cut": cut, "label": "the later take", "from": {"span": {
                "start": {"raw": {"time": {"clock": "src", "t": 20 * SEC}, "reason": "picked"}},
                "end": {"raw": {"time": {"clock": "src", "t": 23 * SEC}, "reason": "picked"}}}}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ver = gmclip_app::openings::planned_version(&c, &pid, &cut, &alt).unwrap();
    // 3 s instead of 5 s at the front; the rest is the same frames
    let shorter = (2.0 * fps).round() as u64;
    assert_eq!(ver.plan.output.frames + shorter, main.plan.output.frames);
    assert_eq!(ver.plan.parts.len(), main.plan.parts.len());
    assert_eq!(
        ver.plan.parts[0].picture.first_frame,
        (20.0 * fps).round() as u64,
        "the version opens on the later take"
    );
    assert_eq!(
        ver.plan.parts.last().unwrap().picture.first_frame,
        main.plan.parts.last().unwrap().picture.first_frame
    );
    // the cut itself is untouched
    assert_eq!(
        c.planned(&pid, &cut).unwrap().plan.output.frames,
        main.plan.output.frames
    );

    // exports record their version; retention lands on that version only
    let (sig_alt, label) = gmclip_app::openings::version_of(&c, &pid, &cut, Some(&alt)).unwrap();
    assert_eq!(label, "the later take");
    let file = root.join("exports/fake.mp4");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"not a video").unwrap();
    c.add_export(
        &pid,
        ExportEntry {
            id: "exp-alt".into(),
            fps: 30,
            fps_den: 1,
            cut: cut.clone(),
            title: "alt".into(),
            file: file.clone(),
            frames: ver.plan.output.frames,
            seconds: 1.0,
            render_sig: None,
            at_ms: 1,
            notes: vec![],
            verdict: None,
            verify: None,
            plan_sig: None,
            opening_sig: Some(sig_alt.clone()),
            opening_label: Some(label),
            framing: None,
        },
    )
    .unwrap();
    let csv = "Video position (%),Absolute audience retention (%)\n0,100\n5,80\n10,70\n100,40\n";
    gmclip_app::retention::import(&c, &pid, "exp-alt", "All.csv", csv).unwrap();
    let view = gmclip_app::openings::view(&c, &pid, &cut).unwrap();
    let vs = view["versions"].as_array().unwrap();
    assert_eq!(vs.len(), 2, "{view}");
    assert!(
        vs[0]["retention"].is_null(),
        "the main version has no export yet"
    );
    assert_eq!(vs[0]["exports"], 0);
    assert_eq!(vs[1]["exports"], 1);
    assert_eq!(vs[1]["sig"], sig_alt.as_str());
    let r = &vs[1]["retention"];
    assert_eq!(r["export"], "exp-alt");
    let at3 = r["at_3s"].as_f64().unwrap();
    let end = r["at_opening_end"].as_f64().unwrap();
    assert!(at3 <= 100.0 && at3 >= end && end >= 40.0, "{r}");
    let _ = std::fs::remove_dir_all(&root);
}
