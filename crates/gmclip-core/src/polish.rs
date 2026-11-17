//! Edit polish (Ideas 2026-10-16, lane K2; decision 0025): the pure parts of the five
//! polish suggestions. The host measures (level frames, luma frames), these decide.
//!
//! - **Punch-in** ([`punch_crop`], [`phrase_end`], [`pick_rare`]): at a stressed word, a
//!   small eased zoom on the screen layer (6–10 %), held for the phrase, then back; never
//!   two within [`PUNCH_GAP_MS`].
//! - **Callouts** ([`callout_hits`], [`place_label`], [`arrow_into`]): a number, a product
//!   or an on-screen thing named in the words, placed inside the safe zones, away from the
//!   captions and the face.
//! - **Loop** ([`loop_state`]): whether the last frame shows the opening's layout and crop.
//! - **Hold screen** ([`change_fraction`], [`screen_jumps`], [`mid_sentence`]): a big change
//!   of the screen region (against the cut's own steps) that no speech boundary explains.

use gmclip_model::entities::{Cut, Framing, LayerRole, Part};
use gmclip_model::geometry::NormRect;
use gmclip_model::{Doc, Flicks, Id};
use serde::Serialize;

use crate::activity::{LumaFrame, NRect};

// ---- punch-in ---------------------------------------------------------------------------

/// Default zoom of a punch-in (8 %); [`PUNCH_MIN`]..=[`PUNCH_MAX`] allowed.
pub const PUNCH_SCALE: f64 = 0.08;
pub const PUNCH_MIN: f64 = 0.06;
pub const PUNCH_MAX: f64 = 0.10;
/// Two punch-ins are at least this far apart (start to start).
pub const PUNCH_GAP_MS: i64 = 3_000;
/// The eased zoom in and the eased return.
pub const PUNCH_IN_MS: u32 = 220;
pub const PUNCH_OUT_MS: u32 = 320;
/// A punch-in holds for its phrase: at least this long, at most that long.
pub const PHRASE_MIN_MS: i64 = 700;
pub const PHRASE_MAX_MS: i64 = 2_500;
/// A gap of this much after a word ends its phrase.
pub const PHRASE_GAP_MS: i64 = 250;

/// `c` zoomed in by `1 + scale` about its centre (the frame shows `scale` more of less).
pub fn punch_crop(c: NormRect, scale: f64) -> NormRect {
    let k = 1.0 + scale.clamp(PUNCH_MIN, PUNCH_MAX);
    let (w, h) = (c.w / k, c.h / k);
    NormRect {
        x: c.x + (c.w - w) / 2.0,
        y: c.y + (c.h - h) / 2.0,
        w,
        h,
    }
}

/// One word on a clock (ms).
#[derive(Clone, Debug, PartialEq)]
pub struct TWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

fn ends_sentence(t: &str) -> bool {
    t.trim_end().ends_with(['.', '?', '!'])
}

/// Where the phrase holding word `i` ends: the end of the first word from `i` on that
/// ends a sentence or a clause, or is followed by a pause of [`PHRASE_GAP_MS`]; at least
/// [`PHRASE_MIN_MS`] and at most [`PHRASE_MAX_MS`] after the word starts.
pub fn phrase_end(words: &[TWord], i: usize) -> i64 {
    let t0 = words[i].start_ms;
    let mut end = words[i].end_ms;
    for j in i..words.len() {
        end = words[j].end_ms;
        let last = j + 1 == words.len()
            || words[j]
                .text
                .trim_end()
                .ends_with(['.', '?', '!', ',', ';', ':'])
            || words[j + 1].start_ms - words[j].end_ms >= PHRASE_GAP_MS;
        if last || end - t0 >= PHRASE_MAX_MS {
            break;
        }
    }
    end.clamp(t0 + PHRASE_MIN_MS, t0 + PHRASE_MAX_MS)
}

/// The rare few of `(time ms, score)` candidates: strongest first, none within `gap_ms`
/// of one already kept. Returns indices in time order.
pub fn pick_rare(cands: &[(i64, f64)], gap_ms: i64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..cands.len()).collect();
    idx.sort_by(|a, b| cands[*b].1.total_cmp(&cands[*a].1).then(a.cmp(b)));
    let mut kept: Vec<usize> = Vec::new();
    for i in idx {
        if kept
            .iter()
            .all(|k| (cands[*k].0 - cands[i].0).abs() >= gap_ms)
        {
            kept.push(i);
        }
    }
    kept.sort_by_key(|i| (cands[*i].0, *i));
    kept
}

// ---- callouts ---------------------------------------------------------------------------

/// What a callout is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalloutKind {
    /// A number said ("67 seconds", "5080"): a label quoting it.
    Number,
    /// A product or name ("MiniMax", "ComfyUI", a project term): a label.
    Product,
    /// Something on the screen ("this button", "right here"): an arrow into the panel.
    Thing,
}

/// A run of words that names something worth a callout.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CalloutHit {
    /// Words `start..end` of the input.
    pub start: usize,
    pub end: usize,
    pub kind: CalloutKind,
    /// The label's text (the words as said, or the term as written).
    pub text: String,
    /// Why, in gmclip's words.
    pub why: String,
}

const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
    "half",
];
const UNITS: &[&str] = &[
    "seconds",
    "second",
    "minutes",
    "minute",
    "hours",
    "hour",
    "days",
    "day",
    "percent",
    "%",
    "times",
    "x",
    "gb",
    "mb",
    "tb",
    "gigs",
    "fps",
    "dollars",
    "bucks",
    "k",
    "frames",
    "tokens",
    "words",
    "steps",
    "lines",
    "users",
    "views",
    "ms",
    "milliseconds",
    "gigabytes",
    "cores",
];
/// Words that, after "this"/"that", name something on the screen.
const THINGS: &[&str] = &[
    "button", "tab", "menu", "box", "panel", "setting", "settings", "field", "option", "icon",
    "link", "window", "page", "bar", "slider", "node", "dropdown", "toggle", "checkbox", "line",
    "error", "message", "column", "row", "file", "folder", "graph", "chart", "number", "card",
];
const PLACES: &[&[&str]] = &[
    &["right", "here"],
    &["over", "here"],
    &["up", "here"],
    &["down", "here"],
    &["look", "at", "this"],
    &["see", "this"],
    &["click", "here"],
    &["click", "on"],
];
/// Common words that are written in capitals but are not names.
const NOT_NAMES: &[&str] = &[
    "i", "i'm", "i've", "i'll", "i'd", "ok", "okay", "tv", "am", "pm",
];

/// The bare lower-case token (letters, digits, `'`, `%`, `$`, `.` inside a word).
fn bare(s: &str) -> String {
    let t = s.trim_matches(|c: char| !c.is_alphanumeric() && c != '%' && c != '$');
    t.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '\'' | '%' | '$' | '.'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_number(t: &str) -> bool {
    t.chars().any(|c| c.is_ascii_digit()) || NUMBER_WORDS.contains(&t)
}

/// Written like a name: all capitals (2+ letters), CamelCase, or letters and digits
/// together ("H3", "GPT4") — not a common word written in capitals.
fn looks_like_name(raw: &str) -> bool {
    let w: String = raw.trim_matches(|c: char| !c.is_alphanumeric()).to_owned();
    let letters: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() < 2 || NOT_NAMES.contains(&bare(&w).as_str()) {
        return false;
    }
    let caps = letters.iter().all(|c| c.is_uppercase());
    let camel = w
        .chars()
        .zip(w.chars().skip(1))
        .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    let mixed = w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(char::is_alphabetic);
    caps || camel || mixed
}

/// The callout candidates of a cut's words (each run once, first mention only):
/// numbers with their unit, project terms and name-like words, and on-screen things.
pub fn callout_hits(words: &[&str], terms: &[String]) -> Vec<CalloutHit> {
    let toks: Vec<String> = words.iter().map(|w| bare(w)).collect();
    let term_toks: Vec<(Vec<String>, &String)> = terms
        .iter()
        .map(|t| (t.split_whitespace().map(bare).collect::<Vec<_>>(), t))
        .filter(|(v, _)| !v.is_empty() && v.len() <= 4)
        .collect();
    let mut out: Vec<CalloutHit> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<CalloutHit>, seen: &mut Vec<String>, h: CalloutHit| {
        let key = h.text.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            out.push(h);
        }
    };
    while i < toks.len() {
        let t = toks[i].as_str();
        // a project term (longest first)
        if let Some((v, term)) = term_toks
            .iter()
            .filter(|(v, _)| i + v.len() <= toks.len() && toks[i..i + v.len()] == v[..])
            .max_by_key(|(v, _)| v.len())
        {
            let n = v.len();
            push(
                &mut out,
                &mut seen,
                CalloutHit {
                    start: i,
                    end: i + n,
                    kind: CalloutKind::Product,
                    text: (*term).clone(),
                    why: "names a project term".into(),
                },
            );
            i += n;
            continue;
        }
        if is_number(t) && !(t == "one" && i + 1 < toks.len() && !is_unit(&toks[i + 1])) {
            // the number's words and its unit
            let mut j = i + 1;
            while j < toks.len() && is_number(&toks[j]) && j - i < 4 {
                j += 1;
            }
            if j < toks.len() && is_unit(&toks[j]) {
                j += 1;
            }
            // a lone small number word with no unit ("two of them") is not worth a label
            let lone_word = j == i + 1 && !t.chars().any(|c| c.is_ascii_digit());
            if !lone_word {
                let text = words[i..j]
                    .iter()
                    .map(|w| {
                        w.trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ';' | ':'))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                push(
                    &mut out,
                    &mut seen,
                    CalloutHit {
                        start: i,
                        end: j,
                        kind: CalloutKind::Number,
                        text,
                        why: "says a number".into(),
                    },
                );
                i = j;
                continue;
            }
        }
        if (t == "this" || t == "that")
            && i + 1 < toks.len()
            && THINGS.contains(&toks[i + 1].as_str())
        {
            push(
                &mut out,
                &mut seen,
                CalloutHit {
                    start: i,
                    end: i + 2,
                    kind: CalloutKind::Thing,
                    text: format!("{} {}", toks[i], toks[i + 1]),
                    why: "points at something on the screen".into(),
                },
            );
            i += 2;
            continue;
        }
        if let Some(p) = PLACES
            .iter()
            .find(|p| i + p.len() <= toks.len() && p.iter().zip(&toks[i..]).all(|(a, b)| a == b))
        {
            push(
                &mut out,
                &mut seen,
                CalloutHit {
                    start: i,
                    end: i + p.len(),
                    kind: CalloutKind::Thing,
                    text: p.join(" "),
                    why: "points at something on the screen".into(),
                },
            );
            i += p.len();
            continue;
        }
        if looks_like_name(words[i]) {
            let w = words[i].trim_matches(|c: char| !c.is_alphanumeric());
            push(
                &mut out,
                &mut seen,
                CalloutHit {
                    start: i,
                    end: i + 1,
                    kind: CalloutKind::Product,
                    text: w.to_owned(),
                    why: "names a product (written like a name)".into(),
                },
            );
        }
        i += 1;
    }
    out
}

fn is_unit(t: &str) -> bool {
    UNITS.contains(&t)
}

/// A rectangle in canvas pixels.
#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
    fn inside(&self, o: &Rect) -> bool {
        self.x >= o.x
            && self.y >= o.y
            && self.x + self.w <= o.x + o.w
            && self.y + self.h <= o.y + o.h
    }
}

/// Instagram's overlay zones on a `w`×`h` canvas (SPEC §5.7, measured at 1080×1920 and
/// scaled): the top 220 px, the bottom 360 px, the right 120 px between y 1000–1560.
pub fn overlay_zones(w: f64, h: f64) -> Vec<Rect> {
    let (sx, sy) = (w / 1080.0, h / 1920.0);
    vec![
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h: 220.0 * sy,
        },
        Rect {
            x: 0.0,
            y: h - 360.0 * sy,
            w,
            h: 360.0 * sy,
        },
        Rect {
            x: w - 120.0 * sx,
            y: 1000.0 * sy,
            w: 120.0 * sx,
            h: 560.0 * sy,
        },
    ]
}

/// Where a `size` label goes: the first spot (rows from the top of `prefer`, then of the
/// canvas; left, centre, right) inside the canvas with a 48 px margin, outside every
/// overlay zone and every `avoid` rectangle (captions, the face, the hook).
pub fn place_label(
    canvas: (f64, f64),
    prefer: Option<Rect>,
    avoid: &[Rect],
    size: (f64, f64),
) -> Option<Rect> {
    let (cw, ch) = canvas;
    let m = 48.0 * cw / 1080.0;
    let room = Rect {
        x: m,
        y: 0.0,
        w: cw - 2.0 * m,
        h: ch,
    };
    let mut blocked = overlay_zones(cw, ch);
    blocked.extend_from_slice(avoid);
    let step = 20.0 * ch / 1920.0;
    let areas: Vec<Rect> = prefer.into_iter().chain(std::iter::once(room)).collect();
    for a in areas {
        let mut y = a.y.max(0.0);
        while y + size.1 <= a.y + a.h {
            for x in [
                a.x + m * 0.5,
                a.x + (a.w - size.0) / 2.0,
                a.x + a.w - size.0 - m * 0.5,
            ] {
                let r = Rect {
                    x,
                    y,
                    w: size.0,
                    h: size.1,
                };
                if r.inside(&room) && blocked.iter().all(|b| !b.intersects(&r)) {
                    return Some(r);
                }
            }
            y += step;
        }
    }
    None
}

/// An arrow into `target` (its centre): from a free spot beside it (above, below, left,
/// right of the target, about 260 px long at 1080 wide), clear of the zones and `avoid`.
/// `(from, to)` in canvas pixels.
pub fn arrow_into(
    canvas: (f64, f64),
    target: Rect,
    avoid: &[Rect],
) -> Option<((f64, f64), (f64, f64))> {
    let (cw, ch) = canvas;
    let to = (target.x + target.w / 2.0, target.y + target.h / 2.0);
    let len = 260.0 * cw / 1080.0;
    let mut blocked = overlay_zones(cw, ch);
    blocked.extend_from_slice(avoid);
    let pad = 30.0 * cw / 1080.0;
    for (dx, dy) in [
        (0.55, -0.85),
        (-0.55, -0.85),
        (0.55, 0.85),
        (-0.55, 0.85),
        (1.0, 0.0),
        (-1.0, 0.0),
    ] {
        let from = (to.0 + dx * len, to.1 + dy * len);
        let r = Rect {
            x: from.0.min(to.0) - pad,
            y: from.1.min(to.1) - pad,
            w: (from.0 - to.0).abs() + 2.0 * pad,
            h: (from.1 - to.1).abs() + 2.0 * pad,
        };
        let inside = from.0 > pad && from.0 < cw - pad && from.1 > pad && from.1 < ch - pad;
        if inside && blocked.iter().all(|b| !b.intersects(&r)) {
            return Some((from, to));
        }
    }
    None
}

// ---- hold screen ------------------------------------------------------------------------

/// A luma step that counts as change (as the activity crop's).
pub const DIFF_THRESHOLD: u8 = crate::activity::DIFF_THRESHOLD;
/// A jump changes at least this share of the screen region's cells …
pub const JUMP_FRAC: f64 = 0.35;
/// … and is this far (robust z) above the cut's own steps.
pub const JUMP_Z: f64 = 4.0;
/// A speech boundary this close to a jump explains it.
pub const BOUNDARY_MS: i64 = 400;
/// The sentence must go on at least this long after the jump to be worth a hold.
pub const MIN_HOLD_MS: i64 = 500;
const CELL: u32 = 8;

/// The share of the region's cells (8×8 px, `exclude` areas skipped) whose mean luma
/// moved by [`DIFF_THRESHOLD`] or more between `a` and `b`.
pub fn change_fraction(a: &LumaFrame, b: &LumaFrame, region: &NRect, exclude: &[NRect]) -> f64 {
    if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
        return 0.0;
    }
    let (w, h) = (a.width, a.height);
    let px = |v: f64, n: u32| ((v.clamp(0.0, 1.0) * f64::from(n)).round() as u32).min(n);
    let (x0, x1) = (px(region.x, w), px(region.x + region.w, w));
    let (y0, y1) = (px(region.y, h), px(region.y + region.h, h));
    let (mut changed, mut cells) = (0u32, 0u32);
    let mut cy = y0;
    while cy + CELL <= y1.max(y0 + CELL).min(h) {
        let mut cx = x0;
        while cx + CELL <= x1.max(x0 + CELL).min(w) {
            let (fx, fy) = (
                (f64::from(cx) + f64::from(CELL) / 2.0) / f64::from(w),
                (f64::from(cy) + f64::from(CELL) / 2.0) / f64::from(h),
            );
            let skip = exclude
                .iter()
                .any(|e| fx >= e.x && fx < e.x + e.w && fy >= e.y && fy < e.y + e.h);
            if !skip {
                let (mut sa, mut sb) = (0u32, 0u32);
                for y in cy..cy + CELL {
                    let row = (y * w) as usize;
                    for x in cx..cx + CELL {
                        sa += u32::from(a.data[row + x as usize]);
                        sb += u32::from(b.data[row + x as usize]);
                    }
                }
                let n = CELL * CELL;
                cells += 1;
                if sa.abs_diff(sb) >= u32::from(DIFF_THRESHOLD) * n {
                    changed += 1;
                }
            }
            cx += CELL;
        }
        cy += CELL;
    }
    if cells == 0 {
        0.0
    } else {
        f64::from(changed) / f64::from(cells)
    }
}

/// A big change of the screen between sample `step` and `step + 1`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScreenJump {
    pub step: usize,
    /// Share of the region's cells that changed.
    pub fraction: f64,
    /// Robust z of that share against the cut's own steps.
    pub z: f64,
}

/// The jumps among `fracs` (one per step between consecutive samples): at least
/// [`JUMP_FRAC`] of the region and [`JUMP_Z`] above the median step (MAD scale).
pub fn screen_jumps(fracs: &[f64]) -> Vec<ScreenJump> {
    if fracs.len() < 3 {
        return Vec::new();
    }
    let mut v = fracs.to_vec();
    v.sort_by(f64::total_cmp);
    let med = v[v.len() / 2];
    let mut dev: Vec<f64> = fracs.iter().map(|x| (x - med).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let scale = (1.4826 * dev[dev.len() / 2]).max(0.01);
    fracs
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let z = (f - med) / scale;
            (*f >= JUMP_FRAC && z >= JUMP_Z).then_some(ScreenJump {
                step: i,
                fraction: *f,
                z,
            })
        })
        .collect()
}

/// When `t_ms` falls inside a sentence (a word of it started at least [`BOUNDARY_MS`]
/// before, it goes on at least [`MIN_HOLD_MS`] after, and no sentence ends within
/// [`BOUNDARY_MS`] of `t_ms`): the index of the sentence's last word.
pub fn mid_sentence(words: &[TWord], t_ms: i64) -> Option<usize> {
    // sentences as word index ranges
    let mut start = 0;
    for i in 0..words.len() {
        let end_here = i + 1 == words.len() || ends_sentence(&words[i].text);
        if !end_here {
            continue;
        }
        let (a, b) = (start, i);
        start = i + 1;
        let (s0, s1) = (words[a].start_ms, words[b].end_ms);
        if t_ms < s0 || t_ms > s1 {
            continue;
        }
        if t_ms - s0 < BOUNDARY_MS || s1 - t_ms < MIN_HOLD_MS.max(BOUNDARY_MS) {
            return None;
        }
        return Some(b);
    }
    None
}

// ---- loop -------------------------------------------------------------------------------

/// Whether a cut's last frame shows its opening's layout and crop, in one framing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoopState {
    pub first: Id<Part>,
    pub last: Id<Part>,
    pub matches: bool,
    /// What differs, in gmclip's words ("screen crop", "camera place", "a layer only at
    /// the end").
    pub differences: Vec<String>,
    /// The last part has visual splits in this framing (the layout fix then removes them;
    /// the tail fix keeps them).
    pub last_has_splits: bool,
}

fn role_name(r: LayerRole) -> String {
    match r {
        LayerRole::Region(x) => serde_json::to_value(x)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "layer".into()),
        LayerRole::Detail => "detail".into(),
        LayerRole::Other => "layer".into(),
    }
}

/// [`LoopState`] of `cut` in `framing`: the visible layers at the first frame of part
/// one against those at the last frame of the last part (role, crop, place, opacity;
/// within 0.002 of the canvas).
pub fn loop_state(doc: &Doc, cut: Id<Cut>, framing: Id<Framing>, fpf: i64) -> Option<LoopState> {
    let layout = gmclip_model::clock::cut_layout(doc, cut);
    let (f, l) = (layout.first()?.part, layout.last()?.part);
    let (pf, pl) = (doc.get(f)?, doc.get(l)?);
    let a = gmclip_model::effective::effective(doc, f, framing, pf.src_in)?;
    let end = Flicks((pl.src_out.0 - fpf.max(1)).max(pl.src_in.0));
    let b = gmclip_model::effective::effective(doc, l, framing, end)?;
    let vis = |s: &gmclip_model::ResolvedStack| {
        let mut v: Vec<_> = s
            .layers
            .iter()
            .filter(|x| x.values.visible && x.values.opacity > 0.0)
            .map(|x| x.values.clone())
            .collect();
        v.sort_by(|p, q| p.role.cmp(&q.role).then(p.z.cmp(&q.z)));
        v
    };
    let (va, vb) = (vis(&a), vis(&b));
    let near = |x: f64, y: f64| (x - y).abs() <= 0.002;
    let mut diffs = Vec::new();
    for x in &va {
        match vb
            .iter()
            .find(|y| y.role == x.role && y.z == x.z)
            .or_else(|| vb.iter().find(|y| y.role == x.role))
        {
            None => diffs.push(format!("{} only at the opening", role_name(x.role))),
            Some(y) => {
                let (c, d) = (x.crop, y.crop);
                if !(near(c.x, d.x) && near(c.y, d.y) && near(c.w, d.w) && near(c.h, d.h)) {
                    diffs.push(format!("{} crop", role_name(x.role)));
                }
                let (p, q) = (x.place, y.place);
                if !(near(p.x, q.x) && near(p.y, q.y) && near(p.w, q.w)) {
                    diffs.push(format!("{} place", role_name(x.role)));
                }
                if !near(x.opacity, y.opacity) {
                    diffs.push(format!("{} opacity", role_name(x.role)));
                }
            }
        }
    }
    for y in &vb {
        if !va.iter().any(|x| x.role == y.role) {
            diffs.push(format!("{} only at the end", role_name(y.role)));
        }
    }
    diffs.dedup();
    let last_has_splits = !gmclip_model::effective::splits_of(doc, l, framing).is_empty();
    Some(LoopState {
        first: f,
        last: l,
        matches: diffs.is_empty(),
        differences: diffs,
        last_has_splits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: i64, e: i64) -> TWord {
        TWord {
            text: text.into(),
            start_ms: s,
            end_ms: e,
        }
    }

    #[test]
    fn a_punch_in_zooms_about_the_centre_and_stays_rare() {
        let c = NormRect {
            x: 0.1,
            y: 0.2,
            w: 0.5,
            h: 0.4,
        };
        let p = punch_crop(c, 0.08);
        assert!((p.w - 0.5 / 1.08).abs() < 1e-12);
        assert!(((p.x + p.w / 2.0) - 0.35).abs() < 1e-12, "centre kept");
        assert!(((p.y + p.h / 2.0) - 0.4).abs() < 1e-12);
        // clamped to 6–10 %
        assert!((punch_crop(c, 0.5).w - 0.5 / 1.10).abs() < 1e-12);
        // never two within 3 s: the strongest of a close pair wins
        let picks = pick_rare(
            &[(1_000, 2.0), (2_500, 3.0), (5_600, 2.2), (8_000, 2.1)],
            PUNCH_GAP_MS,
        );
        assert_eq!(picks, vec![1, 2]);
    }

    #[test]
    fn a_phrase_ends_at_a_pause_or_a_clause() {
        let ws = vec![
            w("it", 0, 100),
            w("took", 120, 300),
            w("sixty", 320, 600),
            w("seven", 620, 900),
            w("seconds,", 920, 1400),
            w("and", 1420, 1500),
        ];
        assert_eq!(phrase_end(&ws, 2), 1400);
        // a pause after "took"
        let mut p = ws.clone();
        p[2].start_ms = 700;
        assert_eq!(phrase_end(&p, 0), 700, "at least PHRASE_MIN_MS");
        assert_eq!(phrase_end(&p, 1), 120 + PHRASE_MIN_MS);
    }

    #[test]
    fn callouts_find_numbers_names_terms_and_things() {
        let text = "So MiniMax H3 made this in 67 seconds on my NVIDIA card, two of them. Click on this button and the minimax model loads in two minutes.";
        let words: Vec<&str> = text.split_whitespace().collect();
        let hits = callout_hits(&words, &["MiniMax H3".into()]);
        let got: Vec<(CalloutKind, &str)> =
            hits.iter().map(|h| (h.kind, h.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (CalloutKind::Product, "MiniMax H3"),
                (CalloutKind::Number, "67 seconds"),
                (CalloutKind::Product, "NVIDIA"),
                (CalloutKind::Thing, "click on"),
                (CalloutKind::Thing, "this button"),
                (CalloutKind::Number, "two minutes"),
            ]
        );
        assert_eq!(hits[1].start, 6);
        assert_eq!(hits[1].end, 8);
    }

    #[test]
    fn labels_avoid_zones_captions_and_the_face() {
        let canvas = (1080.0, 1920.0);
        let captions = Rect {
            x: 0.0,
            y: 1000.0,
            w: 1080.0,
            h: 140.0,
        };
        let face = Rect {
            x: 160.0,
            y: 1120.0,
            w: 760.0,
            h: 520.0,
        };
        let panel = Rect {
            x: 0.0,
            y: 250.0,
            w: 1080.0,
            h: 760.0,
        };
        let hook = Rect {
            x: 0.0,
            y: 220.0,
            w: 1080.0,
            h: 200.0,
        };
        let r = place_label(canvas, Some(panel), &[captions, face, hook], (400.0, 90.0)).unwrap();
        assert!(r.y >= 420.0 && r.y + r.h <= 1000.0, "{r:?}");
        assert!(r.x >= 48.0 && r.x + r.w <= 1032.0, "{r:?}");
        for z in overlay_zones(1080.0, 1920.0) {
            assert!(!z.intersects(&r));
        }
        // no room anywhere: none
        let all = Rect {
            x: 0.0,
            y: 0.0,
            w: 1080.0,
            h: 1920.0,
        };
        assert!(place_label(canvas, Some(panel), &[all], (400.0, 90.0)).is_none());
        // the arrow starts outside the captions and the face
        let (from, to) = arrow_into(
            canvas,
            Rect {
                x: 300.0,
                y: 500.0,
                w: 400.0,
                h: 300.0,
            },
            &[captions, face],
        )
        .unwrap();
        assert_eq!(to, (500.0, 650.0));
        assert!(from.1 < 1000.0 && from.1 > 220.0, "{from:?}");
    }

    fn frame(fill: impl Fn(u32, u32) -> u8) -> LumaFrame {
        let (w, h) = (64, 48);
        LumaFrame {
            width: w,
            height: h,
            data: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| fill(x, y))
                .collect(),
        }
    }

    #[test]
    fn a_switch_to_another_app_is_a_jump_and_typing_is_not() {
        let full = NRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        let editor = frame(|x, y| if (y / 4) % 2 == 0 && x < 50 { 200 } else { 30 });
        let typed = frame(|x, y| {
            if ((y / 4) % 2 == 0 && x < 50) || (y < 8 && x < 16) {
                200
            } else {
                30
            }
        });
        let other = frame(|x, _| if x % 16 < 8 { 240 } else { 90 });
        let small = change_fraction(&editor, &typed, &full, &[]);
        let big = change_fraction(&typed, &other, &full, &[]);
        assert!(small < 0.1, "{small}");
        assert!(big > 0.5, "{big}");
        // excluding the area that changed makes it no change
        let ex = NRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 0.2,
        };
        assert_eq!(change_fraction(&editor, &typed, &full, &[ex]), 0.0);
        // among small steps, the big one is the jump (and only it)
        let mut fr = vec![0.01, 0.02, 0.0, 0.03, 0.01, 0.02, 0.01];
        fr.push(big);
        fr.extend([0.02, 0.01, 0.0]);
        let j = screen_jumps(&fr);
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].step, 7);
        // a busy screen (scrolling throughout): its own steps are big, so nothing stands out
        let busy = vec![0.5, 0.6, 0.55, 0.62, 0.58, 0.7, 0.52];
        assert!(screen_jumps(&busy).is_empty());
    }

    #[test]
    fn only_a_jump_mid_sentence_wants_a_hold() {
        let ws = vec![
            w("I", 0, 100),
            w("opened", 120, 400),
            w("the", 420, 500),
            w("settings", 520, 900),
            w("and", 920, 1000),
            w("changed", 1020, 1300),
            w("the", 1320, 1400),
            w("model.", 1420, 1900),
            w("Then", 2600, 2800),
            w("it", 2820, 2900),
            w("ran.", 2920, 3300),
        ];
        assert_eq!(mid_sentence(&ws, 950), Some(7));
        assert_eq!(mid_sentence(&ws, 1800), None, "the sentence ends");
        assert_eq!(mid_sentence(&ws, 2200), None, "between sentences");
        assert_eq!(mid_sentence(&ws, 2650), None, "a sentence starts");
    }
}
