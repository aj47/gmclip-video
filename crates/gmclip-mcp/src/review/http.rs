//! The review server's HTTP/1.1 subset: one request per connection (`Connection:
//! close`), headers ≤ 16 KiB, bodies ≤ 64 KiB with `Content-Length` (no chunked
//! requests), one byte range per response (SR R-REV-6).

use std::collections::BTreeMap;

/// Header block cap.
pub const MAX_HEADERS: usize = 16 * 1024;
/// Request body cap.
pub const MAX_BODY: usize = 64 * 1024;
/// A `Range` header longer than this is refused with 416 (the size cap of R-REV-6).
pub const MAX_RANGE_HEADER: usize = 12 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lower-cased names; a repeated name keeps the first value (a second `Host` sets
    /// `dup_host`, a second `Content-Length` is malformed).
    pub headers: BTreeMap<String, String>,
    pub dup_host: bool,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// A cookie's value: the first pair with that exact name, and only when its value is
    /// 1..=[`MAX_COOKIE`] bare cookie-octets (RFC 6265 §4.1.1, unquoted: a session id never
    /// needs quotes, commas, semicolons or backslashes).
    pub fn cookie(&self, name: &str) -> Option<&str> {
        let v = self.header("cookie")?.split(';').find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then_some(v)
        })?;
        let octet =
            |b: u8| matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E);
        (!v.is_empty() && v.len() <= MAX_COOKIE && v.bytes().all(octet)).then_some(v)
    }
}

/// A cookie value longer than this is ignored (a session id is 64 hex digits).
pub const MAX_COOKIE: usize = 128;

/// The page's static files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    Index,
    Js,
    Css,
}

/// The six write endpoints (each needs the session cookie, `X-CSRF` and the `Origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteRoute {
    Review,
    Fix,
    Comment,
    Resolve,
    Suggest,
    Approve,
}

/// What a request names, from its method and path alone (no state): the server's whole
/// URL space. A file is named by an export id (`/x/<id>`) or a moment id (`/m/<id>`) and
/// nothing else; `id` is `None` when the rest of the path is not a record id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route<'a> {
    Page(Asset),
    StartSession,
    Home,
    File { clip: bool, id: Option<&'a str> },
    Write(WriteRoute),
    MethodNotAllowed,
    NotFound,
}

/// A record id: 1–64 ASCII letters and digits.
pub fn record_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric())
}

/// Routes a request (the method is any upper-case token `parse_head` accepted).
pub fn route<'a>(method: &str, path: &'a str) -> Route<'a> {
    let read = matches!(method, "GET" | "HEAD");
    let page = |a| {
        if read {
            Route::Page(a)
        } else {
            Route::MethodNotAllowed
        }
    };
    let post = |r| {
        if method == "POST" {
            r
        } else {
            Route::MethodNotAllowed
        }
    };
    match path {
        "/" => page(Asset::Index),
        "/app.js" => page(Asset::Js),
        "/app.css" => page(Asset::Css),
        "/session" => post(Route::StartSession),
        "/api/home" if method == "GET" => Route::Home,
        "/api/home" => Route::MethodNotAllowed,
        "/api/review" => post(Route::Write(WriteRoute::Review)),
        "/api/fix" => post(Route::Write(WriteRoute::Fix)),
        "/api/comment" => post(Route::Write(WriteRoute::Comment)),
        "/api/resolve" => post(Route::Write(WriteRoute::Resolve)),
        "/api/suggest" => post(Route::Write(WriteRoute::Suggest)),
        "/api/approve" => post(Route::Write(WriteRoute::Approve)),
        p if read && (p.starts_with("/x/") || p.starts_with("/m/")) => {
            let id = &p[3..];
            Route::File {
                clip: p.starts_with("/m/"),
                id: record_id(id).then_some(id),
            }
        }
        _ => Route::NotFound,
    }
}

/// Why a request could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    /// 400
    Malformed,
    /// 413
    TooLarge,
    /// 501
    Unsupported,
}

/// RFC 9110 `tchar`.
fn tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// The header block's end in `buf`, if complete.
pub fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parses the header block (everything before the blank line).
pub fn parse_head(head: &[u8]) -> Result<(Request, usize), Bad> {
    let text = std::str::from_utf8(head).map_err(|_| Bad::Malformed)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(Bad::Malformed)?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Bad::Malformed);
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(Bad::Malformed);
    }
    if !method.chars().all(|c| c.is_ascii_uppercase()) || method.is_empty() {
        return Err(Bad::Malformed);
    }
    // origin-form only, visible ASCII (RFC 9112 §3.2); the query string is dropped (no
    // endpoint reads one)
    if !target.starts_with('/') || !target.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Bad::Malformed);
    }
    let path = target.split(['?', '#']).next().unwrap_or("/").to_owned();
    let mut headers = BTreeMap::new();
    let mut dup_host = false;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let (k, v) = l.split_once(':').ok_or(Bad::Malformed)?;
        // a field name is a token; a value has no CR, LF or NUL (RFC 9110 §5.1, §5.5)
        if k.is_empty() || !k.bytes().all(tchar) {
            return Err(Bad::Malformed);
        }
        if v.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return Err(Bad::Malformed);
        }
        let k = k.to_ascii_lowercase();
        if headers.contains_key(&k) {
            // two lengths: a smuggling shape, refused outright
            if k == "content-length" {
                return Err(Bad::Malformed);
            }
            if k == "host" {
                dup_host = true;
            }
            continue;
        }
        headers.insert(k, v.trim_matches([' ', '\t']).to_owned());
    }
    if headers.contains_key("transfer-encoding") {
        return Err(Bad::Unsupported);
    }
    let len = match headers.get("content-length") {
        None => 0,
        Some(v) if !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()) => {
            v.parse::<usize>().map_err(|_| Bad::TooLarge)?
        }
        Some(_) => return Err(Bad::Malformed),
    };
    if len > MAX_BODY {
        return Err(Bad::TooLarge);
    }
    Ok((
        Request {
            method: method.to_owned(),
            path,
            headers,
            dup_host,
            body: Vec::new(),
        },
        len,
    ))
}

/// What to send for a file of `len` bytes given a `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAnswer {
    /// 200, the whole file.
    Whole,
    /// 206, `[start, end]` inclusive.
    Part(u64, u64),
    /// 416.
    Unsatisfiable,
}

/// RFC 9110 single-range handling (R-REV-6): one range is served, several get the whole
/// file, an over-long header or a range past the end gets 416; arithmetic is checked.
pub fn range(header: Option<&str>, len: u64) -> RangeAnswer {
    let Some(h) = header else {
        return RangeAnswer::Whole;
    };
    if h.len() > MAX_RANGE_HEADER {
        return RangeAnswer::Unsatisfiable;
    }
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return RangeAnswer::Whole; // another unit: ignored
    };
    if spec.contains(',') {
        return RangeAnswer::Whole;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeAnswer::Whole;
    };
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.len() <= 20 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if len == 0 {
        return RangeAnswer::Unsatisfiable;
    }
    let last = len - 1;
    match (a.trim(), b.trim()) {
        ("", suffix) => match num(suffix) {
            Some(0) => RangeAnswer::Unsatisfiable,
            Some(n) => RangeAnswer::Part(len.saturating_sub(n), last),
            None => RangeAnswer::Whole,
        },
        (s, "") => match num(s) {
            Some(s) if s <= last => RangeAnswer::Part(s, last),
            Some(_) => RangeAnswer::Unsatisfiable,
            None => RangeAnswer::Whole,
        },
        (s, e) => match (num(s), num(e)) {
            (Some(s), Some(e)) if s <= e => {
                if s > last {
                    RangeAnswer::Unsatisfiable
                } else {
                    RangeAnswer::Part(s, e.min(last))
                }
            }
            _ => RangeAnswer::Whole,
        },
    }
}

/// The reason phrase for a status.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(range(None, 100), RangeAnswer::Whole);
        assert_eq!(range(Some("bytes=0-9"), 100), RangeAnswer::Part(0, 9));
        assert_eq!(range(Some("bytes=90-"), 100), RangeAnswer::Part(90, 99));
        assert_eq!(range(Some("bytes=-10"), 100), RangeAnswer::Part(90, 99));
        assert_eq!(range(Some("bytes=-1000"), 100), RangeAnswer::Part(0, 99));
        assert_eq!(range(Some("bytes=50-1000"), 100), RangeAnswer::Part(50, 99));
        assert_eq!(range(Some("bytes=100-"), 100), RangeAnswer::Unsatisfiable);
        assert_eq!(range(Some("bytes=0-1,5-6"), 100), RangeAnswer::Whole);
        let many = format!(
            "bytes={}",
            (0..1000)
                .map(|i| format!("{}-{}", i % 10, i % 10))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            range(Some(&many), 100),
            RangeAnswer::Whole,
            "R-REV-6: 1000 ranges → the whole file"
        );
        let huge = format!(
            "bytes={}",
            (0..3000)
                .map(|i| format!("{i}-{i}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            range(Some(&huge), 100),
            RangeAnswer::Unsatisfiable,
            "over the size cap"
        );
        assert_eq!(
            range(Some("bytes=18446744073709551615-"), 100),
            RangeAnswer::Unsatisfiable
        );
        assert_eq!(
            range(Some("bytes=99999999999999999999999-"), 100),
            RangeAnswer::Whole
        );
        assert_eq!(range(Some("bytes=9-3"), 100), RangeAnswer::Whole);
        assert_eq!(range(Some("items=0-1"), 100), RangeAnswer::Whole);
    }

    #[test]
    fn routes() {
        use Route::*;
        assert_eq!(route("GET", "/"), Page(Asset::Index));
        assert_eq!(route("HEAD", "/app.js"), Page(Asset::Js));
        assert_eq!(route("GET", "/app.css"), Page(Asset::Css));
        assert_eq!(route("POST", "/session"), StartSession);
        assert_eq!(route("GET", "/api/home"), Home);
        assert_eq!(
            route("HEAD", "/x/ab12"),
            File {
                clip: false,
                id: Some("ab12")
            }
        );
        assert_eq!(
            route("GET", "/m/Z9"),
            File {
                clip: true,
                id: Some("Z9")
            }
        );
        // a file is named by a record id only: no separators, dots or encodings
        for p in [
            "/x/",
            "/x/../etc",
            "/m/a/b",
            "/x/a%2e",
            "/x/a.mp4",
            &format!("/x/{}", "a".repeat(65)),
        ] {
            assert_eq!(
                route("GET", p),
                File {
                    clip: p.starts_with("/m/"),
                    id: None
                },
                "{p}"
            );
        }
        assert_eq!(route("POST", "/api/comment"), Write(WriteRoute::Comment));
        assert_eq!(route("POST", "/api/approve"), Write(WriteRoute::Approve));
        assert_eq!(route("GET", "/api/approve"), MethodNotAllowed);
        assert_eq!(route("POST", "/x/ab"), NotFound);
        assert_eq!(route("DELETE", "/"), MethodNotAllowed);
        assert_eq!(route("GET", "/api/homes"), NotFound);
        assert_eq!(route("GET", "/X/ab"), NotFound);
    }

    #[test]
    fn cookies() {
        let req = |c: &str| {
            parse_head(format!("GET / HTTP/1.1\r\nCookie: {c}\r\n").as_bytes())
                .unwrap()
                .0
        };
        assert_eq!(req("gmr=abc").cookie("gmr"), Some("abc"));
        assert_eq!(req("x=1;gmr=abc ; y=2").cookie("gmr"), Some("abc"));
        assert_eq!(
            req("gmr=a; gmr=b").cookie("gmr"),
            Some("a"),
            "the first wins"
        );
        assert_eq!(req("xgmr=abc").cookie("gmr"), None);
        assert_eq!(req("gmr=").cookie("gmr"), None, "empty");
        assert_eq!(
            req("gmr=\"abc\"").cookie("gmr"),
            None,
            "quoted: not a session id"
        );
        assert_eq!(req("gmr=a,b").cookie("gmr"), None);
        assert_eq!(req("gmr=a\\b").cookie("gmr"), None);
        assert_eq!(
            req(&format!("gmr={}", "a".repeat(129))).cookie("gmr"),
            None,
            "too long"
        );
    }

    #[test]
    fn request_lines_and_headers() {
        // the target is visible ASCII; header names are tokens; values have no CR, LF or NUL
        for bad in [
            &b"GET /a\x01b HTTP/1.1\r\n"[..],
            b"GET /\xc3\xa9 HTTP/1.1\r\n",
            b"GET / HTTP/1.1\r\nHost: a\nX-Evil: 1\r\n",
            b"GET / HTTP/1.1\r\nHost: a\rb\r\n",
            b"GET / HTTP/1.1\r\nHost: a\0b\r\n",
            b"GET / HTTP/1.1\r\nHo(st: a\r\n",
            b"GET / HTTP/1.1\r\n: a\r\n",
            b"GET / HTTP/1.1\r\nContent-Length: +5\r\n",
            b"GET / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n",
            b"GET  / HTTP/1.1\r\n",
            b"GET / HTTP/2\r\n",
        ] {
            assert_eq!(
                parse_head(bad).unwrap_err(),
                Bad::Malformed,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        let (r, _) = parse_head(b"GET / HTTP/1.1\r\nX-A: a\tb\r\n").unwrap();
        assert_eq!(
            r.header("x-a"),
            Some("a\tb"),
            "HTAB is allowed inside a value"
        );
    }

    #[test]
    fn heads() {
        let (r, n) = parse_head(b"GET /x/ab?t=1 HTTP/1.1\r\nHost: 127.0.0.1:1\r\nHOST: evil\r\nCookie: a=1; gmr=xyz\r\n\r\n").unwrap();
        assert_eq!((r.path.as_str(), n, r.dup_host), ("/x/ab", 0, true));
        assert_eq!(r.cookie("gmr"), Some("xyz"));
        assert_eq!(
            parse_head(b"GET http://evil/ HTTP/1.1\r\n\r\n").unwrap_err(),
            Bad::Malformed
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap_err(),
            Bad::Unsupported
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n").unwrap_err(),
            Bad::TooLarge
        );
        assert_eq!(
            parse_head(b"G ET / HTTP/1.1\r\n\r\n").unwrap_err(),
            Bad::Malformed
        );
    }
}
