//! FZ14 (SR §3.2, B.01): the frame socket's upstream record decoder. Any bytes, split
//! into 32-byte records: no panic; a record parses only to an enumerated kind with finite
//! values (key records: transport codes only); a parsed record re-encodes to the same
//! bytes; coalescing keeps every down, up, cancel, wheel and key, in order.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../app/src-tauri/src/framerec.rs"]
#[allow(dead_code)]
mod framerec;

// The body is `framerec::fuzz_check`, which `cargo xt fuzz-replay` also runs over the
// committed seeds and regressions on stable.
fuzz_target!(|data: &[u8]| framerec::fuzz_check(data));
