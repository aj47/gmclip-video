//! A2.02: ASR resumes from its last finished window. `chunked::run_stored` keeps every
//! finished window as an analysis blob in the project; a transcription cancelled (or
//! killed: the process gone, the project reopened) picks up at the window after the last
//! one stored, hears nothing twice, and ends with the transcript an uninterrupted run
//! makes. `force` after a finished run hears it again as a new generation.

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{SEC, open};
use gmclip_core::analysis::chunked::{self, AsrPartial};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, Transcribed,
};
use gmclip_model::Flicks;

const TOTAL_S: i64 = 400;

/// Hears a word every 5 s of the span it is given; records the spans.
#[derive(Debug, Default)]
struct Ears {
    spans: Mutex<Vec<(i64, i64)>>,
}

impl AnalysisHost for Ears {
    fn name(&self) -> &'static str {
        "ears"
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
        let (a, b) = span.map_or((0, TOTAL_S * SEC), |(a, b)| (a.0, b.0));
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
            revision: "ears@1/span".into(),
            words,
        })
    }
    fn listen(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<Heard, HostError> {
        Err(HostError::Unsupported("ears"))
    }
    fn loudness(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<SpanLoudness, HostError> {
        Err(HostError::Unsupported("ears"))
    }
    fn edge(
        &self,
        _: &AudioSource,
        _: EdgeRule,
        _: (&str, Flicks, Flicks),
        _: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        Err(HostError::Unsupported("ears"))
    }
    fn pauses(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: Flicks,
        _: Flicks,
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("ears"))
    }
    fn fillers(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        _: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        Err(HostError::Unsupported("ears"))
    }
    fn blob_summary(
        &self,
        _: &str,
        _: &[u8],
        _: Flicks,
        _: Flicks,
        _: usize,
    ) -> Result<serde_json::Value, HostError> {
        Err(HostError::Unsupported("ears"))
    }
}

fn audio() -> AudioSource {
    AudioSource {
        path: "/m/stream.mkv".into(),
        track: Some(1),
        fingerprint: "fp".into(),
    }
}

/// One run against the project's store, cancelled after `stop_after` windows if given.
fn hear(
    p: &common::Project,
    ears: &Ears,
    force: bool,
    stop_after: Option<usize>,
) -> Result<(AsrPartial, chunked::Resumed), HostError> {
    let h = &p.h;
    let cancel = AtomicBool::new(false);
    let mut n = 0;
    chunked::run_stored(
        ears,
        &audio(),
        TOTAL_S * SEC,
        &h.analysis_keys(),
        "src",
        |r| h.analysis("src", chunked::WINDOW_KIND, r),
        |r, body| {
            h.put_analysis("src", chunked::WINDOW_KIND, r, body)
                .unwrap()
        },
        force,
        &cancel,
        |_| {
            n += 1;
            if stop_after == Some(n) {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
}

#[test]
fn a_transcription_resumes_from_its_last_finished_window() {
    let fresh = chunked::run(
        &Ears::default(),
        &audio(),
        TOTAL_S * SEC,
        AsrPartial::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(fresh.words.len() as i64, TOTAL_S / 5);

    // cancelled after 3 windows (20 s + 90 s + 90 s)
    let p = open("asr-resume");
    let ears = Ears::default();
    assert!(hear(&p, &ears, false, Some(3)).is_err());
    let stored = 200 * SEC;
    let (_, part) = chunked::stored(
        &p.h.analysis_keys(),
        "src",
        &chunked::window_prefix(&audio()),
        |r| p.h.analysis("src", chunked::WINDOW_KIND, r),
    )
    .unwrap();
    assert_eq!(part.done, stored);

    // the process is gone (the project closed without a word), and opens again
    let p = p.reopen();
    let ears = Ears::default();
    let (done, resumed) = hear(&p, &ears, false, None).unwrap();
    assert_eq!(
        resumed.at, stored,
        "resumed at the window after the last stored"
    );
    assert_eq!(resumed.generation, 1);
    let spans = ears.spans.lock().unwrap().clone();
    assert!(
        spans
            .iter()
            .all(|(a, _)| *a >= stored - chunked::PAD_S * SEC),
        "nothing before it heard again: {spans:?}"
    );
    assert_eq!(spans.len(), 3, "200 → 290 → 380 → 400 s");
    assert_eq!(done, fresh, "the transcript an uninterrupted run makes");

    // finished: running again hears nothing; forced, it hears it all as generation 2
    let ears = Ears::default();
    let (again, r) = hear(&p, &ears, false, None).unwrap();
    assert_eq!(again, fresh);
    assert_eq!(r.at, TOTAL_S * SEC);
    assert!(ears.spans.lock().unwrap().is_empty());
    let (forced, r) = hear(&p, &ears, true, None).unwrap();
    assert_eq!(forced, fresh);
    assert_eq!((r.generation, r.at), (2, 0));
    assert_eq!(ears.spans.lock().unwrap().len(), 6);
    p.h.close().unwrap();
}
