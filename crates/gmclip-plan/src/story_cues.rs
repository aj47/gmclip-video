//! The reference's caption cue grouping (`story_reel.caption_cues`), the exact port B-08 made for
//! `gmclip-render::captions::story_v5` (R-CAP-1: byte-identical on the golden cuts),
//! moved here, I/O-free, so the model planner groups captions the same way the reference did
//! (backlog U.09 / the M2 walkthrough). `story_v5` calls it unchanged.
//!
//! The port keeps Python's arithmetic: IEEE doubles in the same order, `round()` as
//! round-half-even, `int()` as truncation, stable sorts.

use std::collections::{BTreeMap, BTreeSet};

/// What `caption_cues` reads from an reel plan: keywords, keep pairs, spelling fixes,
/// multi-word substitutions and no-caption ranges (native seconds).
#[derive(Clone, Debug, Default)]
pub struct CueRules {
    pub keywords: Vec<String>,
    pub keep: Vec<(String, String)>,
    /// In file order: a later key with the same `norm` wins.
    pub fix: Vec<(String, String)>,
    pub cap_sub: Vec<(String, String)>,
    pub no_caption: Vec<(f64, f64)>,
}

/// The reference's `plain()`: `re.sub(r'{[^}]*}', '', s)`.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                // No closing brace: the regex does not match; keep the text as is.
                out.push_str(&rest[i..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `FPS` in `story_reel.py`.
pub const FPS: i64 = 30;
/// Words dropped from captions (`FILLER`).
pub const FILLER: [&str; 6] = ["uh", "um", "uhh", "umm", "er", "hmm"];
/// A cue should not end on these (`WEAK`).
pub const WEAK: [&str; 20] = [
    "by", "in", "to", "that", "the", "a", "on", "my", "and", "of", "an", "if", "it", "is", "with",
    "for", "at", "or", "so", "but",
];
/// Pairs never split across cues (`KEEP_BASE`, 25 pairs); plans add their own `keep`.
pub const KEEP_BASE: [(&str, &str); 25] = [
    ("two", "minutes"),
    ("less", "than"),
    ("more", "than"),
    ("under", "ten"),
    ("ten", "minutes"),
    ("five-second", "video"),
    ("Windows", "PC"),
    ("blown", "away"),
    ("much", "better"),
    ("a", "banana"),
    ("my", "local"),
    ("local", "Windows"),
    ("let's", "see"),
    ("I'm", "a"),
    ("what's", "the"),
    ("the", "clock"),
    ("start", "the"),
    ("per", "cent"),
    ("Comfy", "UI"),
    ("MiniMax", "H3"),
    ("open", "weights"),
    ("text", "to"),
    ("to", "video"),
    ("on", "my"),
    ("in", "under"),
];
/// The reference's `norm`: lower-cased, then everything but `[a-z0-9]` removed.
pub fn norm(w: &str) -> String {
    w.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// Python `round()` on a double: nearest, ties to even.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// One heard word, output seconds (`qa/heard.json` `words[]`).
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Word {
    /// The word as transcribed (punctuation attached).
    pub w: String,
    /// Start, output seconds.
    pub a: f64,
    /// End, output seconds.
    pub z: f64,
}

/// One cut on the output clock (`layout()`'s `spans`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Cut id.
    pub id: String,
    /// Native first frame, `int(a·30 + .5)`.
    pub q0: i64,
    /// Native end frame, `int(z·30 + .5)`.
    pub q1: i64,
    /// Output frame the cut starts at.
    pub c0: i64,
    /// Layout name.
    pub layout: String,
    /// Pill name.
    pub pill: Option<String>,
}

impl Span {
    pub fn end(&self) -> i64 {
        self.c0 + self.q1 - self.q0
    }
}

/// `native_of`: native seconds of an output frame (past the end → the last cut's end).
pub fn native_of(spans: &[Span], frame: i64) -> f64 {
    for s in spans {
        if s.c0 <= frame && frame < s.end() {
            return (s.q0 + frame - s.c0) as f64 / FPS as f64;
        }
    }
    spans.last().map_or(0.0, |s| s.q1 as f64 / FPS as f64)
}

/// `span_at`: the cut an output frame is in (past the end → the last; `None` without cuts).
pub fn span_at(spans: &[Span], frame: i64) -> Option<&Span> {
    spans
        .iter()
        .find(|s| s.c0 <= frame && frame < s.end())
        .or_else(|| spans.last())
}

/// One caption cue: output frames `[t0, t1)`, markup text, the layout it starts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    /// First output frame.
    pub t0: i64,
    /// End output frame (exclusive).
    pub t1: i64,
    /// Text with `{Y}`/`{W}` keyword markup.
    pub text: String,
    /// Layout of the cut the cue starts in (picks `caption_y`).
    pub layout: String,
}

/// `re.search(r'[<set>]$', s)`: Python's `$` also matches before one trailing newline.
pub fn ends_with_any(s: &str, set: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.chars().last().is_some_and(|c| set.contains(c))
}

/// Python `str.split()`: Unicode whitespace plus the separators `\x1c`–`\x1f`.
pub fn py_split(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
        .filter(|t| !t.is_empty())
}

/// `caption_cues(plan, words, spans, total)`.
pub fn caption_cues(plan: &CueRules, words: &[Word], spans: &[Span], total: i64) -> Vec<Cue> {
    let fps = FPS as f64;
    let keep = keep_set(&plan.keep);
    let keys: BTreeSet<String> = plan.keywords.iter().map(|k| norm(k)).collect();
    let mut fix: BTreeMap<String, &str> = BTreeMap::new();
    for (k, v) in &plan.fix {
        fix.insert(norm(k), v);
    }
    let subs: Vec<(Vec<String>, &str)> = plan
        .cap_sub
        .iter()
        .map(|(old, new)| (py_split(old).map(norm).collect::<Vec<_>>(), new.as_str()))
        .filter(|(old, _)| !old.is_empty()) // Python would loop forever on an empty `old`
        .collect();

    let mut ws: Vec<Word> = Vec::new();
    for w in words {
        let k = norm(&w.w);
        if k.is_empty() || FILLER.contains(&k.as_str()) {
            continue;
        }
        let mid = native_of(spans, py_round((w.a + w.z) / 2.0 * fps));
        if plan
            .no_caption
            .iter()
            .any(|&(lo, hi)| lo <= mid && mid < hi)
        {
            continue;
        }
        let mut w = w.clone();
        if let Some(f) = fix.get(&k) {
            let last = w.w.chars().last();
            let tail = match last {
                Some(c) if ".,?!".contains(c) && !f.ends_with(c) => c.to_string(),
                _ => String::new(),
            };
            w.w = format!("{f}{tail}");
        }
        if !w.w.is_empty() {
            ws.push(w);
        }
    }

    // Multi-word substitutions (spelling, names, numbers).
    let mut out: Vec<Word> = Vec::new();
    let mut i = 0;
    while i < ws.len() {
        let mut hit = false;
        for (old, new) in &subs {
            let n = old.len();
            let window: Vec<String> = ws[i..(i + n).min(ws.len())]
                .iter()
                .map(|x| norm(&x.w))
                .collect();
            if window == *old {
                let last = &ws[i + n - 1].w;
                let tail = match last.chars().last() {
                    Some(c) if ".,?!".contains(c) && !new.ends_with(c) => c.to_string(),
                    _ => String::new(),
                };
                out.push(Word {
                    w: format!("{new}{tail}"),
                    a: ws[i].a,
                    z: ws[i + n - 1].z,
                });
                i += n;
                hit = true;
                break;
            }
        }
        if !hit {
            out.push(ws[i].clone());
            i += 1;
        }
    }
    let ws = out;

    let groups = group_words(&ws, &keep, &|_| false, false, total);
    let mut cues: Vec<Cue> = groups
        .into_iter()
        .map(|g| {
            let txt = ws[g.words.clone()]
                .iter()
                .map(|x| x.w.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let txt = py_split(&txt)
                .map(|x| {
                    if keys.contains(&norm(x)) {
                        format!("{{Y}}{x}{{W}}")
                    } else {
                        x.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let layout = span_at(spans, g.t0)
                .map(|s| s.layout.clone())
                .unwrap_or_default();
            Cue {
                t0: g.t0,
                t1: g.t1,
                text: txt,
                layout,
            }
        })
        .collect();
    for i in 0..cues.len() {
        if i == 0 || prev_ends_sentence(&cues[i - 1].text) {
            cues[i].text = capitalise(&cues[i].text);
        }
    }
    cues
}

/// One cue of [`group_words`]: output frames `[t0, t1)` and the words it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub t0: i64,
    pub t1: i64,
    /// Indices into the words.
    pub words: std::ops::Range<usize>,
}

/// The reference's grouping and timing (`caption_cues` after its word clean-up), over words already
/// cleaned: sentences (ending at `.?!` or a pause over 0.9 s, or where `split_before(i)`
/// says a cue must start at word `i`), each cut into 1–4 word cues by the reference's cost search
/// (no weak-word endings, `keep` pairs and a number with its unit stay together, 4 words
/// only when it saves a cue under 0.3 s), then held up to 0.5 s past the last word, never
/// into the next cue; a cue squeezed under 4 frames is dropped. `single` makes every word
/// its own cue. Frames are at [`FPS`]; `total` is the cut's length in them.
pub fn group_words(
    ws: &[Word],
    keep: &BTreeSet<(String, String)>,
    split_before: &dyn Fn(usize) -> bool,
    single: bool,
    total: i64,
) -> Vec<Group> {
    let fps = FPS as f64;
    // Sentences; a sentence also ends at a pause over 0.9 s.
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    let mut from = 0;
    for (i, w) in ws.iter().enumerate() {
        if i > from && (w.a - ws[i - 1].z > 0.9 || split_before(i)) {
            runs.push(from..i);
            from = i;
        }
        if ends_with_any(&w.w, ".?!") {
            runs.push(from..i + 1);
            from = i + 1;
        }
    }
    if from < ws.len() {
        runs.push(from..ws.len());
    }

    let mut cues: Vec<Group> = Vec::new();
    for r in &runs {
        let run = &ws[r.clone()];
        let m = run.len();
        let kmax = if single { 1 } else { 4 };
        let mut best = vec![1e9_f64; m + 1];
        best[0] = 0.0;
        let mut back = vec![0usize; m + 1];
        for j in 1..=m {
            for k in 1..=kmax {
                let Some(i) = j.checked_sub(k) else { continue };
                let gg = &run[i..j];
                let mut cost = 1.0_f64;
                let nw: usize = gg.iter().map(|x| py_split(&plain(&x.w)).count()).sum();
                if k > 1 && nw > 4 {
                    continue;
                }
                if k > 1 && nw == 4 {
                    let joined = gg
                        .iter()
                        .map(|x| x.w.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if plain(&joined).chars().count() > 18 || gg[gg.len() - 1].z - gg[0].a > 0.9 {
                        continue;
                    }
                    cost += 2.0;
                }
                let dur = (if j < m {
                    run[j].a
                } else {
                    gg[gg.len() - 1].z + 0.4
                }) - gg[0].a;
                if dur < 0.3 {
                    cost += 12.0 * (0.3 - dur);
                }
                if j < m && WEAK.contains(&norm(&gg[gg.len() - 1].w).as_str()) {
                    cost += 3.0;
                }
                if k == 1 && j < m {
                    cost += 1.5;
                }
                if j < m && keep.contains(&(norm(&run[j - 1].w), norm(&run[j].w))) {
                    cost += 5.0;
                }
                if j < m {
                    let n = norm(&run[j - 1].w);
                    if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
                        cost += 5.0; // keep a number with its unit (`\d+%?` on the norm)
                    }
                }
                if gg[..gg.len() - 1].iter().any(|x| x.w.ends_with(',')) {
                    cost += 2.0;
                }
                if best[i] + cost < best[j] {
                    best[j] = best[i] + cost;
                    back[j] = i;
                }
            }
        }
        let mut parts = Vec::new();
        let mut j = m;
        while j > 0 {
            parts.push((back[j], j));
            j = back[j];
        }
        for &(i, j) in parts.iter().rev() {
            let gg = &run[i..j];
            let t0 = py_round(gg[0].a * fps).max(0);
            let t1 = py_round(gg[gg.len() - 1].z * fps).min(total);
            cues.push(Group {
                t0,
                t1: t1.max(t0 + 6),
                words: r.start + i..r.start + j,
            });
        }
    }
    cues.sort_by_key(|c| c.t0); // stable, like list.sort
    for i in 0..cues.len() {
        // Hold each cue up to 0.5 s past its last word, never into the next cue.
        let nxt = cues.get(i + 1).map_or(total, |c| c.t0);
        let cu = &mut cues[i];
        cu.t1 = nxt.min(cu.t1.max(cu.t0 + 8) + 15);
    }
    cues.retain(|c| c.t1 - c.t0 >= 4); // a cue squeezed under 4 frames would only flash
    cues
}

/// The keep pairs: [`KEEP_BASE`] plus a plan's own, normalised.
pub fn keep_set(extra: &[(String, String)]) -> BTreeSet<(String, String)> {
    KEEP_BASE
        .iter()
        .map(|(a, b)| (norm(a), norm(b)))
        .chain(extra.iter().map(|(a, b)| (norm(a), norm(b))))
        .collect()
}

/// `re.search(r'[.?!](\{[^}]*\})?$', s)`.
pub fn prev_ends_sentence(s: &str) -> bool {
    if ends_with_any(s, ".?!") {
        return true;
    }
    let Some(head) = s.strip_suffix('}') else {
        return false;
    };
    // `\{[^}]*\}$`: any `{` after which `head` has no `}`, preceded by `.?!`.
    head.char_indices()
        .filter(|&(i, c)| c == '{' && !head[i + 1..].contains('}'))
        .any(|(i, _)| ends_with_any(&head[..i], ".?!"))
}

/// `re.match(r'((?:\{[^}]*\})*)(\w)', s)` → upper-case that first word character.
pub fn capitalise(s: &str) -> String {
    let mut i = 0;
    while s[i..].starts_with('{') {
        match s[i..].find('}') {
            Some(j) => i += j + 1,
            None => break,
        }
    }
    match s[i..].chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => {
            format!("{}{}{}", &s[..i], c.to_uppercase(), &s[i + c.len_utf8()..])
        }
        _ => s.to_owned(),
    }
}
