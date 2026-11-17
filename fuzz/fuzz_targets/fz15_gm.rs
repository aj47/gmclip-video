//! FZ15 (SR §3.2, B.02): the `gm://data` request parser. Any text: no panic; an accepted
//! request names a ULID project, an enumerated kind and at most one ULID cut, so no path,
//! traversal, encoding or other key ever reaches the data handler.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../app/src-tauri/src/gmuri.rs"]
#[allow(dead_code)]
mod gmuri;

// The body is `gmuri::fuzz_check`, which `cargo xt fuzz-replay` also runs over the
// committed seeds and regressions on stable.
fuzz_target!(|data: &[u8]| gmuri::fuzz_check(data));
