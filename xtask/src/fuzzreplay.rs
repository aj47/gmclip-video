//! `cargo xt fuzz-replay` (U.19, B.01/B.02): every committed FZ14/FZ15 input (seeds and
//! minimised crashers, `fuzz/{seeds,regressions}/<target>/`) through the same harness
//! bodies the fuzzer runs (`framerec::fuzz_check`, `gmuri::fuzz_check`), on the stable
//! toolchain. The app's parsers are std-only, so they build here by path, as they do in
//! `fuzz/`; the nightly runs themselves stay `cargo +nightly fuzz run` (see
//! `fuzz/Cargo.toml`).

use std::path::Path;

#[path = "../../app/src-tauri/src/framerec.rs"]
#[allow(dead_code)]
mod framerec;
#[path = "../../app/src-tauri/src/gmuri.rs"]
#[allow(dead_code)]
mod gmuri;

/// A harness body: panics on an input that breaks its invariants.
pub type Check = fn(&[u8]);

/// The app targets this replays, with their harness bodies.
pub const TARGETS: &[(&str, Check)] = &[
    ("fz14_frame", framerec::fuzz_check),
    ("fz15_gm", gmuri::fuzz_check),
];

/// Replays every input of `target` under `root/fuzz/{regressions,seeds}/` through
/// `check`: the number replayed, or the inputs that panicked.
pub fn replay(root: &Path, target: &str, check: Check) -> Result<usize, Vec<String>> {
    let mut n = 0;
    let mut failed = Vec::new();
    for dir in ["regressions", "seeds"] {
        let d = root.join("fuzz").join(dir).join(target);
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths.into_iter().filter(|p| p.is_file()) {
            let data = match std::fs::read(&p) {
                Ok(d) => d,
                Err(e) => {
                    failed.push(format!("{}: {e}", p.display()));
                    continue;
                }
            };
            if std::panic::catch_unwind(|| check(&data)).is_err() {
                failed.push(p.display().to_string());
            }
            n += 1;
        }
    }
    if failed.is_empty() {
        Ok(n)
    } else {
        Err(failed)
    }
}

pub fn run(root: &Path) -> Result<(), String> {
    let mut lines = Vec::new();
    let mut bad = Vec::new();
    // the harness bodies assert; their panics are the result, not noise
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for (t, check) in TARGETS {
        match replay(root, t, *check) {
            Ok(0) => bad.push(format!("{t}: no committed inputs")),
            Ok(n) => lines.push(format!("{t}: {n} inputs replay clean")),
            Err(f) => bad.extend(f.into_iter().map(|p| format!("{t}: {p} fails its harness"))),
        }
    }
    std::panic::set_hook(hook);
    if bad.is_empty() {
        println!("{}", lines.join("\n"));
        Ok(())
    } else {
        Err(bad.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_inputs_replay_clean() {
        let root = crate::repo_root();
        assert_eq!(replay(&root, "fz14_frame", framerec::fuzz_check), Ok(3));
        assert_eq!(replay(&root, "fz15_gm", gmuri::fuzz_check), Ok(4));
        assert!(run(&root).is_ok());
    }

    #[test]
    fn a_failing_input_is_named() {
        let d = std::env::temp_dir().join(format!("gmclip-xt-fuzz-{}", std::process::id()));
        let reg = d.join("fuzz/regressions/fz14_frame");
        std::fs::create_dir_all(&reg).unwrap();
        // a Down record whose x is NaN: the decoder must refuse it, and the harness
        // asserts every parsed value is finite; a decoder that took it would fail here
        let mut rec = [0u8; 32];
        rec[0] = 1;
        rec[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
        std::fs::write(reg.join("nan-x.bin"), rec).unwrap();
        assert_eq!(replay(&d, "fz14_frame", framerec::fuzz_check), Ok(1));
        fn strict(data: &[u8]) {
            assert!(data.is_empty(), "not empty");
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = replay(&d, "fz14_frame", strict);
        std::panic::set_hook(hook);
        let f = r.unwrap_err();
        assert_eq!(f.len(), 1);
        assert!(f[0].ends_with("nan-x.bin"), "{f:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
