//! Windows path text (decisions/0019): verbatim prefixes and case-insensitive equality.
//!
//! `std::fs::canonicalize` on Windows returns verbatim paths (`\\?\C:\…`,
//! `\\?\UNC\server\share\…`), environment variables and configs carry the plain form
//! (`C:\…`), and NTFS compares names without case. Comparing those as `Path`s
//! (component-wise, case-sensitive) would say `\\?\C:\Users\AJ` and `c:\users\aj` differ.
//! Pure string logic, tested on every OS.

/// The plain form of a verbatim path: `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share\x` →
/// `\\srv\share\x`. Other paths (and verbatim non-drive paths such as `\\?\Volume{…}`)
/// are returned unchanged.
pub fn strip_verbatim(p: &str) -> String {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = p.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return rest.to_owned();
        }
    }
    p.to_owned()
}

/// A comparison key: plain form, `\` separators, lower case, no trailing separator
/// except on a drive root (`c:\`).
pub fn key(p: &str) -> String {
    let mut s = strip_verbatim(p).replace('/', "\\").to_lowercase();
    while s.len() > 3 && s.ends_with('\\') && !s.ends_with(":\\") {
        s.pop();
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    s
}

/// Do two Windows paths name the same place (verbatim or not, any case)?
pub fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// Is `p` a drive root (`C:\`, `\\?\C:\`) or a UNC share root (`\\srv\share`)?
pub fn is_root(p: &str) -> bool {
    let k = key(p);
    if k.len() == 3 && k.ends_with(":\\") {
        return true;
    }
    k.strip_prefix(r"\\")
        .is_some_and(|r| r.trim_end_matches('\\').split('\\').count() == 2)
}

/// Is `p` equal to or inside `base` (same rules as [`same`])?
pub fn starts_with(p: &str, base: &str) -> bool {
    let (p, b) = (key(p), key(base));
    p == b
        || p.strip_prefix(&b)
            .is_some_and(|r| b.ends_with('\\') || r.starts_with('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_and_case_do_not_matter() {
        assert!(same(r"\\?\C:\Users\AJ\AppData", r"c:\users\aj\appdata\"));
        assert!(same(r"C:/Users/AJ", r"C:\USERS\aj"));
        assert!(same(r"\\?\UNC\srv\Share\x", r"\\SRV\share\x"));
        assert!(!same(r"C:\Users\AJ", r"D:\Users\AJ"));
        assert!(!same(r"C:\Users\AJ", r"C:\Users\AJ2"));
        assert_eq!(key(r"\\?\C:\"), r"c:\");
        assert_eq!(key("C:"), r"c:\");
        // a verbatim path that is not a drive or UNC stays as it is
        assert_eq!(strip_verbatim(r"\\?\Volume{x}\a"), r"\\?\Volume{x}\a");
    }

    #[test]
    fn roots_and_containment() {
        assert!(is_root(r"C:\"));
        assert!(is_root(r"\\?\c:\"));
        assert!(is_root(r"\\srv\share"));
        assert!(is_root(r"\\?\UNC\srv\share\"));
        assert!(!is_root(r"C:\Users"));
        assert!(!is_root(r"\\srv\share\x"));
        assert!(starts_with(r"\\?\C:\Users\AJ\x", r"c:\users\aj"));
        assert!(starts_with(r"C:\Users\AJ", r"C:\"));
        assert!(!starts_with(r"C:\Users\AJ2", r"C:\Users\AJ"));
        assert!(starts_with(r"C:\Users\AJ", r"c:\users\aj\"));
    }
}
