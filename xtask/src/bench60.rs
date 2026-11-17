//! `cargo xt bench60 [--sources test-media/stream-sources.toml] [--out FILE] [--no-validate]
//! [--no-probe]`: writes the BENCH-60-M0 RenderPlan (SPEC §13 "Benchmarks", R-EXP-1;
//! H-15's generator).
//!
//! The plan, per SPEC: 60 s at 30 fps (1800 frames); 41 parts with 40 discontinuous
//! joins alternating source (a) (the Oct-01 MMO restream, native: picture and audio in one
//! file) and source (b) (the 1440p VP9 MKV's picture with its 48 kHz audio member, the
//! loc_4fadb756 proxy, as a two-source part the way the golden cuts render it); 4 layers
//! per part (screen, camera, 2 detail); 66 story-style caption events; 6 section pills; a run clock;
//! one gain envelope; no key, no roto.
//!
//! So the bench costs what a real cut costs:
//! - (b) parts come only from the stretches the alignment windows measured (≥ 1201 s,
//!   `[[source.window]]`, where AJ's shipped reels were cut: live screen content and a
//!   verified −1642 offset), never from the unmeasured or unwindowed stretches;
//! - start times carry tenths of a second, so seeks land at varied distances after the
//!   MKV's 5 s keyframes (and the restream's ~1.9 s ones), and each source's parts are
//!   visited out of order (backward as well as forward seeks), like a payoff-first cut;
//! - text follows the story style: a ~5 s hook, then pills that change at part boundaries and give way
//!   to the run clock (which ends on "DONE IN"), and 2–3-word captions;
//! - with ffprobe available, every part's frames and samples are checked to exist in its
//!   files before anything is written.
//!
//! Indices follow RP §2.1 and stream-sources.toml: (a) frame = native frame, sample =
//! native × 1600; (b) MKV frame = native frame + offset(t), sample = native × 1600 on the
//! proxy. The plan is validated with `gmclip plan validate` before it replaces the output.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// The frame rate, frames, parts and text counts SPEC §13 fixes.
pub const FPS: u64 = 30;
pub const FRAMES: u64 = 1800;
pub const PARTS: usize = 41;
pub const CAPTIONS: usize = 66;
pub const PILLS: usize = 6;
const SPF: u64 = 48_000 / FPS;

/// (a) part starts, native seconds, in visiting order (out of order on purpose).
const A_TIMES: [f64; 21] = [
    3180.4, 412.7, 1890.2, 655.9, 2741.3, 1033.6, 2268.8, 301.1, 1504.5, 3010.7, 870.3, 2455.6,
    1211.8, 3299.2, 563.4, 1700.9, 2902.5, 1377.1, 2088.4, 760.6, 2613.0,
];
/// (b) part starts, native seconds, all inside measured alignment windows (≥ 1201 s), in
/// visiting order.
const B_TIMES: [f64; 20] = [
    2711.3, 1203.4, 1781.7, 2568.2, 1297.9, 1684.1, 2724.0, 1250.6, 1815.3, 2603.8, 1283.2, 1744.5,
    2590.1, 1311.7, 1832.4, 1670.9, 2694.6, 1324.3, 1761.2, 2581.5,
];

/// What the generator needs from stream-sources.toml.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stream {
    pub vid: String,
    pub mode: String,
    pub picture_path: String,
    pub picture_sha256: String,
    pub audio_path: String,
    pub audio_sha256: String,
    /// `(from_s, until_s, frames)`.
    pub offsets: Vec<(f64, f64, i64)>,
    /// Native-time spans no offset measurement covers (`[[source.unmeasured]]`).
    pub unmeasured: Vec<(f64, f64)>,
    /// `(start_s, end_s)` of each measured alignment window.
    pub windows: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontRow {
    pub fontname: String,
    pub path: String,
    pub sha256: String,
    pub em_per_size: f64,
}

/// The streams and `[[font]]` rows of stream-sources.toml, read through lane-b1's parser
/// ([`crate::streamsources::parse`], which checks the file's whole schema) and
/// [`crate::minitoml`] for the fonts it does not model.
pub fn read_sources(text: &str) -> Result<(Vec<Stream>, Vec<FontRow>), String> {
    let ss = crate::streamsources::parse(text)?;
    let streams = ss
        .sources
        .into_iter()
        .map(|s| Stream {
            vid: s.vid,
            mode: s.mode,
            picture_path: s.picture.1,
            picture_sha256: s.picture.2,
            audio_path: s.audio.1,
            audio_sha256: s.audio.2,
            offsets: s
                .offsets
                .iter()
                .map(|o| (o.from_s, o.until_s, o.frames))
                .collect(),
            unmeasured: s.unmeasured,
            windows: s
                .windows
                .iter()
                .map(|w| (w.start_s as f64, w.end_s as f64))
                .collect(),
        })
        .collect();
    let doc = crate::minitoml::parse(text).map_err(|e| format!("stream-sources.toml: {e}"))?;
    let mut fonts = Vec::new();
    for f in doc
        .get("font")
        .and_then(crate::minitoml::Value::as_array)
        .unwrap_or_default()
    {
        let t = f.as_table().ok_or("[[font]] must be a table")?;
        let s = |k: &str| -> Result<String, String> {
            t.get(k)
                .and_then(crate::minitoml::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("[[font]]: '{k}' must be a string"))
        };
        fonts.push(FontRow {
            fontname: s("fontname")?,
            path: s("path")?,
            sha256: s("sha256")?,
            em_per_size: t
                .get("em_per_size")
                .and_then(crate::minitoml::Value::as_float)
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("[[font]]: 'em_per_size' must be a positive number")?,
        });
    }
    Ok((streams, fonts))
}

impl Stream {
    fn offset_at(&self, t: f64) -> Option<i64> {
        self.offsets
            .iter()
            .find(|o| o.0 <= t && t < o.1)
            .map(|o| o.2)
    }

    fn in_window(&self, a: f64, z: f64) -> bool {
        self.windows.iter().any(|w| w.0 <= a && z <= w.1)
    }
}

/// A place of width `w` at (`x`, `y`) with the crop's aspect (RP rule 5), integer pixels.
fn place(crop: [u64; 4], x: u64, y: u64, w: u64) -> Value {
    let h = (w as f64 * crop[3] as f64 / crop[2] as f64).round() as u64;
    json!({ "x": x, "y": y, "w": w, "h": h })
}

fn rect(r: [u64; 4]) -> Value {
    json!({ "x": r[0], "y": r[1], "w": r[2], "h": r[3] })
}

type Layout = ([u64; 4], (u64, u64, u64));

fn layers(screen: Layout, cam: Layout, details: [Layout; 2]) -> Value {
    let mut v = vec![
        json!({ "id": "screen", "role": "screen", "crop": rect(screen.0), "place": place(screen.0, screen.1.0, screen.1.1, screen.1.2), "z": 0, "time_map": { "mode": "live" } }),
        json!({ "id": "cam", "role": "cam", "crop": rect(cam.0), "place": place(cam.0, cam.1.0, cam.1.1, cam.1.2), "z": 1 }),
    ];
    for (i, (c, p)) in details.iter().enumerate() {
        v.push(json!({ "id": format!("detail-{i}"), "role": "detail", "crop": rect(*c), "place": place(*c, p.0, p.1, p.2), "z": 2 + i, "time_map": { "mode": "live" } }));
    }
    Value::Array(v)
}

/// A source's fingerprint; its path is relative to the lab media folder.
fn fingerprint(path: &str, sha: &str) -> Value {
    let mut f = json!({ "sha256": sha });
    if let Ok(m) = std::fs::metadata(crate::corpus::lab_media().join(path)) {
        f["size"] = m.len().into();
    }
    f
}

/// Neutral captions in the story style's 1–3-word, 10–16-character shape (no AJ transcript text).
const CUES: &[&str] = &[
    "so here's the",
    "first full run",
    "on my own PC",
    "and it takes",
    "about two minutes",
    "watch the bar",
    "fill up here",
    "every single frame",
    "gets rendered",
    "then we check",
    "the final result",
    "right on screen",
    "that's pretty good",
    "for a local run",
    "let's try again",
    "with a new prompt",
];

/// Section pill texts (`LABEL  {Y}detail`).
const PILL_TEXT: [(&str, &str); PILLS] = [
    ("STEP 1  ", "the prompt"),
    ("STEP 2  ", "the settings"),
    ("STEP 3  ", "the first run"),
    ("STEP 4  ", "the result"),
    ("STEP 5  ", "a second take"),
    ("STEP 6  ", "side by side"),
];

/// Seconds of each source file, from ffprobe (dev-only), for the range check.
pub fn probe_seconds(ffprobe: &Path, file: &str) -> Option<f64> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(crate::corpus::lab_media().join(file))
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Builds the plan from stream-sources.toml's text. `seconds(path)` gives a source file's
/// duration when known; every part must lie inside it.
pub fn plan(
    sources_text: &str,
    sources_sha: &str,
    seconds: &dyn Fn(&str) -> Option<f64>,
) -> Result<Value, String> {
    let (streams, fonts) = read_sources(sources_text)?;
    let a = streams
        .iter()
        .find(|s| s.vid == "loc_bd89f38b")
        .ok_or("stream-sources.toml has no loc_bd89f38b (source (a))")?;
    let b = streams
        .iter()
        .find(|s| s.vid == "loc_4fadb756")
        .ok_or("stream-sources.toml has no loc_4fadb756 (source (b))")?;
    if a.mode != "native" || b.mode != "hires_mkv" {
        return Err("expected (a) native and (b) hires_mkv".into());
    }
    let font = fonts
        .iter()
        .find(|f| f.fontname == "Neometric Heavy")
        .ok_or("stream-sources.toml has no Neometric Heavy [[font]]")?;
    let fits = |path: &str, end_s: f64| seconds(path).is_none_or(|d| end_s <= d);

    // Parts: 37 of 44 frames and 4 of 43 = 1800.
    let lens: Vec<u64> = (0..PARTS).map(|i| if i < 37 { 44 } else { 43 }).collect();
    let mut starts = vec![0];
    for n in &lens {
        starts.push(starts.last().unwrap_or(&0) + n);
    }
    let mut parts = Vec::new();
    for i in 0..PARTS {
        let n = lens[i];
        let (k, from_a) = (i / 2, i % 2 == 0);
        let t = if from_a { A_TIMES[k] } else { B_TIMES[k] };
        let native = (t * FPS as f64).round() as u64;
        if (native as f64 - t * FPS as f64).abs() > 1e-6 {
            return Err(format!("part {}: {t} s is not on the frame grid", i + 1));
        }
        let end_s = (native + n) as f64 / FPS as f64;
        let (pic, aud, first_frame, ly) = if from_a {
            if !fits(&a.picture_path, end_s) {
                return Err(format!(
                    "(a) ends at {:.0} s; part {} needs {end_s:.1} s",
                    seconds(&a.picture_path).unwrap_or(0.0),
                    i + 1
                ));
            }
            let ly = layers(
                ([960, 700, 1080, 570], (30, 250, 1020)),
                ([0, 1090, 550, 350], (260, 1176, 560)),
                [
                    ([990, 1116, 800, 80], (30, 876, 1020)),
                    ([1426, 324, 1004, 780], (540, 1560, 480)),
                ],
            );
            ("a", "a", native, ly)
        } else {
            if !b.in_window(t, end_s) {
                return Err(format!(
                    "(b) part at {t} s is outside the measured alignment windows"
                ));
            }
            if b.unmeasured.iter().any(|u| u.0 < end_s && t < u.1) {
                return Err(format!("(b) at {t} s is in the unmeasured offset stretch"));
            }
            let off = b.offset_at(t).ok_or(format!("no MKV offset at {t} s"))?;
            if b.offset_at(end_s - 1.0 / FPS as f64) != Some(off) {
                return Err(format!("(b) part at {t} s straddles an offset change"));
            }
            if !fits(&b.picture_path, end_s) || !fits(&b.audio_path, end_s) {
                return Err(format!("(b) part at {t} s runs past its files"));
            }
            let ff = u64::try_from(native as i64 + off).map_err(|_| "MKV frame before 0")?;
            let ly = layers(
                ([770, 220, 1280, 900], (160, 420, 760)),
                ([2060, 634, 496, 340], (160, 1120, 760)),
                [
                    ([1074, 250, 425, 35], (30, 800, 1020)),
                    ([900, 600, 600, 100], (240, 960, 600)),
                ],
            );
            ("b-mkv", "b-proxy", ff, ly)
        };
        parts.push(json!({
            "id": format!("p{:02}-{}", i + 1, if from_a { "a" } else { "b" }),
            "out": [starts[i], starts[i] + n],
            "picture": { "source": pic, "first_frame": first_frame },
            "audio": { "source": aud, "first_sample": native * SPF, "rate": 48000, "gain_db": 0,
                       "fade_in": { "samples": 144, "shape": "linear" }, "fade_out": { "samples": 144, "shape": "linear" } },
            "native": { "frames": [native, native + n], "fps": { "num": FPS, "den": 1 } },
            "layers": ly,
        }));
    }
    assert_eq!(starts[PARTS], FRAMES);

    // Text, story style: a ~5 s hook; pills from part boundaries once the hook clears; the
    // run clock replaces the pill for 10 s and ends on DONE IN; 66 captions.
    let hook_end = starts[3]; // the end of the third part, 132 frames (4.4 s)
    let mut events = vec![json!({
        "id": "hook", "role": "text_hook", "out": [0, hook_end], "layer": 2, "style": "Hook", "size": 88,
        "pos": { "x": 540, "y": 305 }, "align": 5,
        "runs": [{ "text": "60 seconds,\n" }, { "text": "40 joins.", "fill": "#FFD400FF" }]
    })];
    let (clock_from, clock_to) = (starts[22], starts[27]); // parts 23–27 show the clock
    let pill_bounds = [3, 9, 14, 20, 27, 34, PARTS]; // pill k spans parts [b[k], b[k+1])
    let pill = |k: usize, a: u64, z: u64| {
        json!({
            "id": format!("pill-{}", k + 1), "role": "section_pill", "out": [a, z],
            "layer": 3, "style": "Pill", "size": 50, "pos": { "x": 48, "y": 262 }, "align": 7,
            "runs": [{ "text": PILL_TEXT[k].0 }, { "text": PILL_TEXT[k].1, "fill": "#FFD400FF" }]
        })
    };
    let mut k = 0;
    for w in pill_bounds.windows(2) {
        let (a, z) = (starts[w[0]].max(hook_end), starts[w[1]]);
        // The clock takes the band while it runs.
        if a < clock_from && z > clock_from {
            events.push(pill(k, a, clock_from));
        } else if a >= clock_to || z <= clock_from {
            events.push(pill(k, a, z));
        } else if z > clock_to {
            events.push(pill(k, clock_to, z));
        } else {
            continue;
        }
        k += 1;
    }
    if k != PILLS {
        return Err(format!("{k} pills; SPEC §13 asks for {PILLS}"));
    }
    let clock = |id: String, a: u64, z: u64, runs: Value| {
        json!({
            "id": id, "role": "run_clock", "out": [a, z], "layer": 3, "style": "Pill", "size": 50,
            "pos": { "x": 48, "y": 262 }, "align": 7, "runs": runs,
            "provenance": { "kind": "agent", "detail": { "note": "synthetic benchmark counter" } }
        })
    };
    let mut t = clock_from;
    let mut s = 0;
    while t + FPS <= clock_to - 2 * FPS {
        events.push(clock(format!("clock-{}", s + 1), t, t + FPS, json!([{ "text": "GENERATING  " }, { "text": format!("0:{s:02}"), "fill": "#FFD400FF" }])));
        t += FPS;
        s += 1;
    }
    events.push(clock(
        format!("clock-{}", s + 1),
        t,
        clock_to,
        json!([{ "text": "DONE IN  ", "fill": "#3DDC84FF" }, { "text": format!("0:{s:02}") }]),
    ));
    let span = FRAMES - 60;
    for i in 0..CAPTIONS as u64 {
        let (a0, a1) = (
            60 + span * i / CAPTIONS as u64,
            60 + span * (i + 1) / CAPTIONS as u64,
        );
        let w = CUES[i as usize % CUES.len()];
        let runs = match w.rsplit_once(' ') {
            Some((head, last)) if i % 5 == 0 => vec![
                json!({ "text": format!("{head} ") }),
                json!({ "text": last, "fill": "#FFD400FF" }),
            ],
            _ => vec![json!({ "text": w })],
        };
        events.push(json!({
            "id": format!("c{:02}", i + 1), "role": "caption", "out": [a0, a1], "layer": 1, "style": "Speech",
            "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": runs
        }));
    }
    let shadow =
        json!({ "dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline" });
    let outline = json!({ "width": 12, "colour": "#000000FF", "join": "round" });
    Ok(json!({
        "schema": "gmclip.renderplan/0",
        "meta": {
            "title": "BENCH-60-M0",
            "generator": { "name": "cargo xt bench60", "version": env!("CARGO_PKG_VERSION") },
            "import_report": [
                format!("stream sources: test-media/stream-sources.toml, sha256 {sources_sha}"),
                "SPEC §13 BENCH-60-M0: 41 parts, 40 discontinuous joins alternating (a) loc_bd89f38b native and (b) loc_4fadb756 MKV picture + proxy audio",
                "(b) parts only inside the measured alignment windows (>= 1201 s); starts on tenths of a second, visited out of order",
                "4 layers per part; 66 captions, 6 pills, a run clock ending on DONE IN, one envelope; no key, no roto"
            ]
        },
        "output": {
            "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": FPS, "den": 1 }, "frames": FRAMES,
            "color": { "matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited" },
            "video": { "codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                       "rate": { "mode": "vbr", "target_bps": 16_000_000, "max_bps": 24_000_000 }, "timescale": 15360 },
            "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192_000 },
            "container": { "moov_first": true, "edit_lists": false }
        },
        "background": { "kind": "colour", "rgb": "#0c1118" },
        "sources": [
            { "id": "a", "path": a.picture_path, "fingerprint": fingerprint(&a.picture_path, &a.picture_sha256),
              "expect": { "video": { "codec": "h264", "w": 2560, "h": 1440, "fps": { "num": 30, "den": 1 } },
                          "audio": { "codec": "aac", "rate": 48000, "channels": 2 } } },
            { "id": "b-mkv", "path": b.picture_path, "fingerprint": fingerprint(&b.picture_path, &b.picture_sha256),
              "expect": { "video": { "codec": "vp9", "w": 2560, "h": 1440, "fps": { "num": 30, "den": 1 } } } },
            { "id": "b-proxy", "path": b.audio_path, "fingerprint": fingerprint(&b.audio_path, &b.audio_sha256),
              "expect": { "audio": { "codec": "aac", "rate": 48000, "channels": 2 } } }
        ],
        "fonts": [{ "id": "neometric-heavy", "family": "Neometric", "style": "Heavy", "path": font.path,
                    "sha256": font.sha256, "em_per_size": font.em_per_size }],
        "text_styles": {
            "Speech": { "font": "neometric-heavy", "size": 86, "fill": "#FFFFFFFF", "outline": outline, "shadow": shadow, "align": 5 },
            "Hook": { "font": "neometric-heavy", "size": 88, "fill": "#FFFFFFFF", "outline": outline, "shadow": shadow, "align": 5 },
            "Pill": { "font": "neometric-heavy", "size": 50, "fill": "#FFFFFFFF",
                      "box": { "colour": "#101010CF", "pad_x": 16, "pad_y": 16, "radius": 0, "geometry": "libass_bs3" },
                      "letter_spacing": 1, "align": 7 }
        },
        "parts": parts,
        "text_events": events,
        "audio": {
            "envelopes": [{ "id": "lift", "out_samples": [300 * SPF, 600 * SPF], "gain_db": 10,
                            "ramp_in_samples": 3840, "ramp_out_samples": 5760, "curve": "linear_db" }],
            "loudness": { "mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5 }
        },
        "checks": { "safe_zones": "instagram" }
    }))
}

/// `cargo xt bench60 …`.
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let mut opts = std::collections::BTreeMap::new();
    let mut flags = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sources" | "--out" | "--ffprobe" => {
                opts.insert(
                    a.as_str(),
                    it.next().ok_or(format!("{a} needs a value"))?.clone(),
                );
            }
            "--no-validate" | "--no-probe" => flags.push(a.as_str()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let rel = |p: &str| {
        let p = PathBuf::from(p);
        if p.is_absolute() { p } else { root.join(p) }
    };
    let sources = rel(opts
        .get("--sources")
        .map_or("test-media/stream-sources.toml", String::as_str));
    let out = rel(opts
        .get("--out")
        .map_or("test-media/bench-60-m0.renderplan.json", String::as_str));
    let ffprobe = PathBuf::from(
        opts.get("--ffprobe")
            .map_or("/opt/homebrew/bin/ffprobe", String::as_str),
    );
    let bytes = std::fs::read(&sources).map_err(|e| format!("{}: {e}", sources.display()))?;
    let sha = sha256_hex(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| "stream-sources.toml is not UTF-8")?;
    let probe = !flags.contains(&"--no-probe") && ffprobe.is_file();
    if !probe {
        eprintln!("bench60: not checking source durations (no ffprobe or --no-probe)");
    }
    let p = plan(&text, &sha, &|f| {
        if probe {
            probe_seconds(&ffprobe, f)
        } else {
            None
        }
    })?;
    let mut s = serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?;
    s.push('\n');
    // Validate a temporary file; replace the output only when it passes.
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, s).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if !flags.contains(&"--no-validate") {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo)
            .args([
                "run",
                "--quiet",
                "--locked",
                "-p",
                "gmclip-cli",
                "--",
                "plan",
                "validate",
            ])
            .arg(&tmp)
            .current_dir(root)
            .env("GMCLIP_LOG", "off")
            .status()
            .map_err(|e| format!("cargo run gmclip: {e}"))?;
        if !st.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err("gmclip plan validate refused the plan; the output is unchanged".into());
        }
    }
    std::fs::rename(&tmp, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("bench60: wrote {}", out.display());
    Ok(())
}

/// SHA-256 (FIPS 180-4) for the inputs record, without a crate.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let t1 = hh
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> String {
        std::fs::read_to_string(crate::repo_root().join("test-media/stream-sources.toml")).unwrap()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn reads_stream_sources() {
        let (s, f) = read_sources(&sources()).unwrap();
        let b = s.iter().find(|s| s.vid == "loc_4fadb756").unwrap();
        assert_eq!(b.offset_at(2708.45), Some(-1642));
        assert_eq!(b.offset_at(500.0), Some(-503));
        assert!(b.unmeasured.contains(&(307.0, 1196.0)));
        assert_eq!(b.windows.len(), 10);
        assert!(b.in_window(1203.4, 1205.0) && !b.in_window(1400.0, 1401.0));
        assert_eq!(f[0].fontname, "Neometric Heavy");
        let mut text = sources();
        text = text.replace("em_per_size = 0.8332", "em_per_size = \"x\"");
        assert!(
            read_sources(&text).is_err(),
            "a font without a numeric em_per_size"
        );
        assert!(read_sources("schema = \"other\"\n").is_err());
    }

    /// SPEC §13's shape, counted on the generated plan (it is validated by the CLI in
    /// `cargo xt bench60` and by gmclip-plan's example tests on the committed file).
    #[test]
    fn the_plan_has_bench_60_m0s_shape() {
        let p = plan(&sources(), &"0".repeat(64), &|_| None).unwrap();
        assert_eq!(p["output"]["frames"], FRAMES);
        let parts = p["parts"].as_array().unwrap();
        assert_eq!(parts.len(), PARTS);
        let mut end = 0;
        let (mut back_a, mut back_b, mut last_a, mut last_b) = (0, 0, 0, 0);
        for (i, x) in parts.iter().enumerate() {
            assert_eq!(x["out"][0], end);
            end = x["out"][1].as_u64().unwrap();
            let roles: Vec<&str> = x["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["role"].as_str().unwrap())
                .collect();
            assert_eq!(roles, ["screen", "cam", "detail", "detail"]);
            let native = x["native"]["frames"][0].as_u64().unwrap();
            let (pic, aud) = (
                x["picture"]["source"].as_str().unwrap(),
                x["audio"]["source"].as_str().unwrap(),
            );
            if i % 2 == 0 {
                assert_eq!((pic, aud), ("a", "a"));
                assert_eq!(x["picture"]["first_frame"].as_u64().unwrap(), native);
                back_a += usize::from(native < last_a);
                last_a = native;
            } else {
                assert_eq!(
                    (pic, aud),
                    ("b-mkv", "b-proxy"),
                    "(b)'s picture with its 48 kHz audio member"
                );
                assert_eq!(x["picture"]["first_frame"].as_u64().unwrap(), native - 1642);
                back_b += usize::from(native < last_b);
                last_b = native;
            }
            assert_eq!(x["audio"]["first_sample"].as_u64().unwrap(), native * 1600);
            for l in x["layers"].as_array().unwrap() {
                assert_eq!(
                    l["place"]["h"].as_u64().map(|_| ()),
                    Some(()),
                    "integer heights"
                );
            }
        }
        assert_eq!(end, FRAMES);
        assert!(
            back_a > 3 && back_b > 3,
            "both sources are sought backwards too"
        );
        let ev = p["text_events"].as_array().unwrap();
        let roles = |r: &str| ev.iter().filter(|e| e["role"] == r).count();
        assert_eq!(
            (roles("caption"), roles("section_pill"), roles("text_hook")),
            (CAPTIONS, PILLS, 1)
        );
        assert!(roles("run_clock") > 5);
        assert!(ev.iter().any(|e| e["runs"][0]["text"] == "DONE IN  "));
        // The clock and the pills never share the band.
        let span = |e: &Value| (e["out"][0].as_u64().unwrap(), e["out"][1].as_u64().unwrap());
        for c in ev.iter().filter(|e| e["role"] == "run_clock") {
            for pl in ev.iter().filter(|e| e["role"] == "section_pill") {
                let ((a, b), (x, y)) = (span(c), span(pl));
                assert!(b <= x || y <= a, "clock {a}-{b} overlaps pill {x}-{y}");
            }
        }
        assert!(ev.iter().all(|e| {
            e["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| !r["text"].as_str().unwrap().trim().is_empty())
        }));
        assert_eq!(p["audio"]["envelopes"].as_array().unwrap().len(), 1);
        // A source too short for a part is refused.
        assert!(plan(&sources(), &"0".repeat(64), &|_| Some(600.0)).is_err());
    }

    /// The committed plan is what the generator writes today (sizes come from the lab's
    /// files, so compare without them).
    #[test]
    fn the_committed_plan_is_current() {
        let root = crate::repo_root();
        let bytes = std::fs::read(root.join("test-media/stream-sources.toml")).unwrap();
        let mut fresh = plan(
            &String::from_utf8(bytes.clone()).unwrap(),
            &sha256_hex(&bytes),
            &|_| None,
        )
        .unwrap();
        let mut old: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("test-media/bench-60-m0.renderplan.json")).unwrap(),
        )
        .unwrap();
        for v in [&mut fresh, &mut old] {
            for s in v["sources"].as_array_mut().unwrap() {
                s["fingerprint"].as_object_mut().unwrap().remove("size");
            }
        }
        assert_eq!(fresh, old, "run `cargo xt bench60`");
    }
}
