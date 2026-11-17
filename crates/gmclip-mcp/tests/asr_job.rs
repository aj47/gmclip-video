//! A2.02 through the agent surface: `asr_run` is a job with progress; `job_cancel` stops
//! a transcription at its next window, and running it again resumes from the last
//! finished one (its windows are in the project), hears nothing twice, stores the
//! transcript and makes it the active one.

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{SEC, World, code};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::project;
use gmclip_model::Flicks;
use serde_json::{Value, json};

const TOTAL_S: i64 = 400;

/// Hears a word every 5 s of the span it is given, a little slowly; records the spans.
#[derive(Debug, Default)]
struct Slow {
    spans: Mutex<Vec<(i64, i64)>>,
}

impl AnalysisHost for Slow {
    fn name(&self) -> &'static str {
        "slow"
    }
    fn analyze(&self, _: &AudioSource, _: &AtomicBool) -> Result<Vec<AnalysisBlob>, HostError> {
        Ok(vec![])
    }
    fn transcribe(
        &self,
        _: &AudioSource,
        span: Option<(Flicks, Flicks)>,
        cancel: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        let (a, b) = span.map_or((0, TOTAL_S * SEC), |(a, b)| (a.0, b.0));
        for _ in 0..4 {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(HostError::Failed("cancelled".into()));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        self.spans.lock().unwrap().push((a, b));
        let words = (0..TOTAL_S / 5)
            .map(|k| k * 5 * SEC + SEC)
            .filter(|t| *t >= a && *t + SEC / 2 <= b)
            .map(|t| HeardWord {
                text: format!("w{}", t / SEC),
                start: Flicks(t),
                end: Flicks(t + SEC / 2),
                confidence: None,
            })
            .collect();
        Ok(Transcribed {
            revision: "slow@1/span".into(),
            words,
        })
    }
    fn listen(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<Heard, HostError> {
        Err(HostError::Unsupported("slow"))
    }
    fn loudness(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<SpanLoudness, HostError> {
        Err(HostError::Unsupported("slow"))
    }
    fn edge(
        &self,
        _: &AudioSource,
        _: EdgeRule,
        _: (&str, Flicks, Flicks),
        _: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        Err(HostError::Unsupported("slow"))
    }
    fn pauses(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: Flicks,
        _: Flicks,
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("slow"))
    }
    fn fillers(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("slow"))
    }
    fn blob_summary(
        &self,
        _: &str,
        _: &[u8],
        _: Flicks,
        _: Flicks,
        _: usize,
    ) -> Result<Value, HostError> {
        Err(HostError::Unsupported("slow"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asr_runs_as_a_job_and_resumes_after_a_cancel() {
    let slow = Arc::new(Slow::default());
    let w = World::start_full(None, Some(Arc::clone(&slow) as Arc<dyn AnalysisHost>)).await;
    // a project whose one source is 400 s long
    let home = w.rec.join("asr.gmclip");
    let info = project::create_home(&w.env.app_data, &home, "asr").unwrap();
    let db = project::db_path(&w.env.app_data, &info.id).unwrap();
    let h = ProjectHandle::open(&db, OpenOptions::default()).unwrap();
    let src = h
        .call(CallRequest {
            command: "source_add".into(),
            args: json!({"path": "/m/stream.mkv", "fingerprint": "fp",
                         "probe": {"duration": TOTAL_S * SEC, "tracks": [], "audio_tracks": []}}),
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
        .unwrap()
        .output["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.close().unwrap();

    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": home.to_str().unwrap()}}),
    )
    .await;
    // opening started the background analysis, which transcribes first: asr_run joins it
    let first = c
        .ok(
            "command_run",
            json!({"command": "asr_run", "args": {"source": src}}),
        )
        .await;
    let job = first["job"].as_str().unwrap().to_owned();
    // wait until two windows are heard, then cancel
    for _ in 0..200 {
        if slow.spans.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let st = c
        .ok(
            "command_run",
            json!({"command": "job_cancel", "args": {"job": job}}),
        )
        .await;
    assert_eq!(st["state"], "cancelled", "{st}");
    let heard = slow.spans.lock().unwrap().clone();
    let done = heard.iter().map(|s| s.1).max().unwrap() - common::SEC * 3;
    assert!(heard.len() >= 2 && heard.len() < 6, "{heard:?}");

    // asr_run again: a job of its own, resumed after the last finished window
    let again = c
        .ok(
            "command_run",
            json!({"command": "asr_run", "args": {"source": src}}),
        )
        .await;
    let job2 = again["job"].as_str().unwrap().to_owned();
    assert_ne!(job2, job);
    let mut progress_seen = false;
    let mut res = Value::Null;
    for _ in 0..100 {
        let r = c
            .ok(
                "command_run",
                json!({"command": "job_wait", "args": {"job": job2, "timeout_s": 1}}),
            )
            .await;
        progress_seen |= r["progress"]["step"] == "transcribing";
        if r["state"] != "running" {
            res = r;
            break;
        }
    }
    assert_eq!(res["state"], "done", "{res}");
    assert_eq!(res["kind"], "asr");
    let out = &res["result"];
    assert_eq!(out["words"], TOTAL_S / 5, "{out}");
    assert_eq!(out["activated"], true);
    let at = (out["resumed_at_s"].as_f64().unwrap() * SEC as f64).round() as i64;
    assert!(at > 0 && at <= done + 3 * SEC, "{out}");
    assert!(progress_seen || res["progress"]["done_s"].as_f64() == Some(TOTAL_S as f64));
    let later: Vec<(i64, i64)> = slow.spans.lock().unwrap()[heard.len()..].to_vec();
    assert!(
        later
            .iter()
            .all(|(a, _)| *a >= at - gmclip_core::analysis::chunked::PAD_S * SEC),
        "nothing before {at} heard again: {later:?}"
    );
    // the transcript is the active one
    let t = c.ok("transcript_read", json!({"source": src})).await;
    assert!(
        t["asr_revision"].as_str().unwrap().starts_with("slow@1"),
        "{t}"
    );

    // another identity cannot cancel this one's job; an unknown job is NotFound
    let other = w.client("codex", None).await;
    let e = other
        .call(
            "command_run",
            json!({"command": "job_cancel", "args": {"job": job2}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound");
}

/// A2.12: "Process this stream" as one job: a member of a sync group is processed as
/// its group (heard once), the steps run in the recipe's order, the agent passes wait
/// for a click, and a second run skips what is there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_this_stream_runs_the_recipe_as_a_job_chain() {
    let slow = Arc::new(Slow::default());
    let w = World::start_full(None, Some(Arc::clone(&slow) as Arc<dyn AnalysisHost>)).await;
    let home = w.rec.join("recipe.gmclip");
    let info = project::create_home(&w.env.app_data, &home, "recipe").unwrap();
    let db = project::db_path(&w.env.app_data, &info.id).unwrap();
    let h = ProjectHandle::open(&db, OpenOptions::default()).unwrap();
    let call = |cmd: &str, args: Value| {
        h.call(CallRequest {
            command: cmd.into(),
            args,
            base: None,
            request_id: None,
            caller: Caller::human(),
        })
        .unwrap()
        .output
    };
    let probe = json!({"duration": TOTAL_S * SEC, "tracks": [], "audio_tracks": []});
    let voice = call(
        "source_add",
        json!({"path": "/m/proxy.mp4", "fingerprint": "fpa", "probe": probe}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let picture = call(
        "source_add",
        json!({"path": "/m/1440p.mkv", "fingerprint": "fpv", "probe": probe}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let group = call(
        "sync_group_create",
        json!({"members": [{"source": voice, "role": "audio", "offset": 0}, {"source": picture, "role": "picture", "offset": 0}]}),
    )["group"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        "moment_add",
        json!({"media": {"group": group}, "start_s": 30.0, "end_s": 50.0, "quote": "w31 w36", "score": 4, "type": "result", "why": "test"}),
    );
    h.close().unwrap();

    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": home.to_str().unwrap()}}),
    )
    .await;
    // the background analysis of the sources first
    for _ in 0..500 {
        if w.holder.jobs.running().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = c
        .ok(
            "command_run",
            json!({"command": "stream_process", "args": {"source": picture}}),
        )
        .await;
    assert_eq!(
        started["owner"],
        group.as_str(),
        "a member is processed as its group"
    );
    let job = started["job"].as_str().unwrap().to_owned();
    let r = c
        .ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": job, "timeout_s": 60}}),
        )
        .await;
    assert_eq!(r["state"], "done", "{r}");
    assert_eq!(r["kind"], "recipe");
    let steps = r["result"]["steps"].as_array().unwrap();
    let names: Vec<&str> = steps.iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "transcribe",
            "analyse",
            "find_moments",
            "full_sweep",
            "density",
            "delivery",
            "visual_scout"
        ]
    );
    let state = |i: usize| steps[i]["state"].as_str().unwrap();
    assert_eq!(state(0), "done", "{}", steps[0]);
    assert_eq!(steps[0]["detail"], "heard once for the sync group");
    assert_eq!(steps[0]["result"]["owner"], group.as_str());
    assert_eq!(state(1), "done");
    assert_eq!(
        (state(2), state(3)),
        ("waiting", "waiting"),
        "agent passes wait for a click"
    );
    assert_eq!(state(4), "done", "{}", steps[4]);
    assert_eq!(steps[4]["density"]["moments"], 1);
    assert_eq!((state(5), state(6)), ("later", "later"));
    assert_eq!(r["result"]["next"], "find_moments");
    // the group's transcript is the active one
    let t = c.ok("transcript_read", json!({"source": group})).await;
    assert!(
        t["asr_revision"].as_str().unwrap().starts_with("slow@1"),
        "{t}"
    );

    // again: what is there is skipped; only some steps when asked
    let heard = slow.spans.lock().unwrap().len();
    let again = c
        .ok(
            "command_run",
            json!({"command": "stream_process", "args": {"source": voice, "steps": ["transcribe", "density"]}}),
        )
        .await;
    let r = c
        .ok(
            "command_run",
            json!({"command": "job_wait", "args": {"job": again["job"], "timeout_s": 60}}),
        )
        .await;
    assert_eq!(r["result"]["steps"][0]["state"], "skipped");
    assert_eq!(r["result"]["steps"].as_array().unwrap().len(), 2);
    assert_eq!(
        slow.spans.lock().unwrap().len(),
        heard,
        "nothing heard twice"
    );
    let e = c
        .call(
            "command_run",
            json!({"command": "stream_process", "args": {"source": voice, "steps": ["upload"]}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
}
