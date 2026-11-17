//! Decision 0015: comments pinned to a source time (they follow the picture through
//! reorders and trims), threads, resolve; who may write what (agents only reply, links
//! write as themselves, suggestions only from a link); suggested edits applied only on
//! the human's accept, one undo step each; thumbnail versions, picked by the human.

mod common;

use common::{Project, SEC, fixture, id, open};
use gmclip_core::commands::comment::{locate, src_at};
use gmclip_core::{CallOk, CallRequest, Caller, CmdError, Scope};
use gmclip_model::entities::{
    Comment, Cut, EditProposal, Framing, MediaRef, Part, ProposalState, SrcAt, Thumbnail,
};
use gmclip_model::{Flicks, Id, Ulid};
use gmclip_proto::CallerKind;
use serde_json::{Value, json};

fn link(p: &Project, cmd: &str, args: Value) -> Result<CallOk, CmdError> {
    p.clock.advance(1);
    p.h.call(CallRequest {
        command: cmd.into(),
        args,
        base: None,
        request_id: None,
        caller: Caller {
            identity: "review-link:0123456789abcdef".into(),
            connection: "review:test".into(),
            kind: CallerKind::System,
            scopes: vec![Scope::Read, Scope::Edit, Scope::Admin],
            run: None,
            cut_binding: None,
        },
    })
}

fn ulid<E>(s: &str) -> Id<E> {
    Id::new(Ulid::parse(s).unwrap())
}

fn at(source: &str, t: i64) -> Value {
    json!({"media": {"source": source}, "at_src": t})
}

fn comment(p: &Project, cid: &str) -> Comment {
    p.h.snapshot()
        .doc
        .get(ulid::<Comment>(cid))
        .unwrap()
        .clone()
}

fn parts(p: &Project, cut: &str) -> Vec<(i64, i64)> {
    let d = p.h.snapshot().doc.clone();
    d.children::<Part>(ulid::<Cut>(cut).entity())
        .filter_map(|i| d.get(i).map(|x| (x.src_in.0, x.src_out.0)))
        .collect()
}

#[test]
fn a_comment_follows_its_picture_through_reorders_and_trims() {
    let p = open("comment-follow");
    let fx = fixture(&p, 3); // parts at 100–102, 103–105, 106–108 s
    let cut = ulid::<Cut>(&fx.cut);
    // a comment on the second part, 0.5 s in (cut time 2.5 s)
    let doc = p.h.snapshot().doc.clone();
    let pin = src_at(&doc, cut, 2 * SEC + SEC / 2).unwrap();
    assert_eq!(pin.at_src, Flicks(103 * SEC + SEC / 2));
    assert_eq!(pin.part, Some(ulid::<Part>(&fx.parts[1])));
    let c = id(&p.human(
        "comment_add",
        json!({"cut": fx.cut, "text": "the prompt is unreadable", "at": serde_json::to_value(pin).unwrap(),
               "frame_box": {"x": 0.1, "y": 0.2, "w": 0.5, "h": 0.2}}),
    ));
    let pin: SrcAt = comment(&p, &c).at.unwrap();
    assert_eq!(
        locate(&p.h.snapshot().doc, cut, &pin).unwrap().1,
        2 * SEC + SEC / 2
    );
    // move that part first: the comment moves with it
    p.human("part_reorder", json!({"part": fx.parts[1]}));
    let (part, t) = locate(&p.h.snapshot().doc, cut, &pin).unwrap();
    assert_eq!((part.0.to_string(), t), (fx.parts[1].clone(), SEC / 2));
    // trim the part's head by 0.3 s (on the frame grid): still on the same picture, 0.3 s earlier in the cut
    p.human(
        "part_trim",
        json!({"part": fx.parts[1], "src_in": 103 * SEC + SEC * 3 / 10}),
    );
    assert_eq!(locate(&p.h.snapshot().doc, cut, &pin).unwrap().1, SEC / 5);
    // trim it out: the comment is kept and reads "not in the cut"
    p.human(
        "part_trim",
        json!({"part": fx.parts[1], "src_in": 104 * SEC}),
    );
    assert!(locate(&p.h.snapshot().doc, cut, &pin).is_none());
    assert_eq!(comment(&p, &c).text, "the prompt is unreadable");
    // the box is stored as drawn
    assert_eq!(comment(&p, &c).frame_box.unwrap().w, 0.5);
}

#[test]
fn threads_replies_and_resolve_who_may_do_what() {
    let p = open("comment-roles");
    let fx = fixture(&p, 2);
    // a link opens a thread: attributed to the link, never `human`
    let t = id(&link(
        &p,
        "comment_add",
        json!({"cut": fx.cut, "text": "<b>too</b> jumpy", "at": at(&fx.source, 101 * SEC), "export_ref": "a1b2c3d4e5f60718"}),
    )
    .unwrap());
    let c = comment(&p, &t);
    assert_eq!(c.author, "review-link:0123456789abcdef");
    assert_eq!(c.text, "<b>too</b> jumpy");
    // the human replies; an agent replies (what it changed); an agent cannot open one
    let r = id(&p.human(
        "comment_add",
        json!({"cut": fx.cut, "reply_to": t, "text": "on it"}),
    ));
    assert_eq!(comment(&p, &r).author, "human");
    let a = p
        .agent(
            "a",
            "comment_add",
            json!({"cut": fx.cut, "reply_to": t, "text": "Trimmed part 1 by 0.4 s."}),
            p.as_of(),
        )
        .unwrap();
    assert_eq!(comment(&p, &id(&a)).author, "local:claude-code");
    let e = p
        .agent(
            "a",
            "comment_add",
            json!({"cut": fx.cut, "text": "looks great", "at": at(&fx.source, 101 * SEC)}),
            p.as_of(),
        )
        .unwrap_err();
    assert!(matches!(e, CmdError::Unauthorized { .. }), "{e:?}");
    // replies nest one level only, and sit on their thread's time
    assert!(
        p.try_human(
            "comment_add",
            json!({"cut": fx.cut, "reply_to": r, "text": "x"})
        )
        .is_err()
    );
    assert!(
        p.try_human(
            "comment_add",
            json!({"cut": fx.cut, "reply_to": t, "text": "x", "at": at(&fx.source, 100 * SEC)})
        )
        .is_err()
    );
    // resolve: the agent may not; the link and the human may (and reopen)
    let e = p
        .agent(
            "a",
            "comment_resolve",
            json!({"comment": t, "resolved": true}),
            p.as_of(),
        )
        .unwrap_err();
    assert!(matches!(e, CmdError::Unauthorized { .. }));
    link(
        &p,
        "comment_resolve",
        json!({"comment": t, "resolved": true}),
    )
    .unwrap();
    assert!(comment(&p, &t).resolved);
    p.human("comment_resolve", json!({"comment": t, "resolved": false}));
    assert!(!comment(&p, &t).resolved);
    // a reply is not resolved on its own
    assert!(
        p.try_human("comment_resolve", json!({"comment": r, "resolved": true}))
            .is_err()
    );
    // empty and oversized text are refused; a bad box is refused
    assert!(
        p.try_human("comment_add", json!({"cut": fx.cut, "text": "  "}))
            .is_err()
    );
    assert!(
        p.try_human(
            "comment_add",
            json!({"cut": fx.cut, "text": "x".repeat(2001)})
        )
        .is_err()
    );
    assert!(
        p.try_human(
            "comment_add",
            json!({"cut": fx.cut, "text": "x", "frame_box": {"x": 0.8, "y": 0, "w": 0.5, "h": 0.1}})
        )
        .is_err()
    );
    // only the human deletes; a thread takes its replies with it
    assert!(link(&p, "comment_delete", json!({"comment": t})).is_err());
    p.human("comment_delete", json!({"comment": t}));
    assert_eq!(p.h.snapshot().doc.iter::<Comment>().count(), 0);
    p.human("undo", json!({}));
    assert_eq!(p.h.snapshot().doc.iter::<Comment>().count(), 3);
}

#[test]
fn suggestions_come_from_a_link_and_apply_only_on_accept() {
    let p = open("comment-suggest");
    let fx = fixture(&p, 3);
    let before = parts(&p, &fx.cut);
    let trim = json!({"op": "trim", "part": fx.parts[0], "src_in": 100 * SEC + SEC / 2, "src_out": 102 * SEC});
    // not from an agent, not from the human, not from a plain system caller
    let e = p
        .agent(
            "a",
            "edit_suggest",
            json!({"cut": fx.cut, "edit": trim}),
            p.as_of(),
        )
        .unwrap_err();
    assert!(matches!(e, CmdError::Unauthorized { .. }));
    assert!(
        p.try_human("edit_suggest", json!({"cut": fx.cut, "edit": trim}))
            .is_err()
    );
    let t = id(&link(
        &p,
        "comment_add",
        json!({"cut": fx.cut, "text": "start on the result", "at": at(&fx.source, 100 * SEC)}),
    )
    .unwrap());
    let s1 = id(&link(
        &p,
        "edit_suggest",
        json!({"cut": fx.cut, "edit": trim, "note": "cut the breath", "comment": t}),
    )
    .unwrap());
    let s2 = id(&link(
        &p,
        "edit_suggest",
        json!({"cut": fx.cut, "edit": {"op": "strike", "media": {"source": fx.source}, "start": 103 * SEC, "end": 103 * SEC + SEC / 2, "words": "um so"}}),
    )
    .unwrap());
    let s3 = id(&link(
        &p,
        "edit_suggest",
        json!({"cut": fx.cut, "edit": {"op": "caption_move", "framing": fx.framing, "place": {"x": 0.5, "y": 0.55, "w": 0.9}}}),
    )
    .unwrap());
    // nothing applied yet
    assert_eq!(parts(&p, &fx.cut), before);
    let d = p.h.snapshot().doc.clone();
    assert_eq!(
        d.iter::<EditProposal>()
            .filter(|x| x.state == ProposalState::Proposed)
            .count(),
        3
    );
    // a stretch no part plays is refused at suggestion time
    assert!(
        link(
            &p,
            "edit_suggest",
            json!({"cut": fx.cut, "edit": {"op": "strike", "media": {"source": fx.source}, "start": 50 * SEC, "end": 51 * SEC, "words": "x"}}),
        )
        .is_err()
    );
    // an agent cannot decide them
    assert!(
        p.agent(
            "a",
            "edit_proposal_state",
            json!({"proposals": [s1], "state": "accepted"}),
            p.as_of()
        )
        .is_err()
    );
    // the human accepts the trim: applied, one undo step undoes it and the decision
    p.human(
        "edit_proposal_state",
        json!({"proposals": [s1], "state": "accepted"}),
    );
    assert_eq!(parts(&p, &fx.cut)[0], (100 * SEC + SEC / 2, 102 * SEC));
    p.human("undo", json!({}));
    assert_eq!(parts(&p, &fx.cut), before);
    let st = |id: &str| {
        p.h.snapshot()
            .doc
            .get(ulid::<EditProposal>(id))
            .unwrap()
            .state
    };
    assert_eq!(st(&s1), ProposalState::Proposed);
    p.human("redo", json!({}));
    assert_eq!(st(&s1), ProposalState::Accepted);
    // strike: the second part loses its first half second
    p.human(
        "edit_proposal_state",
        json!({"proposals": [s2], "state": "accepted"}),
    );
    assert_eq!(parts(&p, &fx.cut)[1], (103 * SEC + SEC / 2, 105 * SEC));
    // reject the caption move: nothing changes, state rejected; deciding again is a no-op
    p.human(
        "edit_proposal_state",
        json!({"proposals": [s3], "state": "rejected"}),
    );
    assert_eq!(st(&s3), ProposalState::Rejected);
    let f =
        p.h.snapshot()
            .doc
            .get(ulid::<Framing>(&fx.framing))
            .unwrap()
            .caption_place;
    assert!(f.is_none_or(|pl| (pl.y - 0.55).abs() > 1e-9));
    let again = p.human(
        "edit_proposal_state",
        json!({"proposals": [s3], "state": "accepted"}),
    );
    assert_eq!(again.output["decided"], 0);
}

#[test]
fn caption_move_and_a_gone_part_on_accept() {
    let p = open("comment-caption");
    let fx = fixture(&p, 2);
    let s = id(&link(
        &p,
        "edit_suggest",
        json!({"cut": fx.cut, "edit": {"op": "caption_move", "framing": fx.framing, "part": fx.parts[1], "place": {"x": 0.5, "y": 0.4, "w": 0.9}}}),
    )
    .unwrap());
    p.human(
        "edit_proposal_state",
        json!({"proposals": [s], "state": "accepted"}),
    );
    let d = p.h.snapshot().doc.clone();
    let part = d.get(ulid::<Part>(&fx.parts[1])).unwrap();
    assert_eq!(
        part.caption_place_override
            .get(&ulid::<Framing>(&fx.framing))
            .unwrap()
            .y,
        0.4
    );
    // a trim of a part the human has since deleted: refused, nothing changes
    let s2 = id(&link(
        &p,
        "edit_suggest",
        json!({"cut": fx.cut, "edit": {"op": "trim", "part": fx.parts[0], "src_in": 100 * SEC, "src_out": 101 * SEC}}),
    )
    .unwrap());
    p.human("part_delete", json!({"part": fx.parts[0]}));
    let e = p
        .try_human(
            "edit_proposal_state",
            json!({"proposals": [s2], "state": "accepted"}),
        )
        .unwrap_err();
    assert!(matches!(e, CmdError::Invalid { .. }), "{e:?}");
    assert_eq!(
        p.h.snapshot()
            .doc
            .get(ulid::<EditProposal>(&s2))
            .unwrap()
            .state,
        ProposalState::Proposed
    );
}

#[test]
fn thumbnails_are_versions_the_human_picks() {
    let p = open("comment-thumbs");
    let fx = fixture(&p, 2);
    // an agent's text-hook pass: three options, the first in use, not picked
    let a = id(&p
        .agent(
            "a",
            "thumbnail_add",
            json!({"cut": fx.cut, "cover": at(&fx.source, 101 * SEC),
                   "hook_options": ["My PC made this in 67 seconds", "No cloud, 67 s", "5080 vs a video model"],
                   "notes": "the number reads best"}),
            p.as_of(),
        )
        .unwrap());
    let d = p.h.snapshot().doc.clone();
    let t = d.get(ulid::<Thumbnail>(&a)).unwrap();
    assert_eq!(t.title, "My PC made this in 67 seconds");
    assert!(!t.picked);
    assert_eq!(t.author, "local:claude-code");
    // an agent cannot pick, set, or offer four
    assert!(
        p.agent(
            "a",
            "thumbnail_add",
            json!({"cut": fx.cut, "cover": at(&fx.source, 101 * SEC), "pick": true}),
            p.as_of()
        )
        .is_err()
    );
    assert!(
        p.agent(
            "a",
            "thumbnail_set",
            json!({"thumbnail": a, "pick": true}),
            p.as_of()
        )
        .is_err()
    );
    assert!(
        p.agent(
            "a",
            "thumbnail_add",
            json!({"cut": fx.cut, "cover": at(&fx.source, 101 * SEC), "hook_options": ["a", "b", "c", "d"]}),
            p.as_of()
        )
        .is_err()
    );
    // the human's own version, picked; then picks the agent's with its second option
    let h = id(&p.human(
        "thumbnail_add",
        json!({"cut": fx.cut, "cover": at(&fx.source, 103 * SEC), "title": "67 s", "pick": true}),
    ));
    p.human(
        "thumbnail_set",
        json!({"thumbnail": a, "pick": true, "title": "No cloud, 67 s"}),
    );
    let d = p.h.snapshot().doc.clone();
    assert!(d.get(ulid::<Thumbnail>(&a)).unwrap().picked);
    assert_eq!(
        d.get(ulid::<Thumbnail>(&a)).unwrap().title,
        "No cloud, 67 s"
    );
    assert!(!d.get(ulid::<Thumbnail>(&h)).unwrap().picked);
    assert_eq!(d.iter::<Thumbnail>().count(), 2, "every version is kept");
    // a bad colour or size is refused
    assert!(
        p.try_human(
            "thumbnail_add",
            json!({"cut": fx.cut, "cover": at(&fx.source, 101 * SEC), "style": {"font": "", "size": 0.9, "fill": "#fff", "outline": "#000000", "accent": "#FFD400", "uppercase": false}})
        )
        .is_err()
    );
    // a cover on media the project lacks is refused
    let other = MediaRef::Source(Id::new(Ulid(12345)));
    assert!(
        p.try_human(
            "thumbnail_add",
            json!({"cut": fx.cut, "cover": {"media": other, "at_src": 0}})
        )
        .is_err()
    );
}
