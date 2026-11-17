//! The app takes a project over from the headless holder (A2.15; SPEC R-ARCH-7).
//!
//! Opening a project that `gmclip serve` holds no longer fails with "stop gmclip serve":
//! the app asks that holder to hand over (`Control::Handover`). It stops its jobs where
//! they are, closes every project it holds (snapshot rows written, locks released),
//! answers with them and stops. The app then:
//! - opens every one of those projects;
//! - resumes each transcription at its next window, under the holder's job id
//!   (`<app data>/handover/<job>.json`, chunked the same way, so the transcript is the
//!   one an uninterrupted run makes);
//! - lists the exports, which restart from frame 0 (SPEC §3.9);
//! - serves the gmclip socket itself.
//!
//! Agents' `gmclip mcp` shims reconnect to the app on their own (they replay their
//! handshake; a call in flight is answered "moved, ask again"; a watch from before is a
//! resync). The app never runs as a socket client of a holder except for this one request.
//!
//! Who it talks to: a server that is this app's own executable or the `gmclip` CLI
//! beside it, run by this user (the client's server check, R-ARCH-5a).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmclip_core::analysis::chunked::AsrPartial;
use gmclip_mcp::client::{self, ClientError};
use gmclip_mcp::holder::Control;
use gmclip_mcp::paths::{self, Env};
use gmclip_mcp::wire::Mode;
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};

/// What a takeover did.
#[derive(Debug, Default)]
pub struct Taken {
    pub projects: Vec<String>,
    /// `(project, source, job)` resumed here.
    pub resumed: Vec<(String, String, String)>,
    /// `(project, cut)` of exports the holder stopped (they restart from frame 0).
    pub exports: Vec<(String, String)>,
}

impl Taken {
    pub fn json(&self) -> Value {
        json!({
            "projects": self.projects,
            "resumed": self.resumed.iter().map(|(p, s, j)| json!({"project": p, "source": s, "job": j})).collect::<Vec<_>>(),
            "exports": self.exports.iter().map(|(p, c)| json!({"project": p, "cut": c})).collect::<Vec<_>>(),
        })
    }
}

fn servers() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = paths::our_exe().into_iter().collect();
    if let Some(dir) = v.first().and_then(|e| e.parent()).map(PathBuf::from) {
        v.push(dir.join("gmclip"));
    }
    v
}

async fn handover(env: &Env) -> Result<Value, ClientError> {
    let mut last = ClientError::NoHolder("no gmclip serve".into());
    for want in servers() {
        match client::connect_as(env, Mode::Control, false, None, Some(&want)).await {
            Ok((mut s, _)) => return client::request(&mut s, &Control::Handover).await,
            Err(e @ ClientError::NotTrusted(_)) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Takes every project of the headless holder over (see the module docs).
pub fn takeover(core: &Arc<AppCore>) -> AppResult<Taken> {
    let env = core
        .agents
        .env()
        .or_else(|| Env::from_env().ok())
        .ok_or_else(|| AppError::internal("no gmclip socket"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| AppError::internal("takeover runtime"))?;
    let r = rt.block_on(handover(&env)).map_err(|e| {
        AppError::new(
            "Leased",
            format!("gmclip serve did not hand the project over: {e}"),
        )
    })?;
    if r["ok"] != true {
        return Err(AppError::new(
            "Leased",
            "gmclip serve did not hand the project over; stop it and open the project again",
        ));
    }
    let r = &r["result"];
    // wait for it to stop serving (its socket, or Windows pipe, goes when it does)
    let t0 = Instant::now();
    while env.socket_present() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut taken = Taken::default();
    for p in r["projects"].as_array().into_iter().flatten() {
        let (Some(id), Some(home)) = (p["id"].as_str(), p["home"].as_str()) else {
            continue;
        };
        if core.open_home(std::path::Path::new(home)).is_ok() {
            taken.projects.push(id.to_owned());
        }
    }
    let dir = env.app_data.join("handover");
    for j in r["jobs"].as_array().into_iter().flatten() {
        let (Some(job), Some(kind)) = (j["job"].as_str(), j["kind"].as_str()) else {
            continue;
        };
        match kind {
            "analysis" => {
                // only a job id the holder named, as a file name it wrote
                if !job.chars().all(|c| c.is_ascii_alphanumeric()) || job.len() > 32 {
                    continue;
                }
                let file = dir.join(format!("{job}.json"));
                let body: Option<Value> = std::fs::read(&file)
                    .ok()
                    .filter(|b| b.len() < 64 << 20)
                    .and_then(|b| serde_json::from_slice(&b).ok());
                let _ = std::fs::remove_file(&file);
                let Some(body) = body else { continue };
                let (Some(project), Some(source)) =
                    (body["project"].as_str(), body["source"].as_str())
                else {
                    continue;
                };
                let Ok(partial) = serde_json::from_value::<AsrPartial>(body["partial"].clone())
                else {
                    continue;
                };
                if core.project(project).is_err() {
                    continue;
                }
                core.analysis.resume(core, project, source, job, partial);
                taken
                    .resumed
                    .push((project.to_owned(), source.to_owned(), job.to_owned()));
            }
            "export" => {
                let Some(cut) = j["cut"].as_str() else {
                    continue;
                };
                let owner = core.projects().into_iter().find(|p| {
                    gmclip_model::Ulid::parse(cut).ok().is_some_and(|u| {
                        p.handle
                            .snapshot()
                            .doc
                            .get(gmclip_model::Id::<gmclip_model::entities::Cut>::new(u))
                            .is_some()
                    })
                });
                if let Some(p) = owner {
                    taken.exports.push((p.info.id.clone(), cut.to_owned()));
                }
            }
            _ => {}
        }
    }
    // the rest of their analysis, as on any open
    for id in &taken.projects {
        core.analysis.ensure(core, id);
        core.proxies.ensure(core, id);
    }
    core.agents.serve_again(core);
    Ok(taken)
}
