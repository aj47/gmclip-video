//! Caption styles (SPEC §5.10, §8 [M2]): the M2 groupings and the caption look reach the
//! plan (word bounce = adaptive words + bounce, keywords popping), presets are saved and
//! applied as one undo step, and a preview never touches the document.

mod common;

use common::{SEC, fixture, open};
use gmclip_core::analysis::{AnalysisView, Transcript, Word};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use gmclip_plan::types::{AnimKind, TextRole};
use serde_json::json;

struct Fonts;
impl PlanHost for Fonts {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// The fixture's source with a picture size, and a transcript over its parts.
fn ready(p: &common::Project, fx: &common::Fx) -> (Doc, AnalysisView) {
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
    let text = "we generated that in two minutes on my own GPU. wow that is fast";
    let words: Vec<Word> = text
        .split(' ')
        .enumerate()
        .map(|(i, t)| Word {
            text: t.into(),
            start: 100 * SEC + (i as i64) * SEC / 4,
            end: 100 * SEC + (i as i64 + 1) * SEC / 4,
            confidence: None,
        })
        .collect();
    let mut v = AnalysisView::default();
    v.insert(
        &fx.source,
        Transcript {
            revision: "r1".into(),
            words,
            lines: vec![],
        },
    );
    (doc, v)
}

#[test]
fn word_bounce_plans_to_v1_with_bounce_and_keyword_pop() {
    let p = open("capstyle");
    let fx = fixture(&p, 1);
    p.human(
        "font_add",
        json!({"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}),
    );
    p.human(
        "caption_set",
        json!({"cut": fx.cut, "enabled": true, "source": "heard",
        "keywords_add": [{"asr_revision": "r1", "ordinal": 5}]}),
    );
    let fr: Id<Framing> = uid(&fx.framing);
    let cut: Id<Cut> = uid(&fx.cut);

    // the list says what can be applied; Figtree is not in the project yet
    let list = p
        .human("caption_preset_list", json!({"framing": fx.framing}))
        .output;
    let row = |n: &str| {
        list["presets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(row("word_bounce")["ready"], true);
    assert_eq!(row("bold_pop")["ready"], false);
    assert!(
        p.try_human(
            "caption_preset_apply",
            json!({"framing": fx.framing, "name": "bold_pop"})
        )
        .is_err()
    );

    let before = p.h.snapshot().doc.clone();
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "word_bounce"}),
    );
    let (doc, view) = ready(&p, &fx);
    let f = doc.get(fr).unwrap();
    assert_eq!(f.caption_grouping, CaptionGrouping::AdaptiveWords);
    assert_eq!(f.caption_style_ref.as_deref(), Some("Cap-word_bounce"));
    let plan = planner::plan_cut_with(&doc, cut, fr, &Fonts, Some(&view)).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V1);
    let caps: Vec<_> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .collect();
    assert!(caps.len() >= 4, "{}", caps.len());
    assert!(
        caps.iter()
            .all(|e| e.anim.as_ref().and_then(|a| a.kind) == Some(AnimKind::Bounce))
    );
    assert!(caps.iter().all(|e| e.runs.len() <= 4));
    let popped: Vec<_> = caps
        .iter()
        .filter_map(|e| e.anim.as_ref()?.pop_runs.as_ref().map(|r| (e, r)))
        .collect();
    assert_eq!(popped.len(), 1, "one cue holds the keyword");
    let (e, r) = popped[0];
    assert!(
        e.runs[r[0] as usize]
            .text
            .to_lowercase()
            .starts_with("minutes")
    );
    assert_eq!(list["presets"][0]["current"], true, "captions on = Story");
    assert_eq!(list["presets"][1]["current"], false);
    let list = p
        .human("caption_preset_list", json!({"framing": fx.framing}))
        .output;
    assert_eq!(list["presets"][1]["current"], true);

    // one undo step back to RT's look (the plan is /0 again)
    p.human("undo", json!({}));
    let (doc, view) = ready(&p, &fx);
    assert_eq!(doc.get(fr).unwrap(), before.get(fr).unwrap());

    // save a style of our own, apply it, and preview another without committing
    let mut mine = gmclip_core::library::resolve(&doc, "word_bounce").unwrap();
    mine.look.animation = CaptionAnimation::Fade;
    mine.grouping = CaptionGrouping::Phrase;
    p.human(
        "caption_preset_save",
        json!({"name": "AJ fade", "preset": mine}),
    );
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "AJ fade"}),
    );
    let (doc, view2) = ready(&p, &fx);
    let plan = planner::plan_cut_with(&doc, cut, fr, &Fonts, Some(&view2)).unwrap();
    let caps: Vec<_> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .collect();
    assert!(
        caps.iter()
            .all(|e| e.anim.as_ref().and_then(|a| a.kind) == Some(AnimKind::Fade))
    );
    assert!(caps.iter().all(|e| e.runs.len() <= 5));
    let rev = p.h.snapshot().doc.rev();
    let preset = gmclip_core::library::resolve(&doc, "story").unwrap();
    let scratch = gmclip_core::library::preview_doc(&doc, fr, "story", &preset, &[]).unwrap();
    let plan = planner::plan_cut_with(&scratch, cut, fr, &Fonts, Some(&view)).unwrap();
    assert_eq!(plan.schema, SchemaVersion::V0, "RT's look needs no /1");
    assert_eq!(p.h.snapshot().doc.rev(), rev);
    // a bad keyword colour is refused
    assert!(
        p.try_human(
            "framing_set",
            json!({"framing": fx.framing, "caption_look": {"keyword_colour": "yellow"}})
        )
        .is_err()
    );
    p.human(
        "framing_set",
        json!({"framing": fx.framing, "caption_look": {"animation": "pop"}}),
    );
    assert_eq!(
        p.h.snapshot()
            .doc
            .get(fr)
            .unwrap()
            .caption_look
            .as_ref()
            .unwrap()
            .animation,
        CaptionAnimation::Pop
    );
}

#[test]
fn captions_export_writes_srt_vtt_and_ass() {
    use gmclip_core::command::{Caller, Cx, Services, Tx};
    use gmclip_core::services::UlidGen;
    use gmclip_model::{Author, Staging};
    let p = open("capexport");
    let fx = fixture(&p, 1);
    p.human(
        "font_add",
        json!({"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}),
    );
    p.human(
        "caption_set",
        json!({"cut": fx.cut, "enabled": true, "source": "heard"}),
    );
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "story"}),
    );
    let (doc, view) = ready(&p, &fx);
    let run = |format: &str| -> Result<serde_json::Value, gmclip_core::CmdError> {
        let mut ids = UlidGen::new(Some(1));
        let st = Staging::new(doc.clone(), Author::Human, &mut ids);
        let caller = Caller::human();
        let mut cx = Cx {
            tx: Tx::new(st),
            caller: &caller,
            analysis: &view,
            services: Services { now_ms: common::T0 },
        };
        gmclip_core::registry::find("captions_export")
            .unwrap()
            .call(json!({"cut": fx.cut, "format": format}), &mut cx)
    };
    let srt = run("srt").unwrap();
    assert!(srt["cues"].as_u64().unwrap() >= 3);
    let text = srt["text"].as_str().unwrap();
    assert!(text.starts_with("1\n00:00:00,000 --> "), "{text}");
    assert!(text.contains("We generated"), "{text}");
    assert!(
        run("vtt").unwrap()["text"]
            .as_str()
            .unwrap()
            .starts_with("WEBVTT")
    );
    let ass = run("ass").unwrap();
    assert!(
        ass["text"]
            .as_str()
            .unwrap()
            .contains("Style: Cap-story,Neometric Heavy,86")
    );
    assert!(run("txt").is_err());
}

/// Decision 0022 (SPEC §5.10): `stream` captions were folded into `heard` (the cut's own
/// audio, identical parts reusing the source words); a doc that says `stream` reads as
/// `heard` and plans the same cues, the same times.
#[test]
fn stream_reads_as_heard() {
    let p = open("capstream");
    let fx = fixture(&p, 1);
    p.human(
        "font_add",
        json!({"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}),
    );
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "story"}),
    );
    let fr: Id<Framing> = uid(&fx.framing);
    let cut: Id<Cut> = uid(&fx.cut);
    let caps = |source: &str| {
        p.human(
            "caption_set",
            json!({"cut": fx.cut, "enabled": true, "source": source}),
        );
        let (doc, view) = ready(&p, &fx);
        let plan = planner::plan_cut_with(&doc, cut, fr, &Fonts, Some(&view)).unwrap();
        plan.text_events
            .iter()
            .flatten()
            .filter(|e| e.role == TextRole::Caption)
            .map(|e| {
                (
                    e.out.0,
                    e.runs.iter().map(|r| r.text.clone()).collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let heard = caps("heard");
    let stream = caps("stream");
    assert!(stream.len() >= 3, "{stream:?}");
    assert_eq!(stream, heard);
    assert!(stream[0].1.starts_with("We generated"), "{stream:?}");
}

/// A no-caption span on the source clock (RT's `no_caption`, imported) silences the words
/// whose middle it covers, even when the span starts before the take does (RT's rule:
/// the word's native midpoint in `[lo, hi)`).
#[test]
fn source_clock_no_caption_spans_cover_words_by_their_middle() {
    let p = open("capnocap");
    let fx = fixture(&p, 1);
    p.human(
        "font_add",
        json!({"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}),
    );
    p.human(
        "caption_preset_apply",
        json!({"framing": fx.framing, "name": "story"}),
    );
    p.human(
        "caption_set",
        json!({"cut": fx.cut, "enabled": true, "source": "heard"}),
    );
    // as an imported cut holds it (caption_set refuses a span the cut does not play)
    let (mut doc, view) = ready(&p, &fx);
    let cut: Id<Cut> = uid(&fx.cut);
    let cid = doc.children::<CaptionDoc>(cut.entity()).next().unwrap();
    let mut cd = doc.get(cid).unwrap().clone();
    let src = |t: i64| {
        gmclip_model::anchor::Anchor::Raw(gmclip_model::anchor::RawTime {
            time: gmclip_model::clock::ModelTime::Src {
                source: Some(uid(&fx.source)),
                group: None,
                t: gmclip_model::Flicks(t),
            },
            reason: "rt no_caption".into(),
        })
    };
    cd.no_caption_ranges.insert(
        "nc-0".into(),
        NoCaptionRange {
            from: src(99 * SEC),
            to: src(100 * SEC + 6 * SEC / 10),
        },
    );
    doc.insert_for_test(cd);
    let plan =
        planner::plan_cut_with(&doc, uid(&fx.cut), uid(&fx.framing), &Fonts, Some(&view)).unwrap();
    let text: Vec<String> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .map(|e| e.runs.iter().map(|r| r.text.clone()).collect::<String>())
        .collect();
    // "we" (mid 100.125) and "generated" (100.375) are covered; "that" (100.625) is not
    assert!(text[0].starts_with("That"), "{text:?}");
    assert!(
        !text.join(" ").to_lowercase().contains("generated"),
        "{text:?}"
    );
}
