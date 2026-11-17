//! U.10 review-link controls (SPEC §5.14.1, SR R-REV-1..6, R-UI-SEC-2): serving and a new
//! link go through the sheet; the link's page lists this library's export and plays it
//! by export id; a review note written through the link lands on the cut as
//! `review-link:<id>`, its text kept as text (an XSS fixture stays a string); revoking
//! the link refuses new sessions; a staged moment's clip (H.10) is listed and served by
//! moment id with its recorded size. Lab: VideoToolbox for the export, AJ's h79 file.
// The platform media: VideoToolbox and Metal on macOS, Media Foundation and DX12 on
// Windows (decisions/0019).
#![cfg(any(target_os = "macos", windows))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gmclip_app::core::{AppCore, Dirs};
use serde_json::{Value, json};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}
const F: i64 = 705_600_000;
static SHEETS: AtomicUsize = AtomicUsize::new(0);

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.0.eq_ignore_ascii_case(k))
            .map(|h| h.1.as_str())
    }
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
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(r.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(buf.len(), |i| i + 4);
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        })
        .collect();
    Resp {
        status,
        headers,
        body: buf[end..].to_vec(),
    }
}

#[cfg(any(target_os = "macos", windows))]
#[test]
#[ignore = "lab: needs VideoToolbox + AJ's media"]
fn serve_link_review_and_revoke() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-rl-{}", std::process::id()));
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
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    // a 1 s export
    let p = c.project(&pid).unwrap();
    let doc = p.handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(&cut).unwrap();
    let part = gmclip_model::clock::cut_layout(&doc, cid)[0].part;
    c.call(
        &pid,
        "part_trim",
        json!({"part": part.0.to_string(), "src_in": 0, "src_out": F}),
    )
    .unwrap();
    let jobs = Arc::new(gmclip_app::jobs::Jobs::default());
    let id = jobs.start_export(&c, &pid, &cut).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_owned();
    loop {
        let s = jobs.list().into_iter().find(|x| x.id == id).unwrap();
        if s.state == "done" {
            break;
        }
        assert!(s.state != "failed", "{:?}", s.error);
        std::thread::sleep(Duration::from_millis(200));
    }

    // serving and a link: each one sheet
    let n0 = SHEETS.load(Ordering::SeqCst);
    let st = gmclip_app::reviewlink::serve_on(&c, true, 0).unwrap();
    let addr: SocketAddr = st["serving"].as_str().unwrap().parse().unwrap();
    assert!(addr.ip().is_loopback(), "R-REV-1: 127.0.0.1 only");
    let link = gmclip_app::reviewlink::link_new(&c, None).unwrap();
    assert_eq!(SHEETS.load(Ordering::SeqCst), n0 + 2);
    let url = link["url"].as_str().unwrap();
    let token = url.split("#t=").nth(1).unwrap();
    assert_eq!(gmclip_app::reviewlink::status(&c)["links"][0]["live"], true);
    // the page's session: cookie + CSRF
    let origin = format!("http://{addr}");
    let s = req(
        addr,
        "POST",
        "/session",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        &json!({"token": token}).to_string(),
    );
    assert_eq!(s.status, 200, "{}", String::from_utf8_lossy(&s.body));
    let cookie = s
        .header("Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let home = req(addr, "GET", "/api/home", &[("Cookie", &cookie)], "");
    assert_eq!(home.status, 200);
    let h = home.json();
    let csrf = h["csrf"].as_str().unwrap().to_owned();
    let ex = h["home"]["exports"]
        .as_array()
        .or_else(|| h["exports"].as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(ex.len(), 1, "{h}");
    let xid = ex[0]["id"].as_str().unwrap();
    let file = req(
        addr,
        "GET",
        &format!("/x/{xid}"),
        &[("Cookie", &cookie)],
        "",
    );
    assert_eq!(file.status, 200);
    assert!(file.body.len() > 10_000, "the export's bytes");
    // a review note with an XSS fixture: stored as text, attributed to the link
    let xss = "<img src=x onerror=alert(1)> too jumpy";
    let w = req(
        addr,
        "POST",
        "/api/review",
        &[
            ("Cookie", &cookie),
            ("Origin", &origin),
            ("X-CSRF", &csrf),
            ("Content-Type", "application/json"),
        ],
        &json!({"cut": cut, "text": xss, "verdict": "change"}).to_string(),
    );
    assert_eq!(w.status, 200, "{}", String::from_utf8_lossy(&w.body));
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let r = doc
        .iter::<gmclip_model::entities::Review>()
        .next()
        .expect("the review");
    assert_eq!(r.text, xss);
    assert_eq!(
        r.author,
        format!("review-link:{}", link["id"].as_str().unwrap())
    );
    // H.10: a staged moment's clip, listed and served by moment id
    let m = c
        .call(
            &pid,
            "moment_add",
            json!({"start_s": 2.0, "end_s": 7.0, "quote": "a staged moment", "score": 4,
                   "type": "result", "why": "H.10 review link"}),
        )
        .unwrap()["output"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        &pid,
        "moment_set_state",
        json!({"moment": m, "state": "staged"}),
    )
    .unwrap();
    let (rec, _, _) = gmclip_app::clips::ensure(&c, &pid, &m).unwrap();
    let home = req(addr, "GET", "/api/home", &[("Cookie", &cookie)], "").json();
    let listed = home["home"]["moments"]
        .as_array()
        .or_else(|| home["moments"].as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        listed
            .iter()
            .any(|x| x["id"] == m.as_str() && x["clip"] == true),
        "{home}"
    );
    let clip = req(addr, "GET", &format!("/m/{m}"), &[("Cookie", &cookie)], "");
    assert_eq!(clip.status, 200);
    assert_eq!(clip.body.len() as u64, rec.size, "the clip's bytes");
    // revoked: no new session
    gmclip_app::reviewlink::revoke(&c, Some(link["id"].as_str().unwrap())).unwrap();
    let again = req(
        addr,
        "POST",
        "/session",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        &json!({"token": token}).to_string(),
    );
    assert_eq!(again.status, 401);
    gmclip_app::reviewlink::serve(&c, false).unwrap();
    assert!(gmclip_app::reviewlink::status(&c)["serving"].is_null());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The app, h79 open, the viewer playing it (a frame-socket reader taking every frame),
/// and the review link served on loopback. Lab: VideoToolbox, Metal, AJ's media.
#[cfg(target_os = "macos")]
fn playing_with_review_link(
    tag: &str,
) -> Option<(
    Arc<AppCore>,
    gmclip_app::viewer::Viewer,
    SocketAddr,
    std::path::PathBuf,
)> {
    use gmclip_app::viewer::ViewerMsg;
    if !Path::new(video()).is_file() {
        return None;
    }
    let root = std::env::temp_dir().join(format!("gmclip-rl-{tag}-{}", std::process::id()));
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
    gmclip_app::sheet::install(Box::new(|_, _, _| true));
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    let slot = Arc::new(gmclip_app::frame::FrameSlot::default());
    let (_rtx, rrx) = std::sync::mpsc::channel();
    std::mem::forget(_rtx);
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let viewer = gmclip_app::viewer::spawn(&c, Arc::clone(&slot), rrx, connected);
    std::thread::spawn(move || {
        loop {
            let _ = slot.take(Duration::from_millis(100));
        }
    });
    viewer.send(ViewerMsg::Load {
        project: pid,
        cut,
        framing: None,
        keep: false,
    });
    std::thread::sleep(Duration::from_millis(1500));
    let st = gmclip_app::reviewlink::serve_on(&c, true, 0).unwrap();
    let addr: SocketAddr = st["serving"].as_str().unwrap().parse().unwrap();
    viewer.send(ViewerMsg::Play);
    // past the first frames' decode warm-up
    std::thread::sleep(Duration::from_secs(2));
    Some((c, viewer, addr, root))
}

/// A.03 / R-REV-6, R-PROTO-7: with the preview playing, eight held review-link connections
/// fill the server, the ninth is closed without an answer, and playback drops no frames
/// meanwhile (the server has its own one-thread runtime, the viewer its render thread).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's media (run in release)"]
fn the_ninth_connection_is_refused_while_playback_drops_nothing() {
    let Some((c, viewer, addr, root)) = playing_with_review_link("ninth") else {
        return;
    };
    let s0 = viewer.state();
    let t0 = std::time::Instant::now();
    let held: Vec<TcpStream> = (0..8).map(|_| TcpStream::connect(addr).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(300));
    let mut ninth = TcpStream::connect(addr).unwrap();
    ninth
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut b = [0u8; 16];
    let got = ninth.read(&mut b).unwrap_or(0);
    // and a real request now: refused the same way while the eight are held
    let _ = ninth.write_all(b"GET /app.js HTTP/1.1\r\nHost: x\r\n\r\n");
    // keep playing with the eight held for a few seconds more
    std::thread::sleep(Duration::from_secs(5));
    let s1 = viewer.state();
    drop(held);
    let secs = t0.elapsed().as_secs_f64();
    let (frames, drops) = (s1.frame.saturating_sub(s0.frame), s1.dropped - s0.dropped);
    eprintln!(
        "A.03 ninth connection: answered {got} bytes; playback over {secs:.1} s: {frames} frames shown, {} rendered, {drops} dropped (total {} dropped since load), A/V max {:.1} ms, underruns {}",
        s1.rendered - s0.rendered,
        s1.dropped,
        s1.audio.av_max_ms,
        s1.audio.underruns
    );
    assert_eq!(got, 0, "the ninth connection is closed without an answer");
    assert!(s1.playing && frames > 100, "playback ran ({frames} frames)");
    assert_eq!(
        drops, 0,
        "no dropped preview frames while the server is full"
    );
    gmclip_app::reviewlink::serve(&c, false).unwrap();
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A.03 / R-PROTO-7: slow-loris clients on the review link — each sends its request one
/// byte at a time and never finishes — are dropped at the server's header timeout (10 s),
/// and playback meanwhile drops no preview frames.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "lab: needs VideoToolbox + Metal + AJ's media (run in release)"]
fn slow_loris_clients_hit_the_idle_timeout_and_playback_drops_nothing() {
    let Some((c, viewer, addr, root)) = playing_with_review_link("loris") else {
        return;
    };
    let s0 = viewer.state();
    let t0 = std::time::Instant::now();
    let head = format!("GET /api/home HTTP/1.1\r\nHost: {addr}\r\nX-Slow: ");
    let clients: Vec<std::thread::JoinHandle<(Duration, usize)>> = (0..4)
        .map(|_| {
            let head = head.clone();
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                let mut s = TcpStream::connect(addr).unwrap();
                s.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
                let mut sent = 0;
                // one byte every 250 ms, forever (a header that never ends), until the
                // server closes the connection
                for byte in head.bytes().chain(std::iter::repeat(b'a')) {
                    if s.write_all(&[byte]).is_err() {
                        break;
                    }
                    sent += 1;
                    let mut b = [0u8; 64];
                    match s.read(&mut b) {
                        Ok(0) => break,
                        Ok(_) => break,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(_) => break,
                    }
                    if t.elapsed() > Duration::from_secs(30) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                (t.elapsed(), sent)
            })
        })
        .collect();
    let ends: Vec<(Duration, usize)> = clients.into_iter().map(|h| h.join().unwrap()).collect();
    let s1 = viewer.state();
    let secs = t0.elapsed().as_secs_f64();
    let (frames, drops) = (s1.frame.saturating_sub(s0.frame), s1.dropped - s0.dropped);
    eprintln!(
        "A.03 slow-loris: 4 clients dropped after {:?} ({:?} bytes sent each); playback over {secs:.1} s: {frames} frames shown, {} rendered, {drops} dropped, A/V max {:.1} ms, underruns {}",
        ends.iter()
            .map(|e| format!("{:.2} s", e.0.as_secs_f64()))
            .collect::<Vec<_>>(),
        ends.iter().map(|e| e.1).collect::<Vec<_>>(),
        s1.rendered - s0.rendered,
        s1.audio.av_max_ms,
        s1.audio.underruns
    );
    for (t, _) in &ends {
        assert!(
            *t >= Duration::from_secs(9) && *t < Duration::from_secs(15),
            "dropped at the 10 s header timeout, not before or long after ({t:?})"
        );
    }
    assert!(s1.playing && frames > 200, "playback ran ({frames} frames)");
    assert_eq!(
        drops, 0,
        "no dropped preview frames under the slow-loris clients"
    );
    // the server serves again once they are gone
    let r = req(addr, "GET", "/app.js", &[], "");
    assert_eq!(r.status, 200);
    gmclip_app::reviewlink::serve(&c, false).unwrap();
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
