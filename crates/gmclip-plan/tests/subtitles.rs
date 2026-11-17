//! SRT/VTT/ASS of a plan's captions (SPEC §5.10): the RP §2.3 example's caption cues, on
//! the output clock, with the ASS keeping canvas, styles, positions and coloured runs.

mod common;

use gmclip_plan::subtitles::{Format, captions, write};

#[test]
fn h79_captions_as_srt_vtt_and_ass() {
    let plan = common::validate(&common::h79()).unwrap();
    let n = captions(&plan).len();
    assert!(n > 5);
    let srt = write(&plan, Format::Srt);
    // c01 is frames [55, 67) at 30 fps: 1.833 s → 2.233 s
    assert!(
        srt.starts_with("1\n00:00:01,833 --> 00:00:02,233\nwe were able\n\n2\n00:00:02,233 --> "),
        "{srt}"
    );
    assert_eq!(srt.matches(" --> ").count(), n);
    assert!(!srt.contains("67 seconds"), "the hook is not a caption");
    let vtt = write(&plan, Format::Vtt);
    assert!(
        vtt.starts_with("WEBVTT\n\n00:00:01.833 --> 00:00:02.233\nwe were able\n"),
        "{vtt}"
    );
    let ass = write(&plan, Format::Ass);
    assert!(ass.contains("PlayResX: 1080\nPlayResY: 1920"));
    assert!(ass.contains("\nStyle: Speech,"), "{ass}");
    assert!(
        ass.contains("Dialogue: 1,0:00:01.83,0:00:02.23,Speech,,0,0,0,,{\\an"),
        "{ass}"
    );
    assert_eq!(ass.matches("\nDialogue: ").count(), n);
    // a keyword run keeps its colour (RT yellow &H00D4FF&)
    if plan
        .text_events
        .iter()
        .flatten()
        .any(|e| e.runs.iter().any(|r| r.fill.is_some()))
    {
        assert!(
            ass.contains("\\c&H00D4FF&") || ass.contains("{\\c&H"),
            "{ass}"
        );
    }
    assert_eq!(Format::parse("vtt"), Some(Format::Vtt));
    assert_eq!(Format::parse("txt"), None);
}
