//! The review link server (SR R-REV-1..6) against a mock catalog, over real TCP.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gmclip_mcp::review::{
    self, Allowance, Catalog, Config, FileRec, FixDecision, FixRequest, Mode, Tailnet, links,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn base(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("gmclip-review-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Default)]
struct Mock {
    files: Mutex<Vec<(String, FileRec)>>,
    reviews: Mutex<Vec<(String, String, String, String)>>,
    fixes: Mutex<Vec<FixRequest>>,
}

impl Catalog for Mock {
    fn home(&self) -> Value {
        json!({"project": "p <img src=x onerror=alert(1)>", "exports": [], "cuts": [{"id": "c1", "title": "t"}], "moments": []})
    }
    fn export(&self, id: &str) -> Option<FileRec> {
        self.files
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.0 == id)
            .map(|f| f.1.clone())
    }
    fn clip(&self, _id: &str) -> Option<FileRec> {
        None
    }
    fn add_review(
        &self,
        author: &str,
        cut: &str,
        text: &str,
        verdict: &str,
        _e: Option<&str>,
    ) -> Result<String, String> {
        self.reviews
            .lock()
            .unwrap()
            .push((author.into(), cut.into(), text.into(), verdict.into()));
        Ok("r1".into())
    }
    fn fix(&self, req: &FixRequest) -> Result<Value, String> {
        self.fixes.lock().unwrap().push(req.clone());
        Ok(json!({}))
    }
}

struct Funnel(Arc<AtomicBool>);
impl Tailnet for Funnel {
    fn funnel_covers(&self, _port: u16) -> Result<bool, String> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

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

fn raw(addr: SocketAddr, req: &str) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(buf.len());
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

struct Http {
    addr: SocketAddr,
    host: String,
    origin: String,
    extra: String,
}

impl Http {
    fn req(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
        let mut r = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{}",
            self.host, self.extra
        );
        for (k, v) in headers {
            r.push_str(&format!("{k}: {v}\r\n"));
        }
        if !body.is_empty() || method == "POST" {
            r.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        r.push_str("\r\n");
        r.push_str(body);
        raw(self.addr, &r)
    }
    /// `(cookie, csrf)` for a token.
    fn session(&self, token: &str) -> (String, String) {
        let r = self.req(
            "POST",
            "/session",
            &[
                ("Origin", &self.origin),
                ("Content-Type", "application/json"),
            ],
            &json!({"token": token}).to_string(),
        );
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let set = r.header("Set-Cookie").unwrap().to_owned();
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/")
        );
        let cookie = set.split(';').next().unwrap().to_owned();
        let home = self.req("GET", "/api/home", &[("Cookie", &cookie)], "");
        assert_eq!(home.status, 200);
        (cookie, home.json()["csrf"].as_str().unwrap().to_owned())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn loopback_tokens_hosts_csrf_files_and_ranges() {
    let d = base("lb");
    let app = d.join("app");
    let mock = Arc::new(Mock::default());
    let video = d.join("e.mp4");
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&video, &data).unwrap();
    let sha = hex(&Sha256::digest(&data));
    mock.files.lock().unwrap().push((
        "abc123".into(),
        FileRec {
            path: video.clone(),
            size: data.len() as u64,
            sha256: sha.clone(),
        },
    ));
    let link_target = d.join("link.mp4");
    // where the OS refuses symlinks (Windows without the privilege) the path is absent,
    // which is refused (404) too
    #[cfg(unix)]
    std::os::unix::fs::symlink(&video, &link_target).unwrap();
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_file(&video, &link_target);
    mock.files.lock().unwrap().push((
        "sym".into(),
        FileRec {
            path: link_target,
            size: data.len() as u64,
            sha256: sha.clone(),
        },
    ));
    let other = d.join("other.mp4");
    std::fs::write(&other, vec![0u8; data.len()]).unwrap();
    mock.files.lock().unwrap().push((
        "bad".into(),
        FileRec {
            path: other,
            size: data.len() as u64,
            sha256: sha.clone(),
        },
    ));

    let mut cfg = Config::new(app.clone(), Mode::Loopback);
    cfg.header_timeout = Duration::from_secs(3);
    let srv = review::start(cfg, mock.clone(), None).unwrap();
    assert!(srv.addr.ip().is_loopback(), "never 0.0.0.0");
    let port = srv.addr.port();
    let h = Http {
        addr: srv.addr,
        host: format!("127.0.0.1:{port}"),
        origin: format!("http://127.0.0.1:{port}"),
        extra: String::new(),
    };

    // the page: CSP, no-referrer, no-store, nosniff, no CORS
    let page = h.req("GET", "/", &[], "");
    assert_eq!(page.status, 200);
    let csp = page.header("Content-Security-Policy").unwrap();
    assert!(
        csp.contains("default-src 'none'")
            && csp.contains("script-src 'self'")
            && csp.contains("frame-ancestors 'none'")
    );
    assert_eq!(page.header("Referrer-Policy"), Some("no-referrer"));
    assert_eq!(page.header("Cache-Control"), Some("no-store"));
    assert_eq!(page.header("X-Content-Type-Options"), Some("nosniff"));
    assert!(
        page.headers
            .iter()
            .all(|(k, _)| !k.to_ascii_lowercase().starts_with("access-control"))
    );
    // Host rebinding
    let evil = Http {
        host: "evil.example".into(),
        ..Http {
            addr: h.addr,
            host: String::new(),
            origin: String::new(),
            extra: String::new(),
        }
    };
    assert_eq!(evil.req("GET", "/", &[], "").status, 403);
    // no session → 401; a bad token → 401
    assert_eq!(h.req("GET", "/api/home", &[], "").status, 401);
    let bad = h.req(
        "POST",
        "/session",
        &[("Origin", &h.origin), ("Content-Type", "application/json")],
        r#"{"token":"00"}"#,
    );
    assert_eq!(bad.status, 401);

    let (link, token) = links::create(&app, now(), None).unwrap();
    let (cookie, csrf) = h.session(&token);
    // a write: cookie + CSRF + same Origin
    let body = json!({"cut": "c1", "text": "<script>x</script>", "verdict": "change"}).to_string();
    let ct = ("Content-Type", "application/json");
    assert_eq!(
        h.req(
            "POST",
            "/api/review",
            &[("Cookie", &cookie), ("Origin", &h.origin), ct],
            &body
        )
        .status,
        403,
        "no CSRF"
    );
    assert_eq!(
        h.req(
            "POST",
            "/api/review",
            &[
                ("Cookie", &cookie),
                ("X-CSRF", &csrf),
                ("Origin", "http://evil.example"),
                ct
            ],
            &body
        )
        .status,
        403,
        "foreign origin"
    );
    assert_eq!(
        h.req(
            "POST",
            "/api/review",
            &[("Cookie", &cookie), ("X-CSRF", &csrf), ct],
            &body
        )
        .status,
        403,
        "no origin"
    );
    let ok = h.req(
        "POST",
        "/api/review",
        &[
            ("Cookie", &cookie),
            ("X-CSRF", &csrf),
            ("Origin", &h.origin),
            ct,
        ],
        &body,
    );
    assert_eq!(ok.status, 200, "{}", String::from_utf8_lossy(&ok.body));
    let r = mock.reviews.lock().unwrap()[0].clone();
    assert_eq!(r.0, format!("review-link:{}", link.id), "never `human`");

    // files: whole, one range, a thousand ranges, past the end, symlink, wrong hash
    let whole = h.req("GET", "/x/abc123", &[("Cookie", &cookie)], "");
    assert_eq!((whole.status, whole.body.len()), (200, data.len()));
    let part = h.req(
        "GET",
        "/x/abc123",
        &[("Cookie", &cookie), ("Range", "bytes=10-19")],
        "",
    );
    assert_eq!(part.status, 206);
    assert_eq!(part.body, data[10..20].to_vec());
    assert_eq!(
        part.header("Content-Range"),
        Some(&*format!("bytes 10-19/{}", data.len()))
    );
    let many = format!(
        "bytes={}",
        (0..1000)
            .map(|i| format!("{}-{}", i % 9, i % 9))
            .collect::<Vec<_>>()
            .join(",")
    );
    let m = h.req(
        "GET",
        "/x/abc123",
        &[("Cookie", &cookie), ("Range", &many)],
        "",
    );
    assert_eq!((m.status, m.body.len()), (200, data.len()));
    assert_eq!(
        h.req(
            "GET",
            "/x/abc123",
            &[("Cookie", &cookie), ("Range", "bytes=999999-")],
            ""
        )
        .status,
        416
    );
    assert_eq!(
        h.req("GET", "/x/sym", &[("Cookie", &cookie)], "").status,
        404,
        "a symlink is not served"
    );
    assert_eq!(
        h.req("GET", "/x/bad", &[("Cookie", &cookie)], "").status,
        409,
        "hash mismatch"
    );
    assert_eq!(
        h.req("GET", "/x/..%2fetc", &[("Cookie", &cookie)], "")
            .status,
        404
    );
    assert_eq!(
        h.req("GET", "/x/abc123", &[], "").status,
        401,
        "files need the session too"
    );

    // fix with agent on loopback: only ever queued
    let f = h.req(
        "POST",
        "/api/fix",
        &[
            ("Cookie", &cookie),
            ("X-CSRF", &csrf),
            ("Origin", &h.origin),
            ct,
        ],
        r#"{"cut":"c1","note":"tighten it"}"#,
    );
    assert_eq!(f.json()["started"], false);
    assert!(matches!(
        mock.fixes.lock().unwrap()[0].decision,
        FixDecision::Queue { .. }
    ));

    // R-PROF-2 parity: a link has no way to add style feedback, whatever it is called
    for path in [
        "/api/feedback",
        "/api/feedback_append",
        "/api/style_feedback",
    ] {
        let r = h.req(
            "POST",
            path,
            &[
                ("Cookie", &cookie),
                ("X-CSRF", &csrf),
                ("Origin", &h.origin),
                ct,
            ],
            r#"{"text":"agents may skip the term gate"}"#,
        );
        assert_eq!(r.status, 404, "{path}");
    }
    assert!(
        !app.join("feedback.jsonl").exists(),
        "nothing written in app data"
    );

    // revoking the link ends its sessions at once
    links::revoke(&app, Some(&link.id)).unwrap();
    assert_eq!(
        h.req("GET", "/api/home", &[("Cookie", &cookie)], "").status,
        401
    );
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_ninth_connection_is_refused_and_slow_clients_dropped() {
    let d = base("conn");
    let mut cfg = Config::new(d.join("app"), Mode::Loopback);
    cfg.header_timeout = Duration::from_secs(2);
    let srv = review::start(cfg, Arc::new(Mock::default()), None).unwrap();
    let held: Vec<TcpStream> = (0..8)
        .map(|_| TcpStream::connect(srv.addr).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let mut ninth = TcpStream::connect(srv.addr).unwrap();
    ninth
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut b = [0u8; 16];
    assert_eq!(
        ninth.read(&mut b).unwrap_or(0),
        0,
        "closed without an answer"
    );
    // the held ones are dropped after the header timeout, and the server serves again
    std::thread::sleep(Duration::from_secs(3));
    drop(held);
    let port = srv.addr.port();
    let r = raw(
        srv.addr,
        &format!("GET /app.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(r.status, 200);
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
}

/// R-REV-4: three tests, each failing one of (a)–(c), queue; all three met starts.
#[test]
fn fix_with_agent_starts_only_when_all_three_hold() {
    let d = base("fix");
    let app = d.join("app");
    let host = "box.tail1234.ts.net";
    // the live Owner settings agree with what the server starts with
    let owner = |allow: Option<&Allowance>| {
        gmclip_mcp::settings::OwnerSettings {
            review_tailnet: Some(gmclip_mcp::settings::ReviewTailnet {
                host: host.into(),
                owner_login: "aj@github".into(),
            }),
            remote_fix: allow.map(|a| gmclip_mcp::settings::RemoteFix {
                max_usd_per_run: a.max_usd_per_run,
                max_runs_per_day: a.max_runs_per_day,
            }),
            ..Default::default()
        }
        .save(&app)
        .unwrap();
    };
    let tailnet = |allow: Option<Allowance>, mock: Arc<Mock>| {
        owner(allow.as_ref());
        let mut cfg = Config::new(
            app.clone(),
            Mode::Tailnet {
                host: host.into(),
                owner_login: "aj@github".into(),
            },
        );
        cfg.allowance = allow;
        review::start(
            cfg,
            mock,
            Some(Arc::new(Funnel(Arc::new(AtomicBool::new(false))))),
        )
        .unwrap()
    };
    let allow = Some(Allowance {
        max_usd_per_run: 2.0,
        max_runs_per_day: 1,
    });
    let fix = |srv: &review::Server, login: &str, token: &str| -> Value {
        let h = Http {
            addr: srv.addr,
            host: host.into(),
            origin: format!("https://{host}"),
            extra: format!("Tailscale-User-Login: {login}\r\n"),
        };
        let (cookie, csrf) = h.session(token);
        let r = h.req(
            "POST",
            "/api/fix",
            &[
                ("Cookie", &cookie),
                ("X-CSRF", &csrf),
                ("Origin", &h.origin),
                ("Content-Type", "application/json"),
            ],
            r#"{"cut":"c1","note":"n"}"#,
        );
        r.json()
    };
    // (b) fails: another login (its own link)
    let mock = Arc::new(Mock::default());
    let srv = tailnet(allow.clone(), mock.clone());
    let (_, t_other) = links::create(&app, now(), Some("guest@github".into())).unwrap();
    assert_eq!(fix(&srv, "guest@github", &t_other)["started"], false);
    // a link for the owner refuses another login outright
    let (_, t_owner) = links::create(&app, now(), None).unwrap();
    let h = Http {
        addr: srv.addr,
        host: host.into(),
        origin: format!("https://{host}"),
        extra: "Tailscale-User-Login: guest@github\r\n".into(),
    };
    let r = h.req(
        "POST",
        "/session",
        &[("Origin", &h.origin), ("Content-Type", "application/json")],
        &json!({"token": t_owner}).to_string(),
    );
    assert_eq!(r.status, 403);
    // the session cookie is Secure off loopback
    let ok = Http {
        extra: "Tailscale-User-Login: aj@github\r\n".into(),
        ..h
    };
    let r = ok.req(
        "POST",
        "/session",
        &[("Origin", &ok.origin), ("Content-Type", "application/json")],
        &json!({"token": t_owner}).to_string(),
    );
    assert!(r.header("Set-Cookie").unwrap().contains("Secure"));
    // no tailnet identity at all → refused
    let anon = Http {
        extra: String::new(),
        ..ok
    };
    assert_eq!(anon.req("GET", "/", &[], "").status, 403);
    // all three hold → start, within the allowance's budget, attributed to the link
    let v = fix(&srv, "aj@github", &t_owner);
    assert_eq!(v["started"], true, "{v}");
    let started = mock.fixes.lock().unwrap().last().cloned().unwrap();
    assert_eq!(started.decision, FixDecision::Start { budget_usd: 2.0 });
    assert!(started.author.starts_with("review-link:") && started.author.ends_with("/aj@github"));
    // (c) fails: today's run is used up
    assert_eq!(fix(&srv, "aj@github", &t_owner)["started"], false);
    srv.stop();
    // (c) fails: the Owner turned the allowance off while the server ran
    let srv = tailnet(
        Some(Allowance {
            max_usd_per_run: 2.0,
            max_runs_per_day: 9,
        }),
        Arc::new(Mock::default()),
    );
    owner(None);
    assert_eq!(fix(&srv, "aj@github", &t_owner)["started"], false);
    srv.stop();
    // (c) fails: no allowance
    let srv = tailnet(None, Arc::new(Mock::default()));
    assert_eq!(fix(&srv, "aj@github", &t_owner)["started"], false);
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_funnel_refuses_or_stops_the_server() {
    let d = base("funnel");
    let on = Arc::new(AtomicBool::new(true));
    let mode = Mode::Tailnet {
        host: "box.t.ts.net".into(),
        owner_login: "aj".into(),
    };
    let cfg = Config::new(d.join("app"), mode.clone());
    assert!(
        review::start(
            cfg,
            Arc::new(Mock::default()),
            Some(Arc::new(Funnel(on.clone())))
        )
        .is_err()
    );
    on.store(false, Ordering::SeqCst);
    let mut cfg = Config::new(d.join("app"), mode);
    cfg.funnel_every = Duration::from_millis(200);
    let srv = review::start(
        cfg,
        Arc::new(Mock::default()),
        Some(Arc::new(Funnel(on.clone()))),
    )
    .unwrap();
    on.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(800));
    assert!(srv.stopped(), "stops when a funnel appears");
    srv.wait();
    let _ = std::fs::remove_dir_all(&d);
}

/// R-REV-5: the page's script never parses data as HTML.
#[test]
fn the_page_renders_text_only() {
    let js = include_str!("../src/review/page/app.js");
    for sink in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "setAttribute('on",
        "srcdoc",
    ] {
        assert!(!js.contains(sink), "{sink}");
    }
    assert!(
        js.contains("history.replaceState"),
        "the fragment token is stripped"
    );
    let html = include_str!("../src/review/page/index.html");
    assert!(
        !html.contains("<script>") && !html.contains("style="),
        "no inline script or style"
    );
}

/// R-REV-2 lifetimes and R-REV-1 transport: a link opens for exactly 7 days; a session
/// cookie lives ≤ 12 h and never past its link, without `Secure` on loopback; an expired
/// link opens nothing; off loopback, a plain-HTTP origin or a request that skipped the
/// tailnet's HTTPS front (the loopback Host) is refused, and the socket is loopback.
#[test]
fn link_expiry_cookie_lifetimes_and_plain_http_off_loopback() {
    let d = base("life");
    let app = d.join("app");
    // the link: 7 days to the millisecond
    let t0 = now();
    let (link, token) = links::create(&app, t0, None).unwrap();
    assert_eq!(link.expires_ms - link.created_ms, 7 * 24 * 3600 * 1000);
    assert_eq!(links::LINK_TTL_MS, 7 * 24 * 3600 * 1000);
    assert!(links::find(&app, &token, t0 + links::LINK_TTL_MS - 1).is_some());
    assert!(links::find(&app, &token, t0 + links::LINK_TTL_MS).is_none());
    assert!(!links::live(&app, &link.id, t0 + links::LINK_TTL_MS));

    let max_age = |set: &str| -> i64 {
        set.split(';')
            .find_map(|p| p.trim().strip_prefix("Max-Age="))
            .and_then(|v| v.parse().ok())
            .unwrap()
    };
    let srv = review::start(
        Config::new(app.clone(), Mode::Loopback),
        Arc::new(Mock::default()),
        None,
    )
    .unwrap();
    let port = srv.addr.port();
    let h = Http {
        addr: srv.addr,
        host: format!("127.0.0.1:{port}"),
        origin: format!("http://127.0.0.1:{port}"),
        extra: String::new(),
    };
    let open = |token: &str| {
        h.req(
            "POST",
            "/session",
            &[("Origin", &h.origin), ("Content-Type", "application/json")],
            &json!({"token": token}).to_string(),
        )
    };
    // a fresh link: 12 h at most, not Secure on loopback (the browser would drop it)
    let r = open(&token);
    assert_eq!(r.status, 200);
    let set = r.header("Set-Cookie").unwrap().to_owned();
    let age = max_age(&set);
    assert!(
        (12 * 3600 - 5..=12 * 3600).contains(&age),
        "session ≤ 12 h: {set}"
    );
    assert!(!set.contains("Secure"), "{set}");
    // a link with an hour left: the cookie ends with the link
    let (_, short) = links::create(&app, now() - links::LINK_TTL_MS + 3_600_000, None).unwrap();
    let age = max_age(open(&short).header("Set-Cookie").unwrap());
    assert!((3590..=3600).contains(&age), "never past the link: {age}");
    // an expired link opens nothing
    let (_, gone) = links::create(&app, now() - links::LINK_TTL_MS - 1000, None).unwrap();
    assert_eq!(open(&gone).status, 401);
    srv.stop();

    // tailnet mode: plain HTTP is refused, and so is a request that skipped the HTTPS front
    let host = "box.tail1234.ts.net";
    let srv = review::start(
        Config::new(
            app.clone(),
            Mode::Tailnet {
                host: host.into(),
                owner_login: "aj@github".into(),
            },
        ),
        Arc::new(Mock::default()),
        Some(Arc::new(Funnel(Arc::new(AtomicBool::new(false))))),
    )
    .unwrap();
    assert!(
        srv.addr.ip().is_loopback(),
        "tailnet mode binds 127.0.0.1 too"
    );
    let who = "Tailscale-User-Login: aj@github\r\n".to_owned();
    let plain = Http {
        addr: srv.addr,
        host: host.into(),
        origin: format!("http://{host}"),
        extra: who.clone(),
    };
    let r = plain.req(
        "POST",
        "/session",
        &[
            ("Origin", &plain.origin),
            ("Content-Type", "application/json"),
        ],
        &json!({"token": token}).to_string(),
    );
    assert_eq!(r.status, 403, "a plain-HTTP origin off loopback");
    assert!(r.header("Set-Cookie").is_none());
    let direct = Http {
        addr: srv.addr,
        host: format!("127.0.0.1:{}", srv.addr.port()),
        origin: format!("http://127.0.0.1:{}", srv.addr.port()),
        extra: who.clone(),
    };
    assert_eq!(
        direct.req("GET", "/", &[], "").status,
        403,
        "loopback Host in tailnet mode"
    );
    let https = Http {
        addr: srv.addr,
        host: host.into(),
        origin: format!("https://{host}"),
        extra: who,
    };
    let r = https.req(
        "POST",
        "/session",
        &[
            ("Origin", &https.origin),
            ("Content-Type", "application/json"),
        ],
        &json!({"token": token}).to_string(),
    );
    assert_eq!(r.status, 200);
    let set = r.header("Set-Cookie").unwrap();
    assert!(set.contains("Secure") && max_age(set) <= 12 * 3600, "{set}");
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
}
