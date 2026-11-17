//! B-09: 50 text-raster fixtures with the bundled OFL face (Figtree Black), hashed (FNV-1a 64
//! over anchor, size and premultiplied bytes) and compared with the committed table, so the
//! same bytes must come out on Apple M1, the M4 Pro, x86 macOS and x86 Linux CI.
//!
//! Re-bless (only with a reason): `GMCLIP_BLESS=1 cargo test -p gmclip-render --test text_raster_hashes`.

use std::path::PathBuf;

use gmclip_plan::scalars::{Id, Range, Rgba};
use gmclip_plan::types::{
    Outline, Point, Run, Shadow, ShadowOf, TextBox, TextEvent, TextRole, TextStyle,
};
use gmclip_render::text::font::FontFace;
use gmclip_render::text::raster::TextRaster;

const TEXTS: [&str; 10] = [
    "Hello",
    "My PC made this\nin 67 seconds.",
    "fi fl ffi AVATAR",
    "Ünïcödé çà",
    "  spaced  ",
    "1:07",
    "GENERATING  0:23",
    "a",
    "WAVY Yo To",
    "two lines\nhere",
];

fn rgba(s: &str) -> Option<Rgba> {
    Some(Rgba::new(s).unwrap())
}

fn style(kind: usize) -> TextStyle {
    let base = TextStyle {
        font: Id::new("figtree-black").unwrap(),
        size: 86.0,
        fill: rgba("#FFFFFFFF"),
        outline: None,
        shadow: None,
        text_box: None,
        letter_spacing: None,
        align: Some(5),
    };
    match kind {
        // RT caption: outline 12, hard shadow 7/7 of fill ∪ outline.
        0 => TextStyle {
            outline: Some(Outline {
                width: Some(12.0),
                colour: rgba("#000000FF"),
                join: None,
            }),
            shadow: Some(Shadow {
                dx: Some(7.0),
                dy: Some(7.0),
                blur: Some(0.0),
                colour: rgba("#000000FF"),
                of: Some(ShadowOf::FillAndOutline),
            }),
            ..base
        },
        // RT pill: BS3 box, spacing 1, top-left.
        1 => TextStyle {
            text_box: Some(TextBox {
                colour: rgba("#101010CF"),
                pad_x: Some(16.0),
                pad_y: Some(16.0),
                radius: Some(0.0),
                geometry: None,
            }),
            letter_spacing: Some(1.0),
            align: Some(7),
            ..base
        },
        // Fill only (alignment varies per fixture).
        2 => TextStyle {
            fill: rgba("#3DDC84FF"),
            ..base
        },
        // Blurred, translucent shadow of the fill, thin outline.
        3 => TextStyle {
            outline: Some(Outline {
                width: Some(4.5),
                colour: rgba("#202020FF"),
                join: None,
            }),
            shadow: Some(Shadow {
                dx: Some(3.25),
                dy: Some(5.5),
                blur: Some(3.0),
                colour: rgba("#00000099"),
                of: Some(ShadowOf::Fill),
            }),
            ..base
        },
        // SPEC §5.10 caption pill: rounded box, 0.28 opacity, uneven padding.
        _ => TextStyle {
            text_box: Some(TextBox {
                colour: rgba("#00000047"),
                pad_x: Some(18.0),
                pad_y: Some(8.0),
                radius: Some(18.0),
                geometry: None,
            }),
            letter_spacing: Some(-0.5),
            ..base
        },
    }
}

fn event(i: usize) -> TextEvent {
    let text = TEXTS[i % TEXTS.len()];
    let runs = if i.is_multiple_of(3) && text.len() > 2 {
        let cut = text
            .char_indices()
            .nth(text.chars().count() / 2)
            .map_or(0, |(b, _)| b);
        vec![
            Run {
                text: text[..cut].to_owned(),
                fill: None,
            },
            Run {
                text: text[cut..].to_owned(),
                fill: rgba("#FFD400FF"),
            },
        ]
    } else {
        vec![Run {
            text: text.to_owned(),
            fill: None,
        }]
    };
    TextEvent {
        id: Id::new(format!("f{i:02}")).unwrap(),
        out: Range([0, 30]),
        layer: None,
        role: TextRole::Note,
        style: Id::new("s").unwrap(),
        pos: Point {
            x: 540.0 + i as f64 * 0.37,
            y: 960.25 + i as f64 * 1.13,
        },
        align: (i / 10 == 2).then_some(1 + (i % 9) as u8),
        size: 40.0 + ((i * 7) % 60) as f64,
        runs,
        anim: None,
        provenance: None,
    }
}

fn table_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/text_raster_figtree.tsv")
}

#[test]
fn fifty_fixtures_hash_identically() {
    let font = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/figtree/Figtree-Black.ttf");
    let mut r = TextRaster::new();
    r.add_font("figtree-black", FontFace::open(&font).unwrap())
        .unwrap();
    let mut got =
        String::from("# fixture\tx\ty\twidth\theight\tfnv1a64 (B-09; bless with GMCLIP_BLESS=1)\n");
    for i in 0..50_usize {
        r.kerning = i.is_multiple_of(7);
        let tile = r
            .render(&event(i), &style(i / 10))
            .unwrap_or_else(|e| panic!("fixture {i}: {e}"));
        assert_eq!(tile.rgba.len(), (tile.width * tile.height * 4) as usize);
        assert!(
            tile.rgba.chunks_exact(4).any(|p| p[3] > 0) || TEXTS[i % 10].trim().is_empty(),
            "fixture {i} is blank"
        );
        if let Some(dir) = std::env::var_os("GMCLIP_DUMP") {
            let name = format!("f{i:02}_{}x{}.tile", tile.width, tile.height);
            std::fs::write(PathBuf::from(dir).join(name), &tile.rgba).unwrap();
        }
        got.push_str(&format!(
            "f{i:02}\t{}\t{}\t{}\t{}\t{:016x}\n",
            tile.x,
            tile.y,
            tile.width,
            tile.height,
            tile.fnv1a()
        ));
    }
    if std::env::var_os("GMCLIP_BLESS").is_some() {
        std::fs::create_dir_all(table_path().parent().unwrap()).unwrap();
        std::fs::write(table_path(), &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(table_path()).expect("committed hash table");
    let diff: Vec<String> = got
        .lines()
        .zip(want.lines())
        .filter(|(g, w)| g != w)
        .map(|(g, w)| format!("  got  {g}\n  want {w}"))
        .collect();
    assert!(
        diff.is_empty() && got.lines().count() == want.lines().count(),
        "{} of 50 tiles differ:\n{}",
        diff.len(),
        diff.join("\n")
    );
}

#[test]
fn same_event_twice_is_byte_identical() {
    let font = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/figtree/Figtree-Black.ttf");
    let mut a = TextRaster::new();
    a.add_font("figtree-black", FontFace::open(&font).unwrap())
        .unwrap();
    let mut b = TextRaster::new();
    b.add_font("figtree-black", FontFace::open(&font).unwrap())
        .unwrap();
    for i in [1, 11, 31, 41] {
        assert_eq!(
            a.render(&event(i), &style(i / 10)).unwrap(),
            b.render(&event(i), &style(i / 10)).unwrap()
        );
    }
}
