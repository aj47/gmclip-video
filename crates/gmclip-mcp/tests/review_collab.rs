//! Decision 0015 on the review link, over real TCP against a mock catalog: comments,
//! replies and resolve from a link (as the link, canonical text, CSRF like every write);
//! an editor's link comments and suggests edits only; "Approve" hands an export to Clippy
//! only under tailnet + the Owner's own login + the Owner's remote-approve allowance, and
//! queues it for a click in the app otherwise.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gmclip_mcp::review::body::{CommentBody, ResolveBody, SuggestBody, Suggestion};
use gmclip_mcp::review::{
    self, ApproveDecision, ApproveRequest, Catalog, Config, FileRec, FixRequest, Mode, Tailnet,
    links,
};
use gmclip_mcp::settings::{OwnerSettings, RemoteSend, ReviewTailnet};
use serde_json::{Value, json};

fn base(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("gmclip-rcollab-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[derive(Default)]
struct Mock {
    comments: Mutex<Vec<(String, CommentBody)>>,
    resolves: Mutex<Vec<(String, ResolveBody)>>,
    suggestions: Mutex<Vec<(String, SuggestBody)>>,
    approvals: Mutex<Vec<ApproveRequest>>,
    reviews: Mutex<usize>,
}

impl Catalog for Mock {
    fn home(&self) -> Value {
        json!({"project": "p", "exports": [], "cuts": [], "moments": []})
    }
    fn export(&self, _id: &str) -> Option<FileRec> {
        None
    }
    fn clip(&self, _id: &str) -> Option<FileRec> {
        None
    }
    fn add_review(
        &self,
        _a: &str,
        _c: &str,
        _t: &str,
        _v: &str,
        _e: Option<&str>,
    ) -> Result<String, String> {
        *self.reviews.lock().unwrap() += 1;
        Ok("r".into())
    }
    fn fix(&self, _req: &FixRequest) -> Result<Value, String> {
        Ok(json!({}))
    }
    fn comment(&self, author: &str, c: &CommentBody) -> Result<String, String> {
        self.comments
            .lock()
            .unwrap()
            .push((author.into(), c.clone()));
        Ok("c1".into())
    }
    fn resolve(&self, author: &str, r: &ResolveBody) -> Result<(), String> {
        self.resolves
            .lock()
            .unwrap()
            .push((author.into(), r.clone()));
        Ok(())
    }
    fn suggest(&self, author: &str, s: &SuggestBody) -> Result<String, String> {
        self.suggestions
            .lock()
            .unwrap()
            .push((author.into(), s.clone()));
        Ok("s1".into())
    }
    fn approve(&self, req: &ApproveRequest) -> Result<Value, String> {
        self.approvals.lock().unwrap().push(req.clone());
        Ok(
            json!({"status": match req.decision { ApproveDecision::Send => "sent", _ => "needs_you" }}),
        )
    }
}

struct NoFunnel;
impl Tailnet for NoFunnel {
    fn funnel_covers(&self, _port: u16) -> Result<bool, String> {
        Ok(false)
    }
}

struct Http {
    addr: SocketAddr,
    host: String,
    origin: String,
    extra: String,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
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
        if method == "POST" {
            r.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        r.push_str("\r\n");
        r.push_str(body);
        let mut s = TcpStream::connect(self.addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(r.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let end = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map_or(buf.len(), |i| i + 4);
        let status = String::from_utf8_lossy(&buf[..end])
            .split(' ')
            .nth(1)
            .and_then(|x| x.parse().ok())
            .unwrap_or(0);
        Resp {
            status,
            body: buf[end..].to_vec(),
        }
    }
    /// `(cookie, csrf, home)` for a token (the cookie read from the raw Set-Cookie).
    fn session(&self, token: &str) -> (String, String, Value) {
        let mut s = TcpStream::connect(self.addr).unwrap();
        let body = json!({"token": token}).to_string();
        let req = format!(
            "POST /session HTTP/1.1\r\nHost: {}\r\n{}Origin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.host,
            self.extra,
            self.origin,
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let head = String::from_utf8_lossy(&buf).into_owned();
        let cookie = head
            .lines()
            .find_map(|l| l.strip_prefix("Set-Cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let home = self.req("GET", "/api/home", &[("Cookie", &cookie)], "");
        assert_eq!(home.status, 200);
        let h = home.json();
        (cookie, h["csrf"].as_str().unwrap().to_owned(), h)
    }
    fn post(&self, s: &(String, String, Value), path: &str, body: Value) -> Resp {
        self.req(
            "POST",
            path,
            &[
                ("Cookie", &s.0),
                ("X-CSRF", &s.1),
                ("Origin", &self.origin),
                ("Content-Type", "application/json"),
            ],
            &body.to_string(),
        )
    }
}

#[test]
fn comments_suggestions_and_roles_on_loopback() {
    let d = base("roles");
    let app = d.join("app");
    let mock = Arc::new(Mock::default());
    let srv = review::start(Config::new(app.clone(), Mode::Loopback), mock.clone(), None).unwrap();
    let port = srv.addr.port();
    let h = Http {
        addr: srv.addr,
        host: format!("127.0.0.1:{port}"),
        origin: format!("http://127.0.0.1:{port}"),
        extra: String::new(),
    };

    // a reviewer's link
    let (link, token) = links::create(&app, now(), None).unwrap();
    let s = h.session(&token);
    assert_eq!(s.2["role"], "reviewer");
    let c = h.post(
        &s,
        "/api/comment",
        json!({"cut": "c1", "text": "unreadable\u{E0041}\u{200B} here <b>", "t": 12.5, "box": {"x": 0.1, "y": 0.2, "w": 0.3, "h": 0.1}, "export_ref": "abc123"}),
    );
    assert_eq!(c.status, 200, "{}", String::from_utf8_lossy(&c.body));
    {
        let g = mock.comments.lock().unwrap();
        assert_eq!(g[0].0, format!("review-link:{}", link.id));
        // Tag and zero-width characters never reach the project (R-INJ-6); markup is text
        assert_eq!(g[0].1.text, "unreadable here <b>");
        assert_eq!(g[0].1.t, Some(12.5));
    }
    // a reply, a resolve
    assert_eq!(
        h.post(
            &s,
            "/api/comment",
            json!({"cut": "c1", "text": "agreed", "reply_to": "c1"})
        )
        .status,
        200
    );
    assert_eq!(
        h.post(
            &s,
            "/api/resolve",
            json!({"comment": "c1", "resolved": true})
        )
        .status,
        200
    );
    // malformed bodies are refused before the catalog sees them
    for bad in [
        json!({"cut": "c1", "text": ""}),
        json!({"cut": "../c1", "text": "x"}),
        json!({"cut": "c1", "text": "x", "t": -3}),
        json!({"cut": "c1", "text": "x", "box": {"x": 0.9, "y": 0.0, "w": 0.5, "h": 0.5}}),
        json!({"cut": "c1", "text": "x", "path": "/etc"}),
    ] {
        assert_eq!(h.post(&s, "/api/comment", bad).status, 400);
    }
    // a write without CSRF is refused, as every write
    assert_eq!(
        h.req(
            "POST",
            "/api/comment",
            &[
                ("Cookie", &s.0),
                ("Origin", &h.origin),
                ("Content-Type", "application/json")
            ],
            r#"{"cut":"c1","text":"x"}"#
        )
        .status,
        403
    );
    // a reviewer does not suggest edits
    assert_eq!(
        h.post(
            &s,
            "/api/suggest",
            json!({"cut": "c1", "kind": "strike", "from_s": 1, "to_s": 2})
        )
        .status,
        403
    );
    // approve on loopback: queued for the app, never sent
    let a = h.post(&s, "/api/approve", json!({"export": "abc123"}));
    assert_eq!(a.json()["sent"], false);
    assert!(matches!(
        mock.approvals.lock().unwrap()[0].decision,
        ApproveDecision::Queue { .. }
    ));

    // an editor's link: comments and suggestions only
    let (elink, etoken) =
        links::create_with_role(&app, now(), None, Some("editor".into())).unwrap();
    assert!(links::create_with_role(&app, now(), None, Some("owner".into())).is_err());
    let e = h.session(&etoken);
    assert_eq!(e.2["role"], "editor");
    for (path, body) in [
        (
            "/api/review",
            json!({"cut": "c1", "text": "x", "verdict": "keep"}),
        ),
        ("/api/fix", json!({"cut": "c1", "note": "x"})),
        ("/api/approve", json!({"export": "abc123"})),
    ] {
        assert_eq!(h.post(&e, path, body).status, 403, "{path}");
    }
    assert_eq!(*mock.reviews.lock().unwrap(), 0);
    let sg = h.post(
        &e,
        "/api/suggest",
        json!({"cut": "c1", "kind": "trim", "part": "p1", "head_s": 0.4, "note": "breath\u{E0020}", "comment": "c1"}),
    );
    assert_eq!(sg.status, 200);
    let g = mock.suggestions.lock().unwrap();
    assert_eq!(g[0].0, format!("review-link:{}", elink.id));
    assert_eq!(g[0].1.note, "breath");
    assert_eq!(
        g[0].1.edit,
        Suggestion::Trim {
            part: "p1".into(),
            head_s: 0.4,
            tail_s: 0.0
        }
    );
    drop(g);
    assert_eq!(
        h.post(
            &e,
            "/api/suggest",
            json!({"cut": "c1", "kind": "caption_move", "y": 2})
        )
        .status,
        400
    );
    assert_eq!(
        h.post(&e, "/api/comment", json!({"cut": "c1", "text": "nice"}))
            .status,
        200
    );
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
}

/// Approve sends only when (a) tailnet, (b) the Owner's own login, (c) the Owner's
/// remote-approve allowance has a send left today; each failing one queues.
#[test]
fn approve_sends_only_under_all_three() {
    let d = base("approve");
    let app = d.join("app");
    let host = "box.tail1234.ts.net";
    let settings = |send: Option<u32>| {
        OwnerSettings {
            review_tailnet: Some(ReviewTailnet {
                host: host.into(),
                owner_login: "aj@github".into(),
            }),
            remote_send: send.map(|n| RemoteSend {
                max_sends_per_day: n,
            }),
            ..Default::default()
        }
        .save(&app)
        .unwrap();
    };
    settings(Some(1));
    let mock = Arc::new(Mock::default());
    let srv = review::start(
        Config::new(
            app.clone(),
            Mode::Tailnet {
                host: host.into(),
                owner_login: "aj@github".into(),
            },
        ),
        mock.clone(),
        Some(Arc::new(NoFunnel)),
    )
    .unwrap();
    let as_ = |login: &str| Http {
        addr: srv.addr,
        host: host.into(),
        origin: format!("https://{host}"),
        extra: format!("Tailscale-User-Login: {login}\r\n"),
    };
    let approve = |login: &str, token: &str| -> Value {
        let h = as_(login);
        let s = h.session(token);
        h.post(&s, "/api/approve", json!({"export": "abc123"}))
            .json()
    };
    // (b) fails: a guest's own link
    let (_, guest) = links::create(&app, now(), Some("guest@github".into())).unwrap();
    assert_eq!(approve("guest@github", &guest)["sent"], false);
    // (c) fails: no allowance
    let (_, mine) = links::create(&app, now(), None).unwrap();
    settings(None);
    let r = approve("aj@github", &mine);
    assert_eq!(r["sent"], false);
    assert!(
        r["reasons"][0].as_str().unwrap().contains("allowance"),
        "{r}"
    );
    // all three: sent, attributed to the link and the login
    settings(Some(1));
    let r = approve("aj@github", &mine);
    assert_eq!(r["sent"], true, "{r}");
    let last = mock.approvals.lock().unwrap().last().unwrap().clone();
    assert!(matches!(last.decision, ApproveDecision::Send));
    assert!(last.author.ends_with("/aj@github"));
    // (c) again: today's one send is used
    let r = approve("aj@github", &mine);
    assert_eq!(r["sent"], false);
    assert!(r["reasons"][0].as_str().unwrap().contains("used up"));
    // (a) fails: tailnet mode turned off in the live settings
    OwnerSettings {
        remote_send: Some(RemoteSend {
            max_sends_per_day: 9,
        }),
        ..Default::default()
    }
    .save(&app)
    .unwrap();
    let r = approve("aj@github", &mine);
    assert_eq!(r["sent"], false);
    let sent = mock
        .approvals
        .lock()
        .unwrap()
        .iter()
        .filter(|a| matches!(a.decision, ApproveDecision::Send))
        .count();
    assert_eq!(sent, 1);
    // an unreadable ledger counts as used up (fail closed)
    std::fs::remove_file(app.join("remote-send.jsonl")).unwrap();
    std::fs::create_dir(app.join("remote-send.jsonl")).unwrap();
    settings(Some(5));
    assert_eq!(approve("aj@github", &mine)["sent"], false);
    srv.stop();
    let _ = std::fs::remove_dir_all(&d);
    let _ = AtomicBool::new(false);
}
