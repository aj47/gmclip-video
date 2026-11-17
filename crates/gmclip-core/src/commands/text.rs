//! Fonts and named text styles (caption and overlay presets): what a project needs before
//! its captions, hooks and pills can draw. Font *files* are never stored (SPEC §4.4): a
//! font is a face (family, style), its sha256 and its em-per-size; the host finds the
//! file by hash.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::entities::{Font, Styles, TextStyle};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Created, Done, bounded, created};
use crate::command::{Command, Cx};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FontAddArgs {
    pub family: String,
    #[serde(default)]
    pub style: String,
    /// The font file's sha256 (lower-case hex): how a host finds the file.
    pub sha256: String,
    /// em per libass size unit for this face.
    pub em_per_size: f64,
}

#[derive(Debug)]
pub struct FontAdd;

#[command(
    name = "font_add",
    scope = "edit",
    undo = "undoable",
    doc = "Add a font face to the project (family, style, sha256 of its file, em per size); returns the existing face when it is already there.",
    example = r#"{"family": "Neometric", "style": "Heavy", "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}"#
)]
impl Command for FontAdd {
    type Args = FontAddArgs;
    type Output = Created;
    fn run(a: FontAddArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        bounded(&a.family, "/family", 120)?;
        bounded(&a.style, "/style", 120)?;
        if a.family.trim().is_empty() {
            return Err(CmdError::invalid("/family", "a family name"));
        }
        if a.sha256.len() != 64
            || !a
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(CmdError::invalid("/sha256", "64 lower-case hex characters"));
        }
        if !a.em_per_size.is_finite() || a.em_per_size <= 0.0 || a.em_per_size > 10.0 {
            return Err(CmdError::invalid("/em_per_size", "a positive number"));
        }
        if let Some(f) = cx.tx.peek().iter::<Font>().find(|f| {
            f.family == a.family
                && f.style == a.style
                && f.sha256.as_deref() == Some(a.sha256.as_str())
        }) {
            return Ok(created(f.id));
        }
        let id: Id<Font> = cx.tx.mint();
        cx.tx.create(Font {
            id,
            family: a.family,
            style: a.style,
            sha256: Some(a.sha256),
            em_per_size: a.em_per_size,
        })?;
        Ok(created(id))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextStyleSetArgs {
    /// The style's name (`Speech`, `Hook`, `Pill`, …): `[A-Za-z0-9_-]`, ≤ 64.
    pub name: String,
    /// The style; null removes it.
    #[serde(default)]
    pub style: Option<TextStyle>,
}

#[derive(Debug)]
pub struct TextStyleSet;

#[command(
    name = "text_style_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set (or remove, with style null) a named text style the project's captions and overlays use; its font must be a face the project has (font_add).",
    example = r##"{"name": "Speech", "style": {"font": "Neometric Heavy", "size": 66.35, "fill": "#FFFFFFFF", "outline": {"w": 11.11, "colour": "#000000FF"}, "shadow": {"dx": 6.48, "dy": 6.48, "blur": 0, "colour": "#000000FF"}, "align": "centre", "max_width": null, "fit": "none", "line_height": 1, "letter_spacing": 0, "case": "as_is", "pill": null}}"##
)]
impl Command for TextStyleSet {
    type Args = TextStyleSetArgs;
    type Output = Done;
    fn run(a: TextStyleSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if a.name.is_empty()
            || a.name.len() > 64
            || !a
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(CmdError::invalid("/name", "[A-Za-z0-9_-], 1–64 characters"));
        }
        if let Some(s) = &a.style {
            bounded(&s.font, "/style/font", 200)?;
            let has_font = cx
                .tx
                .peek()
                .iter::<Font>()
                .any(|f| crate::planner::style::font_name(&f.family, &f.style) == s.font);
            if !has_font {
                return Err(CmdError::invalid(
                    "/style/font",
                    "a font face the project has (font_add first)",
                ));
            }
            if !s.size.is_finite() || s.size <= 0.0 || s.size > 1000.0 {
                return Err(CmdError::invalid(
                    "/style/size",
                    "an em size in short-side units, (0, 1000]",
                ));
            }
        }
        let existing = cx.tx.peek().iter::<Styles>().next().map(|s| s.id);
        match existing {
            Some(id) => cx.tx.update(id, |st| match a.style {
                Some(s) => {
                    st.entries.insert(a.name, s);
                }
                None => {
                    st.entries.remove(&a.name);
                }
            })?,
            None => {
                let Some(s) = a.style else {
                    return Ok(Done {});
                };
                let id: Id<Styles> = cx.tx.mint();
                cx.tx.create(Styles {
                    id,
                    entries: [(a.name, s)].into_iter().collect(),
                })?;
            }
        }
        Ok(Done {})
    }
}
