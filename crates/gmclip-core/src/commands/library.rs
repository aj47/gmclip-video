//! The style library (SPEC §8 [M2]): caption presets saved in the project, applied to a
//! framing, and listed for the caption style gallery ([`crate::library`]).

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::*;
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Done, bounded};
use crate::command::{Command, Cx, model_err};
use crate::library::{self, PresetError};

fn preset_name(n: &str) -> Result<(), CmdError> {
    if n.trim().is_empty() || n.chars().count() > 60 || n.chars().any(char::is_control) {
        return Err(CmdError::invalid("/name", "1–60 printable characters"));
    }
    Ok(())
}

/// Checks a preset's style the way `text_style_set` checks a style.
fn check_preset(p: &CaptionPreset, cx: &Cx<'_>) -> Result<(), CmdError> {
    bounded(&p.style.font, "/preset/style/font", 200)?;
    if library::font_of(cx.tx.peek(), &p.style.font).is_none() {
        return Err(CmdError::invalid(
            "/preset/style/font",
            "a font face the project has (font_add first)",
        ));
    }
    if !p.style.size.is_finite() || p.style.size <= 0.0 || p.style.size > 1000.0 {
        return Err(CmdError::invalid("/preset/style/size", "(0, 1000]"));
    }
    if let Some(c) = &p.look.keyword_colour
        && gmclip_plan::scalars::Rgba::new(c.clone()).is_err()
    {
        return Err(CmdError::invalid(
            "/preset/look/keyword_colour",
            "#RRGGBBAA",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionPresetSaveArgs {
    /// The preset's name in the project's style library.
    pub name: String,
    /// The preset; null removes it.
    #[serde(default)]
    pub preset: Option<CaptionPreset>,
}

#[derive(Debug)]
pub struct CaptionPresetSave;

#[command(
    name = "caption_preset_save",
    scope = "edit",
    undo = "undoable",
    doc = "Save (or remove, with preset null) a caption preset — text style, grouping (story_v5, single_word, adaptive_words, readable_phrase, phrase) and look (animation none/bounce/pop/fade, keyword animation none/pop, keyword colour) — in the project's style library.",
    example = r##"{"name": "My bounce", "preset": {"style": {"font": "Figtree Black", "size": 71.1, "fill": "#FFFFFFFF", "outline": {"w": 11.11, "colour": "#000000FF"}, "shadow": null, "align": "centre", "max_width": null, "fit": "none", "line_height": 1, "letter_spacing": 0, "case": "upper", "pill": null}, "grouping": "adaptive_words", "look": {"animation": "bounce", "keyword_animation": "pop", "keyword_colour": "#FFD400FF"}}}"##
)]
impl Command for CaptionPresetSave {
    type Args = CaptionPresetSaveArgs;
    type Output = Done;
    fn run(a: CaptionPresetSaveArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        preset_name(&a.name)?;
        if let Some(p) = &a.preset {
            check_preset(p, cx)?;
        }
        let existing = cx.tx.peek().iter::<CaptionPresets>().next().map(|s| s.id);
        match existing {
            Some(id) => cx.tx.update(id, |l| match a.preset {
                Some(p) => {
                    l.entries.insert(a.name, p);
                }
                None => {
                    l.entries.remove(&a.name);
                }
            })?,
            None => {
                let Some(p) = a.preset else {
                    return Ok(Done {});
                };
                let id: Id<CaptionPresets> = cx.tx.mint();
                cx.tx.create(CaptionPresets {
                    id,
                    entries: [(a.name, p)].into_iter().collect(),
                })?;
            }
        }
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionPresetApplyArgs {
    pub framing: Id<Framing>,
    /// A preset of the project's library or a built-in (`story`, `word_bounce`,
    /// `bold_pop`, `one_word`, `readable`, `phrase_box`).
    pub name: String,
    /// Apply this preset under `name` instead of looking it up.
    #[serde(default)]
    pub preset: Option<CaptionPreset>,
}

#[derive(Debug)]
pub struct CaptionPresetApply;

#[command(
    name = "caption_preset_apply",
    scope = "edit",
    undo = "undoable",
    doc = "Apply a caption preset (the project's or a built-in: story, word_bounce, bold_pop, one_word, readable, phrase_box) to a framing: its style becomes the framing's caption style (as Cap-<name>), with its grouping and look. One undo step.",
    example = r#"{"framing": "01J00000000000000000000003", "name": "word_bounce"}"#
)]
impl Command for CaptionPresetApply {
    type Args = CaptionPresetApplyArgs;
    type Output = Done;
    fn run(a: CaptionPresetApplyArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        preset_name(&a.name)?;
        cx.tx.get(a.framing)?;
        let preset = match a.preset {
            Some(p) => {
                check_preset(&p, cx)?;
                p
            }
            None => library::resolve(cx.tx.peek(), &a.name).map_err(|e| match e {
                PresetError::Unknown => CmdError::invalid("/name", "no preset of that name"),
                PresetError::NoFont(_) => CmdError::invalid(
                    "/name",
                    "the preset's font is not in the project (font_add first)",
                ),
            })?,
        };
        // recorded reads: the styles and presets this replaces
        for id in cx
            .tx
            .peek()
            .iter::<Styles>()
            .map(|s| s.id)
            .collect::<Vec<_>>()
        {
            cx.tx.get(id)?;
        }
        library::apply(cx.tx.staging(), a.framing, &a.name, &preset).map_err(model_err)?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionPresetListArgs {
    /// Marks the preset this framing uses.
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PresetRow {
    pub name: String,
    pub label: String,
    /// Built in (else saved in this project).
    pub builtin: bool,
    pub font: String,
    pub grouping: CaptionGrouping,
    pub look: CaptionLook,
    /// The face is in the project (else `font_add` it first).
    pub ready: bool,
    /// The framing's caption style is this preset's.
    pub current: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PresetList {
    pub presets: Vec<PresetRow>,
}

#[derive(Debug)]
pub struct CaptionPresetList;

#[command(
    name = "caption_preset_list",
    scope = "read",
    undo = "read_only",
    doc = "The caption presets this project can apply: the built-in gallery and the project's saved styles, with their font, grouping and look, whether the font is in the project, and which one a framing uses.",
    example = r#"{"framing": "01J00000000000000000000003"}"#
)]
impl Command for CaptionPresetList {
    type Args = CaptionPresetListArgs;
    type Output = PresetList;
    fn run(a: CaptionPresetListArgs, cx: &mut Cx<'_>) -> Result<PresetList, CmdError> {
        if let Some(f) = a.framing {
            cx.tx.get(f)?;
        }
        let doc = cx.tx.peek();
        let current = |n: &str| a.framing.is_some_and(|f| library::is_current(doc, f, n));
        let mut out = Vec::new();
        for b in library::builtins() {
            let p = b.preset(1.0);
            out.push(PresetRow {
                name: b.name.into(),
                label: b.label.into(),
                builtin: true,
                font: b.font.into(),
                grouping: p.grouping,
                look: p.look,
                ready: library::font_of(doc, b.font).is_some(),
                current: current(b.name),
            });
        }
        if let Some(l) = doc.iter::<CaptionPresets>().next() {
            for (n, p) in &l.entries {
                out.push(PresetRow {
                    name: n.clone(),
                    label: n.clone(),
                    builtin: false,
                    font: p.style.font.clone(),
                    grouping: p.grouping,
                    look: p.look.clone(),
                    ready: library::font_of(doc, &p.style.font).is_some(),
                    current: current(n),
                });
            }
        }
        Ok(PresetList { presets: out })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionsExportArgs {
    pub cut: Id<Cut>,
    /// The framing whose captions (default: the cut's first).
    #[serde(default)]
    pub framing: Option<Id<Framing>>,
    /// `srt`, `vtt` or `ass`.
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptionsFile {
    pub format: String,
    pub cues: usize,
    /// The file's text.
    pub text: String,
}

/// Font files are not needed to write subtitles: any path will do.
struct NoFiles;

impl crate::planner::PlanHost for NoFiles {
    fn font_path(&self, _: &Font) -> Option<String> {
        Some(String::new())
    }
}

#[derive(Debug)]
pub struct CaptionsExport;

#[command(
    name = "captions_export",
    scope = "read",
    undo = "read_only",
    doc = "A cut's captions as a subtitle file (srt, vtt, or ass with the styles approximated): the cues the export burns in, on the output clock. Returns the file's text.",
    example = r#"{"cut": "01J00000000000000000000001", "format": "srt"}"#
)]
impl Command for CaptionsExport {
    type Args = CaptionsExportArgs;
    type Output = CaptionsFile;
    fn run(a: CaptionsExportArgs, cx: &mut Cx<'_>) -> Result<CaptionsFile, CmdError> {
        let format = gmclip_plan::subtitles::Format::parse(&a.format)
            .ok_or_else(|| CmdError::invalid("/format", "srt, vtt or ass"))?;
        cx.tx.get(a.cut)?;
        let framings = cx.tx.children::<Framing>(a.cut.entity());
        let framing = match a.framing {
            Some(f) if framings.contains(&f) => f,
            Some(_) => return Err(CmdError::invalid("/framing", "a framing of this cut")),
            None => *framings
                .first()
                .ok_or_else(|| CmdError::invalid("/cut", "the cut has no framing"))?,
        };
        let plan = crate::planner::plan_cut_with(
            cx.tx.peek(),
            a.cut,
            framing,
            &NoFiles,
            Some(cx.analysis),
        )
        .map_err(|e| CmdError::Unsupported {
            platform: "planner".into(),
            reason: e.0.chars().take(160).collect(),
        })?;
        let cues = gmclip_plan::subtitles::captions(&plan).len();
        Ok(CaptionsFile {
            format: format.extension().into(),
            cues,
            text: gmclip_plan::subtitles::write(&plan, format),
        })
    }
}
