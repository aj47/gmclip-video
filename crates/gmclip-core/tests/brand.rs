//! The brand kit (SPEC §8): defaults for new framings, overlay templates saved and
//! inserted, the lower third and the end card (full card, and the loop rule's "end on the
//! opening layout"), each one undo step, and all of it plans.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::types::{Shape, TextRole};
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

const FIGTREE: &str = "d32cf04815985f3cb0fa811236a1e130db44885ea981927a3237427320e851ef";

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
        fps: None,
        timescale: 15360,
        gop_max_s: None,
    }];
    doc.insert_for_test(s);
    doc
}

#[test]
fn brand_kit_defaults_templates_lower_third_and_end_card() {
    let p = open("brand");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    p.human(
        "font_add",
        json!({"family": "Figtree", "style": "Black", "sha256": FIGTREE, "em_per_size": 0.8}),
    );
    // refusals name the field
    assert!(
        p.try_human("brand_kit_set", json!({"heading_font": "Comic Sans"}))
            .is_err()
    );
    assert!(p.try_human("brand_kit_set", json!({"colours": {"primary": "blue", "accent": "#FFD400", "text": "#FFFFFF", "background": "#000000"}})).is_err());
    assert!(
        p.try_human(
            "lower_third_add",
            json!({"cut": fx.cut, "start": {"cut_start": {"cut": fx.cut}}})
        )
        .is_err(),
        "no kit yet"
    );
    let asset = id(&p.human("asset_add", json!({"name": "logo.png", "sha256": "cd".repeat(32), "size": 999, "media_type": "image/png"})));
    p.human("brand_kit_set", json!({"name": "TechFren", "handle": "@techfren", "heading_font": "Figtree Black",
        "colours": {"primary": "#0A84FF", "accent": "#FFD400", "text": "#FFFFFF", "background": "#101820"},
        "logo": {"asset": asset, "w": 512, "h": 512}, "caption_preset": "bold_pop"}));

    // a new framing takes the kit's caption preset and background
    let f2 = id(&p.human(
        "framing_create",
        json!({"cut": fx.cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    let doc = p.h.snapshot().doc.clone();
    let fr = doc.get(uid::<Framing>(&f2)).unwrap();
    assert_eq!(fr.caption_style_ref.as_deref(), Some("Cap-bold_pop"));
    assert_eq!(fr.caption_grouping, CaptionGrouping::AdaptiveWords);
    assert_eq!(
        fr.background,
        Background::Solid {
            colour: "#101820".into()
        }
    );

    // the lower third: the boxed name/handle and the logo, one undo step
    let lt = p.human("lower_third_add", json!({"cut": fx.cut, "start": {"part_start": {"part": fx.parts[0], "offset_ms": 300}}})).output;
    assert_eq!(lt["ids"].as_array().unwrap().len(), 2);
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let note = plan
        .text_events
        .iter()
        .flatten()
        .find(|e| e.role == TextRole::Note)
        .unwrap();
    assert_eq!(note.runs[0].text, "TechFren");
    assert_eq!(note.runs[1].text, "\n@techfren");
    assert_eq!(note.runs[1].fill.as_ref().unwrap().as_str(), "#FFD400FF");
    assert_eq!(note.out.0, [9, 9 + 105]);
    assert!(
        plan.text_styles.as_ref().unwrap()[&note.style]
            .text_box
            .is_some()
    );
    assert_eq!(plan.image_events.as_ref().unwrap().len(), 1);
    p.human("undo", json!({}));
    assert_eq!(p.h.snapshot().doc.count::<Overlay>(), 0);

    // a template from a callout, inserted twice
    let co = id(&p.human("overlay_add", json!({"cut": fx.cut, "kind": "shape", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 1500},
        "place": {"x": 0, "y": 0, "w": 1}, "rect": {"x": 0.1, "y": 0.3, "w": 0.5, "h": 0.2},
        "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 7.4, "radius": 12}})));
    p.human(
        "overlay_template_save",
        json!({"name": "Red box", "overlay": co}),
    );
    p.human(
        "overlay_template_insert",
        json!({"cut": fx.cut, "name": "Red box", "start": {"part_start": {"part": fx.parts[1]}}}),
    );
    assert!(
        p.try_human(
            "overlay_template_insert",
            json!({"cut": fx.cut, "name": "Nope", "start": {"cut_start": {"cut": fx.cut}}})
        )
        .is_err()
    );
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let boxes = plan.shape_events.as_ref().unwrap();
    assert_eq!(boxes.len(), 2);
    assert_eq!(
        boxes[1].out.0,
        [60, 105],
        "part 2 starts at 2 s; 1.5 s long"
    );
    assert_eq!(boxes[0].shape, boxes[1].shape);

    // the end card: the kit's background over the last 2.5 s, logo, name and handle
    p.human("end_card_add", json!({"cut": fx.cut}));
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let total = plan.output.frames;
    let card = plan
        .shape_events
        .as_ref()
        .unwrap()
        .iter()
        .find(|e| matches!(e.shape, Shape::Box { fill: Some(_), .. }))
        .unwrap();
    assert_eq!(card.out.0, [total - 75, total]);
    assert!(
        plan.image_events
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.out.0 == [total - 75, total])
    );
    assert!(
        plan.text_events
            .iter()
            .flatten()
            .any(|e| e.runs[0].text == "TechFren" && e.out.0 == [total - 75, total])
    );
    p.human("undo", json!({}));

    // the loop rule: the last part takes the first part's layout; no full card
    let lay = id(&p.human("layout_create", json!({"cut": fx.cut, "name": "panel"})));
    p.human(
        "layout_set",
        json!({"part": fx.parts[0], "framing": fx.framing, "layout": lay}),
    );
    p.human(
        "end_card_add",
        json!({"cut": fx.cut, "loop_layout": true, "dur_ms": 2000}),
    );
    let doc = p.h.snapshot().doc.clone();
    let last = doc.get(uid::<Part>(&fx.parts[2])).unwrap();
    assert_eq!(
        last.look[&uid::<Framing>(&fx.framing)].layout_ref,
        Some(uid(&lay))
    );
    let plan = planner::plan_cut(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert!(
        !plan
            .shape_events
            .iter()
            .flatten()
            .any(|e| matches!(e.shape, Shape::Box { fill: Some(_), .. })),
        "no full card"
    );
    assert!(
        plan.text_events
            .iter()
            .flatten()
            .any(|e| e.runs[0].text == "@techfren")
    );
    let _ = SEC;
}

/// Lane K5: a short whose moment comes from a stream with a long-form gets "Full video:",
/// the long-form's title, a frame of it and an arrow over its last 3 s — one undo step
/// (the frame's asset included), the kit's face and colours, its own places in 16:9.
#[test]
fn an_end_card_points_a_short_at_its_long_form() {
    let p = open("lf-card");
    const HOUR: i64 = 3600 * SEC;
    let src = id(&p.human(
        "source_add",
        json!({"path": "/rec/stream.mkv", "fingerprint": "fp",
               "probe": {"duration": HOUR, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080,
                          "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}], "audio_tracks": []}}),
    ));
    p.human("stream_edit_ensure", json!({}));
    let w = |n: i64| json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": n * SEC, "t_src_end": n * SEC + SEC / 2, "normalised_text": "w"});
    let topic = p
        .human(
            "topic_set",
            json!({"start": w(100), "end": w(400), "title": "GPU prices"}),
        )
        .output["topic"]
        .as_str()
        .unwrap()
        .to_owned();
    let long = p
        .human(
            "longform_from_topics",
            json!({"topics": [topic], "title": "I priced every GPU so you don't have to"}),
        )
        .output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    // the short: 2 parts of the same stream, 9:16 and 16:9 framings
    let short = p.human("cut_create", json!({"title": "short"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("caption_set", json!({"cut": short, "enabled": false}));
    let tall = id(&p.human(
        "framing_create",
        json!({"cut": short, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    ));
    let wide = id(&p.human(
        "framing_create",
        json!({"cut": short, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    let later = id(&p.human(
        "framing_create",
        json!({"cut": short, "name": "16:9 later", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    for f in [&tall, &wide, &later] {
        p.human("layer_add", json!({"owner": {"framing": f}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}));
    }
    for t in [150, 160] {
        p.human(
            "part_add",
            json!({"cut": short, "media": {"source": src}, "src_in": t * SEC, "src_out": (t + 5) * SEC}),
        );
    }
    let assets0 = p.h.snapshot().doc.count::<Asset>();
    // no font yet: refused
    assert!(
        p.try_human(
            "end_card_longform_add",
            json!({"cut": short, "longform": long})
        )
        .is_err()
    );
    p.human(
        "font_add",
        json!({"family": "Figtree", "style": "Black", "sha256": FIGTREE, "em_per_size": 0.8}),
    );
    // not on the long-form itself, not pointing at a short
    assert!(
        p.try_human(
            "end_card_longform_add",
            json!({"cut": long, "longform": long})
        )
        .is_err()
    );
    assert!(
        p.try_human(
            "end_card_longform_add",
            json!({"cut": short, "longform": short})
        )
        .is_err()
    );
    p.human("brand_kit_set", json!({"name": "TechFren", "handle": "@techfren", "heading_font": "Figtree Black",
        "colours": {"primary": "#0A84FF", "accent": "#FFD400", "text": "#FFFFFF", "background": "#101820"}}));
    let before = p.h.snapshot().doc.count::<Overlay>();
    let r = p
        .human(
            "end_card_longform_add",
            json!({"cut": short, "longform": long,
                   "image": {"name": "GPU prices frame", "sha256": "ab".repeat(32), "size": 4096, "w": 960, "h": 540}}),
        )
        .output;
    assert_eq!(
        r["ids"].as_array().unwrap().len(),
        4,
        "panel, words, frame, arrow"
    );
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(doc.count::<Asset>(), assets0 + 1);
    // 9:16: the card over the last 3 s (90 frames of 300)
    let plan = planner::plan_cut(&doc, uid(&short), uid(&tall), &Files).unwrap();
    let total = plan.output.frames;
    assert_eq!(total, 300);
    let words: Vec<&gmclip_plan::types::TextEvent> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.out.0 == [total - 90, total])
        .collect();
    assert_eq!(words.len(), 1);
    let runs: Vec<&str> = words[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        runs,
        ["Full video:", "\nI priced every GPU so\nyou don't have to"]
    );
    assert_eq!(
        words[0].runs[0].fill.as_ref().unwrap().as_str(),
        "#FFD400FF",
        "the kit's accent"
    );
    assert_eq!(
        plan.text_styles.as_ref().unwrap()[&words[0].style]
            .fill
            .as_ref()
            .unwrap()
            .as_str(),
        "#FFFFFFFF",
        "the kit's text colour"
    );
    let img = &plan.image_events.as_ref().unwrap()[0];
    assert_eq!(img.out.0, [total - 90, total]);
    // centred, as wide as 22 % of the height allows a 16:9 frame (751 px of 1080)
    let w = 0.22 * 1920.0 * 960.0 / 540.0;
    assert!((img.place.w - w).abs() < 1e-6, "{:?}", img.place);
    assert!((img.place.x - (1080.0 - w) / 2.0).abs() < 1e-6);
    let shapes = plan.shape_events.as_ref().unwrap();
    assert!(
        shapes
            .iter()
            .any(|e| matches!(e.shape, Shape::Arrow { .. }))
    );
    assert!(
        shapes
            .iter()
            .any(|e| matches!(e.shape, Shape::Box { fill: Some(_), .. }))
    );
    // 16:9: no arrow; the frame on the left third
    let plan = planner::plan_cut(&doc, uid(&short), uid(&wide), &Files).unwrap();
    let shapes = plan.shape_events.as_ref().unwrap();
    assert!(
        !shapes
            .iter()
            .any(|e| matches!(e.shape, Shape::Arrow { .. }))
    );
    assert_eq!(shapes.len(), 1);
    let img = &plan.image_events.as_ref().unwrap()[0];
    assert!(
        (img.place.x - 0.09 * 1920.0).abs() < 1e-6,
        "{:?}",
        img.place
    );
    assert!(
        (img.place.w - 0.32 * 1920.0).abs() < 1e-6,
        "a 16:9 frame fits 32 % wide"
    );
    // a landscape framing made after the card: laid out by the fit (one step), as the
    // first one was; nothing twice
    let doc = p.h.snapshot().doc.clone();
    let ov: Vec<&Overlay> = doc.iter::<Overlay>().collect();
    assert!(
        ov.iter()
            .all(|o| o.per_framing.contains_key(&uid::<Framing>(&wide)))
    );
    // (both landscape framings existed: both are fitted already)
    assert_eq!(
        p.human("end_card_longform_fit", json!({"cut": short}))
            .output["set"],
        0
    );
    let later2 = id(&p.human(
        "framing_create",
        json!({"cut": short, "name": "16:9 third", "w": 2560, "h": 1440, "fps": {"num": 30, "den": 1}}),
    ));
    p.human("layer_add", json!({"owner": {"framing": later2}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}));
    let plan = planner::plan_cut(
        &p.h.snapshot().doc.clone(),
        uid(&short),
        uid(&later2),
        &Files,
    )
    .unwrap();
    assert!(
        plan.shape_events
            .iter()
            .flatten()
            .any(|e| matches!(e.shape, Shape::Arrow { .. })),
        "not fitted yet"
    );
    assert_eq!(
        p.human("end_card_longform_fit", json!({"cut": short}))
            .output["set"],
        4
    );
    let plan = planner::plan_cut(
        &p.h.snapshot().doc.clone(),
        uid(&short),
        uid(&later2),
        &Files,
    )
    .unwrap();
    assert!(
        !plan
            .shape_events
            .iter()
            .flatten()
            .any(|e| matches!(e.shape, Shape::Arrow { .. }))
    );
    // back to before that framing (the fit, its layer, the framing)
    for _ in 0..3 {
        p.human("undo", json!({}));
    }
    // one undo: the card and its picture go
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(doc.count::<Overlay>(), before);
    assert_eq!(doc.count::<Asset>(), assets0);
    // without a picture: four overlays, a title given
    let r = p
        .human(
            "end_card_longform_add",
            json!({"cut": short, "longform": long, "title": "Every GPU, priced", "dur_ms": 2500}),
        )
        .output;
    assert_eq!(r["ids"].as_array().unwrap().len(), 3);
}
