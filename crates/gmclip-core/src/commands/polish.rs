//! Edit polish (Ideas 2026-10-16, lane K2; decision 0025): the writes behind the polish
//! suggestions, each one transaction (one undo step).
//!
//! - `caption_keywords_mark`: star (or unstar) many caption words at once — the accepted
//!   stressed-word proposals — and optionally switch the caption style's automatic emphasis.
//! - `punch_ins_set`: add, remove or replace punch-ins — a pair of tagged eased visual splits
//!   per stressed word (zoom in at the word, back at the end of its phrase).
//! - `cut_loop_set`: the per-cut "loop the last frame" option (lint `loop_last_frame`).
//! - `loop_fix`: give the last part (or a short tail split off it) the opening's layout.
//!
//! Callouts go in through `overlay_add` and holds through `hold_screen_here`; the host
//! works out their arguments.

use std::collections::BTreeMap;

use gmclip_macros::command;
use gmclip_model::anchor::WordId;
use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::{Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::look::in_stack;
use super::part::MIN_PART;
use super::record_part_children;
use crate::command::{Command, Cx, model_err};
use crate::polish::{PUNCH_IN_MS, PUNCH_MAX, PUNCH_MIN, PUNCH_OUT_MS, PUNCH_SCALE, punch_crop};

const SEC: i64 = 705_600_000;

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
pub struct AutoEmphasis {
    pub framing: Id<Framing>,
    pub on: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeywordsMarkArgs {
    pub cut: Id<Cut>,
    /// The words to star (or unstar): at most 500.
    pub words: Vec<WordId>,
    /// true stars them, false unstars them.
    pub on: bool,
    /// Also switch the framing's caption style to mark stressed words on its own (or not).
    #[serde(default)]
    pub auto: Option<AutoEmphasis>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct KeywordsMarked {
    /// Words that changed (already starred or unstarred words are not counted).
    pub changed: u32,
}

#[derive(Debug)]
pub struct CaptionKeywordsMark;

#[command(
    name = "caption_keywords_mark",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Star (or unstar) several caption words as keywords in one step — e.g. the stressed-word proposals — and optionally switch the framing's caption style to mark stressed words on its own (auto).",
    example = r#"{"cut": "01J00000000000000000000001", "words": [{"asr_revision": "import:parakeet_coreml.v1", "ordinal": 812}], "on": true}"#
)]
impl Command for CaptionKeywordsMark {
    type Args = KeywordsMarkArgs;
    type Output = KeywordsMarked;
    fn run(a: KeywordsMarkArgs, cx: &mut Cx<'_>) -> Result<KeywordsMarked, CmdError> {
        if a.words.len() > 500 {
            return Err(CmdError::invalid("/words", "at most 500 words"));
        }
        for (i, w) in a.words.iter().enumerate() {
            super::bounded(&w.asr_revision, &format!("/words/{i}/asr_revision"), 128)?;
        }
        let cd = caption_doc(cx, a.cut)?;
        let doc = cx.tx.get(cd)?.clone();
        let mut changed = 0u32;
        let mut add: Vec<(String, WordId)> = Vec::new();
        let mut drop: Vec<String> = Vec::new();
        let mut seen: Vec<&WordId> = Vec::new();
        for w in &a.words {
            if seen.contains(&w) {
                continue;
            }
            seen.push(w);
            let keys: Vec<String> = doc
                .keywords
                .iter()
                .filter(|(_, x)| *x == w)
                .map(|(k, _)| k.clone())
                .collect();
            if a.on && keys.is_empty() {
                let key: Id<()> = cx.tx.mint();
                add.push((key.0.to_string(), w.clone()));
                changed += 1;
            } else if !a.on && !keys.is_empty() {
                drop.extend(keys);
                changed += 1;
            }
        }
        if !add.is_empty() || !drop.is_empty() {
            cx.tx.update(cd, |c| {
                for (k, w) in add {
                    c.keywords.insert(k, w);
                }
                for k in &drop {
                    c.keywords.remove(k);
                }
            })?;
        }
        if let Some(au) = a.auto {
            let f = cx.tx.get(au.framing)?.clone();
            if f.cut != a.cut {
                return Err(CmdError::invalid(
                    "/auto/framing",
                    "not a framing of the cut",
                ));
            }
            let mut look = f.caption_look.clone().unwrap_or_default();
            look.auto_emphasis = au.on;
            let look = (!look.is_plain()).then_some(look);
            if look != f.caption_look {
                cx.tx.update(au.framing, |x| x.caption_look = look)?;
            }
        }
        Ok(KeywordsMarked { changed })
    }
}

/// One punch-in: a zoom of `layer` in `part` from `at` to `until` (source times, snapped
/// to the framing's frame grid).
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Punch {
    pub part: Id<Part>,
    pub layer: Id<Layer>,
    pub at: Flicks,
    pub until: Flicks,
    /// Zoom, 0.06–0.10 (default 0.08 = 8 %).
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PunchInsSetArgs {
    pub framing: Id<Framing>,
    /// Punch-ins to add (at most 200).
    #[serde(default)]
    pub punches: Vec<Punch>,
    /// Remove every punch-in of the cut in this framing first (the per-cut switch).
    #[serde(default)]
    pub replace: bool,
    /// Punch-ins to remove, by their zoom-in split (the pair goes together).
    #[serde(default)]
    pub remove: Vec<Id<VisualSplit>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PunchInsSet {
    /// The zoom-in split of each punch-in added, in the order given.
    pub added: Vec<String>,
    /// Punch-ins removed.
    pub removed: u32,
}

/// The punch-in pairs of `part` in `framing`: `(in split, its out split)`.
pub fn punch_pairs(
    doc: &gmclip_model::Doc,
    part: Id<Part>,
    framing: Id<Framing>,
) -> Vec<(Id<VisualSplit>, Option<Id<VisualSplit>>)> {
    let list = gmclip_model::effective::splits_of(doc, part, framing);
    let mut out = Vec::new();
    for (i, s) in list.iter().enumerate() {
        if s.tag == Some(SplitTag::PunchIn) {
            let pair = list[i + 1..]
                .iter()
                .find(|x| x.tag.is_some())
                .filter(|x| x.tag == Some(SplitTag::PunchOut))
                .map(|x| x.id);
            out.push((s.id, pair));
        }
    }
    out
}

#[derive(Debug)]
pub struct PunchInsSetCmd;

#[command(
    name = "punch_ins_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Punch in on stressed words: each punch-in is a small eased zoom (6–10 %) of a layer's crop at a word, held to the end of its phrase, then eased back — a tagged pair of visual splits. Adds, removes (by the zoom-in split) or replaces every punch-in of the cut in a framing, as one transaction.",
    example = r#"{"framing": "01J00000000000000000000005", "punches": [{"part": "01J00000000000000000000003", "layer": "01J00000000000000000000007", "at": 1842800000000, "until": 1843900000000}], "replace": false}"#
)]
impl Command for PunchInsSetCmd {
    type Args = PunchInsSetArgs;
    type Output = PunchInsSet;
    fn run(a: PunchInsSetArgs, cx: &mut Cx<'_>) -> Result<PunchInsSet, CmdError> {
        if a.punches.len() > 200 || a.remove.len() > 200 {
            return Err(CmdError::invalid("/punches", "at most 200"));
        }
        let framing = cx.tx.get(a.framing)?.clone();
        let fpf = crate::planner::framing_fpf(framing.fps)
            .ok_or_else(|| CmdError::invalid("/framing", "no frame rate"))?;
        let parts = cx.tx.children::<Part>(framing.cut.entity());
        for p in &parts {
            record_part_children(cx, *p);
        }
        let mut removed = 0u32;
        // what goes first: everything (replace) or the named pairs
        let mut gone: Vec<Id<VisualSplit>> = Vec::new();
        for p in &parts {
            for (i, o) in punch_pairs(cx.tx.peek(), *p, a.framing) {
                if a.replace || a.remove.contains(&i) {
                    gone.push(i);
                    gone.extend(o);
                    removed += 1;
                }
            }
        }
        for (k, id) in a.remove.iter().enumerate() {
            if !a.replace && !gone.contains(id) {
                return Err(CmdError::invalid(
                    &format!("/remove/{k}"),
                    "not a punch-in of this framing",
                ));
            }
        }
        for id in gone {
            cx.tx.delete(id)?;
        }
        let snap = |p: &Part, t: Flicks| {
            Flicks(p.src_in.0 + ((t.0 - p.src_in.0) as f64 / fpf as f64).round() as i64 * fpf)
        };
        let mut added = Vec::new();
        for (k, pu) in a.punches.iter().enumerate() {
            let path = |f: &str| format!("/punches/{k}/{f}");
            let p = cx.tx.get(pu.part)?.clone();
            if p.cut != framing.cut {
                return Err(CmdError::invalid(&path("part"), "not in the framing's cut"));
            }
            in_stack(cx, pu.layer, pu.part, a.framing)?;
            let scale = pu.scale.unwrap_or(PUNCH_SCALE);
            if !(PUNCH_MIN..=PUNCH_MAX).contains(&scale) {
                return Err(CmdError::invalid(&path("scale"), "0.06–0.10"));
            }
            let (at, until) = (snap(&p, pu.at), snap(&p, pu.until));
            if !(p.src_in < at && at < until && until < p.src_out) || until.0 - at.0 < SEC * 3 / 10
            {
                return Err(CmdError::invalid(
                    &path("at"),
                    "inside the part, at least 0.3 s long",
                ));
            }
            let doc = cx.tx.peek();
            let list = gmclip_model::effective::splits_of(doc, pu.part, a.framing);
            if list.iter().any(|s| at <= s.at_src && s.at_src <= until) {
                return Err(CmdError::invalid(
                    &path("at"),
                    "another split or punch-in falls inside it",
                ));
            }
            // the split in force at `at` (its overrides stay; only the crop zooms)
            let base: BTreeMap<Id<Layer>, LayerPatch> = list
                .iter()
                .rev()
                .find(|s| s.at_src < at)
                .map(|s| s.overrides.clone())
                .unwrap_or_default();
            let settled = gmclip_model::effective::effective_settled(doc, pu.part, a.framing, at)
                .ok_or(CmdError::NotFound)?;
            let crop = settled
                .layers
                .iter()
                .find(|l| l.id == pu.layer)
                .map(|l| l.values.crop)
                .ok_or_else(|| CmdError::invalid(&path("layer"), "not shown there"))?;
            let mut zoom = base.clone();
            let patch = zoom.entry(pu.layer).or_default();
            patch.crop = Some(punch_crop(crop, scale));
            let in_id: Id<VisualSplit> = cx.tx.mint();
            cx.tx.create(VisualSplit {
                id: in_id,
                part: pu.part,
                framing: a.framing,
                at_src: at,
                overrides: zoom,
                transition: Transition::Ease {
                    dur_ms: PUNCH_IN_MS,
                    curve: Curve::EaseOut,
                },
                tag: Some(SplitTag::PunchIn),
            })?;
            let out_id: Id<VisualSplit> = cx.tx.mint();
            cx.tx.create(VisualSplit {
                id: out_id,
                part: pu.part,
                framing: a.framing,
                at_src: until,
                overrides: base,
                transition: Transition::Ease {
                    dur_ms: PUNCH_OUT_MS,
                    curve: Curve::EaseInOut,
                },
                tag: Some(SplitTag::PunchOut),
            })?;
            added.push(in_id.0.to_string());
        }
        Ok(PunchInsSet { added, removed })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutLoopSetArgs {
    pub cut: Id<Cut>,
    /// true: the cut loops (its last frame should match the opening); false: it does not;
    /// null: not set.
    pub on: Option<bool>,
}

#[derive(Debug)]
pub struct CutLoopSet;

#[command(
    name = "cut_loop_set",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Say whether a cut loops: on, its last frame should show the opening's layout and crop (lint loop_last_frame offers the fix).",
    example = r#"{"cut": "01J00000000000000000000001", "on": true}"#
)]
impl Command for CutLoopSet {
    type Args = CutLoopSetArgs;
    type Output = super::Done;
    fn run(a: CutLoopSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        if cx.tx.get(a.cut)?.loop_check != a.on {
            cx.tx.update(a.cut, |c| c.loop_check = a.on)?;
        }
        Ok(super::Done {})
    }
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopHow {
    /// The last part takes the opening's layout (its visual splits in this framing go).
    Layout,
    /// A short tail is split off the last part and takes the opening's layout.
    Tail,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopFixArgs {
    pub cut: Id<Cut>,
    pub framing: Id<Framing>,
    pub how: LoopHow,
    /// The tail's length (ms, 200–3000, default 600).
    #[serde(default)]
    pub tail_ms: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LoopFixed {
    /// The part that now shows the opening's layout.
    pub part: String,
    /// Visual splits removed from it.
    pub removed_splits: u32,
}

#[derive(Debug)]
pub struct LoopFix;

#[command(
    name = "loop_fix",
    scope = "edit",
    undo = "undoable",
    since = "M2",
    doc = "Make a cut's last frame match its opening (same layout and crop as part one): give the last part the opening's layout, or split a short tail off it (tail_ms) and give that the opening's layout. One transaction; marks the cut as looping.",
    example = r#"{"cut": "01J00000000000000000000001", "framing": "01J00000000000000000000005", "how": "tail", "tail_ms": 600}"#
)]
impl Command for LoopFix {
    type Args = LoopFixArgs;
    type Output = LoopFixed;
    fn run(a: LoopFixArgs, cx: &mut Cx<'_>) -> Result<LoopFixed, CmdError> {
        let c = cx.tx.get(a.cut)?.clone();
        let framing = cx.tx.get(a.framing)?.clone();
        if framing.cut != a.cut {
            return Err(CmdError::invalid("/framing", "not a framing of the cut"));
        }
        let fpf = crate::planner::framing_fpf(framing.fps)
            .ok_or_else(|| CmdError::invalid("/framing", "no frame rate"))?;
        let parts = cx.tx.children::<Part>(a.cut.entity());
        for p in &parts {
            record_part_children(cx, *p);
        }
        let layout = gmclip_model::clock::cut_layout(cx.tx.peek(), a.cut);
        let (first, last) = match (layout.first(), layout.last()) {
            (Some(f), Some(l)) => (f.part, l.part),
            _ => return Err(CmdError::invalid("/cut", "the cut has no parts")),
        };
        let target = match a.how {
            LoopHow::Layout => {
                if first == last {
                    return Err(CmdError::invalid("/how", "one part: add a tail instead"));
                }
                last
            }
            LoopHow::Tail => {
                let ms = a.tail_ms.unwrap_or(600);
                if !(200..=3000).contains(&ms) {
                    return Err(CmdError::invalid("/tail_ms", "200–3000 ms"));
                }
                let p = cx.tx.get(last)?.clone();
                let back = (i64::from(ms) * SEC / 1000).div_euclid(fpf).max(1) * fpf;
                let at = Flicks(p.src_out.0 - back);
                if at.0 - p.src_in.0 < MIN_PART.0 || p.src_out.0 - at.0 < MIN_PART.0 {
                    return Err(CmdError::invalid(
                        "/tail_ms",
                        "the last part is too short for that tail",
                    ));
                }
                edit::split_part(cx.tx.staging(), last, at).map_err(model_err)?
            }
        };
        // its visual splits in this framing go (they would undo the look)
        let mut removed_splits = 0u32;
        let splits: Vec<Id<VisualSplit>> =
            gmclip_model::effective::splits_of(cx.tx.peek(), target, a.framing)
                .iter()
                .map(|s| s.id)
                .collect();
        for s in splits {
            cx.tx.delete(s)?;
            removed_splits += 1;
        }
        // its own added layers in this framing go; the opening's are copied
        let own: Vec<Id<Layer>> = cx.tx.children::<Layer>(target.entity());
        for l in own {
            if cx.tx.get(l)?.owner
                == (LayerOwner::PartOverride {
                    part: target,
                    framing: a.framing,
                })
            {
                cx.tx.delete(l)?;
            }
        }
        let mut remap: BTreeMap<Id<Layer>, Id<Layer>> = BTreeMap::new();
        let theirs: Vec<Id<Layer>> = cx.tx.children::<Layer>(first.entity());
        for l in theirs {
            let mut x = cx.tx.get(l)?.clone();
            if x.owner
                != (LayerOwner::PartOverride {
                    part: first,
                    framing: a.framing,
                })
            {
                continue;
            }
            let nid: Id<Layer> = cx.tx.mint();
            remap.insert(l, nid);
            x.id = nid;
            x.owner = LayerOwner::PartOverride {
                part: target,
                framing: a.framing,
            };
            cx.tx.create(x)?;
        }
        let mut look = cx
            .tx
            .get(first)?
            .look
            .get(&a.framing)
            .cloned()
            .unwrap_or_default();
        look.overrides = look
            .overrides
            .into_iter()
            .map(|(k, v)| (remap.get(&k).copied().unwrap_or(k), v))
            .collect();
        cx.tx.update(target, |p| {
            p.look.insert(a.framing, look);
        })?;
        if c.loop_check != Some(true) {
            cx.tx.update(a.cut, |x| x.loop_check = Some(true))?;
        }
        Ok(LoopFixed {
            part: target.0.to_string(),
            removed_splits,
        })
    }
}
