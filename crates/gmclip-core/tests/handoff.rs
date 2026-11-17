//! A.04: a run's handoff summary and its reversible choices. A choice restores exactly
//! the field values it recorded, in one transaction (one undo takes it back); what an
//! agent records is checked against the cut and the fields' types, and is always about
//! its own open run.

mod common;

use common::{SEC, fixture, open};
use gmclip_core::CmdError;
use gmclip_model::entities::{Cut, Part};
use gmclip_model::{Id, Ulid};
use serde_json::json;

fn pid(s: &str) -> Id<Part> {
    Id::new(Ulid::parse(s).unwrap())
}

fn cid(s: &str) -> Id<Cut> {
    Id::new(Ulid::parse(s).unwrap())
}

fn invalid_at(r: Result<gmclip_core::CallOk, CmdError>) -> String {
    match r {
        Err(CmdError::Invalid { path, .. }) => path,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn a_choice_restores_exactly_its_recorded_values_in_one_transaction() {
    let p = open("handoff");
    let fx = fixture(&p, 3);
    let (a, b) = (fx.parts[0].clone(), fx.parts[1].clone());
    let doc = || p.h.snapshot().doc.clone();
    let before_in = doc().get(pid(&a)).unwrap().src_in.0;
    // the agent's run: trims part one's start and lifts part two
    p.agent(
        "c1",
        "part_trim",
        json!({"part": a, "src_in": before_in + SEC / 2}),
        p.as_of(),
    )
    .unwrap();
    p.agent(
        "c1",
        "part_set_gain",
        json!({"part": b, "gain_db": 4}),
        p.as_of(),
    )
    .unwrap();
    // … and records why, with what undoes each call (it names another run: ignored)
    let r = p
        .agent(
            "c1",
            "run_summary_set",
            json!({"cut": fx.cut, "text": "Started on the result. <b>not markup</b>",
                   "run": "01J0000000000000000000000Z",
                   "reversible_choices": [
                       {"label": "Opened half a second later", "restore": [
                           {"entity": format!("part:{a}"), "field": "src_in", "value": before_in},
                           {"entity": format!("part:{b}"), "field": "gain_db", "value": 0}]},
                       {"label": "Kept the uh", "restore": [
                           {"entity": format!("cut:{}", fx.cut), "field": "notes", "value": "no uh"}]}]}),
            p.as_of(),
        )
        .unwrap();
    let run = r.output["run"].as_str().unwrap().to_owned();
    assert_ne!(
        run, "01J0000000000000000000000Z",
        "an agent summarises its own run"
    );
    let s = doc()
        .get(cid(&fx.cut))
        .unwrap()
        .last_run_summary
        .clone()
        .unwrap();
    assert_eq!(s.run.0.to_string(), run);
    assert_eq!(s.reversible_choices.len(), 2);
    // gain_db is a float field: the integer the agent typed is stored as what the field holds
    assert_eq!(s.reversible_choices[0].restore[1].value, "0.0");
    // the human edits more after the run
    p.human("part_trim", json!({"part": a, "src_in": before_in + SEC}));
    p.human("part_set_gain", json!({"part": b, "gain_db": 7}));
    let rev = doc().rev();
    let out = p.human("run_choice_restore", json!({"cut": fx.cut, "choice": 0}));
    assert_eq!(out.output["restored"], 2);
    assert_eq!(doc().rev(), rev + 1, "one transaction");
    assert_eq!(doc().get(pid(&a)).unwrap().src_in.0, before_in);
    assert_eq!(doc().get(pid(&b)).unwrap().gain_db, 0.0);
    // again: nothing left to write
    let again = p.human("run_choice_restore", json!({"cut": fx.cut, "choice": 0}));
    assert_eq!(again.output["restored"], 0);
    assert_eq!(again.output["unchanged"], 2);
    // one undo takes the restore back whole
    p.human("undo", json!({}));
    assert_eq!(doc().get(pid(&a)).unwrap().src_in.0, before_in + SEC);
    assert_eq!(doc().get(pid(&b)).unwrap().gain_db, 7.0);
    // the second choice, then a choice that is not there
    p.human("run_choice_restore", json!({"cut": fx.cut, "choice": 1}));
    assert_eq!(doc().get(cid(&fx.cut)).unwrap().notes, "no uh");
    assert_eq!(
        invalid_at(p.try_human("run_choice_restore", json!({"cut": fx.cut, "choice": 2}))),
        "/choice"
    );
    p.h.close().unwrap();
}

#[test]
fn what_a_run_records_is_checked() {
    let p = open("handoff-checks");
    let fx = fixture(&p, 2);
    let other = p.human("cut_create", json!({"title": "other"})).output["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let a = fx.parts[0].clone();
    let rec = |restore: serde_json::Value| {
        p.agent(
            "c1",
            "run_summary_set",
            json!({"cut": fx.cut, "text": "t", "reversible_choices": [{"label": "x", "restore": [restore]}]}),
            p.as_of(),
        )
    };
    // no run on this connection yet
    assert_eq!(
        invalid_at(rec(
            json!({"entity": format!("part:{a}"), "field": "gain_db", "value": 1})
        )),
        "/run"
    );
    p.agent(
        "c1",
        "part_set_gain",
        json!({"part": a, "gain_db": 2}),
        p.as_of(),
    )
    .unwrap();
    // another cut's entity, an unknown field, a wrong type, a map without a key, a
    // re-parenting write
    let bad = [
        json!({"entity": format!("cut:{other}"), "field": "notes", "value": "x"}),
        json!({"entity": format!("part:{a}"), "field": "nope", "value": 1}),
        json!({"entity": format!("part:{a}"), "field": "gain_db", "value": "loud"}),
        json!({"entity": format!("part:{a}"), "field": "cut", "value": other}),
        json!({"entity": "part:not-a-ulid", "field": "gain_db", "value": 1}),
    ];
    for b in bad {
        let at = invalid_at(rec(b.clone()));
        // (error paths are cut to 16 bytes, R-CMD-6)
        assert!(at.starts_with("/reversible_choi"), "{b}: {at}");
    }
    // a summary naming another cut than the one the run edited
    assert_eq!(
        invalid_at(p.agent(
            "c1",
            "run_summary_set",
            json!({"cut": other, "text": "t"}),
            p.as_of()
        )),
        "/cut"
    );
    assert!(
        p.h.snapshot()
            .doc
            .get(cid(&fx.cut))
            .unwrap()
            .last_run_summary
            .is_none(),
        "nothing recorded"
    );
    // a human recording one names the run
    assert_eq!(
        invalid_at(p.try_human("run_summary_set", json!({"cut": fx.cut, "text": "t"}))),
        "/run"
    );
    p.h.close().unwrap();
}
