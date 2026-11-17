//! A2.15 / R-ARCH-7, the holder's half: `Control::Handover` stops a transcription where
//! it is (its windows written to `<app data>/handover/<job>.json`), closes the project
//! (lock released), answers with the projects and jobs and stops; the next holder (the
//! app) opens the project, resumes the transcription at its next window — the result is
//! the transcript an uninterrupted run makes, and no window is heard twice — and an MCP
//! session through `gmclip mcp` carries on against it: the shim reconnects without
//! starting a holder, replays the handshake, and a `since` from the old holder is a resync.

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{SEC, base};
use gmclip_core::analysis::chunked::{self, AsrPartial};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::holder::{Control, Holder};
use gmclip_mcp::paths::{self, Env};
use gmclip_mcp::settings::OwnerSettings;
use gmclip_mcp::wire::Mode;
use gmclip_mcp::{client, project};
use gmclip_model::Flicks;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TOTAL_S: i64 = 400;

/// Hears a word every 5 s of whatever span it is given, slowly; records the spans.
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
        self.spans.lock().unwrap().push((a, b));
        for _ in 0..6 {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(HostError::Failed("cancelled".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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

async fn holder_on(env: &Env, host: Option<Arc<Slow>>) -> Arc<Holder> {
    let listener = gmclip_mcp::server::bind(env, &[]).await.unwrap();
    let mut h = Holder::new(env.clone());
    if let Some(a) = host {
        h = h.with_analysis(a);
    }
    let h = Arc::new(h);
    let h2 = Arc::clone(&h);
    tokio::spawn(async move {
        gmclip_mcp::server::serve(h2, listener, Duration::from_secs(3600)).await
    });
    h
}

/// A tool result's JSON (structured content, else its text).
fn out(w: &Value) -> Value {
    let r = &w["result"];
    if r["structuredContent"].is_object() {
        return r["structuredContent"].clone();
    }
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap_or("null")).unwrap_or(Value::Null)
}

async fn line(r: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    let mut l = String::new();
    tokio::time::timeout(Duration::from_secs(20), r.read_line(&mut l))
        .await
        .expect("an answer")
        .unwrap();
    serde_json::from_str(&l).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handover_resumes_the_transcription_and_the_session() {
    let base = base();
    let env = Env {
        app_data: base.join("app"),
        socket: common::socket_for(&base),
    };
    let rec = base.join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    OwnerSettings {
        read_roots: vec![rec.clone()],
        ..Default::default()
    }
    .save(&env.app_data)
    .unwrap();
    // a project whose one source is 400 s long
    let home = rec.join("p.gmclip");
    let info = project::create_home(&env.app_data, &home, "p").unwrap();
    let db = project::db_path(&env.app_data, &info.id).unwrap();
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

    // holder A (gmclip serve) opens it: the transcription starts
    let slow = Arc::new(Slow::default());
    let a = holder_on(&env, Some(Arc::clone(&slow))).await;
    a.open_project(home.to_str().unwrap(), true).unwrap();
    let key = format!("{}:{src}", info.id);
    for _ in 0..200 {
        if a.asr_partials
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|p| p.done >= 110 * SEC)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // an agent's session through the shim
    let (client_side, shim_side) = tokio::io::duplex(1 << 16);
    let (shim_in, shim_out) = tokio::io::split(shim_side);
    let env2 = env.clone();
    let shim = tokio::spawn(async move {
        let me = paths::our_exe();
        client::proxy(
            &env2,
            None,
            None,
            me.as_deref(),
            shim_in,
            shim_out,
            Duration::from_secs(20),
        )
        .await
    });
    let (cr, mut cw) = tokio::io::split(client_side);
    let mut cr = BufReader::new(cr);
    cw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\n").await.unwrap();
    assert_eq!(line(&mut cr).await["id"], 1);
    cw.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let watch = |id: u32, since: u64| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"project_watch\",\"arguments\":{{\"project\":\"{}\",\"since\":{since}}}}}}}\n",
            info.id
        )
    };
    cw.write_all(watch(2, 0).as_bytes()).await.unwrap();
    let w = line(&mut cr).await;
    assert_eq!(w["id"], 2, "{w}");
    let seen = out(&w)["next_since"].as_u64().unwrap_or(0);

    // the handover
    let me = paths::our_exe();
    let (mut s, _) = client::connect_as(&env, Mode::Control, false, None, me.as_deref())
        .await
        .unwrap();
    let r = client::request(&mut s, &Control::Handover).await.unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let r = &r["result"];
    assert_eq!(r["projects"][0]["id"], info.id.as_str());
    let job = r["jobs"][0].clone();
    assert_eq!(job["kind"], "analysis", "{r}");
    assert_eq!(job["source"], src.as_str());
    let done = job["done_flicks"].as_i64().unwrap();
    assert!(
        (110 * SEC..TOTAL_S * SEC).contains(&done),
        "stopped mid-way: {done}"
    );
    let file: Value = serde_json::from_slice(
        &std::fs::read(
            env.app_data
                .join("handover")
                .join(format!("{}.json", job["job"].as_str().unwrap())),
        )
        .unwrap(),
    )
    .unwrap();
    let partial: AsrPartial = serde_json::from_value(file["partial"].clone()).unwrap();
    assert_eq!(partial.done, done);
    // A is gone: its socket too
    // (socket_present, not exists(): a pipe name is not a file)
    for _ in 0..100 {
        if !env.socket_present() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!env.socket_present(), "holder A stopped");

    // holder B (the app) opens the project: the lock was released (B starts no
    // analysis of its own here: the resume below is what the app runs instead)
    let b = holder_on(&env, None).await;
    b.open_project(home.to_str().unwrap(), true).unwrap();
    // resume from the handed-over windows; nothing heard twice; same transcript
    let heard_before = slow.spans.lock().unwrap().len();
    let audio = AudioSource {
        path: "/m/stream.mkv".into(),
        track: None,
        fingerprint: "fp".into(),
    };
    let never = AtomicBool::new(false);
    let resumed = chunked::run(
        slow.as_ref(),
        &audio,
        TOTAL_S * SEC,
        partial,
        &never,
        |_| {},
    )
    .unwrap();
    let new_spans: Vec<(i64, i64)> = slow.spans.lock().unwrap()[heard_before..].to_vec();
    assert!(
        new_spans
            .iter()
            .all(|(a, _)| *a >= done - chunked::PAD_S * SEC),
        "resumed at its next window: {new_spans:?}"
    );
    let fresh = chunked::run(
        &Slow::default(),
        &audio,
        TOTAL_S * SEC,
        AsrPartial::default(),
        &never,
        |_| {},
    )
    .unwrap();
    assert_eq!(
        resumed, fresh,
        "the same transcript as an uninterrupted run"
    );
    assert_eq!(resumed.words.len() as i64, TOTAL_S / 5);

    // the session carries on against B: a `since` from A is a resync, then fresh reads
    cw.write_all(watch(3, seen.max(5)).as_bytes())
        .await
        .unwrap();
    let w = line(&mut cr).await;
    assert_eq!(w["id"], 3, "{w}");
    assert_eq!(out(&w)["resync"], true, "{w}");
    cw.write_all(watch(4, 0).as_bytes()).await.unwrap();
    let w = line(&mut cr).await;
    assert_eq!(w["id"], 4);
    assert!(out(&w)["events"].is_array(), "{w}");
    drop(cw);
    let _ = tokio::time::timeout(Duration::from_secs(5), shim).await;
    b.shutdown.notify_one();
    b.close_all();
    let _ = std::fs::remove_dir_all(&base);
}
