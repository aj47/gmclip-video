//! `gmclip share` (SPEC §5.14.1 static share page; SR R-REV-7; M1 A36): writes a folder
//! with `index.html`, `style.css`, the export MP4 (and a poster when given) for one or
//! more cuts. No server, no script: every text — titles, notes, review notes, and
//! handoff summaries only when `--summaries` is ticked — is HTML-escaped into the page,
//! which carries the R-REV-5 CSP as a `<meta>` tag and the notice that anyone who can
//! open the folder can watch the files. It plays from Files on an iPhone, where Quick
//! Look runs no JavaScript.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use gmclip_core::{OpenOptions, ProjectHandle};
use gmclip_model::entities::{Cut, Moment, Review};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::Output;
use crate::error::{CliError, Code};

/// The page's CSP (R-REV-5, as a `<meta>`; `frame-ancestors` is header-only, so the
/// meta form leaves it out).
pub const SHARE_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'self'; img-src 'self'; media-src 'self'; base-uri 'none'; form-action 'none'";
/// The folder notice (R-REV-7).
pub const NOTICE: &str = "Anyone who can open this folder can watch these files.";

#[derive(Debug, Args)]
pub struct ShareArgs {
    /// The project database (`<id>.sqlite`).
    #[arg(long)]
    pub project: PathBuf,
    /// `<cut id or hook id>=<export mp4>` (repeatable).
    #[arg(long = "cut", required = true)]
    pub cuts: Vec<String>,
    /// The folder to write (created; must be empty or new).
    #[arg(long)]
    pub out: PathBuf,
    /// A poster image (PNG/JPEG) shown before playback (applies to every cut).
    #[arg(long)]
    pub poster: Option<PathBuf>,
    /// Include agents' handoff summaries (excluded by default).
    #[arg(long)]
    pub summaries: bool,
    /// Include the cuts' notes (excluded by default: they may hold agents' edit-pass
    /// handoffs).
    #[arg(long)]
    pub notes: bool,
}

/// HTML text escaping (element content and attribute values).
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c if c.is_control() && c != '\n' && c != '\t' => {}
            // bidi controls and zero-width characters shown, never applied (R-INJ-6)
            c if matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF) =>
            {
                let _ = write!(o, "⟦U+{:04X}⟧", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

fn bad(e: impl std::fmt::Display) -> CliError {
    CliError::new(Code::Invalid)
        .step("share")
        .reason(e.to_string())
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

const CSS: &str = "body{margin:0;padding:16px;font:16px/1.45 -apple-system,system-ui,sans-serif;background:#f6f6f4;color:#1b1b1b}\n@media (prefers-color-scheme:dark){body{background:#121212;color:#eee}.card{background:#1d1d1d;border-color:#333}}\n.card{background:#fff;border:1px solid #ddd;border-radius:10px;padding:12px;margin:0 0 16px}\nvideo{width:100%;max-height:75vh;background:#000;border-radius:8px}\n.notice{font-size:14px;opacity:.75}\n.note{white-space:pre-wrap;overflow-wrap:anywhere}\nh1{font-size:20px}h2{font-size:17px;margin:0 0 8px}\n";

fn regular(p: &Path, what: &str) -> Result<(), CliError> {
    let m = std::fs::symlink_metadata(p).map_err(|e| CliError::io("share", &e))?;
    if !m.is_file() {
        return Err(bad(format!("the {what} is not a regular file")));
    }
    Ok(())
}

pub fn run(a: ShareArgs) -> Result<Output, CliError> {
    if std::fs::symlink_metadata(&a.out).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(bad("the output folder is a symlink"));
    }
    if let Some(p) = &a.poster {
        regular(p, "poster")?;
    }
    if a.out.exists()
        && std::fs::read_dir(&a.out)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return Err(bad("the output folder is not empty"));
    }
    let handle = ProjectHandle::open(&a.project, OpenOptions::default())
        .map_err(|e| bad(format!("open the project: {e}")))?;
    let snap = handle.snapshot();
    let doc = &snap.doc;
    // resolve every cut first: nothing is written for a bad argument
    let mut items = Vec::new();
    for spec in &a.cuts {
        let (key, mp4) = spec
            .split_once('=')
            .ok_or_else(|| bad("--cut takes <cut id or hook id>=<export.mp4>"))?;
        let cut = doc
            .iter::<Cut>()
            .find(|c| {
                c.id.0.to_string().eq_ignore_ascii_case(key)
                    || c.moment
                        .and_then(|m| doc.get::<Moment>(m))
                        .is_some_and(|m| m.origin_ref.as_deref() == Some(key))
            })
            .ok_or_else(|| bad("no such cut in the project"))?
            .clone();
        let mp4 = PathBuf::from(mp4);
        regular(&mp4, "export")?;
        // the export's record, when it has one, must match the file
        if let Ok(rec) = gmclip_media::export_record::ExportRecord::read(&mp4) {
            let sha = sha256_file(&mp4).map_err(|e| CliError::io("export", &e))?;
            if !sha.eq_ignore_ascii_case(&rec.sha256) {
                return Err(bad("the export does not match its export record"));
            }
        }
        let reviews: Vec<Review> = doc
            .children::<Review>(cut.id.entity())
            .filter_map(|r| doc.get(r).cloned())
            .collect();
        items.push((cut, mp4, reviews));
    }
    let _ = handle.close();
    std::fs::create_dir_all(&a.out).map_err(|e| CliError::io("share folder", &e))?;
    std::fs::write(a.out.join("style.css"), CSS).map_err(|e| CliError::io("share folder", &e))?;
    let poster = match &a.poster {
        Some(p) => {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg"))
                .ok_or_else(|| bad("the poster must be .png or .jpg"))?;
            let name = format!("poster.{ext}");
            std::fs::copy(p, a.out.join(&name)).map_err(|e| CliError::io("poster", &e))?;
            Some(name)
        }
        None => None,
    };
    let mut body = String::new();
    let _ = writeln!(
        body,
        "<h1>gmclip share</h1>\n<p class=\"notice\">{}</p>",
        esc(NOTICE)
    );
    for (i, (cut, mp4, reviews)) in items.iter().enumerate() {
        let name = format!("cut-{}.mp4", i + 1);
        std::fs::copy(mp4, a.out.join(&name)).map_err(|e| CliError::io("export copy", &e))?;
        // what was copied is what the record vouches for (checked on the copy)
        if let Ok(rec) = gmclip_media::export_record::ExportRecord::read(mp4) {
            let sha = sha256_file(&a.out.join(&name)).map_err(|e| CliError::io("export", &e))?;
            if !sha.eq_ignore_ascii_case(&rec.sha256) {
                let _ = std::fs::remove_file(a.out.join(&name));
                return Err(bad("the export changed while it was copied"));
            }
        }
        let _ = writeln!(
            body,
            "<section class=\"card\">\n<h2>{}</h2>",
            esc(&cut.title)
        );
        let _ = writeln!(
            body,
            "<video controls playsinline preload=\"metadata\" src=\"{}\"{}></video>",
            esc(&name),
            poster
                .as_ref()
                .map(|p| format!(" poster=\"{}\"", esc(p)))
                .unwrap_or_default()
        );
        if a.notes && !cut.notes.is_empty() {
            let _ = writeln!(body, "<p class=\"note\">{}</p>", esc(&cut.notes));
        }
        for r in reviews {
            let _ = writeln!(
                body,
                "<p class=\"note\">{} · {}: {}</p>",
                esc(&format!("{:?}", r.verdict).to_lowercase()),
                esc(&r.author),
                esc(&r.text)
            );
        }
        if a.summaries
            && let Some(s) = &cut.last_run_summary
        {
            let _ = writeln!(
                body,
                "<p class=\"note\">Agent summary: {}</p>",
                esc(&s.text)
            );
        }
        let _ = writeln!(body, "</section>");
    }
    let page = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">\n<meta name=\"referrer\" content=\"no-referrer\">\n<title>gmclip share</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n{body}</body>\n</html>\n",
        esc(SHARE_CSP)
    );
    std::fs::write(a.out.join("index.html"), page).map_err(|e| CliError::io("share folder", &e))?;
    Ok(Output::Summary(json!({
        "out": a.out,
        "cuts": items.len(),
        "summaries": a.summaries,
        "notes": a.notes,
        "notice": NOTICE,
    })))
}
