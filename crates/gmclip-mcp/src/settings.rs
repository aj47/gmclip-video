//! Owner settings, the audit log and the connection log (R-OWN-3, R-MCP-1c, R-FS-1).
//!
//! The Owner settings live in app data (`owner-settings.json`), never in a project or a
//! profile (R-PROF-4). They change only through the Owner path (`gmclip owner …` on a
//! TTY with a re-typed code). Every Owner action is appended to `audit.log`; every
//! connection to `connections.jsonl`, and the first connection from a client never seen
//! before produces one notice.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::paths;

/// Authority fields: who may read and write where.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerSettings {
    /// Agent read roots the Owner added.
    #[serde(default)]
    pub read_roots: Vec<PathBuf>,
    /// Agent write roots the Owner added.
    #[serde(default)]
    pub write_roots: Vec<PathBuf>,
    /// The recordings folder chosen at setup (a read root).
    #[serde(default)]
    pub recordings: Option<PathBuf>,
    /// Whether the human's home folder is an agent read root (decision 0058). Absent =
    /// yes; the Owner narrows it by removing the home folder from the read roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_read: Option<bool>,
    /// Identities the Owner revoked (`local:<client>`): their calls are refused.
    #[serde(default)]
    pub revoked: Vec<String>,
    /// The active creator profile folder (content only, R-PROF-4). The style rules
    /// file is pinned separately (`style-pin.json`, R-PROF-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<PathBuf>,
    /// Where exports go (a write root); default `~/Movies/gmclip/Exports`.
    #[serde(default)]
    pub export_dir: Option<PathBuf>,
    /// Review link off-loopback mode (R-REV-1): `tailscale serve` at `https://<host>`,
    /// with the Owner's tailnet login. Absent = loopback only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_tailnet: Option<ReviewTailnet>,
    /// The standing remote-fix allowance (R-REV-4); absent = off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_fix: Option<RemoteFix>,
    /// The standing allowance for "Approve" on a review link (decision 0015): sends to
    /// Clippy's calendar without a click here; absent = off (every approve waits for a
    /// click in the app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_send: Option<RemoteSend>,
    /// Executables besides gmclip itself that may register launched runs (R-AGT-1:
    /// launch registration comes from the launcher's verified path). Empty in normal use;
    /// test harnesses that drive the launcher library name their own binary here. Only
    /// absolute paths count. Like every field here it is as strong as the file's
    /// permissions: a process running as the same user can edit it (see `launch_register`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_launchers: Vec<PathBuf>,
    /// Spend caps (R-COST-2); absent = the spec's defaults ([`Budgets::default`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<Budgets>,
    /// Which AI may run and on which models (R-OWN-1 "providers/keys/model allowlist");
    /// absent = [`Providers::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Providers>,
    /// Usage events allowed (SPEC §14: off unless the user opts in). This build has no
    /// sender: the choice is recorded for the beta's analytics (R-MVP-2 [M3]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub telemetry: bool,
}

/// The spend caps in dollars (SPEC §10.8 R-COST-2, with its defaults). Passes inside a
/// run a human launched draw on that run's budget; raising any cap is an Owner command.
/// At M2 no provider bills (AJ, 2026-10-15: AI runs through the user's own agent CLI on
/// its own subscription), so these bound the paid passes M3 brings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    /// An edit pass, per run ($2).
    pub edit_pass_usd: f64,
    /// Other runs, per client identity ($5).
    pub run_usd: f64,
    /// All AI, per day ($20).
    pub day_usd: f64,
    /// An overnight batch in total ($5).
    pub batch_usd: f64,
    /// Ask, per message ($1).
    pub ask_usd: f64,
}

impl Eq for Budgets {}

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            edit_pass_usd: 2.0,
            run_usd: 5.0,
            day_usd: 20.0,
            batch_usd: 5.0,
            ask_usd: 1.0,
        }
    }
}

impl Budgets {
    /// The caps by name, in the order the sheet lists them.
    pub fn named(&self) -> [(&'static str, f64); 5] {
        [
            ("edit_pass_usd", self.edit_pass_usd),
            ("run_usd", self.run_usd),
            ("day_usd", self.day_usd),
            ("batch_usd", self.batch_usd),
            ("ask_usd", self.ask_usd),
        ]
    }

    /// Sets one cap by name; `false` for an unknown name.
    pub fn set(&mut self, name: &str, v: f64) -> bool {
        let slot = match name {
            "edit_pass_usd" => &mut self.edit_pass_usd,
            "run_usd" => &mut self.run_usd,
            "day_usd" => &mut self.day_usd,
            "batch_usd" => &mut self.batch_usd,
            "ask_usd" => &mut self.ask_usd,
            _ => return false,
        };
        *slot = v;
        true
    }
}

/// The largest cap the Owner may set, in dollars.
pub const BUDGET_MAX_USD: f64 = 1_000.0;

/// Which AI runs (R-OWN-1 providers and model allowlist). Keys are never taken: there is
/// no API-key provider at M2 (AJ, 2026-10-15; R-OWN-2's keychain is M3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    pub ai: AiProvider,
    /// Models a launched run may name (`--model`); empty = the harness's own default and
    /// any model it accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl Default for Providers {
    fn default() -> Self {
        Providers {
            ai: AiProvider::AgentCli,
            models: Vec::new(),
        }
    }
}

/// Where AI passes run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    /// No AI: every pass offers its local alternative (SPEC §10.1 "no-provider").
    None,
    /// The user's own agent CLI through the launcher (SPEC §10.6–10.7).
    AgentCli,
}

/// A model id a run may name: the launcher's own rule (letters, digits, `._:-[]/`).
pub fn valid_model_id(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 64
        && !m.starts_with('-')
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-[]/".contains(&b))
}

/// Review links through the tailnet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTailnet {
    /// `<machine>.<tailnet>.ts.net`
    pub host: String,
    /// The Owner's tailnet login (the device owner, R-REV-4 (b)).
    pub owner_login: String,
}

/// The remote-fix allowance (R-REV-4 (c)).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFix {
    pub max_usd_per_run: f64,
    pub max_runs_per_day: u32,
}

impl Eq for RemoteFix {}

/// The remote-approve allowance (decision 0015): how many exports a day a review-link
/// "Approve" from the Owner's own tailnet login may hand to Clippy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSend {
    pub max_sends_per_day: u32,
}

impl OwnerSettings {
    fn file(app: &Path) -> PathBuf {
        app.join("owner-settings.json")
    }

    /// Loads the settings (missing → defaults; unreadable → defaults, never wider).
    pub fn load(app: &Path) -> OwnerSettings {
        fs::read(Self::file(app))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Saves atomically (temp + rename). Only the Owner path calls this.
    pub fn save(&self, app: &Path) -> std::io::Result<()> {
        paths::mkdir_private(app)?;
        let f = Self::file(app);
        let tmp = f.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        fs::rename(tmp, f)
    }

    /// The spend caps in force.
    pub fn budgets(&self) -> Budgets {
        self.budgets.clone().unwrap_or_default()
    }

    /// The providers in force.
    pub fn providers(&self) -> Providers {
        self.providers.clone().unwrap_or_default()
    }

    /// Whether a launched agent run may start, naming `model` (or the harness default):
    /// refused when the Owner turned AI off, or named a model outside the allowlist.
    pub fn agent_run_allowed(&self, model: Option<&str>) -> Result<(), &'static str> {
        let p = self.providers();
        if p.ai == AiProvider::None {
            return Err("AI is off in the Owner settings (providers: none)");
        }
        if let Some(m) = model
            && !p.models.is_empty()
            && !p.models.iter().any(|x| x == m)
        {
            return Err("that model is not in the Owner's model allowlist");
        }
        Ok(())
    }

    /// Whether the human's home folder is a read root (decision 0058: yes unless the
    /// Owner removed it).
    pub fn home_readable(&self) -> bool {
        self.home_read != Some(false)
    }

    /// The agent read roots this settings file names: the read roots, the write roots
    /// (a folder agents may write is one they may read, decision 0058) and the recordings
    /// folder. The human's home folder, when [`OwnerSettings::home_readable`], is added by
    /// the holder, which knows the home.
    pub fn read_roots(&self) -> Vec<PathBuf> {
        let mut v = self.read_roots.clone();
        for w in &self.write_roots {
            if !v.contains(w) {
                v.push(w.clone());
            }
        }
        if let Some(r) = &self.recordings
            && !v.contains(r)
        {
            v.push(r.clone());
        }
        v
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn append(app: &Path, name: &str, line: &Value) {
    if paths::mkdir_private(app).is_err() {
        return;
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(app.join(name))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// One audit line per Owner action, with the requesting process.
pub fn audit(
    app: &Path,
    action: &str,
    command: &str,
    args_hash: &str,
    pid: Option<u32>,
    exe: Option<&Path>,
    outcome: &str,
) {
    append(
        app,
        "audit.log",
        &json!({
            "ts": now_ms(),
            "action": action,
            "command": command,
            "args_hash": args_hash,
            "pid": pid,
            "exe": exe.map(|p| p.display().to_string()),
            "outcome": outcome,
        }),
    );
}

/// A connection-log record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConnRecord {
    pub ts: i64,
    pub connection: String,
    pub identity: String,
    pub uid: u32,
    pub pid: Option<u32>,
    /// The peer process (the shim, usually gmclip itself).
    pub exe: Option<String>,
    /// What the shim says launched it (its parent), for the first-seen notice.
    pub client_exe: Option<String>,
    pub mode: String,
}

/// The executable a connection is first-seen by: the program that launched the shim
/// (the peer itself is usually the gmclip shim, so it names no client). With no path
/// known, the identity stands in.
pub fn first_seen_key(rec: &ConnRecord) -> String {
    rec.client_exe
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| format!("identity:{}", rec.identity))
}

/// A first-seen notice (R-MCP-1c): the first connection from an executable path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Notice {
    pub ts: i64,
    pub identity: String,
    /// The executable path the notice is keyed by (see [`first_seen_key`]).
    pub exe: String,
    pub notice: String,
}

/// Logs a connection; returns the first-seen notice if its executable path is new —
/// once per path, whatever identity it connects under (R-MCP-1c).
pub fn log_connection(app: &Path, rec: &ConnRecord) -> Option<Notice> {
    append(
        app,
        "connections.jsonl",
        &serde_json::to_value(rec).unwrap_or(Value::Null),
    );
    let key = first_seen_key(rec);
    let seen_file = app.join("clients-seen.json");
    let mut seen: BTreeSet<String> = fs::read(&seen_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if !seen.insert(key.clone()) {
        return None;
    }
    let _ = fs::write(&seen_file, serde_json::to_vec(&seen).unwrap_or_default());
    let notice = format!(
        "gmclip: first connection from {} ({}). Any program running as you can use gmclip the way your agent can. Revoke: gmclip owner revoke {}",
        rec.identity,
        rec.client_exe.as_deref().unwrap_or("unknown program"),
        rec.identity
    );
    let n = Notice {
        ts: now_ms(),
        identity: rec.identity.clone(),
        exe: key,
        notice,
    };
    append(
        app,
        "notices.jsonl",
        &serde_json::to_value(&n).unwrap_or(Value::Null),
    );
    Some(n)
}

/// The last `n` connection records.
pub fn connections(app: &Path, n: usize) -> Vec<ConnRecord> {
    let text = fs::read_to_string(app.join("connections.jsonl")).unwrap_or_default();
    let all: Vec<ConnRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

/// The last `n` first-seen notices' text (`gmclip status`).
pub fn notices(app: &Path, n: usize) -> Vec<String> {
    first_seen(app, n).into_iter().map(|x| x.notice).collect()
}

/// The last `n` first-seen notices (the app's activity rail). Lines from before the
/// notice carried its identity and path read with those fields empty.
pub fn first_seen(app: &Path, n: usize) -> Vec<Notice> {
    let text = fs::read_to_string(app.join("notices.jsonl")).unwrap_or_default();
    let all: Vec<Notice> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            Some(Notice {
                ts: v.get("ts").and_then(Value::as_i64).unwrap_or(0),
                identity: v
                    .get("identity")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                exe: v
                    .get("exe")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                notice: v.get("notice").and_then(Value::as_str)?.into(),
            })
        })
        .collect();
    all[all.len().saturating_sub(n)..].to_vec()
}
