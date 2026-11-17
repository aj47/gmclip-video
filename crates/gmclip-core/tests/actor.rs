//! The actor's call pipeline: commands on the model, base checks and conflicts
//! (R-CC-1, R-CMD-6), idempotency (R-CMD-7), scopes, events (§3.10), runs (§3.6),
//! undo/redo and run undo (R-HIST-2/3), reopen.

mod common;

use common::{SEC, fixture, id, open};
use gmclip_core::{CallRequest, Caller, CmdError, Scope};
use gmclip_model::entities::{Part, VisualSplit};
use gmclip_model::{Id, Ulid};
use gmclip_proto::{Base, ConflictOption, Event};
use serde_json::{Value, json};

fn pid(s: &str) -> Id<Part> {
    Id::new(Ulid::parse(s).unwrap())
}

#[test]
fn hot_commands_edit_the_model() {
    let p = open("hot");
    let fx = fixture(&p, 4);
    let doc = || p.h.snapshot().doc.clone();
    // trim, gain
    p.human(
        "part_trim",
        json!({"part": fx.parts[0], "src_out": 101 * SEC}),
    );
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 10}));
    let d = doc();
    let p0 = d.get(pid(&fx.parts[0])).unwrap();
    assert_eq!(p0.src_out.0, 101 * SEC);
    assert_eq!(p0.gain_db, 10.0);
    // split, then merge back
    let b = id(&p.human("part_split", json!({"part": fx.parts[1], "at": 104 * SEC})));
    assert_eq!(doc().get(pid(&b)).unwrap().src_in.0, 104 * SEC);
    p.human("part_merge", json!({"a": fx.parts[1], "b": b}));
    assert!(doc().get(pid(&b)).is_none());
    // reorder: part 3 first
    p.human("part_reorder", json!({"part": fx.parts[3]}));
    let order: Vec<_> = doc()
        .children::<Part>(
            Id::<gmclip_model::entities::Cut>::new(Ulid::parse(&fx.cut).unwrap()).entity(),
        )
        .collect();
    assert_eq!(order[0], pid(&fx.parts[3]));
    // layout set, crop/place override, visual split
    let lay = id(&p.human("layout_create", json!({"cut": fx.cut, "name": "panel"})));
    p.human(
        "layout_set",
        json!({"part": fx.parts[2], "framing": fx.framing, "layout": lay}),
    );
    p.human(
        "layout_set",
        json!({"part": fx.parts[2], "framing": fx.framing}),
    );
    p.human("crop_set", json!({"layer": fx.screen, "part": fx.parts[2], "framing": fx.framing, "crop": {"x": 0.1, "y": 0.1, "w": 0.5, "h": 0.5}}));
    p.human(
        "place_set",
        json!({"layer": fx.cam, "place": {"x": 0.2, "y": 0.6, "w": 0.6}}),
    );
    let vs = id(&p.human("visual_split_add", json!({"part": fx.parts[2], "framing": fx.framing, "at": 106 * SEC + SEC / 2, "overrides": {fx.screen.clone(): {"opacity": 0.5}}})));
    let d = doc();
    assert!(
        d.get(Id::<VisualSplit>::new(Ulid::parse(&vs).unwrap()))
            .is_some()
    );
    let e = gmclip_model::effective(
        &d,
        pid(&fx.parts[2]),
        Id::new(Ulid::parse(&fx.framing).unwrap()),
        gmclip_model::Flicks(107 * SEC),
    )
    .unwrap();
    let screen = e
        .layers
        .iter()
        .find(|l| l.id.0.to_string() == fx.screen)
        .unwrap();
    assert_eq!(screen.values.opacity, 0.5);
    assert_eq!(screen.values.crop.x, 0.1);
    // captions: edit, keyword toggle, word-edge nudge
    let w = json!({"asr_revision": "r1", "ordinal": 7});
    p.human(
        "caption_edit",
        json!({"cut": fx.cut, "word": w, "edit": {"text": "exe.dev"}}),
    );
    assert_eq!(
        p.human("caption_keyword_toggle", json!({"cut": fx.cut, "word": w}))
            .output["keyword"],
        true
    );
    assert_eq!(
        p.human("caption_keyword_toggle", json!({"cut": fx.cut, "word": w}))
            .output["keyword"],
        false
    );
    let wr = json!({"word_id": w, "t_src_start": 106 * SEC, "t_src_end": 106 * SEC + SEC / 2, "normalised_text": "dev"});
    let n = p.human(
        "word_edge_nudge",
        json!({"cut": fx.cut, "word": wr, "edge": "end", "delta_ms": -25}),
    );
    assert_eq!(
        n.output["end"].as_i64().unwrap(),
        106 * SEC + SEC / 2 - 25 * 705_600
    );
    // overlay add + move
    let ov = id(&p.human("overlay_add", json!({"cut": fx.cut, "kind": "text", "start": {"cut_start": {"cut": fx.cut}}, "end": {"dur_ms": 4500}, "place": {"x": 0.5, "y": 0.16, "w": 0.9}})));
    p.human(
        "overlay_move",
        json!({"overlay": ov, "framing": fx.framing, "place": {"x": 0.1, "y": 0.1, "w": 0.5}}),
    );
    // strike a word in the middle of part 2 (106..108 s), then restore it
    let word = json!({"word_id": {"asr_revision": "r1", "ordinal": 9}, "t_src_start": 106 * SEC + SEC / 2, "t_src_end": 107 * SEC, "normalised_text": "um"});
    let s = p.human("words_strike", json!({"cut": fx.cut, "words": [word]}));
    assert_eq!(s.output["parts_changed"], 1);
    let parts_after_strike = doc().count::<Part>();
    assert_eq!(parts_after_strike, 5, "split around the struck word");
    p.human("words_restore", json!({"cut": fx.cut, "words": [word]}));
    p.h.close().unwrap();
}

#[test]
fn agents_need_a_base_and_get_conflicts_on_moved_reads() {
    let p = open("base");
    let fx = fixture(&p, 2);
    let trim = json!({"part": fx.parts[0], "src_out": 101 * SEC});
    // no base → Invalid at /base
    match p.agent("c1", "part_trim", trim.clone(), None) {
        Err(CmdError::Invalid { path, .. }) => assert_eq!(path, "/base"),
        other => panic!("{other:?}"),
    }
    let stale = p.as_of();
    // the human moves the part after the agent's read
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 3}));
    match p.agent("c1", "part_trim", trim.clone(), stale) {
        Err(CmdError::Conflict(c)) => {
            assert_eq!(c.entities.len(), 1);
            assert_eq!(c.entities[0].by, "human");
            assert!(c.entities[0].entity.starts_with("part:"));
        }
        other => panic!("{other:?}"),
    }
    // nothing was written by the refused call
    assert_eq!(
        p.h.snapshot().doc.get(pid(&fx.parts[0])).unwrap().src_out.0,
        102 * SEC
    );
    // a fresh as_of succeeds; per-entity base too
    p.agent("c1", "part_trim", trim, p.as_of()).unwrap();
    let rev =
        p.h.snapshot()
            .doc
            .entity_rev(pid(&fx.parts[1]).entity())
            .unwrap();
    let key = format!("part:{}", fx.parts[1]);
    p.agent(
        "c1",
        "part_set_gain",
        json!({"part": fx.parts[1], "gain_db": 1}),
        Some(Base::Revs([(key.clone(), rev)].into())),
    )
    .unwrap();
    // the same per-entity base is now stale
    assert!(matches!(
        p.agent(
            "c1",
            "part_set_gain",
            json!({"part": fx.parts[1], "gain_db": 2}),
            Some(Base::Revs([(key, rev)].into()))
        ),
        Err(CmdError::Conflict(_))
    ));
    p.h.close().unwrap();
}

#[test]
fn scopes_and_unknown_commands() {
    let p = open("scope");
    let fx = fixture(&p, 1);
    let mut reader = Caller::agent("local:x", "c9");
    reader.scopes = vec![Scope::Read];
    let r = p.h.call(CallRequest {
        command: "part_set_gain".into(),
        args: json!({"part": fx.parts[0], "gain_db": 1}),
        base: p.as_of(),
        request_id: None,
        caller: reader,
    });
    assert_eq!(
        r,
        Err(CmdError::Unauthorized {
            scope: "edit".into()
        })
    );
    assert_eq!(
        p.try_human("no_such_command", json!({})),
        Err(CmdError::NotFound)
    );
    // a bad argument never echoes more than 16 bytes of it
    match p.try_human(
        "part_set_gain",
        json!({"part": "x".repeat(500), "gain_db": 1}),
    ) {
        Err(CmdError::Invalid { path, reason }) => {
            assert!(!reason.contains(&"x".repeat(17)) && !path.contains(&"x".repeat(17)));
        }
        other => panic!("{other:?}"),
    }
    p.h.close().unwrap();
}

#[test]
fn a_repeated_request_returns_the_original_and_keys_do_not_collide() {
    let p = open("idem");
    let fx = fixture(&p, 1);
    let call = |conn: &str, ident: &str, gain: i32| {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: "part_split".into(),
            args: json!({"part": fx.parts[0], "at": 100 * SEC + SEC + gain as i64}),
            base: Some(Base::AsOf {
                as_of: u64::MAX >> 1,
            }),
            request_id: Some("r-1".into()),
            caller: Caller::agent(ident, conn),
        })
    };
    let first = call("c1", "local:claude-code", 0).unwrap();
    let again = call("c1", "local:claude-code", 0).unwrap();
    assert!(again.replayed);
    assert_eq!(again.output, first.output);
    assert_eq!(again.committed, first.committed);
    assert_eq!(p.h.snapshot().doc.count::<Part>(), 2, "not applied twice");
    // another connection of the same client, or another client: its own key space
    // (a split point not yet a part edge, so it would succeed if run)
    let other_conn = call("c2", "local:claude-code", -(SEC as i32) / 2).unwrap();
    assert!(!other_conn.replayed);
    assert_ne!(other_conn.output, first.output);
    // another client replaying the same call under the same request id is refused
    // (R-AGT-5), never answered with the first client's result …
    match call("c1", "local:codex", 0) {
        Err(CmdError::Invalid { path, .. }) => assert_eq!(path, "/request_id"),
        other => panic!("{other:?}"),
    }
    // … while its own different call with that id is its own business
    assert!(
        !call("c1", "local:codex", -3 * (SEC as i32) / 4)
            .unwrap()
            .replayed
    );
    // within 24 h, across a reopen, the original comes back from the store
    let p = p.reopen();
    p.clock.advance(3_600_000);
    let after =
        p.h.call(CallRequest {
            command: "part_split".into(),
            args: json!({"part": fx.parts[0], "at": 100 * SEC + SEC}),
            base: Some(Base::AsOf {
                as_of: u64::MAX >> 1,
            }),
            request_id: Some("r-1".into()),
            caller: Caller::agent("local:claude-code", "c1"),
        })
        .unwrap();
    assert!(after.replayed);
    assert_eq!(after.output, first.output);
    // after 24 h it runs again (and fails: that split point is now a part edge)
    p.clock.advance(24 * 3_600_000);
    let late = p.h.call(CallRequest {
        command: "part_split".into(),
        args: json!({"part": fx.parts[0], "at": 100 * SEC + SEC}),
        base: Some(Base::AsOf {
            as_of: u64::MAX >> 1,
        }),
        request_id: Some("r-1".into()),
        caller: Caller::agent("local:claude-code", "c1"),
    });
    assert!(late.is_err());
    p.h.close().unwrap();
}

#[test]
fn events_carry_triples_and_overflow_into_resync() {
    let p = open("events");
    let fx = fixture(&p, 1);
    let sub = p.h.subscribe().unwrap();
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 2}));
    let evs = sub.drain();
    match &evs[..] {
        [
            Event::Committed {
                command, triples, ..
            },
        ] => {
            assert_eq!(command, "part_set_gain");
            assert_eq!(triples[0].field.as_deref(), Some("gain_db"));
            assert_eq!(triples[0].op, "set");
        }
        other => panic!("{other:?}"),
    }
    // a 1 000-op burst against a subscriber that never reads
    for i in 0..1_100 {
        p.human(
            "part_set_gain",
            json!({"part": fx.parts[0], "gain_db": (i % 20) as f64}),
        );
    }
    let evs = sub.drain();
    assert!(evs.len() <= 1_000);
    assert!(evs.iter().any(|e| matches!(e, Event::Resync { .. })));
    let since = p.h.watch(0, 5);
    assert!(since.is_none(), "too old: resync");
    p.h.close().unwrap();
}

#[test]
fn undo_redo_and_conflicting_undo() {
    let p = open("undo");
    let fx = fixture(&p, 2);
    let gain = |p: &common::Project| p.h.snapshot().doc.get(pid(&fx.parts[0])).unwrap().gain_db;
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 5}));
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 7}));
    p.human("undo", json!({}));
    assert_eq!(gain(&p), 5.0);
    p.human("redo", json!({}));
    assert_eq!(gain(&p), 7.0);
    p.human("undo", json!({}));
    p.human("undo", json!({}));
    assert_eq!(gain(&p), 0.0);
    p.human("redo", json!({}));
    assert_eq!(gain(&p), 5.0);
    // a new edit clears the redo stack
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 9}));
    assert!(p.try_human("redo", json!({})).is_err());

    // an agent edits the same field after the human: the human's undo conflicts
    p.human(
        "part_trim",
        json!({"part": fx.parts[1], "src_out": 104 * SEC + SEC / 2}),
    );
    p.agent(
        "a1",
        "part_trim",
        json!({"part": fx.parts[1], "src_out": 104 * SEC + SEC / 4}),
        p.as_of(),
    )
    .unwrap();
    match p.try_human("undo", json!({})) {
        Err(CmdError::Conflict(c)) => {
            assert_eq!(
                c.options,
                Some(vec![
                    ConflictOption::RevertUntouched,
                    ConflictOption::Cancel
                ])
            );
            assert!(c.fields.unwrap()[0].ends_with(".src_out"));
        }
        other => panic!("{other:?}"),
    }
    // (c) cancel does nothing; (a) reverts what nobody touched (nothing here)
    let before = p.h.snapshot().doc.rev();
    p.human("undo", json!({"option": "cancel"}));
    assert_eq!(p.h.snapshot().doc.rev(), before);
    p.h.close().unwrap();
}

#[test]
fn run_undo_offers_a_c_and_restore_and_lists_dropped_edits() {
    let p = open("rununo");
    let fx = fixture(&p, 3);
    // an agent's implicit run: two edits on the cut
    p.agent(
        "a1",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 4}),
        p.as_of(),
    )
    .unwrap();
    p.agent(
        "a1",
        "part_trim",
        json!({"part": fx.parts[1], "src_out": 104 * SEC}),
        p.as_of(),
    )
    .unwrap();
    let begun = p.agent("a1", "run_end", json!({}), p.as_of()).unwrap();
    let run = begun.output["run"].as_str().unwrap().to_owned();
    assert_eq!(begun.output["transactions"], 2);
    // the human then edits one of the run's fields
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 6}));
    match p.try_human("run_undo", json!({"run": run})) {
        Err(CmdError::Conflict(c)) => {
            assert_eq!(
                c.options,
                Some(vec![
                    ConflictOption::RevertUntouched,
                    ConflictOption::RestoreToPoint,
                    ConflictOption::Cancel
                ])
            );
            assert!(c.diff["later_edits"][0].as_str().unwrap().contains("human"));
        }
        other => panic!("{other:?}"),
    }
    // (a) revert untouched: the trim goes back, the human's gain stays
    let r = p.human(
        "run_undo",
        json!({"run": run, "option": "revert_untouched"}),
    );
    assert!(r.committed.is_some());
    let d = p.h.snapshot().doc.clone();
    assert_eq!(d.get(pid(&fx.parts[1])).unwrap().src_out.0, 105 * SEC);
    assert_eq!(d.get(pid(&fx.parts[0])).unwrap().gain_db, 6.0);
    // restore to the run's point: everything in the cut back, the human edit dropped,
    // as one transaction
    let before = d.rev();
    let r = p.human(
        "run_undo",
        json!({"run": run, "option": "restore_to_point"}),
    );
    assert_eq!(r.committed, Some(before + 1));
    assert_eq!(
        p.h.snapshot().doc.get(pid(&fx.parts[0])).unwrap().gain_db,
        0.0
    );
    // and that restore is itself undoable in one step
    p.human("undo", json!({}));
    assert_eq!(
        p.h.snapshot().doc.get(pid(&fx.parts[0])).unwrap().gain_db,
        6.0
    );
    p.h.close().unwrap();
}

#[test]
fn a_second_cut_starts_a_new_run_with_its_own_restore_point() {
    let p = open("runcut");
    let fx = fixture(&p, 1);
    let other = p.human("cut_create", json!({"title": "other"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    p.agent(
        "a1",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 4}),
        p.as_of(),
    )
    .unwrap();
    let first = p
        .agent("a1", "run_end", json!({}), p.as_of())
        .unwrap()
        .output["run"]
        .clone();
    p.agent(
        "a1",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 5}),
        p.as_of(),
    )
    .unwrap();
    let src = fx.source.clone();
    p.agent(
        "a1",
        "part_add",
        json!({"cut": other, "media": {"source": src}, "src_in": 0, "src_out": SEC}),
        p.as_of(),
    )
    .unwrap();
    let second = p.agent("a1", "run_end", json!({}), p.as_of()).unwrap();
    assert_ne!(second.output["run"], first);
    assert_eq!(
        second.output["transactions"], 1,
        "the write to the second cut opened a new run"
    );
    p.h.close().unwrap();
}

#[test]
fn restore_points_and_reopen() {
    let p = open("restore");
    let fx = fixture(&p, 2);
    let rp = p.human("restore_point_create", json!({})).output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 4}));
    p.human("part_split", json!({"part": fx.parts[1], "at": 104 * SEC}));
    let edited = p.h.snapshot().doc.clone();
    let p = p.reopen();
    assert_eq!(p.h.snapshot().doc, edited, "reopen = entity rows + replay");
    let r = p.human("restore_to", json!({"snapshot": rp}));
    assert!(r.committed.is_some());
    let d = p.h.snapshot().doc.clone();
    assert_eq!(d.get(pid(&fx.parts[0])).unwrap().gain_db, 0.0);
    assert_eq!(d.count::<Part>(), 2);
    let (saved, err) = p.h.saved();
    assert!(err.is_none());
    assert_eq!(p.h.flush(), Some(d.rev()));
    assert!(saved <= d.rev());
    p.h.close().unwrap();
}

/// A2.13: restore points coalesce (20 human edits, or 5 minutes of them), agent edits
/// never touch the human's stack (R-HIST-1), and the history list says what a cut was at
/// each version.
#[test]
fn coalesced_restore_points_and_the_history_list() {
    let p = open("history-list");
    let fx = fixture(&p, 4);
    let part = fx.parts[0].clone();
    let trim = |i: i64| {
        p.human(
            "part_trim",
            json!({"part": part, "src_out": 102 * SEC - (i % 2) * SEC / 10}),
        );
    };
    let list = |cut: Option<&str>| -> Value {
        let args = match cut {
            Some(c) => json!({"cut": c}),
            None => json!({}),
        };
        p.human("history_list", args).output
    };
    // the fixture's own edits count: edit until the next restore point lands
    let base = list(None)["versions"].as_array().unwrap().len();
    let mut i = 0;
    while list(None)["versions"].as_array().unwrap().len() == base {
        trim(i);
        i += 1;
        assert!(i <= 20, "a restore point within 20 edits");
    }
    let n0 = list(None)["versions"].as_array().unwrap().len();
    for i in 0..19 {
        trim(i);
    }
    assert_eq!(
        list(None)["versions"].as_array().unwrap().len(),
        n0,
        "19 edits: none yet"
    );
    trim(19);
    assert_eq!(
        list(None)["versions"].as_array().unwrap().len(),
        n0 + 1,
        "the 20th makes one"
    );
    // five minutes later, one edit is enough
    p.clock.advance(5 * 60 * 1000 + 1);
    trim(20);
    assert_eq!(list(None)["versions"].as_array().unwrap().len(), n0 + 2);
    // a named snapshot, then a part deleted: the list shows 4 parts then, 3 now
    p.human("restore_point_create", json!({"name": "before the delete"}));
    p.human(
        "cut_edit",
        json!({"cut": fx.cut, "ops": [{"op": "part_delete", "part": fx.parts[3]}]}),
    );
    let l = list(Some(&fx.cut));
    assert_eq!(l["now_parts"], 3);
    let named = l["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "named")
        .unwrap()
        .clone();
    assert_eq!(named["name"], "before the delete");
    assert_eq!(named["parts"], 4);
    // restoring it brings the part back, as one undoable step
    p.human(
        "restore_to",
        json!({"snapshot": named["id"], "cut": fx.cut}),
    );
    assert_eq!(list(Some(&fx.cut))["now_parts"], 4);
    p.human("undo", json!({}));
    assert_eq!(list(Some(&fx.cut))["now_parts"], 3);
    // R-HIST-1: an agent's edit neither enters nor clears the human's stack
    p.human("redo", json!({}));
    let rev = p.h.snapshot().doc.rev();
    p.agent(
        "c1",
        "part_trim",
        json!({"part": fx.parts[1], "src_out": 105 * SEC - SEC / 10}),
        Some(gmclip_proto::Base::AsOf { as_of: rev }),
    )
    .unwrap();
    p.human("undo", json!({}));
    assert_eq!(
        list(Some(&fx.cut))["now_parts"],
        3,
        "the human's undo undid the human's restore"
    );
}

/// R-CC-4 until M4 (A2.14): an agent's run is listed open on its cut from its first
/// write until it ends or goes idle, so the app can show "agent working on this cut".
#[test]
fn open_runs_name_the_cut_an_agent_is_writing() {
    let p = open("openruns");
    let fx = fixture(&p, 2);
    assert!(p.h.open_runs().is_empty());
    p.human("part_set_gain", json!({"part": fx.parts[0], "gain_db": 1}));
    assert!(p.h.open_runs().is_empty(), "a human edit opens no run");
    let agent = |command: &str, args: Value| {
        p.clock.advance(1);
        p.h.call(CallRequest {
            command: command.into(),
            args,
            base: Some(Base::AsOf {
                as_of: u64::MAX >> 1,
            }),
            request_id: None,
            caller: Caller::agent("local:claude-code", "c1"),
        })
        .unwrap()
    };
    agent("part_set_gain", json!({"part": fx.parts[1], "gain_db": 2}));
    let runs = p.h.open_runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].cut.as_deref(), Some(fx.cut.as_str()));
    assert_eq!(runs[0].identity, "local:claude-code");
    agent("run_end", json!({}));
    assert!(p.h.open_runs().is_empty(), "ended");
    // a run idle past the window is not working any more
    agent("part_set_gain", json!({"part": fx.parts[1], "gain_db": 3}));
    assert_eq!(p.h.open_runs().len(), 1);
    p.clock.advance(gmclip_core::actor::RUN_IDLE_MS);
    assert!(p.h.open_runs().is_empty(), "idle");
    p.h.close().unwrap();
}

/// Decision 0040: a run is undone after the project was closed and opened again (a
/// session's "Undo its edits" after the app restarted).
#[test]
fn a_run_is_undone_after_a_reopen() {
    let p = open("rununreopen");
    let fx = fixture(&p, 3);
    let before = p.h.snapshot().doc.clone();
    p.agent(
        "a1",
        "part_set_gain",
        json!({"part": fx.parts[0], "gain_db": 4}),
        p.as_of(),
    )
    .unwrap();
    p.agent(
        "a1",
        "part_trim",
        json!({"part": fx.parts[1], "src_out": 104 * SEC}),
        p.as_of(),
    )
    .unwrap();
    let run = p
        .agent("a1", "run_end", json!({}), p.as_of())
        .unwrap()
        .output["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let p = p.reopen();
    p.human("run_undo", json!({"run": run}));
    let now = p.h.snapshot().doc.clone();
    for part in &fx.parts {
        let id = gmclip_model::Id::<gmclip_model::entities::Part>::new(
            gmclip_model::Ulid::parse(part).unwrap(),
        );
        assert_eq!(
            now.get(id).map(|x| (x.src_in, x.src_out, x.gain_db)),
            before.get(id).map(|x| (x.src_in, x.src_out, x.gain_db)),
            "the run's edits are gone"
        );
    }
    p.h.close().unwrap();
}
