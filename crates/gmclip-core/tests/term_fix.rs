//! Ideas 2026-10-16 (K.02): "Fix everywhere" from a caption word. One undoable step
//! gives every cut's captions the term's spelling (a project correction) and adds the
//! heard form to the term list the gate and the correction pass read.

mod common;

use common::{SEC, fixture, open};
use gmclip_model::entities::{CaptionCorrections, Terms};
use serde_json::{Value, json};

fn words(p: &common::Project, owner: &str, ws: &[(&str, f64, f64)]) {
    let rev = "asr-test@1";
    let words: Vec<Value> = ws
        .iter()
        .map(|(t, s, e)| json!({"text": t, "start": (s * SEC as f64) as i64, "end": (e * SEC as f64) as i64}))
        .collect();
    let body = json!({"schema": "gmclip.analysis.words/0", "revision": rev, "words": words});
    p.h.put_analysis(owner, "words", rev, body.to_string().into_bytes())
        .unwrap();
}

#[test]
fn fix_everywhere_corrects_every_caption_and_lists_the_term_in_one_step() {
    let p = open("term-fix");
    let fx = fixture(&p, 2);
    words(
        &p,
        &fx.source,
        &[
            ("I", 100.0, 100.2),
            ("use", 100.3, 100.5),
            ("XD.dev,", 100.6, 101.0),
            ("then", 101.1, 101.3),
            ("XD", 103.0, 103.3),
            ("dev", 103.4, 103.7),
            ("again.", 103.8, 104.1),
        ],
    );
    let r = p
        .human("term_fix", json!({"heard": "XD.dev", "written": "exe.dev"}))
        .output;
    assert_eq!(r["found"], 2, "said as one word and as two");
    assert_eq!(r["variant_added"], true);
    let d = p.h.snapshot().doc.clone();
    let c = d.iter::<CaptionCorrections>().next().unwrap();
    assert_eq!(c.entries.get("XD.dev").map(String::as_str), Some("exe.dev"));
    let t = d.iter::<Terms>().next().unwrap();
    let term = t.entries.get("exe.dev").unwrap();
    assert_eq!(term.written, "exe.dev");
    assert_eq!(term.heard_as, ["XD.dev"]);

    // a second variant joins the same term; the same one again adds nothing
    let r = p
        .human(
            "term_fix",
            json!({"heard": "x e dev", "written": "exe.dev"}),
        )
        .output;
    assert_eq!(r["variant_added"], true);
    let r = p
        .human("term_fix", json!({"heard": "xd.DEV", "written": "exe.dev"}))
        .output;
    assert_eq!(r["variant_added"], false);
    // a case fix of the term itself is a correction, not a variant
    let r = p
        .human(
            "term_fix",
            json!({"heard": "Exe.dev", "written": "exe.dev"}),
        )
        .output;
    assert_eq!(r["variant_added"], false);
    let d = p.h.snapshot().doc.clone();
    let term = d.iter::<Terms>().next().unwrap().entries["exe.dev"].clone();
    assert_eq!(term.heard_as, ["XD.dev", "x e dev"]);

    // the gate and the correction pass read the same list: a new transcript is spelt
    let mut heard = gmclip_core::analysis::Transcript {
        revision: "r".into(),
        words: vec![
            gmclip_core::analysis::Word {
                text: "on".into(),
                start: 0,
                end: 10,
                confidence: None,
            },
            gmclip_core::analysis::Word {
                text: "XD.dev.".into(),
                start: 20,
                end: 30,
                confidence: None,
            },
        ],
        lines: Vec::new(),
    };
    let terms: Vec<_> = d
        .iter::<Terms>()
        .flat_map(|t| t.entries.values().cloned())
        .collect();
    assert_eq!(
        gmclip_core::terms::correct_transcript(&mut heard, &terms),
        1
    );
    assert_eq!(heard.words[1].text, "exe.dev.");
    assert_eq!(heard.lines.len(), 1);

    // one undo per fix: undoing the first leaves nothing behind
    for _ in 0..4 {
        p.human("undo", json!({}));
    }
    let d = p.h.snapshot().doc.clone();
    assert!(
        d.iter::<Terms>()
            .next()
            .is_none_or(|t| t.entries.is_empty())
    );
    assert!(
        d.iter::<CaptionCorrections>()
            .next()
            .is_none_or(|c| c.entries.is_empty())
    );
}

#[test]
fn fix_everywhere_refuses_what_is_not_a_name() {
    let p = open("term-fix-bad");
    for bad in [
        json!({"heard": "", "written": "exe.dev"}),
        json!({"heard": "a b c d e", "written": "exe.dev"}),
        json!({"heard": "XD.dev", "written": "  "}),
        json!({"heard": "...", "written": "exe.dev"}),
    ] {
        assert!(p.try_human("term_fix", bad.clone()).is_err(), "{bad}");
    }
}
