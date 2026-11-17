//! Decision 0045 through the planner: a recorded screen source's cursor plans as one
//! `/1` `cursors[]` entry (its input track and images at the host's stored files), named
//! by every layer showing that source; a hidden one, or none, plans nothing and stays `/0`.

mod common;

use common::{fixture, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use serde_json::json;

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
    fn asset_path(&self, a: &Asset) -> Option<String> {
        Some(format!("/assets/{}", a.sha256))
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

fn asset(id: &str, sha: &str, media_type: &str, probe: Option<(u32, u32)>) -> Asset {
    Asset {
        id: uid(id),
        name: "a".into(),
        sha256: sha.repeat(32),
        size: 100,
        media_type: media_type.into(),
        probe: probe.map(|(w, h)| AssetProbe {
            w,
            h,
            fps: None,
            frames: None,
        }),
    }
}

/// The fixture's doc, its source probed as 2560×1440 at 30 fps, with `cursor` set on it.
fn doc_with(
    fx: &common::Fx,
    p: &common::Project,
    cursor: Option<SourceCursor>,
) -> gmclip_model::Doc {
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
    s.cursor = cursor;
    doc.insert_for_test(s);
    doc
}

#[test]
fn a_recorded_cursor_plans_once_per_source_under_v1() {
    let p = open("cursor");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let bare = doc_with(&fx, &p, None);
    let plan = planner::plan_cut(&bare, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V0, "no cursor: /0 as before");
    assert!(plan.cursors.is_none());

    let track = asset(
        "01J90000000000000000000001",
        "ab",
        gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE,
        None,
    );
    let arrow = asset(
        "01J90000000000000000000002",
        "cd",
        "image/png",
        Some((64, 64)),
    );
    let mut sc = SourceCursor::new(track.id, vec![arrow.id]);
    sc.scale = 1.5;
    sc.clicks = true;
    let mut doc = doc_with(&fx, &p, Some(sc.clone()));
    doc.insert_for_test(track.clone());
    doc.insert_for_test(arrow.clone());
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let cs = plan.cursors.as_ref().unwrap();
    assert_eq!(
        cs.len(),
        1,
        "one entry for the source, however many layers show it"
    );
    let c = &cs[0];
    assert_eq!(c.path, format!("/assets/{}", track.sha256));
    assert_eq!(c.sha256.as_str(), track.sha256);
    assert_eq!(
        (c.scale, c.smooth_ms, c.hide_idle_ms, c.clicks),
        (1.5, None, None, Some(true))
    );
    let img = plan
        .images
        .as_ref()
        .unwrap()
        .iter()
        .find(|i| i.id == c.shapes[0])
        .unwrap();
    assert_eq!(
        (img.w, img.h, img.path.as_str()),
        (64, 64, format!("/assets/{}", arrow.sha256).as_str())
    );
    let layers: Vec<_> = plan.parts.iter().flat_map(|p| &p.layers).collect();
    assert!(!layers.is_empty());
    assert!(
        layers
            .iter()
            .all(|l| l.cursor.as_ref() == Some(&c.id) || l.cover.is_some())
    );
    assert!(
        gmclip_plan::check_plan(&plan).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&plan)
    );

    // hidden: nothing planned
    sc.visible = false;
    let mut doc = doc_with(&fx, &p, Some(sc));
    doc.insert_for_test(track);
    doc.insert_for_test(arrow);
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V0);
    assert!(
        plan.parts
            .iter()
            .flat_map(|p| &p.layers)
            .all(|l| l.cursor.is_none())
    );
}

/// Decision 0046: the cursor's feel passes through to `cursors[]`, clamped to the plan's
/// ranges; a cursor without it plans the same bytes as before (no new keys).
#[test]
fn the_cursor_feel_plans_through_clamped() {
    let p = open("cursorfeel");
    let fx = fixture(&p, 1);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let track = asset(
        "01J90000000000000000000001",
        "ab",
        gmclip_model::rec::INPUT_TRACK_MEDIA_TYPE,
        None,
    );
    let arrow = asset(
        "01J90000000000000000000002",
        "cd",
        "image/png",
        Some((64, 64)),
    );
    let plan_of = |sc: SourceCursor| {
        let mut doc = doc_with(&fx, &p, Some(sc));
        doc.insert_for_test(track.clone());
        doc.insert_for_test(arrow.clone());
        planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap()
    };
    let mut sc = SourceCursor::new(track.id, vec![arrow.id]);
    sc.clicks = true;
    let plain = plan_of(sc.clone());
    let v = serde_json::to_value(&plain.cursors.as_ref().unwrap()[0]).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "clicks", "id", "path", "scale", "sha256", "shapes", "source"
        ],
        "no feel, no new keys"
    );

    sc.click = Some(CursorClick::Ripple);
    sc.smoothness = 0.6;
    sc.click_shrink = true;
    sc.shake_px = 50; // past the plan's 20
    sc.loop_ms = 1500;
    sc.motion_blur = true;
    let plan = plan_of(sc.clone());
    let c = &plan.cursors.as_ref().unwrap()[0];
    assert_eq!(c.click_style(), gmclip_plan::types::CursorClick::Ripple);
    assert_eq!(
        (
            c.clicks,
            c.smoothness,
            c.click_shrink,
            c.shake_px,
            c.loop_ms,
            c.motion_blur
        ),
        (
            Some(true),
            Some(0.6),
            Some(true),
            Some(20.0),
            Some(1500),
            Some(true)
        )
    );
    assert!(gmclip_plan::check_plan(&plan).is_empty());

    sc.click = Some(CursorClick::None);
    let c = plan_of(sc).cursors.unwrap().remove(0);
    assert_eq!((c.clicks, c.click), (None, None), "no click shown");
}
