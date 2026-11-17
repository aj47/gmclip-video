//! Regressions for the step-4 adversarial review, one test per confirmed finding.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::{CallRequest, Caller, CmdError};
use gmclip_model::entities::{Part, VisualSplit};
use gmclip_model::{Id, Ulid};
use gmclip_proto::Base;
use serde_json::json;

fn pid(s: &str) -> Id<Part> {
    Id::new(Ulid::parse(s).unwrap())
}

/// 1: an undo that commits nothing must not move the stacks; a later redo must not
/// revert someone else's transaction.
#[test]
fn an_empty_undo_leaves_the_stacks_alone() {
    let p = open("r1");
    let fx = fixture(&p, 2);
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 5}));
    p.agent(
        "a",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 7}),
        p.as_of(),
    )
    .unwrap();
    let r = p.human("undo", json!({"option": "revert_untouched"}));
    assert_eq!(r.committed, None);
    p.agent(
        "a",
        "part_set_gain",
        json!({"part": fx.parts[1], "gain_db": 3}),
        p.as_of(),
    )
    .unwrap();
    assert!(p.try_human("redo", json!({})).is_err(), "nothing to redo");
    let d = p.h.snapshot().doc.clone();
    assert_eq!(d.get(pid(&fx.parts[1])).unwrap().gain_db, 3.0);
    p.h.close().unwrap();
}

/// 2: undoing a split whose b-side has gained another author's visual split must not
/// orphan it: the delete conflicts.
#[test]
fn undo_never_orphans_children() {
    let p = open("r2");
    let fx = fixture(&p, 1);
    let b = id(&p.human("part_split", json!({"part": fx.parts[0], "at": 101 * SEC})));
    p.agent(
        "a",
        "visual_split_add",
        json!({"part": b, "framing": fx.framing, "at": 101 * SEC + SEC / 2, "overrides": {}}),
        p.as_of(),
    )
    .unwrap();
    assert!(matches!(
        p.try_human("undo", json!({})),
        Err(CmdError::Conflict(_))
    ));
    let r = p.human("undo", json!({"option": "revert_untouched"}));
    let d = p.h.snapshot().doc.clone();
    for v in d.iter::<VisualSplit>() {
        assert!(d.get(v.part).is_some(), "orphaned visual split");
    }
    let _ = r;
    p.h.close().unwrap();
}

/// 3: a request id reused for a different call is refused, not answered with the
/// first call's result.
#[test]
fn a_request_id_reused_for_another_call_is_refused() {
    let p = open("r3");
    let fx = fixture(&p, 1);
    let call = |cmd: &str, args: serde_json::Value| {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: cmd.into(),
            args,
            base: Some(Base::AsOf {
                as_of: u64::MAX >> 1,
            }),
            request_id: Some("1".into()),
            caller: Caller::agent("local:claude-code", "c1"),
        })
    };
    call("part_set_gain", json!({"part": fx.parts[0], "gain_db": 2})).unwrap();
    match call(
        "part_trim",
        json!({"part": fx.parts[0], "src_out": 101 * SEC}),
    ) {
        Err(CmdError::Invalid { path, .. }) => assert_eq!(path, "/request_id"),
        other => panic!("{other:?}"),
    }
    // the same call again is replayed
    assert!(
        call("part_set_gain", json!({"part": fx.parts[0], "gain_db": 2}))
            .unwrap()
            .replayed
    );
    p.h.close().unwrap();
}

/// 4: an empty per-entity base checks nothing, so it is refused.
#[test]
fn an_empty_base_is_refused() {
    let p = open("r4");
    let fx = fixture(&p, 1);
    let r = p.agent(
        "a",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 2}),
        Some(Base::Revs(Default::default())),
    );
    assert!(
        matches!(r, Err(CmdError::Invalid { ref path, .. }) if path == "/base"),
        "{r:?}"
    );
    p.h.close().unwrap();
}

/// 5 + 6: run undo counts human edits made between the restore point and the run's
/// first write, and another agent cannot undo the run.
#[test]
fn run_undo_sees_edits_since_the_restore_point_and_is_bound_to_its_identity() {
    let p = open("r5");
    let fx = fixture(&p, 1);
    let run = p
        .agent("a", "run_begin", json!({"label": "x"}), p.as_of())
        .unwrap()
        .output["run"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 3}));
    p.agent(
        "a",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 5}),
        p.as_of(),
    )
    .unwrap();
    // another identity is told the run does not exist
    p.clock.advance(1);
    let other = p.h.call(CallRequest {
        command: "run_undo".into(),
        args: json!({"run": run}),
        base: p.as_of(),
        request_id: None,
        caller: Caller::agent("local:other", "c9"),
    });
    assert_eq!(other, Err(CmdError::NotFound));
    match p.try_human("run_undo", json!({"run": run})) {
        Err(CmdError::Conflict(c)) => {
            assert!(c.diff["later_edits"][0].as_str().unwrap().contains("human"))
        }
        other => panic!("the human's earlier edit must be listed: {other:?}"),
    }
    p.h.close().unwrap();
}

/// 7: run_begin is idempotent under a request id and refused to the human.
#[test]
fn run_begin_is_idempotent_and_agent_only() {
    let p = open("r7");
    let _fx = fixture(&p, 1);
    let begin = || {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: "run_begin".into(),
            args: json!({"label": "x"}),
            base: Some(Base::AsOf { as_of: 0 }),
            request_id: Some("rb".into()),
            caller: Caller::agent("local:claude-code", "c1"),
        })
    };
    let a = begin().unwrap();
    let b = begin().unwrap();
    assert!(b.replayed);
    assert_eq!(a.output, b.output);
    assert!(p.try_human("run_begin", json!({"label": "x"})).is_err());
    p.h.close().unwrap();
}

/// 8: overflowing ranges are refused, never wrapped.
#[test]
fn overflowing_ranges_are_refused() {
    let p = open("r8");
    let fx = fixture(&p, 1);
    let r = p.try_human(
        "part_add",
        json!({"cut": fx.cut, "media": {"source": fx.source}, "src_in": 1, "src_out": i64::MIN}),
    );
    assert!(matches!(r, Err(CmdError::Invalid { .. })), "{r:?}");
    let r = p.try_human("part_split", json!({"part": fx.parts[0], "at": i64::MIN}));
    assert!(matches!(r, Err(CmdError::Invalid { .. })), "{r:?}");
    p.h.close().unwrap();
}

/// 9: a hostile `since` does not kill the actor (a `since` past everything published is a
/// resync since R-ARCH-7: it comes from another holder's lifetime).
#[test]
fn watch_at_the_end_of_time_is_harmless() {
    let p = open("r9");
    let fx = fixture(&p, 1);
    assert_eq!(p.h.watch(u64::MAX, 10), None);
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 1}));
    p.h.close().unwrap();
}

/// 10: cross-cut references and out-of-range patch values are refused.
#[test]
fn cross_cut_references_and_bad_patches_are_refused() {
    let p = open("r10");
    let fx = fixture(&p, 1);
    let other = p.human("cut_create", json!({"title": "other"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let f2 = id(&p.human(
        "framing_create",
        json!({"cut": other, "name": "x", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    ));
    let r = p.try_human(
        "layer_add",
        json!({"owner": {"part_override": {"part": fx.parts[0], "framing": f2}}, "role": "other", "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    assert!(matches!(r, Err(CmdError::Invalid { .. })), "{r:?}");
    let ov = id(&p.human(
        "overlay_add",
        json!({"cut": fx.cut, "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 100}, "place": {"x": 0, "y": 0, "w": 1}}),
    ));
    let r = p.try_human(
        "overlay_move",
        json!({"overlay": ov, "framing": f2, "place": {"x": 0, "y": 0, "w": 1}}),
    );
    assert!(matches!(r, Err(CmdError::Invalid { .. })), "{r:?}");
    let r = p.try_human(
        "visual_split_add",
        json!({"part": fx.parts[0], "framing": fx.framing, "at": 100 * SEC, "overrides": {fx.screen.clone(): {"opacity": 5}}}),
    );
    assert!(matches!(r, Err(CmdError::Invalid { .. })), "{r:?}");
    p.h.close().unwrap();
}

/// 11: a split depends on the part's visual splits; one added concurrently by another
/// author after the caller's `as_of` is a conflict.
#[test]
fn a_split_conflicts_on_a_concurrent_visual_split() {
    let p = open("r11");
    let fx = fixture(&p, 1);
    let stale = p.as_of();
    p.human(
        "visual_split_add",
        json!({"part": fx.parts[0], "framing": fx.framing, "at": 101 * SEC, "overrides": {}}),
    );
    let r = p.agent(
        "a",
        "part_split",
        json!({"part": fx.parts[0], "at": 101 * SEC + SEC / 2}),
        stale,
    );
    assert!(matches!(r, Err(CmdError::Conflict(_))), "{r:?}");
    p.h.close().unwrap();
}

/// A launched run writes its own cut (plus project-level maps) and nothing else: not
/// another cut, not a source, not a new cut.
#[test]
fn a_launched_run_is_held_to_its_cut_in_the_actor() {
    let p = open("bind");
    let fx = fixture(&p, 1);
    let other = p.human("cut_create", json!({"title": "other"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut caller = Caller::agent("launched:r1", "launched:r1");
    caller.cut_binding = Some(Id::new(Ulid::parse(&fx.cut).unwrap()));
    let run = |cmd: &str, args: serde_json::Value| {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: cmd.into(),
            args,
            base: p.as_of(),
            request_id: None,
            caller: caller.clone(),
        })
    };
    let refused = |r: Result<gmclip_core::CallOk, CmdError>| matches!(r, Err(CmdError::Unauthorized { ref scope }) if scope == "cut_binding");
    assert!(run("part_set_gain", json!({"part": fx.parts[0], "gain_db": 2})).is_ok());
    assert!(refused(run("cut_set", json!({"cut": other, "title": "x"}))));
    assert!(refused(run(
        "source_add",
        json!({"path": "/x.mkv", "fingerprint": "f"})
    )));
    assert!(refused(run("cut_create", json!({"title": "new"}))));
    p.h.close().unwrap();
}
