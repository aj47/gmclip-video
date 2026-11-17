//! Logging (A-02; SPEC §3.12, SR R-PROTO-3): `tracing` events as JSON lines in rolling
//! files, with **shape-only** content fields.
//!
//! - **Where.** `$GMCLIP_LOG_DIR`, else the OS log folder: `~/Library/Logs/gmclip`
//!   (macOS), `$XDG_STATE_HOME/gmclip/logs` or `~/.local/state/gmclip/logs` (Linux),
//!   `%LOCALAPPDATA%\gmclip\logs` (Windows). The folder is made (or tightened to) 0700,
//!   files 0600. `GMCLIP_LOG=off|error|warn|info|debug|trace` (any case; `0`, `none`,
//!   `false` mean off) sets the level, default `info`. Spans are always recorded, so every
//!   line at every level carries its `cmd` span's `trace_id`; the process's `trace_id` is
//!   also stamped on lines from threads or spans outside it.
//! - **Rolling.** One file per UTC day, `gmclip-YYYY-MM-DD.jsonl`, continued in
//!   `gmclip-YYYY-MM-DD.N.jsonl` past [`MAX_FILE_BYTES`]; a process always writes to the
//!   highest part of the day, never an older one. On open and on each roll, files older
//!   than [`KEEP_DAYS`] days are deleted, then the oldest until the folder holds at most
//!   [`MAX_TOTAL_BYTES`] (SPEC: 7 days, 50 MB). Only files with that name pattern are ever
//!   touched. Logging never fails a command: write errors are dropped.
//! - **Shape-only.** A string is logged by value only when its field *name* is on
//!   [`VALUE_FIELDS`], it was passed as a plain `&str` (not `%`/`?`), and it is
//!   identifier-shaped: at most 64 chars of `A-Za-z0-9._:~+@…-`, with `/` allowed only in
//!   `pointer` (an RP JSON Pointer). `location` keeps only the file name. Anything else is
//!   logged as its shape, `{"len": <chars>}`; numbers and booleans by value. Caption text
//!   (it has spaces) and paths therefore reach the log only as lengths, whatever the call
//!   site passes.
//! - **Messages** are logged as their shape in every build, because a message can
//!   interpolate anything (R-PROTO-3: "a debug log of a full session contains no
//!   transcript words"). Name an event with the allow-listed `event` field instead
//!   (`info!(event = "command.started")`). `GMCLIP_LOG_MESSAGES=1` keeps messages, with
//!   the home directory redacted, for local debugging only.
//! - **Redaction.** Kept strings have the home directory (`$HOME`, `%USERPROFILE%`, their
//!   canonical forms; ASCII case-insensitive, `/` and `\` equal) replaced by `~`.
//! - **Robustness.** Field values are formatted before any lock is taken, a log call made
//!   while logging (a `Debug` impl that logs or panics) is dropped instead of deadlocking,
//!   and poisoned locks are recovered.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// A day file rolls to a new part past this size.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// The log folder is pruned to this size (SPEC §3.12: 50 MB).
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// Day files older than this many days are deleted (SPEC §3.12: 7 days).
pub const KEEP_DAYS: i64 = 7;
/// Longest value of an allow-listed string field, in chars.
const MAX_VALUE_CHARS: usize = 64;

/// String fields that may be logged by value (see the module docs for the conditions):
/// identifiers and error vocabulary, never content.
pub const VALUE_FIELDS: &[&str] = &[
    "event",
    "trace_id",
    "request_id",
    "run",
    "caller",
    "cmd",
    "name",
    "code",
    "step",
    "pointer",
    "rule",
    "kind",
    "schema",
    "version",
    "location",
];

/// Line keys the subscriber writes itself; a span field of the same name is renamed
/// `span.<name>` so it cannot overwrite them.
const RESERVED: &[&str] = &["ts", "level", "target", "spans", "message", "fields"];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

thread_local! {
    /// Set while this thread is inside the subscriber: nested log calls are dropped.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` unless this thread is already logging.
fn guarded(f: impl FnOnce()) {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            BUSY.with(|b| b.set(false));
        }
    }
    let _reset = Reset;
    f();
}

/// What a log line may contain.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Home directories to redact (as `~`), longest first.
    pub homes: Vec<String>,
    /// Keep event messages (redacted) instead of logging their shape.
    pub keep_messages: bool,
}

impl Policy {
    /// The policy for this process: homes from the environment; messages kept only with
    /// `GMCLIP_LOG_MESSAGES=1`.
    pub fn from_env() -> Self {
        let homes = ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .collect();
        let keep = std::env::var("GMCLIP_LOG_MESSAGES").is_ok_and(|v| v.trim() == "1");
        Self::new(homes, keep)
    }

    /// A policy redacting `homes` (and their canonical forms). Homes of one character or
    /// less and filesystem roots (`/`, `C:\`) are ignored.
    pub fn new(homes: Vec<String>, keep_messages: bool) -> Self {
        let mut all: Vec<String> = Vec::new();
        let mut add = |h: String| {
            let root = Path::new(&h).parent().is_none();
            let h = h.trim_end_matches(['/', '\\']).to_owned();
            if !root && h.len() > 1 && !all.contains(&h) {
                all.push(h);
            }
        };
        for h in homes {
            if let Ok(c) = fs::canonicalize(&h) {
                // Windows canonical paths are verbatim (`\\?\C:\…`); logs carry `C:\…`
                let c = c.to_string_lossy();
                let plain = match c.strip_prefix(r"\\?\") {
                    Some(r) if r.as_bytes().get(1) == Some(&b':') => r,
                    _ => &c,
                };
                add(plain.to_owned());
            }
            add(h);
        }
        all.sort_by_key(|h| std::cmp::Reverse(h.len()));
        Self {
            homes: all,
            keep_messages,
        }
    }

    /// `s` with every home directory replaced by `~` (ASCII case-insensitive; `/` and `\`
    /// compare equal).
    pub fn redact(&self, s: &str) -> String {
        // Same byte length as the original, so offsets carry over.
        let fold = |x: &str| x.to_ascii_lowercase().replace('\\', "/");
        let mut out = s.to_owned();
        for h in &self.homes {
            let needle = fold(h);
            let mut i = 0;
            while let Some(at) = fold(&out)[i..].find(&needle).map(|p| p + i) {
                out.replace_range(at..at + needle.len(), "~");
                i = at + 1;
            }
        }
        out
    }

    /// A string field: by value (redacted) or as its shape.
    fn string(&self, name: &str, v: &str, plain: bool) -> Value {
        if name == "message" {
            return if self.keep_messages {
                Value::String(self.redact(v))
            } else {
                shape(v)
            };
        }
        if plain && VALUE_FIELDS.contains(&name) {
            let v = if name == "location" {
                // A source location: the file name and line only.
                v.rsplit(['/', '\\']).next().unwrap_or_default()
            } else {
                v
            };
            let v = self.redact(v);
            let ok_char = |c: char| {
                c.is_ascii_alphanumeric()
                    || "._:~+@-…".contains(c)
                    || (c == '/' && name == "pointer")
            };
            if !v.is_empty() && v.chars().count() <= MAX_VALUE_CHARS && v.chars().all(ok_char) {
                return Value::String(v);
            }
        }
        shape(v)
    }
}

fn shape(v: &str) -> Value {
    serde_json::json!({ "len": v.chars().count() })
}

/// Collects one event's or span's fields under the policy.
struct Fields<'a> {
    policy: &'a Policy,
    out: Map<String, Value>,
}

impl Visit for Fields<'_> {
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.out.insert(f.name().into(), v.into());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.out.insert(f.name().into(), v.into());
    }
    fn record_i128(&mut self, f: &Field, v: i128) {
        self.out.insert(f.name().into(), v.to_string().into());
    }
    fn record_u128(&mut self, f: &Field, v: u128) {
        self.out.insert(f.name().into(), v.to_string().into());
    }
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.out.insert(f.name().into(), v.into());
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.out.insert(f.name().into(), v.into());
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        let val = self.policy.string(f.name(), v, true);
        self.out.insert(f.name().into(), val);
    }
    fn record_error(&mut self, f: &Field, v: &(dyn std::error::Error + 'static)) {
        let val = self.policy.string(f.name(), &v.to_string(), false);
        self.out.insert(f.name().into(), val);
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        let val = self.policy.string(f.name(), &format!("{v:?}"), false);
        self.out.insert(f.name().into(), val);
    }
}

struct SpanRec {
    name: &'static str,
    meta: &'static Metadata<'static>,
    parent: Option<u64>,
    fields: Map<String, Value>,
    refs: usize,
}

/// A `tracing` subscriber that writes one JSON object per event.
pub struct JsonLog<W: Write + Send + 'static> {
    policy: Policy,
    max_level: Level,
    /// Stamped on every line that has no `trace_id` from a span.
    trace_id: Option<String>,
    out: Mutex<W>,
    spans: Mutex<HashMap<u64, SpanRec>>,
    stacks: Mutex<HashMap<ThreadId, Vec<u64>>>,
    next_id: AtomicU64,
}

impl<W: Write + Send + 'static> JsonLog<W> {
    pub fn new(out: W, policy: Policy, max_level: Level) -> Self {
        Self {
            policy,
            max_level,
            trace_id: None,
            out: Mutex::new(out),
            spans: Mutex::new(HashMap::new()),
            stacks: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Stamps `trace_id` on every line that does not get one from a span.
    pub fn with_trace_id(mut self, trace_id: &str) -> Self {
        self.trace_id = Some(trace_id.to_owned());
        self
    }

    fn current(&self) -> Option<u64> {
        lock(&self.stacks)
            .get(&std::thread::current().id())
            .and_then(|s| s.last().copied())
    }

    fn fields(&self, record: impl FnOnce(&mut Fields<'_>)) -> Map<String, Value> {
        let mut f = Fields {
            policy: &self.policy,
            out: Map::new(),
        };
        record(&mut f);
        f.out
    }

    /// Drops one reference to `id`, closing it (and releasing its parent) at zero.
    fn release(&self, id: u64) -> bool {
        let mut spans = lock(&self.spans);
        let mut next = Some(id);
        let mut closed_first = false;
        while let Some(i) = next.take() {
            let Some(rec) = spans.get_mut(&i) else { break };
            if rec.refs > 1 {
                rec.refs -= 1;
                break;
            }
            let parent = rec.parent;
            spans.remove(&i);
            if i == id {
                closed_first = true;
            }
            next = parent;
        }
        closed_first
    }
}

impl<W: Write + Send + 'static> Subscriber for JsonLog<W> {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        // Spans always (they carry trace_id); events by level.
        m.is_span() || *m.level() <= self.max_level
    }

    fn new_span(&self, a: &Attributes<'_>) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let parent = if a.is_root() {
            None
        } else {
            a.parent().map(Id::into_u64).or_else(|| self.current())
        };
        let mut fields = Map::new();
        guarded(|| fields = self.fields(|f| a.record(f)));
        let mut spans = lock(&self.spans);
        // A child keeps its parent alive, so its lines keep the parent's fields.
        let parent = parent.filter(|p| match spans.get_mut(p) {
            Some(rec) => {
                rec.refs += 1;
                true
            }
            None => false,
        });
        spans.insert(
            id,
            SpanRec {
                name: a.metadata().name(),
                meta: a.metadata(),
                parent,
                fields,
                refs: 1,
            },
        );
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        guarded(|| {
            let fields = self.fields(|f| values.record(f));
            if let Some(rec) = lock(&self.spans).get_mut(&span.into_u64()) {
                rec.fields.extend(fields);
            }
        });
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, e: &Event<'_>) {
        guarded(|| self.write_event(e));
    }

    fn enter(&self, span: &Id) {
        lock(&self.stacks)
            .entry(std::thread::current().id())
            .or_default()
            .push(span.into_u64());
    }

    fn exit(&self, span: &Id) {
        let mut stacks = lock(&self.stacks);
        let tid = std::thread::current().id();
        if let Some(stack) = stacks.get_mut(&tid) {
            if let Some(pos) = stack.iter().rposition(|&s| s == span.into_u64()) {
                stack.remove(pos);
            }
            if stack.is_empty() {
                stacks.remove(&tid);
            }
        }
    }

    fn clone_span(&self, span: &Id) -> Id {
        if let Some(rec) = lock(&self.spans).get_mut(&span.into_u64()) {
            rec.refs += 1;
        }
        span.clone()
    }

    fn try_close(&self, span: Id) -> bool {
        self.release(span.into_u64())
    }

    fn current_span(&self) -> tracing_core::span::Current {
        let found = self
            .current()
            .and_then(|id| lock(&self.spans).get(&id).map(|r| (id, r.meta)));
        match found {
            Some((id, meta)) => tracing_core::span::Current::new(Id::from_u64(id), meta),
            None => tracing_core::span::Current::none(),
        }
    }
}

impl<W: Write + Send + 'static> JsonLog<W> {
    fn write_event(&self, e: &Event<'_>) {
        let m = e.metadata();
        // Format the event's own fields before taking any lock.
        let mut fields = self.fields(|f| e.record(f));
        let mut line = Map::new();
        line.insert("ts".into(), rfc3339_now().into());
        line.insert("level".into(), m.level().as_str().into());
        line.insert("target".into(), self.policy.redact(m.target()).into());
        let parent = if e.is_root() {
            None
        } else {
            e.parent().map(Id::into_u64).or_else(|| self.current())
        };
        {
            let spans = lock(&self.spans);
            let mut chain = Vec::new();
            let mut at = parent;
            while let Some(id) = at {
                let Some(rec) = spans.get(&id) else { break };
                chain.push(rec);
                at = rec.parent;
            }
            let names: Vec<Value> = chain
                .iter()
                .rev()
                .map(|r| Value::from(self.policy.redact(r.name)))
                .collect();
            if !names.is_empty() {
                line.insert("spans".into(), names.into());
            }
            // Root first; nearer spans win on a name clash.
            for rec in chain.iter().rev() {
                for (k, v) in &rec.fields {
                    let key = if RESERVED.contains(&k.as_str()) {
                        format!("span.{k}")
                    } else {
                        k.clone()
                    };
                    line.insert(key, v.clone());
                }
            }
        }
        if !line.contains_key("trace_id")
            && let Some(t) = &self.trace_id
        {
            line.insert("trace_id".into(), t.as_str().into());
        }
        if let Some(msg) = fields.remove("message") {
            line.insert("message".into(), msg);
        }
        if !fields.is_empty() {
            line.insert("fields".into(), fields.into());
        }
        let mut bytes = Value::Object(line).to_string().into_bytes();
        bytes.push(b'\n');
        let mut out = lock(&self.out);
        let _ = out.write_all(&bytes).and_then(|()| out.flush());
    }
}

/// `GMCLIP_LOG` as a level; `None` means off.
pub fn level_from_env() -> Option<Level> {
    let v = std::env::var("GMCLIP_LOG").unwrap_or_default();
    match v.trim().to_ascii_lowercase().as_str() {
        "off" | "0" | "none" | "false" => None,
        "error" => Some(Level::ERROR),
        "warn" => Some(Level::WARN),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => Some(Level::INFO),
    }
}

/// The log folder for this OS (see the module docs).
pub fn default_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("GMCLIP_LOG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Logs/gmclip"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("gmclip").join("logs"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/state")))
            .map(|d| d.join("gmclip/logs"))
    }
}

/// Installs the JSON file logger as the global default, stamping `trace_id` on every
/// line. Silently does nothing when logging is off or the folder cannot be made: a
/// command never fails for its log.
pub fn init(trace_id: &str) {
    let Some(level) = level_from_env() else {
        return;
    };
    let Some(dir) = default_dir() else { return };
    let Ok(file) = RollingFile::open(&dir) else {
        return;
    };
    let log = JsonLog::new(file, Policy::from_env(), level).with_trace_id(trace_id);
    let _ = tracing::subscriber::set_global_default(log);
}

/// Day-rolled, size-capped log files (see the module docs).
#[derive(Debug)]
pub struct RollingFile {
    dir: PathBuf,
    day: i64,
    part: u32,
    file: File,
    written: u64,
}

impl RollingFile {
    pub fn open(dir: &Path) -> io::Result<Self> {
        create_private_dir(dir)?;
        Self::open_on(dir, today())
    }

    fn open_on(dir: &Path, day: i64) -> io::Result<Self> {
        // Continue the day's highest part, never an older one: pruning deletes oldest
        // first, so the newest lines must always be in the highest part.
        let mut part = highest_part(dir, day);
        prune(dir, day, Some(&dir.join(file_name(day, part))))?;
        loop {
            let path = dir.join(file_name(day, part));
            let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len < MAX_FILE_BYTES {
                let file = open_append(&path)?;
                return Ok(Self {
                    dir: dir.to_owned(),
                    day,
                    part,
                    file,
                    written: len,
                });
            }
            part += 1;
        }
    }

    /// The file being written.
    #[cfg(test)]
    fn path(&self) -> PathBuf {
        self.dir.join(file_name(self.day, self.part))
    }

    fn roll_if_needed(&mut self, incoming: u64) -> io::Result<()> {
        let day = today();
        if day != self.day {
            *self = Self::open_on(&self.dir, day)?;
        } else if self.written > 0 && self.written + incoming > MAX_FILE_BYTES {
            // Another process may already have rolled past us.
            let part = highest_part(&self.dir, self.day).max(self.part) + 1;
            let path = self.dir.join(file_name(self.day, part));
            self.file = open_append(&path)?;
            self.part = part;
            self.written = 0;
            prune(&self.dir, self.day, Some(&path))?;
        }
        Ok(())
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.roll_if_needed(buf.len() as u64)?;
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The highest part number of `day` present in `dir` (0 if none).
fn highest_part(dir: &Path, day: i64) -> u32 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().to_str().and_then(parse_name))
        .filter(|&(d, _)| d == day)
        .map(|(_, p)| p)
        .max()
        .unwrap_or(0)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // An existing folder keeps its mode otherwise.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A file made by an older build may be 0644.
        let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
    }
    Ok(f)
}

fn file_name(day: i64, part: u32) -> String {
    let (y, m, d) = civil(day);
    if part == 0 {
        format!("gmclip-{y:04}-{m:02}-{d:02}.jsonl")
    } else {
        format!("gmclip-{y:04}-{m:02}-{d:02}.{part}.jsonl")
    }
}

/// `(day, part)` of a file this module wrote, or `None` for any other name.
fn parse_name(name: &str) -> Option<(i64, u32)> {
    let rest = name.strip_prefix("gmclip-")?.strip_suffix(".jsonl")?;
    let (date, part) = match rest.split_once('.') {
        Some((d, p)) => (d, p.parse().ok()?),
        None => (rest, 0),
    };
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    (file_name(day, part) == name).then_some((day, part))
}

/// Deletes day files older than [`KEEP_DAYS`], then the oldest files until the folder's
/// log files total at most [`MAX_TOTAL_BYTES`]. `keep` is never deleted.
fn prune(dir: &Path, today: i64, keep: Option<&Path>) -> io::Result<()> {
    let mut files: Vec<((i64, u32), PathBuf, u64)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(key) = name.to_str().and_then(parse_name) else {
            continue;
        };
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        files.push((key, entry.path(), len));
    }
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    for (key, path, len) in &files {
        let expired = key.0 <= today - KEEP_DAYS;
        let over = total > MAX_TOTAL_BYTES;
        if (expired || over) && Some(path.as_path()) != keep && fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
    Ok(())
}

/// Days since 1970-01-01 (UTC).
fn today() -> i64 {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    s.div_euclid(86_400)
}

fn rfc3339_now() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, m, day) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60,
        d.subsec_millis()
    )
}

/// Civil date of a day number (Howard Hinnant's `civil_from_days`).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Day number of a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A shared in-memory sink.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    const HOME: &str = "/Users/redaction-test-home";
    const CAPTION: &str = "peeling itself secretly banana";

    /// Everything a careless call site might do with a path and a caption.
    fn emit_everything() {
        let path = format!("{HOME}/Movies/cut/plan.json");
        let span = tracing::info_span!("cmd", name = "plan.validate", trace_id = "01JTESTTRACE0000000000000", file = %path);
        let _g = span.enter();
        tracing::info!(text = CAPTION, path = %path, words = ?vec![CAPTION], "plain message");
        tracing::warn!("validating {path} with caption {CAPTION}");
        tracing::error!(
            step = "plan",
            pointer = "/text_events/3/runs/0/text",
            code = "Invalid",
            frames = 397_u64,
            ok = false,
            "refused"
        );
        tracing::info!(step = path.as_str(), name = CAPTION, kind = %"ok", "allow-listed fields carrying a path, a caption, a Display value");
        tracing::info!(target: "gmclip", location = "/USERS/REDACTION-TEST-HOME/.cargo/registry/x.rs:1:1", "panic");
    }

    fn lines(policy: Policy) -> (String, Vec<Value>) {
        let sink = Sink::default();
        let sub = JsonLog::new(sink.clone(), policy, Level::TRACE);
        tracing::subscriber::with_default(sub, emit_everything);
        let text = sink.text();
        let lines = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (text, lines)
    }

    #[test]
    fn release_logs_hold_no_home_paths_and_no_caption_text() {
        let (text, lines) = lines(Policy::new(vec![HOME.into()], false));
        assert_eq!(lines.len(), 5, "{text}");
        assert!(
            !text
                .to_ascii_lowercase()
                .contains(&HOME.to_ascii_lowercase()),
            "{text}"
        );
        for word in CAPTION.split(' ') {
            assert!(!text.contains(word), "caption word {word:?} leaked: {text}");
        }
        // Shapes, not values.
        assert_eq!(lines[0]["fields"]["text"], serde_json::json!({"len": 30}));
        assert_eq!(lines[0]["message"], serde_json::json!({"len": 13}));
        assert_eq!(lines[0]["file"], serde_json::json!({"len": 47}));
        // Allow-listed and numeric fields keep their values; span fields are on the line.
        assert_eq!(lines[2]["fields"]["pointer"], "/text_events/3/runs/0/text");
        assert_eq!(lines[2]["fields"]["frames"], 397);
        assert_eq!(lines[2]["fields"]["ok"], false);
        assert_eq!(lines[2]["trace_id"], "01JTESTTRACE0000000000000");
        assert_eq!(lines[2]["spans"], serde_json::json!(["cmd"]));
        assert_eq!(lines[2]["name"], "plan.validate");
        // Allow-listed names still log only identifier-shaped plain strings.
        assert_eq!(lines[3]["fields"]["step"], serde_json::json!({"len": 47}));
        assert_eq!(lines[3]["fields"]["name"], serde_json::json!({"len": 30}));
        assert_eq!(lines[3]["fields"]["kind"], serde_json::json!({"len": 2}));
        assert_eq!(lines[4]["fields"]["location"], "x.rs:1:1");
    }

    #[test]
    fn debug_messages_are_kept_but_redacted() {
        let (text, lines) = lines(Policy::new(vec![HOME.into()], true));
        assert!(!text.contains(HOME), "{text}");
        assert_eq!(lines[0]["message"], "plain message");
        let m = lines[1]["message"].as_str().unwrap();
        assert!(m.starts_with("validating ~/Movies/cut/plan.json"), "{m}");
        // Fields stay shape-only in every build.
        assert_eq!(lines[0]["fields"]["text"], serde_json::json!({"len": 30}));
    }

    fn capture(level: Level, trace: Option<&str>, f: impl FnOnce()) -> Vec<Value> {
        let sink = Sink::default();
        let mut sub = JsonLog::new(sink.clone(), Policy::new(vec![HOME.into()], false), level);
        if let Some(t) = trace {
            sub = sub.with_trace_id(t);
        }
        tracing::subscriber::with_default(sub, f);
        sink.text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A `Debug` impl that logs, then panics, while a span records it: no deadlock,
    /// and later lines still carry the span's fields.
    #[test]
    fn logging_or_panicking_inside_a_field_does_not_deadlock() {
        struct Nasty;
        impl std::fmt::Debug for Nasty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                tracing::warn!(event = "nested");
                f.write_str("x")?;
                panic!("boom in Debug")
            }
        }
        let lines = capture(Level::TRACE, None, || {
            let s = tracing::info_span!("cmd", trace_id = "T1", x = tracing::field::Empty);
            let _g = s.enter();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                s.record("x", tracing::field::debug(&Nasty));
            }));
            assert!(r.is_err());
            tracing::info!(event = "after");
        });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["fields"]["event"], "after");
        assert_eq!(lines[0]["trace_id"], "T1");
    }

    /// At warn level the info `cmd` span still exists, so warn lines carry its trace_id;
    /// a line from another thread gets the process trace_id; a child span outliving its
    /// parent keeps the parent's fields; span fields cannot overwrite line keys.
    #[test]
    fn every_line_carries_a_trace_id() {
        let lines = capture(Level::WARN, Some("PROC"), || {
            let s = tracing::info_span!("cmd", trace_id = "T2", level = "FAKE");
            let child = {
                let _g = s.enter();
                tracing::info!(event = "dropped by level");
                tracing::warn!(event = "kept");
                tracing::info_span!("work")
            };
            drop(s);
            child.in_scope(|| tracing::warn!(event = "child"));
            // A worker thread under the same (global, in the CLI) dispatcher.
            let d = tracing::dispatcher::get_default(Clone::clone);
            std::thread::scope(|t| {
                t.spawn(|| {
                    tracing::dispatcher::with_default(&d, || tracing::error!(event = "thread"))
                });
            });
        });
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0]["trace_id"], "T2");
        assert_eq!(lines[0]["level"], "WARN");
        assert_eq!(lines[0]["span.level"], serde_json::json!({"len": 4}));
        assert_eq!(
            lines[1]["trace_id"], "T2",
            "child keeps its parent: {}",
            lines[1]
        );
        assert_eq!(lines[1]["spans"], serde_json::json!(["cmd", "work"]));
        assert_eq!(lines[2]["trace_id"], "PROC");
    }

    #[test]
    fn values_are_cut_after_redaction_not_before() {
        let p = Policy::new(vec![HOME.into()], true);
        let long = format!("{}{HOME}/x", "a".repeat(245));
        assert!(!p.redact(&long).contains("Users"));
        assert_eq!(
            p.string("step", &long, true),
            serde_json::json!({"len": long.chars().count()})
        );
    }

    #[test]
    fn redaction_is_case_insensitive_and_repeated() {
        let p = Policy::new(vec!["/Users/aj/".into()], false);
        assert_eq!(p.redact("/users/AJ/a and /Users/aj/b"), "~/a and ~/b");
        let w = Policy::new(vec!["C:\\Users\\aj".into()], false);
        assert_eq!(
            w.redact("C:/Users/AJ/x and c:\\users\\aj\\y"),
            "~/x and ~\\y"
        );
        assert_eq!(
            p.redact("/Users/ajx"),
            "~x",
            "a prefix match is still redacted"
        );
        let none = Policy::new(vec!["/".into(), String::new()], false);
        assert!(none.homes.is_empty(), "{:?}", none.homes);
        #[cfg(windows)]
        {
            let none = Policy::new(vec!["C:\\".into(), r"\\?\C:\".into()], false);
            assert!(none.homes.is_empty(), "{:?}", none.homes);
            // the canonical form is kept without its verbatim prefix
            let home = std::env::var("USERPROFILE").unwrap();
            let p = Policy::new(vec![home.clone()], false);
            assert!(
                p.homes.iter().all(|h| !h.starts_with(r"\\?\")),
                "{:?}",
                p.homes
            );
            assert_eq!(p.redact(&format!(r"{home}\x")), r"~\x");
        }
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_740), (2026, 10, 14));
        for day in [-1000, 0, 59, 60, 11_016, 20_740, 50_000] {
            let (y, m, d) = civil(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        assert_eq!(parse_name("gmclip-2026-10-14.jsonl"), Some((20_740, 0)));
        assert_eq!(parse_name("gmclip-2026-10-14.3.jsonl"), Some((20_740, 3)));
        for other in [
            "gmclip-2026-10-14.txt",
            "notes.jsonl",
            "gmclip-2026-13-01.jsonl",
            "gmclip-2026-10-14.x.jsonl",
            "gmclip-2026-1-4.jsonl",
        ] {
            assert_eq!(parse_name(other), None, "{other}");
        }
    }

    #[test]
    fn prune_keeps_seven_days_and_fifty_megabytes_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("gmclip-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let today = 20_740;
        let touch = |name: String, len: u64| {
            let f = File::create(dir.join(name)).unwrap();
            f.set_len(len).unwrap();
        };
        touch(file_name(today - 7, 0), 10); // expired
        touch(file_name(today - 6, 0), 10); // in the 7 days, but oldest → goes for size
        touch(file_name(today - 1, 0), 30 * 1024 * 1024); // over 50 MB → goes too
        touch(file_name(today - 1, 1), 30 * 1024 * 1024);
        touch(file_name(today, 0), 1);
        touch("unrelated.jsonl".into(), 10);
        // Under the size cap, only age prunes.
        touch(file_name(today - 8, 2), 10);
        let small = dir.join("small");
        fs::create_dir_all(&small).unwrap();
        File::create(small.join(file_name(today - 7, 0))).unwrap();
        File::create(small.join(file_name(today - 6, 0))).unwrap();
        prune(&small, today, None).unwrap();
        assert_eq!(fs::read_dir(&small).unwrap().count(), 1);
        fs::remove_dir_all(&small).unwrap();
        prune(&dir, today, None).unwrap();
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                file_name(today - 1, 1),
                file_name(today, 0),
                "unrelated.jsonl".to_owned()
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rolling_file_rolls_past_the_size_cap() {
        let dir = std::env::temp_dir().join(format!("gmclip-roll-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A stale part 0 is never written again once part 2 exists.
        fs::create_dir_all(&dir).unwrap();
        File::create(dir.join(file_name(today(), 2))).unwrap();
        let mut f = RollingFile::open(&dir).unwrap();
        assert!(
            f.path().to_string_lossy().ends_with(".2.jsonl"),
            "{:?}",
            f.path()
        );
        let first = f.path();
        f.written = MAX_FILE_BYTES - 1;
        f.write_all(b"next line\n").unwrap();
        assert_ne!(f.path(), first);
        assert!(f.path().to_string_lossy().ends_with(".3.jsonl"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(f.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
