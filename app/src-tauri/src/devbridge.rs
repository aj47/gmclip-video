//! The dev bridge (SPEC §12.1, R-DEV-1): `gm_call` and `gm_subscribe` over a loopback
//! WebSocket so the UI runs in a browser for Playwright and agent-driven UI work.
//!
//! Compiled only with the `dev-bridge` feature, which `build.rs` refuses in the release
//! profile. It binds `127.0.0.1:0`, prints `GMCLIP_DEV_BRIDGE {"port":…}` on stdout
//! (the token goes to stdout too, for the harness that launched it, never in a URL),
//! accepts only the harness's exact `Origin` and `Host: 127.0.0.1:<port>`, requires the
//! 256-bit token as the first message, serves one client at a time, and exposes nothing
//! the webview could not reach (Owner commands are not in the registry).
//!
//! One client at a time means **the newest**: a client that authenticates ends the
//! session before it (its socket is closed, its subscriptions dropped). Before this, a
//! session held the accept loop until its page went away, so a second page (a reload, or
//! the next Playwright spec while the previous spec's page was still open) sat in the
//! listen backlog with its WebSocket never opened: every call it made waited on the
//! connection and was never sent ("Opening…" forever on Home).
//!
//! Wire (JSON text): `{"id": n, "call": {command, args, project?}}` →
//! `{"id": n, "reply": {...}}`; `{"id": n, "subscribe": "<scope>"}` → a reply with the
//! subscription id, then `{"event": {sub, scope, events}}` pushes. `{"id": n, "data":
//! "gm://data/…"}` (the binary columns, D.02) → one binary message: `n` as a
//! little-endian u32, then exactly what the app's `gm://` scheme answers (nothing after
//! the id when it answers nothing); the same parser and kinds, so a page under the
//! bridge reads the columns the webview reads.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};

use crate::frame::{check_handshake, ct_eq, hex, token};
use crate::ipc::{CallRequestWire, Ctx, check_scope, dispatch, pump};

/// One message to the client.
enum Out {
    Text(Value),
    /// A `data` answer: the request id (u32 LE), then the `gm://data` body.
    Binary(Vec<u8>),
}

/// Starts the bridge; returns `(port, token hex)`.
pub fn start(ctx: Ctx, origin: String) -> std::io::Result<(u16, String)> {
    let l = TcpListener::bind(("127.0.0.1", 0))?;
    let port = l.local_addr()?.port();
    let tok =
        token().ok_or_else(|| std::io::Error::other("no randomness for the dev bridge token"))?;
    let tok_hex = hex(&tok);
    let expect = Arc::new(tok_hex.clone());
    // The session generation: a session ends as soon as a newer client authenticates.
    let current = Arc::new(AtomicU64::new(0));
    std::thread::Builder::new()
        .name("gmclip-dev-bridge".into())
        .spawn(move || {
            for stream in l.incoming() {
                let Ok(stream) = stream else {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                // Each connection authenticates on its own thread, so a slow or silent
                // client never holds the accept loop.
                let (ctx, origin, expect, current) = (
                    ctx.clone(),
                    origin.clone(),
                    Arc::clone(&expect),
                    Arc::clone(&current),
                );
                let _ = std::thread::Builder::new()
                    .name("gmclip-dev-bridge-session".into())
                    .spawn(move || session(stream, &ctx, &origin, port, &expect, &current));
            }
        })?;
    Ok((port, tok_hex))
}

/// One client: handshake, token, then calls and subscriptions until it closes or a newer
/// client takes over.
// tungstenite's handshake callback returns its own (large) `ErrorResponse`.
#[allow(clippy::result_large_err)]
fn session(
    stream: TcpStream,
    ctx: &Ctx,
    origin: &str,
    port: u16,
    expect: &str,
    current: &AtomicU64,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let cb = |req: &Request, resp: Response| match check_handshake(req, origin, port) {
        Ok(()) => Ok(resp),
        Err(_) => {
            let mut r = ErrorResponse::new(None);
            *r.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            Err(r)
        }
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, cb) else {
        eprintln!("gmclip: dev bridge refused a connection (origin or host)");
        return;
    };
    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_secs(5)));
    let ok = matches!(ws.read(), Ok(Message::Text(t)) if ct_eq(t.as_bytes(), expect.as_bytes()));
    if !ok {
        eprintln!("gmclip: dev bridge refused a connection (token)");
        let _ = ws.close(None);
        return;
    }
    // this client is the one served from now on; the one before ends at its next turn
    let me = current.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = ws
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(2)));
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    let mut subs = Vec::new();
    'session: loop {
        if current.load(Ordering::SeqCst) != me {
            let _ = ws.close(None);
            let _ = ws.flush();
            break 'session;
        }
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    let Ok(msg) = serde_json::from_str::<Value>(t.as_str()) else {
                        break 'session;
                    };
                    let id = msg.get("id").cloned().unwrap_or(Value::Null);
                    if let Some(call) = msg.get("call") {
                        let reply = match serde_json::from_value::<CallRequestWire>(call.clone()) {
                            // No Owner command or review-link setting runs from the
                            // dev bridge (R-OWN-1b, U.10, U.11), even with a window.
                            Ok(req)
                                if crate::owner::dev_bridge_refuses(&req.command, &req.args) =>
                            {
                                json!({"ok": false, "error": {"code": "Unauthorized", "scope": "owner", "message": "no Owner command runs from the dev bridge"}})
                            }
                            Ok(req) => {
                                // Calls can block (imports): run them aside.
                                let ctx = ctx.clone();
                                let tx = out_tx.clone();
                                std::thread::spawn(move || {
                                    let r = dispatch(&ctx, req);
                                    let _ = tx.send(Out::Text(json!({"id": id, "reply": r})));
                                });
                                continue;
                            }
                            Err(_) => {
                                json!({"ok": false, "error": {"code": "Invalid", "message": "bad call"}})
                            }
                        };
                        let _ = out_tx.send(Out::Text(json!({"id": id, "reply": reply})));
                    } else if let Some(uri) = msg.get("data").and_then(Value::as_str) {
                        // binary columns: answered aside (a filmstrip may take a while)
                        let (ctx, tx, uri) = (ctx.clone(), out_tx.clone(), uri.to_owned());
                        let n = id.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
                        std::thread::spawn(move || {
                            let body = crate::ipc::gm_data(&ctx.core, &uri).unwrap_or_default();
                            let mut b = Vec::with_capacity(4 + body.len());
                            b.extend_from_slice(&n.to_le_bytes());
                            b.extend_from_slice(&body);
                            let _ = tx.send(Out::Binary(b));
                        });
                    } else if let Some(scope) = msg.get("subscribe").and_then(Value::as_str) {
                        let reply = match check_scope(&ctx.core, scope) {
                            Ok(()) => {
                                let tx = out_tx.clone();
                                let sid = pump(ctx.core.clone(), scope.to_owned(), move |v| {
                                    tx.send(Out::Text(json!({"event": v}))).is_ok()
                                });
                                subs.push(sid);
                                json!({"ok": true, "output": sid})
                            }
                            Err(e) => json!({"ok": false, "error": e}),
                        };
                        let _ = out_tx.send(Out::Text(json!({"id": id, "reply": reply})));
                    }
                }
                Ok(Message::Close(_)) => break 'session,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => break 'session,
            }
        }
        match out_rx.recv_timeout(Duration::from_millis(4)) {
            Ok(v) => {
                let mut batch = vec![v];
                while let Ok(more) = out_rx.try_recv() {
                    batch.push(more);
                }
                for v in batch {
                    let m = match v {
                        Out::Text(v) => Message::Text(v.to_string().into()),
                        Out::Binary(b) => Message::Binary(b.into()),
                    };
                    if ws.send(m).is_err() {
                        break 'session;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'session,
        }
    }
    for s in subs {
        ctx.core.bus.unsubscribe(s);
    }
}
