//! The Windows file dialog's text (W.9): the filter list it is given and the selection
//! it hands back (`GetOpenFileNameW`, `crate::windows::open_file_dialog`). Pure UTF-16
//! logic, tested on every OS.

/// `"<name> (*.a;*.b)\0*.a;*.b\0\0"`: one filter row, as `lpstrFilter` takes it. An
/// extension with a character the dialog treats as syntax (`;`, `*`, NUL) is left out.
pub fn filter(name: &str, exts: &[&str]) -> Vec<u16> {
    let pats: Vec<String> = exts
        .iter()
        .filter(|e| !e.is_empty() && !e.contains([';', '*', '\0', '.']))
        .map(|e| format!("*.{e}"))
        .collect();
    let pats = if pats.is_empty() {
        "*.*".to_owned()
    } else {
        pats.join(";")
    };
    let mut v: Vec<u16> = format!("{} ({pats})", name.replace('\0', ""))
        .encode_utf16()
        .collect();
    v.push(0);
    v.extend(pats.encode_utf16());
    v.extend([0, 0]);
    v
}

/// The paths in a returned `lpstrFile`: one full path (`C:\a\x.mp4\0\0`), or with
/// several chosen (`OFN_ALLOWMULTISELECT | OFN_EXPLORER`) the folder then each name
/// (`C:\a\0x.mp4\0y.mp4\0\0`). Stops at the double NUL or the buffer's end.
pub fn selection(buf: &[u16]) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for s in buf.split(|c| *c == 0) {
        if s.is_empty() {
            break;
        }
        parts.push(String::from_utf16_lossy(s));
    }
    match parts.len() {
        0 => Vec::new(),
        1 => parts,
        _ => {
            let dir = parts.remove(0);
            let sep = if dir.ends_with('\\') { "" } else { "\\" };
            parts
                .into_iter()
                .map(|n| format!("{dir}{sep}{n}"))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn filters_are_double_nul_terminated_rows() {
        let f = filter("Videos", &["mp4", "mkv", "bad;ext", "*"]);
        let mut want = w("Videos (*.mp4;*.mkv)");
        want.push(0);
        want.extend(w("*.mp4;*.mkv"));
        want.extend([0, 0]);
        assert_eq!(f, want);
        let any = filter("Any", &[]);
        assert!(String::from_utf16_lossy(&any).starts_with("Any (*.*)\0*.*\0\0"));
    }

    #[test]
    fn one_or_several_files_come_back_as_full_paths() {
        let mut one = w(r"C:\v\a clip.mp4");
        one.extend([0, 0, 0, 0]);
        assert_eq!(selection(&one), [r"C:\v\a clip.mp4"]);
        let mut many = w(r"C:\v");
        many.push(0);
        many.extend(w("a.mp4"));
        many.push(0);
        many.extend(w("b.mkv"));
        many.extend([0, 0, 0x41, 0]);
        assert_eq!(selection(&many), [r"C:\v\a.mp4", r"C:\v\b.mkv"]);
        let mut root = w(r"D:\");
        root.push(0);
        root.extend(w("x.mov"));
        root.extend([0, 0]);
        assert_eq!(selection(&root), [r"D:\x.mov"]);
        assert!(selection(&[0, 0]).is_empty());
        assert!(selection(&[]).is_empty());
        // no terminator at all (a full buffer): what is there
        assert_eq!(selection(&w(r"C:\x.mp4")), [r"C:\x.mp4"]);
    }
}
