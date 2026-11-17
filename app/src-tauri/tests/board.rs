//! U.10 essentials through the app core: the review board's tiles and keep / change /
//! drop, Send to Clippy (pairing on a sheet, refusals, and — lab — a real export handed
//! to a local stand-in for Clippy), the activity rail and Pause agents. Skips without
//! the lab media (the cut comes from loc_4fadb756_h37, so it carries a Clippy hook).

#[cfg(any(target_os = "macos", windows))]
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
#[cfg(any(target_os = "macos", windows))]
use serde_json::json;
static SHEETS: AtomicUsize = AtomicUsize::new(0);
static ANSWER: Mutex<bool> = Mutex::new(true);

fn setup(tag: &str) -> Option<(Arc<AppCore>, String, String, std::path::PathBuf)> {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-board-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    c.agents.start(
        &c,
        gmclip_mcp::paths::Env {
            app_data: root.join("app"),
            // Windows: a pipe of its own (pipes live in \\.\pipe\, not in a directory)
            socket: if cfg!(windows) {
                gmclip_mcp::paths::pipe_name_for(&format!(
                    "test-board-{tag}-{}",
                    std::process::id()
                ))
            } else {
                root.join("s").join("s")
            },
        },
    );
    gmclip_app::sheet::install(Box::new(|_, _, _| {
        SHEETS.fetch_add(1, Ordering::SeqCst);
        *ANSWER.lock().unwrap()
    }));
    let p = gmclip_app::labfixture::open_project(&c, "loc_4fadb756").unwrap();
    let pid = p["id"].as_str().unwrap().to_owned();
    let cut = p["fixture"]["cuts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hook_id"] == "loc_4fadb756_h37")
        .unwrap()["cut"]
        .as_str()
        .unwrap()
        .to_owned();
    Some((c, pid, cut, root))
}

#[test]
fn board_verdicts_clippy_refusals_and_pause() {
    let Some((c, pid, cut, root)) = setup("ops") else {
        return;
    };
    let file = root.join("exports/fake.mp4");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"not a video").unwrap();
    c.add_export(
        &pid,
        ExportEntry {
            id: "exp-1".into(),
            fps: 30,
            fps_den: 1,
            cut: cut.clone(),
            title: "h37".into(),
            file: file.clone(),
            frames: 300,
            seconds: 4.0,
            render_sig: None,
            at_ms: 1,
            notes: vec![],
            verdict: None,
            verify: None,
            opening_sig: None,
            opening_label: None,
            framing: None,
            plan_sig: None,
        },
    )
    .unwrap();
    let b = gmclip_app::board::board(&c, &pid).unwrap();
    let t = &b["tiles"][0];
    assert_eq!(t["clippy"]["hook"], "loc_4fadb756_h37");
    assert_eq!(t["seconds"], 10.0);
    assert_eq!(b["clippy_paired"], false);

    // keep / change / drop persists (exports.json), and clears with null
    gmclip_app::board::verdict(&c, &pid, "exp-1", Some("change")).unwrap();
    assert!(gmclip_app::board::verdict(&c, &pid, "exp-1", Some("maybe")).is_err());
    assert_eq!(
        gmclip_app::board::board(&c, &pid).unwrap()["tiles"][0]["verdict"],
        "change"
    );
    let home = c.project(&pid).unwrap().home.clone();
    assert!(
        std::fs::read_to_string(home.join("exports.json"))
            .unwrap()
            .contains("\"verdict\": \"change\"")
    );

    // Clippy: unpaired → refused before any sheet; a cancelled pairing sheet pairs nothing
    let n0 = SHEETS.load(Ordering::SeqCst);
    let e = gmclip_app::board::send(&c, &pid, "exp-1").unwrap_err();
    assert!(e.message.contains("not paired"), "{}", e.message);
    assert_eq!(SHEETS.load(Ordering::SeqCst), n0);
    *ANSWER.lock().unwrap() = false;
    assert!(gmclip_app::board::pair(&c).is_err());
    assert_eq!(
        gmclip_app::board::board(&c, &pid).unwrap()["clippy_paired"],
        false
    );
    *ANSWER.lock().unwrap() = true;
    gmclip_app::board::pair(&c).unwrap();
    assert_eq!(
        gmclip_app::board::board(&c, &pid).unwrap()["clippy_paired"],
        true
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(root.join("app/clippy-pairing.json")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
    }
    // an export without its record is never sent (and no sheet asks)
    let n1 = SHEETS.load(Ordering::SeqCst);
    let e = gmclip_app::board::send_to(&c, &pid, "exp-1", "http://127.0.0.1:9").unwrap_err();
    assert!(e.message.contains("record"), "{}", e.message);
    assert_eq!(SHEETS.load(Ordering::SeqCst), n1);
    // never anything but loopback
    assert!(gmclip_app::board::send_to(&c, &pid, "exp-1", "http://10.0.0.1:8878").is_err());

    // activity rail + Pause agents (sheet); refused sheet changes nothing
    let a = gmclip_app::board::activity(&c);
    assert_eq!(a["holder"], true);
    assert_eq!(a["paused"], false);
    *ANSWER.lock().unwrap() = false;
    assert!(gmclip_app::board::pause(&c, true).is_err());
    assert_eq!(gmclip_app::board::activity(&c)["paused"], false);
    *ANSWER.lock().unwrap() = true;
    gmclip_app::board::pause(&c, true).unwrap();
    assert_eq!(gmclip_app::board::activity(&c)["paused"], true);
    assert!(c.agents.holder().unwrap().paused());
    gmclip_app::board::pause(&c, false).unwrap();
    assert_eq!(gmclip_app::board::activity(&c)["paused"], false);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Lab: a real export of h37's first part, handed to a local stand-in for Clippy.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs the platform encoder (VideoToolbox, Media Foundation) + AJ's media (cargo test -- --ignored)"]
fn real_export_sent_to_clippy() {
    let Some((c, pid, cut, root)) = setup("send") else {
        return;
    };
    // keep only the first part (short export)
    let v = c.cut_view(&pid, &cut).unwrap();
    for p in v["parts"].as_array().unwrap().iter().skip(1) {
        c.call(&pid, "part_delete", json!({"part": p["id"]}))
            .unwrap();
    }
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let j = jobs.start_export(&c, &pid, &cut).unwrap();
    let id = j["job"].as_str().unwrap().to_owned();
    loop {
        let st = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if st.state == "done" {
            break;
        }
        assert!(st.state != "failed", "export failed: {:?}", st.error);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let ex = c.exports(&pid).unwrap();
    gmclip_app::board::pair(&c).unwrap();
    let secret: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("app/clippy-pairing.json")).unwrap())
            .unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let got = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let mut buf = vec![0u8; 65536];
        let mut all = Vec::new();
        while let Ok(n) = s.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
            let t = String::from_utf8_lossy(&all);
            if let Some((h, b)) = t.split_once("\r\n\r\n") {
                let len: usize = h
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if b.len() >= len {
                    break;
                }
            }
        }
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}").unwrap();
        String::from_utf8_lossy(&all).into_owned()
    });
    let n = SHEETS.load(Ordering::SeqCst);
    let r = gmclip_app::board::send_to(&c, &pid, &ex[0].id, &format!("http://127.0.0.1:{port}"))
        .unwrap();
    assert_eq!(SHEETS.load(Ordering::SeqCst), n + 1, "one confirm sheet");
    let req = got.join().unwrap();
    eprintln!("{r}\n{}", &req[..req.len().min(600)]);
    assert!(req.starts_with("POST /api/edit_station/final HTTP/1.1"));
    assert!(req.contains(&format!(
        "Authorization: Bearer {}",
        secret["secret"].as_str().unwrap()
    )));
    assert!(req.contains("\"hook_id\":\"loc_4fadb756_h37\""));
    assert_eq!(r["clippy"]["ok"], true);
    assert!(ex[0].file.with_extension("clippy.json").is_file());
    assert_eq!(
        gmclip_app::board::board(&c, &pid).unwrap()["tiles"][0]["clippy"]["sent"],
        true
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
