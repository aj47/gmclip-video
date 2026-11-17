//! ASS-subset importer (B-07; RP §2.4 "From the ASS").
//!
//! Reads exactly what the reference's `story_reel.write_ass` and h79's `build_v4.py` write:
//! `[Script Info]`, `[V4+ Styles]` and `[Events]` `Dialogue` lines, with the override tags
//! `\pos \an \c (\1c) \fs` and the `\N` line break. Anything else in an override block is
//! [`AssError::Invalid`] naming the tag; style features gmclip v0 cannot draw (italic,
//! scaling, rotation, BorderStyle 3 shadows, events without `\pos`, effects) are
//! [`AssError::Unsupported`] by name. Nothing is silently dropped.
//!
//! Timing is what the reference's ffmpeg + libass showed (decision 0003): an event shows on
//! `[F(start_cs), F(end_cs))` with `F` = [`display_frame`] — libass on ffmpeg's truncated
//! double milliseconds, which is `⌈cs·fps/100⌉` except one frame later on some edges.

use std::collections::BTreeMap;
use std::fmt;

use gmclip_plan::consts::{LibassBs3, RoundJoin};
use gmclip_plan::types::{Provenance, ProvenanceDetail, ProvenanceKind};

use super::{
    Colour, Id, Outline, Point, Range, Run, Shadow, ShadowOf, TextBox, TextEvent, TextRole,
    TextStyle,
};

/// An importer error. `path` locates the problem (`"Dialogue 12"`, `"Style Pill"`, …);
/// `reason` names the tag or field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssError {
    /// The file is not valid for the subset (malformed, or an unknown override tag).
    Invalid {
        /// Where.
        path: String,
        /// What, naming the tag or field.
        reason: String,
    },
    /// Valid ASS that v0 cannot draw.
    Unsupported {
        /// Where.
        path: String,
        /// What, by name.
        reason: String,
    },
}

impl fmt::Display for AssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { path, reason } => write!(f, "Invalid{{{path}}}: {reason}"),
            Self::Unsupported { path, reason } => write!(f, "Unsupported{{{path}}}: {reason}"),
        }
    }
}

impl std::error::Error for AssError {}

/// Echoed input is cut to 16 bytes (SR FZ8 / R-FS-1: errors never carry long input).
fn clip(s: &str) -> String {
    if s.len() <= 16 {
        return s.to_owned();
    }
    let mut end = 16;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Output rates RP rule 4 allows.
pub const FPS_ALLOWED: [u32; 6] = [24, 25, 30, 48, 50, 60];

/// RP `$defs/id`: `^[A-Za-z0-9_.:-]{1,96}$`.
fn is_rp_id(s: &str) -> bool {
    (1..=96).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn invalid(path: impl Into<String>, reason: impl Into<String>) -> AssError {
    AssError::Invalid {
        path: path.into(),
        reason: reason.into(),
    }
}

fn unsupported(path: impl Into<String>, reason: impl Into<String>) -> AssError {
    AssError::Unsupported {
        path: path.into(),
        reason: reason.into(),
    }
}

/// An ASS colour as stored: `&HAABBGGRR`, alpha 0 = opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssColour(pub u32);

impl AssColour {
    /// Straight RGBA (ASS alpha inverted).
    pub fn rgba(self) -> Colour {
        let v = self.0;
        Colour([
            (v & 0xff) as u8,
            (v >> 8 & 0xff) as u8,
            (v >> 16 & 0xff) as u8,
            0xff - (v >> 24 & 0xff) as u8,
        ])
    }
}

/// One `Style:` line (V4+ field names).
#[derive(Clone, Debug, PartialEq)]
pub struct AssStyle {
    /// `Name`.
    pub name: String,
    /// `Fontname`.
    pub fontname: String,
    /// `Fontsize`.
    pub fontsize: f64,
    /// `PrimaryColour` (fill).
    pub primary: AssColour,
    /// `OutlineColour` (border; the box in BorderStyle 3).
    pub outline_colour: AssColour,
    /// `BackColour` (shadow).
    pub back_colour: AssColour,
    /// `Bold` (−1 or 1 = bold).
    pub bold: bool,
    /// `Spacing`, script px.
    pub spacing: f64,
    /// `BorderStyle` (1 outline + shadow, 3 opaque box).
    pub border_style: u8,
    /// `Outline`, script px.
    pub outline: f64,
    /// `Shadow`, script px.
    pub shadow: f64,
    /// `Alignment` (numpad).
    pub alignment: u8,
}

/// One `Dialogue:` line, unparsed text.
#[derive(Clone, Debug, PartialEq)]
pub struct AssDialogue {
    /// 1-based index among the file's Dialogue lines.
    pub index: usize,
    /// `Layer`.
    pub layer: i32,
    /// `Start`, centiseconds.
    pub start_cs: u64,
    /// `End`, centiseconds.
    pub end_cs: u64,
    /// `Style`.
    pub style: String,
    /// `Text`, raw.
    pub text: String,
}

/// A parsed script.
#[derive(Clone, Debug, PartialEq)]
pub struct AssScript {
    /// `PlayResX`.
    pub play_res_x: u32,
    /// `PlayResY`.
    pub play_res_y: u32,
    /// `ScaledBorderAndShadow: yes`.
    pub scaled_border_and_shadow: bool,
    /// Styles in file order.
    pub styles: Vec<AssStyle>,
    /// Dialogue lines in file order.
    pub dialogues: Vec<AssDialogue>,
}

/// Import options.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Output frame rate (integer, RP rule 4).
    pub fps: u32,
    /// Event role by style name; styles not listed are [`TextRole::Note`].
    pub roles: BTreeMap<String, TextRole>,
    /// Output canvas `(w, h)`; PlayResX/Y must equal it (RP rule 6: ASS units at PlayResY = canvas).
    pub canvas: (u32, u32),
}

/// The first output frame on which an ASS time of `cs` centiseconds has begun, as the reference's
/// ffmpeg + libass showed it (decision 0003, RP §2.4 "F(cs)"): the smallest `f` with
/// `(i64)(f · (1/fps) · 1000) ≥ 10·cs` in doubles. ffmpeg's `ass` filter truncates its
/// millisecond, which lands 1 ms short on some frames (51 of the first 2000 at 30 fps: 111,
/// 207, 222, 237, 399, …), so an edge exactly there shows one frame after `⌈cs·fps/100⌉`
/// (h79-v4.mp4: "let's also try", 13.30 s, first shows on frame 400). An event
/// `[start, end)` shows on `[display_frame(start), display_frame(end))`.
///
/// The one implementation in the workspace: the reel converter's `rounding::ass_frame` calls it.
pub fn display_frame(cs: u64, fps: u32) -> Option<u64> {
    if fps == 0 {
        return None;
    }
    let ms = cs.checked_mul(10)?;
    let exact = cs.checked_mul(u64::from(fps))?.div_ceil(100);
    let step = 1.0 / f64::from(fps);
    // The truncated double is at most 1 ms short and never long, so it is `exact` or the next.
    let shown = |f: u64| (f as f64 * step * 1000.0) as u64 >= ms;
    Some(if shown(exact) { exact } else { exact + 1 })
}

impl ImportOptions {
    /// The story styles: `Speech` captions, `Hook` text hook, `Pill` section pills.
    pub fn story(fps: u32) -> Self {
        let roles = [
            ("Speech", TextRole::Caption),
            ("Hook", TextRole::TextHook),
            ("Pill", TextRole::SectionPill),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        Self {
            fps,
            roles,
            canvas: (1080, 1920),
        }
    }
}

/// The importer's result: RP `fonts` (id, family), `text_styles` and `text_events`.
#[derive(Clone, Debug)]
pub struct AssImport {
    /// `(fonts[].id, family)` per distinct `Fontname`, in first-use order.
    pub fonts: Vec<(String, String)>,
    /// `text_styles`, keyed by ASS style name.
    pub styles: BTreeMap<String, TextStyle>,
    /// One event per Dialogue line that shows on at least one frame, in file order.
    pub events: Vec<TextEvent>,
    /// What the import did that a reader should know (events showing on no frame).
    pub report: Vec<String>,
    /// Styles with `Bold` set. libass emboldens synthetically when the matched face is
    /// lighter than bold; the caller checks the face (Neometric Heavy is weight 900: none).
    pub bold_styles: Vec<String>,
}

/// The `fonts[].id` for a `Fontname`: lower-case, runs of anything but `[a-z0-9]` → `-`.
pub fn font_id(fontname: &str) -> String {
    let mut out = String::new();
    for c in fontname.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `H:MM:SS.CC` → centiseconds.
fn parse_time(s: &str, path: &str) -> Result<u64, AssError> {
    let bad = || invalid(path, format!("time {:?} is not H:MM:SS.CC", clip(s)));
    let (hms, cs) = s.trim().split_once('.').ok_or_else(bad)?;
    let mut it = hms.split(':');
    let (h, m, sec) = (
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
    );
    if it.next().is_some()
        || m.len() != 2
        || sec.len() != 2
        || cs.len() != 2
        || h.is_empty()
        || h.len() > 9
    {
        return Err(bad());
    }
    let num = |t: &str| -> Result<u64, AssError> {
        if t.bytes().all(|b| b.is_ascii_digit()) {
            t.parse().map_err(|_| bad())
        } else {
            Err(bad())
        }
    };
    let (h, m, sec, cs) = (num(h)?, num(m)?, num(sec)?, num(cs)?);
    if m > 59 || sec > 59 {
        return Err(bad());
    }
    // h ≤ 9 digits, so this cannot overflow; checked anyway.
    h.checked_mul(360_000)
        .and_then(|v| v.checked_add((m * 60 + sec) * 100 + cs))
        .ok_or_else(bad)
}

fn parse_colour(s: &str, path: &str, what: &str) -> Result<AssColour, AssError> {
    let t = s.trim();
    let hex = t
        .strip_prefix("&H")
        .or_else(|| t.strip_prefix("&h"))
        .map(|h| h.strip_suffix('&').unwrap_or(h))
        .filter(|h| !h.is_empty() && h.len() <= 8 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid(path, format!("{what} {:?} is not &HAABBGGRR", clip(t))))?;
    Ok(AssColour(u32::from_str_radix(hex, 16).map_err(|_| {
        invalid(path, format!("{what} {:?}", clip(t)))
    })?))
}

fn parse_num(s: &str, path: &str, what: &str) -> Result<f64, AssError> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| invalid(path, format!("{what} {:?} is not a number", clip(s))))?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(invalid(path, format!("{what} {:?} is not finite", clip(s))))
    }
}

/// Parses a script. Unknown sections (`[Fonts]`, `[Graphics]`, …) are `Unsupported`; unknown
/// `[Script Info]` keys are ignored, as libass does.
pub fn parse(src: &str) -> Result<AssScript, AssError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut section = String::new();
    let mut info: BTreeMap<String, String> = BTreeMap::new();
    let mut style_format: Option<Vec<String>> = None;
    let mut event_format: Option<Vec<String>> = None;
    let mut styles = Vec::new();
    let mut dialogues = Vec::new();
    for (n, raw) in src.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        let path = format!("line {}", n + 1);
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t.starts_with("!:") {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            section = t.to_owned();
            if !matches!(
                section.as_str(),
                "[Script Info]" | "[V4+ Styles]" | "[Events]"
            ) {
                return Err(unsupported(path, format!("section {}", clip(&section))));
            }
            continue;
        }
        let (key, val) = t
            .split_once(':')
            .ok_or_else(|| invalid(&path, format!("expected `Key: value` in {section}")))?;
        let (key, val) = (key.trim(), val.trim_start());
        match section.as_str() {
            "[Script Info]" => {
                info.insert(key.to_owned(), val.trim().to_owned());
            }
            "[V4+ Styles]" => match key {
                "Format" => {
                    style_format = Some(val.split(',').map(|f| f.trim().to_owned()).collect())
                }
                "Style" => {
                    let fmt = style_format
                        .as_ref()
                        .ok_or_else(|| invalid(&path, "Style before Format"))?;
                    styles.push(parse_style(fmt, val, &path)?);
                }
                _ => {
                    return Err(invalid(
                        &path,
                        format!("unknown [V4+ Styles] key {:?}", clip(key)),
                    ));
                }
            },
            "[Events]" => match key {
                "Format" => {
                    event_format = Some(val.split(',').map(|f| f.trim().to_owned()).collect())
                }
                "Dialogue" => {
                    let fmt = event_format
                        .as_ref()
                        .ok_or_else(|| invalid(&path, "Dialogue before Format"))?;
                    let index = dialogues.len() + 1;
                    dialogues.push(parse_dialogue(fmt, val, index)?);
                }
                "Comment" => {}
                _ => {
                    return Err(unsupported(
                        &path,
                        format!("[Events] line type {:?}", clip(key)),
                    ));
                }
            },
            _ => return Err(invalid(&path, "content before any section")),
        }
    }
    let res = |k: &str| -> Result<u32, AssError> {
        let v = info
            .get(k)
            .ok_or_else(|| invalid("[Script Info]", format!("{k} missing")))?;
        v.parse::<u32>().ok().filter(|&v| v > 0).ok_or_else(|| {
            invalid(
                "[Script Info]",
                format!("{k} {:?} is not a positive integer", clip(v)),
            )
        })
    };
    let (play_res_x, play_res_y) = (res("PlayResX")?, res("PlayResY")?);
    if let Some(st) = info.get("ScriptType")
        && !st.eq_ignore_ascii_case("v4.00+")
    {
        return Err(unsupported(
            "[Script Info]",
            format!("ScriptType {}", clip(st)),
        ));
    }
    // libass wraps lines unless WrapStyle is 2 (its default, when absent, is 0); RP never wraps.
    let wrap_style = info.get("WrapStyle").map(String::as_str);
    if wrap_style != Some("2") {
        return Err(unsupported(
            "[Script Info]",
            format!("WrapStyle {:?} (only 2, no wrapping)", wrap_style.map(clip)),
        ));
    }
    let scaled_border_and_shadow = info
        .get("ScaledBorderAndShadow")
        .is_some_and(|v| v.eq_ignore_ascii_case("yes"));
    Ok(AssScript {
        play_res_x,
        play_res_y,
        scaled_border_and_shadow,
        styles,
        dialogues,
    })
}

fn fields<'a>(
    fmt: &[String],
    val: &'a str,
    path: &str,
) -> Result<BTreeMap<String, &'a str>, AssError> {
    // The last field of an event (Text) may contain commas; a style has exactly fmt.len().
    let exact = fmt.last().map(String::as_str) != Some("Text");
    let parts: Vec<&str> = if exact {
        val.split(',').collect()
    } else {
        val.splitn(fmt.len(), ',').collect()
    };
    if parts.len() != fmt.len() {
        return Err(invalid(
            path,
            format!("{} fields, Format names {}", parts.len(), fmt.len()),
        ));
    }
    Ok(fmt.iter().cloned().zip(parts).collect())
}

fn parse_style(fmt: &[String], val: &str, line: &str) -> Result<AssStyle, AssError> {
    let f = fields(fmt, val, line)?;
    let name = f
        .get("Name")
        .ok_or_else(|| invalid(line, "Style without Name"))?
        .trim()
        .to_owned();
    if !is_rp_id(&name) {
        return Err(invalid(
            line,
            format!("style name {:?} is not an RP id", clip(&name)),
        ));
    }
    let path = format!("Style {name}");
    let get = |k: &str| {
        f.get(k)
            .copied()
            .ok_or_else(|| invalid(&path, format!("{k} missing")))
    };
    let num = |k: &str| -> Result<f64, AssError> { parse_num(get(k)?, &path, k) };
    let flag = |k: &str| -> Result<bool, AssError> { Ok(num(k)? != 0.0) };
    for (k, ok) in [
        ("Italic", 0.0),
        ("Underline", 0.0),
        ("StrikeOut", 0.0),
        ("Angle", 0.0),
    ] {
        if f.contains_key(k) && num(k)? != ok {
            return Err(unsupported(&path, format!("{k} {}", clip(get(k)?.trim()))));
        }
    }
    for k in ["ScaleX", "ScaleY"] {
        if f.contains_key(k) && num(k)? != 100.0 {
            return Err(unsupported(&path, format!("{k} {}", clip(get(k)?.trim()))));
        }
    }
    let border_style = num("BorderStyle")?;
    if border_style != 1.0 && border_style != 3.0 {
        return Err(unsupported(&path, format!("BorderStyle {border_style}")));
    }
    let alignment = num("Alignment")?;
    if !(1.0..=9.0).contains(&alignment) || alignment.fract() != 0.0 {
        return Err(invalid(&path, format!("Alignment {alignment}")));
    }
    let fontsize = num("Fontsize")?;
    if fontsize <= 0.0 {
        return Err(invalid(&path, format!("Fontsize {fontsize}")));
    }
    let style = AssStyle {
        fontname: get("Fontname")?.trim().to_owned(),
        fontsize,
        primary: parse_colour(get("PrimaryColour")?, &path, "PrimaryColour")?,
        outline_colour: parse_colour(get("OutlineColour")?, &path, "OutlineColour")?,
        back_colour: parse_colour(get("BackColour")?, &path, "BackColour")?,
        bold: flag("Bold")?,
        spacing: num("Spacing")?,
        border_style: border_style as u8,
        outline: num("Outline")?,
        shadow: num("Shadow")?,
        alignment: alignment as u8,
        name,
    };
    if style.outline < 0.0 || style.shadow < 0.0 {
        return Err(invalid(&path, "negative Outline or Shadow"));
    }
    if style.border_style == 3 && style.shadow != 0.0 {
        return Err(unsupported(&path, "BorderStyle 3 with Shadow (box shadow)"));
    }
    Ok(style)
}

fn parse_dialogue(fmt: &[String], val: &str, index: usize) -> Result<AssDialogue, AssError> {
    let path = format!("Dialogue {index}");
    if fmt.last().map(String::as_str) != Some("Text") {
        return Err(invalid(&path, "[Events] Format must end with Text"));
    }
    let f = fields(fmt, val, &path)?;
    let get = |k: &str| {
        f.get(k)
            .copied()
            .ok_or_else(|| invalid(&path, format!("{k} missing")))
    };
    if let Some(e) = f.get("Effect")
        && !e.trim().is_empty()
    {
        return Err(unsupported(&path, format!("Effect {:?}", clip(e.trim()))));
    }
    let layer = get("Layer")?
        .trim()
        .parse::<i32>()
        .map_err(|_| invalid(&path, "Layer is not an integer"))?;
    let (start_cs, end_cs) = (
        parse_time(get("Start")?, &path)?,
        parse_time(get("End")?, &path)?,
    );
    Ok(AssDialogue {
        index,
        layer,
        start_cs,
        end_cs,
        style: get("Style")?.trim().to_owned(),
        text: get("Text")?.to_owned(),
    })
}

/// What one Dialogue's text says, before styles are applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedText {
    /// `\pos(x,y)` (the first one wins, as in libass).
    pub pos: Option<Point>,
    /// `\an` (the first one wins).
    pub align: Option<u8>,
    /// `\fs`, which must come before any text.
    pub size: Option<f64>,
    /// Runs: text and the `\c` colour in force (`None` = none given yet).
    pub runs: Vec<(String, Option<AssColour>)>,
}

/// Parses a Dialogue's `Text` field against the subset.
pub fn parse_text(text: &str, path: &str) -> Result<ParsedText, AssError> {
    let mut out = ParsedText::default();
    let mut colour: Option<AssColour> = None;
    let mut cur = String::new();
    let mut seen_text = false;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('{') {
            let close = after
                .find('}')
                .ok_or_else(|| invalid(path, "unclosed override block `{`"))?;
            let block = &after[..close];
            rest = &after[close + 1..];
            let mut b = block;
            if !b.starts_with('\\') && !b.is_empty() {
                return Err(invalid(
                    path,
                    format!("override block comment {:?}", clip(block)),
                ));
            }
            while let Some(t) = b.strip_prefix('\\') {
                let end = t.find('\\').unwrap_or(t.len());
                let tag = &t[..end];
                b = &t[end..];
                apply_tag(tag, path, &mut out, &mut colour, &mut cur, seen_text)?;
            }
            continue;
        }
        if let Some(t) = rest.strip_prefix('\\') {
            match t.chars().next() {
                Some('N') => {
                    cur.push('\n');
                    rest = &t[1..];
                    seen_text = true;
                    continue;
                }
                Some(c @ ('n' | 'h')) => return Err(invalid(path, format!("text escape \\{c}"))),
                _ => {}
            }
        }
        let c = rest.chars().next().expect("non-empty");
        cur.push(c);
        seen_text = true;
        rest = &rest[c.len_utf8()..];
    }
    push_run(&mut out, &mut cur, colour);
    Ok(out)
}

fn push_run(out: &mut ParsedText, cur: &mut String, colour: Option<AssColour>) {
    if !cur.is_empty() {
        out.runs.push((std::mem::take(cur), colour));
    }
}

fn apply_tag(
    tag: &str,
    path: &str,
    out: &mut ParsedText,
    colour: &mut Option<AssColour>,
    cur: &mut String,
    seen_text: bool,
) -> Result<(), AssError> {
    // A name is digits then letters (`1c`, `fs`, `pos`); the argument follows.
    let digits = tag.find(|c: char| !c.is_ascii_digit()).unwrap_or(tag.len());
    let name_len = digits
        + tag[digits..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(tag.len() - digits);
    let (name, arg) = tag.split_at(name_len);
    let arg = arg.trim();
    let bad = |why: &str| invalid(path, format!("\\{}: {why}", clip(tag)));
    match name {
        "pos" => {
            let inner = arg
                .strip_prefix('(')
                .and_then(|a| a.strip_suffix(')'))
                .ok_or_else(|| bad("expected (x,y)"))?;
            let (x, y) = inner.split_once(',').ok_or_else(|| bad("expected (x,y)"))?;
            let p = Point {
                x: parse_num(x, path, "\\pos x")?,
                y: parse_num(y, path, "\\pos y")?,
            };
            out.pos.get_or_insert(p);
        }
        "an" => {
            let a: u8 = Some(arg)
                .filter(|a| a.len() == 1 && a.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|a| a.parse().ok())
                .filter(|a| (1..=9).contains(a))
                .ok_or_else(|| bad("expected 1-9"))?;
            out.align.get_or_insert(a);
        }
        "fs" => {
            if seen_text {
                return Err(unsupported(
                    path,
                    format!("\\{} after text (one size per event in v0)", clip(tag)),
                ));
            }
            let s = parse_num(arg, path, "\\fs")?;
            if s <= 0.0 {
                return Err(bad("size must be > 0"));
            }
            out.size = Some(s);
        }
        "c" | "1c" => {
            let c = parse_colour(arg, path, &format!("\\{name}"))?;
            if c.0 > 0xff_ffff {
                return Err(bad("colour has more than 6 hex digits"));
            }
            push_run(out, cur, *colour);
            *colour = Some(c);
        }
        "" => return Err(invalid(path, format!("override tag \\{}", clip(tag)))),
        _ => return Err(invalid(path, format!("override tag \\{}", clip(name)))),
    }
    Ok(())
}

/// Imports a parsed script as RenderPlan text styles and events.
pub fn import(script: &AssScript, opts: &ImportOptions) -> Result<AssImport, AssError> {
    if !FPS_ALLOWED.contains(&opts.fps) {
        return Err(unsupported(
            "fps",
            format!("{} (RP rule 4: 24, 25, 30, 48, 50 or 60)", opts.fps),
        ));
    }
    let (cw, ch) = opts.canvas;
    if (script.play_res_x, script.play_res_y) != (cw, ch) {
        return Err(unsupported(
            "[Script Info]",
            format!(
                "PlayRes {}x{} is not the canvas {cw}x{ch}",
                script.play_res_x, script.play_res_y
            ),
        ));
    }
    // Sizes, widths and positions are canvas px (RP rule 6); bound them by the canvas.
    let limit = f64::from(cw.max(ch));
    let in_range = |v: f64, lo: f64, hi: f64| (lo..=hi).contains(&v);
    if !script.scaled_border_and_shadow {
        return Err(unsupported("[Script Info]", "ScaledBorderAndShadow no"));
    }
    let mut fonts: Vec<(String, String)> = Vec::new();
    let mut styles = BTreeMap::new();
    let mut by_name: BTreeMap<&str, &AssStyle> = BTreeMap::new();
    let mut bold_styles = Vec::new();
    for s in &script.styles {
        let path = format!("Style {}", s.name);
        let id = font_id(&s.fontname);
        if !is_rp_id(&id) {
            return Err(invalid(
                &path,
                format!("Fontname {:?} gives no RP font id", clip(&s.fontname)),
            ));
        }
        match fonts.iter().find(|(i, _)| *i == id) {
            Some((_, family)) if *family != s.fontname => {
                return Err(invalid(
                    &path,
                    format!(
                        "Fontname {:?} collides with {:?} as font id {id}",
                        clip(&s.fontname),
                        clip(family)
                    ),
                ));
            }
            Some(_) => {}
            None => fonts.push((id.clone(), s.fontname.clone())),
        }
        for (what, v, lo) in [
            ("Fontsize", s.fontsize, f64::MIN_POSITIVE),
            ("Outline", s.outline, 0.0),
            ("Shadow", s.shadow, 0.0),
            ("Spacing", s.spacing, -limit),
        ] {
            if !in_range(v, lo, limit) {
                return Err(invalid(&path, format!("{what} {v} outside the canvas")));
            }
        }
        if s.bold {
            bold_styles.push(s.name.clone());
        }
        // Written the way RP §2.3 writes the reference's styles: every field the style uses, explicit.
        let ts = TextStyle {
            font: Id::new(id).map_err(|e| invalid(&path, format!("font id: {e}")))?,
            size: s.fontsize,
            fill: Some(s.primary.rgba().to_plan()),
            outline: (s.border_style == 1 && s.outline > 0.0).then(|| Outline {
                width: Some(s.outline),
                colour: Some(s.outline_colour.rgba().to_plan()),
                join: Some(RoundJoin),
            }),
            shadow: (s.border_style == 1 && s.shadow > 0.0).then(|| Shadow {
                dx: Some(s.shadow),
                dy: Some(s.shadow),
                blur: Some(0.0),
                colour: Some(s.back_colour.rgba().to_plan()),
                of: Some(if s.outline > 0.0 {
                    ShadowOf::FillAndOutline
                } else {
                    ShadowOf::Fill
                }),
            }),
            text_box: (s.border_style == 3).then(|| TextBox {
                colour: Some(s.outline_colour.rgba().to_plan()),
                pad_x: Some(s.outline),
                pad_y: Some(s.outline),
                radius: Some(0.0),
                geometry: Some(LibassBs3),
            }),
            letter_spacing: (s.spacing != 0.0).then_some(s.spacing),
            align: Some(s.alignment),
        };
        if styles.insert(s.name.clone(), ts).is_some() {
            return Err(invalid(format!("Style {}", s.name), "duplicate style name"));
        }
        by_name.insert(s.name.as_str(), s);
    }
    let frame = |cs: u64, path: &str| -> Result<u32, AssError> {
        display_frame(cs, opts.fps)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid(path, "time out of range"))
    };
    let mut events = Vec::new();
    let mut report = Vec::new();
    for d in &script.dialogues {
        let path = format!("Dialogue {}", d.index);
        let st = by_name
            .get(d.style.as_str())
            .ok_or_else(|| invalid(&path, format!("unknown style {:?}", clip(&d.style))))?;
        let parsed = parse_text(&d.text, &path)?;
        let pos = parsed
            .pos
            .ok_or_else(|| unsupported(&path, "event without \\pos"))?;
        if d.end_cs < d.start_cs {
            return Err(invalid(&path, "End before Start"));
        }
        if !in_range(pos.x, -limit, 2.0 * limit) || !in_range(pos.y, -limit, 2.0 * limit) {
            return Err(invalid(&path, "\\pos far outside the canvas"));
        }
        if let Some(fs) = parsed.size
            && fs > limit
        {
            return Err(invalid(&path, format!("\\fs{fs} larger than the canvas")));
        }
        let out = [
            u64::from(frame(d.start_cs, &path)?),
            u64::from(frame(d.end_cs, &path)?),
        ];
        if out[0] >= out[1] {
            report.push(format!(
                "{path}: shows on no frame at {} fps ({} → {} cs), skipped",
                opts.fps, d.start_cs, d.end_cs
            ));
            continue;
        }
        let mut runs: Vec<(String, Option<Colour>)> = Vec::new();
        for (text, c) in parsed.runs {
            // A `\c` colour keeps the style's PrimaryColour alpha, as libass does (RP §2.4's
            // "alpha FF" is the golden case: every story style is opaque).
            let fill = c
                .map(|c| AssColour(c.0 | (st.primary.0 & 0xff00_0000)).rgba())
                .filter(|&f| f != st.primary.rgba());
            match runs.last_mut() {
                Some(last) if last.1 == fill => last.0.push_str(&text),
                _ => runs.push((text, fill)),
            }
        }
        if runs.is_empty() {
            report.push(format!("{path}: no text, skipped"));
            continue;
        }
        events.push(TextEvent {
            id: Id::new(format!("d{:03}", d.index)).expect("dNNN is an id"),
            out: Range(out),
            layer: Some(i64::from(d.layer)),
            role: opts.roles.get(&d.style).copied().unwrap_or(TextRole::Note),
            style: Id::new(d.style.clone()).map_err(|e| invalid(&path, format!("style: {e}")))?,
            pos,
            align: parsed.align,
            size: parsed.size.unwrap_or(st.fontsize),
            runs: runs
                .into_iter()
                .map(|(text, fill)| Run {
                    text,
                    fill: fill.map(Colour::to_plan),
                })
                .collect(),
            provenance: Some(Provenance {
                kind: ProvenanceKind::Import,
                detail: Some(ProvenanceDetail {
                    key: Some(path.clone()),
                    ..ProvenanceDetail::default()
                }),
            }),
            anim: None,
        });
    }
    Ok(AssImport {
        fonts,
        styles,
        events,
        report,
        bold_styles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1080\nPlayResY: 1920\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Speech,Neometric Heavy,86,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,12,7,5,40,40,0,1\nStyle: Hook,Neometric Heavy,88,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,12,7,5,40,40,0,1\nStyle: Pill,Neometric Heavy,50,&H00FFFFFF,&H00FFFFFF,&H30101010,&H00000000,-1,0,0,0,100,100,1,0,3,16,0,7,40,40,0,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

    fn import_str(events: &str) -> Result<AssImport, AssError> {
        import(
            &parse(&format!("{HEAD}{events}"))?,
            &ImportOptions::story(30),
        )
    }

    #[test]
    fn rp_2_3_hook_pill_and_caption() {
        let imp = import_str(concat!(
            "Dialogue: 2,0:00:00.00,0:00:05.87,Hook,,0,0,0,,{\\pos(540,305)}My PC made this\\Nin {\\c&H00D4FF&}67 seconds.\n",
            "Dialogue: 3,0:00:05.87,0:00:25.80,Pill,,0,0,0,,{\\an7\\pos(48,262)\\fs50}MiniMax H3  {\\c&H00D4FF&}running on my PC\n",
            "Dialogue: 1,0:00:01.83,0:00:02.23,Speech,,0,0,0,,{\\pos(540,1065)}we were able\n",
            "Dialogue: 1,0:00:04.87,0:00:05.87,Speech,,0,0,0,,{\\pos(540,1065)}{\\c&H00D4FF&}two{\\c&HFFFFFF&} {\\c&H00D4FF&}minutes.{\\c&HFFFFFF&}\n",
            "Dialogue: 1,0:00:11.63,0:00:13.23,Speech,,0,0,0,,{\\pos(540,1065)}peeling itself.\n",
        ))
        .unwrap();
        assert_eq!(
            imp.fonts,
            vec![("neometric-heavy".to_owned(), "Neometric Heavy".to_owned())]
        );
        // The styles equal RP §2.3's (schemas/examples/h79-parts-1-3.renderplan.json) exactly.
        let example: gmclip_plan::Plan = serde_json::from_str(include_str!(
            "../../../../schemas/examples/h79-parts-1-3.renderplan.json"
        ))
        .unwrap();
        let want = example.text_styles.unwrap();
        for (name, st) in &imp.styles {
            assert_eq!(
                Some(st),
                want.get(&Id::new(name.clone()).unwrap()),
                "style {name}"
            );
        }
        // Events equal the example's on everything the ASS carries (ids/provenance are the
        // importer's own; the example's hook adds an explicit align 5 the ASS leaves to the style).
        let got: Vec<_> = imp
            .events
            .iter()
            .map(|e| {
                (
                    e.role,
                    e.out,
                    e.layer,
                    e.style.clone(),
                    e.size,
                    e.pos,
                    e.runs.clone(),
                )
            })
            .collect();
        let ex = example.text_events.unwrap();
        let pick = |id: &str| ex.iter().find(|e| e.id.as_str() == id).unwrap();
        for (g, id) in got.iter().zip(["hook", "pill-1", "c01", "c05", "c12"]) {
            let e = pick(id);
            assert_eq!(
                (g.0, g.2, &g.3, g.4, g.5, &g.6),
                (e.role, e.layer, &e.style, e.size, e.pos, &e.runs),
                "{id}"
            );
            // The example clips the pill to its 397 frames; the full pill runs to 25.80 s.
            let want_out = if id == "pill-1" {
                Range([177, 774])
            } else {
                e.out
            };
            assert_eq!(g.1, want_out, "{id} out");
        }
        assert_eq!(imp.events[1].align, Some(7));
        assert_eq!(imp.events[0].align, None);
    }

    #[test]
    fn unknown_tags_are_invalid_by_name() {
        for (tag, name) in [
            ("\\blur3", "\\blur"),
            ("\\fscx120", "\\fscx"),
            ("\\fsp2", "\\fsp"),
            ("\\3c&H000000&", "\\3c"),
            ("\\k20", "\\k"),
            ("\\move(1,2,3,4)", "\\move"),
        ] {
            let err = import_str(&format!(
                "Dialogue: 1,0:00:00.00,0:00:01.00,Speech,,0,0,0,,{{\\pos(540,1065){tag}}}hi\n"
            ))
            .unwrap_err();
            match err {
                AssError::Invalid { path, reason } => {
                    assert_eq!(path, "Dialogue 1");
                    assert!(reason.contains(name), "{reason} names {name}");
                }
                other => panic!("{tag}: {other:?}"),
            }
        }
    }

    #[test]
    fn subset_limits_are_reported() {
        let cases = [
            ("{\\pos(1,2)}a\\nb", "\\n"),
            ("{\\pos(1,2)}a\\hb", "\\h"),
            ("{\\pos(1,2)}a{\\fs40}b", "\\fs40"),
            ("{\\pos(1,2)}a{comment}", "comment"),
            ("{\\pos(1,2)a", "unclosed"),
            ("{\\pos(1,2)}{\\an0}a", "\\an0"),
            ("{\\pos(1,2)}{\\c&H1234567&}a", "6 hex"),
        ];
        for (text, needle) in cases {
            let err = import_str(&format!(
                "Dialogue: 1,0:00:00.00,0:00:01.00,Speech,,0,0,0,,{text}\n"
            ))
            .unwrap_err();
            assert!(err.to_string().contains(needle), "{text}: {err}");
        }
        let err = import_str("Dialogue: 1,0:00:00.00,0:00:01.00,Speech,,0,0,0,,hi\n").unwrap_err();
        assert!(matches!(err, AssError::Unsupported { .. }), "{err}");
        let err = import_str("Dialogue: 1,0:00:00.00,0:00:01.00,Nope,,0,0,0,,{\\pos(1,2)}hi\n")
            .unwrap_err();
        assert!(err.to_string().contains("unknown style"), "{err}");
        let err = import_str("Dialogue: 1,0:0:00.00,0:00:01.00,Speech,,0,0,0,,{\\pos(1,2)}hi\n")
            .unwrap_err();
        assert!(err.to_string().contains("H:MM:SS.CC"), "{err}");
    }

    #[test]
    fn display_rule_and_empty_events() {
        // 2.67 s is frame 80.1 → shown from frame 81; a 0.01 s event between two frame times
        // shows on no frame and is reported, not imported.
        let imp = import_str(concat!(
            "Dialogue: 1,0:00:02.67,0:00:02.68,Speech,,0,0,0,,{\\pos(1,2)}a\n",
            "Dialogue: 1,0:00:02.67,0:00:03.63,Speech,,0,0,0,,{\\pos(1,2)}b, with, commas\n",
        ))
        .unwrap();
        assert_eq!(imp.events.len(), 1);
        assert_eq!(imp.events[0].out, Range([81, 109]));
        assert_eq!(super::super::event_text(&imp.events[0]), "b, with, commas");
        assert_eq!(imp.report.len(), 1);
    }

    #[test]
    fn style_features_outside_v0_are_unsupported() {
        let s = HEAD.replace("Style: Hook,Neometric Heavy,88,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100", "Style: Hook,Neometric Heavy,88,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,1,0,0,100,100");
        let err = parse(&s).unwrap_err();
        assert_eq!(
            err,
            AssError::Unsupported {
                path: "Style Hook".into(),
                reason: "Italic 1".into()
            }
        );
    }

    #[test]
    fn font_ids() {
        assert_eq!(font_id("Neometric Heavy"), "neometric-heavy");
        assert_eq!(font_id(" Archivo Black! "), "archivo-black");
    }

    #[test]
    fn tag_forms_and_bounds() {
        let ev = |text: &str| {
            import_str(&format!(
                "Dialogue: 1,0:00:00.00,0:00:01.00,Speech,,0,0,0,,{text}\n"
            ))
        };
        let imp = ev("{\\pos( 540 , 1065.5 )\\fs50.5\\1c&H00D4FF&}a").unwrap();
        let e = &imp.events[0];
        assert_eq!(
            (e.pos, e.size),
            (
                Point {
                    x: 540.0,
                    y: 1065.5
                },
                50.5
            )
        );
        assert_eq!(e.runs[0].fill, Some(Colour::YELLOW.to_plan()));
        for (text, needle) in [
            ("{\\pos(1,2)\\an10}a", "\\an10"),
            ("{\\pos(1,2)\\an+5}a", "\\an+5"),
            ("{\\pos(1,2)\\12c&HFFFFFF&}a", "\\12c"),
            ("{\\pos(1,2)\\fs3000}a", "larger than the canvas"),
            ("{\\pos(99999,2)}a", "outside"),
        ] {
            let err = ev(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text}: {err}");
        }
        let long = "x".repeat(200);
        let err = ev(&format!("{{\\pos(1,2)\\{long}}}a"))
            .unwrap_err()
            .to_string();
        assert!(err.len() < 80, "echo is clipped: {err}");
    }

    #[test]
    fn times_never_overflow() {
        for t in ["99999999999999:00:00.00", "999999999:59:59.99"] {
            let r = import_str(&format!(
                "Dialogue: 1,{t},{t},Speech,,0,0,0,,{{\\pos(1,2)}}a\n"
            ));
            assert!(r.is_err(), "{t}");
        }
        let err = import_str("Dialogue: 1,0:00:02.00,0:00:01.00,Speech,,0,0,0,,{\\pos(1,2)}a\n")
            .unwrap_err();
        assert!(err.to_string().contains("End before Start"), "{err}");
    }

    #[test]
    fn other_rates_and_canvas() {
        let script = parse(&format!(
            "{HEAD}Dialogue: 1,0:00:00.03,0:00:00.05,Speech,,0,0,0,,{{\\pos(1,2)}}a\n"
        ))
        .unwrap();
        let imp = import(&script, &ImportOptions::story(25)).unwrap();
        assert_eq!(imp.events[0].out, Range([1, 2]));
        let mut o = ImportOptions::story(30);
        o.fps = 29;
        assert!(import(&script, &o).is_err());
        o = ImportOptions::story(30);
        o.canvas = (1920, 1080);
        assert!(
            import(&script, &o)
                .unwrap_err()
                .to_string()
                .contains("PlayRes")
        );
        assert_eq!(
            import(&script, &ImportOptions::story(30))
                .unwrap()
                .bold_styles,
            vec!["Speech", "Hook", "Pill"]
        );
        assert!(parse(&HEAD.replace("WrapStyle: 2", "WrapStyle: 0")).is_err());
        assert!(parse(&HEAD.replace("WrapStyle: 2\n", "")).is_err());
        assert!(
            parse(&HEAD.replace("40,40,0,1\nStyle: Hook", "40,40,0,1,junk\nStyle: Hook")).is_err()
        );
        assert!(parse(&HEAD.replace("Style: Hook,", "Style: Ho ok,")).is_err());
    }

    #[test]
    fn display_frame_is_rts_ffmpeg_rule() {
        assert_eq!(display_frame(1330, 30), Some(400)); // h79 "let's also try"
        assert_eq!(display_frame(370, 30), Some(112)); // decision 0003's reproduction
        assert_eq!(display_frame(1333, 30), Some(400));
        assert_eq!(display_frame(1340, 30), Some(402));
        assert_eq!(display_frame(0, 30), Some(0));
        assert_eq!(display_frame(u64::MAX, 30), None);
        // Against a frame-by-frame scan of the truncated milliseconds, 0–200 s at 25/30/60 fps.
        for fps in [25u32, 30, 60] {
            let ms_of = |f: u64| (f as f64 * (1.0 / f64::from(fps)) * 1000.0) as u64;
            let mut f = 0;
            for cs in 0..20_000u64 {
                while ms_of(f) < cs * 10 {
                    f += 1;
                }
                assert_eq!(display_frame(cs, fps), Some(f), "cs {cs} at {fps} fps");
            }
        }
    }
}
