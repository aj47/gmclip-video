//! Chat logs (L.03): a Twitch or YouTube chat replay put on the stream's timeline, its
//! spikes found against the stream's own baseline, and each spike a moment candidate.
//!
//! - **Formats**: TwitchDownloader's chat JSON (`comments[]` with `content_offset_seconds`,
//!   `commenter`, `message.body`/`fragments`; also what other Twitch chat downloaders write
//!   in the same shape) and yt-dlp's `live_chat.json` (JSON Lines of
//!   `replayChatItemAction{videoOffsetTimeMsec, actions[].addChatItemAction.item}` with
//!   text, Super Chat, sticker and membership renderers). A log is detected by its shape.
//! - **Storage**: the messages as an immutable analysis blob (`kind: chat`, revision
//!   `chat:<sha256 prefix of the file>`) on the project's source or sync group; where it
//!   sits on the timeline (`<home>/chat.json`: the sync model's offset plus a manual
//!   nudge) is a setting beside it, so a nudge never rewrites the messages.
//! - **Offset via the sync model**: a log names the sync member it was recorded against
//!   (the VOD); its time 0 is that member's time 0, so on the group's clock a message is at
//!   `t + offset(member)` (SPEC §4.3: member = group − offset). A source on its own: 0.
//! - **Spikes**: 5 s bins of message count and of reaction messages (laughs, "pog", "W",
//!   "clip it", 😂…), each summed over 15 s, then a robust z-score against **this
//!   stream's own** bins (median and MAD; never a fixed count): a spike is a local peak at
//!   ≥ 3 robust deviations, at most one per 30 s. Chat reacts late, so the moment a spike
//!   points at is the 20 s before the burst began (snapped to sentences when made).
//! - **Untrusted**: chat text is someone else's words. It is canonicalised on import
//!   (`gmclip_mcp::canon`), rendered as text in the UI, and reaches agents only inside
//!   untrusted-marked fields (R-INJ-1, R-INJ-6).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmclip_mcp::canon::{self, Kind};
use gmclip_model::Id;
use gmclip_model::entities::{MediaRef, SyncMember, SyncSegment};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::FLICKS;

/// Largest chat log read (a long stream's Twitch JSON is tens of MB).
pub const MAX_BYTES: u64 = 256 << 20;
/// Messages kept from one log.
pub const MAX_MESSAGES: usize = 500_000;
const BIN_MS: i64 = 5_000;
const SPIKE_Z: f64 = 3.0;

/// One chat message on the log's own clock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Msg {
    pub t_ms: i64,
    pub author: String,
    pub text: String,
    /// `text`, `paid` (Super Chat, bits), `member`, `sticker`.
    pub kind: String,
}

/// Which tool wrote the log.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Twitch,
    Youtube,
}

fn clean(s: &str, max: usize) -> String {
    let c = canon::canonical(s, Kind::Text);
    let c = c.trim();
    if c.chars().count() > max {
        c.chars().take(max).collect()
    } else {
        c.to_owned()
    }
}

fn twitch(v: &Value) -> Vec<Msg> {
    let mut out = Vec::new();
    for c in v["comments"].as_array().into_iter().flatten() {
        let Some(t) = c["content_offset_seconds"]
            .as_f64()
            .filter(|t| t.is_finite())
        else {
            continue;
        };
        let author = c["commenter"]["display_name"]
            .as_str()
            .or_else(|| c["commenter"]["name"].as_str())
            .unwrap_or("");
        let m = &c["message"];
        let text = match m["body"].as_str() {
            Some(b) => b.to_owned(),
            None => m["fragments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["text"].as_str())
                .collect::<String>(),
        };
        let kind = if m["bits_spent"].as_i64().unwrap_or(0) > 0 {
            "paid"
        } else {
            "text"
        };
        out.push(Msg {
            t_ms: (t * 1000.0).round() as i64,
            author: clean(author, 60),
            text: clean(&text, 500),
            kind: kind.into(),
        });
        if out.len() >= MAX_MESSAGES {
            break;
        }
    }
    out
}

fn yt_runs(runs: &Value) -> String {
    runs["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            if let Some(t) = r["text"].as_str() {
                t.to_owned()
            } else if let Some(e) = r.get("emoji") {
                // a standard emoji's id is the character itself; custom ones by shortcut
                let id = e["emojiId"].as_str().unwrap_or("");
                if id.chars().count() <= 2 && !id.is_empty() {
                    id.to_owned()
                } else {
                    e["shortcuts"][0].as_str().unwrap_or("").to_owned()
                }
            } else {
                String::new()
            }
        })
        .collect()
}

fn youtube(text: &str) -> Vec<Msg> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let r = &v["replayChatItemAction"];
        let Some(t) = r["videoOffsetTimeMsec"]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| r["videoOffsetTimeMsec"].as_i64())
        else {
            continue;
        };
        for a in r["actions"].as_array().into_iter().flatten() {
            let item = &a["addChatItemAction"]["item"];
            for (key, kind) in [
                ("liveChatTextMessageRenderer", "text"),
                ("liveChatPaidMessageRenderer", "paid"),
                ("liveChatPaidStickerRenderer", "sticker"),
                ("liveChatMembershipItemRenderer", "member"),
            ] {
                let m = &item[key];
                if m.is_null() {
                    continue;
                }
                let body = if m.get("message").is_some() {
                    yt_runs(&m["message"])
                } else {
                    yt_runs(&m["headerSubtext"])
                };
                out.push(Msg {
                    t_ms: t,
                    author: clean(m["authorName"]["simpleText"].as_str().unwrap_or(""), 60),
                    text: clean(&body, 500),
                    kind: kind.into(),
                });
            }
        }
        if out.len() >= MAX_MESSAGES {
            break;
        }
    }
    out
}

/// Reads a chat log: its format and its messages in time order.
pub fn parse(bytes: &[u8]) -> Result<(Format, Vec<Msg>), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the chat log is not UTF-8 text")?;
    let head = text.trim_start();
    // Twitch: one JSON object with comments[]
    if head.starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(head)
        && v.get("comments").is_some_and(Value::is_array)
    {
        let mut m = twitch(&v);
        m.sort_by_key(|x| x.t_ms);
        return Ok((Format::Twitch, m));
    }
    if head.contains("replayChatItemAction") {
        let mut m = youtube(text);
        m.sort_by_key(|x| x.t_ms);
        return Ok((Format::Youtube, m));
    }
    Err(
        "not a chat log gmclip reads: a TwitchDownloader chat JSON or yt-dlp's live_chat.json"
            .into(),
    )
}

// ---- reactions and spikes ----------------------------------------------------------

/// A token with runs of one character collapsed ("LOOOOL" → "lol").
fn squash(t: &str) -> String {
    let mut out = String::new();
    let mut last = None;
    for c in t.chars().flat_map(char::to_lowercase) {
        if Some(c) != last {
            out.push(c);
        }
        last = Some(c);
    }
    out
}

const REACT_WORDS: &[&str] = &[
    "lol",
    "lmao",
    "lmfao",
    "rofl",
    "ha",
    "haha",
    "hahaha",
    "xd",
    "kekw",
    "kek",
    "omegalul",
    "lul",
    "lulw",
    "pog",
    "pogers",
    "pogchamp",
    "pogies",
    "pogu",
    "w",
    "gg",
    "ez",
    "wtf",
    "omg",
    "omfg",
    "holy",
    "wow",
    "clip",
    "clipit",
    "monkas",
    "monkaw",
    "pepelaugh",
    "pepehands",
    "sadge",
    "f",
    "rip",
    "hype",
    "hypers",
    "insane",
    "crazy",
    "bruh",
    "nice",
    "lesgo",
    "letsgo",
    "no",
];

/// Whether a message reacts (a laugh, hype, shock, "clip it", a reaction emoji, ??? or !!!).
pub fn is_reaction(text: &str) -> bool {
    if [
        "😂", "🤣", "💀", "😱", "🔥", "😮", "😳", "👏", "🤯", "😭", "❗", "‼",
    ]
    .iter()
    .any(|e| text.contains(e))
    {
        return true;
    }
    if text.contains("???") || text.contains("!!!") {
        return true;
    }
    let lower = text.to_lowercase();
    if lower.contains("no way")
        || lower.contains("let's go")
        || lower.contains("lets go")
        || lower.contains("clip that")
        || lower.contains("clip it")
    {
        return true;
    }
    let toks: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(squash)
        .collect();
    // short messages that are reactions ("W", "LUL LUL", "F"); a long one needs a laugh
    toks.len() <= 6
        && toks.iter().any(|t| {
            (REACT_WORDS.contains(&t.as_str()) && (t.len() > 1 || toks.len() <= 2))
                || ["haha", "hehe", "lmao", "lolol"]
                    .iter()
                    .any(|p| t.starts_with(p))
        })
}

/// One bin of the ribbon (source clock).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bin {
    pub start_ms: i64,
    pub count: u32,
    pub react: u32,
    pub z_rate: f64,
    pub z_react: f64,
}

/// One spike.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Spike {
    /// The peak bin's start and the burst's start/end (source clock, ms).
    pub peak_ms: i64,
    pub burst_start_ms: i64,
    pub burst_end_ms: i64,
    /// What the spike points at: the 20 s before the burst began, to 3 s into it.
    pub moment_start_ms: i64,
    pub moment_end_ms: i64,
    pub z_rate: f64,
    pub z_react: f64,
    /// Messages in the 15 s at the peak, and that over the stream's median.
    pub messages: u32,
    pub ratio: f64,
    /// The reactions most said there ("lul ×12").
    pub top: Vec<(String, u32)>,
    /// A few messages from the burst (untrusted).
    pub sample: Vec<String>,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Robust z-scores of a series against its own median and MAD; the scale never falls
/// under the Poisson spread of the median (sparse chats stay quiet).
fn robust_z(x: &[f64]) -> (Vec<f64>, f64) {
    let mut a = x.to_vec();
    let med = median(&mut a);
    let mut dev: Vec<f64> = x.iter().map(|v| (v - med).abs()).collect();
    let mad = median(&mut dev);
    let scale = (1.4826 * mad).max(med.max(1.0).sqrt());
    (x.iter().map(|v| (v - med) / scale).collect(), med)
}

/// The ribbon and the spikes of messages already on the source clock (`t_ms` shifted).
pub fn analyse(msgs: &[Msg]) -> (Vec<Bin>, Vec<Spike>) {
    let Some(first) = msgs.first().map(|m| m.t_ms.max(0)) else {
        return (Vec::new(), Vec::new());
    };
    let last = msgs.last().map_or(first, |m| m.t_ms);
    let b0 = first.div_euclid(BIN_MS);
    let n = ((last.div_euclid(BIN_MS) - b0) + 1).clamp(1, 1_000_000) as usize;
    let mut count = vec![0u32; n];
    let mut react = vec![0u32; n];
    for m in msgs.iter().filter(|m| m.t_ms >= 0) {
        let i = (m.t_ms.div_euclid(BIN_MS) - b0) as usize;
        if i < n {
            count[i] += 1;
            if is_reaction(&m.text) {
                react[i] += 1;
            }
        }
    }
    let smooth = |v: &[u32]| -> Vec<f64> {
        (0..n)
            .map(|i| {
                let lo = i.saturating_sub(1);
                let hi = (i + 1).min(n - 1);
                v[lo..=hi].iter().map(|&x| f64::from(x)).sum()
            })
            .collect()
    };
    let (sr, sx) = (smooth(&count), smooth(&react));
    let (zr, med_r) = robust_z(&sr);
    let (zx, _) = robust_z(&sx);
    let bins: Vec<Bin> = (0..n)
        .map(|i| Bin {
            start_ms: (b0 + i as i64) * BIN_MS,
            count: count[i],
            react: react[i],
            z_rate: (zr[i] * 100.0).round() / 100.0,
            z_react: (zx[i] * 100.0).round() / 100.0,
        })
        .collect();
    let score: Vec<f64> = (0..n).map(|i| zr[i].max(zx[i])).collect();
    // local peaks over the threshold, strongest first, one per 30 s
    let mut peaks: Vec<usize> = (0..n)
        .filter(|&i| score[i] >= SPIKE_Z)
        .filter(|&i| {
            (i == 0 || score[i] >= score[i - 1]) && (i + 1 == n || score[i] >= score[i + 1])
        })
        .collect();
    peaks.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let mut kept: Vec<usize> = Vec::new();
    for p in peaks {
        if kept
            .iter()
            .all(|&k| (k as i64 - p as i64).unsigned_abs() as i64 * BIN_MS >= 30_000)
        {
            kept.push(p);
        }
        if kept.len() >= 50 {
            break;
        }
    }
    let spikes = kept
        .into_iter()
        .map(|p| {
            // the burst: out from the peak while the score stays over 1.5
            let mut a = p;
            while a > 0 && score[a - 1] >= 1.5 {
                a -= 1;
            }
            let mut b = p;
            while b + 1 < n && score[b + 1] >= 1.5 {
                b += 1;
            }
            let (s0, s1) = ((b0 + a as i64) * BIN_MS, (b0 + b as i64 + 1) * BIN_MS);
            let mut words: BTreeMap<String, u32> = BTreeMap::new();
            let mut reacts = Vec::new();
            let mut others = Vec::new();
            for m in msgs.iter().filter(|m| m.t_ms >= s0 && m.t_ms < s1) {
                if m.text.is_empty() {
                    continue;
                }
                if is_reaction(&m.text) {
                    for t in m.text.split_whitespace().take(3) {
                        *words.entry(squash(t)).or_default() += 1;
                    }
                    reacts.push(m.text.clone());
                } else {
                    others.push(m.text.clone());
                }
            }
            // what the burst said: its reactions first, then the rest
            let sample: Vec<String> = reacts.into_iter().chain(others).take(5).collect();
            let mut top: Vec<(String, u32)> = words.into_iter().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            top.truncate(3);
            Spike {
                peak_ms: (b0 + p as i64) * BIN_MS,
                burst_start_ms: s0,
                burst_end_ms: s1,
                moment_start_ms: (s0 - 20_000).max(0),
                moment_end_ms: s0 + 3_000,
                z_rate: (zr[p] * 100.0).round() / 100.0,
                z_react: (zx[p] * 100.0).round() / 100.0,
                messages: sr[p] as u32,
                ratio: ((sr[p] / med_r.max(1.0)) * 10.0).round() / 10.0,
                top,
                sample,
            }
        })
        .collect();
    (bins, spikes)
}

// ---- per-project chat logs ------------------------------------------------------------

/// Where one imported log sits (`<home>/chat.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub owner: String,
    pub revision: String,
    pub format: Format,
    pub file_name: String,
    pub messages: usize,
    /// The sync member the log was recorded against (its time 0 is the log's).
    #[serde(default)]
    pub member: Option<String>,
    /// From the sync model: the member's offset on the group's clock.
    pub base_offset_ms: i64,
    /// The human's nudge on top.
    pub nudge_ms: i64,
}

fn settings_path(home: &Path) -> PathBuf {
    home.join("chat.json")
}

fn load_logs(home: &Path) -> Vec<LogEntry> {
    std::fs::read(settings_path(home))
        .ok()
        .filter(|b| b.len() < 1 << 20)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| serde_json::from_value(v["logs"].clone()).ok())
        .unwrap_or_default()
}

fn save_logs(home: &Path, logs: &[LogEntry]) -> AppResult<()> {
    let b = serde_json::to_vec_pretty(&json!({"schema": "gmclip.chat-logs/0", "logs": logs}))
        .map_err(|e| AppError::internal(e.to_string()))?;
    crate::core::write_atomic(&settings_path(home), &b)
        .map_err(|e| AppError::internal(format!("saving chat.json: {}", e.kind())))
}

fn owner_of(m: &MediaRef) -> String {
    match m {
        MediaRef::Source(s) => s.0.to_string(),
        MediaRef::Group(g) => g.0.to_string(),
    }
}

/// The project's only media, or the one named.
fn media_of(doc: &gmclip_model::Doc, media: Option<&Value>) -> AppResult<MediaRef> {
    if let Some(m) = media.filter(|m| !m.is_null()) {
        return serde_json::from_value(m.clone())
            .map_err(|_| AppError::invalid("`media` is {\"source\": id} or {\"group\": id}"));
    }
    let groups: Vec<_> = doc.iter::<gmclip_model::entities::SyncGroup>().collect();
    if let [g] = groups.as_slice() {
        return Ok(MediaRef::Group(g.id));
    }
    let sources: Vec<_> = doc.iter::<gmclip_model::entities::Source>().collect();
    match (groups.len(), sources.as_slice()) {
        (0, [s]) => Ok(MediaRef::Source(s.id)),
        _ => Err(AppError::invalid(
            "name the recording the chat belongs to (`media`)",
        )),
    }
}

/// The member's offset on its group's clock (member time = group − offset).
fn member_offset_ms(doc: &gmclip_model::Doc, member: &str) -> AppResult<i64> {
    let id: Id<SyncMember> = crate::planner::ulid_id(member)?;
    doc.get(id)
        .ok_or_else(|| AppError::not_found("no such sync member"))?;
    let mut segs: Vec<&SyncSegment> = doc
        .children::<SyncSegment>(id.entity())
        .filter_map(|s| doc.get(s))
        .collect();
    segs.sort_by_key(|s| s.src_range.start);
    Ok(segs.first().map_or(0, |s| s.offset.0 * 1000 / FLICKS))
}

/// Imports a chat log onto a project's recording.
pub fn import(
    core: &Arc<AppCore>,
    project: &str,
    path: &Path,
    media: Option<&Value>,
    member: Option<&str>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let meta = std::fs::metadata(path).map_err(|_| AppError::not_found("no file there"))?;
    if !meta.is_file() {
        return Err(AppError::invalid("not a file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(AppError::invalid("the chat log is over 256 MB"));
    }
    let bytes =
        std::fs::read(path).map_err(|e| AppError::internal(format!("reading: {}", e.kind())))?;
    let (format, msgs) = parse(&bytes).map_err(AppError::invalid)?;
    if msgs.is_empty() {
        return Err(AppError::invalid("the chat log has no messages"));
    }
    let doc = p.handle.snapshot().doc.clone();
    let media = media_of(&doc, media)?;
    let owner = owner_of(&media);
    let base = match member {
        Some(m) => member_offset_ms(&doc, m)?,
        None => 0,
    };
    let (sha, _) = gmclip_ml::sha256::file_hex(path)
        .map_err(|_| AppError::internal("hashing the chat log"))?;
    let revision = format!("chat:{}", &sha[..16]);
    let rows: Vec<Value> = msgs
        .iter()
        .map(|m| json!([m.t_ms, m.author, m.text, m.kind]))
        .collect();
    let body = serde_json::to_vec(
        &json!({"schema": "gmclip.analysis.chat/0", "format": format,
                                          "untrusted": true, "messages": rows}),
    )
    .map_err(|e| AppError::internal(e.to_string()))?;
    p.handle
        .put_analysis(&owner, "chat", &revision, body)
        .map_err(|_| AppError::internal("saving the chat log"))?;
    let file_name: String = path
        .file_name()
        .map(|f| canon::canonical(&f.to_string_lossy(), Kind::Name))
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    let mut logs = load_logs(&p.home);
    logs.retain(|l| l.owner != owner);
    logs.push(LogEntry {
        owner: owner.clone(),
        revision,
        format,
        file_name,
        messages: msgs.len(),
        member: member.map(str::to_owned),
        base_offset_ms: base,
        nudge_ms: 0,
    });
    save_logs(&p.home, &logs)?;
    view(core, project)
}

/// A log's messages on its owner's clock.
fn shifted(core: &AppCore, project: &str, log: &LogEntry) -> AppResult<Vec<Msg>> {
    let p = core.project(project)?;
    let body = p
        .handle
        .analysis(&log.owner, "chat", &log.revision)
        .ok_or_else(|| AppError::not_found("the chat log's messages are gone"))?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| AppError::internal("chat blob"))?;
    let off = log.base_offset_ms + log.nudge_ms;
    Ok(v["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Msg {
                t_ms: r[0].as_i64()? + off,
                author: r[1].as_str()?.to_owned(),
                text: r[2].as_str()?.to_owned(),
                kind: r[3].as_str()?.to_owned(),
            })
        })
        .collect())
}

fn spike_json(s: &Spike) -> Value {
    json!({
        "peak_s": s.peak_ms as f64 / 1000.0, "burst": [s.burst_start_ms as f64 / 1000.0, s.burst_end_ms as f64 / 1000.0],
        "start_s": s.moment_start_ms as f64 / 1000.0, "end_s": s.moment_end_ms as f64 / 1000.0,
        "z_rate": s.z_rate, "z_react": s.z_react, "messages": s.messages, "ratio": s.ratio,
        "top": s.top, "sample": s.sample,
    })
}

/// The project's chat logs with their spikes (source seconds) and ribbons.
pub fn view(core: &AppCore, project: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let mut out = Vec::new();
    for log in load_logs(&p.home) {
        let msgs = shifted(core, project, &log)?;
        let (bins, spikes) = analyse(&msgs);
        out.push(json!({
            "owner": log.owner, "media": crate::libsearch::media_json(
                if p.handle.snapshot().doc.get(gmclip_model::Id::<gmclip_model::entities::SyncGroup>::new(
                    gmclip_model::Ulid::parse(&log.owner).map_err(|_| AppError::internal("owner"))?)).is_some() { "group" } else { "source" },
                &log.owner),
            "format": log.format, "file_name": log.file_name, "messages": log.messages,
            "member": log.member, "base_offset_ms": log.base_offset_ms, "nudge_ms": log.nudge_ms,
            "bin_ms": BIN_MS, "bins": bins.len(),
            "spikes": spikes.iter().map(spike_json).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({"logs": out}))
}

/// Moves a log on the timeline by `nudge_ms` (absolute, ±10 min).
pub fn nudge(core: &AppCore, project: &str, owner: &str, nudge_ms: i64) -> AppResult<Value> {
    if nudge_ms.abs() > 600_000 {
        return Err(AppError::invalid("a nudge of at most ±10 minutes"));
    }
    let p = core.project(project)?;
    let mut logs = load_logs(&p.home);
    let l = logs
        .iter_mut()
        .find(|l| l.owner == owner)
        .ok_or_else(|| AppError::not_found("no chat log on that recording"))?;
    l.nudge_ms = nudge_ms;
    save_logs(&p.home, &logs)?;
    view(core, project)
}

/// The ribbon under a cut: per output stretch, the bins its parts play.
pub fn ribbon(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    use gmclip_model::entities::{Cut, Part};
    let p = core.project(project)?;
    let logs = load_logs(&p.home);
    if logs.is_empty() {
        return Ok(json!({"bins": [], "spikes": []}));
    }
    let doc = p.handle.snapshot().doc.clone();
    let cid: Id<Cut> = crate::planner::ulid_id(cut)?;
    let fpf = core
        .planned(project, cut)
        .map_or(FLICKS / 30, |pl| crate::planner::fpf(&pl.plan));
    let layout = gmclip_model::clock::cut_layout(&doc, cid);
    let mut bins_out = Vec::new();
    let mut spikes_out = Vec::new();
    for log in &logs {
        let msgs = shifted(core, project, log)?;
        let (bins, spikes) = analyse(&msgs);
        for sp in &layout {
            let Some(pt) = doc.get::<Part>(sp.part) else {
                continue;
            };
            if owner_of(&pt.media) != log.owner {
                continue;
            }
            let (a, b) = (pt.src_in.0 * 1000 / FLICKS, pt.src_out.0 * 1000 / FLICKS);
            let out0 = sp.start.0 * 1000 / FLICKS;
            let to_frame = |src_ms: i64| (out0 + src_ms - a) * (FLICKS / 1000) / fpf;
            for bin in bins
                .iter()
                .filter(|x| x.start_ms + BIN_MS > a && x.start_ms < b)
            {
                let s = bin.start_ms.max(a);
                let e = (bin.start_ms + BIN_MS).min(b);
                bins_out.push(json!([
                    to_frame(s),
                    to_frame(e),
                    bin.z_rate.max(0.0),
                    bin.z_react.max(0.0)
                ]));
            }
            for s in spikes.iter().filter(|s| s.peak_ms >= a && s.peak_ms < b) {
                let mut j = spike_json(s);
                j["frame"] = json!(to_frame(s.peak_ms));
                j["owner"] = json!(log.owner);
                spikes_out.push(j);
            }
        }
    }
    let frames: u64 = layout.iter().map(|s| (s.dur.0 / fpf) as u64).sum();
    Ok(json!({"frames": frames, "bins": bins_out, "spikes": spikes_out}))
}

/// "＋ Moment" on a spike: the 20 s before the burst, snapped to sentences, origin `chat`.
pub fn moment(
    core: &AppCore,
    project: &str,
    owner: &str,
    start_s: f64,
    end_s: f64,
    note: &str,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let u = gmclip_model::Ulid::parse(owner).map_err(|_| AppError::invalid("owner"))?;
    let media = if doc
        .get(gmclip_model::Id::<gmclip_model::entities::SyncGroup>::new(
            u,
        ))
        .is_some()
    {
        json!({"group": owner})
    } else {
        json!({"source": owner})
    };
    let r = core.call(
        project,
        "moment_mark",
        json!({"media": media, "start_s": start_s, "end_s": end_s, "snap": "sentence", "from": "chat",
               "note": note.chars().take(200).collect::<String>()}),
    )?;
    Ok(r["output"].clone())
}

/// `chat_spikes` for an agent: every chat text inside untrusted-marked fields.
pub fn spikes_for_agent(core: &AppCore, project: &str, args: &Value) -> AppResult<Value> {
    let v = view(core, project)?;
    let want = args.get("media").and_then(Value::as_str);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50) as usize;
    let log = v["logs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|l| want.is_none_or(|w| l["owner"] == w))
        .ok_or_else(|| AppError::not_found("no chat log imported on this project"))?;
    let mut display = Vec::new();
    let spikes: Vec<Value> = log["spikes"]
        .as_array()
        .into_iter()
        .flatten()
        .take(limit)
        .map(|s| {
            let sample: Vec<Value> = s["sample"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|t| canon::field(t, Kind::Text))
                .collect();
            let top: Vec<Value> = s["top"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| json!({"word": canon::field(x[0].as_str().unwrap_or(""), Kind::Text), "n": x[1]}))
                .collect();
            display.push(format!(
                "- peak {:.0} s: {}× the usual rate (z {} / reactions z {}) — {}",
                s["peak_s"].as_f64().unwrap_or(0.0),
                s["ratio"],
                s["z_rate"],
                s["z_react"],
                canon::block(
                    &s["sample"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | "),
                    Kind::Text
                )
            ));
            json!({
                "peak_s": s["peak_s"], "burst": s["burst"], "z_rate": s["z_rate"], "z_react": s["z_react"],
                "ratio": s["ratio"], "messages": s["messages"], "top": top, "sample": sample,
                "moment_mark": {"media": log["media"], "start_s": s["start_s"], "end_s": s["end_s"],
                                "snap": "sentence", "from": "chat"},
            })
        })
        .collect();
    Ok(json!({
        "media": log["media"], "format": log["format"], "messages": log["messages"],
        "offset_ms": log["base_offset_ms"].as_i64().unwrap_or(0) + log["nudge_ms"].as_i64().unwrap_or(0),
        "spikes": spikes,
        "untrusted": ["spikes[].sample[]", "spikes[].top[].word"],
        "display": format!("{} spike(s) in the chat log (z against this stream's own chat)\n{}", spikes.len(), display.join("\n")),
    }))
}

/// `chat_import`, `chat_view`, `chat_nudge`, `chat_ribbon`, `chat_moment`.
pub fn ipc(
    ctx: &crate::ipc::Ctx,
    command: &str,
    project: Option<&str>,
    a: &Value,
) -> AppResult<Value> {
    let core = &ctx.core;
    let pid = project.ok_or_else(|| AppError::invalid("a project is needed"))?;
    let s = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid(format!("`{k}` is required")))
    };
    let n = |k: &str| {
        a.get(k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| AppError::invalid(format!("`{k}` is a number")))
    };
    match command {
        "chat_import" => import(
            core,
            pid,
            Path::new(s("path")?.trim()),
            a.get("media"),
            a.get("member").and_then(Value::as_str),
        ),
        "chat_view" => view(core, pid),
        "chat_nudge" => nudge(core, pid, s("owner")?, n("nudge_ms")? as i64),
        "chat_ribbon" => ribbon(core, pid, s("cut")?),
        "chat_moment" => moment(
            core,
            pid,
            s("owner")?,
            n("start_s")?,
            n("end_s")?,
            a.get("note").and_then(Value::as_str).unwrap_or(""),
        ),
        _ => Err(AppError::invalid("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions() {
        for t in [
            "LUL",
            "LOOOOL",
            "W",
            "KEKW KEKW",
            "😂😂",
            "clip it!",
            "no way",
            "???",
            "Pog",
        ] {
            assert!(is_reaction(t), "{t}");
        }
        for t in [
            "what game is this",
            "hello from brazil",
            "how much ram do you have",
            "w is my favourite letter of the alphabet ok",
        ] {
            assert!(!is_reaction(t), "{t}");
        }
    }

    #[test]
    fn robust_z_never_divides_by_a_tiny_spread() {
        let (z, med) = robust_z(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 9.0]);
        assert_eq!(med, 0.0);
        // scale floor 1 (√max(median, 1)): a 9-message bin is z 9, a 1-message bin z 1
        assert_eq!(z[6], 9.0);
        assert_eq!(z[4], 1.0);
    }
}
