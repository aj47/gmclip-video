//! The [M1] agent lint set (SPEC §5.13) and R-LINT-2.

mod common;

use common::{SEC, fixture, open};
use gmclip_core::lint::{Level, blocks_agent_export, lint_cut};
use gmclip_core::{CallRequest, Caller};
use gmclip_model::anchor::Anchor;
use gmclip_model::entities::*;
use gmclip_model::geometry::NormRect;
use gmclip_model::{Doc, Id, Ulid};
use serde_json::{Value, json};

fn text(s: &str) -> Value {
    json!({"runs": [{"text": s}], "style": {"font": "neometric-heavy", "size": 86.0, "fill": "#ffffff", "outline": null, "shadow": null,
        "align": "centre", "max_width": null, "fit": "none", "line_height": 1.0, "letter_spacing": 0.0, "case": "as_is", "pill": null}})
}

fn rules(doc: &Doc, cut: &str) -> Vec<(String, Level)> {
    lint_cut(doc, Id::new(Ulid::parse(cut).unwrap()))
        .into_iter()
        .map(|f| (f.rule, f.level))
        .collect()
}

#[test]
fn the_agent_set() {
    let p = open("lint");
    let fx = fixture(&p, 2);
    let agent = |cmd: &str, args: Value| {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: cmd.into(),
            args,
            base: Some(gmclip_proto::Base::AsOf {
                as_of: p.h.snapshot().doc.rev(),
            }),
            request_id: None,
            caller: Caller::agent("local:claude-code", "c1"),
        })
        .unwrap()
    };
    let start = json!({"cut_start": {"cut": fx.cut}});
    // a human's number on screen is not linted
    p.human("overlay_add", json!({"cut": fx.cut, "kind": "text", "start": start, "end": {"dur_ms": 3000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9}, "text": text("Human said 42")}));
    let r = rules(&p.h.snapshot().doc, &fx.cut);
    assert!(
        !r.iter().any(|x| x.0 == "number_without_provenance"),
        "{r:?}"
    );
    // an agent's number without provenance: error; with it: fine
    agent(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "start": start, "end": {"dur_ms": 3000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9}, "text": text("My PC made this in 67 seconds")}),
    );
    let findings = lint_cut(&p.h.snapshot().doc, Id::new(Ulid::parse(&fx.cut).unwrap()));
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "number_without_provenance" && f.level == Level::Error),
        "{findings:?}"
    );
    assert!(blocks_agent_export(&findings));
    agent(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "start": start, "end": {"dur_ms": 3000}, "place": {"x": 0.5, "y": 0.2, "w": 0.9}, "text": text("DONE IN 1:07"),
            "number_provenance": [{"text": "1:07", "measurement": null, "note": "queue 0→1 to first visible frame"}]}),
    );
    let n = rules(&p.h.snapshot().doc, &fx.cut)
        .iter()
        .filter(|x| x.0 == "number_without_provenance")
        .count();
    // a note alone is the agent's own claim: still an error until a measurement backs it
    assert_eq!(n, 2, "a note-only provenance does not clear it");
    // agent-typed time, agent gain without measurement
    agent(
        "hide_roles_add",
        json!({"cut": fx.cut, "from": {"part_start": {"part": fx.parts[0], "offset_ms": 300}}, "to": {"part_end": {"part": fx.parts[0]}}, "roles": [{"region": "cam"}]}),
    );
    agent("part_set_gain", json!({"part": fx.parts[1], "gain_db": 6}));
    let r = rules(&p.h.snapshot().doc, &fx.cut);
    assert!(
        r.contains(&("agent_typed_time".into(), Level::Warning)),
        "{r:?}"
    );
    assert!(
        r.contains(&("agent_grading_unmeasured".into(), Level::Warning)),
        "{r:?}"
    );
    // a part under 0.25 s
    let src = fx.source.clone();
    p.human("part_add", json!({"cut": fx.cut, "media": {"source": src}, "src_in": 300 * SEC, "src_out": 300 * SEC + SEC / 5}));
    assert!(
        rules(&p.h.snapshot().doc, &fx.cut)
            .iter()
            .any(|x| x.0 == "part_too_short")
    );
    p.h.close().unwrap();
}

#[test]
fn a_number_that_disagrees_with_its_measurement_is_an_error() {
    use gmclip_model::fixtures as f;
    let mut d = Doc::new();
    let c = f::cut(1);
    let cut = c.id;
    d.insert_for_test(c);
    let m = Measurement {
        id: Id::new(Ulid(50)),
        kind: MeasurementKind::Interval,
        from: MeasureEnd::Anchor(Anchor::CutStart(gmclip_model::anchor::CutEdge {
            cut,
            offset_ms: None,
        })),
        to: None,
        value: 67.4,
        unit: "s".into(),
        evidence_frames: vec![],
        method: "queue to first frame".into(),
        author: "agent".into(),
    };
    d.insert_for_test(m);
    let ov = |n: u128, shown: &str| {
        let mut o: Overlay = serde_json::from_value(json!({
            "id": Id::<Overlay>::new(Ulid(n)), "parent": {"cut": cut}, "kind": "text",
            "start": {"cut_start": {"cut": cut}}, "end": {"dur_ms": 1000}, "band": null, "z": 20, "opacity": 1.0,
            "geom": {"place": {"x": 0.5, "y": 0.2, "w": 0.9}}, "per_framing": {}, "span_mode": "clamp_to_stretch",
            "text": text(&format!("DONE IN {shown}")), "counter": null, "role": null, "number_provenance": [],
        }))
        .unwrap();
        o.number_provenance = vec![NumberProvenance {
            text: shown.into(),
            measurement: Some(Id::new(Ulid(50))),
            note: None,
            spoken: None,
        }];
        o
    };
    let mut ok = d.clone();
    ok.insert_for_test(ov(60, "1:07"));
    assert!(
        !lint_cut(&ok, cut)
            .iter()
            .any(|f| f.rule == "number_disagrees_with_provenance")
    );
    let mut bad = d.clone();
    bad.insert_for_test(ov(61, "1:08"));
    let f = lint_cut(&bad, cut);
    assert!(
        f.iter()
            .any(|f| f.rule == "number_disagrees_with_provenance" && f.level == Level::Error),
        "{f:?}"
    );
}

fn has(doc: &Doc, cut: &str, rule: &str) -> bool {
    rules(doc, cut).iter().any(|x| x.0 == rule)
}

/// A 2560×1440 probe for the fixture's source (the geometry rules need its size).
fn probed(p: &common::Project, fx: &common::Fx) -> Doc {
    let mut doc = p.h.snapshot().doc.clone();
    let sid: Id<Source> = Id::new(Ulid::parse(&fx.source).unwrap());
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
fn the_m2_core_set() {
    // duration: under the band with 2 × 2 s; inside it with 6
    let p = open("lint-m2-a");
    let fx = fixture(&p, 2);
    assert!(has(&p.h.snapshot().doc, &fx.cut, "duration_under_band"));
    let p2 = open("lint-m2-b");
    let fx2 = fixture(&p2, 6);
    let d2 = p2.h.snapshot().doc.clone();
    for r in [
        "duration_under_band",
        "duration_over_cap",
        "overlapping_source",
        "framings_fps_differ",
        "hook_outside_band",
        "overlay_zone",
        "private_region_under_text",
        "term_gate",
    ] {
        assert!(!has(&d2, &fx2.cut, r), "{r} on a clean cut");
    }
    // over the 100 s cap of a 9:16 framing
    let p3 = open("lint-m2-c");
    let fx3 = fixture(&p3, 51);
    assert!(has(&p3.h.snapshot().doc, &fx3.cut, "duration_over_cap"));
    // overlapping source: 1 s over another part fires, 0.3 s does not (appended: part one,
    // the payoff opening, may replay a later take)
    let src = json!({"source": fx2.source});
    p2.human(
        "part_add",
        json!({"cut": fx2.cut, "media": src, "after": fx2.parts[5], "src_in": 100 * SEC + SEC * 17 / 10, "src_out": 100 * SEC + 3 * SEC}),
    );
    assert!(!has(&p2.h.snapshot().doc, &fx2.cut, "overlapping_source"));
    p2.human(
        "part_add",
        json!({"cut": fx2.cut, "media": src, "after": fx2.parts[5], "src_in": 103 * SEC, "src_out": 104 * SEC}),
    );
    assert!(has(&p2.h.snapshot().doc, &fx2.cut, "overlapping_source"));
    // two framings at different rates
    p.human(
        "framing_create",
        json!({"cut": fx.cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 25, "den": 1}}),
    );
    assert!(has(&p.h.snapshot().doc, &fx.cut, "framings_fps_differ"));

    // the hook band: at y 305 it is fine, at y 700 it fires
    let p4 = open("lint-m2-d");
    let fx4 = fixture(&p4, 6);
    let start = json!({"cut_start": {"cut": fx4.cut}});
    let hook = |y: f64| {
        p4.human(
            "overlay_add",
            json!({"cut": fx4.cut, "kind": "text", "role": "text_hook", "start": start, "end": {"dur_ms": 3000},
                   "place": {"x": 0.5, "y": y / 1920.0, "w": 0.9}, "text": text("My hook")}),
        )
    };
    let h1 = hook(305.0);
    assert!(!has(&p4.h.snapshot().doc, &fx4.cut, "hook_outside_band"));
    p4.human("overlay_delete", json!({"overlay": common::id(&h1)}));
    hook(700.0);
    assert!(has(&p4.h.snapshot().doc, &fx4.cut, "hook_outside_band"));

    // the face: AJ's seat (bottom 1640) is calibrated in; a face at 1700–1900 fires
    let p5 = open("lint-m2-e");
    let fx5 = fixture(&p5, 6);
    let place_cam = |y: f64| {
        p5.human(
            "place_set",
            json!({"layer": fx5.cam, "place": {"x": 160.0 / 1080.0, "y": y / 1920.0, "w": 760.0 / 1080.0}}),
        );
    };
    // the fixture's cam crop is 0.19 × 0.24 of 2560×1440: 486 × 346 px → 760 wide is 541 tall
    place_cam(1100.0);
    assert!(!has(&probed(&p5, &fx5), &fx5.cut, "overlay_zone"));
    place_cam(1300.0);
    assert!(has(&probed(&p5, &fx5), &fx5.cut, "overlay_zone"));
    // captions in the bottom zone fire
    place_cam(1100.0);
    p5.human("caption_set", json!({"cut": fx5.cut, "enabled": true}));
    p5.human(
        "layout_edit",
        json!({"cut": fx5.cut, "ops": [{"op": "framing_set", "framing": fx5.framing, "caption_place": {"x": 0.5, "y": 1065.0 / 1920.0, "w": 0.9}}]}),
    );
    assert!(!has(&probed(&p5, &fx5), &fx5.cut, "overlay_zone"));
    p5.human(
        "layout_edit",
        json!({"cut": fx5.cut, "ops": [{"op": "framing_set", "framing": fx5.framing, "caption_place": {"x": 0.5, "y": 1700.0 / 1920.0, "w": 0.9}}]}),
    );
    assert!(has(&probed(&p5, &fx5), &fx5.cut, "overlay_zone"));

    // a private region (the chat, top right of the source) under the captions: error
    p5.human(
        "layout_edit",
        json!({"cut": fx5.cut, "ops": [{"op": "framing_set", "framing": fx5.framing, "caption_place": {"x": 0.5, "y": 1065.0 / 1920.0, "w": 0.9}}]}),
    );
    let with_chat = |rect: NormRect| {
        let mut doc = probed(&p5, &fx5);
        let lid: Id<StreamLayout> = Id::new(Ulid(77));
        doc.insert_for_test(StreamLayout {
            id: lid,
            name: "stream".into(),
            canvas_size: Canvas { w: 2560, h: 1440 },
            fingerprint: "fp".into(),
            regions: vec![StreamRegion {
                role: RegionRole::Chat,
                rect,
            }],
            audio_roles: None,
            default_template: None,
        });
        let sid: Id<Source> = Id::new(Ulid::parse(&fx5.source).unwrap());
        let mut s = doc.get(sid).unwrap().clone();
        s.stream_layout = Some(lid);
        doc.insert_for_test(s);
        doc
    };
    // the screen layer shows the whole source at y 0.13·1920 = 250, 1080 wide → 607 tall:
    // a chat at the source's bottom lands at ~800, a chat at the top at ~250; captions sit
    // at 1065 (1005–1125), over neither
    let d = with_chat(NormRect {
        x: 0.7,
        y: 0.0,
        w: 0.3,
        h: 0.3,
    });
    assert!(!has(&d, &fx5.cut, "private_region_under_text"));
    // the screen moved down so its bottom (the chat) lands under the captions
    p5.human(
        "place_set",
        json!({"layer": fx5.screen, "place": {"x": 0.0, "y": 500.0 / 1920.0, "w": 1.0}}),
    );
    let d = with_chat(NormRect {
        x: 0.0,
        y: 0.8,
        w: 1.0,
        h: 0.2,
    });
    let f = lint_cut(&d, Id::new(Ulid::parse(&fx5.cut).unwrap()));
    assert!(
        f.iter()
            .any(|x| x.rule == "private_region_under_text" && x.level == Level::Error),
        "{f:?}"
    );

    // A2.17 "Cover chat and alerts" (SPEC §5.7): the finding's one-click fix is one
    // layout_edit adding a blurred cover over the chat where it lands; applied (the
    // human's click), the lint is quiet and the cover plans as a `/1` cover layer
    let fix = f
        .iter()
        .find(|x| x.rule == "private_region_under_text")
        .and_then(|x| x.fix.clone())
        .expect("the private region names its fix");
    assert_eq!(fix.label, "Cover chat and alerts");
    assert_eq!(fix.command, "layout_edit");
    assert_eq!(
        fix.args["ops"].as_array().unwrap().len(),
        6,
        "one cover per part"
    );
    assert_eq!(fix.args["ops"][0]["kind"], "cover");
    assert_eq!(fix.args["ops"][0]["role"], json!({"region": "chat"}));
    p5.human(&fix.command, fix.args.clone());
    let chat = NormRect {
        x: 0.0,
        y: 0.8,
        w: 1.0,
        h: 0.2,
    };
    let d = with_chat(chat);
    assert!(
        !has(&d, &fx5.cut, "private_region_under_text"),
        "{:?}",
        rules(&d, &fx5.cut)
    );
    // the cover is the part's own layer, exactly over the chat: the screen (1080 wide at
    // y 500, 607.5 tall) shows the chat's band from y 986 to 1107.5
    let part: Id<Part> = Id::new(Ulid::parse(&fx5.parts[0]).unwrap());
    let fr: Id<Framing> = Id::new(Ulid::parse(&fx5.framing).unwrap());
    let st = gmclip_model::effective::effective(&d, part, fr, d.get(part).unwrap().src_in).unwrap();
    let cover = st
        .layers
        .iter()
        .find(|l| l.values.kind == LayerKind::Cover)
        .expect("a cover layer");
    assert!(cover.addition);
    let c = cover.values.crop;
    assert!(
        (c.x - chat.x).abs() + (c.y - chat.y).abs() + (c.w - chat.w).abs() + (c.h - chat.h).abs()
            < 1e-9,
        "{c:?}"
    );
    assert!(
        (cover.values.place.y * 1920.0 - 986.0).abs() < 1e-6,
        "{:?}",
        cover.values.place
    );
    assert!((cover.values.place.w - 1.0).abs() < 1e-9);
    assert_eq!(cover.values.cover, Some(CoverStyle::Blur { radius: 24.0 }));
    // the cover's style is `layer_set`'s; only a cover layer has one, and a cover needs it
    let lid_s = cover.id.0.to_string();
    p5.human(
        "layer_set",
        json!({"layer": lid_s, "cover": {"pixelate": {"block": 30.0}}}),
    );
    let e = p5
        .try_human(
            "layer_set",
            json!({"layer": fx5.screen, "cover": {"blur": {"radius": 9.0}}}),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    for bad in [
        json!({"kind": "cover"}),
        json!({"kind": "source_view", "cover": {"blur": {"radius": 9.0}}}),
        json!({"cover": {"blur": {"radius": 0.0}}}),
        json!({"cover": {"solid": {"colour": 0x0100_0000u32}}}),
    ] {
        let mut a = json!({"owner": {"framing": fx5.framing}, "role": {"region": "chat"},
                           "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}});
        for (k, v) in bad.as_object().unwrap() {
            a[k] = v.clone();
        }
        let e = p5.try_human("layer_add", a).unwrap_err();
        assert_eq!(e.code(), "Invalid", "{bad}");
    }
    p5.human(
        "layer_set",
        json!({"layer": lid_s, "cover": {"blur": {"radius": 24.0}}}),
    );
    // a cover that does not hold the whole band (half as wide) leaves the error
    let mut half = d.clone();
    let lid = cover.id;
    let mut l = half.get(lid).unwrap().clone();
    l.crop.w = 0.5;
    l.place.w = 0.5;
    half.insert_for_test(l);
    assert!(has(&half, &fx5.cut, "private_region_under_text"));
    // the plan draws it: a `/1` plan with a blur cover over the chat's canvas box
    struct NoFonts;
    impl gmclip_core::planner::PlanHost for NoFonts {
        fn font_path(&self, _: &Font) -> Option<String> {
            None
        }
    }
    // (captions off: heard captions need the transcripts this fixture has none of)
    p5.human("caption_set", json!({"cut": fx5.cut, "enabled": false}));
    let d = with_chat(chat);
    let plan =
        gmclip_core::planner::plan_cut(&d, Id::new(Ulid::parse(&fx5.cut).unwrap()), fr, &NoFonts)
            .expect("plans");
    assert_eq!(
        gmclip_core::planner::schema_for(&plan),
        gmclip_plan::consts::SchemaVersion::V1
    );
    let pc = plan.parts[0]
        .layers
        .iter()
        .find(|l| l.cover.is_some())
        .expect("a cover in the plan");
    assert_eq!(pc.role.as_str(), "chat");
    assert_eq!(
        pc.cover,
        Some(gmclip_plan::types::LayerCover::Blur { radius: 24.0 })
    );
    assert!(
        (pc.place.y - 986.0).abs() < 1e-6 && (pc.place.h - 121.5).abs() < 1e-6,
        "{:?}",
        pc.place
    );
    assert!((pc.crop.y - 1152.0).abs() < 1e-6 && (pc.crop.h - 288.0).abs() < 1e-6);
}

#[test]
fn the_term_gate_in_lint() {
    use gmclip_core::analysis::{AnalysisView, Transcript, Word};
    let p = open("lint-m2-terms");
    let fx = fixture(&p, 6);
    p.human("caption_set", json!({"cut": fx.cut, "enabled": true}));
    p.human(
        "term_set",
        json!({"written": "exe.dev", "heard_as": ["XD.dev"]}),
    );
    let w = |t: &str, s: f64| Word {
        text: t.into(),
        start: (s * SEC as f64) as i64,
        end: ((s + 0.3) * SEC as f64) as i64,
        confidence: None,
    };
    let mk = |word: &str| {
        let mut v = AnalysisView::default();
        v.insert(
            &fx.source,
            Transcript {
                revision: "r1".into(),
                words: vec![w("deployed", 100.2), w("on", 100.6), w(word, 100.9)],
                lines: vec![],
            },
        );
        v
    };
    let doc = p.h.snapshot().doc.clone();
    let cut = Id::new(Ulid::parse(&fx.cut).unwrap());
    let fires = |v: &AnalysisView| {
        gmclip_core::lint::lint_cut_in(&doc, Some(v), cut)
            .iter()
            .any(|f| f.rule == "term_gate")
    };
    assert!(fires(&mk("XD.dev")));
    assert!(!fires(&mk("exe.dev")));
    // A2.17's one-click fix: the suggested spelling as a caption edit, applied only when
    // the human clicks it; afterwards the gate is quiet and the audio words unchanged
    let f = gmclip_core::lint::lint_cut_in(&doc, Some(&mk("XD.dev")), cut)
        .into_iter()
        .find(|f| f.rule == "term_gate")
        .unwrap();
    let fix = f.fix.expect("the term gate names its fix");
    assert_eq!(fix.command, "cut_edit");
    assert_eq!(fix.args["ops"][0]["edit"]["text"], "exe.dev");
    assert_eq!(fix.args["ops"][0]["word"]["ordinal"], 2);
    p.human(&fix.command, fix.args.clone());
    let after = p.h.snapshot().doc.clone();
    assert!(
        !gmclip_core::lint::lint_cut_in(&after, Some(&mk("XD.dev")), cut)
            .iter()
            .any(|f| f.rule == "term_gate")
    );
    // rules the spec names no fix for carry none
    assert!(
        gmclip_core::lint::lint_cut_in(&after, Some(&mk("XD.dev")), cut)
            .iter()
            .all(|f| f.fix.is_none())
    );
}

/// R.03: `screen_text_small` (info, calibrated on AJ's goldens) fires when the screen layer shows a 2560-wide source at
/// under 0.6× in its panel, and stops when the crop zooms in.
#[test]
fn screen_text_small() {
    let p = open("lint-small");
    let c = p.human("cut_create", json!({"title": "small"}));
    let cut = c.output["cut"].as_str().unwrap().to_owned();
    let probe = json!({"duration": 1_000_000 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 2560, "height": 1440, "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": null}], "audio_tracks": []});
    let src = p.human(
        "source_add",
        json!({"path": "/m/wide.mkv", "fingerprint": "fp", "probe": probe}),
    );
    let source = src.output["id"].as_str().unwrap().to_owned();
    let framing = p.human(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    );
    let framing = framing.output["id"].as_str().unwrap().to_owned();
    let screen = p.human("layer_add", json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}));
    let screen = screen.output["id"].as_str().unwrap().to_owned();
    p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": source}, "src_in": 100 * SEC, "src_out": 112 * SEC}),
    );
    let r = rules(&p.h.snapshot().doc, &cut);
    assert!(
        r.iter()
            .any(|x| x.0 == "screen_text_small" && x.1 == Level::Info),
        "{r:?}"
    );
    // 1280 source px in the 1080 panel: 0.84×
    p.human(
        "crop_set",
        json!({"layer": screen, "crop": {"x": 0.25, "y": 0.25, "w": 0.5, "h": 0.5}}),
    );
    let r = rules(&p.h.snapshot().doc, &cut);
    assert!(!r.iter().any(|x| x.0 == "screen_text_small"), "{r:?}");
}

/// Decision 0049: a wordless part under 0.6 s from another moment of the video, between
/// two takes, flashes another pose; the fix plays on from the part before instead. One
/// that continues the part before is not flagged.
#[test]
fn part_filler_elsewhere() {
    use gmclip_core::analysis::{AnalysisView, Transcript, Word};
    let p = open("lint-filler");
    let fx = fixture(&p, 2); // 100–102 s and 103–105 s
    let s = |x: f64| (x * SEC as f64) as i64;
    let w = |t: &str, a: f64| Word {
        text: t.into(),
        start: s(a),
        end: s(a + 0.3),
        confidence: None,
    };
    let mut v = AnalysisView::default();
    v.insert(
        &fx.source,
        Transcript {
            revision: "r1".into(),
            words: vec![w("one", 100.5), w("two", 101.5), w("three", 103.5)],
            lines: vec![],
        },
    );
    let filler = |src_in: f64| {
        let ok = p.human(
            "part_add",
            json!({"cut": fx.cut, "media": {"source": fx.source}, "src_in": s(src_in), "src_out": s(src_in + 0.3), "after": fx.parts[0]}),
        );
        common::id(&ok)
    };
    let cut = Id::new(Ulid::parse(&fx.cut).unwrap());
    let found = || {
        gmclip_core::lint::lint_cut_in(&p.h.snapshot().doc, Some(&v), cut)
            .into_iter()
            .find(|f| f.rule == "part_filler_elsewhere")
    };
    // 0.3 s of 150 s between the takes
    let elsewhere = filler(150.0);
    let f = found().expect("a filler from elsewhere is flagged");
    assert_eq!(f.level, Level::Warning);
    assert_eq!(f.entity, Some(format!("part:{elsewhere}")));
    let fix = f.fix.expect("the fix plays on from the part before");
    p.human(&fix.command, fix.args.clone());
    let doc = p.h.snapshot().doc.clone();
    let part = |id: &str| doc.get(Id::<Part>::new(Ulid::parse(id).unwrap())).cloned();
    assert!(part(&elsewhere).is_none());
    let first = part(&fx.parts[0]).unwrap();
    assert!(
        (first.src_out.0 - s(102.3)).abs() <= SEC / 30,
        "{:?}",
        first.src_out
    );
    assert!(found().is_none(), "the part before now plays on");
    // a short wordless part straight after the part before continues it
    filler(102.3);
    assert!(found().is_none());
}
