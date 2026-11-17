//! Background analysis of an opened video (AJ 2026-10-14: "select any video … see the
//! transcript"): ASR first, in chunks, so words reach the transcript pane while the rest
//! is still being heard; then waveform, energy, quiet grid and VAD (what sweeps and
//! measure read).
//!
//! The analysis host is the CLI's (`crates/gmclip-cli/src/analysis.rs`: lane D's DSP and
//! Parakeet/Silero through gmclip-ml), compiled in here by path so the app and the
//! headless holder hear the same thing. Blobs are stored on the project exactly as the
//! holder's `source_analyze` stores them (`words`/`lines` per source id, then
//! `asr_activate` as the system when the source has no active transcript).
//!
//! Chunking is `gmclip_core::analysis::chunked`, the holder's too: a job the headless
//! holder hands over (R-ARCH-7) resumes here at its next window, under its job id.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gmclip_core::analysis::chunked::{self, AsrPartial};
use gmclip_core::analysis::{Transcript, Word};
use gmclip_core::host::{AnalysisHost, AudioSource};
use gmclip_core::{CallRequest, Caller};
use gmclip_model::entities::{Project, Source};
use serde_json::{Value, json};

use crate::core::{AppCore, OpenProject};
use crate::planner::FLICKS;

#[path = "../../../crates/gmclip-cli/src/analysis.rs"]
#[allow(dead_code, clippy::all)]
pub(crate) mod cli_analysis;

// The CLI's media host names these as `crate::analysis::…` (compiled in by `agent`).
pub(crate) use cli_analysis::{CliAnalysis, PlanHearer, track_16k, verify_joins};

/// Whole voice-track reads the analysis made itself, in this process (D.02: none after a
/// whole transcription, whose decode it shares).
#[doc(hidden)]
pub fn whole_track_reads(path: &std::path::Path) -> u64 {
    cli_analysis::whole_track_reads(path)
}

/// Where one source's analysis is.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// `transcribing`, `analysing`, `done`, `failed`.
    pub phase: String,
    /// Seconds of the source heard so far, and its length.
    pub done_s: f64,
    pub total_s: f64,
    /// Words heard so far (while transcribing; the stored transcript after).
    pub words: Arc<Vec<Word>>,
    /// The revision the words will be stored under (known after the first window).
    pub revision: String,
    pub error: Option<String>,
    pub asr_error: Option<String>,
    /// The holder's job id, for a job taken over from `gmclip serve` (R-ARCH-7).
    pub job: Option<String>,
    /// Where it resumed (seconds), for a taken-over job.
    pub resumed_at_s: Option<f64>,
    /// Paused for playback now (a frame dropped in the last 2 s, D.01), and how many
    /// times it has paused.
    pub paused: bool,
    pub pauses: u32,
    /// When this session started hearing it, and how much was heard then (a resumed job
    /// starts part way): the speed the ETA is measured from.
    pub heard_from: Option<(std::time::Instant, f64)>,
    /// Names the correction pass spelt from the term list when the words were stored.
    pub corrected: usize,
}

/// The last transcription speed measured on this Mac (seconds of audio per second), for
/// an ETA before a new one has heard enough to measure its own.
static SPEED: Mutex<Option<f64>> = Mutex::new(None);

/// ASR speed (× real time) to quote before a run has measured its own: this Mac's last
/// measured run, else Parakeet's measured speed on the M1 Pro under load (STATUS B04: 8.8×).
pub fn asr_speed() -> (f64, &'static str) {
    match SPEED.lock().ok().and_then(|g| *g) {
        Some(s) => (s, "measured"),
        None => (8.8, "typical"),
    }
}

/// Seconds left to hear `total_s` with `done_s` heard, at the speed this run has shown
/// (once it has heard ≥ 10 s for ≥ 2 s), else at [`asr_speed`].
pub fn eta_s(p: &Progress) -> Option<f64> {
    let left = (p.total_s - p.done_s).max(0.0);
    match p.phase.as_str() {
        "transcribing" => {
            let measured = p.heard_from.and_then(|(at, from)| {
                let el = at.elapsed().as_secs_f64();
                let heard = p.done_s - from;
                (heard >= 10.0 && el >= 2.0).then(|| heard / el)
            });
            Some(left / measured.unwrap_or_else(|| asr_speed().0).max(0.1))
        }
        "queued" | "none" => Some(p.total_s / asr_speed().0.max(0.1)),
        _ => None,
    }
}

/// The app's analysis runner: one job per (project, source), progress kept in memory.
pub struct Analyzer {
    host: Arc<dyn AnalysisHost>,
    /// The same host as the CLI's concrete type (the app's holder shares it, so models
    /// load once).
    cli: Option<Arc<CliAnalysis>>,
    state: Mutex<BTreeMap<(String, String), Progress>>,
    cancels: Mutex<BTreeMap<(String, String), Arc<AtomicBool>>>,
    /// Analysis threads alive (shutdown waits for them: a thread inside ONNX Runtime when
    /// the process exits trips its static teardown).
    running: Arc<(Mutex<usize>, std::sync::Condvar)>,
    /// One transcription at a time (a project of several videos hears them in turn,
    /// each with its own progress; the others say `queued`).
    asr_gate: Arc<Mutex<()>>,
}

impl std::fmt::Debug for Analyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyzer")
            .field("host", &self.host.name())
            .finish_non_exhaustive()
    }
}

impl Default for Analyzer {
    fn default() -> Self {
        let cli = Arc::new(CliAnalysis::new());
        let host = Arc::clone(&cli) as Arc<dyn AnalysisHost>;
        // dev-bridge builds: what was heard is kept across launches when the e2e runner
        // names a folder for it (`analysiscache`)
        #[cfg(feature = "dev-bridge")]
        let host = match crate::analysiscache::CachedAnalysis::from_env(Arc::clone(&host)) {
            Some(c) => Arc::new(c) as Arc<dyn AnalysisHost>,
            None => host,
        };
        let mut a = Analyzer::with_host(host);
        a.cli = Some(cli);
        a
    }
}

fn secs(f: i64) -> f64 {
    f as f64 / FLICKS as f64
}

fn has_words(p: &OpenProject, source: &str) -> bool {
    p.handle
        .analysis_keys()
        .iter()
        .any(|(o, k, _)| o == source && k == "words")
}

fn analysed(p: &OpenProject, source: &str) -> bool {
    p.handle
        .analysis_keys()
        .iter()
        .any(|(o, k, _)| o == source && k == "energy_10ms")
}

impl Analyzer {
    pub fn with_host(host: Arc<dyn AnalysisHost>) -> Analyzer {
        Analyzer {
            host,
            cli: None,
            state: Mutex::new(BTreeMap::new()),
            cancels: Mutex::new(BTreeMap::new()),
            running: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
            asr_gate: Arc::new(Mutex::new(())),
        }
    }

    /// The concrete host, when it is the CLI's (the app's holder shares it).
    pub fn cli(&self) -> Option<Arc<CliAnalysis>> {
        self.cli.clone()
    }

    pub fn host(&self) -> Arc<dyn AnalysisHost> {
        Arc::clone(&self.host)
    }

    /// The progress of one source, if a job ran this session.
    pub fn progress(&self, project: &str, source: &str) -> Option<Progress> {
        self.state
            .lock()
            .ok()?
            .get(&(project.to_owned(), source.to_owned()))
            .cloned()
    }

    fn set(&self, core: &AppCore, project: &str, source: &str, f: impl FnOnce(&mut Progress)) {
        let snap = {
            let Ok(mut g) = self.state.lock() else { return };
            let e = g
                .entry((project.to_owned(), source.to_owned()))
                .or_default();
            f(e);
            e.clone()
        };
        core.bus.project(
            project,
            crate::events::ProjectEvent::Analysis(crate::events::AnalysisProgress {
                source: source.to_owned(),
                phase: snap.phase,
                done_s: snap.done_s,
                total_s: snap.total_s,
                words: snap.words.len() as u64,
                error: snap.error,
                asr_error: snap.asr_error,
                job: snap.job,
                resumed_at_s: snap.resumed_at_s,
                paused: snap.paused,
                pauses: snap.pauses,
            }),
        );
    }

    /// Cancels every job and waits (up to `wait`) for their threads to end. Returns
    /// whether they all ended. Called before the app exits.
    pub fn shutdown(&self, wait: std::time::Duration) -> bool {
        self.cancel_all();
        let (m, cv) = &*self.running;
        let Ok(g) = m.lock() else { return false };
        cv.wait_timeout_while(g, wait, |n| *n > 0)
            .map(|(g, _)| *g == 0)
            .unwrap_or(false)
    }

    /// Asks every job to stop (they stop between steps; [`Analyzer::shutdown`] waits).
    pub fn cancel_all(&self) {
        if let Ok(g) = self.cancels.lock() {
            for c in g.values() {
                c.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Stops a project's jobs (on close).
    pub fn cancel_project(&self, project: &str) {
        if let Ok(g) = self.cancels.lock() {
            for ((p, _), c) in g.iter() {
                if p == project {
                    c.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// After the speech model is installed: transcribe the sources that went without.
    pub fn retry_after_models(&self, core: &Arc<AppCore>) {
        for p in core.projects() {
            let pid = p.info.id.clone();
            let doc = p.handle.snapshot().doc.clone();
            for (sid, s) in owners(&doc) {
                let s = &s;
                if has_words(&p, &sid) {
                    continue;
                }
                if let Ok(mut g) = self.state.lock() {
                    g.remove(&(pid.clone(), sid.clone()));
                }
                self.start(core, &pid, &sid, s, false);
            }
        }
    }

    /// Starts analysis for every source of a project that lacks a transcript or energy
    /// (and has no job running this session).
    pub fn ensure(&self, core: &Arc<AppCore>, project: &str) {
        let Ok(p) = core.project(project) else { return };
        let doc = p.handle.snapshot().doc.clone();
        let owners = owners(&doc);
        for (sid, s) in owners {
            let s = &s;
            if has_words(&p, &sid) && analysed(&p, &sid) {
                continue;
            }
            if self
                .progress(project, &sid)
                .is_some_and(|pr| pr.phase != "failed")
            {
                continue;
            }
            self.start(core, project, &sid, s, false);
        }
    }

    /// Runs one source's analysis on its own thread.
    pub fn start(
        &self,
        core: &Arc<AppCore>,
        project: &str,
        sid: &str,
        s: &Source,
        force_asr: bool,
    ) {
        self.start_from(core, project, sid, s, force_asr, None);
    }

    /// Resumes a transcription the headless holder handed over (R-ARCH-7): its job id,
    /// the windows it heard; the rest is heard here.
    pub fn resume(
        &self,
        core: &Arc<AppCore>,
        project: &str,
        sid: &str,
        job: &str,
        from: AsrPartial,
    ) {
        let Ok(p) = core.project(project) else { return };
        let doc = p.handle.snapshot().doc.clone();
        let Some((_, s)) = owners(&doc).into_iter().find(|(o, _)| o == sid) else {
            return;
        };
        self.start_from(core, project, sid, &s, true, Some((job.to_owned(), from)));
    }

    fn start_from(
        &self,
        core: &Arc<AppCore>,
        project: &str,
        sid: &str,
        s: &Source,
        force_asr: bool,
        resumed: Option<(String, AsrPartial)>,
    ) {
        let audio = AudioSource {
            path: std::path::PathBuf::from(&s.path),
            track: s.audio_roles.voice,
            fingerprint: s.fingerprint.clone(),
        };
        let total = s.probe.duration.0;
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut g) = self.cancels.lock() {
            g.insert((project.to_owned(), sid.to_owned()), Arc::clone(&cancel));
        }
        let (job, from) = match resumed {
            Some((j, f)) => (Some(j), f),
            None => (None, AsrPartial::default()),
        };
        let resumed_at_s = job.as_ref().map(|_| secs(from.done));
        let (done_s, words, revision) = (
            secs(from.done),
            Arc::new(from.words.clone()),
            from.revision.clone(),
        );
        self.set(core, project, sid, |p| {
            *p = Progress {
                phase: "transcribing".into(),
                total_s: secs(total),
                done_s,
                words,
                revision,
                job,
                resumed_at_s,
                ..Progress::default()
            };
        });
        let weak = Arc::downgrade(core);
        let (project, sid) = (project.to_owned(), sid.to_owned());
        /// Counts this thread out when it ends, however it ends.
        struct Alive(Arc<(Mutex<usize>, std::sync::Condvar)>);
        impl Drop for Alive {
            fn drop(&mut self) {
                let (m, cv) = &*self.0;
                if let Ok(mut n) = m.lock() {
                    *n = n.saturating_sub(1);
                }
                cv.notify_all();
            }
        }
        if let Ok(mut n) = self.running.0.lock() {
            *n += 1;
        }
        let alive = Alive(Arc::clone(&self.running));
        let _ = std::thread::Builder::new()
            .name("gmclip-analysis".into())
            .spawn(move || {
                let _alive = alive;
                let Some(core) = weak.upgrade() else { return };
                let r = run(
                    &core, &project, &sid, &audio, total, force_asr, from, &cancel,
                );
                let an = &core.analysis;
                match r {
                    Ok(()) => an.set(&core, &project, &sid, |p| {
                        p.phase = "done".into();
                        p.done_s = p.total_s;
                    }),
                    Err(e) => an.set(&core, &project, &sid, |p| {
                        p.phase = "failed".into();
                        p.error = Some(e);
                    }),
                }
            });
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    core: &Arc<AppCore>,
    project: &str,
    sid: &str,
    audio: &AudioSource,
    total: i64,
    force_asr: bool,
    from: AsrPartial,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let an = &core.analysis;
    let host = an.host();
    let p = core.project(project).map_err(|e| e.message)?;
    if force_asr || !has_words(&p, sid) {
        // one source at a time: wait for the gate (cancellable), saying so
        let gate = Arc::clone(&an.asr_gate);
        let mut said = false;
        let _turn = loop {
            match gate.try_lock() {
                Ok(g) => break g,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".into());
                    }
                    if !said {
                        said = true;
                        an.set(core, project, sid, |pr| pr.phase = "queued".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        };
        an.set(core, project, sid, |pr| {
            pr.phase = "transcribing".into();
            pr.heard_from = Some((std::time::Instant::now(), pr.done_s));
        });
        // the analysis after the transcription reads the voice it decodes (D.02)
        if !analysed(&p, sid) {
            host.keep_voice(audio, true);
        }
        // ASR below the UI and the render thread (D.01) — and only ASR: a thread started
        // from here (the meaning index) would inherit the class, and quitting waits ≤ 10 s
        // for work whose cancel is coarse
        crate::qos::utility_thread();
        let heard = transcribe(
            core,
            project,
            sid,
            host.as_ref(),
            audio,
            total,
            from,
            force_asr,
            cancel,
        );
        crate::qos::default_thread();
        match heard {
            Ok(mut tr) => {
                // the correction pass: names in the term list spelt right before storing
                // (words only; the model is never changed)
                let corrected = gmclip_core::terms::correct_transcript(
                    &mut tr,
                    &crate::termlist::term_list(core, &p.handle.snapshot().doc),
                );
                an.set(core, project, sid, |pr| pr.corrected = corrected);
                let h = &p.handle;
                h.put_analysis(sid, "words", &tr.revision, tr.words_body())
                    .map_err(|_| "saving the transcript".to_owned())?;
                h.put_analysis(sid, "lines", &tr.revision, tr.lines_body())
                    .map_err(|_| "saving the transcript".to_owned())?;
                let active = h
                    .snapshot()
                    .doc
                    .iter::<Project>()
                    .next()
                    .is_some_and(|pr| pr.active_asr.contains_key(sid));
                if !active || force_asr {
                    let _ = h.call(CallRequest {
                        command: "asr_activate".into(),
                        args: json!({"owner": sid, "revision": tr.revision}),
                        base: None,
                        request_id: None,
                        caller: Caller::system("analysis"),
                    });
                }
                let words = Arc::new(tr.words);
                an.set(core, project, sid, |pr| {
                    pr.words = words;
                    // this Mac's speed, for the next import's ETA (a whole minute heard)
                    if let Some((at, from)) = pr.heard_from
                        && pr.total_s - from >= 60.0
                        && let Ok(mut g) = SPEED.lock()
                    {
                        *g = Some((pr.total_s - from) / at.elapsed().as_secs_f64().max(0.1));
                    }
                });
                // library search and its meaning index pick the transcript up (lane L)
                crate::libmeaning::kick(core);
            }
            Err(e) if e == "cancelled" => {
                host.keep_voice(audio, false);
                return Err(e);
            }
            // No speech model on this machine (or ASR failed): energy and silences still
            // run, and the pane says why there are no words.
            Err(e) => an.set(core, project, sid, |pr| pr.asr_error = Some(e)),
        }
    }
    if !analysed(&p, sid) {
        an.set(core, project, sid, |pr| pr.phase = "analysing".into());
        // each blob is stored as it is made: the timeline's columns (peaks, energy, the
        // quiet grid) are in before the VAD runs, and the UI is told so
        let mut saved = Ok(());
        let r = host.analyze_each(audio, cancel, &mut |b| {
            let kind = b.kind.clone();
            if let Err(e) = p.handle.put_analysis(sid, &b.kind, &b.revision, b.body) {
                saved = Err(e);
                return Err(gmclip_core::host::HostError::Failed("saving".into()));
            }
            if kind == "silences_40ms" {
                an.set(core, project, sid, |_| {});
            }
            Ok(())
        });
        if saved.is_err() {
            return Err("saving the analysis".to_owned());
        }
        r.map_err(|e| format!("{e:?}").chars().take(160).collect::<String>())?;
    } else {
        host.keep_voice(audio, false);
    }
    core.bus.project(
        project,
        crate::events::ProjectEvent::Transcript {
            source: sid.to_string(),
        },
    );
    Ok(())
}

/// Hears the source window by window from `from` (a handed-over job's windows, R-ARCH-7),
/// else from the windows a run before this one stored in the project (A2.02: a quit or a
/// crash resumes at the window after the last finished one), publishing words as they
/// arrive and storing each finished window.
#[allow(clippy::too_many_arguments)]
fn transcribe(
    core: &Arc<AppCore>,
    project: &str,
    sid: &str,
    host: &dyn AnalysisHost,
    audio: &AudioSource,
    total: i64,
    from: AsrPartial,
    force: bool,
    cancel: &AtomicBool,
) -> Result<Transcript, String> {
    let an = &core.analysis;
    let mut heard = |p: &AsrPartial| {
        let snapshot = Arc::new(p.words.clone());
        let rev = p.revision.clone();
        let done = p.done;
        an.set(core, project, sid, |pr| {
            pr.done_s = secs(done);
            pr.words = snapshot;
            pr.revision = rev;
        });
        // between windows: wait while playback is dropping frames (D.01)
        core.qos.wait(cancel, |paused| {
            an.set(core, project, sid, |pr| {
                pr.paused = paused;
                if paused {
                    pr.pauses += 1;
                }
            });
        });
    };
    let r = if from.done > 0 {
        chunked::run(host, audio, total, from, cancel, &mut heard)
    } else {
        let h = core.project(project).map_err(|e| e.message)?.handle.clone();
        let keys = h.analysis_keys();
        let get = |r: &str| h.analysis(sid, chunked::WINDOW_KIND, r);
        // say where it picks up before the first new window arrives
        if let Some((_, p)) = chunked::stored(&keys, sid, &chunked::window_prefix(audio), get)
            && p.done > 0
            && (p.done < total || !force)
        {
            let (done, words) = (p.done, Arc::new(p.words));
            an.set(core, project, sid, |pr| {
                pr.resumed_at_s = Some(secs(done));
                pr.done_s = secs(done);
                pr.words = words;
            });
        }
        chunked::run_stored(
            host,
            audio,
            total,
            &keys,
            sid,
            get,
            |r, body| {
                let _ = h.put_analysis(sid, chunked::WINDOW_KIND, r, body);
            },
            force,
            cancel,
            &mut heard,
        )
        .map(|(p, _)| p)
    };
    match r {
        Ok(p) => Ok(p.transcript()),
        Err(gmclip_core::host::HostError::Unsupported(r)) => Err(r.to_owned()),
        Err(gmclip_core::host::HostError::Failed(r)) if r == "cancelled" => Err("cancelled".into()),
        Err(other) => Err(format!("{other:?}").chars().take(160).collect()),
    }
}

/// A summary of a project's analysis for the UI (per source).
/// What is heard and who owns the words: each sync group through its audio member (its
/// member sources are not heard alone), then every source in no group.
pub fn owners(doc: &gmclip_model::Doc) -> Vec<(String, Source)> {
    // A sync group is heard once, through its audio member, and its words live on the
    // group (the owner its parts name); its member sources are not heard alone.
    let mut owners: Vec<(String, Source)> = Vec::new();
    let mut grouped = std::collections::BTreeSet::new();
    for g in doc.iter::<gmclip_model::entities::SyncGroup>() {
        let members: Vec<&gmclip_model::entities::SyncMember> = doc
            .children::<gmclip_model::entities::SyncMember>(g.id.entity())
            .filter_map(|m| doc.get(m))
            .collect();
        for m in &members {
            grouped.insert(m.source);
        }
        let aud = g.master_audio.and_then(|m| doc.get(m)).or_else(|| {
            members.iter().copied().find(|m| {
                matches!(
                    m.role,
                    gmclip_model::entities::MemberRole::Audio
                        | gmclip_model::entities::MemberRole::Both
                )
            })
        });
        if let Some(s) = aud.and_then(|m| doc.get(m.source)) {
            owners.push((g.id.0.to_string(), s.clone()));
        }
    }
    for s in doc.iter::<Source>() {
        if !grouped.contains(&s.id) {
            owners.push((s.id.0.to_string(), s.clone()));
        }
    }
    owners
}

pub fn status(core: &AppCore, project: &str) -> Value {
    let Ok(p) = core.project(project) else {
        return json!({"sources": []});
    };
    let doc = p.handle.snapshot().doc.clone();
    let sources: Vec<Value> = owners(&doc)
        .into_iter()
        .map(|(sid, s)| {
            let pr = core.analysis.progress(project, &sid);
            let stored = has_words(&p, &sid);
            json!({
                "source": sid,
                "name": std::path::Path::new(&s.path).file_stem().map(|n| n.to_string_lossy().into_owned()),
                "phase": pr.as_ref().map_or(if stored { "done" } else { "none" }, |p| p.phase.as_str()),
                "done_s": pr.as_ref().map_or(0.0, |p| p.done_s),
                "total_s": secs(s.probe.duration.0),
                "transcript": stored,
                "error": pr.as_ref().and_then(|p| p.error.clone()),
                "asr_error": pr.as_ref().and_then(|p| p.asr_error.clone()),
                "corrected": pr.as_ref().map_or(0, |p| p.corrected),
                "paused": pr.as_ref().is_some_and(|p| p.paused),
                "eta_s": pr.as_ref().and_then(eta_s).map(f64::round),
            })
        })
        .collect();
    json!({"sources": sources})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn at(phase: &str, done_s: f64, total_s: f64, ago_s: u64, from: f64) -> Progress {
        Progress {
            phase: phase.into(),
            done_s,
            total_s,
            heard_from: Instant::now()
                .checked_sub(Duration::from_secs(ago_s))
                .map(|t| (t, from)),
            ..Progress::default()
        }
    }

    #[test]
    fn the_eta_is_this_runs_speed_once_measured() {
        // 40 s heard in 4 s: 10× real time, 360 s left → 36 s
        let e = eta_s(&at("transcribing", 40.0, 400.0, 4, 0.0)).unwrap();
        assert!((e - 36.0).abs() < 1.0, "{e}");
        // a resumed run measures from where it resumed: 40 s more in 4 s
        let e = eta_s(&at("transcribing", 140.0, 500.0, 4, 100.0)).unwrap();
        assert!((e - 36.0).abs() < 1.0, "{e}");
        // too little heard to measure: this Mac's (or the typical) speed
        let (speed, _) = asr_speed();
        let e = eta_s(&at("transcribing", 5.0, 905.0, 4, 0.0)).unwrap();
        assert!((e - 900.0 / speed).abs() < 1.0, "{e}");
        // waiting its turn: the whole length at that speed; analysing or done: none
        let e = eta_s(&at("queued", 0.0, 880.0, 0, 0.0)).unwrap();
        assert!((e - 880.0 / speed).abs() < 1.0, "{e}");
        assert_eq!(eta_s(&at("analysing", 400.0, 400.0, 4, 0.0)), None);
        assert_eq!(eta_s(&at("done", 400.0, 400.0, 4, 0.0)), None);
    }
}
