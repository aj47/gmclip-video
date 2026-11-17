//! Decision 0052: the markers dropped while recording are stored on the recording's files
//! and listed where a cut plays them, on the cut's clock: through a sync group, once per
//! part though both files carry them, at a part's speed, and undone.

mod common;

use common::{SEC, id, open};
use serde_json::{Value, json};

fn source(p: &common::Project, name: &str) -> String {
    id(&p.human(
        "source_add",
        json!({"path": format!("/rec/{name}.mov"), "fingerprint": format!("fp-{name}"),
               "probe": {"duration": 600 * SEC, "tracks": [], "audio_tracks": []}}),
    ))
}

fn listed(p: &common::Project, cut: &str) -> Vec<(u64, i64, String)> {
    let out = p.human("marker_list", json!({"cut": cut})).output;
    out["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["n"].as_u64().unwrap(),
                m["cut_ms"].as_i64().unwrap(),
                m["part"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn cut(p: &common::Project, title: &str) -> String {
    p.human("cut_create", json!({"title": title})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn recording_markers_land_where_the_cut_plays_them() {
    let p = open("rec-markers");
    let face = source(&p, "face");
    let screen = source(&p, "screen");
    let group = p.human(
        "sync_group_create",
        json!({"members": [{"source": face, "role": "audio", "offset": 0}, {"source": screen, "role": "picture", "offset": 0}]}),
    ).output["group"]
        .as_str()
        .unwrap()
        .to_owned();
    let marks: Value = json!([2000, 11000, 12500, 30000]);
    for s in [&face, &screen] {
        p.human(
            "source_markers_set",
            json!({"source": s, "markers_ms": marks}),
        );
    }

    // the recording's group: 10–14 s then 1–3 s
    let c = cut(&p, "rec");
    let a = id(&p.human(
        "part_add",
        json!({"cut": c, "media": {"group": group}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
    ));
    let b = id(&p.human(
        "part_add",
        json!({"cut": c, "after": a, "media": {"group": group}, "src_in": SEC, "src_out": 3 * SEC}),
    ));
    assert_eq!(
        listed(&p, &c),
        [
            (2, 1000, a.clone()),
            (3, 2500, a.clone()),
            (1, 5000, b.clone())
        ]
    );
    let full = p.human("marker_list", json!({"cut": c})).output;
    assert_eq!(full["markers"][0]["src_ms"], 11000);

    // the screen file alone, at 2×: 10–14 s plays in 2 s
    let c2 = cut(&p, "fast");
    let f = id(&p.human(
        "part_add",
        json!({"cut": c2, "media": {"source": screen}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
    ));
    p.human("part_set_speed", json!({"part": f, "speed": 2.0}));
    assert_eq!(listed(&p, &c2), [(2, 500, f.clone()), (3, 1250, f.clone())]);

    // a cut of something else lists none
    let other = source(&p, "other");
    let c3 = cut(&p, "other");
    p.human(
        "part_add",
        json!({"cut": c3, "media": {"source": other}, "src_in": 0, "src_out": 20 * SEC}),
    );
    assert!(listed(&p, &c3).is_empty());

    // out of order or negative is refused at its index; undo takes them away
    for bad in [json!([5, 4]), json!([-1])] {
        let e = p
            .try_human(
                "source_markers_set",
                json!({"source": screen, "markers_ms": bad}),
            )
            .unwrap_err();
        assert!(format!("{e:?}").contains("/markers_ms/"), "{e:?}");
    }
    p.human(
        "source_markers_set",
        json!({"source": screen, "markers_ms": []}),
    );
    assert!(listed(&p, &c2).is_empty());
    p.human("undo", json!({}));
    assert_eq!(listed(&p, &c2).len(), 2);
}
