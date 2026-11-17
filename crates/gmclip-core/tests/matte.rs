//! R.01–R.03 commands: `matte_set` (roto on a layer or one part, undoable, agent-reachable),
//! `matte_brush` (per-part strokes kept across a re-set, cleared, bounded),
//! `crop_follow_apply` (a part crop and a cut visual split in one transaction), and the
//! holder-only commands refusing in core.

mod common;

use common::*;
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn layer_of(p: &common::Project, id: &str) -> Layer {
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    doc.get(Id::<Layer>::new(Ulid::parse(id).unwrap()))
        .unwrap()
        .clone()
}

fn part_matte(p: &common::Project, part: &str, framing: &str, layer: &str) -> Option<Matte> {
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    let part = doc
        .get(Id::<Part>::new(Ulid::parse(part).unwrap()))
        .unwrap()
        .clone();
    part.look
        .get(&Id::new(Ulid::parse(framing).unwrap()))
        .and_then(|l| l.overrides.get(&Id::new(Ulid::parse(layer).unwrap())))
        .and_then(|o| o.matte.clone())
}

#[test]
fn matte_set_on_a_layer_and_a_part_is_undoable() {
    let p = open("matte-set");
    let fx = fixture(&p, 2);
    p.human("matte_set", json!({"layer": fx.cam, "matte": {"roto": {}}}));
    assert_eq!(
        layer_of(&p, &fx.cam).matte,
        Matte::Roto {
            model: "apple_vision".into(),
            soften: 2.0,
            strokes: vec![]
        }
    );
    p.human("undo", json!({}));
    assert_eq!(layer_of(&p, &fx.cam).matte, Matte::None);
    // a part's override, by an agent
    p.agent(
        "c1",
        "matte_set",
        json!({"layer": fx.cam, "part": fx.parts[1], "framing": fx.framing, "matte": {"roto": {"soften": 5}}}),
        p.as_of(),
    )
    .unwrap();
    assert!(matches!(
        part_matte(&p, &fx.parts[1], &fx.framing, &fx.cam),
        Some(Matte::Roto { soften, .. }) if soften == 5.0
    ));
    assert_eq!(part_matte(&p, &fx.parts[0], &fx.framing, &fx.cam), None);
    // bad values refused
    for bad in [
        json!({"layer": fx.cam, "matte": {"roto": {"soften": 40}}}),
        json!({"layer": fx.cam, "matte": {"roto": {"model": "mediapipe"}}}),
        json!({"layer": fx.cam, "part": fx.parts[0], "matte": "none"}),
        json!({"layer": fx.cam, "matte": {"chroma": {}}}),
    ] {
        assert!(p.try_human("matte_set", bad).is_err());
    }
}

#[test]
fn brush_strokes_apply_to_one_part_and_survive_a_reset() {
    let p = open("matte-brush");
    let fx = fixture(&p, 2);
    let stroke = json!({"mode": "subtract", "radius": 0.02, "points": [[0.85, 0.5], [0.86, 0.52]]});
    // no cut-out yet: refused
    let e = p
        .try_human(
            "matte_brush",
            json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing, "strokes": [stroke]}),
        )
        .unwrap_err();
    assert!(format!("{e:?}").contains("matte_set"), "{e:?}");
    p.human("matte_set", json!({"layer": fx.cam, "matte": {"roto": {}}}));
    let ok = p.human(
        "matte_brush",
        json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing, "strokes": [stroke, {"mode": "add", "radius": 0.05, "feather": 0, "points": [[0.9, 0.6]], "at_src": 0}]}),
    );
    assert_eq!(ok.output["strokes"], 2);
    let Some(Matte::Roto { strokes, .. }) = part_matte(&p, &fx.parts[0], &fx.framing, &fx.cam)
    else {
        panic!("part 0 has its own matte")
    };
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[0].mode, StrokeMode::Subtract);
    assert_eq!(strokes[0].feather, 0.5);
    assert_eq!(strokes[1].feather, 0.0);
    // the other part is untouched
    assert_eq!(part_matte(&p, &fx.parts[1], &fx.framing, &fx.cam), None);
    // re-setting soften on the part keeps the strokes
    p.human(
        "matte_set",
        json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing, "matte": {"roto": {"soften": 4}}}),
    );
    assert!(matches!(
        part_matte(&p, &fx.parts[0], &fx.framing, &fx.cam),
        Some(Matte::Roto { soften, strokes, .. }) if soften == 4.0 && strokes.len() == 2
    ));
    // undo, then clear
    p.human("undo", json!({}));
    p.human(
        "matte_brush",
        json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing, "clear": true}),
    );
    assert!(matches!(
        part_matte(&p, &fx.parts[0], &fx.framing, &fx.cam),
        Some(Matte::Roto { strokes, .. }) if strokes.is_empty()
    ));
    // bounds
    for bad in [
        json!({"mode": "add", "radius": 0.0, "points": [[0.5, 0.5]]}),
        json!({"mode": "add", "radius": 0.9, "points": [[0.5, 0.5]]}),
        json!({"mode": "add", "radius": 0.1, "points": []}),
        json!({"mode": "add", "radius": 0.1, "points": [[3.0, 0.5]]}),
        json!({"mode": "add", "radius": 0.1, "feather": 2, "points": [[0.5, 0.5]]}),
    ] {
        assert!(
            p.try_human(
                "matte_brush",
                json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing, "strokes": [bad]}),
            )
            .is_err()
        );
    }
    assert!(
        p.try_human(
            "matte_brush",
            json!({"layer": fx.cam, "part": fx.parts[0], "framing": fx.framing}),
        )
        .is_err()
    );
}

#[test]
fn follow_apply_sets_a_crop_and_a_split_in_one_transaction() {
    let p = open("follow");
    let fx = fixture(&p, 1);
    let part = fx.parts[0].clone();
    let t = 100 * SEC + SEC; // inside the part
    p.human(
        "crop_follow_apply",
        json!({"framing": fx.framing, "crops": [
            {"part": part, "layer": fx.screen, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}},
            {"part": part, "layer": fx.screen, "at": t, "crop": {"x": 0.4, "y": 0.2, "w": 0.5, "h": 0.5}}
        ]}),
    );
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    let pid = Id::<Part>::new(Ulid::parse(&part).unwrap());
    let splits: Vec<&VisualSplit> = doc
        .children::<VisualSplit>(pid.entity())
        .filter_map(|s| doc.get(s))
        .collect();
    assert_eq!(splits.len(), 1);
    assert_eq!(splits[0].at_src.0, t);
    assert_eq!(splits[0].transition, Transition::Cut);
    // one undo removes both
    p.human("undo", json!({}));
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    assert_eq!(doc.children::<VisualSplit>(pid.entity()).count(), 0);
    // a split outside the part is refused, and nothing is applied
    assert!(
        p.try_human(
            "crop_follow_apply",
            json!({"framing": fx.framing, "crops": [
                {"part": part, "layer": fx.screen, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}},
                {"part": part, "layer": fx.screen, "at": 0, "crop": {"x": 0.4, "y": 0.2, "w": 0.5, "h": 0.5}}
            ]}),
        )
        .is_err()
    );
}

#[test]
fn holder_commands_refuse_in_core() {
    let p = open("holder-only");
    let fx = fixture(&p, 1);
    for (c, a) in [
        ("matte_quality", json!({"cut": fx.cut})),
        ("crop_follow_action", json!({"cut": fx.cut})),
    ] {
        let e = p.try_human(c, a).unwrap_err();
        assert!(format!("{e:?}").contains("host"), "{c}: {e:?}");
    }
}
