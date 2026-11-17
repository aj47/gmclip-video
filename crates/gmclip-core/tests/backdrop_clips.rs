//! Decision 0028 through the commands and the planner: a framing's backdrop (a stored
//! video under every part, on the output clock, or a still) and video overlays (a span of
//! a project video or a stored video, picture-in-picture over the overlay's span).

mod common;

use common::{fixture, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use gmclip_plan::types as rp;
use serde_json::json;

const SEC: i64 = 705_600_000;

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
    fn asset_path(&self, a: &Asset) -> Option<String> {
        let ext = if a.media_type.starts_with("video/") {
            "mp4"
        } else {
            "png"
        };
        Some(format!("/assets/{}.{ext}", a.sha256))
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// The project's doc with the fixture source probed as a 2560×1440 30 fps video.
fn probed(p: &common::Project, fx: &common::Fx) -> gmclip_model::Doc {
    let mut doc = p.h.snapshot().doc.clone();
    let sid: Id<Source> = uid(&fx.source);
    let mut s = doc.get(sid).unwrap().clone();
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

#[test]
fn a_backdrop_video_plans_on_the_output_clock() {
    let p = open("backdrop");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let sha = "cd".repeat(32);
    let rain = common::id(&p.human(
        "asset_add",
        json!({"name": "rain.mp4", "sha256": sha, "size": 9_000_000, "media_type": "video/mp4",
               "probe": {"w": 1920, "h": 1080, "fps": {"num": 60, "den": 1}, "frames": 600}}),
    ));
    // a video without its probe is refused, and so is an unknown type
    assert!(
        p.try_human(
            "asset_add",
            json!({"name": "x.mp4", "sha256": "ef".repeat(32), "size": 10, "media_type": "video/mp4"})
        )
        .is_err()
    );
    assert!(
        p.try_human(
            "asset_add",
            json!({"name": "x.gif", "sha256": "ef".repeat(32), "size": 10, "media_type": "image/gif"})
        )
        .is_err()
    );
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "background": {"backdrop": {"asset": rain, "loop": true, "fit": "cover"}}}),
    );
    // stretch is refused (a picture keeps its own aspect)
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing,
        "background": {"backdrop": {"asset": rain, "loop": true, "fit": "stretch"}}})
        )
        .is_err()
    );
    let doc = probed(&p, &fx);
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let rp::Background::Backdrop {
        crop,
        place,
        video: Some(v),
        image: None,
        ..
    } = &plan.background
    else {
        panic!("{:?}", plan.background);
    };
    // cover: the 16:9 picture's centre 9:16 strip fills the canvas
    assert!((crop.w - 607.5).abs() < 1e-9 && (crop.x - 656.25).abs() < 1e-9);
    assert_eq!((place.w, place.h), (1080.0, 1920.0));
    // 600 frames at 60 fps = 300 output frames at 30, read by time, looping
    assert_eq!(v.fps, Some(rp::Rational { num: 60, den: 1 }));
    assert_eq!(
        (v.frames, v.r#loop, v.first_frame),
        (Some(300), Some(true), 0)
    );
    let src = plan.sources.iter().find(|s| s.id == v.source).unwrap();
    assert_eq!(src.path, format!("/assets/{sha}.mp4"));
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
    // the same file as a video overlay too: one source for both
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "video", "start": {"part_start": {"part": fx.parts[0]}}, "end": {"dur_ms": 1000},
        "place": {"x": 0.1, "y": 0.1, "w": 0.3}, "video": {"media": {"asset": rain}}}),
    ));
    let both = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(both.sources.len(), plan.sources.len());
    assert_eq!(
        both.clip_events.as_ref().unwrap()[0].picture.source,
        v.source
    );
    assert!(gmclip_plan::check_plan(&both).is_empty());
    p.human("overlay_delete", json!({"overlay": ov}));
    // the parts and their audio are as without a backdrop
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "background": {"solid": {"colour": "#0c1118"}}}),
    );
    let plain =
        planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plain.schema, SchemaVersion::V0);
    assert_eq!(plain.parts, plan.parts);
}

#[test]
fn a_still_backdrop_is_contained_or_covered() {
    let p = open("still-backdrop");
    let fx = fixture(&p, 1);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let plate = common::id(&p.human(
        "asset_add",
        json!({"name": "plate.png", "sha256": "12".repeat(32), "size": 5000, "media_type": "image/png",
               "probe": {"w": 1200, "h": 1600}}),
    ));
    let bare = common::id(&p.human(
        "asset_add",
        json!({"name": "logo.png", "sha256": "34".repeat(32), "size": 50, "media_type": "image/png"}),
    ));
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing,
        "background": {"backdrop": {"asset": bare, "loop": false, "fit": "cover"}}})
        )
        .is_err(),
        "a still needs its size"
    );
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "background": {"backdrop": {"asset": plate, "loop": false, "fit": "contain"}}}),
    );
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let rp::Background::Backdrop {
        crop,
        place,
        video: None,
        image: Some(img),
        ..
    } = &plan.background
    else {
        panic!("{:?}", plan.background);
    };
    // contain: 1200×1600 × 0.9 = 1080×1440, centred
    assert_eq!((crop.w, crop.h), (1200.0, 1600.0));
    assert_eq!(
        (place.x, place.y, place.w, place.h),
        (0.0, 240.0, 1080.0, 1440.0)
    );
    let images = plan.images.as_ref().unwrap();
    assert_eq!(&images[0].id, img);
    assert_eq!((images[0].w, images[0].h), (1200, 1600));
    assert!(gmclip_plan::check_plan(&plan).is_empty());
}

#[test]
fn video_overlays_plan_as_clip_events() {
    let p = open("video-overlay");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let start = json!({"part_start": {"part": fx.parts[0], "offset_ms": 500}});
    // a span of the project's own video (an earlier moment), top half-left quarter
    let fpf = SEC / 30;
    p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 2000},
        "place": {"x": 0.5, "y": 0.1, "w": 0.4},
        "video": {"media": {"span": {"media": {"source": fx.source}, "src_in": 10 * SEC, "src_out": 10 * SEC + 45 * fpf}},
                  "crop": {"x": 0, "y": 0, "w": 0.5, "h": 0.5}}}),
    );
    let clip = common::id(&p.human(
        "asset_add",
        json!({"name": "broll.mp4", "sha256": "56".repeat(32), "size": 2_000_000, "media_type": "video/mp4",
               "probe": {"w": 960, "h": 540, "fps": {"num": 30, "den": 1}, "frames": 90}}),
    ));
    p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 4000}, "z": 12,
        "place": {"x": 0.1, "y": 0.6, "w": 0.3}, "video": {"media": {"asset": clip}, "loop": true}}),
    );
    // refusals: no video, a still as a video, a span that ends before it starts, a crop
    // outside the picture
    let bad = [
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 0.3}}),
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 0.3},
               "video": {"media": {"asset": common::id(&p.human("asset_add", json!({"name": "s.png", "sha256": "78".repeat(32), "size": 9, "media_type": "image/png"})))}}}),
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 0.3},
               "video": {"media": {"span": {"media": {"source": fx.source}, "src_in": 5 * SEC, "src_out": 5 * SEC}}}}),
        json!({"cut": fx.cut, "kind": "video", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 0.3},
               "video": {"media": {"asset": clip}, "crop": {"x": 0.6, "y": 0, "w": 0.6, "h": 1}}}),
    ];
    for b in bad {
        assert!(p.try_human("overlay_add", b.clone()).is_err(), "{b}");
    }
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let ev = plan.clip_events.as_ref().unwrap();
    assert_eq!(ev.len(), 2);
    let span = ev.iter().find(|e| e.picture.first_frame == 300).unwrap();
    assert_eq!(span.out.0, [15, 75]);
    assert_eq!(
        (span.picture.frames, span.picture.fps, span.picture.r#loop),
        (Some(45), None, None)
    );
    assert_eq!((span.crop.w, span.crop.h), (1280.0, 720.0));
    assert_eq!(
        (span.place.x, span.place.y, span.place.w, span.place.h),
        (540.0, 192.0, 432.0, 243.0)
    );
    assert_eq!(
        span.layer,
        Some(10),
        "a video overlay sits at z 10 by default"
    );
    let src = plan
        .sources
        .iter()
        .find(|s| s.id == span.picture.source)
        .unwrap();
    assert_eq!(src.path, "/m/s.mkv", "the project's own video");
    let asset = ev.iter().find(|e| e.layer == Some(12)).unwrap();
    assert_eq!(
        (
            asset.picture.first_frame,
            asset.picture.frames,
            asset.picture.r#loop
        ),
        (0, Some(90), Some(true))
    );
    assert_eq!(asset.out.0, [15, 120], "clamped to the cut's end");
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );
}

/// Decision 0031: a callout pinned to a moving point (keys on the source's clock and
/// picture) plans as a RenderPlan track: each key on its output frame, at the canvas pixel
/// where the screen layer draws that point.
#[test]
fn a_pinned_callout_plans_its_track_through_the_layout() {
    let p = open("pinned");
    let fx = fixture(&p, 1);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "shape", "start": {"part_start": {"part": fx.parts[0]}}, "end": {"dur_ms": 1000},
        "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": 0.4, "y": 0.2, "w": 0.2, "h": 0.1},
        "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 7.4}}),
    ));
    // the fixture's part plays source 100–102 s; the point moves right over 0.5 s
    let keys: Vec<_> = (0..16)
        .map(|i| json!({"t": 100 * SEC + i * SEC / 30, "x": 0.5 + 0.01 * i as f64, "y": 0.5}))
        .collect();
    let track = json!({"media": {"source": fx.source}, "keys": keys, "method": "apple_vision.track_object"});
    p.human("overlay_track_set", json!({"overlay": ov, "track": track}));
    // refusals: times not increasing, a point far off the picture
    let mut back = track.clone();
    back["keys"][1]["t"] = json!(99 * SEC);
    assert!(
        p.try_human("overlay_track_set", json!({"overlay": ov, "track": back}))
            .is_err()
    );
    let mut far = track.clone();
    far["keys"][0]["x"] = json!(3.0);
    assert!(
        p.try_human("overlay_track_set", json!({"overlay": ov, "track": far}))
            .is_err()
    );
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let t = &plan.tracks.as_ref().unwrap()[0];
    assert_eq!(t.kind, rp::TrackedKind::Shape);
    assert_eq!(t.event.as_str(), "shape-1");
    assert_eq!(t.keys.len(), 16);
    assert_eq!(t.keys[0].frame, 0);
    // the screen layer: crop the whole 2560×1440 picture, place x 0, y 0.13, w 1 on the
    // 1080×1920 canvas, so its height is 1 · (1080/1920) · (1440/2560) = 0.31640625
    let h = 0.316_406_25;
    assert!((t.keys[0].x - 540.0).abs() < 1e-6);
    assert!((t.keys[0].y - (0.13 + 0.5 * h) * 1920.0).abs() < 1e-6);
    assert!((t.keys[15].x - 0.65 * 1080.0).abs() < 1e-6);
    assert_eq!(t.offset(0, 15), (162, 0));
    assert_eq!(plan.schema, SchemaVersion::V1);
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    // unpinned: no track
    p.human("overlay_track_set", json!({"overlay": ov, "track": null}));
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert!(plan.tracks.is_none());
}

/// Decision 0033: a blur cover pinned to a point moving right 0.001 of the picture a frame
/// from 100 s to 103.5 s, over two takes of the video (100–102 s and 103–105 s): each
/// take's cover layer carries the point's moves through its own crop and place — the
/// first take from 0 to 64 px, the second from where the point is at 103 s (97 px) to the
/// track's end (113 px), held after it; layer_track_set refuses a layer that is not a
/// cover; unpinned, no track.
#[test]
fn a_pinned_cover_plans_its_track_on_every_take() {
    let p = open("pinned-cover");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    // over the screen (2560 px into 1080: 0.421875 canvas px a source px)
    let k = 1080.0 / 2560.0;
    let place_y = 0.13 + 576.0 * k / 1920.0;
    let cover = common::id(&p.human(
        "layer_add",
        json!({"owner": {"framing": fx.framing}, "role": {"region": "private"}, "kind": "cover",
               "crop": {"x": 0.4, "y": 0.4, "w": 0.2, "h": 0.2}, "place": {"x": 0.4, "y": place_y, "w": 0.2},
               "z": 5, "cover": {"blur": {"radius": 20}}}),
    ));
    let keys: Vec<_> = (0..=105)
        .map(|i| json!({"t": 100 * SEC + i * SEC / 30, "x": 0.5 + 0.001 * i as f64, "y": 0.5}))
        .collect();
    let track = json!({"media": {"source": fx.source}, "keys": keys, "method": "apple_vision.track_object"});
    assert!(
        p.try_human(
            "layer_track_set",
            json!({"layer": fx.screen, "track": track})
        )
        .is_err(),
        "only a cover is pinned"
    );
    p.human("layer_track_set", json!({"layer": cover, "track": track}));
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    let track_of = |i: usize| {
        plan.parts[i]
            .layers
            .iter()
            .find(|l| l.cover.is_some())
            .and_then(|l| l.track.clone())
            .expect("a pinned cover")
    };
    let (a, b) = (track_of(0), track_of(1));
    // the point starts at the cover's own centre: (0.5 − 0.4) · 2560 · k past its left
    assert!((a.x - (432.0 + 256.0 * k)).abs() < 1e-6, "{}", a.x);
    assert_eq!(a.offset(0), (0, 0));
    assert_eq!(a.offset(59), (64, 0));
    assert_eq!(b.offset(60), (97, 0));
    assert_eq!(b.offset(75), (113, 0));
    assert_eq!(b.offset(119), (113, 0), "held past the track's end");
    assert_eq!((a.keys[0].frame, b.keys[0].frame), (0, 60));
    // unpinned
    p.human("layer_track_set", json!({"layer": cover, "track": null}));
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert!(
        plan.parts
            .iter()
            .flat_map(|p| &p.layers)
            .all(|l| l.track.is_none())
    );
}

/// Decision 0033: a pinned callout over two takes of the video played out of order (103–105 s
/// first, then 100–102 s): each take plays its own keys, so the track is the point at 103 s
/// on frame 0 and at 100 s on frame 60 — none dropped for coming earlier in the video.
#[test]
fn a_pinned_callout_follows_every_take_in_play_order() {
    let p = open("pinned-takes");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    p.human("part_reorder", json!({"part": fx.parts[1]}));
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "shape", "start": {"part_start": {"part": fx.parts[1]}}, "end": {"dur_ms": 4000},
        "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": 0.4, "y": 0.2, "w": 0.2, "h": 0.1},
        "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 7.4}}),
    ));
    // x = 0.4 + 0.001 · frames since 100 s, every frame to 105 s
    let keys: Vec<_> = (0..150)
        .map(|i| json!({"t": 100 * SEC + i * SEC / 30, "x": 0.4 + 0.001 * i as f64, "y": 0.5}))
        .collect();
    let track = json!({"media": {"source": fx.source}, "keys": keys, "method": "apple_vision.track_object"});
    p.human("overlay_track_set", json!({"overlay": ov, "track": track}));
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let t = &plan.tracks.as_ref().unwrap()[0];
    assert_eq!(t.keys.len(), 120, "both takes' 60 keys");
    assert!(t.keys.windows(2).all(|w| w[0].frame < w[1].frame));
    // 1080 canvas px across the picture: frame 0 shows 103 s (key 90), frame 60 shows 100 s
    let x = |i: f64| (0.4 + 0.001 * i) * 1080.0;
    assert!((t.keys[0].x - x(90.0)).abs() < 1e-6, "{}", t.keys[0].x);
    assert!((t.keys[60].x - x(0.0)).abs() < 1e-6, "{}", t.keys[60].x);
    assert_eq!(t.offset(0, 60), (-97, 0));
    assert!(gmclip_plan::check_plan(&plan).is_empty());
}

/// A video overlay over the whole cut, cut at the edge between its two parts: each piece
/// plays on from where the cut fell and is anchored to its own part, so when the parts
/// swap the pieces swap with them (AJ 2026-10-28: "the animations did not move with them").
#[test]
fn a_video_overlay_cut_at_part_edges_moves_with_its_parts() {
    let p = open("video-overlay-split");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "video", "start": {"cut_start": {"cut": fx.cut}}, "end": {"at": {"cut_end": {"cut": fx.cut}}},
        "place": {"x": 0, "y": 0, "w": 1},
        "video": {"media": {"span": {"media": {"source": fx.source}, "src_in": 10 * SEC, "src_out": 20 * SEC}}}}),
    ));
    // refusals: a cut outside it, a video asset
    assert!(
        p.try_human(
            "overlay_split",
            json!({"overlay": ov, "at": {"cut_start": {"cut": fx.cut}}})
        )
        .is_err()
    );
    let clip = common::id(&p.human(
        "asset_add",
        json!({"name": "b.mp4", "sha256": "9a".repeat(32), "size": 9, "media_type": "video/mp4",
               "probe": {"w": 960, "h": 540, "fps": {"num": 30, "den": 1}, "frames": 90}}),
    ));
    let asset = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "video", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 3000},
        "place": {"x": 0, "y": 0, "w": 0.3}, "video": {"media": {"asset": clip}}}),
    ));
    assert!(
        p.try_human("overlay_split_parts", json!({"overlay": asset}))
            .is_err()
    );
    p.human("overlay_delete", json!({"overlay": asset}));

    let r = p.human("overlay_split_parts", json!({"overlay": ov}));
    assert_eq!(r.output["pieces"].as_array().unwrap().len(), 2);
    let spans = |p: &common::Project| {
        let plan =
            planner::plan_cut(&probed(p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
        assert!(
            gmclip_plan::check_plan(&plan).is_empty(),
            "{:?}",
            gmclip_plan::check_plan(&plan)
        );
        let mut v: Vec<_> = plan
            .clip_events
            .unwrap()
            .iter()
            .map(|e| (e.out.0, e.picture.first_frame))
            .collect();
        v.sort();
        v
    };
    assert_eq!(spans(&p), vec![([0, 60], 300), ([60, 120], 360)]);
    p.human("part_reorder", json!({"part": fx.parts[1]}));
    assert_eq!(
        spans(&p),
        vec![([0, 60], 360), ([60, 120], 300)],
        "each piece with its part"
    );

    // one cut in the middle of a part: the second piece plays on from there
    let piece = common::id(&p.human(
        "overlay_split",
        json!({"overlay": ov, "at": {"part_start": {"part": fx.parts[0], "offset_ms": 1000}}}),
    ));
    assert!(!piece.is_empty());
    assert_eq!(
        spans(&p),
        vec![([0, 60], 360), ([60, 90], 300), ([90, 120], 330)]
    );
}

/// A video overlay locked to the talk's clock (graphics rendered against the talk) plays
/// wherever the cut plays those talk times, each frame its own moment: trims, reorders and
/// splits keep it in step, with no re-laying.
#[test]
fn a_locked_video_overlay_follows_the_talk_through_edits() {
    let p = open("locked-overlay");
    let fx = fixture(&p, 3); // the talk at 100–102, 103–105 and 106–108 s, 30 fps, 2 s parts
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let track = common::id(&p.human(
        "source_add",
        json!({"path": "/m/track.mp4", "fingerprint": "tr"}),
    ));
    let span =
        json!({"span": {"media": {"source": track}, "src_in": 101 * SEC, "src_out": 104 * SEC}});
    let ov = json!({"cut": fx.cut, "kind": "video", "start": {"part_start": {"part": fx.parts[0]}}, "end": {"dur_ms": 1000},
        "place": {"x": 0.65, "y": 0.09, "w": 0.33},
        "video": {"media": span, "crop": {"x": 0, "y": 0, "w": 1, "h": 0.5}, "lock": {"media": {"source": fx.source}}}});
    p.human("overlay_add", ov.clone());
    // a lock needs a span of a video (not a stored clip)
    let clip = common::id(&p.human(
        "asset_add",
        json!({"name": "c.mp4", "sha256": "9a".repeat(32), "size": 9, "media_type": "video/mp4",
               "probe": {"w": 64, "h": 64, "fps": {"num": 30, "den": 1}, "frames": 30}}),
    ));
    let mut bad = ov.clone();
    bad["video"]["media"] = json!({"asset": clip});
    assert!(p.try_human("overlay_add", bad).is_err());

    let plan = |p: &common::Project| {
        let mut doc = probed(p, &fx);
        let tid: Id<Source> = uid(&track);
        let mut s = doc.get(tid).unwrap().clone();
        s.probe.tracks = vec![TrackProbe {
            index: 0,
            kind: TrackKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(3520),
            fps: Some(gmclip_model::Rational::new(30, 1)),
            timescale: 15360,
            gop_max_s: None,
        }];
        doc.insert_for_test(s);
        let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
        assert!(
            gmclip_plan::check_plan(&plan).is_empty(),
            "{:?}",
            gmclip_plan::check_plan(&plan)
        );
        let mut ev: Vec<([u64; 2], u64, Option<u64>)> = plan
            .clip_events
            .unwrap_or_default()
            .iter()
            .map(|e| (e.out.0, e.picture.first_frame, e.picture.frames))
            .collect();
        ev.sort();
        ev
    };
    // the span is talk 101–104 s: the second half of part 1 and the first half of part 2,
    // each on its own frames (3030 = 101 s, 3090 = 103 s), nothing over part 3
    assert_eq!(
        plan(&p),
        vec![([30, 60], 3030, Some(30)), ([60, 90], 3090, Some(30))]
    );
    // trim part 1's head to 101.5 s and move part 2 first: the graphic stays on its words
    p.human(
        "part_trim",
        json!({"part": fx.parts[0], "src_in": 101 * SEC + SEC / 2}),
    );
    p.human("part_reorder", json!({"part": fx.parts[1]}));
    assert_eq!(
        plan(&p),
        vec![([0, 30], 3090, Some(30)), ([60, 75], 3045, Some(15))]
    );
    // split part 2 (a visual split's cut): one event runs on across the join
    p.human(
        "part_split",
        json!({"part": fx.parts[1], "at": 103 * SEC + SEC / 2}),
    );
    assert_eq!(
        plan(&p),
        vec![([0, 30], 3090, Some(30)), ([60, 75], 3045, Some(15))]
    );
}
