//! OS decode/encode backends (SPEC §6.2, §6.5) and the platform-neutral pieces they
//! share: the PTS [`reorder`] stage, the frame-exact [`seek`] planner and the
//! [`source::SampleSource`] seam a demuxer plugs into.
//!
//! No `unsafe` here: every OS call goes through the safe wrappers in `gmclip-gpu`
//! (the one crate allowed `unsafe`, SPEC §3.1).

/// AAC decode (macOS and Windows) and the pure `esds` parser.
pub mod aac;
pub mod encode;
/// Frame-exact random access over any OS video decoder.
pub mod frames;
/// Linear PCM by frame (no decoder needed; decision 0051).
pub mod pcm;
pub mod reorder;
pub mod seek;
pub mod source;
pub mod stream;

/// VideoToolbox / AudioToolbox (macOS).
#[cfg(target_os = "macos")]
pub mod vt;

/// Media Foundation (Windows, M5).
#[cfg(windows)]
pub mod mf;

pub use encode::{
    AudioSampleSink, DtsRebaser, EncodedVideoSample, PeakMeter, RATE_CAP_LADDER, SOCIAL_PEAK_BPS,
    VideoSampleSink,
};
pub use reorder::ReorderStage;
pub use seek::{DecodeStep, is_leading_of, plan_seek};
pub use source::SampleSource;
pub use stream::FrameStream;
