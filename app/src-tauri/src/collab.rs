//! Comments, suggested edits and their agent fixes in the app (decision 0015; backlog
//! V.01, V.02, V.04).
//!
//! - **Comments** are pinned to the source the cut plays at an output frame (core
//!   `comment_add`, `SrcAt`), so they follow the picture through edits; the view gives
//!   each thread's output frame on the cut as it plays now (or none when it is cut out),
//!   for the timeline markers and the viewer pins.
//! - **Suggestions** from an editor's review link are listed with what they would do;
//!   the human accepts or rejects them one by one (core `edit_proposal_state`).
//! - **Fix with my agent** turns one comment, or every open one, into tasks in a brief
//!   (`gmclip_mcp::collab::fix_brief`: text inside `<untrusted>`), launched through the
//!   launcher on this click; the run edits the cut and replies in each thread. When the
//!   run ends without replying to a task, the app posts its summary there as the run's
//!   (untrusted) words, so every task has an answer.
//! - **Review-link asks** ("fix with agent", "approve") that did not meet R-REV-4 wait
//!   here as Home → Needs you items until the human starts or dismisses them.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use gmclip_core::{CallRequest, Caller};
use gmclip_model::Doc;
use gmclip_model::entities::{
    Comment, Cut, EditProposal, Framing, Part, ProposalState, SrcAt, SuggestedEdit,
};
use gmclip_model::time::FLICKS_PER_SECOND;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CallerKind;
use serde_json::{Value, json};

use crate::core::{AppCore, Planned, now_ms};
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// Flicks per output frame of a plan.
fn fpf(pl: &Planned) -> i64 {
    crate::planner::fpf(&pl.plan)
}

/// The output frame each model part starts at, in plan order (a part split into several
/// plan parts by visual splits starts at its first).
fn starts(pl: &Planned) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = Vec::new();
    for (pp, ix) in pl.plan.parts.iter().zip(pl.index.parts.iter()) {
        if !v.iter().any(|(m, _)| *m == ix.model) {
            v.push((ix.model.clone(), pp.out.start()));
        }
    }
    v
}

/// The source point the cut shows at output frame `f`.
pub fn frame_to_src(pl: &Planned, doc: &Doc, f: u64) -> Option<SrcAt> {
    let k = pl
        .plan
        .parts
        .iter()
        .position(|p| p.out.start() <= f && f < p.out.end())?;
    let model = &pl.index.parts.get(k)?.model;
    let (_, a0) = starts(pl).into_iter().find(|(m, _)| m == model)?;
    let pid = ulid_id::<Part>(model).ok()?;
    let p = doc.get(pid)?;
    let off =
        gmclip_model::clock::local_to_src(p, Flicks((f - a0) as i64 * fpf(pl))).0 - p.src_in.0;
    Some(SrcAt {
        media: p.media,
        at_src: Flicks((p.src_in.0 + off).min(p.src_out.0 - 1)),
        part: Some(pid),
    })
}

/// The output frame where the cut shows `at` now (its hinted part first), if it does.
pub fn src_to_frame(pl: &Planned, doc: &Doc, at: &SrcAt) -> Option<u64> {
    let mut found = None;
    for (model, a0) in starts(pl) {
        let Ok(pid) = ulid_id::<Part>(&model) else {
            continue;
        };
        let Some(p) = doc.get(pid) else { continue };
        if p.media == at.media && p.src_in <= at.at_src && at.at_src < p.src_out {
            let local = gmclip_model::clock::src_to_local(p, Flicks(at.at_src.0 - p.src_in.0)).0;
            let f = a0 + (local / fpf(pl).max(1)).max(0) as u64;
            if at.part == Some(pid) {
                return Some(f);
            }
            found = found.or(Some(f));
        }
    }
    found
}

fn human_text(author: &str) -> bool {
    author == "human"
}

/// The threads and suggestions of a cut, as the comments panel, the timeline markers, the
/// viewer pins and the review link show them. Text from anyone but the human is marked
/// untrusted (shown as text only).
pub fn view_doc(doc: &Doc, pl: Option<&Planned>, cid: Id<Cut>) -> Value {
    let mut all: Vec<&Comment> = doc
        .children::<Comment>(cid.entity())
        .filter_map(|c| doc.get(c))
        .collect();
    all.sort_by_key(|c| (c.at_ms, c.id.0));
    let fps = pl.map_or(30.0, |p| p.plan.output.fps.as_f64());
    let threads: Vec<Value> = all
        .iter()
        .filter(|c| c.reply_to.is_none())
        .map(|c| {
            let frame = c
                .at
                .as_ref()
                .and_then(|at| pl.and_then(|pl| src_to_frame(pl, doc, at)));
            let replies: Vec<Value> = all
                .iter()
                .filter(|r| r.reply_to == Some(c.id))
                .map(|r| json!({"id": r.id.0.to_string(), "author": r.author, "human": human_text(&r.author),
                                "text": r.text, "at_ms": r.at_ms}))
                .collect();
            json!({
                "id": c.id.0.to_string(),
                "frame": frame,
                "t": frame.map(|f| f as f64 / fps),
                "pinned": c.at.is_some(),
                "box": c.frame_box.map(|b| json!({"x": b.x, "y": b.y, "w": b.w, "h": b.h})),
                "author": c.author, "human": human_text(&c.author), "text": c.text,
                "resolved": c.resolved, "at_ms": c.at_ms, "export_ref": c.export_ref,
                "replies": replies,
            })
        })
        .collect();
    let mut sug: Vec<&EditProposal> = doc
        .children::<EditProposal>(cid.entity())
        .filter_map(|p| doc.get(p))
        .collect();
    sug.sort_by_key(|p| (p.at_ms, p.id.0));
    let suggestions: Vec<Value> = sug
        .iter()
        .map(|p| {
            let (kind, summary, frame) = summary(doc, pl, p);
            json!({"id": p.id.0.to_string(), "kind": kind, "summary": summary, "frame": frame,
                   "author": p.author, "note": p.note, "at_ms": p.at_ms,
                   "state": match p.state { ProposalState::Proposed => "proposed", ProposalState::Accepted => "accepted", ProposalState::Rejected => "rejected" },
                   "comment": p.comment.map(|c| c.0.to_string())})
        })
        .collect();
    let open = threads.iter().filter(|t| t["resolved"] == false).count();
    json!({"threads": threads, "suggestions": suggestions, "open": open, "fps": fps})
}

/// A suggestion in words: its kind, what it does to the cut as it is now, and the output
/// frame it is about.
fn summary(
    doc: &Doc,
    pl: Option<&Planned>,
    p: &EditProposal,
) -> (&'static str, String, Option<u64>) {
    let s = |f: Flicks| f.0 as f64 / FLICKS_PER_SECOND as f64;
    let part_n = |id: Id<Part>| -> Option<usize> {
        doc.children::<Part>(p.cut.entity())
            .position(|x| x == id)
            .map(|n| n + 1)
    };
    match &p.edit {
        SuggestedEdit::Trim {
            part,
            src_in,
            src_out,
        } => {
            let (n, now) = match (part_n(*part), doc.get(*part)) {
                (Some(n), Some(x)) => (n.to_string(), Some((x.src_in, x.src_out))),
                _ => ("(gone)".into(), None),
            };
            let what = match now {
                Some((i, o)) => {
                    let (dh, dt) = (s(*src_in) - s(i), s(o) - s(*src_out));
                    let edge = |d: f64, at: &str| {
                        if d.abs() < 0.0005 {
                            None
                        } else if d > 0.0 {
                            Some(format!("{d:.2} s off its {at}"))
                        } else {
                            Some(format!("{:.2} s more at its {at}", -d))
                        }
                    };
                    [edge(dh, "start"), edge(dt, "end")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                None => "a part that is gone".into(),
            };
            let frame = pl.and_then(|pl| {
                src_to_frame(
                    pl,
                    doc,
                    &SrcAt {
                        media: doc.get(*part).map(|x| x.media)?,
                        at_src: doc.get(*part).map(|x| x.src_in)?,
                        part: Some(*part),
                    },
                )
            });
            ("trim", format!("Trim part {n}: {what}"), frame)
        }
        SuggestedEdit::Strike {
            media,
            start,
            end,
            words,
        } => {
            let frame = pl.and_then(|pl| {
                src_to_frame(
                    pl,
                    doc,
                    &SrcAt {
                        media: *media,
                        at_src: *start,
                        part: None,
                    },
                )
            });
            let len = s(*end) - s(*start);
            let w = if words.trim().is_empty() {
                String::new()
            } else {
                format!(" (“{}”)", words.chars().take(80).collect::<String>())
            };
            ("strike", format!("Strike {len:.2} s{w}"), frame)
        }
        SuggestedEdit::CaptionMove { part, place, .. } => {
            let whose = match part.and_then(part_n) {
                Some(n) => format!("part {n}'s captions"),
                None if part.is_some() => "a gone part's captions".into(),
                None => "the captions".into(),
            };
            (
                "caption_move",
                format!(
                    "Move {whose} to {:.0}% of the frame's height",
                    place.y * 100.0
                ),
                None,
            )
        }
    }
}

/// The comments panel for a cut.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let pl = core.planned(project, cut).ok();
    let mut v = view_doc(&doc, pl.as_ref(), cid);
    v["agent_runs"] = json!(runs_for(project, cut));
    Ok(v)
}

/// A new thread at output frame `frame` (optionally with a box), or a reply, as the human.
pub fn add_at(
    core: &AppCore,
    project: &str,
    cut: &str,
    frame: Option<u64>,
    frame_box: Option<&Value>,
    text: &str,
    reply_to: Option<&str>,
) -> AppResult<Value> {
    let mut args = json!({"cut": cut, "text": text});
    if let Some(r) = reply_to {
        args["reply_to"] = json!(r);
    } else {
        if let Some(f) = frame {
            let p = core.project(project)?;
            let doc = p.handle.snapshot().doc.clone();
            let pl = core.planned(project, cut)?;
            let at = frame_to_src(&pl, &doc, f)
                .ok_or_else(|| AppError::invalid("no part plays at that frame"))?;
            args["at"] = serde_json::to_value(at).unwrap_or(Value::Null);
        }
        if let Some(b) = frame_box.filter(|b| !b.is_null()) {
            args["frame_box"] = b.clone();
        }
    }
    let out = core.call(project, "comment_add", args)?;
    core.bus.project(
        project,
        crate::events::ProjectEvent::Comments {
            cut: Some(cut.to_owned()),
        },
    );
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Fix with my agent
// ---------------------------------------------------------------------------------------

/// Launched fix runs: app run id → (project, cut, the comments it was given).
/// A launched fix run: (project, cut, the comments it was given).
type FixTasks = (String, String, Vec<String>);

static TASKS: Mutex<BTreeMap<String, FixTasks>> = Mutex::new(BTreeMap::new());

fn runs_for(project: &str, cut: &str) -> Vec<Value> {
    TASKS
        .lock()
        .map(|g| {
            g.iter()
                .filter(|(_, (p, c, _))| p == project && c == cut)
                .map(|(r, (_, _, cs))| json!({"run": r, "comments": cs}))
                .collect()
        })
        .unwrap_or_default()
}

/// "Fix with my agent" on one comment (`only`), or "Fix all open comments": one launched
/// run on this click, every comment a task in its brief.
pub fn fix(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    only: Option<Vec<String>>,
    harness: Option<&str>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let ids: Vec<Id<Comment>> = match only {
        Some(v) => v
            .iter()
            .map(|s| ulid_id::<Comment>(s))
            .collect::<AppResult<_>>()?,
        None => gmclip_mcp::collab::open_threads(&doc, cid),
    };
    let brief = gmclip_mcp::collab::fix_brief(&doc, cid, &ids).map_err(AppError::invalid)?;
    let h = match harness {
        Some(h) => Some(
            gmclip_ai::launcher::Harness::parse(h)
                .ok_or_else(|| AppError::invalid("claude-code, codex, pi or opencode"))?,
        ),
        None => None,
    };
    let run = crate::agent::run_composed(core, project, cut, "fix_comments", brief, h)?;
    if let Some(id) = run["id"].as_str()
        && let Ok(mut g) = TASKS.lock()
    {
        g.insert(
            id.to_owned(),
            (
                project.to_owned(),
                cut.to_owned(),
                ids.iter().map(|i| i.0.to_string()).collect(),
            ),
        );
    }
    Ok(run)
}

/// A fix run ended: every task it did not reply to gets the run's summary (or a note that
/// it ended without one), as the run's words — untrusted, never `human`.
pub fn run_ended(core: &Arc<AppCore>, run: &str) {
    let Some((project, cut, comments)) = TASKS.lock().ok().and_then(|mut g| g.remove(run)) else {
        return;
    };
    let Some(view) = core.agents.run(run) else {
        return;
    };
    let Some(launched) = view.launched().map(str::to_owned) else {
        return;
    };
    let Ok(p) = core.project(&project) else {
        return;
    };
    let doc = p.handle.snapshot().doc.clone();
    let author = format!("launched:{launched}");
    let text = match (&view.summary, view.state.as_str()) {
        (Some(s), _) if !s.trim().is_empty() => {
            format!(
                "(The run ended without a reply here; its summary:) {}",
                s.chars().take(1500).collect::<String>()
            )
        }
        (_, "failed") => format!(
            "(The run failed: {}.)",
            view.error.as_deref().unwrap_or("no reason given")
        ),
        _ => "(The run ended without a reply here.)".into(),
    };
    for c in comments {
        let Ok(cid) = ulid_id::<Comment>(&c) else {
            continue;
        };
        let replied = doc
            .iter::<Comment>()
            .any(|r| r.reply_to == Some(cid) && r.author == author);
        if replied || doc.get(cid).is_none() {
            continue;
        }
        let _ = p.handle.call(CallRequest {
            command: "comment_add".into(),
            args: json!({"cut": cut, "reply_to": c, "text": text}),
            base: None,
            request_id: None,
            caller: Caller {
                identity: author.clone(),
                connection: format!("app:{run}"),
                kind: CallerKind::System,
                scopes: vec![gmclip_core::Scope::Edit],
                run: None,
                cut_binding: None,
            },
        });
    }
    core.bus.project(
        &project,
        crate::events::ProjectEvent::Comments {
            cut: Some(cut.to_owned()),
        },
    );
}

// ---------------------------------------------------------------------------------------
// Review-link asks waiting for a click (Home → Needs you)
// ---------------------------------------------------------------------------------------

const ASKS: &str = "needs-you.jsonl";
const HANDLED: &str = "needs-you-handled.json";

/// Appends a review-link ask (its text, if any, is the link's: untrusted).
pub fn queue_ask(core: &AppCore, ask: Value) -> Result<(), String> {
    let f = core.dirs.app_data.join(ASKS);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&f)
        .map_err(|e| format!("needs-you ({})", e.kind()))?;
    writeln!(file, "{ask}").map_err(|e| format!("needs-you ({})", e.kind()))?;
    core.bus
        .home(crate::events::HomeEvent::NeedsYou { cut: None });
    Ok(())
}

fn handled(core: &AppCore) -> Vec<String> {
    std::fs::read(core.dirs.app_data.join(HANDLED))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn ask_id(v: &Value) -> String {
    format!(
        "{}-{}",
        v["ts"].as_i64().unwrap_or(0),
        v["kind"].as_str().unwrap_or("ask")
    )
}

/// The review-link asks still waiting (queued, not started or dismissed), newest first,
/// for Home's "Needs you" (≤ 50).
pub fn asks(core: &AppCore) -> Vec<Value> {
    let done = handled(core);
    let text = std::fs::read_to_string(core.dirs.app_data.join(ASKS)).unwrap_or_default();
    let mut out: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["status"] == "needs_you")
        .filter(|v| !done.contains(&ask_id(v)))
        .map(|v| {
            let id = ask_id(&v);
            let (title, why, action) = match v["kind"].as_str() {
                Some("approve") => (
                    v["title"].as_str().unwrap_or("an export").to_owned(),
                    "approved on a review link: send to Clippy's calendar?".to_owned(),
                    "remote_send",
                ),
                _ => (
                    v["title"].as_str().unwrap_or("a cut").to_owned(),
                    "a review link asks for an agent fix".to_owned(),
                    "remote_fix",
                ),
            };
            json!({"project": v["project"], "cut": v["cut"], "title": title, "why": why, "action": action,
                   "ask": id, "author": v["author"], "note": v["note"]})
        })
        .collect();
    out.reverse();
    out.truncate(50);
    out
}

/// Starts (`start`) or dismisses one waiting ask. Starting is the in-app click: a fix runs
/// the comment-fix pass on the link's comment; an approve is Send to Clippy (its own
/// native sheet).
pub fn ask_act(core: &Arc<AppCore>, ask: &str, start: bool) -> AppResult<Value> {
    let text = std::fs::read_to_string(core.dirs.app_data.join(ASKS)).unwrap_or_default();
    let v = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| ask_id(v) == ask && v["status"] == "needs_you")
        .ok_or_else(|| AppError::not_found("no such request"))?;
    let mut out = json!({"ask": ask, "started": false});
    if start {
        let project = v["project"].as_str().unwrap_or_default();
        out = match v["kind"].as_str() {
            Some("approve") => {
                crate::board::send(core, project, v["entry"].as_str().unwrap_or_default())?
            }
            _ => fix(
                core,
                project,
                v["cut"].as_str().unwrap_or_default(),
                v["comment"].as_str().map(|c| vec![c.to_owned()]),
                None,
            )?,
        };
    }
    let mut done = handled(core);
    done.push(ask.to_owned());
    if done.len() > 2000 {
        let n = done.len() - 2000;
        done.drain(..n);
    }
    crate::core::write_atomic(
        &core.dirs.app_data.join(HANDLED),
        &serde_json::to_vec(&done).unwrap_or_default(),
    )
    .map_err(|e| AppError::internal(format!("needs-you ({})", e.kind())))?;
    core.bus
        .home(crate::events::HomeEvent::NeedsYou { cut: None });
    Ok(out)
}

/// Makes sure a framing id belongs to the cut (the review link's caption moves).
pub fn framing_of(doc: &Doc, cid: Id<Cut>, active: Option<&str>) -> Option<Id<Framing>> {
    let want = active.and_then(|a| ulid_id::<Framing>(a).ok());
    doc.children::<Framing>(cid.entity())
        .find(|f| want.is_none_or(|w| *f == w))
        .or_else(|| doc.children::<Framing>(cid.entity()).next())
}

#[allow(dead_code)]
fn _now() -> i64 {
    now_ms()
}
