//! R-CAP-1 (M0 form), B-08: from each reel's `plan.json` + `qa/heard.json`, `story_v5`
//! rebuilds `captions-v5.ass`: cue text identical on 100 % of cues, start/end within ±1
//! output frame (count at exactly 0 reported), hook and pill events identical, and every
//! `\fs` equal to the golden's.
mod common;

use gmclip_render::captions::pillow::PillowMeasure;
use gmclip_render::captions::story_v5::{StoryPlan, build, heard_words};
use gmclip_render::text::ass::{ImportOptions, import, parse};
use gmclip_render::text::event_text;
use gmclip_render::text::font::FontFace;

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: the golden cuts (lab media) + Neometric Heavy"
)]
fn rebuilds_every_golden_cue_list() {
    let face = FontFace::open(&common::neometric()).unwrap();
    let m = PillowMeasure::new(&face).unwrap();
    let (mut cues, mut exact, mut events, mut identical_files) = (0, 0, 0, 0);
    let mut fails = Vec::new();
    for hook in common::REELS {
        let dir = common::reel_dir(hook);
        let plan =
            StoryPlan::from_json(&std::fs::read_to_string(dir.join("plan.json")).unwrap()).unwrap();
        let words =
            heard_words(&std::fs::read_to_string(dir.join("qa/heard.json")).unwrap()).unwrap();
        let built = build(&plan, &words, &m).unwrap();
        let ours = built.to_ass();
        let golden = std::fs::read_to_string(dir.join("captions-v5.ass")).unwrap();
        if ours == golden {
            identical_files += 1;
        }
        let gl: Vec<&str> = golden
            .lines()
            .filter(|l| l.starts_with("Dialogue:"))
            .collect();
        let ol: Vec<String> = built.lines.iter().map(ToString::to_string).collect();
        events += gl.len();
        if gl.len() != ol.len() {
            fails.push(format!("{hook}: {} events, golden {}", ol.len(), gl.len()));
        }
        // Compare per event: text + tags exact; displayed frames within ±1.
        let (gi, oi) = (
            import(&parse(&golden).unwrap(), &ImportOptions::story(30)).unwrap(),
            import(&parse(&ours).unwrap(), &ImportOptions::story(30)).unwrap(),
        );
        for (g, o) in gi.events.iter().zip(&oi.events) {
            if g.role == gmclip_render::text::TextRole::Caption {
                cues += 1;
            }
            let d = (
                o.out.0[0] as i64 - g.out.0[0] as i64,
                o.out.0[1] as i64 - g.out.0[1] as i64,
            );
            if (
                event_text(g),
                &g.runs,
                g.size,
                g.pos,
                g.align,
                g.style.as_str(),
            ) != (
                event_text(o),
                &o.runs,
                o.size,
                o.pos,
                o.align,
                o.style.as_str(),
            ) || d.0.abs() > 1
                || d.1.abs() > 1
                || (g.role != gmclip_render::text::TextRole::Caption && d != (0, 0))
            {
                fails.push(format!(
                    "{hook} {}: golden {:?} {:?} ours {:?} {:?}",
                    g.id,
                    event_text(g),
                    g.out,
                    event_text(o),
                    o.out
                ));
            } else if d == (0, 0) && g.role == gmclip_render::text::TextRole::Caption {
                exact += 1;
            }
        }
        for (g, o) in gl.iter().zip(&ol) {
            if *g != o {
                eprintln!("{hook} line differs:\n  golden {g}\n  ours   {o}");
            }
        }
    }
    eprintln!(
        "R-CAP-1: {cues} cues, {exact} with start and end at exactly 0 frames; {events} events; {identical_files}/10 ASS files byte-identical"
    );
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    assert_eq!(
        (identical_files, exact, cues),
        (10, 508, 508),
        "byte identity and exact timing"
    );
}

/// B-08: the `\fs` (or style size) of every hook, cue and pill event in all 11 golden ASS
/// files equals RT's fit of its text at the nominal size (hook 88 at 1000 px, caption 86 at
/// 1000, pill 50 at 950), measured with the Rust Pillow port.
#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: the golden cuts (lab media) + Neometric Heavy"
)]
fn every_golden_size_is_rts_fit() {
    let face = FontFace::open(&common::neometric()).unwrap();
    let m = PillowMeasure::new(&face).unwrap();
    let (mut n, mut shrunk, mut files) = (0, 0, 0);
    for (name, path) in common::golden_ass() {
        let src = std::fs::read_to_string(&path).unwrap();
        let imp = import(&parse(&src).unwrap(), &ImportOptions::story(30)).unwrap();
        for ev in &imp.events {
            let (nominal, max) = match ev.style.as_str() {
                "Hook" => (imp.styles["Hook"].size as u32, 1000),
                "Speech" => (86, 1000),
                "Pill" => (50, 950),
                s => panic!("{name}: style {s}"),
            };
            // fit_size reads RT's markup text; `\N` is the only line break it knows.
            let fit = m.fit_size(&event_text(ev).replace('\n', "\\N"), nominal, max);
            assert_eq!(
                f64::from(fit),
                ev.size,
                "{name} {} {:?}",
                ev.id,
                event_text(ev)
            );
            n += 1;
            shrunk += usize::from(fit != nominal);
        }
        files += 1;
    }
    eprintln!("fit sizes: {n} events in {files}/11 files equal RT's fit ({shrunk} shrunk)");
    assert_eq!(files, 11);
}
