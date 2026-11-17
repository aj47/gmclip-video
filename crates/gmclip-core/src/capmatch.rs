//! Match a caption style from a short (AJ 2026-10-15: "match a caption style from a short
//! I drop in"). An approximation from the picture alone — no OCR, no audio: frames of the
//! short are searched for burned-in caption lines (bright or coloured strokes next to
//! strong edges, in a band of rows), and the line's look is measured: where it sits,
//! how tall it is, how thick its strokes are (weight), the fill and a second keyword
//! colour, the outline's width, a box behind it, how many words a cue shows and how long
//! a cue stays up. [`CaptionMatch::preset`] turns that into a caption preset for the
//! gallery; [`CaptionMatch::notes`] says what could not be told.
//!
//! Pure and deterministic: the host supplies RGB frames.

use gmclip_model::entities::*;

/// One RGB picture (rows of `width × 3` bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbFrame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// What one frame's caption line measured (frame px).
#[derive(Clone, Debug, PartialEq)]
struct Line {
    y0: u32,
    y1: u32,
    x0: u32,
    x1: u32,
    lines: u32,
    words: u32,
    stroke: f64,
    outline: f64,
    fill: Vec<[u8; 3]>,
    pill: Option<[u8; 3]>,
    /// Column occupancy in 32 bins (what the line says, roughly).
    sig: u32,
}

/// The measured look (sizes in px at a 1080 short side).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionMatch {
    pub frames: usize,
    pub with_text: usize,
    /// Centre of the caption block, as a share of the frame height.
    pub y: f64,
    /// The text line's height (ascender to descender), px at a 1080 short side.
    pub line_px: f64,
    /// The face size it implies (libass size at a 1080 short side).
    pub size: f64,
    /// Stroke width over line height: ≥ 0.2 heavy, ≥ 0.14 bold, else regular.
    pub stroke_ratio: f64,
    pub weight: &'static str,
    pub fill: [u8; 3],
    pub keyword: Option<[u8; 3]>,
    /// Outline width, px at a 1080 short side (0: none).
    pub outline: f64,
    pub pill: Option<[u8; 3]>,
    pub lines: f64,
    pub words_per_cue: f64,
    /// Seconds a cue stays up, when the frames show cues changing.
    pub cue_s: Option<f64>,
    pub notes: Vec<String>,
}

fn luma(p: &[u8]) -> u8 {
    ((u32::from(p[0]) * 54 + u32::from(p[1]) * 183 + u32::from(p[2]) * 19) >> 8) as u8
}

fn sat(p: &[u8]) -> u8 {
    p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])
}

/// Glyph fill: bright, or coloured and not dark.
fn is_fill(p: &[u8]) -> bool {
    let l = luma(p);
    l >= 175 || (sat(p) >= 80 && l >= 100)
}

/// The caption line of one frame, if there is one.
fn line_of(f: &RgbFrame) -> Option<Line> {
    let (w, h) = (f.width as usize, f.height as usize);
    if w < 32 || h < 32 || f.rgb.len() != w * h * 3 {
        return None;
    }
    let px = |x: usize, y: usize| &f.rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
    let lum: Vec<u8> = (0..w * h).map(|k| luma(&f.rgb[k * 3..k * 3 + 3])).collect();
    // a fill pixel next to a strong edge (strokes are thin; a bright area's inside is not)
    let r = (w / 135).clamp(2, 6);
    let edge = |x: usize, y: usize| {
        let l = i32::from(lum[y * w + x]);
        let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        (y0..=y1).any(|yy| (x0..=x1).any(|xx| l - i32::from(lum[yy * w + xx]) >= 90))
    };
    let (ya, yb) = (h / 5, h * 19 / 20);
    let mut fill = vec![false; w * h];
    let mut rows = vec![0usize; h];
    for y in ya..yb {
        for x in 0..w {
            if is_fill(px(x, y)) && edge(x, y) {
                fill[y * w + x] = true;
                rows[y] += 1;
            }
        }
    }
    // runs of rows with text, merged across small gaps
    let need = (w / 80).max(6);
    let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // (y0, y1, pixels)
    let mut y = ya;
    while y < yb {
        if rows[y] < need {
            y += 1;
            continue;
        }
        let y0 = y;
        let mut total = 0;
        let mut gap = 0;
        let mut y1 = y;
        while y < yb && gap <= 2 {
            if rows[y] >= need {
                total += rows[y];
                y1 = y;
                gap = 0;
            } else {
                gap += 1;
            }
            y += 1;
        }
        runs.push((y0, y1, total));
    }
    let (min_h, max_h) = ((h as f64 * 0.012) as usize, (h as f64 * 0.09) as usize);
    let best = runs
        .iter()
        .filter(|r| {
            let lh = r.1 - r.0 + 1;
            lh >= min_h.max(4) && lh <= max_h
        })
        .max_by_key(|r| r.2)
        .copied()?;
    let lh = best.1 - best.0 + 1;
    // a second line just above or below, as tall
    let second = runs.iter().find(|r| {
        let rh = r.1 - r.0 + 1;
        *r != &best
            && rh * 10 >= lh * 7
            && rh * 10 <= lh * 13
            && (r.0 > best.1 && r.0 - best.1 <= lh / 2 || best.0 > r.1 && best.0 - r.1 <= lh / 2)
    });
    let (y0, y1) = match second {
        Some(s) => (best.0.min(s.0), best.1.max(s.1)),
        None => (best.0, best.1),
    };
    // columns, words and the extent
    let cols: Vec<bool> = (0..w)
        .map(|x| (best.0..=best.1).any(|y| fill[y * w + x]))
        .collect();
    let x0 = cols.iter().position(|c| *c)?;
    let x1 = cols.iter().rposition(|c| *c)?;
    let word_gap = (lh as f64 * 0.28).max(2.0) as usize;
    let mut words = 1;
    let mut gap = 0;
    for c in &cols[x0..=x1] {
        if *c {
            if gap >= word_gap {
                words += 1;
            }
            gap = 0;
        } else {
            gap += 1;
        }
    }
    // strokes, outline and colours along the line's rows
    let mut strokes = Vec::new();
    let mut outlines = Vec::new();
    let mut colours = Vec::new();
    for yy in best.0..=best.1 {
        let mut x = x0;
        while x <= x1 {
            if !fill[yy * w + x] {
                x += 1;
                continue;
            }
            let s = x;
            while x <= x1 && fill[yy * w + x] {
                x += 1;
            }
            let len = x - s;
            strokes.push(len);
            if len >= 3 {
                colours.extend((s + 1..x - 1).map(|xx| {
                    let p = px(xx, yy);
                    [p[0], p[1], p[2]]
                }));
            }
            // dark pixels right after the run (past an anti-aliased pixel or two): the
            // outline
            let soft = (x..(x + 2).min(w))
                .take_while(|&xx| lum[yy * w + xx] > 90)
                .count();
            let dark = (x + soft..(x + soft + 24).min(w))
                .take_while(|&xx| lum[yy * w + xx] <= 90)
                .count();
            outlines.push(if dark > 0 { dark + soft } else { 0 });
        }
    }
    if strokes.is_empty() || colours.is_empty() {
        return None;
    }
    strokes.sort_unstable();
    outlines.sort_unstable();
    let stroke = strokes[strokes.len() / 2] as f64;
    let outline = outlines[outlines.len() / 2] as f64;
    // a box behind the line: the ring around it is one flat colour, unlike above it
    let pad = (lh / 3).max(2);
    let (bx0, bx1) = (x0.saturating_sub(pad), (x1 + pad).min(w - 1));
    let (by0, by1) = (y0.saturating_sub(pad), (y1 + pad).min(h - 1));
    let mut ring: Vec<[u8; 3]> = Vec::new();
    for yy in by0..=by1 {
        for xx in bx0..=bx1 {
            let inside = yy >= y0 && yy <= y1 && xx >= x0 && xx <= x1;
            if !inside && !fill[yy * w + xx] && lum[yy * w + xx] > 70 {
                let p = px(xx, yy);
                ring.push([p[0], p[1], p[2]]);
            }
        }
    }
    let pill = (ring.len() > 32)
        .then(|| {
            let m = mean3(&ring);
            let var = ring
                .iter()
                .map(|p| (f64::from(luma(p)) - f64::from(luma(&m))).powi(2))
                .sum::<f64>()
                / ring.len() as f64;
            let above: Vec<u8> = (by0.saturating_sub(pad * 2)..by0)
                .flat_map(|yy| (bx0..=bx1).map(move |xx| (xx, yy)))
                .map(|(xx, yy)| lum[yy * w + xx])
                .collect();
            let am = above.iter().map(|v| f64::from(*v)).sum::<f64>() / above.len().max(1) as f64;
            (var.sqrt() < 14.0 && (f64::from(luma(&m)) - am).abs() >= 25.0).then_some(m)
        })
        .flatten();
    let mut sig = 0u32;
    for b in 0..32 {
        let (c0, c1) = (b * w / 32, ((b + 1) * w / 32).max(b * w / 32 + 1));
        if cols[c0..c1].iter().any(|c| *c) {
            sig |= 1 << b;
        }
    }
    Some(Line {
        y0: y0 as u32,
        y1: y1 as u32,
        x0: x0 as u32,
        x1: x1 as u32,
        lines: if second.is_some() { 2 } else { 1 },
        words: words as u32,
        stroke,
        outline,
        fill: colours,
        pill,
        sig,
    })
}

fn mean3(v: &[[u8; 3]]) -> [u8; 3] {
    let n = v.len().max(1) as u64;
    let s = v.iter().fold([0u64; 3], |a, p| {
        [
            a[0] + u64::from(p[0]),
            a[1] + u64::from(p[1]),
            a[2] + u64::from(p[2]),
        ]
    });
    [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Measures the caption look of `frames` (sampled `fps` frames a second, consecutive).
/// `None` when no frame shows a caption line.
pub fn analyse(frames: &[RgbFrame], fps: f64) -> Option<CaptionMatch> {
    let found: Vec<(usize, Line)> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| line_of(f).map(|l| (i, l)))
        .collect();
    // a burned-in caption is on most frames of a short; a stray title is not
    if found.len() < 2 || found.len() * 4 < frames.len() {
        return None;
    }
    let f0 = &frames[found[0].0];
    let (fw, fh) = (f64::from(f0.width), f64::from(f0.height));
    let k = 1080.0 / fw.min(fh);
    let line_h: Vec<f64> = found
        .iter()
        .map(|(_, l)| f64::from(l.y1 - l.y0 + 1) / f64::from(l.lines))
        .collect();
    let lh = median(line_h);
    let stroke = median(found.iter().map(|(_, l)| l.stroke).collect());
    let ratio = if lh > 0.0 { stroke / lh } else { 0.0 };
    let weight = if ratio >= 0.2 {
        "heavy"
    } else if ratio >= 0.14 {
        "bold"
    } else {
        "regular"
    };
    let outline = median(found.iter().map(|(_, l)| l.outline).collect());
    // colours: the plain fill, and a coloured minority (keywords)
    let all: Vec<[u8; 3]> = found
        .iter()
        .flat_map(|(_, l)| l.fill.iter().copied())
        .collect();
    let (coloured, plain): (Vec<[u8; 3]>, Vec<[u8; 3]>) =
        all.iter().partition(|p| sat(&p[..]) >= 80);
    let (fill, keyword) = if plain.len() >= coloured.len() {
        // keywords: a coloured share in at least three frames
        let frames_with = found
            .iter()
            .filter(|(_, l)| {
                l.fill.iter().filter(|p| sat(&p[..]) >= 80).count() * 10 >= l.fill.len()
            })
            .count();
        let kw =
            (coloured.len() * 100 >= all.len() * 2 && frames_with >= 3).then(|| mean3(&coloured));
        (mean3(&plain), kw)
    } else {
        (mean3(&coloured), None)
    };
    let pills = found.iter().filter(|(_, l)| l.pill.is_some()).count();
    let pill = (pills * 2 > found.len())
        .then(|| mean3(&found.iter().filter_map(|(_, l)| l.pill).collect::<Vec<_>>()));
    let y = median(
        found
            .iter()
            .map(|(_, l)| f64::from(l.y0 + l.y1) / 2.0 / fh)
            .collect(),
    );
    let words = median(found.iter().map(|(_, l)| f64::from(l.words)).collect());
    let lines = median(found.iter().map(|(_, l)| f64::from(l.lines)).collect());
    // cue changes: consecutive frames with a line whose words moved
    let mut changes = 0;
    let mut pairs = 0;
    for w in found.windows(2) {
        let ((ia, a), (ib, b)) = (&w[0], &w[1]);
        if ib - ia != 1 {
            continue;
        }
        pairs += 1;
        let moved = (a.sig ^ b.sig).count_ones() >= 3
            || a.x0.abs_diff(b.x0) * 20 > f0.width
            || a.x1.abs_diff(b.x1) * 20 > f0.width;
        changes += usize::from(moved);
    }
    let cue_s = (changes > 0 && pairs >= 4).then(|| pairs as f64 / fps / changes as f64);
    let mut notes = vec![
        "measured from the picture: no OCR and no audio, so the face and the case are not matched"
            .to_owned(),
    ];
    if cue_s.is_none() {
        notes.push("cue timing not seen (the frames did not show cues changing)".into());
    }
    if pill.is_none() && pills > 0 {
        notes.push("a box behind the text in some frames only: left off".into());
    }
    Some(CaptionMatch {
        frames: frames.len(),
        with_text: found.len(),
        y,
        line_px: lh * k,
        // a caption line's ink is mostly cap and ascender height, about 0.7 em
        size: (lh * k / 0.7).clamp(30.0, 200.0),
        stroke_ratio: ratio,
        weight,
        fill,
        keyword,
        outline: outline * k,
        pill,
        lines,
        words_per_cue: words,
        cue_s,
        notes,
    })
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}FF", c[0], c[1], c[2])
}

impl CaptionMatch {
    /// The grouping whose cue size is closest to what the short shows.
    pub fn grouping(&self) -> CaptionGrouping {
        let short_cues = self.cue_s.is_some_and(|s| s < 0.45);
        if self.words_per_cue <= 1.3 || short_cues && self.words_per_cue <= 2.0 {
            CaptionGrouping::SingleWord
        } else if self.words_per_cue <= 3.4 {
            CaptionGrouping::AdaptiveWords
        } else if self.words_per_cue <= 5.5 {
            CaptionGrouping::Phrase
        } else {
            CaptionGrouping::ReadablePhrase
        }
    }

    /// A caption preset close to the short's captions, in `font` (with `em_per_size`).
    pub fn preset(&self, font: &str, em_per_size: f64) -> CaptionPreset {
        let u = |px: f64| px * 1000.0 / 1080.0;
        let outline = (self.outline >= 1.0).then(|| Outline {
            w: u(self.outline.min(24.0)),
            colour: "#000000FF".into(),
        });
        CaptionPreset {
            style: TextStyle {
                font: font.to_owned(),
                size: self.size * em_per_size * 1000.0 / 1080.0,
                fill: hex(self.fill),
                shadow: outline.as_ref().map(|o| Shadow {
                    dx: o.w * 0.5,
                    dy: o.w * 0.5,
                    blur: 0.0,
                    colour: "#000000FF".into(),
                }),
                outline,
                align: TextAlign::Centre,
                max_width: None,
                fit: TextFit::None,
                line_height: 1.0,
                letter_spacing: 0.0,
                case: TextCase::AsIs,
                pill: self.pill.map(|c| Pill {
                    on: true,
                    colour: format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
                    opacity: 0.85,
                    radius: u(18.0),
                    pad_x: u(18.0),
                    pad_y: u(8.0),
                    per_line: false,
                }),
            },
            grouping: self.grouping(),
            look: CaptionLook {
                animation: CaptionAnimation::None,
                keyword_animation: KeywordAnimation::None,
                keyword_colour: self.keyword.map(hex),
                auto_emphasis: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 540;
    const H: u32 = 960;

    /// A frame: a mid-grey busy background with a caption line of `words` "words" (each
    /// a row of thick white letters with a black outline) centred at `cy`; the `kw`-th
    /// word yellow.
    fn frame(
        words: usize,
        cy: u32,
        letter_h: u32,
        stroke: u32,
        outline: u32,
        kw: Option<usize>,
        seed: u32,
    ) -> RgbFrame {
        let mut rgb = vec![0u8; (W * H * 3) as usize];
        for y in 0..H {
            for x in 0..W {
                let v = (90 + ((x * 7 + y * 13 + seed * 31) % 40)) as u8;
                let k = ((y * W + x) * 3) as usize;
                rgb[k] = v;
                rgb[k + 1] = v;
                rgb[k + 2] = v.saturating_sub(10);
            }
        }
        let letter_w = letter_h * 3 / 5;
        let gap = letter_w / 3;
        let space = letter_h * 2 / 3;
        let word_w = 3 * letter_w + 2 * gap;
        let total = words as u32 * word_w + (words as u32 - 1) * space;
        let x0 = (W - total) / 2;
        let y0 = cy - letter_h / 2;
        let mut put = |x: u32, y: u32, c: [u8; 3]| {
            let k = ((y * W + x) * 3) as usize;
            rgb[k..k + 3].copy_from_slice(&c);
        };
        for wi in 0..words {
            let colour = if kw == Some(wi) {
                [255, 212, 0]
            } else {
                [255, 255, 255]
            };
            for li in 0..3 {
                let lx = x0 + wi as u32 * (word_w + space) + li * (letter_w + gap);
                // a letter: two vertical strokes and a crossbar ("H"), outlined
                let on = |x: u32, y: u32| {
                    let (dx, dy) = (x.wrapping_sub(lx), y.wrapping_sub(y0));
                    dx < letter_w
                        && dy < letter_h
                        && (dx < stroke
                            || dx >= letter_w - stroke
                            || dy.abs_diff(letter_h / 2) < stroke / 2)
                };
                for y in y0.saturating_sub(outline + 1)..y0 + letter_h + outline + 1 {
                    for x in lx.saturating_sub(outline + 1)..lx + letter_w + outline + 1 {
                        if on(x, y) {
                            put(x, y, colour);
                        } else if (0..=outline).any(|d| {
                            on(x + d, y)
                                || on(x.wrapping_sub(d), y)
                                || on(x, y + d)
                                || on(x, y.wrapping_sub(d))
                        }) {
                            put(x, y, [0, 0, 0]);
                        }
                    }
                }
            }
        }
        RgbFrame {
            width: W,
            height: H,
            rgb,
        }
    }

    #[test]
    fn a_two_word_outlined_caption_with_a_yellow_keyword() {
        // cues of 2 words change every 2 frames (0.5 s at 4 fps), at 55 % of the height
        let mut frames = Vec::new();
        for i in 0..12u32 {
            let words = 2;
            let kw = (i / 2 % 3 == 0).then_some(1);
            let cy = 528;
            let mut f = frame(words, cy, 30, 7, 3, kw, i / 2);
            if i / 2 % 2 == 1 {
                // a different cue: shift the words so the line changes
                f = frame(3, cy, 30, 7, 3, kw, i / 2);
            }
            frames.push(f);
        }
        let m = analyse(&frames, 4.0).expect("captions found");
        assert_eq!(m.with_text, 12);
        assert!((m.y - 0.55).abs() < 0.02, "{m:?}");
        // 30 px letters on a 540 short side: 60 px at 1080
        assert!((m.line_px - 60.0).abs() < 8.0, "{m:?}");
        assert_eq!(m.weight, "heavy", "{m:?}");
        assert!(m.fill.iter().all(|c| *c >= 240), "{m:?}");
        let kw = m.keyword.expect("a yellow keyword");
        assert!(kw[0] > 200 && kw[1] > 170 && kw[2] < 80, "{kw:?}");
        // a 3 px outline at 540: 6 px at 1080
        assert!((m.outline - 6.0).abs() <= 2.5, "{m:?}");
        assert!(m.words_per_cue >= 2.0 && m.words_per_cue <= 3.0, "{m:?}");
        assert_eq!(m.grouping(), CaptionGrouping::AdaptiveWords);
        let cue = m.cue_s.expect("cue changes seen");
        assert!((cue - 0.5).abs() < 0.2, "{cue}");
        let p = m.preset("Figtree Black", 1.0);
        assert!(p.style.outline.is_some());
        assert_eq!(
            p.look.keyword_colour.as_deref().map(|s| &s[..3]),
            Some("#FF")
        );
    }

    #[test]
    fn single_words_and_no_captions() {
        let frames: Vec<RgbFrame> = (0..8).map(|i| frame(1, 700, 40, 9, 4, None, i)).collect();
        let m = analyse(&frames, 4.0).unwrap();
        assert!(m.words_per_cue <= 1.3, "{m:?}");
        assert_eq!(m.grouping(), CaptionGrouping::SingleWord);
        // plain background: nothing
        let blank: Vec<RgbFrame> = (0..8)
            .map(|i| {
                let mut f = frame(1, 700, 40, 9, 4, None, i);
                for (k, v) in f.rgb.iter_mut().enumerate() {
                    *v = (90 + (k % 30)) as u8;
                }
                f
            })
            .collect();
        assert!(analyse(&blank, 4.0).is_none());
    }
}
