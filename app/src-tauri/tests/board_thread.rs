//! U.10: the review thread on the board — one per export. The notes and comments review
//! links wrote on that export (attributed `review-link:<id>`, their words kept as text)
//! and your replies, oldest first; a link note with no export goes on the cut's newest
//! export; your reply joins the export's thread (a reply to its first comment) and is
//! written as yours. An export with no record has no thread to reply on. On AJ's h79 file.

use std::path::Path;
use std::sync::Arc;

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
use serde_json::{Value, json};

fn video() -> String {
    gmclip_model::lab::media_str("h79/h79-v4.mp4")
}

fn entry(id: &str, cut: &str, file: &Path, at_ms: i64) -> ExportEntry {
    ExportEntry {
        id: id.into(),
        fps: 30,
        fps_den: 1,
        cut: cut.into(),
        title: "h79".into(),
        file: file.to_path_buf(),
        frames: 300,
        seconds: 4.0,
        render_sig: None,
        at_ms,
        notes: vec![],
        plan_sig: None,
        verdict: None,
        verify: None,
        opening_sig: None,
        opening_label: None,
        framing: None,
    }
}

#[test]
fn one_thread_per_export_with_the_links_notes_and_your_replies() {
    if !Path::new(&video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-thread-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c: Arc<AppCore> = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(&video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    std::fs::create_dir_all(root.join("exports")).unwrap();
    // an older export with no record, then the newest with its record (what a link serves)
    let old = root.join("exports/old.mp4");
    std::fs::write(&old, b"old").unwrap();
    c.add_export(&pid, entry("exp-1", &cut, &old, 1_000))
        .unwrap();
    let new = root.join("exports/new.mp4");
    std::fs::write(&new, b"new").unwrap();
    let sha = "ab12".repeat(16);
    gmclip_media::export_record::ExportRecord {
        render_sig: None,
        render_sig_absent: Some("test".into()),
        framing: None,
        preset: "story".into(),
        input_rev: None,
        duration_expected: gmclip_media::export_record::DurationExpected {
            frames: 300,
            fps_num: 30,
            fps_den: 1,
            audio_samples: 192_000,
        },
        frames_hash: None,
        file: "new.mp4".into(),
        sha256: sha.clone(),
        loudness: None,
        draft_mattes: vec![],
        notes: vec![],
        blocked: vec![],
        app_version: "test".into(),
        renderer_version: "test".into(),
    }
    .write(&new)
    .unwrap();
    c.add_export(&pid, entry("exp-2", &cut, &new, 2_000))
        .unwrap();
    let link_id = sha[..16].to_owned();

    // the links write: a note with no export, a note on the export (an XSS fixture), and
    // a comment on it
    gmclip_app::reviewlink::add_review_as(&c, "lk1", &cut, "an older note", "keep", None).unwrap();
    let xss = "<img src=x onerror=alert(1)> too jumpy";
    gmclip_app::reviewlink::add_review_as(&c, "lk2", &cut, xss, "change", Some(&link_id)).unwrap();
    let cm = gmclip_app::reviewlink::add_comment_as(
        &c,
        "lk2",
        &cut,
        "start on the result",
        Some(&link_id),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let tile = |id: &str| -> Value {
        gmclip_app::board::board(&c, &pid).unwrap()["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    let t = tile("exp-2");
    eprintln!("{}", serde_json::to_string_pretty(&t["thread"]).unwrap());
    assert_eq!(t["thread_key"], link_id.as_str());
    assert_eq!(
        t["thread_root"],
        cm.as_str(),
        "the export's first comment roots it"
    );
    let th = t["thread"].as_array().unwrap();
    let texts: Vec<&str> = th.iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["an older note", xss, "start on the result"]);
    assert!(th.iter().all(|m| m["human"] == false));
    assert_eq!(th[0]["author"], "review-link:lk1");
    assert_eq!(th[1]["kind"], "note");
    assert_eq!(th[1]["verdict"], "change");
    assert_eq!(th[2]["kind"], "comment");
    // the older export has its own (empty) thread and no key to reply under
    let o = tile("exp-1");
    assert_eq!(o["thread"], json!([]));
    assert!(o["thread_key"].is_null());

    // your reply, as the board sends it: one comment_add on the export's thread
    let rev = c.project(&pid).unwrap().handle.snapshot().doc.rev();
    let reply = "<b>thanks</b> — trimmed the start";
    c.call(
        &pid,
        "comment_add",
        json!({"cut": cut, "text": reply, "export_ref": link_id, "reply_to": cm}),
    )
    .unwrap();
    assert_eq!(
        c.project(&pid).unwrap().handle.snapshot().doc.rev(),
        rev + 1
    );
    let t = tile("exp-2");
    let th = t["thread"].as_array().unwrap();
    assert_eq!(th.len(), 4);
    assert_eq!(th[3]["text"], reply, "kept as text");
    assert_eq!(th[3]["human"], true);
    assert_eq!(th[3]["author"], "human");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
