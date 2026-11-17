//! The style library (SPEC §8 [M2]): caption presets — the built-in gallery and the
//! project's own saved styles — and applying one to a framing.
//!
//! A preset is a text style, a grouping and a look ([`CaptionPreset`]). Applying one
//! writes its style into the project's named styles as `Cap-<name>` and points the
//! framing's caption style, grouping and look at it; that is one transaction, so one
//! undo. The gallery previews a preset by planning the cut on a scratch copy of the
//! document with it applied ([`preview_doc`]); nothing is committed.
//!
//! Built-in sizes are libass sizes on a 1080 short side, converted to the model's em in
//! short-side units with the face's `em_per_size`, so a preset draws the same size in
//! every face: AJ's presets use Neometric Heavy (the reference's face), the others the bundled OFL
//! Figtree Black.

use gmclip_model::entities::*;
use gmclip_model::{Author, Doc, Id, Staging};

use crate::planner::style::font_name;

/// The face AJ's the reference presets use.
pub const STORY_FONT: &str = "Neometric Heavy";
/// The bundled OFL face every other built-in uses.
pub const FREE_FONT: &str = "Figtree Black";

/// A built-in caption preset: its name, gallery label, face and look.
#[derive(Clone, Debug)]
pub struct Builtin {
    pub name: &'static str,
    pub label: &'static str,
    pub font: &'static str,
    /// libass size at a 1080 short side.
    pub size: f64,
    /// Outline and shadow offset, px at 1080.
    pub outline: f64,
    pub shadow: f64,
    /// A box behind each line: (colour `#RRGGBB`, opacity).
    pub pill: Option<(&'static str, f64)>,
    pub case: TextCase,
    pub grouping: CaptionGrouping,
    pub animation: CaptionAnimation,
    pub keyword_animation: KeywordAnimation,
    pub keyword_colour: Option<&'static str>,
}

/// The built-in gallery, AJ's two first.
pub fn builtins() -> Vec<Builtin> {
    let base = Builtin {
        name: "story",
        label: "Story",
        font: STORY_FONT,
        size: 86.0,
        outline: 12.0,
        shadow: 7.0,
        pill: None,
        case: TextCase::AsIs,
        grouping: CaptionGrouping::StoryV5,
        animation: CaptionAnimation::None,
        keyword_animation: KeywordAnimation::None,
        keyword_colour: None,
    };
    vec![
        base.clone(),
        Builtin {
            name: "word_bounce",
            label: "Word bounce",
            grouping: CaptionGrouping::AdaptiveWords,
            animation: CaptionAnimation::Bounce,
            keyword_animation: KeywordAnimation::Pop,
            ..base.clone()
        },
        Builtin {
            name: "bold_pop",
            label: "Bold pop",
            font: FREE_FONT,
            size: 96.0,
            outline: 10.0,
            shadow: 6.0,
            case: TextCase::Upper,
            grouping: CaptionGrouping::AdaptiveWords,
            animation: CaptionAnimation::Pop,
            keyword_animation: KeywordAnimation::Pop,
            keyword_colour: Some("#3DDC84FF"),
            ..base.clone()
        },
        Builtin {
            name: "one_word",
            label: "One word",
            font: FREE_FONT,
            size: 110.0,
            outline: 12.0,
            shadow: 7.0,
            case: TextCase::Upper,
            grouping: CaptionGrouping::SingleWord,
            animation: CaptionAnimation::Bounce,
            ..base.clone()
        },
        Builtin {
            name: "readable",
            label: "Readable",
            font: FREE_FONT,
            size: 64.0,
            outline: 8.0,
            shadow: 4.0,
            grouping: CaptionGrouping::ReadablePhrase,
            animation: CaptionAnimation::Fade,
            ..base.clone()
        },
        Builtin {
            name: "phrase_box",
            label: "Phrase box",
            font: FREE_FONT,
            size: 60.0,
            outline: 0.0,
            shadow: 0.0,
            pill: Some(("#000000", 0.6)),
            grouping: CaptionGrouping::Phrase,
            ..base
        },
    ]
}

fn u(px: f64) -> f64 {
    px * 1000.0 / 1080.0
}

impl Builtin {
    /// The preset in a face with `em_per_size`.
    pub fn preset(&self, em: f64) -> CaptionPreset {
        CaptionPreset {
            style: TextStyle {
                font: self.font.to_owned(),
                size: self.size * em * 1000.0 / 1080.0,
                fill: "#FFFFFFFF".into(),
                outline: (self.outline > 0.0).then(|| Outline {
                    w: u(self.outline),
                    colour: "#000000FF".into(),
                }),
                shadow: (self.shadow > 0.0).then(|| Shadow {
                    dx: u(self.shadow),
                    dy: u(self.shadow),
                    blur: 0.0,
                    colour: "#000000FF".into(),
                }),
                align: TextAlign::Centre,
                max_width: None,
                fit: TextFit::None,
                line_height: 1.0,
                letter_spacing: 0.0,
                case: self.case,
                pill: self.pill.map(|(c, o)| Pill {
                    on: true,
                    colour: c.to_owned(),
                    opacity: o,
                    radius: u(18.0),
                    pad_x: u(18.0),
                    pad_y: u(8.0),
                    per_line: false,
                }),
            },
            grouping: self.grouping,
            look: CaptionLook {
                animation: self.animation,
                keyword_animation: self.keyword_animation,
                keyword_colour: self.keyword_colour.map(str::to_owned),
                auto_emphasis: false,
            },
        }
    }
}

/// A face of the project by its font name.
pub fn font_of<'d>(doc: &'d Doc, name: &str) -> Option<&'d Font> {
    doc.iter::<Font>()
        .find(|f| font_name(&f.family, &f.style) == name)
}

/// Why a preset cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetError {
    Unknown,
    /// The face it draws with is not in the project (`font_add` it first).
    NoFont(String),
}

/// A preset by name: the project's saved ones first, then the built-ins (in the face's
/// em, which must be in the project).
pub fn resolve(doc: &Doc, name: &str) -> Result<CaptionPreset, PresetError> {
    if let Some(p) = doc
        .iter::<CaptionPresets>()
        .next()
        .and_then(|l| l.entries.get(name))
    {
        return Ok(p.clone());
    }
    let b = builtins()
        .into_iter()
        .find(|b| b.name == name)
        .ok_or(PresetError::Unknown)?;
    let f = font_of(doc, b.font).ok_or_else(|| PresetError::NoFont(b.font.to_owned()))?;
    Ok(b.preset(f.em_per_size))
}

/// The named style a preset is written to (`Cap-<name>`, `[A-Za-z0-9_-]`, ≤ 64).
pub fn style_name(preset: &str) -> String {
    let slug: String = preset
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    format!("Cap-{slug}")
}

/// Whether `framing` draws its captions with preset `name`: its caption style is the
/// preset's `Cap-<name>`, or (for `story`) the reference's own `Speech` style with the reference's grouping and
/// look, which is what turning captions on sets.
pub fn is_current(doc: &Doc, framing: Id<Framing>, name: &str) -> bool {
    let Some(f) = doc.get(framing) else {
        return false;
    };
    match f.caption_style_ref.as_deref() {
        Some(r) if r == style_name(name) => true,
        None | Some("Speech") => {
            name == "story"
                && f.caption_grouping == CaptionGrouping::StoryV5
                && f.caption_look.as_ref().is_none_or(CaptionLook::is_plain)
        }
        _ => false,
    }
}

/// Writes `preset` as `Cap-<name>` and points `framing` at it (style, grouping, look).
pub fn apply(
    st: &mut Staging<'_>,
    framing: Id<Framing>,
    name: &str,
    preset: &CaptionPreset,
) -> Result<(), gmclip_model::ModelError> {
    let sname = style_name(name);
    let existing = st.doc().iter::<Styles>().next().map(|s| s.id);
    match existing {
        Some(id) => st.update(id, |s| {
            s.entries.insert(sname.clone(), preset.style.clone());
        })?,
        None => {
            let id: Id<Styles> = st.mint();
            st.create(Styles {
                id,
                entries: [(sname.clone(), preset.style.clone())]
                    .into_iter()
                    .collect(),
            })?;
        }
    }
    st.update(framing, |f| {
        f.caption_style_ref = Some(sname);
        f.caption_grouping = preset.grouping;
        f.caption_look = (!preset.look.is_plain()).then(|| preset.look.clone());
    })?;
    Ok(())
}

/// Ids for scratch documents (never committed, so never seen again).
struct Scratch(u128);

impl gmclip_model::MintId for Scratch {
    fn mint(&mut self) -> gmclip_model::Ulid {
        self.0 += 1;
        gmclip_model::Ulid(self.0)
    }
}

/// `doc` with `preset` applied to `framing`, for previewing (never committed); `fonts`
/// are faces the scratch copy gets when the project lacks them (a gallery preview of a
/// preset whose face is not added yet).
pub fn preview_doc(
    doc: &Doc,
    framing: Id<Framing>,
    name: &str,
    preset: &CaptionPreset,
    fonts: &[Font],
) -> Result<Doc, gmclip_model::ModelError> {
    let mut mint = Scratch(u128::MAX / 2);
    let mut st = Staging::new(doc.clone(), Author::System, &mut mint);
    for f in fonts {
        if font_of(st.doc(), &font_name(&f.family, &f.style)).is_none() {
            let id: Id<Font> = st.mint();
            st.create(Font { id, ..f.clone() })?;
        }
    }
    apply(&mut st, framing, name, preset)?;
    Ok(st.into_work())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_named_once_and_aj_first() {
        let b = builtins();
        let mut names: Vec<_> = b.iter().map(|b| b.name).collect();
        assert_eq!(&names[..2], ["story", "word_bounce"]);
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), b.len());
        // The reference's Speech: libass 86 in Neometric (em 0.8332) is the short.rs size
        let p = b[0].preset(0.8332);
        assert!((p.style.size - 86.0 * 0.8332 * 1000.0 / 1080.0).abs() < 1e-9);
        assert!(p.look.is_plain());
        assert_eq!(b[1].preset(0.8332).look.animation, CaptionAnimation::Bounce);
        assert_eq!(style_name("my style!"), "Cap-my-style-");
    }
}
