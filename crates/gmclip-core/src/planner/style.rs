//! Text styles between RenderPlan v0 (libass units, numpad alignment) and the model
//! (`TextStyle`: em sizes and lengths in short-side units, horizontal alignment).
//!
//! The conversion runs both ways — a RenderPlan seeded into a project (plan → model) and
//! the planner (model → plan) — so the two are inverses by construction. Where the model
//! is coarser than RenderPlan v0, the M0 rule below decides:
//!
//! - **Vertical anchor.** The model keeps only the horizontal alignment. A boxed label
//!   (`pill.on`) hangs from its anchor (numpad top row 7/8/9); plain text centres on it
//!   (middle row 4/5/6). That is the reference's house style: `Pill` = 7, `Hook`/`Speech` = 5.
//! - **Shadow** is always of the fill and outline (`of: fill_and_outline`), outline
//!   joins are round and text boxes use libass `BorderStyle 3` geometry (all three are
//!   the only values v0 allows or the reference writes).
//! - **Lengths** (outline width, shadow offsets and blur, letter spacing, box padding and
//!   radius) are stored in short-side units (`px × 1000 / short_side`, SPEC Conventions,
//!   Units); the em size is `size × em_per_size × 1000 / short_side`.

use gmclip_model::entities::{Outline, Pill, Shadow, TextAlign, TextCase, TextFit, TextStyle};
use gmclip_plan::consts::{LibassBs3, RoundJoin};
use gmclip_plan::scalars::{Id, Rgba};
use gmclip_plan::types as rp;

/// The font name a model style carries: `"<family> <style>"` (the reference's ASS `Fontname`).
pub fn font_name(family: &str, style: &str) -> String {
    if style.is_empty() {
        family.to_owned()
    } else {
        format!("{family} {style}")
    }
}

/// The RenderPlan font id for a font name (`Neometric Heavy` → `neometric-heavy`):
/// lower case, runs of anything but `[a-z0-9]` → one `-`, none at the ends — B-07's rule
/// (`gmclip_render::text::ass::font_id`), so imported ids round-trip.
pub fn font_id(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
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

/// The numpad alignment the M0 rule gives a model style.
pub fn numpad(align: TextAlign, boxed: bool) -> u8 {
    let col = match align {
        TextAlign::Left => 1,
        TextAlign::Centre => 2,
        TextAlign::Right => 3,
    };
    if boxed { 6 + col } else { 3 + col }
}

/// The horizontal alignment of a numpad value, if it is one.
pub fn h_align(n: u8) -> Option<TextAlign> {
    match n {
        1 | 4 | 7 => Some(TextAlign::Left),
        2 | 5 | 8 => Some(TextAlign::Centre),
        3 | 6 | 9 => Some(TextAlign::Right),
        _ => None,
    }
}

fn split_rgba(c: &str) -> Option<(String, f64)> {
    let hex = c.strip_prefix('#')?;
    if hex.len() != 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
    Some((format!("#{}", &hex[..6]), f64::from(a) / 255.0))
}

fn join_rgba(c: &str, opacity: f64) -> Result<Rgba, String> {
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgba::new(format!("{c}{a:02X}"))
}

/// RenderPlan style → model style. `size` is the libass size to store (a style's own or
/// an event's fitted one). Errors name what the M0 rule cannot hold.
pub fn to_model(
    s: &rp::TextStyle,
    size: f64,
    font_name: &str,
    em_per_size: f64,
    short_side: f64,
) -> Result<TextStyle, String> {
    let u = |v: f64| v * 1000.0 / short_side;
    let boxed = s.text_box.is_some();
    let align_n = s.align.unwrap_or(if boxed { 7 } else { 5 });
    let align = h_align(align_n).ok_or_else(|| format!("align {align_n}"))?;
    if numpad(align, boxed) != align_n {
        return Err(format!(
            "align {align_n} is not the M0 anchor for a {} style",
            if boxed { "boxed" } else { "plain" }
        ));
    }
    let outline = s.outline.as_ref().map(|o| Outline {
        w: u(o.width.unwrap_or(0.0)),
        colour: o
            .colour
            .as_ref()
            .map_or_else(|| "#000000FF".to_owned(), |c| c.as_str().to_owned()),
    });
    let shadow = match &s.shadow {
        None => None,
        Some(sh) => {
            if sh.of == Some(rp::ShadowOf::Fill) {
                return Err("a shadow of the fill only".into());
            }
            Some(Shadow {
                dx: u(sh.dx.unwrap_or(0.0)),
                dy: u(sh.dy.unwrap_or(0.0)),
                blur: u(sh.blur.unwrap_or(0.0)),
                colour: sh
                    .colour
                    .as_ref()
                    .map_or_else(|| "#000000FF".to_owned(), |c| c.as_str().to_owned()),
            })
        }
    };
    let pill = match &s.text_box {
        None => None,
        Some(b) => {
            let (colour, opacity) =
                split_rgba(b.colour.as_ref().map_or("#000000FF", |c| c.as_str()))
                    .ok_or("a text box colour")?;
            Some(Pill {
                on: true,
                colour,
                opacity,
                radius: u(b.radius.unwrap_or(0.0)),
                pad_x: u(b.pad_x.unwrap_or(0.0)),
                pad_y: u(b.pad_y.unwrap_or(0.0)),
                per_line: b.geometry.is_none(),
            })
        }
    };
    Ok(TextStyle {
        font: font_name.to_owned(),
        size: size * em_per_size * 1000.0 / short_side,
        fill: s
            .fill
            .as_ref()
            .map_or_else(|| "#FFFFFFFF".to_owned(), |c| c.as_str().to_owned()),
        outline,
        shadow,
        align,
        max_width: None,
        fit: TextFit::None,
        line_height: 1.0,
        letter_spacing: u(s.letter_spacing.unwrap_or(0.0)),
        case: TextCase::AsIs,
        pill,
    })
}

/// The libass size a model style stands for.
pub fn libass_size(s: &TextStyle, em_per_size: f64, short_side: f64) -> f64 {
    s.size * short_side / (1000.0 * em_per_size)
}

/// `text` in a style's case (RenderPlan text is final: the planner applies it).
pub fn cased(text: &str, case: TextCase) -> String {
    match case {
        TextCase::AsIs => text.to_owned(),
        TextCase::Upper => text.to_uppercase(),
        TextCase::Lower => text.to_lowercase(),
        TextCase::Title => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphanumeric() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() {
                        start = true;
                    }
                }
            }
            out
        }
    }
}

/// Model style → RenderPlan style.
pub fn to_plan(
    s: &TextStyle,
    font: &Id,
    em_per_size: f64,
    short_side: f64,
) -> Result<rp::TextStyle, String> {
    let px = |v: f64| v * short_side / 1000.0;
    let rgba = |c: &str| Rgba::new(c.to_owned());
    // case is resolved on the runs by the planner ([`cased`]); the plan carries the text
    if s.line_height != 1.0 || s.fit != TextFit::None || s.max_width.is_some() {
        return Err("line height, fit or max width (not in RenderPlan v0)".into());
    }
    Ok(rp::TextStyle {
        font: font.clone(),
        size: libass_size(s, em_per_size, short_side),
        fill: Some(rgba(&s.fill)?),
        outline: match &s.outline {
            None => None,
            Some(o) => Some(rp::Outline {
                width: Some(px(o.w)),
                colour: Some(rgba(&o.colour)?),
                join: Some(RoundJoin),
            }),
        },
        shadow: match &s.shadow {
            None => None,
            Some(sh) => Some(rp::Shadow {
                dx: Some(px(sh.dx)),
                dy: Some(px(sh.dy)),
                blur: Some(px(sh.blur)),
                colour: Some(rgba(&sh.colour)?),
                of: Some(rp::ShadowOf::FillAndOutline),
            }),
        },
        text_box: match &s.pill {
            Some(p) if p.on => Some(rp::TextBox {
                colour: Some(join_rgba(&p.colour, p.opacity)?),
                pad_x: Some(px(p.pad_x)),
                pad_y: Some(px(p.pad_y)),
                radius: Some(px(p.radius)),
                geometry: (!p.per_line).then_some(LibassBs3),
            }),
            _ => None,
        },
        letter_spacing: (s.letter_spacing != 0.0).then(|| px(s.letter_spacing)),
        align: Some(numpad(s.align, s.pill.as_ref().is_some_and(|p| p.on))),
    })
}

/// Whether two model styles are the same apart from their size.
pub fn same_but_size(a: &TextStyle, b: &TextStyle) -> bool {
    let mut b = b.clone();
    b.size = a.size;
    *a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_rule_round_trips() {
        for n in 1..=9u8 {
            let a = h_align(n).unwrap();
            let boxed = n >= 7;
            if n >= 4 {
                assert_eq!(numpad(a, boxed), n);
            }
        }
        assert_eq!(h_align(0), None);
        assert_eq!(font_id("Neometric Heavy"), "neometric-heavy");
        assert_eq!(font_id(" Neo  Metric (Bold) "), "neo-metric-bold");
        assert_eq!(font_name("Neometric", "Heavy"), "Neometric Heavy");
    }

    /// A per-line box (the TikTok hook) plans with no libass geometry, so the raster draws
    /// one rounded box per line, and reads back the same; a libass box keeps BS3.
    #[test]
    fn per_line_boxes_plan_without_libass_geometry() {
        let st = |per_line: bool| TextStyle {
            font: "Neometric Heavy".into(),
            size: 60.0,
            fill: "#000000FF".into(),
            outline: None,
            shadow: None,
            align: TextAlign::Centre,
            max_width: None,
            fit: TextFit::None,
            line_height: 1.0,
            letter_spacing: 0.0,
            case: TextCase::AsIs,
            pill: Some(Pill {
                on: true,
                colour: "#FFFFFF".into(),
                opacity: 1.0,
                radius: 15.0,
                pad_x: 24.0,
                pad_y: 11.0,
                per_line,
            }),
        };
        let id = Id::new("neometric-heavy".to_owned()).unwrap();
        for per_line in [true, false] {
            let p = to_plan(&st(per_line), &id, 0.8, 1080.0).unwrap();
            let b = p.text_box.as_ref().unwrap();
            assert_eq!(b.geometry.is_none(), per_line);
            let back = to_model(&p, p.size, "Neometric Heavy", 0.8, 1080.0).unwrap();
            assert_eq!(back.pill.unwrap().per_line, per_line);
        }
    }

    #[test]
    fn rgba_splits_and_joins() {
        let (c, a) = split_rgba("#101010CF").unwrap();
        assert_eq!(c, "#101010");
        assert_eq!(join_rgba(&c, a).unwrap().as_str(), "#101010CF");
        assert!(split_rgba("#101010").is_none());
    }
}
