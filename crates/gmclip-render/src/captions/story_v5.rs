//! `story_v5`: the exact port of the reference story captions — cues and ASS events
//! (SPEC §5.10, B-08).
//!
//! Source: the reference reel builder's `story_reel.py` — `layout()` (spans only),
//! `native_of`, `span_at`, `caption_cues`, `write_ass` and `fit_size` — at the revision
//! pinned in `crates/gmclip-render/captions/story_v5.md`, which also lists every constant.
//! The port keeps Python's arithmetic: IEEE doubles in the same order, `round()` as
//! round-half-even, `int()` as truncation, `//` as floor division, stable sorts.
//!
//! Inputs are the reference builder's own: the cut's plan (`plan.json`) and the words
//! heard in the rendered cut (`qa/heard.json`, output seconds). Output is the cue list and
//! the ASS script the reference wrote,
//! byte for byte; [`StoryCaptions::to_ass`] is what R-CAP-1 compares against `captions-v5.ass`.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

use super::pillow::PillowMeasure;
use gmclip_plan::story_cues::py_round;
pub use gmclip_plan::story_cues::{
    Cue, CueRules, FILLER, FPS, KEEP_BASE, Span, WEAK, Word, native_of, norm, plain, span_at,
};
#[cfg(test)]
use gmclip_plan::story_cues::{capitalise, ends_with_any, prev_ends_sentence, py_split};

/// `caption_cues(plan, words, spans, total)`: the shared grouping (gmclip-plan
/// `story_cues`), fed this plan's rules.
pub fn caption_cues(plan: &StoryPlan, words: &[Word], spans: &[Span], total: i64) -> Vec<Cue> {
    gmclip_plan::story_cues::caption_cues(&plan.rules(), words, spans, total)
}

/// Markup → ASS colour overrides (`COL`), applied in this order.
pub const COL: [(&str, &str); 3] = [
    ("{Y}", "{\\c&H00D4FF&}"),
    ("{G}", "{\\c&H84DC3D&}"),
    ("{W}", "{\\c&HFFFFFF&}"),
];
/// Caption size and fit width (`write_ass`: `fit_size(txt, 86, 1000)`).
pub const CAPTION_SIZE: u32 = 86;
/// Hook default size (`plan.get('hook_size', 88)`), fit to 1000 px.
pub const HOOK_SIZE: u32 = 88;
/// Pill size, fit to 950 px.
pub const PILL_SIZE: u32 = 50;
/// Fit width for hooks and captions.
pub const FIT_PX: u32 = 1000;
/// Fit width for pills.
pub const PILL_FIT_PX: u32 = 950;
/// Default caption y and hook y.
pub const CAPTION_Y: i64 = 1065;
/// Default hook y.
pub const HOOK_Y: i64 = 305;

/// The reference's `markup()`.
pub fn markup(s: &str) -> String {
    let mut s = s.to_owned();
    for (k, v) in COL {
        s = s.replace(k, v);
    }
    s
}

/// A plan could not be built into captions (R-CAP-2: callers export without captions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueError(pub String);

impl fmt::Display for CueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "story_v5: {}", self.0)
    }
}

impl std::error::Error for CueError {}

/// A JSON number kept as written, so it formats the way Python's f-strings do.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct PyNum(pub serde_json::Number);

impl PyNum {
    fn f64(&self) -> f64 {
        self.0.as_f64().unwrap_or(f64::NAN)
    }
}

impl fmt::Display for PyNum {
    /// `str(x)`: integers as written; floats as Python `repr` (shortest round trip, `.0`
    /// when integral, exponent outside `1e-4 ≤ |x| < 1e16`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_i64() || self.0.is_u64() {
            return write!(f, "{}", self.0);
        }
        let x = self.f64();
        let a = x.abs();
        if x == 0.0 || (1e-4..1e16).contains(&a) {
            let s = format!("{x}");
            if s.contains('.') || s.contains("inf") || s.contains("NaN") {
                f.write_str(&s)
            } else {
                write!(f, "{s}.0")
            }
        } else {
            let s = format!("{x:e}");
            let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
            let e: i32 = e.parse().unwrap_or(0);
            write!(f, "{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
        }
    }
}

/// `plan.hook`.
#[derive(Clone, Debug, Deserialize)]
pub struct StoryHook {
    /// Markup text; `\N` (literal backslash-N) breaks lines.
    pub text: String,
    /// Cut id the hook stays up through.
    #[serde(default)]
    pub until: Option<String>,
    /// Or an output time in seconds.
    #[serde(default)]
    pub until_s: Option<f64>,
    /// Hook y (default 305).
    #[serde(default)]
    pub y: Option<PyNum>,
}

/// `plan.cuts[]`.
#[derive(Clone, Debug, Deserialize)]
pub struct StoryCut {
    /// Part id.
    pub id: String,
    /// Native start, seconds.
    pub a: f64,
    /// Native end, seconds.
    pub z: f64,
    /// Layout name.
    pub layout: String,
    /// Pill name.
    #[serde(default)]
    pub pill: Option<String>,
}

/// `plan.pills.<name>`: fixed text, or a run clock.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum StoryPill {
    /// `{clock_from, label?}`: `label  {Y}m:ss` of native seconds since `clock_from`.
    Clock {
        /// Native second the clock counts from.
        clock_from: f64,
        /// Label (default `GENERATING`).
        #[serde(default)]
        label: Option<String>,
    },
    /// `{text}`.
    Text {
        /// Markup text.
        text: String,
    },
}

fn ordered_pairs<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, String)>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<(String, String)>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object of strings")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
            let mut v = Vec::new();
            while let Some(kv) = m.next_entry::<String, String>()? {
                v.push(kv);
            }
            Ok(v)
        }
    }
    d.deserialize_map(V)
}

/// The keys of an the reference `plan.json` the captions read (all others are ignored here).
#[derive(Clone, Debug, Deserialize)]
pub struct StoryPlan {
    /// `version` (default `v5`).
    #[serde(default)]
    pub version: Option<String>,
    /// `hook`.
    #[serde(default)]
    pub hook: Option<StoryHook>,
    /// `hook_size` (default 88). [`build`] requires an integer 1..=1000; an integral float such
    /// as `88.0` is read as 88 (Python would also print `88.0` in the Hook style line; no the reference
    /// plan sets it).
    #[serde(default)]
    pub hook_size: Option<f64>,
    /// `caption_y` by layout name.
    #[serde(default)]
    pub caption_y: BTreeMap<String, PyNum>,
    /// `cuts`.
    pub cuts: Vec<StoryCut>,
    /// `pills`.
    #[serde(default)]
    pub pills: BTreeMap<String, StoryPill>,
    /// `keywords`.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `keep` pairs.
    #[serde(default)]
    pub keep: Vec<(String, String)>,
    /// `fix` (in file order: a later key with the same `norm` wins, as in a Python dict).
    #[serde(default, deserialize_with = "ordered_pairs")]
    pub fix: Vec<(String, String)>,
    /// `cap_sub` `[heard words, shown]`.
    #[serde(default)]
    pub cap_sub: Vec<(String, String)>,
    /// `no_caption` native `[a, z)` ranges.
    #[serde(default)]
    pub no_caption: Vec<(f64, f64)>,
}

impl StoryPlan {
    /// Parses `plan.json`.
    pub fn from_json(s: &str) -> Result<Self, CueError> {
        serde_json::from_str(s).map_err(|e| CueError(format!("plan.json: {e}")))
    }

    /// The keys `caption_cues` reads.
    pub fn rules(&self) -> CueRules {
        CueRules {
            keywords: self.keywords.clone(),
            keep: self.keep.clone(),
            fix: self.fix.clone(),
            cap_sub: self.cap_sub.clone(),
            no_caption: self.no_caption.clone(),
        }
    }

    fn hook_size(&self) -> u32 {
        self.hook_size.map_or(HOOK_SIZE, |s| s as u32)
    }
}

#[derive(Deserialize)]
struct Heard {
    words: Vec<Word>,
}

/// Parses `qa/heard.json` (`{key, words, sentences}`) to its words.
pub fn heard_words(s: &str) -> Result<Vec<Word>, CueError> {
    serde_json::from_str::<Heard>(s)
        .map(|h| h.words)
        .map_err(|e| CueError(format!("heard.json: {e}")))
}

/// Largest |time| accepted anywhere (seconds): a day. Keeps every frame and centisecond in
/// range so nothing overflows (R-CAP-2: a caption build never panics).
pub const MAX_SECONDS: f64 = 86_400.0;
/// Longest cut accepted (frames): 4 h, the story-render cap with room to spare. The pill pass
/// walks every output frame, so this bounds its work.
pub const MAX_FRAMES: i64 = 4 * 3600 * FPS;

fn sane(t: f64) -> bool {
    t.is_finite() && t.abs() <= MAX_SECONDS
}

/// `layout()`'s spans and total frame count.
pub fn spans(plan: &StoryPlan) -> Result<(Vec<Span>, i64), CueError> {
    if plan.cuts.is_empty() {
        return Err(CueError("plan has no cuts".into()));
    }
    let mut out = Vec::with_capacity(plan.cuts.len());
    let mut cursor = 0;
    for c in &plan.cuts {
        if !(sane(c.a) && sane(c.z)) {
            return Err(CueError(format!(
                "cut {}: a/z must be finite and within a day",
                c.id
            )));
        }
        // Python int() truncates toward zero, as `as i64` does.
        let (q0, q1) = (
            (c.a * FPS as f64 + 0.5) as i64,
            (c.z * FPS as f64 + 0.5) as i64,
        );
        out.push(Span {
            id: c.id.clone(),
            q0,
            q1,
            c0: cursor,
            layout: c.layout.clone(),
            pill: c.pill.clone(),
        });
        cursor += q1 - q0;
    }
    if !(1..=MAX_FRAMES).contains(&cursor) {
        return Err(CueError(format!(
            "cuts total {cursor} frames (1..={MAX_FRAMES})"
        )));
    }
    Ok((out, cursor))
}

/// One Dialogue line as `write_ass` formats it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssLine {
    /// `Layer`.
    pub layer: i32,
    /// Start, centiseconds.
    pub start_cs: i64,
    /// End, centiseconds.
    pub end_cs: i64,
    /// `Hook`, `Speech` or `Pill`.
    pub style: &'static str,
    /// The leading override block, e.g. `{\pos(540,1065)\fs84}`.
    pub tags: String,
    /// The text, markup already turned into ASS colour tags.
    pub text: String,
}

/// `st(v)`: centiseconds → `H:MM:SS.CC`.
pub fn ass_time(v: i64) -> String {
    // Python's // and % floor (so −1 cs is `-1:59:59.99`).
    format!(
        "{}:{:02}:{:02}.{:02}",
        v.div_euclid(360_000),
        v.div_euclid(6000).rem_euclid(60),
        v.div_euclid(100).rem_euclid(60),
        v.rem_euclid(100)
    )
}

impl fmt::Display for AssLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Dialogue: {},{},{},{},,0,0,0,,{}{}",
            self.layer,
            ass_time(self.start_cs),
            ass_time(self.end_cs),
            self.style,
            self.tags,
            self.text
        )
    }
}

/// What `story_v5` builds for one plan.
#[derive(Clone, Debug)]
pub struct StoryCaptions {
    /// Output frames.
    pub total: i64,
    /// The cuts on the output clock.
    pub spans: Vec<Span>,
    /// Caption cues.
    pub cues: Vec<Cue>,
    /// Hook end frame (0 without a hook); pills wait for it.
    pub hook_end: i64,
    /// Every Dialogue, in `write_ass` order: hook, cues, pills.
    pub lines: Vec<AssLine>,
    hook_size: u32,
}

/// Builds captions, hook and pills for a plan from heard words.
pub fn build(
    plan: &StoryPlan,
    words: &[Word],
    measure: &PillowMeasure,
) -> Result<StoryCaptions, CueError> {
    let (spans, total) = spans(plan)?;
    if let Some(w) = words.iter().find(|w| !(sane(w.a) && sane(w.z))) {
        return Err(CueError(format!(
            "word {:?}: times must be finite and within a day",
            w.w
        )));
    }
    if let Some(s) = plan.hook_size
        && !(s.fract() == 0.0 && (1.0..=1000.0).contains(&s))
    {
        return Err(CueError(format!("hook_size {s} (an integer 1..=1000)")));
    }
    if let Some(hk) = &plan.hook
        && hk.until_s.is_some_and(|t| !sane(t))
    {
        return Err(CueError(
            "hook.until_s must be finite and within a day".into(),
        ));
    }
    if let Some((name, _)) = plan
        .pills
        .iter()
        .find(|(_, p)| matches!(p, StoryPill::Clock { clock_from, .. } if !sane(*clock_from)))
    {
        return Err(CueError(format!(
            "pill {name}: clock_from must be finite and within a day"
        )));
    }
    let cues = caption_cues(plan, words, &spans, total);
    let (lines, hook_end) = write_ass(plan, &spans, total, &cues, measure)?;
    Ok(StoryCaptions {
        total,
        spans,
        cues,
        hook_end,
        lines,
        hook_size: plan.hook_size(),
    })
}

/// `write_ass`'s events.
fn write_ass(
    plan: &StoryPlan,
    spans: &[Span],
    total: i64,
    cues: &[Cue],
    measure: &PillowMeasure,
) -> Result<(Vec<AssLine>, i64), CueError> {
    let cs = |f: i64| -> i64 {
        // min(round(f * 100 / FPS), (total * 100) // FPS); f·100/30 is never a tie.
        py_round((f * 100) as f64 / FPS as f64).min((total * 100).div_euclid(FPS))
    };
    let mut ev = Vec::new();
    let mut end = 0;
    if let Some(hk) = &plan.hook {
        end = match (hk.until_s, &hk.until) {
            (Some(s), _) => py_round(s * FPS as f64),
            (None, Some(id)) => spans
                .iter()
                .find(|s| &s.id == id)
                .map(Span::end)
                .ok_or_else(|| CueError(format!("hook.until {id:?} is not a cut")))?,
            (None, None) => return Err(CueError("hook has neither until nor until_s".into())),
        };
        let size = plan.hook_size();
        let hs = measure.fit_size(&hk.text, size, FIT_PX);
        let fs = if hs != size {
            format!("\\fs{hs}")
        } else {
            String::new()
        };
        let y =
            hk.y.as_ref()
                .map_or_else(|| HOOK_Y.to_string(), ToString::to_string);
        ev.push(AssLine {
            layer: 2,
            start_cs: 0,
            end_cs: cs(end),
            style: "Hook",
            tags: format!("{{\\pos(540,{y}){fs}}}"),
            text: markup(&hk.text),
        });
    }
    for c in cues {
        let cz = measure.fit_size(&c.text, CAPTION_SIZE, FIT_PX);
        let cfs = if cz != CAPTION_SIZE {
            format!("\\fs{cz}")
        } else {
            String::new()
        };
        let y = plan
            .caption_y
            .get(&c.layout)
            .map_or_else(|| CAPTION_Y.to_string(), ToString::to_string);
        ev.push(AssLine {
            layer: 1,
            start_cs: cs(c.t0),
            end_cs: cs(c.t1),
            style: "Speech",
            tags: format!("{{\\pos(540,{y}){cfs}}}"),
            text: markup(&c.text),
        });
    }
    let pill_text = |name: &str, t: f64| -> Result<String, CueError> {
        match plan
            .pills
            .get(name)
            .ok_or_else(|| CueError(format!("pill {name:?} is not in pills")))?
        {
            StoryPill::Clock { clock_from, label } => {
                let e = (t - clock_from).max(0.0);
                let s = e.trunc() as i64;
                Ok(markup(&format!(
                    "{}  {{Y}}{}:{:02}",
                    label.as_deref().unwrap_or("GENERATING"),
                    s / 60,
                    s % 60
                )))
            }
            StoryPill::Text { text } => Ok(markup(text)),
        }
    };
    let mut pill_ev = |a: i64, b: i64, t: &str| {
        let a = a.max(end);
        if b > a {
            let fs = measure.fit_size(t, PILL_SIZE, PILL_FIT_PX);
            ev.push(AssLine {
                layer: 3,
                start_cs: cs(a),
                end_cs: cs(b),
                style: "Pill",
                tags: format!("{{\\an7\\pos(48,262)\\fs{fs}}}"),
                text: t.to_owned(),
            });
        }
    };
    let mut prev: Option<String> = None;
    let mut start = 0;
    for s in spans {
        for k in 0..(s.q1 - s.q0) {
            let txt = match s.pill.as_deref() {
                Some(p) if !p.is_empty() => Some(pill_text(p, (s.q0 + k) as f64 / FPS as f64)?),
                _ => None,
            };
            if txt != prev {
                if let Some(p) = prev.as_deref().filter(|p| !p.is_empty()) {
                    pill_ev(start, s.c0 + k, p);
                }
                prev = txt;
                start = s.c0 + k;
            }
        }
    }
    if let Some(p) = prev.as_deref().filter(|p| !p.is_empty()) {
        pill_ev(start, total, p);
    }
    Ok((ev, end))
}

impl StoryCaptions {
    /// The whole ASS file `write_ass` writes, byte for byte.
    pub fn to_ass(&self) -> String {
        let mut head = vec![
            "[Script Info]".to_owned(),
            "ScriptType: v4.00+".into(),
            "PlayResX: 1080".into(),
            "PlayResY: 1920".into(),
            "WrapStyle: 2".into(),
            "ScaledBorderAndShadow: yes".into(),
            String::new(),
            "[V4+ Styles]".into(),
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding".into(),
            "Style: Speech,Neometric Heavy,86,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,12,7,5,40,40,0,1".into(),
            format!("Style: Hook,Neometric Heavy,{},&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,12,7,5,40,40,0,1", self.hook_size),
            "Style: Pill,Neometric Heavy,50,&H00FFFFFF,&H00FFFFFF,&H30101010,&H00000000,-1,0,0,0,100,100,1,0,3,16,0,7,40,40,0,1".into(),
            String::new(),
            "[Events]".into(),
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text".into(),
        ];
        head.extend(self.lines.iter().map(ToString::to_string));
        head.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_semantics() {
        assert_eq!(norm("Let's"), "lets");
        assert_eq!(norm("50%"), "50");
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
        assert_eq!(py_round(-0.5), 0);
        assert_eq!(ass_time(1733), "0:00:17.33");
        assert_eq!(ass_time(360_000 + 6000 + 5), "1:01:00.05");
        assert_eq!(
            PyNum(serde_json::from_str("305").unwrap()).to_string(),
            "305"
        );
        assert_eq!(
            PyNum(serde_json::from_str("305.0").unwrap()).to_string(),
            "305.0"
        );
        assert_eq!(
            PyNum(serde_json::from_str("1110.5").unwrap()).to_string(),
            "1110.5"
        );
        assert_eq!(
            markup("a {Y}b{W} {G}c"),
            "a {\\c&H00D4FF&}b{\\c&HFFFFFF&} {\\c&H84DC3D&}c"
        );
    }

    #[test]
    fn sentence_end_and_capitalise() {
        assert!(prev_ends_sentence("we {Y}saved{W} him."));
        assert!(prev_ends_sentence("Dude, eat {Y}berries.{W}"));
        assert!(!prev_ends_sentence("before he {Y}kills{W}"));
        assert!(!prev_ends_sentence("a{b.}"));
        assert!(prev_ends_sentence("a.{x{y}"));
        assert_eq!(capitalise("{Y}defend{W} our man."), "{Y}Defend{W} our man.");
        assert_eq!(capitalise("okay."), "Okay.");
        assert_eq!(capitalise("'cause"), "'cause");
    }

    #[test]
    fn ordered_fix_last_norm_wins() {
        let p = StoryPlan::from_json(
            r#"{"cuts":[{"id":"a","a":0,"z":1,"layout":"x"}],"fix":{"yeah":"Yeah,","Yeah":"Yes"}}"#,
        )
        .unwrap();
        assert_eq!(
            p.fix,
            vec![
                ("yeah".into(), "Yeah,".into()),
                ("Yeah".into(), "Yes".into())
            ]
        );
        let words = [Word {
            w: "yeah".into(),
            a: 0.1,
            z: 0.4,
        }];
        let (sp, total) = spans(&p).unwrap();
        let cues = caption_cues(&p, &words, &sp, total);
        assert_eq!(cues[0].text, "Yes");
    }

    #[test]
    fn hostile_inputs_error_instead_of_panicking() {
        let base = r#"{"cuts":[{"id":"a","a":0,"z":1,"layout":"x","pill":"c"}],"pills":{"c":{"clock_from":0}}}"#;
        let plan = StoryPlan::from_json(base).unwrap();
        assert!(spans(&plan).is_ok());
        for json in [
            r#"{"cuts":[{"id":"a","a":0,"z":1e17,"layout":"x"}]}"#,
            r#"{"cuts":[{"id":"a","a":0,"z":4e5,"layout":"x"},{"id":"b","a":0,"z":4e5,"layout":"x"}]}"#,
            r#"{"cuts":[{"id":"a","a":0,"z":1e6,"layout":"x"}]}"#,
            r#"{"cuts":[{"id":"a","a":2,"z":1,"layout":"x"}]}"#,
        ] {
            assert!(
                spans(&StoryPlan::from_json(json).unwrap()).is_err(),
                "{json}"
            );
        }
        for w in [
            Word {
                w: "x".into(),
                a: 1e16,
                z: 1e16,
            },
            Word {
                w: "x".into(),
                a: f64::NAN,
                z: 0.0,
            },
        ] {
            let ok = spans(&plan).is_ok() && [w.a, w.z].iter().all(|t| sane(*t));
            assert!(!ok);
        }
        assert_eq!(ass_time(-1), "-1:59:59.99");
        assert!(ends_with_any("end.\n", ".?!"));
        assert_eq!(
            py_split("a\x1cb  c").collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(span_at(&[], 3).is_none());
    }
}
