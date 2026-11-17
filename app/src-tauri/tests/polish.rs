//! Edit polish (Ideas 2026-10-16, lane K2; decision 0025) through the app core: stressed
//! words from the cut's own levels proposed and starred (and starred on their own when
//! the caption style asks, once per word), punch-ins on the strongest of them, callouts
//! placed clear of the captions and the face, the loop option and its fix, and a hold
//! where the screen jumps mid-sentence (frames from a scripted reader).
//!
//! Synthetic project: words written as an analysis blob (0.5 s apart), level frames with
//! one loud word.

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::activity::LumaFrame;
use gmclip_model::entities::{CaptionDoc, ContentHold, Overlay, SplitTag, VisualSplit};
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;

const TEXT: &str = "So MiniMax made this video in 67 seconds on my NVIDIA card. \
    Then I opened the settings and we were able to generate that clip with the new model running locally here. \
    Click this button and the whole thing renders while the queue keeps going without any trouble at all today. \
    After that the result looks great and it loops back to the start of the video nicely.";

fn core(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-polish-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    (c, root)
}

/// Level frames (40 ms): talk around −30 dB (±3 dB per half second), +12 dB on [21.0, 21.4).
fn level_blob(secs: usize) -> Vec<u8> {
    use gmclip_analysis::energy::{Energy, Hop};
    let mut seed: u64 = 3;
    let jit: Vec<f32> = (0..secs * 2)
        .map(|_| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) * 6.0 - 3.0
        })
        .collect();
    let db: Vec<f32> = (0..secs * 25)
        .map(|i| {
            let t = i as f64 / 25.0;
            let loud = if (21.0..21.4).contains(&t) { 12.0 } else { 0.0 };
            -30.0 + jit[(t * 2.0) as usize] + loud
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

struct Fx {
    c: Arc<AppCore>,
    root: PathBuf,
    pid: String,
    cut: String,
    parts: Vec<String>,
    screen: String,
}

fn fixture(tag: &str) -> Fx {
    let (c, root) = core(tag);
    let pid = c.create_project("Polish").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let s = c
        .call(
            &pid,
            "source_add",
            json!({"path": "/nowhere/polish.mp4", "fingerprint": "fp",
                   "probe": {"duration": 300 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264",
                              "width": 1920, "height": 1080, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                             "audio_tracks": []}}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ws: Vec<Value> = TEXT
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let t = (10.0 + i as f64 * 0.5) * SEC as f64;
            json!({"text": w, "start": t as i64, "end": t as i64 + SEC * 4 / 10})
        })
        .collect();
    let p = c.project(&pid).unwrap();
    p.handle
        .put_analysis(
            &s,
            "words",
            "r1",
            serde_json::to_vec(
                &json!({"schema": "gmclip.analysis.words/0", "revision": "r1", "words": ws}),
            )
            .unwrap(),
        )
        .unwrap();
    p.handle
        .put_analysis(&s, "energy_40ms", "e1", level_blob(300))
        .unwrap();
    let id = |v: Value| v["output"]["id"].as_str().unwrap().to_owned();
    // the caption styles (RT story) a cut with captions plans with
    gmclip_app::short::ensure_presets(&c, &pid).unwrap();
    let cut = c
        .call(&pid, "cut_create", json!({"title": "polish"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let framing = id(c
        .call(
            &pid,
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        )
        .unwrap());
    let screen = id(c
        .call(
            &pid,
            "layer_add",
            json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 0.75, "h": 1},
                   "place": {"x": 0, "y": 0.13, "w": 1}}),
        )
        .unwrap());
    c.call(
        &pid,
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "cam"}, "crop": {"x": 0.8, "y": 0.68, "w": 0.2, "h": 0.32},
               "place": {"x": 0.15, "y": 0.58, "w": 0.7}, "z": 1}),
    )
    .unwrap();
    let mut parts: Vec<String> = Vec::new();
    for (a, b) in [(10, 23), (24, 32), (34, 44)] {
        let mut args =
            json!({"cut": cut, "media": {"source": s}, "src_in": a * SEC, "src_out": b * SEC});
        if let Some(l) = parts.last() {
            args["after"] = json!(l);
        }
        parts.push(id(c.call(&pid, "part_add", args).unwrap()));
    }
    Fx {
        c,
        root,
        pid,
        cut,
        parts,
        screen,
    }
}

impl Fx {
    fn view(&self) -> Value {
        gmclip_app::polish::view(&self.c, &self.pid, &self.cut).unwrap()
    }
    fn apply(&self, kind: &str, key: &str) -> Value {
        gmclip_app::polish::apply(&self.c, &self.pid, &self.cut, kind, key).unwrap()
    }
    fn doc(&self) -> gmclip_model::Doc {
        self.c
            .project(&self.pid)
            .unwrap()
            .handle
            .snapshot()
            .doc
            .clone()
    }
    fn keywords(&self) -> usize {
        let d = self.doc();
        d.iter::<CaptionDoc>().map(|c| c.keywords.len()).sum()
    }
    fn undo(&self) {
        self.c.call(&self.pid, "undo", json!({})).unwrap();
    }
    fn done(self) {
        self.c.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn stressed_words_become_keywords_and_punch_ins() {
    let fx = fixture("stress");
    let v = fx.view();
    let em = &v["emphasis"];
    assert_eq!(em["state"], "ok", "{em}");
    let props = em["proposals"].as_array().unwrap();
    let gw = props
        .iter()
        .find(|p| p["text"] == "generate")
        .unwrap_or_else(|| panic!("{em}"));
    assert!(
        gw["label"].as_str().unwrap().starts_with("louder: +"),
        "{gw}"
    );
    assert!(gw["db_over"].as_f64().unwrap() > 8.0, "{gw}");
    assert!(props.len() <= em["budget"].as_u64().unwrap() as usize);
    // one click: a keyword; one ⌘Z: not
    fx.apply("emphasis", gw["key"].as_str().unwrap());
    assert_eq!(fx.keywords(), 1);
    let v = fx.view();
    let gen2 = v["emphasis"]["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["text"] == "generate")
        .unwrap()
        .clone();
    assert_eq!(gen2["keyword"], true);
    fx.undo();
    assert_eq!(fx.keywords(), 0);

    // punch-in at the stressed word, held to the end of its phrase
    let pu = &v["punch"];
    assert_eq!(pu["on"], false);
    let s = pu["suggestions"].as_array().unwrap();
    assert!(s.iter().any(|x| x["text"] == "generate"), "{pu}");
    fx.apply("punch", "all");
    let doc = fx.doc();
    let splits: Vec<&VisualSplit> = doc.iter::<VisualSplit>().collect();
    assert_eq!(
        splits
            .iter()
            .filter(|x| x.tag == Some(SplitTag::PunchIn))
            .count(),
        s.len()
    );
    assert_eq!(
        splits
            .iter()
            .filter(|x| x.tag == Some(SplitTag::PunchOut))
            .count(),
        s.len()
    );
    let v = fx.view();
    assert_eq!(v["punch"]["on"], true);
    assert!(!v["punch"]["suggestions"][0]["added"].is_null());
    // the cut still plans (eased splits)
    assert!(fx.c.planned(&fx.pid, &fx.cut).is_ok());
    fx.apply("punch", "off");
    assert_eq!(fx.doc().iter::<VisualSplit>().count(), 0);
    fx.undo();
    assert_eq!(fx.doc().iter::<VisualSplit>().count(), 2 * s.len());
    fx.done();
}

#[test]
fn the_caption_style_can_star_stressed_words_once() {
    let fx = fixture("auto");
    fx.apply("emphasis", "auto_on");
    let n = fx.keywords();
    assert!(n >= 1);
    let v = fx.view();
    assert_eq!(v["emphasis"]["auto"], true);
    assert_eq!(v["auto_applied"], 0, "nothing new");
    // the person unstars "generate": the style never stars it again
    let gw = v["emphasis"]["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["text"] == "generate")
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    fx.c.call(
        &fx.pid,
        "caption_keyword_toggle",
        json!({"cut": fx.cut, "word": {"asr_revision": "r1", "ordinal": gw}}),
    )
    .unwrap();
    assert_eq!(fx.keywords(), n - 1);
    let v = fx.view();
    assert_eq!(v["auto_applied"], 0);
    assert_eq!(fx.keywords(), n - 1);
    // off: proposals only
    fx.apply("emphasis", "auto_off");
    assert_eq!(fx.view()["emphasis"]["auto"], false);
    fx.done();
}

#[test]
fn callouts_land_in_the_safe_zones_clear_of_the_face() {
    let fx = fixture("callouts");
    let v = fx.view();
    let cs = v["callouts"].as_array().unwrap();
    let got: Vec<(&str, &str)> = cs
        .iter()
        .map(|c| (c["kind"].as_str().unwrap(), c["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("label", "MiniMax"),
            ("label", "67 seconds"),
            ("label", "NVIDIA"),
            ("arrow", "this button")
        ],
        "{v}"
    );
    // the face is at y 0.58·1920 = 1114 px and down: every label sits above it, below 220
    for c in cs.iter().filter(|c| c["kind"] == "label") {
        let y = c["apply"]["place"]["y"].as_f64().unwrap() * 1920.0;
        assert!((220.0..1114.0 - 92.0).contains(&y), "{c}");
        assert!(c["label"].as_str().unwrap().contains("said at"), "{c}");
    }
    let arrow = &cs[3]["apply"]["shape"];
    assert!(
        arrow["from"][1].as_f64().unwrap() * 1920.0 > 220.0,
        "{arrow}"
    );
    // one click adds it, and it says so
    fx.apply("callout", cs[1]["key"].as_str().unwrap());
    let doc = fx.doc();
    let o: Vec<&Overlay> = doc.iter::<Overlay>().collect();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].role.as_deref(), Some("note"));
    assert_eq!(fx.view()["callouts"][1]["added"], true);
    assert!(fx.c.planned(&fx.pid, &fx.cut).is_ok(), "the label plans");
    fx.undo();
    assert_eq!(fx.doc().iter::<Overlay>().count(), 0);
    fx.done();
}

#[test]
fn the_loop_option_and_its_fix() {
    let fx = fixture("loop");
    let doc = fx.doc();
    let framing = doc
        .iter::<gmclip_model::entities::Framing>()
        .next()
        .unwrap()
        .id
        .0
        .to_string();
    fx.c.call(
        &fx.pid,
        "crop_set",
        json!({"layer": fx.screen, "part": fx.parts[2], "framing": framing, "crop": {"x": 0.2, "y": 0.2, "w": 0.5, "h": 0.5}}),
    )
    .unwrap();
    assert!(fx.view()["loop"]["on"].is_null());
    fx.apply("loop", "on");
    let l = fx.view()["loop"].clone();
    assert_eq!(l["matches"], false, "{l}");
    let keys: Vec<&str> = l["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["layout", "tail"]);
    fx.apply("loop", "layout");
    assert_eq!(fx.view()["loop"]["matches"], true);
    fx.undo();
    fx.apply("loop", "tail");
    let l = fx.view()["loop"].clone();
    assert_eq!(l["matches"], true, "{l}");
    assert_eq!(
        gmclip_model::clock::cut_layout(&fx.doc(), gmclip_app::planner::ulid_id(&fx.cut).unwrap())
            .len(),
        4
    );
    fx.done();
}

/// A 64×36 grey picture with a pattern per "screen".
fn screen(kind: u8) -> LumaFrame {
    let (w, h) = (64u32, 36u32);
    LumaFrame {
        width: w,
        height: h,
        data: (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| match kind {
                0 => {
                    if (y / 4) % 2 == 0 && x < 40 {
                        200
                    } else {
                        30
                    }
                }
                1 => {
                    if x % 16 < 8 {
                        240
                    } else {
                        90
                    }
                }
                _ => {
                    if y % 12 < 6 {
                        10
                    } else {
                        250
                    }
                }
            })
            .collect(),
    }
}

#[test]
fn a_screen_jump_mid_sentence_is_a_hold_and_one_at_a_sentence_start_is_not() {
    let fx = fixture("holds");
    // source frames (30 fps): another app from 29.0 s (mid-sentence), another at 35.5 s
    // (a sentence starts there)
    let read = |_: &gmclip_core::gmclip_plan::Plan, _: &str, frames: &[u64]| {
        Ok(frames
            .iter()
            .map(|f| match f {
                f if *f < 870 => screen(0),
                f if *f < 1065 => screen(1),
                _ => screen(2),
            })
            .collect())
    };
    let r = gmclip_app::polish::holds_find_with(&fx.c, &fx.pid, &fx.cut, &read).unwrap();
    let hs = r["holds"].as_array().unwrap();
    assert_eq!(hs.len(), 1, "{r}");
    // part two starts at 13 s on the cut: 29.0 s of source is 18.0 s = frame 540
    assert_eq!(hs[0]["change_frame"], 540, "{r}");
    assert_eq!(hs[0]["frame"], 539, "the last frame before the jump");
    assert_eq!(hs[0]["until_word"], "today.");
    assert!(hs[0]["label"].as_str().unwrap().contains("σ"));
    // one click: the screen held from there to the end of the sentence
    let h = gmclip_app::tighten::hold_toggle(&fx.c, &fx.pid, &fx.cut, 539).unwrap();
    assert_eq!(h["held"], true, "{h}");
    assert_eq!(fx.doc().iter::<ContentHold>().count(), 1);
    let again = gmclip_app::polish::holds_find_with(&fx.c, &fx.pid, &fx.cut, &read).unwrap();
    assert_eq!(again["holds"].as_array().unwrap().len(), 0, "held already");
    fx.undo();
    assert_eq!(fx.doc().iter::<ContentHold>().count(), 0);
    fx.done();
}
