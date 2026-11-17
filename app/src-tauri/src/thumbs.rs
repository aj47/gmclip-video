//! Thumbnails (decision 0015; backlog V.06): a cover frame from the cut's own footage and
//! a title on it, every version kept beside the cut (`Thumbnail` entities) and one picked.
//!
//! - **Pick a frame**: scrub the viewer and "Use this frame" (the cover is the source
//!   point the cut shows there, so it stays on that picture through edits).
//! - **Title**: typed, or the moment's text hook; style from the project's brand kit when
//!   it has one (read by kind name, so this works before and after the brand kit lands),
//!   else gmclip's default (white, black outline, yellow `*accent*`).
//! - **Render**: the cut's own plan, rendered at the cover frame with every caption, hook
//!   and pill taken off and the title drawn by the export's text renderer (same fonts, same
//!   compositor), saved as a PNG beside the project (`<home>/thumbnails/<cut>/<id>.png`).
//! - **Make thumbnail with my agent**: the thumbnail pass through the launcher. None of
//!   the four clients may make images inside a launched run (their image tools are
//!   switched off by the launcher's flag sets), so the agent returns a layout gmclip
//!   renders: a cover, three text-hook options, a title box. The human previews the three
//!   on the frame and picks one.

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_model::Doc;
use gmclip_model::entities::{Cut, Moment, ThumbStyle, Thumbnail};
use serde_json::{Value, json};

use crate::core::AppCore;
use crate::error::{AppError, AppResult};
use crate::planner::ulid_id;

/// The brand kit's title style, read by kind name (`brand_kit`), or `None`.
pub fn brand_style(doc: &Doc) -> Option<ThumbStyle> {
    let k = gmclip_model::kinds::kind_by_name("brand_kit")?;
    let ops = gmclip_model::kinds::kind_ops(k)?;
    let id = *ops.ids(doc).first()?;
    let v: Value = gmclip_model::cbor::from_slice(&ops.encode(doc, id)?.0).ok()?;
    let hex = |s: &Value| {
        s.as_str()
            .filter(|c| c.len() == 7 && c.starts_with('#'))
            .map(str::to_owned)
    };
    let d = gmclip_core::commands::comment::default_thumb_style();
    Some(ThumbStyle {
        font: v["heading_font"].as_str().unwrap_or_default().to_owned(),
        size: d.size,
        fill: hex(&v["colours"]["text"]).unwrap_or(d.fill),
        outline: hex(&v["colours"]["background"]).unwrap_or(d.outline),
        accent: hex(&v["colours"]["accent"]).unwrap_or(d.accent),
        uppercase: d.uppercase,
    })
}

fn text_hook(doc: &Doc, cid: gmclip_model::Id<Cut>) -> Option<String> {
    doc.get(cid)?
        .moment
        .and_then(|m| doc.get::<Moment>(m))
        .and_then(|m| m.text_hook.clone())
}

fn png_path(home: &std::path::Path, cut: &str, id: &str) -> PathBuf {
    home.join("thumbnails").join(cut).join(format!("{id}.png"))
}

/// The thumbnail panel: every version (newest first) with its cover's output frame, the
/// text hook, the style a new version takes.
pub fn view(core: &AppCore, project: &str, cut: &str) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    doc.get(cid)
        .ok_or_else(|| AppError::not_found("no such cut"))?;
    let pl = core.planned(project, cut).ok();
    let mut v: Vec<&Thumbnail> = doc
        .children::<Thumbnail>(cid.entity())
        .filter_map(|t| doc.get(t))
        .collect();
    v.sort_by_key(|t| std::cmp::Reverse((t.at_ms, t.id.0)));
    let versions: Vec<Value> = v
        .iter()
        .map(|t| {
            let id = t.id.0.to_string();
            let frame = pl
                .as_ref()
                .and_then(|pl| crate::collab::src_to_frame(pl, &doc, &t.cover));
            json!({"id": id, "title": t.title, "hook_options": t.hook_options, "style": t.style,
                   "layout": t.layout, "origin": t.origin, "author": t.author, "human": t.author == "human",
                   "picked": t.picked, "notes": t.notes, "at_ms": t.at_ms, "frame": frame,
                   "rendered": png_path(&p.home, cut, &id).is_file()})
        })
        .collect();
    let brand = brand_style(&doc);
    Ok(json!({
        "versions": versions,
        "text_hook": text_hook(&doc, cid),
        "brand": brand.is_some(),
        "style": brand.unwrap_or_else(gmclip_core::commands::comment::default_thumb_style),
        "layout": gmclip_core::commands::comment::default_thumb_layout(),
        "canvas": pl.as_ref().map(|pl| [pl.plan.output.canvas.w.get(), pl.plan.output.canvas.h.get()]),
    }))
}

/// A new version with its cover at output frame `frame`, as the human.
pub fn add(
    core: &AppCore,
    project: &str,
    cut: &str,
    frame: u64,
    title: &str,
    style: Option<&Value>,
    pick: bool,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let pl = core.planned(project, cut)?;
    let cover = crate::collab::frame_to_src(&pl, &doc, frame)
        .ok_or_else(|| AppError::invalid("no part plays at that frame"))?;
    let cid = ulid_id::<Cut>(cut)?;
    let style = match style.filter(|s| !s.is_null()) {
        Some(s) => s.clone(),
        None => serde_json::to_value(
            brand_style(&doc).unwrap_or_else(gmclip_core::commands::comment::default_thumb_style),
        )
        .unwrap_or(Value::Null),
    };
    let title = if title.trim().is_empty() {
        text_hook(&doc, cid).unwrap_or_default()
    } else {
        title.to_owned()
    };
    core.call(
        project,
        "thumbnail_add",
        json!({"cut": cut, "cover": cover, "title": title, "style": style, "pick": pick}),
    )
}

/// Moves a version's cover to output frame `frame` (the human).
pub fn cover(
    core: &AppCore,
    project: &str,
    cut: &str,
    thumbnail: &str,
    frame: u64,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let pl = core.planned(project, cut)?;
    let c = crate::collab::frame_to_src(&pl, &doc, frame)
        .ok_or_else(|| AppError::invalid("no part plays at that frame"))?;
    core.call(
        project,
        "thumbnail_set",
        json!({"thumbnail": thumbnail, "cover": c}),
    )
}

/// "Make thumbnail with my agent": the thumbnail pass on this click (no media grant), told
/// the brand style, the frame the editor is on and the golden cuts.
pub fn agent(
    core: &Arc<AppCore>,
    project: &str,
    cut: &str,
    frame: Option<u64>,
    harness: Option<&str>,
) -> AppResult<Value> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cid = ulid_id::<Cut>(cut)?;
    let cover_hint = match frame {
        Some(f) => core
            .planned(project, cut)
            .ok()
            .and_then(|pl| crate::collab::frame_to_src(&pl, &doc, f)),
        None => None,
    };
    let brief = gmclip_mcp::collab::thumbnail_brief(
        &doc,
        cid,
        &gmclip_mcp::collab::ThumbContext {
            style: Some(
                brand_style(&doc)
                    .unwrap_or_else(gmclip_core::commands::comment::default_thumb_style),
            ),
            cover_hint,
            golden: None,
        },
    )
    .map_err(AppError::invalid)?;
    // the golden cuts' specs are appended to every brief at launch (agent::start)
    let h = match harness {
        Some(h) => Some(
            gmclip_ai::launcher::Harness::parse(h)
                .ok_or_else(|| AppError::invalid("claude-code, codex, pi or opencode"))?,
        ),
        None => None,
    };
    crate::agent::run_composed(core, project, cut, "thumbnail", brief, h)
}

/// The title's lines and runs: `*accent*` words take the accent colour; upper case when
/// the style says so; greedy word wrap to `max_chars`.
pub fn title_lines(title: &str, upper: bool, max_chars: usize) -> Vec<Vec<(String, bool)>> {
    let t = if upper {
        title.to_uppercase()
    } else {
        title.to_owned()
    };
    // words with their accent flag (a `*…*` run may span words)
    let mut words: Vec<(String, bool)> = Vec::new();
    let mut accent = false;
    for w in t.split_whitespace() {
        let mut s = w.to_owned();
        let starts = s.starts_with('*');
        if starts {
            s.remove(0);
            accent = true;
        }
        let ends = s.ends_with('*');
        if ends {
            s.pop();
        }
        if !s.is_empty() {
            words.push((s, accent));
        }
        if ends {
            accent = false;
        }
    }
    let mut lines: Vec<Vec<(String, bool)>> = Vec::new();
    let mut cur: Vec<(String, bool)> = Vec::new();
    let mut len = 0usize;
    for (w, a) in words {
        let n = w.chars().count();
        if !cur.is_empty() && len + 1 + n > max_chars.max(1) {
            lines.push(std::mem::take(&mut cur));
            len = 0;
        }
        len += n + usize::from(!cur.is_empty());
        cur.push((w, a));
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The cut's plan at the cover frame with only the title as text (validated like any
/// plan), and the frame to render.
pub fn title_plan(
    core: &AppCore,
    project: &str,
    t: &Thumbnail,
) -> AppResult<(gmclip_plan::Plan, u64)> {
    let p = core.project(project)?;
    let doc = p.handle.snapshot().doc.clone();
    let cut = t.cut.0.to_string();
    let pl = core.planned(project, &cut)?;
    let frame = crate::collab::src_to_frame(&pl, &doc, &t.cover).ok_or_else(|| {
        AppError::invalid("the cover frame is no longer in the cut: pick another")
    })?;
    let (w, h) = (
        pl.plan.output.canvas.w.get() as f64,
        pl.plan.output.canvas.h.get() as f64,
    );
    let face = (!t.style.font.is_empty())
        .then(|| crate::fonts::by_name(&t.style.font))
        .flatten()
        .or_else(|| crate::fonts::by_name("Neometric Heavy"))
        .or_else(|| crate::fonts::catalogue().first())
        .ok_or_else(|| AppError::new("Unsupported", "no font to draw the title with"))?;
    let path = face
        .path
        .as_ref()
        .ok_or_else(|| AppError::new("Unsupported", "the title font's file is missing"))?;
    let mut v = serde_json::to_value(&*pl.plan).map_err(|e| AppError::internal(e.to_string()))?;
    let font_id = "thumb-title-font";
    let fonts = v["fonts"].as_array().cloned().unwrap_or_default();
    let mut fonts: Vec<Value> = fonts.into_iter().filter(|f| f["id"] != font_id).collect();
    fonts.push(json!({"id": font_id, "family": face.family, "style": face.style,
                      "path": path.to_string_lossy(), "sha256": face.sha256, "em_per_size": face.em_per_size}));
    v["fonts"] = Value::Array(fonts);
    // the title's size: em height = size × frame height; shrink until it fits the box
    let b = t.layout.title_box;
    let (bw, bh) = (b.w * w, b.h * h);
    let mut em = t.style.size * h;
    let mut lines = Vec::new();
    for _ in 0..12 {
        let per = (bw / (em * 0.56)).floor().max(1.0) as usize;
        lines = title_lines(&t.title, t.style.uppercase, per);
        let longest = lines
            .iter()
            .map(|l| l.iter().map(|(s, _)| s.chars().count() + 1).sum::<usize>())
            .max()
            .unwrap_or(0);
        if lines.len() as f64 * em * 1.1 <= bh && longest as f64 * em * 0.56 <= bw * 1.05 {
            break;
        }
        em *= 0.9;
    }
    if lines.is_empty() {
        return Err(AppError::invalid("the thumbnail has no title"));
    }
    let size = em / face.em_per_size;
    let rgba = |c: &str| format!("{c}FF");
    let mut styles = v["text_styles"].as_object().cloned().unwrap_or_default();
    styles.insert(
        "thumb-title".into(),
        json!({"font": font_id, "size": size, "fill": rgba(&t.style.fill),
               "outline": {"width": (em * 0.09).max(2.0), "colour": rgba(&t.style.outline), "join": "round"}}),
    );
    v["text_styles"] = Value::Object(styles);
    let align: u8 = match t.layout.align {
        gmclip_model::entities::TextAlign::Left => 4,
        gmclip_model::entities::TextAlign::Centre => 5,
        gmclip_model::entities::TextAlign::Right => 6,
    };
    let x = match align {
        4 => b.x * w,
        6 => (b.x + b.w) * w,
        _ => (b.x + b.w / 2.0) * w,
    };
    let n = lines.len() as f64;
    let mut events = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let y = (b.y * h) + bh / 2.0 + (k as f64 - (n - 1.0) / 2.0) * em * 1.1;
        let mut runs = Vec::new();
        for (i, (word, accent)) in line.iter().enumerate() {
            let text = if i + 1 < line.len() {
                format!("{word} ")
            } else {
                word.clone()
            };
            let mut r = json!({"text": text});
            if *accent {
                r["fill"] = json!(rgba(&t.style.accent));
            }
            runs.push(r);
        }
        events.push(
            json!({"id": format!("thumb-title-{k}"), "out": [frame, frame + 1], "role": "note",
                           "style": "thumb-title", "pos": {"x": x, "y": y}, "align": align,
                           "size": size, "runs": runs}),
        );
    }
    v["text_events"] = Value::Array(events);
    let plan = gmclip_plan::parse(&serde_json::to_vec(&v).unwrap_or_default())
        .map_err(|e| AppError::invalid(format!("the thumbnail does not plan: {e}")))?;
    Ok((plan, frame))
}

/// Renders a version to `<home>/thumbnails/<cut>/<id>.png`; returns the path and a small
/// preview (PNG, base64) the panel draws through a blob, never an `img` URL.
#[cfg(any(target_os = "macos", windows))]
pub fn render(core: &AppCore, project: &str, thumbnail: &str) -> AppResult<Value> {
    use gmclip_render::export::sheet::{RgbImage, png_bytes};
    let p = core.project(project)?;
    let tid = ulid_id::<Thumbnail>(thumbnail)?;
    let t = p
        .handle
        .snapshot()
        .doc
        .get(tid)
        .cloned()
        .ok_or_else(|| AppError::not_found("no such thumbnail"))?;
    let (plan, frame) = title_plan(core, project, &t)?;
    let mut inputs = gmclip_render::export::DemuxInputs::new(None);
    let (_, img) = gmclip_render::export::render_frames(&plan, &mut inputs, &[frame])?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::internal("no frame rendered"))?;
    let cut = t.cut.0.to_string();
    let out = png_path(&p.home, &cut, thumbnail);
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)
            .map_err(|e| AppError::internal(format!("thumbnails ({})", e.kind())))?;
    }
    crate::core::write_atomic(&out, &png_bytes(&img))
        .map_err(|e| AppError::internal(format!("thumbnail ({})", e.kind())))?;
    // a quarter-size preview, box-filtered
    let f = 4u32;
    let (pw, ph) = ((img.width / f).max(1), (img.height / f).max(1));
    let mut small = RgbImage::new(pw, ph, [0, 0, 0]);
    for y in 0..ph {
        for x in 0..pw {
            let mut acc = [0u32; 3];
            for dy in 0..f {
                for dx in 0..f {
                    let (sx, sy) = (
                        (x * f + dx).min(img.width - 1),
                        (y * f + dy).min(img.height - 1),
                    );
                    let i = ((sy * img.width + sx) * 3) as usize;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += u32::from(img.px[i + c]);
                    }
                }
            }
            let o = ((y * pw + x) * 3) as usize;
            for (c, a) in acc.iter().enumerate() {
                small.px[o + c] = (a / (f * f)) as u8;
            }
        }
    }
    Ok(
        json!({"path": out, "width": img.width, "height": img.height,
              "preview_png": b64(&png_bytes(&small)), "frame": frame}),
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn render(_core: &AppCore, _project: &str, _thumbnail: &str) -> AppResult<Value> {
    Err(AppError::new(
        "Unsupported",
        "thumbnails render on macOS and Windows",
    ))
}

/// Standard base64 (no dependency for 20 lines).
pub fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_wrap_and_accent() {
        let l = title_lines("My PC made this in *67 seconds*", true, 12);
        let flat: Vec<(String, bool)> = l.concat();
        assert_eq!(flat.last().unwrap(), &("SECONDS".to_owned(), true));
        assert_eq!(flat[0], ("MY".to_owned(), false));
        assert!(l.iter().all(
            |line| line.iter().map(|w| w.0.len() + 1).sum::<usize>() <= 13 || line.len() == 1
        ));
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }
}
