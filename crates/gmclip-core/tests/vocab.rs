//! The completed M1 edit vocabulary through `cut_edit` / `layout_edit`, with server-side
//! anchor resolution (R-ANC-1): every op applies, says how its anchors resolved, and
//! refuses what cannot resolve.

mod common;

use common::{SEC, fixture, open};
use gmclip_core::CmdError;
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn cid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

#[test]
fn every_op_applies_and_reports_its_anchor_resolution() {
    let p = open("vocab");
    let fx = fixture(&p, 3);
    let part0 = json!({"part_start": {"part": fx.parts[0]}});
    let part0_end = json!({"part_end": {"part": fx.parts[0]}});
    let word = json!({"word_start": {"word_id": {"asr_revision": "r1", "ordinal": 4}, "t_src_start": 100 * SEC + SEC / 2, "t_src_end": 101 * SEC, "normalised_text": "banana"}});
    let ok = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [
            {"op": "gain_segment_add", "start": part0, "end": part0_end, "gain_db": 10},
        ]}),
    );
    let out = ok.unwrap().output;
    let g = &out["results"][0];
    assert_eq!(g["from"]["method"], "part_edge");
    assert_eq!(g["from"]["provenance"], "anchored");
    assert_eq!(g["from"]["cut_s"], 0.0);
    assert_eq!(g["to"]["cut_s"], 2.0);
    assert_eq!(g["to"]["src_s"], 102.0);
    let d = p.h.snapshot().doc.clone();
    assert!(d.count::<GainSegment>() >= 1);
    // a word anchor resolves from its fingerprint; a raw time needs a reason and is `raw`
    let r = p
        .human(
            "cut_edit",
            json!({"cut": fx.cut, "ops": [
                {"op": "content_hold_add", "from": word, "to": part0_end, "at": {"at_src": 100 * SEC}},
                {"op": "hide_roles_add", "from": {"raw": {"time": {"clock": "cut", "cut": fx.cut, "t": SEC / 2}, "reason": "camera drops out"}},
                 "to": {"part_end": {"part": fx.parts[1]}}, "roles": [{"region": "cam"}]},
            ]}),
        )
        .output;
    assert_eq!(
        r["results"][0]["from"]["method"],
        "word_fingerprint (unverified: no transcript yet)"
    );
    assert_eq!(
        r["results"][0]["from"]["provenance"], "raw",
        "no transcript can verify it at M1"
    );
    assert_eq!(r["results"][0]["from"]["cut_s"], 0.5);
    assert_eq!(r["results"][1]["from"]["provenance"], "raw");
    let d = p.h.snapshot().doc.clone();
    assert_eq!(d.count::<ContentHold>(), 1);
    assert_eq!(d.count::<HideRoles>(), 1);
    // captions
    let c = p
        .human(
            "cut_edit",
            json!({"cut": fx.cut, "ops": [{"op": "caption_set", "enabled": false, "drop_fillers": true,
                "keywords_add": [{"asr_revision": "r1", "ordinal": 4}],
                "no_caption_add": [{"from": part0, "to": part0_end}]}]}),
        )
        .output;
    let nc = c["results"][0]["no_caption_added"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let d = p.h.snapshot().doc.clone();
    let cd = d.iter::<CaptionDoc>().next().unwrap();
    assert!(!cd.enabled && cd.drop_fillers);
    assert_eq!(cd.keywords.len(), 1);
    assert!(cd.no_caption_ranges.contains_key(&nc));
    // layout side: framing grouping + caption place, layer time map, part caption place,
    // a visual split set and delete
    let vs = p
        .human(
            "visual_split_add",
            json!({"part": fx.parts[1], "framing": fx.framing, "at": 104 * SEC}),
        )
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human(
        "layout_edit",
        json!({"cut": fx.cut, "ops": [
            {"op": "framing_set", "framing": fx.framing, "caption_grouping": "single_word", "caption_place": {"x": 0.5, "y": 0.55, "w": 0.9}},
            {"op": "layer_set", "layer": fx.screen, "time_map": {"hold": {"at_src": 100 * SEC}}},
            {"op": "caption_place", "part": fx.parts[0], "framing": fx.framing, "place": {"x": 0.5, "y": 0.6, "w": 0.8}},
            {"op": "visual_split_set", "split": vs, "overrides": {fx.screen.clone(): {"opacity": 0.5}}},
        ]}),
    );
    let d = p.h.snapshot().doc.clone();
    let f = d.get(cid::<Framing>(&fx.framing)).unwrap();
    assert_eq!(f.caption_grouping, CaptionGrouping::SingleWord);
    assert!(matches!(
        d.get(cid::<Layer>(&fx.screen)).unwrap().time_map,
        TimeMap::Hold(_)
    ));
    assert!(
        d.get(cid::<Part>(&fx.parts[0]))
            .unwrap()
            .caption_place_override
            .contains_key(&cid(&fx.framing))
    );
    assert_eq!(d.get(cid::<VisualSplit>(&vs)).unwrap().overrides.len(), 1);
    p.human(
        "layout_edit",
        json!({"cut": fx.cut, "ops": [{"op": "visual_split_delete", "split": vs}]}),
    );
    assert!(p.h.snapshot().doc.get(cid::<VisualSplit>(&vs)).is_none());
    // and each of these is undoable in one step
    let before = p.h.snapshot().doc.count::<VisualSplit>();
    p.human("undo", json!({}));
    assert_eq!(p.h.snapshot().doc.count::<VisualSplit>(), before + 1);
    p.h.close().unwrap();
}

#[test]
fn unresolvable_or_backwards_anchors_are_refused() {
    let p = open("vocab-bad");
    let fx = fixture(&p, 2);
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "gain_segment_add", "start": {"part_end": {"part": fx.parts[0]}}, "end": {"part_start": {"part": fx.parts[0]}}, "gain_db": 3}]}),
    );
    assert!(
        matches!(e, Err(CmdError::Invalid { ref path, .. }) if path.starts_with("/ops/0")),
        "{e:?}"
    );
    // lines need a transcript
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "hide_roles_add", "from": {"line_start": {"asr_revision": "r1", "line": 3}}, "to": {"part_end": {"part": fx.parts[0]}}, "roles": [{"region": "cam"}]}]}),
    );
    assert!(
        matches!(e, Err(CmdError::Invalid { ref reason, .. }) if reason.contains("transcript")),
        "{e:?}"
    );
    // a raw time with no reason
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "hide_roles_add", "from": {"raw": {"time": {"clock": "cut", "cut": fx.cut, "t": 0}, "reason": " "}}, "to": {"part_end": {"part": fx.parts[0]}}, "roles": [{"region": "cam"}]}]}),
    );
    assert!(matches!(e, Err(CmdError::Invalid { .. })), "{e:?}");
    // another cut's part edge
    let other = p.human("cut_create", json!({"title": "o"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = p.try_human(
        "cut_edit",
        json!({"cut": other, "ops": [{"op": "hide_roles_add", "from": {"part_start": {"part": fx.parts[0]}}, "to": {"part_end": {"part": fx.parts[0]}}, "roles": [{"region": "cam"}]}]}),
    );
    assert!(e.is_err());
    // gain out of range
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "gain_segment_add", "start": {"part_start": {"part": fx.parts[0]}}, "end": {"part_end": {"part": fx.parts[0]}}, "gain_db": 99}]}),
    );
    assert!(matches!(e, Err(CmdError::Invalid { .. })));
    p.h.close().unwrap();
}

#[test]
fn spans_stay_inside_the_cut_offsets_are_bounded_and_probes_are_checked() {
    let p = open("vocab-bounds");
    let fx = fixture(&p, 1);
    // a word time outside every part of the cut: refused
    let far = json!({"word_start": {"word_id": {"asr_revision": "r1", "ordinal": 1}, "t_src_start": 900 * SEC, "t_src_end": 901 * SEC, "normalised_text": "x"}});
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "hide_roles_add", "from": far, "to": {"part_end": {"part": fx.parts[0]}}, "roles": [{"region": "cam"}]}]}),
    );
    assert!(
        matches!(e, Err(CmdError::Invalid { ref reason, .. }) if reason.contains("outside the cut")),
        "{e:?}"
    );
    // an offset beyond ±24 h: refused, not overflowed
    let e = p.try_human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "gain_segment_add", "start": {"part_start": {"part": fx.parts[0], "offset_ms": i64::MAX}}, "end": {"part_end": {"part": fx.parts[0]}}, "gain_db": 1}]}),
    );
    assert!(
        matches!(e, Err(CmdError::Invalid { ref reason, .. }) if reason.contains("24 h")),
        "{e:?}"
    );
    // a probe with a zero size or a zero frame-rate denominator
    for bad in [
        json!({"duration": 0, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 0, "height": 360, "fps": null, "timescale": 1, "gop_max_s": null}], "audio_tracks": []}),
        json!({"duration": 0, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 640, "height": 360, "fps": {"num": 30, "den": 0}, "timescale": 1, "gop_max_s": null}], "audio_tracks": []}),
    ] {
        let e = p.try_human(
            "source_add",
            json!({"path": "/m/x.mkv", "fingerprint": "f", "probe": bad}),
        );
        assert!(matches!(e, Err(CmdError::Invalid { .. })), "{e:?}");
    }
    p.h.close().unwrap();
}

#[test]
fn layer_delete_removes_a_part_addition_only() {
    let p = open("layer_delete");
    let fx = fixture(&p, 2);
    let det = p
        .human(
            "layout_edit",
            json!({"cut": fx.cut, "ops": [{"op": "layer_add",
                   "owner": {"part_override": {"part": fx.parts[0], "framing": fx.framing}},
                   "role": "detail", "crop": {"x": 0.8, "y": 0.0, "w": 0.2, "h": 0.1},
                   "place": {"x": 0.25, "y": 0.42, "w": 0.5}, "z": 2}]}),
        )
        .output["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let n = p.h.snapshot().doc.count::<Layer>();
    // a framing's shared layer is refused
    let e = p
        .try_human("layer_delete", json!({"layer": fx.screen}))
        .unwrap_err();
    assert!(matches!(e, CmdError::Invalid { .. }), "{e:?}");
    p.human("layer_delete", json!({"layer": det}));
    assert_eq!(p.h.snapshot().doc.count::<Layer>(), n - 1);
    p.human("undo", json!({}));
    assert_eq!(p.h.snapshot().doc.count::<Layer>(), n);
    assert!(p.h.snapshot().doc.get(cid::<Layer>(&det)).is_some());
}

/// AJ 2026-10-17: an agent `cut_edit` split at 1.44 s (86.4 frames of 60 fps) left the
/// project timeline unplannable. Word times snap to the cut's frame grid.
#[test]
fn a_word_time_split_on_a_60_fps_cut_lands_on_the_frame_grid() {
    let p = open("vocab-60fps-grid");
    let c = p.human("cut_create", json!({"title": "60"}));
    let cut = c.output["cut"].as_str().unwrap().to_owned();
    let source = id(&p.human(
        "source_add",
        json!({"path": "/m/s.mkv", "fingerprint": "fp"}),
    ));
    p.human(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 60, "den": 1}}),
    );
    let at = 144 * SEC / 100; // 1.44 s = 86.4 frames at 60 fps
    let part = p
        .human(
            "part_add",
            json!({"cut": cut, "media": {"source": source}, "src_in": 0, "src_out": 10 * SEC}),
        )
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human(
        "cut_edit",
        json!({"cut": cut, "ops": [{"op": "part_split", "part": part, "at": at}]}),
    );
    let d = p.h.snapshot().doc.clone();
    let cid: Id<Cut> = cid(&cut);
    let parts: Vec<&Part> = d
        .children::<Part>(cid.entity())
        .filter_map(|id| d.get(id))
        .collect();
    assert_eq!(parts.len(), 2);
    let fpf = SEC / 60;
    for p in &parts {
        assert_eq!(p.src_in.0 % fpf, 0, "src_in {}", p.src_in.0);
        assert_eq!(p.src_out.0 % fpf, 0, "src_out {}", p.src_out.0);
    }
    assert_eq!(
        parts[0].src_out.0,
        86 * fpf,
        "1.44 s ties to the earlier frame (86)"
    );
}

fn id(ok: &gmclip_core::CallOk) -> String {
    ok.output["id"].as_str().unwrap().to_owned()
}
