//! Decision 0024, title and description drafts: an agent's drafts are proposals (never
//! accepted, published or approved by it), checked per item: limits per platform, one
//! line titles, no links, and only numbers the cut backs (said in it or measured). The
//! human accepts (one per platform), edits or rejects.

mod common;

use common::{Project, SEC, fixture, open};
use gmclip_model::entities::{CopyDraft, EditOrigin, ProposalState};
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};

fn words(p: &Project, owner: &str, ws: &[(&str, f64, f64)]) {
    let rev = "asr-test@1";
    let words: Vec<Value> = ws
        .iter()
        .map(|(t, s, e)| json!({"text": t, "start": (s * SEC as f64) as i64, "end": (e * SEC as f64) as i64}))
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": words});
    p.h.put_analysis(owner, "words", rev, body.to_string().into_bytes())
        .unwrap();
}

fn draft(p: &Project, id: &str) -> CopyDraft {
    p.h.snapshot()
        .doc
        .get(Id::<CopyDraft>::new(Ulid::parse(id).unwrap()))
        .unwrap()
        .clone()
}

#[test]
fn drafts_are_proposals_checked_per_item_and_the_human_decides() {
    let p = open("copy");
    let fx = fixture(&p, 1);
    words(
        &p,
        &fx.source,
        &[
            ("My", 100.1, 100.2),
            ("5080", 100.3, 100.6),
            ("made", 100.7, 100.8),
            ("it", 100.85, 100.9),
            ("in", 100.95, 101.0),
            ("two", 101.05, 101.2),
            ("minutes.", 101.25, 101.6),
        ],
    );
    p.human(
        "measurement_add",
        json!({"kind": "interval", "from": {"frame": {"source": fx.source, "frame_index": 10}},
               "to": {"frame": {"source": fx.source, "frame_index": 2032}}, "value": 67.4, "unit": "s",
               "evidence_frames": [], "method": "marked", "author": "local:human"}),
    );
    let out = p
        .agent(
            "a",
            "copy_drafts_add",
            json!({"cut": fx.cut, "pass_ref": "publish_copy@1:abc/cp_1", "drafts": [
                {"platform": "youtube", "title": "My 5080 made this in 67 seconds", "description": "Start to finish in 2 minutes, on one GPU.\n#ai #video"},
                {"platform": "shorts", "title": "It took 10 minutes", "description": "Local video model."},
                {"platform": "tiktok", "title": "Watch my GPU make this", "description": "More at https://example.com"},
                {"platform": "instagram", "title": "Line one\nline two", "description": "x"},
                {"platform": "instagram", "title": "One GPU, 1:07 start to finish", "description": "Measured on screen."}
            ]}),
            p.as_of(),
        )
        .unwrap()
        .output;
    let refused: Vec<(u64, String)> = out["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["index"].as_u64().unwrap(),
                r["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(refused.len(), 3, "{out}");
    assert_eq!(refused[0].0, 1);
    assert!(refused[0].1.contains("number 10"), "{refused:?}");
    assert!(refused[1].1.contains("links"));
    assert!(refused[2].1.contains("one line"));
    let ids: Vec<String> = out["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 2);
    let yt = draft(&p, &ids[0]);
    assert_eq!(
        (yt.state, yt.origin, yt.edited),
        (ProposalState::Proposed, EditOrigin::Agent, false)
    );
    assert_eq!(yt.pass_ref.as_deref(), Some("publish_copy@1:abc/cp_1"));

    // the agent cannot accept its own draft
    let e = p
        .agent(
            "a",
            "copy_draft_state",
            json!({"draft": ids[0], "state": "accepted"}),
            p.as_of(),
        )
        .unwrap_err();
    assert_eq!(e.code(), "Unauthorized");

    // the human accepts one, edits the other (an edit accepts), and a second youtube
    // draft accepted puts the first back to proposed
    p.human(
        "copy_draft_state",
        json!({"draft": ids[0], "state": "accepted"}),
    );
    p.human(
        "copy_draft_state",
        json!({"draft": ids[1], "title": "One GPU, 1:07, start to finish"}),
    );
    let ig = draft(&p, &ids[1]);
    assert_eq!((ig.state, ig.edited), (ProposalState::Accepted, true));
    assert_eq!(ig.title, "One GPU, 1:07, start to finish");
    let mine = p
        .human(
            "copy_drafts_add",
            json!({"cut": fx.cut, "drafts": [{"platform": "youtube", "title": "Ten GPUs, 10 minutes (my words)", "description": "Mine, see https://example.com"}]}),
        )
        .output;
    // the human's own numbers and links are theirs
    assert!(mine["refused"].as_array().unwrap().is_empty(), "{mine}");
    let own = mine["ids"][0].as_str().unwrap().to_owned();
    assert_eq!(draft(&p, &own).origin, EditOrigin::Human);
    p.human(
        "copy_draft_state",
        json!({"draft": own, "state": "accepted"}),
    );
    assert_eq!(draft(&p, &ids[0]).state, ProposalState::Proposed);
    p.human(
        "copy_draft_state",
        json!({"draft": ids[0], "state": "rejected"}),
    );
    let list = p.human("copy_drafts_list", json!({"cut": fx.cut})).output;
    let states: Vec<(&str, &str)> = list["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["platform"].as_str().unwrap(),
                d["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("youtube", "accepted"),
            ("instagram", "accepted"),
            ("youtube", "rejected")
        ]
    );
    assert_eq!(list["platforms"].as_array().unwrap().len(), 4);
    assert_eq!(list["platforms"][2]["description_max"], 2200);
    // limits
    let long = "x".repeat(101);
    let e = p
        .try_human("copy_draft_state", json!({"draft": ids[1], "title": long}))
        .unwrap_err();
    assert_eq!(e.code(), "Invalid");
    // one undo step per decision
    p.human("undo", json!({}));
    assert_eq!(draft(&p, &ids[0]).state, ProposalState::Proposed);
}
