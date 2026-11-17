//! Crash recovery, R-MVP-1 "data loss = any op shown Saved absent after reopen: zero"
//! (SPEC §14; lane S). A child process runs the app core on a project and edits as fast
//! as it can (each edit a new cut with a numbered title); whenever the app would show
//! **Saved** (`project_save_state`: everything committed is power-durable, no error) it
//! prints the titles that are now Saved. The parent `kill -9`s it at a random moment —
//! often mid-edit, with edits still "Saving…" — reopens the project with a fresh core and
//! requires every title ever printed Saved to be there. Several rounds continue the same
//! project.
//!
//! `GMCLIP_CRASH_ROUNDS` (default 6) sets the rounds.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gmclip_app::core::{AppCore, Dirs};
use gmclip_model::entities::Cut;
use serde_json::json;

const CHILD: &str = "GMCLIP_CRASH_CHILD";

fn core_at(root: &Path) -> std::sync::Arc<AppCore> {
    AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    })
}

fn home(root: &Path) -> PathBuf {
    root.join("lib").join("Crash.gmclip")
}

/// The child: open (or create) the project and edit until killed.
#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(root);
    let c = core_at(&root);
    let pid = if home(&root).is_dir() {
        c.open_home(&home(&root)).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        c.create_project("Crash").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let p = c.project(&pid).unwrap();
    let start = p.handle.snapshot().doc.rev();
    // (rev, title) not yet printed Saved
    let mut waiting: Vec<(u64, String)> = Vec::new();
    let out = std::io::stdout();
    println!("OPEN {start}");
    // bursts of edits with short pauses, as a person edits: the app shows Saved in the
    // pauses (the checkpoint debounce is 250 ms), and the kill lands anywhere
    let mut n = 0u64;
    for burst in 0u64.. {
        for _ in 0..=(burst * 7 % 13) {
            let title = format!("edit {start}-{n}");
            n += 1;
            let r = c.call(&pid, "cut_create", json!({"title": title})).unwrap();
            let rev = r["committed"]["rev"]
                .as_u64()
                .unwrap_or_else(|| p.handle.snapshot().doc.rev());
            waiting.push((rev, title));
        }
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(600) {
            let doc_rev = p.handle.snapshot().doc.rev();
            let (saved, err) = p.handle.saved();
            if err.is_none() && saved >= doc_rev {
                use std::io::Write;
                let mut o = out.lock();
                for (_, t) in waiting.drain(..) {
                    writeln!(o, "SAVED {t}").unwrap();
                }
                o.flush().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn titles(root: &Path) -> Vec<String> {
    let c = core_at(root);
    let v = c.open_home(&home(root)).expect("reopens after kill -9");
    let p = c.project(v["id"].as_str().unwrap()).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let t: Vec<String> = doc.iter::<Cut>().map(|c| c.title.clone()).collect();
    c.shutdown();
    t
}

#[test]
fn every_edit_shown_saved_survives_kill_9() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let rounds: u64 = std::env::var("GMCLIP_CRASH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let root = std::env::temp_dir().join(format!("gmclip-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut shown: Vec<String> = Vec::new();
    let mut seed = 0x9e37_79b9_u64 ^ u64::from(std::process::id());
    for round in 0..rounds {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        // kill after 0.3–1.8 s of editing
        let after = Duration::from_millis(300 + (seed >> 33) % 1_500);
        let mut child = Command::new(&exe)
            .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, &root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let t0 = Instant::now();
        let mut opened = false;
        let mut round_saved = 0;
        while t0.elapsed() < Duration::from_secs(30) {
            if let Ok(l) = rx.recv_timeout(Duration::from_millis(5)) {
                // the harness prints "test crash_child ... " on the same line
                if l.contains("OPEN ") {
                    opened = true;
                } else if let Some(t) = l.split_once("SAVED ").map(|x| x.1) {
                    shown.push(t.to_owned());
                    round_saved += 1;
                }
            }
            if opened && t0.elapsed() >= after && round_saved > 0 {
                break;
            }
        }
        // SIGKILL: no clean close, no final checkpoint, mid-edit
        child.kill().unwrap();
        let _ = child.wait();
        // lines already written before the kill were shown too
        let _ = reader.join();
        while let Ok(l) = rx.try_recv() {
            if let Some(t) = l.split_once("SAVED ").map(|x| x.1) {
                shown.push(t.to_owned());
            }
        }
        assert!(opened, "round {round}: the child did not start");
        let have = titles(&root);
        let missing: Vec<&String> = shown.iter().filter(|t| !have.contains(t)).collect();
        eprintln!(
            "round {round}: killed after {:?}; {} shown Saved so far, {} in the project",
            after,
            shown.len(),
            have.len()
        );
        assert!(
            missing.is_empty(),
            "round {round}: {} edit(s) shown Saved are missing after kill -9: {:?}",
            missing.len(),
            &missing[..missing.len().min(5)]
        );
    }
    assert!(
        shown.len() as u64 >= rounds,
        "every round showed Saved edits"
    );
    let _ = std::fs::remove_dir_all(&root);
}
