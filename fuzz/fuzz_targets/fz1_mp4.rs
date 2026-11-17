//! FZ1 (SR §3.2): MP4/MOV box walker + sample-table expansion + edit lists.
//! No panic, abort or OOM (RSS ≤ 256 MB); indexes monotone; R-MEDIA-SEC-2 caps hold.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    gmclip_media::fuzz::mp4(data);
});
