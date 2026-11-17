//! The brand kit (SPEC §8, §10.5; profile content, never authority — R-PROF-4): name and
//! handle, faces, colours, logo, caption preset and overlay templates, and what is made
//! from it — lower thirds, end cards — plus inserting a saved template.
//!
//! A new framing takes the kit's caption preset and background (`framing_create`). A
//! template is an overlay without its times and parent: kind, role, band, z, opacity,
//! geometry, duration, and its text, image or shape; inserting one at an anchor is one
//! `overlay_add`.

use gmclip_macros::command;
use gmclip_model::Id;
use gmclip_model::anchor::{Anchor, CutEdge};
use gmclip_model::entities::*;
use gmclip_model::geometry::{CanvasBox, Place};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Created, Done, bounded, created};
use crate::command::{Command, Cx};
use crate::library;

fn rgb_ok(c: &str, path: &str) -> Result<(), CmdError> {
    gmclip_plan::scalars::Rgb::new(c.to_owned())
        .map(|_| ())
        .map_err(|_| CmdError::invalid(path, "#RRGGBB"))
}

fn face_ok(cx: &Cx<'_>, name: &str, path: &str) -> Result<(), CmdError> {
    bounded(name, path, 200)?;
    if library::font_of(cx.tx.peek(), name).is_none() {
        return Err(CmdError::invalid(
            path,
            "a font face the project has (font_add first)",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrandKitSetArgs {
    #[serde(default)]
    pub name: Option<String>,
    /// `@techfren`.
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub heading_font: Option<String>,
    #[serde(default)]
    pub body_font: Option<String>,
    #[serde(default)]
    pub colours: Option<BrandColours>,
    /// The logo: an `asset_add` asset and its size.
    #[serde(default)]
    pub logo: Option<OverlayImage>,
    #[serde(default)]
    pub clear_logo: bool,
    /// A caption preset (built-in or the project's) new framings use.
    #[serde(default)]
    pub caption_preset: Option<String>,
    #[serde(default)]
    pub clear_caption_preset: bool,
}

#[derive(Debug)]
pub struct BrandKitSet;

/// The project's kit, created empty when absent.
fn kit_id(cx: &mut Cx<'_>) -> Result<Id<BrandKit>, CmdError> {
    let kit = cx.tx.peek().iter::<BrandKit>().next().map(|k| k.id);
    if let Some(k) = kit {
        cx.tx.get(k)?;
        return Ok(k);
    }
    let id: Id<BrandKit> = cx.tx.mint();
    cx.tx.create(BrandKit {
        id,
        name: String::new(),
        handle: String::new(),
        heading_font: None,
        body_font: None,
        colours: BrandColours::default(),
        logo: None,
        caption_preset: None,
        templates: Default::default(),
    })?;
    Ok(id)
}

#[command(
    name = "brand_kit_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set the project's brand kit: name, handle, heading and body faces (font_add first), colours (primary, accent, text, background as #RRGGBB), logo (an asset), and the caption preset new framings use. Only the fields given change.",
    example = r##"{"name": "TechFren", "handle": "@techfren", "heading_font": "Figtree Black", "colours": {"primary": "#0A84FF", "accent": "#FFD400", "text": "#FFFFFF", "background": "#0C1118"}, "caption_preset": "word_bounce"}"##
)]
impl Command for BrandKitSet {
    type Args = BrandKitSetArgs;
    type Output = Done;
    fn run(a: BrandKitSetArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if let Some(n) = &a.name {
            bounded(n, "/name", 80)?;
        }
        if let Some(h) = &a.handle {
            bounded(h, "/handle", 80)?;
        }
        if let Some(f) = &a.heading_font {
            face_ok(cx, f, "/heading_font")?;
        }
        if let Some(f) = &a.body_font {
            face_ok(cx, f, "/body_font")?;
        }
        if let Some(c) = &a.colours {
            rgb_ok(&c.primary, "/colours/primary")?;
            rgb_ok(&c.accent, "/colours/accent")?;
            rgb_ok(&c.text, "/colours/text")?;
            rgb_ok(&c.background, "/colours/background")?;
        }
        if let Some(l) = a.logo {
            cx.tx.get(l.asset)?;
            if l.w == 0 || l.h == 0 || l.w > 8192 || l.h > 8192 {
                return Err(CmdError::invalid("/logo", "1–8192 px a side"));
            }
        }
        if let Some(p) = &a.caption_preset
            && let Err(library::PresetError::Unknown) = library::resolve(cx.tx.peek(), p)
        {
            return Err(CmdError::invalid(
                "/caption_preset",
                "no preset of that name",
            ));
        }
        let id = kit_id(cx)?;
        cx.tx.update(id, |k| {
            if let Some(n) = a.name {
                k.name = n;
            }
            if let Some(h) = a.handle {
                k.handle = h;
            }
            if let Some(f) = a.heading_font {
                k.heading_font = Some(f);
            }
            if let Some(f) = a.body_font {
                k.body_font = Some(f);
            }
            if let Some(c) = a.colours {
                k.colours = c;
            }
            if a.clear_logo {
                k.logo = None;
            }
            if let Some(l) = a.logo {
                k.logo = Some(l);
            }
            if a.clear_caption_preset {
                k.caption_preset = None;
            }
            if let Some(p) = a.caption_preset {
                k.caption_preset = Some(p);
            }
        })?;
        Ok(Done {})
    }
}

/// The template of an overlay (what `overlay_template_save` keeps).
pub fn template_of(o: &Overlay) -> OverlayTemplate {
    OverlayTemplate {
        kind: o.kind,
        role: o.role.clone(),
        band: o.band.clone(),
        z: o.z,
        opacity: o.opacity,
        geom: o.geom,
        dur_ms: match o.end {
            OverlayEnd::DurMs(ms) => ms,
            OverlayEnd::At(_) => 3000,
        },
        text: o.text.clone(),
        counter: o.counter.as_ref().map(|c| TemplateCounter {
            clock: c.clock,
            format: c.format.clone(),
            running_label: c.running_label.clone(),
            done_label: c.done_label.clone(),
        }),
        image: o.image,
        shape: o.shape.clone(),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayTemplateSaveArgs {
    /// The template's name in the brand kit.
    pub name: String,
    /// Save this overlay as the template.
    #[serde(default)]
    pub overlay: Option<Id<Overlay>>,
    /// Or this template as given.
    #[serde(default)]
    pub template: Option<OverlayTemplate>,
    /// Remove the template of that name instead.
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug)]
pub struct OverlayTemplateSave;

#[command(
    name = "overlay_template_save",
    scope = "edit",
    undo = "undoable",
    doc = "Save an overlay (a section pill, run clock, text hook, lower third, image or callout) as a named template in the brand kit, or a template as given; `remove: true` deletes it. Insert one with overlay_template_insert.",
    example = r#"{"name": "Section pill", "overlay": "01J00000000000000000000005"}"#
)]
impl Command for OverlayTemplateSave {
    type Args = OverlayTemplateSaveArgs;
    type Output = Done;
    fn run(a: OverlayTemplateSaveArgs, cx: &mut Cx<'_>) -> Result<Done, CmdError> {
        if a.name.trim().is_empty() || a.name.chars().count() > 60 {
            return Err(CmdError::invalid("/name", "1–60 characters"));
        }
        let t = match (a.remove, a.overlay, a.template) {
            (true, _, _) => None,
            (false, Some(o), _) => Some(template_of(cx.tx.get(o)?)),
            (false, None, Some(t)) => {
                if let Some(sh) = &t.shape {
                    let r = match t.geom {
                        OverlayGeom::Box(b) => Some(b),
                        OverlayGeom::Place(_) => None,
                    };
                    super::overlay::shape_ok(sh, r.as_ref())?;
                }
                if let Some(im) = t.image {
                    cx.tx.get(im.asset)?;
                }
                Some(t)
            }
            (false, None, None) => {
                return Err(CmdError::invalid("/overlay", "an overlay or a template"));
            }
        };
        let id = kit_id(cx)?;
        cx.tx.update(id, |k| match t {
            Some(t) => {
                k.templates.insert(a.name, t);
            }
            None => {
                k.templates.remove(&a.name);
            }
        })?;
        Ok(Done {})
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayTemplateInsertArgs {
    pub cut: Id<Cut>,
    pub name: String,
    pub start: Anchor,
    /// A run clock template's measurement (what it counts).
    #[serde(default)]
    pub measurement: Option<Id<Measurement>>,
}

#[derive(Debug)]
pub struct OverlayTemplateInsert;

#[command(
    name = "overlay_template_insert",
    scope = "edit",
    undo = "undoable",
    doc = "Insert a brand kit overlay template on a cut at an anchor, for the template's duration (a run clock template needs the measurement it counts).",
    example = r#"{"cut": "01J00000000000000000000001", "name": "Section pill", "start": {"cut_start": {"cut": "01J00000000000000000000001"}}}"#
)]
impl Command for OverlayTemplateInsert {
    type Args = OverlayTemplateInsertArgs;
    type Output = Created;
    fn run(a: OverlayTemplateInsertArgs, cx: &mut Cx<'_>) -> Result<Created, CmdError> {
        let kit = cx
            .tx
            .peek()
            .iter::<BrandKit>()
            .next()
            .map(|k| k.id)
            .ok_or_else(|| CmdError::invalid("/name", "the project has no brand kit"))?;
        let t = cx
            .tx
            .get(kit)?
            .templates
            .get(&a.name)
            .cloned()
            .ok_or_else(|| CmdError::invalid("/name", "no template of that name"))?;
        let counter = match (&t.counter, a.measurement) {
            (Some(c), Some(m)) => {
                cx.tx.get(m)?;
                Some(Counter {
                    start: CounterStart::Measurement(m),
                    stop: None,
                    clock: c.clock,
                    format: c.format.clone(),
                    running_label: c.running_label.clone(),
                    done_label: c.done_label.clone(),
                })
            }
            (Some(_), None) => {
                return Err(CmdError::invalid(
                    "/measurement",
                    "a run clock template counts a measurement",
                ));
            }
            (None, _) => None,
        };
        insert(cx, a.cut, &t, a.start, OverlayEnd::DurMs(t.dur_ms), counter)
    }
}

/// Puts a template on `cut` from `start` to `end`.
fn insert(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    t: &OverlayTemplate,
    start: Anchor,
    end: OverlayEnd,
    counter: Option<Counter>,
) -> Result<Created, CmdError> {
    insert_id(cx, cut, t, start, end, counter).map(created)
}

/// [`insert`], answering the overlay's id.
fn insert_id(
    cx: &mut Cx<'_>,
    cut: Id<Cut>,
    t: &OverlayTemplate,
    start: Anchor,
    end: OverlayEnd,
    counter: Option<Counter>,
) -> Result<Id<Overlay>, CmdError> {
    cx.tx.get(cut)?;
    let _ = cx.tx.children::<Part>(cut.entity());
    crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), Some(cut), &start, "/start")?;
    if let OverlayEnd::At(e) = &end {
        crate::anchors::resolve_in(cx.tx.peek(), Some(cx.analysis), Some(cut), e, "/end")?;
    }
    if let Some(im) = t.image {
        cx.tx.get(im.asset)?;
    }
    let id: Id<Overlay> = cx.tx.mint();
    cx.tx.create(Overlay {
        id,
        parent: OverlayParent::Cut(cut),
        kind: t.kind,
        start,
        end,
        band: t.band.clone(),
        z: t.z,
        opacity: t.opacity,
        geom: t.geom,
        per_framing: Default::default(),
        span_mode: SpanMode::ClampToStretch,
        text: t.text.clone(),
        counter,
        role: t.role.clone(),
        number_provenance: Vec::new(),
        image: t.image,
        shape: t.shape.clone(),
        video: None,
        track: None,
        under: None,
    })?;
    Ok(id)
}

/// `#RRGGBB` + alpha → `#RRGGBBAA`.
fn with_alpha(rgb: &str, a: u8) -> String {
    format!("{}{a:02X}", rgb.to_ascii_uppercase())
}

/// A text style in the kit's face: libass `size` at a 1080 short side.
fn kit_style(
    cx: &Cx<'_>,
    face: Option<&str>,
    size: f64,
    fill: &str,
    pill: Option<(&str, f64)>,
    align: TextAlign,
) -> Result<TextStyle, CmdError> {
    let face = face.ok_or_else(|| {
        CmdError::invalid("/", "the brand kit needs a heading face (brand_kit_set)")
    })?;
    let f = library::font_of(cx.tx.peek(), face).ok_or_else(|| {
        CmdError::invalid("/", "the brand kit's face is not in the project (font_add)")
    })?;
    let u = |px: f64| px * 1000.0 / 1080.0;
    Ok(TextStyle {
        font: face.to_owned(),
        size: size * f.em_per_size * 1000.0 / 1080.0,
        fill: fill.to_owned(),
        outline: None,
        shadow: None,
        align,
        max_width: None,
        fit: TextFit::None,
        line_height: 1.0,
        letter_spacing: 0.0,
        case: TextCase::AsIs,
        pill: pill.map(|(c, o)| Pill {
            on: true,
            colour: c.to_owned(),
            opacity: o,
            radius: u(14.0),
            pad_x: u(22.0),
            pad_y: u(12.0),
            per_line: false,
        }),
    })
}

fn the_kit(cx: &mut Cx<'_>) -> Result<BrandKit, CmdError> {
    let id = cx
        .tx
        .peek()
        .iter::<BrandKit>()
        .next()
        .map(|k| k.id)
        .ok_or_else(|| CmdError::invalid("/", "the project has no brand kit (brand_kit_set)"))?;
    Ok(cx.tx.get(id)?.clone())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LowerThirdAddArgs {
    pub cut: Id<Cut>,
    pub start: Anchor,
    /// How long it stays (default 3.5 s).
    #[serde(default)]
    pub dur_ms: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddedMany {
    pub ids: Vec<String>,
}

#[derive(Debug)]
pub struct LowerThirdAdd;

#[command(
    name = "lower_third_add",
    scope = "edit",
    undo = "undoable",
    doc = "A lower third from the brand kit: name over handle on a boxed label (primary colour, accent handle) bottom-left, with the logo beside it when the kit has one. One undo step.",
    example = r#"{"cut": "01J00000000000000000000001", "start": {"cut_start": {"cut": "01J00000000000000000000001"}}}"#
)]
impl Command for LowerThirdAdd {
    type Args = LowerThirdAddArgs;
    type Output = AddedMany;
    fn run(a: LowerThirdAddArgs, cx: &mut Cx<'_>) -> Result<AddedMany, CmdError> {
        let k = the_kit(cx)?;
        if k.name.trim().is_empty() && k.handle.trim().is_empty() {
            return Err(CmdError::invalid(
                "/",
                "the brand kit has no name or handle",
            ));
        }
        let dur = a.dur_ms.unwrap_or(3500).clamp(500, 60_000);
        let style = kit_style(
            cx,
            k.heading_font.as_deref(),
            54.0,
            &with_alpha(&k.colours.text, 0xFF),
            Some((&k.colours.primary, 0.92)),
            TextAlign::Left,
        )?;
        let mut runs = vec![TextRun {
            text: k.name.clone(),
            colour: None,
            keyword: None,
        }];
        if !k.handle.trim().is_empty() {
            runs.push(TextRun {
                text: if k.name.trim().is_empty() {
                    k.handle.clone()
                } else {
                    format!("\n{}", k.handle)
                },
                colour: Some(with_alpha(&k.colours.accent, 0xFF)),
                keyword: None,
            });
        }
        // bottom-left, above IG's bottom 360 px and clear of the caption seat
        let (x, y) = (if k.logo.is_some() { 0.22 } else { 0.06 }, 0.70);
        let text = OverlayTemplate {
            kind: OverlayKind::Text,
            role: Some("note".into()),
            band: Some("lower_third".into()),
            z: 22,
            opacity: 1.0,
            geom: OverlayGeom::Place(Place { x, y, w: 0.7 }),
            dur_ms: dur,
            text: Some(OverlayText { runs, style }),
            counter: None,
            image: None,
            shape: None,
        };
        let mut ids = vec![
            insert(
                cx,
                a.cut,
                &text,
                a.start.clone(),
                OverlayEnd::DurMs(dur),
                None,
            )?
            .id,
        ];
        if let Some(l) = k.logo {
            let logo = OverlayTemplate {
                kind: OverlayKind::Image,
                role: None,
                band: Some("lower_third".into()),
                z: 22,
                opacity: 1.0,
                geom: OverlayGeom::Place(Place {
                    x: 0.06,
                    y: 0.695,
                    w: 0.14,
                }),
                dur_ms: dur,
                text: None,
                counter: None,
                image: Some(l),
                shape: None,
            };
            ids.push(insert(cx, a.cut, &logo, a.start, OverlayEnd::DurMs(dur), None)?.id);
        }
        Ok(AddedMany { ids })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndCardAddArgs {
    pub cut: Id<Cut>,
    /// The card's length at the end of the cut (default 2.5 s).
    #[serde(default)]
    pub dur_ms: Option<u32>,
    /// End on the opening layout (the loop rule, AGENTS.md "the last frame matches the
    /// opening layout"): the last part takes the first part's layout in every framing and
    /// the card is the logo and handle over the picture instead of a full card.
    #[serde(default)]
    pub loop_layout: bool,
}

#[derive(Debug)]
pub struct EndCardAdd;

#[command(
    name = "end_card_add",
    scope = "edit",
    undo = "undoable",
    doc = "An end card from the brand kit over the cut's last seconds: the kit's background, logo, name and handle; or, with loop_layout, the last part linked to the opening part's layout (the loop rule) with the logo and handle over the picture. One undo step.",
    example = r#"{"cut": "01J00000000000000000000001", "dur_ms": 2500, "loop_layout": true}"#
)]
impl Command for EndCardAdd {
    type Args = EndCardAddArgs;
    type Output = AddedMany;
    fn run(a: EndCardAddArgs, cx: &mut Cx<'_>) -> Result<AddedMany, CmdError> {
        let k = the_kit(cx)?;
        let dur = a.dur_ms.unwrap_or(2500).clamp(500, 30_000);
        let parts = cx.tx.children::<Part>(a.cut.entity());
        if parts.is_empty() {
            return Err(CmdError::invalid("/cut", "the cut has no parts"));
        }
        let start = Anchor::CutEnd(CutEdge {
            cut: a.cut,
            offset_ms: Some(-i64::from(dur)),
        });
        let end = || {
            OverlayEnd::At(Anchor::CutEnd(CutEdge {
                cut: a.cut,
                offset_ms: None,
            }))
        };
        let mut ids = Vec::new();
        let tpl = |kind, z, geom, text, image, shape| OverlayTemplate {
            kind,
            role: (kind == OverlayKind::Text).then(|| "note".into()),
            band: Some("end_card".into()),
            z,
            opacity: 1.0,
            geom,
            dur_ms: dur,
            text,
            counter: None,
            image,
            shape,
        };
        if a.loop_layout {
            // the last part (in output order) takes the first part's layout per framing
            let order = gmclip_model::clock::cut_layout(cx.tx.peek(), a.cut);
            let (Some(first), Some(last)) = (order.first(), order.last()) else {
                return Err(CmdError::invalid("/cut", "the cut has no parts"));
            };
            let (first, last) = (first.part, last.part);
            if first != last {
                let looks = cx.tx.get(first)?.look.clone();
                cx.tx.get(last)?;
                cx.tx.update(last, |p| {
                    for (f, l) in looks {
                        p.look.insert(f, l);
                    }
                })?;
            }
        } else {
            let bg = OverlayShape {
                kind: ShapeKind::Box,
                stroke: with_alpha(&k.colours.background, 0xFF),
                width: 0.0,
                fill: Some(with_alpha(&k.colours.background, 0xF2)),
                radius: 0.0,
                from: None,
                to: None,
            };
            ids.push(
                insert(
                    cx,
                    a.cut,
                    &tpl(
                        OverlayKind::Shape,
                        40,
                        OverlayGeom::Box(CanvasBox {
                            x: -0.01,
                            y: -0.01,
                            w: 1.02,
                            h: 1.02,
                        }),
                        None,
                        None,
                        Some(bg),
                    ),
                    start.clone(),
                    end(),
                    None,
                )?
                .id,
            );
        }
        if let Some(l) = k.logo {
            let (w, y) = if a.loop_layout {
                (0.16, 0.05)
            } else {
                (0.36, 0.30)
            };
            ids.push(
                insert(
                    cx,
                    a.cut,
                    &tpl(
                        OverlayKind::Image,
                        41,
                        OverlayGeom::Place(Place {
                            x: (1.0 - w) / 2.0,
                            y,
                            w,
                        }),
                        None,
                        Some(l),
                        None,
                    ),
                    start.clone(),
                    end(),
                    None,
                )?
                .id,
            );
        }
        let mut runs = Vec::new();
        if !a.loop_layout && !k.name.trim().is_empty() {
            runs.push(TextRun {
                text: k.name.clone(),
                colour: None,
                keyword: None,
            });
        }
        if !k.handle.trim().is_empty() {
            runs.push(TextRun {
                text: if runs.is_empty() {
                    k.handle.clone()
                } else {
                    format!("\n{}", k.handle)
                },
                colour: Some(with_alpha(&k.colours.accent, 0xFF)),
                keyword: None,
            });
        }
        if !runs.is_empty() {
            let style = kit_style(
                cx,
                k.heading_font.as_deref(),
                if a.loop_layout { 48.0 } else { 72.0 },
                &with_alpha(&k.colours.text, 0xFF),
                a.loop_layout
                    .then_some((k.colours.background.as_str(), 0.7)),
                TextAlign::Centre,
            )?;
            let y = if a.loop_layout { 0.16 } else { 0.62 };
            ids.push(
                insert(
                    cx,
                    a.cut,
                    &tpl(
                        OverlayKind::Text,
                        42,
                        OverlayGeom::Place(Place { x: 0.5, y, w: 0.9 }),
                        Some(OverlayText { runs, style }),
                        None,
                        None,
                    ),
                    start,
                    end(),
                    None,
                )?
                .id,
            );
        }
        if ids.is_empty() && !a.loop_layout {
            return Err(CmdError::invalid("/", "the brand kit is empty"));
        }
        Ok(AddedMany { ids })
    }
}

// ---------------------------------------------------------------------------------------
// end_card_longform_add (lane K5): a short points at its long-form
// ---------------------------------------------------------------------------------------

/// The long-form's frame for the card, a PNG the host stored by its sha256 (registered as
/// an asset in the same step, so one ⌘Z takes the card and its picture away).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CardImage {
    pub name: String,
    /// sha256 of the stored PNG (lower-case hex).
    pub sha256: String,
    pub size: u64,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndCardLongformAddArgs {
    /// The short.
    pub cut: Id<Cut>,
    /// The long-form the card names (a long-form cut of this project).
    pub longform: Id<Cut>,
    /// The title shown (default: the long-form's title).
    #[serde(default)]
    pub title: Option<String>,
    /// A frame of the long-form (default: no picture).
    #[serde(default)]
    pub image: Option<CardImage>,
    /// The card's length at the end of the short (default 3 s).
    #[serde(default)]
    pub dur_ms: Option<u32>,
}

/// Band of the long-form end card's overlays (the overlay list names it).
pub const LONGFORM_CARD_BAND: &str = "end_card_longform";

/// A title in at most two lines of about `width` characters, words kept whole, the rest
/// cut with an ellipsis.
pub fn card_title_lines(title: &str, width: usize) -> String {
    let mut lines: Vec<String> = vec![String::new()];
    let words: Vec<&str> = title.split_whitespace().collect();
    let mut cut = false;
    for (i, w) in words.iter().enumerate() {
        let cur = lines.last().map_or(0, |l| l.chars().count());
        let fits = cur == 0 || cur + 1 + w.chars().count() <= width;
        if fits {
            let l = lines.last_mut().expect("a line");
            if cur > 0 {
                l.push(' ');
            }
            l.push_str(w);
        } else if lines.len() < 2 {
            lines.push((*w).to_owned());
        } else {
            cut = i < words.len();
            break;
        }
    }
    let mut out: Vec<String> = lines
        .into_iter()
        .map(|l| {
            if l.chars().count() > width + 6 {
                let mut t: String = l.chars().take(width).collect();
                t.push('…');
                t
            } else {
                l
            }
        })
        .collect();
    if cut && let Some(l) = out.last_mut() {
        l.push('…');
    }
    out.join("\n")
}

#[derive(Debug)]
pub struct EndCardLongformAdd;

#[command(
    name = "end_card_longform_add",
    scope = "edit",
    undo = "undoable",
    doc = "End card to the long-form (a suggestion, one undo step): over the short's last seconds, \"Full video:\" with the long-form's title, a frame of it and an arrow, in the brand kit's colours and heading face (else the project's first face). Laid out for 9:16; a 16:9 framing gets its own places and no arrow.",
    example = r#"{"cut": "01J00000000000000000000001", "longform": "01J00000000000000000000002", "dur_ms": 3000}"#
)]
impl Command for EndCardLongformAdd {
    type Args = EndCardLongformAddArgs;
    type Output = AddedMany;
    fn run(a: EndCardLongformAddArgs, cx: &mut Cx<'_>) -> Result<AddedMany, CmdError> {
        let c = cx.tx.get(a.cut)?.clone();
        if !c.kind.is_short() {
            return Err(CmdError::invalid(
                "/cut",
                "an end card to the long-form goes on a short",
            ));
        }
        let lf = cx.tx.get(a.longform)?.clone();
        if lf.kind != CutKind::Long {
            return Err(CmdError::invalid("/longform", "a long-form cut"));
        }
        if cx.tx.children::<Part>(a.cut.entity()).is_empty() {
            return Err(CmdError::invalid("/cut", "the cut has no parts"));
        }
        let title = a.title.unwrap_or(lf.title);
        bounded(&title, "/title", 200)?;
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(CmdError::invalid("/title", "the long-form has no title"));
        }
        let dur = a.dur_ms.unwrap_or(3000).clamp(1000, 10_000);
        // the kit's colours and heading face; without a kit, its defaults and the
        // project's first face
        let kit = cx.tx.peek().iter::<BrandKit>().next().cloned();
        let colours = kit.as_ref().map(|k| k.colours.clone()).unwrap_or_default();
        let face = kit
            .as_ref()
            .and_then(|k| k.heading_font.clone())
            .filter(|f| library::font_of(cx.tx.peek(), f).is_some())
            .or_else(|| {
                cx.tx
                    .peek()
                    .iter::<Font>()
                    .next()
                    .map(|f| crate::planner::style::font_name(&f.family, &f.style))
            })
            .ok_or_else(|| {
                CmdError::invalid("/", "the project has no font yet (set up the brand kit)")
            })?;
        let text_fill = with_alpha(&colours.text, 0xFF);
        let accent = with_alpha(&colours.accent, 0xFF);
        let style = kit_style(cx, Some(&face), 56.0, &text_fill, None, TextAlign::Centre)?;
        // the picture, registered here (one step with the card)
        let image = match a.image {
            None => None,
            Some(im) => {
                bounded(&im.name, "/image/name", 200)?;
                if im.sha256.len() != 64
                    || !im
                        .sha256
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                {
                    return Err(CmdError::invalid(
                        "/image/sha256",
                        "64 lower-case hex characters",
                    ));
                }
                if im.size == 0
                    || im.size > 20 << 20
                    || im.w == 0
                    || im.h == 0
                    || im.w > 8192
                    || im.h > 8192
                {
                    return Err(CmdError::invalid(
                        "/image",
                        "a PNG of 1 byte – 20 MB, 1–8192 px a side",
                    ));
                }
                let have = cx
                    .tx
                    .peek()
                    .iter::<Asset>()
                    .find(|x| x.sha256 == im.sha256)
                    .map(|x| x.id);
                let asset = match have {
                    Some(x) => x,
                    None => {
                        let id: Id<Asset> = cx.tx.mint();
                        cx.tx.create(Asset {
                            id,
                            name: im.name,
                            sha256: im.sha256,
                            size: im.size,
                            media_type: "image/png".into(),
                            probe: None,
                        })?;
                        id
                    }
                };
                Some(OverlayImage {
                    asset,
                    w: im.w,
                    h: im.h,
                })
            }
        };
        let start = Anchor::CutEnd(CutEdge {
            cut: a.cut,
            offset_ms: Some(-i64::from(dur)),
        });
        let end = || {
            OverlayEnd::At(Anchor::CutEnd(CutEdge {
                cut: a.cut,
                offset_ms: None,
            }))
        };
        let tpl = |kind, z, geom, text, image, shape| OverlayTemplate {
            kind,
            role: (kind == OverlayKind::Text).then(|| "note".into()),
            band: Some(LONGFORM_CARD_BAND.into()),
            z,
            opacity: 1.0,
            geom,
            dur_ms: dur,
            text,
            counter: None,
            image,
            shape,
        };
        let mut made: Vec<Id<Overlay>> = Vec::new();
        let mut add = |cx: &mut Cx<'_>, t: OverlayTemplate| -> Result<(), CmdError> {
            made.push(insert_id(cx, a.cut, &t, start.clone(), end(), None)?);
            Ok(())
        };
        let place = |x: f64, y: f64, w: f64| OverlayGeom::Place(Place { x, y, w });
        // the panel across the middle
        let panel = OverlayShape {
            kind: ShapeKind::Box,
            stroke: with_alpha(&colours.background, 0xFF),
            width: 0.0,
            fill: Some(with_alpha(&colours.background, 0xEB)),
            radius: 28.0,
            from: None,
            to: None,
        };
        add(
            cx,
            tpl(
                OverlayKind::Shape,
                40,
                OverlayGeom::Box(CanvasBox {
                    x: 0.05,
                    y: 0.27,
                    w: 0.90,
                    h: 0.46,
                }),
                None,
                None,
                Some(panel),
            ),
        )?;
        // "Full video:" in the accent, the title under it (one text, so the lines never
        // collide in another shape)
        let runs = vec![
            TextRun {
                text: "Full video:".into(),
                colour: Some(accent.clone()),
                keyword: None,
            },
            TextRun {
                text: format!("\n{}", card_title_lines(&title, 22)),
                colour: None,
                keyword: None,
            },
        ];
        add(
            cx,
            tpl(
                OverlayKind::Text,
                42,
                place(0.5, 0.305, 0.84),
                Some(OverlayText { runs, style }),
                None,
                None,
            ),
        )?;
        if let Some(im) = image {
            // under the title, centred: 70 % of a 9:16 frame's width, at most 22 % of its
            // height (a portrait frame stays a thumbnail)
            let w = fit_width(im, (1080.0, 1920.0), 0.70, 0.22);
            add(
                cx,
                tpl(
                    OverlayKind::Image,
                    41,
                    place((1.0 - w) / 2.0, 0.45, w),
                    None,
                    Some(im),
                    None,
                ),
            )?;
        }
        let arrow = OverlayShape {
            kind: ShapeKind::Arrow,
            stroke: accent.clone(),
            width: 12.0,
            fill: None,
            radius: 0.0,
            from: Some([0.5, 0.69]),
            to: Some([0.5, 0.785]),
        };
        add(
            cx,
            tpl(
                OverlayKind::Shape,
                43,
                place(0.45, 0.69, 0.1),
                None,
                None,
                Some(arrow),
            ),
        )?;
        fit_longform_card(cx, a.cut)?;
        Ok(AddedMany {
            ids: made.iter().map(|id| id.0.to_string()).collect(),
        })
    }
}

/// An image's width (canvas fraction) inside `max_w` of the width and `max_h` of the
/// height of a `canvas` (px).
fn fit_width(im: OverlayImage, canvas: (f64, f64), max_w: f64, max_h: f64) -> f64 {
    let ar = f64::from(im.w.max(1)) / f64::from(im.h.max(1));
    max_w.min(max_h * canvas.1 * ar / canvas.0)
}

/// The card's place in a landscape framing: the frame on the left third, the text in the
/// right two thirds, the panel as it is; `None`: hidden there (the arrow).
fn wide_place(o: &Overlay) -> Option<OverlayPerFraming> {
    let at = |x: f64, y: f64, w: f64| {
        Some(OverlayPerFraming {
            visible: true,
            place: Some(Place { x, y, w }),
        })
    };
    match (o.kind, o.shape.as_ref().map(|s| s.kind)) {
        (OverlayKind::Text, _) => at(0.66, 0.40, 0.5),
        (OverlayKind::Image, _) => {
            let w = o
                .image
                .map_or(0.32, |im| fit_width(im, (1920.0, 1080.0), 0.32, 0.34));
            at(0.25 - w / 2.0, 0.33, w)
        }
        (OverlayKind::Shape, Some(ShapeKind::Box)) => Some(OverlayPerFraming {
            visible: true,
            place: None,
        }),
        _ => None,
    }
}

/// Gives the long-form card on `cut` its places in every landscape framing that has none
/// yet: how many it set.
fn fit_longform_card(cx: &mut Cx<'_>, cut: Id<Cut>) -> Result<usize, CmdError> {
    let wide: Vec<Id<Framing>> = cx
        .tx
        .children::<Framing>(cut.entity())
        .into_iter()
        .filter(|f| {
            cx.tx
                .peek()
                .get(*f)
                .is_some_and(|x| x.canvas.w > x.canvas.h)
        })
        .collect();
    let card: Vec<Id<Overlay>> = cx
        .tx
        .children::<Overlay>(cut.entity())
        .into_iter()
        .filter(|o| {
            cx.tx
                .peek()
                .get(*o)
                .is_some_and(|x| x.band.as_deref() == Some(LONGFORM_CARD_BAND))
        })
        .collect();
    let mut n = 0;
    for o in card {
        let ov = cx.tx.get(o)?.clone();
        for f in &wide {
            if ov.per_framing.contains_key(f) {
                continue;
            }
            let pf = wide_place(&ov).unwrap_or(OverlayPerFraming {
                visible: false,
                place: None,
            });
            cx.tx.update(o, |x| {
                x.per_framing.insert(*f, pf);
            })?;
            n += 1;
        }
    }
    Ok(n)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndCardLongformFitArgs {
    pub cut: Id<Cut>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Fitted {
    /// Overlay framings given a place (or hidden).
    pub set: usize,
}

#[derive(Debug)]
pub struct EndCardLongformFit;

#[command(
    name = "end_card_longform_fit",
    scope = "edit",
    undo = "undoable",
    doc = "Lay the short's end card to the long-form out in its landscape framings made after it (the frame on the left third, the words on the right, no arrow); framings already laid out are kept. One undo step; nothing when there is no card.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for EndCardLongformFit {
    type Args = EndCardLongformFitArgs;
    type Output = Fitted;
    fn run(a: EndCardLongformFitArgs, cx: &mut Cx<'_>) -> Result<Fitted, CmdError> {
        cx.tx.get(a.cut)?;
        Ok(Fitted {
            set: fit_longform_card(cx, a.cut)?,
        })
    }
}

/// Applies the kit's defaults to a new framing (caption preset, background).
pub(crate) fn kit_defaults(
    st: &mut gmclip_model::Staging<'_>,
    framing: Id<Framing>,
) -> Result<(), gmclip_model::ModelError> {
    let Some(k) = st.doc().iter::<BrandKit>().next().cloned() else {
        return Ok(());
    };
    if let Some(p) = &k.caption_preset
        && let Ok(preset) = library::resolve(st.doc(), p)
    {
        library::apply(st, framing, p, &preset)?;
    }
    if k.colours != BrandColours::default() {
        let bg = k.colours.background.clone();
        st.update(framing, |f| {
            f.background = Background::Solid { colour: bg };
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn card_titles_wrap_in_two_lines() {
        use super::card_title_lines as t;
        assert_eq!(t("Short title", 22), "Short title");
        assert_eq!(
            t("I built a PC that renders video in 67 seconds flat", 22),
            "I built a PC that\nrenders video in 67…"
        );
        assert_eq!(
            t("Supercalifragilisticexpialidocious-and-more-words", 22),
            "Supercalifragilisticex…"
        );
        assert_eq!(t("  ", 22), "");
    }

    #[test]
    fn alpha_suffix() {
        assert_eq!(super::with_alpha("#0c1118", 0xF2), "#0C1118F2");
    }
}
