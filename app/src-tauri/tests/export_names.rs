//! QA (milestones/m2/qa-e2e.md, W1/W2): an export's file says which cut and, when the cut
//! has several framings, which framing it is ("face big", 9:16 / 16:9); a title cut short
//! to a file name ends on a whole word, never on a space or a dot.

use gmclip_app::core::safe_name;
use gmclip_app::jobs::export_stem;
use serde_json::json;

#[test]
fn one_framing_is_the_title_alone() {
    let v = json!({"title": "My PC made this", "framing": {"active": "f1", "list": [{"id": "f1", "name": "9:16"}]}});
    assert_eq!(export_stem(&v), "My PC made this");
}

#[test]
fn several_framings_name_the_one_exported() {
    let list = json!([{"id": "f1", "name": "9:16"}, {"id": "f2", "name": "face big"}, {"id": "f3", "name": "16:9"}]);
    let v = |a: &str| json!({"title": "The demo", "framing": {"active": a, "list": list}});
    assert_eq!(export_stem(&v("f1")), "The demo - 9x16");
    assert_eq!(export_stem(&v("f2")), "The demo - face big");
    assert_eq!(export_stem(&v("f3")), "The demo - 16x9");
}

#[test]
fn a_long_title_ends_on_a_word() {
    let t = "GTA 6. Weep weep weep weep weep. my job, A I took my job, A I made a movie, and the song, so that";
    let s = safe_name(t);
    assert!(s.chars().count() <= 80, "{s}");
    assert!(
        !s.ends_with(' ') && !s.ends_with('.') && !s.ends_with('-'),
        "{s:?}"
    );
    assert!(
        t.replace(',', "-").starts_with(&s),
        "a prefix of whole words: {s:?}"
    );
    assert!(
        t.replace(',', "-")[s.len()..].starts_with(' '),
        "cut at a word edge: {s:?}"
    );
    assert_eq!(
        safe_name("the song: so that yeah, it's trained on. "),
        "the song- so that yeah- it-s trained on"
    );
    assert_eq!(safe_name("..."), "untitled");
}
