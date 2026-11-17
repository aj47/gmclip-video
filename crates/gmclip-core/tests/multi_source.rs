//! Several videos in one project (AJ 2026-10-15: "i should be able to load multiple video
//! files into one project"; 2026-10-18: one main project view, decision 0036): one stream
//! edit holding every video in the order they were added, topics on it told apart by
//! video, a long-form from topics of two videos, a short mixing them (strikes stay on
//! their own video), the map's sources, removal refused while a cut plays a video (and
//! otherwise taking only that video out of the stream edit), and a same-recording pair
//! as one video. The plan of a mixed cut takes each part's picture from its own file,
//! cropped in that file's pixels.

mod common;

use common::{Project, SEC, id, open};
use gmclip_core::planner::{self, PlanHost};
use gmclip_model::entities::{Cut, Font, Framing, MediaRef, Part};
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};

fn uid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

/// A word every 0.4 s of revision `rev`.
fn word(rev: &str, n: u64) -> Value {
    let t = n as i64 * SEC * 2 / 5;
    json!({"word_id": {"asr_revision": rev, "ordinal": n}, "t_src_start": t, "t_src_end": t + SEC * 3 / 10, "normalised_text": format!("w{n}")})
}

fn w_at(s: i64) -> u64 {
    (s * 5 / 2) as u64
}

fn source(p: &Project, name: &str, secs: i64, w: u32, h: u32) -> String {
    id(&p.human(
        "source_add",
        json!({"path": format!("/rec/{name}.mp4"), "fingerprint": planner::fingerprint(Some(&"b".repeat(64)), Some(5)),
               "probe": {"duration": secs * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": w, "height": h,
                          "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}],
                         "audio_tracks": [{"index": 1, "codec": "aac", "rate": 48000, "layout": "stereo", "bitrate": null, "speech_ratio": null}]}}),
    ))
}

fn parts(p: &Project, cut: &str) -> Vec<Part> {
    let d = p.h.snapshot().doc.clone();
    gmclip_model::clock::cut_layout(&d, uid::<Cut>(cut))
        .iter()
        .filter_map(|sp| d.get(sp.part).cloned())
        .collect()
}

fn src(m: &MediaRef) -> Option<String> {
    match m {
        MediaRef::Source(s) => Some(s.0.to_string()),
        MediaRef::Group(_) => None,
    }
}

fn reason(e: &gmclip_core::CmdError) -> String {
    serde_json::to_value(e).unwrap().to_string()
}

struct Two {
    p: Project,
    a: String,
    b: String,
    /// The stream edit, holding both.
    main: String,
}

/// A 10-minute landscape video and a 5-minute portrait one, both on the stream edit.
fn two(tag: &str) -> Two {
    let p = open(tag);
    let a = source(&p, "desk", 600, 2560, 1440);
    let b = source(&p, "phone", 300, 1080, 1920);
    // two videos and none named: which one starts it is ambiguous
    let e = p.try_human("stream_edit_ensure", json!({})).unwrap_err();
    assert!(reason(&e).contains("name the source"), "{e:?}");
    let sa = p.human("stream_edit_ensure", json!({"media": {"source": a}}));
    assert_eq!(sa.output["how"], "created");
    let sb = p.human("stream_edit_ensure", json!({"media": {"source": b}}));
    assert_eq!(sb.output["how"], "appended");
    assert_eq!(sa.output["cut"], sb.output["cut"], "one stream edit");
    let main = sa.output["cut"].as_str().unwrap().to_owned();
    Two { p, a, b, main }
}

#[test]
fn one_stream_edit_holds_every_video() {
    let t = two("ms-streams");
    let p = &t.p;
    // asking again finds it
    let again = p.human("stream_edit_ensure", json!({"media": {"source": t.b}}));
    assert_eq!(again.output["how"], "existing");
    assert_eq!(again.output["cut"], t.main.as_str());
    // each video whole, in the order they were added
    let ps = parts(p, &t.main);
    assert_eq!(ps.len(), 2);
    assert_eq!(src(&ps[0].media), Some(t.a.clone()));
    assert_eq!((ps[0].src_in.0, ps[0].src_out.0), (0, 600 * SEC));
    assert_eq!(src(&ps[1].media), Some(t.b.clone()));
    assert_eq!((ps[1].src_in.0, ps[1].src_out.0), (0, 300 * SEC));
    // in the first video's shape
    let d = p.h.snapshot().doc.clone();
    let fr: Vec<(u32, u32)> = d
        .children::<Framing>(uid::<Cut>(&t.main).entity())
        .filter_map(|f| d.get(f))
        .map(|f| (f.canvas.w, f.canvas.h))
        .collect();
    assert_eq!(fr, vec![(1920, 1080)]);
    // any of the project's videos may be added to it
    let extra = id(&p.human(
        "part_add",
        json!({"cut": t.main, "media": {"source": t.b}, "src_in": 0, "src_out": 2 * SEC}),
    ));
    p.human("part_delete", json!({"part": extra}));

    // the map lists both videos, on the one stream edit
    let m = p.human("project_map", json!({})).output;
    let src = m["sources"].as_array().unwrap();
    assert_eq!(src.len(), 2);
    assert_eq!(src[0]["name"], "desk");
    assert_eq!(src[1]["name"], "phone");
    assert_eq!(src[1]["width"], 1080);
    assert_eq!(src[1]["duration_ms"], 300_000);
    assert_eq!(src[0]["stream"], t.main.as_str());
    assert_eq!(src[1]["stream"], t.main.as_str());
    assert_eq!(m["streams"].as_array().unwrap().len(), 1);
    assert_eq!(m["stream"]["cut"], t.main.as_str());
    assert!(m["stream"]["media"].is_null(), "several videos");
    assert_eq!(m["stream"]["videos"].as_array().unwrap().len(), 2);
    assert_eq!(m["stream"]["total_ms"], 900_000);
    let listed = p.human("source_list", json!({})).output;
    assert_eq!(listed["sources"], m["sources"]);

    // topics: the video is the words' (an unknown transcript here), or the one named;
    // both videos' clocks start at 0, so source times alone are ambiguous
    let e = p
        .try_human(
            "topic_set",
            json!({"start": word("r9", w_at(10)), "end": word("r9", w_at(20)), "title": "?"}),
        )
        .unwrap_err();
    assert!(reason(&e).contains("several videos"), "{e:?}");
    p.human(
        "topic_set",
        json!({"cut": t.main, "media": {"source": t.a}, "start": word("ra", w_at(10)), "end": word("ra", w_at(60)), "title": "Setup"}),
    );
    let tb = p.human(
        "topic_set",
        json!({"media": {"source": t.b}, "start": word("rb", w_at(10)), "end": word("rb", w_at(60)), "title": "The phone take"}),
    );
    let m = p.human("project_map", json!({})).output;
    let topics = m["stream"]["topics"].as_array().unwrap();
    assert_eq!(
        topics.len(),
        2,
        "the same seconds of two videos do not overlap"
    );
    assert_eq!(topics[0]["title"], "Setup");
    assert_eq!(topics[1]["id"], tb.output["topic"]);
    assert_eq!(topics[1]["media"]["source"], t.b.as_str());
}

#[test]
fn a_long_form_from_topics_of_two_videos() {
    let t = two("ms-long");
    let p = &t.p;
    let ta = p.human(
        "topic_set",
        json!({"media": {"source": t.a}, "start": word("ra", w_at(10)), "end": word("ra", w_at(60)), "title": "Setup"}),
    );
    let tb = p.human(
        "topic_set",
        json!({"media": {"source": t.b}, "start": word("rb", w_at(30)), "end": word("rb", w_at(90)), "title": "The phone take"}),
    );
    let lf = p
        .human(
            "longform_from_topics",
            json!({"topics": [tb.output["topic"], ta.output["topic"]], "order": "source"}),
        )
        .output;
    let long = lf["cut"].as_str().unwrap().to_owned();
    let ps = parts(p, &long);
    assert_eq!(ps.len(), 2);
    assert_eq!(
        src(&ps[0].media),
        Some(t.a.clone()),
        "stream order: the first video first"
    );
    assert_eq!(src(&ps[1].media), Some(t.b.clone()));
    assert_eq!(lf["chapters"][0]["title"], "Setup");
    assert_eq!(lf["chapters"][1]["title"], "The phone take");
    let d = p.h.snapshot().doc.clone();
    let c = d.get(uid::<Cut>(&long)).unwrap();
    assert_eq!(c.lineage.forked_from.unwrap().0.to_string(), t.main);
    assert!(c.brief.contains("2 videos"), "{}", c.brief);
    // a strike of the phone's words on the stream edit shows as a parent edit and pulls in
    p.human(
        "words_strike",
        json!({"cut": t.main, "media": {"source": t.b}, "words": [word("rb", w_at(40)), word("rb", w_at(45))]}),
    );
    let diff = p.human("cut_parent_diff", json!({"cut": long})).output;
    assert_eq!(diff["edits"].as_array().unwrap().len(), 1, "{diff}");
    assert_eq!(diff["edits"][0]["media"]["source"], t.b.as_str());
    let m = p.human("project_map", json!({})).output;
    assert_eq!(m["longs"][0]["parent_edits"], 1);
    assert_eq!(m["longs"][0]["media"].as_array().unwrap().len(), 2);
    p.human("cut_pull_from_parent", json!({"cut": long}));
    assert_eq!(
        parts(p, &long).len(),
        3,
        "the phone stretch split around the strike"
    );
    let diff = p.human("cut_parent_diff", json!({"cut": long})).output;
    assert_eq!(diff["edits"].as_array().unwrap().len(), 0);
}

struct NoFonts;
impl PlanHost for NoFonts {
    fn font_path(&self, _: &Font) -> Option<String> {
        None
    }
}

#[test]
fn a_short_mixing_videos_strikes_and_plans_per_video() {
    let t = two("ms-short");
    let p = &t.p;
    let cut = p.human("cut_create", json!({"title": "Two takes"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("caption_set", json!({"cut": cut, "enabled": false}));
    let framing = id(&p.human(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    ));
    p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0.25, "y": 0, "w": 0.5, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    // the same seconds of both videos, one after the other
    let p1 = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": t.a}, "src_in": 10 * SEC, "src_out": 14 * SEC, "role": "hook"}),
    ));
    // the desk part is linked to a layout made for the desk's 2560×1440 picture
    let lay = id(&p.human("layout_create", json!({"cut": cut, "name": "desk"})));
    p.human(
        "layout_edit",
        json!({"cut": cut, "ops": [
            {"op": "layer_add", "owner": {"layout": lay}, "role": {"region": "screen"},
             "crop": {"x": 0.25, "y": 0, "w": 0.5, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}, "z": 0},
            {"op": "layout_link", "part": p1, "framing": framing, "layout": lay}]}),
    );
    let p2 = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": t.b}, "src_in": 10 * SEC, "src_out": 14 * SEC, "after": p1}),
    ));
    // the phone's part does not take the desk's look (other picture size); a second desk
    // part does
    let d = p.h.snapshot().doc.clone();
    assert!(d.get(uid::<Part>(&p2)).unwrap().look.is_empty());
    let p3 = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": t.a}, "src_in": 20 * SEC, "src_out": 21 * SEC, "after": p1}),
    ));
    let d = p.h.snapshot().doc.clone();
    assert!(!d.get(uid::<Part>(&p3)).unwrap().look.is_empty());
    p.human("part_delete", json!({"part": p3}));
    // a strike must say which video: the same seconds are in both
    let e = p
        .try_human(
            "words_strike",
            json!({"cut": cut, "words": [word("rx", w_at(11)), word("rx", w_at(12))]}),
        )
        .unwrap_err();
    assert!(reason(&e).contains("several videos"), "{e:?}");
    p.human(
        "words_strike",
        json!({"cut": cut, "media": {"source": t.b}, "words": [word("rb", w_at(11)), word("rb", w_at(12))]}),
    );
    let ps = parts(p, &cut);
    assert_eq!(ps.len(), 3, "only the phone's part split");
    assert_eq!(src(&ps[0].media), Some(t.a.clone()));
    assert_eq!((ps[0].src_in.0, ps[0].src_out.0), (10 * SEC, 14 * SEC));
    // restore grows the phone's part back, not the desk's
    p.human(
        "words_restore",
        json!({"cut": cut, "media": {"source": t.b}, "words": [word("rb", w_at(11)), word("rb", w_at(12))]}),
    );
    let ps = parts(p, &cut);
    assert_eq!(ps[0].src_out.0, 14 * SEC, "the desk part untouched");
    assert!(ps[1..].iter().all(|x| src(&x.media) == Some(t.b.clone())));

    // the plan: each part's picture from its own file, the crop in that file's pixels
    let d = p.h.snapshot().doc.clone();
    let pl = planner::plan_cut(&d, uid(&cut), uid(&framing), &NoFonts).unwrap();
    let src_of = |i: usize| {
        let sid = pl.parts[i].picture.source.as_str();
        pl.sources
            .iter()
            .find(|s| s.id.as_str() == sid)
            .unwrap()
            .path
            .clone()
    };
    assert_eq!(src_of(0), "/rec/desk.mp4");
    assert_eq!(src_of(pl.parts.len() - 1), "/rec/phone.mp4");
    assert_eq!(pl.parts[0].picture.first_frame, 300);
    let crop0 = &pl.parts[0].layers[0].crop;
    let crop1 = &pl.parts[pl.parts.len() - 1].layers[0].crop;
    assert_eq!((crop0.x, crop0.w), (640.0, 1280.0), "of 2560 px");
    assert_eq!((crop1.x, crop1.w), (270.0, 540.0), "of 1080 px");

    // the map: one short, two videos
    let m = p.human("project_map", json!({})).output;
    assert_eq!(m["shorts"][0]["media"].as_array().unwrap().len(), 2);
    assert_eq!(m["sources"][1]["used_by"][0], cut.as_str());
}

#[test]
fn removing_a_video_is_refused_while_a_cut_plays_it() {
    let t = two("ms-remove");
    let p = &t.p;
    let cut = p.human("cut_create", json!({"title": "Phone bit"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let part = id(&p.human(
        "part_add",
        json!({"cut": cut, "media": {"source": t.b}, "src_in": 0, "src_out": 2 * SEC}),
    ));
    p.human(
        "topic_set",
        json!({"media": {"source": t.b}, "start": word("rb", w_at(30)), "end": word("rb", w_at(90)), "title": "Phone topic"}),
    );
    p.human(
        "moment_add",
        json!({"media": {"source": t.b}, "start_s": 1.0, "end_s": 5.0, "quote": "a phone moment", "score": 3, "type": "result", "why": "test"}),
    );
    let e = p
        .try_human("source_remove", json!({"source": t.b}))
        .unwrap_err();
    assert!(reason(&e).contains("«Phone bit»"), "{e:?}");
    p.human("part_delete", json!({"part": part}));
    let r = p.human("source_remove", json!({"source": t.b})).output;
    assert_eq!(r["stream_edits"][0], t.main.as_str());
    assert_eq!(r["moments"], 1);
    // only the phone's stretch left the stream edit
    let ps = parts(p, &t.main);
    assert_eq!(ps.len(), 1);
    assert_eq!(src(&ps[0].media), Some(t.a.clone()));
    let m = p.human("project_map", json!({})).output;
    assert_eq!(m["sources"].as_array().unwrap().len(), 1);
    assert_eq!(m["stream"]["videos"].as_array().unwrap().len(), 1);
    assert!(m["stream"]["topics"].as_array().unwrap().is_empty());
    // one undo brings the video, its stretch and its topic back
    p.human("undo", json!({}));
    let m = p.human("project_map", json!({})).output;
    assert_eq!(m["sources"].as_array().unwrap().len(), 2);
    assert_eq!(m["stream"]["topics"][0]["title"], "Phone topic");
    assert_eq!(parts(p, &t.main).len(), 2);
}

#[test]
fn a_same_recording_pair_is_one_video() {
    let p = open("ms-pair");
    let a = source(&p, "proxy", 600, 1280, 720);
    let hi = source(&p, "hires", 600, 2560, 1440);
    let other = source(&p, "other", 120, 1920, 1080);
    let g = p.human(
        "sync_group_create",
        json!({"members": [{"source": a, "role": "audio"}, {"source": hi, "role": "picture"}]}),
    );
    let group = g.output["group"].as_str().unwrap().to_owned();
    let se = p.human("stream_edit_ensure", json!({"media": {"group": group}}));
    let so = p.human("stream_edit_ensure", json!({"media": {"source": other}}));
    assert_eq!(se.output["cut"], so.output["cut"]);
    let m = p.human("project_map", json!({})).output;
    let src = m["sources"].as_array().unwrap();
    assert_eq!(src[0]["media"]["group"], group.as_str());
    assert_eq!(src[1]["group_role"], "picture");
    assert_eq!(src[0]["stream"], se.output["cut"]);
    assert_eq!(
        src[1]["stream"], se.output["cut"],
        "one stream edit for the pair"
    );
    assert_eq!(src[2]["stream"], so.output["cut"]);
    // removing one of the pair dissolves the group (the other stays a video of its own)
    let r = p.human("source_remove", json!({"source": hi})).output;
    assert_eq!(r["group"], group.as_str());
    let m = p.human("project_map", json!({})).output;
    let src = m["sources"].as_array().unwrap();
    assert_eq!(src.len(), 2);
    assert_eq!(src[0]["media"]["source"], a.as_str());
    assert!(
        src[0]["stream"].is_null(),
        "the app appends it to the stream edit as a video of its own"
    );
    assert_eq!(
        src[1]["stream"], so.output["cut"],
        "the other video stays on it"
    );
}
