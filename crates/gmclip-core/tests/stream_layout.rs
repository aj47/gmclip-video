//! A2.20: `source_stream_layout_set` gives a source a stream layout (a profile preset the
//! app matched to it, SPEC §5.7.1 step 3), replacing any it had, as one undoable step;
//! a box outside the canvas, an unknown source or a canvas other than the picture's is
//! refused.

mod common;

use common::{fixture, open};
use gmclip_model::entities::{RegionRole, Source, StreamLayout};
use gmclip_model::{Id, Ulid};
use serde_json::json;

#[test]
fn a_source_gets_a_stream_layout_once_and_undo_takes_it_away() {
    let p = open("streamlayout");
    let fx = fixture(&p, 1);
    let sid: Id<Source> = Id::new(Ulid::parse(&fx.source).unwrap());
    let layout = |x: f64| {
        json!({"source": fx.source, "name": "berigame-restream-layout (profile)",
               "canvas": {"w": 2560, "h": 1440},
               "regions": [{"role": "cam", "rect": {"x": x, "y": 0.76, "w": 0.22, "h": 0.24}},
                           {"role": "screen", "rect": {"x": 0.33, "y": 0.11, "w": 0.34, "h": 0.79}}]})
    };
    p.human("source_stream_layout_set", layout(0.0));
    let doc = p.h.snapshot().doc.clone();
    let sl: &StreamLayout = doc
        .get(doc.get(sid).unwrap().stream_layout.unwrap())
        .unwrap();
    assert_eq!(sl.name, "berigame-restream-layout (profile)");
    assert_eq!((sl.canvas_size.w, sl.canvas_size.h), (2560, 1440));
    assert_eq!(sl.regions[0].role, RegionRole::Cam);
    // a second set replaces it: still one layout in the project
    p.human("source_stream_layout_set", layout(0.01));
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(doc.iter::<StreamLayout>().count(), 1);
    let sl: &StreamLayout = doc
        .get(doc.get(sid).unwrap().stream_layout.unwrap())
        .unwrap();
    assert!((sl.regions[0].rect.x - 0.01).abs() < 1e-12);
    // refusals
    let mut bad = layout(0.9);
    assert!(
        p.try_human("source_stream_layout_set", bad.clone())
            .is_err(),
        "outside"
    );
    bad = layout(0.0);
    bad["source"] = json!("01J00000000000000000000099");
    assert!(
        p.try_human("source_stream_layout_set", bad).is_err(),
        "no source"
    );
    bad = layout(0.0);
    bad["regions"] = json!([]);
    assert!(
        p.try_human("source_stream_layout_set", bad).is_err(),
        "no regions"
    );
    // undo twice: no layout at all
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert!(doc.get(sid).unwrap().stream_layout.is_none());
    assert_eq!(doc.iter::<StreamLayout>().count(), 0);
}
