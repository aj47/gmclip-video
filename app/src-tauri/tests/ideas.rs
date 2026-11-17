//! Ideas 2026-10-16 (library and layout, lane K4) through the app core: draw your layout
//! once (saved on the video, the same-size videos and as a preset), "Fix everywhere"
//! with the one term list, library search by topic landing in the long-form (or the
//! stream edit), long-forms compared with strips, B-roll suggestions from other
//! recordings, and reaction moments from the sound and the words.
//!
//! Synthetic projects (words and level frames written as analysis blobs); the steps that
//! decode a real frame use the lab media folder (`GMCLIP_LAB_MEDIA`, default
//! `~/Development/gmclip-lab-media`) and skip without it.

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::{Moment, MomentOrigin, RegionRole, Source, StreamLayout};
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;

fn lab(rel: &str) -> PathBuf {
    let d = std::env::var_os("GMCLIP_LAB_MEDIA").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join("Development/gmclip-lab-media")
        },
        PathBuf::from,
    );
    d.join(rel)
}

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-ideas-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

/// Words at 0.5 s each from `t0` seconds.
fn words_body(rev: &str, text: &str, t0: f64) -> Vec<u8> {
    let ws: Vec<Value> = text
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let s = ((t0 + i as f64 * 0.5) * SEC as f64) as i64;
            json!({"text": w, "start": s, "end": s + SEC * 4 / 10})
        })
        .collect();
    serde_json::to_vec(&json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": ws}))
        .unwrap()
}

/// A source of `secs` seconds with a `w`×`h` 30 fps picture.
fn source(c: &AppCore, pid: &str, name: &str, secs: i64, w: u32, h: u32) -> String {
    c.call(
        pid,
        "source_add",
        json!({"path": format!("/nowhere/{name}.mp4"), "fingerprint": format!("sha256:{name}"),
               "probe": {"duration": secs * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264",
                          "width": w, "height": h, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                         "audio_tracks": []}}),
    )
    .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn project(c: &Arc<AppCore>, name: &str) -> String {
    c.create_project(name).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn words(c: &AppCore, pid: &str, owner: &str, text: &str, t0: f64) {
    c.project(pid)
        .unwrap()
        .handle
        .put_analysis(owner, "words", "r1", words_body("r1", text, t0))
        .unwrap();
}

fn rects() -> Value {
    json!([{"role": "screen", "rect": {"x": 0.0, "y": 0.0, "w": 0.75, "h": 1.0}},
           {"role": "cam", "rect": {"x": 0.76, "y": 0.68, "w": 0.24, "h": 0.32}},
           {"role": "chat", "rect": {"x": 0.76, "y": 0.0, "w": 0.24, "h": 0.6}},
           {"role": "private", "rect": {"x": 0.0, "y": 0.92, "w": 0.3, "h": 0.08}}])
}

#[test]
fn a_drawn_layout_is_the_videos_its_siblings_and_a_preset() {
    let (c, root) = core("draw");
    let pid = project(&c, "Streams");
    let a = source(&c, &pid, "monday", 600, 1920, 1080);
    let b = source(&c, &pid, "tuesday", 600, 1920, 1080);
    let small = source(&c, &pid, "phone", 60, 1280, 720);
    let v = gmclip_app::drawlayout::view(&c, &pid, &a).unwrap();
    assert_eq!(v["canvas"], json!({"w": 1920, "h": 1080}));
    assert_eq!(v["has_layout"], false);
    assert_eq!(v["siblings"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["siblings"][0]["source"], b.as_str());
    let roles: Vec<&str> = v["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["Screen", "Camera", "Chat", "Private", "Alerts"]);

    // saved on Monday and Tuesday, and as a preset
    let r = gmclip_app::drawlayout::save(
        &c,
        &pid,
        &json!({"source": a, "name": "Desk scene", "regions": rects(), "also": [b], "preset": true}),
    )
    .unwrap();
    assert_eq!(r["videos"], 2);
    assert_eq!(r["private"], 2, "chat and private are private-class");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    for s in [&a, &b] {
        let src: &Source = doc
            .iter::<Source>()
            .find(|x| x.id.0.to_string() == *s)
            .unwrap();
        let l: &StreamLayout = doc.get(src.stream_layout.unwrap()).unwrap();
        assert_eq!(l.name, "Desk scene");
        assert_eq!(l.regions.len(), 4);
        assert_eq!(
            l.regions
                .iter()
                .filter(|r| r.role.is_private_class())
                .count(),
            2
        );
    }
    // the project map says which videos have a layout
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    let laid: Vec<Value> = m["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["layout"].clone())
        .collect();
    assert_eq!(
        laid,
        [json!("Desk scene"), json!("Desk scene"), Value::Null]
    );
    // a picture of another size cannot take it
    assert!(
        gmclip_app::drawlayout::save(
            &c,
            &pid,
            &json!({"source": a, "name": "x", "regions": rects(), "also": [small]}),
        )
        .is_err()
    );
    // undo takes Tuesday's away, Monday keeps it
    c.call(&pid, "undo", json!({})).unwrap();
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let has = |s: &str| {
        doc.iter::<Source>()
            .find(|x| x.id.0.to_string() == s)
            .unwrap()
            .stream_layout
            .is_some()
    };
    assert!(has(&a) && !has(&b));

    // another project: the preset is offered for a picture of its size, not another's
    let other = project(&c, "Elsewhere");
    let x = source(&c, &other, "wednesday", 600, 1920, 1080);
    let y = source(&c, &other, "vertical", 600, 1080, 1920);
    let v = gmclip_app::drawlayout::view(&c, &other, &x).unwrap();
    assert_eq!(v["presets"][0]["name"], "Desk scene");
    assert_eq!(v["presets"][0]["regions"].as_array().unwrap().len(), 4);
    let v = gmclip_app::drawlayout::view(&c, &other, &y).unwrap();
    assert!(v["presets"].as_array().unwrap().is_empty());
    // saving the same name again replaces the preset (one per name and size)
    gmclip_app::drawlayout::save(
        &c,
        &other,
        &json!({"source": x, "name": "Desk scene", "regions": [rects()[1]], "preset": true}),
    )
    .unwrap();
    let ps = gmclip_app::drawlayout::saved_presets(&c);
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].layout.regions.len(), 1);
    assert_eq!(ps[0].layout.regions[0].role, RegionRole::Cam);

    // a frame of a real video to draw on (lab media)
    let video = lab("renders/own-vm-1080.mp4");
    if video.is_file() {
        let o = gmclip_app::video::open_video(&c, &video).unwrap();
        let op = o["project"].as_str().unwrap().to_owned();
        c.analysis.cancel_project(&op);
        let doc = c.project(&op).unwrap().handle.snapshot().doc.clone();
        let s = doc.iter::<Source>().next().unwrap().id.0.to_string();
        let f = gmclip_app::drawlayout::frame(&c, &op, &s, None).unwrap();
        let (w, h) = (f["w"].as_u64().unwrap(), f["h"].as_u64().unwrap());
        assert!(
            w > 0 && w <= u64::from(gmclip_app::drawlayout::FRAME_W),
            "{w}"
        );
        assert_eq!(
            f["rgb"].as_str().unwrap().len() as u64,
            (w * h * 3).div_ceil(3) * 4
        );
        let g = gmclip_app::drawlayout::frame(&c, &op, &s, Some(10.0)).unwrap();
        assert_eq!(g["at_s"], 10.0);
        assert_ne!(f["rgb"], g["rgb"], "a different frame");
    } else {
        eprintln!("frame step skipped: no lab media");
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fix_everywhere_feeds_one_term_list() {
    let (c, root) = core("terms");
    let pid = project(&c, "Deploys");
    let s = source(&c, &pid, "deploy", 600, 1920, 1080);
    words(
        &c,
        &pid,
        &s,
        "So I put it on XD.dev, then I opened XD.dev again.",
        10.0,
    );
    let cut = c
        .call(&pid, "cut_create", json!({"title": "short"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": {"source": s}, "src_in": 10 * SEC, "src_out": 16 * SEC}),
    )
    .unwrap();
    // word 5 is "XD.dev," (its comma is not part of the name)
    let r =
        gmclip_app::termlist::fix_everywhere(&c, &pid, &cut, 5, 5, "exe.dev", true, None).unwrap();
    assert_eq!(r["heard"], "XD.dev");
    assert_eq!(r["found"], 2);
    assert_eq!(r["profile"], true);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let list = gmclip_app::termlist::term_list(&c, &doc);
    assert_eq!(
        list.len(),
        1,
        "the project's and the profile's are one term"
    );
    assert_eq!(list[0].written, "exe.dev");
    assert_eq!(list[0].heard_as, ["XD.dev"]);
    // another project: the profile's term is on its list (the gate and the correction
    // pass read it), and corrects a new transcript there
    let other = project(&c, "Later");
    let doc = c.project(&other).unwrap().handle.snapshot().doc.clone();
    let list = gmclip_app::termlist::term_list(&c, &doc);
    assert_eq!(list.len(), 1);
    let mut t = gmclip_core::analysis::Transcript {
        revision: "r".into(),
        words: vec![gmclip_core::analysis::Word {
            text: "xd.dev".into(),
            start: 0,
            end: 1,
            confidence: None,
        }],
        lines: Vec::new(),
    };
    assert_eq!(gmclip_core::terms::correct_transcript(&mut t, &list), 1);
    assert_eq!(t.words[0].text, "exe.dev");
    // more than four words is not a name
    assert!(gmclip_app::termlist::fix_everywhere(&c, &pid, &cut, 0, 4, "x", false, None).is_err());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

fn word(n: i64, t: i64) -> Value {
    json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": t, "t_src_end": t + SEC / 4, "normalised_text": "w"})
}

#[test]
fn a_topic_is_a_search_result_that_opens_where_it_is_used() {
    let (c, root) = core("topics");
    let pid = project(&c, "Stream one");
    let s = source(&c, &pid, "stream-one", 600, 1920, 1080);
    let stream = c.call(&pid, "stream_edit_ensure", json!({})).unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let topic = |a: i64, b: i64, title: &str| {
        c.call(
            &pid,
            "topic_set",
            json!({"start": word(a, a * SEC), "end": word(b, b * SEC), "title": title}),
        )
        .unwrap()["output"]["topic"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let gpu = topic(30, 90, "GPU prices");
    let setup = topic(120, 200, "Setting up the VM");
    let late = topic(300, 360, "GPU benchmarks");
    let lf = c
        .call(
            &pid,
            "longform_from_topics",
            json!({"topics": [setup, gpu], "order": "given", "title": "VMs and GPUs"}),
        )
        .unwrap()["output"]
        .clone();
    let long = lf["cut"].as_str().unwrap().to_owned();

    let r = gmclip_app::libsearch::search(&c, "gpu", gmclip_app::libsearch::Mode::Words, 20, None)
        .unwrap();
    let ts = r["topics"].as_array().unwrap();
    let titles: Vec<&str> = ts.iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["GPU prices", "GPU benchmarks"], "{r}");
    assert_eq!(ts[0]["long"]["cut"], long.as_str());
    assert_eq!(ts[0]["long"]["title"], "VMs and GPUs");
    assert!(
        ts[0]["long"]["at_s"].as_f64().unwrap() > 0.0,
        "the second chapter"
    );
    assert_eq!(ts[1]["long"], Value::Null);
    assert_eq!(ts[0]["media"]["source"], s.as_str());

    // a topic in a long-form opens there, at its chapter; one in none, in the stream edit
    let o = gmclip_app::libsearch::open_topic(&c, &pid, None, &gpu).unwrap();
    assert_eq!(o["in"], "long");
    assert_eq!(o["cut"], long.as_str());
    assert!(o["frame"].as_u64().unwrap() > 0);
    let o = gmclip_app::libsearch::open_topic(&c, &pid, None, &late).unwrap();
    assert_eq!(o["in"], "stream");
    assert_eq!(o["cut"], stream.as_str());
    assert_eq!(o["frame"], 300 * 30);

    // the long-form's sibling in another order, compared with strips
    let fork = c
        .call(
            &pid,
            "longform_fork",
            json!({"cut": long, "topics": [gpu, setup]}),
        )
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let v = gmclip_app::longcompare::view(&c, &pid, &long, &fork).unwrap();
    assert_eq!(v["same_order"], false);
    assert_eq!(v["a"]["chapters"][0]["title"], "Setting up the VM");
    assert_eq!(v["b"]["chapters"][0]["title"], "GPU prices");
    // (no file behind these sources: the strip frames are empty, not an error)
    assert_eq!(v["a"]["chapters"][0]["frame"], Value::Null);

    // a closed project is searched too, and opening its topic reopens it
    let home = c.project(&pid).unwrap().home.clone();
    c.close_project(&pid).unwrap();
    let r = gmclip_app::libsearch::search(
        &c,
        "setting up",
        gmclip_app::libsearch::Mode::Words,
        20,
        None,
    )
    .unwrap();
    assert_eq!(r["topics"][0]["title"], "Setting up the VM");
    assert_eq!(r["topics"][0]["open"], false);
    let o =
        gmclip_app::libsearch::open_topic(&c, &pid, Some(home.to_str().unwrap()), &setup).unwrap();
    assert_eq!(o["in"], "long");
    assert!(c.is_open(&pid));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// `secs` of 40 ms level frames: speech-like syllables around −30 dB with `bursts`.
fn level_blob(secs: usize, bursts: &[(f64, f64, f32)]) -> Vec<u8> {
    use gmclip_analysis::energy::{Energy, Hop};
    let mut seed: u64 = 7;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) as f32 / (1u64 << 31) as f32
    };
    let syll: Vec<f32> = (0..secs * 4).map(|_| next() * 12.0 - 8.0).collect();
    let db: Vec<f32> = (0..secs * 25)
        .map(|i| {
            let t = i as f64 / 25.0;
            let pause = if (t % 3.0) > 2.6 { -18.0 } else { 0.0 };
            let burst: f32 = bursts
                .iter()
                .filter(|(a, l, _)| t >= *a && t < a + l)
                .map(|b| b.2)
                .sum();
            -30.0 + syll[(t * 4.0) as usize] + pause + burst
        })
        .collect();
    Energy {
        rate: 16_000,
        window: 640,
        hop: 640,
        db,
    }
    .to_blob(Hop::Ms40)
    .encode()
    .unwrap()
}

#[test]
fn reactions_become_moments_found_by_sound_and_transcript() {
    let (c, root) = core("reactions");
    let pid = project(&c, "Funny stream");
    let s = source(&c, &pid, "funny", 300, 1920, 1080);
    // talk throughout (a word every 0.5 s from 0 s); "haha hahaha." at 60.5 s with a
    // burst there; a moderate burst at 200 s with no reaction words
    let text: Vec<&str> = (0..600)
        .map(|i| match i {
            121 => "haha",
            122 => "hahaha.",
            _ if i % 12 == 11 => "okay.",
            _ => "build",
        })
        .collect();
    words(&c, &pid, &s, &text.join(" "), 0.0);
    c.project(&pid)
        .unwrap()
        .handle
        .put_analysis(
            &s,
            "energy_40ms",
            "e1",
            level_blob(300, &[(60.0, 1.5, 16.0), (200.0, 1.0, 6.0)]),
        )
        .unwrap();
    let r = gmclip_app::reactions::find_moments(&c, &pid).unwrap();
    assert_eq!(r["precision"], "not measured");
    assert_eq!(r["added"], 1, "{r}");
    let cand = &r["candidates"][0];
    assert_eq!(cand["kind"], "laugh");
    assert!(
        (cand["peak_s"].as_f64().unwrap() - 60.75).abs() < 1.0,
        "{cand}"
    );
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let m: Vec<&Moment> = doc.iter::<Moment>().collect();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].origin, MomentOrigin::Local);
    assert_eq!(m[0].r#type, "reaction");
    assert!(m[0].why.contains("laughed"), "{}", m[0].why);
    // the moments panel labels it
    let list = c.call(&pid, "moments_list", json!({})).unwrap();
    assert!(list["output"]["moments"][0]["origin"] == "local", "{list}");
    // again: nothing new
    let again = gmclip_app::reactions::find_moments(&c, &pid).unwrap();
    assert_eq!(again["added"], 0);
    assert_eq!(again["candidates"][0]["already"], true);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn b_roll_comes_from_other_recordings_and_goes_in_as_a_part() {
    if !gmclip_app::libmeaning::model_present() {
        eprintln!("skipped: no embedding model");
        return;
    }
    let (c, root) = core("broll");
    let pid = project(&c, "Build log");
    let a = source(&c, &pid, "today", 600, 1920, 1080);
    let b = source(&c, &pid, "last-week", 600, 1920, 1080);
    words(
        &c,
        &pid,
        &a,
        "Welcome back everyone. Graphics cards cost way too much right now and every new card is more expensive. \
         Anyway the cat is asleep on the desk.",
        10.0,
    );
    words(
        &c,
        &pid,
        &b,
        "I baked bread this morning with a crunchy crust. \
         GPU prices went up again and Nvidia keeps charging more for each generation. \
         Then it rained all afternoon.",
        10.0,
    );
    let other = project(&c, "Other stream");
    let o = source(&c, &other, "elsewhere", 600, 1920, 1080);
    words(
        &c,
        &other,
        &o,
        "My new graphics card was so expensive I almost returned it. Video cards are overpriced.",
        10.0,
    );
    let cut = c
        .call(&pid, "cut_create", json!({"title": "rant"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "part_add",
        json!({"cut": cut, "media": {"source": a}, "src_in": 11 * SEC, "src_out": 20 * SEC}),
    )
    .unwrap();
    // index and embed
    gmclip_app::libsearch::search(&c, "x", gmclip_app::libsearch::Mode::Meaning, 5, None).unwrap();
    gmclip_app::libmeaning::wait_idle(&c, std::time::Duration::from_secs(60));
    // the selection: "Graphics cards … more expensive." (words 3–17)
    let s = gmclip_app::broll::suggest(&c, &pid, &cut, 3, 17).unwrap();
    let rows = s["suggestions"].as_array().unwrap();
    assert!(!rows.is_empty(), "{s}");
    assert!(
        rows.iter()
            .all(|r| !(r["project"] == pid.as_str() && r["media"]["source"] == a.as_str())),
        "never the recording the words are from: {s}"
    );
    let same = rows
        .iter()
        .find(|r| r["media"]["source"] == b.as_str())
        .unwrap_or_else(|| panic!("last week's GPU talk: {s}"));
    assert!(
        same["text"].as_str().unwrap().contains("GPU prices"),
        "{same}"
    );
    assert_eq!(same["add_as"], "part");
    let far = rows
        .iter()
        .find(|r| r["project"] == other.as_str())
        .unwrap_or_else(|| panic!("the other project's talk: {s}"));
    assert_eq!(far["add_as"], "image");
    // one click: a part after the selection's part, on the cut's frame grid
    // as a clip instead (decision 0028): that moment's own video, silent, over the words
    // (an overlay is placed on a framing: give the synthetic cut one)
    let f = c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "layer_add",
        json!({"owner": {"framing": f}, "role": {"region": "screen"},
        "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}),
    )
    .unwrap();
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let r = gmclip_app::broll::add(&c, &pid, &cut, 3, 17, same, true).unwrap();
    assert_eq!(r["added"], "clip", "{r}");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let ov = doc
        .iter::<gmclip_model::entities::Overlay>()
        .find(|o| o.kind == gmclip_model::entities::OverlayKind::Video)
        .expect("a video overlay");
    let v = serde_json::to_value(ov.video.as_ref().unwrap()).unwrap();
    assert_eq!(v["media"]["span"]["media"]["source"], b.as_str(), "{v}");
    assert!(r["seconds"].as_f64().unwrap() >= 1.0);
    let r = gmclip_app::broll::add(&c, &pid, &cut, 3, 17, same, false).unwrap();
    assert_eq!(r["added"], "part");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let cid: gmclip_model::Id<gmclip_model::entities::Cut> =
        gmclip_model::Id::new(gmclip_model::Ulid::parse(&cut).unwrap());
    let parts: Vec<&gmclip_model::entities::Part> = gmclip_model::clock::cut_layout(&doc, cid)
        .iter()
        .filter_map(|sp| doc.get(sp.part))
        .collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(
        serde_json::to_value(parts[1].media).unwrap()["source"],
        b.as_str()
    );
    assert_eq!(parts[1].src_in.0 % (SEC / 30), 0);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn b_roll_from_another_project_goes_in_as_its_frame_over_the_selection() {
    let video = lab("renders/own-vm-1080.mp4");
    if !video.is_file() {
        eprintln!("skipped: no lab media");
        return;
    }
    let (c, root) = core("broll-image");
    let o = gmclip_app::video::open_video(&c, &video).unwrap();
    let pid = o["project"].as_str().unwrap().to_owned();
    let cut = o["cut"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let src = doc.iter::<Source>().next().unwrap().clone();
    let sid = src.id.0.to_string();
    words(
        &c,
        &pid,
        &sid,
        "Every project gets its own virtual machine so nothing leaks.",
        2.0,
    );
    // another project whose recording is the same file (its picture is real)
    let other = project(&c, "Elsewhere");
    let osrc = c
        .call(
            &other,
            "source_add",
            json!({"path": src.path, "fingerprint": src.fingerprint, "probe": src.probe}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let hit = json!({"project": other, "media": {"source": osrc}, "start_s": 8.0, "end_s": 12.0});
    let r = gmclip_app::broll::add(&c, &pid, &cut, 1, 5, &hit, false).unwrap();
    assert_eq!(r["added"], "image", "{r}");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let ov: Vec<&gmclip_model::entities::Overlay> =
        doc.iter::<gmclip_model::entities::Overlay>().collect();
    assert_eq!(ov.len(), 1);
    assert!(r["seconds"].as_f64().unwrap() >= 1.0);
    assert!(
        c.planned(&pid, &cut).is_ok(),
        "the cut plans with the picture on it"
    );
    // as a clip (decision 0028): a short excerpt of the other recording, stored with the
    // cut as a video asset (rendered by the exporter), picture-in-picture over the words
    let r = gmclip_app::broll::add(&c, &pid, &cut, 1, 5, &hit, true).unwrap();
    assert_eq!(r["added"], "clip", "{r}");
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let asset = doc
        .iter::<gmclip_model::entities::Asset>()
        .find(|a| a.media_type == "video/mp4")
        .expect("the excerpt is stored");
    let probe = asset.probe.unwrap();
    assert!(probe.w <= 960 && probe.frames.unwrap() >= 100, "{probe:?}");
    let pl = c.planned(&pid, &cut).unwrap();
    let ev = &pl.plan.clip_events.as_ref().unwrap()[0];
    assert_eq!(ev.picture.r#loop, Some(true));
    let src = pl
        .plan
        .sources
        .iter()
        .find(|s| s.id == ev.picture.source)
        .unwrap();
    assert!(std::path::Path::new(&src.path).is_file(), "{}", src.path);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
