//! The style feedback log (SR R-PROF-1/2; SPEC §10.5): `<profile>/feedback.jsonl`,
//! records `{date, text, export, author, via, untrusted: true}`.
//!
//! Appending is a **Human** action only. [`append_by_human`] is the one writer of new
//! human records and is called from no agent surface: not the MCP tools, not
//! `command_run`, not the CLI, not the review link (R-PROF-2; the M2 review board's
//! "Add to my style feedback" is its only caller, and it shows invisible characters
//! first, R-INJ-6). Records are read back for `help{topic:"style"}` marked untrusted with their author.

use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The log's file name in a profile folder.
pub const FEEDBACK_FILE: &str = "feedback.jsonl";
/// The longest record text kept.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// How many records `help{topic:"style"}` serves.
pub const SERVED: usize = 50;

/// One feedback record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// `YYYY-MM-DD` (as given, or the day it was added).
    pub date: String,
    pub text: String,
    /// The export it is about (a path or export id), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    /// The human who added it.
    pub author: String,
    /// Where the text came from: `app`, `review-link:<id>`, `agent-run:<run>`.
    pub via: String,
    /// Always true: feedback text is served marked untrusted.
    pub untrusted: bool,
}

fn valid_via(v: &str) -> bool {
    v == "app"
        || v.strip_prefix("review-link:")
            .is_some_and(|x| !x.is_empty() && x.len() <= 80)
        || v.strip_prefix("agent-run:")
            .is_some_and(|x| !x.is_empty() && x.len() <= 80)
}

fn write_line(dir: &Path, r: &Record) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FEEDBACK_FILE))
        .map_err(|e| format!("open feedback.jsonl ({})", e.kind()))?;
    let line = serde_json::to_string(r).map_err(|_| "encode the record".to_owned())?;
    writeln!(f, "{line}").map_err(|e| format!("append ({})", e.kind()))
}

/// Appends a record a human just added in the app (see the module docs: the M2 review
/// board is the only caller; no agent surface reaches this).
pub fn append_by_human(
    profile_dir: &Path,
    date: &str,
    text: &str,
    export: Option<&str>,
    author: &str,
    via: &str,
) -> Result<Record, String> {
    if !valid_via(via) {
        return Err("`via` must be app, review-link:<id> or agent-run:<run>".into());
    }
    if text.trim().is_empty() {
        return Err("empty feedback".into());
    }
    let r = Record {
        date: date.chars().take(10).collect(),
        text: text.chars().take(MAX_TEXT_CHARS).collect(),
        export: export.map(|e| e.chars().take(300).collect()),
        author: author.chars().take(80).collect(),
        via: via.to_owned(),
        untrusted: true,
    };
    write_line(profile_dir, &r)?;
    Ok(r)
}

/// The last `n` records (unreadable lines skipped; at most the last 4 MiB read).
pub fn last(profile_dir: &Path, n: usize) -> Vec<Record> {
    // a regular file only: no symlink (to /dev/zero…), no FIFO (R-PROF-1 confinement)
    let path = profile_dir.join(FEEDBACK_FILE);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return Vec::new();
    };
    if !meta.is_file() {
        return Vec::new();
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(super::style::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(super::style::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let Ok(f) = o.open(&path) else {
        return Vec::new();
    };
    #[cfg(windows)]
    if f.metadata()
        .is_ok_and(|m| super::style::is_reparse_point(&m))
    {
        return Vec::new();
    }
    if !f.metadata().is_ok_and(|m| m.is_file()) {
        return Vec::new();
    }
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut r = std::io::BufReader::new(f);
    if len > 4 << 20 {
        use std::io::{Seek, SeekFrom};
        let _ = r.seek(SeekFrom::Start(len - (4 << 20)));
        let mut skip = String::new();
        let _ = r.read_line(&mut skip); // a partial first line
    }
    let mut out: Vec<Record> = r
        .lines()
        .map_while(Result::ok)
        .take(1_000_000)
        .filter_map(|l| serde_json::from_str::<Record>(&l).ok())
        .map(|x| Record {
            // capped and checked as they are read back: the file may be someone else's
            date: x.date.chars().take(10).collect(),
            text: x.text.chars().take(MAX_TEXT_CHARS).collect(),
            export: x.export.map(|e| e.chars().take(300).collect()),
            author: x.author.chars().take(80).collect(),
            via: if valid_via(&x.via) {
                x.via
            } else {
                "unknown".into()
            },
            untrusted: true,
        })
        .collect();
    let k = out.len().saturating_sub(n);
    out.drain(..k);
    out
}

/// Parses the bullets of an `AGENTS.md` section named `Feedback log` (any heading
/// level): `- YYYY-MM-DD: text`, continuation lines indented.
pub fn parse_agents_md(md: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut level = 0usize;
    let mut cur: Option<(String, String)> = None;
    for line in md.lines() {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && line[hashes..].starts_with(' ') {
            if inside && hashes <= level {
                break;
            }
            if line[hashes..].trim().eq_ignore_ascii_case("feedback log") {
                inside = true;
                level = hashes;
            }
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            let (date, text) = match rest.split_once(':') {
                Some((d, t))
                    if d.len() == 10 && d.chars().all(|c| c.is_ascii_digit() || c == '-') =>
                {
                    (d.to_owned(), t.trim().to_owned())
                }
                _ => (String::new(), rest.trim().to_owned()),
            };
            cur = Some((date, text));
        } else if line.starts_with(char::is_whitespace) && !line.trim().is_empty() {
            if let Some((_, t)) = cur.as_mut() {
                t.push(' ');
                t.push_str(line.trim());
            }
        } else if !line.trim().is_empty()
            && let Some(c) = cur.take()
        {
            out.push(c);
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// `gmclip feedback export --format agents-md` (SPEC §10.5, M2): the records as
/// AGENTS.md feedback-log lines, `- YYYY-MM-DD: text (author, via …)`, under a
/// `### Feedback log` heading, so [`parse_agents_md`] reads them back. Each record is one
/// line (line breaks become spaces, so a record cannot add a heading or a bullet), and
/// characters that could hide text are shown as `⟦U+XXXX⟧` (R-INJ-6). This only prints:
/// merging the lines into another tool's instruction file is the user's own agent's job,
/// outside gmclip (D16).
pub fn export_agents_md(records: &[Record]) -> String {
    let one_line = |s: &str| {
        super::style::visible(&s.replace(['\r', '\n'], " "))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut out = String::from(
        "### Feedback log\n\n<!-- gmclip feedback export: text people wrote about exports; untrusted -->\n\n",
    );
    for r in records {
        let date = if r.date.len() == 10 && r.date.chars().all(|c| c.is_ascii_digit() || c == '-') {
            r.date.clone()
        } else {
            "0000-00-00".into()
        };
        let mut src = format!("{}, via {}", one_line(&r.author), one_line(&r.via));
        if let Some(e) = &r.export {
            src.push_str(&format!(", export {}", one_line(e)));
        }
        out.push_str(&format!("- {date}: {} ({src})\n", one_line(&r.text)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Notes\n## Keeping\n### Feedback log\n\n- 2026-10-04: v3 (`a.mp4`, 22.5 s): \"too jumpy\".\n  more text\n- 2026-10-04: v4: \"great\".\n\n## Next\n- not feedback\n";

    #[test]
    fn agents_md_feedback_section() {
        let v = parse_agents_md(MD);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "2026-10-04");
        assert!(v[0].1.ends_with("\"too jumpy\". more text"), "{}", v[0].1);
        assert_eq!(v[1].1, "v4: \"great\".");
    }

    #[test]
    fn export_reads_back_as_the_same_records() {
        let r = |date: &str, text: &str| Record {
            date: date.into(),
            text: text.into(),
            export: Some("01J0000000000000000000000A".into()),
            author: "AJ".into(),
            via: "app".into(),
            untrusted: true,
        };
        let recs = [
            r("2026-10-14", "keep the clock"),
            r(
                "2026-10-15",
                "two\n## Heading\n- 2020-01-01: injected\u{202E}",
            ),
        ];
        let md = export_agents_md(&recs);
        let back = parse_agents_md(&md);
        assert_eq!(back.len(), 2, "{md}");
        assert_eq!(back[0].0, "2026-10-14");
        assert!(
            back[0]
                .1
                .starts_with("keep the clock (AJ, via app, export 01J"),
            "{}",
            back[0].1
        );
        assert!(!md.contains("\n## Heading"), "one line per record: {md}");
        assert!(back[1].1.contains("⟦U+202E⟧"), "{}", back[1].1);
    }

    #[test]
    fn append_and_read_back() {
        let d = std::env::temp_dir().join(format!("gmclip-fb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (date, text) in parse_agents_md(MD) {
            append_by_human(&d, &date, &text, None, "AJ", "app").unwrap();
        }
        append_by_human(&d, "2026-10-14", "keep the clock", None, "AJ", "app").unwrap();
        assert!(append_by_human(&d, "2026-10-14", "x", None, "AJ", "import").is_err());
        assert!(append_by_human(&d, "2026-10-14", "x", None, "AJ", "mcp").is_err());
        std::fs::OpenOptions::new()
            .append(true)
            .open(d.join(FEEDBACK_FILE))
            .unwrap()
            .write_all(b"not json\n{\"date\":\"x\",\"text\":\"t\",\"author\":\"a\",\"via\":\"app\",\"untrusted\":false}\n")
            .unwrap();
        let all = last(&d, 50);
        assert_eq!(all.len(), 4);
        #[cfg(unix)]
        {
            let e = d.join("evil");
            std::fs::create_dir_all(&e).unwrap();
            std::os::unix::fs::symlink("/dev/zero", e.join(FEEDBACK_FILE)).unwrap();
            assert!(last(&e, 50).is_empty(), "a symlink is not read");
        }
        assert!(all.iter().all(|r| r.untrusted), "always served untrusted");
        assert_eq!(last(&d, 1).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
