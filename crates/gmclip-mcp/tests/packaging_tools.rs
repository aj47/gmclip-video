//! Decision 0024 on the real MCP surface with scripted agents (no model): the
//! title/description pass contract (ai_pass{publish_copy} / pass_step) writes drafts as
//! proposals only, refusing numbers the cut does not back; a launched run limited to the
//! contract cannot write or decide drafts any other way; agents read hook lines,
//! openings and a long-form's chapters, propose openings, and never choose one.

mod common;

use common::{SEC, World, code};
use gmclip_core::passes::PUBLISH_COPY;
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{Control, LaunchRegister};
use gmclip_mcp::wire;
use gmclip_model::entities::{CopyDraft, Cut, EditOrigin, ProposalState};
use gmclip_model::{Id, Ulid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The fixture's source id (its first part's media).
fn source_of(p: &gmclip_mcp::holder::OpenProject, cut: &str) -> String {
    let doc = p.handle.snapshot().doc.clone();
    let c: Id<Cut> = Id::new(Ulid::parse(cut).unwrap());
    let part = doc
        .children::<gmclip_model::entities::Part>(c.entity())
        .find_map(|x| doc.get(x))
        .unwrap();
    match part.media {
        gmclip_model::entities::MediaRef::Source(s) => s.0.to_string(),
        gmclip_model::entities::MediaRef::Group(g) => g.0.to_string(),
    }
}

fn words(p: &gmclip_mcp::holder::OpenProject, owner: &str, ws: &[(&str, f64, f64)]) {
    let rev = "asr-test@1";
    let words: Vec<Value> = ws
        .iter()
        .map(|(t, s, e)| json!({"text": t, "start": (s * SEC as f64) as i64, "end": (e * SEC as f64) as i64}))
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": words});
    p.handle
        .put_analysis(owner, "words", rev, body.to_string().into_bytes())
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scripted_agent_drafts_titles_through_the_contract_and_the_human_decides() {
    let w = World::start().await;
    let fx = w.fixture("copy.gmclip", 2, "copy");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let p = w.holder.resolve(Some(&fx.project)).unwrap();
    let src = source_of(&p, &fx.cut);
    words(
        &p,
        &src,
        &[
            ("My", 100.1, 100.2),
            ("PC", 100.25, 100.4),
            ("made", 100.45, 100.6),
            ("this", 100.65, 100.8),
            ("in", 100.85, 100.9),
            ("two", 100.95, 101.1),
            ("minutes.", 101.15, 101.6),
            ("Three", 103.1, 103.3),
            ("clips", 103.35, 103.6),
            ("came", 103.65, 103.8),
            ("out.", 103.85, 104.2),
        ],
    );

    // a launched run limited to the contract
    let brief = PUBLISH_COPY.brief();
    let cred = [7u8; 32];
    let v = w
        .control(&Control::LaunchRegister(LaunchRegister {
            project: fx.project.clone(),
            cut: fx.cut.clone(),
            credential: wire::hex(&cred),
            wall_clock_s: 600,
            media: false,
            private_regions: false,
            code: None,
            brief_sha256: Some(wire::hex(&Sha256::digest(brief.as_bytes()))),
            scopes: None,
            full_access: false,
            grant: None,
        }))
        .await;
    assert_eq!(v["ok"], true, "{v}");
    let run = v["result"]["run"].as_str().unwrap().to_owned();
    w.holder.restrict_commands(&run, PUBLISH_COPY.commands);
    let c = w
        .client(
            "claude-code",
            Some(&RunCredential::new(run.clone(), cred.to_vec())),
        )
        .await;
    // the writer and the decision are not on the agent surface
    for cmd in ["copy_drafts_add", "copy_draft_state", "opening_choose"] {
        let e = c
            .call(
                "command_run",
                json!({"command": cmd, "args": {}, "base": {"as_of": 1}}),
            )
            .await
            .unwrap_err();
        assert_eq!(code(&e), "NotFound", "{cmd}: {e}");
    }

    // the pass: the cut's own lines, the facts, each platform's limits
    let s = c
        .ok(
            "ai_pass",
            json!({"op": "begin_self", "pass": "publish_copy", "target": {}}),
        )
        .await;
    let pr = s["pass_run"].as_str().unwrap().to_owned();
    assert!(pr.starts_with("cp_"), "{s}");
    assert_eq!(s["done"], false);
    let lines: Vec<&str> = s["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{s}");
    assert!(
        lines[0].starts_with("L1 [0:00–0:01] <untrusted>My PC made this in two minutes."),
        "{}",
        lines[0]
    );
    assert!(lines[1].starts_with("L2 [0:02–0:03]"), "{}", lines[1]);
    assert_eq!(s["platforms"].as_array().unwrap().len(), 4);
    assert_eq!(s["missing"].as_array().unwrap().len(), 4);
    assert_eq!(s["facts"]["count_of_runs"], 0);
    assert!(s["contract"].as_str().unwrap().contains("no links"));

    // a submit: two good, one with a number the cut does not back, one malformed
    let r = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [
                {"platform": "youtube", "title": "My PC made this in 2 minutes", "description": "Three clips, one GPU, no cloud.\n#ai"},
                {"platform": "shorts", "title": "My PC made this in 90 seconds #Shorts", "description": "Local."},
                {"platform": "tiktok", "title": "Three clips came out", "description": "Made on my PC. #ai #video"},
                {"platform": "fax", "title": "x", "description": "y"}
            ]}),
        )
        .await;
    assert_eq!(r["submitted"]["accepted"], 2, "{r}");
    let refused = r["submitted"]["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 2, "{r}");
    assert_eq!(refused[0]["index"], 1);
    assert!(refused[0]["reason"].as_str().unwrap().contains("number 90"));
    assert_eq!(refused[1]["index"], 3);
    assert_eq!(r["done"], false);
    let missing: Vec<&str> = r["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(missing, vec!["shorts", "instagram"]);
    // the repair: every platform has a draft, the run is done
    let r = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [
                {"platform": "shorts", "title": "My PC made this in 2 minutes #Shorts", "description": "Local."},
                {"platform": "instagram", "title": "Two minutes on my own PC", "description": "Three clips came out. #ai"}
            ]}),
        )
        .await;
    assert_eq!(r["done"], true, "{r}");
    assert_eq!(r["record"]["drafts"].as_array().unwrap().len(), 4);
    assert!(
        r["display"]
            .as_str()
            .unwrap()
            .contains("accept, edit or reject")
    );

    // what the run left: four proposals from the agent with the prompt's hash; nothing
    // accepted
    let doc = p.handle.snapshot().doc.clone();
    let c_id: Id<Cut> = Id::new(Ulid::parse(&fx.cut).unwrap());
    let drafts: Vec<CopyDraft> = doc
        .children::<CopyDraft>(c_id.entity())
        .filter_map(|d| doc.get(d).cloned())
        .collect();
    assert_eq!(drafts.len(), 4);
    assert!(drafts.iter().all(|d| {
        d.state == ProposalState::Proposed
            && d.origin == EditOrigin::Agent
            && d.pass_ref
                .as_deref()
                .is_some_and(|r| r.starts_with(&PUBLISH_COPY.pass_ref()))
    }));
    // an agent reads them back like the human (a launched pass run writes its output
    // only: reads go through the contract)
    let reader = w.client("codex", None).await;
    reader
        .ok(
            "command_run",
            json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
        )
        .await;
    let list = reader
        .ok(
            "command_run",
            json!({"command": "copy_drafts_list", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(list["output"]["drafts"].as_array().unwrap().len(), 4);

    // the human decides
    let yt = drafts
        .iter()
        .find(|d| d.platform == gmclip_model::entities::CopyPlatform::Youtube)
        .unwrap();
    p.handle
        .human(
            "copy_draft_state",
            json!({"draft": yt.id.0.to_string(), "state": "accepted"}),
        )
        .unwrap();
    let doc = p.handle.snapshot().doc.clone();
    assert_eq!(doc.get(yt.id).unwrap().state, ProposalState::Accepted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_read_hook_lines_openings_and_chapters_and_propose_openings() {
    let w = World::start().await;
    let fx = w.fixture("read.gmclip", 3, "read");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let p = w.holder.resolve(Some(&fx.project)).unwrap();
    let src = source_of(&p, &fx.cut);
    words(
        &p,
        &src,
        &[
            ("It", 100.1, 100.2),
            ("rendered", 100.3, 100.6),
            ("three", 100.7, 100.9),
            ("clips", 101.0, 101.3),
            ("for", 101.35, 101.4),
            ("me.", 101.45, 101.7),
        ],
    );
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    // hook lines with their provenance
    let h = c
        .ok(
            "command_run",
            json!({"command": "hook_text_suggest", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(
        h["output"]["suggestions"][0]["text"],
        "It rendered *3 clips* for me."
    );
    assert_eq!(h["output"]["suggestions"][0]["provenance"][0]["text"], "3");
    // an agent proposes an opening; it cannot choose one
    let as_of = c.ok("cut_get", json!({"cut": fx.cut})).await["as_of"]
        .as_u64()
        .unwrap();
    let o = c
        .ok(
            "command_run",
            json!({"command": "opening_add", "base": {"as_of": as_of}, "args": {"cut": fx.cut, "label": "the payoff", "from": {"span": {
                "start": {"raw": {"time": {"clock": "src", "t": 200 * SEC}, "reason": "the payoff, said"}},
                "end": {"raw": {"time": {"clock": "src", "t": 203 * SEC}, "reason": "the payoff, said"}}}}}}),
        )
        .await;
    let alt = o["output"]["id"].as_str().unwrap().to_owned();
    let l = c
        .ok(
            "command_run",
            json!({"command": "openings_list", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(l["output"]["versions"].as_array().unwrap().len(), 2);
    assert_eq!(l["output"]["versions"][1]["opening"], alt.as_str());
    assert_eq!(l["output"]["versions"][1]["origin"], "agent");
    let e = c
        .call(
            "command_run",
            json!({"command": "opening_choose", "base": {"as_of": as_of}, "args": {"opening": alt}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound", "{e}");

    // a long-form's chapters on cut_get and through command_run
    let human = |cmd: &str, args: Value| p.handle.human(cmd, args).unwrap().output;
    let rec = human(
        "source_add",
        json!({"path": "/m/stream.mkv", "fingerprint": "fp2",
               "probe": {"duration": 3600 * SEC, "tracks": [{"index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080,
                          "fps": {"num": 30, "den": 1}, "timescale": 15360, "gop_max_s": 2.0}], "audio_tracks": []}}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    human("stream_edit_ensure", json!({"media": {"source": rec}}));
    let word = |s: i64, n: u64| json!({"word_id": {"asr_revision": "r1", "ordinal": n}, "t_src_start": s * SEC, "t_src_end": s * SEC + SEC / 2, "normalised_text": format!("w{n}")});
    let mut topics = Vec::new();
    for (i, (a, b, t)) in [
        (100, 160, "Setup"),
        (300, 360, "Demo"),
        (600, 630, "Q and A"),
    ]
    .into_iter()
    .enumerate()
    {
        let n = 1000 * i as u64;
        topics.push(
            human(
                "topic_set",
                json!({"start": word(a, n), "end": word(b, n + 1), "title": t}),
            )["topic"]
                .clone(),
        );
    }
    let long = human(
        "longform_from_topics",
        json!({"topics": topics, "order": "source", "title": "Long"}),
    )["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    let g = c.ok("cut_get", json!({"cut": long})).await;
    let ch = &g["cut"]["chapters"];
    assert_eq!(ch["valid"], true, "{g}");
    assert_eq!(ch["count"], 3);
    assert_eq!(ch["youtube"]["untrusted"], true);
    assert_eq!(
        ch["youtube"]["text"],
        "00:00 Setup\n01:00 Demo\n02:01 Q and A"
    );
    let r = c
        .ok(
            "command_run",
            json!({"command": "longform_chapters", "args": {"cut": long}}),
        )
        .await;
    assert_eq!(
        r["output"]["youtube"],
        "00:00 Setup\n01:00 Demo\n02:01 Q and A"
    );
    // a short has none
    let g = c.ok("cut_get", json!({"cut": fx.cut})).await;
    assert!(g["cut"].get("chapters").is_none());
}
