//! `cursor_follow_apply` (SPEC §5.7 follow the cursor): moves written as eased visual
//! splits of one layer in one transaction; applying again replaces the follow (other
//! splits stay); no moves removes it; out-of-part and unordered moves are refused; the
//! host-only `crop_follow_cursor` refuses in core.

mod common;

use common::*;
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn splits(p: &common::Project, part: &str) -> Vec<VisualSplit> {
    let snap = p.h.snapshot();
    let doc = &snap.doc;
    let pid = Id::<Part>::new(Ulid::parse(part).unwrap());
    let mut v: Vec<VisualSplit> = doc
        .children::<VisualSplit>(pid.entity())
        .filter_map(|s| doc.get(s).cloned())
        .collect();
    v.sort_by_key(|s| s.at_src);
    v
}

#[test]
fn a_follow_is_eased_splits_replaced_as_a_whole() {
    let p = open("cursor-follow");
    let fx = fixture(&p, 1);
    let part = fx.parts[0].clone();
    let fpf = SEC / 30;
    let t0 = 100 * SEC; // the part is [100 s, 102 s)
    let crop = |x: f64| json!({"x": x, "y": 0.2, "w": 0.4, "h": 0.4});
    // a cut split someone made by hand stays through every follow
    p.human(
        "crop_follow_apply",
        json!({"framing": fx.framing, "crops": [
            {"part": part, "layer": fx.screen, "at": t0 + 45 * fpf, "crop": crop(0.05)}
        ]}),
    );
    p.human(
        "cursor_follow_apply",
        json!({"framing": fx.framing, "part": part, "layer": fx.screen, "ease_ms": 600, "keys": [
            {"at": t0 + fpf, "crop": crop(0.1)},
            {"at": t0 + 30 * fpf, "crop": crop(0.5)},
            {"at": t0 + 50 * fpf}
        ]}),
    );
    let s = splits(&p, &part);
    assert_eq!(s.len(), 4);
    let eased: Vec<&VisualSplit> = s
        .iter()
        .filter(|x| {
            matches!(
                x.transition,
                Transition::Ease {
                    dur_ms: 600,
                    curve: Curve::EaseInOut
                }
            )
        })
        .collect();
    assert_eq!(eased.len(), 3);
    assert!(
        eased[2].overrides.is_empty(),
        "the zoom out patches nothing"
    );
    // again with one move: the old follow goes, the hand-made split stays
    p.human(
        "cursor_follow_apply",
        json!({"framing": fx.framing, "part": part, "layer": fx.screen, "ease_ms": 400, "keys": [
            {"at": t0 + 10 * fpf, "crop": crop(0.3)}
        ]}),
    );
    let s = splits(&p, &part);
    assert_eq!(s.len(), 2);
    assert_eq!(
        s.iter().filter(|x| x.transition == Transition::Cut).count(),
        1
    );
    // ⌘Z brings the three-move follow back in one step
    p.human("undo", json!({}));
    assert_eq!(splits(&p, &part).len(), 4);
    // no moves: off
    p.human(
        "cursor_follow_apply",
        json!({"framing": fx.framing, "part": part, "layer": fx.screen, "ease_ms": 600, "keys": []}),
    );
    let s = splits(&p, &part);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].transition, Transition::Cut);
    // refused: a move outside the part, moves out of order, a bad ease, a bad crop
    for (keys, ease) in [
        (json!([{"at": t0, "crop": crop(0.1)}]), 600),
        (json!([{"at": t0 + 10 * fpf}, {"at": t0 + 5 * fpf}]), 600),
        (json!([{"at": t0 + 10 * fpf}]), 50),
        (
            json!([{"at": t0 + 10 * fpf, "crop": {"x": 0.9, "y": 0, "w": 0.5, "h": 0.5}}]),
            600,
        ),
    ] {
        assert!(
            p.try_human(
                "cursor_follow_apply",
                json!({"framing": fx.framing, "part": part, "layer": fx.screen, "ease_ms": ease, "keys": keys}),
            )
            .is_err()
        );
    }
    let e = p
        .try_human("crop_follow_cursor", json!({"cut": fx.cut, "part": part}))
        .unwrap_err();
    assert!(format!("{e:?}").contains("host"), "{e:?}");
}
