//! The review link in the app (U.10; SPEC §5.14.1, SR R-REV-1..6): lane A's server
//! (`gmclip_mcp::review`) over the projects open in the app, and its controls.
//!
//! - **Serving** is a setting: turning it on or off goes through the native sheet. It
//!   binds `127.0.0.1` only (R-REV-1); tailnet mode puts `tailscale serve` in front and is
//!   chosen through the Owner sheet (`review_tailnet` / `review_loopback`).
//! - **Links** are made through the Owner sheet (`review_link_new`); the token is shown
//!   once, in the app, and never stored here. Revoking needs no sheet (it only takes
//!   access away).
//! - **What it serves** comes from this catalog: the open projects' exports (by export
//!   id, file and SHA-256 from their export records), cuts with their reviews, staged and
//!   posted moments. Reviews are written as `review-link:<id>` (System caller, R-REV-3);
//!   "fix with agent" requests go to Home's "needs you" with the note as untrusted text.

use std::io::Write;
use std::sync::{Arc, Mutex, Weak};

use gmclip_core::{CallRequest, Caller};
use gmclip_mcp::review::body::{CommentBody, ResolveBody, SuggestBody, Suggestion};
use gmclip_mcp::review::{
    self, ApproveDecision, ApproveRequest, Catalog, FileRec, FixDecision, FixRequest, Mode,
};
use gmclip_mcp::settings::OwnerSettings;
use gmclip_model::entities::{Cut, Moment, MomentState, Review};
use gmclip_proto::CallerKind;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// The default review port (`gmclip review serve` uses the same).
pub const DEFAULT_PORT: u16 = 8790;

/// The running server, if any.
#[derive(Default)]
pub struct ReviewState {
    server: Mutex<Option<review::Server>>,
    /// Where an approved export is handed to Clippy (default [`crate::board::CLIPPY_ENDPOINT`];
    /// tests point it at a stand-in; loopback only either way).
    clippy: Mutex<Option<String>>,
}

/// Points review-link approvals at another loopback Clippy (tests' stand-ins).
pub fn set_clippy_endpoint(core: &AppCore, endpoint: Option<String>) {
    if let Ok(mut g) = core.review.clippy.lock() {
        *g = endpoint;
    }
}

impl std::fmt::Debug for ReviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewState").finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The catalog over the app's open projects.
struct AppCatalog {
    core: Weak<AppCore>,
}

impl AppCatalog {
    fn exports(&self) -> Vec<(String, crate::core::ExportEntry, String)> {
        self.exports_in()
            .into_iter()
            .map(|(id, e, sha, _)| (id, e, sha))
            .collect()
    }

    /// Exports with their project: (catalog id, entry, sha256, project id).
    fn exports_in(&self) -> Vec<(String, crate::core::ExportEntry, String, String)> {
        let Some(core) = self.core.upgrade() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in core.projects() {
            for e in core.exports(&p.info.id).unwrap_or_default() {
                let Ok(rec) = gmclip_media::export_record::ExportRecord::read(&e.file) else {
                    continue;
                };
                if rec.sha256.len() != 64 || !rec.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                    continue;
                }
                out.push((
                    rec.sha256[..16].to_ascii_lowercase(),
                    e,
                    rec.sha256.clone(),
                    p.info.id.clone(),
                ));
            }
        }
        out
    }
}

impl Catalog for AppCatalog {
    fn home(&self) -> Value {
        let Some(core) = self.core.upgrade() else {
            return json!({"project": null, "exports": [], "cuts": [], "moments": []});
        };
        let projects = core.projects();
        let name = match projects.as_slice() {
            [one] => Some(one.info.name.clone()),
            _ => Some("gmclip".to_owned()),
        };
        let mut cuts = Vec::new();
        let mut moments = Vec::new();
        for p in &projects {
            let doc = p.handle.snapshot().doc.clone();
            for c in doc.iter::<Cut>() {
                let reviews: Vec<Value> = doc
                    .children::<Review>(c.id.entity())
                    .filter_map(|r| doc.get(r))
                    .map(|r| json!({"author": r.author, "verdict": format!("{:?}", r.verdict).to_lowercase(), "text": r.text}))
                    .collect();
                // decision 0015: threads and suggestions, the parts (for an editor's
                // trims), on the cut as it plays now
                let pl = core.planned(&p.info.id, &c.id.0.to_string()).ok();
                let fps = pl.as_ref().map_or(30.0, |pl| pl.plan.output.fps.as_f64());
                let parts: Vec<Value> = pl
                    .as_ref()
                    .map(|pl| {
                        pl.plan
                            .parts
                            .iter()
                            .zip(pl.index.parts.iter())
                            .enumerate()
                            .map(|(k, (pp, ix))| json!({"n": k + 1, "id": ix.model,
                                "start_s": pp.out.start() as f64 / fps, "end_s": pp.out.end() as f64 / fps}))
                            .collect()
                    })
                    .unwrap_or_default();
                let duration_s = pl
                    .as_ref()
                    .map_or(0.0, |pl| pl.plan.output.frames as f64 / fps);
                let comments = crate::collab::view_doc(&doc, pl.as_ref(), c.id);
                cuts.push(json!({"id": c.id.0.to_string(), "title": c.title, "notes": c.notes, "reviews": reviews,
                                 "duration_s": duration_s, "parts": parts,
                                 "threads": comments["threads"], "suggestions": comments["suggestions"]}));
            }
            let mut missing = Vec::new();
            for m in doc
                .iter::<Moment>()
                .filter(|m| matches!(m.state, MomentState::Staged | MomentState::Posted))
                .take(200)
            {
                let id = m.id.0.to_string();
                let clip = crate::clips::cached(&core, &p.info.id, &id).is_some();
                if !clip {
                    missing.push(id.clone());
                }
                moments.push(json!({"id": id, "quote": m.quote, "clip": clip}));
            }
            // H.10: the link's moments get clips in the background
            core.clips.queue(&core, &p.info.id, &missing);
        }
        let exports: Vec<Value> = self
            .exports()
            .into_iter()
            .map(|(id, e, _)| {
                json!({"id": id, "title": e.title, "duration_s": e.frames as f64 / e.rate(),
                       "blocked": false, "cut": e.cut, "at_ms": e.at_ms})
            })
            .collect();
        json!({"project": name, "exports": exports, "cuts": cuts, "moments": moments})
    }

    fn export(&self, id: &str) -> Option<FileRec> {
        let (_, e, sha) = self.exports().into_iter().find(|(i, _, _)| i == id)?;
        let size = std::fs::symlink_metadata(&e.file).ok()?.len();
        Some(FileRec {
            path: e.file,
            size,
            sha256: sha,
        })
    }

    /// A moment clip (H.10): only a current one (same transcript revision and span) of a
    /// moment in an open project; the server re-checks its size and SHA-256 (R-REV-2).
    fn clip(&self, id: &str) -> Option<FileRec> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let core = self.core.upgrade()?;
        let mid = crate::planner::ulid_id::<Moment>(id).ok()?;
        let p = core
            .projects()
            .into_iter()
            .find(|p| p.handle.snapshot().doc.get(mid).is_some())?;
        let (rec, path) = crate::clips::cached(&core, &p.info.id, id)?;
        Some(FileRec {
            path,
            size: rec.size,
            sha256: rec.sha256,
        })
    }

    fn add_review(
        &self,
        author: &str,
        cut: &str,
        text: &str,
        verdict: &str,
        export_ref: Option<&str>,
    ) -> Result<String, String> {
        let core = self.core.upgrade().ok_or("the app is closing")?;
        let cid = crate::planner::ulid_id::<Cut>(cut).map_err(|e| e.message)?;
        let p = core
            .projects()
            .into_iter()
            .find(|p| p.handle.snapshot().doc.get(cid).is_some())
            .ok_or("no such cut")?;
        let caller = Caller {
            identity: author.to_owned(),
            connection: format!("review:{author}"),
            kind: CallerKind::System,
            scopes: vec![gmclip_core::Scope::Admin],
            run: None,
            cut_binding: None,
        };
        let ok = p
            .handle
            .call(CallRequest {
                command: "review_add".into(),
                args: json!({"cut": cut, "text": text, "verdict": verdict, "export_ref": export_ref}),
                base: None,
                request_id: None,
                caller,
            })
            .map_err(|e| e.to_string())?;
        core.bus.project(
            &p.info.id,
            crate::events::ProjectEvent::Review {
                cut: cut.to_owned(),
            },
        );
        Ok(ok
            .output
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn fix(&self, req: &FixRequest) -> Result<Value, String> {
        let core = self.core.upgrade().ok_or("the app is closing")?;
        // decision 0015: the note becomes a comment thread (as the link), so the run has a
        // task to answer in; started → the comment-fix pass on it, else a Home ask
        if let Ok((project, comment)) = self.note_comment(&core, req) {
            let started = matches!(req.decision, FixDecision::Start { .. });
            let title = self.cut_title(&core, &project, &req.cut);
            if started {
                return crate::collab::fix(
                    &core,
                    &project,
                    &req.cut,
                    Some(vec![comment.clone()]),
                    None,
                )
                .map(|run| json!({"status": "started", "run": run["id"], "comment": comment}))
                .map_err(|e| e.message);
            }
            crate::collab::queue_ask(
                &core,
                json!({"ts": now_ms(), "kind": "fix_with_agent", "status": "needs_you", "project": project,
                       "cut": req.cut, "title": title, "comment": comment, "author": req.author,
                       "decision": req.decision, "note": {"untrusted": true, "text": req.note}}),
            )?;
            return Ok(json!({"status": "needs_you", "comment": comment}));
        }
        // M2: an allowed remote start is recorded for the launcher, a refused one waits
        // for a click in the app; the link-authored note is untrusted text (R-INJ-1)
        let status = match &req.decision {
            FixDecision::Start { .. } => "approved_remote",
            FixDecision::Queue { .. } => "needs_you",
        };
        let line = json!({
            "ts": now_ms(), "kind": "fix_with_agent", "status": status, "cut": req.cut,
            "author": req.author, "decision": req.decision,
            "note": {"untrusted": true, "text": req.note},
        });
        let f = core.dirs.app_data.join("needs-you.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&f)
            .map_err(|e| format!("needs-you ({})", e.kind()))?;
        writeln!(file, "{line}").map_err(|e| format!("needs-you ({})", e.kind()))?;
        core.bus.home(crate::events::HomeEvent::NeedsYou {
            cut: Some(req.cut.clone()),
        });
        Ok(json!({"status": status}))
    }

    fn comment(&self, author: &str, c: &CommentBody) -> Result<String, String> {
        let core = self.core.upgrade().ok_or("the app is closing")?;
        let p = self.project_of(&core, &c.cut)?;
        let mut args = json!({"cut": c.cut, "text": c.text});
        if let Some(r) = &c.reply_to {
            args["reply_to"] = json!(r);
        }
        if let Some(t) = c.t {
            let (at, _) = Self::src_at(&core, &p.info.id, &c.cut, t)?;
            args["at"] = serde_json::to_value(at).unwrap_or(Value::Null);
        }
        if let Some(b) = c.frame_box {
            args["frame_box"] = json!({"x": b.x, "y": b.y, "w": b.w, "h": b.h});
        }
        if let Some(e) = &c.export_ref {
            args["export_ref"] = json!(e);
        }
        let out = self.as_link(&core, author, &p.info.id, "comment_add", args)?;
        core.bus.project(
            &p.info.id,
            crate::events::ProjectEvent::Comments {
                cut: Some(c.cut.clone()),
            },
        );
        Ok(out["id"].as_str().unwrap_or_default().to_owned())
    }

    fn resolve(&self, author: &str, r: &ResolveBody) -> Result<(), String> {
        let core = self.core.upgrade().ok_or("the app is closing")?;
        let cid = crate::planner::ulid_id::<gmclip_model::entities::Comment>(&r.comment)
            .map_err(|e| e.message)?;
        let p = core
            .projects()
            .into_iter()
            .find(|p| p.handle.snapshot().doc.get(cid).is_some())
            .ok_or("no such comment")?;
        self.as_link(
            &core,
            author,
            &p.info.id,
            "comment_resolve",
            json!({"comment": r.comment, "resolved": r.resolved}),
        )?;
        core.bus.project(
            &p.info.id,
            crate::events::ProjectEvent::Comments { cut: None },
        );
        Ok(())
    }

    fn suggest(&self, author: &str, x: &SuggestBody) -> Result<String, String> {
        const F: f64 = 705_600_000.0;
        let core = self.core.upgrade().ok_or("the app is closing")?;
        let p = self.project_of(&core, &x.cut)?;
        let project = p.info.id.clone();
        let doc = p.handle.snapshot().doc.clone();
        let edit = match &x.edit {
            Suggestion::Trim {
                part,
                head_s,
                tail_s,
            } => {
                let pid = crate::planner::ulid_id::<gmclip_model::entities::Part>(part)
                    .map_err(|e| e.message)?;
                let mp = doc.get(pid).ok_or("no such part")?;
                let sp = mp.speed.max(1e-6);
                // whole output frames, so the edges stay on the frame grid
                let fps = core
                    .planned(&project, &x.cut)
                    .map(|pl| pl.plan.output.fps.as_f64())
                    .unwrap_or(30.0);
                let fpf = (F / fps).round() as i64;
                let frames = |s: f64| ((s * fps).round() as i64) * fpf;
                let i = mp.src_in.0 + (frames(*head_s) as f64 * sp).round() as i64;
                let o = mp.src_out.0 - (frames(*tail_s) as f64 * sp).round() as i64;
                json!({"op": "trim", "part": part, "src_in": i, "src_out": o})
            }
            Suggestion::Strike { from_s, to_s } => {
                let (a, pl) = Self::src_at(&core, &project, &x.cut, *from_s)?;
                // the end is inside the same part: the last frame before `to_s`
                let fps = pl.plan.output.fps.as_f64();
                let (b, _) =
                    Self::src_at(&core, &project, &x.cut, (to_s - 1.0 / fps).max(*from_s))?;
                if a.part != b.part || a.media != b.media {
                    return Err("a strike stays inside one part".into());
                }
                let end = b.at_src.0 + (F / fps).round() as i64;
                json!({"op": "strike", "media": a.media, "start": a.at_src.0, "end": end, "words": ""})
            }
            Suggestion::CaptionMove { y, t } => {
                let active = core.active_framing(&project, &x.cut);
                let cid = crate::planner::ulid_id::<Cut>(&x.cut).map_err(|e| e.message)?;
                let f = crate::collab::framing_of(&doc, cid, active.as_deref())
                    .ok_or("this cut has no framing")?;
                let part = match t {
                    Some(t) => Self::src_at(&core, &project, &x.cut, *t)?.0.part,
                    None => None,
                };
                let now = doc.get(f).and_then(|f| f.caption_place);
                let (px, pw) = now.map_or((0.5, 0.9), |pl| (pl.x, pl.w));
                json!({"op": "caption_move", "framing": f.0.to_string(), "part": part.map(|p| p.0.to_string()),
                       "place": {"x": px, "y": y, "w": pw}})
            }
        };
        let mut args = json!({"cut": x.cut, "edit": edit, "note": x.note});
        if let Some(c) = &x.comment {
            args["comment"] = json!(c);
        }
        let out = self.as_link(&core, author, &project, "edit_suggest", args)?;
        core.bus.project(
            &project,
            crate::events::ProjectEvent::Comments {
                cut: Some(x.cut.clone()),
            },
        );
        Ok(out["id"].as_str().unwrap_or_default().to_owned())
    }

    fn approve(&self, req: &ApproveRequest) -> Result<Value, String> {
        let core = self.core.upgrade().ok_or("the app is closing")?;
        let (_, e, _, project) = self
            .exports_in()
            .into_iter()
            .find(|(i, _, _, _)| *i == req.export)
            .ok_or("no such export")?;
        match &req.decision {
            ApproveDecision::Send => {
                let endpoint = core
                    .review
                    .clippy
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| crate::board::CLIPPY_ENDPOINT.to_owned());
                crate::board::send_approved(&core, &project, &e.id, &endpoint)
                    .map(|v| json!({"status": "sent", "clippy": v}))
                    .map_err(|e| e.message)
            }
            ApproveDecision::Queue { reasons } => {
                crate::collab::queue_ask(
                    &core,
                    json!({"ts": now_ms(), "kind": "approve", "status": "needs_you", "project": project,
                           "cut": e.cut, "entry": e.id, "export": req.export, "title": e.title,
                           "author": req.author, "reasons": reasons}),
                )?;
                Ok(json!({"status": "needs_you"}))
            }
        }
    }
}

impl AppCatalog {
    /// The project holding `cut`.
    fn project_of(
        &self,
        core: &AppCore,
        cut: &str,
    ) -> Result<std::sync::Arc<crate::core::OpenProject>, String> {
        let cid = crate::planner::ulid_id::<Cut>(cut).map_err(|e| e.message)?;
        core.projects()
            .into_iter()
            .find(|p| p.handle.snapshot().doc.get(cid).is_some())
            .ok_or_else(|| "no such cut".to_owned())
    }

    fn cut_title(&self, core: &AppCore, project: &str, cut: &str) -> String {
        core.project(project)
            .ok()
            .and_then(|p| {
                let cid = crate::planner::ulid_id::<Cut>(cut).ok()?;
                p.handle.snapshot().doc.get(cid).map(|c| c.title.clone())
            })
            .unwrap_or_default()
    }

    /// A write as the link (System caller, R-REV-3).
    fn as_link(
        &self,
        core: &AppCore,
        author: &str,
        project: &str,
        command: &str,
        args: Value,
    ) -> Result<Value, String> {
        let p = core.project(project).map_err(|e| e.message)?;
        let ok = p
            .handle
            .call(CallRequest {
                command: command.into(),
                args,
                base: None,
                request_id: None,
                caller: Caller {
                    identity: author.to_owned(),
                    connection: format!("review:{author}"),
                    kind: CallerKind::System,
                    scopes: vec![
                        gmclip_core::Scope::Read,
                        gmclip_core::Scope::Edit,
                        gmclip_core::Scope::Admin,
                    ],
                    run: None,
                    cut_binding: None,
                },
            })
            .map_err(|e| e.to_string())?;
        Ok(ok.output)
    }

    /// The link's "fix with agent" note as a comment thread on the cut.
    fn note_comment(&self, core: &AppCore, req: &FixRequest) -> Result<(String, String), String> {
        let p = self.project_of(core, &req.cut)?;
        let text = if req.note.trim().is_empty() {
            "Fix with agent (no note)".to_owned()
        } else {
            req.note.clone()
        };
        let out = self.as_link(
            core,
            &req.author,
            &p.info.id,
            "comment_add",
            json!({"cut": req.cut, "text": text}),
        )?;
        let id = out["id"].as_str().unwrap_or_default().to_owned();
        core.bus.project(
            &p.info.id,
            crate::events::ProjectEvent::Comments {
                cut: Some(req.cut.clone()),
            },
        );
        Ok((p.info.id.clone(), id))
    }

    fn src_at(
        core: &AppCore,
        project: &str,
        cut: &str,
        t: f64,
    ) -> Result<(gmclip_model::entities::SrcAt, crate::core::Planned), String> {
        let p = core.project(project).map_err(|e| e.message)?;
        let pl = core.planned(project, cut).map_err(|e| e.message)?;
        let fps = pl.plan.output.fps.as_f64();
        let f = ((t * fps).floor() as u64).min(pl.plan.output.frames.saturating_sub(1));
        let doc = p.handle.snapshot().doc.clone();
        let at = crate::collab::frame_to_src(&pl, &doc, f).ok_or("no part plays then")?;
        Ok((at, pl))
    }
}

struct TailscaleCli;

impl review::Tailnet for TailscaleCli {
    fn funnel_covers(&self, port: u16) -> Result<bool, String> {
        let out = std::process::Command::new("tailscale")
            .args(["funnel", "status", "--json"])
            .output()
            .map_err(|e| format!("tailscale: {}", e.kind()))?;
        if !out.status.success() {
            return Err("tailscale funnel status failed".into());
        }
        let v: Value =
            serde_json::from_slice(&out.stdout).map_err(|_| "tailscale: not JSON".to_owned())?;
        let funnel_on = v
            .get("AllowFunnel")
            .and_then(Value::as_object)
            .is_some_and(|m| m.values().any(|x| x.as_bool() == Some(true)));
        if !funnel_on {
            return Ok(false);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let ours = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        Ok(!text.contains("Proxy") || ours.iter().any(|o| text.contains(o.as_str())))
    }
}

fn mode_of(s: &OwnerSettings) -> Mode {
    match &s.review_tailnet {
        Some(t) => Mode::Tailnet {
            host: t.host.clone(),
            owner_login: t.owner_login.clone(),
        },
        None => Mode::Loopback,
    }
}

/// What the review-link panel shows.
pub fn status(core: &AppCore) -> Value {
    let s = OwnerSettings::load(&core.dirs.app_data);
    let addr = core.review.server.lock().ok().and_then(|g| {
        g.as_ref()
            .filter(|x| !x.stopped())
            .map(|x| x.addr.to_string())
    });
    let now = now_ms();
    let links: Vec<Value> = review::links::load(&core.dirs.app_data)
        .iter()
        .map(|l| {
            json!({"id": l.id, "created_ms": l.created_ms, "expires_ms": l.expires_ms,
                   "revoked": l.revoked, "live": !l.revoked && l.expires_ms > now, "login": l.login,
                   "role": l.role.as_deref().unwrap_or("reviewer")})
        })
        .collect();
    json!({
        "serving": addr,
        "mode": match &s.review_tailnet { Some(t) => json!({"tailnet": {"host": t.host, "owner_login": t.owner_login}}), None => json!("loopback") },
        "links": links,
        "remote_fix": s.remote_fix.as_ref().map(|r| json!({"max_usd_per_run": r.max_usd_per_run, "max_runs_per_day": r.max_runs_per_day})),
        "remote_send": s.remote_send.as_ref().map(|r| json!({"max_sends_per_day": r.max_sends_per_day})),
    })
}

/// Starts (`on`) or stops serving; a human's choice on the native sheet.
pub fn serve(core: &Arc<AppCore>, on: bool) -> AppResult<Value> {
    serve_on(core, on, DEFAULT_PORT)
}

/// [`serve`] on a given port (0 picks a free one: tests).
pub fn serve_on(core: &Arc<AppCore>, on: bool, port: u16) -> AppResult<Value> {
    let mut g = core
        .review
        .server
        .lock()
        .map_err(|_| AppError::internal("lock"))?;
    if !on {
        let s = g.take();
        drop(g);
        if let Some(s) = s {
            s.stop();
        }
        return Ok(status(core));
    }
    if g.as_ref().is_some_and(|s| !s.stopped()) {
        drop(g);
        return Ok(status(core));
    }
    let s = OwnerSettings::load(&core.dirs.app_data);
    let mode = mode_of(&s);
    let where_ = match &mode {
        Mode::Loopback => "this Mac only (127.0.0.1)".to_owned(),
        Mode::Tailnet { host, .. } => format!("your tailnet, at https://{host}"),
    };
    if !crate::sheet::confirm(
        "Serve review links?",
        &format!(
            "• Anyone holding a live review link can watch this library's exports and add review notes, from {where_}.\n• You can stop it at any time; links expire after 7 days."
        ),
        "Serve",
    ) {
        return Err(AppError::new(
            "Unauthorized",
            "not allowed (the Owner did not approve it on the sheet)",
        ));
    }
    start_locked(core, &mut g, port)?;
    drop(g);
    Ok(status(core))
}

/// Starts the server (the caller holds the state lock and has asked the human).
fn start_locked(core: &Arc<AppCore>, g: &mut Option<review::Server>, port: u16) -> AppResult<()> {
    let s = OwnerSettings::load(&core.dirs.app_data);
    let mode = mode_of(&s);
    let tailnet: Option<Arc<dyn review::Tailnet>> = match mode {
        Mode::Tailnet { .. } => Some(Arc::new(TailscaleCli)),
        Mode::Loopback => None,
    };
    let mut cfg = review::Config::new(core.dirs.app_data.clone(), mode);
    cfg.port = port;
    cfg.allowance = s.remote_fix.map(|r| review::Allowance {
        max_usd_per_run: r.max_usd_per_run,
        max_runs_per_day: r.max_runs_per_day,
    });
    let catalog = Arc::new(AppCatalog {
        core: Arc::downgrade(core),
    });
    let server = review::start(cfg, catalog, tailnet).map_err(AppError::invalid)?;
    *g = Some(server);
    Ok(())
}

/// Dev bridge builds only: serving on a free port and a link, without the sheets (the
/// review-link e2e in WebKit; R-DEV-1 keeps this out of release builds).
#[cfg(feature = "dev-bridge")]
pub fn dev_link(core: &Arc<AppCore>) -> AppResult<Value> {
    dev_link_as(core, None)
}

/// [`dev_link`] with a role (`editor`), dev builds only.
#[cfg(feature = "dev-bridge")]
pub fn dev_link_as(core: &Arc<AppCore>, role: Option<&str>) -> AppResult<Value> {
    {
        let mut g = core
            .review
            .server
            .lock()
            .map_err(|_| AppError::internal("lock"))?;
        if g.as_ref().is_none_or(|s| s.stopped()) {
            start_locked(core, &mut g, 0)?;
        }
    }
    let (link, token) = review::links::create_with_role(
        &core.dirs.app_data,
        now_ms(),
        None,
        role.map(str::to_owned),
    )
    .map_err(|e| AppError::internal(format!("review links ({})", e.kind())))?;
    let addr = status(core)["serving"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    Ok(json!({"id": link.id, "url": format!("http://{addr}/#t={token}")}))
}

/// A review as a link would write it (the catalog's path): dev-bridge e2e fixtures.
pub fn add_review_as(
    core: &Arc<AppCore>,
    link: &str,
    cut: &str,
    text: &str,
    verdict: &str,
    export_ref: Option<&str>,
) -> AppResult<Value> {
    let cat = AppCatalog {
        core: Arc::downgrade(core),
    };
    cat.add_review(
        &format!("review-link:{link}"),
        cut,
        text,
        verdict,
        export_ref,
    )
    .map(|id| json!({"id": id}))
    .map_err(AppError::invalid)
}

/// A comment as the review link `link` writes one from its page, on export `export_ref`
/// (its id on the link) when given (tests and the dev bridge; the server's own route is
/// [`review::Catalog::comment`]).
pub fn add_comment_as(
    core: &Arc<AppCore>,
    link: &str,
    cut: &str,
    text: &str,
    export_ref: Option<&str>,
) -> AppResult<Value> {
    let cat = AppCatalog {
        core: Arc::downgrade(core),
    };
    let body = gmclip_mcp::review::body::CommentBody {
        cut: cut.to_owned(),
        text: text.to_owned(),
        reply_to: None,
        t: None,
        frame_box: None,
        export_ref: export_ref.map(str::to_owned),
    };
    review::Catalog::comment(&cat, &format!("review-link:{link}"), &body)
        .map(|id| json!({"id": id}))
        .map_err(AppError::invalid)
}

/// A new link through the Owner sheet: the URL (with its token) is returned once.
pub fn link_new(core: &AppCore, login: Option<&str>) -> AppResult<Value> {
    link_new_as(core, login, None)
}

/// [`link_new`] with a role (`editor`: comments and suggested edits only, decision 0015).
pub fn link_new_as(core: &AppCore, login: Option<&str>, role: Option<&str>) -> AppResult<Value> {
    let mut args = match login {
        Some(l) if !l.is_empty() => json!({"login": l}),
        _ => json!({}),
    };
    if let Some(r) = role.filter(|r| !r.is_empty() && *r != "reviewer") {
        args["role"] = json!(r);
    }
    let v = crate::owner::request(core, "review_link_new", &args)?;
    let token = v["token"].as_str().unwrap_or_default();
    let s = OwnerSettings::load(&core.dirs.app_data);
    let port = core
        .review
        .server
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|x| x.addr.port()))
        .unwrap_or(DEFAULT_PORT);
    let url = match &s.review_tailnet {
        Some(t) => format!("https://{}/#t={token}", t.host),
        None => format!("http://127.0.0.1:{port}/#t={token}"),
    };
    Ok(
        json!({"id": v["id"], "expires_ms": v["expires_ms"], "url": url, "role": role.unwrap_or("reviewer")}),
    )
}

/// Revokes one link, or every link with `None` (taking access away needs no sheet).
pub fn revoke(core: &AppCore, id: Option<&str>) -> AppResult<Value> {
    let n = review::links::revoke(&core.dirs.app_data, id)
        .map_err(|e| AppError::internal(format!("review links ({})", e.kind())))?;
    Ok(json!({"revoked": n}))
}
