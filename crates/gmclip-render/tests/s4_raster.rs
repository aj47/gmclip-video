//! B-09 / B-10 (S4): our raster of every distinct golden event, written as premultiplied tiles
//! (anchor and size in `index.tsv`) for `tools/s4_compare.py` when `GMCLIP_S4_OUT` is set.
mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use gmclip_render::text::ass::{ImportOptions, import, parse};
use gmclip_render::text::font::FontFace;
use gmclip_render::text::raster::TextRaster;

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: the golden cuts (lab media) + Neometric Heavy"
)]
fn render_every_distinct_golden_event() {
    let out = std::env::var_os("GMCLIP_S4_OUT").map(PathBuf::from);
    let mut r = TextRaster::new();
    r.add_font(
        "neometric-heavy",
        FontFace::open(&common::neometric()).unwrap(),
    )
    .unwrap();
    let mut seen = BTreeSet::new();
    let mut index = String::new();
    // GMCLIP_S4_ASS=file.ass renders that script's events instead (probes; `name` = "probe").
    let sources = match std::env::var_os("GMCLIP_S4_ASS") {
        Some(p) => vec![("probe".to_owned(), PathBuf::from(p))],
        None => common::golden_ass(),
    };
    for (name, path) in sources {
        let src = std::fs::read_to_string(&path).unwrap();
        let imp = import(&parse(&src).unwrap(), &ImportOptions::story(30)).unwrap();
        let dialogues: Vec<&str> = src.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        let mut di = 0;
        for ev in &imp.events {
            // Find this event's Dialogue line (events skip lines that show on no frame).
            let idx: usize = ev.id.as_str()[1..].parse().unwrap();
            di = di.max(idx);
            let line = dialogues[idx - 1];
            let body = line.splitn(10, ',').nth(9).unwrap();
            let key = format!("{}|{}", ev.style, body);
            if !seen.insert(key) {
                continue;
            }
            let style = &imp.styles[ev.style.as_str()];
            let tile = r
                .render(ev, style)
                .unwrap_or_else(|e| panic!("{name} {}: {e}", ev.id));
            let n = seen.len();
            if let Some(dir) = &out {
                // The tile only (premultiplied RGBA8); its canvas anchor and size are in index.tsv.
                std::fs::write(dir.join(format!("{n:04}.tile")), &tile.rgba).unwrap();
                index.push_str(&format!(
                    "{n:04}\t{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    ev.id, ev.style, tile.x, tile.y, tile.width, tile.height, body
                ));
            }
        }
    }
    if let Some(dir) = &out {
        std::fs::write(dir.join("index.tsv"), index).unwrap();
    }
    eprintln!("rendered {} distinct golden events", seen.len());
}
