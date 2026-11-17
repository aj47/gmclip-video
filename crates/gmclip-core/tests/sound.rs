//! Decision 0051: a recording's system audio under the mic. The screen member of a sync
//! group is heard under the face file's voice at its own gain (`sync_group_create`'s
//! `under_db`, `sync_under_set`), a part or a span of one changes it (`part_set_under`,
//! which splits at the span's edges), and a group whose other members are not heard
//! plans exactly as before.

mod common;

use common::{SEC, id, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::*;
use gmclip_model::{Id, Ulid};
use gmclip_plan::consts::SchemaVersion;
use serde_json::json;

struct Files;
impl PlanHost for Files {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some("/fonts/face.ttf".into())
    }
    fn asset_path(&self, a: &Asset) -> Option<String> {
        Some(format!("/assets/{}.m4a", a.sha256))
    }
}

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

fn source(p: &common::Project, name: &str) -> String {
    id(&p.human(
        "source_add",
        json!({"path": format!("/rec/{name}.mov"), "fingerprint": planner::fingerprint(Some(&"c".repeat(64)), Some(5)),
               "probe": {"duration": 600 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080,
                          "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                         "audio_tracks": [{"index": 1, "codec": "aac", "rate": 48000, "layout": "stereo", "bitrate": null, "speech_ratio": null}]}}),
    ))
}

struct Rec {
    p: common::Project,
    cut: String,
    framing: String,
    parts: Vec<String>,
    screen_member: String,
    group: String,
}

/// A recording as `open_recording` makes it: face (the voice) and screen (the picture)
/// one group at offset 0, the screen's sound under the voice at `under_db`; two parts.
fn rec(tag: &str, under_db: Option<f64>) -> Rec {
    let p = open(tag);
    let face = source(&p, "face");
    let screen = source(&p, "screen");
    let mut pic = json!({"source": screen, "role": "picture", "offset": 0});
    if let Some(db) = under_db {
        pic["under_db"] = json!(db);
    }
    let g = p.human(
        "sync_group_create",
        json!({"members": [{"source": face, "role": "audio", "offset": 0}, pic]}),
    );
    let group = g.output["group"].as_str().unwrap().to_owned();
    let screen_member = g.output["members"][1].as_str().unwrap().to_owned();
    let cut = p.human("cut_create", json!({"title": "rec"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("caption_set", json!({"cut": cut, "enabled": false}));
    let framing = id(&p.human(
        "framing_create",
        json!({"cut": cut, "name": "16:9", "w": 1920, "h": 1080, "fps": {"num": 30, "den": 1}}),
    ));
    p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    let a = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"group": group}, "src_in": 10 * SEC, "src_out": 14 * SEC}),
    ));
    let b = id(&p.human(
        "part_add",
        json!({"cut": cut, "after": a, "media": {"group": group}, "src_in": 20 * SEC, "src_out": 23 * SEC}),
    ));
    Rec {
        p,
        cut,
        framing,
        parts: vec![a, b],
        screen_member,
        group,
    }
}

fn plan(r: &Rec) -> gmclip_plan::Plan {
    let doc = r.p.h.snapshot().doc.clone();
    let plan = planner::plan_cut(&doc, uid(&r.cut), uid(&r.framing), &Files).unwrap();
    gmclip_plan::parse(&serde_json::to_vec(&plan).unwrap()).expect("the plan validates");
    plan
}

/// `(gain, under first_sample − voice first_sample)` per plan part, `None` when it hears
/// the voice alone.
fn unders(plan: &gmclip_plan::Plan) -> Vec<Option<(f64, i64)>> {
    plan.parts
        .iter()
        .map(|p| {
            p.audio.under.as_ref().map(|u| {
                assert_eq!(u.len(), 1);
                assert_ne!(
                    u[0].source, p.audio.source,
                    "the screen file, not the voice"
                );
                assert_eq!(u[0].source, p.picture.source);
                (
                    u[0].gain_db,
                    u[0].first_sample as i64 - p.audio.first_sample as i64,
                )
            })
        })
        .collect()
}

#[test]
fn a_recordings_system_audio_plays_under_the_mic() {
    let r = rec("sound-under", Some(-12.0));
    let p = plan(&r);
    assert_eq!(p.schema, SchemaVersion::V1);
    assert_eq!(unders(&p), [Some((-12.0, 0)), Some((-12.0, 0))]);
    assert_eq!(p.parts[0].audio.first_sample, 10 * 48_000);

    // one part quieter, the other silenced: the member's level elsewhere
    r.p.human(
        "part_set_under",
        json!({"parts": [r.parts[1]], "under_db": -60}),
    );
    r.p.human(
        "part_set_under",
        json!({"parts": [r.parts[0]], "under_db": -20}),
    );
    assert_eq!(unders(&plan(&r)), [Some((-20.0, 0)), None]);
    r.p.human(
        "part_set_under",
        json!({"parts": [r.parts[0], r.parts[1]], "under_db": null}),
    );
    r.p.human(
        "sync_under_set",
        json!({"member": r.screen_member, "under_db": -6}),
    );
    assert_eq!(unders(&plan(&r)), [Some((-6.0, 0)), Some((-6.0, 0))]);

    // a span of one part: split at its edges, only the middle changes
    let out =
        r.p.human(
            "part_set_under",
            json!({"parts": [r.parts[0]], "under_db": -60, "span": [11 * SEC, 12 * SEC]}),
        )
        .output;
    let mid = out["parts"][0].as_str().unwrap().to_owned();
    let doc = r.p.h.snapshot().doc.clone();
    let m = doc.get(uid::<Part>(&mid)).unwrap();
    assert_eq!((m.src_in.0, m.src_out.0), (11 * SEC, 12 * SEC));
    let p = plan(&r);
    assert_eq!(
        unders(&p),
        [Some((-6.0, 0)), None, Some((-6.0, 0)), Some((-6.0, 0))]
    );
    // the voice carries straight on across the split (no fades there)
    assert!(gmclip_plan::validate::continuous_join(&p, 0));
    assert!(p.parts[0].audio.fade_out().is_none());

    // undo puts the part back whole
    r.p.human("undo", json!({}));
    let doc = r.p.h.snapshot().doc.clone();
    assert_eq!(
        doc.children::<Part>(uid::<Cut>(&r.cut).entity()).count(),
        2,
        "one undo step"
    );

    // a member that is not heard, and the voice member itself, are refused or silent
    assert!(
        r.p.try_human(
            "part_set_under",
            json!({"parts": [r.parts[0]], "under_db": 20})
        )
        .is_err()
    );
    r.p.human(
        "sync_under_set",
        json!({"member": r.screen_member, "under_db": null}),
    );
    assert_eq!(unders(&plan(&r)), [None, None]);
}

/// A group whose screen member is not heard plans byte for byte as before (`/0` when
/// nothing else needs `/1`); a sped part hears only its voice.
#[test]
fn without_under_sound_the_plan_is_unchanged() {
    let quiet = rec("sound-quiet", None);
    let loud = rec("sound-loud", Some(-12.0));
    let (a, mut b) = (plan(&quiet), plan(&loud));
    assert_eq!(a.schema, SchemaVersion::V0);
    for p in &mut b.parts {
        p.audio.under = None;
    }
    b.schema = planner::schema_for(&b);
    // the two projects differ only in their ids, which the plan does not carry
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );

    loud.p.human(
        "part_set_speed",
        json!({"part": loud.parts[0], "speed": 2.0}),
    );
    assert_eq!(unders(&plan(&loud))[0], None);
}

/// Decision 0051: music from a stored asset over the whole cut, ducked under the words
/// its parts play (gaps under 0.3 s closed), and a sound effect from a source's own sound,
/// cut short where its stretch ends; changed and removed as undoable steps.
#[test]
fn music_plays_over_the_cut_and_ducks_under_the_voice() {
    use gmclip_core::analysis::AnalysisView;
    use gmclip_core::transcript::{Transcript, Word};
    let r = rec("sound-music", None);
    let music = id(&r.p.human(
        "asset_add",
        json!({"name": "bed.m4a", "sha256": "d".repeat(64), "size": 1000, "media_type": "audio/mp4"}),
    ));
    let o = id(&r.p.human(
        "audio_overlay_add",
        json!({"cut": r.cut, "media": {"asset": music},
               "start": {"part_start": {"part": r.parts[0]}}, "end": {"part_end": {"part": r.parts[1]}},
               "duck": {"depth_db": 12, "attack_ms": 200, "release_ms": 600}}),
    ));
    // the voice: two words close together in the first part, one in the second
    let w = |a: f64, b: f64| Word {
        text: "w".into(),
        start: (a * SEC as f64) as i64,
        end: (b * SEC as f64) as i64,
        confidence: None,
    };
    let mut view = AnalysisView::default();
    view.insert(
        &r.group,
        Transcript {
            revision: "r1".into(),
            words: vec![w(10.5, 11.0), w(11.1, 11.4), w(16.0, 16.5), w(21.0, 21.5)],
            lines: Vec::new(),
        },
    );
    let heard = |r: &Rec| {
        let doc = r.p.h.snapshot().doc.clone();
        let p = planner::plan_cut_with(&doc, uid(&r.cut), uid(&r.framing), &Files, Some(&view))
            .unwrap();
        gmclip_plan::parse(&serde_json::to_vec(&p).unwrap()).expect("the plan validates");
        p
    };
    let p = heard(&r);
    assert_eq!(p.schema, SchemaVersion::V1);
    let s = &p.audio.overlays.as_ref().unwrap()[0];
    let src = p.sources.iter().find(|x| x.id == s.source).unwrap();
    assert_eq!(src.path, format!("/assets/{}.m4a", "d".repeat(64)));
    assert_eq!(
        (s.first_sample, s.out_samples.0, s.gain_db),
        (0, [0, 7 * 48_000], -18.0)
    );
    assert_eq!(
        (s.fade_in_samples, s.fade_out_samples),
        (Some(24_000), Some(48_000))
    );
    let d = s.duck.unwrap();
    assert_eq!(
        (d.depth_db, d.attack_samples, d.release_samples),
        (-12.0, 9_600, 28_800)
    );
    // 0.5–1.4 s (one stretch), the word outside the parts is not heard, 5–5.5 s
    let speech: Vec<[u64; 2]> = p
        .audio
        .speech
        .as_ref()
        .unwrap()
        .iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(speech, [[24_000, 67_200], [240_000, 264_000]]);

    // quieter, not ducked: no speech in the plan
    r.p.human(
        "audio_overlay_set",
        json!({"overlay": o, "gain_db": -24, "no_duck": true}),
    );
    let p = heard(&r);
    let s = &p.audio.overlays.as_ref().unwrap()[0];
    assert_eq!((s.gain_db, s.duck), (-24.0, None));
    assert!(p.audio.speech.is_none());

    // a sound effect: one second of the screen file's own sound from 30 s, laid over the
    // second part (cut short where the stretch ends)
    let screen = {
        let doc = r.p.h.snapshot().doc.clone();
        let m = doc.get(uid::<SyncMember>(&r.screen_member)).unwrap();
        m.source.0.to_string()
    };
    r.p.human(
        "audio_overlay_add",
        json!({"cut": r.cut, "media": {"span": {"media": {"source": screen}, "src_in": 30 * SEC, "src_out": 31 * SEC}},
               "start": {"part_start": {"part": r.parts[1]}}, "end": {"part_end": {"part": r.parts[1]}},
               "gain_db": -6, "fade_in_ms": 0, "fade_out_ms": 0}),
    );
    let p = heard(&r);
    let fx = &p.audio.overlays.as_ref().unwrap()[1];
    assert_eq!(fx.source, p.parts[0].picture.source, "the screen file");
    assert_eq!(
        (fx.first_sample, fx.out_samples.0, fx.fade_in_samples),
        (30 * 48_000, [4 * 48_000, 5 * 48_000], None)
    );

    // removed, then back with one undo
    r.p.human("audio_overlay_delete", json!({"overlay": o}));
    assert_eq!(heard(&r).audio.overlays.as_ref().unwrap().len(), 1);
    r.p.human("undo", json!({}));
    assert_eq!(heard(&r).audio.overlays.as_ref().unwrap().len(), 2);
    // refusals
    for bad in [
        json!({"overlay": o, "gain_db": 20}),
        json!({"overlay": o, "duck": {"depth_db": 50, "attack_ms": 10, "release_ms": 10}}),
        json!({"overlay": o, "no_duck": true, "duck": {"depth_db": 6, "attack_ms": 10, "release_ms": 10}}),
    ] {
        assert!(r.p.try_human("audio_overlay_set", bad).is_err());
    }
    let png = id(&r.p.human(
        "asset_add",
        json!({"name": "logo.png", "sha256": "e".repeat(64), "size": 10, "media_type": "image/png"}),
    ));
    assert!(
        r.p.try_human(
            "audio_overlay_add",
            json!({"cut": r.cut, "media": {"asset": png},
                   "start": {"part_start": {"part": r.parts[0]}}, "end": {"part_end": {"part": r.parts[0]}}}),
        )
        .is_err(),
        "a picture is not a sound"
    );
}
