//! Decision 0045: `source_cursor_set` attaches a recording's input track and cursor
//! images to a screen video, then changes how its cursor is drawn, each as one undoable
//! step; a non-track asset, a size out of range or a change before any track is refused.

mod common;

use common::{fixture, open};
use gmclip_model::entities::{CursorClick, Source};
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn asset(p: &common::Project, name: &str, sha: char, media_type: &str) -> String {
    let out = p.human(
        "asset_add",
        json!({"name": name, "sha256": sha.to_string().repeat(64), "size": 100, "media_type": media_type}),
    );
    out.output["id"].as_str().unwrap().to_owned()
}

#[test]
fn a_recorded_cursor_is_attached_then_restyled_and_undone() {
    let p = open("sourcecursor");
    let fx = fixture(&p, 1);
    let sid: Id<Source> = Id::new(Ulid::parse(&fx.source).unwrap());
    let track = asset(&p, "screen input", 'a', "application/vnd.gmclip.input+json");
    let arrow = asset(&p, "cursor 0.png", 'b', "image/png");

    assert!(
        p.try_human(
            "source_cursor_set",
            json!({"source": fx.source, "scale": 2.0})
        )
        .is_err(),
        "no track yet"
    );
    assert!(
        p.try_human(
            "source_cursor_set",
            json!({"source": fx.source, "track": arrow})
        )
        .is_err(),
        "a PNG is not a track"
    );
    p.human(
        "source_cursor_set",
        json!({"source": fx.source, "track": track, "shapes": [arrow]}),
    );
    let doc = p.h.snapshot().doc.clone();
    let c = doc.get(sid).unwrap().cursor.clone().unwrap();
    assert!(c.visible && c.scale == 1.0 && c.shapes.len() == 1);

    p.human(
        "source_cursor_set",
        json!({"source": fx.source, "scale": 1.5, "hide_idle_ms": 2000, "clicks": true}),
    );
    let doc = p.h.snapshot().doc.clone();
    let c = doc.get(sid).unwrap().cursor.clone().unwrap();
    assert_eq!((c.scale, c.hide_idle_ms, c.clicks), (1.5, 2000, true));
    assert_eq!(c.shapes.len(), 1, "a restyle keeps the images");
    assert!(
        p.try_human(
            "source_cursor_set",
            json!({"source": fx.source, "scale": 9.0})
        )
        .is_err(),
        "too big"
    );

    p.human(
        "source_cursor_set",
        json!({"source": fx.source, "smoothness": 0.6, "click": "ripple", "click_shrink": true,
            "shake_px": 4, "loop_ms": 1500, "motion_blur": true}),
    );
    let doc = p.h.snapshot().doc.clone();
    let c = doc.get(sid).unwrap().cursor.clone().unwrap();
    assert_eq!(
        (
            c.smoothness,
            c.click,
            c.clicks,
            c.click_shrink,
            c.shake_px,
            c.loop_ms,
            c.motion_blur
        ),
        (0.6, Some(CursorClick::Ripple), true, true, 4, 1500, true),
        "the feel (decision 0046); a shown click keeps `clicks` true"
    );
    for bad in [
        json!({"smoothness": 1.5}),
        json!({"shake_px": 21}),
        json!({"loop_ms": 10_001}),
        json!({"click": "burst"}),
    ] {
        let mut a = bad.clone();
        a["source"] = json!(fx.source);
        assert!(p.try_human("source_cursor_set", a).is_err(), "{bad}");
    }
    p.human(
        "source_cursor_set",
        json!({"source": fx.source, "clicks": false}),
    );
    let doc = p.h.snapshot().doc.clone();
    let c = doc.get(sid).unwrap().cursor.clone().unwrap();
    assert_eq!(
        c.click_style(),
        CursorClick::None,
        "the older switch still turns it off"
    );
    p.human("undo", json!({}));
    p.human("undo", json!({}));

    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(doc.get(sid).unwrap().cursor.as_ref().unwrap().scale, 1.0);
    p.human(
        "source_cursor_set",
        json!({"source": fx.source, "clear": true}),
    );
    assert!(p.h.snapshot().doc.get(sid).unwrap().cursor.is_none());
}
