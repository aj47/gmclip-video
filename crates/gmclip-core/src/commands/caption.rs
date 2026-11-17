//! Caption edits on a cut's caption document: per-word text, keywords, word edges.

use gmclip_macros::command;
use gmclip_model::anchor::{WordId, WordRef};
use gmclip_model::clock::ms_to_flicks;
use gmclip_model::entities::{
    CaptionCorrections, CaptionDoc, CaptionEdit as Edit, CueBreak, Cut, KeepPairs, Term, Terms,
    WordTimeOverride,
};
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Done;
use crate::command::{Command, Cx};

fn word_ok(w: &WordId) -> Result<(), CmdError> {
    super::bounded(&w.asr_revision, "/word/asr_revision", 128)
}

fn caption_doc(cx: &mut Cx<'_>, cut: Id<Cut>) -> Result<Id<CaptionDoc>, CmdError> {
    cx.tx.get(cut)?;
    cx.tx
        .children::<CaptionDoc>(cut.entity())
        .first()
        .copied()
        .ok_or(CmdError::NotFound)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionEditArgs {
    pub cut: Id<Cut>,
    pub word: WordId,
    /// `{"text": "exe.dev"}` re-spells the word in captions, `"drop"` hides it, null
    /// clears the edit. Audio is untouched.
    pub edit: Option<Edit>,
}

#[derive(Debug)]
pub struct CaptionEdit;

#[command(
    name = "caption_edit",
    scope = "edit",
    undo = "undoable",
    doc = "Re-spell or drop one word in the captions (audio untouched), or clear that edit.",
    example = r#"{"cut": "01J00000000000000000000001", "word": {"asr_revision": "import:parakeet_coreml.v1", "ordinal": 812}, "edit": {"text": "exe.dev"}}"#
)]
impl Command for CaptionEdit {
    type Args = CaptionEditArgs;
    type Output = Done;
    fn run(a: CaptionEditArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(Edit::Text(t)) = &a.edit
            && t.chars().count() > 200
        {
            return Err(CmdError::invalid("/edit/text", "at most 200 characters"));
        }
        word_ok(&a.word)?;
        let cd = caption_doc(cx, a.cut)?;
        cx.tx.update(cd, |c| match a.edit {
            Some(e) => {
                c.edits.insert(a.word, e);
            }
            None => {
                c.edits.remove(&a.word);
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeywordToggleArgs {
    pub cut: Id<Cut>,
    pub word: WordId,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct KeywordToggled {
    /// The word is a keyword after the toggle.
    pub keyword: bool,
}

#[derive(Debug)]
pub struct CaptionKeywordToggle;

#[command(
    name = "caption_keyword_toggle",
    scope = "edit",
    undo = "undoable",
    doc = "Mark a word as a caption keyword (drawn in the keyword colour), or unmark it.",
    example = r#"{"cut": "01J00000000000000000000001", "word": {"asr_revision": "import:parakeet_coreml.v1", "ordinal": 812}}"#
)]
impl Command for CaptionKeywordToggle {
    type Args = KeywordToggleArgs;
    type Output = KeywordToggled;
    fn run(a: KeywordToggleArgs, cx: &mut Cx<'_>) -> Result<KeywordToggled, CmdError> {
        word_ok(&a.word)?;
        let cd = caption_doc(cx, a.cut)?;
        let existing: Vec<String> = cx
            .tx
            .get(cd)?
            .keywords
            .iter()
            .filter(|(_, w)| **w == a.word)
            .map(|(k, _)| k.clone())
            .collect();
        if existing.is_empty() {
            let key: Id<()> = cx.tx.mint();
            cx.tx.update(cd, |c| {
                c.keywords.insert(key.0.to_string(), a.word);
            })?;
            Ok(KeywordToggled { keyword: true })
        } else {
            cx.tx.update(cd, |c| {
                for k in &existing {
                    c.keywords.remove(k);
                }
            })?;
            Ok(KeywordToggled { keyword: false })
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeywordTermsArgs {
    pub cut: Id<Cut>,
    /// The cut's keyword terms (the reference's `keywords`), replacing the list: every caption word
    /// matching one (case and punctuation ignored) is drawn in the keyword colour. A term of
    /// several words ("MiniMax H3") stars runs that match it. At most 200, 80 characters each.
    pub terms: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct KeywordTermsSet {
    /// The list as stored (trimmed, empty and repeated terms dropped).
    pub terms: Vec<String>,
}

#[derive(Debug)]
pub struct CaptionKeywordsSet;

#[command(
    name = "caption_keywords_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set the cut's caption keyword terms (the story keywords list): every matching word across the cut is drawn in the keyword colour.",
    example = r#"{"cut": "01J00000000000000000000001", "terms": ["two minutes", "NVIDIA", "5080", "GPU", "locally"]}"#
)]
impl Command for CaptionKeywordsSet {
    type Args = KeywordTermsArgs;
    type Output = KeywordTermsSet;
    fn run(a: KeywordTermsArgs, cx: &mut Cx<'_>) -> Result<KeywordTermsSet, CmdError> {
        if a.terms.len() > 200 {
            return Err(CmdError::invalid("/terms", "at most 200 terms"));
        }
        let mut terms: Vec<String> = Vec::new();
        for (i, t) in a.terms.iter().enumerate() {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.chars().count() > 80 {
                return Err(CmdError::invalid(
                    &format!("/terms/{i}"),
                    "at most 80 characters",
                ));
            }
            if !t.is_empty() && !terms.iter().any(|x| x.to_lowercase() == t.to_lowercase()) {
                terms.push(t);
            }
        }
        let cd = caption_doc(cx, a.cut)?;
        let stored = terms.clone();
        cx.tx.update(cd, |c| c.keyword_terms = stored)?;
        Ok(KeywordTermsSet { terms })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TermSetArgs {
    /// The canonical spelling captions should show ("exe.dev").
    pub written: String,
    /// What the speech model has heard instead ("XD.dev", "x e dev").
    #[serde(default)]
    pub heard_as: Vec<String>,
    /// true removes the term.
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug)]
pub struct TermSet;

#[command(
    name = "term_set",
    scope = "edit",
    undo = "undoable",
    doc = "Add or change a project term (a name and how the speech model mishears it) for the caption term gate, or remove it.",
    example = r#"{"written": "exe.dev", "heard_as": ["XD.dev", "x e dev"]}"#
)]
impl Command for TermSet {
    type Args = TermSetArgs;
    type Output = Done;
    fn run(a: TermSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let written = a.written.trim().to_owned();
        if written.is_empty() || written.chars().count() > 80 {
            return Err(CmdError::invalid("/written", "1–80 characters"));
        }
        if a.heard_as.len() > 50 || a.heard_as.iter().any(|h| h.chars().count() > 80) {
            return Err(CmdError::invalid(
                "/heard_as",
                "at most 50, 80 characters each",
            ));
        }
        let key = written.to_lowercase();
        let term = Term {
            written,
            heard_as: a
                .heard_as
                .iter()
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
                .collect(),
        };
        let existing = cx.tx.peek().iter::<Terms>().next().map(|t| t.id);
        match existing {
            Some(id) => cx.tx.update(id, |t| {
                if a.remove {
                    t.entries.remove(&key);
                } else {
                    t.entries.insert(key, term);
                }
            })?,
            None if !a.remove => {
                let id: Id<Terms> = cx.tx.mint();
                cx.tx.create(Terms {
                    id,
                    entries: [(key, term)].into_iter().collect(),
                })?;
            }
            None => {}
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TermFixArgs {
    /// The words as the speech model heard them ("XD.dev", "x e dev"): 1–4 words.
    pub heard: String,
    /// The spelling every caption should show ("exe.dev").
    pub written: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TermFixed {
    /// How many times the heard words are said in the project's transcripts.
    pub found: usize,
    /// Whether the heard form was added to the term's variants (false when it is the
    /// term itself in another case, or already listed).
    pub variant_added: bool,
}

#[derive(Debug)]
pub struct TermFix;

#[command(
    name = "term_fix",
    scope = "edit",
    undo = "undoable",
    doc = "Fix a misheard name everywhere in the project, in one undoable step: every caption of every cut shows the term's spelling for the words heard (a project correction), and the term list gains the term with this variant, so the term gate checks it and later transcriptions spell it right. Audio is untouched.",
    example = r#"{"heard": "XD.dev", "written": "exe.dev"}"#
)]
impl Command for TermFix {
    type Args = TermFixArgs;
    type Output = TermFixed;
    fn run(a: TermFixArgs, cx: &mut Cx<'_>) -> Result<TermFixed, CmdError> {
        let heard = a.heard.split_whitespace().collect::<Vec<_>>().join(" ");
        let written = a.written.trim().to_owned();
        let words = heard.split(' ').count();
        if heard.is_empty() || heard.chars().count() > 80 || words > crate::terms::MAX_RUN {
            return Err(CmdError::invalid(
                "/heard",
                "1–4 words, at most 80 characters",
            ));
        }
        if written.is_empty() || written.chars().count() > 80 {
            return Err(CmdError::invalid("/written", "1–80 characters"));
        }
        if crate::terms::key(&heard).is_empty() || crate::terms::key(&written).is_empty() {
            return Err(CmdError::invalid("/heard", "letters or digits"));
        }
        // the correction every cut's captions apply (heard → written)
        let existing = cx
            .tx
            .peek()
            .iter::<CaptionCorrections>()
            .next()
            .map(|c| c.id);
        match existing {
            Some(id) => cx.tx.update(id, |c| {
                c.entries.insert(heard.clone(), written.clone());
            })?,
            None => {
                let id: Id<CaptionCorrections> = cx.tx.mint();
                cx.tx.create(CaptionCorrections {
                    id,
                    entries: [(heard.clone(), written.clone())].into_iter().collect(),
                })?;
            }
        }
        // the term, with the heard form as a variant (unless it is the term itself)
        let tkey = written.to_lowercase();
        let same = crate::terms::key(&heard) == crate::terms::key(&written);
        let tid = cx.tx.peek().iter::<Terms>().next().map(|t| t.id);
        let mut variant_added = false;
        let mut term = tid
            .and_then(|id| cx.tx.peek().get(id))
            .and_then(|t| t.entries.get(&tkey).cloned())
            .unwrap_or(Term {
                written: written.clone(),
                heard_as: Vec::new(),
            });
        term.written.clone_from(&written);
        if !same
            && !term
                .heard_as
                .iter()
                .any(|h| crate::terms::key(h) == crate::terms::key(&heard))
        {
            if term.heard_as.len() >= 50 {
                return Err(CmdError::invalid(
                    "/heard",
                    "the term has 50 variants already",
                ));
            }
            term.heard_as.push(heard.clone());
            variant_added = true;
        }
        match tid {
            Some(id) => cx.tx.update(id, |t| {
                t.entries.insert(tkey, term.clone());
            })?,
            None => {
                let id: Id<Terms> = cx.tx.mint();
                cx.tx.create(Terms {
                    id,
                    entries: [(tkey, term.clone())].into_iter().collect(),
                })?;
            }
        }
        // how often it is said, over every transcript the project uses
        let doc = cx.tx.peek();
        let mut owners: Vec<String> = cx.analysis.keys().into_iter().map(|k| k.0).collect();
        owners.dedup();
        let found = owners
            .iter()
            .filter_map(|o| cx.analysis.active(doc, o))
            .map(|t| {
                let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
                crate::terms::said(&texts, &heard)
            })
            .sum();
        Ok(TermFixed {
            found,
            variant_added,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepSetArgs {
    /// Words captions never split ("two minutes", "NVIDIA 5080 GPU"): 2–6 words.
    pub words: Vec<String>,
    /// false removes the entry.
    #[serde(default = "yes")]
    pub keep: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug)]
pub struct CaptionKeepSet;

#[command(
    name = "caption_keep_set",
    scope = "edit",
    undo = "undoable",
    doc = "Keep words together in caption cues across the project (story keep pairs: \"two minutes\"), or stop keeping them.",
    example = r#"{"words": ["web", "app"]}"#
)]
impl Command for CaptionKeepSet {
    type Args = KeepSetArgs;
    type Output = Done;
    fn run(a: KeepSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        let words: Vec<String> = a
            .words
            .iter()
            .map(|w| w.trim().to_owned())
            .filter(|w| !w.is_empty())
            .collect();
        if !(2..=6).contains(&words.len()) || words.iter().any(|w| w.chars().count() > 40) {
            return Err(CmdError::invalid("/words", "2–6 words, 40 characters each"));
        }
        let key = words.join(" ").to_lowercase();
        let existing = cx.tx.peek().iter::<KeepPairs>().next().map(|k| k.id);
        match existing {
            Some(id) => cx.tx.update(id, |k| {
                if a.keep {
                    k.entries.insert(key, words);
                } else {
                    k.entries.remove(&key);
                }
            })?,
            None if a.keep => {
                let id: Id<KeepPairs> = cx.tx.mint();
                cx.tx.create(KeepPairs {
                    id,
                    entries: [(key, words)].into_iter().collect(),
                })?;
            }
            None => {}
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CueBreakArgs {
    pub cut: Id<Cut>,
    /// The word the boundary is at.
    pub word: WordId,
    /// `"split"` starts a cue at the word, `"join"` keeps it in the cue of the word before
    /// (to join a cue with the next one, join the next cue's first word); null clears it.
    #[serde(rename = "break")]
    pub brk: Option<CueBreak>,
}

#[derive(Debug)]
pub struct CaptionCueBreak;

#[command(
    name = "caption_cue_break",
    scope = "edit",
    undo = "undoable",
    doc = "Override the caption grouping at one word: start a cue there (split), keep it with the word before (join), or clear that.",
    example = r#"{"cut": "01J00000000000000000000001", "word": {"asr_revision": "import:parakeet_coreml.v1", "ordinal": 812}, "break": "split"}"#
)]
impl Command for CaptionCueBreak {
    type Args = CueBreakArgs;
    type Output = Done;
    fn run(a: CueBreakArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        word_ok(&a.word)?;
        let cd = caption_doc(cx, a.cut)?;
        cx.tx.update(cd, |c| match a.brk {
            Some(b) => {
                c.cue_breaks.insert(a.word, b);
            }
            None => {
                c.cue_breaks.remove(&a.word);
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Start,
    End,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WordEdgeNudgeArgs {
    pub cut: Id<Cut>,
    /// The word with its fingerprint (its ASR start and end).
    pub word: WordRef,
    pub edge: Edge,
    /// Signed ms; the edge stays within ±2 s of the ASR edge and the word ≥ 20 ms.
    pub delta_ms: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Nudged {
    pub start: Flicks,
    pub end: Flicks,
}

#[derive(Debug)]
pub struct WordEdgeNudge;

#[command(
    name = "word_edge_nudge",
    scope = "edit",
    undo = "undoable",
    doc = "Move a word's start or end edge by a few ms (an override; the transcript is untouched).",
    example = r#"{"cut": "01J00000000000000000000001", "word": {"word_id": {"asr_revision": "r1", "ordinal": 5}, "t_src_start": 7056000000, "t_src_end": 7408800000, "normalised_text": "sixty"}, "edge": "end", "delta_ms": -25}"#
)]
impl Command for WordEdgeNudge {
    type Args = WordEdgeNudgeArgs;
    type Output = Nudged;
    fn run(a: WordEdgeNudgeArgs, cx: &mut Cx<'_>) -> Result<Nudged, CmdError> {
        word_ok(&a.word.word_id)?;
        if a.word.t_src_end <= a.word.t_src_start {
            return Err(CmdError::invalid("/word", "word end after start"));
        }
        let cd = caption_doc(cx, a.cut)?;
        let cur = cx
            .tx
            .get(cd)?
            .word_time_overrides
            .get(&a.word.word_id)
            .copied()
            .unwrap_or_default();
        let (mut s, mut e) = (
            cur.start.unwrap_or(a.word.t_src_start),
            cur.end.unwrap_or(a.word.t_src_end),
        );
        let d = ms_to_flicks(a.delta_ms);
        let two_s = ms_to_flicks(2_000).0;
        let min = ms_to_flicks(20).0;
        match a.edge {
            Edge::Start => {
                s = Flicks(s.0.saturating_add(d.0));
                if (s.0 - a.word.t_src_start.0).abs() > two_s || e.0 - s.0 < min {
                    return Err(CmdError::invalid(
                        "/delta_ms",
                        "out of range (±2 s, word ≥ 20 ms)",
                    ));
                }
            }
            Edge::End => {
                e = Flicks(e.0.saturating_add(d.0));
                if (e.0 - a.word.t_src_end.0).abs() > two_s || e.0 - s.0 < min {
                    return Err(CmdError::invalid(
                        "/delta_ms",
                        "out of range (±2 s, word ≥ 20 ms)",
                    ));
                }
            }
        }
        let ov = WordTimeOverride {
            start: (s != a.word.t_src_start).then_some(s),
            end: (e != a.word.t_src_end).then_some(e),
        };
        cx.tx.update(cd, |c| {
            if ov == WordTimeOverride::default() {
                c.word_time_overrides.remove(&a.word.word_id);
            } else {
                c.word_time_overrides.insert(a.word.word_id.clone(), ov);
            }
        })?;
        Ok(Nudged { start: s, end: e })
    }
}
