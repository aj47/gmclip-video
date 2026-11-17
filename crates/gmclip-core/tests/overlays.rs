//! Image and shape overlays (SPEC §5.9 [M2]) through the commands and the planner: an
//! asset, an image on it, a box and an arrow callout plan to `/1` image and shape events
//! with canvas geometry; refusals name the field.

mod common;

use common::{fixture, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use gmclip_plan::types::Shape;
use serde_json::json;

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
    fn asset_path(&self, a: &Asset) -> Option<String> {
        Some(format!("/assets/{}.png", a.sha256))
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

#[test]
fn images_and_callouts_plan_to_v1() {
    let p = open("overlays");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let sha = "ab".repeat(32);
    let asset = common::id(&p.human(
        "asset_add",
        json!({"name": "logo.png", "sha256": sha, "size": 1234, "media_type": "image/png"}),
    ));
    // the same file again is the same asset
    assert_eq!(
        common::id(&p.human(
            "asset_add",
            json!({"name": "again.png", "sha256": sha, "size": 1234, "media_type": "image/png"})
        )),
        asset
    );
    assert!(
        p.try_human(
            "asset_add",
            json!({"name": "x", "sha256": sha, "size": 1, "media_type": "image/gif"})
        )
        .is_err()
    );
    let start = json!({"part_start": {"part": fx.parts[0], "offset_ms": 500}});
    p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "image", "start": start, "end": {"dur_ms": 1000},
        "place": {"x": 0.3, "y": 0.2, "w": 0.4}, "image": {"asset": asset, "w": 400, "h": 200}}),
    );
    p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "shape", "start": start, "end": {"dur_ms": 2000},
        "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": 0.1, "y": 0.4, "w": 0.5, "h": 0.1},
        "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 7.4, "radius": 12}}),
    );
    let arrow = common::id(&p.human("overlay_add", json!({"cut": fx.cut, "kind": "shape", "start": start, "end": {"dur_ms": 2000},
        "place": {"x": 0, "y": 0, "w": 1}, "z": 30,
        "shape": {"kind": "arrow", "stroke": "#FFD400FF", "width": 11, "from": [0.8, 0.5], "to": [0.6, 0.4]}})));
    // refusals
    assert!(p.try_human("overlay_add", json!({"cut": fx.cut, "kind": "image", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 1}})).is_err());
    assert!(p.try_human("overlay_add", json!({"cut": fx.cut, "kind": "shape", "start": start, "end": {"dur_ms": 1000}, "place": {"x": 0, "y": 0, "w": 1},
        "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 7}})).is_err(), "a box needs its rect");
    assert!(p.try_human("overlay_set", json!({"overlay": arrow, "shape": {"kind": "arrow", "stroke": "red", "width": 11, "from": [0, 0], "to": [1, 1]}})).is_err());
    p.human("overlay_set", json!({"overlay": arrow, "opacity": 0.5}));

    // plan (the fixture source gets a picture size)
    let mut doc = p.h.snapshot().doc.clone();
    let sid: Id<Source> = uid(&fx.source);
    let mut s = doc.get(sid).unwrap().clone();
    s.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(2560),
        height: Some(1440),
        fps: None,
        timescale: 15360,
        gop_max_s: None,
    }];
    doc.insert_for_test(s);
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let images = plan.images.as_ref().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].path, format!("/assets/{sha}.png"));
    let ie = &plan.image_events.as_ref().unwrap()[0];
    assert_eq!(ie.out.0, [15, 45]);
    assert_eq!(
        (ie.place.x, ie.place.y, ie.place.w, ie.place.h),
        (324.0, 384.0, 432.0, 216.0)
    );
    assert_eq!(ie.layer, Some(10));
    let se = plan.shape_events.as_ref().unwrap();
    assert_eq!(se.len(), 2);
    match &se[0].shape {
        Shape::Box {
            rect,
            width,
            radius,
            ..
        } => {
            assert_eq!(
                (rect.x, rect.y, rect.w, rect.h),
                (108.0, 768.0, 540.0, 192.0)
            );
            assert!((width - 7.992).abs() < 1e-9 && (radius.unwrap() - 12.96).abs() < 1e-9);
        }
        s => panic!("{s:?}"),
    }
    match &se[1].shape {
        Shape::Arrow {
            from, to, stroke, ..
        } => {
            assert_eq!((from.x, from.y, to.x, to.y), (864.0, 960.0, 648.0, 768.0));
            assert_eq!(stroke.as_str(), "#FFD40080", "half opacity in the stroke");
        }
        s => panic!("{s:?}"),
    }
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    // no image file on this machine: the plan says so
    struct NoAssets;
    impl PlanHost for NoAssets {
        fn font_path(&self, _: &Font) -> Option<String> {
            None
        }
    }
    let e = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &NoAssets).unwrap_err();
    assert!(e.0.contains("image this machine does not have"), "{e}");
}

#[test]
fn blurred_source_background_plans_to_v1() {
    let p = open("blurfill");
    let fx = fixture(&p, 1);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing, "background": {"solid": {"colour": "teal"}}})
        )
        .is_err()
    );
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing, "background": {"plate": {"name": "rt"}}})
        )
        .is_err()
    );
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "background": "blurred_source"}),
    );
    let mut doc = p.h.snapshot().doc.clone();
    let sid: Id<Source> = uid(&fx.source);
    let mut s = doc.get(sid).unwrap().clone();
    s.probe.tracks = vec![TrackProbe {
        index: 0,
        kind: TrackKind::Video,
        codec: "h264".into(),
        width: Some(2560),
        height: Some(1440),
        fps: None,
        timescale: 15360,
        gop_max_s: None,
    }];
    doc.insert_for_test(s);
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    assert!(matches!(
        plan.background,
        gmclip_plan::types::Background::BlurredSource { blur: 24, .. }
    ));
    p.human("undo", json!({}));
    let plan = planner::plan_cut(
        &{
            let mut d = p.h.snapshot().doc.clone();
            d.insert_for_test(doc.get(sid).unwrap().clone());
            d
        },
        uid(&fx.cut),
        uid(&fx.framing),
        &Files,
    )
    .unwrap();
    assert_eq!(plan.schema, SchemaVersion::V0, "solid again: /0");
}
