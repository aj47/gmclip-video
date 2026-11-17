//! Decision 0046: sections — zooms and layouts dragged on a lane above the parts — as
//! commands (cut milliseconds in, anchors stored, cut milliseconds out; undoable; the
//! automatic zooms replaced alone) and through the planner (`/1` `zooms[]`; layout
//! sections as eased splits of the parts they cover; a cut without sections plans as
//! before).

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::CmdError;
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::anchor::Anchor;
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use gmclip_plan::types as rp;
use serde_json::{Value, json};

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

fn section(p: &common::Project, id: &str) -> Option<Section> {
    p.h.snapshot().doc.get(uid::<Section>(id)).cloned()
}

fn listed(p: &common::Project, cut: &str) -> Value {
    p.human("section_list", json!({"cut": cut})).output
}

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
    fn asset_path(&self, a: &Asset) -> Option<String> {
        Some(format!("/assets/{}", a.sha256))
    }
}

#[test]
fn a_zoom_section_is_added_listed_in_cut_ms_and_undone() {
    let p = open("sections");
    let fx = fixture(&p, 3);
    let lay = layout(&p, &fx);
    // 3 parts of 2 s: the section crosses both part edges
    let s = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1200, "end_ms": 4800, "kind": {"zoom": {}}}),
    ));
    let sec = section(&p, &s).unwrap();
    assert_eq!(sec.framing, None, "every framing");
    let SectionKind::Zoom(z) = &sec.kind else {
        panic!("{:?}", sec.kind)
    };
    assert_eq!(z, &ZoomSpec::default());
    assert_eq!(
        (z.depth, z.target, z.ease_in_ms, z.ease_out_ms, z.layer),
        (2.0, ZoomTarget::Cursor, 700, 700, ZoomLayer::Screen)
    );
    assert!(!z.motion_blur && !z.auto && z.camera_scale.is_none());
    // stored as anchors on the media, as a dragged overlay's
    assert!(matches!(sec.start, Anchor::Raw(_)), "{:?}", sec.start);
    assert!(matches!(sec.end, Anchor::Raw(_)), "{:?}", sec.end);
    let l = listed(&p, &fx.cut);
    assert_eq!(l["cut_ms"], 6000);
    assert_eq!(l["sections"][0]["id"], json!(s));
    assert_eq!(
        (&l["sections"][0]["start_ms"], &l["sections"][0]["end_ms"]),
        (&json!(1200), &json!(4800)),
        "ms → anchor → ms round trip"
    );
    assert_eq!(l["sections"][0]["kind"]["zoom"]["target"], "cursor");

    // the cut's edges are cut-edge anchors
    let edge = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "framing": fx.framing, "start_ms": 0, "end_ms": 9000,
               "kind": {"layout": {"layout": lay}}}),
    ));
    let e = section(&p, &edge).unwrap();
    assert!(matches!(e.start, Anchor::CutStart(_)) && matches!(e.end, Anchor::CutEnd(_)));
    assert!(matches!(e.kind, SectionKind::Layout { ease_ms: 400, .. }));
    let l = listed(&p, &fx.cut);
    assert_eq!(l["sections"][0]["id"], json!(edge), "by start");
    assert_eq!(
        (
            &l["sections"][0]["start_ms"],
            &l["sections"][0]["end_ms"],
            &l["sections"][0]["framing"]
        ),
        (&json!(0), &json!(6000), &json!(fx.framing))
    );

    // one undo step each
    p.human("undo", json!({}));
    assert!(section(&p, &edge).is_none());
    p.human("undo", json!({}));
    assert!(section(&p, &s).is_none());
    p.human("redo", json!({}));
    assert!(section(&p, &s).is_some());
}

/// A layout of the cut to use in a layout section.
fn layout(p: &common::Project, fx: &common::Fx) -> String {
    id(&p.human(
        "layout_create",
        json!({"cut": fx.cut, "name": "camera full"}),
    ))
}

#[test]
fn a_section_moves_with_its_media_when_a_part_is_trimmed() {
    let p = open("sections-trim");
    let fx = fixture(&p, 3);
    let s = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1200, "end_ms": 4800, "kind": {"zoom": {"depth": 3}}}),
    ));
    // the middle part loses its first half second: the end (in the last part) comes
    // 500 ms earlier on the cut, the start (in the first) stays
    p.human(
        "part_trim",
        json!({"part": fx.parts[1], "src_in": 103 * SEC + SEC / 2}),
    );
    let l = listed(&p, &fx.cut);
    assert_eq!(l["cut_ms"], 5500);
    assert_eq!(
        (&l["sections"][0]["start_ms"], &l["sections"][0]["end_ms"]),
        (&json!(1200), &json!(4300))
    );
    assert_eq!(l["sections"][0]["id"], json!(s));
}

#[test]
fn section_set_moves_an_edge_changes_the_kind_and_makes_an_auto_zoom_hand_made() {
    let p = open("sections-set");
    let fx = fixture(&p, 3);
    let ok = p.human(
        "section_auto_apply",
        json!({"cut": fx.cut, "sections": [{"start_ms": 500, "end_ms": 2500, "zoom": {"target": "focus"}}]}),
    );
    let s = ok.output["ids"][0].as_str().unwrap().to_owned();
    let sec = section(&p, &s).unwrap();
    assert!(matches!(&sec.kind, SectionKind::Zoom(z) if z.auto && z.target == ZoomTarget::Focus));
    let end_before = sec.end.clone();

    // an edge drag: only the start moves, and the zoom is hand-made from now on
    p.human("section_set", json!({"section": s, "start_ms": 900}));
    let sec = section(&p, &s).unwrap();
    assert_eq!(sec.end, end_before, "the end's anchor is kept");
    assert!(matches!(&sec.kind, SectionKind::Zoom(z) if !z.auto));
    let l = listed(&p, &fx.cut);
    assert_eq!(
        (&l["sections"][0]["start_ms"], &l["sections"][0]["end_ms"]),
        (&json!(900), &json!(2500))
    );

    // the whole kind
    p.human(
        "section_set",
        json!({"section": s, "kind": {"zoom": {"depth": 4, "target": {"point": {"x": 0.25, "y": 0.75}}, "camera_scale": 0.5, "motion_blur": true}}}),
    );
    let sec = section(&p, &s).unwrap();
    let SectionKind::Zoom(z) = &sec.kind else {
        panic!()
    };
    assert_eq!(
        (z.depth, z.target, z.camera_scale, z.motion_blur),
        (4.0, ZoomTarget::Point { x: 0.25, y: 0.75 }, Some(0.5), true)
    );
    p.human("undo", json!({}));
    assert!(matches!(&section(&p, &s).unwrap().kind, SectionKind::Zoom(z) if z.depth == 2.0));

    p.human("section_remove", json!({"section": s}));
    assert!(section(&p, &s).is_none());
    p.human("undo", json!({}));
    assert!(section(&p, &s).is_some());
}

#[test]
fn bad_sections_are_refused_at_their_field() {
    let p = open("sections-bad");
    let fx = fixture(&p, 2);
    let other = p.human("cut_create", json!({"title": "other"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_f = id(&p.human(
        "framing_create",
        json!({"cut": other, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    let cases: [(Value, &str); 7] = [
        (
            json!({"start_ms": 100, "end_ms": 50, "kind": {"zoom": {}}}),
            "/end_ms",
        ),
        (
            json!({"start_ms": 4000, "end_ms": 4500, "kind": {"zoom": {}}}),
            "/start_ms",
        ),
        (
            json!({"start_ms": -1, "end_ms": 500, "kind": {"zoom": {}}}),
            "/start_ms",
        ),
        (
            json!({"start_ms": 0, "end_ms": 500, "kind": {"zoom": {"depth": 7}}}),
            "/kind/zoom/depth",
        ),
        (
            json!({"start_ms": 0, "end_ms": 500, "kind": {"zoom": {"target": {"point": {"x": 1.5, "y": 0}}}}}),
            "/kind/zoom/point",
        ),
        (
            json!({"start_ms": 0, "end_ms": 500, "kind": {"zoom": {"camera_scale": 0.1}}}),
            "/kind/zoom",
        ),
        (
            json!({"start_ms": 0, "end_ms": 500, "framing": other_f, "kind": {"zoom": {}}}),
            "/framing",
        ),
    ];
    for (mut a, at) in cases {
        a["cut"] = json!(fx.cut);
        match p.try_human("section_add", a) {
            Err(CmdError::Invalid { path, .. }) => assert_eq!(path, at),
            other => panic!("{at}: {other:?}"),
        }
    }
    // zoom_auto reads the input log: the host answers it
    assert!(matches!(
        p.try_human("zoom_auto", json!({"cut": fx.cut})),
        Err(CmdError::Unsupported { .. })
    ));
}

#[test]
fn zoom_auto_replaces_only_the_earlier_automatic_zooms_of_its_framing() {
    let p = open("sections-auto");
    let fx = fixture(&p, 3);
    let hand = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 0, "end_ms": 1000, "kind": {"zoom": {}}}),
    ));
    let framed = p.human(
        "section_auto_apply",
        json!({"cut": fx.cut, "framing": fx.framing, "sections": [{"start_ms": 100, "end_ms": 900, "zoom": {}}]}),
    );
    let framed = framed.output["ids"][0].as_str().unwrap().to_owned();
    let first = p.human(
        "section_auto_apply",
        json!({"cut": fx.cut, "sections": [
            {"start_ms": 1000, "end_ms": 2000, "zoom": {"target": "focus"}},
            {"start_ms": 3000, "end_ms": 4000, "zoom": {"target": "cursor"}}]}),
    );
    assert_eq!(first.output["removed"], 0);
    let again = p.human(
        "section_auto_apply",
        json!({"cut": fx.cut, "sections": [{"start_ms": 2000, "end_ms": 5000, "zoom": {"depth": 3, "auto": false}}]}),
    );
    assert_eq!(
        again.output["removed"], 2,
        "the two earlier automatic zooms"
    );
    let ids: Vec<String> = listed(&p, &fx.cut)["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    let new = again.output["ids"][0].as_str().unwrap().to_owned();
    assert_eq!(
        ids,
        [hand.clone(), framed.clone(), new.clone()],
        "hand-made and the other framing's kept"
    );
    assert!(
        matches!(&section(&p, &new).unwrap().kind, SectionKind::Zoom(z) if z.auto && z.depth == 3.0),
        "what zoom_auto writes is automatic"
    );
    // one undo step brings the earlier ones back
    p.human("undo", json!({}));
    assert_eq!(listed(&p, &fx.cut)["sections"].as_array().unwrap().len(), 4);
}

// ---------------------------------------------------------------------------------------
// The planner
// ---------------------------------------------------------------------------------------

/// The project's doc with its source probed as 2560×1440 at 30 fps.
fn probed(p: &common::Project, fx: &common::Fx) -> gmclip_model::Doc {
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
    doc.insert_for_test(s);
    doc
}

fn plan(p: &common::Project, fx: &common::Fx) -> rp::Plan {
    planner::plan_cut(&probed(p, fx), uid(&fx.cut), uid(&fx.framing), &Files).unwrap()
}

#[test]
fn a_cut_without_sections_plans_as_before() {
    let p = open("sections-plan0");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let before = serde_json::to_vec(&plan(&p, &fx)).unwrap();
    let s = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1200, "end_ms": 4800, "kind": {"zoom": {}}}),
    ));
    assert_eq!(plan(&p, &fx).schema, SchemaVersion::V1);
    p.human("section_remove", json!({"section": s}));
    let after = plan(&p, &fx);
    assert_eq!(after.schema, SchemaVersion::V0);
    assert!(after.zooms.is_none());
    assert_eq!(serde_json::to_vec(&after).unwrap(), before, "byte for byte");
    // a layout section and back
    let l = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 0, "end_ms": 1000, "kind": {"layout": {"layout": camera_full(&p, &fx)}}}),
    ));
    assert_ne!(serde_json::to_vec(&plan(&p, &fx)).unwrap(), before);
    p.human("section_remove", json!({"section": l}));
    assert_eq!(serde_json::to_vec(&plan(&p, &fx)).unwrap(), before);
}

/// A "camera full" layout: the camera over the whole width, the screen hidden.
fn camera_full(p: &common::Project, fx: &common::Fx) -> String {
    let lay = layout(p, fx);
    p.human(
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "cam"}, "crop": {"x": 0.8, "y": 0.44, "w": 0.19, "h": 0.24}, "place": {"x": 0, "y": 0.3, "w": 1}, "z": 1}),
    );
    let screen = id(&p.human(
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    ));
    p.human("layer_set", json!({"layer": screen, "visible": false}));
    lay
}

/// A "screen only" layout: the screen centred at half width; no camera layer (the part's
/// own camera is kept: the layout does not map it).
fn screen_half(p: &common::Project, fx: &common::Fx) -> String {
    let lay = layout(p, fx);
    p.human(
        "layer_add",
        json!({"owner": {"layout": lay}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0.25, "y": 0.4, "w": 0.5}}),
    );
    lay
}

fn layer<'a>(p: &'a rp::Part, id: &str) -> &'a rp::Layer {
    p.layers.iter().find(|l| l.id.as_str() == id).unwrap()
}

fn outs(pl: &rp::Plan) -> Vec<(u64, u64)> {
    pl.parts
        .iter()
        .map(|p| (p.out.start(), p.out.end()))
        .collect()
}

#[test]
fn a_layout_section_takes_its_stack_inside_eases_at_its_edges_and_leaves_the_rest() {
    let p = open("sections-layout");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let own = plan(&p, &fx);
    let lay = camera_full(&p, &fx);
    // 1.0–3.0 s at 30 fps: frames 30–90, across the edge at 60; 400 ms = 12 frames
    p.human(
        "section_add",
        json!({"cut": fx.cut, "framing": fx.framing, "start_ms": 1000, "end_ms": 3000,
               "kind": {"layout": {"layout": lay}}}),
    );
    let pl = plan(&p, &fx);
    assert!(
        gmclip_plan::check_plan(&pl).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&pl)
    );
    assert_eq!(pl.schema, SchemaVersion::V1, "eases");
    assert_eq!(
        outs(&pl),
        [(0, 30), (30, 60), (60, 90), (90, 120), (120, 180)],
        "split at the section's edges (and the parts')"
    );
    // the plan's owners follow the split
    let owners = planner::plan_part_owners(&probed(&p, &fx), uid(&fx.cut), uid(&fx.framing));
    let parts: Vec<_> = fx.parts.iter().map(|s| uid::<Part>(s)).collect();
    assert_eq!(
        owners,
        [parts[0], parts[0], parts[1], parts[1], parts[2]],
        "plan parts by model part"
    );
    let (o_cam, o_screen) = (layer(&own.parts[0], "cam"), layer(&own.parts[0], "screen"));

    // outside: untouched (another id, the same layers and picture)
    assert_eq!(pl.parts[0].layers, own.parts[0].layers);
    assert_eq!(pl.parts[4].layers, own.parts[2].layers);
    assert_eq!(pl.parts[4].picture, own.parts[2].picture);
    assert_eq!(
        pl.parts[0].picture.first_frame,
        own.parts[0].picture.first_frame
    );

    // eases in at the start: the camera from its own place, the screen fading out
    let cam = layer(&pl.parts[1], "cam");
    let e = cam.ease.as_ref().expect("the camera eases in");
    assert_eq!((e.frames, e.curve), (12, rp::EaseCurve::EaseInOut));
    assert_eq!((e.from_crop, e.from_place), (o_cam.crop, o_cam.place));
    assert_eq!(
        (cam.place.x, cam.place.y, cam.place.w),
        (0.0, 0.3 * 1920.0, 1080.0)
    );
    let scr = layer(&pl.parts[1], "screen");
    assert_eq!(
        (scr.visible, scr.opacity),
        (None, Some(0.0)),
        "drawn, fading"
    );
    let se = scr.ease.as_ref().unwrap();
    assert_eq!(
        (se.from_opacity, se.from_place),
        (Some(1.0), o_screen.place)
    );
    assert_eq!(
        pl.parts[1].picture.first_frame,
        own.parts[0].picture.first_frame + 30
    );

    // inside, past the ease: the layout's stack, settled
    let cam2 = layer(&pl.parts[2], "cam");
    assert!(cam2.ease.is_none());
    assert_eq!(cam2.place, cam.place);
    assert_eq!(layer(&pl.parts[2], "screen").visible, Some(false));

    // eases back out at the end, to the part's own stack
    let cam3 = layer(&pl.parts[3], "cam");
    let e3 = cam3.ease.as_ref().expect("the camera eases back");
    assert_eq!((e3.frames, e3.from_place), (12, cam.place));
    assert_eq!((cam3.crop, cam3.place), (o_cam.crop, o_cam.place));
    let scr3 = layer(&pl.parts[3], "screen");
    assert_eq!((scr3.visible, scr3.opacity), (None, None));
    assert_eq!(
        scr3.ease.as_ref().unwrap().from_opacity,
        Some(0.0),
        "fades back in"
    );
}

#[test]
fn an_ease_crossing_a_part_edge_carries_on_from_where_it_was() {
    let p = open("sections-layout-edge");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let own = plan(&p, &fx);
    let lay = camera_full(&p, &fx);
    // frames 54–90: the part edge at 60 is 6 frames into the 12-frame ease
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1800, "end_ms": 3000, "kind": {"layout": {"layout": lay}}}),
    );
    let pl = plan(&p, &fx);
    assert!(gmclip_plan::check_plan(&pl).is_empty());
    assert_eq!(
        outs(&pl),
        [(0, 54), (54, 60), (60, 90), (90, 120), (120, 180)]
    );
    let a = layer(&pl.parts[1], "cam");
    let b = layer(&pl.parts[2], "cam");
    assert_eq!(a.ease.as_ref().unwrap().frames, 12);
    let e = b.ease.as_ref().expect("still easing past the edge");
    assert_eq!(e.frames, 6, "what is left of it");
    // halfway through ease-in-out: halfway between the part's own place and the layout's
    let o = layer(&own.parts[1], "cam").place;
    let mid = |x: f64, y: f64| (x + y) / 2.0;
    assert!((e.from_place.x - mid(o.x, b.place.x)).abs() < 1e-9);
    assert!((e.from_place.w - mid(o.w, b.place.w)).abs() < 1e-9);
    // the ease the plan part before draws ends where this one starts
    let (_, place, _) = a.ease.as_ref().unwrap().at(6, a.crop, a.place, 1.0);
    assert!((place.x - e.from_place.x).abs() < 1e-9 && (place.y - e.from_place.y).abs() < 1e-9);
}

#[test]
fn adjacent_layout_sections_ease_from_one_layout_to_the_next() {
    let p = open("sections-layout-next");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    let own = plan(&p, &fx);
    let full = camera_full(&p, &fx);
    let half = screen_half(&p, &fx);
    // frames 30–45 then 45–75
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1000, "end_ms": 1500, "kind": {"layout": {"layout": full}}}),
    );
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1500, "end_ms": 2500, "kind": {"layout": {"layout": half, "ease_ms": 200}}}),
    );
    let pl = plan(&p, &fx);
    assert!(
        gmclip_plan::check_plan(&pl).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&pl)
    );
    assert_eq!(
        outs(&pl),
        [(0, 30), (30, 45), (45, 60), (60, 75), (75, 120), (120, 180)]
    );
    // at 45: from the camera-full layout (its 12-frame ease done) to the next, over 6
    let full_cam = layer(&pl.parts[1], "cam").place;
    let s = layer(&pl.parts[2], "screen");
    let se = s.ease.as_ref().unwrap();
    assert_eq!(
        (se.frames, se.from_opacity),
        (6, Some(0.0)),
        "the hidden screen fades in"
    );
    assert_eq!(
        (s.place.x, s.place.w, s.visible),
        (0.25 * 1080.0, 540.0, None)
    );
    // the second layout has no camera: the part's own is kept, eased back from full
    let c = layer(&pl.parts[2], "cam");
    assert_eq!(c.place, layer(&own.parts[0], "cam").place);
    assert_eq!(c.ease.as_ref().unwrap().from_place, full_cam);
    // the next part, inside the second section: settled
    assert!(layer(&pl.parts[3], "screen").ease.is_none());
    assert_eq!(layer(&pl.parts[3], "screen").place.w, 540.0);
    // after it: the screen eases back to its own over 6 frames, then the parts are as before
    assert_eq!(
        layer(&pl.parts[4], "screen").ease.as_ref().unwrap().frames,
        6
    );
    assert_eq!(pl.parts[5].layers, own.parts[2].layers);
}

#[test]
fn a_zoom_section_plans_its_frames_layers_and_camera() {
    let p = open("sections-plan");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 1200, "end_ms": 4800,
               "kind": {"zoom": {"depth": 2.5, "camera_scale": 0.4, "ease_out_ms": 500}}}),
    );
    // another framing's zoom is not this framing's
    let other = id(&p.human(
        "framing_create",
        json!({"cut": fx.cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    p.human(
        "section_add",
        json!({"cut": fx.cut, "framing": other, "start_ms": 0, "end_ms": 6000, "kind": {"zoom": {}}}),
    );
    // a span shorter than its eases: they are shortened in proportion
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 5000, "end_ms": 5400,
               "kind": {"zoom": {"target": {"point": {"x": 0.2, "y": 0.3}}, "layer": "all", "motion_blur": true}}}),
    );
    let pl = plan(&p, &fx);
    assert_eq!(pl.schema, SchemaVersion::V1);
    assert!(
        gmclip_plan::check_plan(&pl).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&pl)
    );
    let zs = pl.zooms.as_ref().unwrap();
    assert_eq!(zs.len(), 2, "{zs:#?}");
    let z = &zs[0];
    // 30 fps: 1.2 s = frame 36, 4.8 s = frame 144; 700 ms = 21 frames, 500 ms = 15
    assert_eq!((z.out.start(), z.out.end()), (36, 144));
    assert_eq!((z.depth, z.ease_in, z.ease_out), (2.5, 21, 15));
    let parts: Vec<&str> = pl.parts.iter().map(|p| p.id.as_str()).collect();
    let refs: Vec<(&str, &str)> = z
        .layers
        .iter()
        .map(|r| (r.part.as_str(), r.layer.as_str()))
        .collect();
    assert_eq!(
        refs,
        parts.iter().map(|p| (*p, "screen")).collect::<Vec<_>>(),
        "the screen layer of every part it crosses"
    );
    assert_eq!(
        z.camera_layers
            .iter()
            .flatten()
            .map(|r| r.layer.as_str())
            .collect::<Vec<_>>(),
        ["cam", "cam", "cam"]
    );
    assert_eq!(z.camera_scale, Some(0.4));
    // no recorded cursor: it holds on the centre
    assert_eq!(z.target, rp::ZoomTarget::Point([0.5, 0.5]));
    assert_eq!(z.motion_blur, None);

    let w = &zs[1];
    assert_eq!((w.out.start(), w.out.end()), (150, 162));
    assert_eq!((w.ease_in, w.ease_out), (6, 6));
    assert_eq!(w.target, rp::ZoomTarget::Point([0.2, 0.3]));
    assert_eq!(w.motion_blur, Some(true));
    assert_eq!(
        w.layers
            .iter()
            .map(|r| r.layer.as_str())
            .collect::<Vec<_>>(),
        ["screen", "cam"],
        "all: every picture layer of the part"
    );
    assert!(w.camera_layers.is_none());
}

#[test]
fn a_zoom_on_a_recording_follows_its_recorded_cursor() {
    let p = open("sections-cursor");
    let fx = fixture(&p, 2);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 0, "end_ms": 3000, "kind": {"zoom": {"target": "focus", "layer": "cursor"}}}),
    );
    let mut doc = probed(&p, &fx);
    let asset = |id: &str, sha: &str, media_type: &str, probe: Option<(u32, u32)>| Asset {
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
    };
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
    let mut s = doc.get(uid::<Source>(&fx.source)).unwrap().clone();
    s.cursor = Some(SourceCursor::new(track.id, vec![arrow.id]));
    doc.insert_for_test(s);
    doc.insert_for_test(track);
    doc.insert_for_test(arrow);
    let pl = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    assert!(
        gmclip_plan::check_plan(&pl).is_empty(),
        "{:?}",
        gmclip_plan::check_plan(&pl)
    );
    let c = &pl.cursors.as_ref().unwrap()[0];
    let z = &pl.zooms.as_ref().unwrap()[0];
    assert_eq!(z.target, rp::ZoomTarget::Focus(c.id.clone()));
    assert_eq!((z.out.start(), z.out.end()), (0, 90));
    // `cursor`: every layer that draws the recorded cursor (both show the one source)
    assert_eq!(
        z.layers
            .iter()
            .map(|r| (r.part.as_str(), r.layer.as_str()))
            .collect::<Vec<_>>(),
        [
            ("p01", "screen"),
            ("p01", "cam"),
            ("p02", "screen"),
            ("p02", "cam")
        ]
    );
}

/// AJ 2026-10-20: a 95 s video moved in front of a recording took its zooms. A section
/// (and an overlay) stored on one video's clock stays on that video's parts when another
/// video, whose clock also has that time, plays first.
#[test]
fn a_section_stays_on_its_own_video_when_another_plays_the_same_time_first() {
    let p = open("sections-other-video");
    let fx = fixture(&p, 3);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": false}));
    // another video whose clock covers 100–110 s too, added after the three parts
    let other = id(&p.human(
        "source_add",
        json!({"path": "/m/other.mkv", "fingerprint": "fp2"}),
    ));
    let intro = id(&p.human(
        "part_add",
        json!({"cut": fx.cut, "media": {"source": other}, "src_in": 100 * SEC, "src_out": 110 * SEC,
               "after": fx.parts[2]}),
    ));
    // a zoom on the second part (cut 2.5–3.5 s) and an overlay from its start
    let s = id(&p.human(
        "section_add",
        json!({"cut": fx.cut, "start_ms": 2500, "end_ms": 3500, "kind": {"zoom": {}}}),
    ));
    let ov = id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "shape", "start": {"part_start": {"part": fx.parts[1], "offset_ms": 200}},
               "end": {"dur_ms": 500}, "place": {"x": 0.1, "y": 0.1, "w": 0.3},
               "shape": {"kind": "box", "stroke": "#FF3B30FF", "width": 0.004, "radius": 0.01},
               "rect": {"x": 0.1, "y": 0.1, "w": 0.3, "h": 0.2}}),
    ));
    // the other video first: everything of the recording moves 10 s later
    p.human("part_reorder", json!({"part": intro}));
    let l = listed(&p, &fx.cut);
    let row = l["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == json!(s))
        .unwrap()
        .clone();
    assert_eq!(
        (&row["start_ms"], &row["end_ms"]),
        (&json!(12500), &json!(13500)),
        "{l}"
    );
    let mut doc = probed(&p, &fx);
    let mut o = doc.get(uid::<Source>(&other)).unwrap().clone();
    o.probe.tracks = doc
        .get(uid::<Source>(&fx.source))
        .unwrap()
        .probe
        .tracks
        .clone();
    doc.insert_for_test(o);
    let plan = planner::plan_cut(&doc, uid(&fx.cut), uid(&fx.framing), &Files).unwrap();
    let z = &plan.zooms.as_ref().unwrap()[0];
    assert_eq!(
        (z.out.start(), z.out.end()),
        (375, 405),
        "frames 12.5–13.5 s at 30 fps"
    );
    let e = &plan.shape_events.as_ref().unwrap()[0];
    assert_eq!(
        e.out.start(),
        366,
        "the overlay {ov} at 12.2 s, on its part"
    );
}
