//! A.01, find moments and the full sweep agent-executed, end to end on the real MCP
//! surface with a scripted agent (no model): a launched run limited to the pass contract
//! starts `ai_pass{begin_self}` over a 45-minute transcript, reads each step's lines,
//! submits moments by line reference and quote, gets its misquote back for repair, has a
//! seam twin settled and every uncarded stretch ≥ 120 s offered as a gap, and ends with
//! the density verdict. Its moments are in the panel labelled as the agent's, recall
//! not measured. Then the full sweep reads the same video against what is now carded.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{SEC, World, code};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_core::passes::{FIND_MOMENTS, FULL_SWEEP};
use gmclip_mcp::client::RunCredential;
use gmclip_mcp::holder::{Control, LaunchRegister};
use gmclip_mcp::wire;
use gmclip_model::Flicks;
use gmclip_model::entities::{Moment, MomentOrigin};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 45 minutes of talk: sentence `i` is 8 words from 5·i s to 5·i + 4 s, one line each.
const SENTENCES: usize = 540;

fn sentence(i: usize) -> String {
    format!("line{i} says alpha{i} beta{i} gamma{i} delta{i} and more.")
}

#[derive(Debug, Default)]
struct Talk;

impl AnalysisHost for Talk {
    fn name(&self) -> &'static str {
        "talk"
    }
    fn analyze(&self, _: &AudioSource, _: &AtomicBool) -> Result<Vec<AnalysisBlob>, HostError> {
        Ok(vec![])
    }
    fn transcribe(
        &self,
        _: &AudioSource,
        _: Option<(Flicks, Flicks)>,
        _: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        let f = |s: f64| Flicks((s * SEC as f64).round() as i64);
        let mut words = Vec::new();
        for i in 0..SENTENCES {
            for (k, w) in sentence(i).split(' ').enumerate() {
                let t = i as f64 * 5.0 + k as f64 * 0.5;
                words.push(HeardWord {
                    text: w.into(),
                    start: f(t),
                    end: f(t + 0.45),
                    confidence: None,
                });
            }
        }
        Ok(Transcribed {
            revision: "talk@1".into(),
            words,
        })
    }
    fn listen(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<Heard, HostError> {
        Err(HostError::Unsupported("talk"))
    }
    fn loudness(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<SpanLoudness, HostError> {
        Err(HostError::Unsupported("talk"))
    }
    fn edge(
        &self,
        _: &AudioSource,
        _: EdgeRule,
        _: (&str, Flicks, Flicks),
        _: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        Err(HostError::Unsupported("talk"))
    }
    fn pauses(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: Flicks,
        _: Flicks,
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("talk"))
    }
    fn fillers(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("talk"))
    }
    fn blob_summary(
        &self,
        _: &str,
        _: &[u8],
        _: Flicks,
        _: Flicks,
        _: usize,
    ) -> Result<Value, HostError> {
        Err(HostError::Unsupported("talk"))
    }
}

/// The served lines of a step, as an agent reads them: `(L-number, text)`.
fn lines_of(step: &Value) -> Vec<(usize, String)> {
    step["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let l = l.as_str().unwrap();
            let n: usize = l[1..l.find(' ').unwrap()].parse().unwrap();
            let text = l
                .split_once("<untrusted>")
                .unwrap()
                .1
                .strip_suffix("</untrusted>")
                .unwrap();
            (n, text.to_owned())
        })
        .collect()
}

/// A moment over served lines `a..=b` (1-based), its quote copied from them.
fn moment(step: &Value, a: usize, b: usize, score: u8) -> Value {
    let lines = lines_of(step);
    let q: Vec<&str> = lines
        .iter()
        .filter(|(n, _)| (a..=b).contains(n))
        .map(|(_, t)| t.as_str())
        .collect();
    assert_eq!(q.len(), b - a + 1, "L{a}–L{b} are in this step");
    json!({"start_line": format!("L{a}"), "end_line": format!("L{b}"), "quote": q.join(" "),
           "score": score, "type": "result", "why": "a number said out loud", "excerpt": format!("alpha{}", a - 1)})
}

async fn launch(
    w: &World,
    fx: &common::Fx,
    brief: &str,
    cred: [u8; 32],
) -> (String, common::Client) {
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
    let c = w
        .client(
            "claude-code",
            Some(&RunCredential::new(run.clone(), cred.to_vec())),
        )
        .await;
    (run, c)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scripted_agent_finds_moments_through_the_chunked_pass() {
    let w = World::start_full(None, Some(Arc::new(Talk))).await;
    let fx = w.fixture("find.gmclip", 2, "find");
    w.holder
        .open_project(fx.home.to_str().unwrap(), false)
        .unwrap();
    let p = w.holder.resolve(Some(&fx.project)).unwrap();
    // the transcript is made in the background on first read
    let human = w.client("claude-code", None).await;
    let parts = human
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let source = parts["parts"][0]["media"]["source"]
        .as_str()
        .unwrap()
        .to_owned();
    for _ in 0..50 {
        let v = human
            .ok("transcript_read", json!({"source": source, "limit": 1}))
            .await;
        match v.get("job").and_then(Value::as_str) {
            Some(job) => {
                human
                    .ok(
                        "command_run",
                        json!({"command": "job_wait", "args": {"job": job, "timeout_s": 10}}),
                    )
                    .await;
            }
            None => {
                assert_eq!(v["total_lines"], SENTENCES);
                break;
            }
        }
    }

    // the launched run may use the contract and nothing else
    let brief = FIND_MOMENTS.brief();
    let (run, c) = launch(&w, &fx, &brief, [3; 32]).await;
    w.holder.restrict_commands(&run, FIND_MOMENTS.commands);
    let e = c
        .call(
            "command_run",
            json!({"command": "moment_add", "args": {"start_s": 1.0, "end_s": 9.0, "quote": "line0", "score": 3, "type": "result", "why": "x"}, "base": {"as_of": 1}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unauthorized", "{e}");
    let e = c
        .call(
            "command_run",
            json!({"command": "moments_found_add", "args": {}, "base": {"as_of": 1}}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        code(&e),
        "NotFound",
        "the pass's writer is not on the surface: {e}"
    );
    let e = c
        .call("ai_pass", json!({"op": "run", "pass": "find_moments"}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Unsupported");

    // chunk 1 of 3: first read
    let s = c
        .ok(
            "ai_pass",
            json!({"op": "begin_self", "pass": "find_moments", "target": {}}),
        )
        .await;
    let pr = s["pass_run"].as_str().unwrap().to_owned();
    assert_eq!(s["step"]["kind"], "first");
    assert_eq!(s["step"]["of"], "chunk 1 of 3");
    assert_eq!(s["step"]["lines"], "L1–L240");
    assert_eq!(s["pages"], 1);
    assert!(s["instructions"].as_str().unwrap().contains("a 2 is"));
    assert!(
        s["contract"]
            .as_str()
            .unwrap()
            .contains("never computed times")
    );
    assert_eq!(s["untrusted"], json!(["lines", "already", "candidates"]));
    assert!(
        s["lines"][10]
            .as_str()
            .unwrap()
            .starts_with("L11 [0:50–0:54] <untrusted>line10 says alpha10"),
        "{}",
        s["lines"][10]
    );
    let misquote = json!({"start_line": "L50", "end_line": "L50", "quote": "words nobody said at all anywhere", "score": 3});
    let mut snapped = moment(&s, 101, 102, 3);
    snapped["start_line"] = json!("L60");
    snapped["end_line"] = json!("L61");
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [
                moment(&s, 11, 13, 4),
                snapped,
                misquote,
                {"start_line": "L999", "end_line": "L999", "quote": "x", "score": 3},
                {"start_line": "L5", "end_line": "L5", "quote": "line4", "score": 1},
            ]}),
        )
        .await;
    assert_eq!(
        out["submitted"]["accepted"], 0,
        "nothing is written mid-chunk"
    );
    assert_eq!(out["submitted"]["to_repair"], json!([2]));
    let refused = out["submitted"]["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 2);
    assert!(refused[0]["reason"].as_str().unwrap().contains("L1–L240"));
    assert!(refused[1]["reason"].as_str().unwrap().contains("2–5"));
    // the sweep is told what the first read found
    assert_eq!(out["step"]["kind"], "sweep");
    assert_eq!(out["already"].as_array().unwrap().len(), 3);
    assert!(
        out["already"][0]
            .as_str()
            .unwrap()
            .starts_with("<untrusted>L11–L13: \"line10 says"),
        "{}",
        out["already"][0]
    );
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [moment(&out, 151, 152, 2)]}),
        )
        .await;
    // the repair: the misquote comes back as a candidate
    assert_eq!(out["step"]["kind"], "repair");
    assert_eq!(out["candidates"].as_array().unwrap().len(), 1);
    assert!(
        out["candidates"][0]
            .as_str()
            .unwrap()
            .contains("words nobody said")
    );
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [moment(&out, 51, 51, 3)]}),
        )
        .await;
    assert_eq!(
        out["submitted"]["accepted"], 4,
        "the chunk closes: 2 + swept + repaired"
    );

    // chunk 2: a moment on the seam with chunk 3 is held, its better twin wins there
    assert_eq!(
        (out["step"]["kind"].as_str(), out["step"]["lines"].as_str()),
        (Some("first"), Some("L226–L465"))
    );
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [moment(&out, 301, 302, 3), moment(&out, 456, 457, 2)]}),
        )
        .await;
    assert_eq!(
        (
            out["step"]["kind"].as_str(),
            out["submitted"]["accepted"].as_u64()
        ),
        (Some("sweep"), Some(0))
    );
    let out = c
        .ok("pass_step", json!({"pass_run": pr, "submit": []}))
        .await;
    assert_eq!(out["submitted"]["accepted"], 1);
    assert_eq!(out["step"]["lines"], "L451–L540");
    // a page can be read again without submitting
    let again = c.ok("pass_step", json!({"pass_run": pr, "page": 0})).await;
    assert_eq!(again["step"], out["step"]);
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr, "submit": [moment(&out, 456, 457, 5), moment(&out, 521, 522, 3)]}),
        )
        .await;
    assert_eq!(out["step"]["kind"], "sweep");
    let out = c
        .ok("pass_step", json!({"pass_run": pr, "submit": []}))
        .await;
    assert_eq!(out["submitted"]["accepted"], 2);
    assert_eq!(out["submitted"]["duplicates"], 1);

    // the gaps, each an uncarded stretch ≥ 120 s; the second gets a moment
    let mut out = out;
    let mut gaps = Vec::new();
    while out["done"] == false {
        assert_eq!(out["step"]["kind"], "gap");
        assert_eq!(out["step"]["of"], "a gap");
        let lines = out["step"]["lines"].as_str().unwrap().to_owned();
        let submit = if gaps.len() == 1 {
            json!([moment(&out, 81, 82, 3)])
        } else {
            json!([])
        };
        gaps.push(lines);
        out = c
            .ok("pass_step", json!({"pass_run": pr, "submit": submit}))
            .await;
    }
    assert_eq!(
        gaps,
        [
            "L14–L50",
            "L52–L100",
            "L103–L150",
            "L153–L300",
            "L303–L455",
            "L458–L520"
        ]
    );
    let r = &out["record"];
    assert_eq!(r["state"], "done");
    assert_eq!(r["harness"], format!("agent:launched:{run}"));
    assert_eq!(
        (&r["model"], &r["effort"], &r["recall"]),
        (
            &json!("unverified"),
            &json!("unknown"),
            &json!("unverified")
        )
    );
    assert_eq!(r["label"], "Found by your agent — recall not measured");
    assert_eq!(r["stats"]["written"], 8);
    assert_eq!(r["stats"]["snapped"], 1);
    assert_eq!(r["stats"]["repaired"], 1);
    assert_eq!(r["stats"]["gap_found"], 1);
    assert_eq!(r["quote_verified"], 1.0);
    assert_eq!(r["density"]["ok"], false);
    assert!(
        r["density"]["verdict"]
            .as_str()
            .unwrap()
            .starts_with("THIN: 8 moments over 45 min")
    );
    assert!(
        out["display"]
            .as_str()
            .unwrap()
            .contains("recall not measured")
    );
    assert!(
        r["pass_ref"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/{pr}", FIND_MOMENTS.pass_ref()))
    );
    // a finished run takes nothing more
    let end = c
        .ok("pass_step", json!({"pass_run": pr, "submit": []}))
        .await;
    assert_eq!(end["done"], true);

    // the moments are in the panel, the agent's, on their lines' times
    let doc = p.handle.snapshot().doc.clone();
    let ms: Vec<&Moment> = doc.iter::<Moment>().collect();
    assert_eq!(ms.len(), 8);
    assert!(ms.iter().all(|m| m.origin == MomentOrigin::Agent
        && m.harness_label.is_none()
        && m.pass_ref.as_deref() == r["pass_ref"].as_str()));
    let twin = ms.iter().find(|m| m.quote.starts_with("line455 ")).unwrap();
    assert_eq!(twin.score, 5);
    let snapped = ms.iter().find(|m| m.quote.starts_with("line100 ")).unwrap();
    assert!(
        format!("{:?}", snapped.span.start).contains(&format!("t: Flicks({})", 500 * SEC)),
        "{:?}",
        snapped.span.start
    );
    assert!(ms.iter().all(|m| m.excerpt.exact));

    // another connection cannot step this run
    let e = human
        .call("pass_step", json!({"pass_run": pr, "submit": []}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound");

    // the full sweep: every chunk read against what is carded now
    w.control(&Control::LaunchEnd { run: run.clone() }).await;
    let brief = FULL_SWEEP.brief();
    let (run2, c) = launch(&w, &fx, &brief, [4; 32]).await;
    w.holder.restrict_commands(&run2, FULL_SWEEP.commands);
    let s = c
        .ok(
            "ai_pass",
            json!({"op": "begin_self", "pass": "full_sweep", "target": {"source": source}}),
        )
        .await;
    let pr2 = s["pass_run"].as_str().unwrap().to_owned();
    assert_eq!(
        (s["step"]["kind"].as_str(), s["step"]["round"].as_u64()),
        (Some("full_sweep"), Some(1))
    );
    let already = s["already"].as_array().unwrap();
    assert_eq!(already.len(), 5, "{already:?}");
    assert!(
        already[0]
            .as_str()
            .unwrap()
            .starts_with("<untrusted>[0:50 – 1:04] \"line10 says")
    );
    // a re-card of a carded moment is a duplicate; a new one is written
    let out = c
        .ok(
            "pass_step",
            json!({"pass_run": pr2, "submit": [moment(&s, 11, 12, 5), moment(&s, 201, 202, 4)]}),
        )
        .await;
    assert_eq!(out["submitted"]["accepted"], 1);
    assert_eq!(out["submitted"]["duplicates"], 1);
    let mut out = out;
    while out["done"] == false {
        out = c
            .ok("pass_step", json!({"pass_run": pr2, "submit": []}))
            .await;
    }
    let r = &out["record"];
    assert_eq!(r["pass"], "full_sweep");
    assert_eq!(r["stats"]["written"], 1);
    assert_eq!(
        r["steps_done"].as_array().unwrap().len(),
        3 + 3,
        "a second round: still thin"
    );
    let doc = p.handle.snapshot().doc.clone();
    let swept: Vec<&Moment> = doc
        .iter::<Moment>()
        .filter(|m| m.origin == MomentOrigin::Sweep)
        .collect();
    assert_eq!(swept.len(), 1);
    assert!(
        swept[0]
            .pass_ref
            .as_deref()
            .unwrap()
            .starts_with(&FULL_SWEEP.pass_ref())
    );
}
