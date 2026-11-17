//! `gm://data/…` request parsing (R-IPC-1, FZ15): standard library only, so the fuzz
//! target (`fuzz/fuzz_targets/fz15_gm.rs`) builds it by path. A request names an open
//! project by ULID and one enumerated kind, optionally a cut by ULID, and for the
//! filmstrip two bounded integers (`every`: output frames between thumbnails, `w`:
//! thumbnail width); nothing else is accepted (no path, no other query key, no
//! percent-encoding, no sign or leading zero).
#![forbid(unsafe_code)]

/// The kinds `gm://data` serves.
pub const KINDS: &[&str] = &["parts", "words", "filmstrip", "peaks", "energy", "quiet"];

/// `every` bounds (output frames between filmstrip thumbnails).
pub const EVERY: (u32, u32) = (1, 1_000_000);
/// `w` bounds (filmstrip thumbnail width, pixels).
pub const WIDTH: (u32, u32) = (16, 320);

/// A parsed `gm://data/<project>/<kind>?cut=<ulid>` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GmReq<'a> {
    pub project: &'a str,
    pub kind: &'a str,
    pub cut: Option<&'a str>,
    /// Filmstrip only: output frames between thumbnails.
    pub every: Option<u32>,
    /// Filmstrip only: thumbnail width.
    pub width: Option<u32>,
}

/// A decimal integer within `[lo, hi]`: digits only, no leading zero, ≤ 7 digits.
fn bounded(v: &str, (lo, hi): (u32, u32)) -> Option<u32> {
    let b = v.as_bytes();
    if b.is_empty() || b.len() > 7 || b[0] == b'0' || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: u32 = v.parse().ok()?;
    (lo..=hi).contains(&n).then_some(n)
}

/// A ULID's text form: 26 Crockford base-32 characters, the first ≤ '7' (128 bits).
pub fn is_ulid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 26
        && b[0] <= b'7'
        && b.iter().all(|c| {
            matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
        })
}

/// Parses a `gm://data` URI; `None` for anything else.
pub fn parse(uri: &str) -> Option<GmReq<'_>> {
    let rest = uri.strip_prefix("gm://data/")?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut segs = path.split('/');
    let (project, kind) = (segs.next()?, segs.next()?);
    if segs.next().is_some() || !is_ulid(project) || !KINDS.contains(&kind) {
        return None;
    }
    let mut cut = None;
    let (mut every, mut width) = (None, None);
    let strip = kind == "filmstrip";
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        match kv.split_once('=') {
            Some(("cut", v)) if is_ulid(v) && cut.is_none() => cut = Some(v),
            Some(("every", v)) if strip && every.is_none() => every = Some(bounded(v, EVERY)?),
            Some(("w", v)) if strip && width.is_none() => width = Some(bounded(v, WIDTH)?),
            _ => return None,
        }
    }
    Some(GmReq {
        project,
        kind,
        cut,
        every,
        width,
    })
}

/// FZ15's harness body (SR §3.2, B.02), shared by `fuzz/fuzz_targets/fz15_gm.rs` and
/// `cargo xt fuzz-replay`. Any text: no panic; an accepted request names a ULID project,
/// an enumerated kind and at most one ULID cut, so no path, traversal, encoding or other
/// key reaches the data handler. Panics on a violation.
pub fn fuzz_check(data: &[u8]) {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    for uri in [s.to_owned(), format!("gm://data/{s}")] {
        if let Some(r) = parse(&uri) {
            assert!(is_ulid(r.project));
            assert!(KINDS.contains(&r.kind));
            assert!(r.cut.is_none_or(is_ulid));
            // the filmstrip's integers are in range, and only the filmstrip takes them
            assert!(r.every.is_none_or(|n| (EVERY.0..=EVERY.1).contains(&n)));
            assert!(r.width.is_none_or(|n| (WIDTH.0..=WIDTH.1).contains(&n)));
            assert!(r.kind == "filmstrip" || (r.every.is_none() && r.width.is_none()));
            for part in [r.project, r.kind, r.cut.unwrap_or("")] {
                assert!(!part.contains(['/', '.', '%', '\\', '?', '&', '=', '\0']));
            }
            // the URI is exactly what was parsed: nothing else rode along
            let rebuilt = match r.cut {
                Some(c) => format!("gm://data/{}/{}?cut={c}", r.project, r.kind),
                None => format!("gm://data/{}/{}", r.project, r.kind),
            };
            let trimmed = uri
                .trim_end_matches(['&', '?'])
                .replace("?&", "?")
                .replace("&&", "&");
            assert!(rebuilt == trimmed || rebuilt.len() <= uri.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ids_and_kinds() {
        let p = "01J00000000000000000000001";
        assert_eq!(
            parse(&format!("gm://data/{p}/parts?cut={p}")),
            Some(GmReq {
                project: p,
                kind: "parts",
                cut: Some(p),
                every: None,
                width: None,
            })
        );
        assert_eq!(
            parse(&format!("gm://data/{p}/filmstrip?cut={p}&every=48&w=96")),
            Some(GmReq {
                project: p,
                kind: "filmstrip",
                cut: Some(p),
                every: Some(48),
                width: Some(96),
            })
        );
        // the timeline's columns (D.02): a cut, nothing else
        for k in ["peaks", "energy", "quiet", "words"] {
            assert_eq!(
                parse(&format!("gm://data/{p}/{k}?cut={p}")).map(|r| r.kind),
                Some(k)
            );
            assert!(parse(&format!("gm://data/{p}/{k}?cut={p}&every=4")).is_none());
        }
        for bad in [
            format!("gm://data/{p}/parts/x"),
            format!("gm://data/{p}/../etc"),
            format!("gm://data/{p}/files"),
            format!("gm://data/{p}/parts?cut={p}&cut={p}"),
            format!("gm://data/{p}/parts?path=/etc"),
            "gm://data/01J0000000000000000000000I/parts".to_owned(),
            "gm://data/81J00000000000000000000001/parts".to_owned(),
            format!("gm://data/{p}/parts?cut={p}&every=48"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=0"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=048"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=-4"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=4&every=8"),
            format!("gm://data/{p}/filmstrip?cut={p}&w=8"),
            format!("gm://data/{p}/filmstrip?cut={p}&w=1000"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=99999999"),
            format!("gm://data/{p}/filmstrip?cut={p}&every=%34"),
        ] {
            assert!(parse(&bad).is_none(), "{bad}");
        }
    }
}
