//! The app's own log: what failed, as JSON lines in `app-YYYY-MM-DD.jsonl` (UTC day) in
//! the CLI's log folder (`$GMCLIP_LOG_DIR`, else `~/Library/Logs/gmclip` on macOS; see
//! `gmclip-cli`'s `log.rs`). A toast is gone once read; this keeps the failures a bug
//! report needs: the command and its error code, and the playback engine's "no sound"
//! reasons (plan pointers and rule names). Error messages can quote anything, so they are
//! kept only with `GMCLIP_LOG_MESSAGES=1` (local debugging), the home directory replaced
//! by `~`, as the CLI does (R-PROTO-3). `GMCLIP_LOG=off` turns it off. Writing never
//! fails a call; files older than [`KEEP_DAYS`] days are deleted when a day file opens.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Day files older than this many days are deleted.
pub const KEEP_DAYS: u64 = 7;

static DAY: Mutex<Option<u64>> = Mutex::new(None);

fn dir() -> Option<PathBuf> {
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
        home.map(|h| h.join(".local/state/gmclip/logs"))
    }
}

fn off() -> bool {
    std::env::var("GMCLIP_LOG").is_ok_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "off" | "0" | "none" | "false"
        )
    })
}

/// `YYYY-MM-DD` of a day since the epoch (civil from days, proleptic Gregorian).
fn ymd(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn redact(s: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => s.replace(&h, "~"),
        _ => s.to_owned(),
    }
}

fn prune(dir: &std::path::Path, today: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(day) = name
            .strip_prefix("app-")
            .and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if (0..KEEP_DAYS).all(|k| today.checked_sub(k).map(ymd).as_deref() != Some(day)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn write(line: Value) {
    if off() {
        return;
    }
    let Some(dir) = dir() else { return };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let today = now.as_secs() / 86_400;
    let Ok(mut day) = DAY.lock() else { return };
    if *day != Some(today) {
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        prune(&dir, today);
        *day = Some(today);
    }
    let mut line = line;
    line["ts_ms"] = json!(now.as_millis() as u64);
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    if let Ok(mut f) = o.open(dir.join(format!("app-{}.jsonl", ymd(today)))) {
        let _ = writeln!(f, "{line}");
    }
}

/// A call that failed: its command and error code (the message with
/// `GMCLIP_LOG_MESSAGES=1`).
pub fn failed(command: &str, code: &str, message: &str) {
    let mut l = json!({"event": "call.failed", "cmd": command, "code": code});
    if std::env::var_os("GMCLIP_LOG_MESSAGES").is_some_and(|v| v == "1") {
        l["message"] = json!(redact(message));
    }
    write(l);
}

/// An app event with a reason in the validator's or engine's own words (plan pointers
/// and rule names, never content).
pub fn event(event: &str, command: Option<&str>, reason: &str) {
    write(json!({"event": event, "cmd": command, "reason": redact(reason)}));
}

#[cfg(test)]
mod tests {
    use super::ymd;

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(19_782), "2024-02-29");
        assert_eq!(ymd(20_743), "2026-10-17");
    }
}
