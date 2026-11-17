//! Decision 0040 lab: the retakes found in real transcripts, printed for a human to
//! judge (`GMCLIP_SCRIPT_LAB=<transcript_read pages as JSON>`, `--ignored`).

use gmclip_core::script::{PartScript, ScriptWord, retakes};
use gmclip_model::entities::MediaRef;
use gmclip_model::{Id, Ulid};

#[test]
#[ignore = "lab: reads a transcript dump named by GMCLIP_SCRIPT_LAB"]
fn retakes_in_a_real_transcript() {
    let Some(path) = std::env::var_os("GMCLIP_SCRIPT_LAB") else {
        return;
    };
    let pages: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut words = Vec::new();
    for pg in &pages {
        for l in pg["lines"].as_array().unwrap() {
            for w in l["words"].as_array().unwrap() {
                words.push(ScriptWord {
                    ordinal: w[0].as_u64().unwrap() as u32,
                    text: w[1].as_str().unwrap().to_owned(),
                    src_start: w[2].as_i64().unwrap(),
                    src_end: w[3].as_i64().unwrap(),
                    spoken_end: w[3].as_i64().unwrap(),
                    at: w[2].as_i64().unwrap(),
                });
            }
        }
    }
    let end = words.last().map_or(0, |w| w.src_end);
    let p = PartScript {
        index: 0,
        part: Id::new(Ulid::from_parts(0, 0)),
        media: MediaRef::Source(Id::new(Ulid::from_parts(0, 0))),
        owner: "s".into(),
        start: 0,
        dur: end,
        src_in: 0,
        src_out: end,
        transcript: true,
        no_audio: false,
        words: words.clone(),
        lead: 0,
        trail: 0,
        gaps: vec![],
        starts_mid_sentence: false,
        ends_mid_sentence: false,
        cuts_word_in: None,
        cuts_word_out: None,
    };
    let r = retakes(std::slice::from_ref(&p));
    for x in &r {
        println!(
            "{:?} @{:.1}s  STRIKE «{}»  KEEP «{}»",
            x.kind,
            x.at as f64 / 705_600_000.0,
            x.earlier,
            x.later
        );
    }
    let kept: Vec<&str> = words
        .iter()
        .filter(|w| {
            !r.iter().any(|x| {
                x.spans
                    .iter()
                    .any(|(_, a, b)| w.src_start >= *a && w.src_start < *b)
            })
        })
        .map(|w| w.text.as_str())
        .collect();
    println!("\nKEPT: {}", kept.join(" "));
}
