//! L.06 consolidate: a short of three parts on a finished reel (AJ's h79 v4, read only)
//! is consolidated (1 s handles: the reel's GOPs are 8.3 s, so 5 s would keep it all); the folder holds only the stretches it uses, re-wrapped
//! without re-encoding; moved to another place and opened in a fresh app data dir (another
//! Mac, as far as gmclip can tell), the same cut renders the same frames (SHA-256 of every
//! output frame) and mixes the same sound. Skips without the reel; macOS (VideoToolbox).
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_media::audio::graph::{AudioPlan, PcmSource};
use gmclip_media::backend::aac::AacRandomReader;
use gmclip_plan::Plan;
use gmclip_render::export::Inputs;
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
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

/// SHA-256 of every output frame, rendered 30 at a time.
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

type Reader = AacRandomReader<Box<dyn gmclip_media::backend::SampleSource + Send>>;

struct Pcm(std::collections::HashMap<String, Reader>);

impl PcmSource for Pcm {
    fn read(&mut self, source: &str, first_sample: u64, out: &mut [f32]) -> Result<(), String> {
        self.0
            .get_mut(source)
            .ok_or("no reader")?
            .read(first_sample, out)
            .map_err(|e| e.to_string())
    }
}

/// The plan's mixed sound (before loudness), as the export mixes it.
fn mix(plan: &Plan) -> Vec<f32> {
    let ap = AudioPlan::from_plan(plan).unwrap();
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let mut readers = std::collections::HashMap::new();
    for p in &plan.parts {
        let id = p.audio.source.as_str().to_owned();
        if readers.contains_key(&id) {
            continue;
        }
        let a = inputs.audio(plan, &id).unwrap();
        readers.insert(
            id,
            AacRandomReader::new(a.source, &a.asc, a.sample_rate, a.channels, a.skip).unwrap(),
        );
    }
    ap.mix(&mut Pcm(readers)).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_consolidated_copy_renders_the_same_frames_elsewhere() {
    same_frames_elsewhere(false);
}

/// The recording already a sync group (picture and sound members, as a paired stream
/// is): the members' segments are re-cut onto the trimmed file.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_sync_group_consolidates_too() {
    same_frames_elsewhere(true);
}

#[cfg(any(target_os = "macos", windows))]
fn same_frames_elsewhere(group: bool) {
    if !Path::new(video()).is_file() {
        eprintln!("skipped: no test video");
        return;
    }
    let root =
        std::env::temp_dir().join(format!("gmclip-consolidate-{}-{group}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root.join("mac-a"));
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let pid = v["project"].as_str().unwrap().to_owned();
    c.analysis.cancel_project(&pid);
    let stream = v["cut"].as_str().unwrap().to_owned();
    let media: Value = c.cut_view(&pid, &stream).unwrap()["parts"][0]["media"].clone();
    let media = if media.is_null() {
        let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
        let s = doc
            .iter::<gmclip_model::entities::Source>()
            .next()
            .unwrap()
            .id;
        json!({"source": s.0.to_string()})
    } else {
        media
    };
    let media = if group {
        let s = media["source"].as_str().unwrap().to_owned();
        // a second copy of the reel as the sound (each source once in a group)
        let snd = root.join("sound.mp4");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(video(), &snd).unwrap();
        let s2 = c
            .call(&pid, "source_add", json!({"path": snd.to_string_lossy(),
                   "fingerprint": format!("sha256:{}", gmclip_ml::sha256::file_hex(&snd).unwrap().0),
                   "probe": c.project(&pid).unwrap().handle.snapshot().doc.iter::<gmclip_model::entities::Source>().next().map(|x| serde_json::to_value(&x.probe).unwrap())}))
            .unwrap()["output"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let g = c
            .call(&pid, "sync_group_create", json!({"members": [
                {"source": s, "role": "picture", "offset": 0}, {"source": s2, "role": "audio", "offset": 0}]}))
            .unwrap()["output"]["group"]
            .as_str()
            .unwrap()
            .to_owned();
        json!({"group": g})
    } else {
        media
    };
    // a 9:16 short of three parts (3–6 s, 20–23 s, 45–48 s)
    let cut = c
        .call(&pid, "cut_create", json!({"title": "three parts"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut after: Option<String> = None;
    for (a, b) in [(3, 6), (27, 30), (51, 54)] {
        let mut args = json!({"cut": cut, "media": media, "src_in": a * SEC, "src_out": b * SEC});
        if let Some(x) = &after {
            args["after"] = json!(x);
        }
        after = c.call(&pid, "part_add", args).unwrap()["output"]["id"]
            .as_str()
            .map(str::to_owned);
    }
    // no transcript here: captions off
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let f = c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.set_active_framing(&pid, &cut, &f).unwrap();
    gmclip_app::framing::set_output(&c, &pid, &cut, "9:16").unwrap();
    gmclip_app::framing::apply_preset(&c, &pid, &cut, &[], "screen_cam").unwrap();
    let plan_a = c.planned(&pid, &cut).unwrap().plan;
    let t = std::time::Instant::now();
    let frames_a = frame_hashes(&plan_a);
    eprintln!(
        "original: {} frames hashed in {:?}",
        frames_a.len(),
        t.elapsed()
    );
    let sound_a = mix(&plan_a);

    // consolidate
    let r = gmclip_app::consolidate::consolidate(
        &c,
        &pid,
        &gmclip_app::consolidate::Options {
            cuts: Some(vec![cut.clone()]),
            topics: vec![],
            // the reel's GOPs are 8.3 s: 1 s handles keep three stretches of a 57 s file
            handles_s: 1.0,
            dest: root.join("out"),
            name: Some("Three parts".into()),
        },
    )
    .unwrap();
    eprintln!("consolidated: {r:#}");
    let src = &r["sources"][0];
    assert!(
        r["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["how"] == "trimmed"),
        "{r}"
    );
    assert!(
        r["bytes_after"].as_u64().unwrap() < r["bytes_before"].as_u64().unwrap() * 3 / 4,
        "{r}"
    );
    // the stretches kept: each part ± 1 s, on whole GOPs
    let pieces = src["pieces"].as_array().unwrap();
    assert_eq!(pieces.len(), 3, "{r}");
    for (a, b) in [(3, 6), (27, 30), (51, 54)] {
        assert!(
            pieces
                .iter()
                .any(|p| p[0].as_i64().unwrap() <= (a - 1) * 1000
                    && p[1].as_i64().unwrap() >= (b + 1) * 1000),
            "{a}–{b} s ± 1 s is kept: {pieces:?}"
        );
    }
    let home = PathBuf::from(r["home"].as_str().unwrap());
    assert!(home.join("consolidated.json").is_file());
    assert!(home.join("owner.json").is_file());

    // another Mac: the folder copied elsewhere, a fresh app data dir
    let moved = root.join("elsewhere/Three parts.gmclip");
    copy_dir(&home, &moved);
    std::fs::remove_dir_all(&home).unwrap();
    let c2 = app(&root.join("mac-b"));
    let opened = gmclip_app::consolidate::open_copy(&c2, &moved).unwrap();
    let pid2 = opened["id"].as_str().unwrap().to_owned();
    assert_ne!(pid2, pid);
    let plan_b = c2.planned(&pid2, &cut).unwrap().plan;
    assert_eq!(plan_b.output.frames, plan_a.output.frames);
    assert!(
        plan_b.sources.iter().all(|s| s
            .path
            .starts_with(moved.canonicalize().unwrap().to_str().unwrap())),
        "{:?}",
        plan_b.sources
    );
    let frames_b = frame_hashes(&plan_b);
    let same = frames_a
        .iter()
        .zip(&frames_b)
        .filter(|(x, y)| x == y)
        .count();
    assert_eq!(
        same,
        frames_a.len(),
        "every frame identical ({same} of {})",
        frames_a.len()
    );
    let sound_b = mix(&plan_b);
    assert_eq!(sound_a.len(), sound_b.len());
    let diff = sound_a
        .iter()
        .zip(&sound_b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    eprintln!(
        "sound: max |a − b| = {diff:e} over {} samples",
        sound_a.len()
    );
    assert!(diff == 0.0, "the same decoded samples");

    // the other Mac's cut stays editable: a trim commits and plans
    let doc = c2.project(&pid2).unwrap().handle.snapshot().doc.clone();
    let part = doc
        .iter::<gmclip_model::entities::Part>()
        .find(|p| p.cut.0.to_string() == cut)
        .unwrap()
        .id;
    c2.call(
        &pid2,
        "part_trim",
        json!({"part": part.0.to_string(), "edge": "out", "delta": -(SEC / 30) * 3}),
    )
    .ok();
    assert!(c2.planned(&pid2, &cut).is_ok());
    // QA (milestones/m2/qa-e2e.md, W5): the cut that was not consolidated (the opened
    // video's whole-file cut) is cut to the stretches that travelled, so it still opens.
    // (In the group case the whole-file cut names the picture's own source, which is not
    // regrouped: a known limit, left as it was.)
    if !group {
        let whole = c2.planned(&pid2, &stream);
        assert!(whole.is_ok(), "the left-out cut plans: {:?}", whole.err());
        let kept: Vec<(i64, i64)> = pieces
            .iter()
            .map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap()))
            .collect();
        let doc = c2.project(&pid2).unwrap().handle.snapshot().doc.clone();
        let left: Vec<(i64, i64)> = doc
            .iter::<gmclip_model::entities::Part>()
            .filter(|p| p.cut.0.to_string() == stream)
            .map(|p| (p.src_in.0 * 1000 / SEC, p.src_out.0 * 1000 / SEC))
            .collect();
        assert_eq!(left.len(), 3, "one part per kept stretch: {left:?}");
        for (a, b) in &left {
            assert!(
                kept.iter().any(|(x, y)| x <= a && b <= y),
                "{left:?} inside {kept:?}"
            );
        }
    }
    c2.shutdown();
    c.analysis.shutdown(std::time::Duration::from_secs(10));
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A stream gmclip cannot cut losslessly — AJ's 1440p VP9 Matroska recording as the
/// picture of a sync group, with a 48 kHz AAC proxy as its sound (both read only): the
/// picture goes through the export encoder (H.264, ≤ 1920 wide), the sound is trimmed
/// losslessly; the copy opens elsewhere, its frames are close to the original's (lossy,
/// not identical) and its sound is the same. Lab: `--ignored`.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: AJ's 1440p VP9 stream and a stream proxy"]
fn a_stream_it_cannot_cut_is_reencoded() {
    /// `streams/jMcAmooE53I-source.mkv` in the lab media folder.
    fn mkv() -> &'static str {
        static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            gmclip_model::lab::media_str("streams/jMcAmooE53I-source.mkv")
        });
        P.as_str()
    }
    /// `preview/loc_4fadb756.proxy.mp4` in the lab media folder.
    fn sound() -> &'static str {
        static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            gmclip_model::lab::media_str("preview/loc_4fadb756.proxy.mp4")
        });
        P.as_str()
    }
    if !Path::new(mkv()).is_file() || !Path::new(sound()).is_file() {
        eprintln!("skipped: no MKV or proxy");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-consolidate-mkv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = app(&root.join("mac-a"));
    let p = c.create_project("vp9").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let add = |path: &str| {
        let pr = gmclip_app::video::probe(Path::new(path)).unwrap();
        c.call(&pid, "source_add", json!({"path": path, "fingerprint": format!("sha256:{}", gmclip_ml::sha256::file_hex(Path::new(path)).unwrap().0),
                                          "probe": pr.probe}))
            .unwrap()["output"]["id"].as_str().unwrap().to_owned()
    };
    let (pic, snd) = (add(mkv()), add(sound()));
    let g = c
        .call(&pid, "sync_group_create", json!({"members": [
            {"source": pic, "role": "picture", "offset": 0}, {"source": snd, "role": "audio", "offset": 0}]}))
        .unwrap()["output"]["group"].as_str().unwrap().to_owned();
    let media = json!({"group": g});
    let cut = c.call(&pid, "cut_create", json!({"title": "two"})).unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let a = c
        .call(
            &pid,
            "part_add",
            json!({"cut": cut, "media": media, "src_in": 600 * SEC, "src_out": 603 * SEC}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(&pid, "part_add", json!({"cut": cut, "media": media, "after": a, "src_in": 2400 * SEC, "src_out": 2402 * SEC})).unwrap();
    c.call(&pid, "caption_set", json!({"cut": cut, "enabled": false}))
        .unwrap();
    let f = c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "16:9", "w": 1280, "h": 720, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.set_active_framing(&pid, &cut, &f).unwrap();
    c.call(&pid, "layout_edit", json!({"cut": cut, "ops": [{"op": "layer_add", "owner": {"framing": f}, "role": {"region": "screen"},
        "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}]})).unwrap();
    let plan_a = c.planned(&pid, &cut).unwrap().plan;
    let sound_a = mix(&plan_a);
    let t = std::time::Instant::now();
    let r = gmclip_app::consolidate::consolidate(
        &c,
        &pid,
        &gmclip_app::consolidate::Options {
            cuts: Some(vec![cut.clone()]),
            topics: vec![],
            handles_s: 5.0,
            dest: root.join("out"),
            name: None,
        },
    )
    .unwrap();
    eprintln!("consolidated in {:?}: {r:#}", t.elapsed());
    let by = |id: &str| {
        r["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["source"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by(&pic)["picture"]["how"], "reencoded");
    assert_eq!(by(&snd)["sound"]["how"], "trimmed");
    assert!(
        r["bytes_after"].as_u64().unwrap() < r["bytes_before"].as_u64().unwrap() / 20,
        "{r}"
    );
    let c2 = app(&root.join("mac-b"));
    let opened =
        gmclip_app::consolidate::open_copy(&c2, Path::new(r["home"].as_str().unwrap())).unwrap();
    let plan_b = c2
        .planned(opened["id"].as_str().unwrap(), &cut)
        .unwrap()
        .plan;
    assert_eq!(plan_b.output.frames, plan_a.output.frames);
    let mut ia = gmclip_render::export::DemuxInputs::new(None);
    let mut ib = gmclip_render::export::DemuxInputs::new(None);
    for k in [0u64, 45, 89, 95, 140] {
        let fa = gmclip_render::export::render_frames(&plan_a, &mut ia, &[k]).unwrap();
        let fb = gmclip_render::export::render_frames(&plan_b, &mut ib, &[k]).unwrap();
        let (x, y) = (&fa[0].1.px, &fb[0].1.px);
        let mad = x
            .iter()
            .zip(y)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / x.len() as f64;
        eprintln!("frame {k}: mean |a − b| = {mad:.2}");
        assert!(mad < 6.0, "frame {k} close to the original ({mad:.2})");
    }
    let sound_b = mix(&plan_b);
    let diff = sound_a
        .iter()
        .zip(&sound_b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    eprintln!("sound: max |a − b| = {diff:e}");
    assert!(diff == 0.0);
    c2.shutdown();
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
