//! Decision 0015 through the app core and its review link (loopback, real TCP), on AJ's
//! loc_4fadb756_h37 from its stream's project fixture (skips without the lab media):
//! - comments pinned at an output frame follow their picture through a reorder, and the
//!   review link writes threads, replies and resolves as the link;
//! - an editor's link suggests a trim, a strike and a caption move; nothing changes until
//!   the human accepts, one ⌘Z each; a reviewer's link cannot suggest;
//! - "Approve" on loopback queues a Home ask; the ask is dismissed or sent (the sheet);
//!   an approved send needs no sheet, reaches a stand-in Clippy with the hook, never the
//!   real one;
//! - golden cuts: marking needs an export, briefs carry the spec, compare names what is
//!   outside the band;
//! - thumbnails: a version at a frame, its title plan (fonts, one title event, valid).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs, ExportEntry};
use serde_json::{Value, json};
static SHEETS: AtomicUsize = AtomicUsize::new(0);

fn setup(tag: &str) -> Option<(Arc<AppCore>, String, String, std::path::PathBuf)> {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media on this machine");
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-collab-{tag}-{}", std::process::id()));
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
            socket: root.join("s").join("s"),
        },
    );
    gmclip_app::sheet::install(Box::new(|_, _, _| {
        SHEETS.fetch_add(1, Ordering::SeqCst);
        true
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

struct Resp {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn req(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" {
        r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    r.push_str("\r\n");
    r.push_str(body);
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(r.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(buf.len(), |i| i + 4);
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    Resp {
        status,
        head,
        body: buf[end..].to_vec(),
    }
}

/// A page session on the app's review server for a link role.
struct Page {
    addr: SocketAddr,
    origin: String,
    cookie: String,
    csrf: String,
    home: Value,
}

impl Page {
    fn open(c: &AppCore, addr: SocketAddr, role: Option<&str>) -> Page {
        let (_, token) = gmclip_mcp::review::links::create_with_role(
            &c.dirs.app_data,
            gmclip_app::core::now_ms(),
            None,
            role.map(str::to_owned),
        )
        .unwrap();
        let origin = format!("http://{addr}");
        let s = req(
            addr,
            "POST",
            "/session",
            &[("Origin", &origin), ("Content-Type", "application/json")],
            &json!({"token": token}).to_string(),
        );
        assert_eq!(s.status, 200);
        let cookie = s
            .head
            .lines()
            .find_map(|l| l.strip_prefix("Set-Cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let mut p = Page {
            addr,
            origin,
            cookie,
            csrf: String::new(),
            home: Value::Null,
        };
        p.reload();
        p
    }
    fn reload(&mut self) {
        let h = req(
            self.addr,
            "GET",
            "/api/home",
            &[("Cookie", &self.cookie)],
            "",
        );
        assert_eq!(h.status, 200);
        self.home = h.json();
        self.csrf = self.home["csrf"].as_str().unwrap().to_owned();
    }
    fn post(&self, path: &str, body: Value) -> Resp {
        req(
            self.addr,
            "POST",
            path,
            &[
                ("Cookie", &self.cookie),
                ("Origin", &self.origin),
                ("X-CSRF", &self.csrf),
                ("Content-Type", "application/json"),
            ],
            &body.to_string(),
        )
    }
    fn cut<'a>(&'a self, id: &str) -> &'a Value {
        self.home["cuts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()
    }
}

/// A fake export of the cut (bytes that are not a video, but a record that matches them,
/// with the cut's own length), as `exp-1`.
fn fake_export(c: &AppCore, pid: &str, cut: &str, root: &Path) -> std::path::PathBuf {
    let pl = c.planned(pid, cut).unwrap();
    let frames = pl.plan.output.frames;
    let fps = pl.plan.output.fps.num.get();
    let file = root.join("exports/h37.mp4");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let bytes = b"stand-in bytes for an export".repeat(100);
    std::fs::write(&file, &bytes).unwrap();
    let (sha, _) = gmclip_ml::sha256::file_hex(&file).unwrap();
    let rec = gmclip_media::export_record::ExportRecord {
        render_sig: None,
        render_sig_absent: Some("test".into()),
        framing: Some("9:16".into()),
        preset: "reels".into(),
        input_rev: None,
        duration_expected: gmclip_media::export_record::DurationExpected {
            frames,
            fps_num: fps,
            fps_den: 1,
            audio_samples: frames * 48_000 / fps,
        },
        frames_hash: None,
        file: "h37.mp4".into(),
        sha256: sha,
        loudness: None,
        draft_mattes: vec![],
        notes: vec![],
        blocked: vec![],
        app_version: "test".into(),
        renderer_version: "test".into(),
    };
    rec.write(&file).unwrap();
    c.add_export(
        pid,
        ExportEntry {
            id: "exp-1".into(),
            fps,
            fps_den: 1,
            cut: cut.to_owned(),
            title: "h37".into(),
            file: file.clone(),
            frames,
            seconds: 1.0,
            render_sig: None,
            at_ms: gmclip_app::core::now_ms(),
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
    file
}

fn threads(c: &AppCore, pid: &str, cut: &str) -> Vec<Value> {
    gmclip_app::collab::view(c, pid, cut).unwrap()["threads"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn comments_suggestions_and_asks_through_the_review_link() {
    let Some((c, pid, cut, root)) = setup("link") else {
        return;
    };
    let v = c.cut_view(&pid, &cut).unwrap();
    let parts = v["parts"].as_array().unwrap().clone();
    let (p1, p2) = (parts[0].clone(), parts[1].clone());
    let p2_start = p2["out"][0].as_u64().unwrap();

    // a comment 10 frames into part 2, with a box
    gmclip_app::collab::add_at(
        &c,
        &pid,
        &cut,
        Some(p2_start + 10),
        Some(&json!({"x": 0.1, "y": 0.2, "w": 0.4, "h": 0.1})),
        "the prompt is unreadable",
        None,
    )
    .unwrap();
    let t = threads(&c, &pid, &cut);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0]["frame"], p2_start + 10);
    assert_eq!(t[0]["author"], "human");
    assert_eq!(t[0]["box"]["w"], 0.4);
    // part 2 moves first: the comment follows its picture
    c.call(&pid, "part_reorder", json!({"part": p2["id"]}))
        .unwrap();
    assert_eq!(threads(&c, &pid, &cut)[0]["frame"], 10);
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(threads(&c, &pid, &cut)[0]["frame"], p2_start + 10);

    // the review link (loopback): a reviewer comments at 2.0 s, replies, resolves
    fake_export(&c, &pid, &cut, &root);
    let st = gmclip_app::reviewlink::serve_on(&c, true, 0).unwrap();
    let addr: SocketAddr = st["serving"].as_str().unwrap().parse().unwrap();
    let mut rv = Page::open(&c, addr, None);
    assert_eq!(rv.home["role"], "reviewer");
    let x = rv.home["exports"].as_array().unwrap()[0].clone();
    assert_eq!(x["cut"], cut.as_str());
    let r = rv.post(
        "/api/comment",
        json!({"cut": cut, "text": "<b>too</b> slow\u{200B} here", "t": 2.0, "export_ref": x["id"]}),
    );
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let link_thread = r.json()["comment"].as_str().unwrap().to_owned();
    rv.reload();
    let lt = rv.cut(&cut)["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == link_thread.as_str())
        .unwrap()
        .clone();
    assert!(lt["author"].as_str().unwrap().starts_with("review-link:"));
    assert_eq!(lt["text"], "<b>too</b> slow here", "canonical, and text");
    let fps = c.planned(&pid, &cut).unwrap().plan.output.fps.num.get();
    assert_eq!(lt["frame"], 2 * fps, "2.0 s on the cut");
    assert_eq!(
        rv.post(
            "/api/comment",
            json!({"cut": cut, "text": "agreed", "reply_to": link_thread})
        )
        .status,
        200
    );
    assert_eq!(
        rv.post(
            "/api/resolve",
            json!({"comment": link_thread, "resolved": true})
        )
        .status,
        200
    );
    let t = threads(&c, &pid, &cut);
    let lt = t.iter().find(|t| t["id"] == link_thread.as_str()).unwrap();
    assert_eq!(lt["resolved"], true);
    assert_eq!(lt["replies"].as_array().unwrap().len(), 1);
    // a reviewer cannot suggest
    assert_eq!(
        rv.post(
            "/api/suggest",
            json!({"cut": cut, "kind": "trim", "part": p1["id"], "head_s": 0.4})
        )
        .status,
        403
    );

    // an editor's link: three suggestions, nothing changes yet
    let ed = Page::open(&c, addr, Some("editor"));
    assert_eq!(ed.home["role"], "editor");
    let before = c.cut_view(&pid, &cut).unwrap()["frames"].clone();
    for (b, want) in [
        (
            json!({"cut": cut, "kind": "trim", "part": p1["id"], "head_s": 0.4, "note": "start on the result"}),
            "trim",
        ),
        (
            json!({"cut": cut, "kind": "strike", "from_s": 3.0, "to_s": 3.5}),
            "strike",
        ),
        (
            json!({"cut": cut, "kind": "caption_move", "y": 0.5}),
            "caption_move",
        ),
    ] {
        let r = ed.post("/api/suggest", b);
        assert_eq!(
            r.status,
            200,
            "{want}: {}",
            String::from_utf8_lossy(&r.body)
        );
    }
    assert_eq!(
        ed.post(
            "/api/review",
            json!({"cut": cut, "text": "x", "verdict": "keep"})
        )
        .status,
        403
    );
    assert_eq!(c.cut_view(&pid, &cut).unwrap()["frames"], before);
    let view = gmclip_app::collab::view(&c, &pid, &cut).unwrap();
    let sug = view["suggestions"].as_array().unwrap().clone();
    assert_eq!(sug.len(), 3);
    assert!(
        sug[0]["summary"]
            .as_str()
            .unwrap()
            .starts_with("Trim part 1: 0.40 s off its start"),
        "{}",
        sug[0]["summary"]
    );
    // the human accepts the trim: 0.4 s shorter, and one ⌘Z puts it back
    let f0 = before.as_u64().unwrap();
    c.call(
        &pid,
        "edit_proposal_state",
        json!({"proposals": [sug[0]["id"]], "state": "accepted"}),
    )
    .unwrap();
    let f1 = c.cut_view(&pid, &cut).unwrap()["frames"].as_u64().unwrap();
    assert!(
        (f0 - f1).abs_diff((0.4 * fps as f64).round() as u64) <= 1,
        "{f0} → {f1}"
    );
    c.call(&pid, "undo", json!({})).unwrap();
    assert_eq!(c.cut_view(&pid, &cut).unwrap()["frames"], before);
    // reject the strike, accept the caption move
    c.call(
        &pid,
        "edit_proposal_state",
        json!({"proposals": [sug[1]["id"]], "state": "rejected"}),
    )
    .unwrap();
    c.call(
        &pid,
        "edit_proposal_state",
        json!({"proposals": [sug[2]["id"]], "state": "accepted"}),
    )
    .unwrap();
    let states: Vec<String> = gmclip_app::collab::view(&c, &pid, &cut).unwrap()["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["proposed", "rejected", "accepted"]);

    // approve on loopback: a Home ask, dismissed; another, sent through the sheet
    let n0 = SHEETS.load(Ordering::SeqCst);
    let a = rv.post("/api/approve", json!({"export": x["id"]}));
    assert_eq!(a.json()["sent"], false);
    let asks = gmclip_app::collab::asks(&c);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["action"], "remote_send");
    gmclip_app::collab::ask_act(&c, asks[0]["ask"].as_str().unwrap(), false).unwrap();
    assert!(gmclip_app::collab::asks(&c).is_empty());
    assert_eq!(
        SHEETS.load(Ordering::SeqCst),
        n0,
        "approve on a link opens no sheet"
    );
    // "fix with agent" from the link: its note becomes a thread, and the ask waits here
    let f = rv.post("/api/fix", json!({"cut": cut, "note": "tighten the open"}));
    assert_eq!(f.json()["started"], false);
    let asks = gmclip_app::collab::asks(&c);
    assert_eq!(asks[0]["action"], "remote_fix");
    assert!(
        threads(&c, &pid, &cut)
            .iter()
            .any(|t| t["text"] == "tighten the open"
                && t["author"].as_str().unwrap().starts_with("review-link:"))
    );
    let _ = gmclip_app::reviewlink::serve_on(&c, false, 0);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_approved_send_needs_no_sheet_and_reaches_the_stand_in() {
    let Some((c, pid, cut, root)) = setup("send") else {
        return;
    };
    fake_export(&c, &pid, &cut, &root);
    gmclip_app::board::pair(&c).unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let got = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut all = Vec::new();
        let mut buf = vec![0u8; 65536];
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
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
            .unwrap();
        String::from_utf8_lossy(&all).into_owned()
    });
    let n0 = SHEETS.load(Ordering::SeqCst);
    let out =
        gmclip_app::board::send_approved(&c, &pid, "exp-1", &format!("http://127.0.0.1:{port}"))
            .unwrap();
    assert_eq!(out["hook_id"], "loc_4fadb756_h37");
    assert_eq!(
        SHEETS.load(Ordering::SeqCst),
        n0,
        "the link's approve is the click"
    );
    let body = got.join().unwrap();
    assert!(body.starts_with("POST /api/edit_station/final"));
    assert!(body.contains("loc_4fadb756_h37"));
    // a blocked export is never handed over, approved or not
    let file = root.join("exports/h37.mp4");
    let mut rec = gmclip_media::export_record::ExportRecord::read(&file).unwrap();
    rec.blocked = vec!["error-level lint".into()];
    rec.write(&file).unwrap();
    let e = gmclip_app::board::send_approved(&c, &pid, "exp-1", "http://127.0.0.1:9").unwrap_err();
    assert!(e.message.contains("blocked"), "{}", e.message);
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn golden_cuts_and_thumbnails() {
    let Some((c, pid, cut, root)) = setup("golden") else {
        return;
    };
    // a cut without an export is not finished
    let e = gmclip_app::golden::mark(&c, &pid, &cut, true).unwrap_err();
    assert!(e.message.contains("export"), "{}", e.message);
    fake_export(&c, &pid, &cut, &root);
    gmclip_app::golden::mark(&c, &pid, &cut, true).unwrap();
    assert!(gmclip_app::golden::is_golden(&c, &pid, &cut));
    let g = gmclip_app::golden::list(&c);
    assert_eq!(g.len(), 1);
    let s = g[0].spec.clone();
    assert!(s.parts >= 2 && s.duration_s > 10.0, "{s:?}");
    assert_eq!(s.canvas, [1080, 1920]);
    // every brief carries it, once
    let b = gmclip_app::golden::with_golden(&c, "Do the pass.".into());
    assert!(b.contains("## Golden cuts") && b.contains("<untrusted>"));
    assert_eq!(gmclip_app::golden::with_golden(&c, b.clone()), b);
    // another cut compared with it: the band, and what is outside it
    let other = c
        .call(&pid, "cut_create", json!({"title": "short one"}))
        .unwrap()["output"]["cut"]
        .as_str()
        .map(str::to_owned);
    let _ = other;
    let cmp = gmclip_app::golden::compare(&c, &pid, &cut).unwrap();
    assert_eq!(
        cmp["goldens"].as_array().unwrap().len(),
        0,
        "a cut is not compared with itself"
    );
    // trim the cut by a third: its length leaves the band of the golden
    let v = c.cut_view(&pid, &cut).unwrap();
    let last = v["parts"].as_array().unwrap().last().unwrap()["id"].clone();
    // compare a second golden-less view: unmark, re-mark as a different "cut" is not
    // possible here, so compare the trimmed cut against a stored copy of itself
    let mut store: Value =
        serde_json::from_slice(&std::fs::read(root.join("app/golden-cuts.json")).unwrap()).unwrap();
    for (_, list) in store.as_object_mut().unwrap() {
        for g in list.as_array_mut().unwrap() {
            g["cut"] = json!("01J00000000000000000000000");
        }
    }
    std::fs::write(
        root.join("app/golden-cuts.json"),
        serde_json::to_vec(&store).unwrap(),
    )
    .unwrap();
    c.call(&pid, "part_delete", json!({"part": last})).unwrap();
    // keep cutting until it is well under the golden's length
    let gold = s.duration_s;
    loop {
        let v = c.cut_view(&pid, &cut).unwrap();
        let ps = v["parts"].as_array().unwrap().clone();
        let secs = v["frames"].as_f64().unwrap() / v["fps"]["num"].as_f64().unwrap();
        if secs < gold * 0.7 || ps.len() <= 2 {
            break;
        }
        c.call(
            &pid,
            "part_delete",
            json!({"part": ps.last().unwrap()["id"]}),
        )
        .unwrap();
    }
    let cmp = gmclip_app::golden::compare(&c, &pid, &cut).unwrap();
    assert_eq!(cmp["goldens"].as_array().unwrap().len(), 1);
    let len_row = cmp["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "duration_s")
        .unwrap()
        .clone();
    eprintln!("length row: {len_row}");
    if len_row["mine"].as_f64().unwrap() < gold * 0.8 {
        assert_eq!(len_row["status"], "below");
        assert!(
            cmp["outside"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o == "Length (s)")
        );
    }
    assert!(cmp["mine_line"].as_str().unwrap().contains("takes"));

    // thumbnails: a version at a frame; its title plan has one title event per line
    let f = c.cut_view(&pid, &cut).unwrap()["parts"][0]["out"][0]
        .as_u64()
        .unwrap()
        + 15;
    let t = gmclip_app::thumbs::add(
        &c,
        &pid,
        &cut,
        f,
        "My PC made this in *67 seconds*",
        None,
        true,
    )
    .unwrap();
    let tid = t["output"]["id"].as_str().unwrap().to_owned();
    let view = gmclip_app::thumbs::view(&c, &pid, &cut).unwrap();
    let ver = &view["versions"][0];
    assert_eq!(ver["frame"], f);
    assert_eq!(ver["picked"], true);
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let th = doc
        .get(gmclip_app::planner::ulid_id::<gmclip_model::entities::Thumbnail>(&tid).unwrap())
        .unwrap()
        .clone();
    match gmclip_app::thumbs::title_plan(&c, &pid, &th) {
        Ok((plan, frame)) => {
            assert_eq!(frame, f);
            let ev = plan.text_events.as_ref().unwrap();
            assert!(!ev.is_empty() && ev.iter().all(|e| e.out.start() == f));
            let text: String = ev
                .iter()
                .flat_map(|e| e.runs.iter().map(|r| r.text.clone()))
                .collect();
            assert!(
                text.contains("67") && text.contains("SECONDS") && text.starts_with("MY PC"),
                "{text}"
            );
            assert!(
                ev.iter()
                    .flat_map(|e| e.runs.iter())
                    .any(|r| r.fill.as_ref().is_some_and(|f| f.as_str() == "#FFD400FF")),
                "the *accent* words"
            );
        }
        Err(e) if e.message.contains("font") => eprintln!("no font on this machine: {}", e.message),
        Err(e) => panic!("{e}"),
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Lab: the thumbnail rendered by the export's compositor to a PNG beside the project.
#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's media (cargo test -- --ignored)"]
fn a_thumbnail_renders_to_a_png() {
    let Some((c, pid, cut, root)) = setup("render") else {
        return;
    };
    let f = c.cut_view(&pid, &cut).unwrap()["parts"][1]["out"][0]
        .as_u64()
        .unwrap()
        + 5;
    let t = gmclip_app::thumbs::add(&c, &pid, &cut, f, "GPU *2% → 100%*", None, true).unwrap();
    let tid = t["output"]["id"].as_str().unwrap().to_owned();
    let t0 = std::time::Instant::now();
    let r = gmclip_app::thumbs::render(&c, &pid, &tid).unwrap();
    eprintln!("rendered in {:?}: {}", t0.elapsed(), r["path"]);
    let png = std::fs::read(r["path"].as_str().unwrap()).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        (r["width"].as_u64(), r["height"].as_u64()),
        (Some(1080), Some(1920))
    );
    assert!(r["preview_png"].as_str().unwrap().len() > 1000);
    assert!(gmclip_app::thumbs::view(&c, &pid, &cut).unwrap()["versions"][0]["rendered"] == true);
    // GMCLIP_THUMB_OUT=<file>: keep a copy to look at
    if let Ok(out) = std::env::var("GMCLIP_THUMB_OUT") {
        std::fs::write(out, &png).unwrap();
    }
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
