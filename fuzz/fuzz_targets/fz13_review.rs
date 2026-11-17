//! FZ13 (SR §3.2, B.03): the review server's request reading — the request line and
//! header block, the `Range` header, the session cookie, the routing and (decision 0015)
//! the JSON write bodies. Any bytes: no panic; an accepted head has a visible-ASCII
//! origin-form path with no query, token header names, values with no CR/LF/NUL, a body
//! length ≤ 64 KiB; a cookie value is 1–128 bare cookie-octets; a range is one range
//! inside the file; a route serves a file only by an export or moment record id (never a
//! path), and only to GET/HEAD; an accepted write body names only valid ids, times from 0
//! to 6 h, a box inside the frame and text ≤ 2000 characters.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../crates/gmclip-mcp/src/review/http.rs"]
#[allow(dead_code)]
mod http;

#[path = "../../crates/gmclip-mcp/src/review/body.rs"]
#[allow(dead_code)]
mod body;

const ROUTES: &[&str] = &["/api/comment", "/api/resolve", "/api/suggest", "/api/approve"];

fuzz_target!(|data: &[u8]| {
    // the head
    if let Ok((r, len)) = http::parse_head(data) {
        assert!(r.path.starts_with('/'));
        assert!(!r.path.contains(['?', '#']));
        assert!(len <= http::MAX_BODY);
        assert!(!r.method.is_empty() && r.method.bytes().all(|b| b.is_ascii_uppercase()));
        assert!(r.path.bytes().all(|b| b.is_ascii_graphic()));
        for (k, v) in &r.headers {
            assert!(!k.is_empty() && !k.bytes().any(|b| b.is_ascii_uppercase() || b <= b' ' || b == b':'));
            assert!(!v.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)));
        }
        // the session cookie
        if let Some(c) = r.cookie("gmr") {
            assert!(!c.is_empty() && c.len() <= http::MAX_COOKIE);
            assert!(!c.bytes().any(|b| b <= b' ' || matches!(b, b'"' | b',' | b';' | b'\\') || b >= 0x7f));
        }
        // the routing: a file only by record id, only for reads
        match http::route(&r.method, &r.path) {
            http::Route::File { clip, id } => {
                assert!(r.method == "GET" || r.method == "HEAD");
                assert_eq!(r.path.as_bytes()[1], if clip { b'm' } else { b'x' });
                if let Some(id) = id {
                    assert!(http::record_id(id) && r.path.len() == 3 + id.len());
                }
            }
            http::Route::Page(_) => assert!(r.method == "GET" || r.method == "HEAD"),
            http::Route::Home => assert_eq!(r.method, "GET"),
            http::Route::StartSession | http::Route::Write(_) => assert_eq!(r.method, "POST"),
            http::Route::MethodNotAllowed | http::Route::NotFound => {}
        }
        // the range against a file length taken from the input
        let flen = data.iter().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b))) % (1 << 40);
        match http::range(r.header("range"), flen) {
            http::RangeAnswer::Part(a, b) => assert!(a <= b && b < flen),
            http::RangeAnswer::Whole | http::RangeAnswer::Unsatisfiable => {}
        }
    }
    // a Range header on its own
    if let Ok(s) = std::str::from_utf8(data) {
        for flen in [0u64, 1, 100, u64::MAX] {
            if let http::RangeAnswer::Part(a, b) = http::range(Some(s), flen) {
                assert!(a <= b && b < flen);
            }
        }
    }
    // the write bodies, on every route
    let route = ROUTES[usize::from(data.first().copied().unwrap_or(0)) % ROUTES.len()];
    let rest = data.get(1..).unwrap_or_default();
    if body::parse(route, rest).is_ok() {
        let v: serde_json::Value = serde_json::from_slice(rest).unwrap();
        match route {
            "/api/comment" => {
                let c = body::comment(&v).unwrap();
                assert!(body::valid_id(&c.cut));
                assert!(c.reply_to.as_deref().is_none_or(body::valid_id));
                assert!(c.export_ref.as_deref().is_none_or(body::valid_id));
                assert!(c.t.is_none_or(|t| t.is_finite() && (0.0..=body::MAX_T).contains(&t)));
                if let Some(b) = c.frame_box {
                    assert!(b.w > 0.0 && b.h > 0.0 && b.x + b.w <= 1.0 + 1e-9 && b.y + b.h <= 1.0 + 1e-9);
                }
                assert!(!c.text.trim().is_empty() && c.text.chars().count() <= body::MAX_TEXT);
                assert!(c.reply_to.is_none() || (c.t.is_none() && c.frame_box.is_none()));
            }
            "/api/suggest" => {
                let s = body::suggest(&v).unwrap();
                assert!(body::valid_id(&s.cut));
                match s.edit {
                    body::Suggestion::Trim { part, head_s, tail_s } => {
                        assert!(body::valid_id(&part));
                        assert!(head_s >= 0.0 && tail_s >= 0.0 && head_s + tail_s > 0.0);
                    }
                    body::Suggestion::Strike { from_s, to_s } => {
                        assert!(from_s >= 0.0 && to_s > from_s && to_s - from_s <= 120.0);
                    }
                    body::Suggestion::CaptionMove { y, t } => {
                        assert!((0.05..=0.95).contains(&y));
                        assert!(t.is_none_or(|t| (0.0..=body::MAX_T).contains(&t)));
                    }
                }
            }
            "/api/resolve" => assert!(body::valid_id(&body::resolve(&v).unwrap().comment)),
            _ => assert!(body::valid_id(&body::approve(&v).unwrap().export)),
        }
    }
});
