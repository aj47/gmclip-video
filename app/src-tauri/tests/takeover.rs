//! A2.15 / R-ARCH-7, the app's half: opening a project `gmclip serve` holds takes it over.
//! The holder hands over mid-transcription; the app opens the project, resumes the
//! transcription under the holder's job id at its next window (no window heard twice,
//! the transcript an uninterrupted run makes) and serves the gmclip socket itself, so
//! agents reconnect to it. A fake, slow ASR host stands in for Parakeet.
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use gmclip_core::analysis::chunked::{self, AsrPartial};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::holder::Holder;
use gmclip_mcp::paths::{self, Env};
use gmclip_mcp::wire::Mode;
use gmclip_model::Flicks;
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;
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

#[test]
fn opening_a_held_project_takes_it_over_and_resumes_asr() {
    let root = std::env::temp_dir().join(format!("gmclip-take-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    b.mode(0o700);
    b.create(root.join("run")).unwrap();
    let env = Env {
        app_data: root.join("app"),
        // Windows: a pipe of its own (pipes live in \\.\pipe\, not in a directory)
        socket: if cfg!(windows) {
            paths::pipe_name_for(&format!("test-take-{}", std::process::id()))
        } else {
            root.join("run").join("s")
        },
    };
    let home = root.join("lib").join("p.gmclip");
    let info = gmclip_mcp::project::create_home(&env.app_data, &home, "p").unwrap();
    let db = gmclip_mcp::project::db_path(&env.app_data, &info.id).unwrap();
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

    // `gmclip serve` holds it and is transcribing
    let slow = Arc::new(Slow::default());
    let a = Arc::new(Holder::new(env.clone()).with_analysis(slow.clone()));
    let a2 = Arc::clone(&a);
    let env2 = env.clone();
    let served = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let l = gmclip_mcp::server::bind(&env2, &[]).await.unwrap();
            gmclip_mcp::server::serve(a2, l, Duration::from_secs(3600)).await;
        });
    });
    for _ in 0..200 {
        // (socket_present, not exists(): a pipe name is not a file)
        if env.socket_present() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    a.open_project(home.to_str().unwrap(), true).unwrap();
    let key = format!("{}:{src}", info.id);
    for _ in 0..300 {
        if a.asr_partials
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|p| p.done >= 110 * SEC)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let job = a
        .jobs
        .running()
        .first()
        .map(|j| j.id.clone())
        .expect("the holder's ASR job");

    // the app: its agent server finds `gmclip serve` on the socket
    let mine = Arc::new(Slow::default());
    let c = AppCore::with_analyzer(
        Dirs {
            app_data: root.join("app"),
            library: root.join("lib"),
            exports: root.join("exports"),
        },
        gmclip_app::analysis::Analyzer::with_host(mine.clone()),
    );
    c.agents.start(&c, env.clone());
    std::thread::sleep(Duration::from_millis(300));

    // the human opens the project: taken over
    let v = c.open_home(&home).unwrap();
    assert_eq!(v["id"], info.id.as_str());
    served.join().unwrap();
    let pr = c.analysis.progress(&info.id, &src).expect("resumed here");
    assert_eq!(pr.job.as_deref(), Some(job.as_str()), "the holder's job id");
    let at = pr.resumed_at_s.unwrap();
    assert!((110.0..400.0).contains(&at), "resumed mid-way: {at}");
    for _ in 0..500 {
        if c.analysis
            .progress(&info.id, &src)
            .is_some_and(|p| p.phase == "done" || p.phase == "failed")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let pr = c.analysis.progress(&info.id, &src).unwrap();
    assert_eq!(pr.phase, "done", "{:?} {:?}", pr.error, pr.asr_error);
    // no window heard twice; the stored transcript is the uninterrupted one
    let first = mine
        .spans
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.0)
        .min()
        .unwrap();
    assert!(
        first >= (at * SEC as f64) as i64 - chunked::PAD_S * SEC,
        "heard from {first}"
    );
    let audio = AudioSource {
        path: "/m/stream.mkv".into(),
        track: None,
        fingerprint: "fp".into(),
    };
    let fresh = chunked::run(
        &Slow::default(),
        &audio,
        TOTAL_S * SEC,
        AsrPartial::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let p = c.project(&info.id).unwrap();
    let keys = p.handle.analysis_keys();
    let (_, _, rev) = keys
        .iter()
        .find(|(o, k, _)| *o == src && k == "words")
        .expect("stored words")
        .clone();
    assert_eq!(rev, fresh.revision);
    assert_eq!(pr.words.len(), fresh.words.len());
    assert_eq!(*pr.words, fresh.words);

    // the app is the agent server now
    let mut ok = false;
    for _ in 0..200 {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let me = paths::our_exe();
        if rt
            .block_on(gmclip_mcp::client::connect_as(
                &env,
                Mode::Control,
                false,
                None,
                me.as_deref(),
            ))
            .is_ok()
        {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(ok, "the app serves the gmclip socket");
    assert!(
        !env.app_data
            .join("handover")
            .join(format!("{job}.json"))
            .exists(),
        "handover file consumed"
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
