//! The stream edit in the app (decision 0011, SPEC §5.17): the UI addresses words by
//! ordinal in the transcript it shows, so labelling, splitting and edge drags arrive
//! here as ordinals and leave as the core's word fingerprints (`topic_set`,
//! `topic_split`); no time arithmetic happens in the UI (R-CMD-2).
//!
//! - `stream_overlay`: the transcript pane's topic bands, fluff proposals and coverage
//!   chips (which long-forms and shorts use which words), as word ordinal ranges.
//! - `project_map_view`: the core's `project_map` plus each cut's export state.
//! - `framing_pick`: which of a cut's framings the viewer shows and exports.

use gmclip_core::commands::topic::span_range;
use gmclip_model::Id;
use gmclip_model::entities::{Cut, CutKind, FluffProposal, MediaRef, Part, ProposalState, Topic};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::{FLICKS, ulid_id};
use crate::review::Transcript;

/// The word ordinal whose middle is the first at or after `t` (flicks).
fn first_word_at(t: &Transcript, at: i64) -> usize {
    t.words.partition_point(|w| (w.start + w.end) / 2 < at)
}

/// `[w0, w1]` (inclusive) of the words whose middles lie in `[a, b)`, if any.
fn words_in(t: &Transcript, a: i64, b: i64) -> Option<(usize, usize)> {
    let w0 = first_word_at(t, a);
    let w1 = first_word_at(t, b);
    (w1 > w0).then(|| (w0, w1 - 1))
}

/// Word `i` of a video's transcript (`owner`; the cut's first video's when `None`).
fn word(core: &AppCore, project: &str, cut: &str, i: u64, owner: Option<&str>) -> AppResult<Value> {
    let w = crate::words::refs_of(core, project, cut, i, i, owner)?
        .pop()
        .ok_or_else(|| AppError::invalid("a word outside the transcript"))?;
    serde_json::to_value(w).map_err(|_| AppError::internal("word"))
}

/// The video a topic is on, as an analysis owner id (its words' transcript).
fn topic_owner(core: &AppCore, project: &str, topic: &str) -> AppResult<String> {
    let doc = core.project(project)?.handle.snapshot().doc.clone();
    let t = doc
        .get(ulid_id::<Topic>(topic)?)
        .ok_or_else(|| AppError::not_found("no such topic"))?;
    Ok(crate::words::owner(&t.media))
}

/// `t`: labels the words `from..=to` of one video's transcript (`owner`, the one the
/// transcript pane shows; the stream edit may hold several, decision 0036) as a topic.
#[allow(clippy::too_many_arguments)]
pub fn label(
    core: &AppCore,
    project: &str,
    cut: &str,
    from: u64,
    to: u64,
    title: &str,
    colour: Option<&str>,
    owner: Option<&str>,
) -> AppResult<Value> {
    let (a, b) = (from.min(to), from.max(to));
    let mut args = json!({"cut": cut, "start": word(core, project, cut, a, owner)?,
                          "end": word(core, project, cut, b, owner)?, "title": title});
    if let Some(m) = owner.and_then(|o| {
        let doc = core.project(project).ok()?.handle.snapshot().doc.clone();
        gmclip_core::commands::topic::media_of_owner(&doc, o)
    }) {
        args["media"] = json!(m);
    }
    if let Some(c) = colour {
        args["colour"] = json!(c);
    }
    core.call(project, "topic_set", args)
}

/// A topic edge dragged to word `word` (`start`: it becomes the first word; `end`: the
/// last).
pub fn edge(
    core: &AppCore,
    project: &str,
    cut: &str,
    topic: &str,
    which: &str,
    i: u64,
) -> AppResult<Value> {
    let w = word(
        core,
        project,
        cut,
        i,
        Some(&topic_owner(core, project, topic)?),
    )?;
    let args = match which {
        "start" => json!({"topic": topic, "start": w}),
        "end" => json!({"topic": topic, "end": w}),
        _ => return Err(AppError::invalid("`edge` is start or end")),
    };
    core.call(project, "topic_set", args)
}

/// `⇧t`: splits the topic at word `at` (the caret): the word before ends the first half.
pub fn split(
    core: &AppCore,
    project: &str,
    cut: &str,
    topic: &str,
    at: u64,
    title: Option<&str>,
) -> AppResult<Value> {
    let o = topic_owner(core, project, topic)?;
    let mut args = json!({"topic": topic, "at": word(core, project, cut, at, Some(&o))?});
    if at > 0 {
        args["before"] = word(core, project, cut, at - 1, Some(&o))?;
    }
    if let Some(t) = title {
        args["title"] = json!(t);
    }
    core.call(project, "topic_split", args)
}

/// The stream view's overlay: topics, fluff proposals and coverage chips by ordinal,
/// each in its own video's transcript (`owner`; a cut may play several, decision 0036).
pub fn stream_overlay(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = ulid_id(cut)?;
    let c = doc
        .get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let stream = c.kind == CutKind::Stream;
    // each video the cut plays (or has topics on), with its transcript
    let videos: Vec<(
        MediaRef,
        String,
        String,
        std::sync::Arc<crate::review::Transcript>,
    )> = gmclip_core::commands::topic::stream_medias(&doc, cid)
        .into_iter()
        .filter_map(|m| {
            let o = crate::words::owner(&m);
            let (_, rev, t, _) = crate::words::transcript_for(core, &p, &doc, cid, Some(&o))?;
            Some((m, o, rev, t))
        })
        .collect();
    let Some(rev) = videos.first().map(|v| v.2.clone()) else {
        return Ok(json!({"stream": stream, "topics": [], "fluff": [], "coverage": []}));
    };
    let of = |m: &MediaRef| videos.iter().find(|v| v.0 == *m);
    let ms = |f: i64| f * 1000 / FLICKS;
    // where a source range of a video plays in the cut, as output frames (the topic lane)
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    let out_of = |m: &MediaRef, a: i64, b: i64| -> Vec<[i64; 2]> {
        layout
            .iter()
            .filter_map(|sp| {
                let p = doc.get(sp.part)?;
                if p.media != *m {
                    return None;
                }
                let (s, e) = (a.max(p.src_in.0), b.min(p.src_out.0));
                (e > s).then(|| {
                    let o = sp.start.0 + (s - p.src_in.0);
                    [o / fpf, (o + (e - s)) / fpf]
                })
            })
            .collect()
    };
    let frames: i64 = layout.iter().map(|sp| sp.dur.0).sum::<i64>() / fpf;
    let mut topics = Vec::new();
    let mut fluff = Vec::new();
    // topics live on the stream edit; any other cut shows them on the words it plays
    // from the same videos, read-only (AJ 2026-11-02: "the transcript ... coloured based
    // on topic inside full talk")
    let topic_cut = if stream {
        Some(cid)
    } else {
        gmclip_core::commands::topic::stream_edit(&doc)
    };
    if let Some(tc) = topic_cut {
        for tid in doc.children::<Topic>(tc.entity()) {
            let Some(x) = doc.get(tid) else { continue };
            if x.state == ProposalState::Rejected {
                continue;
            }
            let Some((_, o, rev, t)) = of(&x.media) else {
                continue;
            };
            let (a, b) = span_range(&x.span);
            let Some((w0, w1)) = words_in(t, a, b) else {
                continue;
            };
            topics.push(json!({
                "id": tid.0.to_string(), "title": x.title, "colour": x.colour,
                "state": x.state, "origin": x.origin, "conflict": x.conflict,
                "replaces": x.replaces.iter().map(|r| r.0.to_string()).collect::<Vec<_>>(),
                "owner": o, "revision": rev,
                "w0": w0, "w1": w1, "from_ms": ms(a), "to_ms": ms(b), "out": out_of(&x.media, a, b),
                "from_stream": !stream,
            }));
        }
    }
    if stream {
        for fid in doc.children::<FluffProposal>(cid.entity()) {
            let Some(f) = doc.get(fid) else { continue };
            if f.state != ProposalState::Proposed {
                continue;
            }
            let Some((_, o, _, t)) = of(&f.media) else {
                continue;
            };
            let (a, b) = span_range(&f.span);
            let Some((w0, w1)) = words_in(t, a, b) else {
                continue;
            };
            fluff.push(json!({
                "id": fid.0.to_string(), "reason": f.reason, "conflict": f.conflict,
                "origin": f.origin, "owner": o, "w0": w0, "w1": w1, "from_ms": ms(a), "to_ms": ms(b),
            }));
        }
    }
    // coverage: every other cut's parts on this cut's videos, as word ranges
    let mut coverage = Vec::new();
    for other in doc.iter::<Cut>() {
        if other.id == cid {
            continue;
        }
        for pid in doc.children::<Part>(other.id.entity()) {
            let Some(pt) = doc.get(pid) else { continue };
            let Some((_, o, _, t)) = of(&pt.media) else {
                continue;
            };
            if let Some((w0, w1)) = words_in(t, pt.src_in.0, pt.src_out.0) {
                coverage.push(json!({"cut": other.id.0.to_string(), "title": other.title,
                                     "kind": other.kind.name(), "owner": o, "w0": w0, "w1": w1}));
            }
        }
    }
    Ok(
        json!({"stream": stream, "revision": rev, "frames": frames, "topics": topics, "fluff": fluff, "coverage": coverage}),
    )
}

/// One cut's export state without its view: `never`, `fresh` or `stale`.
fn export_state(core: &AppCore, project: &str, cut: &str) -> Value {
    let Ok(p) = core.project(project) else {
        return json!({"state": "never"});
    };
    let last = p.exports.lock().ok().and_then(|g| {
        g.iter()
            .filter(|e| e.cut == cut)
            .max_by_key(|e| e.at_ms)
            .cloned()
    });
    let Some(e) = last else {
        return json!({"state": "never"});
    };
    let Ok(pl) = core.planned(project, cut) else {
        return json!({"state": "stale", "at_ms": e.at_ms});
    };
    let fresh = (e.render_sig.is_some() && e.render_sig == crate::core::sig_of(&pl.plan))
        || (e.render_sig.is_none()
            && e.plan_sig.is_some()
            && e.plan_sig == crate::core::plan_sig(&pl.plan));
    json!({"state": if fresh { "fresh" } else { "stale" }, "at_ms": e.at_ms})
}

/// The project map (core `project_map`) with each cut's export state.
pub fn project_map_view(core: &AppCore, project: &str) -> AppResult<Value> {
    let mut m = core.call(project, "project_map", json!({}))?["output"].take();
    for list in ["longs", "shorts"] {
        if let Some(rows) = m[list].as_array_mut() {
            for r in rows {
                let cut = r["cut"].as_str().unwrap_or_default().to_owned();
                r["export"] = export_state(core, project, &cut);
            }
        }
    }
    if let Some(cut) = m["stream"]["cut"].as_str().map(str::to_owned) {
        m["stream"]["export"] = export_state(core, project, &cut);
    }
    // each video: is its file there, how far its transcription is (its analysis owner:
    // the source, or its same-recording group), and its scrub proxy
    let asr = crate::analysis::status(core, project);
    let proxies = core.proxies.status(Some(project));
    if let Some(rows) = m["sources"].as_array_mut() {
        for r in rows {
            let path = r["path"].as_str().unwrap_or_default().to_owned();
            r["offline"] = json!(!std::path::Path::new(&path).is_file());
            let owner = r["media"]["group"]
                .as_str()
                .or(r["media"]["source"].as_str())
                .unwrap_or_default()
                .to_owned();
            r["owner"] = json!(owner);
            r["asr"] = asr["sources"]
                .as_array()
                .and_then(|v| v.iter().find(|x| x["source"] == owner.as_str()))
                .cloned()
                .unwrap_or(Value::Null);
            r["proxy"] = proxies["jobs"]
                .as_array()
                .and_then(|v| v.iter().find(|j| j["path"] == path.as_str()))
                .map_or(Value::Null, |j| j["phase"].clone());
        }
    }
    Ok(m)
}

/// Which framing of a cut the viewer shows and exports ("screen focus" / "face big").
pub fn framing_pick(core: &AppCore, project: &str, cut: &str, framing: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let fid: Id<gmclip_model::entities::Framing> = ulid_id(framing)?;
    let f = doc
        .get(fid)
        .ok_or_else(|| AppError::not_found("no such framing"))?;
    if f.cut.0.to_string() != cut {
        return Err(AppError::invalid("a framing of that cut"));
    }
    core.set_active_framing(project, cut, framing)?;
    Ok(json!({"framing": framing}))
}
