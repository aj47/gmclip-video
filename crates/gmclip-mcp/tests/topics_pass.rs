//! Decision 0011 step 3: the topics + fluff pass, run by a scripted agent through the
//! real MCP surface (no model): it follows the pass's prompt asset with `command_run`,
//! proposes topics and fluff only, every proposal carries the prompt's versioned hash
//! (R-AI-5), the human's edits during the run win (overlapping proposals arrive flagged,
//! nothing is applied), and an accepted topic is never touched by the agent.

mod common;

use common::{SEC, World, code};
use gmclip_core::passes::TOPICS_FLUFF;
use gmclip_model::Id;
use gmclip_model::entities::{Cut, FluffProposal, Part, ProposalState, Topic};
use serde_json::{Value, json};

/// A word every 0.4 s (no transcript in this fixture: the agent's fingerprints stand).
fn word(s: f64) -> Value {
    let n = (s * 2.5).round() as i64;
    let t = n * SEC * 2 / 5;
    json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": t, "t_src_end": t + SEC * 3 / 10, "normalised_text": "w"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scripted_agent_runs_the_topics_pass_and_the_human_wins() {
    let w = World::start().await;
    let fx = w.fixture("pass.gmclip", 1, "fixture");
    let c = w.client("Claude-Code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let p = w.holder.resolve(Some(&fx.project)).unwrap();
    let human = |cmd: &str, args: Value| p.handle.human(cmd, args).unwrap().output;
    let src = human(
        "source_add",
        json!({"path": "/m/stream.mkv", "fingerprint": "fp2", "probe": {"duration": 600 * SEC, "tracks": [], "audio_tracks": []}}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stream = human("stream_edit_ensure", json!({"media": {"source": src}}))["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let mine = human(
        "topic_set",
        json!({"start": word(10.0), "end": word(100.0), "title": "Setup"}),
    )["topic"]
        .as_str()
        .unwrap()
        .to_owned();

    // The pass, as its prompt says: SINCE from project_overview, the map, then proposals.
    let brief = TOPICS_FLUFF.brief();
    let pass_ref = TOPICS_FLUFF.pass_ref();
    assert!(brief.contains(&pass_ref) && brief.contains("SINCE"));
    let ov = c.ok("project_overview", json!({})).await;
    let since = ov["as_of"].as_u64().unwrap();
    let kinds: Vec<&str> = ov["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"stream"), "{kinds:?}");
    let map = c
        .ok("command_run", json!({"command": "project_map", "args": {}}))
        .await;
    assert_eq!(map["output"]["stream"]["cut"], stream.as_str());
    assert_eq!(map["output"]["stream"]["topics"][0]["state"], "accepted");

    // meanwhile the human strikes 300–320 s
    human(
        "words_strike",
        json!({"cut": stream, "words": [word(300.0), word(320.0)]}),
    );

    // base: the as_of of the agent's last read or write (its own writes move the topic
    // index; the human's strike does not refuse it, it flags)
    let last = std::sync::Mutex::new(since);
    let run = |cmd: &'static str, args: Value| {
        let c = &c;
        let last = &last;
        async move {
            let base = json!({"as_of": *last.lock().unwrap()});
            let r = c
                .call(
                    "command_run",
                    json!({"command": cmd, "args": args, "base": base}),
                )
                .await;
            if let Ok(v) = &r
                && let Some(n) = v["as_of"].as_u64().or(v["committed"].as_u64())
            {
                *last.lock().unwrap() = n;
            }
            r
        }
    };
    // over the human's topic: flagged
    let over = run(
        "topic_set",
        json!({"start": word(50.0), "end": word(150.0), "title": "Install", "since": since, "pass_ref": pass_ref}),
    )
    .await
    .unwrap();
    assert_eq!(over["output"]["conflict"], true, "{over}");
    // over the human's strike made during the run: flagged
    let edited = run(
        "topic_set",
        json!({"start": word(200.0), "end": word(400.0), "title": "GPU prices", "since": since, "pass_ref": pass_ref}),
    )
    .await
    .unwrap();
    assert_eq!(edited["output"]["conflict"], true);
    // clear of human work: a plain proposal
    let clean = run(
        "topic_set",
        json!({"start": word(420.0), "end": word(590.0), "title": "Benchmarks", "since": since, "pass_ref": pass_ref}),
    )
    .await
    .unwrap();
    assert_eq!(clean["output"]["state"], "proposed");
    assert_eq!(clean["output"]["conflict"], false);
    let fl = run(
        "fluff_propose",
        json!({"since": since, "pass_ref": pass_ref, "items": [
            {"start": word(305.0), "end": word(315.0), "reason": "already struck by the human"},
            {"start": word(290.0), "end": word(330.0), "reason": "chat"},
            {"start": word(450.0), "end": word(460.0), "reason": "waiting for the render"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(fl["output"]["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(fl["output"]["conflicts"], 1);
    // the human's topic is out of reach; the agent cannot accept its own proposals
    for (cmd, args) in [
        ("topic_set", json!({"topic": mine, "title": "Mine now"})),
        (
            "topic_merge",
            json!({"topics": [mine, clean["output"]["topic"]]}),
        ),
        ("topic_delete", json!({"topic": mine})),
        (
            "topic_state",
            json!({"topic": clean["output"]["topic"], "state": "accepted"}),
        ),
    ] {
        let e = run(cmd, args).await.unwrap_err();
        assert_eq!(code(&e), "Invalid", "{cmd}: {e}");
    }
    // a split of the human's topic is only proposed
    let sp = run(
        "topic_split",
        json!({"topic": mine, "at": word(60.0), "since": since, "pass_ref": pass_ref}),
    )
    .await
    .unwrap();
    assert_eq!(sp["output"]["proposed"], true);

    // what the pass left: proposals with the prompt's hash; nothing applied
    let doc = p.handle.snapshot().doc.clone();
    let cut: Id<Cut> = Id::new(gmclip_model::Ulid::parse(&stream).unwrap());
    let topics: Vec<Topic> = doc
        .children::<Topic>(cut.entity())
        .filter_map(|id| doc.get(id).cloned())
        .collect();
    let human_t = topics.iter().find(|t| t.id.0.to_string() == mine).unwrap();
    assert_eq!(
        (human_t.title.as_str(), human_t.state),
        ("Setup", ProposalState::Accepted)
    );
    let proposed: Vec<&Topic> = topics
        .iter()
        .filter(|t| t.state == ProposalState::Proposed)
        .collect();
    assert_eq!(proposed.len(), 5);
    assert!(
        proposed
            .iter()
            .all(|t| t.pass_ref.as_deref() == Some(pass_ref.as_str()))
    );
    let fluff: Vec<FluffProposal> = doc
        .children::<FluffProposal>(cut.entity())
        .filter_map(|id| doc.get(id).cloned())
        .collect();
    assert_eq!(fluff.len(), 2);
    assert!(fluff.iter().all(|f| f.state == ProposalState::Proposed));
    assert_eq!(
        doc.children::<Part>(cut.entity()).count(),
        2,
        "only the human's strike"
    );

    // the human decides: the clean topic and the fluff stretch are accepted
    human(
        "topic_state",
        json!({"topic": clean["output"]["topic"], "state": "accepted"}),
    );
    let calm = fluff.iter().find(|f| !f.conflict).unwrap();
    human(
        "fluff_state",
        json!({"proposals": [calm.id], "state": "accepted"}),
    );
    let doc = p.handle.snapshot().doc.clone();
    assert_eq!(doc.children::<Part>(cut.entity()).count(), 3);
    c.ok(
        "command_run",
        json!({"command": "run_end", "args": {}, "base": {"as_of": doc.rev()}}),
    )
    .await;
}
