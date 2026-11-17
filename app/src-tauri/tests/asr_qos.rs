//! D.01: ASR on import yields to playback. Between windows it pauses only when the
//! scheduler has reported a dropped frame in the last 2 s, and resumes 5 s after that
//! drop; with nothing dropped it never waits. A fake, slow ASR host stands in for
//! Parakeet; the render thread's report is made directly (`AppCore::qos`).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_app::qos::QosEvent;
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_model::Flicks;
use serde_json::{Value, json};

const SEC: i64 = 705_600_000;
const TOTAL_S: i64 = 400;

/// Takes 400 ms per window; records when each window started.
#[derive(Debug, Default)]
struct Slow {
    starts: Mutex<Vec<Instant>>,
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
        _: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        self.starts.lock().unwrap().push(Instant::now());
        std::thread::sleep(Duration::from_millis(400));
        let (a, b) = span.map_or((0, TOTAL_S * SEC), |(a, b)| (a.0, b.0));
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

fn wait_for(mut f: impl FnMut() -> bool, max: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn asr_pauses_after_a_dropped_frame_and_resumes_five_seconds_later() {
    let root = std::env::temp_dir().join(format!("gmclip-asrqos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let app_data = root.join("app");
    let home = root.join("lib").join("p.gmclip");
    let info = gmclip_mcp::project::create_home(&app_data, &home, "p").unwrap();
    let db = gmclip_mcp::project::db_path(&app_data, &info.id).unwrap();
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

    let slow = Arc::new(Slow::default());
    let c = AppCore::with_analyzer(
        Dirs {
            app_data,
            library: root.join("lib"),
            exports: root.join("exports"),
        },
        gmclip_app::analysis::Analyzer::with_host(slow.clone()),
    );
    c.open_home(&home).unwrap();
    let pid = info.id.clone();
    c.analysis.ensure(&c, &pid);
    // ASR starts on open; let it hear a window with nothing dropped (no pause)
    assert!(wait_for(
        || slow.starts.lock().unwrap().len() >= 2,
        Duration::from_secs(10)
    ));
    assert!(c.qos.trace().is_empty(), "no drop, no pause");
    assert!(!c.analysis.progress(&pid, &src).unwrap().paused);

    // the scheduler drops a frame while playing
    let dropped = Instant::now();
    c.qos.frame_dropped(dropped);
    let before = slow.starts.lock().unwrap().len();
    assert!(
        wait_for(
            || c.analysis.progress(&pid, &src).is_some_and(|p| p.paused),
            Duration::from_secs(2)
        ),
        "paused at the next window"
    );
    // no window starts until 5 s after the drop
    assert!(wait_for(
        || c.analysis.progress(&pid, &src).is_some_and(|p| !p.paused),
        Duration::from_secs(8)
    ));
    let starts = slow.starts.lock().unwrap().clone();
    let next = starts[before..].first().copied();
    assert!(
        wait_for(
            || slow.starts.lock().unwrap().len() > before,
            Duration::from_secs(2)
        ),
        "resumed"
    );
    let next = next.unwrap_or_else(|| slow.starts.lock().unwrap()[before]);
    let gap = next.duration_since(dropped);
    assert!(gap >= Duration::from_secs(5), "{gap:?}");
    assert!(gap < Duration::from_millis(5600), "{gap:?}");
    // the trace: one pause within 2 s of the drop, one resume 5 s after it
    let tr = c.qos.trace();
    eprintln!("trace: {tr:?}");
    assert_eq!(tr.len(), 2, "{tr:?}");
    match (&tr[0], &tr[1]) {
        (QosEvent::Paused { since, .. }, QosEvent::Resumed { after, .. }) => {
            assert!(*since <= Duration::from_secs(2));
            assert!(*after >= Duration::from_secs(5) && *after < Duration::from_millis(5200));
        }
        other => panic!("{other:?}"),
    }
    // it runs to the end, paused once
    assert!(wait_for(
        || c.analysis
            .progress(&pid, &src)
            .is_some_and(|p| p.phase == "done"),
        Duration::from_secs(20)
    ));
    let pr = c.analysis.progress(&pid, &src).unwrap();
    assert_eq!(pr.pauses, 1);
    assert!(!pr.paused);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
