//! The agent path *form* check (R-FS-1, FZ10): standard library only, so the fuzz target
//! (`fuzz/fuzz_targets/fz10_paths.rs`) builds it by path. [`crate::fsroots::validate`]
//! wraps it in the command error; cap-std then confines every access beneath a root.
#![forbid(unsafe_code)]

/// Which path grammar to check.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Grammar {
    Unix,
    Windows,
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

fn check_component(c: &str, g: Grammar) -> Result<(), &'static str> {
    if c == ".." {
        return Err("`..` is not allowed");
    }
    if c == "." {
        return Err("`.` components are not allowed");
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err("a trailing dot or space is not allowed");
    }
    if g == Grammar::Windows {
        if c.contains(':') {
            return Err("alternate data streams are not allowed");
        }
        let stem = c.split('.').next().unwrap_or(c).to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err("reserved device names are not allowed");
        }
        if c.chars()
            .any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*') || (ch as u32) < 32)
        {
            return Err("characters Windows forbids");
        }
    }
    Ok(())
}

/// Checks the path *form* (no filesystem access) and returns its components.
pub fn validate(path: &str, g: Grammar) -> Result<Vec<String>, &'static str> {
    if path.is_empty() {
        return Err("empty");
    }
    if path.len() > 4096 {
        return Err("too long");
    }
    if path.contains('\0') {
        return Err("NUL is not allowed");
    }
    match g {
        Grammar::Unix => {
            if path.contains('\\') {
                return Err("backslashes are not allowed");
            }
            if !path.starts_with('/') {
                return Err("an absolute path is required");
            }
            let parts: Vec<String> = path
                .split('/')
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect();
            for c in &parts {
                check_component(c, g)?;
            }
            Ok(parts)
        }
        Grammar::Windows => {
            if path.starts_with("\\\\.\\")
                || path.starts_with("\\\\?\\")
                || path.starts_with("//./")
                || path.starts_with("//?/")
            {
                return Err("device paths are not allowed");
            }
            if path.starts_with("\\\\") || path.starts_with("//") {
                return Err("UNC paths are not allowed");
            }
            let b = path.as_bytes();
            let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
            if !drive {
                return Err("an absolute path with a drive letter is required");
            }
            if b.len() == 2 || (b[2] != b'\\' && b[2] != b'/') {
                return Err("drive-relative paths are not allowed");
            }
            let rest = &path[3..];
            let parts: Vec<String> = rest
                .split(['\\', '/'])
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect();
            for c in &parts {
                check_component(c, g)?;
            }
            Ok(parts)
        }
    }
}
