//! `cargo xt stylegate`: the R-COMP-2 style gate's blind A/B page (H-13).
//!
//! ```text
//! cargo xt stylegate build --pairs pairs.json --out PAGE --key KEY.json [--ffmpeg PATH]
//! cargo xt stylegate build --self-test --out PAGE --key KEY.json   (the 11 references vs themselves)
//! cargo xt stylegate serve PAGE [--port 8765] [--host NAME]        (read-only, 127.0.0.1 only)
//! cargo xt stylegate score KEY.json results.json [--k3] [--any-count]
//! ```
//!
//! `pairs.json` is `[{"id": "h03", "reference": "/…/x-v5.mp4", "candidate": "/…/y.mp4"}, …]`
//! (SPEC R-COMP-2: the reference is the golden render, the candidate gmclip's).
//!
//! **What is hidden.** `PAGE/` holds `index.html` and `media/<name>.mp4`. Names and the
//! page id are fresh OS randomness, independent of the generator that decides the layout
//! (so they cannot be run back to the seed); pair order and which render is A are
//! random; copies carry a fixed modification date and no extended attributes; the page
//! shows no levels. Which side is the reference lives only in `KEY.json`, which must be
//! outside `PAGE/` (`build` refuses otherwise, `serve` refuses a folder holding a key).
//! What is not hidden: the two renders' byte sizes, and — in the page source only — the
//! playback gains.
//!
//! **Loudness.** Integrated loudness (BS.1770) is measured offline with ffmpeg's
//! `ebur128` (the dev-only reference meter); the louder side plays attenuated by the
//! difference (gains ≤ 0 dB), by playback gain only (R-COMP-2). Sides more than 6 dB
//! apart are refused (a silent or broken render), unless `--force-loudness`.
//!
//! **On AJ's iPhone.** iOS opens an HTML file from Files or AirDrop in Quick Look, which
//! runs no JavaScript, so ratings need Safari over http(s): run `serve` on the Mac and
//! publish it on the tailnet (`tailscale serve --bg 8765`, then open the
//! `https://<mac>.<tailnet>.ts.net/` address in Safari; pass `--host` with that name).
//! The page still renders its pairs without JavaScript and says so. Gain is applied with
//! Web Audio after a tap on "Start" (iOS ignores `video.volume`); where Web Audio is not
//! running the page says the levels are not matched. From a folder on a Mac, any browser
//! works (gain through `video.volume`).
//!
//! **Scoring.** `score` maps each "B compared with A" answer back to "candidate worse /
//! same / better than the reference": R-COMP-2 passes at ≥ 9 of 11 same-or-better with
//! every pair rated; `--k3` applies K3's fallback (≥ 7, every "worse" listed). Commit the
//! score JSON (it holds hook ids, not paths), not the key.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

/// The 11 golden renders R-COMP-2 names, relative to the lab media folder.
pub const REFERENCES: [(&str, &str); 11] = [
    ("h03", "reels/loc_086da141_h03/loc_086da141_h03-v5.mp4"),
    ("h25", "reels/loc_086da141_h25/loc_086da141_h25-v5.mp4"),
    ("h47", "reels/loc_086da141_h47/loc_086da141_h47-v5.mp4"),
    ("h34", "reels/loc_4fadb756_h34/loc_4fadb756_h34-v5.mp4"),
    ("h37", "reels/loc_4fadb756_h37/loc_4fadb756_h37-v5.mp4"),
    ("h48", "reels/loc_4fadb756_h48/loc_4fadb756_h48-v5.mp4"),
    ("h41", "reels/loc_bd89f38b_h41/loc_bd89f38b_h41-v5.mp4"),
    ("h43", "reels/loc_bd89f38b_h43/loc_bd89f38b_h43-v5.mp4"),
    ("h53", "reels/loc_bd89f38b_h53/loc_bd89f38b_h53-v5.mp4"),
    ("h88", "reels/loc_bd89f38b_h88/loc_bd89f38b_h88-v5.mp4"),
    ("h79", "h79/h79-v4.mp4"),
];

/// R-COMP-2: 11 pairs, ≥ 9 same-or-better; K3's fallback: ≥ 7 with every "worse" listed.
pub const PAIRS: usize = 11;
pub const PASS: usize = 9;
pub const PASS_K3: usize = 7;
/// Largest loudness difference accepted between the two sides of a pair.
pub const MAX_LUFS_GAP_DB: f64 = 6.0;

/// One pair to rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub id: String,
    pub reference: PathBuf,
    pub candidate: PathBuf,
}

/// splitmix64, for the layout only (never for names).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Fisher–Yates.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

/// 128 bits of OS randomness as hex: `/dev/urandom` where it exists, else std's
/// randomly keyed hasher (OS-seeded per process).
pub fn os_random_hex() -> String {
    let mut b = [0u8; 16];
    let from_dev = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    if from_dev.is_err() {
        for (i, chunk) in b.chunks_mut(8).enumerate() {
            let mut h = RandomState::new().build_hasher();
            h.write_usize(i);
            h.write_u128(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            );
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Integrated loudness in LUFS by ffmpeg's `ebur128` filter (dev-only reference meter).
pub fn lufs_ffmpeg(ffmpeg: &Path, file: &Path) -> Result<f64, String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(file)
        .args(["-vn", "-af", "ebur128=framelog=quiet", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("{}: {e}", ffmpeg.display()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "{}: ffmpeg failed measuring loudness",
            file.display()
        ));
    }
    parse_ebur128(&text).ok_or_else(|| {
        format!(
            "{}: no integrated loudness in ffmpeg's output",
            file.display()
        )
    })
}

/// The `I:` value of ebur128's summary.
pub fn parse_ebur128(stderr: &str) -> Option<f64> {
    let summary = &stderr[stderr.rfind("Summary:")?..];
    summary
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("I:"))
        .and_then(|v| v.trim().trim_end_matches("LUFS").trim().parse().ok())
}

/// Copies `src` to `dst` as plain bytes (no extended attributes, no clone metadata) and
/// gives it a fixed modification date, so neither says which render it is.
fn place(src: &Path, dst: &Path) -> Result<(), String> {
    let mut i = std::fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut o = std::fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    std::io::copy(&mut i, &mut o).map_err(|e| format!("{}: {e}", dst.display()))?;
    let fixed = SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_800); // 2000-01-01
    o.set_modified(fixed)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok(())
}

/// Whether `dir` or anything below it holds a `key.json`.
fn holds_key(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            holds_key(&p)
        } else {
            p.file_name().is_some_and(|n| n == "key.json")
        }
    })
}

/// Options for [`build`].
pub struct Build<'a> {
    pub pairs: &'a [Pair],
    pub page: &'a Path,
    pub key: &'a Path,
    pub seed: u64,
    pub force_loudness: bool,
    pub lufs: &'a dyn Fn(&Path) -> Result<f64, String>,
}

/// Builds the page folder and writes the key beside (never inside) it.
pub fn build(b: &Build<'_>) -> Result<Value, String> {
    if b.pairs.is_empty() {
        return Err("no pairs".into());
    }
    if b.page.exists() {
        return Err(format!("{} exists; choose a new --out", b.page.display()));
    }
    let page_abs = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(b.page);
    let key_abs = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(b.key);
    if key_abs.starts_with(&page_abs) {
        return Err("the key must be outside the page folder".into());
    }
    let media = b.page.join("media");
    std::fs::create_dir_all(&media).map_err(|e| format!("{}: {e}", media.display()))?;
    let mut rng = Rng::new(b.seed);
    let page_id = os_random_hex();
    let mut order: Vec<usize> = (0..b.pairs.len()).collect();
    rng.shuffle(&mut order);
    let (mut page_pairs, mut key_pairs) = (Vec::new(), Vec::new());
    for (k, &i) in order.iter().enumerate() {
        let p = &b.pairs[i];
        let n = k + 1;
        let ref_first = rng.next() & 1 == 0;
        let (a_file, b_file, a_role, b_role) = if ref_first {
            (&p.reference, &p.candidate, "reference", "candidate")
        } else {
            (&p.candidate, &p.reference, "candidate", "reference")
        };
        let (la, lb) = ((b.lufs)(a_file)?, (b.lufs)(b_file)?);
        if !b.force_loudness && (la - lb).abs() > MAX_LUFS_GAP_DB {
            return Err(format!(
                "pair {}: the sides measure {la:.1} and {lb:.1} LUFS; a gap over {MAX_LUFS_GAP_DB} dB means a broken render (--force-loudness to accept)",
                p.id
            ));
        }
        // Attenuate the louder side to the quieter one (≤ 0 dB each).
        let target = la.min(lb);
        let (ga, gb) = (round2(target - la), round2(target - lb));
        let (ta, tb) = (os_random_hex(), os_random_hex());
        place(a_file, &media.join(format!("{ta}.mp4")))?;
        place(b_file, &media.join(format!("{tb}.mp4")))?;
        page_pairs.push(json!({
            "n": n, "a": format!("media/{ta}.mp4"), "b": format!("media/{tb}.mp4"),
            "gain_a_db": ga, "gain_b_db": gb,
        }));
        key_pairs.push(json!({
            "n": n, "id": p.id, "a": a_role, "b": b_role,
            "a_token": ta, "b_token": tb,
            "a_file": a_file, "b_file": b_file,
            "lufs_a": la, "lufs_b": lb, "gain_a_db": ga, "gain_b_db": gb,
        }));
    }
    let data = json!({ "page_id": page_id, "pairs": page_pairs });
    let html = PAGE
        .replace("/*DATA*/null", &data.to_string())
        .replace("<!--PAIRS-->", &static_pairs(&page_pairs));
    std::fs::write(b.page.join("index.html"), html).map_err(|e| format!("index.html: {e}"))?;
    let key = json!({
        "schema": "gmclip.stylegate.key/0",
        "page_id": page_id, "seed": b.seed, "pairs": key_pairs,
        "meter": "ffmpeg ebur128 (integrated, BS.1770)",
    });
    std::fs::write(
        b.key,
        serde_json::to_string_pretty(&key).unwrap_or_default(),
    )
    .map_err(|e| format!("{}: {e}", b.key.display()))?;
    Ok(key)
}

/// The pairs as plain HTML, so the page shows them even where JavaScript does not run.
fn static_pairs(pairs: &[Value]) -> String {
    let mut s = String::new();
    for p in pairs {
        let n = p["n"].as_u64().unwrap_or(0);
        let (a, b) = (p["a"].as_str().unwrap_or(""), p["b"].as_str().unwrap_or(""));
        s.push_str(&format!(
            "<section class=\"pair\" id=\"pair{n}\" data-n=\"{n}\"><h2>Pair {n}<span class=\"done\" aria-live=\"polite\"></span></h2>\
<div class=\"player\"><video data-side=\"a\" src=\"{a}\" controls playsinline preload=\"metadata\" aria-label=\"Pair {n} A\"></video>\
<video data-side=\"b\" src=\"{b}\" controls playsinline preload=\"metadata\" aria-label=\"Pair {n} B\" hidden></video></div>\
<div class=\"ab\" role=\"group\" aria-label=\"Show A or B\"><button type=\"button\" data-show=\"a\" aria-pressed=\"true\">A</button><button type=\"button\" data-show=\"b\" aria-pressed=\"false\">B</button></div>\
<fieldset class=\"rate\"><legend>B compared with A</legend>\
<label><input type=\"radio\" name=\"r{n}\" value=\"worse\"><span>worse</span></label>\
<label><input type=\"radio\" name=\"r{n}\" value=\"same\"><span>same</span></label>\
<label><input type=\"radio\" name=\"r{n}\" value=\"better\"><span>better</span></label></fieldset>\
<textarea aria-label=\"Note for pair {n}\" placeholder=\"Note (optional)\"></textarea></section>\n"
        ));
    }
    s
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// How [`score`] judges a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Same-or-better pairs needed.
    pub pass: usize,
    /// Pairs the key must have (`None` for fixtures).
    pub pairs: Option<usize>,
}

impl Rule {
    pub const R_COMP_2: Rule = Rule {
        pass: PASS,
        pairs: Some(PAIRS),
    };
    pub const K3: Rule = Rule {
        pass: PASS_K3,
        pairs: Some(PAIRS),
    };
}

/// Scores an exported results file against the key: per pair, the candidate's verdict
/// relative to the reference; pass when every pair is rated and enough are same-or-better.
pub fn score(key: &Value, results: &Value, rule: Rule) -> Result<Value, String> {
    if key["schema"] != "gmclip.stylegate.key/0"
        || results["schema"] != "gmclip.stylegate.results/0"
    {
        return Err("not a stylegate key and results file".into());
    }
    if !key["page_id"].is_string() || key["page_id"] != results["page_id"] {
        return Err("the results are from another page (page_id differs)".into());
    }
    let kpairs = key["pairs"].as_array().ok_or("key has no pairs")?;
    if let Some(want) = rule.pairs
        && kpairs.len() != want
    {
        return Err(format!(
            "the key has {} pairs; R-COMP-2 rates {want}",
            kpairs.len()
        ));
    }
    let ratings = results["ratings"]
        .as_array()
        .ok_or("results have no ratings")?;
    let mut seen = std::collections::BTreeSet::new();
    for r in ratings {
        let n = r["n"].as_u64().ok_or("a rating without n")?;
        if !seen.insert(n) {
            return Err(format!("pair {n} is rated twice"));
        }
        if !kpairs.iter().any(|k| k["n"].as_u64() == Some(n)) {
            return Err(format!("pair {n} is not in the key"));
        }
    }
    let (mut rows, mut ok, mut worse, mut unplayed) = (Vec::new(), 0, Vec::new(), Vec::new());
    for kp in kpairs {
        let n = kp["n"].as_u64().ok_or("key pair without n")?;
        let r = ratings.iter().find(|r| r["n"].as_u64() == Some(n));
        let rating = r.and_then(|r| r["rating"].as_str());
        // The page asks "B compared with A".
        let candidate = match (rating, kp["b"].as_str()) {
            (None, _) => None,
            (Some(x @ ("worse" | "same" | "better")), Some("candidate")) => Some(x),
            (Some("worse"), Some("reference")) => Some("better"),
            (Some("better"), Some("reference")) => Some("worse"),
            (Some("same"), Some("reference")) => Some("same"),
            _ => return Err(format!("pair {n}: unknown rating or role")),
        };
        ok += usize::from(matches!(candidate, Some("same" | "better")));
        if candidate == Some("worse") {
            worse.push(kp["id"].clone());
        }
        let played = r.is_some_and(|r| r["a_played"] == true && r["b_played"] == true);
        if candidate.is_some() && !played {
            unplayed.push(kp["id"].clone());
        }
        rows.push(json!({
            "id": kp["id"], "candidate_vs_reference": candidate,
            "note": r.and_then(|r| r.get("note")).cloned().unwrap_or(Value::Null),
            "both_played": played,
        }));
    }
    let rated = rows
        .iter()
        .filter(|r| !r["candidate_vs_reference"].is_null())
        .count();
    Ok(json!({
        "schema": "gmclip.stylegate.score/0",
        "page_id": key["page_id"],
        "pairs": rows,
        "rated": rated,
        "same_or_better": ok,
        "needed": rule.pass,
        "worse": worse,
        "rated_without_playing_both": unplayed,
        "pass": rated == rows.len() && ok >= rule.pass,
    }))
}

// ------------------------------------------------------------------ read-only loopback server

/// Most connections handled at once; more wait in the listen queue.
const MAX_CONNECTIONS: usize = 32;
/// Most bytes of request line + headers read.
const MAX_HEADER_BYTES: u64 = 8 * 1024;
/// The most bytes one range response carries.
const RANGE_CHUNK: u64 = 2 * 1024 * 1024;

/// Serves `root` read-only on 127.0.0.1:`port` (GET and HEAD, byte ranges, no listing).
/// `hosts` are the Host header values accepted besides 127.0.0.1 and localhost (the
/// tailnet name when published with `tailscale serve`).
pub fn serve(root: &Path, port: u16, hosts: &[String]) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    if holds_key(&root) {
        return Err("this folder holds a key.json; serve the page folder only".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("bind: {e}"))?;
    println!(
        "serving {} read-only at http://127.0.0.1:{port}/ (Ctrl-C to stop)",
        root.display()
    );
    let live = Arc::new(AtomicUsize::new(0));
    let hosts: Arc<Vec<String>> = Arc::new(hosts.to_vec());
    for stream in listener.incoming().flatten() {
        while live.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            std::thread::sleep(Duration::from_millis(5));
        }
        live.fetch_add(1, Ordering::AcqRel);
        let (root, live, hosts) = (root.clone(), live.clone(), hosts.clone());
        std::thread::spawn(move || {
            let _ = handle(stream, &root, port, &hosts);
            live.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

/// The file a request path names, or `None` (traversal, hidden files, not a file).
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let path = url_path.split(['?', '#']).next()?;
    let decoded = percent_decode(path)?;
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let mut out = root.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) if !s.to_string_lossy().starts_with('.') => out.push(s),
            _ => return None,
        }
    }
    let canon = out.canonicalize().ok()?;
    (canon.starts_with(root) && canon.is_file()).then_some(canon)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.contains('\0'))
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("mp4") => "video/mp4",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// What a Range header asks for: a satisfiable range, an unsatisfiable one (416), or
/// something to ignore (a 200 with the whole file, RFC 9110 §14.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeAsk {
    Range(u64, u64),
    Unsatisfiable,
    Ignore,
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` against a length.
pub fn parse_range(h: &str, len: u64) -> RangeAsk {
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return RangeAsk::Ignore;
    };
    if spec.contains(',') {
        return RangeAsk::Ignore;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return RangeAsk::Ignore;
    };
    let (a, b) = (a.trim(), b.trim());
    let parsed = match (a, b) {
        ("", n) => n
            .parse::<u64>()
            .ok()
            .map(|n| (len.saturating_sub(n), len.saturating_sub(1))),
        (a, "") => a.parse::<u64>().ok().map(|a| (a, len.saturating_sub(1))),
        (a, b) => match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) if a <= b => Some((a, b.min(len.saturating_sub(1)))),
            _ => None,
        },
    };
    match parsed {
        None => RangeAsk::Ignore,
        Some((s, e)) if len > 0 && s < len && s <= e => RangeAsk::Range(s, e),
        Some(_) => RangeAsk::Unsatisfiable,
    }
}

fn handle(mut stream: TcpStream, root: &Path, port: u16, hosts: &[String]) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?.take(MAX_HEADER_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (mut range, mut host) = (None, None);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_owned());
            } else if k.eq_ignore_ascii_case("host") {
                host = Some(v.trim().to_owned());
            }
        }
    }
    let reply = |s: &mut TcpStream, status: &str, extra: &str| {
        write!(
            s,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n"
        )
    };
    // Host check (DNS rebinding): loopback names, or a name the operator passed.
    let host_ok = host.as_deref().is_some_and(|h| {
        let name = h
            .rsplit_once(':')
            .filter(|(_, p)| p.parse::<u16>().is_ok())
            .map_or(h, |(n, _)| n);
        name == "127.0.0.1" || name == "localhost" || hosts.iter().any(|x| x == name)
    });
    if !host_ok {
        return reply(&mut stream, "421 Misdirected Request", "");
    }
    let _ = port;
    if method != "GET" && method != "HEAD" {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
        );
    }
    let Some(file) = resolve(root, target) else {
        return reply(&mut stream, "404 Not Found", "");
    };
    let mut f = std::fs::File::open(&file)?;
    let len = f.metadata()?.len();
    let common = format!(
        "Content-Type: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n",
        content_type(&file)
    );
    let ask = range
        .as_deref()
        .map_or(RangeAsk::Ignore, |r| parse_range(r, len));
    let (status, start, end) = match ask {
        // A browser asks for `bytes=a-` and then stops reading; answer in bounded chunks
        // (HTTP allows a shorter 206) so a stalled <video> cannot hold the connection.
        RangeAsk::Range(a, b) => ("206 Partial Content", a, b.min(a + RANGE_CHUNK - 1)),
        RangeAsk::Unsatisfiable => {
            return reply(
                &mut stream,
                "416 Range Not Satisfiable",
                &format!("Content-Range: bytes */{len}\r\n"),
            );
        }
        RangeAsk::Ignore => ("200 OK", 0, len.saturating_sub(1)),
    };
    let n = if len == 0 { 0 } else { end - start + 1 };
    let mut head = format!("HTTP/1.1 {status}\r\n{common}Content-Length: {n}\r\n");
    if status.starts_with("206") {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if method == "GET" && n > 0 {
        f.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut f.take(n), &mut stream)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ CLI

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The pairs of `pairs.json`.
pub fn read_pairs(v: &Value) -> Result<Vec<Pair>, String> {
    v.as_array()
        .ok_or("pairs.json must be an array")?
        .iter()
        .map(|p| {
            let s = |k: &str| {
                p[k].as_str()
                    .map(str::to_owned)
                    .ok_or(format!("pair: `{k}` missing"))
            };
            Ok(Pair {
                id: s("id")?,
                reference: s("reference")?.into(),
                candidate: s("candidate")?.into(),
            })
        })
        .collect()
}

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("build") => {
            let page = PathBuf::from(arg(args, "--out").ok_or("--out PAGE is required")?);
            let key = PathBuf::from(arg(args, "--key").ok_or("--key KEY.json is required (outside PAGE)")?);
            let pairs = if args.iter().any(|a| a == "--self-test") {
                let root = crate::corpus::lab_media();
                REFERENCES
                    .iter()
                    .map(|(id, rel)| Pair { id: (*id).into(), reference: root.join(rel), candidate: root.join(rel) })
                    .collect()
            } else {
                let p = arg(args, "--pairs").ok_or("--pairs pairs.json (or --self-test) is required")?;
                let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
                read_pairs(&serde_json::from_str(&text).map_err(|e| format!("{p}: {e}"))?)?
            };
            for p in &pairs {
                for f in [&p.reference, &p.candidate] {
                    if !f.is_file() {
                        return Err(format!("{}: missing", f.display()));
                    }
                }
            }
            let seed = match arg(args, "--seed") {
                Some(s) => {
                    eprintln!("stylegate: --seed fixes the layout; use it for tests only, never twice for real rounds");
                    s.parse().map_err(|_| "--seed must be an integer")?
                }
                None => u64::from_str_radix(&os_random_hex()[..16], 16).unwrap_or(0),
            };
            let ffmpeg = PathBuf::from(
                arg(args, "--ffmpeg").map(str::to_owned).or_else(|| std::env::var("GMCLIP_FFMPEG").ok()).unwrap_or_else(|| "/opt/homebrew/bin/ffmpeg".into()),
            );
            let keyv = build(&Build {
                pairs: &pairs,
                page: &page,
                key: &key,
                seed,
                force_loudness: args.iter().any(|a| a == "--force-loudness"),
                lufs: &|f| lufs_ffmpeg(&ffmpeg, f),
            })?;
            println!(
                "stylegate: {} pairs -> {} (the page); key -> {} (keep it off the phone)",
                keyv["pairs"].as_array().map_or(0, Vec::len),
                page.display(),
                key.display()
            );
            Ok(())
        }
        Some("serve") => {
            let dir = args.get(1).ok_or("serve PAGE")?;
            let port = arg(args, "--port").map_or(Ok(8765), str::parse).map_err(|_| "--port")?;
            let hosts: Vec<String> = arg(args, "--host").map(|h| vec![h.to_owned()]).unwrap_or_default();
            serve(Path::new(dir), port, &hosts)
        }
        Some("score") => {
            let (k, r) = (args.get(1).ok_or("score KEY.json results.json")?, args.get(2).ok_or("score KEY.json results.json")?);
            let read = |p: &str| -> Result<Value, String> {
                serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?).map_err(|e| format!("{p}: {e}"))
            };
            let mut rule = if args.iter().any(|a| a == "--k3") { Rule::K3 } else { Rule::R_COMP_2 };
            if args.iter().any(|a| a == "--any-count") {
                rule.pairs = None;
            }
            let s = score(&read(k)?, &read(r)?, rule)?;
            println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
            if s["pass"] == true { Ok(()) } else { Err("style gate: not passed".into()) }
        }
        _ => Err("usage: cargo xt stylegate <build (--pairs P | --self-test) --out PAGE --key KEY [--seed N] [--ffmpeg F] [--force-loudness] | serve PAGE [--port N] [--host NAME] | score KEY RESULTS [--k3] [--any-count]>".into()),
    }
}

/// The page. `/*DATA*/null` is replaced by the pairs JSON, `<!--PAIRS-->` by their HTML.
pub const PAGE: &str = include_str!("stylegate.html");

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("gmclip-stylegate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fixture(dir: &Path, n: usize) -> Vec<Pair> {
        (0..n)
            .map(|i| {
                let r = dir.join(format!("secret-reference-{i}.mp4"));
                let c = dir.join(format!("secret-candidate-{i}.mp4"));
                std::fs::write(&r, format!("ref {i}")).unwrap();
                std::fs::write(&c, format!("cand {i}")).unwrap();
                Pair {
                    id: format!("h{i:02}"),
                    reference: r,
                    candidate: c,
                }
            })
            .collect()
    }

    /// References read −16 LUFS, candidates −14.5.
    fn fake_lufs(p: &Path) -> Result<f64, String> {
        Ok(if p.to_string_lossy().contains("reference") {
            -16.0
        } else {
            -14.5
        })
    }

    fn build_at(d: &Path, name: &str, pairs: &[Pair], seed: u64) -> Result<Value, String> {
        build(&Build {
            pairs,
            page: &d.join(name),
            key: &d.join(format!("{name}.key.json")),
            seed,
            force_loudness: false,
            lufs: &fake_lufs,
        })
    }

    #[test]
    fn the_page_hides_names_randomises_sides_and_matches_loudness() {
        let d = scratch("build");
        let pairs = fixture(&d, 11);
        let key = build_at(&d, "page", &pairs, 7).unwrap();
        let html = std::fs::read_to_string(d.join("page/index.html")).unwrap();
        for leak in [
            "secret",
            "reference",
            "candidate",
            "h03",
            &d.to_string_lossy(),
        ] {
            assert!(!html.contains(leak), "the page mentions {leak:?}");
        }
        assert!(
            html.contains("<section class=\"pair\""),
            "pairs render without JavaScript"
        );
        assert!(html.contains("<noscript>"));
        let kp = key["pairs"].as_array().unwrap();
        assert_eq!(kp.len(), 11);
        let ref_first = kp.iter().filter(|p| p["a"] == "reference").count();
        assert!(
            ref_first > 0 && ref_first < 11,
            "sides are random, not fixed: {ref_first}"
        );
        let ids: Vec<&str> = kp.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_ne!(ids, (0..11).map(|i| format!("h{i:02}")).collect::<Vec<_>>());
        let fixed = SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_800);
        for p in kp {
            let (ga, gb) = (
                p["gain_a_db"].as_f64().unwrap(),
                p["gain_b_db"].as_f64().unwrap(),
            );
            let gain_cand = if p["a"] == "candidate" { ga } else { gb };
            let gain_ref = if p["a"] == "reference" { ga } else { gb };
            assert_eq!(
                (gain_ref, gain_cand),
                (0.0, -1.5),
                "the louder side is attenuated"
            );
            for t in ["a_token", "b_token"] {
                let tok = p[t].as_str().unwrap();
                assert_eq!(tok.len(), 32);
                let f = d.join("page/media").join(format!("{tok}.mp4"));
                assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), fixed);
            }
        }
        // Names are not derived from the layout seed: the same seed gives new names.
        let again = build_at(&d, "page2", &pairs, 7).unwrap();
        assert_ne!(again["pairs"][0]["a_token"], key["pairs"][0]["a_token"]);
        assert_ne!(again["page_id"], key["page_id"]);
        assert_eq!(
            again["pairs"][0]["id"], key["pairs"][0]["id"],
            "same seed, same layout"
        );
        // The page never shows levels.
        assert!(!html.contains("Level match") && !html.contains("dB ("));
        // Refusals: an existing page; a key inside the page.
        assert!(build_at(&d, "page", &pairs, 7).is_err());
        let inside = build(&Build {
            pairs: &pairs,
            page: &d.join("p3"),
            key: &d.join("p3/key.json"),
            seed: 1,
            force_loudness: false,
            lufs: &fake_lufs,
        });
        assert!(inside.is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_broken_side_is_refused() {
        let d = scratch("loud");
        let pairs = fixture(&d, 1);
        let silent = |p: &Path| {
            Ok(if p.to_string_lossy().contains("candidate") {
                -70.0
            } else {
                -16.0
            })
        };
        let r = build(&Build {
            pairs: &pairs,
            page: &d.join("p"),
            key: &d.join("k.json"),
            seed: 1,
            force_loudness: false,
            lufs: &silent,
        });
        assert!(r.unwrap_err().contains("broken render"));
        let _ = std::fs::remove_dir_all(d);
    }

    fn key3() -> Value {
        json!({"schema": "gmclip.stylegate.key/0", "page_id": "p", "pairs": [
            {"n": 1, "id": "h03", "a": "reference", "b": "candidate"},
            {"n": 2, "id": "h25", "a": "candidate", "b": "reference"},
            {"n": 3, "id": "h47", "a": "candidate", "b": "reference"}]})
    }

    fn results(r: Value) -> Value {
        json!({"schema": "gmclip.stylegate.results/0", "page_id": "p", "ratings": r})
    }

    #[test]
    fn scoring_maps_b_vs_a_back_to_the_candidate() {
        let rule = Rule {
            pass: 2,
            pairs: None,
        };
        let res = results(json!([
            {"n": 1, "rating": "better", "note": "crisper", "a_played": true, "b_played": true},
            {"n": 2, "rating": "better", "a_played": true, "b_played": true},
            {"n": 3, "rating": "worse", "a_played": true, "b_played": false}]));
        let s = score(&key3(), &res, rule).unwrap();
        let v: Vec<&str> = s["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["candidate_vs_reference"].as_str().unwrap())
            .collect();
        assert_eq!(v, ["better", "worse", "better"]);
        assert_eq!(s["same_or_better"], 2);
        assert_eq!(s["worse"], json!(["h25"]));
        assert_eq!(s["rated_without_playing_both"], json!(["h47"]));
        assert_eq!(s["pass"], true);
        // R-COMP-2 needs 11 pairs.
        assert!(score(&key3(), &res, Rule::R_COMP_2).is_err());
        // Refusals: another page, duplicates, unknown pairs, unknown ratings, no schema.
        assert!(
            score(
                &key3(),
                &json!({"schema": "gmclip.stylegate.results/0", "page_id": "q", "ratings": []}),
                rule
            )
            .is_err()
        );
        assert!(
            score(
                &key3(),
                &results(json!([{"n": 1, "rating": "same"}, {"n": 1, "rating": "worse"}])),
                rule
            )
            .is_err()
        );
        assert!(score(&key3(), &results(json!([{"n": 9, "rating": "same"}])), rule).is_err());
        assert!(score(&key3(), &results(json!([{"n": 1, "rating": "meh"}])), rule).is_err());
        assert!(score(&key3(), &json!({"page_id": "p", "ratings": []}), rule).is_err());
        let partial = score(
            &key3(),
            &results(json!([{"n": 1, "rating": "same"}])),
            Rule {
                pass: 1,
                pairs: None,
            },
        )
        .unwrap();
        assert_eq!(partial["pass"], false, "every pair must be rated");
    }

    #[test]
    fn ebur128_summary() {
        let out = "[Parsed_ebur128_0 @ 0x1] Summary:\n\n  Integrated loudness:\n    I:         -16.3 LUFS\n    Threshold: -26.4 LUFS\n";
        assert_eq!(parse_ebur128(out), Some(-16.3));
        assert_eq!(parse_ebur128("no summary"), None);
    }

    #[test]
    fn ranges_and_paths() {
        use RangeAsk::*;
        assert_eq!(parse_range("bytes=0-1", 10), Range(0, 1));
        assert_eq!(parse_range("bytes=5-", 10), Range(5, 9));
        assert_eq!(parse_range("bytes=-3", 10), Range(7, 9));
        assert_eq!(parse_range("bytes=4-100", 10), Range(4, 9));
        assert_eq!(parse_range("bytes=10-", 10), Unsatisfiable);
        for ignored in ["bytes=0-1,3-4", "items=0-1", "bytes=5-3", "bytes=x-"] {
            assert_eq!(parse_range(ignored, 10), Ignore, "{ignored}");
        }
        let d = scratch("paths");
        std::fs::write(d.join("index.html"), "x").unwrap();
        std::fs::write(d.join(".hidden"), "x").unwrap();
        let root = d.canonicalize().unwrap();
        assert!(resolve(&root, "/").is_some());
        assert!(resolve(&root, "/index.html?x=1").is_some());
        for bad in ["/../etc/passwd", "/%2e%2e/x", "/.hidden", "/nope", "/a%00b"] {
            assert!(resolve(&root, bad).is_none(), "{bad}");
        }
        std::fs::write(d.join("key.json"), "{}").unwrap();
        assert!(
            serve(&d, 0, &[]).unwrap_err().contains("key.json"),
            "never serves a key"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    /// The server answers GET with byte ranges (what Safari's <video> needs), refuses
    /// writes and foreign Host headers, on loopback only.
    #[test]
    fn the_loopback_server_is_read_only_with_ranges() {
        let d = scratch("serve");
        std::fs::write(d.join("index.html"), "<p>hello</p>").unwrap();
        let root = d.canonicalize().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // five served connections: a failed accept (Windows under load) must not
            // count, or the last ask meets a closed listener and is reset
            for s in listener.incoming().flatten().take(5) {
                let _ = handle(s, &root, port, &["mac.tailnet.ts.net".to_owned()]);
            }
        });
        let ask = |req: &str| {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut out = String::new();
            if let Err(e) = s.read_to_string(&mut out) {
                panic!("{:?}: {e} (read: {out:?})", req.lines().next());
            }
            out
        };
        let full = ask("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(
            full.starts_with("HTTP/1.1 200") && full.ends_with("<p>hello</p>"),
            "{full}"
        );
        let part =
            ask("GET /index.html HTTP/1.1\r\nHost: mac.tailnet.ts.net\r\nRange: bytes=3-7\r\n\r\n");
        assert!(
            part.starts_with("HTTP/1.1 206")
                && part.contains("Content-Range: bytes 3-7/12")
                && part.ends_with("hello"),
            "{part}"
        );
        let put =
            ask("PUT /index.html HTTP/1.1\r\nHost: localhost:8765\r\nContent-Length: 0\r\n\r\n");
        assert!(put.starts_with("HTTP/1.1 405"), "{put}");
        let evil = ask("GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
        assert!(evil.starts_with("HTTP/1.1 421"), "{evil}");
        let none = ask("GET / HTTP/1.1\r\n\r\n");
        assert!(none.starts_with("HTTP/1.1 421"), "{none}");
        let _ = std::fs::remove_dir_all(d);
    }
}
