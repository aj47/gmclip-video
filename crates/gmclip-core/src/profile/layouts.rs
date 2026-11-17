//! A profile's stream layout presets (`stream-layouts.json`) matched to a source
//! (A2.20, SPEC §5.7.1 step 3: the face comes from the source's `cam` region). Until the
//! fixed-furniture fingerprints of "Draw your layout once" (§5.7.2, M3), a preset matches
//! a source when its canvas is the source picture's size **and** the preset names the
//! source: a distinctive word of the preset's name or note (`tg4qtpedrro`, `berigame`) is
//! a word of the source's path. Several presets naming it with the same strength match
//! none (ambiguous, said so); the canvas alone never matches, because every restream of a
//! 2560×1440 desktop has that canvas.

use serde::Deserialize;

use gmclip_model::entities::StreamLayout;

/// One preset as `stream-layouts.json` carries it.
#[derive(Clone, Debug, Deserialize)]
pub struct LayoutPreset {
    pub layout: StreamLayout,
    /// The memory note it was read from (untrusted text; only its words are used).
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    presets: Vec<LayoutPreset>,
}

/// Parses `stream-layouts.json`; `None` when it is not one.
pub fn parse(text: &str) -> Option<Vec<LayoutPreset>> {
    serde_json::from_str::<File>(text).ok().map(|f| f.presets)
}

/// Words that name no particular stream.
const GENERIC: &[&str] = &[
    "stream",
    "streams",
    "layout",
    "layouts",
    "restream",
    "native",
    "scene",
    "main",
    "facecam",
    "camera",
    "game",
    "screen",
    "source",
    "video",
    "recording",
    "from",
    "with",
    "the",
    "and",
    "notes",
    "note",
    "inset",
    "region",
    "regions",
    "desktop",
    "capture",
    "youtube",
    "local",
    "live",
];

/// Lowercase alphanumeric runs of four or more characters, minus [`GENERIC`].
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(str::to_ascii_lowercase)
        .filter(|w| !GENERIC.contains(&w.as_str()))
        .collect()
}

/// How a source matched the presets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// The preset at this index, by the words it shares with the source's path.
    One {
        index: usize,
        by: Vec<String>,
    },
    /// Presets naming the source equally (indices); none applied.
    Ambiguous(Vec<usize>),
    None,
}

/// Matches a source (its file path, its picture's `[w, h]`) against the presets.
pub fn match_source(presets: &[LayoutPreset], path: &str, dims: [u32; 2]) -> Match {
    let have = words(path);
    let mut best: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, p) in presets.iter().enumerate() {
        if [p.layout.canvas_size.w, p.layout.canvas_size.h] != dims {
            continue;
        }
        let note = p.note.trim_end_matches(".md");
        let mut by: Vec<String> = words(&p.layout.name)
            .into_iter()
            .chain(words(note))
            .filter(|w| have.contains(w))
            .collect();
        by.sort();
        by.dedup();
        if by.is_empty() {
            continue;
        }
        match best.first().map(|b| b.1.len()) {
            Some(n) if by.len() < n => {}
            Some(n) if by.len() == n => best.push((i, by)),
            _ => best = vec![(i, by)],
        }
    }
    match best.len() {
        0 => Match::None,
        1 => {
            let (index, by) = best.remove(0);
            Match::One { index, by }
        }
        _ => Match::Ambiguous(best.into_iter().map(|b| b.0).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmclip_model::entities::{Canvas, RegionRole, StreamRegion};
    use gmclip_model::geometry::NormRect;
    use gmclip_model::{Id, Ulid};

    fn preset(name: &str, note: &str, w: u32, h: u32) -> LayoutPreset {
        LayoutPreset {
            layout: StreamLayout {
                id: Id::new(Ulid(1)),
                name: name.into(),
                canvas_size: Canvas { w, h },
                fingerprint: String::new(),
                regions: vec![StreamRegion {
                    role: RegionRole::Cam,
                    rect: NormRect {
                        x: 0.0,
                        y: 0.75,
                        w: 0.2,
                        h: 0.25,
                    },
                }],
                audio_roles: None,
                default_template: None,
            },
            note: note.into(),
        }
    }

    #[test]
    fn a_preset_matches_the_source_it_names_on_its_canvas() {
        let ps = [
            preset(
                "berigame-restream-layout",
                "berigame-restream-layout.md",
                2560,
                1440,
            ),
            preset(
                "Tg4qTpEDRRo stream layout",
                "tg4qtpedrro-stream-layout.md",
                1920,
                1080,
            ),
        ];
        assert_eq!(
            match_source(
                &ps,
                "/Volumes/s/2026-09-30 BeriGame restream/source.mkv",
                [2560, 1440]
            ),
            Match::One {
                index: 0,
                by: vec!["berigame".into()]
            }
        );
        assert!(matches!(
            match_source(&ps, "/x/streams/Tg4qTpEDRRo/source.mp4", [1920, 1080]),
            Match::One { index: 1, .. }
        ));
        // the same name on another canvas, or the canvas with no name: nothing
        assert_eq!(
            match_source(&ps, "/x/streams/Tg4qTpEDRRo/source.mp4", [2560, 1440]),
            Match::None
        );
        assert_eq!(
            match_source(&ps, "/x/streams/loc_4fadb756/source.mkv", [2560, 1440]),
            Match::None
        );
        // generic words ("stream", "layout", "restream") never match by themselves
        assert_eq!(
            match_source(&ps, "/x/stream layout restream.mkv", [2560, 1440]),
            Match::None
        );
    }

    #[test]
    fn two_presets_naming_the_source_equally_match_neither() {
        let ps = [
            preset("berigame A", "", 2560, 1440),
            preset("berigame B", "", 2560, 1440),
            preset("berigame duel arena", "", 2560, 1440),
        ];
        assert_eq!(
            match_source(&ps, "/s/berigame-2026.mkv", [2560, 1440]),
            Match::Ambiguous(vec![0, 1, 2])
        );
        // more shared words wins
        assert!(matches!(
            match_source(&ps, "/s/berigame duel arena.mkv", [2560, 1440]),
            Match::One { index: 2, .. }
        ));
    }

    #[test]
    fn parses_the_profile_file() {
        let text = serde_json::json!({"schema": "gmclip.stream-layouts/0", "untrusted": true,
            "presets": [{"layout": preset("x", "", 10, 10).layout, "note": "n.md", "note_sha256": "ab", "evidence": [], "untrusted": true}]})
        .to_string();
        let ps = parse(&text).unwrap();
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].note, "n.md");
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"presets": 3}"#).is_none());
    }
}
