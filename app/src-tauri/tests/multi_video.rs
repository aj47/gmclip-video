//! Several videos in one project (AJ 2026-10-15: "i should be able to load multiple video
//! files into one project"), on two finished reels from the lab media (read only):
//! h79 v4 (60.2 s) opened, h79 v3 (22.5 s) added — two sources, both on the main cut
//! (decision 0036), transcribed one at a time; the same file again is skipped; a short mixing both renders
//! the frames of each video's own short back to back (SHA-256 of every output frame);
//! removing a video a cut plays is refused, naming the cut; ＋ add as part from the other
//! video's transcript; a copy of v4 added as "the same recording" is one video (a sync
//! group, heard once). Skips without the reels.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::video::AddFile;
use gmclip_plan::Plan;
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn v4() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
/// `h79/h79-v3.mp4` in the lab media folder.
fn v3() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v3.mp4"));
    P.as_str()
}
const SEC: i64 = 705_600_000;

fn app(root: &Path) -> Arc<AppCore> {
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

/// SHA-256 of every output frame.
fn frame_hashes(plan: &Plan) -> Vec<String> {
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let n = plan.output.frames;
    let mut out = Vec::new();
    let mut k = 0;
    while k < n {
        let at: Vec<u64> = (k..(k + 30).min(n)).collect();
        for (_, im) in gmclip_render::export::render_frames(plan, &mut inputs, &at).unwrap() {
            out.push(gmclip_ml::sha256::hex(&im.px));
        }
        k += 30;
    }
    out
}

fn out(v: Value) -> Value {
    v["output"].clone()
}

/// A 9:16 short of `parts` (`(source id, from s, to s)`), captions off, one full-frame
/// screen layer.
fn short(c: &AppCore, pid: &str, title: &str, parts: &[(&str, i64, i64)]) -> String {
    let cut = out(c.call(pid, "cut_create", json!({"title": title})).unwrap())["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let f = out(c
        .call(
            pid,
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap())["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        pid,
        "layer_add",
        json!({"owner": {"framing": f}, "role": {"region": "screen"},
               "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}, "z": 0}),
    )
    .unwrap();
    let mut after: Option<String> = None;
    for (src, a, b) in parts {
        let mut args =
            json!({"cut": cut, "media": {"source": src}, "src_in": a * SEC, "src_out": b * SEC});
        if let Some(x) = &after {
            args["after"] = json!(x);
        }
        after = out(c.call(pid, "part_add", args).unwrap())["id"]
            .as_str()
            .map(str::to_owned);
    }
    c.set_active_framing(pid, &cut, &f).unwrap();
    cut
}

fn source_of(c: &AppCore, pid: &str, path: &str) -> String {
    let p = c.project(pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let want = Path::new(path).canonicalize().unwrap();
    doc.iter::<gmclip_model::entities::Source>()
        .find(|s| Path::new(&s.path) == want)
        .unwrap()
        .id
        .0
        .to_string()
}

/// Waits for every source's analysis; checks no two transcribe at once. `false` when the
/// speech model is not on this machine.
fn wait_heard(c: &AppCore, pid: &str) -> bool {
    let t0 = Instant::now();
    let mut heard = true;
    loop {
        let st = gmclip_app::analysis::status(c, pid);
        let rows = st["sources"].as_array().unwrap().clone();
        let busy = rows.iter().filter(|r| r["phase"] == "transcribing").count();
        assert!(busy <= 1, "one transcription at a time: {st}");
        if rows.iter().any(|r| r["asr_error"].is_string()) {
            heard = false;
        }
        if rows
            .iter()
            .all(|r| r["phase"] == "done" || r["phase"] == "failed")
            || t0.elapsed() > Duration::from_secs(400)
        {
            assert!(
                rows.iter().all(|r| r["phase"] == "done"),
                "analysis finished: {st}"
            );
            return heard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn several_videos_in_one_project() {
    if !Path::new(v4()).is_file() || !Path::new(v3()).is_file() {
        eprintln!("skipped: the h79 reels are not here");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root);
    let v = gmclip_app::video::open_video(&c, Path::new(v4())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();

    // add the second video (and the first again: skipped)
    let r = gmclip_app::video::add_many(
        &c,
        &pid,
        &[
            AddFile {
                path: v3().into(),
                voice: None,
            },
            AddFile {
                path: v4().into(),
                voice: None,
            },
        ],
        &[],
    )
    .unwrap();
    assert_eq!(r["added"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["skipped"][0]["why"], "already in this project");
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    let srcs = m["sources"].as_array().unwrap();
    assert_eq!(srcs.len(), 2);
    assert_eq!(srcs[1]["name"], "h79-v3");
    assert_eq!(srcs[1]["fps"]["num"], 30);
    assert_eq!(srcs[1]["offline"], false);
    assert_eq!(m["streams"].as_array().unwrap().len(), 1, "one main cut");
    assert_eq!(m["stream"]["videos"].as_array().unwrap().len(), 2);
    assert_eq!(
        srcs[1]["stream"], r["added"][0]["stream"],
        "the added video went on the main cut"
    );
    assert_eq!(srcs[1]["stream"], v["cut"]);
    let (a, b) = (source_of(&c, &pid, v4()), source_of(&c, &pid, v3()));
    let heard = wait_heard(&c, &pid);

    // a short mixing both: its frames are each video's own frames, back to back
    let mixed = short(&c, &pid, "Two reels", &[(&a, 2, 4), (&b, 5, 7)]);
    let only_a = short(&c, &pid, "v4 only", &[(&a, 2, 4)]);
    let only_b = short(&c, &pid, "v3 only", &[(&b, 5, 7)]);
    let plan = c.planned(&pid, &mixed).unwrap().plan;
    assert_eq!(plan.sources.len(), 2);
    let fm = frame_hashes(&plan);
    let fa = frame_hashes(&c.planned(&pid, &only_a).unwrap().plan);
    let fb = frame_hashes(&c.planned(&pid, &only_b).unwrap().plan);
    assert_eq!(fm.len(), 120);
    assert_eq!(fm[..60], fa[..], "the first video's frames");
    assert_eq!(fm[60..], fb[..], "then the second's");
    assert_ne!(fa[0], fb[0]);

    // removing a video a cut plays is refused, naming it
    let e = gmclip_app::video::remove_source(&c, &pid, &b).unwrap_err();
    assert!(e.message.contains("«Two reels»"), "{}", e.message);

    // the transcript pane of the mixed cut shows either video's words
    if heard {
        let tv = gmclip_app::transcript::transcript_view_of(&c, &pid, &mixed, Some(&b)).unwrap();
        assert_eq!(tv["owner"], b.as_str());
        assert_eq!(tv["videos"].as_array().unwrap().len(), 2);
        let played = tv["frame"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f.as_i64().unwrap() >= 60)
            .count();
        assert!(played > 0, "the second video's words play after frame 60");
        // ＋ add as part from the second video into the first video's short: words 10..15
        gmclip_app::words::add_as_part_of(&c, &pid, &only_a, 10, 15, None, Some(&b)).unwrap();
        let pl = c.planned(&pid, &only_a).unwrap().plan;
        assert_eq!(pl.parts.len(), 2);
        let pic = |i: usize| {
            let s = pl.parts[i].picture.source.as_str();
            pl.sources
                .iter()
                .find(|x| x.id.as_str() == s)
                .unwrap()
                .path
                .clone()
        };
        assert!(pic(1).ends_with("h79-v3.mp4"), "{}", pic(1));
        // a strike on the second video's words leaves the first's part alone
        gmclip_app::words::strike_of(&c, &pid, &only_a, 11, 12, false, Some(&b)).unwrap();
        let pl = c.planned(&pid, &only_a).unwrap().plan;
        assert_eq!(pl.parts.len(), 3);
        assert_eq!(pl.parts[0].out.len(), 60);
    }

    // the same recording: a copy of v4 added beside it is one video, heard once
    let copy: PathBuf = root.join("h79-v4 copy.mp4");
    std::fs::copy(v4(), &copy).unwrap();
    let other = gmclip_app::video::open_many(
        &c,
        &[
            AddFile {
                path: copy.clone(),
                voice: None,
            },
            AddFile {
                path: v3().into(),
                voice: None,
            },
            AddFile {
                path: v4().into(),
                voice: None,
            },
        ],
        &[(0, 2)],
        true,
        None,
    )
    .unwrap();
    let p2 = other["project"].as_str().unwrap().to_owned();
    assert_ne!(p2, pid, "one new project with the three files");
    let st = gmclip_app::analysis::status(&c, &p2);
    assert_eq!(
        st["sources"].as_array().unwrap().len(),
        2,
        "the pair is heard once: {st}"
    );
    let m2 = gmclip_app::streamedit::project_map_view(&c, &p2).unwrap();
    assert_eq!(m2["sources"].as_array().unwrap().len(), 3);
    assert_eq!(m2["streams"].as_array().unwrap().len(), 1);
    assert_eq!(
        m2["stream"]["videos"].as_array().unwrap().len(),
        2,
        "the pair and v3"
    );
    let grouped = m2["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["group"].is_string())
        .count();
    assert_eq!(grouped, 2);
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// `renders/own-vm-1080.mp4` in the lab media folder.
fn wide() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("renders/own-vm-1080.mp4"));
    P.as_str()
}

/// A short mixing a 16:9 video and a 9:16 one: each part's look is made for its own
/// video (one preset layout per picture shape), and a part added later from the other
/// video takes the preset the cut uses, for its own shape.
#[test]
fn a_mixed_short_looks_right_per_video() {
    if !Path::new(v4()).is_file() || !Path::new(wide()).is_file() {
        eprintln!("skipped: the test videos are not here");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-multi-look-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root);
    let v = gmclip_app::video::open_video(&c, Path::new(wide())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    gmclip_app::video::add_many(
        &c,
        &pid,
        &[AddFile {
            path: v4().into(),
            voice: None,
        }],
        &[],
    )
    .unwrap();
    c.analysis.cancel_project(&pid);
    let (wide, tall) = (source_of(&c, &pid, wide()), source_of(&c, &pid, v4()));
    let cut = short(&c, &pid, "Mixed", &[(&wide, 1, 3), (&tall, 4, 6)]);
    // the default stack was made for one shape; the preset gives each part its own
    let r = gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();
    assert_eq!(r["layouts"], 2, "{r}");
    let cv = c.cut_view(&pid, &cut).unwrap();
    let crop = |i: usize| {
        let a = cv["parts"][i]["layers"][0]["crop_n"]
            .as_array()
            .unwrap()
            .clone();
        [0, 1, 2, 3].map(|k| a[k].as_f64().unwrap())
    };
    // the screen panel is 1080×760: its crop has that pixel aspect on each video
    let aspect = |r: [f64; 4], (w, h): (f64, f64)| (r[2] * w) / (r[3] * h);
    assert!((aspect(crop(0), (1920.0, 1080.0)) - 1080.0 / 760.0).abs() < 0.01);
    assert!((aspect(crop(1), (1080.0, 1920.0)) - 1080.0 / 760.0).abs() < 0.01);
    assert!(c.planned(&pid, &cut).is_ok());
    // a part added later from the wide video takes the wide layout
    let p = out(c
        .call(
            &pid,
            "part_add",
            json!({"cut": cut, "media": {"source": wide}, "src_in": 8 * SEC, "src_out": 9 * SEC}),
        )
        .unwrap())["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gmclip_app::framing::fit_new_part(&c, &pid, &cut, &p).unwrap();
    let cv = c.cut_view(&pid, &cut).unwrap();
    let names: Vec<String> = cv["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_owned())
        .collect();
    let fr = gmclip_app::framing::framings(&c, &pid, &cut);
    let layout_of = |id: &str| fr["part_layouts"][id].as_str().unwrap_or("").to_owned();
    let mut ls: Vec<String> = names
        .iter()
        .filter(|x| **x != p)
        .map(|x| layout_of(x))
        .collect();
    ls.sort();
    assert_eq!(
        ls,
        ["Screen + facecam 9:16", "Screen + facecam 9:16 · h79-v4"]
    );
    assert_eq!(
        layout_of(&p),
        "Screen + facecam 9:16",
        "the wide video's layout"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// QA (milestones/m2/qa-e2e.md, W8): two different recordings of about the same length
/// (AJ's 51- and 53-minute streams) are asked "the same recording?" with Yes by default.
/// A "yes" whose sound does not line up must not make them one video (one file's picture
/// over the other's voice at a 0 s offset): they stay two videos, and the note says why.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_wrong_same_recording_answer_keeps_two_videos() {
    if !Path::new(v4()).is_file() || !Path::new(wide()).is_file() {
        eprintln!("skipped: no test videos");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-multi-wrong-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root);
    let r = gmclip_app::video::open_many(
        &c,
        &[
            AddFile {
                path: v4().into(),
                voice: None,
            },
            AddFile {
                path: wide().into(),
                voice: None,
            },
        ],
        &[(0, 1)],
        true,
        None,
    )
    .unwrap();
    let pid = r["project"].as_str().unwrap().to_owned();
    let m = gmclip_app::streamedit::project_map_view(&c, &pid).unwrap();
    let grouped = m["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["group"].is_string())
        .count();
    assert_eq!(grouped, 0, "not one video: {m}");
    assert_eq!(m["sources"].as_array().unwrap().len(), 2, "{m}");
    assert!(r.to_string().contains("separate"), "the note says why: {r}");
    c.analysis.shutdown(Duration::from_secs(30));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
