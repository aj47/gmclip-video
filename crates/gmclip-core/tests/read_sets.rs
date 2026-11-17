//! A09: the read set a handler records equals the one declared for it, in 20 cases.
//! Each case runs the registered command on a tracking `Tx` over the fixture document
//! and compares `{entity}` / `{owner}/{child kind}` keys (written entities count as
//! read; entities a command creates are not reads).

mod common;

use std::collections::BTreeSet;

use common::{SEC, fixture, open};
use gmclip_core::command::{AnalysisView, Caller, Cx, Services, Tx};
use gmclip_core::registry;
use gmclip_core::services::UlidGen;
use gmclip_model::{Author, Doc, Staging};
use serde_json::{Value, json};

fn reads(doc: &Doc, cmd: &str, args: Value) -> BTreeSet<String> {
    let mut ids = UlidGen::new(Some(1));
    ids.at(common::T0 + 10_000);
    let st = Staging::new(doc.clone(), Author::Human, &mut ids);
    let caller = Caller::human();
    let analysis = AnalysisView::default();
    let mut cx = Cx {
        tx: Tx::new(st),
        caller: &caller,
        analysis: &analysis,
        services: Services { now_ms: common::T0 },
    };
    registry::find(cmd)
        .unwrap()
        .call(args, &mut cx)
        .unwrap_or_else(|e| panic!("{cmd}: {e:?}"));
    let (_, reads) = cx.tx.finish();
    reads
        .into_iter()
        .map(|r| match r.children {
            None => r.e.to_string(),
            Some(k) => format!("{}/{}", r.e, k.0),
        })
        .collect()
}

fn set(items: &[String]) -> BTreeSet<String> {
    items.iter().cloned().collect()
}

#[test]
fn twenty_recorded_read_sets_equal_their_declarations() {
    let p = open("reads");
    let fx = fixture(&p, 3);
    let lay = common::id(&p.human("layout_create", json!({"cut": fx.cut, "name": "panel"})));
    let ov = common::id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 100}, "place": {"x": 0, "y": 0, "w": 1}}),
    ));
    let doc = p.h.snapshot().doc.clone();
    let cd = doc
        .iter::<gmclip_model::entities::CaptionDoc>()
        .next()
        .unwrap()
        .id
        .0
        .to_string();
    let e = |k: &str, id: &str| format!("{k}:{id}");
    let ix = |k: &str, id: &str, child: &str| format!("{k}:{id}/{child}");
    let (cut, fr, src) = (fx.cut.as_str(), fx.framing.as_str(), fx.source.as_str());
    let (p0, p1, p2) = (
        fx.parts[0].as_str(),
        fx.parts[1].as_str(),
        fx.parts[2].as_str(),
    );
    let (screen, cam) = (fx.screen.as_str(), fx.cam.as_str());
    let word = json!({"asr_revision": "r1", "ordinal": 1});
    let wref = json!({"word_id": word, "t_src_start": 103 * SEC + SEC / 2, "t_src_end": 104 * SEC, "normalised_text": "w"});
    let part_children = |id: &str| vec![ix("part", id, "layer"), ix("part", id, "visual_split")];
    let stack = |part: &str| vec![ix("framing", fr, "layer"), ix("part", part, "layer")];
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "part_set_gain",
            json!({"part": p0, "gain_db": 1}),
            vec![e("part", p0)],
        ),
        (
            "part_trim",
            json!({"part": p0, "src_out": 101 * SEC}),
            vec![e("part", p0)],
        ),
        (
            "part_split",
            json!({"part": p1, "at": 104 * SEC}),
            [
                vec![e("part", p1), ix("cut", cut, "part")],
                part_children(p1),
            ]
            .concat(),
        ),
        (
            "part_reorder",
            json!({"part": p2, "after": p0}),
            vec![e("part", p2), e("part", p0), ix("cut", cut, "part")],
        ),
        (
            "part_add",
            json!({"cut": cut, "media": {"source": src}, "src_in": 0, "src_out": SEC, "after": p2}),
            // the part it follows: a new take copies its look
            vec![
                e("cut", cut),
                e("source", src),
                ix("cut", cut, "part"),
                e("part", p2),
            ],
        ),
        (
            "layout_set",
            json!({"part": p0, "framing": fr, "layout": lay}),
            vec![e("part", p0), e("framing", fr), e("layout", &lay)],
        ),
        (
            "crop_set",
            json!({"layer": screen, "crop": {"x": 0, "y": 0, "w": 0.5, "h": 0.5}}),
            vec![e("layer", screen)],
        ),
        (
            "crop_set",
            json!({"layer": screen, "part": p0, "framing": fr, "crop": {"x": 0, "y": 0, "w": 0.5, "h": 0.5}}),
            [
                vec![e("layer", screen), e("framing", fr), e("part", p0)],
                stack(p0),
            ]
            .concat(),
        ),
        (
            "place_set",
            json!({"layer": cam, "place": {"x": 0, "y": 0.6, "w": 0.5}}),
            vec![e("layer", cam)],
        ),
        (
            "place_set",
            json!({"layer": cam, "part": p1, "framing": fr, "place": {"x": 0, "y": 0.6, "w": 0.5}}),
            [
                vec![e("layer", cam), e("framing", fr), e("part", p1)],
                stack(p1),
            ]
            .concat(),
        ),
        (
            "overlay_move",
            json!({"overlay": ov, "place": {"x": 0, "y": 0, "w": 0.5}}),
            vec![e("overlay", &ov)],
        ),
        (
            "overlay_move",
            json!({"overlay": ov, "framing": fr, "place": {"x": 0, "y": 0, "w": 0.5}}),
            vec![e("overlay", &ov), e("framing", fr)],
        ),
        (
            "caption_edit",
            json!({"cut": cut, "word": word, "edit": "drop"}),
            vec![
                e("cut", cut),
                ix("cut", cut, "caption_doc"),
                e("caption_doc", &cd),
            ],
        ),
        (
            "caption_keyword_toggle",
            json!({"cut": cut, "word": word}),
            vec![
                e("cut", cut),
                ix("cut", cut, "caption_doc"),
                e("caption_doc", &cd),
            ],
        ),
        (
            "word_edge_nudge",
            json!({"cut": cut, "word": wref, "edge": "start", "delta_ms": 20}),
            vec![
                e("cut", cut),
                ix("cut", cut, "caption_doc"),
                e("caption_doc", &cd),
            ],
        ),
        (
            "visual_split_add",
            json!({"part": p0, "framing": fr, "at": 100 * SEC, "overrides": {}}),
            [
                vec![
                    e("part", p0),
                    e("framing", fr),
                    ix("part", p0, "visual_split"),
                ],
                stack(p0),
            ]
            .concat(),
        ),
        (
            "words_strike",
            json!({"cut": cut, "words": [wref]}),
            // it reads every part of the cut to find the ones that play the words
            [
                vec![
                    e("cut", cut),
                    ix("cut", cut, "part"),
                    e("part", p0),
                    e("part", p1),
                    e("part", p2),
                ],
                part_children(p1),
            ]
            .concat(),
        ),
        (
            "framing_create",
            json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
            vec![e("cut", cut)],
        ),
        (
            "layout_create",
            json!({"cut": cut, "name": "b"}),
            vec![e("cut", cut)],
        ),
        ("cut_create", json!({"title": "new"}), vec![]),
    ];
    assert_eq!(cases.len(), 20);
    for (cmd, args, want) in cases {
        let got = reads(&doc, cmd, args.clone());
        assert_eq!(got, set(&want), "{cmd} {args}");
    }
    p.h.close().unwrap();
}

/// A09: handlers do no I/O — no file, network or process API in `commands/`.
#[test]
fn command_handlers_do_no_io() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let banned = [
        "std::fs",
        "std::net",
        "std::process",
        "File::",
        "TcpStream",
        "UdpSocket",
        "Command::new",
        "std::env",
    ];
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        n += 1;
        for b in banned {
            assert!(!text.contains(b), "{} uses {b}", path.display());
        }
    }
    assert!(n >= 7);
}
