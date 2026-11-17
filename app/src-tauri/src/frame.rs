//! The frame socket (SPEC D5, §6.3; R-FRAME-1, R-PROTO-7).
//!
//! A loopback WebSocket bound to `127.0.0.1:0` on its own thread. It accepts exactly
//! **one** connection per launch: the handshake must carry the exact webview `Origin`
//! (no prefix or suffix logic) and `Host: 127.0.0.1:<port>`, and the first message must
//! be the 32-byte token, compared in constant time. The token reaches the webview only
//! through `gm_call` (never argv, env, a file or a URL). Any later connection is refused
//! and logged.
//!
//! Downstream: binary frames, a 32-byte header then the NV12 planes ([`encode_frame`]).
//! Upstream: 32-byte viewer-input records ([`Record`]); anything else closes the socket.
//! Records drive viewer manipulation only; they can never mint a nonce or reach an
//! Owner command (they are not commands at all).

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

/// Frame header magic: `GMF1` little-endian.
pub const MAGIC: u32 = u32::from_le_bytes(*b"GMF1");
pub use crate::framerec::RECORD;

pub use crate::framerec::{Record, RecordKind, coalesce, keys};

/// One frame for the socket.
#[derive(Clone, Debug)]
pub struct OutFrame {
    pub seq: u32,
    pub out_frame: u32,
    pub width: u16,
    pub height: u16,
    /// `t` of the newest input record this frame reflects (0 when none): the UI measures
    /// input-to-photon from it.
    pub echo_t: f64,
    pub render_us: u32,
    pub flags: u16,
    /// Frames dropped since the session began, mod 2^16 (the UI takes differences).
    pub dropped: u16,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

/// A one-frame mailbox between the render thread and the socket: a newer frame replaces
/// an unsent older one (late frames drop, never queue), and nothing is ever refused.
#[derive(Debug, Default)]
pub struct FrameSlot {
    slot: Mutex<Option<OutFrame>>,
    cv: std::sync::Condvar,
}

impl FrameSlot {
    /// Puts `f`, replacing an unsent frame. Returns whether one was replaced (dropped).
    pub fn put(&self, f: OutFrame) -> bool {
        let mut g = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replaced = g.replace(f).is_some();
        self.cv.notify_all();
        replaced
    }

    /// Takes the frame, waiting up to `timeout`.
    pub fn take(&self, timeout: Duration) -> Option<OutFrame> {
        let g = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |s| s.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.take()
    }
}

/// Most records one read pass takes before serving a frame (a flood cannot starve frames).
pub const RECORDS_PER_PASS: usize = 256;
/// Largest upstream message before and after the token (records only; 64 KiB = 2048).
pub const MAX_UPSTREAM: usize = 64 * 1024;

/// Header flags.
pub const FLAG_PLAYING: u16 = 1;
pub const FLAG_FULL: u16 = 2;

/// Header (32 bytes) + Y + UV.
pub fn encode_frame(f: &OutFrame) -> Vec<u8> {
    let mut b = Vec::with_capacity(RECORD + f.y.len() + f.uv.len());
    b.extend_from_slice(&MAGIC.to_le_bytes());
    b.extend_from_slice(&f.seq.to_le_bytes());
    b.extend_from_slice(&f.out_frame.to_le_bytes());
    b.extend_from_slice(&f.width.to_le_bytes());
    b.extend_from_slice(&f.height.to_le_bytes());
    b.extend_from_slice(&f.echo_t.to_le_bytes());
    b.extend_from_slice(&f.render_us.to_le_bytes());
    b.extend_from_slice(&f.flags.to_le_bytes());
    b.extend_from_slice(&f.dropped.to_le_bytes());
    b.extend_from_slice(&f.y);
    b.extend_from_slice(&f.uv);
    b
}

/// Constant-time equality.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A fresh 256-bit token, or `None` when the OS has no randomness (then nothing serves).
pub fn token() -> Option<[u8; 32]> {
    let mut t = [0u8; 32];
    getrandom::fill(&mut t).ok().map(|()| t)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Checks the handshake: exact `Origin`, exact `Host`.
pub fn check_handshake(req: &Request, origin: &str, port: u16) -> Result<(), &'static str> {
    let h = |k: &str| req.headers().get(k).and_then(|v| v.to_str().ok());
    if h("origin") != Some(origin) {
        return Err("origin");
    }
    if h("host") != Some(format!("127.0.0.1:{port}").as_str()) {
        return Err("host");
    }
    Ok(())
}

fn refuse(code: u16) -> ErrorResponse {
    let mut r = ErrorResponse::new(None);
    *r.status_mut() = tungstenite::http::StatusCode::from_u16(code)
        .unwrap_or(tungstenite::http::StatusCode::FORBIDDEN);
    r
}

/// The frame socket's public facts and its channels.
/// R-FRAME-1: frames stop when the window is hidden or minimised for longer than
/// [`HiddenGate::limit`] (60 s): the session closes the socket, so nothing renders (the
/// viewer renders only for a connected client) and, one connection per launch, showing
/// the window again needs a relaunch.
#[derive(Debug)]
pub struct HiddenGate {
    since: Mutex<Option<std::time::Instant>>,
    pub limit: Duration,
    pub closed: AtomicBool,
    /// Dev bridge builds only: expire now (the e2e cycle of hide → close → show).
    pub force: AtomicBool,
}

impl HiddenGate {
    pub fn new(limit: Duration) -> Self {
        Self {
            since: Mutex::new(None),
            limit,
            closed: AtomicBool::new(false),
            force: AtomicBool::new(false),
        }
    }

    /// The window is hidden or minimised (`true`) or showing again (`false`).
    pub fn set(&self, hidden: bool) {
        if let Ok(mut g) = self.since.lock() {
            match (hidden, g.is_some()) {
                (true, false) => *g = Some(std::time::Instant::now()),
                (false, _) => *g = None,
                _ => {}
            }
        }
    }

    /// Hidden for longer than the limit.
    pub fn expired(&self) -> bool {
        if self.force.load(Ordering::SeqCst) {
            return true;
        }
        self.since
            .lock()
            .ok()
            .and_then(|g| *g)
            .is_some_and(|t| t.elapsed() > self.limit)
    }
}

/// The live frame session's credential (decision 0010, amending R-FRAME-1): one token and
/// one live connection at a time. A session closed because the window stayed hidden is
/// re-armed when the window shows again: a fresh token (the old one never works again)
/// and room for exactly one new connection, which the UI makes on its own.
#[derive(Debug)]
pub struct FrameSession {
    token: Mutex<[u8; 32]>,
    used: AtomicBool,
    generation: std::sync::atomic::AtomicU64,
}

impl FrameSession {
    pub(crate) fn new(token: [u8; 32]) -> Self {
        Self {
            token: Mutex::new(token),
            used: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn token(&self) -> [u8; 32] {
        self.token.lock().map(|t| *t).unwrap_or([0; 32])
    }

    /// The current token as hex and its generation (`gm_call app_frame_socket` only).
    pub fn current(&self) -> (String, u64) {
        (hex(&self.token()), self.generation.load(Ordering::SeqCst))
    }

    /// A fresh token and room for one new connection; `false` (and nothing changes)
    /// when the OS has no randomness.
    pub fn rearm(&self) -> bool {
        let Some(t) = token() else { return false };
        if let Ok(mut g) = self.token.lock() {
            *g = t;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.used.store(false, Ordering::SeqCst);
        true
    }
}

/// How long a hidden window keeps its frame socket.
pub const HIDDEN_LIMIT: Duration = Duration::from_secs(60);

pub struct FrameServer {
    pub port: u16,
    pub hidden: Arc<HiddenGate>,
    pub origin: String,
    pub session: Arc<FrameSession>,
    pub connected: Arc<AtomicBool>,
    pub refused: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for FrameServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameServer")
            .field("port", &self.port)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl FrameServer {
    /// Binds `127.0.0.1:0` (nothing accepted until [`Self::serve`]).
    pub fn bind(origin: &str) -> std::io::Result<(FrameServer, TcpListener)> {
        let token = token()
            .ok_or_else(|| std::io::Error::other("no randomness for the frame socket token"))?;
        let l = TcpListener::bind(("127.0.0.1", 0))?;
        let port = l.local_addr()?.port();
        Ok((
            FrameServer {
                port,
                hidden: Arc::new(HiddenGate::new(HIDDEN_LIMIT)),
                origin: origin.to_owned(),
                session: Arc::new(FrameSession::new(token)),
                connected: Arc::new(AtomicBool::new(false)),
                refused: Arc::new(Mutex::new(Vec::new())),
            },
            l,
        ))
    }

    /// The token as hex, for `gm_call app_frame_socket` only.
    pub fn token_hex(&self) -> String {
        self.session.current().0
    }

    // tungstenite's handshake callback returns its own (large) `ErrorResponse`.
    #[allow(clippy::result_large_err)]
    /// Accepts on its own thread: the first valid client gets frames from `frames` and
    /// its records go to `records`; every later connection is refused.
    pub fn serve(
        &self,
        listener: TcpListener,
        frames: Arc<FrameSlot>,
        records: SyncSender<Vec<Record>>,
    ) -> std::io::Result<()> {
        let (port, origin, creds) = (self.port, self.origin.clone(), self.session.clone());
        let (connected, refused, hidden) = (
            self.connected.clone(),
            self.refused.clone(),
            self.hidden.clone(),
        );
        std::thread::Builder::new()
            .name("gmclip-frame-socket".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        // e.g. EMFILE: back off rather than spin
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    let log = |why: &str| {
                        if let Ok(mut g) = refused.lock() {
                            g.push(why.to_owned());
                        }
                        eprintln!("gmclip: frame socket refused a connection ({why})");
                    };
                    if creds.used.load(Ordering::SeqCst) {
                        log("second connection");
                        drop(stream);
                        continue;
                    }
                    let _ = stream.set_nodelay(true);
                    // A silent or slow client cannot hold the accept loop (R-PROTO-7).
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let cb =
                        |req: &Request, resp: Response| match check_handshake(req, &origin, port) {
                            Ok(()) => Ok(resp),
                            Err(_) => Err(refuse(403)),
                        };
                    let cfg = tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(MAX_UPSTREAM))
                        .max_frame_size(Some(MAX_UPSTREAM));
                    let mut ws = match tungstenite::accept_hdr_with_config(stream, cb, Some(cfg)) {
                        Ok(ws) => ws,
                        Err(_) => {
                            log("bad handshake (origin, host or protocol)");
                            continue;
                        }
                    };
                    // First message: the token.
                    let token = creds.token();
                    let ok = matches!(ws.read(), Ok(Message::Binary(b)) if ct_eq(&b, &token));
                    if !ok {
                        log("bad or missing token");
                        let _ = ws.close(None);
                        continue;
                    }
                    // the slot may have been taken meanwhile (two clients racing the token)
                    if creds.used.swap(true, Ordering::SeqCst) {
                        log("second connection");
                        let _ = ws.close(None);
                        continue;
                    }
                    connected.store(true, Ordering::SeqCst);
                    // The session runs beside the accept loop, which keeps refusing.
                    let (frames, records, connected, hidden) = (
                        frames.clone(),
                        records.clone(),
                        connected.clone(),
                        hidden.clone(),
                    );
                    let _ = std::thread::Builder::new()
                        .name("gmclip-frame-session".into())
                        .spawn(move || {
                            session(&mut ws, &frames, &records, &hidden);
                            connected.store(false, Ordering::SeqCst);
                        });
                }
            })?;
        Ok(())
    }
}

fn session(
    ws: &mut WebSocket<TcpStream>,
    frames: &FrameSlot,
    records: &SyncSender<Vec<Record>>,
    hidden: &HiddenGate,
) {
    let _ = ws
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(1)));
    // Records the render thread has not taken yet (it was busy): kept, coalesced.
    let mut pending: Vec<Record> = Vec::new();
    loop {
        if hidden.expired() {
            eprintln!(
                "gmclip: frame socket closed: the window was hidden for over {} s (R-FRAME-1)",
                hidden.limit.as_secs()
            );
            hidden.closed.store(true, Ordering::SeqCst);
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        // Upstream: up to RECORDS_PER_PASS records, then serve a frame.
        let mut n = 0;
        while n < RECORDS_PER_PASS {
            match ws.read() {
                Ok(Message::Binary(b)) => {
                    if b.len() % RECORD != 0 {
                        let _ = ws.close(None);
                        return;
                    }
                    for c in b.chunks(RECORD) {
                        match Record::parse(c) {
                            Some(r) => pending.push(r),
                            None => {
                                let _ = ws.close(None);
                                return;
                            }
                        }
                        n += 1;
                    }
                }
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Ok(Message::Close(_)) => return,
                Ok(Message::Text(_)) => {
                    let _ = ws.close(None);
                    return;
                }
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => return,
            }
        }
        if !pending.is_empty() {
            coalesce(&mut pending);
            match records.try_send(std::mem::take(&mut pending)) {
                Ok(()) => {}
                Err(TrySendError::Full(back)) => pending = back,
                Err(TrySendError::Disconnected(_)) => return,
            }
            // A render thread that never catches up: shed the oldest moves only.
            if pending.len() > 4 * RECORDS_PER_PASS {
                let excess = pending.len() - 4 * RECORDS_PER_PASS;
                let mut dropped = 0;
                pending.retain(|r| {
                    let drop = dropped < excess && r.kind == RecordKind::Move;
                    dropped += usize::from(drop);
                    !drop
                });
            }
        }
        // Downstream: the newest frame, if any.
        match frames.take(Duration::from_millis(3)) {
            Some(f) => {
                if ws.send(Message::Binary(encode_frame(&f).into())).is_err() {
                    return;
                }
            }
            None => {
                let _ = ws.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_unknown_kinds_are_refused() {
        let r = Record {
            kind: RecordKind::Move,
            buttons: 1,
            mods: 0,
            pointer: 0,
            x: 0.25,
            y: 0.75,
            dx: 0.0,
            dy: 0.0,
            code: 0,
            t: 1234.5,
        };
        assert_eq!(Record::parse(&r.encode()), Some(r));
        let mut b = r.encode();
        b[0] = 99;
        assert!(Record::parse(&b).is_none());
        let mut k = Record {
            kind: RecordKind::Key,
            code: 77,
            ..r
        }
        .encode();
        assert!(Record::parse(&k).is_none(), "only transport keys");
        k[20] = 1;
        assert!(Record::parse(&k).is_some());
        let mut nan = r.encode();
        nan[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(Record::parse(&nan).is_none());
        assert!(Record::parse(&[0u8; 31]).is_none());
    }

    #[test]
    fn coalescing_keeps_every_button_and_key() {
        let r = |kind, x| Record {
            kind,
            buttons: 1,
            mods: 0,
            pointer: 0,
            x,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            code: 1,
            t: 0.0,
        };
        let mut b = vec![
            r(RecordKind::Viewport, 1.0),
            r(RecordKind::Down, 0.1),
            r(RecordKind::Move, 0.2),
            r(RecordKind::Move, 0.3),
            r(RecordKind::Viewport, 2.0),
            r(RecordKind::Move, 0.4),
            r(RecordKind::Up, 0.5),
            r(RecordKind::Key, 0.0),
        ];
        coalesce(&mut b);
        let kinds: Vec<_> = b.iter().map(|r| (r.kind, r.x)).collect();
        assert_eq!(
            kinds,
            vec![
                (RecordKind::Down, 0.1),
                (RecordKind::Move, 0.3),
                (RecordKind::Viewport, 2.0),
                (RecordKind::Move, 0.4),
                (RecordKind::Up, 0.5),
                (RecordKind::Key, 0.0)
            ]
        );
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    fn client(
        port: u16,
        origin: &str,
    ) -> Result<WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>, tungstenite::Error> {
        let mut req = tungstenite::client::IntoClientRequest::into_client_request(format!(
            "ws://127.0.0.1:{port}/"
        ))?;
        req.headers_mut().insert(
            "origin",
            origin
                .parse()
                .map_err(|_| tungstenite::Error::Utf8(String::new()))?,
        );
        tungstenite::connect(req).map(|(ws, _)| ws)
    }

    /// R-FRAME-1: a window hidden past the limit closes the socket (no more frames, and
    /// no new connection is accepted); hidden for less, nothing happens.
    #[test]
    fn hidden_window_closes_the_socket() {
        let (mut srv, l) = FrameServer::bind("tauri://localhost").unwrap();
        srv.hidden = Arc::new(HiddenGate::new(Duration::from_millis(300)));
        let slot = Arc::new(FrameSlot::default());
        let (rtx, _rrx) = std::sync::mpsc::sync_channel(4);
        srv.serve(l, slot.clone(), rtx).unwrap();
        let mut ws = client(srv.port, "tauri://localhost").unwrap();
        ws.send(Message::Binary(srv.session.token().to_vec().into()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(srv.connected.load(Ordering::SeqCst));
        // hidden briefly, then shown: still open
        srv.hidden.set(true);
        std::thread::sleep(Duration::from_millis(150));
        srv.hidden.set(false);
        std::thread::sleep(Duration::from_millis(400));
        assert!(srv.connected.load(Ordering::SeqCst));
        // hidden past the limit: closed
        srv.hidden.set(true);
        let t = std::time::Instant::now();
        while srv.connected.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!srv.connected.load(Ordering::SeqCst));
        assert!(srv.hidden.closed.load(Ordering::SeqCst));
        // until it is re-armed, nothing reconnects, not even with the token
        let old = srv.session.token();
        if let Ok(mut s) = client(srv.port, "tauri://localhost") {
            let _ = s.send(Message::Binary(old.to_vec().into()));
            assert!(!matches!(s.read(), Ok(Message::Binary(_))));
        }
        // shown again: a fresh token; the old one is refused, the new one gets frames
        srv.hidden.set(false);
        let (_, g0) = srv.session.current();
        assert!(srv.session.rearm());
        let (_, g1) = srv.session.current();
        assert_eq!(g1, g0 + 1);
        assert_ne!(srv.session.token(), old);
        if let Ok(mut s) = client(srv.port, "tauri://localhost") {
            let _ = s.send(Message::Binary(old.to_vec().into()));
            assert!(!matches!(s.read(), Ok(Message::Binary(_))));
        }
        let mut ws = client(srv.port, "tauri://localhost").unwrap();
        ws.send(Message::Binary(srv.session.token().to_vec().into()))
            .unwrap();
        let t = std::time::Instant::now();
        while !srv.connected.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(srv.connected.load(Ordering::SeqCst));
        slot.put(OutFrame {
            seq: 2,
            out_frame: 9,
            width: 2,
            height: 2,
            echo_t: 0.0,
            render_us: 1,
            flags: 0,
            dropped: 0,
            y: vec![16; 4],
            uv: vec![128; 2],
        });
        assert!(matches!(ws.read(), Ok(Message::Binary(_))));
        // and it is the only one: a third connection is refused
        if let Ok(mut s) = client(srv.port, "tauri://localhost") {
            let _ = s.send(Message::Binary(srv.session.token().to_vec().into()));
            assert!(!matches!(s.read(), Ok(Message::Binary(_))));
        }
    }

    /// R-FRAME-1: wrong Origin refused; wrong token refused; one connection only.
    #[test]
    fn origin_token_and_single_connection() {
        let (srv, l) = FrameServer::bind("tauri://localhost").unwrap();
        let slot = Arc::new(FrameSlot::default());
        let (rtx, rrx) = std::sync::mpsc::sync_channel(4);
        srv.serve(l, slot.clone(), rtx).unwrap();
        // Wrong Host: the handshake fails.
        {
            let mut req = tungstenite::client::IntoClientRequest::into_client_request(format!(
                "ws://127.0.0.1:{}/",
                srv.port
            ))
            .unwrap();
            req.headers_mut()
                .insert("origin", "tauri://localhost".parse().unwrap());
            req.headers_mut()
                .insert("host", "localhost".parse().unwrap());
            assert!(tungstenite::connect(req).is_err());
        }
        // Wrong origin: the handshake fails.
        assert!(client(srv.port, "http://evil.example").is_err());
        assert!(client(srv.port, "tauri://localhost.evil").is_err());
        // Right origin, wrong token: closed, and the slot is still free.
        let mut ws = client(srv.port, "tauri://localhost").unwrap();
        ws.send(Message::Binary(vec![0u8; 32].into())).unwrap();
        assert!(!matches!(ws.read(), Ok(Message::Binary(_))));
        // Right token: accepted; records flow.
        let mut ws = client(srv.port, "tauri://localhost").unwrap();
        ws.send(Message::Binary(srv.session.token().to_vec().into()))
            .unwrap();
        let r = Record {
            kind: RecordKind::Down,
            buttons: 1,
            mods: 0,
            pointer: 0,
            x: 0.5,
            y: 0.5,
            dx: 0.0,
            dy: 0.0,
            code: 0,
            t: 1.0,
        };
        ws.send(Message::Binary(r.encode().to_vec().into()))
            .unwrap();
        let got = rrx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got, vec![r]);
        // Frames flow down.
        slot.put(OutFrame {
            seq: 1,
            out_frame: 7,
            width: 2,
            height: 2,
            echo_t: 0.0,
            render_us: 1,
            flags: 0,
            dropped: 0,
            y: vec![16; 4],
            uv: vec![128; 2],
        });
        match ws.read().unwrap() {
            Message::Binary(b) => assert_eq!(b.len(), 32 + 4 + 2),
            m => panic!("{m:?}"),
        }
        // A second connection, even with the token, is refused.
        let before = srv.refused.lock().unwrap().len();
        let second = client(srv.port, "tauri://localhost");
        if let Ok(mut s) = second {
            let _ = s.send(Message::Binary(srv.session.token().to_vec().into()));
            assert!(!matches!(s.read(), Ok(Message::Binary(_))));
        }
        std::thread::sleep(Duration::from_millis(100));
        let log = srv.refused.lock().unwrap().clone();
        assert_eq!(
            log.get(before).map(String::as_str),
            Some("second connection"),
            "{log:?}"
        );
    }
}
