//! B-08: RT's Pillow measure reproduced bit for bit, against the `tools/s4_pillow_widths.py`
//! table (`captions/pillow-widths.tsv`) of every golden hook, caption and pill line.
mod common;

use gmclip_render::captions::pillow::PillowMeasure;
use gmclip_render::text::font::FontFace;

#[test]
#[cfg_attr(not(feature = "lab"), ignore = "lab: needs Neometric Heavy")]
fn every_golden_line_matches_pillow() {
    let face = FontFace::open(&common::neometric()).unwrap();
    let m = PillowMeasure::new(&face).unwrap();
    let table = include_str!("../captions/pillow-widths.tsv");
    let (mut n, mut bad) = (0, Vec::new());
    for line in table.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.splitn(5, '\t').collect();
        let (size, want, text) = (
            f[2].parse::<u32>().unwrap(),
            f[3].parse::<f64>().unwrap(),
            unescape(f[4]),
        );
        let got = m.getlength(&text, size);
        n += 1;
        if got.to_bits() != want.to_bits() {
            bad.push(format!("{size} {text:?}: pillow {want} rust {got}"));
        }
    }
    eprintln!("{n} widths compared, {} differ", bad.len());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The table's `\uXXXX` escapes (a few golden lines are kept escaped).
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\\u") {
        out.push_str(&rest[..i]);
        let hex = &rest[i + 2..i + 6];
        out.push(char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap());
        rest = &rest[i + 6..];
    }
    out.push_str(rest);
    out
}
