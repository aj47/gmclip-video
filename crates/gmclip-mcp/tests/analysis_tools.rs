//! Analysis through the agent surface, on a fake analysis host (the real one, lane D's
//! DSP and Parakeet, is tested in gmclip-cli): background analysis on open, the first
//! transcript made active, `transcript_read`, word anchors resolved against the stored
//! words, `measure`, `analysis_get`, the sweeps, and the ASR switch re-anchoring.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{SEC, World, code};
use gmclip_core::host::{
    AnalysisBlob, AnalysisHost, AudioSource, EdgeRule, Heard, HeardWord, HostError, MeasuredEdge,
    SpanLoudness, SweepCandidates, SweepSpan, Transcribed,
};
use gmclip_model::Flicks;
use serde_json::{Value, json};

const REV: &str = "fake-asr@1";
/// The quiet grid's generation: a test bumps it to change the analysis under a preview.
static QUIET_GEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
/// Words on the fixture source's clock (parts play 100–102, 103–105, 106–108 s).
const WORDS: &[(&str, f64, f64)] = &[
    ("so", 100.2, 100.4),
    ("um", 100.6, 100.9),
    ("this", 101.2, 101.4),
    ("took", 101.45, 101.7),
    ("sixty", 103.2, 103.5),
    ("seven", 103.5, 103.8),
    ("seconds.", 103.9, 104.4),
];

fn f(s: f64) -> Flicks {
    Flicks((s * SEC as f64).round() as i64)
}

#[derive(Debug, Default)]
struct FakeAnalysis;

impl AnalysisHost for FakeAnalysis {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn analyze(&self, _: &AudioSource, _: &AtomicBool) -> Result<Vec<AnalysisBlob>, HostError> {
        Ok(["energy_10ms", "energy_20ms", "silences_40ms"]
            .iter()
            .map(|k| AnalysisBlob {
                kind: (*k).into(),
                revision: "fake@1/fp".into(),
                body: k.as_bytes().to_vec(),
            })
            .collect())
    }
    fn transcribe(
        &self,
        _: &AudioSource,
        _: Option<(Flicks, Flicks)>,
        _: &AtomicBool,
    ) -> Result<Transcribed, HostError> {
        Ok(Transcribed {
            revision: REV.into(),
            words: WORDS
                .iter()
                .map(|(t, a, b)| HeardWord {
                    text: (*t).into(),
                    start: f(*a),
                    end: f(*b),
                    confidence: None,
                })
                .collect(),
        })
    }
    fn listen(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<Heard, HostError> {
        Err(HostError::Unsupported("fake"))
    }
    fn loudness(&self, _: &AudioSource, _: Flicks, _: Flicks) -> Result<SpanLoudness, HostError> {
        Ok(SpanLoudness {
            integrated_lufs: -16.0,
            true_peak_dbtp: Some(-2.0),
        })
    }
    fn edge(
        &self,
        _: &AudioSource,
        rule: EdgeRule,
        w1: (&str, Flicks, Flicks),
        w2: Option<(&str, Flicks, Flicks)>,
    ) -> Result<MeasuredEdge, HostError> {
        let v = match (rule, w2) {
            (EdgeRule::EnergyMin | EdgeRule::Arbiter, Some(w2)) => Flicks((w1.2.0 + w2.1.0) / 2),
            (EdgeRule::AudibleStart, _) => Flicks(w1.1.0 - SEC / 100),
            _ => Flicks(w1.2.0 + SEC / 100),
        };
        Ok(MeasuredEdge {
            value: v,
            method: format!("fake {rule:?}"),
            window: (w1.1, w1.2),
            rms_db: Some(-50.0),
        })
    }
    fn pauses(
        &self,
        audio: &AudioSource,
        spans: &[(Flicks, Flicks)],
        _: Flicks,
        _: Flicks,
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        if audio.path.ends_with("graphic.mp4") {
            return Err(HostError::Unsupported("the source has no audio"));
        }
        // the gap between "this took" and the part's end in the first part
        let first = spans
            .iter()
            .min()
            .copied()
            .ok_or(HostError::Unsupported("no spans"))?;
        assert!(
            words.iter().all(|w| w.end > first.0),
            "words are on the source clock"
        );
        Ok(SweepCandidates {
            cuts: vec![SweepSpan {
                from: f(101.75),
                to: f(101.95),
                reason: "pause".into(),
            }],
            skipped: vec![],
            revision: format!(
                "quiet@{}/fp",
                QUIET_GEN.load(std::sync::atomic::Ordering::Relaxed)
            ),
        })
    }
    fn fillers(
        &self,
        _: &AudioSource,
        _: &[(Flicks, Flicks)],
        words: &[HeardWord],
    ) -> Result<SweepCandidates, HostError> {
        let um = words
            .iter()
            .find(|w| w.text == "um")
            .ok_or(HostError::Unsupported("no um"))?;
        Ok(SweepCandidates {
            cuts: vec![SweepSpan {
                from: um.start,
                to: um.end,
                reason: "filler \"um\"".into(),
            }],
            skipped: vec![],
            revision: "fillers@1/fp".into(),
        })
    }
    fn blob_summary(
        &self,
        kind: &str,
        body: &[u8],
        _: Flicks,
        _: Flicks,
        _: usize,
    ) -> Result<Value, HostError> {
        Ok(
            json!({"points": [[100.0, -20.5]], "kind_seen": kind, "body": String::from_utf8_lossy(body)}),
        )
    }
}

async fn wait_transcript(c: &common::Client, source: &str) -> Value {
    for _ in 0..50 {
        let v = c.ok("transcript_read", json!({"source": source})).await;
        if let Some(job) = v.get("job").and_then(Value::as_str) {
            c.ok(
                "command_run",
                json!({"command": "job_wait", "args": {"job": job, "timeout_s": 10}}),
            )
            .await;
            continue;
        }
        return v;
    }
    panic!("no transcript");
}

fn word_ref(t: &Value, ordinal: u64) -> Value {
    let w = t["lines"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|l| l["words"].as_array().unwrap().iter())
        .find(|w| w[0] == json!(ordinal))
        .unwrap()
        .clone();
    json!({"word_id": {"asr_revision": t["asr_revision"], "ordinal": ordinal}, "t_src_start": w[2], "t_src_end": w[3], "normalised_text": w[1]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analysis_reaches_the_agent_surface() {
    let w = World::start_full(None, Some(Arc::new(FakeAnalysis))).await;
    let fx = w.fixture("a", 3, "Hook");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let ov = c.ok("project_overview", json!({})).await;
    let source = ov["sources"][0]["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            // the overview lists sources by id; fall back to the first part's media
            String::new()
        });
    let cut = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let source = if source.is_empty() {
        cut["parts"][0]["media"]["source"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        source
    };

    // the background job made the transcript and made it active
    let t = wait_transcript(&c, &source).await;
    assert_eq!(t["asr_revision"], REV);
    assert_eq!(t["lines"][0]["l"], "L1");
    assert_eq!(t["lines"][0]["text"]["untrusted"], true);
    assert!(
        t["display"]
            .as_str()
            .unwrap()
            .contains("L2 [103.2–104.4] <untrusted>sixty seven seconds.</untrusted>")
    );
    let s = c
        .ok(
            "transcript_read",
            json!({"source": source, "search": "SIXTY"}),
        )
        .await;
    assert_eq!(s["total_lines"], 1);
    let around = c
        .ok(
            "transcript_read",
            json!({"source": source, "around": {"part_start": {"part": fx.parts[2]}}}),
        )
        .await;
    assert_eq!(around["total_lines"], 2, "lines within 30 s of 106 s");
    let far = c
        .ok(
            "transcript_read",
            json!({"source": source, "from_s": 103.0, "to_s": 200.0}),
        )
        .await;
    assert_eq!(far["lines"][0]["l"], "L2");

    // a word anchor resolves against the stored word (anchored, not raw)
    let took = word_ref(&t, 3);
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    let g = c
        .ok("command_run", json!({"command": "gain_segment_add", "base": {"as_of": as_of}, "args": {"cut": fx.cut, "start": {"word_start": word_ref(&t, 2)}, "end": {"word_end": took}, "gain_db": 2}}))
        .await;
    assert_eq!(g["output"]["from"]["provenance"], "anchored", "{g}");
    assert_eq!(g["output"]["from"]["method"], "word");
    // a fingerprint that disagrees with the stored word is refused
    let mut bad = word_ref(&t, 2);
    bad["t_src_start"] = json!(bad["t_src_start"].as_i64().unwrap() + SEC);
    let as_of = g["as_of"].as_u64().unwrap();
    let e = c
        .call("command_run", json!({"command": "gain_segment_add", "base": {"as_of": as_of}, "args": {"cut": fx.cut, "start": {"word_start": bad}, "end": {"word_end": word_ref(&t, 3)}, "gain_db": 2}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");

    // measure: an interval between two words, an edge, loudness
    let m = c.ok("measure", json!({"kind": "interval", "spec": {"from": {"anchor": {"word_start": word_ref(&t, 4)}}, "to": {"anchor": {"word_end": word_ref(&t, 6)}}}})).await;
    assert!((m["value"].as_f64().unwrap() - 1.2).abs() < 1e-6, "{m}");
    assert!(m["measurement"].as_str().is_some());
    let cp =
        json!({"cut_point": {"word": word_ref(&t, 4), "side": "before", "rule": "energy_min"}});
    let e = c
        .ok("measure", json!({"kind": "edge", "spec": {"anchor": cp}}))
        .await;
    assert!(
        (e["value"].as_f64().unwrap() - 102.45).abs() < 1e-6,
        "between 'took' (ends 101.7) and 'sixty' (starts 103.2): {e}"
    );
    let l = c
        .ok(
            "measure",
            json!({"kind": "loudness", "spec": {"cut": fx.cut}}),
        )
        .await;
    assert!((l["value"].as_f64().unwrap() + 16.0).abs() < 1e-9);
    // the cut point now resolves to the measured edge
    let iv = c.ok("measure", json!({"kind": "interval", "spec": {"from": {"anchor": cp}, "to": {"anchor": {"word_end": word_ref(&t, 4)}}}})).await;
    assert!(
        iv["method"]
            .as_str()
            .unwrap()
            .starts_with("measured cut point"),
        "{iv}"
    );
    // the holder's own writes are not agent commands
    assert_eq!(
        code(
            &c.call(
                "command_run",
                json!({"command": "measurement_add", "args": {}})
            )
            .await
            .unwrap_err()
        ),
        "NotFound"
    );
    assert_eq!(
        code(
            &c.call("command_run", json!({"command": "sweep_apply", "args": {}}))
                .await
                .unwrap_err()
        ),
        "NotFound"
    );

    // analysis_get
    let en = c
        .ok(
            "analysis_get",
            json!({"source": source, "kind": "energy", "from_s": 100, "to_s": 102}),
        )
        .await;
    assert_eq!(en["kind_seen"], "energy_20ms");
    let lo = c
        .ok(
            "analysis_get",
            json!({"source": source, "kind": "loudness", "from_s": 100, "to_s": 102}),
        )
        .await;
    assert_eq!(lo["integrated_lufs"], json!(-16.0));
    assert_eq!(
        code(
            &c.call("analysis_get", json!({"source": source, "kind": "prosody"}))
                .await
                .unwrap_err()
        ),
        "Unsupported"
    );
    assert_eq!(
        code(
            &c.call("analysis_get", json!({"source": source, "kind": "ocr"}))
                .await
                .unwrap_err()
        ),
        // an outside agent has read_media (decision 0048): past the scope check
        "Unsupported"
    );

    // sweeps: preview first, then one transaction
    let pv = c
        .ok(
            "command_run",
            json!({"command": "sweep_fillers", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(pv["dry_run"], true);
    assert_eq!(pv["cuts"].as_array().unwrap().len(), 1);
    assert_eq!(pv["cuts"][0]["from_s"], json!(100.6));
    let pp = c.ok("command_run", json!({"command": "sweep_pauses", "args": {"cut": fx.cut, "min_s": 0.2, "keep_s": 0.1}})).await;
    assert_eq!(pp["cuts"].as_array().unwrap().len(), 1);
    assert_eq!(
        code(
            &c.call(
                "command_run",
                json!({"command": "sweep_pauses", "args": {"cut": fx.cut, "min_s": 9}})
            )
            .await
            .unwrap_err()
        ),
        "Invalid"
    );
    // applying needs a base (agents), and commits once
    assert_eq!(
        code(
            &c.call(
                "command_run",
                json!({"command": "sweep_fillers", "args": {"cut": fx.cut, "dry_run": false}})
            )
            .await
            .unwrap_err()
        ),
        "Invalid"
    );
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    let ap = c.ok("command_run", json!({"command": "sweep_fillers", "base": {"as_of": as_of}, "args": {"cut": fx.cut, "dry_run": false}})).await;
    assert!(ap["committed"].as_u64().is_some(), "{ap}");
    assert_eq!(ap["applied"]["removed_spans"], 1);
    let parts = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let p: Vec<(i64, i64)> = parts["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["src_in"].as_i64().unwrap(),
                p["src_out"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(p[0], (100 * SEC, f(100.6).0), "the part before the filler");
    assert_eq!(p[1], (f(100.9).0, 102 * SEC), "the part after it");
    assert_eq!(p.len(), 4);

    // the ASR switch moves the gain segment's words onto the new revision
    let proj = w.holder.resolve(None).unwrap();
    let words2: Vec<Value> = WORDS
        .iter()
        .map(|(t, a, b)| json!({"text": t, "start": f(*a + 0.02).0, "end": f(*b + 0.02).0}))
        .collect();
    proj.handle
        .put_analysis(&source, "words", "fake-asr@2", serde_json::to_vec(&json!({"schema": "gmclip.analysis.words/0", "revision": "fake-asr@2", "untrusted": true, "words": words2})).unwrap())
        .unwrap();
    let as_of = c.ok("project_overview", json!({})).await["as_of"]
        .as_u64()
        .unwrap();
    let sw = c.ok("command_run", json!({"command": "asr_activate", "base": {"as_of": as_of}, "args": {"owner": source, "revision": "fake-asr@2"}})).await;
    assert_eq!(sw["output"]["from"], REV);
    assert!(sw["output"]["remapped"].as_u64().unwrap() >= 2, "{sw}");
    let t2 = c.ok("transcript_read", json!({"source": source})).await;
    assert_eq!(t2["asr_revision"], "fake-asr@2");
    // and it is one undoable step
    assert!(sw["committed"].as_u64().is_some());
    let d = c
        .ok("command_describe", json!({"name": "asr_activate"}))
        .await;
    assert_eq!(d["undo"], "undoable");
}

/// A2.06: `sweep_preview` → `sweep_apply{preview_id}` applies exactly what was listed,
/// `Conflict` when the cut moved, `StaleAnalysis` when the analysis changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeps_apply_exactly_what_was_previewed() {
    let w = World::start_full(None, Some(Arc::new(FakeAnalysis))).await;
    let fx = w.fixture("sweep-preview", 3, "Hook");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let cut = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let source = cut["parts"][0]["media"]["source"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_transcript(&c, &source).await;
    let as_of = |v: &Value| v["as_of"].as_u64().unwrap();
    let parts = |v: &Value| -> Vec<(i64, i64)> {
        v["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["src_in"].as_i64().unwrap(),
                    p["src_out"].as_i64().unwrap(),
                )
            })
            .collect()
    };

    // preview, then apply exactly that
    let pv = c
        .ok(
            "command_run",
            json!({"command": "sweep_preview", "args": {"cut": fx.cut, "kind": "fillers"}}),
        )
        .await;
    let id = pv["preview_id"].as_str().unwrap().to_owned();
    assert_eq!(pv["cuts"].as_array().unwrap().len(), 1);
    assert_eq!(pv["cuts"][0]["from_s"], json!(100.6));
    assert_eq!(pv["cuts"][0]["to_s"], json!(100.9));
    let before = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    // an index the preview did not list
    let e = c
        .call("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&before)}, "args": {"cut": fx.cut, "preview_id": id, "only": [3]}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Invalid");
    let ap = c
        .ok("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&before)}, "args": {"cut": fx.cut, "preview_id": id}}))
        .await;
    assert!(ap["committed"].as_u64().is_some(), "{ap}");
    assert_eq!(ap["output"]["removed_spans"], 1);
    assert_eq!(ap["output"]["kind"], "fillers");
    let after = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let (b, a) = (parts(&before), parts(&after));
    assert_eq!(a.len(), b.len() + 1);
    // the cut lost exactly the previewed span, on the 30 fps grid: [100.6, 100.9) s
    assert_eq!(a[0], (100 * SEC, f(100.6).0));
    assert_eq!(a[1], (f(100.9).0, 102 * SEC));
    assert_eq!(&a[2..], &b[1..]);

    // the cut moves under a preview: Conflict
    let pv = c
        .ok("command_run", json!({"command": "sweep_preview", "args": {"cut": fx.cut, "kind": "pauses", "min_s": 0.2, "keep_s": 0.1}}))
        .await;
    let id2 = pv["preview_id"].as_str().unwrap().to_owned();
    let tr = c
        .ok("command_run", json!({"command": "part_trim", "base": {"as_of": as_of(&pv)}, "args": {"part": fx.parts[2], "src_out": 107 * SEC}}))
        .await;
    let e = c
        .call("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&tr)}, "args": {"preview_id": id2}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "Conflict", "{e}");

    // the analysis changes under a preview: StaleAnalysis
    let pv = c
        .ok("command_run", json!({"command": "sweep_preview", "args": {"cut": fx.cut, "kind": "pauses", "min_s": 0.2, "keep_s": 0.1}}))
        .await;
    let id3 = pv["preview_id"].as_str().unwrap().to_owned();
    QUIET_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let e = c
        .call("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&pv)}, "args": {"preview_id": id3}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "StaleAnalysis", "{e}");
    // previewed again on the new analysis, it applies
    let pv = c
        .ok("command_run", json!({"command": "sweep_pauses", "args": {"cut": fx.cut, "min_s": 0.2, "keep_s": 0.1}}))
        .await;
    let id4 = pv["preview_id"].as_str().unwrap().to_owned();
    let ap = c
        .ok("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&pv)}, "args": {"preview_id": id4}}))
        .await;
    assert_eq!(ap["output"]["kind"], "pauses");
    // an unknown preview, and spans without one, are not the agent's
    let e = c
        .call("command_run", json!({"command": "sweep_apply", "base": {"as_of": as_of(&ap)}, "args": {"preview_id": "sp999999"}}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "NotFound");
}

/// Decision 0040: a soundless clip in the cut (a graphic) does not fail the pause
/// sweep; its parts are counted as without audio and the rest is swept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_soundless_clip_does_not_fail_the_pause_sweep() {
    let w = World::start_full(None, Some(Arc::new(FakeAnalysis))).await;
    let fx = w.fixture("sweep-soundless", 3, "Hook");
    let c = w.client("claude-code", None).await;
    c.ok(
        "command_run",
        json!({"command": "project_open", "args": {"path": fx.home.to_str().unwrap()}}),
    )
    .await;
    let cut = c
        .ok("cut_get", json!({"cut": fx.cut, "projection": "parts"}))
        .await;
    let source = cut["parts"][0]["media"]["source"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_transcript(&c, &source).await;
    let gfx = w
        .holder
        .resolve(None)
        .unwrap()
        .handle
        .call(gmclip_core::CallRequest {
            command: "source_add".into(),
            args: json!({"path": "/m/graphic.mp4", "fingerprint": "fp2",
                         "probe": {"duration": 10 * SEC, "tracks": [], "audio_tracks": []}}),
            base: None,
            request_id: None,
            caller: gmclip_core::Caller::human(),
        })
        .unwrap();
    let gfx = gfx.output["id"].as_str().unwrap().to_owned();
    let as_of = c.ok("project_overview", json!({})).await["as_of"].clone();
    c.ok(
        "cut_edit",
        json!({"cut": fx.cut, "base": {"as_of": as_of}, "ops": [
            {"op": "part_add", "media": {"source": gfx}, "src_in": 0, "src_out": 4 * SEC}]}),
    )
    .await;
    let pv = c
        .ok(
            "command_run",
            json!({"command": "sweep_pauses", "args": {"cut": fx.cut}}),
        )
        .await;
    assert_eq!(pv["parts_without_audio"], 1, "{pv}");
    assert!(
        !pv["cuts"].as_array().unwrap().is_empty(),
        "the talking parts are still swept: {pv}"
    );
}
