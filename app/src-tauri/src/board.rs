//! The review board, Send to Clippy and the activity rail (backlog U.10 essentials).
//!
//! - **Board**: every export of a project as a tile, with the human's keep / change /
//!   drop (kept on the export's row in `<home>/exports.json`), its review thread and
//!   Reveal in Finder. The app has no board view since decision 0054: the cut header reads
//!   the cut's newest tile (its file, and the run's handoff with Restore).
//! - **Review thread** (one per export): the notes (`Review` rows) and comments review
//!   links wrote on that export — attributed `review-link:<id>`, their words untrusted and
//!   shown as text only (R-UI-SEC-2) — and your replies, oldest first. An export is known
//!   on a link by the start of its file's SHA-256 (`thread_key`); a link note with no
//!   export belongs to the cut's newest export. Your reply is one `comment_add` on the
//!   export's thread (a reply to its first comment, `thread_root`, else that first
//!   comment), written as `human`.
//! - **Send to Clippy** (SPEC §5.14.2, R-CLIP-1; the protocol of `gmclip clippy send`):
//!   paired first (a 256-bit secret, app data 0600, shown once on the native sheet for
//!   the human to enter in Clippy), loopback only, never a blocked export or one whose
//!   bytes no longer match its record, the human confirms on the native sheet, the file
//!   is hashed again right before the post; a `<stem>.clippy.json` sidecar beside it.
//! - **Activity rail**: connected agent clients (the holder's connection log) with
//!   Revoke (Owner), launched runs with Stop, and Pause agents (Owner: the sheet asks;
//!   while paused every agent tool call is refused and launched runs end).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gmclip_media::export_record::ExportRecord;
use gmclip_model::entities::{Comment, Cut, Framing, Moment, Part, Review};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// Clippy's local server.
pub const CLIPPY_ENDPOINT: &str = "http://127.0.0.1:8878";
const PAIRING_FILE: &str = "clippy-pairing.json";

fn bad(m: impl Into<String>) -> AppError {
    AppError::invalid(m.into())
}

/// The board's tiles.
pub fn board(core: &AppCore, project: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let exports = core.exports(project)?;
    // the newest export of each cut takes the link notes written on no export
    let newest: std::collections::BTreeMap<&str, &str> = {
        let mut m: std::collections::BTreeMap<&str, (i64, &str)> = Default::default();
        for e in &exports {
            let k = m.entry(e.cut.as_str()).or_insert((e.at_ms, e.id.as_str()));
            if e.at_ms >= k.0 {
                *k = (e.at_ms, e.id.as_str());
            }
        }
        m.into_iter().map(|(c, (_, id))| (c, id)).collect()
    };
    let tiles: Vec<Value> = exports
        .iter()
        .rev()
        .map(|e| {
            let cid = ulid_id::<Cut>(&e.cut).ok();
            let key = thread_key(e);
            let (thread, root) = cid.map_or((Vec::new(), None), |c| {
                thread(&doc, c, key.as_deref(), newest.get(e.cut.as_str()) == Some(&e.id.as_str()))
            });
            let hook = cid
                .and_then(|c| doc.get(c))
                .and_then(|c| c.moment)
                .and_then(|m| doc.get::<Moment>(m))
                .and_then(|m| m.origin_ref.clone());
            // the run's handoff summary (untrusted: the agent's words) and its choices
            let handoff = cid
                .and_then(|c| doc.get(c))
                .and_then(|c| c.last_run_summary.as_ref())
                .map(|s| {
                    json!({"run": s.run.0.to_string(),
                           "text": s.text.chars().take(4000).collect::<String>(),
                           "choices": s.reversible_choices.iter().enumerate().map(|(i, c)| json!({
                               "index": i, "label": c.label.chars().take(300).collect::<String>(),
                               "fields": c.restore.len()})).collect::<Vec<_>>()})
                });
            let sidecar = e.file.with_extension("clippy.json");
            json!({"id": e.id, "cut": e.cut, "title": e.title, "file": e.file, "at_ms": e.at_ms,
                   "seconds": e.frames as f64 / e.rate(), "render_s": e.seconds,
                   "notes": e.notes, "verdict": e.verdict, "handoff": handoff,
                   "thread": thread, "thread_key": key, "thread_root": root,
                   "exists": e.file.is_file(),
                   "clippy": {"hook": hook, "sent": sidecar.is_file(),
                              "why_not": if hook.is_none() { Some("this cut did not come from a Clippy hook") } else { None }}})
        })
        .collect();
    Ok(json!({"tiles": tiles, "clippy_paired": secret(core).is_some()}))
}

/// The id an export goes by on a review link (the start of its file's SHA-256, from its
/// export record), which link notes and comments name as their `export_ref`.
fn thread_key(e: &crate::core::ExportEntry) -> Option<String> {
    let rec = ExportRecord::read(&e.file).ok()?;
    (rec.sha256.len() == 64 && rec.sha256.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| rec.sha256[..16].to_ascii_lowercase())
}

/// An export's review thread, oldest first, and the comment a reply goes under: the
/// notes and comments on `key` (with the link notes on no export when `newest`) and the
/// replies to those comments. Everything not the human's is the writer's own words.
fn thread(
    doc: &gmclip_model::Doc,
    cut: gmclip_model::Id<Cut>,
    key: Option<&str>,
    newest: bool,
) -> (Vec<Value>, Option<String>) {
    let on = |r: &Option<String>| matches!((r.as_deref(), key), (Some(r), Some(k)) if r == k);
    let mut items: Vec<(i64, String, Value)> = doc
        .children::<Review>(cut.entity())
        .filter_map(|id| doc.get(id))
        .filter(|r| on(&r.export_ref) || (newest && r.export_ref.is_none()))
        .map(|r| {
            (
                r.at_ms,
                r.id.0.to_string(),
                json!({"kind": "note", "id": r.id.0.to_string(), "author": r.author,
                       "human": r.author == "human", "verdict": r.verdict, "at_ms": r.at_ms,
                       "text": r.text.chars().take(2000).collect::<String>()}),
            )
        })
        .collect();
    let comments: Vec<&Comment> = doc
        .children::<Comment>(cut.entity())
        .filter_map(|id| doc.get(id))
        .collect();
    let mut roots: Vec<&&Comment> = comments
        .iter()
        .filter(|c| c.reply_to.is_none() && on(&c.export_ref))
        .collect();
    roots.sort_by_key(|c| (c.at_ms, c.id.0));
    for c in comments.iter().filter(|c| {
        roots.iter().any(|r| r.id == c.id)
            || c.reply_to.is_some_and(|t| roots.iter().any(|r| r.id == t))
    }) {
        items.push((
            c.at_ms,
            c.id.0.to_string(),
            json!({"kind": "comment", "id": c.id.0.to_string(), "author": c.author,
                   "human": c.author == "human", "verdict": null, "at_ms": c.at_ms,
                   "text": c.text.chars().take(2000).collect::<String>()}),
        ));
    }
    items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    (
        items.into_iter().map(|x| x.2).collect(),
        roots.first().map(|c| c.id.0.to_string()),
    )
}

/// The profile folder style feedback goes to: the Owner's active profile.
fn feedback_dir(core: &AppCore) -> AppResult<PathBuf> {
    let s = match core.agents.holder() {
        Some(h) => h.settings(),
        None => gmclip_mcp::settings::OwnerSettings::load(&core.dirs.app_data),
    };
    s.profile.filter(|p| p.is_dir()).ok_or_else(|| {
        AppError::new(
            "Unsupported",
            "no profile is set: choose one in Settings first (feedback lives in the profile)",
        )
    })
}

/// "Add to my style feedback", step one: the text as it will be written, with every
/// invisible, bidi and tag character shown as `⟦U+XXXX⟧` (R-INJ-6), for the human to
/// read before adding it.
pub fn feedback_preview(core: &AppCore, text: &str) -> AppResult<Value> {
    let dir = feedback_dir(core)?;
    let text: String = text
        .chars()
        .take(gmclip_core::profile::feedback::MAX_TEXT_CHARS)
        .collect();
    let shown = gmclip_core::profile::style::visible(&text);
    Ok(json!({"shown": shown, "hidden_characters": shown != text, "profile": dir}))
}

/// "Add to my style feedback", step two (a **Human** command, R-PROF-2: the app's own
/// button only — no MCP tool, no `command_run`, no CLI, no review-link route reaches
/// it). Appends to `<profile>/feedback.jsonl`, the only file it writes; `link` names the
/// review link the text came from.
pub fn feedback_append(
    core: &AppCore,
    text: &str,
    export: Option<&str>,
    link: Option<&str>,
) -> AppResult<Value> {
    let dir = feedback_dir(core)?;
    let via = match link {
        None => "app".to_owned(),
        Some(l)
            if !l.is_empty()
                && l.len() <= 64
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            format!("review-link:{l}")
        }
        Some(_) => return Err(bad("a review link id")),
    };
    let author = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "me".into());
    let date = ymd(crate::core::now_ms());
    let r =
        gmclip_core::profile::feedback::append_by_human(&dir, &date, text, export, &author, &via)
            .map_err(bad)?;
    Ok(json!({"added": r}))
}

/// ms since the epoch → `YYYY-MM-DD` (UTC).
pub fn ymd(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

/// keep / change / drop (or `null` to clear) on an export.
pub fn verdict(core: &AppCore, project: &str, export: &str, v: Option<&str>) -> AppResult<Value> {
    if v.is_some_and(|x| !["keep", "change", "drop"].contains(&x)) {
        return Err(bad("keep, change or drop"));
    }
    core.set_export_verdict(project, export, v.map(str::to_owned))?;
    Ok(json!({"export": export, "verdict": v}))
}

fn secret(core: &AppCore) -> Option<String> {
    let v: Value =
        serde_json::from_slice(&std::fs::read(core.dirs.app_data.join(PAIRING_FILE)).ok()?).ok()?;
    v.get("secret")
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// Pairs with Clippy: a new secret, shown once on the native sheet.
pub fn pair(core: &AppCore) -> AppResult<Value> {
    let s = gmclip_mcp::wire::nonce();
    if !crate::sheet::confirm(
        "Pair gmclip with Clippy?",
        &format!(
            "Enter this code in Clippy (shown once):\n\n{s}\n\nAfter that, “Send to Clippy” hands an export to it (on {} only).",
            crate::caps::THIS_MACHINE
        ),
        "Pair",
    ) {
        return Err(AppError::new(
            "Unauthorized",
            "not paired (the sheet was cancelled)",
        ));
    }
    gmclip_mcp::paths::mkdir_private(&core.dirs.app_data)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let f = core.dirs.app_data.join(PAIRING_FILE);
    let tmp = core.dirs.app_data.join(format!("{PAIRING_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    {
        #[cfg(unix)]
        let mut t = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| AppError::internal(e.to_string()))?
        };
        // the Windows 0600: created with an owner-only DACL (decisions/0019)
        #[cfg(windows)]
        let mut t = gmclip_platform::windows::create_file_owner_only(&tmp)
            .map_err(|e| AppError::internal(e.to_string()))?;
        t.write_all(json!({"secret": s}).to_string().as_bytes())
            .map_err(|e| AppError::internal(e.to_string()))?;
    }
    std::fs::rename(&tmp, &f).map_err(|e| AppError::internal(e.to_string()))?;
    Ok(json!({"paired": true}))
}

/// The export checked against its record (the CLI's `checked_record`).
fn checked_record(export: &Path) -> AppResult<ExportRecord> {
    for (p, what) in [
        (export.to_path_buf(), "export"),
        (ExportRecord::path_for(export), "export record"),
    ] {
        let m = std::fs::symlink_metadata(&p).map_err(|_| bad(format!("the {what} is missing")))?;
        if !m.is_file() {
            return Err(bad(format!("the {what} is not a regular file")));
        }
    }
    let rec =
        ExportRecord::read(export).map_err(|_| bad("the export has no export record beside it"))?;
    if export.file_name().and_then(|n| n.to_str()) != Some(rec.file.as_str()) {
        return Err(bad("the export record names another file"));
    }
    if !rec.blocked.is_empty() {
        return Err(bad("a blocked export is never handed to Clippy"));
    }
    let (sha, _) =
        gmclip_ml::sha256::file_hex(export).map_err(|_| bad("cannot read the export"))?;
    if !sha.eq_ignore_ascii_case(&rec.sha256) {
        return Err(bad("the export does not match its record (re-export it)"));
    }
    Ok(rec)
}

fn sidecar(doc: &gmclip_model::Doc, cut: &str, rec: &ExportRecord) -> AppResult<Value> {
    let c = doc
        .get(ulid_id::<Cut>(cut)?)
        .ok_or_else(|| bad("no such cut"))?;
    let m = c
        .moment
        .and_then(|m| doc.get::<Moment>(m))
        .ok_or_else(|| bad("the cut has no moment, so no Clippy hook to hand it to"))?;
    let hook = m
        .origin_ref
        .clone()
        .ok_or_else(|| bad("the cut's moment has no Clippy hook id"))?;
    let d = &rec.duration_expected;
    let duration = if d.fps_num > 0 {
        d.frames as f64 * d.fps_den as f64 / d.fps_num as f64
    } else {
        0.0
    };
    let parts: f64 = doc
        .children::<Part>(c.id.entity())
        .filter_map(|p| doc.get(p))
        .map(|p| gmclip_model::clock::part_duration(p).0 as f64 / 705_600_000.0)
        .sum();
    if (parts - duration).abs() > 0.5 {
        return Err(bad(format!(
            "the export is {duration:.1} s but the cut is {parts:.1} s now: export it again first"
        )));
    }
    let framing = rec.framing.clone().or_else(|| {
        doc.children::<Framing>(c.id.entity())
            .find_map(|f| doc.get(f))
            .map(|f| f.name.clone())
    });
    Ok(
        json!({"hook_id": hook, "title": c.title, "text_hook": m.text_hook, "excerpt": m.excerpt.text,
              "render_sig": rec.render_sig, "duration": duration, "framing": framing}),
    )
}

fn post(endpoint: &str, secret: &str, body: &Value) -> AppResult<Value> {
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| bad("loopback only"))?
        .trim_end_matches('/');
    let addr: SocketAddr = rest.parse().map_err(|_| bad("loopback only"))?;
    if !addr.ip().is_loopback() {
        return Err(bad("Clippy is reached on this machine only"));
    }
    let payload = body.to_string();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|_| {
        bad(format!(
            "Clippy is not running on {} ({rest})",
            crate::caps::THIS_MACHINE
        ))
    })?;
    let req = format!(
        "POST /api/edit_station/final HTTP/1.1\r\nHost: {rest}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    s.write_all(req.as_bytes())
        .map_err(|e| AppError::internal(e.to_string()))?;
    s.set_read_timeout(Some(Duration::from_secs(120))).ok();
    let mut buf = Vec::new();
    let _ = s.take(1 << 20).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(bad(format!("Clippy answered {status}")));
    }
    Ok(serde_json::from_str(body).unwrap_or(Value::Null))
}

/// Hands an export to Clippy after the human confirms on the native sheet.
pub fn send(core: &AppCore, project: &str, export: &str) -> AppResult<Value> {
    send_to(core, project, export, CLIPPY_ENDPOINT)
}

/// [`send`] to a given loopback endpoint (tests use their own listener).
pub fn send_to(core: &AppCore, project: &str, export: &str, endpoint: &str) -> AppResult<Value> {
    send_checked(core, project, export, endpoint, &|title, shown| {
        crate::sheet::confirm(title, shown, "Send")
    })
}

/// Send to Clippy for an "Approve" a review link made under R-REV-4's conditions
/// (decision 0015: tailnet, the Owner's own login, the Owner's remote-approve allowance):
/// that tap is the human click, so no sheet; every other check is the same. It only
/// registers the final in Clippy; approving and submitting the post stay Clippy's.
pub fn send_approved(
    core: &AppCore,
    project: &str,
    export: &str,
    endpoint: &str,
) -> AppResult<Value> {
    send_checked(core, project, export, endpoint, &|_, _| true)
}

fn send_checked(
    core: &AppCore,
    project: &str,
    export: &str,
    endpoint: &str,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> AppResult<Value> {
    let secret = secret(core)
        .ok_or_else(|| bad("gmclip is not paired with Clippy: Pair with Clippy first"))?;
    let e = core
        .exports(project)?
        .into_iter()
        .find(|x| x.id == export)
        .ok_or_else(|| AppError::not_found("no such export"))?;
    let file: PathBuf = e.file.clone();
    let rec = checked_record(&file)?;
    let p = core.project(project)?;
    let side = sidecar(&p.handle.snapshot().doc, &e.cut, &rec)?;
    let hook = side["hook_id"].as_str().unwrap_or_default().to_owned();
    let shown = format!(
        "• It replaces the current final of hook {}.\n• File: {}\n• Title: {}\n• {:.1} s, {}",
        hook.chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect::<String>(),
        file.display(),
        side["title"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect::<String>(),
        side["duration"].as_f64().unwrap_or(0.0),
        side["framing"].as_str().unwrap_or("")
    );
    if !confirm("Send this export to Clippy?", &shown) {
        return Err(AppError::new(
            "Unauthorized",
            "not sent (the sheet was cancelled)",
        ));
    }
    let rec2 = checked_record(&file)?;
    if rec2.sha256 != rec.sha256 {
        return Err(bad("the export changed while you were confirming"));
    }
    let side_path = file.with_extension("clippy.json");
    if std::fs::symlink_metadata(&side_path).is_ok_and(|m| !m.is_file()) {
        return Err(bad("the sidecar path is not a regular file"));
    }
    std::fs::write(
        &side_path,
        serde_json::to_vec_pretty(&side).unwrap_or_default(),
    )
    .map_err(|e| AppError::internal(e.to_string()))?;
    let answer = post(
        endpoint,
        &secret,
        &json!({"hook_id": hook, "path": file, "quality": "1080", "gmclip": side}),
    )?;
    Ok(json!({"hook_id": hook, "clippy": answer}))
}

/// The activity rail: connected clients, launched runs, paused.
pub fn activity(core: &AppCore) -> Value {
    let Some(h) = core.agents.holder() else {
        return json!({"holder": false});
    };
    let mut seen = std::collections::BTreeMap::<String, Value>::new();
    for c in gmclip_mcp::settings::connections(&h.env.app_data, 200) {
        seen.insert(
            c.identity.clone(),
            json!({"identity": c.identity, "last_ms": c.ts, "exe": c.exe, "client": c.client_exe, "mode": c.mode}),
        );
    }
    let revoked = h.settings().revoked;
    let clients: Vec<Value> = seen
        .into_values()
        .map(|mut v| {
            let id = v["identity"].as_str().unwrap_or_default().to_owned();
            v["revoked"] = json!(revoked.contains(&id));
            v
        })
        .collect();
    let runs: Vec<Value> = h
        .launched_runs()
        .into_iter()
        .map(|(r, p, c)| json!({"run": r, "project": p, "cut": c}))
        .collect();
    let approvals: Vec<Value> = h
        .approvals()
        .into_iter()
        .map(|a| {
            json!({"id": a.id, "identity": a.identity, "run": a.run, "tool": a.tool, "effects": a.effects,
                   "prose": a.prose, "ts_ms": a.ts_ms, "run_paused": a.run_paused,
                   "exportable": a.effects.get("export").is_some()})
        })
        .collect();
    let acked = first_seen_acked(&h.env.app_data);
    let first_seen: Vec<Value> = gmclip_mcp::settings::first_seen(&h.env.app_data, 20)
        .into_iter()
        .filter(|n| n.ts > acked && !n.identity.is_empty())
        .map(|n| {
            json!({"ts": n.ts, "identity": n.identity, "exe": n.exe,
                   "revoked": revoked.contains(&n.identity)})
        })
        .collect();
    json!({"holder": true, "paused": h.paused(), "clients": clients, "runs": runs, "approvals": approvals,
           "first_seen": first_seen})
}

const FIRST_SEEN_ACK: &str = "first-seen-ack.json";

fn first_seen_acked(app: &Path) -> i64 {
    std::fs::read(app.join(FIRST_SEEN_ACK))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("ts").and_then(Value::as_i64))
        .unwrap_or(0)
}

/// The human read the first-seen notices up to `ts` (R-MCP-1c UI): the rail stops
/// showing them; the connection log keeps every line.
pub fn first_seen_ack(core: &AppCore, ts: i64) -> AppResult<Value> {
    let h = core
        .agents
        .holder()
        .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
    let ts = ts.max(first_seen_acked(&h.env.app_data));
    gmclip_mcp::paths::mkdir_private(&h.env.app_data)
        .map_err(|e| AppError::internal(e.to_string()))?;
    std::fs::write(
        h.env.app_data.join(FIRST_SEEN_ACK),
        serde_json::to_vec(&json!({"ts": ts})).unwrap_or_default(),
    )
    .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(json!({"acked": ts}))
}

/// A human handled an approval (`id`), or reviewed a paused run (`run`: its run, or the
/// identity that asked): R-PROTO-6.
pub fn approval_resolve(core: &AppCore, id: Option<&str>, run: Option<&str>) -> AppResult<Value> {
    let h = core
        .agents
        .holder()
        .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
    Ok(json!({"resolved": h.approval_resolve(id, run)}))
}

/// "Approve and export" (decision 0050): the human runs the export an agent was refused
/// (R-LINT-2 blocks agent exports, never a human's), as the human, then the ask is done.
/// The cut must be as it was when the agent asked: what the card showed is what is
/// exported. The agent may wait on and verify the job, as if it had started it.
pub fn approval_export(
    core: &Arc<AppCore>,
    jobs: &Arc<crate::jobs::Jobs>,
    id: &str,
) -> AppResult<Value> {
    let h = core
        .agents
        .holder()
        .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
    let a = h
        .approval(id)
        .ok_or_else(|| AppError::not_found("that ask is no longer waiting"))?;
    let ex = &a.effects["export"];
    let args = &ex["args"];
    let (Some(command), Some(project), Some(cut)) = (
        ex["command"].as_str(),
        args["project"].as_str(),
        args["cut"].as_str(),
    ) else {
        return Err(AppError::new("Unsupported", "this ask is not an export"));
    };
    core.ensure_open(project)?;
    let p = core.project(project)?;
    let cid = ulid_id::<Cut>(cut)?;
    let now = gmclip_mcp::tools::cut_rev(&p.handle.snapshot().doc, cid);
    if a.effects["cut_rev"].as_u64() != Some(now) {
        return Err(AppError::new(
            "Conflict",
            "the cut changed after the agent asked: look at it again, then export it yourself or dismiss the ask",
        ));
    }
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let out = match command {
        "export" => match args["framing"].as_str() {
            Some(f) => jobs.start_export_framing(core, project, cut, f)?,
            None => jobs.start_export(core, project, cut)?,
        },
        "export_start" => jobs.start_export_preset(
            core,
            project,
            cut,
            flag("draft_mattes"),
            args["preset"].as_str(),
        )?,
        "export_both" => crate::bothexport::start(core, jobs, project, cut, flag("make_missing"))?,
        "opening_export" => jobs.start_export_version(
            core,
            project,
            cut,
            args["opening"]
                .as_str()
                .ok_or_else(|| AppError::invalid("the ask names no opening"))?,
        )?,
        _ => return Err(AppError::new("Unsupported", "this ask is not an export")),
    };
    for job in gmclip_mcp::tools::app_job_ids(&out) {
        h.app_job_note(
            &job,
            gmclip_mcp::holder::AppJobOwner {
                identity: a.identity.clone(),
                project: project.to_owned(),
                cut: cut.to_owned(),
            },
        );
    }
    h.approval_resolve(Some(id), None);
    Ok(json!({"approval": id, "exported": out}))
}

/// Pause or resume agents (Owner: the sheet asks).
pub fn pause(core: &AppCore, on: bool) -> AppResult<Value> {
    let h = core
        .agents
        .holder()
        .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
    let (title, text) = if on {
        (
            "Pause all agents?",
            "• Every agent's gmclip tools stop answering until you resume.\n• Runs your agent is doing now end.",
        )
    } else {
        (
            "Resume agents?",
            "• Connected agents can use gmclip's tools again.",
        )
    };
    if !crate::sheet::confirm(title, text, if on { "Pause" } else { "Resume" }) {
        return Err(AppError::new(
            "Unauthorized",
            "not allowed (the Owner did not approve it on the sheet)",
        ));
    }
    h.set_paused(on);
    Ok(json!({"paused": on}))
}

/// Stops one launched run (protective; no sheet).
pub fn stop_run(core: &AppCore, run: &str) -> AppResult<Value> {
    let h = core
        .agents
        .holder()
        .ok_or_else(|| AppError::new("Unsupported", "no agent server in this app"))?;
    h.launch_end(run);
    Ok(json!({"stopped": run}))
}
