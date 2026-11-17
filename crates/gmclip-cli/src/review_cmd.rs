//! `gmclip review …` (SPEC §5.14.1): review links and the review-link server for one
//! project, and `gmclip share` lives in [`crate::share_cmd`].
//!
//! Links are made and revoked by the human at a terminal (they expose exports); the
//! token is printed once, on the terminal (stderr), never on stdout and never stored.
//! `serve` runs `gmclip_mcp::review` with a catalog over the project (through its
//! actor), an export folder (`*.export.json` records beside their MP4s) and an optional
//! moment-clip folder (`<moment id>.mp4` + `<moment id>.clip.json`).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Subcommand;
use gmclip_core::{CallRequest, Caller, OpenOptions, ProjectHandle};
use gmclip_mcp::review::{self, Catalog, FileRec, FixDecision, FixRequest, Mode};
use gmclip_mcp::settings::OwnerSettings;
use gmclip_model::entities::{Cut, Moment, MomentState, Review};
use gmclip_proto::CallerKind;
use serde_json::{Value, json};

use crate::Output;
use crate::error::{CliError, Code};
use crate::socket;

/// The default review port.
pub const DEFAULT_PORT: u16 = 8790;

#[derive(Debug, Subcommand)]
pub enum ReviewCmd {
    /// Review links (needs a terminal: the token is shown there once).
    Link {
        #[command(subcommand)]
        command: LinkCmd,
    },
    /// Serve one project's review board, its exports and moment clips.
    Serve {
        /// The project database (`<id>.sqlite`).
        #[arg(long)]
        project: PathBuf,
        /// The export folder (`*.export.json` beside each MP4).
        #[arg(long)]
        exports: PathBuf,
        /// Moment clips (`<moment id>.mp4` + `.clip.json`).
        #[arg(long)]
        clips: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
}

#[derive(Debug, Subcommand)]
pub enum LinkCmd {
    /// Make a link (7 days). In tailnet mode it admits the Owner's login unless
    /// `--login` names another.
    New {
        #[arg(long)]
        login: Option<String>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// List links (ids, expiry, revoked).
    List,
    /// Revoke one link.
    Revoke { id: String },
    /// Revoke every link.
    RevokeAll,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn bad(step: &str, e: impl std::fmt::Display) -> CliError {
    CliError::new(Code::Invalid)
        .step(step)
        .reason(e.to_string())
}

fn human_terminal() -> Result<(), CliError> {
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(CliError::new(Code::Unauthorized)
            .step("review")
            .reason("review links are made at an interactive terminal (they expose your exports)"))
    }
}

pub fn run(cmd: ReviewCmd) -> Result<Output, CliError> {
    let env = socket::env()?;
    let app = env.app_data;
    match cmd {
        ReviewCmd::Link { command } => match command {
            LinkCmd::New { login, port } => {
                // a link exposes exports: an Owner action (code on the terminal, R-OWN-3)
                let settings = OwnerSettings::load(&app);
                let Output::Summary(v) = socket::owner_described(
                    "review_link_new",
                    json!({"login": login}),
                    "make a review link (anyone with it can watch your exports for 7 days)",
                )?
                else {
                    return Err(bad("review", "no link"));
                };
                let token = v["token"].as_str().unwrap_or_default().to_owned();
                let link = review::links::Link {
                    id: v["id"].as_str().unwrap_or_default().to_owned(),
                    token_sha256: String::new(),
                    created_ms: 0,
                    expires_ms: v["expires_ms"].as_i64().unwrap_or(0),
                    revoked: false,
                    login: None,
                    role: None,
                };
                let url = match &settings.review_tailnet {
                    Some(t) => format!("https://{}/#t={token}", t.host),
                    None => format!("http://127.0.0.1:{port}/#t={token}"),
                };
                let mut e = std::io::stderr().lock();
                let _ = writeln!(e, "Review link (shown once; valid 7 days):\n{url}");
                Ok(Output::Summary(
                    json!({"link": link.id, "expires_ms": link.expires_ms}),
                ))
            }
            LinkCmd::List => Ok(Output::Summary(json!({
                "links": review::links::load(&app).iter().map(|l| json!({
                    "id": l.id, "created_ms": l.created_ms, "expires_ms": l.expires_ms,
                    "revoked": l.revoked, "login": l.login,
                })).collect::<Vec<_>>()
            }))),
            LinkCmd::Revoke { id } => {
                human_terminal()?;
                let n = review::links::revoke(&app, Some(&id))
                    .map_err(|e| CliError::io("review links", &e))?;
                Ok(Output::Summary(json!({"revoked": n})))
            }
            LinkCmd::RevokeAll => {
                human_terminal()?;
                let n = review::links::revoke(&app, None)
                    .map_err(|e| CliError::io("review links", &e))?;
                Ok(Output::Summary(json!({"revoked": n})))
            }
        },
        ReviewCmd::Serve {
            project,
            exports,
            clips,
            port,
        } => {
            let settings = OwnerSettings::load(&app);
            let handle = ProjectHandle::open(&project, OpenOptions::default())
                .map_err(|e| bad("review", format!("open the project: {e}")))?;
            let catalog = Arc::new(ProjectCatalog {
                handle,
                exports,
                clips,
                app: app.clone(),
            });
            let mode = match &settings.review_tailnet {
                Some(t) => Mode::Tailnet {
                    host: t.host.clone(),
                    owner_login: t.owner_login.clone(),
                },
                None => Mode::Loopback,
            };
            let tailnet: Option<Arc<dyn review::Tailnet>> = match mode {
                Mode::Tailnet { .. } => Some(Arc::new(TailscaleCli)),
                Mode::Loopback => None,
            };
            let mut cfg = review::Config::new(app.clone(), mode);
            cfg.port = port;
            cfg.allowance = settings.remote_fix.map(|r| review::Allowance {
                max_usd_per_run: r.max_usd_per_run,
                max_runs_per_day: r.max_runs_per_day,
            });
            let server = review::start(cfg, catalog, tailnet).map_err(|e| bad("review", e))?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "gmclip review: serving on {} (make a link: gmclip review link new)",
                server.addr
            );
            server.wait();
            Ok(Output::Summary(json!({"stopped": true})))
        }
    }
}

/// `tailscale funnel status --json`: whether anything funnels to our port. A failure to
/// tell is an error (the server then refuses to run off-loopback).
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
        // funnelled somewhere: covered unless we can see it proxies elsewhere only
        let text = String::from_utf8_lossy(&out.stdout);
        let ours = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        let proxies_known = text.contains("Proxy");
        Ok(!proxies_known || ours.iter().any(|o| text.contains(o.as_str())))
    }
}

/// The review catalog over one project.
struct ProjectCatalog {
    handle: ProjectHandle,
    exports: PathBuf,
    clips: Option<PathBuf>,
    app: PathBuf,
}

/// Exports in the folder: `(id, mp4 path, record)`; the id is the first 16 hex of the
/// recorded SHA-256.
fn exports(dir: &Path) -> Vec<(String, PathBuf, gmclip_media::export_record::ExportRecord)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".export.json") else {
            continue;
        };
        let Ok(b) = std::fs::read(&p) else { continue };
        let Ok(rec) = serde_json::from_slice::<gmclip_media::export_record::ExportRecord>(&b)
        else {
            continue;
        };
        // the record names its file by name only, beside it
        if rec.file.contains(['/', '\\'])
            || rec.file.is_empty()
            || rec.sha256.len() != 64
            || !rec.sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            continue;
        }
        let _ = stem;
        out.push((
            rec.sha256[..16].to_ascii_lowercase(),
            dir.join(&rec.file),
            rec,
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Catalog for ProjectCatalog {
    fn home(&self) -> Value {
        let snap = self.handle.snapshot();
        let doc = &snap.doc;
        let project = doc
            .iter::<gmclip_model::entities::Project>()
            .next()
            .map(|p| p.meta.name.clone());
        let cuts: Vec<Value> = doc
            .iter::<Cut>()
            .map(|c| {
                let reviews: Vec<Value> = doc
                    .children::<Review>(c.id.entity())
                    .filter_map(|r| doc.get(r))
                    .map(|r| json!({"author": r.author, "verdict": format!("{:?}", r.verdict).to_lowercase(), "text": r.text}))
                    .collect();
                json!({"id": c.id.0.to_string(), "title": c.title, "notes": c.notes, "reviews": reviews})
            })
            .collect();
        let moments: Vec<Value> = doc
            .iter::<Moment>()
            .filter(|m| matches!(m.state, MomentState::Staged | MomentState::Posted))
            .take(200)
            .map(|m| {
                let id = m.id.0.to_string();
                json!({"id": id, "quote": m.quote, "clip": self.clip(&id).is_some()})
            })
            .collect();
        let ex: Vec<Value> = exports(&self.exports)
            .into_iter()
            .map(|(id, p, rec)| {
                let d = &rec.duration_expected;
                let secs = if d.fps_num > 0 {
                    d.frames as f64 * d.fps_den as f64 / d.fps_num as f64
                } else {
                    0.0
                };
                json!({
                    "id": id,
                    "title": p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                    "duration_s": secs,
                    "blocked": rec.blocked,
                })
            })
            .collect();
        json!({"project": project, "exports": ex, "cuts": cuts, "moments": moments})
    }

    fn export(&self, id: &str) -> Option<FileRec> {
        let (_, path, rec) = exports(&self.exports)
            .into_iter()
            .find(|(i, _, _)| i == id)?;
        let size = std::fs::symlink_metadata(&path).ok()?.len();
        Some(FileRec {
            path,
            size,
            sha256: rec.sha256,
        })
    }

    fn clip(&self, id: &str) -> Option<FileRec> {
        let dir = self.clips.as_ref()?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        // only clips of this project's moments
        let snap = self.handle.snapshot();
        if !snap.doc.iter::<Moment>().any(|m| m.id.0.to_string() == id) {
            return None;
        }
        let rec: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{id}.clip.json"))).ok()?)
                .ok()?;
        Some(FileRec {
            path: dir.join(format!("{id}.mp4")),
            size: rec.get("size")?.as_u64()?,
            sha256: rec.get("sha256")?.as_str()?.to_owned(),
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
        let caller = Caller {
            identity: author.to_owned(),
            connection: format!("review:{author}"),
            kind: CallerKind::System,
            scopes: vec![gmclip_core::Scope::Admin],
            run: None,
            cut_binding: None,
        };
        let ok = self
            .handle
            .call(CallRequest {
                command: "review_add".into(),
                args: json!({"cut": cut, "text": text, "verdict": verdict, "export_ref": export_ref}),
                base: None,
                request_id: None,
                caller,
            })
            .map_err(|e| e.to_string())?;
        Ok(ok
            .output
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn fix(&self, req: &FixRequest) -> Result<Value, String> {
        // M2 has no launcher wiring for remote starts yet: an allowed request is
        // recorded as approved for the launcher, a refused one as "needs you"; both
        // carry the link-authored note as untrusted text (R-INJ-1).
        let status = match &req.decision {
            FixDecision::Start { .. } => "approved_remote",
            FixDecision::Queue { .. } => "needs_you",
        };
        let line = json!({
            "ts": now_ms(), "kind": "fix_with_agent", "status": status, "cut": req.cut,
            "author": req.author, "decision": req.decision,
            "note": {"untrusted": true, "text": req.note},
        });
        let f = self.app.join("needs-you.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&f)
            .map_err(|e| format!("needs-you ({})", e.kind()))?;
        writeln!(file, "{line}").map_err(|e| format!("needs-you ({})", e.kind()))?;
        Ok(json!({"status": status}))
    }
}
