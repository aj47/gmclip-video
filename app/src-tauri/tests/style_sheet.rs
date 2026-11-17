//! A.05 (R-PROF-3 M2 sheet, R-PROF-5): the style rules file is picked in the native panel
//! and pinned only through the Owner sheet, read confined (no symlink, ≤ 256 KB); an edit
//! on disk is batched onto one sheet at launch or batch start, and until the human accepts
//! it there, agents keep the old text and hash. And A.04's "Add to my style feedback": a
//! human command that writes the profile's log, with hidden characters shown first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gmclip_app::core::{AppCore, Dirs};

static SHEETS: AtomicUsize = AtomicUsize::new(0);
static ANSWER: Mutex<bool> = Mutex::new(true);
static TITLES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PICKED: Mutex<Option<PathBuf>> = Mutex::new(None);

fn setup(tag: &str) -> (Arc<AppCore>, PathBuf) {
    let root = std::env::temp_dir().join(format!("gmclip-style-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.agents.start(
        &c,
        gmclip_mcp::paths::Env {
            app_data: root.join("app"),
            socket: root.join("s").join("s"),
        },
    );
    gmclip_app::sheet::install(Box::new(|title, _, _| {
        SHEETS.fetch_add(1, Ordering::SeqCst);
        TITLES.lock().unwrap().push(title);
        *ANSWER.lock().unwrap()
    }));
    gmclip_app::owner::install_style_picker(Box::new(|| PICKED.lock().unwrap().clone()));
    (c, root)
}

fn served(c: &AppCore) -> (String, String) {
    let h = c.agents.holder().unwrap().help("style").unwrap();
    let st = &h["content"]["style"];
    let text = st["text"].to_string();
    (st["sha256"].as_str().unwrap().to_owned(), text)
}

fn pick(p: &Path) {
    *PICKED.lock().unwrap() = Some(p.to_owned());
}

#[test]
fn style_file_pinned_through_the_sheet_and_edits_wait_for_it() {
    let (c, root) = setup("pin");
    let f = root.join("AGENTS.md");
    std::fs::write(&f, "# Style\nOpen on the result.\nCaptions on.\n").unwrap();
    let before = SHEETS.load(Ordering::SeqCst);

    // cancelled panel: nothing asked, nothing pinned
    *PICKED.lock().unwrap() = None;
    assert_eq!(
        gmclip_app::owner::style_pick(&c).unwrap()["cancelled"],
        true
    );
    assert_eq!(SHEETS.load(Ordering::SeqCst), before);

    // a symlink and an oversized file are refused before any sheet
    #[cfg(unix)]
    {
        let link = root.join("link.md");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        pick(&link);
        assert!(gmclip_app::owner::style_pick(&c).is_err(), "a symlink");
    }
    let big = root.join("big.md");
    std::fs::write(&big, vec![b'a'; 300 * 1024]).unwrap();
    pick(&big);
    assert!(gmclip_app::owner::style_pick(&c).is_err(), "over 256 KB");
    assert_eq!(
        SHEETS.load(Ordering::SeqCst),
        before,
        "no sheet for a refused file"
    );

    // the sheet says no: not pinned
    *ANSWER.lock().unwrap() = false;
    pick(&f);
    assert!(gmclip_app::owner::style_pick(&c).is_err());
    assert!(gmclip_core::profile::style::pinned(&root.join("app")).is_none());
    // yes: pinned to exactly that version
    *ANSWER.lock().unwrap() = true;
    let r = gmclip_app::owner::style_pick(&c).unwrap();
    let (old_sha, old_text) = served(&c);
    assert_eq!(
        r["output"]["sha256"].as_str().or(r["sha256"].as_str()),
        Some(old_sha.as_str()),
        "{r}"
    );
    assert!(old_text.contains("Open on the result"));
    let after_pin = SHEETS.load(Ordering::SeqCst);

    // nothing changed: the launch sheet does not ask
    assert_eq!(
        gmclip_app::owner::style_changes_sheet(&c, false).unwrap()["pending"],
        false
    );
    assert_eq!(SHEETS.load(Ordering::SeqCst), after_pin);

    // AJ edits the file on disk (twice): agents keep the old text and hash
    std::fs::write(
        &f,
        "# Style\nOpen on the result.\nCaptions on, 1–3 words.\n",
    )
    .unwrap();
    std::fs::write(
        &f,
        "# Style\nOpen on the result.\nCaptions on, 1–3 words.\n\nPills orient.\n",
    )
    .unwrap();
    assert_eq!(served(&c), (old_sha.clone(), old_text.clone()));

    // launch: one sheet with the diff; declined → still the old version, and not asked
    // again this launch
    *ANSWER.lock().unwrap() = false;
    let r = gmclip_app::owner::style_changes_sheet(&c, false).unwrap();
    assert_eq!(r["accepted"], false, "{r}");
    assert_eq!(SHEETS.load(Ordering::SeqCst), after_pin + 1);
    let title = TITLES.lock().unwrap().last().cloned().unwrap();
    assert!(title.contains("to AGENTS.md since"), "{title}");
    assert_eq!(served(&c).0, old_sha);
    let r = gmclip_app::owner::style_changes_sheet(&c, false).unwrap();
    assert_eq!(r["asked"], false, "batch start after a decline: {r}");
    assert_eq!(SHEETS.load(Ordering::SeqCst), after_pin + 1);

    // Settings' "Review changes…": accepted on the sheet → agents get the new text
    *ANSWER.lock().unwrap() = true;
    let r = gmclip_app::owner::style_changes_sheet(&c, true).unwrap();
    assert_eq!(r["accepted"], true, "{r}");
    let (new_sha, new_text) = served(&c);
    assert_ne!(new_sha, old_sha);
    assert!(new_text.contains("Pills orient"));
    assert_eq!(
        gmclip_app::owner::settings(&c)["style"]["changed_on_disk"],
        false
    );
    // the style file itself was never written
    assert!(
        std::fs::read_to_string(&f)
            .unwrap()
            .ends_with("Pills orient.\n")
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn style_feedback_is_added_by_the_human_with_hidden_characters_shown() {
    let (c, root) = setup("fb");
    // no profile yet: refused
    assert!(gmclip_app::board::feedback_preview(&c, "x").is_err());
    let prof = root.join("profile");
    std::fs::create_dir_all(&prof).unwrap();
    gmclip_mcp::settings::OwnerSettings {
        profile: Some(prof.clone()),
        ..gmclip_mcp::settings::OwnerSettings::load(&root.join("app"))
    }
    .save(&root.join("app"))
    .unwrap();
    let text = "less jumpy\u{E0041}\u{202E}";
    let p = gmclip_app::board::feedback_preview(&c, text).unwrap();
    assert_eq!(p["hidden_characters"], true);
    assert!(
        p["shown"].as_str().unwrap().contains("⟦U+E0041⟧⟦U+202E⟧"),
        "{p}"
    );
    gmclip_app::board::feedback_append(&c, "keep the clock", Some("e1"), None).unwrap();
    gmclip_app::board::feedback_append(&c, "too long", None, Some("link-1")).unwrap();
    assert!(gmclip_app::board::feedback_append(&c, "x", None, Some("../x")).is_err());
    let recs = gmclip_core::profile::feedback::last(&prof, 10);
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].via, "app");
    assert_eq!(recs[0].export.as_deref(), Some("e1"));
    assert_eq!(recs[1].via, "review-link:link-1");
    assert_eq!(recs[0].date.len(), 10);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
