//! H.09 (R-PLAT-1): the startup capability probe's VideoToolbox questions. Asking never
//! fails and gives the same answer twice; on an Apple silicon Mac (lab) H.264, HEVC and
//! VP9 decode and H.264/HEVC encode are offered in hardware.
#![cfg(target_os = "macos")]

use gmclip_gpu::apple::vt_decode::{ProbeCodec, hardware_decode};
use gmclip_gpu::apple::vt_encode::hardware_encode_supported;

#[test]
fn the_probe_answers_and_is_stable() {
    for c in [
        ProbeCodec::H264,
        ProbeCodec::Hevc,
        ProbeCodec::Vp9,
        ProbeCodec::Av1,
    ] {
        assert_eq!(hardware_decode(c), hardware_decode(c), "{c:?}");
    }
    assert_eq!(
        hardware_encode_supported(false),
        hardware_encode_supported(false)
    );
    let _ = hardware_encode_supported(true);
}

#[test]
#[cfg_attr(
    not(feature = "lab"),
    ignore = "lab: needs VideoToolbox hardware (run with --features lab)"
)]
fn apple_silicon_offers_h264_hevc_vp9_in_hardware() {
    if std::env::consts::ARCH != "aarch64" {
        return;
    }
    assert!(hardware_decode(ProbeCodec::H264));
    assert!(hardware_decode(ProbeCodec::Hevc));
    assert!(hardware_decode(ProbeCodec::Vp9));
    assert!(hardware_encode_supported(false));
    assert!(hardware_encode_supported(true));
}
