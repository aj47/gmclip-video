//! FZ3 (SR §3.2): codec header field readers (H.264 avcC/SPS, VP9 superframe index and
//! uncompressed header) and vpcC synthesis, which must round-trip through our reader.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    gmclip_media::fuzz::headers(data);
});
