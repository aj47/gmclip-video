//! Audio (SPEC §5.6, §6.4): the export mix graph (B-11), loudness (B-12), AAC priming
//! compensation for edit-list-free files (R-EXP-5) and WAV I/O for PCM handed to and from
//! dev tools. Decode and AAC encode are the OS's (Lane H); everything here runs on f32
//! PCM at 48 kHz, a source at another rate converted at decode ([`resample`]).

pub mod graph;
pub mod loudness;
pub mod priming;
pub mod resample;
pub mod stretch;
pub mod wav;

pub use graph::{
    AudioPlan, Envelope, Fade, FadeShape, Join, LoudnessMode, MixError, Overlay, PartAudio,
    PcmSource, PlanError, Under,
};
