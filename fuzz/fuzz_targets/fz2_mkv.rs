//! FZ2 (SR §3.2): EBML/Matroska reader, cluster walk and resync.
//! As FZ1; resync terminates; no quadratic blow-up (`-timeout=2`).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    gmclip_media::fuzz::mkv(data);
});
