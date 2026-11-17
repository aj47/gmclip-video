//! Decision 0046, "screen look" and "camera bubble": a layer's frame set on the layer or
//! per part and planned as RenderPlan `/1` `layers[].frame` (a plan without frames stays
//! `/0`, byte for byte); the Screen Studio-like built-in plates; the built-in layouts made
//! from a recording's screen and face files (the camera a live layer of the face file) or
//! from one source's screen and cam regions.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use serde_json::{Value, json};

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// The fixture's doc with its source probed as 2560×1440 at 30 fps.
fn probed(fx: &common::Fx, p: &common::Project) -> gmclip_model::Doc {
    let mut doc = p.h.snapshot().doc.clone();
    let mut s = doc.get(uid::<Source>(&fx.source)).unwrap().clone();
    s.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(2560),
        height: Some(1440),
        fps: Some(gmclip_model::Rational::new(30, 1)),
        timescale: 15360,
        gop_max_s: None,
    }];
    doc.insert_for_test(s);
    doc
}

fn plan_bytes(fx: &common::Fx, p: &common::Project) -> (SchemaVersion, Vec<u8>, Value) {
    let plan = planner::plan_cut(&probed(fx, p), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let v = serde_json::to_value(&plan).unwrap();
    (plan.schema, serde_json::to_vec(&plan).unwrap(), v)
}

fn bubble() -> Value {
    json!({"radius": 0.5,
           "shadow": {"blur": 0.04, "offset_y": 0.01, "opacity": 0.35, "colour": "#000000"},
           "border": {"width": 0.005, "colour": "#FFFFFF"}})
}

#[test]
fn a_layer_frame_is_set_cleared_and_planned() {
    let p = open("frame-set");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let (v0, before, _) = plan_bytes(&fx, &p);
    assert_eq!(v0, SchemaVersion::V0);

    // on the layer: every part's cam is a bubble
    p.human(
        "layer_frame_set",
        json!({"layer": fx.cam, "frame": bubble()}),
    );
    let doc = p.h.snapshot().doc.clone();
    let l = doc.get(uid::<Layer>(&fx.cam)).unwrap();
    let f = l.frame.as_ref().unwrap();
    assert_eq!(f.radius, 0.5);
    assert_eq!(f.border.as_ref().unwrap().colour, "#FFFFFF");
    let (v1, _, plan) = plan_bytes(&fx, &p);
    assert_eq!(v1, SchemaVersion::V1);
    for part in plan["parts"].as_array().unwrap() {
        let cam = part["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["role"] == "cam")
            .unwrap();
        assert_eq!(
            cam["frame"],
            json!({"radius": 0.5,
                   "shadow": {"blur": 0.04, "offset_y": 0.01, "opacity": 0.35, "rgb": "#000000"},
                   "border": {"width": 0.005, "rgb": "#FFFFFF"}})
        );
        let screen = part["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["role"] == "screen")
            .unwrap();
        assert!(screen.get("frame").is_none());
    }

    // one part's override clears it there only
    p.human(
        "layer_frame_set",
        json!({"layer": fx.cam, "frame": null, "part": fx.parts[1], "framing": fx.framing}),
    );
    let (_, _, plan) = plan_bytes(&fx, &p);
    let cam = |k: usize| {
        plan["parts"][k]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["role"] == "cam")
            .unwrap()
            .clone()
    };
    assert!(cam(0).get("frame").is_some());
    assert!(cam(1).get("frame").is_none());

    // out of range, a lone part, a cover: refused
    for (bad, at) in [
        (json!({"radius": 0.7}), "/frame/radius"),
        (
            json!({"radius": 0.1, "shadow": {"blur": 0.1, "offset_y": 0, "opacity": 2, "colour": "#000000"}}),
            "/frame/shadow",
        ),
        (
            json!({"radius": 0.1, "border": {"width": 0.01, "colour": "white"}}),
            "/frame/border",
        ),
    ] {
        let e = p
            .try_human("layer_frame_set", json!({"layer": fx.screen, "frame": bad}))
            .unwrap_err();
        assert_eq!(e.code(), "Invalid");
        assert!(format!("{e:?}").contains(at), "{e:?}");
    }
    let e = p
        .try_human(
            "layer_frame_set",
            json!({"layer": fx.screen, "frame": {"radius": 0.1}, "part": fx.parts[0]}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    let cover = id(&p.human(
        "layer_add",
        json!({"owner": {"framing": fx.framing}, "role": {"region": "chat"}, "crop": {"x": 0, "y": 0, "w": 0.2, "h": 0.2}, "place": {"x": 0, "y": 0, "w": 0.2}, "cover": {"solid": {"colour": 0}}}),
    ));
    let e = p
        .try_human(
            "layer_frame_set",
            json!({"layer": cover, "frame": {"radius": 0.1}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");

    // cleared everywhere: the plan is the frameless one, byte for byte
    for _ in 0..3 {
        p.human("undo", json!({}));
    }
    let doc = p.h.snapshot().doc.clone();
    assert!(doc.get(uid::<Layer>(&fx.cam)).unwrap().frame.is_none());
    let (v, after, _) = plan_bytes(&fx, &p);
    assert_eq!(v, SchemaVersion::V0);
    assert_eq!(after, before);
}

#[test]
fn the_screen_studio_plates_are_built_in() {
    let p = open("plates-0046");
    let items = p
        .human("style_library_list", json!({"kind": "plate"}))
        .output["items"]
        .clone();
    for name in [
        "Dusk", "Ocean", "Mint", "Sunset", "Graphite", "Lilac", "Midnight",
    ] {
        let row = items
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("{name} listed"));
        assert_eq!(row["builtin"], true);
        let stops = row["item"]["plate"]["stops"].as_array().unwrap();
        assert!((2..=8).contains(&stops.len()));
    }
}

fn source(p: &common::Project, name: &str, w: u32, h: u32) -> String {
    source_at(p, name, w, h, 30)
}

fn source_at(p: &common::Project, name: &str, w: u32, h: u32, fps: u32) -> String {
    id(&p.human(
        "source_add",
        json!({"path": format!("/rec/{name}.mov"), "fingerprint": planner::fingerprint(Some(&"c".repeat(64)), Some(5)),
               "probe": {"duration": 600 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": w, "height": h,
                          "fps": {"num": fps, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                         "audio_tracks": [{"index": 1, "codec": "aac", "rate": 48000, "layout": "stereo", "bitrate": null, "speech_ratio": null}]}}),
    ))
}

fn layout_layers(doc: &gmclip_model::Doc, layout: &str) -> Vec<Layer> {
    doc.children::<Layer>(uid::<Layout>(layout).entity())
        .filter_map(|l| doc.get(l).cloned())
        .collect()
}

/// Decision 0047: a capture card's 25 fps face file beside a 30 fps screen. The bubble
/// reads it by time (`layers[].fps`), so the cut plans instead of refusing the rate.
#[test]
fn a_camera_at_another_rate_is_read_by_time() {
    camera_by_time(30, 25, 30);
}

#[test]
fn a_camera_beside_a_rate_converting_screen_is_read_by_time() {
    for output in [24, 25, 30, 48, 50, 60] {
        for (screen, camera) in [(60, 25), (30, 60), (42, 25), (25, 30)] {
            camera_by_time(screen, camera, output);
        }
    }
}

fn camera_by_time(screen_fps: u32, camera_fps: u32, output_fps: u32) {
    let p = open("bubble-25");
    let face = source_at(&p, "face", 1920, 1080, camera_fps);
    let screen = source_at(&p, "screen", 2560, 1600, screen_fps);
    let g = p.human(
        "sync_group_create",
        json!({"members": [{"source": face, "role": "audio", "offset": 0}, {"source": screen, "role": "picture", "offset": 0}]}),
    );
    let group = g.output["group"].as_str().unwrap().to_owned();
    let cut = p.human("cut_create", json!({"title": "rec"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("caption_set", json!({"cut": cut, "enabled": false}));
    let framing = id(&p.human(
        "framing_create",
        json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": output_fps, "den": 1}}),
    ));
    p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    let part = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"group": group}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
    ));
    p.human(
        "layout_builtin_add",
        json!({"cut": cut, "framing": framing, "name": "Screen + camera bubble (bottom right)", "parts": [part]}),
    );
    let doc = p.h.snapshot().doc.clone();
    let plan = planner::plan_cut(&doc, uid(&cut), uid(&framing), &Files).unwrap();
    let pl = &plan.parts[0];
    assert_eq!(
        pl.picture.fps.map(|r| (r.num, r.den)),
        (screen_fps != output_fps).then_some((screen_fps as u64, 1))
    );
    let cam = pl.layers.iter().find(|l| l.role.as_str() == "cam").unwrap();
    assert_eq!(cam.first_frame, Some(10 * u64::from(output_fps)));
    assert_eq!(
        cam.fps.map(|r| (r.num, r.den)),
        (camera_fps != output_fps).then_some((u64::from(camera_fps), 1))
    );
    let screen_l = pl
        .layers
        .iter()
        .find(|l| l.role.as_str() == "screen")
        .unwrap();
    assert!(screen_l.fps.is_none());
    let v = serde_json::to_vec(&plan).unwrap();
    gmclip_plan::parse(&v).expect("the plan validates");
}

/// A 0045 recording: the face file (the voice) and the screen file one sync group, as the
/// app's `open_recording` pairs them.
#[test]
fn builtin_layouts_from_a_recordings_screen_and_face() {
    let p = open("bubble-rec");
    let face = source(&p, "face", 1920, 1080);
    let screen = source(&p, "screen", 2560, 1600);
    let g = p.human(
        "sync_group_create",
        json!({"members": [{"source": face, "role": "audio", "offset": 0}, {"source": screen, "role": "picture", "offset": 0}]}),
    );
    let group = g.output["group"].as_str().unwrap().to_owned();
    let cut = p.human("cut_create", json!({"title": "rec"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("caption_set", json!({"cut": cut, "enabled": false}));
    let framing = id(&p.human(
        "framing_create",
        json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    let part = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"group": group}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
    ));
    let list = p.human("layout_builtin_list", json!({"cut": cut})).output;
    let rows = list["layouts"].as_array().unwrap();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().all(|r| r["available"] == true), "{list}");

    let made = p
        .human(
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": "Screen + camera bubble (bottom right)", "parts": [part]}),
        )
        .output;
    let doc = p.h.snapshot().doc.clone();
    let layers = layout_layers(&doc, made["id"].as_str().unwrap());
    assert_eq!(layers.len(), 2);
    let face_member = doc
        .children::<SyncMember>(uid::<SyncGroup>(&group).entity())
        .filter_map(|m| doc.get(m))
        .find(|m| m.source == uid::<Source>(&face))
        .unwrap()
        .id;
    let s = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Screen))
        .unwrap();
    let c = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Cam))
        .unwrap();
    // the screen: 6 % of 1080 = 64.8 px padding; 16:10 fits by height
    let h_px = 1080.0 - 2.0 * 64.8;
    let w_px = h_px * 1.6;
    assert!((s.place.w - w_px / 1920.0).abs() < 1e-9);
    assert!((s.place.y - 64.8 / 1080.0).abs() < 1e-9);
    assert_eq!(s.frame.as_ref().unwrap().radius, 0.03);
    assert!(s.frame.as_ref().unwrap().shadow.is_some());
    // the camera: the face file, a centred square crop, 22 % of 1080 across, 4 % in
    assert_eq!(c.member, Some(face_member));
    assert!((c.crop.w * 1920.0 - 1080.0).abs() < 1e-6 && c.crop.h == 1.0);
    assert!((c.crop.x - (1920.0 - 1080.0) / 2.0 / 1920.0).abs() < 1e-9);
    let d = 0.22 * 1080.0;
    assert!((c.place.w - d / 1920.0).abs() < 1e-9);
    assert!((c.place.x - (1920.0 - 43.2 - d) / 1920.0).abs() < 1e-9);
    assert!((c.place.y - (1080.0 - 43.2 - d) / 1080.0).abs() < 1e-9);
    let cf = c.frame.as_ref().unwrap();
    assert_eq!(cf.radius, 0.5);
    assert_eq!(cf.border.as_ref().unwrap().width, 0.005);
    assert_eq!(cf.border.as_ref().unwrap().colour, "#FFFFFF");
    assert!(c.z > s.z);
    assert_eq!(
        doc.get(uid::<Part>(&part)).unwrap().look[&uid::<Framing>(&framing)].layout_ref,
        Some(uid::<Layout>(made["id"].as_str().unwrap()))
    );

    // planned: the camera reads the face file live from the part's frame
    let plan = planner::plan_cut(&doc, uid(&cut), uid(&framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let pl = &plan.parts[0];
    let cam = pl.layers.iter().find(|l| l.role.as_str() == "cam").unwrap();
    let face_id = plan
        .sources
        .iter()
        .find(|s| s.path == "/rec/face.mov")
        .unwrap()
        .id
        .clone();
    assert_ne!(pl.picture.source, face_id);
    assert_eq!(cam.picture_source.as_ref(), Some(&face_id));
    assert_eq!(cam.first_frame, Some(300));
    assert_eq!(cam.frame.as_ref().unwrap().radius, 0.5);
    assert!((cam.place.w - cam.place.h).abs() < 1e-6, "a square place");
    let screen_l = pl
        .layers
        .iter()
        .find(|l| l.role.as_str() == "screen")
        .unwrap();
    assert!(screen_l.picture_source.is_none() && screen_l.first_frame.is_none());
    let v = serde_json::to_vec(&plan).unwrap();
    gmclip_plan::parse(&v).expect("the plan validates");

    // camera full and screen only: the other picture is there, hidden, so a layout
    // section of them hides it (sections map layout layers by role)
    for (name, roles) in [
        (
            "Camera full",
            vec![
                LayerRole::Region(RegionRole::Screen),
                LayerRole::Region(RegionRole::Cam),
            ],
        ),
        (
            "Screen only",
            vec![
                LayerRole::Region(RegionRole::Screen),
                LayerRole::Region(RegionRole::Cam),
            ],
        ),
    ] {
        let made = p
            .human(
                "layout_builtin_add",
                json!({"cut": cut, "framing": framing, "name": name}),
            )
            .output;
        let doc = p.h.snapshot().doc.clone();
        let layers = layout_layers(&doc, made["id"].as_str().unwrap());
        assert_eq!(layers.iter().map(|l| l.role).collect::<Vec<_>>(), roles);
        let shown: Vec<bool> = layers.iter().map(|l| l.visible).collect();
        assert_eq!(
            shown,
            if name == "Camera full" {
                vec![false, true]
            } else {
                vec![true, false]
            }
        );
        if name == "Camera full" {
            let l = &layers[1];
            assert_eq!((l.place.x, l.place.y, l.place.w), (0.0, 0.0, 1.0));
            assert!(l.frame.is_none());
            assert_eq!(l.crop, gmclip_model::geometry::NormRect::FULL);
        }
    }
    // one undo step each
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert!(doc.iter::<Layout>().next().is_none());
    let e = p
        .try_human(
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": "Picture in picture"}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
}

/// Decision 0052: "Screen over camera" on a 9:16 canvas: the whole screen across the
/// top, the face file below filling the rest, cropped about its centre; a portrait
/// display is cut to 55 % of the height. It plans with the camera read live.
#[test]
fn screen_over_camera_stacks_a_recording_for_9_16() {
    for (sw, sh, screen_h) in [(2560u32, 1600u32, 675.0), (2160, 3840, 1056.0)] {
        let p = open(&format!("stack-{sw}"));
        let face = source(&p, "face", 1920, 1080);
        let screen = source(&p, "screen", sw, sh);
        let g = p.human(
            "sync_group_create",
            json!({"members": [{"source": face, "role": "audio", "offset": 0}, {"source": screen, "role": "picture", "offset": 0}]}),
        );
        let group = g.output["group"].as_str().unwrap().to_owned();
        let cut = p.human("cut_create", json!({"title": "rec"})).output["cut"]
            .as_str()
            .unwrap()
            .to_owned();
        p.human("caption_set", json!({"cut": cut, "enabled": false}));
        let framing = id(&p.human(
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        ));
        p.human(
            "layer_add",
            json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
        );
        let part = id(&p.human(
            "part_add",
            json!({"cut": cut, "media": {"group": group}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
        ));
        let list = p.human("layout_builtin_list", json!({"cut": cut})).output;
        assert!(
            list["layouts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == "Screen over camera" && r["available"] == true),
            "{list}"
        );
        let made = p
            .human(
                "layout_builtin_add",
                json!({"cut": cut, "framing": framing, "name": "Screen over camera", "parts": [part]}),
            )
            .output;
        let doc = p.h.snapshot().doc.clone();
        let layers = layout_layers(&doc, made["id"].as_str().unwrap());
        let s = layers
            .iter()
            .find(|l| l.role == LayerRole::Region(RegionRole::Screen))
            .unwrap();
        let c = layers
            .iter()
            .find(|l| l.role == LayerRole::Region(RegionRole::Cam))
            .unwrap();
        // the screen: full width at the top, its crop the shape of its box
        assert_eq!((s.place.x, s.place.y, s.place.w), (0.0, 0.0, 1.0));
        assert!(s.visible && c.visible && s.frame.is_none() && c.frame.is_none());
        let box_h = 1080.0 * (s.crop.h * f64::from(sh)) / (s.crop.w * f64::from(sw));
        assert!((box_h - screen_h).abs() < 1e-6, "{box_h}");
        assert!((s.crop.w - 1.0).abs() < 1e-9);
        assert!((s.crop.y - (1.0 - s.crop.h) / 2.0).abs() < 1e-9);
        // the camera: the face file, below it, filling the rest, centred
        assert_ne!(c.member, None);
        assert!((c.place.y - screen_h / 1920.0).abs() < 1e-9);
        assert_eq!((c.place.x, c.place.w), (0.0, 1.0));
        let cam_h = 1080.0 * (c.crop.h * 1080.0) / (c.crop.w * 1920.0);
        assert!((cam_h - (1920.0 - screen_h)).abs() < 1e-6, "{cam_h}");
        assert_eq!(c.crop.h, 1.0);
        assert!((c.crop.x - (1.0 - c.crop.w) / 2.0).abs() < 1e-9);
        assert!(c.z > s.z);

        // planned: both pictures, the camera reading the face file live
        let plan = planner::plan_cut(&doc, uid(&cut), uid(&framing), &Files).unwrap();
        let pl = &plan.parts[0];
        let cam = pl.layers.iter().find(|l| l.role.as_str() == "cam").unwrap();
        assert!(cam.picture_source.is_some());
        let v = serde_json::to_vec(&plan).unwrap();
        gmclip_plan::parse(&v).expect("the plan validates");
    }
}

/// One source with a screen and a cam region (an OBS composite): the bubble squares the
/// cam layer's crop; a source without a cam region has no camera layouts.
#[test]
fn builtin_layouts_from_one_sources_regions() {
    let p = open("bubble-one");
    let src = source(&p, "obs", 2560, 1440);
    let plain = source(&p, "plain", 2560, 1440);
    let mut cuts = Vec::new();
    for s in [&src, &plain] {
        let cut = p.human("cut_create", json!({"title": "obs"})).output["cut"]
            .as_str()
            .unwrap()
            .to_owned();
        p.human("caption_set", json!({"cut": cut, "enabled": false}));
        let framing = id(&p.human(
            "framing_create",
            json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
        ));
        p.human(
            "layer_add",
            json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 0.75, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
        );
        if *s == src {
            p.human(
                "layer_add",
                json!({"owner": {"framing": framing}, "role": {"region": "cam"}, "crop": {"x": 0.8, "y": 0.4, "w": 0.2, "h": 0.3}, "place": {"x": 0, "y": 0.6, "w": 1}, "z": 1}),
            );
        }
        p.human(
            "part_add",
            json!({"cut": cut, "media": {"source": s}, "src_in": 0, "src_out": 2 * SEC}),
        );
        cuts.push((cut, framing));
    }
    let (cut, framing) = &cuts[0];
    let made = p
        .human(
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": "Screen + camera bubble (top left)", "size": 0.3, "shape": "rounded"}),
        )
        .output;
    assert_eq!(made["layers"][0]["of"], "screen");
    assert_eq!(made["layers"][1]["of"], "camera");
    let doc = p.h.snapshot().doc.clone();
    let layers = layout_layers(&doc, made["id"].as_str().unwrap());
    let s = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Screen))
        .unwrap();
    let c = layers
        .iter()
        .find(|l| l.role == LayerRole::Region(RegionRole::Cam))
        .unwrap();
    // the screen keeps the stack's screen crop
    assert_eq!(s.crop.w, 0.75);
    // the cam region (0.8, 0.4, 0.2, 0.3) of 2560×1440 is 512×432 px: squared to 432
    assert!(c.member.is_none());
    assert!((c.crop.w * 2560.0 - 432.0).abs() < 1e-6 && (c.crop.h - 0.3).abs() < 1e-12);
    assert!((c.crop.x - (0.8 + 40.0 / 2560.0)).abs() < 1e-12);
    assert_eq!(c.frame.as_ref().unwrap().radius, 0.2);
    assert!((c.place.x - 0.04).abs() < 1e-9 && (c.place.y - 43.2 / 1920.0).abs() < 1e-9);
    assert!((c.place.w - 0.3).abs() < 1e-9);
    // the plan: both layers read the part's own picture
    let plan = planner::plan_cut(&doc, uid(cut), uid(framing), &Files);
    let plan = plan.unwrap();
    assert!(
        plan.parts[0]
            .layers
            .iter()
            .all(|l| l.picture_source.is_none())
    );
    // no cam region: only "Screen only" is available, the camera ones are refused
    let (cut2, framing2) = &cuts[1];
    let list = p.human("layout_builtin_list", json!({"cut": cut2})).output;
    for r in list["layouts"].as_array().unwrap() {
        assert_eq!(r["available"], r["name"] == "Screen only", "{r}");
    }
    let e = p
        .try_human(
            "layout_builtin_add",
            json!({"cut": cut2, "framing": framing2, "name": "Camera full"}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    let e = p
        .try_human(
            "layout_builtin_add",
            json!({"cut": cut, "framing": framing, "name": "Screen only", "size": 0.5}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
}
