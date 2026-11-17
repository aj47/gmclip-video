//! The agent surface over analysis (SPEC §9, §11.2): background analysis jobs, and the
//! `transcript_read`, `analysis_get`, `measure`, `listen` tools and the sweep commands.
//!
//! The holder runs the [`AnalysisHost`] (I/O, DSP, ML) and stores what it makes through
//! the project actor (`put_analysis`, immutable blobs); the actor folds transcripts into
//! the analysis view every handler and read sees. Writes that follow from analysis — the
//! first `active_asr`, a measurement — go through ordinary commands, so they are undoable
//! and appear in the history.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gmclip_core::analysis::Word;
use gmclip_core::analysis::chunked::{self, AsrPartial};
use gmclip_core::command::{Caller, Scope};
use gmclip_core::commands::analysis::SweepKind;
use gmclip_core::host::{AnalysisHost, AudioSource, EdgeRule, HeardWord};
use gmclip_core::planner::audio_source;
use gmclip_core::{CallRequest, CmdError, ProjectHandle};
use gmclip_model::anchor::{Anchor, CutSide, FrameRef, WordRef};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id, Ulid};
use gmclip_proto::Base;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::canon::{self, Kind};
use crate::holder::{Conn, Holder, OpenProject};
use crate::media::Job;

const SEC: i64 = 705_600_000;

fn secs(t: i64) -> f64 {
    (t as f64 / SEC as f64 * 1000.0).round() / 1000.0
}

fn flicks(s: f64, path: &str) -> Result<Flicks, CmdError> {
    if !s.is_finite() || !(0.0..=86_400.0 * 2.0).contains(&s) {
        return Err(CmdError::invalid(path, "seconds within the source"));
    }
    Ok(Flicks((s * SEC as f64).round() as i64))
}

fn host_err(e: gmclip_core::host::HostError) -> CmdError {
    CmdError::from(e)
}

fn system_call(h: &ProjectHandle, command: &str, args: Value) -> Result<Value, CmdError> {
    h.call(CallRequest {
        command: command.into(),
        args,
        base: None,
        request_id: None,
        caller: Caller::system("analysis"),
    })
    .map(|ok| ok.output)
}

/// A media owner: a source, or a sync group, by its id string.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Owner {
    Source(Id<Source>),
    Group(Id<SyncGroup>),
}

impl Owner {
    pub fn parse(doc: &Doc, s: &str) -> Result<Owner, CmdError> {
        let raw = s.split_once(':').map_or(s, |(_, r)| r);
        let u = Ulid::parse(raw)
            .map_err(|_| CmdError::invalid("/source", "a source or sync group id"))?;
        if doc.get(Id::<Source>::new(u)).is_some() {
            return Ok(Owner::Source(Id::new(u)));
        }
        if doc.get(Id::<SyncGroup>::new(u)).is_some() {
            return Ok(Owner::Group(Id::new(u)));
        }
        Err(CmdError::NotFound)
    }

    pub fn key(self) -> String {
        match self {
            Owner::Source(s) => s.0.to_string(),
            Owner::Group(g) => g.0.to_string(),
        }
    }

    pub fn media(self) -> MediaRef {
        match self {
            Owner::Source(s) => MediaRef::Source(s),
            Owner::Group(g) => MediaRef::Group(g),
        }
    }
}

/// A launched run reads only media its own cut plays.
fn check_binding(doc: &Doc, binding: Option<Id<Cut>>, owner: Owner) -> Result<(), CmdError> {
    let Some(cut) = binding else { return Ok(()) };
    let plays = doc
        .children::<Part>(cut.entity())
        .filter_map(|p| doc.get(p))
        .any(|p| p.media == owner.media());
    if plays {
        Ok(())
    } else {
        Err(CmdError::Unauthorized {
            scope: "cut_binding".into(),
        })
    }
}

fn audio_of(doc: &Doc, s: Id<Source>) -> Result<AudioSource, CmdError> {
    let src = doc.get(s).ok_or(CmdError::NotFound)?;
    Ok(AudioSource {
        path: std::path::PathBuf::from(&src.path),
        track: src.audio_roles.voice,
        fingerprint: src.fingerprint.clone(),
    })
}

/// The audio source and its offset for an owner at media time `t`.
fn audio_at(doc: &Doc, owner: Owner, t: i64) -> Result<(Id<Source>, i64, AudioSource), CmdError> {
    let (s, off) = audio_source(doc, owner.media(), t)
        .ok_or_else(|| CmdError::invalid("/source", "no audio for this media at that time"))?;
    Ok((s, off, audio_of(doc, s)?))
}

// -------------------------------------------------------------------------------------
// Background analysis
// -------------------------------------------------------------------------------------

/// Has a source been analysed (energy stored)?
fn analysed(keys: &[(String, String, String)], owner: &str) -> bool {
    keys.iter()
        .any(|(o, k, _)| o == owner && k == "energy_10ms")
}

/// Does a source (or a group it plays audio in) have a transcript?
fn has_words(doc: &Doc, keys: &[(String, String, String)], s: Id<Source>) -> bool {
    let groups: Vec<String> = doc
        .iter::<SyncMember>()
        .filter(|m| m.source == s && m.role != MemberRole::Picture)
        .map(|m| m.group.0.to_string())
        .collect();
    keys.iter()
        .any(|(o, k, _)| k == "words" && (*o == s.0.to_string() || groups.contains(o)))
}

/// Starts (or joins) the analysis job of one source: waveform, energy, silences, VAD,
/// then ASR of the voice track when the source has no transcript (or `force_asr`); the
/// first transcript of a source becomes its active one.
pub fn analyze(
    holder: &Holder,
    p: &OpenProject,
    s: Id<Source>,
    identity: &str,
    force_asr: bool,
) -> Result<Arc<Job>, CmdError> {
    let doc = p.handle.snapshot().doc.clone();
    let audio = audio_of(&doc, s)?;
    let handle = p.handle.clone();
    let host = Arc::clone(&holder.analysis);
    let owner = s.0.to_string();
    let key = format!("{}:{owner}", p.info.id);
    let total = doc.get(s).map_or(0, |x| x.probe.duration.0);
    let partials = Arc::clone(&holder.asr_partials);
    let k2 = key.clone();
    holder.jobs.start_task(
        "analysis",
        identity.to_owned(),
        owner.clone(),
        key,
        Box::new(move |cancel: &AtomicBool| {
            let r = run_analysis(
                host.as_ref(),
                &handle,
                &audio,
                s,
                total,
                force_asr,
                cancel,
                &|p| {
                    if let Ok(mut g) = partials.lock() {
                        g.insert(k2.clone(), p.clone());
                    }
                },
            );
            // a finished (or failed) job leaves nothing to hand over; a cancelled one
            // keeps its windows for the handover (R-ARCH-7)
            if !cancel.load(std::sync::atomic::Ordering::Relaxed)
                && let Ok(mut g) = partials.lock()
            {
                g.remove(&k2);
            }
            r
        }),
    )
}

/// Transcript first (chunked like the app's, so a handover resumes at a window), then
/// Stores a transcript blob. Another transcription of the same source (the app's own,
/// started when the video was added) may have stored this revision first: its blob
/// stands and this one is not an error (decision 0039: AJ's agent saw `put-words`).
fn put_once(
    h: &ProjectHandle,
    owner: &str,
    kind: &str,
    revision: &str,
    body: Vec<u8>,
    trace: &str,
) -> Result<(), CmdError> {
    match h.put_analysis(owner, kind, revision, body) {
        Ok(()) => Ok(()),
        Err(_) if h.analysis(owner, kind, revision).is_some() => Ok(()),
        Err(_) => Err(CmdError::Internal {
            trace_id: trace.into(),
        }),
    }
}

/// waveform, energy, silences and VAD.
#[allow(clippy::too_many_arguments)]
fn run_analysis(
    host: &dyn AnalysisHost,
    h: &ProjectHandle,
    audio: &AudioSource,
    s: Id<Source>,
    total: i64,
    force_asr: bool,
    cancel: &AtomicBool,
    heard: &dyn Fn(&AsrPartial),
) -> Result<Value, CmdError> {
    let owner = s.0.to_string();
    let doc = h.snapshot().doc.clone();
    let asr = if force_asr || !has_words(&doc, &h.analysis_keys(), s) {
        // the analysis below reads the voice this transcription decodes (D.02)
        if !analysed(&h.analysis_keys(), &owner) {
            host.keep_voice(audio, true);
        }
        match chunked::run_stored(
            host,
            audio,
            total,
            &h.analysis_keys(),
            &owner,
            |r| h.analysis(&owner, chunked::WINDOW_KIND, r),
            // a window that cannot be stored is heard again on a resume, nothing worse
            |r, body| {
                let _ = h.put_analysis(&owner, chunked::WINDOW_KIND, r, body);
            },
            force_asr,
            cancel,
            heard,
        ) {
            Ok((p, _)) => {
                // the correction pass: the project's terms spelt right before storing
                let mut tr = p.transcript();
                gmclip_core::terms::correct_transcript(
                    &mut tr,
                    &gmclip_core::terms::project_terms(&h.snapshot().doc),
                );
                put_once(
                    h,
                    &owner,
                    "words",
                    &tr.revision,
                    tr.words_body(),
                    "put-words",
                )?;
                put_once(
                    h,
                    &owner,
                    "lines",
                    &tr.revision,
                    tr.lines_body(),
                    "put-lines",
                )?;
                let has_active = h
                    .snapshot()
                    .doc
                    .iter::<Project>()
                    .next()
                    .is_some_and(|p| p.active_asr.contains_key(&owner));
                let activated = if has_active {
                    None
                } else {
                    Some(system_call(
                        h,
                        "asr_activate",
                        json!({"owner": owner, "revision": tr.revision}),
                    )?)
                };
                json!({"revision": tr.revision, "words": tr.words.len(), "lines": tr.lines.len(), "activated": activated.is_some()})
            }
            Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                host.keep_voice(audio, false);
                return Err(CmdError::Cancelled);
            }
            Err(e) => json!({"error": CmdError::from(e)}),
        }
    } else {
        json!({"skipped": "the source already has a transcript (source_analyze{transcribe: true} runs ASR again)"})
    };
    let keys = h.analysis_keys();
    let mut stored = Vec::new();
    if analysed(&keys, &owner) {
        host.keep_voice(audio, false);
    } else {
        for b in host.analyze(audio, cancel).map_err(host_err)? {
            h.put_analysis(&owner, &b.kind, &b.revision, b.body)
                .map_err(|_| CmdError::Internal {
                    trace_id: "put-analysis".into(),
                })?;
            stored.push(json!({"kind": b.kind, "revision": b.revision}));
        }
    }
    Ok(json!({"source": owner, "stored": stored, "asr": asr}))
}

// -------------------------------------------------------------------------------------
// asr_run (A2.02): ASR as a resumable job
// -------------------------------------------------------------------------------------

/// The voice audio an owner is heard through: a source's voice track, or a sync group's
/// master audio member (its words live on the group, heard once, SPEC §5.1).
fn voice_of(doc: &Doc, owner: Owner) -> Result<(Id<Source>, AudioSource, i64), CmdError> {
    let s = match owner {
        Owner::Source(s) => s,
        Owner::Group(g) => {
            let grp = doc.get(g).ok_or(CmdError::NotFound)?;
            let members: Vec<&SyncMember> = doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .collect();
            grp.master_audio
                .and_then(|m| doc.get(m))
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|m| matches!(m.role, MemberRole::Audio | MemberRole::Both))
                })
                .map(|m| m.source)
                .ok_or_else(|| CmdError::invalid("/args/source", "the group has no audio member"))?
        }
    };
    let total = doc.get(s).map_or(0, |x| x.probe.duration.0);
    Ok((s, audio_of(doc, s)?, total))
}

/// Transcribes `owner` window by window, storing each finished window, and stores the
/// transcript; the first transcript of an owner (or a forced one) becomes active.
#[allow(clippy::too_many_arguments)]
fn run_asr(
    host: &dyn AnalysisHost,
    h: &ProjectHandle,
    audio: &AudioSource,
    owner: &str,
    total: i64,
    force: bool,
    cancel: &AtomicBool,
    progress: &crate::media::Progress,
    heard: &dyn Fn(&AsrPartial),
) -> Result<Value, CmdError> {
    let report = |p: &AsrPartial| {
        json!({"step": "transcribing", "done_s": secs(p.done), "total_s": secs(total),
               "words": p.words.len()})
    };
    let (p, resumed) = chunked::run_stored(
        host,
        audio,
        total,
        &h.analysis_keys(),
        owner,
        |r| h.analysis(owner, chunked::WINDOW_KIND, r),
        |r, body| {
            let _ = h.put_analysis(owner, chunked::WINDOW_KIND, r, body);
        },
        force,
        cancel,
        |p| {
            progress.set(report(p));
            heard(p);
        },
    )
    .map_err(|e| {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            CmdError::Cancelled
        } else {
            CmdError::from(e)
        }
    })?;
    // the correction pass: the project's terms spelt right before storing
    let mut tr = p.transcript();
    gmclip_core::terms::correct_transcript(
        &mut tr,
        &gmclip_core::terms::project_terms(&h.snapshot().doc),
    );
    put_once(
        h,
        owner,
        "words",
        &tr.revision,
        tr.words_body(),
        "put-words",
    )?;
    put_once(
        h,
        owner,
        "lines",
        &tr.revision,
        tr.lines_body(),
        "put-lines",
    )?;
    let active = h
        .snapshot()
        .doc
        .iter::<Project>()
        .next()
        .and_then(|p| p.active_asr.get(owner).cloned());
    let activate = active.is_none() || (force && active.as_deref() != Some(&tr.revision));
    if activate {
        system_call(
            h,
            "asr_activate",
            json!({"owner": owner, "revision": tr.revision}),
        )?;
    }
    Ok(json!({
        "owner": owner, "revision": tr.revision, "words": tr.words.len(), "lines": tr.lines.len(),
        "generation": resumed.generation, "resumed_at_s": (resumed.at > 0).then(|| secs(resumed.at)),
        "activated": activate,
    }))
}

/// `asr_run{source, force?}`: a job hearing a source's (or sync group's) voice track in
/// windows, each stored as it finishes; after a cancel, a quit or a crash, running it
/// again resumes from the last finished window. `job_wait` reports progress.
pub fn asr_run(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: &Value,
) -> Result<Value, CmdError> {
    let doc = p.handle.snapshot().doc.clone();
    let s = args
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| CmdError::invalid("/args/source", "required"))?;
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let owner = Owner::parse(&doc, s)?;
    if holder.analysis.name() == "null" {
        return Err(CmdError::Unsupported {
            platform: "holder".into(),
            reason: "this holder has no analysis".into(),
        });
    }
    let key = format!("{}:{}", p.info.id, owner.key());
    // the background analysis of the same source transcribes it already
    if let Some(j) = holder
        .jobs
        .of("analysis", &key)
        .into_iter()
        .find(|j| j.key == key && j.state().0 == "running")
    {
        return Ok(
            json!({"job": j.id, "state": "running", "display": format!("already transcribing as job {}; job_wait{{job:\"{}\"}} to wait", j.id, j.id)}),
        );
    }
    let (_, audio, total) = voice_of(&doc, owner)?;
    let handle = p.handle.clone();
    let host = Arc::clone(&holder.analysis);
    let partials = Arc::clone(&holder.asr_partials);
    let (k2, ok) = (key.clone(), owner.key());
    let job = holder.jobs.start_job(
        "asr",
        conn.caller().identity,
        ok.clone(),
        key,
        Box::new(move |cancel: &AtomicBool, progress| {
            let r = run_asr(
                host.as_ref(),
                &handle,
                &audio,
                &ok,
                total,
                force,
                cancel,
                progress,
                &|p| {
                    if let Ok(mut g) = partials.lock() {
                        g.insert(k2.clone(), p.clone());
                    }
                },
            );
            if !cancel.load(std::sync::atomic::Ordering::Relaxed)
                && let Ok(mut g) = partials.lock()
            {
                g.remove(&k2);
            }
            r
        }),
    )?;
    Ok(
        json!({"job": job.id, "state": "running", "display": format!("transcribing {} as job {}; job_wait{{job:\"{}\"}} for progress and the result", owner.key(), job.id, job.id)}),
    )
}

// -------------------------------------------------------------------------------------
// stream_process (A2.12): "Process this stream", the profile recipe as a job chain
// -------------------------------------------------------------------------------------

/// AJ's recipe (SPEC §5.1 "Process this stream"): hear it, analyse it, find moments,
/// the full sweep, the density gate; delivery and the visual scout are M4. The agent
/// passes never start on their own: with an agent harness they wait for a click (D17).
pub const RECIPE: &[&str] = &[
    "transcribe",
    "analyse",
    "find_moments",
    "full_sweep",
    "density",
    "delivery",
    "visual_scout",
];

/// The owner a source is heard as: its sync group when it is in one (one ASR per group).
fn heard_as(doc: &Doc, s: Id<Source>) -> Owner {
    doc.iter::<SyncMember>()
        .find(|m| m.source == s)
        .map_or(Owner::Source(s), |m| Owner::Group(m.group))
}

/// The density gate over an owner's moments (D.04's rule, as the moments panel shows it).
fn density_step(h: &ProjectHandle, owner: &str) -> Value {
    let snap = h.snapshot();
    let Some(t) = snap.analysis.active(&snap.doc, owner) else {
        return json!({"step": "density", "state": "skipped", "detail": "no transcript"});
    };
    let lines: Vec<gmclip_core::density::Line> = t
        .lines
        .iter()
        .map(|l| gmclip_core::density::Line {
            start: secs(l.start),
            end: secs(l.end),
            words: l.words[1].saturating_sub(l.words[0]) as usize,
        })
        .collect();
    let listed = system_call(h, "moments_list", json!({"media": owner})).unwrap_or(Value::Null);
    let spans: Vec<(f64, f64)> = listed["moments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["start_s"].as_f64()?, m["end_s"].as_f64()?)))
        .collect();
    match gmclip_core::density::density(&lines, &spans) {
        Some(d) => json!({"step": "density", "state": "done",
            "detail": if d.ok { format!("{} moments, {:.2} per active minute: at or above the floor", d.moments, d.per_min) }
                      else { format!("{} moments, {:.2} per active minute: under the {:.2} floor, short by {}", d.moments, d.per_min, d.floor, d.short_by) },
            "density": d}),
        None => json!({"step": "density", "state": "skipped", "detail": "no talk to measure"}),
    }
}

/// `stream_process{source, steps?}`: the recipe as one job, step by step (`job_wait`
/// shows the step it is on); the result lists every step's state: `done`, `skipped`
/// (already there), `waiting` (an agent pass: press its button), `later` (not in this
/// build) or `failed`.
pub fn stream_process(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: &Value,
) -> Result<Value, CmdError> {
    let doc = p.handle.snapshot().doc.clone();
    let s = args
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| CmdError::invalid("/args/source", "required"))?;
    let steps: Vec<&'static str> = match args.get("steps").and_then(Value::as_array) {
        None => RECIPE.to_vec(),
        Some(v) => {
            let mut out = Vec::new();
            for x in v {
                let name = x.as_str().unwrap_or_default();
                let known = RECIPE
                    .iter()
                    .find(|r| **r == name)
                    .ok_or_else(|| CmdError::invalid("/args/steps", "steps of the recipe"))?;
                out.push(*known);
            }
            out
        }
    };
    let owner = match Owner::parse(&doc, s)? {
        Owner::Source(sid) => heard_as(&doc, sid),
        g => g,
    };
    let (audio_src, audio, total) = voice_of(&doc, owner)?;
    let null = holder.analysis.name() == "null";
    let key = format!("{}:{}", p.info.id, owner.key());
    let handle = p.handle.clone();
    let host = Arc::clone(&holder.analysis);
    let ok = owner.key();
    let job = holder.jobs.start_job(
        "recipe",
        conn.caller().identity,
        ok.clone(),
        key,
        Box::new(move |cancel: &AtomicBool, progress| {
            let mut log: Vec<Value> = Vec::new();
            let grouped = matches!(owner, Owner::Group(_));
            for (i, step) in steps.iter().enumerate() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(CmdError::Cancelled);
                }
                progress.set(json!({"step": step, "index": i, "of": steps.len(), "log": log}));
                let keys = handle.analysis_keys();
                let entry = match *step {
                    "transcribe" if keys.iter().any(|(o, k, _)| *o == ok && k == "words") => {
                        json!({"step": step, "state": "skipped", "detail": "already transcribed"})
                    }
                    "transcribe" if null => {
                        json!({"step": step, "state": "failed", "detail": "this holder has no speech model"})
                    }
                    "transcribe" => match run_asr(
                        host.as_ref(), &handle, &audio, &ok, total, false, cancel, progress, &|_| {},
                    ) {
                        Ok(v) => json!({"step": step, "state": "done",
                            "detail": if grouped { "heard once for the sync group" } else { "heard" }, "result": v}),
                        Err(CmdError::Cancelled) => return Err(CmdError::Cancelled),
                        Err(e) => json!({"step": step, "state": "failed", "detail": e.to_string()}),
                    },
                    "analyse" if analysed(&keys, &audio_src.0.to_string()) => {
                        json!({"step": step, "state": "skipped", "detail": "already analysed"})
                    }
                    "analyse" if null => {
                        json!({"step": step, "state": "failed", "detail": "this holder has no analysis"})
                    }
                    "analyse" => match host.analyze(&audio, cancel) {
                        Ok(blobs) => {
                            let src = audio_src.0.to_string();
                            let n = blobs.len();
                            for b in blobs {
                                let _ = handle.put_analysis(&src, &b.kind, &b.revision, b.body);
                            }
                            json!({"step": step, "state": "done", "detail": format!("{n} analysis blobs")})
                        }
                        Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                            return Err(CmdError::Cancelled);
                        }
                        Err(e) => json!({"step": step, "state": "failed", "detail": CmdError::from(e).to_string()}),
                    },
                    "find_moments" => json!({"step": step, "state": "waiting",
                        "detail": "an agent pass: it starts when you press Find moments (never on its own)", "pass": "find_moments"}),
                    "full_sweep" => json!({"step": step, "state": "waiting",
                        "detail": "an agent pass after find moments: press Sweep for missed moments", "pass": "sweep_hooks"}),
                    "density" => density_step(&handle, &ok),
                    _ => json!({"step": step, "state": "later", "detail": "not in this build (M4)"}),
                };
                log.push(entry);
            }
            let next = log
                .iter()
                .find(|e| e["state"] == "waiting")
                .and_then(|e| e["pass"].as_str())
                .map(str::to_owned);
            progress.set(json!({"step": "done", "index": steps.len(), "of": steps.len()}));
            Ok(json!({"owner": ok, "steps": log, "next": next}))
        }),
    )?;
    Ok(
        json!({"job": job.id, "state": "running", "owner": owner.key(), "display": format!("processing {} as job {}: {}; job_wait{{job:\"{}\"}} for each step", owner.key(), job.id, RECIPE.join(" → "), job.id)}),
    )
}

/// Starts analysis for every source of a project that has none yet (once per holder
/// lifetime per source; `source_analyze` runs it again on demand).
pub fn ensure(holder: &Holder, p: &OpenProject) {
    if holder.analysis.name() == "null" {
        return;
    }
    let doc = p.handle.snapshot().doc.clone();
    let keys = p.handle.analysis_keys();
    for s in doc.iter::<Source>() {
        let k = format!("{}:{}", p.info.id, s.id.0);
        if analysed(&keys, &s.id.0.to_string()) && has_words(&doc, &keys, s.id) {
            continue;
        }
        if holder.analysis_tried(&k) {
            continue;
        }
        let _ = analyze(holder, p, s.id, "system", false);
    }
}

/// The source's analysis as an agent read finds it missing: the running job (pending),
/// else why the last one could not make `what`, else a new job.
fn missing(
    holder: &Holder,
    p: &OpenProject,
    s: Id<Source>,
    what: &str,
    part: &str,
) -> Result<Value, CmdError> {
    let jobs = holder
        .jobs
        .of("analysis", &format!("{}:{}", p.info.id, s.0));
    if let Some(j) = jobs.iter().find(|j| j.state().0 == "running") {
        return Ok(pending_value(j, what));
    }
    if let Some(j) = jobs.first() {
        let (state, result) = j.state();
        let r = result.unwrap_or(Value::Null);
        let err = if state == "done" {
            r.get(part).and_then(|a| a.get("error")).cloned()
        } else {
            r.get("error").cloned()
        };
        if let Some(e) = err {
            let why = e
                .get("reason")
                .and_then(Value::as_str)
                .or(e.get("code").and_then(Value::as_str))
                .unwrap_or("failed")
                .to_owned();
            return Err(CmdError::Unsupported {
                platform: "holder".into(),
                reason: format!(
                    "the analysis (job {}) could not make {what}: {why}; source_analyze runs it again",
                    j.id
                ),
            });
        }
    }
    let j = analyze(holder, p, s, "system", false)?;
    Ok(pending_value(&j, what))
}

fn pending_value(job: &Job, what: &str) -> Value {
    json!({"state": "pending", "job": job.id, "display": format!("{what} is being analysed (job {}); job_wait{{job:\"{}\"}} then ask again", job.id, job.id)})
}

// -------------------------------------------------------------------------------------
// transcript_read
// -------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscriptArgs {
    #[serde(default)]
    pub project: Option<String>,
    /// A source or sync group id.
    pub source: String,
    /// A transcript revision other than the active one.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub from_s: Option<f64>,
    #[serde(default)]
    pub to_s: Option<f64>,
    /// Lines containing these words (case-insensitive).
    #[serde(default)]
    pub search: Option<String>,
    /// Lines within 30 s of an anchor (any anchor kind, e.g. a word or a part edge).
    #[serde(default)]
    #[schemars(with = "Option<Anchor>")]
    pub around: Option<Value>,
    /// From an earlier page's `next_cursor`.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Lines per page (default 40, ≤ 200).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `transcript_read`: L-numbered lines, each with its words `[ordinal, text, start,
/// end]` (flicks on the media clock; a word anchor is `{word_start: {word_id:
/// {asr_revision, ordinal}, t_src_start, t_src_end, normalised_text}}`).
pub fn transcript_read(
    holder: &Holder,
    p: &OpenProject,
    a: TranscriptArgs,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let owner = Owner::parse(doc, &a.source)?;
    check_binding(doc, binding, owner)?;
    let key = owner.key();
    let tr = match &a.revision {
        Some(r) => snap
            .analysis
            .transcript(&key, r)
            .cloned()
            .ok_or(CmdError::NotFound)?,
        None => match snap.analysis.active(doc, &key) {
            Some(t) => Arc::clone(t),
            None => {
                if let Owner::Source(s) = owner {
                    return missing(holder, p, s, "the transcript", "asr");
                }
                return Err(CmdError::invalid(
                    "/source",
                    "no transcript for this media (source_analyze transcribes a source)",
                ));
            }
        },
    };
    let mut lo = a
        .from_s
        .map(|s| flicks(s, "/from_s"))
        .transpose()?
        .map_or(i64::MIN, |f| f.0);
    let mut hi = a
        .to_s
        .map(|s| flicks(s, "/to_s"))
        .transpose()?
        .map_or(i64::MAX, |f| f.0);
    if let Some(v) = a.around {
        let anchor: Anchor =
            serde_json::from_value(v).map_err(|_| CmdError::invalid("/around", "an anchor"))?;
        let r = gmclip_core::anchors::resolve_in(
            doc,
            Some(&snap.analysis),
            binding,
            &anchor,
            "/around",
        )?;
        lo = r.src.0 - 30 * SEC;
        hi = r.src.0 + 30 * SEC;
    }
    let needle: Option<Vec<String>> = a.search.as_ref().map(|q| {
        q.split_whitespace()
            .map(gmclip_core::analysis::norm)
            .filter(|w| !w.is_empty())
            .collect()
    });
    if a.search.as_ref().is_some_and(|q| q.len() > 200) {
        return Err(CmdError::invalid("/search", "at most 200 bytes"));
    }
    let limit = a.limit.unwrap_or(40).clamp(1, 200);
    let start: usize = a.cursor.as_deref().map_or(Ok(0), |c| {
        c.parse()
            .map_err(|_| CmdError::invalid("/cursor", "a cursor from an earlier page"))
    })?;
    let matching: Vec<usize> = tr
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.end >= lo && l.start <= hi)
        .filter(|(_, l)| {
            needle.as_ref().is_none_or(|n| {
                let words: Vec<String> = l
                    .text
                    .split_whitespace()
                    .map(gmclip_core::analysis::norm)
                    .collect();
                n.iter()
                    .all(|w| words.iter().any(|x| x.contains(w.as_str())))
            })
        })
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    let mut display = Vec::new();
    let mut used = 0usize;
    let mut next = None;
    for (k, &i) in matching.iter().enumerate().skip(start) {
        if out.len() >= limit || used > crate::reads::PAGE_BYTES * 3 / 4 {
            next = Some(k.to_string());
            break;
        }
        let l = &tr.lines[i];
        let [w0, w1] = l.words;
        let words: Vec<Value> = (w0..w1.min(tr.words.len() as u32))
            .map(|o| {
                let w = &tr.words[o as usize];
                json!([o, canon::canonical(&w.text, Kind::Text), w.start, w.end])
            })
            .collect();
        let v = json!({"l": format!("L{}", i + 1), "start_s": secs(l.start), "end_s": secs(l.end), "text": canon::field(&l.text, Kind::Text), "words": words});
        used += serde_json::to_string(&v).map_or(0, |s| s.len());
        display.push(format!(
            "L{} [{:.1}–{:.1}] {}",
            i + 1,
            secs(l.start),
            secs(l.end),
            canon::block(&l.text, Kind::Text)
        ));
        out.push(v);
    }
    let shown = out.len();
    Ok(json!({
        "source": key,
        "asr_revision": tr.revision,
        "untrusted": ["lines[].text", "lines[].words[][1]"],
        "word_times": "flicks on the media clock",
        "lines": out,
        "total_lines": matching.len(),
        "next_cursor": next,
        "as_of": doc.rev(),
        "display": format!("transcript {} rev {} — {shown} of {} line(s){}\n{}\nas_of {}", key, canon::block(&tr.revision, Kind::Name), matching.len(), if next.is_some() { ", more: next_cursor" } else { "" }, display.join("\n"), doc.rev()),
    }))
}

// -------------------------------------------------------------------------------------
// analysis_get
// -------------------------------------------------------------------------------------

/// `analysis_get` for the read kinds: energy and silences from the stored blobs,
/// loudness and speech measured or read for the span.
pub fn analysis_get(
    holder: &Holder,
    p: &OpenProject,
    source: &str,
    kind: &str,
    from_s: Option<f64>,
    to_s: Option<f64>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    if !matches!(kind, "energy" | "silences" | "speech" | "loudness") {
        return Err(CmdError::Unsupported {
            platform: "m1".into(),
            reason: format!("{kind} analysis is not built yet (prosody M3/M4; visual kinds M2+)"),
        });
    }
    let doc = p.handle.snapshot().doc.clone();
    let owner = Owner::parse(&doc, source)?;
    check_binding(&doc, binding, owner)?;
    let from = from_s
        .map(|s| flicks(s, "/from_s"))
        .transpose()?
        .unwrap_or(Flicks(0));
    let (s, off, audio) = audio_at(&doc, owner, from.0)?;
    let dur = doc.get(s).map_or(0, |x| x.probe.duration.0);
    let to = to_s
        .map(|s| flicks(s, "/to_s"))
        .transpose()?
        .unwrap_or(Flicks(if dur > 0 {
            dur + off
        } else {
            from.0 + 3_600 * SEC
        }));
    if to.0 <= from.0 {
        return Err(CmdError::invalid("/to_s", "after from_s"));
    }
    let (sf, st) = (Flicks(from.0 - off), Flicks(to.0 - off));
    let blob_kind = match kind {
        "energy" => "energy_20ms",
        "silences" => "silences_40ms",
        "speech" => "vad_speech",
        "loudness" => {
            let l = holder.analysis.loudness(&audio, sf, st).map_err(host_err)?;
            return Ok(
                json!({"source": owner.key(), "kind": "loudness", "from_s": secs(from.0), "to_s": secs(to.0),
                "integrated_lufs": (l.integrated_lufs * 10.0).round() / 10.0, "true_peak_dbtp": l.true_peak_dbtp.map(|x| (x * 10.0).round() / 10.0),
                "display": format!("loudness {:.1}–{:.1} s: {:.1} LUFS integrated", secs(from.0), secs(to.0), l.integrated_lufs)}),
            );
        }
        _ => {
            return Err(CmdError::Unsupported {
                platform: "m1".into(),
                reason: format!(
                    "{kind} analysis is not built yet (prosody M3/M4; visual kinds M2+)"
                ),
            });
        }
    };
    let keys = p.handle.analysis_keys();
    let owner_s = s.0.to_string();
    let Some((_, _, rev)) = keys
        .iter()
        .find(|(o, k, _)| *o == owner_s && k == blob_kind)
        .cloned()
    else {
        if blob_kind == "vad_speech" && analysed(&keys, &owner_s) {
            return Err(CmdError::Unsupported {
                platform: "holder".into(),
                reason: "voice activity needs the VAD model".into(),
            });
        }
        return missing(holder, p, s, "this analysis", "stored");
    };
    let body = p
        .handle
        .analysis(&owner_s, blob_kind, &rev)
        .ok_or(CmdError::NotFound)?;
    let mut v = holder
        .analysis
        .blob_summary(blob_kind, &body, sf, st, 400)
        .map_err(host_err)?;
    // regions and points on the media clock
    let shift = secs(off);
    if shift != 0.0 {
        for key in ["regions_s", "points"] {
            if let Some(arr) = v.get_mut(key).and_then(Value::as_array_mut) {
                for x in arr {
                    if let Some(t) = x.get_mut(0).and_then(|t| t.as_f64().map(|f| (t, f))) {
                        *t.0 = json!(t.1 + shift);
                    }
                    if key == "regions_s"
                        && let Some(t) = x.get_mut(1).and_then(|t| t.as_f64().map(|f| (t, f)))
                    {
                        *t.0 = json!(t.1 + shift);
                    }
                }
            }
        }
    }
    let n = v
        .get("points")
        .or(v.get("regions_s"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if let Some(o) = v.as_object_mut() {
        o.insert("source".into(), json!(owner.key()));
        o.insert("kind".into(), json!(kind));
        o.insert("revision".into(), json!(rev));
        o.insert("from_s".into(), json!(secs(from.0)));
        o.insert("to_s".into(), json!(secs(to.0)));
        o.insert(
            "display".into(),
            json!(format!(
                "{kind} {:.1}–{:.1} s: {n} entries (revision {rev})",
                secs(from.0),
                secs(to.0)
            )),
        );
    }
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

// -------------------------------------------------------------------------------------
// listen
// -------------------------------------------------------------------------------------

pub fn listen(
    holder: &Holder,
    p: &OpenProject,
    source: &str,
    from_s: f64,
    to_s: f64,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let doc = p.handle.snapshot().doc.clone();
    let owner = Owner::parse(&doc, source)?;
    check_binding(&doc, binding, owner)?;
    let (from, to) = (flicks(from_s, "/from_s")?, flicks(to_s, "/to_s")?);
    let (_, off, audio) = audio_at(&doc, owner, from.0)?;
    let h = holder
        .analysis
        .listen(&audio, Flicks(from.0 - off), Flicks(to.0 - off))
        .map_err(host_err)?;
    let words: Vec<Value> = h
        .transcribed
        .words
        .iter()
        .map(|w| {
            json!([
                canon::canonical(&w.text, Kind::Text),
                secs(w.start.0 + off),
                secs(w.end.0 + off)
            ])
        })
        .collect();
    let text: String = h
        .transcribed
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let energy: Vec<f32> = h
        .energy_db_50ms
        .iter()
        .map(|d| (d * 10.0).round() / 10.0)
        .collect();
    Ok(crate::reads::cap(
        json!({
            "source": owner.key(),
            "from_s": secs(from.0), "to_s": secs(to.0),
            "asr_revision": h.transcribed.revision,
            "untrusted": ["words[][0]"],
            "words": words,
            "loudness": h.loudness.map(|l| json!({"integrated_lufs": (l.integrated_lufs * 10.0).round() / 10.0, "true_peak_dbtp": l.true_peak_dbtp})),
            "energy_db_50ms": energy,
            "silences_s": h.silences.iter().map(|(a, b)| json!([secs(a.0 + off), secs(b.0 + off)])).collect::<Vec<_>>(),
            "display": format!("heard {:.1}–{:.1} s: {}", secs(from.0), secs(to.0), canon::block(canon::clip(&text, 2_000), Kind::Text)),
        }),
        crate::reads::PAGE_BYTES,
    ))
}

// -------------------------------------------------------------------------------------
// measure
// -------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntervalSpec {
    from: MeasureEnd,
    to: MeasureEnd,
    #[serde(default)]
    evidence_frames: Vec<FrameRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeSpec {
    /// `word_start`, `word_end`, `audible_start`, `audible_end` or `cut_point`.
    anchor: Anchor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoudnessSpec {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    from_s: Option<f64>,
    #[serde(default)]
    to_s: Option<f64>,
    #[serde(default)]
    cut: Option<String>,
}

/// The frame of a source at source time `t` (its first video track's rate).
fn frame_at(doc: &Doc, s: Id<Source>, t: i64) -> Option<FrameRef> {
    let src = doc.get(s)?;
    let fps = src.probe.tracks.iter().find_map(|t| t.fps)?;
    if fps.num <= 0 || fps.den == 0 {
        return None;
    }
    let idx =
        (i128::from(t) * i128::from(fps.num) / (i128::from(fps.den) * i128::from(SEC))) as i64;
    Some(FrameRef {
        source: s,
        frame_index: idx.max(0),
    })
}

fn end_time(
    doc: &Doc,
    view: &gmclip_core::analysis::AnalysisView,
    e: &MeasureEnd,
    binding: Option<Id<Cut>>,
    path: &str,
) -> Result<(i64, String, Option<FrameRef>), CmdError> {
    match e {
        MeasureEnd::Anchor(a) => {
            let r = gmclip_core::anchors::resolve_in(doc, Some(view), binding, a, path)?;
            Ok((r.src.0, r.method, None))
        }
        MeasureEnd::Frame(f) => {
            let src = doc.get(f.source).ok_or(CmdError::NotFound)?;
            let fps = src
                .probe
                .tracks
                .iter()
                .find_map(|t| t.fps)
                .ok_or_else(|| CmdError::invalid(path, "the source has no frame rate"))?;
            if fps.num <= 0 || f.frame_index < 0 {
                return Err(CmdError::invalid(path, "a frame of the source"));
            }
            let t = i128::from(f.frame_index) * i128::from(fps.den) * i128::from(SEC)
                / i128::from(fps.num);
            Ok((
                i64::try_from(t).map_err(|_| CmdError::invalid(path, "out of range"))?,
                "frame".into(),
                Some(*f),
            ))
        }
        MeasureEnd::Event(_) => Err(CmdError::Unsupported {
            platform: "m1".into(),
            reason: "visual events land with OCR (M2)".into(),
        }),
    }
}

/// The two words of a cut point: `(w1, w2, owner)` around the referenced word.
fn cut_pair(
    view: &gmclip_core::analysis::AnalysisView,
    w: &WordRef,
    side: CutSide,
) -> Result<(Word, Word, String), CmdError> {
    let id = &w.word_id;
    let (owner, _) = view.word(w).ok_or_else(|| {
        CmdError::invalid(
            "/spec/anchor",
            "no such word (read it with transcript_read)",
        )
    })?;
    let tr = view
        .transcript(owner, &id.asr_revision)
        .ok_or(CmdError::NotFound)?;
    let k = id.ordinal as usize;
    let (a, b) = match side {
        CutSide::Before => (k.checked_sub(1), Some(k)),
        CutSide::After => (Some(k), Some(k + 1)),
    };
    let (Some(a), Some(b)) = (
        a.and_then(|i| tr.words.get(i)),
        b.and_then(|i| tr.words.get(i)),
    ) else {
        return Err(CmdError::invalid(
            "/spec/anchor",
            "a cut point at the transcript's edge has no neighbour",
        ));
    };
    Ok((a.clone(), b.clone(), owner.to_owned()))
}

pub fn measure(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    kind: &str,
    spec: Value,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let snap = p.handle.snapshot();
    let doc = &snap.doc;
    let view = &snap.analysis;
    let identity = conn.caller().identity;
    let (args, display) = match kind {
        "interval" => {
            let s: IntervalSpec = serde_json::from_value(spec).map_err(|_| CmdError::invalid("/spec", "interval{from, to, evidence_frames?}: each end {anchor: …} or {frame: {source, frame_index}}"))?;
            let (a, ma, fa) = end_time(doc, view, &s.from, binding, "/spec/from")?;
            let (b, mb, fb) = end_time(doc, view, &s.to, binding, "/spec/to")?;
            let mut frames = s.evidence_frames;
            frames.extend(fa);
            frames.extend(fb);
            frames.truncate(16);
            let value = (b - a) as f64 / SEC as f64;
            (
                json!({"kind": "interval", "from": s.from, "to": s.to, "value": value, "unit": "s", "evidence_frames": frames, "method": format!("{ma} → {mb}"), "author": identity}),
                format!("interval {value:.3} s"),
            )
        }
        "edge" => {
            let s: EdgeSpec = serde_json::from_value(spec).map_err(|_| CmdError::invalid("/spec", "edge{anchor}: a word_start/word_end/audible_start/audible_end or cut_point anchor"))?;
            let (rule, w1, w2, owner) = match &s.anchor {
                Anchor::WordStart(w)
                | Anchor::AudibleStart(w)
                | Anchor::WordEnd(w)
                | Anchor::AudibleEnd(w) => {
                    let (owner, word) = view.word(w).ok_or_else(|| {
                        CmdError::invalid(
                            "/spec/anchor",
                            "no such word (read it with transcript_read)",
                        )
                    })?;
                    let rule = if matches!(s.anchor, Anchor::WordStart(_) | Anchor::AudibleStart(_))
                    {
                        EdgeRule::AudibleStart
                    } else {
                        EdgeRule::AudibleEnd
                    };
                    (rule, word.clone(), None, owner.to_owned())
                }
                Anchor::CutPoint(c) => {
                    let (a, b, owner) = cut_pair(view, &c.word, c.side)?;
                    let rule = match c.rule {
                        gmclip_model::anchor::CutRule::EnergyMin => EdgeRule::EnergyMin,
                        gmclip_model::anchor::CutRule::Arbiter => EdgeRule::Arbiter,
                    };
                    (rule, a, Some(b), owner)
                }
                _ => {
                    return Err(CmdError::invalid(
                        "/spec/anchor",
                        "an edge is measured on a word or cut point anchor",
                    ));
                }
            };
            let o = Owner::parse(doc, &owner)?;
            check_binding(doc, binding, o)?;
            let (src, off, audio) = audio_at(doc, o, w1.start)?;
            let e = holder
                .analysis
                .edge(
                    &audio,
                    rule,
                    (&w1.text, Flicks(w1.start - off), Flicks(w1.end - off)),
                    w2.as_ref()
                        .map(|w| (w.text.as_str(), Flicks(w.start - off), Flicks(w.end - off))),
                )
                .map_err(|e| match e {
                    gmclip_core::host::HostError::Failed(m) if m == "unresolvable edge" => {
                        CmdError::invalid("/spec/anchor", "unresolvable edge")
                    }
                    other => host_err(other),
                })?;
            let t = e.value.0 + off;
            let frames: Vec<FrameRef> = frame_at(doc, src, e.value.0).into_iter().collect();
            (
                json!({"kind": "edge", "from": {"anchor": s.anchor}, "value": t as f64 / SEC as f64, "unit": "s", "evidence_frames": frames, "method": e.method, "author": identity}),
                format!("edge at {:.3} s ({})", secs(t), e.method),
            )
        }
        "loudness" => {
            let s: LoudnessSpec = serde_json::from_value(spec).map_err(|_| {
                CmdError::invalid("/spec", "loudness{source, from_s, to_s} or loudness{cut}")
            })?;
            match (&s.cut, &s.source) {
                (Some(c), None) => {
                    let cut = crate::reads::cut_id(c)?;
                    crate::reads::check_cut_binding(binding, cut)?;
                    doc.get(cut).ok_or(CmdError::NotFound)?;
                    // each part's loudness, combined as a duration-weighted power mean
                    let (mut power, mut total) = (0.0f64, 0.0f64);
                    for part in gmclip_model::clock::cut_layout(doc, cut) {
                        let pt = doc.get(part.part).ok_or(CmdError::NotFound)?;
                        let Some((_, off, audio)) =
                            audio_at(doc, Owner::from_media(pt.media), pt.src_in.0).ok()
                        else {
                            continue;
                        };
                        let Ok(l) = holder.analysis.loudness(
                            &audio,
                            Flicks(pt.src_in.0 - off),
                            Flicks(pt.src_out.0 - off),
                        ) else {
                            continue;
                        };
                        let d = (pt.src_out.0 - pt.src_in.0) as f64;
                        if l.integrated_lufs.is_finite() {
                            power += d * 10f64.powf(l.integrated_lufs / 10.0);
                            total += d;
                        }
                    }
                    if total == 0.0 {
                        return Err(CmdError::invalid(
                            "/spec/cut",
                            "no part with measurable audio",
                        ));
                    }
                    let lufs = 10.0 * (power / total).log10();
                    (
                        json!({"kind": "loudness", "from": {"anchor": {"cut_start": {"cut": cut}}}, "to": {"anchor": {"cut_end": {"cut": cut}}}, "value": lufs, "unit": "LUFS", "method": "bs1770 per part, power mean by duration", "author": identity}),
                        format!("cut loudness {lufs:.1} LUFS"),
                    )
                }
                (None, Some(src)) => {
                    let o = Owner::parse(doc, src)?;
                    check_binding(doc, binding, o)?;
                    let (from, to) = (
                        flicks(s.from_s.unwrap_or(f64::NAN), "/spec/from_s")?,
                        flicks(s.to_s.unwrap_or(f64::NAN), "/spec/to_s")?,
                    );
                    if to.0 <= from.0 {
                        return Err(CmdError::invalid("/spec/to_s", "after from_s"));
                    }
                    let (sid, off, audio) = audio_at(doc, o, from.0)?;
                    let l = holder
                        .analysis
                        .loudness(&audio, Flicks(from.0 - off), Flicks(to.0 - off))
                        .map_err(host_err)?;
                    let time = |t: Flicks| gmclip_model::anchor::RawTime {
                        time: gmclip_model::clock::ModelTime::Src {
                            source: Some(sid),
                            group: None,
                            t: Flicks(t.0 - off),
                        },
                        reason: "measured span".into(),
                    };
                    (
                        json!({"kind": "loudness", "from": {"anchor": {"raw": time(from)}}, "to": {"anchor": {"raw": time(to)}}, "value": l.integrated_lufs, "unit": "LUFS", "method": "bs1770 integrated", "author": identity}),
                        format!(
                            "loudness {:.1} LUFS ({:.1}–{:.1} s)",
                            l.integrated_lufs,
                            secs(from.0),
                            secs(to.0)
                        ),
                    )
                }
                _ => {
                    return Err(CmdError::invalid(
                        "/spec",
                        "a cut, or a source with from_s and to_s",
                    ));
                }
            }
        }
        _ => return Err(CmdError::invalid("/kind", "interval, edge or loudness")),
    };
    if args["value"].as_f64().is_none_or(|v| !v.is_finite()) {
        return Err(CmdError::invalid(
            "/spec",
            "the measurement has no finite value (silence?)",
        ));
    }
    let out = system_call(&p.handle, "measurement_add", args.clone())?;
    let id = out
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(json!({
        "measurement": id,
        "anchor": {"measured": id},
        "value": args["value"], "unit": args["unit"], "method": args["method"], "evidence_frames": args["evidence_frames"],
        "display": format!("{display}; cite it as {{\"measured\": \"{id}\"}}"),
    }))
}

impl Owner {
    fn from_media(m: MediaRef) -> Owner {
        match m {
            MediaRef::Source(s) => Owner::Source(s),
            MediaRef::Group(g) => Owner::Group(g),
        }
    }
}

// -------------------------------------------------------------------------------------
// Sweeps (SPEC §5.5)
// -------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SweepArgs {
    cut: String,
    #[serde(default)]
    min_s: Option<f64>,
    #[serde(default)]
    keep_s: Option<f64>,
    #[serde(default)]
    dry_run: Option<bool>,
}

/// One part's span, grouped by its audio source.
struct PartSpan {
    media: MediaRef,
    owner: String,
    src: Id<Source>,
    off: i64,
    from: i64,
    to: i64,
}

/// Sweep candidates over a cut's parts: `(media, from, to, reason)` source spans, the
/// skipped ones, the analysis revision and how many parts have no audio.
#[derive(Debug)]
pub struct Candidates {
    pub cuts: Vec<(MediaRef, Flicks, Flicks, String)>,
    pub skipped: Vec<Value>,
    pub revision: String,
    pub unsupported: usize,
}

/// The candidates of `sweep_pauses`, `sweep_fillers` or `sweep_retakes` on `cut`
/// (shared with the app's sweeps panel).
pub fn sweep_candidates(
    host: &dyn AnalysisHost,
    snap: &gmclip_core::Snapshot,
    cut: Id<Cut>,
    kind: SweepKind,
    min: f64,
    keep: f64,
) -> Result<Candidates, CmdError> {
    let doc = &snap.doc;
    doc.get(cut).ok_or(CmdError::NotFound)?;
    if kind == SweepKind::Retakes {
        return Ok(retake_candidates(snap, cut));
    }
    let pauses = kind == SweepKind::Pauses;
    let mut spans: Vec<PartSpan> = Vec::new();
    let mut unsupported = 0;
    for sp in gmclip_model::clock::cut_layout(doc, cut) {
        let Some(pt) = doc.get(sp.part) else { continue };
        match audio_source(doc, pt.media, pt.src_in.0) {
            Some((src, off)) => spans.push(PartSpan {
                media: pt.media,
                owner: Owner::from_media(pt.media).key(),
                src,
                off,
                from: pt.src_in.0,
                to: pt.src_out.0,
            }),
            None => unsupported += 1,
        }
    }
    let mut cuts: Vec<(MediaRef, Flicks, Flicks, String)> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut revisions = BTreeSet::new();
    let srcs: BTreeSet<(Id<Source>, i64)> = spans.iter().map(|s| (s.src, s.off)).collect();
    for (src, off) in srcs {
        let mine: Vec<&PartSpan> = spans
            .iter()
            .filter(|s| s.src == src && s.off == off)
            .collect();
        let audio = audio_of(doc, src)?;
        // the words these parts play, on the source clock
        let mut words: Vec<HeardWord> = Vec::new();
        for owner in mine
            .iter()
            .map(|s| s.owner.as_str())
            .collect::<BTreeSet<_>>()
        {
            if let Some(t) = snap.analysis.active(doc, owner) {
                words.extend(
                    t.words
                        .iter()
                        .filter(|w| {
                            mine.iter()
                                .any(|s| s.owner == owner && w.end > s.from && w.start < s.to)
                        })
                        .map(|w| HeardWord {
                            text: w.text.clone(),
                            start: Flicks(w.start - off),
                            end: Flicks(w.end - off),
                            confidence: None,
                        }),
                );
            }
        }
        let soundless = doc
            .get(src)
            .is_some_and(|s| s.probe.audio_tracks.is_empty());
        if !pauses && words.is_empty() && soundless {
            unsupported += mine.len();
            continue;
        }
        if !pauses && words.is_empty() {
            return Err(CmdError::invalid(
                "/args/cut",
                "fillers need a transcript of the cut's media (source_analyze)",
            ));
        }
        let ranges: Vec<(Flicks, Flicks)> = mine
            .iter()
            .map(|s| (Flicks(s.from - off), Flicks(s.to - off)))
            .collect();
        let found = if pauses {
            host.pauses(
                &audio,
                &ranges,
                Flicks((min * SEC as f64) as i64),
                Flicks((keep * SEC as f64) as i64),
                &words,
            )
        } else {
            host.fillers(&audio, &ranges, &words)
        };
        // a video with no sound (a graphic, a silent clip) has nothing to sweep: its
        // parts are listed as without audio, the rest of the cut is still swept
        // (decision 0040: one soundless clip failed the whole sweep)
        let c = match found {
            Ok(c) => c,
            Err(_) if soundless => {
                unsupported += mine.len();
                continue;
            }
            Err(e) => return Err(host_err(e)),
        };
        revisions.insert(c.revision.clone());
        for x in c.cuts {
            let (f, t) = (Flicks(x.from.0 + off), Flicks(x.to.0 + off));
            if let Some(s) = mine.iter().find(|s| f.0 >= s.from && t.0 <= s.to) {
                cuts.push((s.media, f, t, x.reason));
            }
        }
        skipped.extend(c.skipped.into_iter().map(|x| json!({"from_s": secs(x.from.0 + off), "to_s": secs(x.to.0 + off), "reason": x.reason})));
    }
    cuts.sort_by_key(|c| c.1);
    Ok(Candidates {
        cuts,
        skipped,
        revision: sweep_revision(revisions.into_iter().collect::<Vec<_>>().join("+")),
        unsupported,
    })
}

/// A sweep's revision as `sweep_apply` takes it (at most 300 bytes): the sources'
/// revisions joined, or their digest when a cut of many videos makes that longer (25
/// sources ran past the bound and every apply failed with "too long"). It is only ever
/// compared for equality, so the digest stays stale-safe.
fn sweep_revision(joined: String) -> String {
    if joined.len() <= 200 {
        return joined;
    }
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut h, joined.as_bytes());
    format!("sweep:{}", crate::wire::hex(&sha2::Digest::finalize(h)))
}

#[cfg(test)]
mod sweep_revision_tests {
    #[test]
    fn many_sources_fit_the_bound() {
        let short = "quiet_grid@1/aa+quiet_grid@1/bb".to_owned();
        assert_eq!(super::sweep_revision(short.clone()), short);
        let long: String = (0..25)
            .map(|i| format!("quiet_grid@1/{i:032}"))
            .collect::<Vec<_>>()
            .join("+");
        let r = super::sweep_revision(long.clone());
        assert!(r.len() <= 300 && r.starts_with("sweep:"));
        assert_eq!(
            r,
            super::sweep_revision(long),
            "the same sources, the same revision"
        );
    }
}

/// The retakes of what `cut` says (decision 0040, `gmclip_core::script`): each
/// earlier attempt's spans, listed with both attempts; the revision is the
/// transcripts read.
fn retake_candidates(snap: &gmclip_core::Snapshot, cut: Id<Cut>) -> Candidates {
    let sc = gmclip_core::script::script(&snap.doc, &snap.analysis, cut);
    let mut cuts = Vec::new();
    for r in &sc.retakes {
        let why = format!(
            "{:?} «{}» → «{}»",
            r.kind,
            canon::clip(&r.earlier, 120),
            canon::clip(&r.later, 120)
        )
        .to_lowercase();
        for (m, a, b) in &r.spans {
            cuts.push((*m, Flicks(*a), Flicks(*b), why.clone()));
        }
    }
    cuts.sort_by_key(|c| c.1);
    let unsupported = sc.parts.iter().filter(|p| !p.transcript).count();
    Candidates {
        cuts,
        skipped: vec![],
        // the transcripts read, as a fingerprint (their revisions in full run past the
        // 300 bytes a sweep records)
        revision: {
            let mut h = <sha2::Sha256 as sha2::Digest>::new();
            for (o, r) in &sc.revisions {
                sha2::Digest::update(&mut h, format!("{o}={r}\n").as_bytes());
            }
            format!(
                "retakes@1/{}t/{}",
                sc.revisions.len(),
                crate::wire::hex(&sha2::Digest::finalize(h)[..12])
            )
        },
        unsupported,
    }
}

/// `sweep_pauses` / `sweep_fillers` / `sweep_retakes`: the candidates over a cut's parts (dry run, the
/// default), or applied as one `sweep_apply` transaction under the caller's base.
#[allow(clippy::too_many_arguments)]
pub fn sweep(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    command: &str,
    args: Value,
    base: Option<Base>,
    request_id: Option<String>,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: SweepArgs = serde_json::from_value(args)
        .map_err(|_| CmdError::invalid("/args", "{cut, min_s?, keep_s?, dry_run?}"))?;
    let kind = match command {
        "sweep_pauses" => SweepKind::Pauses,
        "sweep_retakes" => SweepKind::Retakes,
        _ => SweepKind::Fillers,
    };
    let pauses = kind == SweepKind::Pauses;
    if !pauses && (a.min_s.is_some() || a.keep_s.is_some()) {
        return Err(CmdError::invalid(
            "/args",
            "sweep_fillers and sweep_retakes take {cut, dry_run?}",
        ));
    }
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let min = a.min_s.unwrap_or(0.32);
    let keep = a.keep_s.unwrap_or(0.10);
    if !(0.15..=5.0).contains(&min) {
        return Err(CmdError::invalid("/args/min_s", "0.15–5 s"));
    }
    if !(0.0..=1.0).contains(&keep) || keep >= min {
        return Err(CmdError::invalid("/args/keep_s", "0–1 s, below min_s"));
    }
    // a dry run is a preview too: its id applies exactly these spans (A2.06)
    let rec = sweep_preview_make(
        holder.analysis.as_ref(),
        &p.handle,
        &p.info.id,
        cut,
        kind,
        min,
        keep,
    )?;
    let (cuts, skipped, revision, unsupported) = (
        &rec.cuts,
        rec.skipped.clone(),
        &rec.revision,
        rec.unsupported,
    );
    let doc_rev = rec.as_of;
    let total: f64 = cuts
        .iter()
        .map(|c| (c.2.0 - c.1.0) as f64 / SEC as f64)
        .sum();
    let listed: Vec<Value> = cuts.iter().map(|(m, f, t, r)| json!({"media": m, "from_s": secs(f.0), "to_s": secs(t.0), "seconds": secs(t.0 - f.0), "reason": canon::canonical(r, Kind::Text)})).collect();
    let kind = kind.name();
    let dry = a.dry_run.unwrap_or(true);
    let mut v = json!({
        "kind": kind, "dry_run": dry, "cuts": listed, "skipped": skipped, "removes_s": (total * 1000.0).round() / 1000.0,
        "parts_without_audio": unsupported, "revision": revision, "as_of": doc_rev,
        "preview_id": rec.id,
    });
    if dry || cuts.is_empty() {
        v["display"] = json!(format!(
            "{kind}: {} cut(s), {:.2} s, {} skipped{} · as_of {}",
            cuts.len(),
            total,
            v["skipped"].as_array().map_or(0, Vec::len),
            if dry {
                " (preview: sweep_apply{preview_id} with base applies exactly these)"
            } else {
                ""
            },
            doc_rev
        ));
        return Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES));
    }
    let ok = p.handle.call(CallRequest {
        command: "sweep_apply".into(),
        args: json!({"cut": cut, "kind": kind, "revision": revision, "spans": cuts.iter().map(|(m, f, t, _)| json!({"media": m, "from": f, "to": t})).collect::<Vec<_>>()}),
        base,
        request_id,
        caller: conn.caller(),
    })?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    v["applied"] = ok.output.clone();
    v["committed"] = json!(ok.committed);
    v["as_of"] = json!(ok.as_of);
    v["display"] = json!(format!(
        "{kind}: removed {} span(s), {:.2} s; committed rev {:?} · as_of {}",
        cuts.len(),
        total,
        ok.committed,
        ok.as_of
    ));
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

// -------------------------------------------------------------------------------------
// Sweep previews (A2.06): preview, then apply exactly what was previewed
// -------------------------------------------------------------------------------------

/// A sweep preview: the spans listed, on the cut and analysis they were found on.
#[derive(Debug)]
pub struct SweepPreviewRec {
    pub id: String,
    pub project: String,
    pub cut: Id<Cut>,
    pub sweep: SweepKind,
    pub min_s: f64,
    pub keep_s: f64,
    pub cuts: Vec<(MediaRef, Flicks, Flicks, String)>,
    pub skipped: Vec<Value>,
    pub revision: String,
    /// The cut's parts when previewed (`gmclip_core::commands::analysis::cut_sig`).
    pub cut_sig: String,
    pub unsupported: usize,
    pub as_of: u64,
}

impl SweepPreviewRec {
    pub fn kind(&self) -> &'static str {
        self.sweep.name()
    }

    /// Seconds the listed spans remove.
    pub fn removes_s(&self) -> f64 {
        self.cuts
            .iter()
            .map(|c| (c.2.0 - c.1.0) as f64 / SEC as f64)
            .sum()
    }
}

/// Previews kept for `sweep_apply{preview_id}` (process-wide: the app and its holder
/// share them); the oldest go first. A preview lasts the session: after a restart,
/// preview again.
const KEEP_PREVIEWS: usize = 64;

fn previews() -> &'static std::sync::Mutex<std::collections::VecDeque<Arc<SweepPreviewRec>>> {
    static P: std::sync::OnceLock<
        std::sync::Mutex<std::collections::VecDeque<Arc<SweepPreviewRec>>>,
    > = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// Checks the sweep parameters (pauses of 0.15–5 s, keeping less than the pause).
pub fn sweep_params(
    kind: SweepKind,
    min: Option<f64>,
    keep: Option<f64>,
) -> Result<(f64, f64), CmdError> {
    if kind != SweepKind::Pauses && (min.is_some() || keep.is_some()) {
        return Err(CmdError::invalid(
            "/args",
            "fillers and retakes take no min_s or keep_s",
        ));
    }
    let (min, keep) = (min.unwrap_or(0.32), keep.unwrap_or(0.10));
    if !(0.15..=5.0).contains(&min) {
        return Err(CmdError::invalid("/args/min_s", "0.15–5 s"));
    }
    if !(0.0..=1.0).contains(&keep) || keep >= min {
        return Err(CmdError::invalid("/args/keep_s", "0–1 s, below min_s"));
    }
    Ok((min, keep))
}

/// Finds a sweep's spans on `cut` and keeps them as a preview (`sweep_preview`).
pub fn sweep_preview_make(
    host: &dyn AnalysisHost,
    handle: &ProjectHandle,
    project: &str,
    cut: Id<Cut>,
    sweep: SweepKind,
    min_s: f64,
    keep_s: f64,
) -> Result<Arc<SweepPreviewRec>, CmdError> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let snap = handle.snapshot();
    let c = sweep_candidates(host, &snap, cut, sweep, min_s, keep_s)?;
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let rec = Arc::new(SweepPreviewRec {
        id: format!("sp{n}"),
        project: project.to_owned(),
        cut,
        sweep,
        min_s,
        keep_s,
        cuts: c.cuts,
        skipped: c.skipped,
        revision: c.revision,
        cut_sig: gmclip_core::commands::analysis::cut_sig(&snap.doc, cut),
        unsupported: c.unsupported,
        as_of: snap.doc.rev(),
    });
    if let Ok(mut g) = previews().lock() {
        while g.len() >= KEEP_PREVIEWS {
            g.pop_front();
        }
        g.push_back(Arc::clone(&rec));
    }
    Ok(rec)
}

/// A preview kept by [`sweep_preview_make`].
pub fn sweep_preview_get(id: &str) -> Option<Arc<SweepPreviewRec>> {
    previews().lock().ok()?.iter().find(|p| p.id == id).cloned()
}

/// Applies a preview exactly (`only`: a subset of its spans, by index) as one
/// `sweep_apply` transaction: `NotFound` for an unknown preview, `StaleAnalysis` when
/// the analysis it was found on changed, `Conflict` when the cut's parts did.
#[allow(clippy::too_many_arguments)]
pub fn sweep_preview_apply(
    host: &dyn AnalysisHost,
    handle: &ProjectHandle,
    project: &str,
    preview_id: &str,
    only: Option<&[usize]>,
    caller: Caller,
    base: Option<Base>,
    request_id: Option<String>,
) -> Result<gmclip_core::CallOk, CmdError> {
    let rec = sweep_preview_get(preview_id)
        .filter(|r| r.project == project)
        .ok_or(CmdError::NotFound)?;
    if caller.cut_binding.is_some_and(|c| c != rec.cut) {
        return Err(CmdError::Unauthorized {
            scope: "cut_binding".into(),
        });
    }
    let snap = handle.snapshot();
    // the cut first: a changed cut is a conflict (even if its analysis changed too)
    if gmclip_core::commands::analysis::cut_sig(&snap.doc, rec.cut) == rec.cut_sig {
        let now = sweep_candidates(host, &snap, rec.cut, rec.sweep, rec.min_s, rec.keep_s)?;
        if now.revision != rec.revision {
            return Err(CmdError::StaleAnalysis {
                kind: format!("sweep_{}", rec.kind()),
                rev: snap.doc.rev(),
            });
        }
    }
    let picked: Vec<&(MediaRef, Flicks, Flicks, String)> = match only {
        None => rec.cuts.iter().collect(),
        Some(ix) => {
            if ix.iter().any(|&i| i >= rec.cuts.len()) {
                return Err(CmdError::invalid(
                    "/args/only",
                    "an index the preview listed",
                ));
            }
            rec.cuts
                .iter()
                .enumerate()
                .filter(|(i, _)| ix.contains(i))
                .map(|(_, c)| c)
                .collect()
        }
    };
    if picked.is_empty() {
        return Err(CmdError::invalid(
            "/args",
            "the preview lists nothing to cut",
        ));
    }
    handle.call(CallRequest {
        command: "sweep_apply".into(),
        args: json!({"cut": rec.cut, "kind": rec.kind(), "revision": rec.revision, "cut_sig": rec.cut_sig,
                     "spans": picked.iter().map(|(m, f, t, _)| json!({"media": m, "from": f, "to": t})).collect::<Vec<_>>()}),
        base,
        request_id,
        caller,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SweepPreviewArgs {
    cut: String,
    kind: String,
    #[serde(default)]
    min_s: Option<f64>,
    #[serde(default)]
    keep_s: Option<f64>,
}

/// `sweep_preview{cut, kind, min_s?, keep_s?}`: the spans, and the id that applies them.
pub fn sweep_preview(
    holder: &Holder,
    p: &OpenProject,
    args: Value,
    binding: Option<Id<Cut>>,
) -> Result<Value, CmdError> {
    let a: SweepPreviewArgs = serde_json::from_value(args)
        .map_err(|_| CmdError::invalid("/args", "{cut, kind, min_s?, keep_s?}"))?;
    let kind = SweepKind::parse(&a.kind)
        .ok_or_else(|| CmdError::invalid("/args/kind", "pauses, fillers or retakes"))?;
    let cut = crate::reads::cut_id(&a.cut)?;
    crate::reads::check_cut_binding(binding, cut)?;
    let (min, keep) = sweep_params(kind, a.min_s, a.keep_s)?;
    let rec = sweep_preview_make(
        holder.analysis.as_ref(),
        &p.handle,
        &p.info.id,
        cut,
        kind,
        min,
        keep,
    )?;
    let listed: Vec<Value> = rec.cuts.iter().enumerate().map(|(i, (m, f, t, r))| json!({"i": i, "media": m, "from_s": secs(f.0), "to_s": secs(t.0), "seconds": secs(t.0 - f.0), "reason": canon::canonical(r, Kind::Text)})).collect();
    let v = json!({
        "preview_id": rec.id, "kind": rec.kind(), "cuts": listed, "skipped": rec.skipped,
        "removes_s": (rec.removes_s() * 1000.0).round() / 1000.0, "parts_without_audio": rec.unsupported,
        "revision": rec.revision, "as_of": rec.as_of,
        "display": format!("{}: {} span(s), {:.2} s, {} skipped; apply with sweep_apply{{preview_id:\"{}\"}} and base · as_of {}",
            rec.kind(), rec.cuts.len(), rec.removes_s(), rec.skipped.len(), rec.id, rec.as_of),
    });
    Ok(crate::reads::cap(v, crate::reads::PAGE_BYTES))
}

/// `sweep_apply{preview_id, only?}` from an agent or the CLI.
pub fn sweep_apply_preview(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: &Value,
    base: Option<Base>,
    request_id: Option<String>,
) -> Result<Value, CmdError> {
    let id = args
        .get("preview_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CmdError::invalid("/args/preview_id", "required"))?;
    let only: Option<Vec<usize>> = match args.get("only") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|_| CmdError::invalid("/args/only", "indices"))?,
        ),
    };
    if let (Some(c), Some(r)) = (
        args.get("cut").and_then(Value::as_str),
        sweep_preview_get(id),
    ) && crate::reads::cut_id(c)? != r.cut
    {
        return Err(CmdError::invalid("/args/cut", "not the preview's cut"));
    }
    let ok = sweep_preview_apply(
        holder.analysis.as_ref(),
        &p.handle,
        &p.info.id,
        id,
        only.as_deref(),
        conn.caller(),
        base,
        request_id,
    )?;
    if ok.committed.is_some()
        && let Ok(mut t) = conn.touched.lock()
    {
        t.insert(p.info.id.clone());
    }
    Ok(json!({
        "output": ok.output, "committed": ok.committed, "as_of": ok.as_of, "replayed": ok.replayed,
        "display": format!("sweep_apply: preview {id} applied; committed rev {:?} · as_of {}", ok.committed, ok.as_of),
    }))
}

/// `source_analyze{source, transcribe?}`.
pub fn source_analyze(
    holder: &Holder,
    conn: &Conn,
    p: &OpenProject,
    args: &Value,
) -> Result<Value, CmdError> {
    if !conn.scopes().contains(&Scope::Edit) {
        return Err(CmdError::Unauthorized {
            scope: "edit".into(),
        });
    }
    let doc = p.handle.snapshot().doc.clone();
    let s = args
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| CmdError::invalid("/args/source", "required"))?;
    let Owner::Source(sid) = Owner::parse(&doc, s)? else {
        return Err(CmdError::invalid(
            "/args/source",
            "a source id (a group is analysed through its sources)",
        ));
    };
    let force = args
        .get("transcribe")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if holder.analysis.name() == "null" {
        return Err(CmdError::Unsupported {
            platform: "holder".into(),
            reason: "this holder has no analysis".into(),
        });
    }
    let job = analyze(holder, p, sid, &conn.caller().identity, force)?;
    Ok(
        json!({"job": job.id, "state": "running", "display": format!("analysing the source as job {}; job_wait{{job:\"{}\"}} to wait", job.id, job.id)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_another_run_stored_first_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("gmclip-put-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let h = ProjectHandle::open(&dir.join("p.sqlite"), gmclip_core::OpenOptions::default())
            .unwrap();
        // the app's transcription stored this revision first
        h.put_analysis("src", "words", "r1", b"app's".to_vec())
            .unwrap();
        // the agent's job hears it slightly differently: its blob is dropped, not an error
        put_once(&h, "src", "words", "r1", b"job's".to_vec(), "put-words").unwrap();
        assert_eq!(h.analysis("src", "words", "r1").unwrap(), b"app's");
        put_once(&h, "src", "words", "r2", b"new".to_vec(), "put-words").unwrap();
        assert_eq!(h.analysis("src", "words", "r2").unwrap(), b"new");
        h.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
