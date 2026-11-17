//! A plan's captions as subtitle files (SPEC §5.10 "SRT/VTT/ASS export [M2] (ASS styles
//! approximated)"): the `caption` text events, in time order, each cue's runs joined.
//!
//! Times come from the output clock: frame `f` is `f · 1000 / fps` ms, rounded to the
//! nearest ms (SRT, VTT) or centisecond (ASS, libass's own unit).
//!
//! The ASS file keeps the look as far as ASS can say it: `PlayResX/Y` = the canvas, one
//! `Style` per text style used (font family and style, size, fill, outline, shadow, box as
//! `BorderStyle 3`, numpad alignment), each cue at its `pos` (`\pos`, `\an`), coloured runs
//! as `\c` overrides. Animation is not written (ASS has no word bounce; the export draws it).

use std::fmt::Write as _;

use crate::types::{Plan, TextEvent, TextRole};

/// A subtitle format.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Format {
    Srt,
    Vtt,
    Ass,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "srt" => Some(Format::Srt),
            "vtt" => Some(Format::Vtt),
            "ass" => Some(Format::Ass),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Srt => "srt",
            Format::Vtt => "vtt",
            Format::Ass => "ass",
        }
    }
}

/// The caption events of `plan`, by start frame (then plan order).
pub fn captions(plan: &Plan) -> Vec<&TextEvent> {
    let mut v: Vec<&TextEvent> = plan
        .text_events
        .iter()
        .flatten()
        .filter(|e| e.role == TextRole::Caption)
        .collect();
    v.sort_by_key(|e| e.out.start());
    v
}

/// Frame `frame` at `num/den` fps in ms (rounded): the whole rate, so a 24000/1001 cut's
/// frame 864 is 36.036 s, not 36 ms.
fn ms(frame: u64, (num, den): (u64, u64)) -> u64 {
    (frame * 1000 * den + num / 2) / num
}

fn clock(ms: u64, sep: char) -> String {
    let (h, m, s, r) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02}{sep}{r:03}")
}

fn ass_clock(frame: u64, (num, den): (u64, u64)) -> String {
    let cs = (frame * 100 * den + num / 2) / num;
    let (h, m, s, r) = (cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    format!("{h}:{m:02}:{s:02}.{r:02}")
}

fn plain(e: &TextEvent) -> String {
    e.runs
        .iter()
        .map(|r| r.text.as_str())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `#RRGGBBAA` → ASS `&HAABBGGRR` (ASS alpha is transparency).
fn ass_colour(c: Option<&crate::scalars::Rgba>, default: &str) -> String {
    let h = c.map_or(default, |c| c.as_str()).trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("FF"), 16).unwrap_or(255);
    let (r, g, bl, a) = (b(0), b(2), b(4), b(6));
    format!("&H{:02X}{bl:02X}{g:02X}{r:02X}", 255 - a)
}

/// The subtitle file text. Empty cues (whitespace only) are skipped.
pub fn write(plan: &Plan, format: Format) -> String {
    let fps = (plan.output.fps.num.get(), plan.output.fps.den.get());
    let caps = captions(plan);
    let mut out = String::new();
    match format {
        Format::Srt | Format::Vtt => {
            let sep = if format == Format::Srt { ',' } else { '.' };
            if format == Format::Vtt {
                out.push_str("WEBVTT\n\n");
            }
            let mut n = 0;
            for e in caps {
                let t = plain(e);
                if t.is_empty() {
                    continue;
                }
                n += 1;
                if format == Format::Srt {
                    let _ = writeln!(out, "{n}");
                }
                let _ = writeln!(
                    out,
                    "{} --> {}\n{t}\n",
                    clock(ms(e.out.start(), fps), sep),
                    clock(ms(e.out.end(), fps), sep)
                );
            }
        }
        Format::Ass => {
            let (w, h) = (plan.output.canvas.w.get(), plan.output.canvas.h.get());
            let _ = write!(
                out,
                "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nScaledBorderAndShadow: yes\nWrapStyle: 2\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
            );
            let mut used: Vec<&str> = caps.iter().map(|e| e.style.as_str()).collect();
            used.sort_unstable();
            used.dedup();
            for name in used {
                let Some(st) = plan.text_styles.as_ref().and_then(|s| s.get(name)) else {
                    continue;
                };
                let font = plan
                    .fonts
                    .iter()
                    .flatten()
                    .find(|f| f.id == st.font)
                    .map_or_else(
                        || st.font.as_str().to_owned(),
                        |f| match (&f.family, &f.style) {
                            (Some(fa), Some(sy)) if !sy.is_empty() => format!("{fa} {sy}"),
                            (Some(fa), _) => fa.clone(),
                            _ => f.id.as_str().to_owned(),
                        },
                    );
                let boxed = st.text_box.as_ref();
                let outline = st.outline.as_ref();
                let shadow = st.shadow.as_ref();
                let _ = writeln!(
                    out,
                    "Style: {name},{font},{},{},&H000000FF,{},{},0,0,0,0,100,100,{},0,{},{},{},{},0,0,0,1",
                    st.size,
                    ass_colour(st.fill.as_ref(), "#FFFFFFFF"),
                    ass_colour(outline.and_then(|o| o.colour.as_ref()), "#000000FF"),
                    match boxed {
                        Some(b) => ass_colour(b.colour.as_ref(), "#000000FF"),
                        None => ass_colour(shadow.and_then(|s| s.colour.as_ref()), "#000000FF"),
                    },
                    st.letter_spacing.unwrap_or(0.0),
                    if boxed.is_some() { 3 } else { 1 },
                    match boxed {
                        Some(b) => b.pad_x.unwrap_or(0.0),
                        None => outline.and_then(|o| o.width).unwrap_or(0.0),
                    },
                    shadow.and_then(|s| s.dx).unwrap_or(0.0),
                    st.align.unwrap_or(2),
                );
            }
            out.push_str(
                "\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            );
            for e in caps {
                if plain(e).is_empty() {
                    continue;
                }
                let style = plan.text_styles.as_ref().and_then(|s| s.get(&e.style));
                let fill = style.and_then(|s| s.fill.as_ref());
                let mut text = format!(
                    "{{\\an{}\\pos({},{})\\fs{}}}",
                    e.align.or(style.and_then(|s| s.align)).unwrap_or(2),
                    e.pos.x.round(),
                    e.pos.y.round(),
                    e.size
                );
                for r in &e.runs {
                    let t = r
                        .text
                        .replace('\\', "\\\\")
                        .replace('{', "(")
                        .replace('}', ")")
                        .replace('\n', "\\N");
                    match &r.fill {
                        Some(c) if Some(c) != fill => {
                            let colour = ass_colour(Some(c), "#FFFFFFFF");
                            let _ = write!(text, "{{\\c&H{}&}}{t}{{\\r}}", &colour[4..]);
                        }
                        _ => text.push_str(&t),
                    }
                }
                let _ = writeln!(
                    out,
                    "Dialogue: {},{},{},{},,0,0,0,,{text}",
                    e.layer.unwrap_or(0),
                    ass_clock(e.out.start(), fps),
                    ass_clock(e.out.end(), fps),
                    e.style.as_str()
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(clock(3_723_456, ','), "01:02:03,456");
        assert_eq!(ass_clock(55, (30, 1)), "0:00:01.83");
        assert_eq!(ms(1, (30, 1)), 33);
        // NTSC-style rates use the whole fraction
        assert_eq!(ms(864, (24000, 1001)), 36_036);
        assert_eq!(ass_clock(864, (24000, 1001)), "0:00:36.04");
        assert_eq!(ass_colour(None, "#FFD400FF"), "&H0000D4FF");
        assert_eq!(
            ass_colour(
                Some(&crate::scalars::Rgba::new("#101010CF").unwrap()),
                "#000000FF"
            ),
            "&H30101010"
        );
    }
}
