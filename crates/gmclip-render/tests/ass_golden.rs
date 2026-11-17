//! B-07 acceptance: all 11 golden ASS files import; event count, text and size equal the source.
mod common;

use gmclip_render::text::ass::{ImportOptions, display_frame, import, parse};
use gmclip_render::text::event_text;

/// RT's `plain()`: override blocks removed; `\N` → newline.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut depth = false;
    for c in text.chars() {
        match c {
            '{' => depth = true,
            '}' => depth = false,
            c if !depth => out.push(c),
            _ => {}
        }
    }
    out.replace("\\N", "\n")
}

fn fs_of(text: &str) -> Option<f64> {
    let i = text.find("\\fs")?;
    let digits: String = text[i + 3..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: reads the golden cuts (lab media)"
)]
fn all_golden_ass_import() {
    let mut total = 0;
    let mut late = 0;
    for (name, path) in common::golden_ass() {
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let script = parse(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
        let imp =
            import(&script, &ImportOptions::story(30)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let lines: Vec<&str> = src.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(
            script.dialogues.len(),
            lines.len(),
            "{name}: parsed Dialogue count"
        );
        // One h79 pill ("GENERATING 0:30", 32.87-32.90 s) falls between two frame times and
        // shows on no frame under libass either; it is reported, not imported (RP rule 1).
        let skipped = if name.ends_with("h79") {
            vec!["Dialogue 65"]
        } else {
            vec![]
        };
        let got: Vec<&str> = imp
            .report
            .iter()
            .map(|r| r.split(':').next().unwrap())
            .collect();
        assert_eq!(got, skipped, "{name}: {:?}", imp.report);
        assert_eq!(
            imp.events.len() + skipped.len(),
            lines.len(),
            "{name}: event count"
        );
        let shown = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| !skipped.contains(&format!("Dialogue {}", i + 1).as_str()));
        for (ev, (_, line)) in imp.events.iter().zip(shown) {
            let f: Vec<&str> = line.splitn(10, ',').collect();
            assert_eq!(event_text(ev), plain(f[9]), "{name} {}", ev.id);
            let style_size = imp.styles[f[3]].size;
            assert_eq!(
                ev.size,
                fs_of(f[9]).unwrap_or(style_size),
                "{name} {} size",
                ev.id
            );
            assert!(ev.out.0[0] < ev.out.0[1], "{name} {}", ev.id);
        }
        assert_eq!(imp.fonts.len(), 1, "{name}");
        // Edges where RT's shipped rule (decision 0003) differs from RP v0's ⌈0.3·cs⌉.
        for d in &script.dialogues {
            for cs in [d.start_cs, d.end_cs] {
                late += usize::from(display_frame(cs, 30) != Some((cs * 30).div_ceil(100)));
            }
        }
        total += imp.events.len();
    }
    eprintln!("{late} Dialogue edges are one frame after ⌈0.3·cs⌉ (ffmpeg's truncated ms)");
    assert_eq!(late, 20, "decision 0003: 20 edges in the golden ASS");
    eprintln!(
        "imported {total} events from 11 golden ASS files (649 Dialogue lines, 1 shows on no frame)"
    );
    assert_eq!(total, 648);
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: reads the golden cuts (lab media)"
)]
fn h79_frames_match_rp_2_3() {
    let (_, path) = common::golden_ass().pop().unwrap();
    let imp = import(
        &parse(&std::fs::read_to_string(path).unwrap()).unwrap(),
        &ImportOptions::story(30),
    )
    .unwrap();
    let got: Vec<(String, [u64; 2])> = imp
        .events
        .iter()
        .take(13)
        .map(|e| (event_text(e), e.out.0))
        .collect();
    let want = [
        ("My PC made this\nin 67 seconds.", [0, 177]),
        ("we were able", [55, 67]),
        ("to generate that", [67, 87]),
        ("five-second video", [87, 111]),
        ("in less than", [111, 147]),
        ("two minutes.", [147, 177]),
        ("I mean this", [186, 202]),
        ("is gonna be", [202, 214]),
        ("a terrible prompt,", [214, 243]),
        ("but let's", [243, 258]),
        ("just say", [258, 270]),
        ("a cartoon banana", [270, 348]),
        ("peeling itself.", [349, 397]),
    ];
    for ((t, o), (wt, wo)) in got.iter().zip(want) {
        assert_eq!((t.as_str(), *o), (wt, wo));
    }
    let pill = imp
        .events
        .iter()
        .find(|e| e.style.as_str() == "Pill")
        .unwrap();
    assert_eq!(pill.out.0[0], 177);
}
