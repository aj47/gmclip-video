//! FZ10 (SR §3.2, B.04): the agent path form check, both grammars. Any text: no panic;
//! an accepted path is absolute in its grammar and its components, joined beneath a root,
//! never leave it (no `.`/`..`, no separator, no NUL, no trailing dot or space); on the
//! Windows grammar no UNC, device, drive-relative, stream or reserved name gets through.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../crates/gmclip-mcp/src/pathform.rs"]
#[allow(dead_code)]
mod pathform;

use pathform::{Grammar, validate};

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

/// The model: a root and the accepted components; the resolved path stays beneath it.
fn beneath(parts: &[String]) -> bool {
    let mut depth: i64 = 0;
    for p in parts {
        match p.as_str() {
            ".." => depth -= 1,
            "." | "" => {}
            _ => depth += 1,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    for g in [Grammar::Unix, Grammar::Windows] {
        let Ok(parts) = validate(s, g) else { continue };
        assert!(!s.contains('\0'));
        assert!(beneath(&parts));
        for c in &parts {
            assert!(!c.is_empty() && c != "." && c != "..");
            assert!(!c.contains(['/', '\0']));
            assert!(!c.ends_with(['.', ' ']));
            if g == Grammar::Windows {
                assert!(!c.contains(['\\', ':', '<', '>', '"', '|', '?', '*']));
                let stem = c.split('.').next().unwrap_or(c).to_ascii_uppercase();
                assert!(!RESERVED.contains(&stem.as_str()));
            }
        }
        match g {
            Grammar::Unix => assert!(s.starts_with('/') && !s.contains('\\')),
            Grammar::Windows => {
                let b = s.as_bytes();
                assert!(b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'));
                assert!(!s.starts_with("\\\\") && !s.starts_with("//"));
            }
        }
    }
});
