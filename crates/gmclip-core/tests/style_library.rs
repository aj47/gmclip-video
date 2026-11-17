//! A2.09, the rest of the SPEC §8 style library: text styles, pill presets, band sets,
//! layouts and stream layouts saved by name in the project and applied where each
//! belongs, one undo step each; and a stream layout's chat region, applied to a source,
//! is what `private_region_under_text` and its "Cover chat and alerts" fix read.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Id, Ulid};
use serde_json::{Value, json};

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

fn style(pill: bool) -> Value {
    json!({"font": "Figtree Black", "size": 50.0, "fill": "#FFFFFFFF", "outline": null,
           "shadow": null, "align": "left", "max_width": null, "fit": "none", "line_height": 1.0,
           "letter_spacing": 0.0, "case": "upper",
           "pill": if pill { json!({"on": true, "colour": "#000000", "opacity": 0.8, "pad_x": 18.0, "pad_y": 10.0, "radius": 12.0}) } else { Value::Null }})
}

/// The project's own items (built-in plates left out).
fn list(p: &common::Project, kind: Option<&str>) -> Vec<(String, String)> {
    let a = kind.map_or(json!({}), |k| json!({"kind": k}));
    p.human("style_library_list", a).output["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["builtin"] != true)
        .map(|r| {
            (
                r["kind"].as_str().unwrap().to_owned(),
                r["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn styles_pills_and_bands() {
    let p = open("stylelib-a");
    let fx = fixture(&p, 2);
    p.human(
        "style_library_save",
        json!({"name": "Hook", "item": {"text_style": style(false)}}),
    );
    p.human(
        "style_library_save",
        json!({"name": "Section", "item": {"pill_preset": style(true)}}),
    );
    // a pill preset without a pill is refused
    let e = p
        .try_human(
            "style_library_save",
            json!({"name": "Bad", "item": {"pill_preset": style(false)}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    assert_eq!(
        list(&p, None),
        [
            ("pill_preset".to_owned(), "Section".to_owned()),
            ("text_style".to_owned(), "Hook".to_owned())
        ]
    );
    assert_eq!(list(&p, Some("text_style")).len(), 1);
    // applying a text style needs its face in the project
    let to = json!({"kind": "text_style", "name": "Hook", "to": {"style": "Hook"}});
    let e = p.try_human("style_library_apply", to.clone()).unwrap_err();
    assert_eq!(e.code(), "Invalid");
    p.human(
        "font_add",
        json!({"family": "Figtree", "style": "Black", "sha256": "ab".repeat(32), "em_per_size": 0.8}),
    );
    p.human("style_library_apply", to);
    let doc = p.h.snapshot().doc.clone();
    let st = doc.iter::<Styles>().next().unwrap();
    assert_eq!(st.entries["Hook"].size, 50.0);
    // a kind that does not go there is refused
    let e = p
        .try_human(
            "style_library_apply",
            json!({"kind": "text_style", "name": "Hook", "to": {"framing": fx.framing}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    // bands: saved from a framing, applied to another, one undo step
    let bands =
        json!({"bands": {"hook": {"y0": 0.11, "y1": 0.22}, "pill": {"y0": 0.13, "y1": 0.16}}});
    p.human(
        "style_library_save",
        json!({"name": "Story bands", "item": {"bands": bands}}),
    );
    let f2 = id(&p.human(
        "framing_create",
        json!({"cut": fx.cut, "name": "4:5", "w": 1080, "h": 1350, "fps": {"num": 30, "den": 1}}),
    ));
    p.human(
        "style_library_apply",
        json!({"kind": "bands", "name": "Story bands", "to": {"framing": f2}}),
    );
    let doc = p.h.snapshot().doc.clone();
    assert_eq!(doc.get(uid::<Framing>(&f2)).unwrap().bands.bands.len(), 2);
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert!(doc.get(uid::<Framing>(&f2)).unwrap().bands.bands.is_empty());
    // saved from that framing again is the same set; removal by kind and name
    p.human(
        "style_library_save",
        json!({"name": "From 9:16", "from": {"framing": fx.framing}}),
    );
    assert!(list(&p, Some("bands")).contains(&("bands".to_owned(), "From 9:16".to_owned())));
    p.human(
        "style_library_save",
        json!({"name": "From 9:16", "kind": "bands"}),
    );
    assert!(!list(&p, Some("bands")).contains(&("bands".to_owned(), "From 9:16".to_owned())));
    // a bad band set is refused
    let e = p
        .try_human(
            "style_library_save",
            json!({"name": "x", "item": {"bands": {"bands": {"a": {"y0": 0.5, "y1": 0.4}}}}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
}

#[test]
fn a_layout_round_trips_through_the_library() {
    let p = open("stylelib-b");
    let fx = fixture(&p, 3);
    let lay = id(&p.human(
        "layout_create",
        json!({"cut": fx.cut, "name": "split", "caption_place": {"x": 0.5, "y": 0.55, "w": 0.9}}),
    ));
    p.human(
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "cam"}, "crop": {"x": 0.8, "y": 0.0, "w": 0.2, "h": 0.2},
               "place": {"x": 0.0, "y": 0.0, "w": 1.0}}),
    );
    p.human(
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "screen"}, "crop": {"x": 0.0, "y": 0.0, "w": 0.5, "h": 0.5},
               "place": {"x": 0.0, "y": 0.3, "w": 1.0}, "z": 1}),
    );
    p.human(
        "style_library_save",
        json!({"name": "Split", "from": {"layout": lay}}),
    );
    // applied: a new layout with the same layers, linked to parts 2 and 3 — one undo step
    let made = p.human(
        "style_library_apply",
        json!({"kind": "layout", "name": "Split",
               "to": {"cut": {"cut": fx.cut, "framing": fx.framing, "parts": [fx.parts[1], fx.parts[2]]}}}),
    );
    let new = made.output["id"].as_str().unwrap().to_owned();
    assert_ne!(new, lay);
    let doc: Doc = p.h.snapshot().doc.clone();
    let fr = uid::<Framing>(&fx.framing);
    let vals = |d: &Doc, part: &str| {
        let pid = uid::<Part>(part);
        gmclip_model::effective::effective(d, pid, fr, d.get(pid).unwrap().src_in)
            .unwrap()
            .values()
    };
    let old = doc.get(uid::<Layout>(&lay)).unwrap().clone();
    let l2 = doc.get(uid::<Layout>(&new)).unwrap();
    assert_eq!(l2.caption_place, old.caption_place);
    assert_eq!(l2.name, "Split");
    assert_eq!(vals(&doc, &fx.parts[1]).len(), 2);
    assert_eq!(vals(&doc, &fx.parts[1]), vals(&doc, &fx.parts[2]));
    // the same stack as linking the original layout
    let mut direct = doc.clone();
    let mut pt = direct.get(uid::<Part>(&fx.parts[0])).unwrap().clone();
    pt.look.entry(fr).or_default().layout_ref = Some(uid(&lay));
    direct.insert_for_test(pt);
    assert_eq!(vals(&direct, &fx.parts[0]), vals(&doc, &fx.parts[1]));
    // part one keeps the framing's defaults
    assert_eq!(vals(&doc, &fx.parts[0]).len(), 2);
    assert_ne!(vals(&doc, &fx.parts[0]), vals(&doc, &fx.parts[1]));
    p.human("undo", json!({}));
    let doc = p.h.snapshot().doc.clone();
    assert!(doc.get(uid::<Layout>(&new)).is_none(), "one undo step");
    // parts need the framing to link in
    let e = p
        .try_human(
            "style_library_apply",
            json!({"kind": "layout", "name": "Split", "to": {"cut": {"cut": fx.cut, "parts": [fx.parts[1]]}}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    // an item from elsewhere (the app-wide library) applies without being saved here
    let item = json!({"layout": {"aspect_class": "portrait", "caption_place": null, "layers": [
        {"kind": "cover", "role": {"region": "chat"}, "crop": {"x": 0.7, "y": 0.0, "w": 0.3, "h": 0.3},
         "place": {"x": 0.6, "y": 0.1, "w": 0.4}, "z": 3, "opacity": 1.0, "visible": true,
         "cover": {"pixelate": {"block": 20.0}}}]}});
    let made = p.human(
        "style_library_apply",
        json!({"kind": "layout", "name": "Mine", "item": item, "to": {"cut": {"cut": fx.cut}}}),
    );
    let doc = p.h.snapshot().doc.clone();
    let lid = uid::<Layout>(made.output["id"].as_str().unwrap());
    let layers: Vec<&Layer> = doc
        .children::<Layer>(lid.entity())
        .filter_map(|x| doc.get(x))
        .collect();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].kind, LayerKind::Cover);
    assert_eq!(layers[0].cover, Some(CoverStyle::Pixelate { block: 20.0 }));
    assert!(!list(&p, Some("layout")).iter().any(|(_, n)| n == "Mine"));
}

/// A stream layout with a chat region, applied to the source from the library, is what
/// lint reads: captions over the chat fire `private_region_under_text`, and its fix
/// covers it — all through commands, no hand-made document.
#[test]
fn a_stream_layout_feeds_the_private_region_lint_and_its_fix() {
    use gmclip_core::lint::lint_cut;
    let p = open("stylelib-c");
    let fx = fixture(&p, 2);
    p.human(
        "style_library_save",
        json!({"name": "OBS scene", "item": {"stream_layout": {"canvas_size": {"w": 2560, "h": 1440},
               "regions": [{"role": "chat", "rect": {"x": 0.0, "y": 0.8, "w": 1.0, "h": 0.2}}]}}}),
    );
    let made = p.human(
        "style_library_apply",
        json!({"kind": "stream_layout", "name": "OBS scene", "to": {"source": fx.source}}),
    );
    let sl = uid::<StreamLayout>(made.output["id"].as_str().unwrap());
    // the screen at y 500, captions at 1065: the chat lands under them
    p.human(
        "place_set",
        json!({"layer": fx.screen, "place": {"x": 0.0, "y": 500.0 / 1920.0, "w": 1.0}}),
    );
    p.human("caption_set", json!({"cut": fx.cut, "enabled": true}));
    p.human(
        "layout_edit",
        json!({"cut": fx.cut, "ops": [{"op": "framing_set", "framing": fx.framing, "caption_place": {"x": 0.5, "y": 1065.0 / 1920.0, "w": 0.9}}]}),
    );
    // the geometry rules need the source's picture size
    let probed = |p: &common::Project| {
        let mut doc = p.h.snapshot().doc.clone();
        let sid = uid::<Source>(&fx.source);
        let mut s = doc.get(sid).unwrap().clone();
        assert_eq!(s.stream_layout, Some(sl));
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
    };
    let cut = uid::<Cut>(&fx.cut);
    let f = lint_cut(&probed(&p), cut);
    let hit = f
        .iter()
        .find(|x| x.rule == "private_region_under_text")
        .expect("the chat is under the captions");
    let fix = hit.fix.clone().expect("Cover chat and alerts");
    p.human(&fix.command, fix.args.clone());
    assert!(
        !lint_cut(&probed(&p), cut)
            .iter()
            .any(|x| x.rule == "private_region_under_text")
    );
    let _ = SEC;
}

/// Decision 0029: background plates are a style-library kind — the built-in ones listed,
/// one saved by its stops, either applied to a framing as its background (kept in the
/// project's library under its name, one undo step), and planned as a RenderPlan plate.
#[test]
fn plates_save_apply_and_plan() {
    let p = open("plates");
    let fx = fixture(&p, 1);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let all = p
        .human("style_library_list", json!({"kind": "plate"}))
        .output["items"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        all.iter()
            .any(|r| r["name"] == "Midnight" && r["builtin"] == true)
    );
    // a plate of my own
    p.human(
        "style_library_save",
        json!({"name": "Brand", "item": {"plate": {"stops": ["#101820", "#FEE715"]}}}),
    );
    assert!(list(&p, Some("plate")).contains(&("plate".to_owned(), "Brand".to_owned())));
    for bad in [json!(["#101820"]), json!(["#101820", "teal"])] {
        let e = p
            .try_human(
                "style_library_save",
                json!({"name": "Bad", "item": {"plate": {"stops": bad}}}),
            )
            .unwrap_err();
        assert_eq!(e.code(), "Invalid");
    }
    // a built-in applied: the framing's background, the plate kept in the project
    p.human(
        "style_library_apply",
        json!({"kind": "plate", "name": "Midnight", "to": {"framing": fx.framing}}),
    );
    let doc = p.h.snapshot().doc.clone();
    let f = doc.get(uid::<Framing>(&fx.framing)).unwrap();
    assert_eq!(
        f.background,
        Background::Plate {
            name: "Midnight".into()
        }
    );
    assert!(list(&p, Some("plate")).contains(&("plate".to_owned(), "Midnight".to_owned())));
    let plan = probed_plan(&p, &fx);
    let gmclip_plan::types::Background::Plate { stops } = &plan.background else {
        panic!("{:?}", plan.background);
    };
    assert_eq!(
        stops.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["#0c1118", "#1b2a4a"]
    );
    // a plate is RenderPlan /0 (it was always in the schema; decision 0029 draws it)
    assert_eq!(plan.schema, gmclip_plan::consts::SchemaVersion::V0);
    assert!(gmclip_plan::check_plan(&plan).is_empty());
    // my own by name through framing_set; an unknown name is refused
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "background": {"plate": {"name": "Brand"}}}),
    );
    let plan = probed_plan(&p, &fx);
    assert!(
        matches!(&plan.background, gmclip_plan::types::Background::Plate { stops } if stops[1].as_str() == "#FEE715")
    );
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing, "background": {"plate": {"name": "Nope"}}})
        )
        .is_err()
    );
}

fn probed_plan(p: &common::Project, fx: &common::Fx) -> gmclip_plan::Plan {
    struct NoFonts;
    impl gmclip_core::planner::PlanHost for NoFonts {
        fn font_path(&self, _: &Font) -> Option<String> {
            None
        }
    }
    let mut doc: Doc = p.h.snapshot().doc.clone();
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
    gmclip_core::planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &NoFonts).unwrap()
}
